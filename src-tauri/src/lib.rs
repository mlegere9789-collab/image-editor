use std::path::{Path, PathBuf};

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use serde::Serialize;

/// Refuse anything large enough that base64-ing it into the webview would be a
/// memory problem. Phase 0 ships the pixels as a data URL; a streaming/asset
/// protocol path is what lifts this ceiling later.
const MAX_IMAGE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoadedImage {
    path: String,
    file_name: String,
    width: u32,
    height: u32,
    byte_length: u64,
    /// `data:image/png;base64,…` holding the file's original, unmodified bytes.
    data_url: String,
}

fn load(path: &Path) -> Result<LoadedImage, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("Could not read {}: {err}", path.display()))?;

    if !metadata.is_file() {
        return Err(format!("{} is not a file.", path.display()));
    }

    let byte_length = metadata.len();
    if byte_length > MAX_IMAGE_BYTES {
        return Err(format!(
            "{} is {:.1} MB, which is over the {} MB limit.",
            path.display(),
            byte_length as f64 / (1024.0 * 1024.0),
            MAX_IMAGE_BYTES / (1024 * 1024)
        ));
    }

    let bytes = std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;

    // Decoding the header both validates that this really is a PNG and gives us
    // the dimensions to show in the status bar.
    let reader = image::ImageReader::with_format(
        std::io::Cursor::new(&bytes),
        image::ImageFormat::Png,
    );
    let (width, height) = reader
        .into_dimensions()
        .map_err(|err| format!("{} is not a readable PNG: {err}", path.display()))?;

    Ok(LoadedImage {
        path: path.display().to_string(),
        file_name: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        width,
        height,
        byte_length,
        data_url: format!("data:image/png;base64,{}", STANDARD.encode(&bytes)),
    })
}

#[tauri::command]
fn load_image(path: String) -> Result<LoadedImage, String> {
    load(&PathBuf::from(path))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![load_image])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Smallest valid PNG: a 1x1 opaque red pixel.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn reads_dimensions_and_embeds_original_bytes() {
        let path = write_temp("image_editor_ok.png", ONE_PIXEL_PNG);
        let loaded = load(&path).unwrap();

        assert_eq!((loaded.width, loaded.height), (1, 1));
        assert_eq!(loaded.file_name, "image_editor_ok.png");
        assert_eq!(loaded.byte_length, ONE_PIXEL_PNG.len() as u64);
        assert_eq!(
            loaded.data_url,
            format!("data:image/png;base64,{}", STANDARD.encode(ONE_PIXEL_PNG))
        );
    }

    #[test]
    fn rejects_a_file_that_is_not_a_png() {
        let path = write_temp("image_editor_bad.png", b"definitely not a png");
        assert!(load(&path).unwrap_err().contains("not a readable PNG"));
    }

    #[test]
    fn rejects_a_missing_file() {
        let path = std::env::temp_dir().join("image_editor_absent.png");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).unwrap_err().contains("Could not read"));
    }
}
