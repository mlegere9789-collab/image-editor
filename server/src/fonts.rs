//! Fonts: Adobe Fonts' open equivalent. A catalogue of open-licensed
//! families (`fonts_catalogue.json`: family, category and licence as
//! Google Fonts publishes them -- SIL Open Font License, Apache 2.0,
//! Ubuntu Font Licence), each fetched on first request from Google
//! Fonts' own servers through its CSS API, cached under
//! `<data-dir>/fonts/cache/`, and served to the desktop app as TrueType
//! bytes. Any `.ttf`/`.otf` the operator drops in `<data-dir>/fonts/local/`
//! is listed too, under its file name, licence "local".

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::store::StoreError;

const CATALOGUE: &str = include_str!("../fonts_catalogue.json");

/// One family a user can activate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FontEntry {
    pub family: String,
    pub category: String,
    pub license: String,
    /// `"google"` for catalogue families, `"local"` for the operator's own files.
    #[serde(default = "google")]
    pub source: String,
}

fn google() -> String {
    "google".into()
}

pub fn catalogue() -> Vec<FontEntry> {
    serde_json::from_str(CATALOGUE).expect("the bundled catalogue parses")
}

fn local_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("fonts").join("local")
}

/// The operator's own font files, by file stem.
pub fn local_fonts(data_dir: &Path) -> Vec<FontEntry> {
    let mut fonts: Vec<FontEntry> = fs::read_dir(local_dir(data_dir))
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| {
                    let path = entry.path();
                    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
                    if ext != "ttf" && ext != "otf" {
                        return None;
                    }
                    Some(FontEntry {
                        family: path.file_stem()?.to_str()?.to_string(),
                        category: "local".into(),
                        license: "local".into(),
                        source: "local".into(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    fonts.sort_by(|a, b| a.family.cmp(&b.family));
    fonts
}

/// Every family: the catalogue, then the local files.
pub fn list(data_dir: &Path) -> Vec<FontEntry> {
    let mut all = catalogue();
    all.extend(local_fonts(data_dir));
    all
}

/// The `url(...)` of the first TrueType face in a Google Fonts CSS
/// response.
pub fn truetype_url(css: &str) -> Option<String> {
    css.split("url(").skip(1).find_map(|rest| {
        let url = rest
            .split(')')
            .next()?
            .trim_matches(|c| c == '"' || c == '\'');
        if url.ends_with(".ttf") && url.starts_with("https://fonts.gstatic.com/") {
            Some(url.to_string())
        } else {
            None
        }
    })
}

fn slug(family: &str) -> String {
    family
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

/// The bytes of `family` at `weight` (100..=900), upright or italic:
/// a local file as it is, a catalogue family from the cache or, once,
/// from Google Fonts.
pub fn font_file(
    data_dir: &Path,
    family: &str,
    weight: u16,
    italic: bool,
) -> Result<Vec<u8>, StoreError> {
    if let Some(local) = local_fonts(data_dir).iter().find(|f| f.family == family) {
        for ext in ["ttf", "otf"] {
            let path = local_dir(data_dir).join(format!("{}.{ext}", local.family));
            if path.exists() {
                return Ok(fs::read(path)?);
            }
        }
        return Err(StoreError::NotFound);
    }
    if !catalogue().iter().any(|f| f.family == family) {
        return Err(StoreError::NotFound);
    }
    if !(100..=900).contains(&weight) || !weight.is_multiple_of(100) {
        return Err(StoreError::Invalid("weight is 100, 200, ... 900".into()));
    }
    let cache = data_dir.join("fonts").join("cache");
    let path = cache.join(format!(
        "{}-{weight}{}.ttf",
        slug(family),
        if italic { "i" } else { "" }
    ));
    if path.exists() {
        return Ok(fs::read(path)?);
    }
    let bytes = fetch_from_google(family, weight, italic)?;
    fs::create_dir_all(&cache)?;
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, &bytes)?;
    fs::rename(&tmp, &path)?;
    Ok(bytes)
}

fn fetch_from_google(family: &str, weight: u16, italic: bool) -> Result<Vec<u8>, StoreError> {
    let agent = ureq::AgentBuilder::new().try_proxy_from_env(true).build();
    let css_url = format!(
        "https://fonts.googleapis.com/css2?family={}:ital,wght@{},{weight}",
        family.replace(' ', "+"),
        u8::from(italic)
    );
    let css = agent
        .get(&css_url)
        // A user agent Google Fonts answers with TrueType rather than WOFF2.
        .set("User-Agent", "curl/8.0")
        .call()
        .map_err(|e| StoreError::Io(format!("Google Fonts refused {family}: {e}")))?
        .into_string()
        .map_err(|e| StoreError::Io(e.to_string()))?;
    let url = truetype_url(&css).ok_or_else(|| {
        StoreError::Io(format!(
            "Google Fonts has no TrueType file for {family} at weight {weight}"
        ))
    })?;
    let mut bytes = Vec::new();
    agent
        .get(&url)
        .call()
        .map_err(|e| StoreError::Io(format!("fetching {family}: {e}")))?
        .into_reader()
        .take(32 * 1024 * 1024)
        .read_to_end(&mut bytes)?;
    if bytes.is_empty() {
        return Err(StoreError::Io(format!("{family} came back empty")));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_catalogue_is_open_licensed_and_sorted() {
        let fonts = catalogue();
        assert!(fonts.len() > 100);
        for (a, b) in fonts.iter().zip(fonts.iter().skip(1)) {
            assert!(a.family < b.family, "{} before {}", a.family, b.family);
        }
        for f in &fonts {
            assert!(
                [
                    "SIL Open Font License 1.1",
                    "Apache License 2.0",
                    "Ubuntu Font Licence 1.0"
                ]
                .contains(&f.license.as_str()),
                "{}",
                f.family
            );
            assert!(
                ["sans-serif", "serif", "display", "handwriting", "monospace"]
                    .contains(&f.category.as_str())
            );
            assert_eq!(f.source, "google");
        }
        assert!(fonts.iter().any(|f| f.family == "Open Sans"));
    }

    #[test]
    fn truetype_url_is_read_from_google_fonts_css() {
        let css = "@font-face {\n  font-family: 'Open Sans';\n  src: url(https://fonts.gstatic.com/s/opensans/v44/abc.woff2) format('woff2');\n}\n@font-face {\n  src: url(https://fonts.gstatic.com/s/opensans/v44/memSYaGs.ttf) format('truetype');\n}";
        assert_eq!(
            truetype_url(css).as_deref(),
            Some("https://fonts.gstatic.com/s/opensans/v44/memSYaGs.ttf")
        );
        assert_eq!(truetype_url("nothing here"), None);
        assert_eq!(truetype_url("src: url(https://evil.example/x.ttf)"), None);
        assert_eq!(slug("Open Sans"), "open-sans");
    }

    #[test]
    fn local_files_are_listed_and_served_and_bad_requests_refused() {
        let dir = tempfile::tempdir().unwrap();
        assert!(local_fonts(dir.path()).is_empty());
        fs::create_dir_all(local_dir(dir.path())).unwrap();
        fs::write(local_dir(dir.path()).join("House Face.ttf"), b"TTF").unwrap();
        fs::write(local_dir(dir.path()).join("notes.txt"), b"x").unwrap();
        let local = local_fonts(dir.path());
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].family, "House Face");
        assert_eq!(local[0].source, "local");
        assert_eq!(list(dir.path()).last().unwrap().family, "House Face");
        assert_eq!(
            font_file(dir.path(), "House Face", 400, false).unwrap(),
            b"TTF"
        );
        assert_eq!(
            font_file(dir.path(), "No Such", 400, false),
            Err(StoreError::NotFound)
        );
        assert!(matches!(
            font_file(dir.path(), "Open Sans", 450, false),
            Err(StoreError::Invalid(_))
        ));
        // A cached file is served without any network.
        let cache = dir.path().join("fonts").join("cache");
        fs::create_dir_all(&cache).unwrap();
        fs::write(cache.join("open-sans-700i.ttf"), b"CACHED").unwrap();
        assert_eq!(
            font_file(dir.path(), "Open Sans", 700, true).unwrap(),
            b"CACHED"
        );
    }
}
