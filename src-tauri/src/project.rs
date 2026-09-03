//! A layered project file format that round-trips the full layer stack —
//! order, name, visibility, opacity, and blend mode, plus each layer's own
//! pixels — across a save and reopen. **Export PNG…** only ever wrote the
//! flattened composite; nothing until now could save (and get back) the
//! *editable* document.
//!
//! Layout, chosen to reuse the PNG codec already in `png.rs` rather than
//! inventing a second pixel format or pulling in an archive library:
//!
//! ```text
//! b"IEDP1"              5-byte magic + format version
//! u32 LE                manifest length, in bytes
//! <manifest JSON>        width, height, and each layer's name/visible/
//!                        opacity/blend_mode/png_len, in stack order
//! <layer 0 PNG bytes><layer 1 PNG bytes>...
//! ```
//!
//! Each layer's own pixels are PNG-encoded independently and concatenated
//! after the manifest, in the same order the manifest lists them — the
//! manifest's `png_len` for each is what lets a reader find where one layer's
//! bytes end and the next begins without a fragile scan.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::document::Document;
use crate::png;

const MAGIC: &[u8; 5] = b"IEDP1";

#[derive(Serialize, Deserialize)]
struct Manifest {
    width: u32,
    height: u32,
    layers: Vec<LayerManifest>,
}

#[derive(Serialize, Deserialize)]
struct LayerManifest {
    name: String,
    visible: bool,
    opacity: f32,
    blend_mode: BlendMode,
    png_len: u32,
}

/// Write `document` to `path` as a project file.
pub fn save(document: &Document, path: &Path) -> Result<(), String> {
    let mut layers = Vec::with_capacity(document.layers().len());
    let mut layer_bytes = Vec::with_capacity(document.layers().len());
    for layer in document.layers() {
        let bytes = png::encode_pixels(document.width(), document.height(), &layer.pixels)
            .map_err(|err| format!("Could not encode layer '{}': {err}", layer.name))?;
        layers.push(LayerManifest {
            name: layer.name.clone(),
            visible: layer.visible,
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            png_len: bytes.len() as u32,
        });
        layer_bytes.push(bytes);
    }

    let manifest = Manifest {
        width: document.width(),
        height: document.height(),
        layers,
    };
    let manifest_json = serde_json::to_vec(&manifest)
        .map_err(|err| format!("Could not encode the project manifest: {err}"))?;

    let mut out = Vec::with_capacity(
        MAGIC.len() + 4 + manifest_json.len() + layer_bytes.iter().map(Vec::len).sum::<usize>(),
    );
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(manifest_json.len() as u32).to_le_bytes());
    out.extend_from_slice(&manifest_json);
    for bytes in layer_bytes {
        out.extend_from_slice(&bytes);
    }

    std::fs::write(path, out).map_err(|err| format!("Could not write {}: {err}", path.display()))
}

/// Read a project file back into a [`Document`], layer stack and all.
pub fn load(path: &Path) -> Result<Document, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;

    if bytes.len() < MAGIC.len() + 4 || &bytes[..MAGIC.len()] != MAGIC {
        return Err(format!(
            "{} is not an image-editor project file.",
            path.display()
        ));
    }
    let mut offset = MAGIC.len();

    let manifest_len =
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
    offset += 4;
    let manifest_end = offset
        .checked_add(manifest_len)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| format!("{} is truncated (manifest).", path.display()))?;
    let manifest: Manifest = serde_json::from_slice(&bytes[offset..manifest_end])
        .map_err(|err| format!("{} has a corrupt project manifest: {err}", path.display()))?;
    offset = manifest_end;

    let mut document = Document::new(manifest.width, manifest.height)?;
    for layer in &manifest.layers {
        let len = layer.png_len as usize;
        let end = offset
            .checked_add(len)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| format!("{} is truncated (layer '{}').", path.display(), layer.name))?;
        let decoded = png::decode_bytes(&bytes[offset..end]).map_err(|err| {
            format!(
                "{} has a corrupt layer '{}': {err}",
                path.display(),
                layer.name
            )
        })?;
        offset = end;

        if decoded.width != manifest.width || decoded.height != manifest.height {
            return Err(format!(
                "{}: layer '{}' is {}x{}, but the document is {}x{}.",
                path.display(),
                layer.name,
                decoded.width,
                decoded.height,
                manifest.width,
                manifest.height
            ));
        }

        let id = document.add_layer(
            layer.name.clone(),
            &decoded.pixels,
            decoded.width,
            decoded.height,
        )?;
        document.set_visible(id, layer.visible)?;
        document.set_opacity(id, layer.opacity)?;
        document.set_blend_mode(id, layer.blend_mode)?;
    }

    Ok(document)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::MoveDirection;

    fn temp_path(name: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(name)
    }

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * 4)
            .collect()
    }

    #[test]
    fn a_round_trip_preserves_layer_stack_and_pixels() {
        let mut document = Document::new(2, 2).unwrap();
        let bottom = document
            .add_layer("bottom", &solid(2, 2, [255, 0, 0, 255]), 2, 2)
            .unwrap();
        let top = document
            .add_layer("top", &solid(2, 2, [0, 255, 0, 200]), 2, 2)
            .unwrap();
        document.set_opacity(top, 0.5).unwrap();
        document.set_blend_mode(top, BlendMode::Multiply).unwrap();
        document.set_visible(bottom, false).unwrap();

        let path = temp_path("project_rs_round_trip.iep");
        save(&document, &path).unwrap();
        let reloaded = load(&path).unwrap();

        assert_eq!((reloaded.width(), reloaded.height()), (2, 2));
        assert_eq!(reloaded.layers().len(), 2);

        let reloaded_bottom = &reloaded.layers()[0];
        assert_eq!(reloaded_bottom.name, "bottom");
        assert!(!reloaded_bottom.visible);
        assert_eq!(reloaded_bottom.opacity, 1.0);
        assert_eq!(reloaded_bottom.blend_mode, BlendMode::Normal);
        assert_eq!(reloaded_bottom.pixels, solid(2, 2, [255, 0, 0, 255]));

        let reloaded_top = &reloaded.layers()[1];
        assert_eq!(reloaded_top.name, "top");
        assert!(reloaded_top.visible);
        assert_eq!(reloaded_top.opacity, 0.5);
        assert_eq!(reloaded_top.blend_mode, BlendMode::Multiply);
        assert_eq!(reloaded_top.pixels, solid(2, 2, [0, 255, 0, 200]));
    }

    #[test]
    fn a_round_trip_preserves_stack_order_after_reordering() {
        let mut document = Document::new(1, 1).unwrap();
        let a = document.add_layer("a", &solid(1, 1, [1; 4]), 1, 1).unwrap();
        document.add_layer("b", &solid(1, 1, [2; 4]), 1, 1).unwrap();
        document.move_layer(a, MoveDirection::Up).unwrap();

        let path = temp_path("project_rs_order.iep");
        save(&document, &path).unwrap();
        let reloaded = load(&path).unwrap();

        let names: Vec<_> = reloaded.layers().iter().map(|l| l.name.clone()).collect();
        assert_eq!(names, vec!["b", "a"]);
    }

    #[test]
    fn an_empty_document_round_trips() {
        let document = Document::new(3, 4).unwrap();
        let path = temp_path("project_rs_empty.iep");
        save(&document, &path).unwrap();
        let reloaded = load(&path).unwrap();
        assert_eq!((reloaded.width(), reloaded.height()), (3, 4));
        assert!(reloaded.layers().is_empty());
    }

    #[test]
    fn loading_a_missing_file_is_an_error() {
        let path = temp_path("project_rs_missing_definitely.iep");
        let _ = std::fs::remove_file(&path);
        assert!(load(&path).unwrap_err().contains("Could not read"));
    }

    #[test]
    fn loading_a_file_with_the_wrong_magic_is_an_error() {
        let path = temp_path("project_rs_wrong_magic.iep");
        std::fs::write(&path, b"NOTAPROJECTFILE").unwrap();
        assert!(load(&path)
            .unwrap_err()
            .contains("not an image-editor project file"));
    }

    #[test]
    fn loading_a_truncated_manifest_is_an_error() {
        let path = temp_path("project_rs_truncated_manifest.iep");
        let mut bytes = MAGIC.to_vec();
        // Claims a 1000-byte manifest but the file has none.
        bytes.extend_from_slice(&1000u32.to_le_bytes());
        std::fs::write(&path, bytes).unwrap();
        assert!(load(&path).unwrap_err().contains("truncated"));
    }

    #[test]
    fn loading_a_truncated_layer_is_an_error() {
        let document = {
            let mut d = Document::new(1, 1).unwrap();
            d.add_layer("l", &solid(1, 1, [9; 4]), 1, 1).unwrap();
            d
        };
        let path = temp_path("project_rs_truncated_layer.iep");
        save(&document, &path).unwrap();

        // Chop off the last 10 bytes, into the middle of the one layer's PNG.
        let mut bytes = std::fs::read(&path).unwrap();
        bytes.truncate(bytes.len() - 10);
        std::fs::write(&path, bytes).unwrap();

        assert!(load(&path).unwrap_err().contains("truncated"));
    }
}
