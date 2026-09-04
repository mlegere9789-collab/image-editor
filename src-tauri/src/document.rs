//! The core document model: a stack of same-sized RGBA layers.
//!
//! Layer 0 is the bottom of the stack; the last layer is the top. Every layer
//! owns a document-sized, non-premultiplied RGBA8 buffer, which keeps the
//! compositor free of per-layer bounds arithmetic. Source images smaller than
//! the document are pasted at the origin and padded with transparency; larger
//! ones are clipped.

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::composite::{to_byte, to_unit, Rect};

pub type LayerId = u64;

/// Bytes per pixel in every buffer this module produces or accepts.
pub const CHANNELS: usize = 4;

#[derive(Debug, Clone)]
pub struct Layer {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    /// `0.0..=1.0`, multiplied into the layer's own alpha at composite time.
    pub opacity: f32,
    pub blend_mode: BlendMode,
    /// Blocks paint/erase strokes onto this layer's pixels — Photoshop's
    /// "Lock image pixels", the one lock sub-mode that actually protects
    /// against the edits this app can make. Compositing (visibility,
    /// opacity, blend mode, stacking order) is untouched by it: those
    /// aren't edits to the layer's own pixel data.
    pub locked: bool,
    /// Document-sized, non-premultiplied RGBA8.
    pub pixels: Vec<u8>,
}

/// The subset of a layer the UI needs — everything except the pixels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct LayerView {
    pub id: LayerId,
    pub name: String,
    pub visible: bool,
    pub opacity: f32,
    pub blend_mode: BlendMode,
    pub locked: bool,
}

impl Layer {
    pub fn view(&self) -> LayerView {
        LayerView {
            id: self.id,
            name: self.name.clone(),
            visible: self.visible,
            opacity: self.opacity,
            blend_mode: self.blend_mode,
            locked: self.locked,
        }
    }

    /// Whether this layer can affect the composite at all.
    pub fn contributes(&self) -> bool {
        self.visible && self.opacity > 0.0
    }
}

#[derive(Debug, Clone)]
pub struct Document {
    width: u32,
    height: u32,
    /// Bottom-to-top.
    layers: Vec<Layer>,
    next_id: LayerId,
    /// `None` means no active selection — the same as Photoshop's "Select
    /// All" state: every command that respects a selection treats a `None`
    /// document as unrestricted.
    selection: Option<Selection>,
    /// The selection `deselect` most recently cleared, kept around for
    /// `reselect` (Select > Reselect) to restore. Not touched by making a
    /// new selection while one is already active — only by `deselect`.
    last_selection: Option<Selection>,
}

/// The shape of the region a [`Selection`] covers within its bounding box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SelectionShape {
    Rectangle,
    Ellipse,
    /// A rectangle with corners rounded to `radius` pixels — the result of
    /// [`Document::smooth_selection`] on a `Rectangle` (or already-rounded)
    /// selection. Never produced any other way.
    RoundedRectangle {
        radius: u32,
    },
}

/// A hard-edged (no feather, no anti-aliasing) region of the document that
/// paint/erase strokes are clipped to. Represented as a shape plus its
/// bounding box rather than a document-sized mask — cheap to clone (every
/// `stroke()` call needs its own copy, see below) and exact for the two
/// shapes this supports today. A freehand/lasso selection, once added, would
/// need an actual mask and probably a third variant here rather than
/// replacing this representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Selection {
    pub shape: SelectionShape,
    pub bounds: Rect,
    /// Selects everywhere *except* the shape — Select > Inverse. Kept as a
    /// flag on the existing shape+bounds representation rather than a new
    /// variant: "the whole canvas minus a rectangle" is still exactly
    /// expressible by flipping one boolean, no mask needed.
    pub inverted: bool,
    /// Select > Modify > Border: when `Some(width)`, only a `width`-pixel
    /// band hugging the *inside* of the shape's own edge is selected — the
    /// interior beyond that band is excluded, carving a same-shaped hole
    /// out of the middle. `None` (the default) selects the shape's whole
    /// interior, as before.
    pub border: Option<u32>,
}

/// Whether `(px, py)` — the same `+0.5` pixel-centre convention
/// [`Document::stroke`] already samples at — falls inside `shape` sized to
/// `bounds`. Free of [`Selection`]'s `inverted`/`border` handling, which
/// callers layer on top; factored out of [`Selection::contains`] so it can
/// be reused against a shrunk copy of `bounds` for Select > Modify > Border.
fn shape_contains(shape: SelectionShape, bounds: Rect, px: f32, py: f32) -> bool {
    let Rect { x0, y0, x1, y1 } = bounds;
    let in_bounds = !(px < x0 as f32 || px >= x1 as f32 || py < y0 as f32 || py >= y1 as f32);
    in_bounds
        && match shape {
            SelectionShape::Rectangle => true,
            SelectionShape::Ellipse => {
                let (cx, cy) = ((x0 as f32 + x1 as f32) / 2.0, (y0 as f32 + y1 as f32) / 2.0);
                let (rx, ry) = ((x1 - x0) as f32 / 2.0, (y1 - y0) as f32 / 2.0);
                let (nx, ny) = ((px - cx) / rx, (py - cy) / ry);
                nx * nx + ny * ny <= 1.0
            }
            SelectionShape::RoundedRectangle { radius } => {
                // Clamp (px, py) onto the rectangle inset by `radius` on
                // every side, then require the point be within `radius` of
                // that clamped point. On a flat edge this reduces to an
                // ordinary straight-edge distance check; in a corner square
                // it checks distance to that corner's rounding circle.
                // Standard rounded-rect hit test. `radius` is clamped again
                // here (not just at creation) since a Border-shrunk `bounds`
                // can be smaller than the shape's own original radius.
                let r = (radius as f32)
                    .min((x1 - x0) as f32 / 2.0)
                    .min((y1 - y0) as f32 / 2.0);
                let cx = px.clamp(x0 as f32 + r, x1 as f32 - r);
                let cy = py.clamp(y0 as f32 + r, y1 as f32 - r);
                let (dx, dy) = (px - cx, py - cy);
                dx * dx + dy * dy <= r * r
            }
        }
}

/// `bounds` shrunk by `width` pixels on every side, or `None` if that would
/// collapse it to zero or negative area.
fn shrink_rect(bounds: Rect, width: u32) -> Option<Rect> {
    let Rect { x0, y0, x1, y1 } = bounds;
    let width = width as i64;
    let (nx0, ny0) = (x0 as i64 + width, y0 as i64 + width);
    let (nx1, ny1) = (x1 as i64 - width, y1 as i64 - width);
    if nx0 >= nx1 || ny0 >= ny1 {
        return None;
    }
    Some(Rect {
        x0: nx0 as u32,
        y0: ny0 as u32,
        x1: nx1 as u32,
        y1: ny1 as u32,
    })
}

impl Selection {
    /// Whether the pixel centred at `(px, py)` — the same `+0.5` convention
    /// [`Document::stroke`] already samples at — falls inside this selection.
    fn contains(&self, px: f32, py: f32) -> bool {
        let in_shape = shape_contains(self.shape, self.bounds, px, py);
        let in_border_hole = self
            .border
            .and_then(|width| shrink_rect(self.bounds, width))
            .is_some_and(|inner| shape_contains(self.shape, inner, px, py));
        (in_shape && !in_border_hole) != self.inverted
    }
}

/// The subset of a [`Selection`] the UI needs to draw its outline.
pub type SelectionView = Selection;

/// What [`Document::copy`]/[`Document::cut`] hand back for
/// [`Document::paste`] to restore later: the copied region's own pixels
/// (already masked to the selection's exact shape, not just its bounding
/// box — see [`Document::extract`]) plus where it sat in the document it
/// was copied from. Opaque outside this module — `lib.rs` only stores and
/// forwards it, never inspects its fields.
#[derive(Debug, Clone)]
pub struct Clipboard {
    origin: Rect,
    width: u32,
    height: u32,
    /// `width * height * CHANNELS` bytes, row-major, relative to `origin`.
    pixels: Vec<u8>,
}

/// Turn two arbitrary drag corners into a selection's bounding box: sorted
/// into min/max, clamped to the document, and rejected if it covers no
/// pixels (a click with no drag).
fn normalize_selection_bounds(
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    width: u32,
    height: u32,
) -> Result<Rect, String> {
    if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
        return Err("Selection coordinates must be finite numbers.".to_string());
    }
    let (min_x, max_x) = (x0.min(x1), x0.max(x1));
    let (min_y, max_y) = (y0.min(y1), y0.max(y1));
    let cx0 = (min_x.floor().max(0.0) as u32).min(width);
    let cy0 = (min_y.floor().max(0.0) as u32).min(height);
    let cx1 = (max_x.ceil().max(0.0) as u32).min(width);
    let cy1 = (max_y.ceil().max(0.0) as u32).min(height);
    if cx0 >= cx1 || cy0 >= cy1 {
        return Err("A selection must cover at least one pixel.".to_string());
    }
    Ok(Rect {
        x0: cx0,
        y0: cy0,
        x1: cx1,
        y1: cy1,
    })
}

/// Where a layer should move in the stack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct DocumentView {
    pub width: u32,
    pub height: u32,
    /// Bottom-to-top, matching the model. The UI reverses this for display.
    pub layers: Vec<LayerView>,
    /// `None` when nothing is selected — the frontend draws no marching-ants
    /// outline and every stroke is unrestricted.
    pub selection: Option<SelectionView>,
    /// Whether `reselect` has something to restore right now.
    pub can_reselect: bool,
}

impl Document {
    /// A document with no layers. Flattening it yields fully transparent pixels.
    pub fn new(width: u32, height: u32) -> Result<Self, String> {
        if width == 0 || height == 0 {
            return Err(format!("A document cannot be {width}x{height}."));
        }
        Ok(Self {
            width,
            height,
            layers: Vec::new(),
            next_id: 1,
            selection: None,
            last_selection: None,
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn layers(&self) -> &[Layer] {
        &self.layers
    }

    pub fn view(&self) -> DocumentView {
        DocumentView {
            width: self.width,
            height: self.height,
            layers: self.layers.iter().map(Layer::view).collect(),
            selection: self.selection,
            can_reselect: self.last_selection.is_some(),
        }
    }

    /// Replace the selection with an axis-aligned rectangle spanning the two
    /// corners `(x0, y0)` and `(x1, y1)` — in either order, as a drag can go
    /// any direction.
    pub fn select_rectangle(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> Result<(), String> {
        let bounds = normalize_selection_bounds(x0, y0, x1, y1, self.width, self.height)?;
        self.selection = Some(Selection {
            shape: SelectionShape::Rectangle,
            bounds,
            inverted: false,
            border: None,
        });
        Ok(())
    }

    /// Replace the selection with an ellipse inscribed in the bounding box
    /// spanning `(x0, y0)` and `(x1, y1)`.
    pub fn select_ellipse(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) -> Result<(), String> {
        let bounds = normalize_selection_bounds(x0, y0, x1, y1, self.width, self.height)?;
        self.selection = Some(Selection {
            shape: SelectionShape::Ellipse,
            bounds,
            inverted: false,
            border: None,
        });
        Ok(())
    }

    /// Select the entire canvas — a rectangle spanning the whole document.
    pub fn select_all(&mut self) -> Result<(), String> {
        self.selection = Some(Selection {
            shape: SelectionShape::Rectangle,
            bounds: Rect {
                x0: 0,
                y0: 0,
                x1: self.width,
                y1: self.height,
            },
            inverted: false,
            border: None,
        });
        Ok(())
    }

    /// Select > Inverse: swap selected and unselected pixels. An error if
    /// nothing is currently selected — same as Photoshop, which disables the
    /// menu item rather than making "invert nothing" mean "select nothing".
    pub fn invert_selection(&mut self) -> Result<(), String> {
        let selection = self
            .selection
            .as_mut()
            .ok_or_else(|| "Nothing is selected.".to_string())?;
        selection.inverted = !selection.inverted;
        Ok(())
    }

    /// Select > Modify > Expand: grow the selected region outward by
    /// `amount` pixels on every side, clamped to the canvas edge. An error
    /// if nothing is selected, or `amount` is zero (Photoshop's own dialog
    /// requires a positive number of pixels).
    pub fn expand_selection(&mut self, amount: u32) -> Result<(), String> {
        if amount == 0 {
            return Err("Expand By must be greater than zero pixels.".to_string());
        }
        self.resize_selection_bounds(amount as i64)
    }

    /// Select > Modify > Contract: shrink the selected region inward by
    /// `amount` pixels on every side. An error if nothing is selected,
    /// `amount` is zero, or contracting that far would leave no pixels
    /// selected at all.
    pub fn contract_selection(&mut self, amount: u32) -> Result<(), String> {
        if amount == 0 {
            return Err("Contract By must be greater than zero pixels.".to_string());
        }
        self.resize_selection_bounds(-(amount as i64))
    }

    /// Shared bounds arithmetic for [`Self::expand_selection`] and
    /// [`Self::contract_selection`]: grows the shape's bounding box by
    /// `delta` pixels per side (negative shrinks it). For an *inverted*
    /// selection — everywhere except the shape — growing the *selected*
    /// area means shrinking the excluded shape, so `delta`'s sign flips
    /// relative to the shape's own bounds in that case; this is what makes
    /// Expand/Contract behave correctly after Select > Inverse without any
    /// mask-based representation.
    fn resize_selection_bounds(&mut self, delta: i64) -> Result<(), String> {
        let (width, height) = (self.width as i64, self.height as i64);
        let selection = self
            .selection
            .as_mut()
            .ok_or_else(|| "Nothing is selected.".to_string())?;
        let shape_delta = if selection.inverted { -delta } else { delta };
        let b = selection.bounds;
        let x0 = (b.x0 as i64 - shape_delta).clamp(0, width);
        let y0 = (b.y0 as i64 - shape_delta).clamp(0, height);
        let x1 = (b.x1 as i64 + shape_delta).clamp(0, width);
        let y1 = (b.y1 as i64 + shape_delta).clamp(0, height);
        if x0 >= x1 || y0 >= y1 {
            return Err("That would leave nothing selected.".to_string());
        }
        selection.bounds = Rect {
            x0: x0 as u32,
            y0: y0 as u32,
            x1: x1 as u32,
            y1: y1 as u32,
        };
        Ok(())
    }

    /// Select > Modify > Smooth: rounds a rectangular selection's corners.
    /// Photoshop's own Smooth operates on arbitrary, possibly irregular
    /// selections by rounding off jagged edges and filling small gaps in a
    /// pixel-mask representation; since this selection system represents a
    /// selection as a shape plus its bounding box rather than a mask (see
    /// [`Selection`]'s own doc comment), the well-defined analogue for a
    /// `Rectangle` (or already-`RoundedRectangle`) selection is to round its
    /// corners by `radius` pixels — exactly what "smoothing" means when
    /// there are no jagged pixels to begin with. Applied to an `Ellipse`
    /// selection this is a no-op: an ellipse's boundary is already smooth
    /// everywhere, so rounding its nonexistent corners changes nothing.
    /// `radius` is clamped to at most half the shorter side of the
    /// selection's bounding box — beyond that a corner radius has no
    /// further visual effect, since the rectangle is already as rounded as
    /// it can get. An error if nothing is selected, or `radius` is zero.
    pub fn smooth_selection(&mut self, radius: u32) -> Result<(), String> {
        if radius == 0 {
            return Err("Smooth radius must be greater than zero pixels.".to_string());
        }
        let selection = self
            .selection
            .as_mut()
            .ok_or_else(|| "Nothing is selected.".to_string())?;
        if matches!(
            selection.shape,
            SelectionShape::Rectangle | SelectionShape::RoundedRectangle { .. }
        ) {
            let b = selection.bounds;
            let half_short_side = (b.x1 - b.x0).min(b.y1 - b.y0) / 2;
            selection.shape = SelectionShape::RoundedRectangle {
                radius: radius.min(half_short_side),
            };
        }
        Ok(())
    }

    /// Select > Modify > Border: turn the selection into a `width`-pixel
    /// band hugging the *inside* of its own edge, excluding the interior
    /// beyond that band — the classic "photo frame" selection, useful for
    /// painting an outline around a shape without touching its middle.
    /// Photoshop's own Border straddles the original edge (extending
    /// outward too, into a fresh region that would need re-clamping against
    /// the canvas) and feathers the result; this hard-edged selection
    /// system instead keeps the shape's *outer* boundary exactly where it
    /// was and only carves a same-shaped hole out of the interior — a
    /// deliberate scope cut that still produces the same everyday "frame a
    /// selection" effect without growing the bounding box. Once `width` is
    /// at least half the shorter side, the hole disappears entirely and the
    /// whole shape is selected, same as before Border was applied (see
    /// [`shrink_rect`]). Reapplying Border recomputes the band from the
    /// selection's original shape, not the current ring — it does not
    /// stack into a border of a border. An error if nothing is selected, or
    /// `width` is zero.
    pub fn border_selection(&mut self, width: u32) -> Result<(), String> {
        if width == 0 {
            return Err("Border Width must be greater than zero pixels.".to_string());
        }
        let selection = self
            .selection
            .as_mut()
            .ok_or_else(|| "Nothing is selected.".to_string())?;
        selection.border = Some(width);
        Ok(())
    }

    /// Clear the selection — every stroke goes back to being unrestricted.
    /// Remembers what was cleared, for `reselect` to restore.
    pub fn deselect(&mut self) {
        if let Some(selection) = self.selection.take() {
            self.last_selection = Some(selection);
        }
    }

    /// Select > Reselect: restore the selection `deselect` most recently
    /// cleared. An error if there is nothing to restore — same as
    /// Photoshop, which disables the menu item rather than making
    /// "reselect nothing" mean "select nothing".
    pub fn reselect(&mut self) -> Result<(), String> {
        self.selection = Some(
            self.last_selection
                .ok_or_else(|| "Nothing to reselect.".to_string())?,
        );
        Ok(())
    }

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    /// Number of bytes in a document-sized RGBA8 buffer.
    pub fn buffer_len(&self) -> usize {
        self.width as usize * self.height as usize * CHANNELS
    }

    /// Add `source` (an RGBA8 buffer of `source_width` x `source_height`) as a new
    /// top layer, pasted at the origin and clipped or padded to document size.
    pub fn add_layer(
        &mut self,
        name: impl Into<String>,
        source: &[u8],
        source_width: u32,
        source_height: u32,
    ) -> Result<LayerId, String> {
        let expected = source_width as usize * source_height as usize * CHANNELS;
        if source.len() != expected {
            return Err(format!(
                "Expected {expected} bytes for a {source_width}x{source_height} RGBA image, got {}.",
                source.len()
            ));
        }

        let mut pixels = vec![0u8; self.buffer_len()];
        let copy_width = source_width.min(self.width) as usize * CHANNELS;
        let copy_height = source_height.min(self.height) as usize;
        let src_stride = source_width as usize * CHANNELS;
        let dst_stride = self.width as usize * CHANNELS;
        for row in 0..copy_height {
            let src = row * src_stride;
            let dst = row * dst_stride;
            pixels[dst..dst + copy_width].copy_from_slice(&source[src..src + copy_width]);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        });
        Ok(id)
    }

    /// Layer > New Fill Layer > Solid Color: adds a new top layer filled
    /// entirely with `color` (RGBA8). A real Photoshop fill layer stays
    /// "live" — double-clicking it later reopens a colour picker and
    /// repaints the whole layer in place, all without needing a mask or
    /// touching any layer below it. This app's layer model has no such
    /// generative layer kind (every [`Layer`] is an ordinary pixel buffer —
    /// see the `PIXEL LAYER` entry in `docs/PHOTOSHOP_PARITY.md`), so the
    /// scope cut here is the same one [`Self::add_layer`] (Add Layer from
    /// file) already makes: this creates an ordinary pixel layer whose
    /// initial content happens to be a flat fill, exactly as if the whole
    /// canvas had been painted with the Paint Bucket at 100% opacity —
    /// editable afterward like any other layer, just not re-openable as a
    /// live "recipe". Cannot fail: `color` and the document's own size are
    /// always valid.
    pub fn add_solid_color_layer(&mut self, name: impl Into<String>, color: [u8; 4]) -> LayerId {
        let mut pixels = vec![0u8; self.buffer_len()];
        for pixel in pixels.chunks_exact_mut(CHANNELS) {
            pixel.copy_from_slice(&color);
        }

        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        });
        id
    }

    /// Layer > New Fill Layer > Gradient: adds a new top layer filled with
    /// a linear gradient from `start_color` at the top-left corner to
    /// `end_color` at the bottom-right corner — the same linear-
    /// interpolation math [`Self::gradient_fill`] (the Gradient tool)
    /// already implements, applied once to a brand new, fully transparent
    /// layer instead of onto existing pixels. Photoshop's own Gradient
    /// Fill Layer dialog lets you configure angle, scale, gradient style
    /// (linear/radial/angle/reflected/diamond), and offset; this always
    /// runs a linear gradient along the canvas's own top-left-to-
    /// bottom-right diagonal — a deliberate scope cut, in the same spirit
    /// as Gradient Map's own fixed two-stop straight line. Cannot fail:
    /// the new layer is never locked, and a document's diagonal is always
    /// nonzero (a document can't be 0x0 — see [`Self::new`]), so the two
    /// preconditions [`Self::gradient_fill`] itself checks always hold.
    pub fn add_gradient_layer(
        &mut self,
        name: impl Into<String>,
        start_color: [u8; 4],
        end_color: [u8; 4],
    ) -> LayerId {
        let id = self.add_solid_color_layer(name, [0, 0, 0, 0]);
        let to = (self.width as f32, self.height as f32);
        self.gradient_fill(id, (0.0, 0.0), to, start_color, end_color)
            .expect("a brand new layer is never locked, and a document's diagonal is nonzero");
        id
    }

    fn index_of(&self, id: LayerId) -> Result<usize, String> {
        self.layers
            .iter()
            .position(|layer| layer.id == id)
            .ok_or_else(|| format!("No layer with id {id}."))
    }

    fn layer_mut(&mut self, id: LayerId) -> Result<&mut Layer, String> {
        let index = self.index_of(id)?;
        Ok(&mut self.layers[index])
    }

    pub fn set_visible(&mut self, id: LayerId, visible: bool) -> Result<(), String> {
        self.layer_mut(id)?.visible = visible;
        Ok(())
    }

    /// Toggle Lock (image pixels) — see [`Layer::locked`] for exactly what
    /// it blocks.
    pub fn set_locked(&mut self, id: LayerId, locked: bool) -> Result<(), String> {
        self.layer_mut(id)?.locked = locked;
        Ok(())
    }

    /// Layer > Rasterize > Type / Shape / Smart Object / Layer Style / ...:
    /// converts a vector, text, or smart-object layer into an ordinary
    /// pixel layer. Every [`Layer`] in this app is already a document-sized
    /// RGBA8 pixel buffer — there is no vector, text, shape, or
    /// smart-object layer type to convert *from* (see the `PIXEL LAYER`
    /// entry in `docs/PHOTOSHOP_PARITY.md`) — so this is always a true
    /// no-op that touches nothing. It still validates that `id` names a
    /// real layer, the same "No layer with id N" error every other layer
    /// command gives for an unknown id, exactly matching Photoshop's own
    /// behaviour of disabling Rasterize entirely (rather than silently
    /// accepting the click) once a layer is already pixels.
    pub fn rasterize_layer(&mut self, id: LayerId) -> Result<(), String> {
        self.layer_mut(id)?;
        Ok(())
    }

    /// Edit > Transform > Flip Horizontal: mirrors layer `id`'s own pixels
    /// left-to-right, in place. Applies to the whole layer regardless of
    /// any active selection — modelled on Image > Image Rotation, which is
    /// likewise unaffected by a selection, rather than the selection-aware
    /// behaviour Edit > Transform can have on a normal layer in real
    /// Photoshop — a deliberate scope cut, since precisely constraining a
    /// flip to an arbitrary (possibly non-rectangular) selection shape
    /// would need a real mask. Document dimensions never change: every
    /// layer stays document-sized, so a flip is always well-defined.
    /// Errors if the layer is unknown or locked, same as every other
    /// command that rewrites a layer's own pixels.
    pub fn flip_layer_horizontal(&mut self, id: LayerId) -> Result<(), String> {
        let width = self.width as usize;
        let height = self.height as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        for row in 0..height {
            let row_start = row * width * CHANNELS;
            for col in 0..width / 2 {
                let left = row_start + col * CHANNELS;
                let right = row_start + (width - 1 - col) * CHANNELS;
                for c in 0..CHANNELS {
                    layer.pixels.swap(left + c, right + c);
                }
            }
        }
        Ok(())
    }

    /// Edit > Transform > Flip Vertical: mirrors layer `id`'s own pixels
    /// top-to-bottom, in place. Same scope (whole layer, selection
    /// ignored) and error conditions as [`Self::flip_layer_horizontal`].
    pub fn flip_layer_vertical(&mut self, id: LayerId) -> Result<(), String> {
        let width = self.width as usize;
        let height = self.height as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let row_bytes = width * CHANNELS;
        for row in 0..height / 2 {
            let top = row * row_bytes;
            let bottom = (height - 1 - row) * row_bytes;
            let (a, b) = layer.pixels.split_at_mut(bottom);
            a[top..top + row_bytes].swap_with_slice(&mut b[..row_bytes]);
        }
        Ok(())
    }

    /// Edit > Transform > Rotate 180°: rotates layer `id`'s own pixels by
    /// half a turn, in place — equivalent to a horizontal flip followed by
    /// a vertical one, implemented directly as a single reversal of the
    /// whole pixel buffer (each pixel at index `i` swaps with the one at
    /// `total - 1 - i`, which is exactly `(x, y) -> (width-1-x,
    /// height-1-y)` for a row-major buffer). Same scope (whole layer,
    /// selection ignored) and error conditions as
    /// [`Self::flip_layer_horizontal`]. Document dimensions never change —
    /// unlike a 90° rotation, which would need to swap width and height
    /// and so isn't offered here (every layer must stay document-sized).
    pub fn rotate_layer_180(&mut self, id: LayerId) -> Result<(), String> {
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let total_pixels = layer.pixels.len() / CHANNELS;
        for i in 0..total_pixels / 2 {
            let a = i * CHANNELS;
            let b = (total_pixels - 1 - i) * CHANNELS;
            for c in 0..CHANNELS {
                layer.pixels.swap(a + c, b + c);
            }
        }
        Ok(())
    }

    /// Image > Image Rotation > 90° Clockwise / 90° Counter Clockwise:
    /// rotates the entire document — every layer, and the canvas itself —
    /// by a quarter turn, swapping width and height. Unlike
    /// [`Self::flip_layer_horizontal`] and its siblings, which rotate or
    /// mirror a single layer's own pixels in place, a 90° turn can't
    /// preserve dimensions (a W×H canvas becomes H×W), so this necessarily
    /// operates on the whole document rather than one layer — there is no
    /// way to keep every layer document-sized without resizing the
    /// document itself. Each layer's pixel buffer is rebuilt at the new
    /// dimensions by pulling from the old one: for clockwise, new pixel
    /// `(nx, ny)` comes from old pixel `(ny, old_height - 1 - nx)`; for
    /// counter-clockwise, from `(old_width - 1 - ny, nx)` — the standard
    /// "transpose, then reverse rows/columns" matrix rotation, derived and
    /// hand-verified against a small lettered grid in this function's own
    /// tests. Clears the active selection and whatever `reselect` would
    /// have restored: a selection's bounds are meaningless against a
    /// document whose dimensions just changed shape, and there is no
    /// sensible way to carry it forward. Cannot fail — every layer is
    /// exactly document-sized before and after, so there is nothing to
    /// validate — even a document with no layers yet simply swaps its own
    /// width and height.
    pub fn rotate_document_90(&mut self, clockwise: bool) {
        let (old_width, old_height) = (self.width, self.height);
        let (new_width, new_height) = (old_height, old_width);
        for layer in &mut self.layers {
            let mut rotated = vec![0u8; new_width as usize * new_height as usize * CHANNELS];
            for new_y in 0..new_height {
                for new_x in 0..new_width {
                    let (old_x, old_y) = if clockwise {
                        (new_y, old_height - 1 - new_x)
                    } else {
                        (old_width - 1 - new_y, new_x)
                    };
                    let src = (old_y as usize * old_width as usize + old_x as usize) * CHANNELS;
                    let dst = (new_y as usize * new_width as usize + new_x as usize) * CHANNELS;
                    rotated[dst..dst + CHANNELS]
                        .copy_from_slice(&layer.pixels[src..src + CHANNELS]);
                }
            }
            layer.pixels = rotated;
        }
        self.width = new_width;
        self.height = new_height;
        self.selection = None;
        self.last_selection = None;
    }

    fn layer(&self, id: LayerId) -> Result<&Layer, String> {
        let index = self.index_of(id)?;
        Ok(&self.layers[index])
    }

    /// The region [`Self::copy`]/[`Self::cut`] act on: the active
    /// selection's bounding box, or the whole canvas when nothing is
    /// selected — Photoshop's own "no selection means everything" rule for
    /// Edit > Copy and Edit > Cut.
    fn copy_bounds(&self) -> Rect {
        self.selection.map(|s| s.bounds).unwrap_or(Rect {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        })
    }

    /// `layer`'s pixels within `bounds`, masked by the active selection's
    /// actual shape (not just its bounding box): a pixel inside `bounds`
    /// but outside a non-rectangular selection (an ellipse, a rounded
    /// rectangle, a bordered ring, an inverted selection, ...) comes out
    /// fully transparent rather than copied, exactly as a paste of that
    /// same shape would look pasted onto an empty layer.
    fn extract(&self, layer: &Layer, bounds: Rect) -> Clipboard {
        let width = bounds.x1 - bounds.x0;
        let height = bounds.y1 - bounds.y0;
        let doc_width = self.width as usize;
        let mut pixels = vec![0u8; width as usize * height as usize * CHANNELS];
        for row in 0..height {
            for col in 0..width {
                let px = bounds.x0 + col;
                let py = bounds.y0 + row;
                let keep = self
                    .selection
                    .map_or(true, |s| s.contains(px as f32 + 0.5, py as f32 + 0.5));
                if !keep {
                    continue;
                }
                let src = (py as usize * doc_width + px as usize) * CHANNELS;
                let dst = (row as usize * width as usize + col as usize) * CHANNELS;
                pixels[dst..dst + CHANNELS].copy_from_slice(&layer.pixels[src..src + CHANNELS]);
            }
        }
        Clipboard {
            origin: bounds,
            width,
            height,
            pixels,
        }
    }

    /// Edit > Copy: captures layer `id`'s pixels within the active
    /// selection (or the whole canvas, with none) into a [`Clipboard`],
    /// ready for [`Self::paste`]. Doesn't touch the document at all, so
    /// there's nothing to checkpoint and, unlike every command that
    /// rewrites pixels, no lock check — copying from a locked layer is
    /// fine, the same as in Photoshop.
    pub fn copy(&self, id: LayerId) -> Result<Clipboard, String> {
        let layer = self.layer(id)?;
        let bounds = self.copy_bounds();
        Ok(self.extract(layer, bounds))
    }

    /// Edit > Cut: [`Self::copy`], then clears the copied pixels (to fully
    /// transparent) from layer `id` — Photoshop's own "copy, then delete"
    /// behaviour. Only the cleared region is reported dirty. Errors if the
    /// layer is locked; the copy itself never fails once the layer id is
    /// valid, so a locked layer leaves both the document and the caller's
    /// existing clipboard untouched.
    pub fn cut(&mut self, id: LayerId) -> Result<(Clipboard, Option<Rect>), String> {
        let clipboard = self.copy(id)?;
        let bounds = clipboard.origin;
        self.paint_region(id, bounds, [0, 0, 0, 0])?;
        Ok((clipboard, Some(bounds)))
    }

    /// Overwrites every selection-covered pixel of layer `id` within
    /// `bounds` with `color`, leaving pixels outside the selection (but
    /// still inside `bounds`) untouched — the shared pixel-writing loop
    /// behind [`Self::cut`] and [`Self::delete_selection`]
    /// (`color = [0, 0, 0, 0]`, i.e. "clear to transparent") and
    /// [`Self::fill_selection`] (any other colour). Errors if the layer is
    /// locked, the same as every other command that rewrites a layer's own
    /// pixels.
    fn paint_region(&mut self, id: LayerId, bounds: Rect, color: [u8; 4]) -> Result<(), String> {
        let selection = self.selection;
        let doc_width = self.width as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let base = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[base..base + CHANNELS].copy_from_slice(&color);
            }
        }
        Ok(())
    }

    /// Edit > Delete (this app has no separate Edit > Clear: with no
    /// dedicated "Background" layer type — every [`Layer`] here already
    /// supports transparency, see the `PIXEL LAYER` entry in
    /// `docs/PHOTOSHOP_PARITY.md` — Delete and Clear would do exactly the
    /// same thing, so one command covers both menu items): clears layer
    /// `id`'s pixels within the active selection (or the whole layer, with
    /// none) to fully transparent, in place. Unlike [`Self::cut`], nothing
    /// is captured to the clipboard first.
    pub fn delete_selection(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        let bounds = self.copy_bounds();
        self.paint_region(id, bounds, [0, 0, 0, 0])?;
        Ok(Some(bounds))
    }

    /// Edit > Fill: paints layer `id`'s pixels within the active selection
    /// (or the whole layer, with none) with a single flat `color`,
    /// replacing whatever was there — the same "paint at 100% opacity,
    /// Normal blend, no live recipe" scope cut
    /// [`Self::add_solid_color_layer`] already makes for a brand new
    /// layer, just applied in place to an existing one instead. Unlike
    /// [`Self::flood_fill`] (the Paint Bucket tool), this doesn't stop at a
    /// colour boundary — it overwrites every selected pixel regardless of
    /// what was under it, exactly like Photoshop's Edit > Fill with a
    /// solid colour (foreground/background/custom colour are all just
    /// "some RGBA value" at this layer, so there's a single `color`
    /// parameter rather than a fill-source enum).
    pub fn fill_selection(&mut self, id: LayerId, color: [u8; 4]) -> Result<Option<Rect>, String> {
        let bounds = self.copy_bounds();
        self.paint_region(id, bounds, color)?;
        Ok(Some(bounds))
    }

    /// Edit > Paste — and, since this app has no scrollable viewport to
    /// paste into the middle of, also Edit > Paste Special > Paste in
    /// Place: adds `clipboard`'s pixels as a new top layer, positioned at
    /// the same document coordinates they were copied from. Clipped
    /// against the *current* document's own size, which can differ from
    /// the one the clipboard was copied out of — like Photoshop's own
    /// clipboard, it survives switching documents (see `AppState::clipboard`
    /// in `lib.rs`, kept outside per-document undo/redo history) and
    /// operations like [`Self::rotate_document_90`] that change a
    /// document's dimensions mid-flight. Cannot fail: a paste that lands
    /// partly or fully outside the current canvas simply produces a new
    /// layer with that much less visible on it, the same as pasting into a
    /// too-small canvas in real Photoshop.
    pub fn paste(&mut self, clipboard: &Clipboard, name: impl Into<String>) -> LayerId {
        let mut pixels = vec![0u8; self.buffer_len()];
        let doc_width = self.width as usize;
        for row in 0..clipboard.height {
            let py = clipboard.origin.y0 + row;
            if py >= self.height {
                break;
            }
            for col in 0..clipboard.width {
                let px = clipboard.origin.x0 + col;
                if px >= self.width {
                    continue;
                }
                let src = (row as usize * clipboard.width as usize + col as usize) * CHANNELS;
                let dst = (py as usize * doc_width + px as usize) * CHANNELS;
                pixels[dst..dst + CHANNELS].copy_from_slice(&clipboard.pixels[src..src + CHANNELS]);
            }
        }
        let id = self.next_id;
        self.next_id += 1;
        self.layers.push(Layer {
            id,
            name: name.into(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        });
        id
    }

    /// Filter > Blur > Box Blur: averages every channel of each pixel in
    /// the active selection (or the whole layer, with none) with its
    /// neighbours in a `(2*radius+1)`-square window, clamped to the
    /// layer's own edges rather than wrapping or padding with transparency
    /// — sampling past an edge just repeats the edge pixel, which also
    /// keeps every window exactly `(2*radius+1)^2` samples regardless of
    /// where the pixel sits, so integer-division rounding is uniform
    /// everywhere rather than shifting near a border. A box blur (a flat
    /// mean) is the simplest blur there is; unlike Photoshop's own blur
    /// filters, which are alpha-aware to avoid dark fringing where an
    /// opaque pixel blurs into a fully transparent neighbour, this
    /// averages R, G, B, and A independently and un-premultiplied — the
    /// same "no extra colour science beyond what's already in the file"
    /// scope cut the Levels/Curves/Color Balance adjustments already make.
    /// Every sample is read from a snapshot of the layer taken before any
    /// pixel is written, so a wide radius never blurs already-blurred
    /// pixels into their neighbours mid-pass. Errors on a zero radius (a
    /// 1x1 "blur" is a no-op, not worth a menu item) or a locked/unknown
    /// layer.
    pub fn box_blur(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Blur radius must be at least 1 pixel.".to_string());
        }
        let bounds = self.copy_bounds();
        let selection = self.selection;
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let source = layer.pixels.clone();
        let r = radius as i64;
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let mut sums = [0u32; CHANNELS];
                let mut count = 0u32;
                for dy in -r..=r {
                    let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
                    for dx in -r..=r {
                        let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
                        let base = (sy * doc_width + sx) * CHANNELS;
                        for (c, sum) in sums.iter_mut().enumerate() {
                            *sum += source[base + c] as u32;
                        }
                        count += 1;
                    }
                }
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                for (sum, pixel) in sums.iter().zip(&mut layer.pixels[dst..dst + CHANNELS]) {
                    *pixel = (sum / count) as u8;
                }
            }
        }
        Ok(Some(bounds))
    }

    /// Values outside `0.0..=1.0` are clamped rather than rejected, so a slider
    /// that overshoots by a rounding step is not an error.
    pub fn set_opacity(&mut self, id: LayerId, opacity: f32) -> Result<(), String> {
        if !opacity.is_finite() {
            return Err(format!("Opacity must be a number, got {opacity}."));
        }
        self.layer_mut(id)?.opacity = opacity.clamp(0.0, 1.0);
        Ok(())
    }

    pub fn set_blend_mode(&mut self, id: LayerId, blend_mode: BlendMode) -> Result<(), String> {
        self.layer_mut(id)?.blend_mode = blend_mode;
        Ok(())
    }

    pub fn remove_layer(&mut self, id: LayerId) -> Result<(), String> {
        let index = self.index_of(id)?;
        self.layers.remove(index);
        Ok(())
    }

    /// Move a layer one step through the stack. Moving the top layer up (or the
    /// bottom layer down) is a no-op rather than an error — it is what a UI
    /// button press at the end of the stack should do.
    pub fn move_layer(&mut self, id: LayerId, direction: MoveDirection) -> Result<(), String> {
        let index = self.index_of(id)?;
        let target = match direction {
            MoveDirection::Up if index + 1 < self.layers.len() => index + 1,
            MoveDirection::Down if index > 0 => index - 1,
            _ => return Ok(()),
        };
        self.layers.swap(index, target);
        Ok(())
    }

    /// Merge Visible: composite every visible layer into one new layer, in
    /// place of the layers it replaces — hidden layers are left exactly
    /// where they were, in their original relative order. The new layer
    /// lands at the position of the bottommost layer it merged, is itself
    /// visible, fully opaque, and blends Normal (its pixels are already
    /// pre-blended, so nothing further needs to happen at composite time to
    /// reproduce the same appearance). Errors with fewer than two visible
    /// layers — there is nothing meaningful to merge.
    pub fn merge_visible(&mut self) -> Result<LayerId, String> {
        let visible_indices: Vec<usize> = self
            .layers
            .iter()
            .enumerate()
            .filter(|(_, layer)| layer.visible)
            .map(|(index, _)| index)
            .collect();
        if visible_indices.len() < 2 {
            return Err("Merge Visible needs at least two visible layers.".to_string());
        }

        let pixels = crate::composite::flatten_subset(self, &visible_indices).pixels;
        let id = self.next_id;
        self.next_id += 1;
        let mut merged = Some(Layer {
            id,
            name: "Merged".to_string(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        });

        let old_layers = std::mem::take(&mut self.layers);
        self.layers = old_layers
            .into_iter()
            .filter_map(|layer| {
                if layer.visible {
                    merged.take()
                } else {
                    Some(layer)
                }
            })
            .collect();
        Ok(id)
    }

    /// Flatten Image: composite every layer — visible or not — into a
    /// single new `"Background"` layer, discarding the rest of the stack
    /// entirely. Unlike [`Document::merge_visible`], hidden layers do not
    /// survive: this is meant to produce the one flattened layer a finished
    /// document reduces to, not to preserve work in progress. Errors only
    /// when there are no layers to flatten at all.
    pub fn flatten_image(&mut self) -> Result<LayerId, String> {
        if self.layers.is_empty() {
            return Err("Nothing to flatten.".to_string());
        }
        let pixels = crate::composite::flatten(self).pixels;
        let id = self.next_id;
        self.next_id += 1;
        self.layers = vec![Layer {
            id,
            name: "Background".to_string(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        }];
        Ok(id)
    }

    /// Merge Down: composite a layer with the one directly below it in the
    /// stack, replacing both with one new layer at that position. Respects
    /// each layer's own visibility and opacity exactly like `flatten` does —
    /// a currently hidden or zero-opacity layer contributes nothing, the
    /// same as it would if left alone, rather than always showing through
    /// regardless. The merged layer takes the name of the layer it merged
    /// *into* (the one below), matching Photoshop's own Merge Down. Errors
    /// if `id` is unknown, or is already the bottom layer with nothing
    /// below it to merge into.
    pub fn merge_down(&mut self, id: LayerId) -> Result<LayerId, String> {
        let index = self.index_of(id)?;
        if index == 0 {
            return Err(format!(
                "\"{}\" has no layer below it to merge down into.",
                self.layers[index].name
            ));
        }

        let contributing: Vec<usize> = [index - 1, index]
            .into_iter()
            .filter(|&i| self.layers[i].contributes())
            .collect();
        let pixels = crate::composite::flatten_subset(self, &contributing).pixels;
        let new_id = self.next_id;
        self.next_id += 1;
        let merged = Layer {
            id: new_id,
            name: self.layers[index - 1].name.clone(),
            visible: true,
            opacity: 1.0,
            blend_mode: BlendMode::Normal,
            locked: false,
            pixels,
        };
        self.layers.splice(index - 1..=index, [merged]);
        Ok(new_id)
    }

    /// Apply `stroke` along the polyline `points` (document pixel coordinates,
    /// fractional) with the given `radius`, onto layer `id`'s own pixels — not
    /// the composite. A single point paints a dot; consecutive points are
    /// joined into capsule-shaped segments so a stroke drawn from fast pointer
    /// movement has no gaps between samples.
    ///
    /// Coverage from overlapping segments within one call is taken as a
    /// maximum, not summed: a stroke that briefly doubles back on itself (a
    /// tight curve, a corner) must not paint or erase that overlap twice as
    /// hard as the rest of the stroke.
    ///
    /// Returns the pixel rectangle actually touched (the stroke's bounding
    /// box, expanded by `radius` and clamped to the document) — `None` if
    /// nothing was painted (an empty `points`, or a stroke entirely off
    /// canvas). The caller can recomposite just that rect instead of the
    /// whole document.
    pub fn stroke(
        &mut self,
        id: LayerId,
        points: &[(f32, f32)],
        radius: f32,
        stroke: Stroke,
    ) -> Result<Option<Rect>, String> {
        if points.is_empty() {
            return Ok(None);
        }
        if !radius.is_finite() || radius <= 0.0 {
            return Err(format!(
                "Brush radius must be a positive number, got {radius}."
            ));
        }
        if points.iter().any(|(x, y)| !x.is_finite() || !y.is_finite()) {
            return Err("Stroke points must be finite numbers.".to_string());
        }

        let (width, height) = (self.width, self.height);
        // Copied out before borrowing `self.layers` mutably below — `Selection`
        // is small (an enum plus four `u32`s), so this is cheap per call.
        let selection = self.selection;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }

        let mut min_x = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for &(x, y) in points {
            min_x = min_x.min(x);
            max_x = max_x.max(x);
            min_y = min_y.min(y);
            max_y = max_y.max(y);
        }
        // The stroke's bounding box, expanded by the brush radius and clamped
        // to the document — painting off the edge of the canvas is clipped,
        // not an error.
        let x0 = (min_x - radius).floor().max(0.0) as u32;
        let y0 = (min_y - radius).floor().max(0.0) as u32;
        let x1 = ((max_x + radius).ceil().max(0.0) as u32).min(width);
        let y1 = ((max_y + radius).ceil().max(0.0) as u32).min(height);
        if x0 >= x1 || y0 >= y1 {
            return Ok(None);
        }
        let box_width = (x1 - x0) as usize;
        let box_height = (y1 - y0) as usize;

        let segments: Vec<((f32, f32), (f32, f32))> = if points.len() == 1 {
            vec![(points[0], points[0])]
        } else {
            points.windows(2).map(|pair| (pair[0], pair[1])).collect()
        };

        let mut coverage = vec![0f32; box_width * box_height];
        for (a, b) in segments {
            for row in 0..box_height {
                let cy = (y0 as usize + row) as f32 + 0.5;
                for col in 0..box_width {
                    let cx = (x0 as usize + col) as f32 + 0.5;
                    let distance = point_segment_distance(cx, cy, a, b);
                    // A soft 1px edge rather than a hard aliased circle.
                    let mut c = (radius - distance + 0.5).clamp(0.0, 1.0);
                    if let Some(selection) = &selection {
                        if !selection.contains(cx, cy) {
                            c = 0.0;
                        }
                    }
                    let slot = &mut coverage[row * box_width + col];
                    if c > *slot {
                        *slot = c;
                    }
                }
            }
        }

        for row in 0..box_height {
            for col in 0..box_width {
                let c = coverage[row * box_width + col];
                if c <= 0.0 {
                    continue;
                }
                let base = ((y0 as usize + row) * width as usize + (x0 as usize + col)) * CHANNELS;
                match stroke {
                    Stroke::Brush { color } => {
                        let source_alpha = to_unit(color[3]) * c;
                        if source_alpha <= 0.0 {
                            continue;
                        }
                        let dest_alpha = to_unit(layer.pixels[base + 3]);
                        let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
                        let dest = &mut layer.pixels[base..base + CHANNELS];
                        for (channel, &source_byte) in color.iter().enumerate().take(3) {
                            let cs = to_unit(source_byte);
                            let cb = to_unit(dest[channel]);
                            let out = if out_alpha > 0.0 {
                                (source_alpha * cs + dest_alpha * cb * (1.0 - source_alpha))
                                    / out_alpha
                            } else {
                                0.0
                            };
                            dest[channel] = to_byte(out);
                        }
                        dest[3] = to_byte(out_alpha);
                    }
                    Stroke::Eraser => {
                        let dest_alpha = to_unit(layer.pixels[base + 3]);
                        layer.pixels[base + 3] = to_byte(dest_alpha * (1.0 - c));
                    }
                }
            }
        }

        Ok(Some(Rect { x0, y0, x1, y1 }))
    }

    /// Paint Bucket: flood-fill from `(x, y)` with `color` (RGBA8, the same
    /// normal `source-over` blending [`Stroke::Brush`] uses), spreading to
    /// 4-connected neighbours whose colour is within `tolerance` (per
    /// channel, `0..=255`) of the seed pixel's own colour — the default
    /// "Contiguous" fill Photoshop's own Paint Bucket starts from. Confined
    /// to the active selection and blocked by a locked layer, exactly like
    /// [`Document::stroke`]. Returns the filled region's bounding box, or
    /// `None` if the seed pixel itself is excluded by the selection (so
    /// nothing was filled) — not an error, the same way a stroke entirely
    /// outside the selection paints nothing without one either.
    pub fn flood_fill(
        &mut self,
        id: LayerId,
        x: u32,
        y: u32,
        color: [u8; 4],
        tolerance: u8,
    ) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width, self.height);
        if x >= width || y >= height {
            return Err(format!(
                "({x}, {y}) is outside the {width}x{height} canvas."
            ));
        }
        let selection = self.selection;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }

        let in_selection = |px: u32, py: u32| {
            selection
                .map(|s| s.contains(px as f32 + 0.5, py as f32 + 0.5))
                .unwrap_or(true)
        };
        if !in_selection(x, y) {
            return Ok(None);
        }

        let pixel_at = |pixels: &[u8], px: u32, py: u32| -> [u8; 4] {
            let base = (py as usize * width as usize + px as usize) * CHANNELS;
            [
                pixels[base],
                pixels[base + 1],
                pixels[base + 2],
                pixels[base + 3],
            ]
        };
        let matches_seed = |candidate: [u8; 4], seed: [u8; 4]| {
            candidate
                .iter()
                .zip(seed.iter())
                .all(|(&a, &b)| a.abs_diff(b) <= tolerance)
        };

        let seed_color = pixel_at(&layer.pixels, x, y);
        let mut visited = vec![false; width as usize * height as usize];
        visited[(y as usize * width as usize) + x as usize] = true;
        let mut stack = vec![(x, y)];
        let (mut x0, mut x1, mut y0, mut y1) = (x, x, y, y);

        while let Some((px, py)) = stack.pop() {
            let base = (py as usize * width as usize + px as usize) * CHANNELS;
            let dest = pixel_at(&layer.pixels, px, py);
            let source_alpha = to_unit(color[3]);
            let dest_alpha = to_unit(dest[3]);
            let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
            for (channel, &source_byte) in color.iter().enumerate().take(3) {
                let cs = to_unit(source_byte);
                let cb = to_unit(dest[channel]);
                let out = if out_alpha > 0.0 {
                    (source_alpha * cs + dest_alpha * cb * (1.0 - source_alpha)) / out_alpha
                } else {
                    0.0
                };
                layer.pixels[base + channel] = to_byte(out);
            }
            layer.pixels[base + 3] = to_byte(out_alpha);

            x0 = x0.min(px);
            x1 = x1.max(px);
            y0 = y0.min(py);
            y1 = y1.max(py);

            let neighbors = [
                px.checked_sub(1).map(|nx| (nx, py)),
                (px + 1 < width).then_some((px + 1, py)),
                py.checked_sub(1).map(|ny| (px, ny)),
                (py + 1 < height).then_some((px, py + 1)),
            ];
            for (nx, ny) in neighbors.into_iter().flatten() {
                let idx = ny as usize * width as usize + nx as usize;
                if visited[idx] {
                    continue;
                }
                if in_selection(nx, ny) && matches_seed(pixel_at(&layer.pixels, nx, ny), seed_color)
                {
                    visited[idx] = true;
                    stack.push((nx, ny));
                }
            }
        }

        Ok(Some(Rect {
            x0,
            y0,
            x1: x1 + 1,
            y1: y1 + 1,
        }))
    }

    /// Gradient (Linear): blends a linear interpolation between
    /// `start_color` and `end_color`, along the line from `from` to `to`,
    /// over every pixel of layer `id` — or, with an active selection, just
    /// the pixels it includes. Each pixel's position projected onto that
    /// line (clamped to the segment) picks its place in the interpolation;
    /// the blended colour is composited with the same normal `source-over`
    /// math [`Stroke::Brush`] uses. Confined to the active selection and
    /// blocked by a locked layer, exactly like every other paint command.
    /// Errors if the two points coincide — a gradient needs a direction —
    /// or the layer is unknown.
    pub fn gradient_fill(
        &mut self,
        id: LayerId,
        from: (f32, f32),
        to: (f32, f32),
        start_color: [u8; 4],
        end_color: [u8; 4],
    ) -> Result<Option<Rect>, String> {
        let (x0, y0) = from;
        let (x1, y1) = to;
        if ![x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
            return Err("Gradient coordinates must be finite numbers.".to_string());
        }
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len_sq = dx * dx + dy * dy;
        if len_sq <= f32::EPSILON {
            return Err("A gradient needs two distinct points.".to_string());
        }

        let (width, height) = (self.width, self.height);
        let selection = self.selection;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }

        let mut touched: Option<(u32, u32, u32, u32)> = None;
        for py in 0..height {
            for px in 0..width {
                let (cx, cy) = (px as f32 + 0.5, py as f32 + 0.5);
                if let Some(selection) = &selection {
                    if !selection.contains(cx, cy) {
                        continue;
                    }
                }
                let t = (((cx - x0) * dx + (cy - y0) * dy) / len_sq).clamp(0.0, 1.0);

                let base = (py as usize * width as usize + px as usize) * CHANNELS;
                let dest_alpha = to_unit(layer.pixels[base + 3]);
                let source_alpha = lerp(to_unit(start_color[3]), to_unit(end_color[3]), t);
                let out_alpha = source_alpha + dest_alpha * (1.0 - source_alpha);
                for channel in 0..3 {
                    let cs = lerp(
                        to_unit(start_color[channel]),
                        to_unit(end_color[channel]),
                        t,
                    );
                    let cb = to_unit(layer.pixels[base + channel]);
                    let out = if out_alpha > 0.0 {
                        (source_alpha * cs + dest_alpha * cb * (1.0 - source_alpha)) / out_alpha
                    } else {
                        0.0
                    };
                    layer.pixels[base + channel] = to_byte(out);
                }
                layer.pixels[base + 3] = to_byte(out_alpha);

                touched = Some(match touched {
                    None => (px, py, px, py),
                    Some((min_x, min_y, max_x, max_y)) => {
                        (min_x.min(px), min_y.min(py), max_x.max(px), max_y.max(py))
                    }
                });
            }
        }

        Ok(touched.map(|(min_x, min_y, max_x, max_y)| Rect {
            x0: min_x,
            y0: min_y,
            x1: max_x + 1,
            y1: max_y + 1,
        }))
    }

    /// Shared pixel-iteration for whole-layer, per-pixel adjustments
    /// (Invert, Threshold, and any future Image > Adjustments entry that
    /// transforms each pixel independently of its neighbours): visits every
    /// pixel the active selection includes, lets `f` rewrite that pixel's
    /// 4 bytes in place, and reports the touched region the same way
    /// `flood_fill`/`gradient_fill` do. Confines to the selection and
    /// blocks a locked layer — the two guards every other in-place pixel
    /// edit already respects — so a caller never needs to repeat either.
    fn adjust_layer_pixels(
        &mut self,
        id: LayerId,
        mut f: impl FnMut([u8; 4]) -> [u8; 4],
    ) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width, self.height);
        let selection = self.selection;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }

        let mut touched: Option<(u32, u32, u32, u32)> = None;
        for py in 0..height {
            for px in 0..width {
                if let Some(selection) = &selection {
                    if !selection.contains(px as f32 + 0.5, py as f32 + 0.5) {
                        continue;
                    }
                }
                let base = (py as usize * width as usize + px as usize) * CHANNELS;
                let pixel = [
                    layer.pixels[base],
                    layer.pixels[base + 1],
                    layer.pixels[base + 2],
                    layer.pixels[base + 3],
                ];
                layer.pixels[base..base + CHANNELS].copy_from_slice(&f(pixel));

                touched = Some(match touched {
                    None => (px, py, px, py),
                    Some((min_x, min_y, max_x, max_y)) => {
                        (min_x.min(px), min_y.min(py), max_x.max(px), max_y.max(py))
                    }
                });
            }
        }

        Ok(touched.map(|(min_x, min_y, max_x, max_y)| Rect {
            x0: min_x,
            y0: min_y,
            x1: max_x + 1,
            y1: max_y + 1,
        }))
    }

    /// Image > Adjustments > Invert: subtract each RGB channel from 255,
    /// leaving alpha untouched — the same "flip every channel" transform
    /// Photoshop's own Invert applies, with no intermediate curve.
    pub fn invert_colors(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.adjust_layer_pixels(id, |[r, g, b, a]| [255 - r, 255 - g, 255 - b, a])
    }

    /// Image > Adjustments > Threshold: converts a layer to pure black or
    /// white per pixel, based on standard ITU-R BT.601 luma (`0.299R +
    /// 0.587G + 0.114B`, the same weights Photoshop's own Threshold uses)
    /// against `level` — at or above it, that pixel becomes white; below
    /// it, black. Alpha untouched. `level` must be `1..=255`, matching the
    /// range Photoshop's own dialog allows (a level of 0 would make every
    /// pixel white unconditionally, which isn't a meaningful threshold).
    pub fn threshold(&mut self, id: LayerId, level: u8) -> Result<Option<Rect>, String> {
        if level == 0 {
            return Err("Threshold level must be between 1 and 255.".to_string());
        }
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let value = if luma.round() >= level as f32 { 255 } else { 0 };
            [value, value, value, a]
        })
    }

    /// Image > Adjustments > Posterize: quantizes each RGB channel
    /// independently down to `levels` evenly spaced tones (Photoshop's own
    /// dialog defaults to 4), leaving alpha untouched. Each channel value
    /// snaps to the nearest of `levels` steps spanning `0..=255` — `step =
    /// 255 / (levels - 1)`, `output = round(round(value / step) * step)` —
    /// so `levels == 2` reduces a channel to pure black or white and
    /// `levels == 256` would reproduce it exactly (though `levels` is a
    /// `u8`, so 255 is the practical ceiling). `levels` must be at least 2:
    /// one level would collapse every channel to the same flat value,
    /// which isn't what Photoshop's own dialog (minimum 2) considers a
    /// meaningful posterize.
    pub fn posterize(&mut self, id: LayerId, levels: u8) -> Result<Option<Rect>, String> {
        if levels < 2 {
            return Err("Posterize levels must be at least 2.".to_string());
        }
        let step = 255.0 / (levels as f32 - 1.0);
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let quantize = |v: u8| -> u8 { ((v as f32 / step).round() * step).round() as u8 };
            [quantize(r), quantize(g), quantize(b), a]
        })
    }

    /// Image > Adjustments > Brightness/Contrast: a flat per-channel
    /// offset (`brightness`) plus a scale around the mid-grey point 128
    /// (`contrast`), the same "legacy" formula widely used for this
    /// adjustment: `factor = 259*(contrast+255) / (255*(259-contrast))`,
    /// `output = factor*(value-128) + 128 + brightness`, clamped to
    /// `0..=255`. Alpha untouched. Both sliders are clamped to
    /// `-255..=255` before use (Photoshop's own dialog bounds them well
    /// inside that range) rather than erroring on an out-of-range value —
    /// there's no invalid input here, just one that saturates, the same
    /// way a `u8` field would. `i32`, not a signed byte type, since Rust
    /// has no 9-bit integer to hold `-255..=255` exactly.
    pub fn brightness_contrast(
        &mut self,
        id: LayerId,
        brightness: i32,
        contrast: i32,
    ) -> Result<Option<Rect>, String> {
        let brightness = brightness.clamp(-255, 255) as f32;
        let contrast = contrast.clamp(-255, 255) as f32;
        let factor = 259.0 * (contrast + 255.0) / (255.0 * (259.0 - contrast));
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let apply = |v: u8| -> u8 {
                (factor * (v as f32 - 128.0) + 128.0 + brightness).clamp(0.0, 255.0) as u8
            };
            [apply(r), apply(g), apply(b), a]
        })
    }

    /// Image > Adjustments > Hue/Saturation: shifts hue by `hue` degrees,
    /// scales saturation by `1 + saturation/100`, and offsets lightness by
    /// `lightness/100` — each pixel round-trips RGB -> HSL -> (adjusted)
    /// HSL -> RGB. Alpha untouched. A pixel with no saturation (a neutral
    /// grey) has no hue to shift, so it's unaffected by `hue` regardless of
    /// its value — the same as Photoshop's own behaviour on greys. `hue`
    /// clamps to `-180..=180`, `saturation` and `lightness` to `-100..=100`
    /// (Photoshop's own dialog ranges), rather than erroring on an
    /// out-of-range value, the same saturating convention
    /// `brightness_contrast` uses.
    pub fn hue_saturation(
        &mut self,
        id: LayerId,
        hue: i32,
        saturation: i32,
        lightness: i32,
    ) -> Result<Option<Rect>, String> {
        let hue_shift = hue.clamp(-180, 180) as f32;
        let sat_factor = saturation.clamp(-100, 100) as f32 / 100.0;
        let light_offset = lightness.clamp(-100, 100) as f32 / 100.0;
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let h = (h + hue_shift).rem_euclid(360.0);
            let s = (s * (1.0 + sat_factor)).clamp(0.0, 1.0);
            let l = (l + light_offset).clamp(0.0, 1.0);
            let (r, g, b) = hsl_to_rgb(h, s, l);
            [r, g, b, a]
        })
    }

    /// Image > Adjustments > Black & White: desaturates a layer to
    /// greyscale using the same ITU-R BT.601 luma weights (`0.299R +
    /// 0.587G + 0.114B`) [`Self::threshold`] uses, setting all three RGB
    /// channels to that luma rather than thresholding it to pure black or
    /// white. Alpha untouched. Photoshop's own Black & White dialog offers
    /// six colour-range sliders (reds, yellows, greens, cyans, blues,
    /// magentas) for a custom weighting; this uses one fixed, standard
    /// weighting instead — a deliberate scope cut, the same kind Paint
    /// Bucket's fixed tolerance and Posterize's UI-capped slider already
    /// made in this project, not an oversight.
    pub fn black_and_white(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.adjust_layer_pixels(id, |[r, g, b, a]| {
            let luma = to_byte(0.299 * to_unit(r) + 0.587 * to_unit(g) + 0.114 * to_unit(b));
            [luma, luma, luma, a]
        })
    }

    /// Image > Adjustments > Vibrance: like [`Self::hue_saturation`]'s
    /// saturation slider, but weighted to protect already-saturated
    /// pixels (and, not incidentally, skin tones — usually the least
    /// saturated colours in a photo) from clipping to a garish maximum.
    /// `vibrance` scales saturation by `1 - current_saturation`, so a
    /// pixel that's already fully saturated gets no boost at all while a
    /// near-grey pixel gets the full effect; `saturation` then applies
    /// uniformly on top, the same linear scale `hue_saturation` uses.
    /// Both `-100..=100`, matching Photoshop's own dialog range, and
    /// clamped rather than erroring on an out-of-range value.
    pub fn vibrance(
        &mut self,
        id: LayerId,
        vibrance: i32,
        saturation: i32,
    ) -> Result<Option<Rect>, String> {
        let vibrance_factor = vibrance.clamp(-100, 100) as f32 / 100.0;
        let sat_factor = saturation.clamp(-100, 100) as f32 / 100.0;
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let (h, s, l) = rgb_to_hsl(r, g, b);
            let s = (s + vibrance_factor * (1.0 - s)).clamp(0.0, 1.0);
            let s = (s * (1.0 + sat_factor)).clamp(0.0, 1.0);
            let (r, g, b) = hsl_to_rgb(h, s, l);
            [r, g, b, a]
        })
    }

    /// Image > Adjustments > Photo Filter: tints a layer toward `color` by
    /// blending each pixel's RGB toward it by `density` percent
    /// (`0..=100` — clamped, not erroring, above 100, since Photoshop's
    /// own slider tops out there too). Alpha untouched. Photoshop's own
    /// dialog also offers a "Preserve Luminosity" checkbox that
    /// renormalizes brightness after tinting; this omits it — a
    /// deliberate scope cut, the same kind Black & White's single fixed
    /// luma weighting already made in this project.
    pub fn photo_filter(
        &mut self,
        id: LayerId,
        color: [u8; 3],
        density: u8,
    ) -> Result<Option<Rect>, String> {
        let t = density.min(100) as f32 / 100.0;
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let blend = |c: u8, f: u8| to_byte(lerp(to_unit(c), to_unit(f), t));
            [
                blend(r, color[0]),
                blend(g, color[1]),
                blend(b, color[2]),
                a,
            ]
        })
    }

    /// Image > Adjustments > Exposure: the same three-control model
    /// Photoshop's own dialog uses, applied per channel to a `0.0..=1.0`
    /// working value — `exposure` (a stop count, `2^exposure` multiplies
    /// the value), `offset` (added after exposure, shifts black), and
    /// `gamma` (`value.powf(1.0 / gamma)`, curving the midtones) — each
    /// clamped rather than erroring on an out-of-range value:
    /// `exposure` to `-2000..=2000` (hundredths of a stop, `±20.00`,
    /// Photoshop's own range), `offset` to `-50..=50` (hundredths,
    /// `±0.50`), `gamma` to `1..=999` (hundredths, `0.01..=9.99` — never
    /// zero, which would make `1.0 / gamma` divide by zero). The value is
    /// floored at zero before the gamma power (a negative base raised to
    /// a fractional exponent is undefined) and clamped to `0.0..=1.0`
    /// only at the very end, so a highlight exposure pushes past white
    /// exactly the way it would on a real sensor before finally clipping.
    /// Alpha untouched.
    pub fn exposure(
        &mut self,
        id: LayerId,
        exposure: i32,
        offset: i32,
        gamma: i32,
    ) -> Result<Option<Rect>, String> {
        let factor = 2f32.powf(exposure.clamp(-2000, 2000) as f32 / 100.0);
        let offset = offset.clamp(-50, 50) as f32 / 100.0;
        let exponent = 100.0 / gamma.clamp(1, 999) as f32;
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let apply = |c: u8| {
                let v = (to_unit(c) * factor + offset).max(0.0).powf(exponent);
                to_byte(v.clamp(0.0, 1.0))
            };
            [apply(r), apply(g), apply(b), a]
        })
    }

    /// Image > Adjustments > Gradient Map: replaces each pixel's colour
    /// with a point along the line from `shadow_color` to
    /// `highlight_color`, picked by that pixel's own ITU-R BT.601 luma
    /// (`0.299R + 0.587G + 0.114B`, the same weighting `threshold` and
    /// `black_and_white` already use) — a shadow-luma pixel lands on
    /// `shadow_color`, a highlight-luma pixel on `highlight_color`, and
    /// everything between blends smoothly. Photoshop's own dialog accepts
    /// an arbitrary multi-stop gradient preset; this always maps to a
    /// straight two-colour line between the shadow and highlight colours,
    /// the same two-stop-gradient scope [`Self::gradient_fill`] already
    /// uses for its own gradients — a deliberate scope cut, not an
    /// oversight. Alpha untouched.
    pub fn gradient_map(
        &mut self,
        id: LayerId,
        shadow_color: [u8; 3],
        highlight_color: [u8; 3],
    ) -> Result<Option<Rect>, String> {
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let luma = 0.299 * to_unit(r) + 0.587 * to_unit(g) + 0.114 * to_unit(b);
            let map = |channel: usize| {
                to_byte(lerp(
                    to_unit(shadow_color[channel]),
                    to_unit(highlight_color[channel]),
                    luma,
                ))
            };
            [map(0), map(1), map(2), a]
        })
    }

    /// Image > Adjustments > Channel Mixer: builds each output channel as
    /// a weighted sum of all three input channels plus a constant —
    /// `output_c = r*matrix[c][0] + g*matrix[c][1] + b*matrix[c][2] +
    /// matrix[c][3]`, one row of `matrix` per output channel (`R`, `G`,
    /// `B` in that order), clamped to `0..=255`. The three per-channel
    /// coefficients are percentages (`-200..=200`, i.e. `-2.00..=2.00`,
    /// Photoshop's own range) and the constant is a direct `-200..=200`
    /// byte-scale offset — both clamped rather than erroring on an
    /// out-of-range value. The identity matrix (`[[100,0,0,0],
    /// [0,100,0,0], [0,0,100,0]]`) is a no-op; swapping a row's own
    /// 100-weight onto a different input channel swaps channels outright,
    /// and negative weights invert a channel's contribution — this one
    /// command subsumes plain channel-swap and channel-invert tricks
    /// Photoshop users often reach for Channel Mixer to do. Alpha
    /// untouched.
    pub fn channel_mixer(
        &mut self,
        id: LayerId,
        matrix: [[i32; 4]; 3],
    ) -> Result<Option<Rect>, String> {
        let matrix: Vec<[f32; 4]> = matrix
            .iter()
            .map(|row| {
                [
                    row[0].clamp(-200, 200) as f32 / 100.0,
                    row[1].clamp(-200, 200) as f32 / 100.0,
                    row[2].clamp(-200, 200) as f32 / 100.0,
                    row[3].clamp(-200, 200) as f32 / 255.0,
                ]
            })
            .collect();
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let (ru, gu, bu) = (to_unit(r), to_unit(g), to_unit(b));
            let mix = |row: &[f32; 4]| to_byte(ru * row[0] + gu * row[1] + bu * row[2] + row[3]);
            [mix(&matrix[0]), mix(&matrix[1]), mix(&matrix[2]), a]
        })
    }

    /// Image > Adjustments > Levels: the classic histogram remap, applied
    /// identically to all three RGB channels (Photoshop's own dialog also
    /// lets you pick one channel at a time via a dropdown; this always
    /// applies to the RGB composite channel — a deliberate scope cut, the
    /// same kind Black & White's single fixed luma weighting already made
    /// in this project). Each channel value goes through three steps:
    /// normalize against the input black/white points (`(value -
    /// input_black) / (input_white - input_black)`, clamped to
    /// `0.0..=1.0`), apply a gamma curve (`normalized.powf(1.0 / gamma)`),
    /// then remap onto the output black/white points (`output_black +
    /// corrected * (output_white - output_black)`). `input_black`,
    /// `input_white`, `output_black`, `output_white` are all `0..=255`;
    /// `gamma` is hundredths (`1..=999`, i.e. `0.01..=9.99`, Photoshop's
    /// own range). `input_white` is clamped to be at least one greater
    /// than `input_black` — a zero-width input range has no meaningful
    /// normalization — rather than erroring or dividing by zero. Alpha
    /// untouched.
    #[allow(clippy::too_many_arguments)]
    pub fn levels(
        &mut self,
        id: LayerId,
        input_black: u8,
        input_white: u8,
        gamma: i32,
        output_black: u8,
        output_white: u8,
    ) -> Result<Option<Rect>, String> {
        let input_black = input_black as f32;
        let input_white = (input_white as f32).max(input_black + 1.0);
        let exponent = 100.0 / gamma.clamp(1, 999) as f32;
        let output_black = to_unit(output_black);
        let output_white = to_unit(output_white);
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let apply = |c: u8| {
                let normalized =
                    ((c as f32 - input_black) / (input_white - input_black)).clamp(0.0, 1.0);
                let corrected = normalized.powf(exponent);
                to_byte(output_black + corrected * (output_white - output_black))
            };
            [apply(r), apply(g), apply(b), a]
        })
    }

    /// Image > Adjustments > Curves: a tone curve applied identically to
    /// all three RGB channels (like [`Document::levels`], always the RGB
    /// composite channel rather than Photoshop's own per-channel
    /// Red/Green/Blue dropdown — a deliberate scope cut). Photoshop's own
    /// dialog is an interactive editor with an arbitrary number of
    /// draggable points connected by a smooth spline; here the curve is
    /// fixed to five control points at evenly spaced input positions (`0`,
    /// `64`, `128`, `192`, `255`) whose five output values are each
    /// independently adjustable, connected by straight line segments
    /// rather than a spline — another deliberate scope cut, invisible for
    /// modest adjustments and only really apparent on extreme ones, in
    /// exchange for a vastly simpler and more directly testable
    /// implementation. `points[i]` is the output value for input `XS[i]`;
    /// at the identity mapping (`[0, 64, 128, 192, 255]`) every input
    /// value reproduces exactly, since each segment's output span exactly
    /// matches its input span. Alpha untouched.
    pub fn curves(&mut self, id: LayerId, points: [u8; 5]) -> Result<Option<Rect>, String> {
        const XS: [f32; 5] = [0.0, 64.0, 128.0, 192.0, 255.0];
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let apply = |c: u8| {
                let x = c as f32;
                let seg = ((x / 64.0) as usize).min(3);
                let (x0, x1) = (XS[seg], XS[seg + 1]);
                let (y0, y1) = (points[seg] as f32, points[seg + 1] as f32);
                let t = (x - x0) / (x1 - x0);
                (y0 + t * (y1 - y0)).round().clamp(0.0, 255.0) as u8
            };
            [apply(r), apply(g), apply(b), a]
        })
    }

    /// Image > Adjustments > Color Balance: shifts each RGB channel by an
    /// amount that depends on how shadow-like, midtone-like, or
    /// highlight-like a pixel's luminance is. Photoshop's own version
    /// blends its three tonal ranges with a proprietary lookup curve and
    /// offers a "Preserve Luminosity" option that re-normalizes lightness
    /// after the shift; both are deliberate scope cuts here (consistent
    /// with Photo Filter already omitting Preserve Luminosity), in favour
    /// of a simple, fully documented, and exactly testable blending
    /// scheme: BT.601 luma (`0.0..=255.0`, the same weighting Threshold
    /// and Black & White already use) is split into shadow/midtone/
    /// highlight weights with two linear ramps that never overlap and
    /// always sum to exactly `1.0` — `shadow_weight = clamp((127 - luma)
    /// / 127, 0, 1)` (`1.0` at luma `0`, `0.0` from luma `127` up),
    /// `highlight_weight = clamp((luma - 128) / 127, 0, 1)` (`0.0` up to
    /// luma `128`, `1.0` at luma `255`), and `midtone_weight = 1.0 -
    /// shadow_weight - highlight_weight` (exactly `1.0` at luma `127` and
    /// `128`, tapering to `0.0` at both ends). Each range's three
    /// per-channel sliders (`-100..=100`, Photoshop's own range,
    /// cyan-red/magenta-green/yellow-blue mapping directly onto
    /// R/G/B) are blended by that pixel's three weights and added
    /// directly to the channel byte, then clamped. No Preserve
    /// Luminosity. Alpha untouched.
    pub fn color_balance(
        &mut self,
        id: LayerId,
        shadows: [i32; 3],
        midtones: [i32; 3],
        highlights: [i32; 3],
    ) -> Result<Option<Rect>, String> {
        let shadows = shadows.map(|v| v.clamp(-100, 100) as f32);
        let midtones = midtones.map(|v| v.clamp(-100, 100) as f32);
        let highlights = highlights.map(|v| v.clamp(-100, 100) as f32);
        self.adjust_layer_pixels(id, move |[r, g, b, a]| {
            let luma = 0.299 * r as f32 + 0.587 * g as f32 + 0.114 * b as f32;
            let shadow_w = ((127.0 - luma) / 127.0).clamp(0.0, 1.0);
            let highlight_w = ((luma - 128.0) / 127.0).clamp(0.0, 1.0);
            let midtone_w = 1.0 - shadow_w - highlight_w;
            let apply = |v: u8, c: usize| {
                let shift =
                    shadow_w * shadows[c] + midtone_w * midtones[c] + highlight_w * highlights[c];
                (v as f32 + shift).round().clamp(0.0, 255.0) as u8
            };
            [apply(r, 0), apply(g, 1), apply(b, 2), a]
        })
    }
}

/// `(r, g, b)` (each `0..=255`) to `(hue, saturation, lightness)`
/// (`hue` in `0.0..360.0` degrees, `saturation`/`lightness` in `0.0..=1.0`).
/// A pixel with no colour (max channel == min channel) is defined as
/// hue `0.0`, saturation `0.0` — there is no hue to report, and reporting
/// one would make an achromatic pixel spuriously sensitive to a hue shift.
fn rgb_to_hsl(r: u8, g: u8, b: u8) -> (f32, f32, f32) {
    let (r, g, b) = (to_unit(r), to_unit(g), to_unit(b));
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let l = (max + min) / 2.0;
    let d = max - min;
    if d <= f32::EPSILON {
        return (0.0, 0.0, l);
    }
    let s = if l > 0.5 {
        d / (2.0 - max - min)
    } else {
        d / (max + min)
    };
    let h = if max == r {
        ((g - b) / d).rem_euclid(6.0)
    } else if max == g {
        (b - r) / d + 2.0
    } else {
        (r - g) / d + 4.0
    };
    (h * 60.0, s, l)
}

/// The inverse of [`rgb_to_hsl`]: `hue` in `0.0..360.0` degrees,
/// `saturation`/`lightness` in `0.0..=1.0`, back to `(r, g, b)` bytes.
fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    if s <= 0.0 {
        let v = to_byte(l);
        return (v, v, v);
    }
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hp = h / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = if hp < 1.0 {
        (c, x, 0.0)
    } else if hp < 2.0 {
        (x, c, 0.0)
    } else if hp < 3.0 {
        (0.0, c, x)
    } else if hp < 4.0 {
        (0.0, x, c)
    } else if hp < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    (to_byte(r1 + m), to_byte(g1 + m), to_byte(b1 + m))
}

/// Linear interpolation from `a` to `b` at `t` (`0.0..=1.0`).
fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

/// A tool [`Document::stroke`] applies.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Stroke {
    /// Paints `color` (RGBA8) over the layer with normal, `source-over`
    /// blending — the same math the compositor uses to stack layers, applied
    /// here to a layer's own pixels instead of the accumulated backdrop.
    Brush { color: [u8; 4] },
    /// Multiplies existing alpha down toward zero; colour is left alone; a
    /// fully transparent pixel's colour is invisible and not otherwise
    /// meaningful.
    Eraser,
}

/// Shortest distance from `(px, py)` to the segment `a`-`b`.
fn point_segment_distance(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        return ((px - a.0).powi(2) + (py - a.1).powi(2)).sqrt();
    }
    let t = (((px - a.0) * dx + (py - a.1) * dy) / len_sq).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((px - cx).powi(2) + (py - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `width` x `height` buffer where every pixel is `rgba`.
    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * CHANNELS)
            .collect()
    }

    fn doc_with_one_layer() -> (Document, LayerId) {
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc
            .add_layer("base", &solid(2, 2, [10, 20, 30, 255]), 2, 2)
            .unwrap();
        (doc, id)
    }

    #[test]
    fn a_new_document_has_no_layers() {
        let doc = Document::new(4, 3).unwrap();
        assert!(doc.layers().is_empty());
        assert_eq!((doc.width(), doc.height()), (4, 3));
        assert_eq!(doc.buffer_len(), 4 * 3 * 4);
    }

    #[test]
    fn zero_sized_documents_are_rejected() {
        assert!(Document::new(0, 5).is_err());
        assert!(Document::new(5, 0).is_err());
    }

    #[test]
    fn layers_default_to_visible_opaque_and_normal() {
        let (doc, id) = doc_with_one_layer();
        let layer = &doc.layers()[0];
        assert_eq!(layer.id, id);
        assert_eq!(layer.name, "base");
        assert!(layer.visible);
        assert_eq!(layer.opacity, 1.0);
        assert_eq!(layer.blend_mode, BlendMode::Normal);
        assert!(layer.contributes());
    }

    #[test]
    fn layer_ids_are_unique_even_after_removal() {
        let mut doc = Document::new(1, 1).unwrap();
        let first = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        doc.remove_layer(first).unwrap();
        let second = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn a_mismatched_source_buffer_is_rejected() {
        let mut doc = Document::new(2, 2).unwrap();
        let err = doc.add_layer("bad", &[0, 0, 0], 2, 2).unwrap_err();
        assert!(err.contains("Expected 16 bytes"), "{err}");
    }

    #[test]
    fn a_smaller_source_is_pasted_at_the_origin_and_padded() {
        let mut doc = Document::new(2, 2).unwrap();
        doc.add_layer("small", &solid(1, 1, [9, 9, 9, 255]), 1, 1)
            .unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(pixels.len(), 16);
        assert_eq!(&pixels[0..4], &[9, 9, 9, 255]); // top-left got the pixel
        assert_eq!(&pixels[4..16], &[0; 12]); // the rest is transparent
    }

    #[test]
    fn a_larger_source_is_clipped() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("big", &solid(2, 2, [7, 7, 7, 255]), 2, 2)
            .unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(pixels.len(), 4);
        assert_eq!(pixels, &[7, 7, 7, 255]);
    }

    #[test]
    fn a_wider_source_does_not_bleed_into_the_next_row() {
        // A 3x2 source into a 2x2 document: row 1 of the source must land on row 1
        // of the layer, not continue filling row 0.
        let mut doc = Document::new(2, 2).unwrap();
        let mut source = solid(3, 2, [1, 1, 1, 255]);
        // Mark the first pixel of the source's second row.
        source[3 * CHANNELS..3 * CHANNELS + CHANNELS].copy_from_slice(&[5, 5, 5, 255]);
        doc.add_layer("wide", &source, 3, 2).unwrap();

        let pixels = &doc.layers()[0].pixels;
        assert_eq!(&pixels[0..4], &[1, 1, 1, 255]);
        assert_eq!(&pixels[8..12], &[5, 5, 5, 255]); // start of layer row 1
    }

    #[test]
    fn a_solid_color_layer_fills_the_whole_canvas() {
        let mut doc = Document::new(3, 2).unwrap();
        let id = doc.add_solid_color_layer("Color Fill 1", [10, 20, 30, 255]);
        for y in 0..2 {
            for x in 0..3 {
                assert_eq!(pixel(&doc, id, x, y), [10, 20, 30, 255]);
            }
        }
    }

    #[test]
    fn a_solid_color_layer_is_named_and_stacked_on_top() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("base", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let id = doc.add_solid_color_layer("Color Fill 1", [255, 0, 0, 255]);
        let top = doc.layers().last().unwrap();
        assert_eq!(top.id, id);
        assert_eq!(top.name, "Color Fill 1");
    }

    #[test]
    fn a_solid_color_layer_can_have_transparency() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_solid_color_layer("Color Fill 1", [0, 0, 0, 128]);
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 128]);
    }

    #[test]
    fn a_gradient_layer_interpolates_along_the_canvas_diagonal() {
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc.add_gradient_layer("Gradient Fill 1", [0, 0, 0, 255], [255, 255, 255, 255]);
        // Pixel centres (0.5, 0.5) and (1.5, 1.5) project to t=0.25 and
        // t=0.75 along the (0,0)-(2,2) diagonal - the same fractions (and
        // so the same byte values) gradient_fill_interpolates_along_the_line
        // already established for a horizontal gradient.
        assert_eq!(pixel(&doc, id, 0, 0), [64, 64, 64, 255]);
        assert_eq!(pixel(&doc, id, 1, 1), [191, 191, 191, 255]);
    }

    #[test]
    fn a_gradient_layer_is_named_and_stacked_on_top() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("base", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let id = doc.add_gradient_layer("Gradient Fill 1", [0, 0, 0, 255], [255, 255, 255, 255]);
        let top = doc.layers().last().unwrap();
        assert_eq!(top.id, id);
        assert_eq!(top.name, "Gradient Fill 1");
    }

    #[test]
    fn a_gradient_layer_honours_start_and_end_alpha() {
        // The gradient is painted onto a brand new, fully transparent
        // layer, so a fully transparent start/end colour leaves the layer
        // fully transparent - there's nothing underneath on the new layer
        // itself to show through.
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc.add_gradient_layer("Gradient Fill 1", [0, 0, 0, 0], [255, 255, 255, 0]);
        assert_eq!(pixel(&doc, id, 0, 0)[3], 0);
        assert_eq!(pixel(&doc, id, 1, 1)[3], 0);
    }

    #[test]
    fn visibility_opacity_and_blend_mode_round_trip() {
        let (mut doc, id) = doc_with_one_layer();

        doc.set_visible(id, false).unwrap();
        doc.set_opacity(id, 0.25).unwrap();
        doc.set_blend_mode(id, BlendMode::Multiply).unwrap();

        let view = &doc.view().layers[0];
        assert!(!view.visible);
        assert_eq!(view.opacity, 0.25);
        assert_eq!(view.blend_mode, BlendMode::Multiply);
        assert!(!doc.layers()[0].contributes());
    }

    #[test]
    fn layers_default_to_unlocked_and_locking_round_trips() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(!doc.layers()[0].locked);

        doc.set_locked(id, true).unwrap();
        assert!(doc.view().layers[0].locked);
        doc.set_locked(id, false).unwrap();
        assert!(!doc.view().layers[0].locked);
    }

    #[test]
    fn a_locked_layer_rejects_a_stroke() {
        let (mut doc, id) = transparent_doc(4);
        doc.set_locked(id, true).unwrap();
        let err = doc
            .stroke(
                id,
                &[(2.0, 2.0)],
                1.0,
                Stroke::Brush {
                    color: [255, 0, 0, 255],
                },
            )
            .unwrap_err();
        assert!(err.contains("locked"), "{err}");
        // Untouched — the stroke was rejected outright, not clipped to nothing.
        assert_eq!(pixel(&doc, id, 2, 2), [0, 0, 0, 0]);
    }

    #[test]
    fn unlocking_a_layer_allows_a_stroke_again() {
        let (mut doc, id) = transparent_doc(9);
        doc.set_locked(id, true).unwrap();
        doc.set_locked(id, false).unwrap();
        doc.stroke(
            id,
            &[(4.0, 4.0)],
            5.0,
            Stroke::Brush {
                color: [255, 0, 0, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 4, 4), [255, 0, 0, 255]);
    }

    #[test]
    fn rasterizing_an_existing_layer_is_a_no_op() {
        let (mut doc, id) = doc_with_one_layer();
        let before = doc.view();
        doc.rasterize_layer(id).unwrap();
        assert_eq!(doc.view(), before);
    }

    #[test]
    fn rasterizing_a_locked_layer_still_succeeds() {
        // Rasterize never touches pixels, so the pixel lock is irrelevant to
        // it - unlike every paint/adjustment command, which rejects a
        // locked layer outright.
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        doc.rasterize_layer(id).unwrap();
    }

    #[test]
    fn rasterizing_an_unknown_layer_is_an_error() {
        let (mut doc, _id) = doc_with_one_layer();
        assert!(doc.rasterize_layer(999).is_err());
    }

    #[test]
    fn flip_horizontal_mirrors_pixels_left_to_right() {
        let mut doc = Document::new(3, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([10, 10, 10, 255]); // x=0
        pixels.extend([20, 20, 20, 255]); // x=1 (odd width: no partner, stays put)
        pixels.extend([30, 30, 30, 255]); // x=2
        let id = doc.add_layer("row", &pixels, 3, 1).unwrap();

        doc.flip_layer_horizontal(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [30, 30, 30, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [20, 20, 20, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [10, 10, 10, 255]);
    }

    #[test]
    fn flip_horizontal_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.flip_layer_horizontal(id).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn flip_horizontal_on_an_unknown_layer_is_an_error() {
        let (mut doc, _id) = doc_with_one_layer();
        assert!(doc.flip_layer_horizontal(999).is_err());
    }

    #[test]
    fn flip_vertical_mirrors_pixels_top_to_bottom() {
        let mut doc = Document::new(1, 3).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([10, 10, 10, 255]); // y=0
        pixels.extend([20, 20, 20, 255]); // y=1 (odd height: no partner)
        pixels.extend([30, 30, 30, 255]); // y=2
        let id = doc.add_layer("col", &pixels, 1, 3).unwrap();

        doc.flip_layer_vertical(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [30, 30, 30, 255]);
        assert_eq!(pixel(&doc, id, 0, 1), [20, 20, 20, 255]);
        assert_eq!(pixel(&doc, id, 0, 2), [10, 10, 10, 255]);
    }

    #[test]
    fn flip_vertical_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.flip_layer_vertical(id).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn flip_vertical_on_an_unknown_layer_is_an_error() {
        let (mut doc, _id) = doc_with_one_layer();
        assert!(doc.flip_layer_vertical(999).is_err());
    }

    #[test]
    fn rotate_180_maps_each_pixel_to_the_opposite_corner() {
        let mut doc = Document::new(2, 2).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([1, 0, 0, 255]); // (0,0) = A
        pixels.extend([2, 0, 0, 255]); // (1,0) = B
        pixels.extend([3, 0, 0, 255]); // (0,1) = C
        pixels.extend([4, 0, 0, 255]); // (1,1) = D
        let id = doc.add_layer("square", &pixels, 2, 2).unwrap();

        doc.rotate_layer_180(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [4, 0, 0, 255]); // was D
        assert_eq!(pixel(&doc, id, 1, 0), [3, 0, 0, 255]); // was C
        assert_eq!(pixel(&doc, id, 0, 1), [2, 0, 0, 255]); // was B
        assert_eq!(pixel(&doc, id, 1, 1), [1, 0, 0, 255]); // was A
    }

    #[test]
    fn rotate_180_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.rotate_layer_180(id).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn rotate_180_on_an_unknown_layer_is_an_error() {
        let (mut doc, _id) = doc_with_one_layer();
        assert!(doc.rotate_layer_180(999).is_err());
    }

    #[test]
    fn rotate_document_90_clockwise_matches_the_hand_derived_example() {
        // A 2-wide x 3-tall grid:
        //   A B
        //   C D
        //   E F
        // rotated 90 clockwise becomes 3-wide x 2-tall:
        //   E C A
        //   F D B
        let mut doc = Document::new(2, 3).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([1, 0, 0, 255]); // A
        pixels.extend([2, 0, 0, 255]); // B
        pixels.extend([3, 0, 0, 255]); // C
        pixels.extend([4, 0, 0, 255]); // D
        pixels.extend([5, 0, 0, 255]); // E
        pixels.extend([6, 0, 0, 255]); // F
        let id = doc.add_layer("grid", &pixels, 2, 3).unwrap();

        doc.rotate_document_90(true);

        assert_eq!((doc.width(), doc.height()), (3, 2));
        assert_eq!(pixel(&doc, id, 0, 0), [5, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [3, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [1, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 0, 1), [6, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 1), [4, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 2, 1), [2, 0, 0, 255]);
    }

    #[test]
    fn rotate_document_90_counter_clockwise_matches_the_hand_derived_example() {
        // The same A..F grid rotated 90 counter-clockwise instead becomes:
        //   B D F
        //   A C E
        let mut doc = Document::new(2, 3).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([1, 0, 0, 255]); // A
        pixels.extend([2, 0, 0, 255]); // B
        pixels.extend([3, 0, 0, 255]); // C
        pixels.extend([4, 0, 0, 255]); // D
        pixels.extend([5, 0, 0, 255]); // E
        pixels.extend([6, 0, 0, 255]); // F
        let id = doc.add_layer("grid", &pixels, 2, 3).unwrap();

        doc.rotate_document_90(false);

        assert_eq!((doc.width(), doc.height()), (3, 2));
        assert_eq!(pixel(&doc, id, 0, 0), [2, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [4, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [6, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 0, 1), [1, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 1), [3, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 2, 1), [5, 0, 0, 255]);
    }

    #[test]
    fn rotating_90_twice_clockwise_and_twice_counter_clockwise_returns_to_the_original() {
        let mut doc = Document::new(2, 3).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([1, 0, 0, 255]);
        pixels.extend([2, 0, 0, 255]);
        pixels.extend([3, 0, 0, 255]);
        pixels.extend([4, 0, 0, 255]);
        pixels.extend([5, 0, 0, 255]);
        pixels.extend([6, 0, 0, 255]);
        let id = doc.add_layer("grid", &pixels, 2, 3).unwrap();

        doc.rotate_document_90(true);
        doc.rotate_document_90(true);
        doc.rotate_document_90(true);
        doc.rotate_document_90(true);
        assert_eq!((doc.width(), doc.height()), (2, 3));
        for (i, expected) in pixels.chunks_exact(4).enumerate() {
            let (x, y) = ((i % 2) as u32, (i / 2) as u32);
            assert_eq!(pixel(&doc, id, x, y), expected);
        }

        doc.rotate_document_90(false);
        doc.rotate_document_90(false);
        doc.rotate_document_90(false);
        doc.rotate_document_90(false);
        assert_eq!((doc.width(), doc.height()), (2, 3));
        for (i, expected) in pixels.chunks_exact(4).enumerate() {
            let (x, y) = ((i % 2) as u32, (i / 2) as u32);
            assert_eq!(pixel(&doc, id, x, y), expected);
        }
    }

    #[test]
    fn rotating_90_swaps_the_document_dimensions_even_with_no_layers() {
        let mut doc = Document::new(4, 7).unwrap();
        doc.rotate_document_90(true);
        assert_eq!((doc.width(), doc.height()), (7, 4));
    }

    #[test]
    fn rotating_90_clears_the_selection_and_reselect_history() {
        let mut doc = Document::new(4, 4).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 2.0).unwrap();
        doc.deselect();
        doc.select_rectangle(0.0, 0.0, 2.0, 2.0).unwrap();

        doc.rotate_document_90(true);

        assert_eq!(doc.selection(), None);
        assert!(doc.reselect().is_err());
    }

    #[test]
    fn copy_without_a_selection_captures_the_whole_layer() {
        let (doc, id) = doc_with_one_layer();
        let clipboard = doc.copy(id).unwrap();
        assert_eq!((clipboard.width, clipboard.height), (2, 2));
        assert_eq!(
            clipboard.origin,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2
            }
        );
        assert_eq!(clipboard.pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn copy_with_a_rectangular_selection_captures_only_that_region() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [1, 2, 3, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let clipboard = doc.copy(id).unwrap();

        assert_eq!((clipboard.width, clipboard.height), (2, 2));
        assert_eq!(
            clipboard.origin,
            Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 3
            }
        );
        assert_eq!(clipboard.pixels, solid(2, 2, [1, 2, 3, 255]));
    }

    #[test]
    fn copy_masks_pixels_outside_a_non_rectangular_selection() {
        // A 4x4 canvas with an ellipse inscribed in the whole selection
        // bounds: hand-derived against `shape_contains`'s own math
        // (centre (2,2), radii (2,2)) — the four corner pixel-centres land
        // outside the ellipse, the rest of the border band and the whole
        // interior land inside it.
        let mut doc = Document::new(4, 4).unwrap();
        let id = doc
            .add_layer("base", &solid(4, 4, [5, 6, 7, 255]), 4, 4)
            .unwrap();
        doc.select_ellipse(0.0, 0.0, 4.0, 4.0).unwrap();

        let clipboard = doc.copy(id).unwrap();
        assert_eq!((clipboard.width, clipboard.height), (4, 4));

        let idx = |x: usize, y: usize| (y * 4 + x) * 4;
        for &(x, y) in &[(0, 0), (3, 0), (0, 3), (3, 3)] {
            assert_eq!(&clipboard.pixels[idx(x, y)..idx(x, y) + 4], &[0, 0, 0, 0]);
        }
        for &(x, y) in &[(1, 0), (2, 0), (0, 1), (1, 1), (2, 2), (1, 3), (2, 3)] {
            assert_eq!(&clipboard.pixels[idx(x, y)..idx(x, y) + 4], &[5, 6, 7, 255]);
        }
    }

    #[test]
    fn copy_succeeds_on_a_locked_layer() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        assert!(doc.copy(id).is_ok());
    }

    #[test]
    fn copy_errors_on_an_unknown_layer() {
        let doc = Document::new(2, 2).unwrap();
        assert!(doc.copy(999).is_err());
    }

    #[test]
    fn cut_clears_only_the_selected_pixels_and_reports_that_rect_dirty() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [4, 5, 6, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let (clipboard, rect) = doc.cut(id).unwrap();

        assert_eq!(
            rect,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 3
            })
        );
        assert_eq!(clipboard.pixels, solid(2, 2, [4, 5, 6, 255]));

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Untouched: outside the selected bottom-right 2x2.
        assert_eq!(&pixels[idx(0, 0)..idx(0, 0) + 4], &[4, 5, 6, 255]);
        assert_eq!(&pixels[idx(2, 0)..idx(2, 0) + 4], &[4, 5, 6, 255]);
        assert_eq!(&pixels[idx(0, 2)..idx(0, 2) + 4], &[4, 5, 6, 255]);
        // Cleared: the selected region itself.
        assert_eq!(&pixels[idx(1, 1)..idx(1, 1) + 4], &[0, 0, 0, 0]);
        assert_eq!(&pixels[idx(2, 2)..idx(2, 2) + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn cut_errors_on_a_locked_layer_and_leaves_it_untouched() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();

        let err = doc.cut(id).unwrap_err();

        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn cut_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.cut(999).is_err());
    }

    #[test]
    fn paste_adds_a_new_top_layer_at_the_original_coordinates() {
        let mut doc = Document::new(3, 3).unwrap();
        doc.add_layer("base", &solid(3, 3, [1, 1, 1, 255]), 3, 3)
            .unwrap();
        let id = doc
            .add_layer("subject", &solid(3, 3, [9, 8, 7, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();
        let clipboard = doc.copy(id).unwrap();

        let pasted = doc.paste(&clipboard, "Pasted");

        assert_eq!(doc.layers().len(), 3);
        let layer = doc.layers().iter().find(|l| l.id == pasted).unwrap();
        assert_eq!(layer.name, "Pasted");
        assert_eq!(layer.pixels.len(), 3 * 3 * 4);
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Outside the copied region: transparent, not the base layer's colour
        // — a pasted layer starts empty everywhere the clipboard didn't cover.
        assert_eq!(&layer.pixels[idx(0, 0)..idx(0, 0) + 4], &[0, 0, 0, 0]);
        assert_eq!(&layer.pixels[idx(1, 1)..idx(1, 1) + 4], &[9, 8, 7, 255]);
        assert_eq!(&layer.pixels[idx(2, 2)..idx(2, 2) + 4], &[9, 8, 7, 255]);
    }

    #[test]
    fn paste_clips_against_a_smaller_current_document() {
        let mut source = Document::new(4, 4).unwrap();
        let id = source
            .add_layer("s", &solid(4, 4, [3, 3, 3, 255]), 4, 4)
            .unwrap();
        let clipboard = source.copy(id).unwrap();

        let mut target = Document::new(2, 2).unwrap();
        let pasted = target.paste(&clipboard, "Pasted");

        let layer = &target.layers()[0];
        assert_eq!(layer.id, pasted);
        assert_eq!(layer.pixels, solid(2, 2, [3, 3, 3, 255]));
    }

    #[test]
    fn paste_survives_a_document_with_no_room_for_it_at_all() {
        let mut source = Document::new(3, 3).unwrap();
        let id = source
            .add_layer("s", &solid(3, 3, [8, 8, 8, 255]), 3, 3)
            .unwrap();
        source.select_rectangle(2.0, 2.0, 3.0, 3.0).unwrap();
        let clipboard = source.copy(id).unwrap(); // origin (2,2)-(3,3): entirely outside a 1x1 target

        let mut target = Document::new(1, 1).unwrap();
        target.paste(&clipboard, "Pasted");

        assert_eq!(target.layers()[0].pixels, vec![0u8; 4]);
    }

    #[test]
    fn delete_selection_clears_only_the_selected_pixels() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [4, 5, 6, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let rect = doc.delete_selection(id).unwrap();

        assert_eq!(
            rect,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 3
            })
        );
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(&pixels[idx(0, 0)..idx(0, 0) + 4], &[4, 5, 6, 255]);
        assert_eq!(&pixels[idx(1, 1)..idx(1, 1) + 4], &[0, 0, 0, 0]);
        assert_eq!(&pixels[idx(2, 2)..idx(2, 2) + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn delete_selection_with_no_selection_clears_the_whole_layer() {
        let (mut doc, id) = doc_with_one_layer();
        doc.delete_selection(id).unwrap();
        assert_eq!(doc.layers()[0].pixels, vec![0u8; 16]);
    }

    #[test]
    fn delete_selection_errors_on_a_locked_layer_and_leaves_it_untouched() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();

        let err = doc.delete_selection(id).unwrap_err();

        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn delete_selection_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.delete_selection(999).is_err());
    }

    #[test]
    fn fill_selection_overwrites_only_the_selected_pixels_with_the_given_colour() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [4, 5, 6, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let rect = doc.fill_selection(id, [200, 100, 50, 255]).unwrap();

        assert_eq!(
            rect,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 3
            })
        );
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(&pixels[idx(0, 0)..idx(0, 0) + 4], &[4, 5, 6, 255]);
        assert_eq!(&pixels[idx(1, 1)..idx(1, 1) + 4], &[200, 100, 50, 255]);
        assert_eq!(&pixels[idx(2, 2)..idx(2, 2) + 4], &[200, 100, 50, 255]);
    }

    #[test]
    fn fill_selection_with_no_selection_fills_the_whole_layer() {
        let (mut doc, id) = doc_with_one_layer();
        doc.fill_selection(id, [1, 2, 3, 4]).unwrap();
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [1, 2, 3, 4]));
    }

    #[test]
    fn fill_selection_respects_a_non_rectangular_selection() {
        // Same 4x4 ellipse layout hand-derived for the copy-masking test:
        // corners fall outside the inscribed ellipse, so they should be
        // left at the original colour while the rest is overwritten.
        let mut doc = Document::new(4, 4).unwrap();
        let id = doc
            .add_layer("base", &solid(4, 4, [9, 9, 9, 255]), 4, 4)
            .unwrap();
        doc.select_ellipse(0.0, 0.0, 4.0, 4.0).unwrap();

        doc.fill_selection(id, [255, 0, 0, 255]).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 4 + x) * 4;
        for &(x, y) in &[(0, 0), (3, 0), (0, 3), (3, 3)] {
            assert_eq!(&pixels[idx(x, y)..idx(x, y) + 4], &[9, 9, 9, 255]);
        }
        assert_eq!(&pixels[idx(1, 1)..idx(1, 1) + 4], &[255, 0, 0, 255]);
    }

    #[test]
    fn fill_selection_errors_on_a_locked_layer_and_leaves_it_untouched() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();

        let err = doc.fill_selection(id, [1, 2, 3, 255]).unwrap_err();

        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn fill_selection_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.fill_selection(999, [1, 2, 3, 255]).is_err());
    }

    /// A 3x3 layer whose red channel climbs left-to-right, top-to-bottom
    /// (10, 20, 30 / 40, 50, 60 / 70, 80, 90), green and blue both zero,
    /// alpha fully opaque — used by the box-blur tests below so every
    /// pixel's 3x3 neighbourhood average can be hand-derived from its
    /// position alone.
    fn ramped_3x3() -> (Document, LayerId) {
        let mut doc = Document::new(3, 3).unwrap();
        #[rustfmt::skip]
        let pixels = [
            10, 0, 0, 255,  20, 0, 0, 255,  30, 0, 0, 255,
            40, 0, 0, 255,  50, 0, 0, 255,  60, 0, 0, 255,
            70, 0, 0, 255,  80, 0, 0, 255,  90, 0, 0, 255,
        ];
        let id = doc.add_layer("base", &pixels, 3, 3).unwrap();
        (doc, id)
    }

    #[test]
    fn box_blur_averages_a_neighbourhood_with_edge_clamping() {
        let (mut doc, id) = ramped_3x3();

        doc.box_blur(id, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Centre pixel's window is the whole 3x3 grid: (10+..+90)/9 = 50,
        // its own original value, since the grid is symmetric around it.
        assert_eq!(pixels[idx(1, 1)], 50);
        // Top-left corner repeats the edge row/column for the missing
        // neighbours: (10+10+20+10+10+20+40+40+50)/9 = 210/9 = 23 (integer
        // division truncates, not rounds).
        assert_eq!(pixels[idx(0, 0)], 23);
        // Bottom-right corner, by the same edge-clamped math:
        // (50+60+60+80+90+90+80+90+90)/9 = 690/9 = 76.
        assert_eq!(pixels[idx(2, 2)], 76);
        // Alpha was uniformly 255 everywhere, so it survives the average
        // exactly (255*9/9 = 255) even though it's blurred like any other
        // channel.
        assert_eq!(pixels[idx(0, 0) + 3], 255);
    }

    #[test]
    fn box_blur_is_confined_to_the_selection() {
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap(); // just the top-left pixel

        doc.box_blur(id, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Same corner value hand-derived in the unconfined test above.
        assert_eq!(pixels[idx(0, 0)], 23);
        // Everywhere outside the selection is untouched, including the
        // centre pixel, whose own blurred value (50) happens to equal its
        // original one — a weaker check on its own, so the corner (2, 2)
        // and an edge pixel are asserted unchanged too.
        assert_eq!(pixels[idx(1, 1)], 50);
        assert_eq!(pixels[idx(2, 2)], 90);
        assert_eq!(pixels[idx(1, 0)], 20);
    }

    #[test]
    fn box_blur_with_zero_radius_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        let err = doc.box_blur(id, 0).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn box_blur_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.box_blur(id, 1).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn box_blur_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.box_blur(999, 1).is_err());
    }

    #[test]
    fn opacity_is_clamped_and_nan_is_rejected() {
        let (mut doc, id) = doc_with_one_layer();

        doc.set_opacity(id, 1.5).unwrap();
        assert_eq!(doc.layers()[0].opacity, 1.0);
        doc.set_opacity(id, -0.5).unwrap();
        assert_eq!(doc.layers()[0].opacity, 0.0);
        assert!(doc.set_opacity(id, f32::NAN).is_err());
    }

    #[test]
    fn a_fully_transparent_layer_does_not_contribute() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_opacity(id, 0.0).unwrap();
        assert!(!doc.layers()[0].contributes());
    }

    #[test]
    fn operations_on_an_unknown_layer_are_errors() {
        let (mut doc, _) = doc_with_one_layer();
        assert!(doc.set_visible(999, true).is_err());
        assert!(doc.set_opacity(999, 0.5).is_err());
        assert!(doc.set_blend_mode(999, BlendMode::Screen).is_err());
        assert!(doc.remove_layer(999).is_err());
        assert!(doc.move_layer(999, MoveDirection::Up).is_err());
    }

    #[test]
    fn new_layers_go_on_top() {
        let mut doc = Document::new(1, 1).unwrap();
        let bottom = doc.add_layer("bottom", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let top = doc.add_layer("top", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let ids: Vec<_> = doc.layers().iter().map(|l| l.id).collect();
        assert_eq!(ids, vec![bottom, top]);
    }

    #[test]
    fn moving_reorders_the_stack() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let c = doc.add_layer("c", &solid(1, 1, [0; 4]), 1, 1).unwrap();

        doc.move_layer(a, MoveDirection::Up).unwrap();
        assert_eq!(ids(&doc), vec![b, a, c]);

        doc.move_layer(c, MoveDirection::Down).unwrap();
        assert_eq!(ids(&doc), vec![b, c, a]);
    }

    #[test]
    fn moving_past_the_end_of_the_stack_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();

        doc.move_layer(b, MoveDirection::Up).unwrap();
        doc.move_layer(a, MoveDirection::Down).unwrap();
        assert_eq!(ids(&doc), vec![a, b]);
    }

    #[test]
    fn merge_visible_requires_at_least_two_visible_layers() {
        let mut doc = Document::new(1, 1).unwrap();
        let err = doc.merge_visible().unwrap_err();
        assert!(err.contains("at least two"), "{err}");

        doc.add_layer("solo", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        assert!(doc.merge_visible().is_err());

        let hidden = doc.add_layer("hidden", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        doc.set_visible(hidden, false).unwrap();
        // `solo` is still the only visible layer — one visible layer plus
        // any number of hidden ones is still not "at least two visible".
        assert!(doc.merge_visible().is_err());
    }

    #[test]
    fn merge_visible_combines_exactly_the_visible_layers() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("red", &solid(1, 1, [255, 0, 0, 255]), 1, 1)
            .unwrap();
        let hidden = doc
            .add_layer("hidden", &solid(1, 1, [0, 255, 0, 255]), 1, 1)
            .unwrap();
        doc.set_visible(hidden, false).unwrap();
        doc.add_layer("green half", &solid(1, 1, [0, 255, 0, 128]), 1, 1)
            .unwrap();

        let merged_id = doc.merge_visible().unwrap();

        // `red` (bottommost of the two visible layers) and `green_half` are
        // gone, replaced by the merged layer at `red`'s old position;
        // `hidden` survived, untouched, still above it.
        assert_eq!(ids(&doc), vec![merged_id, hidden]);
        let merged = &doc.layers()[0];
        assert!(merged.visible);
        assert_eq!(merged.opacity, 1.0);
        assert_eq!(merged.blend_mode, BlendMode::Normal);
        // Half-alpha green over opaque red == the same source-over blend
        // `flatten` would produce for just those two layers.
        assert_eq!(merged.pixels, vec![127, 128, 0, 255]);
        assert!(!doc.layers()[1].visible); // `hidden`, still hidden
    }

    #[test]
    fn merge_visible_lands_at_the_bottommost_merged_layers_position() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("bottom", &solid(1, 1, [1; 4]), 1, 1).unwrap();
        let middle_hidden = doc
            .add_layer("middle hidden", &solid(1, 1, [2; 4]), 1, 1)
            .unwrap();
        doc.set_visible(middle_hidden, false).unwrap();
        doc.add_layer("top", &solid(1, 1, [3; 4]), 1, 1).unwrap();

        let merged_id = doc.merge_visible().unwrap();
        // `bottom` and `top` (both visible) merge into one layer at
        // `bottom`'s old position; `middle_hidden` keeps its place above it.
        assert_eq!(ids(&doc), vec![merged_id, middle_hidden]);
    }

    #[test]
    fn merge_down_on_the_bottom_layer_is_an_error() {
        let mut doc = Document::new(1, 1).unwrap();
        let bottom = doc.add_layer("bottom", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let err = doc.merge_down(bottom).unwrap_err();
        assert!(err.contains("no layer below"), "{err}");
    }

    #[test]
    fn merge_down_combines_a_layer_with_the_one_below_it() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("red", &solid(1, 1, [255, 0, 0, 255]), 1, 1)
            .unwrap();
        let green_half = doc
            .add_layer("green half", &solid(1, 1, [0, 255, 0, 128]), 1, 1)
            .unwrap();
        let top = doc.add_layer("top", &solid(1, 1, [9; 4]), 1, 1).unwrap();

        let merged_id = doc.merge_down(green_half).unwrap();

        // `red` and `green half` collapse into one layer, named after `red`
        // (the layer merged into); `top` is untouched, still above it.
        assert_eq!(ids(&doc), vec![merged_id, top]);
        let merged = &doc.layers()[0];
        assert_eq!(merged.name, "red");
        assert!(merged.visible);
        assert_eq!(merged.opacity, 1.0);
        // Same source-over blend as the equivalent `merge_visible` test.
        assert_eq!(merged.pixels, vec![127, 128, 0, 255]);
    }

    #[test]
    fn merge_down_respects_visibility_of_either_layer() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("bottom", &solid(1, 1, [10, 20, 30, 255]), 1, 1)
            .unwrap();
        let top = doc
            .add_layer("top", &solid(1, 1, [255, 255, 255, 255]), 1, 1)
            .unwrap();
        doc.set_visible(top, false).unwrap();

        doc.merge_down(top).unwrap();

        // `top` was hidden, so it contributed nothing — the merged result
        // is exactly `bottom`'s own pixels, not a blend with white.
        assert_eq!(doc.layers()[0].pixels, vec![10, 20, 30, 255]);
    }

    #[test]
    fn flattening_an_empty_document_is_an_error() {
        let mut doc = Document::new(1, 1).unwrap();
        let err = doc.flatten_image().unwrap_err();
        assert!(err.contains("Nothing to flatten"), "{err}");
    }

    #[test]
    fn flatten_image_discards_hidden_layers_unlike_merge_visible() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("red", &solid(1, 1, [255, 0, 0, 255]), 1, 1)
            .unwrap();
        let hidden = doc
            .add_layer("hidden", &solid(1, 1, [0, 255, 0, 255]), 1, 1)
            .unwrap();
        doc.set_visible(hidden, false).unwrap();

        let id = doc.flatten_image().unwrap();

        assert_eq!(ids(&doc), vec![id]);
        let flattened = &doc.layers()[0];
        assert_eq!(flattened.name, "Background");
        assert!(flattened.visible);
        assert_eq!(flattened.opacity, 1.0);
        assert_eq!(flattened.blend_mode, BlendMode::Normal);
        // Only the visible red layer contributed — matches flatten()'s own
        // "hidden layers don't contribute" rule, and the hidden layer's
        // pixels are gone from the document entirely, not just invisible.
        assert_eq!(flattened.pixels, vec![255, 0, 0, 255]);
    }

    #[test]
    fn flattening_a_single_layer_document_is_a_no_op_visually() {
        let mut doc = Document::new(1, 1).unwrap();
        doc.add_layer("solo", &solid(1, 1, [9, 8, 7, 255]), 1, 1)
            .unwrap();
        doc.flatten_image().unwrap();
        assert_eq!(ids(&doc).len(), 1);
        assert_eq!(doc.layers()[0].pixels, vec![9, 8, 7, 255]);
    }

    #[test]
    fn removing_takes_the_right_layer_out() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        doc.remove_layer(a).unwrap();
        assert_eq!(ids(&doc), vec![b]);
    }

    fn ids(doc: &Document) -> Vec<LayerId> {
        doc.layers().iter().map(|l| l.id).collect()
    }

    fn pixel(doc: &Document, id: LayerId, x: u32, y: u32) -> [u8; 4] {
        let layer = doc.layers().iter().find(|l| l.id == id).unwrap();
        let base = (y as usize * doc.width() as usize + x as usize) * CHANNELS;
        [
            layer.pixels[base],
            layer.pixels[base + 1],
            layer.pixels[base + 2],
            layer.pixels[base + 3],
        ]
    }

    fn transparent_doc(size: u32) -> (Document, LayerId) {
        transparent_doc_wh(size, size)
    }

    fn transparent_doc_wh(width: u32, height: u32) -> (Document, LayerId) {
        let mut doc = Document::new(width, height).unwrap();
        let id = doc
            .add_layer(
                "layer",
                &vec![0u8; (width * height) as usize * CHANNELS],
                width,
                height,
            )
            .unwrap();
        (doc, id)
    }

    #[test]
    fn a_brush_dot_paints_a_solid_circle_on_a_transparent_layer() {
        let (mut doc, id) = transparent_doc(9);
        let rect = doc
            .stroke(
                id,
                &[(4.0, 4.0)],
                3.0,
                Stroke::Brush {
                    color: [255, 0, 0, 255],
                },
            )
            .unwrap();
        // A dot at (4,4) with radius 3 touches roughly (1,1)-(7,7), clamped
        // to the document — the returned rect is what a caller would
        // recomposite instead of the whole 9x9 canvas.
        assert_eq!(
            rect,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 7,
                y1: 7
            })
        );
        // The centre gets full coverage.
        assert_eq!(pixel(&doc, id, 4, 4), [255, 0, 0, 255]);
        // Well outside the radius is untouched.
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn brush_alpha_blends_source_over_the_existing_pixel() {
        let (mut doc, id) = transparent_doc(3);
        doc.stroke(
            id,
            &[(1.0, 1.0)],
            3.0,
            Stroke::Brush {
                color: [0, 0, 0, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 1, 1), [0, 0, 0, 255]);

        doc.stroke(
            id,
            &[(1.0, 1.0)],
            3.0,
            Stroke::Brush {
                color: [255, 255, 255, 128],
            },
        )
        .unwrap();
        // 50%-alpha white over opaque black is mid-grey, same as the compositor.
        let out = pixel(&doc, id, 1, 1);
        assert!(out[3] == 255, "alpha was {}", out[3]);
        for channel in out[..3].iter() {
            assert!(
                channel.abs_diff(128) <= 1,
                "channel {channel} was not near mid-grey"
            );
        }
    }

    #[test]
    fn eraser_multiplies_alpha_toward_zero() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("l", &solid(3, 3, [10, 20, 30, 255]), 3, 3)
            .unwrap();
        doc.stroke(id, &[(1.0, 1.0)], 3.0, Stroke::Eraser).unwrap();
        assert_eq!(pixel(&doc, id, 1, 1)[3], 0);
        // Colour is left alone; only alpha is erased.
        assert_eq!(&pixel(&doc, id, 1, 1)[..3], &[10, 20, 30]);
    }

    #[test]
    fn a_two_point_stroke_fills_the_segment_between_them_not_just_the_endpoints() {
        let (mut doc, id) = transparent_doc(9);
        doc.stroke(
            id,
            &[(1.0, 4.0), (7.0, 4.0)],
            1.0,
            Stroke::Brush {
                color: [1, 2, 3, 255],
            },
        )
        .unwrap();
        // The midpoint, far from either endpoint, is still painted.
        assert_eq!(pixel(&doc, id, 4, 4), [1, 2, 3, 255]);
    }

    #[test]
    fn overlapping_coverage_within_one_stroke_is_maxed_not_summed() {
        // Two dots on the same spot, each with 50%-alpha colour: if coverage
        // summed instead of maxing, the overlap would come out more opaque
        // than a single 50%-alpha dot painted alone.
        let (mut doc, id) = transparent_doc(9);
        let color = [255, 0, 0, 128];
        doc.stroke(id, &[(4.0, 4.0)], 3.0, Stroke::Brush { color })
            .unwrap();
        let once = pixel(&doc, id, 4, 4);

        let (mut doubled, id2) = transparent_doc(9);
        doubled
            .stroke(id2, &[(4.0, 4.0), (4.0, 4.0)], 3.0, Stroke::Brush { color })
            .unwrap();
        let twice = pixel(&doubled, id2, 4, 4);

        assert_eq!(once, twice);
    }

    #[test]
    fn a_zero_or_negative_radius_is_rejected() {
        let (mut doc, id) = transparent_doc(3);
        assert!(doc.stroke(id, &[(1.0, 1.0)], 0.0, Stroke::Eraser).is_err());
        assert!(doc.stroke(id, &[(1.0, 1.0)], -1.0, Stroke::Eraser).is_err());
    }

    #[test]
    fn non_finite_points_are_rejected() {
        let (mut doc, id) = transparent_doc(3);
        assert!(doc
            .stroke(id, &[(f32::NAN, 1.0)], 1.0, Stroke::Eraser)
            .is_err());
    }

    #[test]
    fn an_empty_stroke_is_a_no_op() {
        let (mut doc, id) = transparent_doc(3);
        let rect = doc.stroke(id, &[], 1.0, Stroke::Eraser).unwrap();
        assert_eq!(rect, None);
        assert_eq!(pixel(&doc, id, 1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn a_stroke_entirely_off_canvas_is_clipped_to_nothing() {
        let (mut doc, id) = transparent_doc(3);
        let rect = doc
            .stroke(
                id,
                &[(100.0, 100.0)],
                2.0,
                Stroke::Brush {
                    color: [255, 255, 255, 255],
                },
            )
            .unwrap();
        assert_eq!(rect, None);
        for y in 0..3 {
            for x in 0..3 {
                assert_eq!(pixel(&doc, id, x, y), [0, 0, 0, 0]);
            }
        }
    }

    #[test]
    fn stroking_an_unknown_layer_is_an_error() {
        let (mut doc, _) = transparent_doc(3);
        assert!(doc.stroke(999, &[(1.0, 1.0)], 1.0, Stroke::Eraser).is_err());
    }

    /// A 4x1 document: red, red, blue, red — so a contiguous fill starting
    /// at the left can reach pixel 1 but not pixel 3, which matches colour
    /// but is cut off by the blue pixel breaking the 4-connected chain.
    fn contiguity_test_doc() -> (Document, LayerId) {
        let mut doc = Document::new(4, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([255, 0, 0, 255]); // 0: red
        pixels.extend([255, 0, 0, 255]); // 1: red
        pixels.extend([0, 0, 255, 255]); // 2: blue
        pixels.extend([255, 0, 0, 255]); // 3: red, but unreachable
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        (doc, id)
    }

    #[test]
    fn flood_fill_stops_at_a_differently_coloured_pixel() {
        let (mut doc, id) = contiguity_test_doc();
        let rect = doc
            .flood_fill(id, 0, 0, [0, 255, 0, 255], 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        assert_eq!(pixel(&doc, id, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [0, 0, 255, 255]); // blue: untouched
        assert_eq!(pixel(&doc, id, 3, 0), [255, 0, 0, 255]); // unreachable: untouched
    }

    #[test]
    fn flood_fill_tolerance_controls_how_close_a_match_must_be() {
        let mut doc = Document::new(2, 1).unwrap();
        let id = doc
            .add_layer("pair", &[200, 0, 0, 255, 210, 0, 0, 255], 2, 1)
            .unwrap();

        // Zero tolerance: the 10-off neighbour does not match.
        let rect = doc.flood_fill(id, 0, 0, [0, 255, 0, 255], 0).unwrap();
        assert_eq!(
            rect,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1
            })
        );
        assert_eq!(pixel(&doc, id, 1, 0), [210, 0, 0, 255]); // untouched

        // With enough tolerance the same fill reaches both pixels.
        let mut doc = Document::new(2, 1).unwrap();
        let id = doc
            .add_layer("pair", &[200, 0, 0, 255, 210, 0, 0, 255], 2, 1)
            .unwrap();
        let rect = doc.flood_fill(id, 0, 0, [0, 255, 0, 255], 10).unwrap();
        assert_eq!(
            rect,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            })
        );
        assert_eq!(pixel(&doc, id, 1, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn flood_fill_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let id = doc.add_layer("row", &solid(4, 1, [1; 4]), 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        let rect = doc
            .flood_fill(id, 0, 0, [0, 255, 0, 255], 0)
            .unwrap()
            .unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        assert_eq!(pixel(&doc, id, 1, 0), [0, 255, 0, 255]);
        // Same colour, same contiguous run, but outside the selection.
        assert_eq!(pixel(&doc, id, 2, 0), [1, 1, 1, 1]);
    }

    #[test]
    fn flood_fill_with_the_seed_outside_the_selection_fills_nothing() {
        let mut doc = Document::new(4, 1).unwrap();
        let id = doc.add_layer("row", &solid(4, 1, [1; 4]), 4, 1).unwrap();
        doc.select_rectangle(2.0, 0.0, 4.0, 1.0).unwrap();

        let result = doc.flood_fill(id, 0, 0, [0, 255, 0, 255], 0).unwrap();
        assert_eq!(result, None);
        assert_eq!(pixel(&doc, id, 0, 0), [1, 1, 1, 1]);
    }

    #[test]
    fn flood_fill_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = contiguity_test_doc();
        doc.set_locked(id, true).unwrap();
        let err = doc.flood_fill(id, 0, 0, [0, 255, 0, 255], 0).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn flood_fill_outside_the_canvas_is_an_error() {
        let (mut doc, id) = contiguity_test_doc();
        assert!(doc.flood_fill(id, 4, 0, [0, 255, 0, 255], 0).is_err());
        assert!(doc.flood_fill(id, 0, 1, [0, 255, 0, 255], 0).is_err());
    }

    #[test]
    fn gradient_fill_interpolates_along_the_line() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        let rect = doc
            .gradient_fill(
                id,
                (0.0, 0.0),
                (2.0, 0.0),
                [0, 0, 0, 255],
                [255, 255, 255, 255],
            )
            .unwrap()
            .unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        // Pixel centres at x=0.5 and x=1.5 project to t=0.25 and t=0.75
        // along the 0..2 line — a quarter and three-quarters of the way
        // from black to white.
        assert_eq!(pixel(&doc, id, 0, 0), [64, 64, 64, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [191, 191, 191, 255]);
    }

    #[test]
    fn gradient_fill_clamps_past_either_endpoint() {
        let (mut doc, id) = transparent_doc_wh(3, 1);
        // The line only spans the middle pixel; both outer pixels project
        // past an endpoint and clamp to the colour there rather than
        // extrapolating.
        doc.gradient_fill(
            id,
            (1.0, 0.0),
            (2.0, 0.0),
            [10, 20, 30, 255],
            [200, 210, 220, 255],
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [200, 210, 220, 255]);
    }

    #[test]
    fn gradient_fill_is_confined_to_the_selection() {
        let (mut doc, id) = transparent_doc_wh(4, 1);
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();
        doc.gradient_fill(
            id,
            (0.0, 0.0),
            (4.0, 0.0),
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        )
        .unwrap();
        // Outside the selection: untouched, despite being on the line.
        assert_eq!(pixel(&doc, id, 2, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&doc, id, 3, 0), [0, 0, 0, 0]);
    }

    #[test]
    fn gradient_fill_with_coincident_points_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        let err = doc
            .gradient_fill(
                id,
                (1.0, 1.0),
                (1.0, 1.0),
                [0, 0, 0, 255],
                [255, 255, 255, 255],
            )
            .unwrap_err();
        assert!(err.contains("two distinct points"), "{err}");
    }

    #[test]
    fn gradient_fill_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc
            .gradient_fill(
                id,
                (0.0, 0.0),
                (2.0, 0.0),
                [0, 0, 0, 255],
                [255, 255, 255, 255],
            )
            .unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn gradient_fill_on_an_unknown_layer_is_an_error() {
        let (mut doc, _) = transparent_doc_wh(2, 1);
        assert!(doc
            .gradient_fill(
                999,
                (0.0, 0.0),
                (2.0, 0.0),
                [0, 0, 0, 255],
                [255, 255, 255, 255]
            )
            .is_err());
    }

    #[test]
    fn invert_colors_flips_every_rgb_channel_but_leaves_alpha() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([0, 64, 255, 128]); // 0
        pixels.extend([10, 20, 30, 255]); // 1
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        let rect = doc.invert_colors(id).unwrap().unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        assert_eq!(pixel(&doc, id, 0, 0), [255, 191, 0, 128]);
        assert_eq!(pixel(&doc, id, 1, 0), [245, 235, 225, 255]);
    }

    #[test]
    fn inverting_twice_restores_the_original_colours() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([12, 200, 77, 255]);
        pixels.extend([0, 0, 0, 0]);
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();
        let original = [pixel(&doc, id, 0, 0), pixel(&doc, id, 1, 0)];

        doc.invert_colors(id).unwrap();
        doc.invert_colors(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), original[0]);
        assert_eq!(pixel(&doc, id, 1, 0), original[1]);
    }

    #[test]
    fn invert_colors_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [10u8, 20, 30, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        let rect = doc.invert_colors(id).unwrap().unwrap();
        assert_eq!(
            rect,
            Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 1
            }
        );
        assert_eq!(pixel(&doc, id, 0, 0), [245, 235, 225, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [245, 235, 225, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [10, 20, 30, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn invert_colors_flips_rgb_even_under_zero_alpha() {
        // Invert operates on the layer's raw pixel data, not what's visibly
        // rendered — a fully transparent pixel's RGB (all zero, same as any
        // freshly added layer) still flips to white, even though neither
        // colour is visible until the layer's alpha changes.
        let (mut doc, id) = transparent_doc_wh(1, 1);
        doc.invert_colors(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 0]);
    }

    #[test]
    fn invert_colors_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.invert_colors(id).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn invert_colors_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.invert_colors(999).is_err());
    }

    #[test]
    fn threshold_converts_each_pixel_to_pure_black_or_white_by_luma() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([200, 200, 200, 255]); // luma 200, above a mid threshold
        pixels.extend([50, 50, 50, 255]); // luma 50, below it
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.threshold(id, 128).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn threshold_uses_the_standard_luma_weights_not_a_flat_average() {
        // Pure green's luma (0.587 * 255 ≈ 149.685, rounds to 150) sits well
        // above pure red's (0.299 * 255 ≈ 76.245, rounds to 76) despite both
        // being a single channel maxed at 255 — proving the weighting is
        // actually applied, not just "any channel bright enough".
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([255, 0, 0, 255]); // red: luma 76
        pixels.extend([0, 255, 0, 255]); // green: luma 150
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.threshold(id, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn threshold_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[200, 200, 200, 77], 1, 1).unwrap();
        doc.threshold(id, 128).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 77]);
    }

    #[test]
    fn threshold_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [200u8, 200, 200, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.threshold(id, 128).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 255, 255, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [200, 200, 200, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [200, 200, 200, 255]);
    }

    #[test]
    fn threshold_level_zero_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        let err = doc.threshold(id, 0).unwrap_err();
        assert!(err.contains("between 1 and 255"), "{err}");
    }

    #[test]
    fn threshold_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.threshold(id, 128).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn threshold_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.threshold(999, 128).is_err());
    }

    #[test]
    fn posterize_quantizes_each_channel_to_the_nearest_of_n_evenly_spaced_steps() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[100, 140, 255, 200], 1, 1).unwrap();
        doc.posterize(id, 4).unwrap();
        // step = 255 / 3 = 85: 100 -> 85 (round(1.176) = 1), 140 -> 170
        // (round(1.647) = 2), 255 -> 255 (round(3.0) = 3). Alpha untouched.
        assert_eq!(pixel(&doc, id, 0, 0), [85, 170, 255, 200]);
    }

    #[test]
    fn posterize_of_two_levels_produces_pure_black_or_white_per_channel() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[50, 220, 10, 255], 1, 1).unwrap();
        doc.posterize(id, 2).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn posterize_of_one_level_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        let err = doc.posterize(id, 1).unwrap_err();
        assert!(err.contains("at least 2"), "{err}");
    }

    #[test]
    fn posterize_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [50u8, 50, 50, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.posterize(id, 2).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [0, 0, 0, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [50, 50, 50, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [50, 50, 50, 255]);
    }

    #[test]
    fn posterize_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.posterize(id, 4).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn posterize_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.posterize(999, 4).is_err());
    }

    #[test]
    fn brightness_contrast_of_zero_and_zero_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 128, 240, 77], 1, 1).unwrap();
        doc.brightness_contrast(id, 0, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 128, 240, 77]);
    }

    #[test]
    fn brightness_shifts_every_channel_and_clamps_at_the_ceiling() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[100, 220, 0, 255], 1, 1).unwrap();
        doc.brightness_contrast(id, 50, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [150, 255, 50, 255]);
    }

    #[test]
    fn minimum_contrast_collapses_every_channel_to_mid_grey() {
        // At contrast = -255 the scale factor is exactly 0, so every
        // channel lands on 128 regardless of its original value.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.brightness_contrast(id, 0, -255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn minimum_contrast_plus_brightness_shifts_the_collapsed_grey() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.brightness_contrast(id, 20, -255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [148, 148, 148, 255]);
    }

    #[test]
    fn maximum_contrast_pushes_values_toward_the_extremes() {
        // At contrast = 255 the scale factor is 129.5: the midpoint (128)
        // stays put, but a value just one step to either side of it
        // clamps all the way to black or white.
        let mut doc = Document::new(3, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([127, 127, 127, 255]);
        pixels.extend([128, 128, 128, 255]);
        pixels.extend([129, 129, 129, 255]);
        let id = doc.add_layer("row", &pixels, 3, 1).unwrap();
        doc.brightness_contrast(id, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [128, 128, 128, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn brightness_contrast_sliders_are_clamped_to_their_range() {
        // An out-of-range brightness isn't an error, the same way an
        // out-of-range value in a bounded numeric field just saturates.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[100, 100, 100, 255], 1, 1).unwrap();
        doc.brightness_contrast(id, 9999, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn brightness_contrast_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [100u8, 100, 100, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.brightness_contrast(id, 50, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [150, 150, 150, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [150, 150, 150, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [100, 100, 100, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [100, 100, 100, 255]);
    }

    #[test]
    fn brightness_contrast_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.brightness_contrast(id, 10, 10).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn brightness_contrast_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.brightness_contrast(999, 10, 10).is_err());
    }

    #[test]
    fn hue_shift_of_120_turns_pure_red_into_pure_green() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 120, 0, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 255, 0, 255]);
    }

    #[test]
    fn hue_shift_wraps_around_360_degrees() {
        // +240 from red (h=0) lands at h=240 (blue), the same destination
        // -120 would reach going the other way — confirms rem_euclid
        // wrapping rather than an out-of-range hue breaking the lookup.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 180, 0, 0).unwrap();
        let a = pixel(&doc, id, 0, 0);
        let mut doc2 = Document::new(1, 1).unwrap();
        let id2 = doc2.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc2.hue_saturation(id2, -180, 0, 0).unwrap();
        let b = pixel(&doc2, id2, 0, 0);
        assert_eq!(a, b);
    }

    #[test]
    fn saturation_of_negative_100_fully_desaturates() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 0, -100, 0).unwrap();
        // Pure red's lightness is 0.5; fully desaturated, that's mid-grey.
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn lightness_of_positive_100_turns_any_colour_white() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 0, 0, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn lightness_of_negative_100_turns_any_colour_black() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 0, 0, -100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn hue_saturation_leaves_a_neutral_grey_pixel_unchanged_regardless_of_hue() {
        // A grey pixel has no hue to shift and no saturation to scale —
        // this exercises rgb_to_hsl's zero-chroma branch.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[128, 128, 128, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 90, 0, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn hue_saturation_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 77], 1, 1).unwrap();
        doc.hue_saturation(id, 120, -50, 10).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn hue_saturation_sliders_are_clamped_to_their_range() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.hue_saturation(id, 0, 0, 9999).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn hue_saturation_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [255u8, 0, 0, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.hue_saturation(id, 120, 0, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 255, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [0, 255, 0, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn hue_saturation_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.hue_saturation(id, 10, 10, 10).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn hue_saturation_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.hue_saturation(999, 10, 10, 10).is_err());
    }

    #[test]
    fn black_and_white_sets_every_channel_to_the_bt601_luma() {
        let mut doc = Document::new(3, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([255, 255, 255, 255]); // white: luma 255
        pixels.extend([255, 0, 0, 255]); // red: luma 76
        pixels.extend([0, 255, 0, 255]); // green: luma 150
        let id = doc.add_layer("row", &pixels, 3, 1).unwrap();

        doc.black_and_white(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [76, 76, 76, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [150, 150, 150, 255]);
    }

    #[test]
    fn black_and_white_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 77], 1, 1).unwrap();
        doc.black_and_white(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn black_and_white_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [255u8, 0, 0, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.black_and_white(id).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [76, 76, 76, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [76, 76, 76, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn black_and_white_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.black_and_white(id).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn black_and_white_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.black_and_white(999).is_err());
    }

    #[test]
    fn vibrance_leaves_a_fully_saturated_pixel_unchanged() {
        // Already at saturation 1.0, so vibrance's (1 - s) weighting gives
        // it nothing left to boost.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.vibrance(id, 100, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn vibrance_boosts_a_lightly_saturated_pixel_toward_full_saturation() {
        // (153, 102, 102) is hue 0, saturation 0.2, lightness 0.5. At
        // vibrance +100, s_new = 0.2 + 1.0*(1 - 0.2) = 1.0 - full
        // saturation, landing on pure red at that same lightness.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[153, 102, 102, 255], 1, 1).unwrap();
        doc.vibrance(id, 100, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn negative_vibrance_protects_saturated_pixels_more_than_light_ones() {
        // The same -100 vibrance leaves a fully saturated pixel untouched
        // (protected) while driving a lightly saturated one all the way
        // to grey - the asymmetric protection is the whole point of
        // vibrance over a flat saturation slider.
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([255, 0, 0, 255]); // fully saturated
        pixels.extend([153, 102, 102, 255]); // saturation 0.2
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.vibrance(id, -100, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn vibrances_saturation_slider_applies_uniformly_like_hue_saturations() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 0, 0, 255], 1, 1).unwrap();
        doc.vibrance(id, 0, -100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn vibrance_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[153, 102, 102, 77], 1, 1).unwrap();
        doc.vibrance(id, 50, 20).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn vibrance_sliders_are_clamped_to_their_range() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[153, 102, 102, 255], 1, 1).unwrap();
        doc.vibrance(id, 9999, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn vibrance_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [153u8, 102, 102, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.vibrance(id, 100, 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 0, 0, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [153, 102, 102, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [153, 102, 102, 255]);
    }

    #[test]
    fn vibrance_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.vibrance(id, 10, 10).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn vibrance_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.vibrance(999, 10, 10).is_err());
    }

    #[test]
    fn photo_filter_at_full_density_fully_replaces_the_colour() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.photo_filter(id, [255, 128, 0], 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 128, 0, 255]);
    }

    #[test]
    fn photo_filter_at_zero_density_leaves_the_pixel_unchanged() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.photo_filter(id, [255, 128, 0], 0).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn photo_filter_at_half_density_lands_at_the_midpoint() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 100, 200, 255], 1, 1).unwrap();
        doc.photo_filter(id, [100, 200, 0], 50).unwrap();
        // Midpoint of (0,100,200) and (100,200,0): (50, 150, 100).
        assert_eq!(pixel(&doc, id, 0, 0), [50, 150, 100, 255]);
    }

    #[test]
    fn photo_filter_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.photo_filter(id, [255, 128, 0], 60).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn photo_filter_density_is_clamped_above_100() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.photo_filter(id, [255, 128, 0], 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 128, 0, 255]);
    }

    #[test]
    fn photo_filter_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [10u8, 200, 60, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.photo_filter(id, [255, 128, 0], 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 128, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 128, 0, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [10, 200, 60, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn photo_filter_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.photo_filter(id, [255, 128, 0], 50).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn photo_filter_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.photo_filter(999, [255, 128, 0], 50).is_err());
    }

    #[test]
    fn exposure_defaults_are_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.exposure(id, 0, 0, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn positive_offset_lifts_black_toward_grey() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        doc.exposure(id, 0, 50, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn one_stop_of_exposure_doubles_a_midtone_and_clamps_a_highlight() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([64, 64, 64, 255]); // 64/255 * 2 = 128/255 exactly
        pixels.extend([200, 200, 200, 255]); // doubled would overflow 1.0
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.exposure(id, 100, 0, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn exposure_multiplies_true_black_by_zero_regardless_of_stops() {
        // Unlike offset, exposure is purely multiplicative - it cannot
        // lift a fully black pixel no matter how many stops are dialed in.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        doc.exposure(id, 2000, 0, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn gamma_of_two_applies_a_square_root_curve() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[64, 64, 64, 255], 1, 1).unwrap();
        doc.exposure(id, 0, 0, 200).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn exposure_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.exposure(id, 50, 10, 150).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn exposure_sliders_are_clamped_to_their_range() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        // An out-of-range offset (9999) clamps to the same 50 (+0.50) the
        // dedicated offset test uses, landing on the same mid-grey.
        doc.exposure(id, 0, 9999, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn exposure_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [0u8, 0, 0, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.exposure(id, 0, 50, 100).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [128, 128, 128, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn exposure_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.exposure(id, 50, 10, 100).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn exposure_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.exposure(999, 50, 10, 100).is_err());
    }

    #[test]
    fn gradient_map_sends_black_to_the_shadow_colour_exactly() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        doc.gradient_map(id, [10, 20, 30], [200, 210, 220]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 20, 30, 255]);
    }

    #[test]
    fn gradient_map_sends_white_to_the_highlight_colour_exactly() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 255, 255, 255], 1, 1).unwrap();
        doc.gradient_map(id, [10, 20, 30], [200, 210, 220]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 210, 220, 255]);
    }

    #[test]
    fn gradient_map_uses_bt601_luma_not_a_flat_average() {
        // Against a black-to-white gradient, the mapped grey exactly
        // equals each colour's own luma - the same 76/150 values
        // threshold's and black_and_white's own weighting tests use.
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([255, 0, 0, 255]); // red: luma 76
        pixels.extend([0, 255, 0, 255]); // green: luma 150
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.gradient_map(id, [0, 0, 0], [255, 255, 255]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [76, 76, 76, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [150, 150, 150, 255]);
    }

    #[test]
    fn gradient_map_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[128, 128, 128, 77], 1, 1).unwrap();
        doc.gradient_map(id, [0, 0, 255], [255, 255, 0]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn gradient_map_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [255u8, 255, 255, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.gradient_map(id, [0, 0, 0], [200, 210, 220]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 210, 220, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [200, 210, 220, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn gradient_map_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc
            .gradient_map(id, [0, 0, 0], [255, 255, 255])
            .unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn gradient_map_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.gradient_map(999, [0, 0, 0], [255, 255, 255]).is_err());
    }

    const IDENTITY_MATRIX: [[i32; 4]; 3] = [[100, 0, 0, 0], [0, 100, 0, 0], [0, 0, 100, 0]];
    const IDENTITY_CURVE: [u8; 5] = [0, 64, 128, 192, 255];

    #[test]
    fn channel_mixer_identity_matrix_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.channel_mixer(id, IDENTITY_MATRIX).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn channel_mixer_builds_each_output_as_a_weighted_sum_of_inputs() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 200, 255], 1, 1).unwrap();
        // R output = G input, G output = R input, B output = 50% of B input.
        let matrix = [[0, 100, 0, 0], [100, 0, 0, 0], [0, 0, 50, 0]];
        doc.channel_mixer(id, matrix).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 10, 100, 255]);
    }

    #[test]
    fn channel_mixer_constant_alone_produces_a_flat_colour() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        let matrix = [[0, 0, 0, 200], [0, 0, 0, 200], [0, 0, 0, 200]];
        doc.channel_mixer(id, matrix).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 200, 200, 255]);
    }

    #[test]
    fn channel_mixer_negative_coefficients_invert_the_channels_contribution() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([0, 50, 60, 255]);
        pixels.extend([255, 50, 60, 255]);
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();
        let matrix = [[-100, 0, 0, 100], [0, 100, 0, 0], [0, 0, 100, 0]];
        doc.channel_mixer(id, matrix).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [100, 50, 60, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [0, 50, 60, 255]);
    }

    #[test]
    fn channel_mixer_coefficients_and_output_are_both_clamped() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[128, 0, 0, 255], 1, 1).unwrap();
        // An out-of-range coefficient (9999) clamps to 200 (2.00x), and
        // 2.00 * 128/255 still overflows 1.0, clamping the final output.
        let matrix = [[9999, 0, 0, 0], [0, 100, 0, 0], [0, 0, 100, 0]];
        doc.channel_mixer(id, matrix).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 0, 0, 255]);
    }

    #[test]
    fn channel_mixer_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.channel_mixer(id, IDENTITY_MATRIX).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn channel_mixer_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [10u8, 200, 60, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        let matrix = [[0, 0, 0, 200], [0, 0, 0, 200], [0, 0, 0, 200]];
        doc.channel_mixer(id, matrix).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 200, 200, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [200, 200, 200, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [10, 200, 60, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn channel_mixer_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.channel_mixer(id, IDENTITY_MATRIX).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn channel_mixer_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.channel_mixer(999, IDENTITY_MATRIX).is_err());
    }

    #[test]
    fn levels_at_defaults_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.levels(id, 0, 255, 100, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn levels_narrows_the_input_range() {
        let mut doc = Document::new(3, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([50, 50, 50, 255]); // at input_black: maps to 0
        pixels.extend([200, 200, 200, 255]); // at input_white: maps to 255
        pixels.extend([125, 125, 125, 255]); // exact midpoint: maps to 128
        let id = doc.add_layer("row", &pixels, 3, 1).unwrap();

        doc.levels(id, 50, 200, 100, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 255, 255, 255]);
        assert_eq!(pixel(&doc, id, 2, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn levels_gamma_of_two_applies_a_square_root_curve() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[64, 64, 64, 255], 1, 1).unwrap();
        doc.levels(id, 0, 255, 200, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [128, 128, 128, 255]);
    }

    #[test]
    fn levels_narrows_the_output_range() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([0, 0, 0, 255]);
        pixels.extend([255, 255, 255, 255]);
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.levels(id, 0, 255, 100, 50, 200).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [50, 50, 50, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [200, 200, 200, 255]);
    }

    #[test]
    fn levels_input_white_is_clamped_above_input_black() {
        // input_white (100) below input_black (200) would divide by a
        // negative range; it's clamped to input_black + 1 instead of
        // erroring, giving a step-like but still well-defined result.
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([200, 200, 200, 255]);
        pixels.extend([255, 255, 255, 255]);
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.levels(id, 200, 100, 100, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [255, 255, 255, 255]);
    }

    #[test]
    fn levels_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.levels(id, 0, 255, 100, 0, 255).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn levels_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [0u8, 0, 0, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.levels(id, 0, 255, 100, 50, 200).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [50, 50, 50, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [50, 50, 50, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn levels_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.levels(id, 0, 255, 100, 0, 255).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn levels_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.levels(999, 0, 255, 100, 0, 255).is_err());
    }

    #[test]
    fn curves_at_identity_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.curves(id, IDENTITY_CURVE).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn curves_control_points_reproduce_exactly_at_their_input_position() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[128, 128, 128, 255], 1, 1).unwrap();
        doc.curves(id, [0, 64, 200, 192, 255]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [200, 200, 200, 255]);
    }

    #[test]
    fn curves_interpolates_linearly_between_control_points() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[96, 96, 96, 255], 1, 1).unwrap();
        // Between input 64 (output 64) and input 128 (output 200): halfway
        // across the input span lands halfway across the output span too.
        doc.curves(id, [0, 64, 200, 192, 255]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [132, 132, 132, 255]);
    }

    #[test]
    fn curves_can_flatten_a_range_to_a_constant() {
        let mut doc = Document::new(2, 1).unwrap();
        let mut pixels = Vec::new();
        pixels.extend([80, 80, 80, 255]);
        pixels.extend([180, 180, 180, 255]);
        let id = doc.add_layer("row", &pixels, 2, 1).unwrap();

        doc.curves(id, [0, 150, 150, 150, 255]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [150, 150, 150, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [150, 150, 150, 255]);
    }

    #[test]
    fn curves_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.curves(id, IDENTITY_CURVE).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn curves_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [80u8, 80, 80, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.curves(id, [0, 150, 150, 150, 255]).unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [150, 150, 150, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [150, 150, 150, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [80, 80, 80, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [80, 80, 80, 255]);
    }

    #[test]
    fn curves_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc.curves(id, IDENTITY_CURVE).unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn curves_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc.curves(999, IDENTITY_CURVE).is_err());
    }

    #[test]
    fn color_balance_at_zero_is_a_no_op() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 255], 1, 1).unwrap();
        doc.color_balance(id, [0, 0, 0], [0, 0, 0], [0, 0, 0])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [10, 200, 60, 255]);
    }

    #[test]
    fn color_balance_shifts_pure_shadows() {
        // Luma 0 (pure black) is 100% shadow weight, 0% midtone/highlight.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        doc.color_balance(id, [40, 20, 10], [0, 0, 0], [0, 0, 0])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [40, 20, 10, 255]);
    }

    #[test]
    fn color_balance_shifts_pure_midtones() {
        // Luma 127 (r=g=b=127) is 100% midtone weight: the shadow ramp
        // has reached 0 by luma 127 and the highlight ramp hasn't started
        // until luma 128.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[127, 127, 127, 255], 1, 1).unwrap();
        doc.color_balance(id, [0, 0, 0], [10, -10, 5], [0, 0, 0])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [137, 117, 132, 255]);
    }

    #[test]
    fn color_balance_shifts_pure_highlights() {
        // Luma 255 (pure white) is 100% highlight weight. Also exercises
        // clamping: 255 + 15 saturates at 255.
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[255, 255, 255, 255], 1, 1).unwrap();
        doc.color_balance(id, [0, 0, 0], [0, 0, 0], [-30, 15, -5])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [225, 255, 250, 255]);
    }

    #[test]
    fn color_balance_sliders_are_clamped_to_plus_minus_100() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[0, 0, 0, 255], 1, 1).unwrap();
        doc.color_balance(id, [500, -500, 0], [0, 0, 0], [0, 0, 0])
            .unwrap();
        // 500 clamps to 100 (0 + 100 = 100); -500 clamps to -100 (0 - 100
        // clamps again, at the byte floor, to 0).
        assert_eq!(pixel(&doc, id, 0, 0), [100, 0, 0, 255]);
    }

    #[test]
    fn color_balance_leaves_alpha_untouched() {
        let mut doc = Document::new(1, 1).unwrap();
        let id = doc.add_layer("layer", &[10, 200, 60, 77], 1, 1).unwrap();
        doc.color_balance(id, [0, 0, 0], [0, 0, 0], [0, 0, 0])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0)[3], 77);
    }

    #[test]
    fn color_balance_is_confined_to_the_selection() {
        let mut doc = Document::new(4, 1).unwrap();
        let pixels = [0u8, 0, 0, 255].repeat(4);
        let id = doc.add_layer("row", &pixels, 4, 1).unwrap();
        doc.select_rectangle(0.0, 0.0, 2.0, 1.0).unwrap();

        doc.color_balance(id, [50, 0, 0], [0, 0, 0], [0, 0, 0])
            .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [50, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 1, 0), [50, 0, 0, 255]);
        // Outside the selection: untouched.
        assert_eq!(pixel(&doc, id, 2, 0), [0, 0, 0, 255]);
        assert_eq!(pixel(&doc, id, 3, 0), [0, 0, 0, 255]);
    }

    #[test]
    fn color_balance_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = transparent_doc_wh(2, 1);
        doc.set_locked(id, true).unwrap();
        let err = doc
            .color_balance(id, [0, 0, 0], [0, 0, 0], [0, 0, 0])
            .unwrap_err();
        assert!(err.contains("locked"), "{err}");
    }

    #[test]
    fn color_balance_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 1).unwrap();
        assert!(doc
            .color_balance(999, [0, 0, 0], [0, 0, 0], [0, 0, 0])
            .is_err());
    }

    #[test]
    fn a_new_document_has_no_selection() {
        let doc = Document::new(4, 4).unwrap();
        assert_eq!(doc.selection(), None);
    }

    #[test]
    fn selecting_a_rectangle_clamps_to_the_document_and_sorts_corners() {
        let mut doc = Document::new(10, 10).unwrap();
        // Drag from bottom-right to top-left, past both edges.
        doc.select_rectangle(-5.0, -5.0, 6.0, 6.0).unwrap();
        assert_eq!(
            doc.selection(),
            Some(Selection {
                shape: SelectionShape::Rectangle,
                bounds: Rect {
                    x0: 0,
                    y0: 0,
                    x1: 6,
                    y1: 6
                },
                inverted: false,
                border: None,
            })
        );
    }

    #[test]
    fn a_zero_area_selection_is_rejected() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.select_rectangle(3.0, 3.0, 3.0, 3.0).is_err());
        assert!(doc.select_ellipse(3.0, 3.0, 3.0, 8.0).is_err()); // zero width
    }

    #[test]
    fn non_finite_selection_coordinates_are_rejected() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.select_rectangle(f32::NAN, 0.0, 5.0, 5.0).is_err());
    }

    #[test]
    fn deselect_clears_the_selection() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(0.0, 0.0, 5.0, 5.0).unwrap();
        assert!(doc.selection().is_some());
        doc.deselect();
        assert_eq!(doc.selection(), None);
    }

    #[test]
    fn select_all_covers_the_whole_canvas() {
        let mut doc = Document::new(10, 6).unwrap();
        doc.select_all().unwrap();
        assert_eq!(
            doc.selection(),
            Some(Selection {
                shape: SelectionShape::Rectangle,
                bounds: Rect {
                    x0: 0,
                    y0: 0,
                    x1: 10,
                    y1: 6
                },
                inverted: false,
                border: None,
            })
        );
    }

    #[test]
    fn inverting_with_nothing_selected_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.invert_selection().is_err());
    }

    #[test]
    fn inverting_twice_returns_to_the_original_selection() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        let original = doc.selection().unwrap();
        doc.invert_selection().unwrap();
        assert!(doc.selection().unwrap().inverted);
        doc.invert_selection().unwrap();
        assert_eq!(doc.selection(), Some(original));
    }

    #[test]
    fn an_inverted_selection_confines_a_stroke_to_outside_its_bounds() {
        let (mut doc, id) = transparent_doc(9);
        doc.select_rectangle(3.0, 0.0, 6.0, 9.0).unwrap();
        doc.invert_selection().unwrap();
        doc.stroke(
            id,
            &[(4.0, 4.0)], // dead centre of the (now unselected) rectangle
            1.0,
            Stroke::Brush {
                color: [255, 0, 0, 255],
            },
        )
        .unwrap();
        // Inside the original rectangle: still unselected, so untouched.
        assert_eq!(pixel(&doc, id, 4, 4), [0, 0, 0, 0]);

        doc.stroke(
            id,
            &[(0.5, 4.0)], // outside the original rectangle: selected once inverted
            1.0,
            Stroke::Brush {
                color: [255, 0, 0, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 4), [255, 0, 0, 255]);
    }

    #[test]
    fn expanding_with_nothing_selected_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.expand_selection(2).is_err());
    }

    #[test]
    fn contracting_with_nothing_selected_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.contract_selection(2).is_err());
    }

    #[test]
    fn expanding_by_zero_pixels_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        assert!(doc.expand_selection(0).is_err());
    }

    #[test]
    fn contracting_by_zero_pixels_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        assert!(doc.contract_selection(0).is_err());
    }

    #[test]
    fn expand_grows_the_bounds_on_every_side() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 10.0, 10.0).unwrap();
        doc.expand_selection(2).unwrap();
        assert_eq!(
            doc.selection().unwrap().bounds,
            Rect {
                x0: 3,
                y0: 3,
                x1: 12,
                y1: 12
            }
        );
    }

    #[test]
    fn expand_clamps_to_the_canvas_edge() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(1.0, 1.0, 19.0, 19.0).unwrap();
        doc.expand_selection(5).unwrap();
        assert_eq!(
            doc.selection().unwrap().bounds,
            Rect {
                x0: 0,
                y0: 0,
                x1: 20,
                y1: 20
            }
        );
    }

    #[test]
    fn contract_shrinks_the_bounds_on_every_side() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 15.0, 15.0).unwrap();
        doc.contract_selection(3).unwrap();
        assert_eq!(
            doc.selection().unwrap().bounds,
            Rect {
                x0: 8,
                y0: 8,
                x1: 12,
                y1: 12
            }
        );
    }

    #[test]
    fn contracting_past_the_selection_size_is_an_error_and_leaves_it_unchanged() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 10.0, 10.0).unwrap();
        let before = doc.selection().unwrap();
        assert!(doc.contract_selection(10).is_err());
        assert_eq!(doc.selection().unwrap(), before);
    }

    #[test]
    fn expanding_an_inverted_selection_shrinks_the_excluded_shape() {
        // Inverted: everything *except* the rectangle is selected. Growing
        // that selected area outward means the excluded rectangle itself
        // shrinks — the opposite direction from expanding a normal selection.
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 15.0, 15.0).unwrap();
        doc.invert_selection().unwrap();
        doc.expand_selection(2).unwrap();
        let selection = doc.selection().unwrap();
        assert!(selection.inverted);
        assert_eq!(
            selection.bounds,
            Rect {
                x0: 7,
                y0: 7,
                x1: 13,
                y1: 13
            }
        );
    }

    #[test]
    fn contracting_an_inverted_selection_grows_the_excluded_shape() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 15.0, 15.0).unwrap();
        doc.invert_selection().unwrap();
        doc.contract_selection(2).unwrap();
        let selection = doc.selection().unwrap();
        assert!(selection.inverted);
        assert_eq!(
            selection.bounds,
            Rect {
                x0: 3,
                y0: 3,
                x1: 17,
                y1: 17
            }
        );
    }

    #[test]
    fn smooth_rounds_a_rectangles_corners() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(2.0, 2.0, 18.0, 18.0).unwrap();
        doc.smooth_selection(4).unwrap();
        let selection = doc.selection().unwrap();
        assert_eq!(
            selection.shape,
            SelectionShape::RoundedRectangle { radius: 4 }
        );
        assert_eq!(
            selection.bounds,
            Rect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18
            }
        );
    }

    #[test]
    fn smooth_clamps_the_radius_to_half_the_shorter_side() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(5.0, 5.0, 15.0, 10.0).unwrap(); // 10 wide, 5 tall
        doc.smooth_selection(100).unwrap();
        assert_eq!(
            doc.selection().unwrap().shape,
            SelectionShape::RoundedRectangle { radius: 2 } // half of 5, floored
        );
    }

    #[test]
    fn smooth_on_an_ellipse_selection_is_a_no_op() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_ellipse(2.0, 2.0, 18.0, 18.0).unwrap();
        let before = doc.selection().unwrap();
        doc.smooth_selection(4).unwrap();
        assert_eq!(doc.selection().unwrap(), before);
    }

    #[test]
    fn smooth_with_zero_radius_is_an_error() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(2.0, 2.0, 18.0, 18.0).unwrap();
        assert!(doc.smooth_selection(0).is_err());
    }

    #[test]
    fn smoothing_with_nothing_selected_is_an_error() {
        let mut doc = Document::new(20, 20).unwrap();
        assert!(doc.smooth_selection(4).is_err());
    }

    #[test]
    fn a_rounded_rectangle_selection_excludes_only_its_corners() {
        let (mut doc, id) = transparent_doc(10);
        doc.select_rectangle(0.0, 0.0, 10.0, 10.0).unwrap();
        doc.smooth_selection(3).unwrap();

        // A true corner pixel: outside the rounding circle.
        doc.stroke(
            id,
            &[(0.5, 0.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 0]);

        // The flat left edge, mid-height: still selected, unlike an ellipse -
        // rounding only cuts the corners, not the whole boundary.
        doc.stroke(
            id,
            &[(0.5, 5.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn border_sets_the_border_field_without_touching_shape_or_bounds() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(2.0, 2.0, 18.0, 18.0).unwrap();
        doc.border_selection(3).unwrap();
        let selection = doc.selection().unwrap();
        assert_eq!(selection.shape, SelectionShape::Rectangle);
        assert_eq!(selection.border, Some(3));
        assert_eq!(
            selection.bounds,
            Rect {
                x0: 2,
                y0: 2,
                x1: 18,
                y1: 18
            }
        );
    }

    #[test]
    fn border_with_zero_width_is_an_error() {
        let mut doc = Document::new(20, 20).unwrap();
        doc.select_rectangle(2.0, 2.0, 18.0, 18.0).unwrap();
        assert!(doc.border_selection(0).is_err());
    }

    #[test]
    fn bordering_with_nothing_selected_is_an_error() {
        let mut doc = Document::new(20, 20).unwrap();
        assert!(doc.border_selection(3).is_err());
    }

    #[test]
    fn a_rectangle_border_selects_only_a_band_near_the_edge() {
        let (mut doc, id) = transparent_doc(10);
        doc.select_rectangle(0.0, 0.0, 10.0, 10.0).unwrap();
        doc.border_selection(2).unwrap();

        // Near the edge: inside the band.
        doc.stroke(
            id,
            &[(0.5, 0.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [255, 255, 255, 255]);

        // Dead centre: inside the excluded hole.
        doc.stroke(
            id,
            &[(5.5, 5.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 5, 5), [0, 0, 0, 0]);
    }

    #[test]
    fn a_border_at_least_half_the_shorter_side_selects_the_whole_shape() {
        // Width 10 on a 10x10 selection collapses the hole entirely (see
        // `shrink_rect`) — the border selects everywhere the shape did.
        let (mut doc, id) = transparent_doc(10);
        doc.select_rectangle(0.0, 0.0, 10.0, 10.0).unwrap();
        doc.border_selection(10).unwrap();

        doc.stroke(
            id,
            &[(5.5, 5.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 5, 5), [255, 255, 255, 255]);
    }

    #[test]
    fn an_ellipse_border_selects_a_ring() {
        let (mut doc, id) = transparent_doc(20);
        doc.select_ellipse(0.0, 0.0, 20.0, 20.0).unwrap();
        doc.border_selection(3).unwrap();

        // Radius ~8.5 from the centre (10, 10): between the inner ellipse's
        // radius (7) and the outer one's (10) — inside the ring.
        doc.stroke(
            id,
            &[(18.5, 10.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 18, 10), [255, 255, 255, 255]);

        // Radius ~5.5 from the centre: inside the inner ellipse, so inside
        // the excluded hole.
        doc.stroke(
            id,
            &[(15.5, 10.5)],
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 15, 10), [0, 0, 0, 0]);
    }

    #[test]
    fn reselecting_with_nothing_previously_deselected_is_an_error() {
        let mut doc = Document::new(10, 10).unwrap();
        assert!(doc.reselect().is_err());

        // Selecting and then reselecting without ever deselecting is also an
        // error — reselect restores what `deselect` cleared, not "whatever
        // was ever selected".
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        assert!(doc.reselect().is_err());
    }

    #[test]
    fn reselect_restores_what_deselect_cleared() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        let original = doc.selection().unwrap();
        doc.deselect();
        assert_eq!(doc.selection(), None);

        doc.reselect().unwrap();
        assert_eq!(doc.selection(), Some(original));
    }

    #[test]
    fn reselect_is_available_again_after_a_second_deselect() {
        let mut doc = Document::new(10, 10).unwrap();
        doc.select_rectangle(2.0, 2.0, 5.0, 5.0).unwrap();
        doc.deselect();
        doc.reselect().unwrap();
        doc.deselect();
        doc.reselect().unwrap();
    }

    #[test]
    fn a_rectangle_selection_confines_a_stroke_to_its_bounds() {
        let (mut doc, id) = transparent_doc(9);
        doc.select_rectangle(4.0, 0.0, 9.0, 9.0).unwrap();
        doc.stroke(
            id,
            &[(4.0, 4.0)],
            5.0,
            Stroke::Brush {
                color: [255, 0, 0, 255],
            },
        )
        .unwrap();
        // Inside the selection: painted.
        assert_eq!(pixel(&doc, id, 6, 4), [255, 0, 0, 255]);
        // Same brush stroke, but left of x=4: outside the selection, untouched
        // even though it is well within the brush's radius.
        assert_eq!(pixel(&doc, id, 1, 4), [0, 0, 0, 0]);
    }

    #[test]
    fn an_ellipse_selection_excludes_its_bounding_box_corners() {
        let (mut doc, id) = transparent_doc(9);
        doc.select_ellipse(0.0, 0.0, 9.0, 9.0).unwrap();
        doc.stroke(
            id,
            &[(0.5, 0.5)], // a bounding-box corner: outside the inscribed circle
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 0, 0), [0, 0, 0, 0]);

        doc.stroke(
            id,
            &[(4.5, 4.5)], // dead centre: well inside the inscribed circle
            1.0,
            Stroke::Brush {
                color: [255, 255, 255, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 4, 4), [255, 255, 255, 255]);
    }

    #[test]
    fn eraser_strokes_are_also_confined_to_the_selection() {
        let mut doc = Document::new(9, 1).unwrap();
        let id = doc
            .add_layer("l", &solid(9, 1, [1, 2, 3, 255]), 9, 1)
            .unwrap();
        doc.select_rectangle(5.0, 0.0, 9.0, 1.0).unwrap();
        // A stroke well clear of both endpoints at the two sample points below,
        // so each one gets full brush coverage rather than the soft-edge
        // falloff near a stroke's own ends.
        doc.stroke(id, &[(0.0, 0.0), (8.0, 0.0)], 1.0, Stroke::Eraser)
            .unwrap();
        // Outside the selection: alpha untouched.
        assert_eq!(pixel(&doc, id, 2, 0)[3], 255);
        // Inside the selection: erased.
        assert_eq!(pixel(&doc, id, 7, 0)[3], 0);
    }

    #[test]
    fn with_no_selection_a_stroke_is_unrestricted() {
        // Same geometry as the confinement test above, minus the select
        // call: without an active selection every stroke paints normally.
        let (mut doc, id) = transparent_doc(9);
        doc.stroke(
            id,
            &[(1.0, 4.0)],
            2.0,
            Stroke::Brush {
                color: [255, 0, 0, 255],
            },
        )
        .unwrap();
        assert_eq!(pixel(&doc, id, 1, 4), [255, 0, 0, 255]);
    }
}
