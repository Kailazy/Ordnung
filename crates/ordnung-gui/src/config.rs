//! Persistent GUI settings, stored at `~/.ordnung/config.toml` (next to the
//! catalog). Policy and process I/O live in the GUI boundary per
//! `ordnung-architecture`; `ordnung-core` stays pure.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// User settings that must survive across launches — including launches from
/// Finder/Dock, which inherit none of the shell environment. Currently just the
/// Discogs token; extend in place as more settings appear.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Discogs personal access token. Empty means "not set" — callers then fall
    /// back to the `DISCOGS_TOKEN` environment variable.
    #[serde(default)]
    pub discogs_token: String,
    /// Discogs username of the token owner, captured on the first collection
    /// sync. Lets the "My Vinyl Collection" view link to the user's collection
    /// page across launches without re-resolving it. Empty until a sync runs.
    #[serde(default)]
    pub discogs_username: String,
    /// Track-table column order as stable column keys (see `TableColumn::key`).
    /// Empty means "use the default order". Tolerant of unknown or missing keys
    /// on load, so a config from an older build keeps working as columns change.
    #[serde(default)]
    pub column_order: Vec<String>,
    /// Track-table columns the user has hidden, as stable column keys.
    #[serde(default)]
    pub hidden_columns: Vec<String>,
}

impl Config {
    /// Load settings from disk, or return defaults if the file is missing or
    /// unreadable. Never fails: a broken/absent config simply yields defaults.
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|text| toml::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Persist settings to `~/.ordnung/config.toml`, creating the directory if
    /// needed. Returns a user-facing error string on failure.
    pub fn save(&self) -> Result<(), String> {
        let path = config_path().ok_or("could not resolve HOME for config path")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        let text = toml::to_string_pretty(self).map_err(|e| e.to_string())?;
        std::fs::write(&path, text).map_err(|e| e.to_string())
    }
}

/// `~/.ordnung/config.toml` — same directory as the catalog database.
pub fn config_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".ordnung").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Proves the token survives a save → fresh-load cycle (the whole point of
    /// the feature). Uses a throwaway HOME so it touches no real config.
    #[test]
    fn token_round_trips_through_disk() {
        let tmp = std::env::temp_dir().join(format!("ordnung-cfg-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // SAFETY: single-threaded test; we restore HOME before returning.
        let prev = std::env::var_os("HOME");
        std::env::set_var("HOME", &tmp);

        let cfg = Config {
            discogs_token: "secret-token-123".into(),
            ..Config::default()
        };
        cfg.save().unwrap();

        // A brand-new load (no shared state) must see the saved token.
        let loaded = Config::load();
        assert_eq!(loaded.discogs_token, "secret-token-123");

        // The file really lives at ~/.ordnung/config.toml.
        assert!(tmp.join(".ordnung/config.toml").exists());

        match prev {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        std::fs::remove_dir_all(&tmp).ok();
    }
}
