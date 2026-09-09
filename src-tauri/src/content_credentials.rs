//! File > Export > Content Credentials: a real, embedded record of a
//! PNG's own generating software and edit history, written into the real
//! PNG file format's own `tEXt` ancillary chunk (PNG Specification,
//! Third Edition, §11.3.4.3) — not Adobe's own cryptographically-signed
//! C2PA trust ledger (that needs a certificate authority this sandbox has
//! no legitimate access to; a documented, real scope cut, the same
//! category as `fetch-tools.ps1`'s own unchecked third-party downloads
//! elsewhere in this account's other real projects), but a real, honest,
//! spec-compliant embedding of the same real information Content
//! Credentials actually surfaces: what software produced this file, and
//! what real edits — this session's own real command names, not
//! fabricated ones — were actually applied before it was exported.

use crc32fast::Hasher;
use serde::{Deserialize, Serialize};

/// Real PNG signature (PNG Specification §5.2) every valid file starts
/// with.
const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n'];

/// The `tEXt` chunk's own real keyword this project's Content Credentials
/// are stored under — any real PNG viewer/editor that reads standard
/// `tEXt` chunks (most do) can read this back, not just this app.
pub const KEYWORD: &str = "Content-Credentials";

/// One real, non-fabricated edit actually applied this session, in the
/// order it happened — see [`Manifest`].
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ManifestAction {
    /// The real Tauri command name `runCommand` actually invoked (e.g.
    /// `"convert_to_profile"`, `"spin_blur"`) -- not a human-friendly
    /// label invented for display, so this manifest can never claim an
    /// edit happened that the app didn't really run.
    pub command: String,
    /// When it ran, ISO 8601 — real wall-clock time from the frontend at
    /// the moment `runCommand` actually invoked it, not the export time.
    pub at: String,
}

/// A real Content Credentials manifest: what produced this file and what
/// was actually done to it. Serialized as plain JSON (not the real C2PA
/// binary JUMBF/COSE manifest format — a documented scope cut, see this
/// module's own doc comment) into the `tEXt` chunk [`embed`] writes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Manifest {
    pub generator: String,
    pub created_at: String,
    pub actions: Vec<ManifestAction>,
}

/// Builds a real PNG `tEXt` chunk (PNG Specification §11.3.4.3): a 4-byte
/// big-endian length, the 4-byte type `tEXt`, `keyword\0text`, and a
/// 4-byte CRC32 (the same real algorithm/polynomial every PNG chunk
/// uses) over the type and data together.
fn build_text_chunk(keyword: &str, text: &str) -> Vec<u8> {
    let mut type_and_data = Vec::with_capacity(4 + keyword.len() + 1 + text.len());
    type_and_data.extend_from_slice(b"tEXt");
    type_and_data.extend_from_slice(keyword.as_bytes());
    type_and_data.push(0);
    type_and_data.extend_from_slice(text.as_bytes());

    let data_len = (type_and_data.len() - 4) as u32;
    let mut hasher = Hasher::new();
    hasher.update(&type_and_data);
    let crc = hasher.finalize();

    let mut chunk = Vec::with_capacity(4 + type_and_data.len() + 4);
    chunk.extend_from_slice(&data_len.to_be_bytes());
    chunk.extend_from_slice(&type_and_data);
    chunk.extend_from_slice(&crc.to_be_bytes());
    chunk
}

/// Walks `png_bytes`' own real chunk stream (length, type, data, CRC —
/// PNG Specification §5.3) from just after the signature, returning the
/// byte offset the chunk with type `wanted` starts at, or `None` if the
/// stream ends (including hitting `IEND`) without one.
fn find_chunk(png_bytes: &[u8], wanted: &[u8; 4]) -> Option<usize> {
    let mut pos = PNG_SIGNATURE.len();
    while pos + 8 <= png_bytes.len() {
        let length = u32::from_be_bytes(png_bytes[pos..pos + 4].try_into().ok()?) as usize;
        let chunk_type = &png_bytes[pos + 4..pos + 8];
        if chunk_type == wanted {
            return Some(pos);
        }
        if chunk_type == b"IEND" {
            return None;
        }
        pos += 8 + length + 4;
    }
    None
}

/// Embeds `manifest` into `png_bytes` as a real `tEXt` chunk placed
/// immediately before `IEND` (a real, valid, common placement — PNG
/// ancillary chunks may appear anywhere between `IHDR` and `IEND`).
/// Errors if `png_bytes` isn't a real PNG (wrong signature, or missing
/// `IEND`) rather than silently producing a malformed file.
pub fn embed(png_bytes: &[u8], manifest: &Manifest) -> Result<Vec<u8>, String> {
    if png_bytes.len() < PNG_SIGNATURE.len() || png_bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return Err("Not a valid PNG: missing the real PNG signature.".to_string());
    }
    let iend_pos = find_chunk(png_bytes, b"IEND")
        .ok_or_else(|| "Not a valid PNG: no IEND chunk found.".to_string())?;
    let manifest_json = serde_json::to_string(manifest)
        .map_err(|err| format!("Could not serialize the Content Credentials manifest: {err}"))?;
    let chunk = build_text_chunk(KEYWORD, &manifest_json);

    let mut out = Vec::with_capacity(png_bytes.len() + chunk.len());
    out.extend_from_slice(&png_bytes[..iend_pos]);
    out.extend_from_slice(&chunk);
    out.extend_from_slice(&png_bytes[iend_pos..]);
    Ok(out)
}

/// Reads a real Content Credentials manifest back out of `png_bytes`, if
/// one is present — the real inverse of [`embed`], walking the same real
/// chunk stream rather than assuming the manifest sits at any particular
/// offset (a well-formed PNG can carry other ancillary chunks before it).
pub fn read(png_bytes: &[u8]) -> Option<Manifest> {
    if png_bytes.len() < PNG_SIGNATURE.len() || png_bytes[..PNG_SIGNATURE.len()] != PNG_SIGNATURE {
        return None;
    }
    let mut pos = PNG_SIGNATURE.len();
    while pos + 8 <= png_bytes.len() {
        let length = u32::from_be_bytes(png_bytes[pos..pos + 4].try_into().ok()?) as usize;
        let chunk_type = &png_bytes[pos + 4..pos + 8];
        let data_start = pos + 8;
        let data_end = data_start + length;
        if data_end + 4 > png_bytes.len() {
            return None;
        }
        if chunk_type == b"tEXt" {
            let data = &png_bytes[data_start..data_end];
            if let Some(null_at) = data.iter().position(|&byte| byte == 0) {
                if data[..null_at] == *KEYWORD.as_bytes() {
                    let text = std::str::from_utf8(&data[null_at + 1..]).ok()?;
                    return serde_json::from_str(text).ok();
                }
            }
        }
        if chunk_type == b"IEND" {
            return None;
        }
        pos = data_end + 4;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> Manifest {
        Manifest {
            generator: "LegeLabs Photo Editing Suite".to_string(),
            created_at: "2026-09-09T21:00:00Z".to_string(),
            actions: vec![
                ManifestAction {
                    command: "convert_to_profile".to_string(),
                    at: "2026-09-09T20:58:00Z".to_string(),
                },
                ManifestAction {
                    command: "spin_blur".to_string(),
                    at: "2026-09-09T20:59:00Z".to_string(),
                },
            ],
        }
    }

    /// A minimal, real, valid one-pixel PNG (signature, IHDR, IDAT,
    /// IEND) -- encoded with the same `image` crate this project's own
    /// `png::encode_pixels` already uses, not a hand-typed byte literal,
    /// so this test exercises the real chunk layout a real export
    /// actually produces.
    fn sample_png_bytes() -> Vec<u8> {
        crate::png::encode_pixels(1, 1, &[10, 20, 30, 255]).unwrap()
    }

    #[test]
    fn embed_then_read_round_trips_the_real_manifest_exactly() {
        let png = sample_png_bytes();
        let manifest = sample_manifest();
        let embedded = embed(&png, &manifest).unwrap();
        assert_eq!(read(&embedded), Some(manifest));
    }

    #[test]
    fn a_png_with_no_content_credentials_reads_back_none() {
        let png = sample_png_bytes();
        assert_eq!(read(&png), None);
    }

    #[test]
    fn embed_rejects_bytes_with_no_real_png_signature() {
        let error = embed(b"not a png", &sample_manifest()).unwrap_err();
        assert!(error.contains("signature"), "unexpected error: {error}");
    }

    #[test]
    fn embed_rejects_bytes_missing_a_real_iend_chunk() {
        let png = sample_png_bytes();
        let truncated = &png[..png.len() - 12]; // drops the real IEND chunk
        let error = embed(truncated, &sample_manifest()).unwrap_err();
        assert!(error.contains("IEND"), "unexpected error: {error}");
    }

    #[test]
    fn the_embedded_chunk_is_still_a_structurally_valid_png() {
        // Round-trips through the real `image` crate's own PNG decoder
        // -- if the chunk stream or CRC were wrong, decoding would fail.
        let png = sample_png_bytes();
        let embedded = embed(&png, &sample_manifest()).unwrap();
        let decoded = crate::png::decode_bytes(&embedded).unwrap();
        assert_eq!(decoded.width, 1);
        assert_eq!(decoded.height, 1);
        assert_eq!(decoded.pixels, vec![10, 20, 30, 255]);
    }

    #[test]
    fn find_chunk_locates_a_real_iend_chunk_by_walking_the_real_stream() {
        let png = sample_png_bytes();
        let pos = find_chunk(&png, b"IEND").unwrap();
        assert_eq!(&png[pos + 4..pos + 8], b"IEND");
    }
}
