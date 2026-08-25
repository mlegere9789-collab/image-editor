//! End-to-end checks over the real sample images: decode from disk, build a
//! document, flatten it, and encode the result. The unit tests pin the
//! arithmetic; these pin the seams between the stages.

use image_editor_lib::blend::BlendMode;
use image_editor_lib::composite::flatten;
use image_editor_lib::document::Document;
use image_editor_lib::png;

use std::path::PathBuf;

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("samples")
        .join(name)
}

fn document_from(name: &str) -> Document {
    let decoded = png::read(&sample(name)).expect("sample should decode");
    let mut document =
        Document::new(decoded.width, decoded.height).expect("sample should have a valid size");
    document
        .add_layer(name, &decoded.pixels, decoded.width, decoded.height)
        .expect("sample should fit its own document");
    document
}

fn push_layer(document: &mut Document, name: &str) -> u64 {
    let decoded = png::read(&sample(name)).expect("sample should decode");
    document
        .add_layer(name, &decoded.pixels, decoded.width, decoded.height)
        .expect("layer should be addable")
}

#[test]
fn the_bundled_samples_load_at_their_documented_size() {
    for name in ["sample.png", "rings.png"] {
        let decoded = png::read(&sample(name)).expect("sample should decode");
        assert_eq!(
            (decoded.width, decoded.height),
            (640, 400),
            "{name} is not 640x400"
        );
        assert_eq!(decoded.pixels.len(), 640 * 400 * 4);
    }
}

#[test]
fn a_single_layer_document_flattens_back_to_its_source() {
    let decoded = png::read(&sample("sample.png")).unwrap();
    let document = document_from("sample.png");
    let composite = flatten(&document);

    assert_eq!((composite.width, composite.height), (640, 400));

    // Every visible pixel of a lone layer survives the round trip exactly.
    // Fully transparent pixels are the documented exception: they carry no
    // visible colour, so the compositor normalises them to all-zero rather than
    // preserving whatever RGB happened to sit under alpha 0.
    let mut checked_visible = 0;
    let mut checked_transparent = 0;
    for (index, (out, src)) in composite
        .pixels
        .chunks_exact(4)
        .zip(decoded.pixels.chunks_exact(4))
        .enumerate()
    {
        if src[3] == 0 {
            assert_eq!(out, [0, 0, 0, 0], "transparent pixel {index} was not normalised");
            checked_transparent += 1;
        } else {
            assert_eq!(out, src, "visible pixel {index} changed");
            checked_visible += 1;
        }
    }

    // The sample is built with soft transparent edges, so both branches must
    // actually have been exercised.
    assert!(checked_visible > 0 && checked_transparent > 0);
}

#[test]
fn colour_under_zero_alpha_is_dropped_from_the_composite() {
    // sample.png has grid lines running through its fully transparent border,
    // i.e. pixels with alpha 0 but non-zero RGB. This pins the normalisation so
    // it cannot change silently.
    let decoded = png::read(&sample("sample.png")).unwrap();
    let hidden_colour = decoded
        .pixels
        .chunks_exact(4)
        .filter(|p| p[3] == 0 && (p[0] > 0 || p[1] > 0 || p[2] > 0))
        .count();
    assert!(hidden_colour > 0, "sample.png no longer exercises this case");

    let composite = flatten(&document_from("sample.png"));
    assert!(composite
        .pixels
        .chunks_exact(4)
        .all(|p| p[3] != 0 || p == [0, 0, 0, 0]));
}

#[test]
fn hiding_the_top_layer_restores_the_layer_below() {
    let mut document = document_from("sample.png");
    let baseline = flatten(&document).pixels;

    let rings = push_layer(&mut document, "rings.png");
    assert_ne!(flatten(&document).pixels, baseline, "adding a layer changed nothing");

    document.set_visible(rings, false).unwrap();
    assert_eq!(flatten(&document).pixels, baseline);
}

#[test]
fn zero_opacity_matches_hidden() {
    let mut document = document_from("sample.png");
    let rings = push_layer(&mut document, "rings.png");

    document.set_visible(rings, false).unwrap();
    let hidden = flatten(&document).pixels;

    document.set_visible(rings, true).unwrap();
    document.set_opacity(rings, 0.0).unwrap();
    assert_eq!(flatten(&document).pixels, hidden);
}

#[test]
fn each_blend_mode_produces_a_distinct_composite() {
    let mut document = document_from("sample.png");
    let rings = push_layer(&mut document, "rings.png");

    let mut seen: Vec<(BlendMode, Vec<u8>)> = Vec::new();
    for mode in BlendMode::ALL {
        document.set_blend_mode(rings, mode).unwrap();
        let pixels = flatten(&document).pixels;
        if let Some((other, _)) = seen.iter().find(|(_, other)| *other == pixels) {
            panic!("{mode:?} produced the same composite as {other:?}");
        }
        seen.push((mode, pixels));
    }
    assert_eq!(seen.len(), BlendMode::ALL.len());
}

#[test]
fn the_composite_encodes_to_a_png_data_url_that_decodes_back() {
    let mut document = document_from("sample.png");
    let rings = push_layer(&mut document, "rings.png");
    document.set_blend_mode(rings, BlendMode::Multiply).unwrap();
    document.set_opacity(rings, 0.75).unwrap();

    let composite = flatten(&document);
    let url = png::to_data_url(&composite).unwrap();
    assert!(url.starts_with("data:image/png;base64,"));

    // Write it out and read it back through the same decoder the app uses.
    let raw = base64_decode(url.trim_start_matches("data:image/png;base64,"));
    let path = std::env::temp_dir().join("pipeline_composite.png");
    std::fs::write(&path, raw).unwrap();

    let reread = png::read(&path).unwrap();
    assert_eq!((reread.width, reread.height), (640, 400));
    assert_eq!(reread.pixels, composite.pixels);
}

/// Minimal standard-alphabet base64 decoder, so the test does not depend on the
/// crate whose output it is checking.
fn base64_decode(input: &str) -> Vec<u8> {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    let mut accumulator: u32 = 0;
    let mut bits = 0;
    for byte in input.bytes().filter(|b| *b != b'=' && !b.is_ascii_whitespace()) {
        let value = ALPHABET
            .iter()
            .position(|c| *c == byte)
            .unwrap_or_else(|| panic!("unexpected base64 byte {byte:?}")) as u32;
        accumulator = (accumulator << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
        }
    }
    out
}
