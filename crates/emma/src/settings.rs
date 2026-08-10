//! `~/.emma/settings.json` — the small amount of state that belongs to the
//! person rather than to the project.
//!
//! Today that is one field. It lives beside `credentials.json` under the home
//! directory and **never** in the working tree, for the same reason the key
//! does: Emma runs inside repositories, and anything it writes next to the code
//! is something the next `git add .` publishes. A model id is not a secret, but
//! a file Emma creates in someone's repository without being asked is a
//! surprise either way.
//!
//! Deliberately *not* the harness. `.emma/config.json` is the project's
//! configuration, read by `emma-harness`, and putting a personal default in it
//! would mean two files answering "what is the model" with no rule for
//! disagreeing.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

pub fn path(home: &Path) -> PathBuf {
    home.join(".emma").join("settings.json")
}

/// A missing or unreadable file is an empty `Settings`, not an error: this is a
/// preference store, and refusing to start because a preference could not be
/// read would be the tail wagging the dog.
pub fn load(home: &Path) -> Settings {
    std::fs::read_to_string(path(home))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

pub fn save(home: &Path, settings: &Settings) -> Result<PathBuf> {
    let path = path(home);
    let dir = path.parent().expect("settings path always has a parent");
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let body = serde_json::to_string_pretty(settings)?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(path)
}

/// Which model to use, and where the answer came from — the second half
/// matters, because "emma is using the wrong model" is otherwise a question
/// with no way to ask it.
pub fn resolve(flag: Option<&str>, home: Option<&Path>) -> (String, &'static str) {
    if let Some(model) = flag {
        return (model.to_string(), "--model");
    }
    if let Some(model) = home.and_then(|h| load(h).model) {
        return (model, "settings.json");
    }
    (emma_llm::DEFAULT_MODEL.to_string(), "built-in default")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_saved_model_round_trips_and_the_flag_still_wins() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(resolve(None, Some(home.path())).1, "built-in default");

        save(
            home.path(),
            &Settings {
                model: Some("claude-something".into()),
            },
        )
        .unwrap();
        assert_eq!(
            resolve(None, Some(home.path())),
            ("claude-something".into(), "settings.json")
        );
        // A one-run override must not be quietly outranked by a preference the
        // user set six weeks ago.
        assert_eq!(
            resolve(Some("other"), Some(home.path())),
            ("other".into(), "--model")
        );
    }

    #[test]
    fn a_corrupt_settings_file_is_not_a_startup_failure() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(home.path().join(".emma")).unwrap();
        std::fs::write(path(home.path()), "{ not json").unwrap();
        assert_eq!(resolve(None, Some(home.path())).1, "built-in default");
    }
}
