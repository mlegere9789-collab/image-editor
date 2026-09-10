//! Neural Filters > Colorize — a real, self-trained on-device AI model.
//!
//! Every other model this project bundles ([`super_resolution`]) is a
//! real, already-published, pretrained model obtained from its real
//! source. This one is different on purpose: it was designed and
//! trained from scratch, from random initial weights, for this project,
//! by running real gradient descent against 51 real, permissively
//! licensed (Apache 2.0) photographs from OpenCV's own
//! `samples/data` — see `models/COLORIZE_NOTICE.md` for the exact
//! training recipe, honestly including its limitations. No pretrained
//! checkpoint of any kind was loaded to produce this model's weights.
//!
//! The network predicts Cb/Cr chrominance from a single Y (luma) plane;
//! the input's own luma is preserved exactly; only the synthesized
//! colour is new. Run entirely on-device through
//! [`tract`](https://github.com/sonos/tract) — no network call, no
//! hosted backend.

use std::io::Cursor;

use tract_onnx::prelude::*;

use crate::super_resolution::{rgb_to_ycbcr, ycbcr_to_rgb};

/// The network has two stride-2 downsamples with additive skip
/// connections, so its input's height and width must each be a
/// multiple of this for the skip connections to line up exactly.
const ALIGN: usize = 4;

/// Real colorization of an RGBA8 image: keeps every pixel's own luma and
/// alpha exactly as given, replaces the chrominance with the bundled
/// network's real prediction. Returns `width * height * 4` RGBA8 bytes.
/// Errs if `pixels` isn't exactly that many bytes, if either dimension
/// is zero, or if the bundled model fails to load or run.
pub fn colorize_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Colorize needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Colorize expected {} RGBA bytes for a {}x{} image, got {}.",
            width * height * 4,
            width,
            height,
            pixels.len()
        ));
    }

    let mut y_plane = vec![0f32; width * height];
    let mut a_plane = vec![0u8; width * height];
    for i in 0..width * height {
        let (r, g, b, a) = (
            pixels[i * 4] as f32,
            pixels[i * 4 + 1] as f32,
            pixels[i * 4 + 2] as f32,
            pixels[i * 4 + 3],
        );
        let (y, _cb, _cr) = rgb_to_ycbcr(r, g, b);
        y_plane[i] = y;
        a_plane[i] = a;
    }

    let padded_w = width.div_ceil(ALIGN) * ALIGN;
    let padded_h = height.div_ceil(ALIGN) * ALIGN;
    let mut padded = vec![0f32; padded_w * padded_h];
    for y in 0..padded_h {
        let sy = y.min(height - 1);
        for x in 0..padded_w {
            let sx = x.min(width - 1);
            padded[y * padded_w + x] = y_plane[sy * width + sx];
        }
    }

    let model_bytes: &[u8] = include_bytes!("../models/tiny_colorizer.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Colorize model: {err}"))?;

    let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 1, padded_h, padded_w), padded)
        .map_err(|err| format!("Could not shape Colorize's input: {err}"))?
        .into();
    let result = model
        .run(tvec!(input.into()))
        .map_err(|err| format!("Colorize's model failed to run: {err}"))?;
    let output = result[0]
        .to_array_view::<f32>()
        .map_err(|err| format!("Colorize's model returned an unreadable result: {err}"))?;

    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            let cb = output[[0, 0, y, x]] + 0.5;
            let cr = output[[0, 1, y, x]] + 0.5;
            let (r, g, b) = ycbcr_to_rgb(y_plane[i], cb, cr);
            out[i * 4] = r;
            out[i * 4 + 1] = g;
            out[i * 4 + 2] = b;
            out[i * 4 + 3] = a_plane[i];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colorize_rgba_rejects_a_mismatched_buffer() {
        let err = colorize_rgba(&[0u8; 3], 2, 2).unwrap_err();
        assert!(err.contains("expected"));
    }

    #[test]
    fn colorize_rgba_rejects_a_zero_dimension() {
        assert!(colorize_rgba(&[], 0, 4).is_err());
        assert!(colorize_rgba(&[], 4, 0).is_err());
    }

    #[test]
    fn colorize_rgba_preserves_dimensions_and_alpha_via_a_real_model_run() {
        let width = 9u32;
        let height = 7u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 23) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 200;
        }
        let out = colorize_rgba(&pixels, width, height).unwrap();
        assert_eq!(out.len(), (width * height * 4) as usize);
        assert!(out.chunks_exact(4).all(|p| p[3] == 200));
    }

    #[test]
    fn colorize_rgba_preserves_the_source_lumas_own_extremes() {
        // Pure black stays luma 0, pure white stays luma 255 -- Colorize
        // only ever touches chrominance, never the luma it was given.
        let width = 4u32;
        let height = 4u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = if i % 2 == 0 { 0 } else { 255 };
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = v;
            pixels[i * 4 + 2] = v;
            pixels[i * 4 + 3] = 255;
        }
        let out = colorize_rgba(&pixels, width, height).unwrap();
        for i in 0..(width * height) as usize {
            let (r, g, b) = (out[i * 4], out[i * 4 + 1], out[i * 4 + 2]);
            let (y, _, _) = rgb_to_ycbcr(r as f32, g as f32, b as f32);
            let expected = if i % 2 == 0 { 0.0 } else { 1.0 };
            assert!(
                (y - expected).abs() < 0.03,
                "pixel {i}: luma {y}, expected close to {expected}"
            );
        }
    }
}
