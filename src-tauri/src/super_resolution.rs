//! Neural Filters > Super Zoom (real on-device AI upscale): a real,
//! pretrained convolutional neural network — the sub-pixel CNN from Shi et
//! al., ["Real-Time Single Image and Video Super-Resolution Using an
//! Efficient Sub-Pixel Convolutional Neural Network"](https://arxiv.org/abs/1609.05158)
//! (CVPR 2016), published to the [ONNX Model
//! Zoo](https://github.com/onnx/models) under Apache License 2.0 and
//! bundled here at `models/super-resolution-10.onnx` (see
//! `models/NOTICE.md` for its full provenance) — run entirely on-device
//! through [`tract`](https://github.com/sonos/tract), a pure-Rust ONNX
//! inference engine. No network call, no hosted backend, no native binary
//! download: the model's weights ship inside this binary
//! (`include_bytes!`) and inference runs on the CPU that is already
//! running this process.
//!
//! The network itself, per its own published design, takes only a
//! single-channel luma (Y) plane and produces a real 3x upscale of it via
//! sub-pixel (pixel-shuffle) convolution. Chroma (Cb/Cr) was never part of
//! its training, so — matching the model's own published reference
//! post-processing — this module upsamples Cb, Cr, and alpha separately
//! with a real bicubic (Catmull-Rom) resampler and recombines all four
//! planes back into RGBA.
//!
//! The network's own input size is fixed at 224x224, so a larger image is
//! tiled into non-overlapping 224x224 blocks (the source is edge-padded up
//! to a whole multiple of 224 first), each tile is run through the network
//! independently, and the real 672x672 outputs are stitched back into one
//! full-resolution 3x luma plane, then cropped to the exact `3 * width` by
//! `3 * height` the caller asked for.

use std::io::Cursor;

use tract_onnx::prelude::*;

/// The network's fixed square input size.
const TILE: usize = 224;
/// The network's own upscale factor (a property of this specific model,
/// not a caller-chosen parameter).
pub const SCALE: usize = 3;

/// Real 3x AI super-resolution of an RGBA8 image. Returns the new
/// `(width * SCALE, height * SCALE)` RGBA8 pixels. Errs if `pixels` isn't
/// exactly `width * height * 4` bytes, if either dimension is zero, or if
/// the bundled model fails to load or run (it never should — a broken
/// build would fail every call identically, not just this one).
pub fn upscale_rgba(pixels: &[u8], width: u32, height: u32) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Super Zoom needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Super Zoom expected {} RGBA bytes for a {}x{} image, got {}.",
            width * height * 4,
            width,
            height,
            pixels.len()
        ));
    }

    let mut y_plane = vec![0f32; width * height];
    let mut cb_plane = vec![0f32; width * height];
    let mut cr_plane = vec![0f32; width * height];
    let mut a_plane = vec![0f32; width * height];
    for i in 0..width * height {
        let (r, g, b, a) = (
            pixels[i * 4] as f32,
            pixels[i * 4 + 1] as f32,
            pixels[i * 4 + 2] as f32,
            pixels[i * 4 + 3] as f32,
        );
        let (y, cb, cr) = rgb_to_ycbcr(r, g, b);
        y_plane[i] = y;
        cb_plane[i] = cb;
        cr_plane[i] = cr;
        a_plane[i] = a;
    }

    let out_width = width * SCALE;
    let out_height = height * SCALE;
    let y_out = upscale_luma_tiled(&y_plane, width, height)?;
    let cb_out = bicubic_upsample_plane(&cb_plane, width, height, SCALE);
    let cr_out = bicubic_upsample_plane(&cr_plane, width, height, SCALE);
    let a_out = bicubic_upsample_plane(&a_plane, width, height, SCALE);

    let mut out = vec![0u8; out_width * out_height * 4];
    for i in 0..out_width * out_height {
        let (r, g, b) = ycbcr_to_rgb(y_out[i], cb_out[i], cr_out[i]);
        out[i * 4] = r;
        out[i * 4 + 1] = g;
        out[i * 4 + 2] = b;
        out[i * 4 + 3] = a_out[i].round().clamp(0.0, 255.0) as u8;
    }
    Ok(out)
}

/// Real BT.601 (JPEG full-range) RGB -> YCbCr, all three outputs
/// `0.0..=1.0`. The reciprocals of [`ycbcr_to_rgb`]'s own multipliers, so
/// the two round-trip up to floating-point rounding — proven by this
/// module's own `ycbcr_round_trip_recovers_the_original_rgb` test.
fn rgb_to_ycbcr(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    let luma = 0.299 * r + 0.587 * g + 0.114 * b;
    let y = luma / 255.0;
    let cb = 0.5 + (b - luma) / 1.772 / 255.0;
    let cr = 0.5 + (r - luma) / 1.402 / 255.0;
    (y, cb, cr)
}

/// The inverse of [`rgb_to_ycbcr`]: real BT.601 YCbCr (each `0.0..=1.0`)
/// back to clamped RGB8.
fn ycbcr_to_rgb(y: f32, cb: f32, cr: f32) -> (u8, u8, u8) {
    let y = y * 255.0;
    let cb = (cb - 0.5) * 255.0;
    let cr = (cr - 0.5) * 255.0;
    let r = y + 1.402 * cr;
    let g = y - 0.344136 * cb - 0.714136 * cr;
    let b = y + 1.772 * cb;
    (
        r.round().clamp(0.0, 255.0) as u8,
        g.round().clamp(0.0, 255.0) as u8,
        b.round().clamp(0.0, 255.0) as u8,
    )
}

/// Runs `y_plane` (real `width * height` luma samples, `0.0..=1.0`)
/// through the bundled network one `TILE * TILE` block at a time and
/// stitches the real outputs into one `width * SCALE` by `height * SCALE`
/// plane.
fn upscale_luma_tiled(y_plane: &[f32], width: usize, height: usize) -> Result<Vec<f32>, String> {
    let padded_w = width.div_ceil(TILE) * TILE;
    let padded_h = height.div_ceil(TILE) * TILE;

    let model_bytes: &[u8] = include_bytes!("../models/super-resolution-10.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Super Zoom model: {err}"))?;

    let out_width = width * SCALE;
    let out_height = height * SCALE;
    let mut stitched = vec![0f32; out_width * out_height];

    let mut ty = 0;
    while ty < padded_h {
        let mut tx = 0;
        while tx < padded_w {
            let mut block = vec![0f32; TILE * TILE];
            for y in 0..TILE {
                let sy = (ty + y).min(height - 1);
                for x in 0..TILE {
                    let sx = (tx + x).min(width - 1);
                    block[y * TILE + x] = y_plane[sy * width + sx];
                }
            }
            let input: Tensor = tract_ndarray::Array4::from_shape_vec((1, 1, TILE, TILE), block)
                .map_err(|err| format!("Could not shape Super Zoom's input tile: {err}"))?
                .into();
            let result = model
                .run(tvec!(input.into()))
                .map_err(|err| format!("Super Zoom's model failed to run: {err}"))?;
            let output = result[0].to_array_view::<f32>().map_err(|err| {
                format!("Super Zoom's model returned an unreadable result: {err}")
            })?;

            let dst_ty = ty * SCALE;
            let dst_tx = tx * SCALE;
            for y in 0..TILE * SCALE {
                let dy = dst_ty + y;
                if dy >= out_height {
                    continue;
                }
                for x in 0..TILE * SCALE {
                    let dx = dst_tx + x;
                    if dx >= out_width {
                        continue;
                    }
                    stitched[dy * out_width + dx] = output[[0, 0, y, x]];
                }
            }
            tx += TILE;
        }
        ty += TILE;
    }
    Ok(stitched)
}

/// The Catmull-Rom cubic convolution kernel (`a = -0.5`), the same real
/// bicubic weighting most image resamplers use.
fn cubic_weight(t: f32) -> f32 {
    const A: f32 = -0.5;
    let t = t.abs();
    if t <= 1.0 {
        (A + 2.0) * t.powi(3) - (A + 3.0) * t.powi(2) + 1.0
    } else if t < 2.0 {
        A * t.powi(3) - 5.0 * A * t.powi(2) + 8.0 * A * t - 4.0 * A
    } else {
        0.0
    }
}

/// Samples `values` at real position `x` (which may be fractional or
/// outside `0..values.len()`) via real bicubic (four-tap Catmull-Rom)
/// interpolation, clamping taps to the array's own edges.
fn sample_cubic_1d(values: &[f32], x: f32) -> f32 {
    let len = values.len() as isize;
    let x0 = x.floor() as isize;
    let frac = x - x0 as f32;
    let mut total = 0.0f32;
    for k in -1..=2 {
        let idx = (x0 + k).clamp(0, len - 1) as usize;
        total += values[idx] * cubic_weight(k as f32 - frac);
    }
    total
}

/// Real separable bicubic upsampling of a `width * height` plane by
/// `scale`: each row is resampled horizontally first, then each column of
/// that intermediate result is resampled vertically. Output position `d`
/// maps back to source position `(d + 0.5) / scale - 0.5` (pixel-centre
/// alignment), so the very first and last source samples are not
/// duplicated at the edges of a scaled-up plane.
fn bicubic_upsample_plane(plane: &[f32], width: usize, height: usize, scale: usize) -> Vec<f32> {
    let out_width = width * scale;
    let out_height = height * scale;

    let mut horizontal = vec![0f32; out_width * height];
    for y in 0..height {
        let row = &plane[y * width..(y + 1) * width];
        for x in 0..out_width {
            let sx = (x as f32 + 0.5) / scale as f32 - 0.5;
            horizontal[y * out_width + x] = sample_cubic_1d(row, sx);
        }
    }

    let mut out = vec![0f32; out_width * out_height];
    let mut column = vec![0f32; height];
    for x in 0..out_width {
        for (y, slot) in column.iter_mut().enumerate() {
            *slot = horizontal[y * out_width + x];
        }
        for y in 0..out_height {
            let sy = (y as f32 + 0.5) / scale as f32 - 0.5;
            out[y * out_width + x] = sample_cubic_1d(&column, sy);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cubic_weight_is_one_at_zero_and_zero_at_and_beyond_two() {
        assert!((cubic_weight(0.0) - 1.0).abs() < 1e-6);
        assert!(cubic_weight(2.0).abs() < 1e-6);
        assert!(cubic_weight(3.0).abs() < 1e-6);
    }

    #[test]
    fn sample_cubic_1d_matches_an_independently_hand_computed_3x_upsample() {
        // Independently computed in Python with the same Catmull-Rom
        // kernel (a = -0.5) and the same (d + 0.5)/scale - 0.5 source
        // mapping: values [10, 20, 30, 40] upsampled 3x.
        let values = [10.0f32, 20.0, 30.0, 40.0];
        let expected = [
            9.2593, 10.0000, 12.5926, 16.2963, 20.0000, 23.3333, 26.6667, 30.0000, 33.7037,
            37.4074, 40.0000, 40.7407,
        ];
        for (i, &want) in expected.iter().enumerate() {
            let x = (i as f32 + 0.5) / 3.0 - 0.5;
            let got = sample_cubic_1d(&values, x);
            assert!(
                (got - want).abs() < 1e-3,
                "index {i}: got {got}, want {want}"
            );
        }
    }

    #[test]
    fn bicubic_upsample_of_a_constant_plane_stays_constant() {
        let plane = vec![42.0f32; 4 * 4];
        let out = bicubic_upsample_plane(&plane, 4, 4, 3);
        assert_eq!(out.len(), 12 * 12);
        assert!(out.iter().all(|&v| (v - 42.0).abs() < 1e-4));
    }

    #[test]
    fn bicubic_upsample_passes_through_original_samples_at_their_own_centres() {
        // At output index i = scale*j + (scale-1)/2 for odd scale, the
        // source mapping (i + 0.5)/scale - 0.5 lands exactly on integer j,
        // so the resampled value should equal the original sample exactly
        // (interior points, away from edge clamping).
        let values = [1.0f32, 5.0, 9.0, 2.0, 7.0];
        let plane: Vec<f32> = values.to_vec();
        let out = bicubic_upsample_plane(&plane, 5, 1, 3);
        for (j, &want) in values.iter().enumerate() {
            let i = 3 * j + 1;
            assert!(
                (out[i] - want).abs() < 1e-3,
                "j {j}: got {}, want {want}",
                out[i]
            );
        }
    }

    #[test]
    fn ycbcr_round_trip_recovers_the_original_rgb() {
        // Independently recomputed in Python with the same constants;
        // every case round-trips exactly (to the nearest integer).
        for &(r, g, b) in &[
            (200u8, 50u8, 30u8),
            (0, 0, 0),
            (255, 255, 255),
            (10, 240, 60),
            (128, 128, 128),
        ] {
            let (y, cb, cr) = rgb_to_ycbcr(r as f32, g as f32, b as f32);
            let (r2, g2, b2) = ycbcr_to_rgb(y, cb, cr);
            assert_eq!((r, g, b), (r2, g2, b2));
        }
    }

    #[test]
    fn upscale_rgba_rejects_a_mismatched_buffer() {
        let err = upscale_rgba(&[0u8; 3], 2, 2).unwrap_err();
        assert!(err.contains("expected"));
    }

    #[test]
    fn upscale_rgba_rejects_a_zero_dimension() {
        assert!(upscale_rgba(&[], 0, 4).is_err());
        assert!(upscale_rgba(&[], 4, 0).is_err());
    }

    #[test]
    fn upscale_rgba_produces_a_real_3x_image_from_a_real_model_run() {
        // Small enough to stay well under one 224x224 tile, so this test
        // exercises one real inference call end to end, not a mocked one.
        let width = 8u32;
        let height = 6u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 37) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = 255 - v;
            pixels[i * 4 + 2] = v / 2;
            pixels[i * 4 + 3] = 255;
        }
        let out = upscale_rgba(&pixels, width, height).unwrap();
        assert_eq!(
            out.len(),
            (width as usize * SCALE) * (height as usize * SCALE) * 4
        );
        // Alpha was fully opaque everywhere, and bicubic-upsampling a
        // constant plane stays constant (proven above), so it should
        // still read fully opaque.
        assert!(out.chunks_exact(4).all(|p| p[3] == 255));
    }
}
