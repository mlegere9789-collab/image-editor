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
}

impl Selection {
    /// Whether the pixel centred at `(px, py)` — the same `+0.5` convention
    /// [`Document::stroke`] already samples at — falls inside this selection.
    fn contains(&self, px: f32, py: f32) -> bool {
        let Rect { x0, y0, x1, y1 } = self.bounds;
        let in_bounds = !(px < x0 as f32 || px >= x1 as f32 || py < y0 as f32 || py >= y1 as f32);
        let in_shape = in_bounds
            && match self.shape {
                SelectionShape::Rectangle => true,
                SelectionShape::Ellipse => {
                    let (cx, cy) = ((x0 as f32 + x1 as f32) / 2.0, (y0 as f32 + y1 as f32) / 2.0);
                    let (rx, ry) = ((x1 - x0) as f32 / 2.0, (y1 - y0) as f32 / 2.0);
                    let (nx, ny) = ((px - cx) / rx, (py - cy) / ry);
                    nx * nx + ny * ny <= 1.0
                }
            };
        in_shape != self.inverted
    }
}

/// The subset of a [`Selection`] the UI needs to draw its outline.
pub type SelectionView = Selection;

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
