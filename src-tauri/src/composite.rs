//! Flattens a [`Document`]'s layer stack into a single RGBA8 image.
//!
//! The math follows the W3C compositing spec: each layer is blended against the
//! accumulated backdrop with its blend function, then composited with the
//! `source-over` Porter-Duff operator.
//!
//! For a source with alpha `as` over a backdrop with alpha `ab`:
//!
//! ```text
//! Cs' = (1 - ab) * Cs + ab * B(Cb, Cs)          // blend against the backdrop
//! ao  = as + ab * (1 - as)                      // source-over alpha
//! Co  = (as * Cs' + ab * Cb * (1 - as)) / ao    // back to non-premultiplied
//! ```
//!
//! Accumulation happens in `f32` with non-premultiplied alpha, so repeated
//! layers do not accumulate 8-bit rounding error. Values are quantized to `u8`
//! once, at the end.

use crate::document::{Document, CHANNELS};

/// A flattened RGBA8 image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composite {
    pub width: u32,
    pub height: u32,
    /// Non-premultiplied RGBA8, row-major, `width * height * 4` bytes.
    pub pixels: Vec<u8>,
}

/// Flatten every contributing layer, bottom to top.
///
/// Layers that are hidden or at zero opacity are skipped. A document with no
/// contributing layers flattens to fully transparent pixels.
///
/// Pixels that end up fully transparent are emitted as `[0, 0, 0, 0]`: colour
/// under zero alpha is not visible, so it is not carried into the result even
/// when the source layer stored something there.
pub fn flatten(document: &Document) -> Composite {
    let pixel_count = document.width() as usize * document.height() as usize;

    // Accumulated backdrop: non-premultiplied RGBA, starting fully transparent.
    let mut backdrop = vec![0f32; pixel_count * CHANNELS];

    for layer in document.layers().iter().filter(|layer| layer.contributes()) {
        for i in 0..pixel_count {
            let base = i * CHANNELS;

            let source_alpha = to_unit(layer.pixels[base + 3]) * layer.opacity;
            if source_alpha <= 0.0 {
                continue;
            }
            let backdrop_alpha = backdrop[base + 3];
            let out_alpha = source_alpha + backdrop_alpha * (1.0 - source_alpha);
            if out_alpha <= 0.0 {
                continue;
            }

            for channel in 0..3 {
                let cs = to_unit(layer.pixels[base + channel]);
                let cb = backdrop[base + channel];
                // Where the backdrop is transparent there is nothing to blend
                // against, so the source shows through unblended.
                let blended =
                    (1.0 - backdrop_alpha) * cs + backdrop_alpha * layer.blend_mode.blend(cb, cs);
                backdrop[base + channel] = (source_alpha * blended
                    + backdrop_alpha * cb * (1.0 - source_alpha))
                    / out_alpha;
            }
            backdrop[base + 3] = out_alpha;
        }
    }

    Composite {
        width: document.width(),
        height: document.height(),
        pixels: backdrop.into_iter().map(to_byte).collect(),
    }
}

fn to_unit(byte: u8) -> f32 {
    f32::from(byte) / 255.0
}

fn to_byte(unit: f32) -> u8 {
    // `round` then clamp: the arithmetic above can land a hair outside 0..=1.
    (unit * 255.0).round().clamp(0.0, 255.0) as u8
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::blend::BlendMode;
    use crate::document::MoveDirection;

    fn solid(width: u32, height: u32, rgba: [u8; 4]) -> Vec<u8> {
        rgba.iter()
            .copied()
            .cycle()
            .take(width as usize * height as usize * CHANNELS)
            .collect()
    }

    /// A 1x1 document, the smallest thing that exercises the full pipeline.
    fn dot(layers: &[([u8; 4], f32, bool, BlendMode)]) -> [u8; 4] {
        let mut doc = Document::new(1, 1).unwrap();
        for (index, &(rgba, opacity, visible, mode)) in layers.iter().enumerate() {
            let id = doc
                .add_layer(format!("layer {index}"), &solid(1, 1, rgba), 1, 1)
                .unwrap();
            doc.set_opacity(id, opacity).unwrap();
            doc.set_visible(id, visible).unwrap();
            doc.set_blend_mode(id, mode).unwrap();
        }
        let out = flatten(&doc);
        [out.pixels[0], out.pixels[1], out.pixels[2], out.pixels[3]]
    }

    /// Allow one 8-bit step of quantization slack.
    fn near(actual: [u8; 4], expected: [u8; 4]) -> bool {
        actual
            .iter()
            .zip(expected.iter())
            .all(|(a, e)| a.abs_diff(*e) <= 1)
    }

    macro_rules! assert_near {
        ($actual:expr, $expected:expr) => {{
            let actual = $actual;
            let expected = $expected;
            assert!(near(actual, expected), "expected {expected:?}, got {actual:?}");
        }};
    }

    const OPAQUE: f32 = 1.0;
    const SHOWN: bool = true;
    const HIDDEN: bool = false;
    use BlendMode::Normal;

    #[test]
    fn an_empty_document_flattens_to_transparent() {
        let doc = Document::new(3, 2).unwrap();
        let out = flatten(&doc);
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(out.pixels, vec![0u8; 3 * 2 * 4]);
    }

    #[test]
    fn output_dimensions_match_the_document() {
        let mut doc = Document::new(5, 7).unwrap();
        doc.add_layer("a", &solid(5, 7, [1, 2, 3, 255]), 5, 7).unwrap();
        let out = flatten(&doc);
        assert_eq!((out.width, out.height), (5, 7));
        assert_eq!(out.pixels.len(), 5 * 7 * 4);
    }

    #[test]
    fn a_single_opaque_layer_passes_through_unchanged() {
        assert_near!(dot(&[([200, 100, 50, 255], OPAQUE, SHOWN, Normal)]), [200, 100, 50, 255]);
    }

    #[test]
    fn a_hidden_layer_contributes_nothing() {
        assert_near!(
            dot(&[
                ([255, 0, 0, 255], OPAQUE, SHOWN, Normal),
                ([0, 255, 0, 255], OPAQUE, HIDDEN, Normal),
            ]),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn a_zero_opacity_layer_contributes_nothing() {
        assert_near!(
            dot(&[
                ([255, 0, 0, 255], OPAQUE, SHOWN, Normal),
                ([0, 255, 0, 255], 0.0, SHOWN, Normal),
            ]),
            [255, 0, 0, 255]
        );
    }

    #[test]
    fn a_fully_transparent_source_leaves_the_backdrop_alone() {
        assert_near!(
            dot(&[
                ([10, 20, 30, 255], OPAQUE, SHOWN, Normal),
                ([99, 99, 99, 0], OPAQUE, SHOWN, Normal),
            ]),
            [10, 20, 30, 255]
        );
    }

    #[test]
    fn half_opacity_white_over_black_is_mid_grey() {
        assert_near!(
            dot(&[
                ([0, 0, 0, 255], OPAQUE, SHOWN, Normal),
                ([255, 255, 255, 255], 0.5, SHOWN, Normal),
            ]),
            [128, 128, 128, 255]
        );
    }

    #[test]
    fn layer_alpha_and_layer_opacity_multiply() {
        // A 50%-alpha white pixel at 50% layer opacity == 25% coverage.
        assert_near!(
            dot(&[
                ([0, 0, 0, 255], OPAQUE, SHOWN, Normal),
                ([255, 255, 255, 128], 0.5, SHOWN, Normal),
            ]),
            [64, 64, 64, 255]
        );
    }

    #[test]
    fn stacking_order_matters() {
        let red_over_green = dot(&[
            ([0, 255, 0, 255], OPAQUE, SHOWN, Normal),
            ([255, 0, 0, 255], OPAQUE, SHOWN, Normal),
        ]);
        let green_over_red = dot(&[
            ([255, 0, 0, 255], OPAQUE, SHOWN, Normal),
            ([0, 255, 0, 255], OPAQUE, SHOWN, Normal),
        ]);
        assert_near!(red_over_green, [255, 0, 0, 255]);
        assert_near!(green_over_red, [0, 255, 0, 255]);
    }

    #[test]
    fn reordering_layers_changes_the_composite() {
        let mut doc = Document::new(1, 1).unwrap();
        let bottom = doc.add_layer("b", &solid(1, 1, [255, 0, 0, 255]), 1, 1).unwrap();
        doc.add_layer("t", &solid(1, 1, [0, 255, 0, 255]), 1, 1).unwrap();
        assert_eq!(&flatten(&doc).pixels[..], &[0, 255, 0, 255]);

        doc.move_layer(bottom, MoveDirection::Up).unwrap();
        assert_eq!(&flatten(&doc).pixels[..], &[255, 0, 0, 255]);
    }

    #[test]
    fn multiply_darkens_against_the_backdrop() {
        // 0.5 * 0.5 == 0.25 -> 64
        assert_near!(
            dot(&[
                ([128, 128, 128, 255], OPAQUE, SHOWN, Normal),
                ([128, 128, 128, 255], OPAQUE, SHOWN, BlendMode::Multiply),
            ]),
            [64, 64, 64, 255]
        );
    }

    #[test]
    fn screen_lightens_against_the_backdrop() {
        // 0.5 + 0.5 - 0.25 == 0.75 -> 191
        assert_near!(
            dot(&[
                ([128, 128, 128, 255], OPAQUE, SHOWN, Normal),
                ([128, 128, 128, 255], OPAQUE, SHOWN, BlendMode::Screen),
            ]),
            [191, 191, 191, 255]
        );
    }

    #[test]
    fn difference_of_a_layer_with_itself_is_black() {
        assert_near!(
            dot(&[
                ([200, 100, 50, 255], OPAQUE, SHOWN, Normal),
                ([200, 100, 50, 255], OPAQUE, SHOWN, BlendMode::Difference),
            ]),
            [0, 0, 0, 255]
        );
    }

    #[test]
    fn blend_modes_have_no_effect_over_a_transparent_backdrop() {
        // With nothing underneath, every mode must show the source as-is —
        // there is no backdrop to blend against.
        for mode in BlendMode::ALL {
            assert_near!(dot(&[([200, 100, 50, 255], OPAQUE, SHOWN, mode)]), [200, 100, 50, 255]);
        }
    }

    #[test]
    fn the_bottom_layer_alpha_is_preserved() {
        // A half-transparent lone layer stays half-transparent.
        assert_near!(dot(&[([255, 0, 0, 128], OPAQUE, SHOWN, Normal)]), [255, 0, 0, 128]);
    }

    #[test]
    fn two_half_alpha_layers_accumulate_alpha_correctly() {
        // ao = 0.5 + 0.5 * (1 - 0.5) = 0.75 -> 191
        let out = dot(&[
            ([255, 0, 0, 128], OPAQUE, SHOWN, Normal),
            ([0, 0, 255, 128], OPAQUE, SHOWN, Normal),
        ]);
        assert!(out[3].abs_diff(191) <= 1, "alpha was {}", out[3]);
    }

    #[test]
    fn per_pixel_independence_is_respected() {
        // Two pixels with different content must not leak into each other.
        let mut doc = Document::new(2, 1).unwrap();
        let mut base = vec![0u8; 8];
        base[0..4].copy_from_slice(&[255, 0, 0, 255]);
        base[4..8].copy_from_slice(&[0, 0, 255, 255]);
        doc.add_layer("base", &base, 2, 1).unwrap();

        let out = flatten(&doc);
        assert_eq!(&out.pixels[0..4], &[255, 0, 0, 255]);
        assert_eq!(&out.pixels[4..8], &[0, 0, 255, 255]);
    }

    #[test]
    fn many_stacked_layers_stay_in_range() {
        // Guards against f32 drift or overflow pushing a channel past 255.
        let mut doc = Document::new(1, 1).unwrap();
        for (index, mode) in BlendMode::ALL.into_iter().enumerate() {
            let id = doc
                .add_layer(format!("l{index}"), &solid(1, 1, [180, 90, 200, 200]), 1, 1)
                .unwrap();
            doc.set_blend_mode(id, mode).unwrap();
            doc.set_opacity(id, 0.7).unwrap();
        }
        let out = flatten(&doc);
        assert_eq!(out.pixels.len(), 4);
        // Reaching here without a panic plus a valid alpha is the assertion; u8
        // cannot represent an out-of-range value, so the clamp is what is tested.
        assert!(out.pixels[3] > 0);
    }
}
