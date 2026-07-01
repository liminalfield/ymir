//! App-global user preferences: settings that belong to the person, not to any one project.
//!
//! Today this is the author profile (#106): the identity a user optionally attaches when they
//! save a subgraph to share it (their name, email, website, and documentation link). It is kept
//! here, separate from a project, because it follows the user across every project and is edited
//! once in Settings rather than per file.
//!
//! Stored as pretty JSON at `$XDG_CONFIG_HOME/ymir/preferences.json` (configuration, so the XDG
//! config base). Every field is optional and defaults to empty, so the file evolves additively
//! (a new setting is a new `#[serde(default)]` field) without a format version or migration; the
//! app always rewrites the whole file, so there is no older-writer to guard against.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The author identity attached to a shared subgraph. Every field is optional; a blank field is
/// simply omitted from a saved subgraph. Never populated from `git config` or the system account
/// without the user typing it, so identity is never leaked into a shared file by default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct AuthorProfile {
    /// The author's display name.
    #[serde(default)]
    pub name: String,
    /// A contact email.
    #[serde(default)]
    pub email: String,
    /// A personal or project website URL.
    #[serde(default)]
    pub website: String,
    /// A link to online documentation for the author's work.
    #[serde(default)]
    pub docs: String,
}

/// The root of the user's app-global settings. A thin container today (just the author profile),
/// named so future settings hang off it and the Settings dialog grows sections without renaming
/// the file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Preferences {
    /// The author identity used when saving a subgraph to the library.
    #[serde(default)]
    pub author: AuthorProfile,
}

/// The preferences file path (`$XDG_CONFIG_HOME/ymir/preferences.json`, or the `$HOME/.config`
/// fallback). `None` if no config base can be resolved, in which case preferences are in-memory
/// only for the session rather than an error.
pub(crate) fn preferences_path() -> Option<PathBuf> {
    crate::config_path(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
        "preferences.json",
    )
}

/// Reads preferences from `path`. Missing fields fall back to their defaults, so an older file
/// (or a hand-edited partial one) still loads.
///
/// # Errors
///
/// Returns a message if the file cannot be read or is not valid JSON.
pub(crate) fn read_preferences(path: &Path) -> Result<Preferences, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    serde_json::from_slice(&bytes).map_err(|e| e.to_string())
}

/// Writes preferences to `path` as pretty JSON (git-diffable), creating or truncating it. The
/// parent directory must already exist.
///
/// # Errors
///
/// Returns a message if serialization or the write fails.
pub(crate) fn write_preferences(path: &Path, prefs: &Preferences) -> Result<(), String> {
    let json = serde_json::to_string_pretty(prefs).map_err(|e| e.to_string())?;
    std::fs::write(path, json).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preferences_round_trip_through_json() {
        let prefs = Preferences {
            author: AuthorProfile {
                name: "Ada".to_string(),
                email: "ada@example.com".to_string(),
                website: "https://example.com".to_string(),
                docs: "https://example.com/docs".to_string(),
            },
        };
        let json = serde_json::to_string_pretty(&prefs).expect("serialize");
        let back: Preferences = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(prefs, back);
    }

    #[test]
    fn missing_fields_fall_back_to_defaults() {
        // A partial file (only a name) still loads, the rest defaulting to empty.
        let back: Preferences =
            serde_json::from_str(r#"{"author":{"name":"Ada"}}"#).expect("deserialize partial");
        assert_eq!(back.author.name, "Ada");
        assert!(back.author.email.is_empty());
        assert_eq!(back, {
            let mut p = Preferences::default();
            p.author.name = "Ada".to_string();
            p
        });
    }
}
