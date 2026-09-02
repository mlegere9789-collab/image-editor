// Suppress the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod blend;
pub mod composite;
pub mod document;
pub mod png;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use serde::Serialize;
use tauri::{Manager, State};

use blend::BlendMode;
use document::{Document, DocumentView, LayerId, MoveDirection, Stroke};

/// The latest flattened composite, encoded as PNG bytes and served to the
/// webview by the `composite://` protocol below rather than embedded in every
/// command response. `generation` is bumped each time `bytes` changes, so the
/// frontend can cache-bust its `<img>` src without the bytes themselves
/// crossing the IPC boundary.
#[derive(Default)]
struct CompositeCache {
    bytes: Mutex<Option<Vec<u8>>>,
    generation: AtomicU64,
}

/// The open document. `None` until the first image is opened.
#[derive(Default)]
struct AppState {
    document: Mutex<Option<Document>>,
    composite: CompositeCache,
}

/// What every mutating command hands back: the new layer state plus the
/// generation of the re-flattened composite now cached in `AppState`.
/// Keeping them together means one round trip per edit instead of two.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    document: DocumentView,
    generation: u64,
}

/// One entry in the blend-mode picker.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlendModeInfo {
    mode: BlendMode,
    label: &'static str,
}

/// Re-flatten `document`, cache the encoded result, and hand back the new
/// document view plus the generation the frontend should now request.
fn snapshot(state: &AppState, document: &Document) -> Result<Snapshot, String> {
    let bytes = png::encode(&composite::flatten(document))?;
    *state
        .composite
        .bytes
        .lock()
        .map_err(|_| POISONED.to_string())? = Some(bytes);
    let generation = state.composite.generation.fetch_add(1, Ordering::SeqCst) + 1;
    Ok(Snapshot {
        document: document.view(),
        generation,
    })
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

/// Run `edit` against the open document, then re-flatten.
fn edit<F>(state: &State<'_, AppState>, edit: F) -> Result<Snapshot, String>
where
    F: FnOnce(&mut Document) -> Result<(), String>,
{
    let mut guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_mut().ok_or_else(|| NO_DOCUMENT.to_string())?;
    edit(document)?;
    snapshot(state, document)
}

const POISONED: &str = "The document is in an inconsistent state; please reopen the image.";
const NO_DOCUMENT: &str = "No document is open.";

fn layer_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
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

    let result = snapshot(&state, &document)?;
    *state.document.lock().map_err(|_| POISONED.to_string())? = Some(document);
    Ok(result)
}

/// Add `path` as a new top layer of the open document. The document keeps its
/// original size: a smaller image is pasted at the origin, a larger one clipped.
#[tauri::command]
fn add_layer(state: State<'_, AppState>, path: String) -> Result<Snapshot, String> {
    let path = PathBuf::from(path);
    let decoded = png::read(&path)?;
    edit(&state, |document| {
        document
            .add_layer(
                layer_name(&path),
                &decoded.pixels,
                decoded.width,
                decoded.height,
            )
            .map(|_| ())
    })
}

#[tauri::command]
fn set_layer_visible(
    state: State<'_, AppState>,
    id: LayerId,
    visible: bool,
) -> Result<Snapshot, String> {
    edit(&state, |document| document.set_visible(id, visible))
}

#[tauri::command]
fn set_layer_opacity(
    state: State<'_, AppState>,
    id: LayerId,
    opacity: f32,
) -> Result<Snapshot, String> {
    edit(&state, |document| document.set_opacity(id, opacity))
}

#[tauri::command]
fn set_layer_blend_mode(
    state: State<'_, AppState>,
    id: LayerId,
    blend_mode: BlendMode,
) -> Result<Snapshot, String> {
    edit(&state, |document| document.set_blend_mode(id, blend_mode))
}

#[tauri::command]
fn remove_layer(state: State<'_, AppState>, id: LayerId) -> Result<Snapshot, String> {
    edit(&state, |document| document.remove_layer(id))
}

#[tauri::command]
fn move_layer(
    state: State<'_, AppState>,
    id: LayerId,
    direction: MoveDirection,
) -> Result<Snapshot, String> {
    edit(&state, |document| document.move_layer(id, direction))
}

/// Paint `color` (RGBA8) along `points` (document pixel coordinates) onto
/// layer `id`, with normal `source-over` blending. `points` is the polyline
/// since the previous pointer event, not the whole stroke — the frontend
/// calls this once per pointer move, so each call's own bounding box stays
/// small regardless of how long the drag has run.
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
            add_layer,
            set_layer_visible,
            set_layer_opacity,
            set_layer_blend_mode,
            remove_layer,
            move_layer,
            paint_stroke,
            erase_stroke,
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

        let first = snapshot(&state, &document).unwrap();
        let second = snapshot(&state, &document).unwrap();

        assert_eq!(first.generation, 1);
        assert_eq!(second.generation, 2);
        assert!(state.composite.bytes.lock().unwrap().is_some());
    }
}
