//! Crash-safe autosave and recovery: one recovery file per app data
//! directory, written atomically, read back at launch. Photoshop's own
//! pain point -- an unsaved document lost to a crash -- answered the way
//! its Auto Save answers it: the work is on disk every interval, and the
//! next launch offers it back.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

const FILE: &str = "recovery.imgproj";

fn path(dir: &Path) -> PathBuf {
    dir.join("autosave").join(FILE)
}

/// What the recovery file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    /// Unix seconds.
    pub modified_at: u64,
    pub bytes: u64,
}

/// Writes `bytes` as the recovery file: to a temporary name first, then
/// renamed into place, so a reader never sees a partial file. Returns
/// the size written.
pub fn write(dir: &Path, bytes: &[u8]) -> Result<u64, String> {
    let target = path(dir);
    let parent = target.parent().expect("autosave dir");
    fs::create_dir_all(parent)
        .map_err(|e| format!("Could not create {}: {e}", parent.display()))?;
    let tmp = target.with_extension("imgproj.tmp");
    fs::write(&tmp, bytes).map_err(|e| format!("Could not write the autosave: {e}"))?;
    fs::rename(&tmp, &target).map_err(|e| format!("Could not place the autosave: {e}"))?;
    Ok(bytes.len() as u64)
}

pub fn status(dir: &Path) -> Option<Status> {
    let meta = fs::metadata(path(dir)).ok()?;
    let modified_at = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(Status {
        modified_at,
        bytes: meta.len(),
    })
}

pub fn read(dir: &Path) -> Result<Vec<u8>, String> {
    fs::read(path(dir)).map_err(|e| format!("No recovery file to recover: {e}"))
}

/// Removes the recovery file; nothing to remove is not an error.
pub fn discard(dir: &Path) -> Result<(), String> {
    match fs::remove_file(path(dir)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not discard the autosave: {e}")),
    }
}

/// Now, in unix seconds -- for the frontend's "autosaved N seconds ago".
pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_recovery_file_round_trips_atomically_and_discards_cleanly() {
        let dir =
            std::env::temp_dir().join(format!("image-editor-autosave-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(status(&dir).is_none());
        assert!(read(&dir).is_err());
        discard(&dir).unwrap();
        assert_eq!(write(&dir, b"IEDP1 first").unwrap(), 11);
        let first = status(&dir).unwrap();
        assert_eq!(first.bytes, 11);
        assert!(first.modified_at > 1_600_000_000);
        assert_eq!(read(&dir).unwrap(), b"IEDP1 first");
        // A second write replaces the first, and no temporary file is left.
        write(&dir, b"IEDP1 second, longer").unwrap();
        assert_eq!(read(&dir).unwrap(), b"IEDP1 second, longer");
        assert_eq!(status(&dir).unwrap().bytes, 20);
        assert!(!dir.join("autosave").join("recovery.imgproj.tmp").exists());
        discard(&dir).unwrap();
        assert!(status(&dir).is_none());
        discard(&dir).unwrap();
        let _ = fs::remove_dir_all(&dir);
    }
}
