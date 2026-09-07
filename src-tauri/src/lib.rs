// Suppress the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod blend;
pub mod composite;
pub mod document;
pub mod png;
pub mod project;

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State};

use blend::BlendMode;
use composite::Rect;
use document::{
    Clipboard, DiffuseMode, Document, DocumentView, LayerId, MoveDirection, Stroke, ZigZagStyle,
    CHANNELS,
};

/// Same order of magnitude as `png::MAX_FILE_BYTES` — a blank canvas this
/// large would be as much of a memory problem as a PNG that big.
const MAX_NEW_DOCUMENT_BYTES: u64 = 64 * 1024 * 1024;

/// The latest flattened composite: raw RGBA8 pixels, so a stroke's dirty
/// rect can be recomposited into just that region instead of the whole
/// document (see [`snapshot`]), plus the PNG-encoded bytes actually served
/// to the webview by the `composite://` protocol below rather than embedded
/// in every command response. `generation` is bumped each time `bytes`
/// changes, so the frontend can cache-bust its `<img>` src without the bytes
/// themselves crossing the IPC boundary.
#[derive(Default)]
struct CompositeCache {
    /// `None` until the first flatten; always replaced outright (never
    /// region-patched in place) whenever [`snapshot`] does a full flatten.
    pixels: Mutex<Option<composite::Composite>>,
    bytes: Mutex<Option<Vec<u8>>>,
    generation: AtomicU64,
}

/// The open document. `None` until the first image is opened.
#[derive(Default)]
struct AppState {
    document: Mutex<Option<Document>>,
    composite: CompositeCache,
    history: Mutex<History>,
    /// Edit > Copy/Cut's most recent capture, ready for Edit > Paste. Kept
    /// here rather than on `Document` itself: a real clipboard survives
    /// undo, redo, and even opening a different document, none of which
    /// `Document`'s own state does.
    clipboard: Mutex<Option<Clipboard>>,
    /// The state the History Brush paints from: a whole-document clone
    /// taken when the user last pressed Set Source. Kept here, like the
    /// clipboard, since it must outlive undo and redo.
    history_source: Mutex<Option<Document>>,
}

/// Undo/redo stacks of whole-document snapshots. A checkpoint clones the
/// document onto `undo` before a gesture (a stroke, an opacity drag) starts;
/// commands that are already one discrete action (add a layer, toggle
/// visibility, ...) checkpoint themselves. Undoing moves the current
/// document onto `redo`; a fresh checkpoint clears `redo`, the same as every
/// other editor's undo history — you cannot redo past a new edit.
#[derive(Default)]
struct History {
    undo: VecDeque<Document>,
    redo: VecDeque<Document>,
}

/// Bounds how much whole-document history can pile up behind one open
/// document. Old entries fall off the far end rather than growing forever.
const MAX_HISTORY: usize = 50;

fn push_bounded(stack: &mut VecDeque<Document>, document: Document) {
    stack.push_back(document);
    if stack.len() > MAX_HISTORY {
        stack.pop_front();
    }
}

/// What every mutating command hands back: the new layer state plus the
/// generation of the re-flattened composite now cached in `AppState`.
/// Keeping them together means one round trip per edit instead of two.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    document: DocumentView,
    generation: u64,
    can_undo: bool,
    can_redo: bool,
    /// Whether a History Brush source has been set.
    has_history_source: bool,
}

/// What [`checkpoint`] hands back: just the two flags of [`Snapshot`] that
/// change, since a checkpoint does not touch the document or the composite.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryState {
    can_undo: bool,
    can_redo: bool,
    /// Whether a History Brush source has been set.
    has_history_source: bool,
}

/// One entry in the blend-mode picker.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlendModeInfo {
    mode: BlendMode,
    label: &'static str,
}

/// Re-flatten `document` — or, given a dirty `rect`, recomposite only that
/// region of the cached composite — cache the encoded result, and hand back
/// the new document view plus the generation the frontend should now
/// request.
///
/// `rect` is `Some` only after a brush/eraser stroke, whose caller already
/// knows exactly which pixels it touched (see [`document::Document::stroke`]).
/// Every other edit — opacity, visibility, blend mode, a layer being added,
/// removed, or reordered — can change any pixel in the composite, so those
/// pass `None` and get a full flatten. A `rect` is also ignored (falls back
/// to a full flatten) whenever nothing has been cached yet, or the cached
/// composite's dimensions do not match `document`'s — the latter cannot
/// actually happen given how commands call this (every path that can change
/// the document's size, i.e. [`open_document`], always passes `None`), but
/// the check costs little and turns a would-be silent mismatch into the
/// always-correct fallback rather than a subtle bug.
fn snapshot(state: &AppState, document: &Document, rect: Option<Rect>) -> Result<Snapshot, String> {
    let mut pixels_guard = state
        .composite
        .pixels
        .lock()
        .map_err(|_| POISONED.to_string())?;
    let fresh_composite = match (rect, pixels_guard.as_mut()) {
        (Some(rect), Some(cached))
            if cached.width == document.width() && cached.height == document.height() =>
        {
            composite::recomposite_region(document, rect, &mut cached.pixels);
            None
        }
        _ => Some(composite::flatten(document)),
    };
    if let Some(fresh) = fresh_composite {
        *pixels_guard = Some(fresh);
    }
    let composite = pixels_guard.as_ref().expect("just populated above");
    let bytes = png::encode(composite)?;
    drop(pixels_guard);

    *state
        .composite
        .bytes
        .lock()
        .map_err(|_| POISONED.to_string())? = Some(bytes);
    let generation = state.composite.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let history = state.history.lock().map_err(|_| POISONED.to_string())?;
    Ok(Snapshot {
        document: document.view(),
        generation,
        can_undo: !history.undo.is_empty(),
        can_redo: !history.redo.is_empty(),
        has_history_source: has_history_source(state)?,
    })
}

/// Snapshot the open document (if any) onto the undo stack and clear the
/// redo stack — the checkpoint a gesture takes before it starts changing the
/// document, so the whole gesture undoes as one step rather than one step
/// per command it happens to have sent.
fn push_checkpoint(state: &AppState) -> Result<(), String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    if let Some(document) = guard.as_ref() {
        let mut history = state.history.lock().map_err(|_| POISONED.to_string())?;
        push_bounded(&mut history.undo, document.clone());
        history.redo.clear();
    }
    Ok(())
}

fn has_history_source(state: &AppState) -> Result<bool, String> {
    Ok(state
        .history_source
        .lock()
        .map_err(|_| POISONED.to_string())?
        .is_some())
}

fn history_state(state: &AppState) -> Result<HistoryState, String> {
    let history = state.history.lock().map_err(|_| POISONED.to_string())?;
    Ok(HistoryState {
        can_undo: !history.undo.is_empty(),
        can_redo: !history.redo.is_empty(),
        has_history_source: has_history_source(state)?,
    })
}

const NOTHING_TO_UNDO: &str = "Nothing to undo.";
const NOTHING_TO_REDO: &str = "Nothing to redo.";

fn perform_undo(state: &AppState) -> Result<Snapshot, String> {
    let mut doc_guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let mut history = state.history.lock().map_err(|_| POISONED.to_string())?;
    let previous = history
        .undo
        .pop_back()
        .ok_or_else(|| NOTHING_TO_UNDO.to_string())?;
    if let Some(current) = doc_guard.take() {
        push_bounded(&mut history.redo, current);
    }
    *doc_guard = Some(previous);
    drop(history);
    snapshot(state, doc_guard.as_ref().expect("just set"), None)
}

fn perform_redo(state: &AppState) -> Result<Snapshot, String> {
    let mut doc_guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let mut history = state.history.lock().map_err(|_| POISONED.to_string())?;
    let next = history
        .redo
        .pop_back()
        .ok_or_else(|| NOTHING_TO_REDO.to_string())?;
    if let Some(current) = doc_guard.take() {
        push_bounded(&mut history.undo, current);
    }
    *doc_guard = Some(next);
    drop(history);
    snapshot(state, doc_guard.as_ref().expect("just set"), None)
}

/// Flatten `document` and write the result to `path` as PNG. Kept separate
/// from the `#[tauri::command]` wrapper below so it can be unit-tested
/// directly, the same way [`snapshot`] is.
fn export(document: &Document, path: &Path) -> Result<(), String> {
    let bytes = png::encode(&composite::flatten(document))?;
    std::fs::write(path, bytes).map_err(|err| format!("Could not write {}: {err}", path.display()))
}

/// Eyedropper: the RGBA colour of the cached composite at document pixel
/// `(x, y)` — what's actually visible on screen, the same convention
/// Photoshop's own eyedropper defaults to (sampling the merged image, not
/// one specific layer). Errors if nothing has been composited yet, or the
/// point falls outside the canvas.
fn sample_pixel_color(cache: &CompositeCache, x: u32, y: u32) -> Result<[u8; 4], String> {
    let guard = cache.pixels.lock().map_err(|_| POISONED.to_string())?;
    let composite = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    if x >= composite.width || y >= composite.height {
        return Err(format!(
            "({x}, {y}) is outside the {}x{} canvas.",
            composite.width, composite.height
        ));
    }
    let base = (y as usize * composite.width as usize + x as usize) * CHANNELS;
    Ok([
        composite.pixels[base],
        composite.pixels[base + 1],
        composite.pixels[base + 2],
        composite.pixels[base + 3],
    ])
}

/// Build the response the `composite://` protocol hands the webview: the
/// cached PNG bytes, or 404 before anything has ever been composited.
fn serve_composite(cache: &CompositeCache) -> tauri::http::Response<Vec<u8>> {
    let bytes = cache.bytes.lock().ok().and_then(|guard| guard.clone());
    match bytes {
        Some(bytes) => tauri::http::Response::builder()
            .header(tauri::http::header::CONTENT_TYPE, "image/png")
            .header(tauri::http::header::CACHE_CONTROL, "no-store")
            .body(bytes)
            .expect("a static response is always well-formed"),
        None => tauri::http::Response::builder()
            .status(tauri::http::StatusCode::NOT_FOUND)
            .body(Vec::new())
            .expect("a static response is always well-formed"),
    }
}

/// Run `edit` against the open document, then re-flatten (or recomposite just
/// the rect `edit` reports touching — see [`snapshot`]). Does not itself
/// checkpoint: callers that are one whole gesture on their own (add a layer,
/// toggle visibility, ...) should use [`edit_checkpointed`] instead. Callers
/// that are one step of a longer gesture (a stroke, an opacity drag) call
/// this directly — the frontend checkpoints once, at the start of the
/// gesture, not on every step.
fn edit<F>(state: &State<'_, AppState>, edit: F) -> Result<Snapshot, String>
where
    F: FnOnce(&mut Document) -> Result<Option<Rect>, String>,
{
    let mut guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_mut().ok_or_else(|| NO_DOCUMENT.to_string())?;
    let rect = edit(document)?;
    snapshot(state, document, rect)
}

/// [`edit`], preceded by a checkpoint — for commands that are a whole,
/// discrete user action on their own rather than one step of a longer one.
fn edit_checkpointed<F>(state: &State<'_, AppState>, edit_fn: F) -> Result<Snapshot, String>
where
    F: FnOnce(&mut Document) -> Result<Option<Rect>, String>,
{
    push_checkpoint(state)?;
    edit(state, edit_fn)
}

const POISONED: &str = "The document is in an inconsistent state; please reopen the image.";
const NO_DOCUMENT: &str = "No document is open.";

fn layer_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// Replace whatever document is open with `document` — the shared tail of
/// [`open_document`] and [`open_project`]: a new document always starts its
/// own history, since undoing "past" it into whatever was open before is not
/// a thing any editor does.
fn replace_open_document(state: &AppState, document: Document) -> Result<Snapshot, String> {
    *state.history.lock().map_err(|_| POISONED.to_string())? = History::default();
    let result = snapshot(state, &document, None)?;
    *state.document.lock().map_err(|_| POISONED.to_string())? = Some(document);
    Ok(result)
}

/// Open `path` as a new single-layer document, replacing whatever was open.
#[tauri::command]
fn open_document(state: State<'_, AppState>, path: String) -> Result<Snapshot, String> {
    let path = PathBuf::from(path);
    let decoded = png::read(&path)?;

    let mut document = Document::new(decoded.width, decoded.height)?;
    document.add_layer(
        layer_name(&path),
        &decoded.pixels,
        decoded.width,
        decoded.height,
    )?;
    replace_open_document(&state, document)
}

/// Create a blank `width` x `height` document with one fully transparent
/// layer to paint on immediately, replacing whatever was open. Kept separate
/// from the `#[tauri::command]` wrapper below so it can be unit-tested
/// directly, the same way [`export`] is.
fn create_new_document(state: &AppState, width: u32, height: u32) -> Result<Snapshot, String> {
    let mut document = Document::new(width, height)?;
    let byte_len = document.buffer_len() as u64;
    if byte_len > MAX_NEW_DOCUMENT_BYTES {
        return Err(format!(
            "{width}x{height} would be {:.1} MB, which is over the {} MB limit.",
            byte_len as f64 / (1024.0 * 1024.0),
            MAX_NEW_DOCUMENT_BYTES / (1024 * 1024)
        ));
    }
    let blank = vec![0u8; document.buffer_len()];
    document.add_layer("Layer 1", &blank, width, height)?;
    replace_open_document(state, document)
}

#[tauri::command]
fn new_document(state: State<'_, AppState>, width: u32, height: u32) -> Result<Snapshot, String> {
    create_new_document(&state, width, height)
}

/// Replace the selection with an axis-aligned rectangle. Corners can be given
/// in either order, as a drag can go any direction. A whole, discrete action
/// on its own (not one step of a longer gesture), so it checkpoints itself —
/// the same as every other one-shot command below.
#[tauri::command]
fn select_rectangle(
    state: State<'_, AppState>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    mode: Option<document::SelectionMode>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        let mode = mode.unwrap_or(document::SelectionMode::New);
        document.select_rectangle_with(mode, x0, y0, x1, y1)?;
        Ok(None)
    })
}

/// Replace the selection with an ellipse inscribed in the given bounding
/// box, or combine it with the current selection per `mode`.
#[tauri::command]
fn select_ellipse(
    state: State<'_, AppState>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    mode: Option<document::SelectionMode>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        let mode = mode.unwrap_or(document::SelectionMode::New);
        document.select_ellipse_with(mode, x0, y0, x1, y1)?;
        Ok(None)
    })
}

/// Polygonal Lasso / Lasso: select the pixels inside the polygon through
/// `points`, combined with the current selection per `mode`.
#[tauri::command]
fn select_polygon(
    state: State<'_, AppState>,
    points: Vec<(f32, f32)>,
    mode: Option<document::SelectionMode>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        let mode = mode.unwrap_or(document::SelectionMode::New);
        document.select_polygon_with(mode, &points)?;
        Ok(None)
    })
}

/// Lasso: select the pixels inside a freehand drag's closed trail,
/// combined with the current selection per `mode`.
#[tauri::command]
fn select_lasso(
    state: State<'_, AppState>,
    trail: Vec<(f32, f32)>,
    mode: Option<document::SelectionMode>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        let mode = mode.unwrap_or(document::SelectionMode::New);
        document.select_lasso_with(mode, &trail)?;
        Ok(None)
    })
}

/// Magic Wand: replace the selection with every pixel of layer `id` within
/// `tolerance` of the pixel at `(x, y)`, contiguous or not.
#[tauri::command]
fn select_magic_wand(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
    tolerance: u8,
    contiguous: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_magic_wand(id, x, y, tolerance, contiguous)?;
        Ok(None)
    })
}

/// Select > Color Range: replace the selection with every pixel of layer
/// `id` whose RGB is within `fuzziness` (per channel) of `color`.
#[tauri::command]
fn select_color_range(
    state: State<'_, AppState>,
    id: LayerId,
    color: [u8; 3],
    fuzziness: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_color_range(id, color, fuzziness)?;
        Ok(None)
    })
}

/// Select > Grow: extend the selection to adjacent pixels of layer `id`
/// within `tolerance` of the colours already selected.
#[tauri::command]
fn grow_selection(
    state: State<'_, AppState>,
    id: LayerId,
    tolerance: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.grow_selection(id, tolerance)?;
        Ok(None)
    })
}

/// Select > Similar: extend the selection to every pixel of layer `id`
/// within `tolerance` of the colours already selected, wherever it sits.
#[tauri::command]
fn select_similar(
    state: State<'_, AppState>,
    id: LayerId,
    tolerance: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_similar(id, tolerance)?;
        Ok(None)
    })
}

/// Select the entire canvas.
#[tauri::command]
fn select_all(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_all()?;
        Ok(None)
    })
}

/// Select > Inverse: swap selected and unselected pixels.
#[tauri::command]
fn invert_selection(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.invert_selection()?;
        Ok(None)
    })
}

/// Select > Modify > Expand: grow the selection outward by `amount` pixels.
#[tauri::command]
fn expand_selection(state: State<'_, AppState>, amount: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.expand_selection(amount)?;
        Ok(None)
    })
}

/// Move the selection outline by `(dx, dy)` pixels without moving pixels.
#[tauri::command]
fn move_selection(state: State<'_, AppState>, dx: i64, dy: i64) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.move_selection(dx, dy)?;
        Ok(None)
    })
}

/// Move tool: shift layer `id`'s selected pixels (or the whole layer) by
/// `(dx, dy)`, carrying the selection outline along. A whole, discrete
/// action, so it checkpoints itself.
#[tauri::command]
fn move_pixels(
    state: State<'_, AppState>,
    id: LayerId,
    dx: i32,
    dy: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.move_pixels(id, dx, dy))
}

/// Patch tool: rebuild the selected pixels of layer `id` from the area
/// `(dx, dy)` away, matched to their own tone. A whole, discrete action,
/// so it checkpoints itself.
#[tauri::command]
fn patch(state: State<'_, AppState>, id: LayerId, dx: i32, dy: i32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.patch(id, dx, dy))
}

/// Content-Aware Move tool: move the selected pixels of layer `id` by
/// `(dx, dy)` and fill the vacated area from its surroundings. A whole,
/// discrete action, so it checkpoints itself.
#[tauri::command]
fn content_aware_move(
    state: State<'_, AppState>,
    id: LayerId,
    dx: i32,
    dy: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.content_aware_move(id, dx, dy))
}

/// Edit > Content-Aware Fill: fill the selected pixels of layer `id` from
/// their surroundings. A whole, discrete action, so it checkpoints itself.
#[tauri::command]
fn content_aware_fill(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.content_aware_fill(id))
}

/// Select > Transform Selection: scale, rotate, and move the selection
/// outline about its own centre without touching pixels.
#[tauri::command]
fn transform_selection(
    state: State<'_, AppState>,
    width_percent: f32,
    height_percent: f32,
    degrees: f32,
    dx: f32,
    dy: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.transform_selection(width_percent, height_percent, degrees, dx, dy)?;
        Ok(None)
    })
}

/// Select > Save Selection: store the active selection under `name`.
#[tauri::command]
fn save_selection(state: State<'_, AppState>, name: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.save_selection(&name)?;
        Ok(None)
    })
}

/// Select > Load Selection: replace the selection with the one saved as `name`.
#[tauri::command]
fn load_selection(state: State<'_, AppState>, name: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.load_selection(&name)?;
        Ok(None)
    })
}

/// Count tool: place the next numbered mark at `(x, y)`.
#[tauri::command]
fn add_count_mark(state: State<'_, AppState>, x: u32, y: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_count_mark(x, y)?;
        Ok(None)
    })
}

/// Count tool: remove every mark.
#[tauri::command]
fn clear_count_marks(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.clear_count_marks();
        Ok(None)
    })
}

/// Note tool: pin a text note at `(x, y)`.
#[tauri::command]
fn add_note(state: State<'_, AppState>, x: u32, y: u32, text: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_note(x, y, &text)?;
        Ok(None)
    })
}

/// Note tool: rewrite note `index`'s text.
#[tauri::command]
fn set_note_text(
    state: State<'_, AppState>,
    index: usize,
    text: String,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.set_note_text(index, &text)?;
        Ok(None)
    })
}

/// Note tool: delete note `index`.
#[tauri::command]
fn remove_note(state: State<'_, AppState>, index: usize) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.remove_note(index)?;
        Ok(None)
    })
}

/// Note tool: remove every note.
#[tauri::command]
fn clear_notes(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.clear_notes();
        Ok(None)
    })
}

/// Layer Comps: record every layer's visibility, opacity, and blend mode
/// under `name`.
#[tauri::command]
fn save_layer_comp(state: State<'_, AppState>, name: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.save_layer_comp(&name)?;
        Ok(None)
    })
}

/// Layer Comps: restore the comp saved as `name`.
#[tauri::command]
fn apply_layer_comp(state: State<'_, AppState>, name: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.apply_layer_comp(&name)?;
        Ok(None)
    })
}

/// Layer Comps: delete the comp saved as `name`.
#[tauri::command]
fn delete_layer_comp(state: State<'_, AppState>, name: String) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.delete_layer_comp(&name)?;
        Ok(None)
    })
}

/// Select > Modify > Contract: shrink the selection inward by `amount` pixels.
#[tauri::command]
fn contract_selection(state: State<'_, AppState>, amount: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.contract_selection(amount)?;
        Ok(None)
    })
}

/// Select > Modify > Smooth: round the selection's corners by `radius` pixels.
#[tauri::command]
fn smooth_selection(state: State<'_, AppState>, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.smooth_selection(radius)?;
        Ok(None)
    })
}

/// Select > Modify > Border: turn the selection into a `width`-pixel band
/// hugging the inside of its own edge.
#[tauri::command]
fn border_selection(state: State<'_, AppState>, width: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.border_selection(width)?;
        Ok(None)
    })
}

/// Select > Reselect: restore the selection `deselect` most recently cleared.
#[tauri::command]
fn reselect(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.reselect()?;
        Ok(None)
    })
}

/// Clear the selection.
#[tauri::command]
fn deselect(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.deselect();
        Ok(None)
    })
}

/// Add `path` as a new top layer of the open document. The document keeps its
/// original size: a smaller image is pasted at the origin, a larger one clipped.
#[tauri::command]
fn add_layer(state: State<'_, AppState>, path: String) -> Result<Snapshot, String> {
    let path = PathBuf::from(path);
    let decoded = png::read(&path)?;
    edit_checkpointed(&state, |document| {
        document
            .add_layer(
                layer_name(&path),
                &decoded.pixels,
                decoded.width,
                decoded.height,
            )
            .map(|_| None)
    })
}

/// Layer > New Fill Layer > Solid Color: add a new top layer filled
/// entirely with `color` (RGBA8). Always named "Color Fill 1" — there is
/// no auto-incrementing layer-name scheme in this app yet (the first
/// layer of a brand new document is likewise always plainly "Layer 1").
#[tauri::command]
fn add_solid_color_layer(state: State<'_, AppState>, color: [u8; 4]) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_solid_color_layer("Color Fill 1", color);
        Ok(None)
    })
}

/// Layer > New Fill Layer > Gradient: add a new top layer filled with a
/// linear gradient from `start_color` to `end_color` along the canvas's
/// own top-left-to-bottom-right diagonal. Always named "Gradient Fill 1".
#[tauri::command]
fn add_gradient_layer(
    state: State<'_, AppState>,
    start_color: [u8; 4],
    end_color: [u8; 4],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_gradient_layer("Gradient Fill 1", start_color, end_color);
        Ok(None)
    })
}

#[tauri::command]
fn set_layer_visible(
    state: State<'_, AppState>,
    id: LayerId,
    visible: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.set_visible(id, visible).map(|_| None)
    })
}

#[tauri::command]
fn set_layer_locked(
    state: State<'_, AppState>,
    id: LayerId,
    locked: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.set_locked(id, locked).map(|_| None)
    })
}

/// Layer > Rasterize on layer `id`. Every layer in this app is already
/// pixels, so this is always a no-op beyond validating `id` exists.
#[tauri::command]
fn rasterize_layer(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.rasterize_layer(id).map(|_| None)
    })
}

/// Edit > Transform > Flip Horizontal on layer `id`.
#[tauri::command]
fn flip_layer_horizontal(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.flip_layer_horizontal(id).map(|_| None)
    })
}

/// Edit > Transform > Flip Vertical on layer `id`.
#[tauri::command]
fn flip_layer_vertical(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.flip_layer_vertical(id).map(|_| None)
    })
}

/// Edit > Transform > Rotate 180° on layer `id`.
#[tauri::command]
fn rotate_layer_180(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.rotate_layer_180(id).map(|_| None)
    })
}

/// Image > Image Rotation > 90° Clockwise / 90° Counter Clockwise: rotates
/// the whole document (every layer, and the canvas itself), swapping
/// width and height.
#[tauri::command]
fn rotate_document_90(state: State<'_, AppState>, clockwise: bool) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.rotate_document_90(clockwise);
        Ok(None)
    })
}

/// Camera Raw Filter > Geometry > Constrain Crop: crop the whole document to
/// the largest fully opaque rectangle of layer `id`.
#[tauri::command]
fn constrain_crop(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.constrain_crop(id).map(|_| None))
}

/// Edit > Copy: captures layer `id`'s pixels — within the active selection,
/// or the whole layer with none — into the clipboard, ready for [`paste`].
/// Doesn't actually change the document; still returns a [`Snapshot`] (an
/// unchanged one) rather than `()` so the frontend can drive this through
/// the same `runCommand` path as every other command instead of a bespoke
/// one just for this.
#[tauri::command]
fn copy(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    let clipboard = document.copy(id)?;
    *state.clipboard.lock().map_err(|_| POISONED.to_string())? = Some(clipboard);
    snapshot(&state, document, None)
}

/// Image > Apply Image: blend layer `source` (or the merged composite, with
/// `None`) onto layer `target` with `blend` at `opacity` percent.
#[tauri::command]
fn apply_image(
    state: State<'_, AppState>,
    target: LayerId,
    source: Option<LayerId>,
    blend: BlendMode,
    opacity: u8,
    invert: bool,
    preserve_transparency: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.apply_image(
            target,
            source,
            blend,
            opacity,
            invert,
            preserve_transparency,
        )
    })
}

/// Edit > Copy Merged: capture the visible composite within the active
/// selection into the clipboard. Read-only, like [`copy`].
#[tauri::command]
fn copy_merged(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    let clipboard = document.copy_merged()?;
    *state.clipboard.lock().map_err(|_| POISONED.to_string())? = Some(clipboard);
    snapshot(&state, document, None)
}

/// Edit > Cut: [`copy`], then clears the copied pixels from layer `id`.
#[tauri::command]
fn cut(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    push_checkpoint(&state)?;
    let mut guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_mut().ok_or_else(|| NO_DOCUMENT.to_string())?;
    let (clipboard, rect) = document.cut(id)?;
    *state.clipboard.lock().map_err(|_| POISONED.to_string())? = Some(clipboard);
    snapshot(&state, document, rect)
}

/// Edit > Paste — also serves as Edit > Paste Special > Paste in Place,
/// since [`document::Document::paste`] always lands the clipboard back at
/// its original coordinates (see that function's own docs for why). Errors
/// if nothing has been copied or cut yet, the same as Photoshop greying
/// the menu item out.
#[tauri::command]
fn paste(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let clipboard = {
        let guard = state.clipboard.lock().map_err(|_| POISONED.to_string())?;
        guard
            .as_ref()
            .ok_or_else(|| "Nothing has been copied or cut yet.".to_string())?
            .clone()
    };
    edit_checkpointed(&state, |document| {
        document.paste(&clipboard, "Pasted Layer");
        Ok(None)
    })
}

/// Edit > Paste Special > Paste Into: paste the clipboard centred in the
/// active selection, keeping only the pixels inside it. Errors if nothing
/// has been copied yet or nothing is selected.
#[tauri::command]
fn paste_into(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let clipboard = {
        let guard = state.clipboard.lock().map_err(|_| POISONED.to_string())?;
        guard
            .as_ref()
            .ok_or_else(|| "Nothing has been copied or cut yet.".to_string())?
            .clone()
    };
    edit_checkpointed(&state, |document| {
        document
            .paste_into(&clipboard, "Pasted Layer")
            .map(|_| None)
    })
}

/// Edit > Paste Special > Paste Outside: paste the clipboard centred on the
/// active selection, keeping only the pixels outside it. Errors if nothing
/// has been copied yet or nothing is selected.
#[tauri::command]
fn paste_outside(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let clipboard = {
        let guard = state.clipboard.lock().map_err(|_| POISONED.to_string())?;
        guard
            .as_ref()
            .ok_or_else(|| "Nothing has been copied or cut yet.".to_string())?
            .clone()
    };
    edit_checkpointed(&state, |document| {
        document
            .paste_outside(&clipboard, "Pasted Layer")
            .map(|_| None)
    })
}

/// Camera Raw Filter > Geometry (Manual) on layer `id`, as one undo step.
#[tauri::command]
fn camera_raw_geometry(
    state: State<'_, AppState>,
    id: LayerId,
    settings: document::GeometrySettings,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.camera_raw_geometry(id, settings)
    })
}

/// Layer > New > Layer via Copy on layer `id`: unlike [`copy`]/[`paste`],
/// this never touches the clipboard at all.
#[tauri::command]
fn new_layer_via_copy(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document
            .new_layer_via_copy(id, "Layer via Copy")
            .map(|_| None)
    })
}

/// Layer > New > Layer via Cut on layer `id`.
#[tauri::command]
fn new_layer_via_cut(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        let (_, rect) = document.new_layer_via_cut(id, "Layer via Cut")?;
        Ok(rect)
    })
}

/// Edit > Delete (also covers Edit > Clear — see
/// [`document::Document::delete_selection`] for why one command is
/// enough) on layer `id`.
#[tauri::command]
fn delete_selection(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.delete_selection(id))
}

/// Edit > Fill on layer `id` with a flat `color`.
#[tauri::command]
fn fill_selection(
    state: State<'_, AppState>,
    id: LayerId,
    color: [u8; 4],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.fill_selection(id, color))
}

/// Filter > Blur > Box Blur on layer `id`.
#[tauri::command]
fn box_blur(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.box_blur(id, radius))
}

/// Filter > Blur > Shape Blur on layer `id`: a flat average over a square,
/// diamond, or disc of the given radius.
#[tauri::command]
fn shape_blur(
    state: State<'_, AppState>,
    id: LayerId,
    kernel: document::ShapeBlurKernel,
    radius: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.shape_blur(id, kernel, radius))
}

/// Filter > Sharpen > Unsharp Mask on layer `id`.
#[tauri::command]
fn unsharp_mask(
    state: State<'_, AppState>,
    id: LayerId,
    radius: u32,
    amount: f32,
    threshold: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.unsharp_mask(id, radius, amount, threshold)
    })
}

/// Filter > Sharpen > Smart Sharpen on layer `id`.
#[tauri::command]
fn smart_sharpen(
    state: State<'_, AppState>,
    id: LayerId,
    radius: u32,
    amount: f32,
    reduce_noise: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.smart_sharpen(id, radius, amount, reduce_noise)
    })
}

/// Filter > Noise > Reduce Noise (Basic mode) on layer `id`.
#[tauri::command]
fn reduce_noise(
    state: State<'_, AppState>,
    id: LayerId,
    strength: u32,
    preserve_details: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.reduce_noise(id, strength, preserve_details)
    })
}

/// Filter > Blur > Blur on layer `id`.
#[tauri::command]
fn blur(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.blur(id))
}

/// Filter > Blur > Blur More on layer `id`.
#[tauri::command]
fn blur_more(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.blur_more(id))
}

/// Filter > Sharpen > Sharpen on layer `id`.
#[tauri::command]
fn sharpen(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.sharpen(id))
}

/// Filter > Sharpen > Sharpen More on layer `id`.
#[tauri::command]
fn sharpen_more(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.sharpen_more(id))
}

/// Filter > Sharpen > Sharpen Edges on layer `id`.
#[tauri::command]
fn sharpen_edges(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.sharpen_edges(id))
}

/// Filter > Noise > Median on layer `id`.
#[tauri::command]
fn median(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.median(id, radius))
}

/// Filter > Noise > Despeckle on layer `id`.
#[tauri::command]
fn despeckle(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.despeckle(id))
}

/// Filter > Noise > Dust & Scratches on layer `id`.
#[tauri::command]
fn dust_and_scratches(
    state: State<'_, AppState>,
    id: LayerId,
    radius: u32,
    threshold: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.dust_and_scratches(id, radius, threshold)
    })
}

/// Filter > Noise > Add Noise on layer `id`. The frontend sends a fresh
/// `seed` on every apply so repeated applications differ, as in Photoshop.
#[tauri::command]
fn add_noise(
    state: State<'_, AppState>,
    id: LayerId,
    amount: f32,
    gaussian: bool,
    monochromatic: bool,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_noise(id, amount, gaussian, monochromatic, seed)
    })
}

/// Image > Adjustments > Equalize on layer `id`. With a selection active,
/// `entire_image = false` is Photoshop's "Equalize selected area only" and
/// `true` is "Equalize entire image based on selected area"; with no
/// selection the flag makes no difference.
#[tauri::command]
fn equalize(
    state: State<'_, AppState>,
    id: LayerId,
    entire_image: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.equalize(id, entire_image))
}

/// Image > Adjustments > Auto Tone on layer `id`.
#[tauri::command]
fn auto_tone(
    state: State<'_, AppState>,
    id: LayerId,
    shadow_clip: Option<u32>,
    highlight_clip: Option<u32>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.auto_tone_clipped(id, shadow_clip.unwrap_or(0), highlight_clip.unwrap_or(0))
    })
}

/// Image > Adjustments > Auto Color on layer `id`: the clipped per-channel
/// stretch, then the mean snapped to neutral.
#[tauri::command]
fn auto_color(
    state: State<'_, AppState>,
    id: LayerId,
    shadow_clip: Option<u32>,
    highlight_clip: Option<u32>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.auto_color(id, shadow_clip.unwrap_or(0), highlight_clip.unwrap_or(0))
    })
}

/// Image > Adjustments > Auto Contrast on layer `id`.
#[tauri::command]
fn auto_contrast(
    state: State<'_, AppState>,
    id: LayerId,
    shadow_clip: Option<u32>,
    highlight_clip: Option<u32>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.auto_contrast_clipped(id, shadow_clip.unwrap_or(0), highlight_clip.unwrap_or(0))
    })
}

/// Image > Adjustments > Match Color on layer `id`, transferring
/// `source_layer_id`'s own per-channel statistics.
#[tauri::command]
fn match_color(
    state: State<'_, AppState>,
    id: LayerId,
    source_layer_id: LayerId,
    fade: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.match_color(id, source_layer_id, fade)
    })
}

/// Filter > Other > Maximum on layer `id`.
#[tauri::command]
fn maximum(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.maximum(id, radius))
}

/// Filter > Other > Minimum on layer `id`.
#[tauri::command]
fn minimum(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.minimum(id, radius))
}

/// Filter > Other > High Pass on layer `id`.
#[tauri::command]
fn high_pass(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.high_pass(id, radius))
}

/// Filter > Other > Offset (wrap around) on layer `id`.
#[tauri::command]
fn offset(state: State<'_, AppState>, id: LayerId, dx: i32, dy: i32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.offset(id, dx, dy))
}

/// Filter > Other > Custom on layer `id`: a 5×5 kernel with Scale and Offset.
#[tauri::command]
fn custom(
    state: State<'_, AppState>,
    id: LayerId,
    kernel: [i32; 25],
    scale: i32,
    offset: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.custom(id, kernel, scale, offset)
    })
}

/// Filter > Stylize > Find Edges on layer `id`.
#[tauri::command]
fn find_edges(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.find_edges(id))
}

/// Filter > Stylize > Solarize on layer `id`.
#[tauri::command]
fn solarize(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.solarize(id))
}

/// Filter > Stylize > Emboss on layer `id`.
#[tauri::command]
fn emboss(
    state: State<'_, AppState>,
    id: LayerId,
    angle: f32,
    height: u32,
    amount: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.emboss(id, angle, height, amount)
    })
}

/// Filter > Stylize > Trace Contour on layer `id`.
#[tauri::command]
fn trace_contour(
    state: State<'_, AppState>,
    id: LayerId,
    level: u8,
    upper: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.trace_contour(id, level, upper))
}

/// Filter > Blur > Gaussian Blur on layer `id`.
#[tauri::command]
fn gaussian_blur(state: State<'_, AppState>, id: LayerId, radius: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.gaussian_blur(id, radius))
}

/// Filter > Stylize > Glowing Edges on layer `id`.
#[tauri::command]
fn glowing_edges(
    state: State<'_, AppState>,
    id: LayerId,
    edge_width: u32,
    edge_brightness: u32,
    smoothness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.glowing_edges(id, edge_width, edge_brightness, smoothness)
    })
}

/// Filter > Pixelate > Mosaic on layer `id`.
#[tauri::command]
fn mosaic(state: State<'_, AppState>, id: LayerId, cell_size: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.mosaic(id, cell_size))
}

/// Filter > Pixelate > Fragment on layer `id`.
#[tauri::command]
fn fragment(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.fragment(id))
}

/// Filter > Distort > Ripple on layer `id`: `amplitude` in pixels,
/// `wavelength` in pixels.
#[tauri::command]
fn ripple(
    state: State<'_, AppState>,
    id: LayerId,
    amplitude: f32,
    wavelength: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.ripple(id, amplitude, wavelength)
    })
}

/// Filter > Blur > Radial Blur (Zoom method) on layer `id`.
#[tauri::command]
fn radial_blur(
    state: State<'_, AppState>,
    id: LayerId,
    amount: u32,
    center_x: f32,
    center_y: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.radial_blur(id, amount, center_x, center_y)
    })
}

/// Filter > Distort > Twirl on layer `id`: `angle` in degrees at the centre.
#[tauri::command]
fn twirl(state: State<'_, AppState>, id: LayerId, angle: f32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.twirl(id, angle))
}

/// Filter > Distort > Pinch on layer `id`: `amount` in percent, −100..=100.
#[tauri::command]
fn pinch(state: State<'_, AppState>, id: LayerId, amount: f32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.pinch(id, amount))
}

/// Filter > Distort > Spherize on layer `id`: `amount` in percent, −100..=100.
#[tauri::command]
fn spherize(state: State<'_, AppState>, id: LayerId, amount: f32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.spherize(id, amount))
}

/// Filter > Distort > ZigZag on layer `id`.
#[tauri::command]
fn zig_zag(
    state: State<'_, AppState>,
    id: LayerId,
    amount: f32,
    ridges: u32,
    style: ZigZagStyle,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.zig_zag(id, amount, ridges, style)
    })
}

/// Filter > Distort > Polar Coordinates on layer `id`; `to_polar` picks
/// Rectangular to Polar (true) or Polar to Rectangular (false).
#[tauri::command]
fn polar_coordinates(
    state: State<'_, AppState>,
    id: LayerId,
    to_polar: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.polar_coordinates(id, to_polar))
}

/// Filter > Distort > Wave on layer `id`. The frontend sends a fresh `seed`
/// on every apply so repeated applications differ, as with Add Noise.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn wave(
    state: State<'_, AppState>,
    id: LayerId,
    generators: u32,
    wavelength_min: u32,
    wavelength_max: u32,
    amplitude_min: u32,
    amplitude_max: u32,
    horizontal_scale: f32,
    vertical_scale: f32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.wave(
            id,
            generators,
            wavelength_min,
            wavelength_max,
            amplitude_min,
            amplitude_max,
            horizontal_scale,
            vertical_scale,
            seed,
        )
    })
}

/// Filter > Distort > Shear on layer `id`. `control_points` are evenly
/// spaced horizontal-offset anchors from the top row to the bottom row;
/// `wrap_around` picks Photoshop's Wrap Around undefined-area mode (true)
/// over the default Repeat Edge Pixels (false).
#[tauri::command]
fn shear(
    state: State<'_, AppState>,
    id: LayerId,
    control_points: Vec<f32>,
    wrap_around: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.shear(id, control_points, wrap_around)
    })
}

/// Filter > Distort > Displace on layer `id`, using `map_layer_id`'s
/// red/green channels as the horizontal/vertical displacement map.
#[tauri::command]
fn displace(
    state: State<'_, AppState>,
    id: LayerId,
    map_layer_id: LayerId,
    horizontal_scale: f32,
    vertical_scale: f32,
    wrap_around: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.displace(
            id,
            map_layer_id,
            horizontal_scale,
            vertical_scale,
            wrap_around,
        )
    })
}

/// Filter > Pixelate > Color Halftone on layer `id`.
#[tauri::command]
fn color_halftone(
    state: State<'_, AppState>,
    id: LayerId,
    max_radius: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.color_halftone(id, max_radius))
}

/// Filter > Pixelate > Mezzotint on layer `id`. The frontend sends a fresh
/// `seed` on every apply so repeated applications differ, as with Add Noise.
#[tauri::command]
fn mezzotint(
    state: State<'_, AppState>,
    id: LayerId,
    cell_size: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.mezzotint(id, cell_size, seed))
}

/// Filter > Stylize > Extrude on layer `id`. `random` picks Photoshop's
/// Random depth basis over Level-based; the frontend sends a fresh `seed`
/// on every apply so repeated Random applications differ, as with Add
/// Noise.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn extrude(
    state: State<'_, AppState>,
    id: LayerId,
    cell_size: u32,
    depth: u32,
    random: bool,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.extrude(id, cell_size, depth, random, seed)
    })
}

/// Filter > Render > Lighting Effects on layer `id`. `light_x`/`light_y`
/// are canvas-pixel coordinates of a single Point light; `light_height` is
/// its height above the surface. `intensity`/`ambience`/`bump_height` are
/// percentages (0-100). `color` tints the light.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn lighting_effects(
    state: State<'_, AppState>,
    id: LayerId,
    light_x: f32,
    light_y: f32,
    light_height: f32,
    intensity: u32,
    ambience: u32,
    bump_height: u32,
    color: [u8; 3],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.lighting_effects(
            id,
            light_x,
            light_y,
            light_height,
            intensity,
            ambience,
            bump_height,
            color,
        )
    })
}

/// Filter Gallery > Artistic > Colored Pencil on layer `id`.
#[tauri::command]
fn colored_pencil(
    state: State<'_, AppState>,
    id: LayerId,
    pencil_width: u32,
    stroke_pressure: u32,
    paper_brightness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.colored_pencil(id, pencil_width, stroke_pressure, paper_brightness)
    })
}

/// Filter Gallery > Artistic > Cutout on layer `id`.
#[tauri::command]
fn cutout(
    state: State<'_, AppState>,
    id: LayerId,
    levels: u32,
    edge_simplicity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.cutout(id, levels, edge_simplicity)
    })
}

/// Filter Gallery > Artistic > Dry Brush on layer `id`.
#[tauri::command]
fn dry_brush(
    state: State<'_, AppState>,
    id: LayerId,
    brush_size: u32,
    brush_detail: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.dry_brush(id, brush_size, brush_detail)
    })
}

/// Filter Gallery > Artistic > Film Grain on layer `id`. The frontend
/// sends a fresh `seed` on every apply so repeated applications differ,
/// as with Add Noise.
#[tauri::command]
fn film_grain(
    state: State<'_, AppState>,
    id: LayerId,
    grain: u32,
    highlight_area: u32,
    intensity: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.film_grain(id, grain, highlight_area, intensity, seed)
    })
}

/// Filter Gallery > Artistic > Neon Glow on layer `id`.
#[tauri::command]
fn neon_glow(
    state: State<'_, AppState>,
    id: LayerId,
    glow_size: u32,
    glow_brightness: u32,
    color: [u8; 3],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.neon_glow(id, glow_size, glow_brightness, color)
    })
}

/// Filter Gallery > Artistic > Poster Edges on layer `id`.
#[tauri::command]
fn poster_edges(
    state: State<'_, AppState>,
    id: LayerId,
    edge_thickness: u32,
    edge_intensity: u32,
    levels: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.poster_edges(id, edge_thickness, edge_intensity, levels)
    })
}

/// Filter Gallery > Artistic > Sponge on layer `id`. The frontend sends
/// a fresh `seed` on every apply, as with Crystallize.
#[tauri::command]
fn sponge(
    state: State<'_, AppState>,
    id: LayerId,
    brush_size: u32,
    definition: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.sponge(id, brush_size, definition, seed)
    })
}

/// Filter Gallery > Artistic > Watercolor on layer `id`.
#[tauri::command]
fn watercolor(
    state: State<'_, AppState>,
    id: LayerId,
    brush_detail: u32,
    shadow_intensity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.watercolor(id, brush_detail, shadow_intensity)
    })
}

/// Filter Gallery > Brush Strokes > Dark Strokes on layer `id`.
#[tauri::command]
fn dark_strokes(
    state: State<'_, AppState>,
    id: LayerId,
    balance: u32,
    black_intensity: u32,
    white_intensity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.dark_strokes(id, balance, black_intensity, white_intensity)
    })
}

/// Filter Gallery > Brush Strokes > Ink Outlines on layer `id`.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn ink_outlines(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    dark_intensity: u32,
    light_intensity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.ink_outlines(id, stroke_length, dark_intensity, light_intensity)
    })
}

/// Filter Gallery > Brush Strokes > Spatter on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Diffuse.
#[tauri::command]
fn spatter(
    state: State<'_, AppState>,
    id: LayerId,
    spray_radius: u32,
    smoothness: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.spatter(id, spray_radius, smoothness, seed)
    })
}

/// Filter Gallery > Brush Strokes > Crosshatch on layer `id`.
#[tauri::command]
fn crosshatch(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    sharpness: u32,
    strength: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.crosshatch(id, stroke_length, sharpness, strength)
    })
}

/// Filter Gallery > Brush Strokes > Accented Edges on layer `id`.
#[tauri::command]
fn accented_edges(
    state: State<'_, AppState>,
    id: LayerId,
    edge_width: u32,
    edge_brightness: u32,
    smoothness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.accented_edges(id, edge_width, edge_brightness, smoothness)
    })
}

/// Filter Gallery > Brush Strokes > Angled Strokes on layer `id`.
#[tauri::command]
fn angled_strokes(
    state: State<'_, AppState>,
    id: LayerId,
    direction_balance: u32,
    stroke_length: u32,
    sharpness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.angled_strokes(id, direction_balance, stroke_length, sharpness)
    })
}

/// Filter > Pixelate > Crystallize on layer `id`. The frontend sends a fresh
/// `seed` on every apply so repeated applications differ, as with Add Noise.
#[tauri::command]
fn crystallize(
    state: State<'_, AppState>,
    id: LayerId,
    cell_size: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.crystallize(id, cell_size, seed))
}

/// Filter > Pixelate > Facet on layer `id`. No dialog, matching Photoshop's
/// own Facet; the frontend sends a fresh `seed` on every apply so repeated
/// applications differ, as with Add Noise.
#[tauri::command]
fn facet(state: State<'_, AppState>, id: LayerId, seed: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.facet(id, seed))
}

/// Filter > Pixelate > Pointillize on layer `id`. `background` is an RGBA
/// colour for the gaps between dots. The frontend sends a fresh `seed` on
/// every apply so repeated applications differ, as with Add Noise.
#[tauri::command]
fn pointillize(
    state: State<'_, AppState>,
    id: LayerId,
    cell_size: u32,
    background: [u8; CHANNELS],
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.pointillize(id, cell_size, background, seed)
    })
}

/// Filter > Render > Clouds on layer `id`. `foreground`/`background` are the
/// two RGBA colours the noise field lerps between. The frontend sends a
/// fresh `seed` on every apply so repeated applications differ, as with Add
/// Noise.
#[tauri::command]
fn clouds(
    state: State<'_, AppState>,
    id: LayerId,
    foreground: [u8; CHANNELS],
    background: [u8; CHANNELS],
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.clouds(id, foreground, background, seed)
    })
}

/// Filter > Render > Difference Clouds on layer `id`. Same parameters as
/// [`clouds`], but blended with the layer's existing colour via the
/// Difference formula instead of replacing it.
#[tauri::command]
fn difference_clouds(
    state: State<'_, AppState>,
    id: LayerId,
    foreground: [u8; CHANNELS],
    background: [u8; CHANNELS],
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.difference_clouds(id, foreground, background, seed)
    })
}

/// Filter > Render > Fibers on layer `id`. `variance` is Photoshop's own
/// 1..=100 range and `strength` its own 1..=64 range. The frontend sends a
/// fresh `seed` on every apply so repeated applications differ, as with Add
/// Noise.
#[allow(clippy::too_many_arguments)]
#[tauri::command]
fn fibers(
    state: State<'_, AppState>,
    id: LayerId,
    variance: u32,
    strength: u32,
    foreground: [u8; CHANNELS],
    background: [u8; CHANNELS],
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.fibers(id, variance, strength, foreground, background, seed)
    })
}

/// Filter > Render > Lens Flare on layer `id`. `center_x`/`center_y` are
/// document pixel coordinates; `brightness` is Photoshop's own 10..=300 %
/// range.
#[tauri::command]
fn lens_flare(
    state: State<'_, AppState>,
    id: LayerId,
    center_x: f32,
    center_y: f32,
    brightness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.lens_flare(id, center_x, center_y, brightness)
    })
}

/// Filter > Blur > Surface Blur on layer `id`.
#[tauri::command]
fn surface_blur(
    state: State<'_, AppState>,
    id: LayerId,
    radius: u32,
    threshold: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.surface_blur(id, radius, threshold)
    })
}

/// Filter > Stylize > Diffuse on layer `id`. The frontend sends a fresh
/// `seed` on every apply so repeated applications differ, as in Photoshop.
#[tauri::command]
fn diffuse(
    state: State<'_, AppState>,
    id: LayerId,
    mode: DiffuseMode,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.diffuse(id, mode, seed))
}

/// Filter > Blur > Motion Blur on layer `id`.
#[tauri::command]
fn motion_blur(
    state: State<'_, AppState>,
    id: LayerId,
    angle: f32,
    distance: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.motion_blur(id, angle, distance))
}

/// Filter Gallery > Brush Strokes > Sprayed Strokes on layer `id`.
/// `direction`: 0 Right Diagonal, 1 Horizontal, 2 Left Diagonal, 3 Vertical.
#[tauri::command]
fn sprayed_strokes(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    spray_radius: u32,
    direction: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.sprayed_strokes(id, stroke_length, spray_radius, direction)
    })
}

/// Not checkpointed: dragging the slider fires this once per pointer move,
/// and the whole drag should undo as one step. The frontend checkpoints once
/// itself, when the drag starts.
#[tauri::command]
fn set_layer_opacity(
    state: State<'_, AppState>,
    id: LayerId,
    opacity: f32,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.set_opacity(id, opacity).map(|_| None)
    })
}

#[tauri::command]
fn set_layer_blend_mode(
    state: State<'_, AppState>,
    id: LayerId,
    blend_mode: BlendMode,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.set_blend_mode(id, blend_mode).map(|_| None)
    })
}

#[tauri::command]
fn remove_layer(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.remove_layer(id).map(|_| None))
}

/// Layer > Duplicate Layer on layer `id`.
#[tauri::command]
fn duplicate_layer(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.duplicate_layer(id).map(|_| None)
    })
}

#[tauri::command]
fn move_layer(
    state: State<'_, AppState>,
    id: LayerId,
    direction: MoveDirection,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.move_layer(id, direction).map(|_| None)
    })
}

#[tauri::command]
fn merge_visible(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.merge_visible().map(|_| None))
}

#[tauri::command]
fn flatten_image(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.flatten_image().map(|_| None))
}

#[tauri::command]
fn merge_down(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.merge_down(id).map(|_| None))
}

#[tauri::command]
fn sample_color(state: State<'_, AppState>, x: u32, y: u32) -> Result<[u8; 4], String> {
    sample_pixel_color(&state.composite, x, y)
}

/// Paint `color` (RGBA8) along `points` (document pixel coordinates) onto
/// layer `id`, with normal `source-over` blending. `points` is the polyline
/// since the previous pointer event, not the whole stroke — the frontend
/// calls this once per pointer move, so each call's own bounding box stays
/// small regardless of how long the drag has run. Not checkpointed for the
/// same reason: the frontend checkpoints once, when the stroke starts, so
/// the whole stroke undoes as one step.
///
/// [`document::Document::stroke`] hands back exactly which pixels it
/// touched, so [`snapshot`] recomposites just that rect instead of the whole
/// document — the point of each call's bounding box staying small.
#[tauri::command]
fn paint_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    color: [u8; 4],
    symmetry: Option<document::Symmetry>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke_symmetric(id, &points, radius, Stroke::Brush { color }, symmetry)
    })
}

/// Erase along `points` on layer `id`: multiplies existing alpha toward zero
/// rather than painting a colour. See [`paint_stroke`] for `points`.
#[tauri::command]
fn erase_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    symmetry: Option<document::Symmetry>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke_symmetric(id, &points, radius, Stroke::Eraser, symmetry)
    })
}

/// Dodge tool: lighten along `points` on layer `id` by `exposure` percent.
/// See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn dodge_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    exposure: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Dodge { exposure })
    })
}

/// Burn tool: darken along `points` on layer `id` by `exposure` percent.
/// See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn burn_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    exposure: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Burn { exposure })
    })
}

/// Sponge tool: saturate (or desaturate) along `points` on layer `id` by
/// `flow` percent. See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn sponge_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    flow: u8,
    saturate: bool,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Sponge { flow, saturate })
    })
}

/// Blur tool: soften along `points` on layer `id` by `strength` percent.
/// See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn blur_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    strength: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Blur { strength })
    })
}

/// Sharpen tool: sharpen along `points` on layer `id` by `strength`
/// percent. See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn sharpen_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    strength: u8,
    protect_detail: Option<bool>,
    sample_all_layers: Option<bool>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(
            id,
            &points,
            radius,
            Stroke::Sharpen {
                strength,
                protect_detail: protect_detail.unwrap_or(false),
                sample_all_layers: sample_all_layers.unwrap_or(false),
            },
        )
    })
}

/// Clone Stamp tool: paint the pre-stroke layer's pixels `offset` away
/// along `points` on layer `id`. See [`paint_stroke`] for `points` and
/// checkpointing.
#[tauri::command]
fn clone_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    offset: (i32, i32),
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Clone { offset })
    })
}

/// Smudge tool: drag colour along `points` on layer `id` by `strength`
/// percent. See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn smudge_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    strength: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Smudge { strength })
    })
}

/// Color Replacement tool: recolour pixels near the colour under the
/// stroke's start along `points` on layer `id` with `color`'s hue and
/// saturation. See [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn color_replace_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    color: [u8; 3],
    tolerance: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(
            id,
            &points,
            radius,
            Stroke::ColorReplace { color, tolerance },
        )
    })
}

/// Background Eraser: erase, along `points` on layer `id`, only pixels
/// within `tolerance` of the colour under the stroke's start. See
/// [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn background_erase_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    tolerance: u8,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::BackgroundErase { tolerance })
    })
}

/// Healing Brush: paint the pre-stroke layer's texture `offset` away,
/// matched to the destination's tone, along `points` on layer `id`. See
/// [`paint_stroke`] for `points` and checkpointing.
#[tauri::command]
fn heal_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    offset: (i32, i32),
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Heal { offset })
    })
}

/// Spot Healing Brush: replace pixels along `points` on layer `id` with
/// the mean of their pre-stroke surroundings. See [`paint_stroke`] for
/// `points` and checkpointing.
#[tauri::command]
fn spot_heal_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::SpotHeal)
    })
}

/// Remove tool: fill pixels along `points` on layer `id` from the
/// surroundings outside the brushed area. See [`paint_stroke`] for
/// `points` and checkpointing.
#[tauri::command]
fn remove_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Remove)
    })
}

/// Rectangle tool (Pixels mode): fill and/or inside-stroke an
/// axis-aligned, optionally rounded rectangle onto a layer.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn draw_rectangle(
    state: State<'_, AppState>,
    id: LayerId,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    radius: u32,
    fill: Option<[u8; 4]>,
    stroke: Option<([u8; 4], u32)>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_rectangle(id, x0, y0, x1, y1, radius, fill, stroke)
    })
}

/// Ellipse tool (Pixels mode): fill and/or inside-stroke the ellipse
/// inscribed in a dragged box onto a layer.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn draw_ellipse(
    state: State<'_, AppState>,
    id: LayerId,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    fill: Option<[u8; 4]>,
    stroke: Option<([u8; 4], u32)>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_ellipse(id, x0, y0, x1, y1, fill, stroke)
    })
}

/// Line tool (Pixels mode): paint a straight line of a given weight.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn draw_line(
    state: State<'_, AppState>,
    id: LayerId,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    weight: u32,
    color: [u8; 4],
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_line(id, x0, y0, x1, y1, weight, color)
    })
}

/// Polygon tool (Pixels mode): paint a regular polygon dragged out from
/// its centre.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn draw_polygon(
    state: State<'_, AppState>,
    id: LayerId,
    cx: f32,
    cy: f32,
    x: f32,
    y: f32,
    sides: u32,
    color: [u8; 4],
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_polygon(id, cx, cy, x, y, sides, color)
    })
}

/// Star tool (Pixels mode): paint a star dragged out from its centre,
/// with `ratio` percent inner points.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn draw_star(
    state: State<'_, AppState>,
    id: LayerId,
    cx: f32,
    cy: f32,
    x: f32,
    y: f32,
    points: u32,
    ratio: u32,
    color: [u8; 4],
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_star(id, cx, cy, x, y, points, ratio, color)
    })
}

/// Triangle tool (Pixels mode): paint the isosceles triangle fitted to a
/// dragged box.
#[tauri::command]
fn draw_triangle(
    state: State<'_, AppState>,
    id: LayerId,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: [u8; 4],
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.draw_triangle(id, x0, y0, x1, y1, color)
    })
}

/// History Brush: remember the current document as the state the brush
/// paints from. Not an edit — nothing to checkpoint.
#[tauri::command]
fn set_history_source(state: State<'_, AppState>) -> Result<Snapshot, String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    *state
        .history_source
        .lock()
        .map_err(|_| POISONED.to_string())? = Some(document.clone());
    snapshot(&state, document, None)
}

/// History Brush: paint layer `id`'s pixels back from the remembered
/// source along `points`. See [`paint_stroke`] for `points` and
/// checkpointing.
#[tauri::command]
fn history_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
) -> Result<Snapshot, String> {
    let source = {
        let guard = state
            .history_source
            .lock()
            .map_err(|_| POISONED.to_string())?;
        let remembered = guard.as_ref().ok_or_else(|| {
            "Set a history source first (History Brush > Set Source).".to_string()
        })?;
        remembered
            .layers()
            .iter()
            .find(|layer| layer.id == id)
            .ok_or_else(|| "The history source has no layer with that id.".to_string())?
            .pixels
            .clone()
    };
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::History { source: &source })
    })
}

/// Pattern Stamp tool: paint the defined pattern along `points` on layer
/// `id`, tiles aligned to the canvas origin. See [`paint_stroke`] for
/// `points` and checkpointing.
#[tauri::command]
fn pattern_stamp_stroke(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(f32, f32)>,
    radius: f32,
    opacity: u8,
    symmetry: Option<document::Symmetry>,
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke_symmetric(
            id,
            &points,
            radius,
            Stroke::PatternStamp { opacity },
            symmetry,
        )
    })
}

/// Paint Bucket: flood-fill from `(x, y)` on layer `id` with `color`. A
/// whole, discrete action on its own (not one step of a longer gesture, the
/// way a brush stroke is), so it checkpoints itself.
#[tauri::command]
fn flood_fill(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
    color: [u8; 4],
    tolerance: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.flood_fill(id, x, y, color, tolerance)
    })
}

/// Magic Eraser: erase to transparency, at `opacity`, every pixel of layer
/// `id` the Magic Wand would select from a click at `(x, y)`. A whole,
/// discrete action like the Paint Bucket, so it checkpoints itself.
#[tauri::command]
fn magic_erase(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
    tolerance: u8,
    contiguous: bool,
    opacity: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.magic_erase(id, x, y, tolerance, contiguous, opacity)
    })
}

/// Red Eye tool: neutralise the red-dominant region around a click at
/// `(x, y)` on layer `id`, darkening it by `darken` percent. A whole,
/// discrete action, so it checkpoints itself.
#[tauri::command]
fn red_eye(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
    darken: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.red_eye(id, x, y, darken))
}

/// Ruler tool: the width, height, distance, and angle of a drag from
/// `(x0, y0)` to `(x1, y1)` in document pixels. Pure geometry — touches no
/// document state.
#[tauri::command]
fn ruler_measure(x0: f32, y0: f32, x1: f32, y1: f32) -> Result<document::Measurement, String> {
    document::measure(x0, y0, x1, y1)
}

/// Gradient (Linear): blends `start_color` to `end_color` from `(x0, y0)`
/// to `(x1, y1)` on layer `id`. A whole, discrete action on its own, so it
/// checkpoints itself, the same as [`flood_fill`].
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn gradient_fill(
    state: State<'_, AppState>,
    id: LayerId,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    start_color: [u8; 4],
    end_color: [u8; 4],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.gradient_fill(id, (x0, y0), (x1, y1), start_color, end_color)
    })
}

/// Image > Adjustments > Invert on layer `id`: flip every RGB channel,
/// leaving alpha untouched. A whole, discrete action on its own, so it
/// checkpoints itself, the same as [`flood_fill`] and [`gradient_fill`].
#[tauri::command]
fn invert_colors(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.invert_colors(id))
}

/// Image > Adjustments > Threshold on layer `id`: converts each pixel to
/// pure black or white based on luma against `level`. A whole, discrete
/// action on its own, so it checkpoints itself.
#[tauri::command]
fn threshold(state: State<'_, AppState>, id: LayerId, level: u8) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.threshold(id, level))
}

/// Image > Adjustments > Posterize on layer `id`: quantize each RGB
/// channel to `levels` evenly spaced tones. A whole, discrete action on
/// its own, so it checkpoints itself.
#[tauri::command]
fn posterize(state: State<'_, AppState>, id: LayerId, levels: u8) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.posterize(id, levels))
}

/// Image > Adjustments > Brightness/Contrast on layer `id`.
#[tauri::command]
fn brightness_contrast(
    state: State<'_, AppState>,
    id: LayerId,
    brightness: i32,
    contrast: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.brightness_contrast(id, brightness, contrast)
    })
}

/// Filter Gallery > Brush Strokes > Sumi-e on layer `id`.
#[tauri::command]
fn sumi_e(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_width: u32,
    stroke_pressure: u32,
    contrast: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.sumi_e(id, stroke_width, stroke_pressure, contrast)
    })
}

/// Filter Gallery > Artistic > Smudge Stick on layer `id`.
#[tauri::command]
fn smudge_stick(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    highlight_area: u32,
    intensity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.smudge_stick(id, stroke_length, highlight_area, intensity)
    })
}

/// Filter Gallery > Artistic > Paint Daubs on layer `id`.
#[tauri::command]
fn paint_daubs(
    state: State<'_, AppState>,
    id: LayerId,
    brush_size: u32,
    sharpness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.paint_daubs(id, brush_size, sharpness)
    })
}

/// Filter Gallery > Artistic > Palette Knife on layer `id`.
#[tauri::command]
fn palette_knife(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_size: u32,
    stroke_detail: u32,
    softness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.palette_knife(id, stroke_size, stroke_detail, softness)
    })
}

/// Filter Gallery > Artistic > Plastic Wrap on layer `id`.
#[tauri::command]
fn plastic_wrap(
    state: State<'_, AppState>,
    id: LayerId,
    highlight_strength: u32,
    detail: u32,
    smoothness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.plastic_wrap(id, highlight_strength, detail, smoothness)
    })
}

/// Filter Gallery > Artistic > Fresco on layer `id`.
#[tauri::command]
fn fresco(
    state: State<'_, AppState>,
    id: LayerId,
    brush_size: u32,
    brush_detail: u32,
    texture: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.fresco(id, brush_size, brush_detail, texture)
    })
}

/// Filter Gallery > Artistic > Rough Pastels on layer `id`.
#[tauri::command]
fn rough_pastels(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    stroke_detail: u32,
    relief: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.rough_pastels(id, stroke_length, stroke_detail, relief)
    })
}

/// Filter Gallery > Artistic > Underpainting on layer `id`.
#[tauri::command]
fn underpainting(
    state: State<'_, AppState>,
    id: LayerId,
    brush_size: u32,
    texture_coverage: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.underpainting(id, brush_size, texture_coverage)
    })
}

/// Filter Gallery > Sketch > Stamp on layer `id`.
#[tauri::command]
fn stamp(
    state: State<'_, AppState>,
    id: LayerId,
    light_dark_balance: u32,
    smoothness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.stamp(id, light_dark_balance, smoothness)
    })
}

/// Filter Gallery > Sketch > Photocopy on layer `id`.
#[tauri::command]
fn photocopy(
    state: State<'_, AppState>,
    id: LayerId,
    detail: u32,
    darkness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.photocopy(id, detail, darkness))
}

/// Filter Gallery > Sketch > Reticulation on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Film Grain.
#[tauri::command]
fn reticulation(
    state: State<'_, AppState>,
    id: LayerId,
    density: u32,
    foreground_level: u32,
    background_level: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.reticulation(id, density, foreground_level, background_level, seed)
    })
}

/// Filter Gallery > Sketch > Note Paper on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Film Grain.
#[tauri::command]
fn note_paper(
    state: State<'_, AppState>,
    id: LayerId,
    image_balance: u32,
    graininess: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.note_paper(id, image_balance, graininess, seed)
    })
}

/// Filter Gallery > Sketch > Graphic Pen on layer `id`.
/// `direction`: 0 Right Diagonal, 1 Horizontal, 2 Left Diagonal, 3 Vertical.
#[tauri::command]
fn graphic_pen(
    state: State<'_, AppState>,
    id: LayerId,
    stroke_length: u32,
    light_dark_balance: u32,
    direction: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.graphic_pen(id, stroke_length, light_dark_balance, direction)
    })
}

/// Filter Gallery > Sketch > Chalk & Charcoal on layer `id`.
#[tauri::command]
fn chalk_and_charcoal(
    state: State<'_, AppState>,
    id: LayerId,
    charcoal_area: u32,
    chalk_area: u32,
    stroke_pressure: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.chalk_and_charcoal(id, charcoal_area, chalk_area, stroke_pressure)
    })
}

/// Filter Gallery > Sketch > Plaster on layer `id`. `light_direction`: 0
/// Top, 1 Top Right, 2 Right, 3 Bottom Right, 4 Bottom, 5 Bottom Left, 6
/// Left, 7 Top Left.
#[tauri::command]
fn plaster(
    state: State<'_, AppState>,
    id: LayerId,
    image_balance: u32,
    smoothness: u32,
    light_direction: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.plaster(id, image_balance, smoothness, light_direction)
    })
}

/// Filter Gallery > Sketch > Water Paper on layer `id`.
#[tauri::command]
fn water_paper(
    state: State<'_, AppState>,
    id: LayerId,
    fiber_length: u32,
    brightness: u32,
    contrast: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.water_paper(id, fiber_length, brightness, contrast)
    })
}

/// Filter Gallery > Sketch > Torn Edges on layer `id`. The frontend sends
/// a fresh `seed` on every apply, as with Film Grain.
#[tauri::command]
fn torn_edges(
    state: State<'_, AppState>,
    id: LayerId,
    image_balance: u32,
    smoothness: u32,
    contrast: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.torn_edges(id, image_balance, smoothness, contrast, seed)
    })
}

/// Filter Gallery > Sketch > Bas Relief on layer `id`.
#[tauri::command]
fn bas_relief(
    state: State<'_, AppState>,
    id: LayerId,
    detail: u32,
    smoothness: u32,
    light_direction: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.bas_relief(id, detail, smoothness, light_direction)
    })
}

/// Filter Gallery > Sketch > Halftone Pattern on layer `id`.
#[tauri::command]
fn halftone_pattern(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    contrast: u32,
    pattern_type: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.halftone_pattern(id, size, contrast, pattern_type)
    })
}

/// Filter Gallery > Sketch > Chrome on layer `id`.
#[tauri::command]
fn chrome(
    state: State<'_, AppState>,
    id: LayerId,
    detail: u32,
    smoothness: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.chrome(id, detail, smoothness))
}

/// Filter Gallery > Distort > Diffuse Glow on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Film Grain.
#[tauri::command]
fn diffuse_glow(
    state: State<'_, AppState>,
    id: LayerId,
    graininess: u32,
    glow_amount: u32,
    clear_amount: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.diffuse_glow(id, graininess, glow_amount, clear_amount, seed)
    })
}

/// Filter Gallery > Distort > Glass on layer `id`. The frontend sends a
/// fresh `seed` on every apply, as with Diffuse Glow.
#[tauri::command]
fn glass(
    state: State<'_, AppState>,
    id: LayerId,
    distortion: u32,
    smoothness: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.glass(id, distortion, smoothness, seed)
    })
}

/// Filter Gallery > Distort > Ocean Ripple on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Glass.
#[tauri::command]
fn ocean_ripple(
    state: State<'_, AppState>,
    id: LayerId,
    ripple_size: u32,
    ripple_magnitude: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.ocean_ripple(id, ripple_size, ripple_magnitude, seed)
    })
}

/// Filter > Stylize > Wind on layer `id`.
#[tauri::command]
fn wind(
    state: State<'_, AppState>,
    id: LayerId,
    method: u32,
    direction: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.wind(id, method, direction))
}

/// Filter Gallery > Texture > Grain on layer `id`. The frontend sends a
/// fresh `seed` on every apply, as with Film Grain.
#[tauri::command]
fn grain(
    state: State<'_, AppState>,
    id: LayerId,
    intensity: u32,
    contrast: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.grain(id, intensity, contrast, seed)
    })
}

/// Filter > Stylize > Tiles on layer `id`. The frontend sends a fresh
/// `seed` on every apply, as with Glass.
#[tauri::command]
fn tiles(
    state: State<'_, AppState>,
    id: LayerId,
    tile_size: u32,
    max_offset: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.tiles(id, tile_size, max_offset, seed)
    })
}

/// Filter Gallery > Texture > Mosaic Tiles on layer `id`.
#[tauri::command]
fn mosaic_tiles(
    state: State<'_, AppState>,
    id: LayerId,
    tile_size: u32,
    grout_width: u32,
    lighten_grout: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.mosaic_tiles(id, tile_size, grout_width, lighten_grout)
    })
}

/// Filter Gallery > Texture > Patchwork on layer `id`.
#[tauri::command]
fn patchwork(
    state: State<'_, AppState>,
    id: LayerId,
    square_size: u32,
    relief: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.patchwork(id, square_size, relief)
    })
}

/// Filter Gallery > Texture > Stained Glass on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Crystallize.
#[tauri::command]
fn stained_glass(
    state: State<'_, AppState>,
    id: LayerId,
    cell_size: u32,
    border_thickness: u32,
    light_intensity: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.stained_glass(id, cell_size, border_thickness, light_intensity, seed)
    })
}

/// Filter Gallery > Texture > Craquelure on layer `id`. The frontend
/// sends a fresh `seed` on every apply, as with Stained Glass.
#[tauri::command]
fn craquelure(
    state: State<'_, AppState>,
    id: LayerId,
    crack_spacing: u32,
    crack_depth: u32,
    crack_brightness: u32,
    seed: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.craquelure(id, crack_spacing, crack_depth, crack_brightness, seed)
    })
}

/// Image > Adjustments > Selective Color on layer `id`.
#[tauri::command]
fn selective_color(
    state: State<'_, AppState>,
    id: LayerId,
    cyan: i32,
    magenta: i32,
    yellow: i32,
    black: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.selective_color(id, cyan, magenta, yellow, black)
    })
}

/// Layer > Layer Style > Stroke on layer `id`, baked in destructively.
#[tauri::command]
fn stroke_outline(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.stroke_outline(id, size, color, opacity)
    })
}

/// Layer > Layer Style > Color Overlay on layer `id`, baked in
/// destructively.
#[tauri::command]
fn color_overlay(
    state: State<'_, AppState>,
    id: LayerId,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.color_overlay(id, color, opacity)
    })
}

/// Layer > Layer Style > Gradient Overlay on layer `id`, baked in
/// destructively.
#[tauri::command]
fn gradient_overlay(
    state: State<'_, AppState>,
    id: LayerId,
    color1: [u8; 3],
    color2: [u8; 3],
    direction: u32,
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.gradient_overlay(id, color1, color2, direction, opacity)
    })
}

/// Layer > Layer Style > Outer Glow on layer `id`, baked in
/// destructively.
#[tauri::command]
fn outer_glow(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.outer_glow(id, size, color, opacity)
    })
}

/// Layer > Layer Style > Inner Glow on layer `id`, baked in
/// destructively.
#[tauri::command]
fn inner_glow(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.inner_glow(id, size, color, opacity)
    })
}

/// Layer > Layer Style > Drop Shadow on layer `id`, baked in
/// destructively.
#[tauri::command]
fn drop_shadow(
    state: State<'_, AppState>,
    id: LayerId,
    distance: u32,
    angle: f32,
    size: u32,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.drop_shadow(id, distance, angle, size, color, opacity)
    })
}

/// Layer > Layer Style > Inner Shadow on layer `id`, baked in
/// destructively.
#[tauri::command]
fn inner_shadow(
    state: State<'_, AppState>,
    id: LayerId,
    distance: u32,
    angle: f32,
    size: u32,
    color: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.inner_shadow(id, distance, angle, size, color, opacity)
    })
}

/// Layer > Layer Style > Pattern Overlay on layer `id`, baked in
/// destructively.
#[tauri::command]
fn pattern_overlay(
    state: State<'_, AppState>,
    id: LayerId,
    scale: u32,
    color1: [u8; 3],
    color2: [u8; 3],
    opacity: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.pattern_overlay(id, scale, color1, color2, opacity)
    })
}

/// Layer > Layer Style > Bevel & Emboss on layer `id`, baked in
/// destructively.
#[tauri::command]
fn bevel_emboss(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    light_direction: u32,
    strength: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.bevel_emboss(id, size, light_direction, strength)
    })
}

/// Layer > Layer Style > Contour on layer `id`, baked in destructively.
#[tauri::command]
fn contour(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    light_direction: u32,
    strength: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.contour(id, size, light_direction, strength)
    })
}

/// Layer > Layer Style > Texture on layer `id`, baked in destructively.
#[tauri::command]
fn texture(
    state: State<'_, AppState>,
    id: LayerId,
    size: u32,
    light_direction: u32,
    strength: u32,
    scale: u32,
    depth: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.texture(id, size, light_direction, strength, scale, depth)
    })
}

/// Filter Gallery > Texture > Texturizer on layer `id`.
#[tauri::command]
fn texturizer(
    state: State<'_, AppState>,
    id: LayerId,
    scale: u32,
    relief: u32,
    light_direction: u32,
    invert: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.texturizer(id, scale, relief, light_direction, invert)
    })
}

/// Image > Adjustments > Hue/Saturation on layer `id`.
#[tauri::command]
fn hue_saturation(
    state: State<'_, AppState>,
    id: LayerId,
    hue: i32,
    saturation: i32,
    lightness: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.hue_saturation(id, hue, saturation, lightness)
    })
}

/// Image > Adjustments > Replace Color on layer `id`.
#[tauri::command]
fn replace_color(
    state: State<'_, AppState>,
    id: LayerId,
    target: [u8; 3],
    fuzziness: u32,
    hue: i32,
    saturation: i32,
    lightness: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.replace_color(id, target, fuzziness, hue, saturation, lightness)
    })
}

/// Image > Adjustments > Black & White on layer `id`.
#[tauri::command]
fn black_and_white(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.black_and_white(id))
}

/// Image > Adjustments > Vibrance on layer `id`.
#[tauri::command]
fn vibrance(
    state: State<'_, AppState>,
    id: LayerId,
    vibrance: i32,
    saturation: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.vibrance(id, vibrance, saturation)
    })
}

/// Image > Adjustments > Photo Filter on layer `id`: tints toward `color`
/// by `density` percent.
#[tauri::command]
fn photo_filter(
    state: State<'_, AppState>,
    id: LayerId,
    color: [u8; 3],
    density: u8,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.photo_filter(id, color, density))
}

/// Camera Raw Filter > Temperature/Tint on layer `id`.
#[tauri::command]
fn temperature_tint(
    state: State<'_, AppState>,
    id: LayerId,
    temperature: i32,
    tint: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.temperature_tint(id, temperature, tint)
    })
}

/// Image > Adjustments > Exposure on layer `id`.
#[tauri::command]
fn exposure(
    state: State<'_, AppState>,
    id: LayerId,
    exposure: i32,
    offset: i32,
    gamma: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.exposure(id, exposure, offset, gamma)
    })
}

/// Image > Adjustments > Gradient Map on layer `id`: maps luma to a point
/// between `shadow_color` and `highlight_color`.
#[tauri::command]
fn gradient_map(
    state: State<'_, AppState>,
    id: LayerId,
    shadow_color: [u8; 3],
    highlight_color: [u8; 3],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.gradient_map(id, shadow_color, highlight_color)
    })
}

/// Image > Adjustments > Channel Mixer on layer `id`: `matrix[c]` is
/// `[r_coeff, g_coeff, b_coeff, constant]` for output channel `c` (R, G,
/// B in that order).
#[tauri::command]
fn channel_mixer(
    state: State<'_, AppState>,
    id: LayerId,
    matrix: [[i32; 4]; 3],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.channel_mixer(id, matrix))
}

/// Image > Adjustments > Levels on layer `id`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn levels(
    state: State<'_, AppState>,
    id: LayerId,
    input_black: u8,
    input_white: u8,
    gamma: i32,
    output_black: u8,
    output_white: u8,
    channel: Option<document::LevelsChannel>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.levels_on(
            id,
            channel.unwrap_or(document::LevelsChannel::Rgb),
            input_black,
            input_white,
            gamma,
            output_black,
            output_white,
        )
    })
}

/// Image > Adjustments > Curves on layer `id`.
#[tauri::command]
fn curves(state: State<'_, AppState>, id: LayerId, points: [u8; 5]) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.curves(id, points))
}

/// Image > Adjustments > Curves with per-channel curves: the RGB composite
/// list plus one list each for Red, Green, and Blue, on layer `id`.
#[tauri::command]
fn curves_channels(
    state: State<'_, AppState>,
    id: LayerId,
    rgb: Vec<(u8, u8)>,
    red: Vec<(u8, u8)>,
    green: Vec<(u8, u8)>,
    blue: Vec<(u8, u8)>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.curves_channels(id, &rgb, &red, &green, &blue)
    })
}

/// The Curves dialog's graph: the 256-entry lookup table `points` describe.
/// Read-only; needs no document.
#[tauri::command]
fn curves_lookup(points: Vec<(u8, u8)>) -> Result<Vec<u8>, String> {
    document::curve_lookup(&points).map(|lut| lut.to_vec())
}

/// Levels/Curves Black Point eyedropper: make pixel `(x, y)` of layer `id`
/// black, per channel.
#[tauri::command]
fn levels_black_point(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.levels_black_point(id, x, y))
}

/// Levels/Curves White Point eyedropper: make pixel `(x, y)` of layer `id`
/// white, per channel.
#[tauri::command]
fn levels_white_point(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.levels_white_point(id, x, y))
}

/// Levels/Curves Gray Point eyedropper: make pixel `(x, y)` of layer `id`
/// neutral, per-channel gamma.
#[tauri::command]
fn levels_gray_point(
    state: State<'_, AppState>,
    id: LayerId,
    x: u32,
    y: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.levels_gray_point(id, x, y))
}

/// Image > Adjustments > Curves in Point mode: arbitrary `(input, output)`
/// control points on layer `id`.
#[tauri::command]
fn curves_points(
    state: State<'_, AppState>,
    id: LayerId,
    points: Vec<(u8, u8)>,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.curves_points(id, &points))
}

/// Image > Adjustments > Color Balance on layer `id`.
#[tauri::command]
fn color_balance(
    state: State<'_, AppState>,
    id: LayerId,
    shadows: [i32; 3],
    midtones: [i32; 3],
    highlights: [i32; 3],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.color_balance(id, shadows, midtones, highlights)
    })
}

/// Camera Raw Filter > Highlights/Shadows on layer `id`.
#[tauri::command]
fn highlights_shadows(
    state: State<'_, AppState>,
    id: LayerId,
    highlights: i32,
    shadows: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.highlights_shadows(id, highlights, shadows)
    })
}

/// Camera Raw Filter > Clarity on layer `id`.
#[tauri::command]
fn clarity(state: State<'_, AppState>, id: LayerId, amount: i32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.clarity(id, amount))
}

/// Camera Raw Filter > Optics > Defringe on layer `id`.
#[tauri::command]
fn defringe(state: State<'_, AppState>, id: LayerId, amount: u32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.defringe(id, amount))
}

/// Filter Gallery > Blur Gallery > Tilt-Shift on layer `id`.
#[tauri::command]
fn tilt_shift(
    state: State<'_, AppState>,
    id: LayerId,
    focus_row: u32,
    half_height: u32,
    blur_radius: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.tilt_shift(id, focus_row, half_height, blur_radius)
    })
}

/// Filter Gallery > Blur Gallery > Iris Blur on layer `id`.
#[tauri::command]
fn iris_blur(
    state: State<'_, AppState>,
    id: LayerId,
    center_x: f32,
    center_y: f32,
    radius: f32,
    blur_radius: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.iris_blur(id, center_x, center_y, radius, blur_radius)
    })
}

/// Filter Gallery > Blur Gallery > Field Blur (two pins) on layer `id`.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
fn field_blur(
    state: State<'_, AppState>,
    id: LayerId,
    x1: f32,
    y1: f32,
    radius1: u32,
    x2: f32,
    y2: f32,
    radius2: u32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.field_blur(id, x1, y1, radius1, x2, y2, radius2)
    })
}

/// Filter Gallery > Blur Gallery > Spin Blur on layer `id`.
#[tauri::command]
fn spin_blur(
    state: State<'_, AppState>,
    id: LayerId,
    center_x: f32,
    center_y: f32,
    angle: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.spin_blur(id, center_x, center_y, angle)
    })
}

/// Filter > Blur > Lens Blur on layer `id`, driven by its own alpha channel.
#[tauri::command]
fn lens_blur(
    state: State<'_, AppState>,
    id: LayerId,
    max_radius: u32,
    invert: bool,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.lens_blur(id, max_radius, invert)
    })
}

/// Camera Raw Filter > Basic > Saturation on layer `id`.
#[tauri::command]
fn camera_raw_saturation(
    state: State<'_, AppState>,
    id: LayerId,
    saturation: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.camera_raw_saturation(id, saturation)
    })
}

/// Camera Raw Filter > Histogram: read-only per-channel 256-bin counts of
/// layer `id`'s own R, G, and B values over the selection (or whole layer).
/// Nested `Vec`s rather than `[[u32; 256]; 3]` only because serde has no
/// serializer for arrays that long.
#[tauri::command]
fn histogram(state: State<'_, AppState>, id: LayerId) -> Result<Vec<Vec<u32>>, String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    let counts = document.histogram(id)?;
    Ok(counts.iter().map(|channel| channel.to_vec()).collect())
}

/// Camera Raw Filter > RGB Levels: layer `id`'s own RGBA8 pixel at `(x, y)`.
#[tauri::command]
fn rgb_levels(state: State<'_, AppState>, id: LayerId, x: u32, y: u32) -> Result<[u8; 4], String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    document.layer_pixel(id, x, y)
}

/// Color Sampler tool: the composited RGBA8 value under each of `points`,
/// in order. Read-only.
#[tauri::command]
fn sample_points(
    state: State<'_, AppState>,
    points: Vec<(u32, u32)>,
) -> Result<Vec<[u8; 4]>, String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    document.sample_points(&points)
}

/// Camera Raw Filter > Shadow Clipping: per-channel counts of layer `id`'s
/// sampled pixels clipped to 0, over the selection (or whole layer).
#[tauri::command]
fn shadow_clipping(state: State<'_, AppState>, id: LayerId) -> Result<[u32; 3], String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    document.shadow_clipping(id)
}

/// Camera Raw Filter > Curve > Point Curve on layer `id`.
#[tauri::command]
fn camera_raw_point_curve(
    state: State<'_, AppState>,
    id: LayerId,
    points: [u8; 5],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.camera_raw_point_curve(id, points)
    })
}

/// Camera Raw Filter > Color Grading on layer `id`; each wheel is
/// `[hue_degrees, saturation]`.
#[tauri::command]
fn color_grading(
    state: State<'_, AppState>,
    id: LayerId,
    shadows: [i32; 2],
    midtones: [i32; 2],
    highlights: [i32; 2],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.color_grading(id, shadows, midtones, highlights)
    })
}

/// Camera Raw Filter > Color Mixer on layer `id`: shift one hue range
/// (`0` Reds through `7` Magentas) by hue/saturation/luminance.
#[tauri::command]
fn color_mixer(
    state: State<'_, AppState>,
    id: LayerId,
    range: u8,
    hue: i32,
    saturation: i32,
    luminance: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.color_mixer(id, range, hue, saturation, luminance)
    })
}

/// Camera Raw Filter > Point Color on layer `id`.
#[tauri::command]
fn point_color(
    state: State<'_, AppState>,
    id: LayerId,
    target: [u8; 3],
    range: u32,
    hue: i32,
    saturation: i32,
    luminance: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.point_color(id, target, range, hue, saturation, luminance)
    })
}

/// Camera Raw Filter > Curve > Parametric Curve on layer `id`.
#[tauri::command]
fn parametric_curve(
    state: State<'_, AppState>,
    id: LayerId,
    highlights: i32,
    lights: i32,
    darks: i32,
    shadows: i32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.parametric_curve(id, highlights, lights, darks, shadows)
    })
}

/// Filter > Camera Raw Filter on layer `id`: every panel at once, as one
/// undo step.
#[tauri::command]
fn camera_raw_filter(
    state: State<'_, AppState>,
    id: LayerId,
    settings: document::CameraRawSettings,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.camera_raw_filter(id, settings))
}

/// Edit > Transform > Rotate layer `id` by `degrees` (positive clockwise).
#[tauri::command]
fn rotate(state: State<'_, AppState>, id: LayerId, degrees: f32) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.rotate(id, degrees))
}

/// Edit > Transform > Scale layer `id` to `width_percent` x `height_percent`.
#[tauri::command]
fn scale(
    state: State<'_, AppState>,
    id: LayerId,
    width_percent: f32,
    height_percent: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.scale(id, width_percent, height_percent)
    })
}

/// Edit > Transform > Skew layer `id` by horizontal then vertical angles.
#[tauri::command]
fn skew(
    state: State<'_, AppState>,
    id: LayerId,
    horizontal_degrees: f32,
    vertical_degrees: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.skew(id, horizontal_degrees, vertical_degrees)
    })
}

/// Edit > Free Transform on layer `id`: scale, rotate, skew, and move as
/// one undo step.
#[tauri::command]
fn free_transform(
    state: State<'_, AppState>,
    id: LayerId,
    transform: document::FreeTransform,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.free_transform(id, transform))
}

/// Edit > Transform > Again: repeat the last transform on layer `id`.
#[tauri::command]
fn transform_again(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.transform_again(id))
}

/// Edit > Transform > Distort layer `id`: its four corners (TL, TR, BR,
/// BL) land on `corners`.
#[tauri::command]
fn distort(
    state: State<'_, AppState>,
    id: LayerId,
    corners: [[f32; 2]; 4],
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.distort(id, corners))
}

/// Edit > Transform > Perspective on layer `id`: mirrored corner insets.
#[tauri::command]
fn perspective(
    state: State<'_, AppState>,
    id: LayerId,
    horizontal: f32,
    vertical: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.perspective(id, horizontal, vertical)
    })
}

/// Edit > Define Pattern: capture layer `id`'s pixels inside the selection
/// (or the whole layer) as the document's pattern.
#[tauri::command]
fn define_pattern(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| document.define_pattern(id).map(|_| None))
}

/// Layer > New Fill Layer > Pattern: add a new top layer tiled with the
/// document's defined pattern. Always named "Pattern Fill 1".
#[tauri::command]
fn add_pattern_layer(state: State<'_, AppState>) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.add_pattern_layer("Pattern Fill 1").map(|_| None)
    })
}

/// Flatten the open document and write it to `path` as a new PNG file. The
/// open document itself is untouched — this reads it, it does not mutate it —
/// so unlike every other command here there is no [`Snapshot`] to return.
#[tauri::command]
fn export_png(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    export(document, Path::new(&path))
}

/// Write the open document to `path` as a project file — the full editable
/// layer stack (order, visibility, opacity, blend mode, and each layer's own
/// pixels), unlike [`export_png`], which only ever writes the flattened
/// composite. Like `export_png`, this reads the open document without
/// mutating it, so there is no [`Snapshot`] to return.
#[tauri::command]
fn save_project(state: State<'_, AppState>, path: String) -> Result<(), String> {
    let guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_ref().ok_or_else(|| NO_DOCUMENT.to_string())?;
    project::save(document, Path::new(&path))
}

/// Open `path` as a project file, replacing whatever document was open — the
/// counterpart to [`open_document`], but for a project file's full layer
/// stack instead of a single flattened image.
#[tauri::command]
fn open_project(state: State<'_, AppState>, path: String) -> Result<Snapshot, String> {
    let document = project::load(Path::new(&path))?;
    replace_open_document(&state, document)
}

/// Snapshot the open document onto the undo stack, for the frontend to call
/// once at the start of a multi-step gesture (a stroke, an opacity drag) —
/// see [`edit`] vs [`edit_checkpointed`]. A no-op, not an error, when no
/// document is open.
#[tauri::command]
fn checkpoint(state: State<'_, AppState>) -> Result<HistoryState, String> {
    push_checkpoint(&state)?;
    history_state(&state)
}

/// Undo the most recent checkpoint, moving the current document onto the
/// redo stack. An error, not a silent no-op, when there is nothing to undo —
/// same as every other command here reporting what it could not do.
#[tauri::command]
fn undo(state: State<'_, AppState>) -> Result<Snapshot, String> {
    perform_undo(&state)
}

/// Redo the most recently undone checkpoint. See [`undo`].
#[tauri::command]
fn redo(state: State<'_, AppState>) -> Result<Snapshot, String> {
    perform_redo(&state)
}

/// The blend modes the compositor supports, in display order.
#[tauri::command]
fn blend_modes() -> Vec<BlendModeInfo> {
    BlendMode::ALL
        .into_iter()
        .map(|mode| BlendModeInfo {
            mode,
            label: mode.label(),
        })
        .collect()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(AppState::default())
        .plugin(tauri_plugin_dialog::init())
        // Serves the cached composite to `<img src="composite://composite.png?g=…">`
        // in the frontend, so a re-render ships raw PNG bytes over a normal
        // resource fetch instead of a base64 string through IPC/JSON.
        .register_uri_scheme_protocol("composite", |ctx, _request| {
            serve_composite(&ctx.app_handle().state::<AppState>().composite)
        })
        .invoke_handler(tauri::generate_handler![
            open_document,
            new_document,
            add_layer,
            add_solid_color_layer,
            add_gradient_layer,
            set_layer_visible,
            set_layer_locked,
            rasterize_layer,
            flip_layer_horizontal,
            flip_layer_vertical,
            rotate_layer_180,
            rotate_document_90,
            constrain_crop,
            copy,
            copy_merged,
            apply_image,
            cut,
            paste,
            paste_into,
            paste_outside,
            camera_raw_geometry,
            new_layer_via_copy,
            new_layer_via_cut,
            delete_selection,
            fill_selection,
            box_blur,
            shape_blur,
            unsharp_mask,
            smart_sharpen,
            reduce_noise,
            motion_blur,
            sprayed_strokes,
            blur,
            blur_more,
            sharpen,
            sharpen_more,
            sharpen_edges,
            median,
            despeckle,
            dust_and_scratches,
            add_noise,
            equalize,
            auto_tone,
            auto_contrast,
            auto_color,
            match_color,
            maximum,
            minimum,
            high_pass,
            offset,
            custom,
            find_edges,
            solarize,
            emboss,
            trace_contour,
            gaussian_blur,
            diffuse,
            surface_blur,
            glowing_edges,
            mosaic,
            fragment,
            ripple,
            radial_blur,
            twirl,
            pinch,
            spherize,
            zig_zag,
            polar_coordinates,
            wave,
            shear,
            displace,
            color_halftone,
            mezzotint,
            extrude,
            lighting_effects,
            colored_pencil,
            cutout,
            dry_brush,
            film_grain,
            neon_glow,
            poster_edges,
            sponge,
            watercolor,
            dark_strokes,
            ink_outlines,
            spatter,
            crosshatch,
            accented_edges,
            angled_strokes,
            crystallize,
            facet,
            pointillize,
            clouds,
            difference_clouds,
            fibers,
            lens_flare,
            set_layer_opacity,
            set_layer_blend_mode,
            remove_layer,
            duplicate_layer,
            move_layer,
            merge_visible,
            flatten_image,
            merge_down,
            sample_color,
            paint_stroke,
            erase_stroke,
            dodge_stroke,
            burn_stroke,
            sponge_stroke,
            blur_stroke,
            sharpen_stroke,
            smudge_stroke,
            color_replace_stroke,
            background_erase_stroke,
            heal_stroke,
            spot_heal_stroke,
            remove_stroke,
            draw_rectangle,
            draw_ellipse,
            draw_line,
            draw_polygon,
            draw_star,
            draw_triangle,
            clone_stroke,
            set_history_source,
            history_stroke,
            flood_fill,
            gradient_fill,
            invert_colors,
            threshold,
            posterize,
            brightness_contrast,
            sumi_e,
            smudge_stick,
            paint_daubs,
            palette_knife,
            plastic_wrap,
            fresco,
            rough_pastels,
            underpainting,
            stamp,
            photocopy,
            reticulation,
            note_paper,
            graphic_pen,
            chalk_and_charcoal,
            plaster,
            water_paper,
            torn_edges,
            bas_relief,
            halftone_pattern,
            chrome,
            diffuse_glow,
            glass,
            ocean_ripple,
            wind,
            grain,
            tiles,
            mosaic_tiles,
            patchwork,
            stained_glass,
            craquelure,
            selective_color,
            stroke_outline,
            color_overlay,
            gradient_overlay,
            outer_glow,
            inner_glow,
            drop_shadow,
            inner_shadow,
            pattern_overlay,
            bevel_emboss,
            contour,
            texture,
            texturizer,
            hue_saturation,
            replace_color,
            black_and_white,
            vibrance,
            photo_filter,
            temperature_tint,
            exposure,
            gradient_map,
            channel_mixer,
            levels,
            curves,
            curves_points,
            curves_lookup,
            curves_channels,
            levels_black_point,
            levels_white_point,
            levels_gray_point,
            color_balance,
            highlights_shadows,
            clarity,
            defringe,
            tilt_shift,
            iris_blur,
            field_blur,
            spin_blur,
            lens_blur,
            camera_raw_saturation,
            histogram,
            rgb_levels,
            shadow_clipping,
            camera_raw_point_curve,
            color_grading,
            color_mixer,
            point_color,
            parametric_curve,
            camera_raw_filter,
            rotate,
            scale,
            skew,
            free_transform,
            transform_again,
            distort,
            perspective,
            define_pattern,
            add_pattern_layer,
            pattern_stamp_stroke,
            select_rectangle,
            select_ellipse,
            select_polygon,
            select_lasso,
            select_magic_wand,
            select_color_range,
            grow_selection,
            select_similar,
            magic_erase,
            red_eye,
            ruler_measure,
            sample_points,
            select_all,
            invert_selection,
            expand_selection,
            move_selection,
            move_pixels,
            patch,
            content_aware_move,
            content_aware_fill,
            transform_selection,
            save_selection,
            load_selection,
            add_count_mark,
            clear_count_marks,
            add_note,
            set_note_text,
            remove_note,
            clear_notes,
            save_layer_comp,
            apply_layer_comp,
            delete_layer_comp,
            contract_selection,
            smooth_selection,
            border_selection,
            reselect,
            deselect,
            export_png,
            save_project,
            open_project,
            checkpoint,
            undo,
            redo,
            blend_modes,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_blend_mode_is_offered_to_the_ui() {
        let offered = blend_modes();
        assert_eq!(offered.len(), BlendMode::ALL.len());
        for (info, mode) in offered.iter().zip(BlendMode::ALL) {
            assert_eq!(info.mode, mode);
            assert!(!info.label.is_empty());
        }
    }

    #[test]
    fn layer_names_come_from_the_file_name() {
        assert_eq!(layer_name(Path::new("/tmp/photo.png")), "photo.png");
    }

    #[test]
    fn serve_composite_is_not_found_before_anything_is_cached() {
        let cache = CompositeCache::default();
        let response = serve_composite(&cache);
        assert_eq!(response.status(), tauri::http::StatusCode::NOT_FOUND);
        assert!(response.body().is_empty());
    }

    #[test]
    fn serve_composite_returns_the_cached_bytes_as_a_png_response() {
        let cache = CompositeCache::default();
        *cache.bytes.lock().unwrap() = Some(vec![1, 2, 3]);

        let response = serve_composite(&cache);

        assert_eq!(response.status(), tauri::http::StatusCode::OK);
        assert_eq!(response.body(), &vec![1u8, 2, 3]);
        assert_eq!(
            response
                .headers()
                .get(tauri::http::header::CONTENT_TYPE)
                .unwrap(),
            "image/png"
        );
    }

    #[test]
    fn sampling_before_anything_is_composited_is_an_error() {
        let cache = CompositeCache::default();
        assert!(sample_pixel_color(&cache, 0, 0).is_err());
    }

    #[test]
    fn sampling_outside_the_canvas_is_an_error() {
        let state = AppState::default();
        let mut document = Document::new(2, 2).unwrap();
        document
            .add_layer("solid", &[9, 8, 7, 255].repeat(4), 2, 2)
            .unwrap();
        snapshot(&state, &document, None).unwrap();

        assert!(sample_pixel_color(&state.composite, 2, 0).is_err());
        assert!(sample_pixel_color(&state.composite, 0, 2).is_err());
    }

    #[test]
    fn sampling_reads_the_composited_colour_at_that_pixel() {
        let state = AppState::default();
        let mut document = Document::new(2, 1).unwrap();
        let mut pixels = vec![255, 0, 0, 255]; // left pixel: red
        pixels.extend([0, 0, 255, 255]); // right pixel: blue
        document.add_layer("two-tone", &pixels, 2, 1).unwrap();
        snapshot(&state, &document, None).unwrap();

        assert_eq!(
            sample_pixel_color(&state.composite, 0, 0).unwrap(),
            [255, 0, 0, 255]
        );
        assert_eq!(
            sample_pixel_color(&state.composite, 1, 0).unwrap(),
            [0, 0, 255, 255]
        );
    }

    #[test]
    fn snapshot_bumps_the_generation_and_caches_the_encoded_composite() {
        let state = AppState::default();
        let document = Document::new(1, 1).unwrap();

        let first = snapshot(&state, &document, None).unwrap();
        let second = snapshot(&state, &document, None).unwrap();

        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert!(state.composite.bytes.lock().unwrap().is_some());
    }

    #[test]
    fn a_region_snapshot_matches_a_full_flatten_of_the_same_document() {
        let state = AppState::default();
        let mut document = Document::new(4, 4).unwrap();
        document
            .add_layer("l", &[10u8, 20, 30, 255].repeat(16), 4, 4)
            .unwrap();

        // Seed the cache with a full flatten first, the same as any real
        // command sequence would (a stroke is never the very first edit on
        // a freshly opened document).
        snapshot(&state, &document, None).unwrap();

        let rect = Rect {
            x0: 1,
            y0: 1,
            x1: 3,
            y1: 3,
        };
        snapshot(&state, &document, Some(rect)).unwrap();

        let cached = state.composite.pixels.lock().unwrap().clone().unwrap();
        assert_eq!(cached.pixels, composite::flatten(&document).pixels);
    }

    #[test]
    fn a_region_snapshot_falls_back_to_a_full_flatten_when_nothing_is_cached_yet() {
        let state = AppState::default();
        let document = Document::new(2, 2).unwrap();

        // No prior full snapshot — the (unrealistic, defensive-only) case of
        // a rect passed in before there is anything to patch.
        let result = snapshot(
            &state,
            &document,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            }),
        )
        .unwrap();
        assert_eq!(result.generation, 1);
        assert!(state.composite.pixels.lock().unwrap().is_some());
    }

    #[test]
    fn a_region_snapshot_falls_back_to_a_full_flatten_on_a_dimension_mismatch() {
        let state = AppState::default();
        let small = Document::new(2, 2).unwrap();
        snapshot(&state, &small, None).unwrap();

        let mut big = Document::new(4, 4).unwrap();
        big.add_layer("l", &[1u8, 2, 3, 255].repeat(16), 4, 4)
            .unwrap();
        // A rect that would be valid for `small` but not `big` - the cached
        // buffer is still 2x2, so this must fully re-flatten rather than
        // writing a 4x4 pixel's worth of data into a 2x2 buffer.
        let result = snapshot(
            &state,
            &big,
            Some(Rect {
                x0: 0,
                y0: 0,
                x1: 1,
                y1: 1,
            }),
        )
        .unwrap();

        let cached = state.composite.pixels.lock().unwrap().clone().unwrap();
        assert_eq!((cached.width, cached.height), (4, 4));
        assert_eq!(cached.pixels, composite::flatten(&big).pixels);
        assert_eq!(result.generation, 2);
    }

    #[test]
    fn export_writes_the_flattened_composite_as_a_png_that_decodes_back() {
        let mut document = Document::new(2, 1).unwrap();
        document
            .add_layer("l", &[255, 0, 0, 255, 0, 0, 255, 255], 2, 1)
            .unwrap();

        let path = std::env::temp_dir().join("lib_rs_export_ok.png");
        export(&document, &path).unwrap();

        let decoded = png::read(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.pixels, composite::flatten(&document).pixels);
    }

    #[test]
    fn export_reports_a_directory_that_does_not_exist() {
        let document = Document::new(1, 1).unwrap();
        let path = std::env::temp_dir()
            .join("lib_rs_export_missing_dir_that_does_not_exist")
            .join("out.png");
        let err = export(&document, &path).unwrap_err();
        assert!(err.contains("Could not write"), "{err}");
    }

    #[test]
    fn new_document_creates_one_blank_paintable_layer() {
        let state = AppState::default();
        let result = create_new_document(&state, 4, 3).unwrap();
        assert_eq!((result.document.width, result.document.height), (4, 3));
        assert_eq!(result.document.layers.len(), 1);
        assert_eq!(result.document.layers[0].name, "Layer 1");

        let doc_guard = state.document.lock().unwrap();
        let document = doc_guard.as_ref().unwrap();
        assert_eq!(
            document.layers()[0].pixels,
            vec![0u8; 4 * 3 * document::CHANNELS]
        );
    }

    #[test]
    fn new_document_rejects_zero_dimensions() {
        let state = AppState::default();
        assert!(create_new_document(&state, 0, 5).is_err());
        assert!(create_new_document(&state, 5, 0).is_err());
    }

    #[test]
    fn new_document_rejects_a_canvas_over_the_memory_limit() {
        let state = AppState::default();
        // Bytes needed = width * height * 4; pick dimensions comfortably
        // over MAX_NEW_DOCUMENT_BYTES (64 MB) without actually allocating it.
        let err = create_new_document(&state, 1 << 16, 1 << 16).unwrap_err();
        assert!(err.contains("over the"), "{err}");
    }

    #[test]
    fn new_document_replaces_whatever_was_open_and_resets_history() {
        let state = AppState::default();
        *state.document.lock().unwrap() = Some(Document::new(1, 1).unwrap());
        push_checkpoint(&state).unwrap();
        assert!(history_state(&state).unwrap().can_undo);

        let result = create_new_document(&state, 2, 2).unwrap();
        assert!(!result.can_undo);
        assert!(!result.can_redo);
        assert_eq!(state.document.lock().unwrap().as_ref().unwrap().width(), 2);
    }

    #[test]
    fn snapshot_reports_whether_there_is_anything_to_undo_or_redo() {
        let state = AppState::default();
        let document = Document::new(1, 1).unwrap();

        let fresh = snapshot(&state, &document, None).unwrap();
        assert!(!fresh.can_undo);
        assert!(!fresh.can_redo);

        state
            .history
            .lock()
            .unwrap()
            .undo
            .push_back(document.clone());
        let with_undo = snapshot(&state, &document, None).unwrap();
        assert!(with_undo.can_undo);
        assert!(!with_undo.can_redo);

        state
            .history
            .lock()
            .unwrap()
            .redo
            .push_back(document.clone());
        let with_both = snapshot(&state, &document, None).unwrap();
        assert!(with_both.can_undo);
        assert!(with_both.can_redo);
    }

    #[test]
    fn checkpoint_with_no_document_open_is_a_no_op() {
        let state = AppState::default();
        push_checkpoint(&state).unwrap();
        let history = history_state(&state).unwrap();
        assert!(!history.can_undo);
        assert!(!history.can_redo);
    }

    #[test]
    fn undo_restores_the_document_from_before_the_checkpoint() {
        let state = AppState::default();
        let mut document = Document::new(1, 1).unwrap();
        document.add_layer("a", &[255, 0, 0, 255], 1, 1).unwrap();
        *state.document.lock().unwrap() = Some(document);

        push_checkpoint(&state).unwrap();
        // Simulate an edit that happened after the checkpoint.
        state
            .document
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .add_layer("b", &[0, 255, 0, 255], 1, 1)
            .unwrap();
        assert_eq!(
            state
                .document
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .layers()
                .len(),
            2
        );

        let after_undo = perform_undo(&state).unwrap();
        assert_eq!(after_undo.document.layers.len(), 1);
        assert!(!after_undo.can_undo);
        assert!(after_undo.can_redo);
    }

    #[test]
    fn redo_reapplies_what_undo_undid() {
        let state = AppState::default();
        *state.document.lock().unwrap() = Some(Document::new(1, 1).unwrap());

        push_checkpoint(&state).unwrap();
        state
            .document
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .add_layer("l", &[1, 2, 3, 255], 1, 1)
            .unwrap();

        perform_undo(&state).unwrap();
        assert_eq!(
            state
                .document
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .layers()
                .len(),
            0
        );

        let after_redo = perform_redo(&state).unwrap();
        assert_eq!(after_redo.document.layers.len(), 1);
        assert!(after_redo.can_undo);
        assert!(!after_redo.can_redo);
    }

    #[test]
    fn undo_with_nothing_to_undo_is_an_error() {
        let state = AppState::default();
        assert_eq!(perform_undo(&state).unwrap_err(), NOTHING_TO_UNDO);
    }

    #[test]
    fn redo_with_nothing_to_redo_is_an_error() {
        let state = AppState::default();
        assert_eq!(perform_redo(&state).unwrap_err(), NOTHING_TO_REDO);
    }

    #[test]
    fn a_new_checkpoint_clears_the_redo_stack() {
        let state = AppState::default();
        *state.document.lock().unwrap() = Some(Document::new(1, 1).unwrap());

        push_checkpoint(&state).unwrap();
        perform_undo(&state).unwrap();
        assert!(history_state(&state).unwrap().can_redo);

        push_checkpoint(&state).unwrap();
        assert!(!history_state(&state).unwrap().can_redo);
    }

    #[test]
    fn history_is_capped_so_it_cannot_grow_without_bound() {
        let state = AppState::default();
        *state.document.lock().unwrap() = Some(Document::new(1, 1).unwrap());

        for _ in 0..MAX_HISTORY + 5 {
            push_checkpoint(&state).unwrap();
        }

        assert_eq!(state.history.lock().unwrap().undo.len(), MAX_HISTORY);
    }
}
