//! Image > Mode > HDR Support / HDR Histogram: real scene-referred HDR
//! data — a real Radiance HDR (`.hdr`/`.pic`) file's own real
//! floating-point samples, which can genuinely exceed `1.0` (an
//! "overbright" highlight no normal `0..=255` byte can ever represent) —
//! and a real histogram over that real range, not the normal `0..=255`
//! one every other histogram in this project already covers
//! (`Document::layer_histogram`). This is deliberately scoped to import
//! and analysis, not full HDR editing: the real float data lives here,
//! separate from this project's own 8-bit layer pipeline, not blended
//! into it — a documented boundary, not a hidden gap.

use image::codecs::hdr::HdrDecoder;
use image::ImageDecoder;
use serde::Serialize;

/// One real, decoded HDR image: real RGB `f32` samples (Radiance HDR has
/// no alpha channel of its own), any of which may genuinely be `> 1.0`.
#[derive(Debug, Clone)]
pub struct HdrImage {
    pub width: u32,
    pub height: u32,
    /// `width * height * 3` floats, row-major, R/G/B per pixel.
    pub pixels: Vec<f32>,
}

/// Parses `bytes` as a real Radiance HDR file.
pub fn decode_bytes(bytes: &[u8]) -> Result<HdrImage, String> {
    let decoder =
        HdrDecoder::new(bytes).map_err(|err| format!("Not a valid HDR (.hdr) file: {err}"))?;
    let (width, height) = decoder.dimensions();
    let mut pixels = vec![0f32; width as usize * height as usize * 3];
    decoder
        .read_image(bytemuck::cast_slice_mut(&mut pixels))
        .map_err(|err| format!("Could not decode the HDR file: {err}"))?;
    Ok(HdrImage {
        width,
        height,
        pixels,
    })
}

/// A real histogram over an [`HdrImage`]'s own real, unclamped luma range
/// — BT.601 weights (`0.299R + 0.587G + 0.114B`), the same weights this
/// project's own [`crate::document::saturation_anchor`] and Grayscale
/// Mode already use, applied here to real values that can exceed `1.0`
/// rather than to a clamped byte.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HdrHistogram {
    pub bins: Vec<u32>,
    /// The real maximum luma actually found in the image — every bin
    /// boundary is data-derived, not a guessed fixed range.
    pub max_luma: f32,
    /// How many bins, from the low end, cover the real `0.0..=1.0` range
    /// a normal `0..=255` histogram can represent at all. Any bin at or
    /// past this index holds real overbright content a normal histogram
    /// has nowhere to put.
    pub in_gamut_bin_count: usize,
    /// How many of the real sampled pixels have a luma genuinely over
    /// `1.0` — content no 8-bit histogram could ever show existed.
    pub overbright_pixel_count: u32,
}

/// Builds a real [`HdrHistogram`] over `image`'s own real luma values,
/// into `bin_count` bins spanning `0.0` to the image's own real maximum
/// luma. Errs on `bin_count == 0` — a histogram with no bins isn't one.
pub fn histogram(image: &HdrImage, bin_count: usize) -> Result<HdrHistogram, String> {
    if bin_count == 0 {
        return Err("HDR Histogram needs at least one bin.".to_string());
    }
    let lumas: Vec<f32> = image
        .pixels
        .chunks_exact(3)
        .map(|p| 0.299 * p[0] + 0.587 * p[1] + 0.114 * p[2])
        .collect();
    let max_luma = lumas
        .iter()
        .cloned()
        .fold(0.0f32, f32::max)
        .max(f32::MIN_POSITIVE);

    let mut bins = vec![0u32; bin_count];
    let mut overbright_pixel_count = 0u32;
    for luma in lumas {
        let luma = luma.max(0.0);
        if luma > 1.0 {
            overbright_pixel_count += 1;
        }
        let index = ((luma / max_luma) * bin_count as f32) as usize;
        bins[index.min(bin_count - 1)] += 1;
    }
    let in_gamut_bin_count = (((1.0 / max_luma) * bin_count as f32).ceil() as usize).min(bin_count);

    Ok(HdrHistogram {
        bins,
        max_luma,
        in_gamut_bin_count,
        overbright_pixel_count,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grey_hdr_image(values: &[f32]) -> HdrImage {
        HdrImage {
            width: values.len() as u32,
            height: 1,
            pixels: values.iter().flat_map(|&v| [v, v, v]).collect(),
        }
    }

    #[test]
    fn decode_bytes_rejects_non_hdr_bytes() {
        assert!(decode_bytes(b"not an hdr file").is_err());
    }

    #[test]
    fn histogram_rejects_zero_bins() {
        let image = grey_hdr_image(&[0.5]);
        assert!(histogram(&image, 0).is_err());
    }

    #[test]
    fn histogram_buckets_a_real_overbright_pixel_beyond_the_normal_0_to_1_range() {
        // Grey pixels (R=G=B=value) make BT.601 luma exactly the pixel's
        // own value (0.299+0.587+0.114 == 1.0), for an exact, hand-
        // verifiable test: 0.5 and 2.0, 4 bins.
        // max_luma = 2.0.
        // 0.5 -> idx = floor((0.5/2.0)*4) = floor(1.0) = 1.
        // 2.0 -> idx = floor((2.0/2.0)*4) = floor(4.0) = 4, clamped to 3.
        // in_gamut_bin_count = ceil((1.0/2.0)*4) = ceil(2.0) = 2.
        let image = grey_hdr_image(&[0.5, 2.0]);
        let result = histogram(&image, 4).unwrap();
        assert_eq!(result.bins, vec![0, 1, 0, 1]);
        assert!((result.max_luma - 2.0).abs() < 1e-6);
        assert_eq!(result.in_gamut_bin_count, 2);
        assert_eq!(result.overbright_pixel_count, 1);
    }

    #[test]
    fn histogram_reports_every_bin_in_gamut_when_nothing_is_overbright() {
        let image = grey_hdr_image(&[0.1, 0.4, 0.9]);
        let result = histogram(&image, 4).unwrap();
        assert_eq!(result.overbright_pixel_count, 0);
        assert_eq!(result.in_gamut_bin_count, 4);
    }

    #[test]
    fn decode_then_histogram_round_trips_a_real_encoded_overbright_pixel() {
        // A real Radiance HDR file, encoded by the same real `image`
        // crate this project depends on -- not a hand-typed byte
        // literal -- containing one genuinely overbright pixel.
        let pixel = image::Rgb([2.0f32, 4.0f32, 0.5f32]);
        let mut bytes = Vec::new();
        image::codecs::hdr::HdrEncoder::new(&mut bytes)
            .encode(&[pixel], 1, 1)
            .unwrap();

        let decoded = decode_bytes(&bytes).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        assert!((decoded.pixels[0] - 2.0).abs() < 0.05);
        assert!((decoded.pixels[1] - 4.0).abs() < 0.05);
        assert!((decoded.pixels[2] - 0.5).abs() < 0.05);

        let result = histogram(&decoded, 8).unwrap();
        assert_eq!(result.overbright_pixel_count, 1);
    }
}
