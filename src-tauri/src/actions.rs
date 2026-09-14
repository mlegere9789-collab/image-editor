//! The Actions panel's store: recorded actions, one JSON file each,
//! under the app's data directory.
//!
//! Photoshop's Actions panel keeps its recordings in the application's
//! preferences and has a long history of losing them — a crash, a reset,
//! an update, and every action is gone. Here each action is its own file,
//! written atomically the moment a step is recorded, so nothing recorded
//! is ever only in memory, and a broken file costs that one action, not
//! the rest.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One step of an action: a command as the app ran it, or a stop that
/// pauses playback with a message (Photoshop's Insert Stop).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Step {
    /// A command with the arguments it ran with. The frontend records
    /// the selected layer's id as the string `"$selected"`, so playback
    /// targets whichever layer is selected then, as Photoshop's do.
    Command {
        command: String,
        args: serde_json::Value,
    },
    /// Playback pauses here and shows `message`, with Continue and Stop.
    Stop { message: String },
}

/// A recorded action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Action {
    pub name: String,
    pub steps: Vec<Step>,
}

/// Checks a name is fit to be a file name on every platform: 1–64
/// characters of letters, digits, spaces, `-`, `_`, and `.`, not all
/// spaces or dots.
pub fn validate_name(name: &str) -> Result<(), String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("An action needs a name.".to_string());
    }
    if name.chars().count() > 64 {
        return Err("An action's name is at most 64 characters.".to_string());
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'))
    {
        return Err(
            "An action's name uses letters, digits, spaces, '-', '_', and '.' only.".to_string(),
        );
    }
    if trimmed.chars().all(|c| c == '.') {
        return Err("That is not a usable action name.".to_string());
    }
    Ok(())
}

fn actions_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("actions")
}

fn action_path(data_dir: &Path, name: &str) -> PathBuf {
    actions_dir(data_dir).join(format!("{}.json", name.trim()))
}

/// Writes `action` as `actions/<name>.json` under `data_dir`, replacing
/// any action of that name, through a temporary file and a rename so a
/// crash mid-write leaves the previous file intact.
pub fn save(data_dir: &Path, action: &Action) -> Result<(), String> {
    validate_name(&action.name)?;
    let dir = actions_dir(data_dir);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    let bytes = serde_json::to_vec_pretty(action)
        .map_err(|e| format!("Could not encode the action: {e}"))?;
    let path = action_path(data_dir, &action.name);
    let tmp = dir.join(format!("{}.json.tmp", action.name.trim()));
    std::fs::write(&tmp, bytes).map_err(|e| format!("Could not write {}: {e}", tmp.display()))?;
    std::fs::rename(&tmp, &path)
        .map_err(|e| format!("Could not replace {}: {e}", path.display()))?;
    Ok(())
}

/// Every action under `data_dir`, by name. A file that no longer parses
/// is skipped, never fatal — one broken action does not hide the rest.
pub fn list(data_dir: &Path) -> Result<Vec<Action>, String> {
    let dir = actions_dir(data_dir);
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("Could not read {}: {e}", dir.display())),
    };
    let mut actions = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        if let Some(action) = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Action>(&bytes).ok())
        {
            actions.push(action);
        }
    }
    actions.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(actions)
}

/// Deletes the action called `name`; deleting one that is not there is
/// not an error.
pub fn remove(data_dir: &Path, name: &str) -> Result<(), String> {
    validate_name(name)?;
    let path = action_path(data_dir, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Could not delete {}: {e}", path.display())),
    }
}

/// The PNG files directly inside `dir`, sorted by name — what File >
/// Automate > Batch plays an action over.
pub fn list_pngs(dir: &Path) -> Result<Vec<PathBuf>, String> {
    let entries =
        std::fs::read_dir(dir).map_err(|e| format!("Could not read {}: {e}", dir.display()))?;
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("png"))
        })
        .collect();
    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("image-editor-actions-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn action(name: &str) -> Action {
        Action {
            name: name.to_string(),
            steps: vec![
                Step::Command {
                    command: "brightness_contrast".to_string(),
                    args: serde_json::json!({ "id": "$selected", "brightness": 20, "contrast": 0 }),
                },
                Step::Stop {
                    message: "Check the exposure".to_string(),
                },
                Step::Command {
                    command: "gaussian_blur".to_string(),
                    args: serde_json::json!({ "id": "$selected", "radius": 1.5 }),
                },
            ],
        }
    }

    #[test]
    fn actions_round_trip_through_their_own_files_and_list_by_name() {
        let dir = temp_dir("round-trip");
        assert_eq!(list(&dir).unwrap(), Vec::<Action>::new());
        save(&dir, &action("Soften")).unwrap();
        save(&dir, &action("brighten")).unwrap();
        save(&dir, &action("Zebra")).unwrap();
        let listed = list(&dir).unwrap();
        assert_eq!(
            listed.iter().map(|a| a.name.as_str()).collect::<Vec<_>>(),
            vec!["brighten", "Soften", "Zebra"]
        );
        assert_eq!(listed[1], action("Soften"));
        // Saving again replaces, leaving one file and no temporary.
        let mut changed = action("Soften");
        changed.steps.pop();
        save(&dir, &changed).unwrap();
        let listed = list(&dir).unwrap();
        assert_eq!(listed.len(), 3);
        assert_eq!(listed[1].steps.len(), 2);
        assert!(!dir.join("actions").join("Soften.json.tmp").exists());
        // The file is plain JSON with the step kinds spelled out.
        let text = std::fs::read_to_string(dir.join("actions").join("Zebra.json")).unwrap();
        assert!(text.contains("\"kind\": \"command\""));
        assert!(text.contains("\"kind\": \"stop\""));
        assert!(text.contains("\"$selected\""));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_broken_file_is_skipped_and_remove_is_quiet_about_a_missing_one() {
        let dir = temp_dir("broken");
        save(&dir, &action("Good")).unwrap();
        std::fs::write(dir.join("actions").join("Bad.json"), b"{not json").unwrap();
        std::fs::write(dir.join("actions").join("notes.txt"), b"ignored").unwrap();
        let listed = list(&dir).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name, "Good");
        remove(&dir, "Good").unwrap();
        remove(&dir, "Good").unwrap();
        assert_eq!(list(&dir).unwrap(), Vec::<Action>::new());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_must_be_safe_file_names() {
        assert!(validate_name("Soften edges v2.1").is_ok());
        assert!(validate_name("").is_err());
        assert!(validate_name("   ").is_err());
        assert!(validate_name("..").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("a\\b").is_err());
        assert!(validate_name("Café").is_err());
        assert!(validate_name(&"x".repeat(65)).is_err());
        let dir = temp_dir("names");
        assert!(save(&dir, &action("../escape")).is_err());
        assert!(remove(&dir, "").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn list_pngs_finds_the_pngs_directly_inside_a_folder_in_order() {
        let dir = temp_dir("pngs");
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        for name in ["b.png", "A.PNG", "c.jpg", "notes.txt"] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        std::fs::write(dir.join("nested").join("d.png"), b"").unwrap();
        let files: Vec<String> = list_pngs(&dir)
            .unwrap()
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(files, vec!["A.PNG", "b.png"]);
        assert!(list_pngs(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
