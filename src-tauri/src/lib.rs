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
use document::{Clipboard, Document, DocumentView, LayerId, MoveDirection, Stroke, CHANNELS};

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
}

/// What [`checkpoint`] hands back: just the two flags of [`Snapshot`] that
/// change, since a checkpoint does not touch the document or the composite.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct HistoryState {
    can_undo: bool,
    can_redo: bool,
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

fn history_state(state: &AppState) -> Result<HistoryState, String> {
    let history = state.history.lock().map_err(|_| POISONED.to_string())?;
    Ok(HistoryState {
        can_undo: !history.undo.is_empty(),
        can_redo: !history.redo.is_empty(),
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
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_rectangle(x0, y0, x1, y1)?;
        Ok(None)
    })
}

/// Replace the selection with an ellipse inscribed in the given bounding box.
#[tauri::command]
fn select_ellipse(
    state: State<'_, AppState>,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.select_ellipse(x0, y0, x1, y1)?;
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
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Brush { color })
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
) -> Result<Snapshot, String> {
    edit(&state, |document| {
        document.stroke(id, &points, radius, Stroke::Eraser)
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
) -> Result<Snapshot, String> {
    edit_checkpointed(&state, |document| {
        document.levels(
            id,
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
            copy,
            cut,
            paste,
            new_layer_via_copy,
            new_layer_via_cut,
            delete_selection,
            fill_selection,
            box_blur,
            unsharp_mask,
            motion_blur,
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
            maximum,
            minimum,
            high_pass,
            offset,
            custom,
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
            flood_fill,
            gradient_fill,
            invert_colors,
            threshold,
            posterize,
            brightness_contrast,
            hue_saturation,
            black_and_white,
            vibrance,
            photo_filter,
            exposure,
            gradient_map,
            channel_mixer,
            levels,
            curves,
            color_balance,
            select_rectangle,
            select_ellipse,
            select_all,
            invert_selection,
            expand_selection,
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
