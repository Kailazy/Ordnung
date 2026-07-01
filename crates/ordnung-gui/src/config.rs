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
    /// Waveform smoothing strength `[0, 1]`: scales the attack/release time
    /// constants below from `0` (raw envelope) to their full values, so one knob
    /// sweeps raw → fully smoothed. See `smooth_source` and
    /// `WaveformStyle::smoothing`.
    #[serde(default = "default_waveform_smoothing")]
    pub waveform_smoothing: f32,
    /// Waveform smoothing attack time constant (ms of audio) at full smoothing:
    /// how much a *rising* edge is rounded. A few ms irons out pixel-scale
    /// jaggies while keeping transient onsets crisp. See `smooth_source`.
    #[serde(default = "default_waveform_smooth_attack_ms")]
    pub waveform_smooth_attack_ms: f32,
    /// Waveform smoothing release time constant (ms of audio) at full smoothing:
    /// how long a *falling* tail rings out. Beat-scale (~450 ms) keeps a kick's
    /// tail standing until the next kick so the envelope reads as a connected
    /// silhouette; short values let it pinch to the centerline between beats
    /// (separate petals). See `smooth_source`.
    #[serde(default = "default_waveform_smooth_release_ms")]
    pub waveform_smooth_release_ms: f32,
    /// Bass floor threshold `[0, 1]` (fraction of full scale): low-band content
    /// quieter than this is treated as sustained sub (the tail lingering under a
    /// kick) rather than a transient peak, and is dimmed by
    /// `waveform_bass_floor_amount`. Louder bass (kick attacks) is kept at full
    /// height. See `bass_floor_gain`.
    #[serde(default = "default_waveform_bass_floor_threshold")]
    pub waveform_bass_floor_threshold: f32,
    /// How much to dim sustained sub below `waveform_bass_floor_threshold`:
    /// `0` keeps it (no change), `1` removes it entirely, leaving only bass
    /// transients. See `bass_floor_gain`.
    #[serde(default = "default_waveform_bass_floor_amount")]
    pub waveform_bass_floor_amount: f32,
    /// Saved snapshots of the Waveform settings tab (Settings → Waveform →
    /// Presets), keyed by their 1-based `slot`. At most one entry per slot; a
    /// plain `Vec` (not `[Option<_>; 5]`) because TOML can't represent `None`
    /// holes in an array.
    #[serde(default)]
    pub waveform_presets: Vec<WaveformPreset>,
}

/// One saved snapshot of every tunable on the Waveform settings tab. Saving to
/// an occupied slot overwrites it; loading applies the whole snapshot at once,
/// so an in-progress tweak can be parked and recalled without redialing each
/// slider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformPreset {
    /// 1-based UI slot this preset lives in.
    pub slot: u8,
    pub color_mode: String,
    pub height_exp: f32,
    pub band_gain: [f32; 3],
    pub energy_gain: f32,
    pub band_colors: [[u8; 3]; 3],
    pub energy_colors: [[u8; 3]; 5],
    pub low_hz: f32,
    pub mid_hz: f32,
    pub smoothing: f32,
    pub smooth_attack_ms: f32,
    pub smooth_release_ms: f32,
    pub bass_floor_threshold: f32,
    pub bass_floor_amount: f32,
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

pub(crate) fn default_waveform_smooth_attack_ms() -> f32 {
    4.0
}

pub(crate) fn default_waveform_smooth_release_ms() -> f32 {
    450.0
}

pub(crate) fn default_waveform_bass_floor_threshold() -> f32 {
    0.35
}

pub(crate) fn default_waveform_bass_floor_amount() -> f32 {
    0.0
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
            waveform_smooth_attack_ms: default_waveform_smooth_attack_ms(),
            waveform_smooth_release_ms: default_waveform_smooth_release_ms(),
            waveform_bass_floor_threshold: default_waveform_bass_floor_threshold(),
            waveform_bass_floor_amount: default_waveform_bass_floor_amount(),
            waveform_presets: Vec::new(),
        }
    }
}

impl Config {
    /// The waveform preset saved in 1-based `slot`, if any.
    pub fn waveform_preset(&self, slot: u8) -> Option<&WaveformPreset> {
        self.waveform_presets.iter().find(|p| p.slot == slot)
    }

    /// Snapshot the current waveform settings into `slot`, overwriting whatever
    /// was there. Caller persists with [`Config::save`].
    pub fn save_waveform_preset(&mut self, slot: u8) {
        let preset = WaveformPreset {
            slot,
            color_mode: self.waveform_color_mode.clone(),
            height_exp: self.waveform_height_exp,
            band_gain: self.waveform_band_gain,
            energy_gain: self.waveform_energy_gain,
            band_colors: self.waveform_band_colors,
            energy_colors: self.waveform_energy_colors,
            low_hz: self.waveform_low_hz,
            mid_hz: self.waveform_mid_hz,
            smoothing: self.waveform_smoothing,
            smooth_attack_ms: self.waveform_smooth_attack_ms,
            smooth_release_ms: self.waveform_smooth_release_ms,
            bass_floor_threshold: self.waveform_bass_floor_threshold,
            bass_floor_amount: self.waveform_bass_floor_amount,
        };
        self.waveform_presets.retain(|p| p.slot != slot);
        self.waveform_presets.push(preset);
        // Keep the saved TOML stable regardless of save order.
        self.waveform_presets.sort_by_key(|p| p.slot);
    }

    /// Apply the preset in `slot` to the live settings. Returns whether the
    /// band crossovers changed — the caller must then invalidate the loaded
    /// track's hi-res bands so the zoom lane recomputes — or `None` if the slot
    /// is empty.
    pub fn load_waveform_preset(&mut self, slot: u8) -> Option<bool> {
        let p = self.waveform_preset(slot)?.clone();
        let freq_changed =
            p.low_hz != self.waveform_low_hz || p.mid_hz != self.waveform_mid_hz;
        self.waveform_color_mode = p.color_mode;
        self.waveform_height_exp = p.height_exp;
        self.waveform_band_gain = p.band_gain;
        self.waveform_energy_gain = p.energy_gain;
        self.waveform_band_colors = p.band_colors;
        self.waveform_energy_colors = p.energy_colors;
        self.waveform_low_hz = p.low_hz;
        self.waveform_mid_hz = p.mid_hz;
        self.waveform_smoothing = p.smoothing;
        self.waveform_smooth_attack_ms = p.smooth_attack_ms;
        self.waveform_smooth_release_ms = p.smooth_release_ms;
        self.waveform_bass_floor_threshold = p.bass_floor_threshold;
        self.waveform_bass_floor_amount = p.bass_floor_amount;
        Some(freq_changed)
    }

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
