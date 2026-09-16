//! A layered project file format that round-trips the full layer stack —
//! order, name, visibility, opacity, blend mode, and lock state, plus each
//! layer's own pixels — across a save and reopen. **Export PNG…** only ever
//! wrote the flattened composite; nothing until now could save (and get
//! back) the *editable* document. Version 2 of the manifest (the same
//! magic; every new field has a serde default, so a version-1 file still
//! reads) adds each layer's records — mask, adjustment, fill, text, shape,
//! smart object, link and clip — and the document's own: guides,
//! artboards, notes, count marks, the work path, presets, saved
//! selections and the selection, mode and depth, groups, generated-layer
//! records, and the defined pattern. A mask, a smart object's source, and
//! the pattern travel as further PNG blobs after the layer's own. Version
//! 3 adds the alpha channels, spot channels, the brush tip, and the
//! pattern presets as blobs after the pattern, and Layer Comps, the
//! Indexed Color table, and Duotone's inks to the document records.
//!
//! Layout, chosen to reuse the PNG codec already in `png.rs` rather than
//! inventing a second pixel format or pulling in an archive library:
//!
//! ```text
//! b"IEDP1"              5-byte magic + format version
//! u32 LE                manifest length, in bytes
//! <manifest JSON>        width, height, and each layer's name/visible/
//!                        opacity/blend_mode/locked/png_len, in stack order
//! <layer 0 PNG bytes>[<layer 0 mask PNG>][<layer 0 smart source PNG>]
//! <layer 1 PNG bytes>...[<pattern PNG>]
//! [<channel PNG>...][<spot PNG>...][<brush tip f32s>][<pattern preset PNG>...]
//! ```
//!
//! Each layer's own pixels are PNG-encoded independently and concatenated
//! after the manifest, in the same order the manifest lists them — the
//! manifest's `png_len` for each is what lets a reader find where one layer's
//! bytes end and the next begins without a fragile scan.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::blend::BlendMode;
use crate::document::{
    AlphaChannel, BrushTip, ColorProfile, Document, DocumentRecords, LayerId, LayerRecords,
    Pattern, SpotChannel,
};
use crate::png;

const MAGIC: &[u8; 5] = b"IEDP1";

#[derive(Serialize, Deserialize)]
struct Manifest {
    width: u32,
    height: u32,
    /// Embed Color Profile: Assign Profile's own label (README Phase 292),
    /// carried by the project file itself so a reopened document knows
    /// what working space its numbers are in, exactly as `locked` is.
    /// `Option` rather than a plain `ColorProfile` with `#[serde(default)]`
    /// so `decode` can tell a genuinely missing profile (a file saved
    /// before this existed) apart from one explicitly saved as sRGB — the
    /// distinction Missing Profile Warning (README Phase 305) needs.
    /// `#[serde(default)]` so a project file saved before this existed
    /// still loads instead of failing to parse.
    #[serde(default)]
    profile: Option<ColorProfile>,
    layers: Vec<LayerManifest>,
    /// `1` when absent (a file from before the records were saved).
    #[serde(default = "one")]
    version: u32,
    #[serde(default)]
    records: DocumentRecords,
    /// The defined pattern's PNG, after every layer's blobs; `0` for none.
    #[serde(default)]
    pattern_png_len: u32,
    #[serde(default)]
    pattern_size: (u32, u32),
    /// Version 3: alpha channels, spot channels, the brush tip, and the
    /// pattern presets, whose planes follow the pattern's PNG in this
    /// order. Absent (empty) in older files.
    #[serde(default)]
    channels: Vec<PlaneManifest>,
    #[serde(default)]
    spots: Vec<SpotManifest>,
    #[serde(default)]
    brush_tip: Option<TipManifest>,
    #[serde(default)]
    pattern_presets: Vec<PatternPresetManifest>,
}

/// An alpha channel: its name and the byte length of its grey PNG.
#[derive(Serialize, Deserialize)]
struct PlaneManifest {
    name: String,
    png_len: u32,
}

/// A spot channel: its ink and the byte length of its density PNG.
#[derive(Serialize, Deserialize)]
struct SpotManifest {
    name: String,
    color: [u8; 3],
    solidity: f32,
    png_len: u32,
}

/// The brush tip: its size and the byte length of its coverages, kept
/// exact as little-endian `f32`s rather than quantised to a PNG.
#[derive(Serialize, Deserialize)]
struct TipManifest {
    width: u32,
    height: u32,
    len: u32,
}

/// A pattern preset: its name, size, and the byte length of its PNG.
#[derive(Serialize, Deserialize)]
struct PatternPresetManifest {
    name: String,
    width: u32,
    height: u32,
    png_len: u32,
}

fn one() -> u32 {
    1
}

#[derive(Serialize, Deserialize)]
struct LayerManifest {
    name: String,
    visible: bool,
    opacity: f32,
    blend_mode: BlendMode,
    /// `#[serde(default)]` so a project file saved before Lock existed
    /// still loads — a missing field means "not locked", not an error.
    #[serde(default)]
    locked: bool,
    png_len: u32,
    /// The layer's id at save time, so document records can be remapped.
    #[serde(default)]
    id: LayerId,
    #[serde(default)]
    records: LayerRecords,
    /// The mask's PNG (grey in every channel, opaque), `0` for none.
    #[serde(default)]
    mask_png_len: u32,
    /// The smart object's source PNG, `0` for none.
    #[serde(default)]
    smart_png_len: u32,
}

/// A document-sized byte plane as an opaque grey PNG.
fn encode_plane(width: u32, height: u32, plane: &[u8]) -> Result<Vec<u8>, String> {
    let rgba: Vec<u8> = plane.iter().flat_map(|&v| [v, v, v, 255]).collect();
    png::encode_pixels(width, height, &rgba)
}

/// Encode `document` as project-file bytes, in memory -- the format
/// [`save`] itself writes to disk, and what [`decode`] reads back. Split
/// out so a project can round-trip somewhere other than the filesystem
/// (Cloud Documents' own upload, sending these same bytes to a
/// user-configured endpoint instead of `std::fs::write`).
pub fn encode(document: &Document) -> Result<Vec<u8>, String> {
    let mut layers = Vec::with_capacity(document.layers().len());
    let mut layer_bytes = Vec::with_capacity(document.layers().len());
    let (width, height) = (document.width(), document.height());
    for layer in document.layers() {
        let bytes = png::encode_pixels(width, height, &layer.pixels)
            .map_err(|err| format!("Could not encode layer '{}': {err}", layer.name))?;
        let (records, mask, smart_source) = document.layer_records(layer.id)?;
        let mask_bytes = match mask {
            Some(mask) => encode_plane(width, height, mask)
                .map_err(|err| format!("Could not encode layer '{}' mask: {err}", layer.name))?,
            None => Vec::new(),
        };
        let smart_bytes = match smart_source {
            Some(source) => png::encode_pixels(width, height, source)
                .map_err(|err| format!("Could not encode layer '{}' source: {err}", layer.name))?,
            None => Vec::new(),
        };
        layers.push(LayerManifest {
            name: layer.name.clone(),
            visible: layer.visible,
            opacity: layer.opacity,
            blend_mode: layer.blend_mode,
            locked: layer.locked,
            png_len: bytes.len() as u32,
            id: layer.id,
            records,
            mask_png_len: mask_bytes.len() as u32,
            smart_png_len: smart_bytes.len() as u32,
        });
        layer_bytes.push(bytes);
        layer_bytes.push(mask_bytes);
        layer_bytes.push(smart_bytes);
    }
    let (pattern_bytes, pattern_size) = match document.pattern() {
        Some(pattern) => (
            png::encode_pixels(pattern.width, pattern.height, &pattern.pixels)
                .map_err(|err| format!("Could not encode the pattern: {err}"))?,
            (pattern.width, pattern.height),
        ),
        None => (Vec::new(), (0, 0)),
    };
    let pattern_png_len = pattern_bytes.len() as u32;
    layer_bytes.push(pattern_bytes);

    // Version 3: the channel planes, spot planes, brush tip, and pattern
    // presets, after the pattern.
    let mut channels = Vec::with_capacity(document.channels().len());
    for channel in document.channels() {
        let bytes = encode_plane(width, height, &channel.pixels)
            .map_err(|err| format!("Could not encode channel '{}': {err}", channel.name))?;
        channels.push(PlaneManifest {
            name: channel.name.clone(),
            png_len: bytes.len() as u32,
        });
        layer_bytes.push(bytes);
    }
    let mut spots = Vec::with_capacity(document.spots().len());
    for spot in document.spots() {
        let bytes = encode_plane(width, height, &spot.pixels)
            .map_err(|err| format!("Could not encode spot channel '{}': {err}", spot.name))?;
        spots.push(SpotManifest {
            name: spot.name.clone(),
            color: spot.color,
            solidity: spot.solidity,
            png_len: bytes.len() as u32,
        });
        layer_bytes.push(bytes);
    }
    let brush_tip = document.brush_tip().map(|tip| {
        let bytes: Vec<u8> = tip.values.iter().flat_map(|v| v.to_le_bytes()).collect();
        let manifest = TipManifest {
            width: tip.width,
            height: tip.height,
            len: bytes.len() as u32,
        };
        layer_bytes.push(bytes);
        manifest
    });
    let mut pattern_presets = Vec::with_capacity(document.pattern_presets().len());
    for (name, pattern) in document.pattern_presets() {
        let bytes = png::encode_pixels(pattern.width, pattern.height, &pattern.pixels)
            .map_err(|err| format!("Could not encode pattern preset '{name}': {err}"))?;
        pattern_presets.push(PatternPresetManifest {
            name: name.clone(),
            width: pattern.width,
            height: pattern.height,
            png_len: bytes.len() as u32,
        });
        layer_bytes.push(bytes);
    }

    let manifest = Manifest {
        width,
        height,
        profile: Some(document.profile()),
        layers,
        version: 3,
        records: document.document_records(),
        pattern_png_len,
        pattern_size,
        channels,
        spots,
        brush_tip,
        pattern_presets,
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
    Ok(out)
}

/// Write `document` to `path` as a project file.
pub fn save(document: &Document, path: &Path) -> Result<(), String> {
    let bytes = encode(document)?;
    std::fs::write(path, bytes).map_err(|err| format!("Could not write {}: {err}", path.display()))
}

/// Decode already-in-memory project-file `bytes` back into a [`Document`],
/// layer stack and all -- [`load`]'s own format, minus the filesystem read,
/// for a project fetched from somewhere other than disk (Cloud Documents'
/// own download).
pub fn decode(bytes: &[u8]) -> Result<Document, String> {
    if bytes.len() < MAGIC.len() + 4 || &bytes[..MAGIC.len()] != MAGIC {
        return Err("not an image-editor project file.".to_string());
    }
    let mut offset = MAGIC.len();

    let manifest_len =
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("4 bytes")) as usize;
    offset += 4;
    let manifest_end = offset
        .checked_add(manifest_len)
        .filter(|&end| end <= bytes.len())
        .ok_or_else(|| "truncated (manifest).".to_string())?;
    let manifest: Manifest = serde_json::from_slice(&bytes[offset..manifest_end])
        .map_err(|err| format!("Corrupt project manifest: {err}"))?;
    offset = manifest_end;

    let mut document = Document::new(manifest.width, manifest.height)?;
    document.profile_was_missing = manifest.profile.is_none();
    document.assign_profile(manifest.profile.unwrap_or_default());
    let mut take = |len: u32, what: &str| -> Result<&[u8], String> {
        let end = offset
            .checked_add(len as usize)
            .filter(|&end| end <= bytes.len())
            .ok_or_else(|| format!("truncated ({what})."))?;
        let slice = &bytes[offset..end];
        offset = end;
        Ok(slice)
    };
    let mut id_map: Vec<(LayerId, LayerId)> = Vec::with_capacity(manifest.layers.len());
    for layer in &manifest.layers {
        let decoded = png::decode_bytes(take(layer.png_len, &format!("layer '{}'", layer.name))?)
            .map_err(|err| format!("Corrupt layer '{}': {err}", layer.name))?;
        if decoded.width != manifest.width || decoded.height != manifest.height {
            return Err(format!(
                "Layer '{}' is {}x{}, but the document is {}x{}.",
                layer.name, decoded.width, decoded.height, manifest.width, manifest.height
            ));
        }
        let mask = if layer.mask_png_len > 0 {
            let plane = png::decode_bytes(take(
                layer.mask_png_len,
                &format!("layer '{}' mask", layer.name),
            )?)
            .map_err(|err| format!("Corrupt layer '{}' mask: {err}", layer.name))?;
            Some(plane.pixels.chunks_exact(4).map(|p| p[0]).collect())
        } else {
            None
        };
        let smart_source = if layer.smart_png_len > 0 {
            let source = png::decode_bytes(take(
                layer.smart_png_len,
                &format!("layer '{}' source", layer.name),
            )?)
            .map_err(|err| format!("Corrupt layer '{}' source: {err}", layer.name))?;
            Some(source.pixels)
        } else {
            None
        };

        let id = document.add_layer(
            layer.name.clone(),
            &decoded.pixels,
            decoded.width,
            decoded.height,
        )?;
        document.set_visible(id, layer.visible)?;
        document.set_opacity(id, layer.opacity)?;
        document.set_blend_mode(id, layer.blend_mode)?;
        document.set_locked(id, layer.locked)?;
        document
            .restore_layer_records(id, layer.records.clone(), mask, smart_source)
            .map_err(|err| format!("Layer '{}': {err}", layer.name))?;
        id_map.push((layer.id, id));
    }
    if manifest.pattern_png_len > 0 {
        let decoded = png::decode_bytes(take(manifest.pattern_png_len, "pattern")?)
            .map_err(|err| format!("Corrupt pattern: {err}"))?;
        if (decoded.width, decoded.height) != manifest.pattern_size {
            return Err("The pattern's size does not match its manifest.".to_string());
        }
        document.restore_pattern(Pattern {
            width: decoded.width,
            height: decoded.height,
            pixels: decoded.pixels,
        })?;
    }
    if manifest.version >= 3 {
        let mut plane = |len: u32, what: &str| -> Result<Vec<u8>, String> {
            let decoded = png::decode_bytes(take(len, what)?)
                .map_err(|err| format!("Corrupt {what}: {err}"))?;
            if decoded.width != manifest.width || decoded.height != manifest.height {
                return Err(format!("The {what} does not match the document's size."));
            }
            Ok(decoded.pixels.chunks_exact(4).map(|p| p[0]).collect())
        };
        let mut channels = Vec::with_capacity(manifest.channels.len());
        for entry in &manifest.channels {
            channels.push(AlphaChannel {
                name: entry.name.clone(),
                pixels: plane(entry.png_len, &format!("channel '{}'", entry.name))?,
            });
        }
        let mut spots = Vec::with_capacity(manifest.spots.len());
        for entry in &manifest.spots {
            spots.push(SpotChannel {
                name: entry.name.clone(),
                color: entry.color,
                solidity: entry.solidity,
                pixels: plane(entry.png_len, &format!("spot channel '{}'", entry.name))?,
            });
        }
        let brush_tip = match &manifest.brush_tip {
            Some(tip) => {
                let bytes = take(tip.len, "brush tip")?;
                if bytes.len() % 4 != 0 {
                    return Err("Corrupt brush tip.".to_string());
                }
                Some(BrushTip {
                    width: tip.width,
                    height: tip.height,
                    values: bytes
                        .chunks_exact(4)
                        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                        .collect(),
                })
            }
            None => None,
        };
        let mut presets = Vec::with_capacity(manifest.pattern_presets.len());
        for entry in &manifest.pattern_presets {
            let decoded = png::decode_bytes(take(
                entry.png_len,
                &format!("pattern preset '{}'", entry.name),
            )?)
            .map_err(|err| format!("Corrupt pattern preset '{}': {err}", entry.name))?;
            if (decoded.width, decoded.height) != (entry.width, entry.height) {
                return Err(format!(
                    "Pattern preset '{}' does not match its manifest.",
                    entry.name
                ));
            }
            presets.push((
                entry.name.clone(),
                Pattern {
                    width: decoded.width,
                    height: decoded.height,
                    pixels: decoded.pixels,
                },
            ));
        }
        document.restore_channels(channels)?;
        document.restore_spots(spots)?;
        if let Some(tip) = brush_tip {
            document.restore_brush_tip(tip)?;
        }
        document.restore_pattern_presets(presets)?;
    }
    if manifest.version >= 2 {
        document.restore_document_records(manifest.records, |old| {
            id_map
                .iter()
                .find(|(saved, _)| *saved == old)
                .map(|(_, new)| *new)
        });
    }

    Ok(document)
}

/// Read a project file back into a [`Document`], layer stack and all.
pub fn load(path: &Path) -> Result<Document, String> {
    let bytes =
        std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;
    decode(&bytes).map_err(|err| format!("{}: {err}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document::{MaskSource, MoveDirection};

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
        document.set_locked(top, true).unwrap();

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
        assert!(!reloaded_bottom.locked);
        assert_eq!(reloaded_bottom.pixels, solid(2, 2, [255, 0, 0, 255]));

        let reloaded_top = &reloaded.layers()[1];
        assert_eq!(reloaded_top.name, "top");
        assert!(reloaded_top.visible);
        assert_eq!(reloaded_top.opacity, 0.5);
        assert_eq!(reloaded_top.blend_mode, BlendMode::Multiply);
        assert!(reloaded_top.locked);
        assert_eq!(reloaded_top.pixels, solid(2, 2, [0, 255, 0, 200]));
    }

    #[test]
    fn encode_and_decode_round_trip_purely_in_memory() {
        // Cloud Documents' own path -- `save`/`load` minus the filesystem,
        // for bytes that come from (and go to) a network call instead of a
        // path. `save`/`load` already exercise the exact same bytes, so
        // this only needs to confirm `encode`/`decode` themselves work
        // without ever touching disk.
        let mut document = Document::new(2, 1).unwrap();
        document
            .add_layer("only", &solid(2, 1, [10, 20, 30, 255]), 2, 1)
            .unwrap();
        let bytes = encode(&document).unwrap();
        assert_eq!(&bytes[..MAGIC.len()], MAGIC);
        let reloaded = decode(&bytes).unwrap();
        assert_eq!((reloaded.width(), reloaded.height()), (2, 1));
        assert_eq!(reloaded.layers()[0].name, "only");
        assert_eq!(reloaded.layers()[0].pixels, solid(2, 1, [10, 20, 30, 255]));
    }

    #[test]
    fn version_two_keeps_every_layer_record_and_the_documents_own() {
        use crate::document::{
            Adjustment, Fill, GuideOrientation, MaskSource, ShapeLayer, ShapeSpec, TextLayer,
        };
        let mut document = Document::new(8, 6).unwrap();
        let photo = document
            .add_layer("photo", &solid(8, 6, [90, 60, 30, 255]), 8, 6)
            .unwrap();
        document.select_rectangle(2.0, 1.0, 6.0, 5.0).unwrap();
        document
            .add_layer_mask(photo, MaskSource::RevealSelection)
            .unwrap();
        document.set_linked(photo, true).unwrap();
        document.save_selection("window").unwrap();
        document.define_pattern(photo).unwrap();
        let fill = document
            .add_fill_layer(
                "Color Fill 1",
                Fill::SolidColor {
                    color: [0, 0, 255, 255],
                },
            )
            .unwrap();
        document.set_clipped(fill, true).unwrap();
        let adjust = document
            .add_adjustment_layer("Threshold 1", Adjustment::Threshold { level: 99 })
            .unwrap();
        let text = document
            .add_text_layer(
                "Hello",
                &TextLayer {
                    text: "Hello".into(),
                    x: 1,
                    y: 1,
                    size: 1,
                    color: [255, 0, 0, 255],
                    vertical: false,
                    font: None,
                },
            )
            .unwrap();
        let shape = document
            .add_shape_layer(
                "Rectangle 1",
                &ShapeLayer {
                    spec: ShapeSpec::Rectangle {
                        x0: 1.0,
                        y0: 1.0,
                        x1: 5.0,
                        y1: 4.0,
                        radius: 0,
                    },
                    fill: Some([0, 255, 0, 255]),
                    stroke: None,
                },
            )
            .unwrap();
        let smart = document
            .add_layer("smart", &solid(8, 6, [10, 20, 30, 255]), 8, 6)
            .unwrap();
        document.convert_to_smart_object(smart).unwrap();
        document
            .add_smart_filter(smart, Adjustment::Invert)
            .unwrap();
        document.add_guide(GuideOrientation::Vertical, 3).unwrap();
        document.add_note(1, 1, "check the sky").unwrap();
        document.add_count_mark(4, 4).unwrap();
        document
            .save_gradient_preset("dusk", [255, 128, 0, 255], [0, 0, 64, 255])
            .unwrap();
        let generated = document.generate_image("a lake", 3, 2, 2.0).unwrap();
        // Removing an earlier layer shifts every later id on reload, so the
        // generated record's id must be remapped, not copied.
        document.remove_layer(adjust).unwrap();

        let bytes = encode(&document).unwrap();
        let reloaded = decode(&bytes).unwrap();
        let by_name = |name: &str| reloaded.layers().iter().find(|l| l.name == name).unwrap();
        let orig = |id: LayerId| document.layers().iter().find(|l| l.id == id).unwrap();
        let photo2 = by_name("photo");
        assert_eq!(photo2.mask, orig(photo).mask);
        assert!(photo2.linked);
        assert!(by_name("Color Fill 1").clipped);
        assert_eq!(
            by_name("Color Fill 1").fill,
            Some(Fill::SolidColor {
                color: [0, 0, 255, 255]
            })
        );
        assert_eq!(by_name("Hello").text, orig(text).text);
        assert_eq!(by_name("Rectangle 1").shape, orig(shape).shape);
        assert_eq!(by_name("smart").smart, orig(smart).smart);
        assert_eq!(by_name("smart").pixels, orig(smart).pixels);
        assert!(reloaded.layers().iter().all(|l| l.name != "Threshold 1"));
        assert_eq!(reloaded.pattern(), document.pattern());
        let records = reloaded.document_records();
        let original = document.document_records();
        assert_eq!(records.guides, original.guides);
        assert_eq!(records.notes, original.notes);
        assert_eq!(records.count_marks, original.count_marks);
        assert_eq!(records.gradient_presets, original.gradient_presets);
        assert_eq!(records.saved_selections, original.saved_selections);
        assert_eq!(records.selection, original.selection);
        // The generated record follows the layer to its new id.
        assert_eq!(records.generated.len(), 1);
        assert_eq!(records.generated[0].prompt, "a lake");
        assert_eq!(records.generated[0].id, by_name("a lake").id);
        assert_ne!(
            records.generated[0].id, generated,
            "ids are reassigned on load"
        );
        assert_eq!(reloaded.view().generated_layers.len(), 1);
        // And it all survives a second save: the format is stable.
        assert_eq!(encode(&reloaded).unwrap().len(), bytes.len());
    }

    #[test]
    fn version_three_keeps_channels_spots_the_brush_tip_presets_comps_and_palettes() {
        use crate::document::{
            AlphaChannel, BrushTip, Ink, LayerComp, LayerCompState, Palette, SpotChannel,
        };
        let (w, h) = (6u32, 4u32);
        let mut document = Document::new(w, h).unwrap();
        let mut photo = Vec::new();
        for i in 0..(w * h) as u8 {
            photo.extend_from_slice(&[i * 10, 255 - i * 10, 7, 255]);
        }
        let base = document.add_layer("photo", &photo, w, h).unwrap();
        let top = document
            .add_layer("top", &[200u8; 6 * 4 * 4], w, h)
            .unwrap();
        // Alpha and spot channels with real planes, a brush tip from the
        // photo, a defined pattern saved as a preset, two layer comps,
        // and an Indexed Color table.
        let plane: Vec<u8> = (0..(w * h) as u8).map(|i| i * 9).collect();
        document
            .restore_channels(vec![AlphaChannel {
                name: "cut-out".to_string(),
                pixels: plane.clone(),
            }])
            .unwrap();
        document
            .restore_spots(vec![SpotChannel {
                name: "PANTONE-ish".to_string(),
                color: [220, 30, 90],
                solidity: 37.5,
                pixels: plane.iter().rev().copied().collect(),
            }])
            .unwrap();
        document.select_rectangle(1.0, 1.0, 4.0, 3.0).unwrap();
        document.define_brush_tip(base).unwrap();
        document.define_pattern(base).unwrap();
        document.save_pattern_preset("window").unwrap();
        document.deselect();
        document.save_layer_comp("both").unwrap();
        document.set_visible(top, false).unwrap();
        document.set_opacity(top, 0.25).unwrap();
        document.save_layer_comp("photo only").unwrap();
        document
            .convert_to_indexed(Palette::Adaptive { colors: 4 })
            .unwrap();
        let tip = document.brush_tip().unwrap().clone();
        assert!(tip.values.iter().any(|&v| v > 0.0 && v < 1.0));

        let bytes = encode(&document).unwrap();
        let reloaded = decode(&bytes).unwrap();
        assert_eq!(reloaded.channels(), document.channels());
        assert_eq!(reloaded.spots(), document.spots());
        assert_eq!(reloaded.brush_tip(), Some(&tip));
        assert_eq!(reloaded.pattern_presets(), document.pattern_presets());
        assert_eq!(reloaded.color_table(), document.color_table());
        let records = reloaded.document_records();
        assert_eq!(records.color_table.len(), 4);
        assert_eq!(records.layer_comps.len(), 2);
        assert_eq!(records.layer_comps[1].name, "photo only");
        // The comps' layer ids are remapped to the reloaded layers.
        let reloaded_top = reloaded
            .layers()
            .iter()
            .find(|layer| layer.name == "top")
            .unwrap()
            .id;
        let state = records.layer_comps[1]
            .states
            .iter()
            .find(|state| state.id == reloaded_top)
            .unwrap();
        assert_eq!(
            *state,
            LayerCompState {
                id: reloaded_top,
                visible: false,
                opacity: 0.25,
                blend_mode: BlendMode::Normal
            }
        );
        // Duotone inks ride along as records too.
        let mut duo = Document::new(2, 2).unwrap();
        duo.add_layer(
            "grey",
            &[
                90u8, 90, 90, 255, 200, 200, 200, 255, 0, 0, 0, 255, 255, 255, 255, 255,
            ],
            2,
            2,
        )
        .unwrap();
        let inks = vec![
            Ink {
                color: [0, 0, 0],
                curve: Vec::new(),
            },
            Ink {
                color: [200, 120, 0],
                curve: vec![(0, 0), (128, 90), (255, 255)],
            },
        ];
        duo.convert_to_duotone(&inks).unwrap();
        let back = decode(&encode(&duo).unwrap()).unwrap();
        assert_eq!(back.document_records().duotone, inks);
        assert_eq!(back.document_records().mode, duo.document_records().mode);
        // A comp whose layers are all gone is dropped rather than kept empty.
        let mut lone = Document::new(2, 2).unwrap();
        let only = lone.add_layer("only", &[1u8; 16], 2, 2).unwrap();
        lone.save_layer_comp("solo").unwrap();
        let mut records = lone.document_records();
        records.layer_comps.push(LayerComp {
            name: "ghost".to_string(),
            states: vec![LayerCompState {
                id: 999,
                visible: true,
                opacity: 1.0,
                blend_mode: BlendMode::Normal,
            }],
        });
        lone.restore_document_records(records, |old| if old == only { Some(only) } else { None });
        assert_eq!(
            lone.document_records()
                .layer_comps
                .iter()
                .map(|c| c.name.as_str())
                .collect::<Vec<_>>(),
            vec!["solo"]
        );
        // Bad planes are refused.
        let mut small = Document::new(2, 2).unwrap();
        assert!(small
            .restore_channels(vec![AlphaChannel {
                name: "x".to_string(),
                pixels: vec![0; 3],
            }])
            .is_err());
        assert!(small
            .restore_brush_tip(BrushTip {
                width: 2,
                height: 2,
                values: vec![0.5; 3],
            })
            .is_err());
    }

    #[test]
    fn a_version_one_file_still_reads() {
        // A file exactly as the version-1 writer laid it out: no records,
        // no blob lengths, no version.
        let pixels = png::encode_pixels(2, 2, &solid(2, 2, [1, 2, 3, 255])).unwrap();
        let manifest = format!(
            r#"{{"width":2,"height":2,"layers":[{{"name":"old","visible":true,"opacity":1.0,"blend_mode":"normal","png_len":{}}}]}}"#,
            pixels.len()
        );
        let mut bytes = Vec::new();
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(manifest.len() as u32).to_le_bytes());
        bytes.extend_from_slice(manifest.as_bytes());
        bytes.extend_from_slice(&pixels);
        let document = decode(&bytes).unwrap();
        assert_eq!(document.layers().len(), 1);
        assert_eq!(document.layers()[0].name, "old");
        assert!(document.layers()[0].mask.is_none());
        assert!(document.profile_was_missing);
        assert!(document.document_records().guides.is_empty());
        // A truncated mask blob is reported as such.
        let mut doc = Document::new(2, 2).unwrap();
        let id = doc
            .add_layer("m", &solid(2, 2, [5, 5, 5, 255]), 2, 2)
            .unwrap();
        doc.add_layer_mask(id, MaskSource::HideAll).unwrap();
        let full = encode(&doc).unwrap();
        assert!(decode(&full[..full.len() - 10])
            .unwrap_err()
            .contains("truncated"));
    }

    #[test]
    fn a_round_trip_preserves_the_assigned_colour_profile() {
        let mut document = Document::new(1, 1).unwrap();
        document
            .add_layer("solo", &solid(1, 1, [1, 2, 3, 255]), 1, 1)
            .unwrap();
        document.assign_profile(ColorProfile::AdobeRgb1998);
        let bytes = encode(&document).unwrap();
        let reloaded = decode(&bytes).unwrap();
        assert_eq!(reloaded.profile(), ColorProfile::AdobeRgb1998);
    }

    #[test]
    fn a_manifest_from_before_embed_color_profile_existed_defaults_to_srgb() {
        // Same rewrite-the-manifest trick as the pre-Lock test below,
        // proving `#[serde(default)]` on `profile` loads a project file
        // saved before Embed Color Profile existed, defaulting to this
        // format's own original working space, sRGB.
        let mut document = Document::new(1, 1).unwrap();
        document
            .add_layer("solo", &solid(1, 1, [4, 5, 6, 255]), 1, 1)
            .unwrap();
        document.assign_profile(ColorProfile::AdobeRgb1998);
        let path = temp_path("project_rs_pre_profile_format.iep");
        save(&document, &path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let manifest_start = MAGIC.len() + 4;
        let manifest_len =
            u32::from_le_bytes(bytes[MAGIC.len()..manifest_start].try_into().unwrap()) as usize;
        let manifest_end = manifest_start + manifest_len;
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&bytes[manifest_start..manifest_end]).unwrap();
        manifest.as_object_mut().unwrap().remove("profile");
        let rewritten_manifest = serde_json::to_vec(&manifest).unwrap();

        let mut rewritten_file = Vec::new();
        rewritten_file.extend_from_slice(&bytes[..MAGIC.len()]);
        rewritten_file.extend_from_slice(&(rewritten_manifest.len() as u32).to_le_bytes());
        rewritten_file.extend_from_slice(&rewritten_manifest);
        rewritten_file.extend_from_slice(&bytes[manifest_end..]);
        std::fs::write(&path, rewritten_file).unwrap();

        let reloaded = load(&path).unwrap();
        assert_eq!(reloaded.profile(), ColorProfile::Srgb);
        // Color Settings > Missing Profile Warning: a project file with no
        // `profile` key at all is flagged, distinct from one explicitly
        // saved as sRGB.
        assert!(reloaded.profile_was_missing());
    }

    #[test]
    fn a_normal_round_trip_never_flags_a_missing_profile() {
        let mut document = Document::new(1, 1).unwrap();
        document
            .add_layer("solo", &solid(1, 1, [7, 8, 9, 255]), 1, 1)
            .unwrap();
        let bytes = encode(&document).unwrap();
        let reloaded = decode(&bytes).unwrap();
        assert!(!reloaded.profile_was_missing());
    }

    #[test]
    fn decode_rejects_the_same_malformed_bytes_load_does() {
        assert!(decode(b"NOTAPROJECTFILE")
            .unwrap_err()
            .contains("not an image-editor project file"));
        let mut truncated = MAGIC.to_vec();
        truncated.extend_from_slice(&1000u32.to_le_bytes());
        assert!(decode(&truncated).unwrap_err().contains("truncated"));
    }

    #[test]
    fn a_manifest_from_before_lock_existed_defaults_to_unlocked() {
        // A project file saved before the `locked` field existed has no key
        // for it in the manifest JSON at all — `#[serde(default)]` is what
        // makes that still load instead of a hard parse error. Rebuilds the
        // file with the key stripped from the manifest and a recomputed
        // length prefix, since the manifest is free to change size — only
        // the `png_len` values (untouched here) have to stay honest.
        let mut document = Document::new(1, 1).unwrap();
        document
            .add_layer("solo", &solid(1, 1, [1, 2, 3, 255]), 1, 1)
            .unwrap();
        let path = temp_path("project_rs_pre_lock_format.iep");
        save(&document, &path).unwrap();

        let bytes = std::fs::read(&path).unwrap();
        let manifest_start = MAGIC.len() + 4;
        let manifest_len =
            u32::from_le_bytes(bytes[MAGIC.len()..manifest_start].try_into().unwrap()) as usize;
        let manifest_end = manifest_start + manifest_len;
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&bytes[manifest_start..manifest_end]).unwrap();
        manifest["layers"][0]
            .as_object_mut()
            .unwrap()
            .remove("locked");
        let rewritten_manifest = serde_json::to_vec(&manifest).unwrap();

        let mut rewritten_file = Vec::new();
        rewritten_file.extend_from_slice(&bytes[..MAGIC.len()]);
        rewritten_file.extend_from_slice(&(rewritten_manifest.len() as u32).to_le_bytes());
        rewritten_file.extend_from_slice(&rewritten_manifest);
        rewritten_file.extend_from_slice(&bytes[manifest_end..]); // layer PNG bytes, untouched
        std::fs::write(&path, rewritten_file).unwrap();

        let reloaded = load(&path).unwrap();
        assert!(!reloaded.layers()[0].locked);
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
