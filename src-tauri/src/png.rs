//! Reading PNG files into RGBA8 and writing composites back out as data URLs.

use std::path::Path;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use image::{ImageEncoder, ImageFormat};

use crate::composite::Composite;

/// Refuse anything large enough that compositing and base64-ing it would be a
/// memory problem. Phase 1 still ships each composite to the webview as a data
/// URL; a shared-buffer or asset-protocol path is what lifts this ceiling.
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

/// A decoded PNG in non-premultiplied RGBA8.
#[derive(Debug)]
pub struct DecodedPng {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

/// Read and decode a PNG from disk.
pub fn read(path: &Path) -> Result<DecodedPng, String> {
    let metadata = std::fs::metadata(path)
        .map_err(|err| format!("Could not read {}: {err}", path.display()))?;

    if !metadata.is_file() {
        return Err(format!("{} is not a file.", path.display()));
    }

    let byte_length = metadata.len();
    if byte_length > MAX_FILE_BYTES {
        return Err(format!(
            "{} is {:.1} MB, which is over the {} MB limit.",
            path.display(),
            byte_length as f64 / (1024.0 * 1024.0),
            MAX_FILE_BYTES / (1024 * 1024)
        ));
    }

    let bytes =
        std::fs::read(path).map_err(|err| format!("Could not read {}: {err}", path.display()))?;

    let reader =
        image::ImageReader::with_format(std::io::Cursor::new(&bytes), ImageFormat::Png);
    let decoded = reader
        .decode()
        .map_err(|err| format!("{} is not a readable PNG: {err}", path.display()))?
        .to_rgba8();

    Ok(DecodedPng {
        width: decoded.width(),
        height: decoded.height(),
        pixels: decoded.into_raw(),
    })
}

/// Encode a composite as `data:image/png;base64,…` for an `<img>` tag.
pub fn to_data_url(composite: &Composite) -> Result<String, String> {
    let mut buffer = Vec::new();
    image::codecs::png::PngEncoder::new(&mut buffer)
        .write_image(
            &composite.pixels,
            composite.width,
            composite.height,
            image::ExtendedColorType::Rgba8,
        )
        .map_err(|err| format!("Could not encode the composite: {err}"))?;

    Ok(format!("data:image/png;base64,{}", STANDARD.encode(&buffer)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// 1x1 opaque red.
    const ONE_PIXEL_PNG: &[u8] = &[
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xDD, 0x8D, 0xB0, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];

    fn write_temp(name: &str, bytes: &[u8]) -> PathBuf {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn decodes_a_png_to_rgba8() {
        let path = write_temp("png_rs_ok.png", ONE_PIXEL_PNG);
        let decoded = read(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (1, 1));
        // The source is RGB; decoding to RGBA must add an opaque alpha channel.
        assert_eq!(decoded.pixels, vec![255, 0, 0, 255]);
    }

    #[test]
    fn rejects_a_file_that_is_not_a_png() {
        let path = write_temp("png_rs_bad.png", b"definitely not a png");
        assert!(read(&path).unwrap_err().contains("not a readable PNG"));
    }

    #[test]
    fn rejects_a_missing_file() {
        let path = std::env::temp_dir().join("png_rs_absent.png");
        let _ = std::fs::remove_file(&path);
        assert!(read(&path).unwrap_err().contains("Could not read"));
    }

    #[test]
    fn composites_round_trip_through_png() {
        let composite = Composite {
            width: 2,
            height: 1,
            pixels: vec![255, 0, 0, 255, 0, 0, 255, 128],
        };
        let url = to_data_url(&composite).unwrap();
        assert!(url.starts_with("data:image/png;base64,"));

        // Decode it back and confirm the pixels survived, alpha included.
        let raw = STANDARD
            .decode(url.trim_start_matches("data:image/png;base64,"))
            .unwrap();
        let path = write_temp("png_rs_roundtrip.png", &raw);
        let decoded = read(&path).unwrap();
        assert_eq!((decoded.width, decoded.height), (2, 1));
        assert_eq!(decoded.pixels, composite.pixels);
    }
}
