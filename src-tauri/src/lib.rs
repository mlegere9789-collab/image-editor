// Suppress the extra console window on Windows in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

pub mod blend;
pub mod composite;
pub mod document;
pub mod png;

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::Serialize;
use tauri::State;

use blend::BlendMode;
use document::{Document, DocumentView, LayerId, MoveDirection};

/// The open document. `None` until the first image is opened.
#[derive(Default)]
struct AppState {
    document: Mutex<Option<Document>>,
}

/// What every mutating command hands back: the new layer state plus the
/// re-flattened image to show. Keeping them together means one round trip per
/// edit instead of two.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    document: DocumentView,
    /// `data:image/png;base64,…` of the flattened composite.
    composite: String,
}

/// One entry in the blend-mode picker.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BlendModeInfo {
    mode: BlendMode,
    label: &'static str,
}

fn snapshot(document: &Document) -> Result<Snapshot, String> {
    Ok(Snapshot {
        document: document.view(),
        composite: png::to_data_url(&composite::flatten(document))?,
    })
}

/// Run `edit` against the open document, then re-flatten.
fn edit<F>(state: &State<'_, AppState>, edit: F) -> Result<Snapshot, String>
where
    F: FnOnce(&mut Document) -> Result<(), String>,
{
    let mut guard = state.document.lock().map_err(|_| POISONED.to_string())?;
    let document = guard.as_mut().ok_or_else(|| NO_DOCUMENT.to_string())?;
    edit(document)?;
    snapshot(document)
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

    let result = snapshot(&document)?;
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
        .invoke_handler(tauri::generate_handler![
            open_document,
            add_layer,
            set_layer_visible,
            set_layer_opacity,
            set_layer_blend_mode,
            remove_layer,
            move_layer,
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
}
