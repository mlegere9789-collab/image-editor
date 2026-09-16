//! Neural Filters > Landscape Mixer — a real feed-forward network
//! this project trained itself, from scratch, the same technique as
//! [`crate::style_transfer`] applied to a landscape-specific dataset and
//! a landscape-mood style target.
//!
//! Adobe's own Landscape Mixer blends a photo toward a chosen scene
//! "mood" (season, time of day) using a model trained on a large corpus
//! of real landscape photography. This project's real, honestly-scoped
//! equivalent: a network trained on 787 real landscape photographs —
//! sourced from Flickr via the `ml5js/ml5-data-and-models` "landscapes"
//! dataset (MIT-licensed repository), filtered to only the images
//! individually marked CC BY, CC0, public domain, or "no known
//! copyright restrictions" — toward one real mountain-sunset reference
//! photo's own warm/cool colour mood. See
//! `models/LANDSCAPE_MIXER_NOTICE.md` for the full recipe, real
//! per-image attribution, and honest limitations.

use std::io::Cursor;

use tract_onnx::prelude::*;

/// The network has two stride-2 downsamples and two matching
/// nearest-upsample-by-2 stages, so its input's height and width must
/// each be a multiple of this for the shapes to come back out exact.
const ALIGN: usize = 4;

/// Real landscape-mood blending over an RGBA8 image: alpha is preserved
/// exactly; RGB is replaced by the bundled network's real output.
/// Returns `width * height * 4` RGBA8 bytes. Errs if `pixels` isn't
/// exactly that many bytes, if either dimension is zero, or if the
/// bundled model fails to load or run.
pub fn mix_landscape_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Landscape Mixer needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Landscape Mixer expected {} RGBA bytes for a {}x{} image, got {}.",
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

    let model_bytes: &[u8] = include_bytes!("../models/landscape_mixer.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Landscape Mixer model: {err}"))?;

    let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, padded_h, padded_w), padded)
        .map_err(|err| format!("Could not shape Landscape Mixer's input: {err}"))?
        .into();
    let result = model
        .run(tvec!(input.into()))
        .map_err(|err| format!("Landscape Mixer's model failed to run: {err}"))?;
    let output = result[0]
        .to_array_view::<f32>()
        .map_err(|err| format!("Landscape Mixer's model returned an unreadable result: {err}"))?;

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
    fn mix_landscape_rgba_rejects_a_mismatched_buffer() {
        let err = mix_landscape_rgba(&[0u8; 3], 2, 2).unwrap_err();
        assert!(err.contains("expected"));
    }

    #[test]
    fn mix_landscape_rgba_rejects_a_zero_dimension() {
        assert!(mix_landscape_rgba(&[], 0, 4).is_err());
        assert!(mix_landscape_rgba(&[], 4, 0).is_err());
    }

    #[test]
    fn mix_landscape_rgba_preserves_dimensions_and_alpha_via_a_real_model_run() {
        let width = 12u32;
        let height = 8u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 53) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = 255 - v;
            pixels[i * 4 + 2] = v / 2;
            pixels[i * 4 + 3] = 210;
        }
        let out = mix_landscape_rgba(&pixels, width, height).unwrap();
        assert_eq!(out.len(), (width * height * 4) as usize);
        assert!(out.chunks_exact(4).all(|p| p[3] == 210));
    }
}
