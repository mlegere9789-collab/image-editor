//! The font registry behind the Type tools: real TrueType/OpenType faces,
//! rasterised by `fontdue` (a pure-Rust font parser and rasteriser), kept
//! process-wide like a system's installed fonts. One face is bundled --
//! Open Sans Regular, SIL Open Font License 1.1, `fonts/OFL.txt` -- so a
//! text layer can use a real scalable font with nothing activated; every
//! other face arrives through [`register`] (the Fonts… dialog activating
//! one from image-editor-server's catalogue, or a `.ttf` the user picks)
//! and is written to the app's data directory so it is there next launch.
//!
//! A text layer whose `font` is `None` still draws the built-in 5×7
//! bitmap face (`document::glyph`), so every project saved before this
//! module existed renders exactly as it did.

use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock, RwLock};

use fontdue::layout::{CoordinateSystem, Layout, LayoutSettings, TextStyle};

/// The bundled face's registry name.
pub const BUNDLED: &str = "Open Sans";
const BUNDLED_BYTES: &[u8] = include_bytes!("../fonts/OpenSans-Regular.ttf");

/// The largest pixel size a font face renders at.
pub const MAX_SIZE: u32 = 1024;

fn registry() -> &'static RwLock<BTreeMap<String, Arc<fontdue::Font>>> {
    static REGISTRY: OnceLock<RwLock<BTreeMap<String, Arc<fontdue::Font>>>> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let font = fontdue::Font::from_bytes(BUNDLED_BYTES, fontdue::FontSettings::default())
            .expect("the bundled Open Sans parses");
        let mut map = BTreeMap::new();
        map.insert(BUNDLED.to_string(), Arc::new(font));
        RwLock::new(map)
    })
}

/// A registry name: 1-80 characters, no control characters, not blank.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 80 {
        return Err("A font name is 1 to 80 characters.".to_string());
    }
    if trimmed.chars().any(char::is_control) {
        return Err("A font name cannot contain control characters.".to_string());
    }
    Ok(())
}

/// Activates a face under `name` from its TrueType/OpenType bytes,
/// replacing a face of the same name. Errors for a name that fails
/// [`validate_name`] or bytes `fontdue` cannot parse.
pub fn register(name: &str, bytes: &[u8]) -> Result<(), String> {
    validate_name(name)?;
    let font = fontdue::Font::from_bytes(bytes, fontdue::FontSettings::default())
        .map_err(|e| format!("Not a readable font: {e}"))?;
    registry()
        .write()
        .expect("font registry lock")
        .insert(name.trim().to_string(), Arc::new(font));
    Ok(())
}

/// Every activated face, in name order -- the bundled one always among
/// them.
pub fn names() -> Vec<String> {
    registry()
        .read()
        .expect("font registry lock")
        .keys()
        .cloned()
        .collect()
}

pub fn get(name: &str) -> Option<Arc<fontdue::Font>> {
    registry()
        .read()
        .expect("font registry lock")
        .get(name.trim())
        .cloned()
}

/// One rasterised glyph, placed: its top-left in text-layer space and a
/// `width × height` coverage bitmap, 0..=255.
pub struct PlacedGlyph {
    pub x: i32,
    pub y: i32,
    pub width: usize,
    pub height: usize,
    pub coverage: Vec<u8>,
}

/// Lays out `text` in `font` at `size` pixels and rasterises every
/// glyph. Horizontal type: `fontdue`'s own layout, lines dropping by the
/// face's line height. Vertical type: each line of the text becomes a
/// column, its characters stacked one per line, columns stepping right
/// by the face's line height (the same measure, turned).
pub fn layout(font: &fontdue::Font, text: &str, size: u32, vertical: bool) -> Vec<PlacedGlyph> {
    let px = size as f32;
    let fonts = [font];
    let mut placed = Vec::new();
    let columns: Vec<String> = if vertical {
        text.split('\n')
            .map(|line| {
                line.chars()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .collect()
    } else {
        vec![text.to_string()]
    };
    let column_step = font
        .horizontal_line_metrics(px)
        .map_or(px * 1.2, |m| m.new_line_size);
    for (column, column_text) in columns.iter().enumerate() {
        let mut layout = Layout::new(CoordinateSystem::PositiveYDown);
        layout.reset(&LayoutSettings {
            x: column as f32 * column_step,
            y: 0.0,
            ..LayoutSettings::default()
        });
        layout.append(&fonts, &TextStyle::new(column_text, px, 0));
        for glyph in layout.glyphs() {
            if glyph.width == 0 || glyph.height == 0 {
                continue;
            }
            let (metrics, coverage) = font.rasterize_config(glyph.key);
            debug_assert_eq!((metrics.width, metrics.height), (glyph.width, glyph.height));
            placed.push(PlacedGlyph {
                x: glyph.x.round() as i32,
                y: glyph.y.round() as i32,
                width: metrics.width,
                height: metrics.height,
                coverage,
            });
        }
    }
    placed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bundled_face_is_always_there_and_names_are_checked() {
        assert!(names().contains(&BUNDLED.to_string()));
        assert!(get(BUNDLED).is_some());
        assert!(get("Nope").is_none());
        assert!(register("", BUNDLED_BYTES).is_err());
        assert!(register("a\nb", BUNDLED_BYTES).is_err());
        assert!(register(&"x".repeat(81), BUNDLED_BYTES).is_err());
        assert!(register("Bogus", b"not a font").is_err());
        register("  Copy of Open Sans ", BUNDLED_BYTES).unwrap();
        assert!(get("Copy of Open Sans").is_some());
        assert!(names().contains(&"Copy of Open Sans".to_string()));
    }

    #[test]
    fn layout_matches_fontdue_glyph_by_glyph() {
        let font = get(BUNDLED).unwrap();
        // Two lines: the second line's glyphs sit one line height lower.
        let glyphs = layout(&font, "Hi\nyo", 32, false);
        assert_eq!(glyphs.len(), 4);
        let line = font.horizontal_line_metrics(32.0).unwrap().new_line_size;
        assert!(glyphs[2].y as f32 >= glyphs[0].y as f32 + line * 0.5);
        // Each placed bitmap is exactly what fontdue rasterises for that
        // character at that size.
        let (metrics, coverage) = font.rasterize('H', 32.0);
        assert_eq!(
            (glyphs[0].width, glyphs[0].height),
            (metrics.width, metrics.height)
        );
        assert_eq!(glyphs[0].coverage, coverage);
        // "i" starts to the right of "H" by H's advance.
        assert!(glyphs[1].x >= glyphs[0].x + metrics.advance_width.floor() as i32 - 1);
        // Vertical type stacks the characters of a line and puts the next
        // line in a column to the right.
        let vertical = layout(&font, "Hi\nyo", 32, true);
        assert_eq!(vertical.len(), 4);
        assert!(vertical[1].y > vertical[0].y + metrics.height as i32 / 2);
        assert!(vertical[2].x > vertical[0].x + 16);
        assert_eq!(vertical[0].coverage, coverage);
        // Spaces place no glyph.
        assert_eq!(layout(&font, "a b", 20, false).len(), 2);
    }
}
