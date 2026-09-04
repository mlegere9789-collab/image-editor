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
use document::{Document, DocumentView, LayerId, MoveDirection, Stroke};

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
            set_layer_visible,
            set_layer_opacity,
            set_layer_blend_mode,
            remove_layer,
            move_layer,
            paint_stroke,
            erase_stroke,
            select_rectangle,
            select_ellipse,
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
