//! Neural Filters > Photo Restoration — a real convolutional network
//! this project trained itself, from scratch, for this project.
//!
//! A real, official, permissively-licensed pretrained candidate for
//! this exact item was found and confirmed feasible earlier in this
//! same investigation — Xintao Wang's `RealESRGAN_x4plus.pth`, real
//! weights, real BSD-3-Clause license, an architecture that would
//! export cleanly to `tract`-compatible ONNX — but converting that
//! downloaded `.pth` checkpoint required running `torch.load` on it,
//! which Claude Code's own auto-mode security classifier refused
//! (executing a downloaded model checkpoint is a real risk category —
//! PyTorch's pickle format can run arbitrary code during
//! deserialization — independent of this specific file's verified
//! provenance). Rather than leave the item unshipped, this trains a
//! smaller network itself instead, the same way Colorize and Style
//! Transfer already do.
//!
//! `TinyRestorer` predicts a correction on top of a degraded image
//! (residual learning, standard in the real image-restoration
//! literature) — trained against 51 real photographs run through a
//! real degradation pipeline (Gaussian blur, additive Gaussian noise,
//! real JPEG re-encoding at a random low quality). See
//! `models/RESTORATION_NOTICE.md` for the full recipe and its honest
//! limitations.

use std::io::Cursor;

use tract_onnx::prelude::*;

/// The network has two stride-2 downsamples with additive skip
/// connections, so its input's height and width must each be a
/// multiple of this for the skip connections to line up exactly.
const ALIGN: usize = 4;

/// Real photo restoration of an RGBA8 image: alpha is preserved
/// exactly; RGB is replaced by the bundled network's real correction.
/// Returns `width * height * 4` RGBA8 bytes. Errs if `pixels` isn't
/// exactly that many bytes, if either dimension is zero, or if the
/// bundled model fails to load or run.
pub fn restore_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Photo Restoration needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Photo Restoration expected {} RGBA bytes for a {}x{} image, got {}.",
            width * height * 4,
            width,
            height,
            pixels.len()
        ));
    }

    let padded_w = width.div_ceil(ALIGN) * ALIGN;
    let padded_h = height.div_ceil(ALIGN) * ALIGN;
    let mut padded = vec![0f32; padded_w * padded_h * 3];
    for y in 0..padded_h {
        let sy = y.min(height - 1);
        for x in 0..padded_w {
            let sx = x.min(width - 1);
            let src = (sy * width + sx) * 4;
            for c in 0..3 {
                padded[(c * padded_h + y) * padded_w + x] = pixels[src + c] as f32 / 255.0;
            }
        }
    }

    let model_bytes: &[u8] = include_bytes!("../models/tiny_restorer.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Photo Restoration model: {err}"))?;

    let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, padded_h, padded_w), padded)
        .map_err(|err| format!("Could not shape Photo Restoration's input: {err}"))?
        .into();
    let result = model
        .run(tvec!(input.into()))
        .map_err(|err| format!("Photo Restoration's model failed to run: {err}"))?;
    let output = result[0]
        .to_array_view::<f32>()
        .map_err(|err| format!("Photo Restoration's model returned an unreadable result: {err}"))?;

    let mut out = vec![0u8; width * height * 4];
    for y in 0..height {
        for x in 0..width {
            let src_a = (y * width + x) * 4 + 3;
            let dst = (y * width + x) * 4;
            for c in 0..3 {
                let v = output[[0, c, y, x]] * 255.0;
                out[dst + c] = v.round().clamp(0.0, 255.0) as u8;
            }
            out[dst + 3] = pixels[src_a];
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restore_rgba_rejects_a_mismatched_buffer() {
        let err = restore_rgba(&[0u8; 3], 2, 2).unwrap_err();
        assert!(err.contains("expected"));
    }

    #[test]
    fn restore_rgba_rejects_a_zero_dimension() {
        assert!(restore_rgba(&[], 0, 4).is_err());
        assert!(restore_rgba(&[], 4, 0).is_err());
    }

    #[test]
    fn restore_rgba_preserves_dimensions_and_alpha_via_a_real_model_run() {
        let width = 11u32;
        let height = 5u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 41) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = 255 - v;
            pixels[i * 4 + 2] = v / 3;
            pixels[i * 4 + 3] = 91;
        }
        let out = restore_rgba(&pixels, width, height).unwrap();
        assert_eq!(out.len(), (width * height * 4) as usize);
        assert!(out.chunks_exact(4).all(|p| p[3] == 91));
    }
}
