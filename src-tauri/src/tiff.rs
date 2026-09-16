//! Image > Mode > 32 Bits/Channel's own real, distinguishing export
//! target: a genuine 32-bit-float-per-channel TIFF. PNG (this project's
//! only other export format) cannot hold float samples at all — a real
//! limitation of the format, not of this project — so 32-bit output
//! needs a real format that can, and TIFF is the one the `image` crate
//! this project already depends on actually supports encoding as such.

use std::io::Cursor;

use image::{ExtendedColorType, ImageEncoder};

/// Encodes `pixels` (RGBA8, byte per channel — the same shape every other
/// export path in this project takes) as a real 32-bit-float-per-channel
/// TIFF: each byte normalized to `0.0..=1.0` (`byte / 255.0`) and written
/// as an IEEE 754 `f32`, the standard way real float-capable image
/// formats represent 8-bit source data before any further grading — not
/// a bit-replication trick the way [`crate::png::encode_pixels_16`]'s own
/// 16-bit widening is, since a float sample has no fixed bit width to
/// replicate into.
pub fn encode_pixels_32f(width: u32, height: u32, pixels: &[u8]) -> Result<Vec<u8>, String> {
    let floats: Vec<u8> = pixels
        .iter()
        .flat_map(|&byte| (f32::from(byte) / 255.0).to_le_bytes())
        .collect();

    let mut buffer = Vec::new();
    let encoder = image::codecs::tiff::TiffEncoder::new(Cursor::new(&mut buffer));
    encoder
        .write_image(&floats, width, height, ExtendedColorType::Rgba32F)
        .map_err(|err| format!("Could not encode the image: {err}"))?;

    Ok(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_pixels_32f_normalizes_every_byte_exactly_and_is_a_real_float_tiff() {
        let pixels = vec![0u8, 64, 128, 255];
        let bytes = encode_pixels_32f(1, 1, &pixels).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!(decoded.color(), image::ColorType::Rgba32F);
        let rgba32f = decoded.into_rgba32f();
        let px = rgba32f.get_pixel(0, 0).0;
        // 0/255=0.0, 64/255, 128/255, 255/255=1.0 -- hand-computed, f32
        // precision, matching what byte/255.0 actually produces in Rust.
        assert_eq!(px[0], 0.0);
        assert!((px[1] - 64.0 / 255.0).abs() < 1e-7);
        assert!((px[2] - 128.0 / 255.0).abs() < 1e-7);
        assert_eq!(px[3], 1.0);
    }

    #[test]
    fn encode_pixels_32f_round_trips_a_two_pixel_image_correctly() {
        let pixels = vec![255u8, 0, 0, 255, 0, 255, 0, 128];
        let bytes = encode_pixels_32f(2, 1, &pixels).unwrap();
        let decoded = image::load_from_memory(&bytes).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (2, 1));
        let rgba32f = decoded.into_rgba32f();
        assert_eq!(rgba32f.get_pixel(0, 0).0, [1.0, 0.0, 0.0, 1.0]);
        let second = rgba32f.get_pixel(1, 0).0;
        assert_eq!(second[0], 0.0);
        assert_eq!(second[1], 1.0);
        assert_eq!(second[2], 0.0);
        assert!((second[3] - 128.0 / 255.0).abs() < 1e-7);
    }
}
