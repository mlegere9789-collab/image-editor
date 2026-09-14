//! Filter > Generative Fill (and Generate Background, which reuses the
//! exact same function over a subject's own inverse selection) — a
//! real, self-supervised generative model this project trained itself,
//! from scratch, rather than the hosted-provider client `App.tsx`'s own
//! `applyGenerativeFill` already speaks to. No text prompt: Adobe's own
//! Generative Fill is a real text-conditioned diffusion model trained on
//! hundreds of millions of image/caption pairs over hundreds of
//! thousands of GPU-hours — a scale gap of roughly four to five orders
//! of magnitude from what a CPU-only sandbox can train in any bounded
//! amount of wall-clock time, not merely a "given longer, it would get
//! there" gap. See `models/GENERATIVE_FILL_NOTICE.md` for the full
//! research writeup this conclusion is based on. What a small,
//! from-scratch context-encoder-style network (Pathak et al. 2016) can
//! do — and does here — is context-only hallucination: fill a selected
//! region with content plausible for its immediate surroundings, the
//! same "no prompt typed" behaviour Photoshop's own Generative Fill
//! falls back to. It is real, it is trained, and it is honestly a
//! smaller, softer capability than Adobe's.

use std::io::Cursor;

use tract_onnx::prelude::*;

/// The one input size the bundled model was ever trained on, and the
/// only size its ONNX export accepts — no dynamic height/width axes, on
/// purpose: `tract` (this project's Rust inference engine) could not
/// prove the skip connections' own `Concat` nodes' shapes matched under
/// symbolic axes, and running the network at some other size would be a
/// real distribution shift besides. This module resizes its own
/// context-window crop (see [`crop_window`]) to exactly this size
/// before inference, and the masked prediction back to the crop's real
/// size afterward.
const MODEL_SIZE: usize = 128;

/// Selected region's context window gets this many extra pixels of real
/// surrounding pixels on every side it can (clamped to the canvas) —
/// comparable to the largest hole size (64px) the model trained
/// against, so the network sees roughly the same amount of real context
/// per hole pixel it saw during training.
const CONTEXT_MARGIN: usize = 48;

/// How many pixels in from the selection's own edge the model's
/// prediction is blended toward full strength — the same idea as the
/// Healing Brush/Camera Raw retouch spots' own Feather, applied here to
/// soften the seam between the model's inherently softer, blurrier
/// output (see the module docs above) and the sharp real pixels right
/// at the boundary, without touching the model itself.
const FEATHER_PX: i64 = 6;

/// The mean of every real, unmasked pixel on the ring two pixels out
/// from `(x, y)` — [`crate::document::Document::ring_mean`]'s own
/// proximity-fill idea, reimplemented locally (this module doesn't
/// depend on `document.rs`) and restricted to unmasked neighbours only,
/// so it never blends the very content being removed back into the
/// result. Falls back to `(x, y)`'s own pixel if every ring neighbour is
/// masked or off-canvas.
fn nearby_unmasked_mean(
    pixels: &[u8],
    mask: &[bool],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
) -> [u8; 3] {
    let mut sum = [0u32; 3];
    let mut count = 0u32;
    for dy in -2i64..=2 {
        for dx in -2i64..=2 {
            if dx.abs().max(dy.abs()) != 2 {
                continue;
            }
            let (nx, ny) = (x as i64 + dx, y as i64 + dy);
            if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                continue;
            }
            let nidx = ny as usize * width + nx as usize;
            if mask[nidx] {
                continue;
            }
            let base = nidx * 4;
            for (c, s) in sum.iter_mut().enumerate() {
                *s += pixels[base + c] as u32;
            }
            count += 1;
        }
    }
    let base = (y * width + x) * 4;
    [
        sum[0].checked_div(count).map_or(pixels[base], |v| v as u8),
        sum[1]
            .checked_div(count)
            .map_or(pixels[base + 1], |v| v as u8),
        sum[2]
            .checked_div(count)
            .map_or(pixels[base + 2], |v| v as u8),
    ]
}

/// How much of the model's own prediction to use at masked pixel
/// `(x, y)`: `0.0` right next to a real, unmasked pixel, ramping up to
/// `1.0` once it's [`FEATHER_PX`] or further from the nearest one — a
/// selection smaller than `FEATHER_PX` across never reaches full
/// strength anywhere in it, which is intentional: a tiny selection is
/// mostly boundary.
fn feather_alpha(mask: &[bool], width: usize, height: usize, x: usize, y: usize) -> f32 {
    let (xi, yi) = (x as i64, y as i64);
    for r in 1..=FEATHER_PX {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx.abs().max(dy.abs()) != r {
                    continue;
                }
                let (nx, ny) = (xi + dx, yi + dy);
                if nx < 0 || ny < 0 || nx as usize >= width || ny as usize >= height {
                    continue;
                }
                if !mask[ny as usize * width + nx as usize] {
                    return r as f32 / (FEATHER_PX as f32 + 1.0);
                }
            }
        }
    }
    1.0
}

/// The selected pixels' bounding box, in canvas coordinates,
/// `(x0, y0, x1, y1)` with `x1`/`y1` exclusive — `None` if nothing in
/// `mask` is `true`.
fn mask_bounds(mask: &[bool], width: usize, height: usize) -> Option<(usize, usize, usize, usize)> {
    let mut x0 = width;
    let mut y0 = height;
    let mut x1 = 0usize;
    let mut y1 = 0usize;
    let mut any = false;
    for y in 0..height {
        for x in 0..width {
            if mask[y * width + x] {
                any = true;
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x + 1);
                y1 = y1.max(y + 1);
            }
        }
    }
    any.then_some((x0, y0, x1, y1))
}

/// A context window around `bounds`, expanded by [`CONTEXT_MARGIN`] and
/// clamped to the canvas — the region actually run through the model.
fn crop_window(
    bounds: (usize, usize, usize, usize),
    width: usize,
    height: usize,
) -> (usize, usize, usize, usize) {
    let (x0, y0, x1, y1) = bounds;
    (
        x0.saturating_sub(CONTEXT_MARGIN),
        y0.saturating_sub(CONTEXT_MARGIN),
        (x1 + CONTEXT_MARGIN).min(width),
        (y1 + CONTEXT_MARGIN).min(height),
    )
}

/// Bilinear resize of one `src_w × src_h` plane to `dst_w × dst_h`,
/// each output sample reading source coordinate
/// `(i + 0.5) · src / dst - 0.5`, clamped to the source's own edges.
fn resize_plane(plane: &[f32], src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> Vec<f32> {
    if src_w == dst_w && src_h == dst_h {
        return plane.to_vec();
    }
    let mut out = vec![0f32; dst_w * dst_h];
    for y in 0..dst_h {
        let sy =
            ((y as f32 + 0.5) * src_h as f32 / dst_h as f32 - 0.5).clamp(0.0, src_h as f32 - 1.0);
        let y0 = sy.floor() as usize;
        let y1 = (y0 + 1).min(src_h - 1);
        let fy = sy - y0 as f32;
        for x in 0..dst_w {
            let sx = ((x as f32 + 0.5) * src_w as f32 / dst_w as f32 - 0.5)
                .clamp(0.0, src_w as f32 - 1.0);
            let x0 = sx.floor() as usize;
            let x1 = (x0 + 1).min(src_w - 1);
            let fx = sx - x0 as f32;
            let top = plane[y0 * src_w + x0] * (1.0 - fx) + plane[y0 * src_w + x1] * fx;
            let bottom = plane[y1 * src_w + x0] * (1.0 - fx) + plane[y1 * src_w + x1] * fx;
            out[y * dst_w + x] = top * (1.0 - fy) + bottom * fy;
        }
    }
    out
}

/// The model's fifth input channel: one standard-normal value per model
/// pixel, drawn deterministically from `seed` (splitmix64, then
/// Box–Muller) so the same seed always means the same fill and the next
/// seed a different one. The network was fine-tuned to respond to this
/// plane rather than ignore it — see `models/GENERATIVE_FILL_NOTICE.md`.
fn noise_plane(seed: u64) -> Vec<f32> {
    let mut state = seed;
    let mut next = move || {
        state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    };
    // (0, 1): the top 53 bits, offset by half a step, so ln never sees 0.
    let mut unit = move || ((next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
    (0..MODEL_SIZE * MODEL_SIZE)
        .map(|_| {
            let (u1, u2) = (unit(), unit());
            ((-2.0 * u1.ln()).sqrt() * (std::f64::consts::TAU * u2).cos()) as f32
        })
        .collect()
}

/// Real, context-only generative fill: every pixel where `mask` is
/// `true` is replaced by the bundled network's own hallucinated content,
/// inferred from a real window of the surrounding pixels; every other
/// pixel (including alpha, everywhere) is returned byte-for-byte
/// unchanged. Seed 0 — the one fixed draw every plain fill uses, so a
/// fill is reproducible; [`generative_fill_rgba_seeded`] is the same
/// with a chosen seed, Generate Similar's own entry point. Errs if
/// `pixels`/`mask` don't match `width`/`height`, either dimension is
/// zero, `mask` selects nothing, or the bundled model fails to load or
/// run.
pub fn generative_fill_rgba(
    pixels: &[u8],
    width: u32,
    height: u32,
    mask: &[bool],
) -> Result<Vec<u8>, String> {
    generative_fill_rgba_seeded(pixels, width, height, mask, 0)
}

/// [`generative_fill_rgba`] with a chosen `seed` for the model's noise
/// plane: a different seed is a different plausible fill of the same
/// hole from the same context.
pub fn generative_fill_rgba_seeded(
    pixels: &[u8],
    width: u32,
    height: u32,
    mask: &[bool],
    seed: u64,
) -> Result<Vec<u8>, String> {
    if width == 0 || height == 0 {
        return Err("Generative Fill needs a non-empty image.".to_string());
    }
    let (width, height) = (width as usize, height as usize);
    if pixels.len() != width * height * 4 {
        return Err(format!(
            "Generative Fill expected {} RGBA bytes for a {}x{} image, got {}.",
            width * height * 4,
            width,
            height,
            pixels.len()
        ));
    }
    if mask.len() != width * height {
        return Err(format!(
            "Generative Fill expected a {}-pixel mask for a {}x{} image, got {}.",
            width * height,
            width,
            height,
            mask.len()
        ));
    }
    let Some(bounds) = mask_bounds(mask, width, height) else {
        return Err("Generative Fill needs a selection.".to_string());
    };
    let (cx0, cy0, cx1, cy1) = crop_window(bounds, width, height);
    let (crop_w, crop_h) = (cx1 - cx0, cy1 - cy0);

    // Build the crop at its own real size first, then resize every
    // channel to MODEL_SIZE — the only size the bundled model accepts.
    let mut crop_channels = vec![vec![0f32; crop_w * crop_h]; 4];
    for y in 0..crop_h {
        for x in 0..crop_w {
            let (sx, sy) = (cx0 + x, cy0 + y);
            let masked = mask[sy * width + sx];
            let src = (sy * width + sx) * 4;
            for c in 0..3 {
                crop_channels[c][y * crop_w + x] = if masked {
                    0.0
                } else {
                    pixels[src + c] as f32 / 255.0
                };
            }
            crop_channels[3][y * crop_w + x] = if masked { 1.0 } else { 0.0 };
        }
    }
    // RGB, mask, then the seed's noise plane — five channels, the last
    // already model-sized so it is never resized.
    let mut input = vec![0f32; 5 * MODEL_SIZE * MODEL_SIZE];
    for (c, plane) in crop_channels.iter().enumerate() {
        let resized = resize_plane(plane, crop_w, crop_h, MODEL_SIZE, MODEL_SIZE);
        input[c * MODEL_SIZE * MODEL_SIZE..(c + 1) * MODEL_SIZE * MODEL_SIZE]
            .copy_from_slice(&resized);
    }
    input[4 * MODEL_SIZE * MODEL_SIZE..].copy_from_slice(&noise_plane(seed));

    let model_bytes: &[u8] = include_bytes!("../models/generative_fill.onnx");
    let model = tract_onnx::onnx()
        .model_for_read(&mut Cursor::new(model_bytes))
        .and_then(|m| m.into_optimized())
        .and_then(|m| m.into_runnable())
        .map_err(|err| format!("Could not load the bundled Generative Fill model: {err}"))?;

    let tensor: Tensor =
        tract_ndarray::Array4::from_shape_vec((1, 5, MODEL_SIZE, MODEL_SIZE), input)
            .map_err(|err| format!("Could not shape Generative Fill's input: {err}"))?
            .into();
    let result = model
        .run(tvec!(tensor.into()))
        .map_err(|err| format!("Generative Fill's model failed to run: {err}"))?;
    let output = result[0]
        .to_array_view::<f32>()
        .map_err(|err| format!("Generative Fill's model returned an unreadable result: {err}"))?;

    // Resize each of the model's 3 output channels back down (or up) to
    // the crop's own real size before compositing.
    let predicted: Vec<Vec<f32>> = (0..3)
        .map(|c| {
            let plane: Vec<f32> = (0..MODEL_SIZE * MODEL_SIZE)
                .map(|i| output[[0, c, i / MODEL_SIZE, i % MODEL_SIZE]])
                .collect();
            resize_plane(&plane, MODEL_SIZE, MODEL_SIZE, crop_w, crop_h)
        })
        .collect();

    let mut out = pixels.to_vec();
    for y in cy0..cy1 {
        for x in cx0..cx1 {
            let idx = y * width + x;
            if !mask[idx] {
                continue;
            }
            let (ly, lx) = (y - cy0, x - cx0);
            let dst = idx * 4;
            let alpha = feather_alpha(mask, width, height, x, y);
            let fallback = nearby_unmasked_mean(pixels, mask, width, height, x, y);
            for c in 0..3 {
                let predicted_v = predicted[c][ly * crop_w + lx] * 255.0;
                let v = alpha * predicted_v + (1.0 - alpha) * fallback[c] as f32;
                out[dst + c] = v.round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        (0..(width * height) as usize).flat_map(|_| rgba).collect()
    }

    #[test]
    fn generative_fill_rgba_rejects_a_mismatched_pixel_buffer() {
        let mask = vec![false; 4];
        let err = generative_fill_rgba(&[0u8; 3], 2, 2, &mask).unwrap_err();
        assert!(err.contains("RGBA bytes"));
    }

    #[test]
    fn generative_fill_rgba_rejects_a_mismatched_mask() {
        let pixels = solid(2, 2, [0, 0, 0, 255]);
        let err = generative_fill_rgba(&pixels, 2, 2, &[false]).unwrap_err();
        assert!(err.contains("mask"));
    }

    #[test]
    fn generative_fill_rgba_rejects_a_zero_dimension() {
        assert!(generative_fill_rgba(&[], 0, 4, &[]).is_err());
        assert!(generative_fill_rgba(&[], 4, 0, &[]).is_err());
    }

    #[test]
    fn generative_fill_rgba_rejects_an_empty_selection() {
        let pixels = solid(4, 4, [10, 20, 30, 255]);
        let mask = vec![false; 16];
        let err = generative_fill_rgba(&pixels, 4, 4, &mask).unwrap_err();
        assert!(err.contains("selection"));
    }

    #[test]
    fn generative_fill_rgba_only_changes_selected_pixels_alpha_included() {
        let width = 16u32;
        let height = 16u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let v = ((i * 37) % 256) as u8;
            pixels[i * 4] = v;
            pixels[i * 4 + 1] = 255 - v;
            pixels[i * 4 + 2] = v / 2;
            pixels[i * 4 + 3] = 173;
        }
        let mut mask = vec![false; (width * height) as usize];
        for y in 6..10 {
            for x in 6..10 {
                mask[(y * width + x) as usize] = true;
            }
        }
        let out = generative_fill_rgba(&pixels, width, height, &mask).unwrap();
        assert_eq!(out.len(), pixels.len());
        // Alpha is never touched by this model at all, anywhere.
        assert!(out.chunks_exact(4).all(|p| p[3] == 173));
        // Every unselected pixel's RGB is byte-for-byte the original.
        for (idx, &masked) in mask.iter().enumerate() {
            if !masked {
                assert_eq!(out[idx * 4..idx * 4 + 3], pixels[idx * 4..idx * 4 + 3]);
            }
        }
    }

    #[test]
    fn noise_plane_is_model_sized_standard_normal_and_seed_deterministic() {
        let a = noise_plane(7);
        assert_eq!(a.len(), MODEL_SIZE * MODEL_SIZE);
        assert_eq!(a, noise_plane(7));
        assert_ne!(a, noise_plane(8));
        let mean = a.iter().map(|&v| v as f64).sum::<f64>() / a.len() as f64;
        let var = a.iter().map(|&v| (v as f64 - mean).powi(2)).sum::<f64>() / a.len() as f64;
        assert!(mean.abs() < 0.05, "mean {mean}");
        assert!((var - 1.0).abs() < 0.1, "variance {var}");
    }

    #[test]
    fn a_different_seed_is_a_different_fill_of_the_same_hole_and_a_seed_repeats_exactly() {
        // This is Generate Similar's whole contract, so it is asserted on
        // the real model: the noise plane must actually change the fill.
        let width = 32u32;
        let height = 32u32;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        for i in 0..(width * height) as usize {
            let (x, y) = (i % width as usize, i / width as usize);
            pixels[i * 4] = (x * 8) as u8;
            pixels[i * 4 + 1] = (y * 8) as u8;
            pixels[i * 4 + 2] = ((x + y) * 4) as u8;
            pixels[i * 4 + 3] = 255;
        }
        let mut mask = vec![false; (width * height) as usize];
        for y in 10..22 {
            for x in 10..22 {
                mask[(y * width + x) as usize] = true;
            }
        }
        let first = generative_fill_rgba_seeded(&pixels, width, height, &mask, 1).unwrap();
        let again = generative_fill_rgba_seeded(&pixels, width, height, &mask, 1).unwrap();
        let second = generative_fill_rgba_seeded(&pixels, width, height, &mask, 2).unwrap();
        assert_eq!(first, again);
        let differing = mask
            .iter()
            .enumerate()
            .filter(|&(idx, &masked)| {
                masked && first[idx * 4..idx * 4 + 3] != second[idx * 4..idx * 4 + 3]
            })
            .count();
        assert!(differing > 0, "two seeds gave byte-identical fills");
        // Outside the hole both are the untouched original.
        for (idx, &masked) in mask.iter().enumerate() {
            if !masked {
                assert_eq!(second[idx * 4..idx * 4 + 4], pixels[idx * 4..idx * 4 + 4]);
            }
        }
    }
}
