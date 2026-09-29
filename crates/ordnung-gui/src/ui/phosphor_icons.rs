//! Every Phosphor (regular) icon by name, for the playlist icon picker.
//! Names are the kebab-case Phosphor names ("vinyl-record"), which is what
//! the catalog stores on a playlist; the glyphs are the `egui-phosphor`
//! crate's own `regular::ICONS` table, renamed from its "VINYL_RECORD".

use std::sync::OnceLock;

/// `(name, glyph)` for every icon, sorted by name.
pub fn icons() -> &'static [(String, &'static str)] {
    static TABLE: OnceLock<Vec<(String, &'static str)>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut table: Vec<_> = egui_phosphor::regular::ICONS
            .iter()
            .map(|(n, g)| (n.to_ascii_lowercase().replace('_', "-"), *g))
            .collect();
        table.sort_unstable();
        table
    })
}

/// The glyph for a stored icon name, or `None` when the name is unknown
/// (a catalog written by a build with a different icon set).
pub fn glyph(name: &str) -> Option<&'static str> {
    let icons = icons();
    icons
        .binary_search_by(|(n, _)| n.as_str().cmp(name))
        .ok()
        .map(|i| icons[i].1)
}

/// The glyph for an icon the app itself names (a compile-time literal such
/// as `"vinyl-record"`), so the sidebar's own marks come out of the same face
/// as the icons a user picks for a playlist. A misspelt name renders as the
/// replacement character rather than panicking, and the test below keeps
/// every name the app uses in the table.
pub fn named(name: &str) -> &'static str {
    glyph(name).unwrap_or("\u{FFFD}")
}

/// Every icon name the app reaches for by itself, in one place so the test
/// can check them and the next feature can see what is already in use.
pub mod app {
    pub const MUSIC_NOTE: &str = "music-note";
    pub const VINYL: &str = "vinyl-record";
    pub const HEART: &str = "heart";
    pub const SPARKLE: &str = "sparkle";
    pub const WARNING: &str = "warning";
    pub const EJECT: &str = "eject";
    pub const LIGHTNING: &str = "lightning";
    pub const PLUS: &str = "plus";
    pub const PACKAGE: &str = "package";
    pub const TAG: &str = "tag";
    pub const FOLDER: &str = "folder";
    pub const FILE_AUDIO: &str = "file-audio";
    pub const CARET_UP: &str = "caret-up";
    pub const CARET_DOWN: &str = "caret-down";

    #[cfg(test)]
    pub const ALL: &[&str] = &[
        MUSIC_NOTE, VINYL, HEART, SPARKLE, WARNING, EJECT, LIGHTNING, PLUS, PACKAGE, TAG, FOLDER,
        FILE_AUDIO, CARET_UP, CARET_DOWN,
    ];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_app_icon_is_in_the_table() {
        for name in app::ALL {
            assert!(glyph(name).is_some(), "{name} is not a Phosphor icon");
        }
    }

    #[test]
    fn kebab_names_find_the_crate_glyphs() {
        assert_eq!(glyph("vinyl-record"), Some(egui_phosphor::regular::VINYL_RECORD));
    }
}
