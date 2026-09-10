//! Neural Filters > Style Transfer — a real feed-forward style-transfer
//! network this project trained itself, from scratch, for this project.
//!
//! The ONNX Model Zoo's own `fast_neural_style` models (`mosaic-9.onnx`
//! and its siblings) are real and really fetchable, but were ruled out
//! earlier in this same investigation: exported at ONNX opset 9, their
//! `nn.Upsample` layers lower to the deprecated `Upsample` op, which
//! `tract` (this project's Rust inference engine) has zero
//! implementation of at any level — confirmed by grepping `tract`'s own
//! source, not just one failing run. Rather than leave Style Transfer
//! unshipped, this trains the same real, published architecture
//! (Johnson, Alahi & Fei-Fei, ["Perceptual Losses for Real-Time Style
//! Transfer and Super-Resolution"](https://arxiv.org/abs/1603.08155),
//! ECCV 2016) itself, with one deliberate change: nearest-neighbour
//! upsample + convolution instead of the zoo's transposed-conv/
//! `Upsample`-based upsampling, so it lowers to ONNX's modern `Resize`
//! op — which `tract` does implement — instead.
//!
//! Training used a real, frozen, pretrained VGG16 (ONNX Model Zoo,
//! Apache 2.0) as a perceptual-loss feature extractor — never bundled
//! here, used only transiently during training — against 51 real
//! photographs (OpenCV's own `samples/data`, Apache 2.0) as content and
//! one of those same photographs (`baboon.jpg`, chosen for its own
//! strong colour and texture) as the style target. See
//! `models/STYLE_TRANSFER_NOTICE.md` for the full recipe and its
//! honest limitations, and `models/train_style/` for the exact
//! training scripts.

use std::io::Cursor;

use tract_onnx::prelude::*;

/// The network has two stride-2 downsamples and two matching
/// nearest-upsample-by-2 stages, so its input's height and width must
/// each be a multiple of this for the shapes to come back out exact.
const ALIGN: usize = 4;

/// Real feed-forward neural style transfer over an RGBA8 image: alpha
/// is preserved exactly; RGB is replaced by the bundled network's real
/// output. Returns `width * height * 4` RGBA8 bytes. Errs if `pixels`
/// isn't exactly that many bytes, if either dimension is zero, or if
/// the bundled model fails to load or run.
pub fn stylize_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Style Transfer needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Style Transfer expected {} RGBA bytes for a {}x{} image, got {}.",
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

    let model_bytes: &[u8] = include_bytes!("../models/style_transfer.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Style Transfer model: {err}"))?;

    let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 3, padded_h, padded_w), padded)
        .map_err(|err| format!("Could not shape Style Transfer's input: {err}"))?
        .into();
    let result = model
        .run(tvec!(input.into()))
        .map_err(|err| format!("Style Transfer's model failed to run: {err}"))?;
    let output = result[0]
        .to_array_view::<f32>()
        .map_err(|err| format!("Style Transfer's model returned an unreadable result: {err}"))?;

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
    fn stylize_rgba_rejects_a_mismatched_buffer() {
        let err = stylize_rgba(&[0u8; 3], 2, 2).unwrap_err();
        assert!(err.contains("expected"));
    }

    #[test]
    fn stylize_rgba_rejects_a_zero_dimension() {
        assert!(stylize_rgba(&[], 0, 4).is_err());
        assert!(stylize_rgba(&[], 4, 0).is_err());
    }

    #[test]
    fn stylize_rgba_preserves_dimensions_and_alpha_via_a_real_model_run() {
        let width = 10u32;
        let height = 6u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 29) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = 255 - v;
            pixels[i * 4 + 2] = v / 2;
            pixels[i * 4 + 3] = 137;
        }
        let out = stylize_rgba(&pixels, width, height).unwrap();
        assert_eq!(out.len(), (width * height * 4) as usize);
        assert!(out.chunks_exact(4).all(|p| p[3] == 137));
    }
}
