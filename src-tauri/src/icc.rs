//! Real ICC profile (`.icc`/`.icm`) file parsing — Color Settings > ICC
//! Color Profiles / Monitor Profile / Input Device Profile / Output Device
//! Profile: a profile a user actually assigns there is a real file on disk
//! in the ICC.1 binary format (ICC Specification ICC.1:2010), not a name
//! picked from a fixed list. This module reads that real binary layout —
//! the 128-byte header, the tag table, and enough of the common tag types
//! (`desc`/`mluc`/`text` for the profile's own description, `XYZType` for
//! its white point and RGB primaries) to show real, file-derived metadata
//! rather than fabricate any of it.

use std::collections::HashMap;
use std::convert::TryInto;

use serde::Serialize;

use crate::document::RenderingIntent;

/// One ICC profile's own parsed header + the handful of tags this project
/// reads. Everything here is read directly out of the file's own bytes —
/// nothing is inferred or guessed when a tag is present.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IccProfile {
    /// The header's own Data Colour Space signature (bytes 16-19), trimmed
    /// of trailing spaces — `"RGB"`, `"GRAY"`, `"CMYK"`, `"Lab"`, etc.
    pub color_space: String,
    /// The header's own Profile/Device Class signature (bytes 12-15) —
    /// `"mntr"` (Monitor: used for both display and, per the spec,
    /// input/output device profiles), `"scnr"` (input), `"prtr"` (output),
    /// `"spac"` (colour space conversion), `"link"`, `"abst"`, `"nmcl"`.
    pub device_class: String,
    /// The header's own Rendering Intent field (bytes 64-67), decoded into
    /// this project's own [`RenderingIntent`] — the exact same four values
    /// the ICC spec and Photoshop's own Rendering Intent dropdown share (0
    /// = Perceptual, 1 = Media-Relative Colorimetric, 2 = Saturation, 3 =
    /// ICC-Absolute Colorimetric).
    pub rendering_intent: RenderingIntent,
    /// The profile's own human-readable description — the `desc` tag,
    /// parsed from whichever real tag type actually stores it (`desc`'s
    /// own textDescriptionType in an ICC v2 file, `mluc`
    /// multiLocalizedUnicodeType in a v4 one). `None` when the tag is
    /// missing.
    pub description: Option<String>,
    /// The profile's own white point (`wtpt` tag, an `XYZType`), in
    /// PCS-relative CIE XYZ. `None` when the tag is missing.
    pub white_point: Option<(f64, f64, f64)>,
    /// The profile's own red/green/blue primaries (`rXYZ`/`gXYZ`/`bXYZ`,
    /// each an `XYZType`) — present on a real display/RGB working-space
    /// profile, absent on e.g. a CMYK output profile. `None` per channel
    /// when that tag is missing.
    pub red_primary: Option<(f64, f64, f64)>,
    pub green_primary: Option<(f64, f64, f64)>,
    pub blue_primary: Option<(f64, f64, f64)>,
}

fn read_u32(bytes: &[u8], at: usize) -> Result<u32, String> {
    bytes
        .get(at..at + 4)
        .and_then(|slice| slice.try_into().ok())
        .map(u32::from_be_bytes)
        .ok_or_else(|| "Not a valid ICC profile: the file is truncated.".to_string())
}

fn read_s15fixed16(bytes: &[u8], at: usize) -> Result<f64, String> {
    read_u32(bytes, at).map(|raw| raw as i32 as f64 / 65536.0)
}

fn read_signature(bytes: &[u8], at: usize) -> Result<String, String> {
    let raw = bytes
        .get(at..at + 4)
        .ok_or_else(|| "Not a valid ICC profile: the file is truncated.".to_string())?;
    Ok(String::from_utf8_lossy(raw).trim_end().to_string())
}

/// The header's own Rendering Intent field (bytes 64-67) decoded into this
/// project's own [`RenderingIntent`] — any value other than the three
/// explicit ICC-spec alternatives falls back to Media-Relative Colorimetric
/// (ICC value `1`), the spec's own default and this project's own
/// [`RenderingIntent::default`].
fn decode_rendering_intent(raw: u32) -> RenderingIntent {
    match raw {
        0 => RenderingIntent::Perceptual,
        2 => RenderingIntent::Saturation,
        3 => RenderingIntent::AbsoluteColorimetric,
        _ => RenderingIntent::RelativeColorimetric,
    }
}

/// The `desc` tag's own two real shapes: ICC v2's `textDescriptionType`
/// (tag type signature `desc`) or ICC v4's `multiLocalizedUnicodeType` (tag
/// type signature `mluc`), which every v4 profile's own `desc` tag actually
/// uses instead. Returns `Ok(None)` on a tag type this project doesn't
/// recognise (rather than guessing at its layout) or on a truncated tag,
/// never fabricating a description that was not actually in the file.
fn parse_desc_tag(bytes: &[u8], offset: usize, size: usize) -> Result<Option<String>, String> {
    let tag_type = read_signature(bytes, offset)?;
    match tag_type.as_str() {
        // textDescriptionType: 4-byte type signature, 4 reserved bytes,
        // then a u32 ASCII length (including the trailing NUL) followed by
        // that many bytes of ASCII. Unicode/ScriptCode fields can follow;
        // this project only reads the ASCII form.
        "desc" => {
            if size < 12 {
                return Ok(None);
            }
            let ascii_len = read_u32(bytes, offset + 8)? as usize;
            let start = offset + 12;
            let Some(raw) = bytes.get(start..bytes.len().min(start + ascii_len)) else {
                return Ok(None);
            };
            let text = raw.split(|&byte| byte == 0).next().unwrap_or(&[]);
            Ok(Some(String::from_utf8_lossy(text).into_owned()))
        }
        // multiLocalizedUnicodeType: 4-byte type signature, 4 reserved
        // bytes, a u32 record count, a u32 record size (12 per the spec),
        // then that many 12-byte records (language code, country code,
        // string length, string offset from the tag's own start) pointing
        // at UTF-16BE text. Only the first record is read — this project
        // has no locale preference of its own to pick a different one by.
        "mluc" => {
            if size < 16 {
                return Ok(None);
            }
            let record_count = read_u32(bytes, offset + 8)?;
            let record_size = read_u32(bytes, offset + 12)?;
            if record_count == 0 || record_size < 12 {
                return Ok(None);
            }
            let record_at = offset + 16;
            let str_len = read_u32(bytes, record_at + 4)? as usize;
            let str_offset = read_u32(bytes, record_at + 8)? as usize;
            let start = offset + str_offset;
            let Some(raw) = bytes.get(start..bytes.len().min(start + str_len)) else {
                return Ok(None);
            };
            let units: Vec<u16> = raw
                .chunks_exact(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect();
            Ok(Some(String::from_utf16_lossy(&units)))
        }
        _ => Ok(None),
    }
}

/// Parses `bytes` as a real ICC profile file: the 128-byte header, then the
/// tag table, then the `desc`/`wtpt`/`rXYZ`/`gXYZ`/`bXYZ` tags among
/// whatever tags the file actually has. Any other tag (e.g. `rTRC`, `cprt`,
/// `chad`) may be present in a real file and is simply not read by this
/// project — not an error, since a real profile always carries tags beyond
/// the ones listed here.
pub fn parse(bytes: &[u8]) -> Result<IccProfile, String> {
    // 128-byte header + a 4-byte tag count immediately after it.
    if bytes.len() < 132 {
        return Err("Not a valid ICC profile: the file is too short to hold a header.".to_string());
    }
    let file_signature = read_signature(bytes, 36)?;
    if file_signature != "acsp" {
        return Err(
            "Not a valid ICC profile: missing the \"acsp\" file signature at byte 36.".to_string(),
        );
    }

    let color_space = read_signature(bytes, 16)?;
    let device_class = read_signature(bytes, 12)?;
    let rendering_intent = decode_rendering_intent(read_u32(bytes, 64)?);

    let tag_count = read_u32(bytes, 128)? as usize;
    let mut tags: HashMap<String, (usize, usize)> = HashMap::with_capacity(tag_count);
    for index in 0..tag_count {
        let entry_at = 132 + index * 12;
        let signature = read_signature(bytes, entry_at)?;
        let offset = read_u32(bytes, entry_at + 4)? as usize;
        let size = read_u32(bytes, entry_at + 8)? as usize;
        tags.insert(signature, (offset, size));
    }

    // XYZType: 4-byte type signature, 4 reserved bytes, then one or more
    // XYZ triples of s15Fixed16Number — `wtpt`/`rXYZ`/`gXYZ`/`bXYZ` each
    // carry exactly one.
    let read_xyz_tag = |name: &str| -> Result<Option<(f64, f64, f64)>, String> {
        let Some(&(offset, size)) = tags.get(name) else {
            return Ok(None);
        };
        if size < 20 {
            return Ok(None);
        }
        Ok(Some((
            read_s15fixed16(bytes, offset + 8)?,
            read_s15fixed16(bytes, offset + 12)?,
            read_s15fixed16(bytes, offset + 16)?,
        )))
    };

    let description = match tags.get("desc") {
        Some(&(offset, size)) => parse_desc_tag(bytes, offset, size)?,
        None => None,
    };

    Ok(IccProfile {
        color_space,
        device_class,
        rendering_intent,
        description,
        white_point: read_xyz_tag("wtpt")?,
        red_primary: read_xyz_tag("rXYZ")?,
        green_primary: read_xyz_tag("gXYZ")?,
        blue_primary: read_xyz_tag("bXYZ")?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a minimal, real-shaped ICC profile file byte-for-byte: a
    /// 128-byte header, a tag table for the given `(signature, data)`
    /// pairs, and each tag's own data appended after the table — the same
    /// structure [`parse`] itself expects, assembled by hand the same way
    /// a real ICC encoder would, not a shortcut around the real format.
    fn build_icc_bytes(
        device_class: &str,
        color_space: &str,
        rendering_intent: u32,
        tags: &[(&str, Vec<u8>)],
    ) -> Vec<u8> {
        let mut header = vec![0u8; 128];
        header[12..16].copy_from_slice(device_class.as_bytes());
        header[16..20].copy_from_slice(color_space.as_bytes());
        header[36..40].copy_from_slice(b"acsp");
        header[64..68].copy_from_slice(&rendering_intent.to_be_bytes());

        let mut tag_table = Vec::new();
        tag_table.extend_from_slice(&(tags.len() as u32).to_be_bytes());
        let mut tag_data = Vec::new();
        let data_start = 128 + 4 + tags.len() * 12;
        for (signature, data) in tags {
            let offset = data_start + tag_data.len();
            tag_table.extend_from_slice(signature.as_bytes());
            tag_table.extend_from_slice(&(offset as u32).to_be_bytes());
            tag_table.extend_from_slice(&(data.len() as u32).to_be_bytes());
            tag_data.extend_from_slice(data);
        }

        let mut bytes = header;
        bytes.extend_from_slice(&tag_table);
        bytes.extend_from_slice(&tag_data);
        // The header's own Profile Size field (bytes 0-3) is the whole
        // file's own real length -- filled in last, now that it's known.
        let total_len = bytes.len() as u32;
        bytes[0..4].copy_from_slice(&total_len.to_be_bytes());
        bytes
    }

    fn xyz_tag_bytes(x: f64, y: f64, z: f64) -> Vec<u8> {
        let mut data = b"XYZ ".to_vec();
        data.extend_from_slice(&[0, 0, 0, 0]);
        for value in [x, y, z] {
            let fixed = (value * 65536.0).round() as i32;
            data.extend_from_slice(&fixed.to_be_bytes());
        }
        data
    }

    fn desc_tag_bytes(text: &str) -> Vec<u8> {
        let mut data = b"desc".to_vec();
        data.extend_from_slice(&[0, 0, 0, 0]);
        let ascii_len = (text.len() + 1) as u32; // + the trailing NUL.
        data.extend_from_slice(&ascii_len.to_be_bytes());
        data.extend_from_slice(text.as_bytes());
        data.push(0);
        data
    }

    fn mluc_tag_bytes(text: &str) -> Vec<u8> {
        let units: Vec<u8> = text.encode_utf16().flat_map(|u| u.to_be_bytes()).collect();
        let mut data = b"mluc".to_vec();
        data.extend_from_slice(&[0, 0, 0, 0]);
        data.extend_from_slice(&1u32.to_be_bytes()); // record count
        data.extend_from_slice(&12u32.to_be_bytes()); // record size
        data.extend_from_slice(b"en"); // language code
        data.extend_from_slice(b"US"); // country code
        data.extend_from_slice(&(units.len() as u32).to_be_bytes());
        // String offset is measured from the tag's own start (byte 0),
        // which is 16 (header) + 12 (one record) here.
        data.extend_from_slice(&28u32.to_be_bytes());
        data.extend_from_slice(&units);
        data
    }

    #[test]
    fn rejects_a_file_with_no_acsp_signature() {
        let bytes = vec![0u8; 200];
        let error = parse(&bytes).unwrap_err();
        assert!(error.contains("acsp"), "unexpected error: {error}");
    }

    #[test]
    fn rejects_a_file_too_short_to_hold_a_header() {
        let error = parse(&[0u8; 40]).unwrap_err();
        assert!(error.contains("too short"), "unexpected error: {error}");
    }

    #[test]
    fn parses_device_class_colour_space_and_rendering_intent() {
        let bytes = build_icc_bytes("mntr", "RGB ", 1, &[]);
        let profile = parse(&bytes).unwrap();
        assert_eq!(profile.device_class, "mntr");
        assert_eq!(profile.color_space, "RGB");
        assert_eq!(
            profile.rendering_intent,
            RenderingIntent::RelativeColorimetric
        );
        assert_eq!(profile.description, None);
        assert_eq!(profile.white_point, None);
    }

    #[test]
    fn decodes_every_real_icc_rendering_intent_value() {
        for (raw, expected) in [
            (0u32, RenderingIntent::Perceptual),
            (1, RenderingIntent::RelativeColorimetric),
            (2, RenderingIntent::Saturation),
            (3, RenderingIntent::AbsoluteColorimetric),
        ] {
            let bytes = build_icc_bytes("mntr", "RGB ", raw, &[]);
            assert_eq!(parse(&bytes).unwrap().rendering_intent, expected);
        }
    }

    #[test]
    fn parses_white_point_and_rgb_primaries_from_real_xyz_tags() {
        let tags = [
            ("wtpt", xyz_tag_bytes(0.9642, 1.0, 0.8249)),
            ("rXYZ", xyz_tag_bytes(0.4361, 0.2225, 0.0139)),
            ("gXYZ", xyz_tag_bytes(0.3851, 0.7169, 0.0971)),
            ("bXYZ", xyz_tag_bytes(0.1431, 0.0606, 0.7141)),
        ];
        let bytes = build_icc_bytes("mntr", "RGB ", 1, &tags);
        let profile = parse(&bytes).unwrap();
        let (wx, wy, wz) = profile.white_point.unwrap();
        assert!((wx - 0.9642).abs() < 1e-4);
        assert!((wy - 1.0).abs() < 1e-4);
        assert!((wz - 0.8249).abs() < 1e-4);
        let (rx, ry, rz) = profile.red_primary.unwrap();
        assert!((rx - 0.4361).abs() < 1e-4);
        assert!((ry - 0.2225).abs() < 1e-4);
        assert!((rz - 0.0139).abs() < 1e-4);
        assert!(profile.green_primary.is_some());
        assert!(profile.blue_primary.is_some());
    }

    #[test]
    fn parses_an_icc_v2_ascii_desc_tag() {
        let bytes = build_icc_bytes(
            "mntr",
            "RGB ",
            1,
            &[("desc", desc_tag_bytes("sRGB IEC61966-2.1"))],
        );
        let profile = parse(&bytes).unwrap();
        assert_eq!(profile.description.as_deref(), Some("sRGB IEC61966-2.1"));
    }

    #[test]
    fn parses_an_icc_v4_multi_localized_unicode_desc_tag() {
        let bytes = build_icc_bytes("mntr", "RGB ", 1, &[("desc", mluc_tag_bytes("Display P3"))]);
        let profile = parse(&bytes).unwrap();
        assert_eq!(profile.description.as_deref(), Some("Display P3"));
    }

    #[test]
    fn a_negative_s15fixed16_value_round_trips_through_two_complement() {
        // A primary's own XYZ can carry a small negative component (real
        // wide-gamut primaries do) -- confirms the cast through i32 middle
        // step doesn't silently clamp it to zero the way an unsigned
        // reading would.
        let bytes = build_icc_bytes(
            "mntr",
            "RGB ",
            1,
            &[("rXYZ", xyz_tag_bytes(-0.0123, 0.5, 0.25))],
        );
        let profile = parse(&bytes).unwrap();
        let (rx, _, _) = profile.red_primary.unwrap();
        assert!((rx - (-0.0123)).abs() < 1e-4, "got {rx}");
    }
}
