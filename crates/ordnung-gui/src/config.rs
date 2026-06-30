//! Persistent GUI settings, stored at `~/.ordnung/config.toml` (next to the
//! catalog). Policy and process I/O live in the GUI boundary per
//! `ordnung-architecture`; `ordnung-core` stays pure.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// User settings that must survive across launches — including launches from
/// Finder/Dock, which inherit none of the shell environment. Currently just the
/// Discogs token; extend in place as more settings appear.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Track-table column widths in points, keyed by stable column key (see
    /// `TableColumn::key`). Shared across every view (library and all playlists)
    /// and durable across rebuilds — unlike egui's own per-layout width memory,
    /// whose id shifts when the surrounding UI changes. Missing keys fall back to
    /// the per-column default width. A `BTreeMap` so the saved TOML is stable.
    #[serde(default)]
    pub column_widths: BTreeMap<String, f32>,
    /// Sort applied to the track table on launch, as a stable column key (see
    /// `TableColumn::key`). Empty (the default) means "natural order" — catalog
    /// or playlist order, the prior behavior. Unknown or unsortable keys also
    /// fall back to natural order.
    #[serde(default)]
    pub default_sort: String,
    /// Direction for `default_sort` (`true` = ascending). Ignored when
    /// `default_sort` is empty.
    #[serde(default = "default_true")]
    pub default_sort_ascending: bool,
    /// Run analysis (BPM, key, waveform) automatically on each track as it's
    /// imported, instead of waiting for the explicit "Analyze" action. On by
    /// default; defaults to on for older configs that predate the field too.
    #[serde(default = "default_true")]
    pub auto_analyze: bool,
    /// Default target format pre-selected in the convert dialogs, as a stable
    /// lowercase key (`mp3`/`aac`/`flac`/`wav`/`aiff`; see `util::format_key`).
    /// Empty or unknown falls back to AIFF, the prior hard-coded default.
    #[serde(default = "default_convert_format")]
    pub convert_format: String,
    /// Bitrate (kbps) prefilled for lossy convert targets (MP3/AAC), as the text
    /// shown in the field. Empty means "use the per-format hint" (320 / 256).
    #[serde(default)]
    pub convert_bitrate_kbps: String,
    /// Default output folder for conversions. `None` (the default) means
    /// "alongside each source file".
    #[serde(default)]
    pub convert_out_dir: Option<PathBuf>,
    /// Whether the convert dialogs default to replacing the source file in place.
    /// On by default, preserving the prior hard-coded behavior.
    #[serde(default = "default_true")]
    pub convert_in_place: bool,
    /// How the player's waveform is colored: `"energy"` (cool→hot gradient by the
    /// loudness of each section) or `"spectrum"` (additive RGB from the low/mid/
    /// high band balance, like rekordbox/Serato). Unknown values fall back to
    /// `"energy"`. See `WaveformColorMode`.
    #[serde(default = "default_waveform_color_mode")]
    pub waveform_color_mode: String,
    /// Render-time height companding for the waveform. `1.0` keeps the stored
    /// sqrt-companded amplitude (most compressed); `2.0` cancels the sqrt back to
    /// linear amplitude (least compressed, rekordbox-like). See `wave_height`.
    #[serde(default = "default_waveform_height_exp")]
    pub waveform_height_exp: f32,
    /// Per-band visual height gain for spectrum mode `[low, mid, high]`. The bass
    /// band swamps the others, so the default trims it and lifts mid/high.
    #[serde(default = "default_waveform_band_gain")]
    pub waveform_band_gain: [f32; 3],
    /// Visual height gain for the single envelope in energy mode. `1.0` keeps the
    /// stored amplitude; lower trims, higher lifts. The spectrum-mode equivalent
    /// is `waveform_band_gain`.
    #[serde(default = "default_waveform_energy_gain")]
    pub waveform_energy_gain: f32,
    /// RGB colors for the three spectrum bands `[low, mid, high]`. Defaults to the
    /// Serato/rekordbox convention (low = red, mid = green, high = light blue).
    #[serde(default = "default_waveform_band_colors")]
    pub waveform_band_colors: [[u8; 3]; 3],
    /// RGB stops for the energy-mode cool→hot gradient, quiet → loudest (5 stops).
    #[serde(default = "default_waveform_energy_colors")]
    pub waveform_energy_colors: [[u8; 3]; 5],
    /// Low/mid band crossover (Hz) for the zoom detail lane's live hi-res bands.
    /// Lower it toward kick + sub so low-mid energy stays out of the bass band.
    /// Only the zoom lane honors this live; the full-track overview uses the split
    /// baked in at analysis time. See `compute_hires_bands`.
    #[serde(default = "default_waveform_low_hz")]
    pub waveform_low_hz: f32,
    /// Mid/high band crossover (Hz) for the zoom detail lane's live hi-res bands.
    /// Everything above this reads as the high band. See `compute_hires_bands`.
    #[serde(default = "default_waveform_mid_hz")]
    pub waveform_mid_hz: f32,
    /// Bar smoothing `[0, 1]` for the zoom detail lane: blends each bar's height
    /// with its neighbors so the envelope reads as a continuous curve
    /// (rekordbox-style) instead of showing every dip between bins. `0` = raw
    /// bars. See `smooth_aggs` and `WaveformStyle::smoothing`.
    #[serde(default = "default_waveform_smoothing")]
    pub waveform_smoothing: f32,
}

fn default_true() -> bool {
    true
}

fn default_convert_format() -> String {
    "aiff".to_string()
}

fn default_waveform_color_mode() -> String {
    "energy".to_string()
}

pub(crate) fn default_waveform_height_exp() -> f32 {
    2.0
}

pub(crate) fn default_waveform_band_gain() -> [f32; 3] {
    [0.78, 1.2, 1.35]
}

pub(crate) fn default_waveform_energy_gain() -> f32 {
    1.0
}

pub(crate) fn default_waveform_band_colors() -> [[u8; 3]; 3] {
    [[232, 76, 60], [95, 200, 95], [95, 175, 235]]
}

pub(crate) fn default_waveform_low_hz() -> f32 {
    120.0
}

pub(crate) fn default_waveform_mid_hz() -> f32 {
    2000.0
}

pub(crate) fn default_waveform_smoothing() -> f32 {
    0.5
}

pub(crate) fn default_waveform_energy_colors() -> [[u8; 3]; 5] {
    [
        [45, 80, 150],
        [40, 160, 170],
        [70, 190, 110],
        [235, 195, 70],
        [225, 75, 55],
    ]
}

/// How the player waveform is colored. Parsed from `Config::waveform_color_mode`;
/// presentation policy, so it lives in the GUI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaveformColorMode {
    /// Cool→hot gradient driven by each section's total energy.
    Energy,
    /// Additive RGB from the low/mid/high band balance (rekordbox/Serato style).
    Spectrum,
}

impl WaveformColorMode {
    /// Parse a config string; anything unrecognized falls back to `Energy`.
    pub fn from_key(key: &str) -> Self {
        match key {
            "spectrum" => WaveformColorMode::Spectrum,
            _ => WaveformColorMode::Energy,
        }
    }

    /// Stable lowercase key stored in the config TOML.
    pub fn key(self) -> &'static str {
        match self {
            WaveformColorMode::Energy => "energy",
            WaveformColorMode::Spectrum => "spectrum",
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            discogs_token: String::new(),
            discogs_username: String::new(),
            column_order: Vec::new(),
            hidden_columns: Vec::new(),
            column_widths: BTreeMap::new(),
            default_sort: String::new(),
            default_sort_ascending: true,
            auto_analyze: true,
            convert_format: default_convert_format(),
            convert_bitrate_kbps: String::new(),
            convert_out_dir: None,
            convert_in_place: true,
            waveform_color_mode: default_waveform_color_mode(),
            waveform_height_exp: default_waveform_height_exp(),
            waveform_band_gain: default_waveform_band_gain(),
            waveform_energy_gain: default_waveform_energy_gain(),
            waveform_band_colors: default_waveform_band_colors(),
            waveform_energy_colors: default_waveform_energy_colors(),
            waveform_low_hz: default_waveform_low_hz(),
            waveform_mid_hz: default_waveform_mid_hz(),
            waveform_smoothing: default_waveform_smoothing(),
        }
    }
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
