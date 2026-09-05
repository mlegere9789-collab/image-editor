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

/// How Filter > Stylize > Diffuse decides which neighbour a pixel takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiffuseMode {
    /// Take the randomly chosen neighbour unconditionally.
    Normal,
    /// Take the random neighbour only if it is darker (smaller R+G+B).
    DarkenOnly,
    /// Take the random neighbour only if it is lighter (larger R+G+B).
    LightenOnly,
    /// No randomness: take the in-bounds neighbour closest in colour.
    Anisotropic,
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

/// The flat average of every channel of `source` (a document-sized RGBA8
/// buffer, `doc_width` pixels wide) in a `(2*radius+1)`-square window
/// centred on `(col, row)`, clamped to `width`x`height` — sampling past an
/// edge repeats the edge pixel rather than wrapping or padding with
/// transparency. Shared by [`Document::box_blur`] (writes this straight to
/// the pixel) and [`Document::unsharp_mask`] (subtracts this from the
/// original instead).
fn box_blur_at(
    source: &[u8],
    doc_width: usize,
    width: i64,
    height: i64,
    row: u32,
    col: u32,
    radius: i64,
) -> [u8; CHANNELS] {
    let samples = (-radius..=radius).flat_map(|dy| {
        let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
        (-radius..=radius).map(move |dx| {
            let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
            (sx, sy)
        })
    });
    average_samples(source, doc_width, samples)
}

/// The flat average of `source`'s (a document-sized RGBA8 buffer,
/// `doc_width` pixels wide) pixels at each `(x, y)` coordinate in
/// `samples`, with no weighting — every sample counts equally regardless
/// of how many times its coordinate repeats (which is exactly how
/// clamp-to-edge sampling is meant to behave: an edge pixel sampled twice
/// really does count twice). Shared building block behind [`box_blur_at`]
/// (a square neighbourhood) and [`motion_blur_at`] (a line of samples
/// along a direction).
fn average_samples(
    source: &[u8],
    doc_width: usize,
    samples: impl Iterator<Item = (usize, usize)>,
) -> [u8; CHANNELS] {
    let mut sums = [0u32; CHANNELS];
    let mut count = 0u32;
    for (sx, sy) in samples {
        let base = (sy * doc_width + sx) * CHANNELS;
        for (c, sum) in sums.iter_mut().enumerate() {
            *sum += source[base + c] as u32;
        }
        count += 1;
    }
    let mut averaged = [0u8; CHANNELS];
    for (out, sum) in averaged.iter_mut().zip(&sums) {
        *out = (sum / count) as u8;
    }
    averaged
}

/// Like [`box_blur_at`], but the window is a straight line through
/// `(col, row)` instead of a square: `2 * half + 1` samples at integer
/// steps `t` from `-half` to `half`, each offset by `t * (dx, dy)` and
/// rounded to the nearest pixel (nearest-neighbour, not a true
/// anti-aliased line — the same "hard-edged, no anti-aliasing" scope cut
/// this project's selection system already makes). `(dx, dy)` is expected
/// to be a unit vector; passing a non-unit vector just scales the streak
/// length. Clamps to the layer's edges exactly like [`box_blur_at`].
fn motion_blur_at(
    source: &[u8],
    doc_width: usize,
    (width, height): (i64, i64),
    (row, col): (u32, u32),
    (dx, dy): (f32, f32),
    half: i64,
) -> [u8; CHANNELS] {
    let samples = (-half..=half).map(|t| {
        let sx = (col as i64 + (t as f32 * dx).round() as i64).clamp(0, width - 1) as usize;
        let sy = (row as i64 + (t as f32 * dy).round() as i64).clamp(0, height - 1) as usize;
        (sx, sy)
    });
    average_samples(source, doc_width, samples)
}

/// The per-channel *median* (not mean) of `source`'s pixels in the same
/// `(2*radius+1)`-square, edge-clamped window [`box_blur_at`] uses. Each
/// channel is sorted independently and its middle sample taken — the
/// window always holds an odd number of samples, so there's a true
/// middle and no averaging of two neighbours is ever needed. Repeated
/// edge samples count as many times as they're sampled, exactly as in
/// `box_blur_at`. A median filter is the classic way to remove isolated
/// specks (dust, hot pixels, salt-and-pepper noise) without softening
/// edges the way a mean does: an outlier never survives to the middle of
/// the sorted list, while a genuine edge — where roughly half the window
/// is one colour and half another — keeps a value from one side or the
/// other rather than a smeared blend.
fn median_at(
    source: &[u8],
    doc_width: usize,
    width: i64,
    height: i64,
    row: u32,
    col: u32,
    radius: i64,
) -> [u8; CHANNELS] {
    let side = (2 * radius + 1) as usize;
    let mut channels: [Vec<u8>; CHANNELS] =
        std::array::from_fn(|_| Vec::with_capacity(side * side));
    for dy in -radius..=radius {
        let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
        for dx in -radius..=radius {
            let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
            let base = (sy * doc_width + sx) * CHANNELS;
            for (c, samples) in channels.iter_mut().enumerate() {
                samples.push(source[base + c]);
            }
        }
    }
    let mut median = [0u8; CHANNELS];
    for (out, samples) in median.iter_mut().zip(channels.iter_mut()) {
        samples.sort_unstable();
        *out = samples[samples.len() / 2];
    }
    median
}

/// The per-channel maximum (`want_max`) or minimum of `source`'s pixels in
/// the same `(2*radius+1)`-square, edge-clamped window [`median_at`] and
/// [`box_blur_at`] use. Photoshop's Maximum and Minimum filters are the
/// morphological *dilate* and *erode*: Maximum grows light regions into
/// dark ones (every pixel becomes the brightest thing within `radius`),
/// Minimum grows dark regions into light ones. Each channel is treated
/// independently, alpha included, like the other window filters.
fn extreme_at(
    source: &[u8],
    doc_width: usize,
    (width, height): (i64, i64),
    (row, col): (u32, u32),
    radius: i64,
    want_max: bool,
) -> [u8; CHANNELS] {
    let mut out = if want_max {
        [0u8; CHANNELS]
    } else {
        [255u8; CHANNELS]
    };
    for dy in -radius..=radius {
        let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
        for dx in -radius..=radius {
            let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
            let base = (sy * doc_width + sx) * CHANNELS;
            for (c, slot) in out.iter_mut().enumerate() {
                let v = source[base + c];
                *slot = if want_max {
                    (*slot).max(v)
                } else {
                    (*slot).min(v)
                };
            }
        }
    }
    out
}

/// Photoshop's Filter > Other > Custom convolution at one pixel: each
/// colour channel becomes `(Σ kernel[i] · sample[i]) / scale + offset`,
/// clamped to 0..=255. The 25 kernel entries are laid out row by row over
/// the 5×5 neighbourhood centred on `(col, row)` — `kernel[12]` is the
/// pixel itself, `kernel[0]` the sample two up and two left — and samples
/// beyond the layer edge clamp to the nearest edge pixel like every other
/// window filter here. The division is integer division truncating toward
/// zero, the arithmetic a person can redo on paper, and alpha is carried
/// over unchanged because Custom is a colour filter. `scale` must be
/// non-zero; the caller checks.
fn convolve_at(
    source: &[u8],
    doc_width: usize,
    (width, height): (i64, i64),
    (row, col): (u32, u32),
    kernel: &[i32; 25],
    scale: i32,
    offset: i32,
) -> [u8; CHANNELS] {
    let mut sums = [0i64; 3];
    for (i, &weight) in kernel.iter().enumerate() {
        if weight == 0 {
            continue;
        }
        let (kx, ky) = ((i % 5) as i64 - 2, (i / 5) as i64 - 2);
        let sx = (col as i64 + kx).clamp(0, width - 1) as usize;
        let sy = (row as i64 + ky).clamp(0, height - 1) as usize;
        let base = (sy * doc_width + sx) * CHANNELS;
        for (c, sum) in sums.iter_mut().enumerate() {
            *sum += weight as i64 * source[base + c] as i64;
        }
    }
    let centre = (row as usize * doc_width + col as usize) * CHANNELS;
    let mut out = [0u8; CHANNELS];
    for (slot, sum) in out.iter_mut().zip(&sums) {
        *slot = (sum / scale as i64 + offset as i64).clamp(0, 255) as u8;
    }
    out[3] = source[centre + 3];
    out
}

/// The Sobel edge magnitude of `source` at `(col, row)`, per colour
/// channel: `|Gx| + |Gy|` clamped to 255, where `Gx` weights the 3×3
/// neighbourhood by `[-1 0 1; -2 0 2; -1 0 1]` and `Gy` by its transpose,
/// with samples past the layer edge clamped like every other window
/// filter here. Those are the two kernels every image-processing text
/// prints, and the L1 sum of their absolute responses keeps the result in
/// integers a person can check; a flat region scores exactly 0. Alpha is
/// carried over unchanged. Shared by Find Edges, which inverts it.
fn sobel_at(
    source: &[u8],
    doc_width: usize,
    (width, height): (i64, i64),
    (row, col): (u32, u32),
) -> [u8; CHANNELS] {
    let sample = |dx: i64, dy: i64| {
        let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
        let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
        (sy * doc_width + sx) * CHANNELS
    };
    let mut out = [0u8; CHANNELS];
    for (c, slot) in out.iter_mut().enumerate().take(3) {
        let v = |dx: i64, dy: i64| source[sample(dx, dy) + c] as i32;
        let gx = (v(1, -1) + 2 * v(1, 0) + v(1, 1)) - (v(-1, -1) + 2 * v(-1, 0) + v(-1, 1));
        let gy = (v(-1, 1) + 2 * v(0, 1) + v(1, 1)) - (v(-1, -1) + 2 * v(0, -1) + v(1, -1));
        *slot = (gx.abs() + gy.abs()).min(255) as u8;
    }
    out[3] = source[sample(0, 0) + 3];
    out
}

/// The normalised binomial kernel that stands in for a Gaussian of
/// standard deviation `sigma` pixels: Pascal's triangle row `2n` with
/// `n = 2·sigma²`, whose variance is exactly `n/2 = sigma²`, cut off at
/// `3·sigma` taps each side (the tails beyond hold well under 0.3 % of the
/// weight) and renormalised — the discrete Gaussian of the textbooks.
/// Returned as the `2·half + 1` weights with the centre in the middle.
/// Built outward from the centre by the ratio
/// `C(2n, n+k+1) / C(2n, n+k) = (n − k) / (n + k + 1)`, so nothing
/// overflows however large `n` grows; tails that underflow to zero simply
/// drop out. `sigma = 1` gives exactly `[1 4 6 4 1] / 16`.
fn binomial_weights(sigma: u32) -> Vec<f64> {
    let n = 2 * (sigma as i64) * (sigma as i64);
    let half = (3 * sigma as i64).min(n);
    let mut side = Vec::with_capacity(half as usize + 1);
    let mut w = 1.0f64;
    side.push(w);
    for k in 0..half {
        w *= (n - k) as f64 / (n + k + 1) as f64;
        side.push(w);
    }
    let total = side[0] + 2.0 * side[1..].iter().sum::<f64>();
    side[1..]
        .iter()
        .rev()
        .chain(side.iter())
        .map(|w| w / total)
        .collect()
}

/// A tiny, dependency-free pseudo-random generator (Marsaglia's
/// xorshift32) for [`Document::add_noise`]. Chosen over pulling in the
/// `rand` crate for one reason that matters more than statistical
/// quality here: it is *fully deterministic for a given seed*, so a test
/// can seed it, work out the first few draws by hand (or in a separate
/// script), and assert the exact bytes the filter produces — the same
/// "hand-verified expected values" bar every other filter in this
/// project meets. Photoshop's own Add Noise is deliberately different on
/// every run; the frontend gets the same effect by sending a fresh seed
/// each time, so determinism lives in the tests, not in the UI.
struct XorShift32 {
    state: u32,
}

impl XorShift32 {
    /// xorshift's one hard rule is a nonzero state (zero is a fixed
    /// point), so a zero seed is swapped for an arbitrary constant rather
    /// than producing a generator stuck on 0 forever.
    fn new(seed: u32) -> Self {
        Self {
            state: if seed == 0 { 0x9E37_79B9 } else { seed },
        }
    }

    fn next_u32(&mut self) -> u32 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.state = x;
        x
    }

    /// A draw mapped onto `-1.0..=1.0`.
    fn next_unit(&mut self) -> f32 {
        (self.next_u32() as f32 / u32::MAX as f32) * 2.0 - 1.0
    }
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

    /// Layer > New > Layer via Copy (Ctrl/Cmd+J on a selection): copies
    /// layer `id`'s selected pixels (or the whole layer, with none, same
    /// as [`Self::copy`]) straight onto a new top layer, without going
    /// through the clipboard at all — unlike copying and pasting through
    /// Edit's own menu items, this never touches, or is affected by,
    /// whatever the user already had copied. Exactly [`Self::copy`] then
    /// [`Self::paste`] composed together locally; succeeds on a locked
    /// layer, since nothing about the source layer is written.
    pub fn new_layer_via_copy(
        &mut self,
        id: LayerId,
        name: impl Into<String>,
    ) -> Result<LayerId, String> {
        let clipboard = self.copy(id)?;
        Ok(self.paste(&clipboard, name))
    }

    /// Layer > New > Layer via Cut (Ctrl/Cmd+Shift+J on a selection):
    /// [`Self::new_layer_via_copy`], but the selected pixels are removed
    /// from the source layer first, the same "copy, then delete" relation
    /// [`Self::cut`] has to [`Self::copy`] — and, like `cut`, this errors
    /// (rather than silently doing nothing) if the source layer is
    /// locked, since it does write to it.
    pub fn new_layer_via_cut(
        &mut self,
        id: LayerId,
        name: impl Into<String>,
    ) -> Result<(LayerId, Option<Rect>), String> {
        let (clipboard, rect) = self.cut(id)?;
        let new_id = self.paste(&clipboard, name);
        Ok((new_id, rect))
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
                let averaged = box_blur_at(&source, doc_width, width, height, row, col, r);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&averaged);
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Sharpen > Unsharp Mask: the classic "subtract a blurred
    /// copy from the original, then add that difference back in,
    /// amplified" sharpen, built directly on [`box_blur_at`] (the same
    /// clamp-to-edge box-blur sampling [`Self::box_blur`] itself uses) as
    /// the "blurred copy" — this app's box blur *is* its unsharp mask's
    /// low-pass filter, not a separate Gaussian one, the same well-scoped
    /// simplification `box_blur` itself made relative to Photoshop's own
    /// Gaussian-based filters. For every pixel in the active selection (or
    /// the whole layer, with none): `diff = original - blurred` on each of
    /// the R, G, and B channels (alpha is left untouched — sharpening is a
    /// contrast operation, not a transparency one); if `|diff|` is at
    /// least `threshold`, the output is `original + diff * amount`,
    /// clamped to `0..=255`; otherwise the pixel is left exactly as it
    /// was, which is `threshold`'s whole job — protecting flat, low-
    /// contrast areas (skin, sky) from picking up sharpening noise while
    /// real edges (where `|diff|` is large) still get boosted. `amount`
    /// is a plain multiplier here rather than Photoshop's 1-500% dial;
    /// `1.0` corresponds to a nominal "100%". Errors on a zero radius, a
    /// non-finite or non-positive `amount`, or a locked/unknown layer.
    pub fn unsharp_mask(
        &mut self,
        id: LayerId,
        radius: u32,
        amount: f32,
        threshold: u8,
    ) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Sharpen radius must be at least 1 pixel.".to_string());
        }
        if !amount.is_finite() || amount <= 0.0 {
            return Err(format!(
                "Sharpen amount must be a positive number, got {amount}."
            ));
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
        let threshold = threshold as i32;
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let blurred = box_blur_at(&source, doc_width, width, height, row, col, r);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                for c in 0..3 {
                    let original = source[dst + c] as i32;
                    let diff = original - blurred[c] as i32;
                    layer.pixels[dst + c] = if diff.abs() < threshold {
                        source[dst + c]
                    } else {
                        (original as f32 + diff as f32 * amount)
                            .round()
                            .clamp(0.0, 255.0) as u8
                    };
                }
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Blur > Blur: Photoshop's one-click "just soften it a
    /// touch" preset — a [`Self::box_blur`] at the smallest possible
    /// radius (1, a 3x3 window), no dialog. Photoshop's own Blur is a
    /// fixed 3x3 kernel with a slightly heavier centre weight; this app's
    /// flat 3x3 mean is the same "one pixel of softening" at the same
    /// scale, the same flat-versus-weighted simplification `box_blur`
    /// itself already makes.
    pub fn blur(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.box_blur(id, 1)
    }

    /// Filter > Blur > Blur More: Photoshop documents this as "three to
    /// four times stronger than Blur" — here a [`Self::box_blur`] of
    /// radius 3 (a 7x7 window), i.e. three pixels of softening on every
    /// side instead of one.
    pub fn blur_more(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.box_blur(id, 3)
    }

    /// Filter > Sharpen > Sharpen: the one-click counterpart of
    /// [`Self::unsharp_mask`] — radius 1, a gentle 50% amount, and no
    /// threshold, so every pixel gets a light contrast boost against its
    /// immediate neighbours.
    pub fn sharpen(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.unsharp_mask(id, 1, 0.5, 0)
    }

    /// Filter > Sharpen > Sharpen More: [`Self::sharpen`] at double the
    /// strength (100% amount), matching Photoshop's own "a stronger
    /// Sharpen" description.
    pub fn sharpen_more(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.unsharp_mask(id, 1, 1.0, 0)
    }

    /// Filter > Sharpen > Sharpen Edges: Photoshop's "sharpen only where
    /// there's a real edge, leave smooth areas alone" preset. That is
    /// precisely what [`Self::unsharp_mask`]'s threshold is for, so this
    /// is Sharpen More's strength gated behind a threshold of 20 — a
    /// pixel has to differ from its blurred surroundings by at least 20
    /// levels (out of 255) on a channel before that channel is touched.
    pub fn sharpen_edges(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.unsharp_mask(id, 1, 1.0, 20)
    }

    /// Filter > Noise > Median: replaces every channel of each pixel in
    /// the active selection (or the whole layer, with none) with the
    /// median of its `(2*radius+1)`-square neighbourhood — see
    /// [`median_at`] for why a median removes specks without softening
    /// edges the way [`Self::box_blur`]'s mean does. Same edge-clamping,
    /// same pre-pass snapshot (so already-filtered pixels never feed
    /// later ones), same "all four channels, un-premultiplied" scope cut
    /// as the blur filters. Errors on a zero radius or a locked/unknown
    /// layer.
    pub fn median(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        self.dust_and_scratches(id, radius, 0)
    }

    /// Filter > Noise > Despeckle: Photoshop's one-click "remove the
    /// specks, keep the edges" filter, described in its own docs as
    /// detecting edges and blurring everything except them. A radius-1
    /// (3x3) median is the textbook implementation of exactly that
    /// behaviour — outliers vanish, edges survive — so this is
    /// [`Self::median`] at radius 1, with no dialog.
    pub fn despeckle(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        self.median(id, 1)
    }

    /// Filter > Noise > Dust & Scratches: [`Self::median`] with
    /// Photoshop's Threshold control — a channel is only replaced by its
    /// neighbourhood median when it differs from that median by at
    /// least `threshold` levels, so a genuine speck (which differs a
    /// lot) is removed while fine, low-contrast texture (which differs
    /// only slightly) is left alone. A threshold of 0 replaces every
    /// pixel and is exactly `median`, which is why `median` is
    /// implemented on top of this rather than the other way round.
    /// Errors on a zero radius or a locked/unknown layer.
    pub fn dust_and_scratches(
        &mut self,
        id: LayerId,
        radius: u32,
        threshold: u8,
    ) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Median radius must be at least 1 pixel.".to_string());
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
        let threshold = threshold as i32;
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let median = median_at(&source, doc_width, width, height, row, col, r);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                for (c, &m) in median.iter().enumerate() {
                    let original = source[dst + c];
                    if (original as i32 - m as i32).abs() >= threshold {
                        layer.pixels[dst + c] = m;
                    }
                }
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Noise > Add Noise: perturbs every pixel in the active
    /// selection (or the whole layer, with none) by a random offset of
    /// up to `amount * 255` levels, drawn from a [`XorShift32`] seeded
    /// with `seed`. Photoshop's three controls map directly: `amount`
    /// is its Amount dial as a fraction of the full range (Photoshop
    /// shows 0.1–400%; `1.0` here is 100%); `gaussian` selects its
    /// Gaussian distribution instead of Uniform, approximated as the
    /// mean of three uniform draws (an Irwin–Hall bell curve — the same
    /// "close enough, no extra maths" simplification `box_blur` makes
    /// versus a true Gaussian kernel); `monochromatic` applies one
    /// offset to R, G, and B together so the grain is grey rather than
    /// coloured. Alpha is never touched: noise is a colour effect, not a
    /// transparency one. Draws are consumed in row-major order over the
    /// selection's bounding box, skipping pixels the selection excludes
    /// (which consume nothing), one draw per channel — or per pixel when
    /// monochromatic, or three per channel/pixel when Gaussian — so the
    /// exact output for a seed is fully specified and testable. Errors
    /// on a non-finite or non-positive `amount` or a locked/unknown
    /// layer.
    pub fn add_noise(
        &mut self,
        id: LayerId,
        amount: f32,
        gaussian: bool,
        monochromatic: bool,
        seed: u32,
    ) -> Result<Option<Rect>, String> {
        if !amount.is_finite() || amount <= 0.0 {
            return Err(format!(
                "Noise amount must be a positive number, got {amount}."
            ));
        }
        let bounds = self.copy_bounds();
        let selection = self.selection;
        let doc_width = self.width as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let mut rng = XorShift32::new(seed);
        let draw = |rng: &mut XorShift32| {
            if gaussian {
                (rng.next_unit() + rng.next_unit() + rng.next_unit()) / 3.0
            } else {
                rng.next_unit()
            }
        };
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                let shared = if monochromatic {
                    Some(draw(&mut rng))
                } else {
                    None
                };
                for c in 0..3 {
                    let unit = shared.unwrap_or_else(|| draw(&mut rng));
                    let offset = unit * amount * 255.0;
                    layer.pixels[dst + c] = (layer.pixels[dst + c] as f32 + offset)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                }
            }
        }
        Ok(Some(bounds))
    }

    /// Image > Adjustments > Equalize: redistributes each channel's values
    /// so its histogram is as flat as possible — the darkest level present
    /// becomes 0, the brightest 255, and every level in between lands
    /// where its cumulative share of the pixels puts it. Classic histogram
    /// equalisation, per channel, via a 256-entry lookup table:
    /// `out(v) = round((cdf(v) - cdf_min) / (n - cdf_min) * 255)`, where
    /// `cdf(v)` counts sampled pixels at or below `v`, `cdf_min` is the
    /// count at the darkest populated level, and `n` is the number of
    /// sampled pixels. A channel with a single value everywhere
    /// (`cdf_min == n`) has nothing to spread and is left unchanged
    /// rather than dividing by zero. R, G, and B are equalised
    /// independently (Photoshop's own Equalize is likewise per-channel);
    /// alpha is untouched.
    ///
    /// With a selection active, Photoshop asks which of two things you
    /// meant, and `entire_image` answers it: `false` is "Equalize
    /// selected area only" (the histogram is built from the selected
    /// pixels and only they are remapped), `true` is "Equalize entire
    /// image based on selected area" (the same selection-built table,
    /// applied to every pixel of the layer). With no selection both are
    /// the plain menu command: whole layer in, whole layer out. Errors
    /// on a locked/unknown layer.
    pub fn equalize(&mut self, id: LayerId, entire_image: bool) -> Result<Option<Rect>, String> {
        let selection = self.selection;
        let doc_width = self.width as usize;
        let sample_bounds = self.copy_bounds();
        let full = Rect {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        };
        let remap_everything = entire_image || selection.is_none();
        let target_bounds = if remap_everything {
            full
        } else {
            sample_bounds
        };
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }

        let mut histogram = [[0u32; 256]; 3];
        let mut sampled = 0u32;
        for row in sample_bounds.y0..sample_bounds.y1 {
            for col in sample_bounds.x0..sample_bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                sampled += 1;
                let base = (row as usize * doc_width + col as usize) * CHANNELS;
                for (c, counts) in histogram.iter_mut().enumerate() {
                    counts[layer.pixels[base + c] as usize] += 1;
                }
            }
        }

        let mut lut = [[0u8; 256]; 3];
        for (table, counts) in lut.iter_mut().zip(&histogram) {
            let cdf_min = counts.iter().copied().find(|&count| count > 0).unwrap_or(0);
            let spread = sampled.saturating_sub(cdf_min);
            let mut cdf = 0u32;
            for (value, slot) in table.iter_mut().enumerate() {
                cdf += counts[value];
                *slot = if spread == 0 {
                    value as u8
                } else {
                    (cdf.saturating_sub(cdf_min) as f32 / spread as f32 * 255.0).round() as u8
                };
            }
        }

        for row in target_bounds.y0..target_bounds.y1 {
            for col in target_bounds.x0..target_bounds.x1 {
                if !remap_everything {
                    let keep =
                        selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                    if !keep {
                        continue;
                    }
                }
                let base = (row as usize * doc_width + col as usize) * CHANNELS;
                for (c, table) in lut.iter().enumerate() {
                    layer.pixels[base + c] = table[layer.pixels[base + c] as usize];
                }
            }
        }
        Ok(Some(target_bounds))
    }

    /// Filter > Other > Maximum: every channel of each selected pixel
    /// becomes the largest value of that channel within `radius` — the
    /// morphological dilate, which spreads light areas into dark ones
    /// (Photoshop suggests it for choking a mask's dark edges). See
    /// [`extreme_at`]. Same edge clamping, pre-pass snapshot, selection
    /// confinement, and error conditions as [`Self::box_blur`].
    pub fn maximum(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        self.extreme_filter(id, radius, true)
    }

    /// Filter > Other > Minimum: the counterpart of [`Self::maximum`] —
    /// every channel becomes the smallest value within `radius`, the
    /// morphological erode, spreading dark areas into light ones.
    pub fn minimum(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        self.extreme_filter(id, radius, false)
    }

    fn extreme_filter(
        &mut self,
        id: LayerId,
        radius: u32,
        want_max: bool,
    ) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Radius must be at least 1 pixel.".to_string());
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
                let picked =
                    extreme_at(&source, doc_width, (width, height), (row, col), r, want_max);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&picked);
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Other > High Pass: keeps only each pixel's deviation from
    /// its local average, centred on mid-grey — `out = original -
    /// box_blurred + 128` per colour channel, clamped. Flat areas come
    /// out a uniform 128 and only edges and fine detail survive, which is
    /// why Photoshop's High Pass is the usual starting point for
    /// "overlay-blend sharpening" and for finding detail before a
    /// Threshold. Built on [`box_blur_at`], so the "local average" is the
    /// same flat `(2*radius+1)`-square mean [`Self::box_blur`] uses rather
    /// than Photoshop's Gaussian — the same simplification `box_blur`
    /// itself makes. Alpha is untouched: it's a colour-detail filter, not
    /// a transparency one. Errors on a zero radius or a locked/unknown
    /// layer.
    pub fn high_pass(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("High Pass radius must be at least 1 pixel.".to_string());
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
                let blurred = box_blur_at(&source, doc_width, width, height, row, col, r);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                for c in 0..3 {
                    let v = source[dst + c] as i32 - blurred[c] as i32 + 128;
                    layer.pixels[dst + c] = v.clamp(0, 255) as u8;
                }
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Other > Offset: shifts the whole layer by `dx` pixels
    /// right and `dy` pixels down, with whatever slides off one edge
    /// wrapping back in on the opposite one — Photoshop's "Wrap Around"
    /// mode, the one that matters for making seamless tiles (shift by
    /// half the canvas and the old edges meet in the middle, where you
    /// can retouch the seam). Negative or oversized amounts are taken
    /// modulo the layer size, so `dx = -1` and `dx = width - 1` are the
    /// same shift and `dx = width` is a no-op. Photoshop's other two
    /// fill modes for the vacated area (Repeat Edge Pixels, Set to
    /// Transparent) and its confine-to-selection behaviour are
    /// deliberate scope cuts: this always moves the entire layer,
    /// selection ignored, the same whole-layer stance
    /// [`Self::flip_layer_horizontal`] takes. Errors on a locked/unknown
    /// layer.
    pub fn offset(&mut self, id: LayerId, dx: i32, dy: i32) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let source = layer.pixels.clone();
        for y in 0..height {
            let sy = (y - dy as i64).rem_euclid(height) as usize;
            for x in 0..width {
                let sx = (x - dx as i64).rem_euclid(width) as usize;
                let src = (sy * doc_width + sx) * CHANNELS;
                let dst = (y as usize * doc_width + x as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&source[src..src + CHANNELS]);
            }
        }
        Ok(Some(Rect {
            x0: 0,
            y0: 0,
            x1: self.width,
            y1: self.height,
        }))
    }

    /// Filter > Other > Custom: a user-supplied 5×5 convolution kernel
    /// with Photoshop's Scale (the divisor) and Offset (added after
    /// dividing), applied per colour channel by [`convolve_at`]. `kernel`
    /// is the 25 coefficients row by row with `kernel[12]` the centre;
    /// Photoshop accepts −999..=999 for each coefficient, 1..=9999 for
    /// Scale and −9999..=9999 for Offset, and the frontend's inputs use
    /// the same ranges. Every one of the classic kernels is a setting of
    /// this one dialog — the identity (a lone 1 in the middle), a box
    /// blur (nine 1s over a Scale of 9), the textbook sharpen (5 in the
    /// middle, −1 above, below, left and right), an emboss (−1 and +1 on
    /// a diagonal with an Offset of 128) — which is why it is the natural
    /// stepping stone to the Stylize filters. Loading and saving kernels
    /// to Photoshop's `.acf` files is a deliberate scope cut. Alpha is
    /// untouched. Honours the selection and the layer lock; errors on a
    /// zero Scale, which would divide by zero.
    pub fn custom(
        &mut self,
        id: LayerId,
        kernel: [i32; 25],
        scale: i32,
        offset: i32,
    ) -> Result<Option<Rect>, String> {
        if scale == 0 {
            return Err("Scale must not be zero.".to_string());
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
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let picked = convolve_at(
                    &source,
                    doc_width,
                    (width, height),
                    (row, col),
                    &kernel,
                    scale,
                    offset,
                );
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&picked);
            }
        }
        Ok(Some(bounds))
    }

    /// The shared skeleton of the per-pixel filters: snapshot the layer,
    /// then let `pick(source, row, col)` compute each selected pixel's new
    /// value from that untouched snapshot, so a filter never reads its own
    /// output. Confined to the selection, returns the dirty rect, and
    /// errors on a locked or unknown layer.
    fn filter_pixels(
        &mut self,
        id: LayerId,
        mut pick: impl FnMut(&[u8], u32, u32) -> [u8; CHANNELS],
    ) -> Result<Option<Rect>, String> {
        let bounds = self.copy_bounds();
        let selection = self.selection;
        let doc_width = self.width as usize;
        let layer = self.layer_mut(id)?;
        if layer.locked {
            return Err(format!("Layer \"{}\" is locked.", layer.name));
        }
        let source = layer.pixels.clone();
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let picked = pick(&source, row, col);
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&picked);
            }
        }
        Ok(Some(bounds))
    }

    /// Filter > Stylize > Find Edges: each colour channel becomes
    /// `255 − sobel`, the inverted [`sobel_at`] edge magnitude, so flat
    /// areas come out white and edges dark in the colour of whichever
    /// channel changed — the familiar "pencil sketch on white" look. No
    /// parameters, as in Photoshop. Alpha is untouched.
    pub fn find_edges(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        self.filter_pixels(id, |source, row, col| {
            let mut out = sobel_at(source, doc_width, (width, height), (row, col));
            for slot in out.iter_mut().take(3) {
                *slot = 255 - *slot;
            }
            out
        })
    }

    /// Filter > Stylize > Solarize: each colour channel becomes
    /// `min(v, 255 − v)` — the lower half of the range is left alone and
    /// the upper half is folded back down, the tent-shaped curve that
    /// mimics a print re-exposed to light mid-development. The whole
    /// result therefore sits in 0..=127, which is why the classic recipe
    /// follows Solarize with Auto Levels. Alpha is untouched.
    pub fn solarize(&mut self, id: LayerId) -> Result<Option<Rect>, String> {
        let doc_width = self.width as usize;
        self.filter_pixels(id, |source, row, col| {
            let base = (row as usize * doc_width + col as usize) * CHANNELS;
            let mut out = [0u8; CHANNELS];
            out.copy_from_slice(&source[base..base + CHANNELS]);
            for slot in out.iter_mut().take(3) {
                *slot = (*slot).min(255 - *slot);
            }
            out
        })
    }

    /// Filter > Stylize > Emboss: a relief map lit from `angle` degrees
    /// (0° is from the right, increasing anticlockwise like the Motion
    /// Blur dial, so Photoshop's default 135° lights from the upper left).
    /// Each colour channel becomes `128 + (away − toward) · amount / 100`,
    /// where `toward` is the sample `height` pixels from the pixel in the
    /// light's direction and `away` the sample the same distance the other
    /// way, both clamped to the layer. An edge whose bright side faces the
    /// light therefore reads light and its far side dark, the way a raised
    /// surface looks; flat areas come out mid-grey. `height` is in pixels
    /// (Photoshop 1..=100), `amount` in percent (Photoshop 1..=500).
    /// Sampling is nearest-neighbour, the same scope cut Motion Blur
    /// makes. Alpha is untouched. Errors on a zero height or amount or a
    /// non-finite angle.
    pub fn emboss(
        &mut self,
        id: LayerId,
        angle: f32,
        height: u32,
        amount: u32,
    ) -> Result<Option<Rect>, String> {
        if height == 0 {
            return Err("Emboss height must be at least 1 pixel.".to_string());
        }
        if amount == 0 {
            return Err("Emboss amount must be at least 1%.".to_string());
        }
        if !angle.is_finite() {
            return Err(format!("Emboss angle must be a number, got {angle}."));
        }
        let (width, layer_height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let (sin, cos) = angle.to_radians().sin_cos();
        let dx = (height as f32 * cos).round() as i64;
        let dy = -((height as f32 * sin).round() as i64);
        let amount = amount as i32;
        self.filter_pixels(id, |source, row, col| {
            let at = |sx: i64, sy: i64| {
                let x = sx.clamp(0, width - 1) as usize;
                let y = sy.clamp(0, layer_height - 1) as usize;
                (y * doc_width + x) * CHANNELS
            };
            let toward = at(col as i64 + dx, row as i64 + dy);
            let away = at(col as i64 - dx, row as i64 - dy);
            let centre = at(col as i64, row as i64);
            let mut out = [0u8; CHANNELS];
            for (c, slot) in out.iter_mut().enumerate().take(3) {
                let relief = source[away + c] as i32 - source[toward + c] as i32;
                *slot = (128 + relief * amount / 100).clamp(0, 255) as u8;
            }
            out[3] = source[centre + 3];
            out
        })
    }

    /// Filter > Stylize > Trace Contour: for each colour channel, draws
    /// the contour line where the channel crosses `level`. With `upper`
    /// false (Photoshop's "Lower" edge) the pixels *below* the level that
    /// touch — left, right, above or below — a pixel at or above it are
    /// marked; with `upper` true it is the pixels at or above the level
    /// touching one below. Marked channels become 0 and everything else
    /// 255, so a contour in one channel is a line in that channel's
    /// complementary colour on white, and a contour in all three is black.
    /// Neighbours past the layer edge clamp onto the pixel itself, so the
    /// border never counts as a crossing. Alpha is untouched.
    pub fn trace_contour(
        &mut self,
        id: LayerId,
        level: u8,
        upper: bool,
    ) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        self.filter_pixels(id, |source, row, col| {
            let at = |sx: i64, sy: i64| {
                let x = sx.clamp(0, width - 1) as usize;
                let y = sy.clamp(0, height - 1) as usize;
                (y * doc_width + x) * CHANNELS
            };
            let (x, y) = (col as i64, row as i64);
            let centre = at(x, y);
            let neighbours = [at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1)];
            let mut out = [255u8; CHANNELS];
            for (c, slot) in out.iter_mut().enumerate().take(3) {
                let above = |base: usize| source[base + c] >= level;
                let here = above(centre);
                if here == upper && neighbours.iter().any(|&n| above(n) != here) {
                    *slot = 0;
                }
            }
            out[3] = source[centre + 3];
            out
        })
    }

    /// Filter > Blur > Gaussian Blur: the bell-curve-weighted blur that is
    /// the workhorse of Photoshop's Blur menu, with `radius` playing the
    /// role of the standard deviation in pixels. The kernel is
    /// [`binomial_weights`]`(radius)` — the normalised binomial that
    /// approximates a Gaussian of that σ, cut at ±3σ — applied separably:
    /// first every row of the whole layer is blurred horizontally into a
    /// scratch buffer (the whole layer, not just the selection, because the
    /// second pass reads rows above and below the selected pixels), then
    /// each selected pixel is blurred vertically from that buffer. Each
    /// pass rounds to the nearest whole value and clamps its samples to the
    /// layer's edges like [`Self::box_blur`]. R, G, B and A are blurred
    /// independently and un-premultiplied, the same scope cut `box_blur`
    /// makes. Photoshop allows radii from 0.1 to 250 px; this takes whole
    /// pixels, 1 and up. Errors on a zero radius or a locked/unknown layer.
    pub fn gaussian_blur(&mut self, id: LayerId, radius: u32) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Gaussian blur radius must be at least 1 pixel.".to_string());
        }
        let weights = binomial_weights(radius);
        let half = (weights.len() / 2) as i64;
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let source = {
            let layer = self.layer_mut(id)?;
            if layer.locked {
                return Err(format!("Layer \"{}\" is locked.", layer.name));
            }
            layer.pixels.clone()
        };
        let round = |sum: &f64| sum.round().clamp(0.0, 255.0) as u8;
        let mut horizontal = source.clone();
        for y in 0..height {
            for x in 0..width {
                let mut acc = [0.0f64; CHANNELS];
                for (k, w) in weights.iter().enumerate() {
                    let sx = (x + k as i64 - half).clamp(0, width - 1) as usize;
                    let base = (y as usize * doc_width + sx) * CHANNELS;
                    for (sum, &v) in acc.iter_mut().zip(&source[base..base + CHANNELS]) {
                        *sum += w * v as f64;
                    }
                }
                let dst = (y as usize * doc_width + x as usize) * CHANNELS;
                for (out, sum) in horizontal[dst..dst + CHANNELS].iter_mut().zip(&acc) {
                    *out = round(sum);
                }
            }
        }
        self.filter_pixels(id, |_, row, col| {
            let mut acc = [0.0f64; CHANNELS];
            for (k, w) in weights.iter().enumerate() {
                let sy = (row as i64 + k as i64 - half).clamp(0, height - 1) as usize;
                let base = (sy * doc_width + col as usize) * CHANNELS;
                for (sum, &v) in acc.iter_mut().zip(&horizontal[base..base + CHANNELS]) {
                    *sum += w * v as f64;
                }
            }
            let mut out = [0u8; CHANNELS];
            for (slot, sum) in out.iter_mut().zip(&acc) {
                *slot = round(sum);
            }
            out
        })
    }

    /// Filter > Stylize > Diffuse: shuffles each pixel with one of its
    /// eight neighbours so hard edges soften into a grainy, out-of-focus
    /// texture. For every selected pixel in scan order, two seeded
    /// [`XorShift32`] draws pick a horizontal and a vertical offset in
    /// −1..=1 (`draw % 3 − 1`), clamped to the layer, and the pixel takes
    /// that neighbour's colour: always in `Normal`, only when the
    /// neighbour is darker (smaller R+G+B) in `DarkenOnly`, only when it is
    /// lighter in `LightenOnly`. `Anisotropic` uses no randomness at all:
    /// the pixel takes whichever in-bounds neighbour is closest in colour
    /// (smallest summed R, G, B difference; the first in scan order on a
    /// tie), which shuffles along edges rather than across them. Whole
    /// pixels move, alpha included, so a copied neighbour keeps its own
    /// transparency. Deterministic for a given seed and selection; the
    /// frontend sends a fresh seed per apply, as with Add Noise.
    pub fn diffuse(
        &mut self,
        id: LayerId,
        mode: DiffuseMode,
        seed: u32,
    ) -> Result<Option<Rect>, String> {
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let mut rng = XorShift32::new(seed);
        self.filter_pixels(id, |source, row, col| {
            let at = |x: i64, y: i64| (y as usize * doc_width + x as usize) * CHANNELS;
            let (x, y) = (col as i64, row as i64);
            let centre = at(x, y);
            let brightness =
                |base: usize| -> i32 { source[base..base + 3].iter().map(|&v| v as i32).sum() };
            let chosen = match mode {
                DiffuseMode::Anisotropic => {
                    let mut best: Option<(i32, usize)> = None;
                    for ny in (y - 1)..=(y + 1) {
                        for nx in (x - 1)..=(x + 1) {
                            let in_bounds = nx >= 0 && ny >= 0 && nx < width && ny < height;
                            if !in_bounds || (nx, ny) == (x, y) {
                                continue;
                            }
                            let base = at(nx, ny);
                            let distance: i32 = (0..3)
                                .map(|c| {
                                    (source[base + c] as i32 - source[centre + c] as i32).abs()
                                })
                                .sum();
                            match best {
                                Some((closest, _)) if distance >= closest => {}
                                _ => best = Some((distance, base)),
                            }
                        }
                    }
                    best.map_or(centre, |(_, base)| base)
                }
                _ => {
                    let dx = (rng.next_u32() % 3) as i64 - 1;
                    let dy = (rng.next_u32() % 3) as i64 - 1;
                    let neighbour = at((x + dx).clamp(0, width - 1), (y + dy).clamp(0, height - 1));
                    match mode {
                        DiffuseMode::Normal => neighbour,
                        DiffuseMode::DarkenOnly if brightness(neighbour) < brightness(centre) => {
                            neighbour
                        }
                        DiffuseMode::LightenOnly if brightness(neighbour) > brightness(centre) => {
                            neighbour
                        }
                        _ => centre,
                    }
                }
            };
            let mut out = [0u8; CHANNELS];
            out.copy_from_slice(&source[chosen..chosen + CHANNELS]);
            out
        })
    }

    /// Filter > Blur > Surface Blur: blurs flat areas while leaving edges
    /// sharp. Each colour channel becomes a weighted mean of the
    /// `(2·radius+1)`-square, edge-clamped window, where a neighbour's
    /// weight is `threshold − |neighbour − centre|` when that is positive
    /// and zero otherwise — so samples within `threshold` of the pixel's
    /// own value count in proportion to how close they are, and anything
    /// further away (the far side of an edge) is ignored entirely. The
    /// pixel itself always carries weight `threshold`, so the weights never
    /// sum to zero. The mean is rounded to the nearest whole value.
    /// Photoshop's Surface Blur is the same idea with the same two controls
    /// (Radius 1..=100, Threshold 2..=255); a threshold of 1 here admits
    /// only exact matches and so changes nothing. Alpha is untouched.
    /// Errors on a zero radius or threshold or a locked/unknown layer.
    pub fn surface_blur(
        &mut self,
        id: LayerId,
        radius: u32,
        threshold: u8,
    ) -> Result<Option<Rect>, String> {
        if radius == 0 {
            return Err("Surface blur radius must be at least 1 pixel.".to_string());
        }
        if threshold == 0 {
            return Err("Surface blur threshold must be at least 1.".to_string());
        }
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let r = radius as i64;
        let threshold = threshold as i64;
        self.filter_pixels(id, |source, row, col| {
            let centre = (row as usize * doc_width + col as usize) * CHANNELS;
            let mut out = [0u8; CHANNELS];
            out[3] = source[centre + 3];
            for (c, slot) in out.iter_mut().enumerate().take(3) {
                let own = source[centre + c] as i64;
                let (mut weighted, mut weights) = (0i64, 0i64);
                for dy in -r..=r {
                    let sy = (row as i64 + dy).clamp(0, height - 1) as usize;
                    for dx in -r..=r {
                        let sx = (col as i64 + dx).clamp(0, width - 1) as usize;
                        let v = source[(sy * doc_width + sx) * CHANNELS + c] as i64;
                        let w = (threshold - (v - own).abs()).max(0);
                        weighted += w * v;
                        weights += w;
                    }
                }
                *slot = ((weighted + weights / 2) / weights) as u8;
            }
            out
        })
    }

    /// Filter > Stylize > Glowing Edges: Find Edges' neon cousin — edges
    /// drawn bright on black instead of dark on white, then widened,
    /// brightened and softened by Photoshop's three controls. The pipeline
    /// runs over the whole layer into scratch buffers so every stage sees
    /// its neighbours: (1) the [`sobel_at`] edge magnitude per colour
    /// channel; (2) a maximum filter of radius `edge_width − 1` (the same
    /// [`extreme_at`] Maximum uses), so a one-pixel edge becomes
    /// `2·edge_width − 1` pixels wide — width 1 is no dilation; (3) each
    /// value scaled by `edge_brightness / 5`, truncated and clamped, so
    /// brightness 5 is the raw magnitude, 0 is black and Photoshop's
    /// default 6 lifts it by a fifth; (4) a box blur of radius
    /// `smoothness − 1` (the same [`box_blur_at`] Box Blur uses) —
    /// smoothness 1 is none. Only the selected pixels are written, from the
    /// final buffer, and alpha is untouched. Photoshop's ranges are Edge
    /// Width 1..=14, Edge Brightness 0..=20, Smoothness 1..=15. Errors on a
    /// zero width or smoothness or a locked/unknown layer.
    pub fn glowing_edges(
        &mut self,
        id: LayerId,
        edge_width: u32,
        edge_brightness: u32,
        smoothness: u32,
    ) -> Result<Option<Rect>, String> {
        if edge_width == 0 {
            return Err("Edge width must be at least 1 pixel.".to_string());
        }
        if smoothness == 0 {
            return Err("Smoothness must be at least 1.".to_string());
        }
        let (width, height) = (self.width as i64, self.height as i64);
        let doc_width = self.width as usize;
        let source = {
            let layer = self.layer_mut(id)?;
            if layer.locked {
                return Err(format!("Layer \"{}\" is locked.", layer.name));
            }
            layer.pixels.clone()
        };
        let mut stage = source.clone();
        for row in 0..self.height {
            for col in 0..self.width {
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                let edge = sobel_at(&source, doc_width, (width, height), (row, col));
                stage[dst..dst + CHANNELS].copy_from_slice(&edge);
            }
        }
        if edge_width > 1 {
            let edges = stage.clone();
            let radius = edge_width as i64 - 1;
            for row in 0..self.height {
                for col in 0..self.width {
                    let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                    let widened =
                        extreme_at(&edges, doc_width, (width, height), (row, col), radius, true);
                    stage[dst..dst + 3].copy_from_slice(&widened[..3]);
                }
            }
        }
        for px in stage.chunks_mut(CHANNELS) {
            for v in &mut px[..3] {
                *v = (*v as u32 * edge_brightness / 5).min(255) as u8;
            }
        }
        let glow = stage;
        let blur_radius = smoothness as i64 - 1;
        self.filter_pixels(id, |original, row, col| {
            let base = (row as usize * doc_width + col as usize) * CHANNELS;
            let mut out = if blur_radius > 0 {
                box_blur_at(&glow, doc_width, width, height, row, col, blur_radius)
            } else {
                let mut px = [0u8; CHANNELS];
                px.copy_from_slice(&glow[base..base + CHANNELS]);
                px
            };
            out[3] = original[base + 3];
            out
        })
    }

    /// Filter > Blur > Motion Blur: like [`Self::box_blur`], but instead
    /// of averaging a square neighbourhood, it averages a straight line of
    /// samples through each pixel, along `angle` degrees (0° is
    /// horizontal, increasing anticlockwise, matching Photoshop's own
    /// dial) — built on [`motion_blur_at`], the directional counterpart of
    /// [`box_blur_at`]. `distance` behaves like `box_blur`'s own `radius`
    /// parameter rather than Photoshop's single "total streak length"
    /// number: it's how far the sampled line extends on *each side* of
    /// the pixel, so the streak is `2 * distance + 1` samples long — the
    /// same "close enough, not a pixel-for-pixel port of Photoshop's own
    /// dialog maths" simplification `box_blur`'s own `radius` already
    /// makes relative to Photoshop's blur filters. Off-angle samples land
    /// on the nearest whole pixel rather than being anti-aliased between
    /// two — a documented limitation, not a bug, consistent with this
    /// project's hard-edged selection system. Errors on a zero distance or
    /// a locked/unknown layer.
    pub fn motion_blur(
        &mut self,
        id: LayerId,
        angle: f32,
        distance: u32,
    ) -> Result<Option<Rect>, String> {
        if distance == 0 {
            return Err("Motion blur distance must be at least 1 pixel.".to_string());
        }
        if !angle.is_finite() {
            return Err(format!("Motion blur angle must be a number, got {angle}."));
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
        let radians = angle.to_radians();
        let (sin, cos) = radians.sin_cos();
        let half = distance as i64;
        for row in bounds.y0..bounds.y1 {
            for col in bounds.x0..bounds.x1 {
                let keep =
                    selection.map_or(true, |s| s.contains(col as f32 + 0.5, row as f32 + 0.5));
                if !keep {
                    continue;
                }
                let averaged = motion_blur_at(
                    &source,
                    doc_width,
                    (width, height),
                    (row, col),
                    (cos, sin),
                    half,
                );
                let dst = (row as usize * doc_width + col as usize) * CHANNELS;
                layer.pixels[dst..dst + CHANNELS].copy_from_slice(&averaged);
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

    /// Layer > Duplicate Layer: adds a copy of layer `id`'s pixels and
    /// attributes (visibility, opacity, blend mode, lock state) as a new
    /// layer directly above the original — Photoshop's own placement, not
    /// necessarily the very top of the stack (see [`Self::add_layer`] and
    /// friends for "always on top" instead). The duplicate's name is the
    /// original's with " copy" appended, the same convention Photoshop
    /// itself uses before any renaming. Clones the whole [`Layer`] rather
    /// than listing its fields out by hand, so a future field added to
    /// `Layer` is duplicated correctly by construction. Errors only if
    /// `id` is unknown; duplicating a locked layer is fine (the new layer
    /// starts out locked too, matching the original — nothing about the
    /// original is touched either way).
    pub fn duplicate_layer(&mut self, id: LayerId) -> Result<LayerId, String> {
        let index = self.index_of(id)?;
        let mut duplicate = self.layers[index].clone();
        let new_id = self.next_id;
        self.next_id += 1;
        duplicate.id = new_id;
        duplicate.name = format!("{} copy", duplicate.name);
        self.layers.insert(index + 1, duplicate);
        Ok(new_id)
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
    fn new_layer_via_copy_adds_the_selected_pixels_as_a_new_layer_and_leaves_the_source_alone() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [4, 5, 6, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let new_id = doc.new_layer_via_copy(id, "Layer via Copy").unwrap();

        assert_ne!(new_id, id);
        assert_eq!(doc.layers().len(), 2);
        let new_layer = doc.layers().iter().find(|l| l.id == new_id).unwrap();
        assert_eq!(new_layer.name, "Layer via Copy");
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(&new_layer.pixels[idx(0, 0)..idx(0, 0) + 4], &[0, 0, 0, 0]);
        assert_eq!(&new_layer.pixels[idx(1, 1)..idx(1, 1) + 4], &[4, 5, 6, 255]);
        // The source layer is completely untouched -- this is Copy, not Cut.
        let source = doc.layers().iter().find(|l| l.id == id).unwrap();
        assert_eq!(source.pixels, solid(3, 3, [4, 5, 6, 255]));
    }

    #[test]
    fn new_layer_via_copy_succeeds_on_a_locked_layer() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        assert!(doc.new_layer_via_copy(id, "Layer via Copy").is_ok());
    }

    #[test]
    fn new_layer_via_copy_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.new_layer_via_copy(999, "Layer via Copy").is_err());
    }

    #[test]
    fn new_layer_via_cut_adds_the_selected_pixels_as_a_new_layer_and_clears_the_source() {
        let mut doc = Document::new(3, 3).unwrap();
        let id = doc
            .add_layer("base", &solid(3, 3, [4, 5, 6, 255]), 3, 3)
            .unwrap();
        doc.select_rectangle(1.0, 1.0, 3.0, 3.0).unwrap();

        let (new_id, rect) = doc.new_layer_via_cut(id, "Layer via Cut").unwrap();

        assert_eq!(
            rect,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 3,
                y1: 3
            })
        );
        let new_layer = doc.layers().iter().find(|l| l.id == new_id).unwrap();
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(&new_layer.pixels[idx(1, 1)..idx(1, 1) + 4], &[4, 5, 6, 255]);
        // Unlike Copy, the source layer's selected region is now transparent.
        let source = doc.layers().iter().find(|l| l.id == id).unwrap();
        assert_eq!(&source.pixels[idx(0, 0)..idx(0, 0) + 4], &[4, 5, 6, 255]);
        assert_eq!(&source.pixels[idx(1, 1)..idx(1, 1) + 4], &[0, 0, 0, 0]);
    }

    #[test]
    fn new_layer_via_cut_errors_on_a_locked_layer_and_leaves_it_untouched() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();

        let err = doc.new_layer_via_cut(id, "Layer via Cut").unwrap_err();

        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers().len(), 1);
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn new_layer_via_cut_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.new_layer_via_cut(999, "Layer via Cut").is_err());
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
    fn unsharp_mask_boosts_contrast_using_the_same_box_blur_as_its_low_pass() {
        let (mut doc, id) = ramped_3x3();

        doc.unsharp_mask(id, 1, 0.5, 0).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Centre: blurred value (50) equals the original, so diff is zero
        // and the pixel is unchanged.
        assert_eq!(pixels[idx(1, 1)], 50);
        // Top-left corner: original 10, box-blurred 23 (same hand-derived
        // value as the box_blur test above), diff = -13, sharpened =
        // 10 + (-13 * 0.5) = 3.5, which rounds (half away from zero) to 4.
        assert_eq!(pixels[idx(0, 0)], 4);
        // Bottom-right corner: original 90, box-blurred 76, diff = 14,
        // sharpened = 90 + (14 * 0.5) = 97 exactly.
        assert_eq!(pixels[idx(2, 2)], 97);
        // Alpha is a transparency channel, not a contrast one: sharpening
        // never touches it, so the uniformly-255 alpha survives untouched.
        assert_eq!(pixels[idx(0, 0) + 3], 255);
    }

    #[test]
    fn unsharp_mask_threshold_protects_low_contrast_pixels() {
        let (mut doc, id) = ramped_3x3();

        // Both corners' |diff| (13 and 14) are below this threshold, so
        // with a high enough amount to prove it isn't just doing nothing,
        // neither should move from its original value.
        doc.unsharp_mask(id, 1, 1.0, 20).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(pixels[idx(0, 0)], 10);
        assert_eq!(pixels[idx(2, 2)], 90);
    }

    #[test]
    fn unsharp_mask_is_confined_to_the_selection() {
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap(); // just the top-left pixel

        doc.unsharp_mask(id, 1, 1.0, 0).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // diff = 10 - 23 = -13, sharpened = 10 + (-13 * 1.0) = -3, clamped to 0.
        assert_eq!(pixels[idx(0, 0)], 0);
        // Everywhere outside the selection is untouched.
        assert_eq!(pixels[idx(1, 1)], 50);
        assert_eq!(pixels[idx(2, 2)], 90);
        assert_eq!(pixels[idx(1, 0)], 20);
    }

    #[test]
    fn unsharp_mask_with_zero_radius_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        let err = doc.unsharp_mask(id, 0, 1.0, 0).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn unsharp_mask_rejects_a_non_positive_or_non_finite_amount() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.unsharp_mask(id, 1, 0.0, 0).is_err());
        assert!(doc.unsharp_mask(id, 1, -1.0, 0).is_err());
        assert!(doc.unsharp_mask(id, 1, f32::NAN, 0).is_err());
    }

    #[test]
    fn unsharp_mask_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.unsharp_mask(id, 1, 1.0, 0).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn unsharp_mask_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.unsharp_mask(999, 1, 1.0, 0).is_err());
    }

    #[test]
    fn motion_blur_at_zero_degrees_averages_along_the_row() {
        let (mut doc, id) = ramped_3x3();

        doc.motion_blur(id, 0.0, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Left edge repeats col 0: (10+10+20)/3 = 40/3 = 13 (truncating).
        assert_eq!(pixels[idx(0, 0)], 13);
        // Middle column's window is the whole row, symmetric around it:
        // (10+20+30)/3 = 20 exactly, its own original value.
        assert_eq!(pixels[idx(1, 0)], 20);
        // Right edge repeats col 2: (20+30+30)/3 = 80/3 = 26.
        assert_eq!(pixels[idx(2, 0)], 26);
    }

    #[test]
    fn motion_blur_at_ninety_degrees_averages_along_the_column() {
        let (mut doc, id) = ramped_3x3();

        doc.motion_blur(id, 90.0, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Same shape of maths as the horizontal test, down column 0 instead
        // of along row 0: (10+10+40)/3 = 20.
        assert_eq!(pixels[idx(0, 0)], 20);
        // Middle row: (10+40+70)/3 = 40 exactly.
        assert_eq!(pixels[idx(0, 1)], 40);
        // Bottom edge repeats row 2: (40+70+70)/3 = 60.
        assert_eq!(pixels[idx(0, 2)], 60);
    }

    #[test]
    fn motion_blur_is_confined_to_the_selection() {
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap(); // just the top-left pixel

        doc.motion_blur(id, 0.0, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(pixels[idx(0, 0)], 13);
        // Everywhere else on the same row (and elsewhere) is untouched.
        assert_eq!(pixels[idx(1, 0)], 20);
        assert_eq!(pixels[idx(2, 0)], 30);
        assert_eq!(pixels[idx(0, 1)], 40);
    }

    #[test]
    fn motion_blur_with_zero_distance_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        let err = doc.motion_blur(id, 0.0, 0).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn motion_blur_rejects_a_non_finite_angle() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.motion_blur(id, f32::NAN, 1).is_err());
    }

    #[test]
    fn motion_blur_on_a_locked_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.motion_blur(id, 0.0, 1).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn motion_blur_on_an_unknown_layer_is_an_error() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.motion_blur(999, 0.0, 1).is_err());
    }

    // The five one-click presets are thin wrappers, so each test pins the
    // preset to a value already hand-derived for the underlying filter at
    // those exact parameters (or derives the one new case, Blur More's
    // radius 3, by the same edge-clamping arithmetic).

    #[test]
    fn blur_is_a_radius_one_box_blur() {
        let (mut doc, id) = ramped_3x3();
        doc.blur(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Same corner value as box_blur_averages_a_neighbourhood_with_edge_clamping.
        assert_eq!(pixels[idx(0, 0)], 23);
        assert_eq!(pixels[idx(1, 1)], 50);
    }

    #[test]
    fn blur_more_is_a_radius_three_box_blur() {
        let (mut doc, id) = ramped_3x3();
        doc.blur_more(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Radius 3 on a 3x3 layer: for the top-left corner, offsets -3..=3
        // clamp to row/col 0 four times, 1 once, and 2 twice (weights
        // 4/1/2, summing to 7 per axis, 49 samples). With red = 10*(3r+c+1):
        // sum = 10 * (3*5*7 + 7*5 + 49) = 1890, and 1890/49 = 38.
        assert_eq!(pixels[idx(0, 0)], 38);
        // Centre: weights 3/1/3 per axis: 10 * (3*7*7 + 7*7 + 49) = 2450,
        // 2450/49 = 50 exactly — its own original value, by symmetry.
        assert_eq!(pixels[idx(1, 1)], 50);
    }

    #[test]
    fn sharpen_is_a_half_strength_unsharp_mask() {
        let (mut doc, id) = ramped_3x3();
        doc.sharpen(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Same values as unsharp_mask_boosts_contrast_using_the_same_box_blur_as_its_low_pass.
        assert_eq!(pixels[idx(0, 0)], 4);
        assert_eq!(pixels[idx(2, 2)], 97);
    }

    #[test]
    fn sharpen_more_is_a_full_strength_unsharp_mask() {
        let (mut doc, id) = ramped_3x3();
        doc.sharpen_more(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // 10 + (-13 * 1.0) = -3, clamped to 0; 90 + (14 * 1.0) = 104.
        assert_eq!(pixels[idx(0, 0)], 0);
        assert_eq!(pixels[idx(2, 2)], 104);
    }

    #[test]
    fn sharpen_edges_leaves_pixels_below_the_edge_threshold_alone() {
        let (mut doc, id) = ramped_3x3();
        doc.sharpen_edges(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Both corners' |diff| (13 and 14) sit under the threshold of 20,
        // so unlike Sharpen More they are untouched.
        assert_eq!(pixels[idx(0, 0)], 10);
        assert_eq!(pixels[idx(2, 2)], 90);
    }

    #[test]
    fn presets_propagate_the_underlying_filters_errors() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        assert!(doc.blur(id).is_err());
        assert!(doc.blur_more(id).is_err());
        assert!(doc.sharpen(id).is_err());
        assert!(doc.sharpen_more(id).is_err());
        assert!(doc.sharpen_edges(id).is_err());
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.blur(999).is_err());
        assert!(empty.sharpen_edges(999).is_err());
    }

    #[test]
    fn median_replaces_each_channel_with_its_neighbourhood_median() {
        let (mut doc, id) = ramped_3x3();

        doc.median(id, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Centre: all nine values 10..=90, middle (5th of 9) is 50.
        assert_eq!(pixels[idx(1, 1)], 50);
        // Top-left corner's edge-clamped window is the same nine samples
        // the box-blur test derives (10,10,20,10,10,20,40,40,50); sorted
        // that is 10,10,10,10,20,20,40,40,50 and the 5th is 20 — where the
        // mean gave 23, the median stays on an actual sampled value.
        assert_eq!(pixels[idx(0, 0)], 20);
        // Bottom-right: samples 50,60,60,80,90,90,80,90,90 sort to
        // 50,60,60,80,80,90,90,90,90 — 5th is 80 (the mean gave 76).
        assert_eq!(pixels[idx(2, 2)], 80);
        // Green/blue are uniformly 0 and alpha uniformly 255: a median of
        // identical samples is that sample, so both pass through exactly.
        assert_eq!(pixels[idx(0, 0) + 1], 0);
        assert_eq!(pixels[idx(0, 0) + 3], 255);
    }

    #[test]
    fn median_removes_an_isolated_speck_but_a_mean_would_only_dim_it() {
        // A flat 3x3 field of 100 with one 255 speck in the middle: the
        // median throws the outlier away entirely (100), whereas box_blur
        // would have smeared it to (8*100 + 255)/9 = 117.
        let mut doc = Document::new(3, 3).unwrap();
        let mut pixels = solid(3, 3, [100, 100, 100, 255]);
        let centre = |x: usize, y: usize| (y * 3 + x) * 4;
        pixels[centre(1, 1)] = 255;
        let id = doc.add_layer("speck", &pixels, 3, 3).unwrap();

        doc.median(id, 1).unwrap();

        assert_eq!(doc.layers()[0].pixels[centre(1, 1)], 100);
    }

    #[test]
    fn median_is_confined_to_the_selection() {
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap(); // just the top-left pixel

        doc.median(id, 1).unwrap();

        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(pixels[idx(0, 0)], 20);
        assert_eq!(pixels[idx(1, 0)], 20); // untouched original
        assert_eq!(pixels[idx(2, 2)], 90); // untouched original
    }

    #[test]
    fn median_with_zero_radius_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        let err = doc.median(id, 0).unwrap_err();
        assert!(err.contains("at least 1"), "{err}");
    }

    #[test]
    fn median_on_a_locked_or_unknown_layer_is_an_error() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        let err = doc.median(id, 1).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        assert!(doc.median(999, 1).is_err());
        assert!(doc.despeckle(999).is_err());
        assert!(doc.dust_and_scratches(999, 1, 0).is_err());
    }

    #[test]
    fn despeckle_is_a_radius_one_median() {
        let (mut doc, id) = ramped_3x3();
        doc.despeckle(id).unwrap();
        let pixels = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(pixels[idx(0, 0)], 20);
        assert_eq!(pixels[idx(2, 2)], 80);
    }

    #[test]
    fn dust_and_scratches_threshold_gates_replacement() {
        // Both corners differ from their median by exactly 10 levels
        // (10 vs 20, 90 vs 80): a threshold of 11 protects them, a
        // threshold of 10 (the boundary, inclusive) replaces them.
        let (mut doc, id) = ramped_3x3();
        doc.dust_and_scratches(id, 1, 11).unwrap();
        let pixels = doc.layers()[0].pixels.clone();
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(pixels[idx(0, 0)], 10);
        assert_eq!(pixels[idx(2, 2)], 90);

        let (mut doc, id) = ramped_3x3();
        doc.dust_and_scratches(id, 1, 10).unwrap();
        let pixels = &doc.layers()[0].pixels;
        assert_eq!(pixels[idx(0, 0)], 20);
        assert_eq!(pixels[idx(2, 2)], 80);
    }

    // Add Noise: every expected byte below comes from the first draws of
    // xorshift32 seeded with 1 — 270369, 67634689, 2647435461, ... —
    // mapped to [-1, 1] (-0.99987, -0.96851, +0.23281, -0.85676, +0.11698,
    // -0.65285, -0.70550, -0.79709, -0.06618, ...), scaled by amount*255
    // and added to a flat 128 grey. The generator's own first outputs are
    // pinned separately so a change to either half shows up on its own.

    #[test]
    fn xorshift32_produces_the_documented_sequence_and_survives_a_zero_seed() {
        let mut rng = XorShift32::new(1);
        assert_eq!(rng.next_u32(), 270_369);
        assert_eq!(rng.next_u32(), 67_634_689);
        assert_eq!(rng.next_u32(), 2_647_435_461);
        let mut zero = XorShift32::new(0);
        assert_ne!(zero.next_u32(), 0);
    }

    fn grey_2x2() -> (Document, LayerId) {
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc
            .add_layer("grey", &solid(2, 2, [128, 128, 128, 255]), 2, 2)
            .unwrap();
        (doc, id)
    }

    #[test]
    fn add_noise_uniform_colour_matches_the_seeded_draws_exactly() {
        let (mut doc, id) = grey_2x2();
        doc.add_noise(id, 0.25, false, false, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        // Pixel 0 consumes draws 1-3: 128 + (-63.74, -61.74, +14.84) rounded.
        assert_eq!(&p[0..4], &[64, 66, 143, 255]);
        // Pixel 1 consumes draws 4-6: 128 + (-54.62, +7.46, -41.62).
        assert_eq!(&p[4..8], &[73, 135, 86, 255]);
        // Pixel 2 consumes draws 7-9: 128 + (-44.98, -50.81, -4.22).
        assert_eq!(&p[8..12], &[83, 77, 124, 255]);
    }

    #[test]
    fn add_noise_monochromatic_applies_one_draw_to_all_three_channels() {
        let (mut doc, id) = grey_2x2();
        doc.add_noise(id, 0.25, false, true, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(&p[0..4], &[64, 64, 64, 255]);
        assert_eq!(&p[4..8], &[66, 66, 66, 255]);
        assert_eq!(&p[8..12], &[143, 143, 143, 255]);
    }

    #[test]
    fn add_noise_gaussian_averages_three_draws_per_channel() {
        let (mut doc, id) = grey_2x2();
        doc.add_noise(id, 0.25, true, false, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        // R = mean(draws 1-3) = -0.5785 -> 128 - 36.88; G = mean(4-6) =
        // -0.4642 -> 128 - 29.59; B = mean(7-9) = -0.5229 -> 128 - 33.34.
        assert_eq!(&p[0..4], &[91, 98, 95, 255]);
    }

    #[test]
    fn add_noise_clamps_to_the_byte_range() {
        let (mut doc, id) = grey_2x2();
        doc.add_noise(id, 1.0, false, false, 1).unwrap();
        // 128 - 255 and 128 - 247 both clamp to 0; 128 + 59 = 187.
        assert_eq!(&doc.layers()[0].pixels[0..4], &[0, 0, 187, 255]);
    }

    #[test]
    fn add_noise_is_deterministic_per_seed_and_differs_across_seeds() {
        let (mut a, ida) = grey_2x2();
        let (mut b, idb) = grey_2x2();
        let (mut c, idc) = grey_2x2();
        a.add_noise(ida, 0.5, true, false, 42).unwrap();
        b.add_noise(idb, 0.5, true, false, 42).unwrap();
        c.add_noise(idc, 0.5, true, false, 43).unwrap();
        assert_eq!(a.layers()[0].pixels, b.layers()[0].pixels);
        assert_ne!(a.layers()[0].pixels, c.layers()[0].pixels);
    }

    #[test]
    fn add_noise_is_confined_to_the_selection() {
        let (mut doc, id) = grey_2x2();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap(); // just pixel 0
        doc.add_noise(id, 0.25, false, false, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(&p[0..4], &[64, 66, 143, 255]);
        assert_eq!(&p[4..8], &[128, 128, 128, 255]);
        assert_eq!(&p[12..16], &[128, 128, 128, 255]);
    }

    #[test]
    fn add_noise_rejects_a_non_positive_or_non_finite_amount() {
        let (mut doc, id) = grey_2x2();
        assert!(doc.add_noise(id, 0.0, false, false, 1).is_err());
        assert!(doc.add_noise(id, -0.5, false, false, 1).is_err());
        assert!(doc.add_noise(id, f32::NAN, false, false, 1).is_err());
    }

    #[test]
    fn add_noise_on_a_locked_or_unknown_layer_is_an_error() {
        let (mut doc, id) = grey_2x2();
        doc.set_locked(id, true).unwrap();
        let err = doc.add_noise(id, 0.25, false, false, 1).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [128, 128, 128, 255]));
        assert!(doc.add_noise(999, 0.25, false, false, 1).is_err());
    }

    /// A 2x2 layer whose four pixels are the greys `values[0..4]` (R = G =
    /// B, alpha 255), for the equalize tests.
    fn greys_2x2(values: [u8; 4]) -> (Document, LayerId) {
        let mut doc = Document::new(2, 2).unwrap();
        let mut pixels = Vec::with_capacity(16);
        for v in values {
            pixels.extend_from_slice(&[v, v, v, 255]);
        }
        let id = doc.add_layer("greys", &pixels, 2, 2).unwrap();
        (doc, id)
    }

    fn reds(doc: &Document) -> [u8; 4] {
        let p = &doc.layers()[0].pixels;
        [p[0], p[4], p[8], p[12]]
    }

    #[test]
    fn equalize_spreads_distinct_values_evenly_across_the_full_range() {
        // Four distinct levels: cdf = 1,2,3,4, cdf_min = 1, so
        // out = (cdf-1)/3 * 255 = 0, 85, 170, 255.
        let (mut doc, id) = greys_2x2([10, 20, 30, 40]);
        doc.equalize(id, false).unwrap();
        assert_eq!(reds(&doc), [0, 85, 170, 255]);
        // Every channel got the same table, alpha stayed put.
        assert_eq!(&doc.layers()[0].pixels[4..8], &[85, 85, 85, 255]);
    }

    #[test]
    fn equalize_uses_the_cumulative_count_for_repeated_values() {
        // cdf(50) = 3 (= cdf_min), cdf(200) = 4: 50 -> 0, 200 -> 255.
        let (mut doc, id) = greys_2x2([50, 50, 50, 200]);
        doc.equalize(id, false).unwrap();
        assert_eq!(reds(&doc), [0, 0, 0, 255]);
    }

    #[test]
    fn equalize_leaves_a_single_valued_channel_unchanged() {
        let (mut doc, id) = greys_2x2([77, 77, 77, 77]);
        doc.equalize(id, false).unwrap();
        assert_eq!(reds(&doc), [77, 77, 77, 77]);
    }

    #[test]
    fn equalize_selected_area_only_samples_and_remaps_just_the_selection() {
        // Select column 0 (pixels 0 and 2, values 10 and 30): n = 2,
        // cdf(10) = 1 = cdf_min, cdf(30) = 2 -> 10 -> 0, 30 -> 255. The
        // unselected pixels (20, 40) are left exactly as they were.
        let (mut doc, id) = greys_2x2([10, 20, 30, 40]);
        doc.select_rectangle(0.0, 0.0, 1.0, 2.0).unwrap();
        let rect = doc.equalize(id, false).unwrap();
        assert_eq!(reds(&doc), [0, 20, 255, 40]);
        assert_eq!(
            rect,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 2
            })
        );
    }

    #[test]
    fn equalize_entire_image_based_on_selection_applies_the_selection_table_everywhere() {
        // Same selection-built table as above (cdf over {10, 30}), applied
        // to all four pixels: 20 sits above only 10 (cdf 1 -> 0), 40 above
        // both (cdf 2 -> 255).
        let (mut doc, id) = greys_2x2([10, 20, 30, 40]);
        doc.select_rectangle(0.0, 0.0, 1.0, 2.0).unwrap();
        let rect = doc.equalize(id, true).unwrap();
        assert_eq!(reds(&doc), [0, 0, 255, 255]);
        assert_eq!(
            rect,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 2,
                y1: 2
            })
        );
    }

    #[test]
    fn equalize_flag_makes_no_difference_without_a_selection() {
        let (mut a, ida) = greys_2x2([10, 20, 30, 40]);
        let (mut b, idb) = greys_2x2([10, 20, 30, 40]);
        a.equalize(ida, false).unwrap();
        b.equalize(idb, true).unwrap();
        assert_eq!(a.layers()[0].pixels, b.layers()[0].pixels);
    }

    #[test]
    fn equalize_on_a_locked_or_unknown_layer_is_an_error() {
        let (mut doc, id) = greys_2x2([10, 20, 30, 40]);
        doc.set_locked(id, true).unwrap();
        let err = doc.equalize(id, false).unwrap_err();
        assert!(err.contains("locked"), "{err}");
        assert_eq!(reds(&doc), [10, 20, 30, 40]);
        assert!(doc.equalize(999, false).is_err());
    }

    // Filter > Other, on the ramped 3x3 layer whose radius-1 windows are
    // already listed sample-by-sample in the box-blur and median tests.

    #[test]
    fn maximum_takes_the_neighbourhood_maximum() {
        let (mut doc, id) = ramped_3x3();
        doc.maximum(id, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Top-left window {10,10,20,10,10,20,40,40,50} -> 50; the centre
        // sees the whole grid -> 90; the top edge pixel (1,0) sees rows
        // 0,0,1 x cols 0,1,2 -> 60.
        assert_eq!(p[idx(0, 0)], 50);
        assert_eq!(p[idx(1, 1)], 90);
        assert_eq!(p[idx(1, 0)], 60);
        assert_eq!(p[idx(0, 0) + 3], 255);
    }

    #[test]
    fn minimum_takes_the_neighbourhood_minimum() {
        let (mut doc, id) = ramped_3x3();
        doc.minimum(id, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        assert_eq!(p[idx(0, 0)], 10);
        assert_eq!(p[idx(1, 1)], 10);
        // Bottom-right window {50,60,60,80,90,90,80,90,90} -> 50.
        assert_eq!(p[idx(2, 2)], 50);
    }

    #[test]
    fn maximum_and_minimum_are_confined_to_the_selection() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap();
        doc.maximum(id, 1).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(0, 0)], 50);
        assert_eq!(doc.layers()[0].pixels[idx(1, 0)], 20);

        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(2.0, 2.0, 3.0, 3.0).unwrap();
        doc.minimum(id, 1).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(2, 2)], 50);
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 50); // untouched original
    }

    #[test]
    fn high_pass_keeps_only_the_deviation_from_the_local_mean() {
        let (mut doc, id) = ramped_3x3();
        doc.high_pass(id, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Box-blurred values from the box-blur test: 23 / 50 / 76.
        assert_eq!(p[idx(0, 0)], 115); // 10 - 23 + 128
        assert_eq!(p[idx(1, 1)], 128);
        assert_eq!(p[idx(2, 2)], 142); // 90 - 76 + 128

        // A flat channel (green is 0 everywhere) becomes uniform mid-grey;
        // alpha is not a colour channel and stays put.
        assert_eq!(p[idx(0, 0) + 1], 128);
        assert_eq!(p[idx(0, 0) + 3], 255);
    }

    #[test]
    fn offset_wraps_pixels_around_the_layer() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.offset(id, 1, 0).unwrap();
        let p = &doc.layers()[0].pixels;
        // Each row rotates right by one: 10,20,30 -> 30,10,20.
        assert_eq!([p[idx(0, 0)], p[idx(1, 0)], p[idx(2, 0)]], [30, 10, 20]);

        let (mut doc, id) = ramped_3x3();
        doc.offset(id, 0, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        // Rows move down by one; the old bottom row wraps to the top.
        assert_eq!([p[idx(0, 0)], p[idx(1, 0)], p[idx(2, 0)]], [70, 80, 90]);
        assert_eq!([p[idx(0, 1)], p[idx(1, 1)], p[idx(2, 1)]], [10, 20, 30]);
    }

    #[test]
    fn offset_by_a_full_dimension_is_a_no_op_and_negative_amounts_wrap() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        let before = doc.layers()[0].pixels.clone();
        doc.offset(id, 3, -3).unwrap();
        assert_eq!(doc.layers()[0].pixels, before);

        let (mut a, ida) = ramped_3x3();
        let (mut b, idb) = ramped_3x3();
        a.offset(ida, -1, 0).unwrap();
        b.offset(idb, 2, 0).unwrap();
        assert_eq!(a.layers()[0].pixels, b.layers()[0].pixels);
        let p = &a.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(1, 0)], p[idx(2, 0)]], [20, 30, 10]);
    }

    #[test]
    fn other_filters_propagate_errors() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.maximum(id, 0).is_err());
        assert!(doc.minimum(id, 0).is_err());
        assert!(doc.high_pass(id, 0).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.maximum(id, 1).is_err());
        assert!(doc.minimum(id, 1).is_err());
        assert!(doc.high_pass(id, 1).is_err());
        assert!(doc.offset(id, 1, 1).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.maximum(999, 1).is_err());
        assert!(empty.offset(999, 1, 0).is_err());
    }

    fn kernel(entries: &[(usize, i32)]) -> [i32; 25] {
        let mut k = [0i32; 25];
        for &(i, weight) in entries {
            k[i] = weight;
        }
        k
    }

    #[test]
    fn custom_identity_kernel_leaves_the_layer_alone() {
        let (mut doc, id) = ramped_3x3();
        let before = doc.layers()[0].pixels.clone();
        let dirty = doc.custom(id, kernel(&[(12, 1)]), 1, 0).unwrap();
        assert_eq!(doc.layers()[0].pixels, before);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 3,
                y1: 3
            })
        );
    }

    #[test]
    fn custom_offset_shifts_every_colour_channel_and_clamps() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(12, 1)]), 1, 5).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(1, 1)], p[idx(2, 2)]], [15, 55, 95]);
        assert_eq!(p[idx(0, 0) + 1], 5); // the flat green channel lifts too
        assert_eq!(p[idx(0, 0) + 3], 255); // alpha is not a colour channel

        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(12, 1)]), 1, -20).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(1, 0)], p[idx(2, 0)]], [0, 0, 10]);
    }

    #[test]
    fn custom_box_kernel_reproduces_the_box_blur() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        let ones: Vec<(usize, i32)> = [6, 7, 8, 11, 12, 13, 16, 17, 18]
            .iter()
            .map(|&i| (i, 1))
            .collect();
        doc.custom(id, kernel(&ones), 9, 0).unwrap();
        let p = &doc.layers()[0].pixels;
        // The same windows the box-blur test lists: 450/9, 210/9, 690/9.
        assert_eq!(p[idx(1, 1)], 50);
        assert_eq!(p[idx(0, 0)], 23);
        assert_eq!(p[idx(2, 2)], 76);
    }

    #[test]
    fn custom_sharpen_kernel_amplifies_the_centre_against_its_neighbours() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.custom(
            id,
            kernel(&[(12, 5), (7, -1), (11, -1), (13, -1), (17, -1)]),
            1,
            0,
        )
        .unwrap();
        let p = &doc.layers()[0].pixels;
        // Centre: 5*50 - (20+40+60+80) = 50. Top-left, with the up and
        // left samples clamped onto itself: 5*10 - (10+10+20+40) = -30 -> 0.
        // Bottom-right, right and down clamped: 5*90 - (60+80+90+90) = 130.
        assert_eq!(p[idx(1, 1)], 50);
        assert_eq!(p[idx(0, 0)], 0);
        assert_eq!(p[idx(2, 2)], 130);
        assert_eq!(p[idx(2, 2) + 1], 0);
        assert_eq!(p[idx(2, 2) + 3], 255);
    }

    #[test]
    fn custom_scale_divides_toward_zero_and_negative_weights_invert() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(12, 1)]), 4, 0).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(1, 1)], p[idx(2, 2)]], [2, 12, 22]);

        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(12, -1)]), 1, 100).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(1, 1)], p[idx(2, 2)]], [90, 50, 10]);
        assert_eq!(p[idx(1, 1) + 1], 100);
    }

    #[test]
    fn custom_kernel_reaches_two_pixels_out_with_edge_clamping() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // kernel[24] is the (+2, +2) corner: the top-left pixel reads the
        // bottom-right one, and anything nearer the edge clamps onto it.
        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(24, 1)]), 1, 0).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(p[idx(0, 0)], 90);
        assert_eq!(p[idx(1, 1)], 90);

        // kernel[14] is (+2, 0): the left column reads the right column.
        let (mut doc, id) = ramped_3x3();
        doc.custom(id, kernel(&[(14, 1)]), 1, 0).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!([p[idx(0, 0)], p[idx(0, 1)], p[idx(0, 2)]], [30, 60, 90]);
        assert_eq!(p[idx(1, 1)], 60); // (3, 1) clamps to (2, 1)
    }

    #[test]
    fn custom_is_confined_to_the_selection() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(1.0, 1.0, 2.0, 2.0).unwrap();
        let dirty = doc.custom(id, kernel(&[(12, 1)]), 1, 100).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(p[idx(1, 1)], 150);
        assert_eq!(p[idx(0, 0)], 10);
        assert_eq!(p[idx(2, 2)], 90);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 2,
                y1: 2
            })
        );
    }

    #[test]
    fn custom_propagates_errors() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.custom(id, kernel(&[(12, 1)]), 0, 0).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.custom(id, kernel(&[(12, 1)]), 1, 0).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.custom(999, kernel(&[(12, 1)]), 1, 0).is_err());
    }

    #[test]
    fn solarize_folds_the_upper_half_of_the_range_back_down() {
        let (mut doc, id) = greys_2x2([10, 128, 200, 255]);
        doc.solarize(id).unwrap();
        let p = &doc.layers()[0].pixels;
        // min(v, 255 - v): 10 stays, 128 -> 127, 200 -> 55, 255 -> 0.
        assert_eq!([p[0], p[4], p[8], p[12]], [10, 127, 55, 0]);
        assert_eq!([p[1], p[2]], [10, 10]); // every colour channel alike
        assert_eq!(p[3], 255); // alpha untouched
    }

    #[test]
    fn find_edges_is_white_where_flat_and_dark_where_the_gradient_is_steep() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.find_edges(id).unwrap();
        let p = &doc.layers()[0].pixels;
        // Centre: Gx = (30+120+90) - (10+80+70) = 80, Gy = (70+160+90) -
        // (10+40+30) = 240, |Gx|+|Gy| = 320 -> 255 -> inverted to 0.
        // Top-left corner with every missing sample clamped: Gx =
        // (20+40+50) - (10+20+40) = 40, Gy = (40+80+50) - (10+20+20) = 120,
        // 160 -> 95.
        assert_eq!(p[idx(1, 1)], 0);
        assert_eq!(p[idx(0, 0)], 95);
        assert_eq!(p[idx(0, 0) + 1], 255); // the flat green channel has no edges
        assert_eq!(p[idx(0, 0) + 3], 255);

        let (mut doc, id) = grey_2x2();
        doc.find_edges(id).unwrap();
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [255, 255, 255, 255]));
    }

    #[test]
    fn emboss_lights_the_side_facing_the_angle() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Angle 0: the light comes from the right, so each pixel becomes
        // 128 + (left neighbour - right neighbour). Centre: 128 + 40 - 60.
        // The corners clamp their missing neighbour onto themselves.
        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 0.0, 1, 100).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(p[idx(1, 1)], 108);
        assert_eq!(p[idx(0, 0)], 118);
        assert_eq!(p[idx(2, 2)], 118);
        assert_eq!(p[idx(1, 1) + 1], 128); // flat green: no relief
        assert_eq!(p[idx(1, 1) + 3], 255);

        // Angle 180 is the mirror image of angle 0.
        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 180.0, 1, 100).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 148);

        // Angle 90: light from above, so 128 + (below - above) = 128 + 80 - 20.
        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 90.0, 1, 100).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 188);
    }

    #[test]
    fn emboss_amount_scales_the_relief_and_height_reaches_further() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 0.0, 1, 200).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 88);

        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 0.0, 1, 50).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 118);

        // Height 2 from the middle column reaches both (clamped) edges.
        let (mut doc, id) = ramped_3x3();
        doc.emboss(id, 0.0, 2, 100).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(p[idx(1, 1)], 108);
        assert_eq!(p[idx(0, 1)], 108); // left edge: 40 (clamped) - 60
    }

    #[test]
    fn trace_contour_outlines_where_a_channel_crosses_the_level() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Lower edge at level 50 marks the pixels below 50 that touch one
        // at or above it: 20 (above the 50), 30 (above the 60) and 40 (left
        // of the 50). The 10 in the corner only touches 20 and 40.
        let (mut doc, id) = ramped_3x3();
        doc.trace_contour(id, 50, false).unwrap();
        let p = &doc.layers()[0].pixels;
        let reds: Vec<u8> = (0..9).map(|i| p[i * 4]).collect();
        assert_eq!(reds, [255, 0, 0, 0, 255, 255, 255, 255, 255]);
        assert_eq!(p[idx(1, 0) + 1], 255); // the flat green channel never crosses
        assert_eq!(p[idx(1, 0) + 3], 255);

        // Upper edge marks the other side of the same contour: 50, 60, 70.
        let (mut doc, id) = ramped_3x3();
        doc.trace_contour(id, 50, true).unwrap();
        let p = &doc.layers()[0].pixels;
        let reds: Vec<u8> = (0..9).map(|i| p[i * 4]).collect();
        assert_eq!(reds, [255, 255, 255, 255, 0, 0, 0, 255, 255]);
    }

    #[test]
    fn trace_contour_at_the_extremes_draws_nothing() {
        let (mut doc, id) = ramped_3x3();
        doc.trace_contour(id, 0, false).unwrap(); // nothing is below 0
        assert!(doc.layers()[0]
            .pixels
            .chunks(4)
            .all(|px| px[0] == 255 && px[3] == 255));

        let (mut doc, id) = ramped_3x3();
        doc.trace_contour(id, 255, true).unwrap(); // nothing reaches 255
        assert!(doc.layers()[0].pixels.chunks(4).all(|px| px[0] == 255));
    }

    #[test]
    fn stylize_filters_honour_the_selection_and_propagate_errors() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(1.0, 1.0, 2.0, 2.0).unwrap();
        let dirty = doc.emboss(id, 0.0, 1, 100).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 108);
        assert_eq!(doc.layers()[0].pixels[idx(0, 0)], 10);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 1,
                y0: 1,
                x1: 2,
                y1: 2
            })
        );

        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.emboss(id, 0.0, 0, 100).is_err());
        assert!(doc.emboss(id, 0.0, 1, 0).is_err());
        assert!(doc.emboss(id, f32::NAN, 1, 100).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.find_edges(id).is_err());
        assert!(doc.solarize(id).is_err());
        assert!(doc.emboss(id, 0.0, 1, 100).is_err());
        assert!(doc.trace_contour(id, 128, false).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.find_edges(999).is_err());
        assert!(empty.solarize(999).is_err());
        assert!(empty.trace_contour(999, 128, true).is_err());
    }

    #[test]
    fn gaussian_weights_are_normalised_binomials() {
        let w = binomial_weights(1);
        assert_eq!(w.len(), 5);
        for (got, want) in w.iter().zip([1.0, 4.0, 6.0, 4.0, 1.0].map(|v| v / 16.0)) {
            assert!((got - want).abs() < 1e-12, "{got} vs {want}");
        }

        // Radius 2 is Pascal's row 16 cut to ±6 and renormalised: the two
        // biggest weights are C(16,8) and C(16,7) over the sum of the 13
        // kept coefficients, 65536 - 2*(1 + 16) = 65502.
        let w = binomial_weights(2);
        assert_eq!(w.len(), 13);
        assert!((w[6] - 12870.0 / 65502.0).abs() < 1e-12);
        assert!((w[5] - 11440.0 / 65502.0).abs() < 1e-12);
        assert!((w.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(binomial_weights(25).iter().all(|w| w.is_finite()));
    }

    #[test]
    fn gaussian_blur_radius_one_is_the_1_4_6_4_1_kernel_applied_twice() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.gaussian_blur(id, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        let reds: Vec<u8> = (0..9).map(|i| p[i * 4]).collect();
        // Horizontal pass, edge-clamped and rounded: 14 20 26 / 44 50 56 /
        // 74 80 86 (e.g. top-left (10+40+60+80+30)/16 = 13.75); then the
        // same kernel down each column (top-left (14+56+84+176+74)/16 =
        // 25.25).
        assert_eq!(reds, [25, 31, 37, 44, 50, 56, 63, 69, 75]);
        assert_eq!(p[idx(1, 1) + 1], 0);
        assert_eq!(p[idx(1, 1) + 3], 255);
    }

    #[test]
    fn gaussian_blur_leaves_a_flat_layer_alone_and_is_confined_to_the_selection() {
        let (mut doc, id) = grey_2x2();
        doc.gaussian_blur(id, 3).unwrap();
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [128, 128, 128, 255]));

        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap();
        let dirty = doc.gaussian_blur(id, 1).unwrap();
        let p = &doc.layers()[0].pixels;
        assert_eq!(p[idx(0, 0)], 25); // blurred with its unselected neighbours
        assert_eq!(p[idx(1, 0)], 20); // untouched
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1
            })
        );
    }

    #[test]
    fn gaussian_blur_propagates_errors() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.gaussian_blur(id, 0).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.gaussian_blur(id, 1).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.gaussian_blur(999, 1).is_err());
    }

    fn reds_3x3(doc: &Document) -> Vec<u8> {
        (0..9).map(|i| doc.layers()[0].pixels[i * 4]).collect()
    }

    #[test]
    fn diffuse_normal_takes_the_seeded_neighbour() {
        let (mut doc, id) = ramped_3x3();
        doc.diffuse(id, DiffuseMode::Normal, 1).unwrap();
        // Seed 1's xorshift32 draws, two per pixel in scan order and mapped
        // through `draw % 3 - 1`: (-1,0) (-1,+1) (+1,0) / (0,-1) (+1,0)
        // (-1,+1) / (0,+1) (+1,-1) (0,-1). The first two draws, 270369 and
        // 67634689, are the ones the Add Noise tests already pin (0 and 1
        // mod 3). Each pixel takes that clamped neighbour's red: the centre
        // takes its right-hand 60, the bottom row reads 70 (clamped onto
        // itself), 60 and 60.
        assert_eq!(reds_3x3(&doc), [10, 40, 30, 10, 60, 80, 70, 60, 60]);
        assert_eq!(doc.layers()[0].pixels[3], 255);
    }

    #[test]
    fn diffuse_darken_and_lighten_only_move_in_one_direction() {
        // The same draws as Normal; a neighbour is taken only when it is
        // darker (or, below, lighter) than the pixel it would replace.
        let (mut doc, id) = ramped_3x3();
        doc.diffuse(id, DiffuseMode::DarkenOnly, 1).unwrap();
        assert_eq!(reds_3x3(&doc), [10, 20, 30, 10, 50, 60, 70, 60, 60]);

        let (mut doc, id) = ramped_3x3();
        doc.diffuse(id, DiffuseMode::LightenOnly, 1).unwrap();
        assert_eq!(reds_3x3(&doc), [10, 40, 30, 40, 60, 80, 70, 80, 90]);
    }

    #[test]
    fn diffuse_anisotropic_takes_the_closest_neighbour_deterministically() {
        let (mut doc, id) = ramped_3x3();
        doc.diffuse(id, DiffuseMode::Anisotropic, 1).unwrap();
        let first = reds_3x3(&doc);
        // Each pixel takes its nearest-valued in-bounds neighbour, the first
        // in scan order on a tie: the centre's 40 and 60 both differ by 10,
        // so it takes the 40; the corner 10's neighbours are 20, 40 and 50.
        assert_eq!(first, [20, 10, 20, 50, 40, 50, 80, 70, 80]);

        let (mut doc, id) = ramped_3x3();
        doc.diffuse(id, DiffuseMode::Anisotropic, 99).unwrap();
        assert_eq!(reds_3x3(&doc), first); // the seed plays no part
    }

    #[test]
    fn diffuse_is_seeded_and_confined_to_the_selection() {
        let (mut a, ida) = ramped_3x3();
        let (mut b, idb) = ramped_3x3();
        a.diffuse(ida, DiffuseMode::Normal, 7).unwrap();
        b.diffuse(idb, DiffuseMode::Normal, 7).unwrap();
        assert_eq!(a.layers()[0].pixels, b.layers()[0].pixels);

        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(1.0, 0.0, 2.0, 1.0).unwrap();
        let dirty = doc.diffuse(id, DiffuseMode::Normal, 1).unwrap();
        // The one selected pixel gets the *first* draw pair, (-1, 0), and
        // takes its left neighbour's 10; nothing else moves.
        assert_eq!(doc.layers()[0].pixels[idx(1, 0)], 10);
        assert_eq!(doc.layers()[0].pixels[idx(0, 0)], 10);
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 50);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 1,
                y0: 0,
                x1: 2,
                y1: 1
            })
        );
    }

    #[test]
    fn diffuse_propagates_errors() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_locked(id, true).unwrap();
        assert!(doc.diffuse(id, DiffuseMode::Normal, 1).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.diffuse(999, DiffuseMode::Anisotropic, 1).is_err());
    }

    #[test]
    fn surface_blur_averages_only_within_the_threshold() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.surface_blur(id, 1, 25).unwrap();
        let p = &doc.layers()[0].pixels;
        // Centre 50: only 40, 50 and 60 lie within 25 (weights 15, 25, 15),
        // so (15*40 + 25*50 + 15*60) / 55 = 50. Top-left corner 10, whose
        // clamped window holds four 10s (weight 25 each), two 20s (weight
        // 15) and a 40 and a 50 that fall outside: 1600 / 130 = 12.3 -> 12,
        // far less pull than the box blur's 23 — which is the point. The 20
        // beside it: (2*15*10 + 2*25*20 + 2*15*30 + 5*40) / 115 = 20.9 -> 21.
        assert_eq!(p[idx(1, 1)], 50);
        assert_eq!(p[idx(0, 0)], 12);
        assert_eq!(p[idx(1, 0)], 21);
        assert_eq!(p[idx(1, 1) + 1], 0); // the flat green channel stays flat
        assert_eq!(p[idx(1, 1) + 3], 255); // alpha untouched
    }

    #[test]
    fn surface_blur_threshold_extremes_are_a_full_weighted_mean_and_the_identity() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Threshold 255 admits every sample, weighted 255 - |difference|;
        // the ramp is symmetric around its centre, so the centre stays 50.
        let (mut doc, id) = ramped_3x3();
        doc.surface_blur(id, 1, 255).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(1, 1)], 50);

        // Threshold 1 admits only exact matches, so nothing moves.
        let (mut doc, id) = ramped_3x3();
        let before = doc.layers()[0].pixels.clone();
        doc.surface_blur(id, 1, 1).unwrap();
        assert_eq!(doc.layers()[0].pixels, before);
    }

    #[test]
    fn surface_blur_leaves_a_flat_layer_alone_and_is_confined_to_the_selection() {
        let (mut doc, id) = grey_2x2();
        doc.surface_blur(id, 2, 40).unwrap();
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [128, 128, 128, 255]));

        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap();
        let dirty = doc.surface_blur(id, 1, 25).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(0, 0)], 12);
        assert_eq!(doc.layers()[0].pixels[idx(1, 0)], 20);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1
            })
        );
    }

    #[test]
    fn surface_blur_propagates_errors() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.surface_blur(id, 0, 25).is_err());
        assert!(doc.surface_blur(id, 1, 0).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.surface_blur(id, 1, 25).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.surface_blur(999, 1, 25).is_err());
    }

    #[test]
    fn glowing_edges_is_the_scaled_sobel_magnitude_on_black() {
        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        // Width 1, brightness 5, smoothness 1 is exactly the Sobel L1
        // magnitude the Find Edges test derived by hand: 160 in the corners,
        // 200 mid-top and mid-bottom, 255 (clamped) across the middle row.
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 1, 5, 1).unwrap();
        assert_eq!(
            reds_3x3(&doc),
            [160, 200, 160, 255, 255, 255, 160, 200, 160]
        );
        assert_eq!(doc.layers()[0].pixels[idx(0, 0) + 1], 0); // flat green: no edge, black
        assert_eq!(doc.layers()[0].pixels[idx(0, 0) + 3], 255); // alpha untouched

        // Photoshop's default brightness 6 scales by 6/5: 192, 240 and a
        // clamped 255.
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 1, 6, 1).unwrap();
        assert_eq!(
            reds_3x3(&doc),
            [192, 240, 192, 255, 255, 255, 192, 240, 192]
        );

        // Brightness 3 dims to 3/5, truncating: 96, 120, 153. Brightness 0
        // is black.
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 1, 3, 1).unwrap();
        assert_eq!(reds_3x3(&doc), [96, 120, 96, 153, 153, 153, 96, 120, 96]);
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 1, 0, 1).unwrap();
        assert!(reds_3x3(&doc).iter().all(|&v| v == 0));
    }

    #[test]
    fn glowing_edges_width_dilates_and_smoothness_blurs() {
        // Width 2 is a radius-1 maximum: every 3x3 window holds a 255.
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 2, 5, 1).unwrap();
        assert!(reds_3x3(&doc).iter().all(|&v| v == 255));

        // Smoothness 2 is a radius-1 box blur of the magnitudes; on the
        // clamped 3x3 every window holds four 160s, two 200s and three 255s,
        // 1805 / 9 = 200.
        let (mut doc, id) = ramped_3x3();
        doc.glowing_edges(id, 1, 5, 2).unwrap();
        assert!(reds_3x3(&doc).iter().all(|&v| v == 200));
    }

    #[test]
    fn glowing_edges_is_black_on_a_flat_layer_and_confined_to_the_selection() {
        let (mut doc, id) = grey_2x2();
        doc.glowing_edges(id, 2, 6, 3).unwrap();
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [0, 0, 0, 255]));

        let idx = |x: usize, y: usize| (y * 3 + x) * 4;
        let (mut doc, id) = ramped_3x3();
        doc.select_rectangle(0.0, 0.0, 1.0, 1.0).unwrap();
        let dirty = doc.glowing_edges(id, 1, 5, 1).unwrap();
        assert_eq!(doc.layers()[0].pixels[idx(0, 0)], 160);
        assert_eq!(doc.layers()[0].pixels[idx(1, 0)], 20);
        assert_eq!(
            dirty,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1
            })
        );
    }

    #[test]
    fn glowing_edges_propagates_errors() {
        let (mut doc, id) = doc_with_one_layer();
        assert!(doc.glowing_edges(id, 0, 6, 1).is_err());
        assert!(doc.glowing_edges(id, 1, 6, 0).is_err());
        doc.set_locked(id, true).unwrap();
        assert!(doc.glowing_edges(id, 1, 6, 1).is_err());
        assert_eq!(doc.layers()[0].pixels, solid(2, 2, [10, 20, 30, 255]));
        let mut empty = Document::new(2, 2).unwrap();
        assert!(empty.glowing_edges(999, 1, 6, 1).is_err());
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

    #[test]
    fn duplicate_layer_lands_directly_above_the_original_not_at_the_top() {
        let mut doc = Document::new(1, 1).unwrap();
        let a = doc.add_layer("a", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let b = doc.add_layer("b", &solid(1, 1, [0; 4]), 1, 1).unwrap();
        let c = doc.add_layer("c", &solid(1, 1, [0; 4]), 1, 1).unwrap();

        let dup = doc.duplicate_layer(a).unwrap();

        assert_eq!(ids(&doc), vec![a, dup, b, c]);
        assert_ne!(dup, a);
    }

    #[test]
    fn duplicate_layer_copies_pixels_and_attributes_and_appends_copy_to_the_name() {
        let (mut doc, id) = doc_with_one_layer();
        doc.set_opacity(id, 0.5).unwrap();
        doc.set_blend_mode(id, BlendMode::Multiply).unwrap();
        doc.set_locked(id, true).unwrap();

        let dup = doc.duplicate_layer(id).unwrap();

        let duplicate = doc.layers().iter().find(|l| l.id == dup).unwrap();
        assert_eq!(duplicate.name, "base copy");
        assert_eq!(duplicate.pixels, solid(2, 2, [10, 20, 30, 255]));
        assert_eq!(duplicate.opacity, 0.5);
        assert_eq!(duplicate.blend_mode, BlendMode::Multiply);
        assert!(duplicate.locked);
        assert!(duplicate.visible);
    }

    #[test]
    fn duplicate_layer_does_not_modify_the_original() {
        let (mut doc, id) = doc_with_one_layer();
        doc.duplicate_layer(id).unwrap();
        let original = doc.layers().iter().find(|l| l.id == id).unwrap();
        assert_eq!(original.name, "base");
        assert_eq!(original.pixels, solid(2, 2, [10, 20, 30, 255]));
    }

    #[test]
    fn duplicate_layer_errors_on_an_unknown_layer() {
        let mut doc = Document::new(2, 2).unwrap();
        assert!(doc.duplicate_layer(999).is_err());
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
