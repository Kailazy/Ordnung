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
    /// Release carriers to hide in the Discogs release picker, as stable
    /// [`ReleaseMedium`] keys. Empty (the default, and what older configs get)
    /// means show every format.
    ///
    /// Stored as what's *hidden* rather than what's shown so the default stays
    /// "show everything" and a medium added in a later build appears without
    /// needing a config migration. A collector who only buys records hides
    /// `cd`/`digital`/`cassette` and stops scrolling past pressings they'd
    /// never want.
    #[serde(default)]
    pub hidden_release_mediums: Vec<String>,
    /// Discogs username of the token owner, captured on the first collection
    /// sync. Lets the "Vinyl Collection" view link to the user's collection
    /// page across launches without re-resolving it. Empty until a sync runs.
    #[serde(default)]
    pub discogs_username: String,
    /// The folder the user's music library lives in — the persistent answer to
    /// "where does my music come from?". Picked in the welcome tour (or
    /// Settings → General) and used to kick off the first import; later it is
    /// what makes "scan for new arrivals" possible without re-picking a folder
    /// every time. `None` — the default, and what every config predating the
    /// field gets — means no root is set and music arrives only via
    /// "Add songs…" or drag-drop, exactly as before.
    #[serde(default)]
    pub library_root: Option<PathBuf>,
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
    /// How the "Vinyl Collection" grid is ordered: `"added"` (Discogs date
    /// added), `"price"` (lowest marketplace listing), or `"artist"`. Unknown
    /// values fall back to artist, the original fixed order. See `VinylSort`.
    #[serde(default = "default_vinyl_sort")]
    pub vinyl_sort: String,
    /// Direction for `vinyl_sort` (`true` = ascending: oldest / cheapest / A–Z
    /// first). Defaults to descending, so the default view is newest first.
    #[serde(default)]
    pub vinyl_sort_ascending: bool,
    /// Which library sits at the top of the left navigation sidebar:
    /// `"digital"` (Library / New / playlists first, the default) or
    /// `"vinyl"` (the Discogs vinyl collection first). A vinyl-led collector
    /// gets their shelf up top and the digital library pinned below. Unknown
    /// values fall back to `"digital"`. See `NavPrimary`.
    #[serde(default = "default_nav_primary")]
    pub nav_primary: String,
    /// Which of the sidebar's three width tiers is in force: `"icon"`,
    /// `"narrow"` or `"wide"` (the default). The sidebar snaps between designed
    /// layouts rather than resizing freely, so what persists is the chosen tier,
    /// not a pixel width. Unknown values fall back to `"wide"`. See `NavDensity`.
    #[serde(default = "default_nav_density")]
    pub nav_density: String,
    /// Which section the app opens on: `"library"` (Library, the default),
    /// `"vinyl"` (the vinyl collection), or `"recent"` (new imports). Unknown
    /// values fall back to `"library"`. See `StartupView`.
    #[serde(default = "default_startup_view")]
    pub startup_view: String,
    /// Run analysis (BPM, key, waveform) automatically on each track as it's
    /// imported, instead of waiting for the explicit "Analyze" action. On by
    /// default; defaults to on for older configs that predate the field too.
    #[serde(default = "default_true")]
    pub auto_analyze: bool,
    /// Write tag edits straight into the source files instead of parking them
    /// behind the toolbar's "Write N edited to files" button. When on, the
    /// inspector's Save writes the file too, and edits made elsewhere (Discogs
    /// enrichment, bulk fetches, fetched cover art) are flushed by a background
    /// write as soon as no other job is running.
    ///
    /// **On by default**, at the user's explicit direction. This is the one
    /// place the GUI departs from the `ordnung-architecture` "tag writeback is
    /// opt-in" rule: a catalog edit that never reaches the file is a surprise,
    /// not a safeguard, so the app keeps files in sync unless told otherwise.
    /// Only ever touches the tag block of tracks the user actually edited, and
    /// turning it off restores the explicit two-step. Older configs that predate
    /// the field get the new default too.
    #[serde(default = "default_true")]
    pub auto_write_tags: bool,
    /// Version of the first-run welcome tour this install has completed (see
    /// `onboarding::TOUR_VERSION`). `0` — the default, and what every config
    /// predating the tour gets — means "never seen it", which is what opens the
    /// tour on a fresh install. Storing a version rather than a bool lets a
    /// materially changed tour show once more to existing users.
    #[serde(default)]
    pub onboarding_completed_version: u32,
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
    /// Master playback volume as a linear amplitude factor, `0.0`–`1.0`. Driven
    /// by the toolbar knob and restored at startup so the app comes back at the
    /// level it was left at.
    #[serde(default = "default_volume")]
    pub volume: f32,
    /// How the player's waveform is colored: `"energy"` (cool→hot gradient by
    /// each section's energy — perceived loudness × spectral occupancy) or
    /// `"spectrum"` (additive RGB from the low/mid/high band balance, like
    /// rekordbox/Serato). Unknown values fall back to `"energy"`. See
    /// `WaveformColorMode`.
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

fn default_vinyl_sort() -> String {
    "added".to_string()
}

fn default_nav_density() -> String {
    "wide".into()
}

fn default_nav_primary() -> String {
    "digital".to_string()
}

fn default_startup_view() -> String {
    "library".to_string()
}

pub(crate) fn default_volume() -> f32 {
    1.0
}

fn default_waveform_color_mode() -> String {
    "energy".to_string()
}

pub(crate) fn default_waveform_height_exp() -> f32 {
    1.01
}

pub(crate) fn default_waveform_band_gain() -> [f32; 3] {
    [1.0, 0.85, 0.38]
}

pub(crate) fn default_waveform_energy_gain() -> f32 {
    0.9
}

pub(crate) fn default_waveform_band_colors() -> [[u8; 3]; 3] {
    [[0, 50, 255], [207, 156, 42], [230, 241, 255]]
}

pub(crate) fn default_waveform_low_hz() -> f32 {
    120.0
}

pub(crate) fn default_waveform_mid_hz() -> f32 {
    2000.0
}

pub(crate) fn default_waveform_smoothing() -> f32 {
    0.79
}

pub(crate) fn default_waveform_smooth_attack_ms() -> f32 {
    1.2
}

pub(crate) fn default_waveform_smooth_release_ms() -> f32 {
    60.0
}

pub(crate) fn default_waveform_bass_floor_threshold() -> f32 {
    0.2
}

pub(crate) fn default_waveform_bass_floor_amount() -> f32 {
    0.25
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

/// Which library leads the left navigation sidebar. Parsed from
/// `Config::nav_primary`; presentation policy, so it lives in the GUI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavPrimary {
    /// Library / New / playlists on top, vinyl pinned below (the default).
    Digital,
    /// The vinyl collection on top, the digital library pinned below.
    Vinyl,
}

impl NavPrimary {
    /// Parse a config string; anything unrecognized falls back to `Digital`.
    pub fn from_key(key: &str) -> Self {
        match key {
            "vinyl" => NavPrimary::Vinyl,
            _ => NavPrimary::Digital,
        }
    }

    /// Stable lowercase key stored in the config TOML.
    pub fn key(self) -> &'static str {
        match self {
            NavPrimary::Digital => "digital",
            NavPrimary::Vinyl => "vinyl",
        }
    }

    /// Label shown in the settings picker.
    pub fn label(self) -> &'static str {
        match self {
            NavPrimary::Digital => "Digital library",
            NavPrimary::Vinyl => "Vinyl collection",
        }
    }

    /// Both options, in picker order.
    pub const ALL: [NavPrimary; 2] = [NavPrimary::Digital, NavPrimary::Vinyl];
}

/// A physical (or digital) carrier a Discogs release can come on, used to filter
/// the release picker down to the formats the user actually collects.
///
/// Discogs reports a release's format as a free-text list like `Vinyl, 12", 45
/// RPM` or `CD, Album, Reissue`, mixing the carrier with descriptors. Only the
/// carrier matters here, so each variant matches on the substrings Discogs
/// actually uses for it. `Other` is the catch-all that keeps an unrecognized or
/// blank format visible rather than silently dropping it — better to show one
/// odd row than to hide the pressing the user was looking for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseMedium {
    Vinyl,
    Cd,
    Digital,
    Cassette,
    Other,
}

impl ReleaseMedium {
    /// Classify a Discogs format string. Non-vinyl carriers are tested first so
    /// a `CD, Comp` can't be pulled into `Vinyl` by the `lp` inside a word like
    /// "sampler" — the same ordering [`crate::dig`]'s vinyl filter relies on.
    pub fn classify(format: &str) -> Self {
        let f = format.to_ascii_lowercase();
        if f.trim().is_empty() {
            return ReleaseMedium::Other;
        }
        if f.contains("cd") || f.contains("dvd") {
            return ReleaseMedium::Cd;
        }
        if f.contains("file") || f.contains("mp3") || f.contains("flac") {
            return ReleaseMedium::Digital;
        }
        if f.contains("cassette") {
            return ReleaseMedium::Cassette;
        }
        if f.contains("vinyl")
            || f.contains("lp")
            || f.contains("12\"")
            || f.contains("10\"")
            || f.contains("7\"")
            || f.contains("shellac")
        {
            return ReleaseMedium::Vinyl;
        }
        ReleaseMedium::Other
    }

    /// Stable lowercase key stored in the config TOML.
    pub fn key(self) -> &'static str {
        match self {
            ReleaseMedium::Vinyl => "vinyl",
            ReleaseMedium::Cd => "cd",
            ReleaseMedium::Digital => "digital",
            ReleaseMedium::Cassette => "cassette",
            ReleaseMedium::Other => "other",
        }
    }

    /// Label shown next to the setting's checkbox.
    pub fn label(self) -> &'static str {
        match self {
            ReleaseMedium::Vinyl => "Vinyl",
            ReleaseMedium::Cd => "CD / DVD",
            ReleaseMedium::Digital => "Digital / file",
            ReleaseMedium::Cassette => "Cassette",
            ReleaseMedium::Other => "Other or unlisted",
        }
    }

    /// One line of hover help per medium.
    pub fn hint(self) -> &'static str {
        match self {
            ReleaseMedium::Vinyl => "Records: LPs, 12\", 10\", 7\", shellac",
            ReleaseMedium::Cd => "Compact discs and DVDs",
            ReleaseMedium::Digital => "Download and streaming releases",
            ReleaseMedium::Cassette => "Tapes",
            ReleaseMedium::Other => "Anything Discogs lists no format for",
        }
    }

    /// All mediums, in the order the settings list shows them.
    pub const ALL: [ReleaseMedium; 5] = [
        ReleaseMedium::Vinyl,
        ReleaseMedium::Cd,
        ReleaseMedium::Digital,
        ReleaseMedium::Cassette,
        ReleaseMedium::Other,
    ];
}

/// Which section the app selects on launch. Parsed from `Config::startup_view`;
/// presentation policy, so it lives in the GUI boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StartupView {
    /// The whole catalog ("Library") — the default.
    Library,
    /// The Discogs vinyl collection grid.
    Vinyl,
    /// The self-clearing inbox of fresh imports.
    Recent,
}

impl StartupView {
    /// Parse a config string; anything unrecognized falls back to `Library`.
    pub fn from_key(key: &str) -> Self {
        match key {
            "vinyl" => StartupView::Vinyl,
            "recent" => StartupView::Recent,
            _ => StartupView::Library,
        }
    }

    /// Stable lowercase key stored in the config TOML.
    pub fn key(self) -> &'static str {
        match self {
            StartupView::Library => "library",
            StartupView::Vinyl => "vinyl",
            StartupView::Recent => "recent",
        }
    }

    /// Label shown in the settings picker.
    pub fn label(self) -> &'static str {
        match self {
            StartupView::Library => "Library",
            StartupView::Vinyl => "Vinyl collection",
            StartupView::Recent => "New imports",
        }
    }

    /// All options, in picker order.
    pub const ALL: [StartupView; 3] = [
        StartupView::Library,
        StartupView::Vinyl,
        StartupView::Recent,
    ];
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
            hidden_release_mediums: Vec::new(),
            library_root: None,
            column_order: Vec::new(),
            hidden_columns: Vec::new(),
            column_widths: BTreeMap::new(),
            default_sort: String::new(),
            default_sort_ascending: true,
            vinyl_sort: default_vinyl_sort(),
            vinyl_sort_ascending: false,
            nav_primary: default_nav_primary(),
            nav_density: default_nav_density(),
            startup_view: default_startup_view(),
            auto_analyze: true,
            auto_write_tags: true,
            onboarding_completed_version: 0,
            convert_format: default_convert_format(),
            convert_bitrate_kbps: String::new(),
            convert_out_dir: None,
            convert_in_place: true,
            volume: default_volume(),
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
    /// Whether releases on `medium` should be shown in the release picker.
    pub fn shows_release_medium(&self, medium: ReleaseMedium) -> bool {
        !self
            .hidden_release_mediums
            .iter()
            .any(|k| k == medium.key())
    }

    /// Whether a Discogs format string passes the user's medium filter.
    ///
    /// Hiding *every* medium would leave the picker permanently empty and make
    /// the app look broken, so an all-hidden config is treated as no filter at
    /// all — the setting is a way to narrow the list, never to disable matching.
    pub fn shows_release_format(&self, format: &str) -> bool {
        if ReleaseMedium::ALL
            .iter()
            .all(|m| !self.shows_release_medium(*m))
        {
            return true;
        }
        self.shows_release_medium(ReleaseMedium::classify(format))
    }

    /// Show or hide one medium in the release picker. Caller persists with
    /// [`Config::save`].
    pub fn set_release_medium_shown(&mut self, medium: ReleaseMedium, shown: bool) {
        self.hidden_release_mediums.retain(|k| k != medium.key());
        if !shown {
            self.hidden_release_mediums.push(medium.key().to_string());
        }
    }

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
        let freq_changed = p.low_hz != self.waveform_low_hz || p.mid_hz != self.waveform_mid_hz;
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

    #[test]
    fn classify_reads_the_carrier_out_of_a_discogs_format_string() {
        use ReleaseMedium::*;
        assert_eq!(ReleaseMedium::classify("Vinyl, 12\", 45 RPM"), Vinyl);
        assert_eq!(ReleaseMedium::classify("Vinyl, LP, Album"), Vinyl);
        assert_eq!(ReleaseMedium::classify("2 x Vinyl, 12\""), Vinyl);
        assert_eq!(ReleaseMedium::classify("Shellac, 10\""), Vinyl);
        assert_eq!(ReleaseMedium::classify("CD, Album, Reissue"), Cd);
        assert_eq!(ReleaseMedium::classify("DVD, Compilation"), Cd);
        assert_eq!(ReleaseMedium::classify("File, MP3, 320 kbps"), Digital);
        assert_eq!(ReleaseMedium::classify("Cassette, Album"), Cassette);
        // A master row names no format; it must stay visible rather than vanish.
        assert_eq!(ReleaseMedium::classify(""), Other);
        assert_eq!(ReleaseMedium::classify("Box Set"), Other);
    }

    /// The ordering trap the dig filter already guards against: "sampler"
    /// contains "lp", so a CD comp must be classified before vinyl is tried.
    #[test]
    fn classify_does_not_let_a_cd_sampler_pass_as_vinyl() {
        assert_eq!(ReleaseMedium::classify("CD, Sampler"), ReleaseMedium::Cd);
    }

    #[test]
    fn hiding_a_medium_filters_only_that_carrier() {
        let mut cfg = Config::default();
        // Default is show-everything.
        assert!(cfg.shows_release_format("CD, Album"));
        assert!(cfg.shows_release_format("Vinyl, 12\""));

        cfg.set_release_medium_shown(ReleaseMedium::Cd, false);
        cfg.set_release_medium_shown(ReleaseMedium::Digital, false);
        assert!(!cfg.shows_release_format("CD, Album"));
        assert!(!cfg.shows_release_format("File, FLAC"));
        assert!(cfg.shows_release_format("Vinyl, 12\""));
        // Unlisted formats stay visible: better one odd row than a hidden pressing.
        assert!(cfg.shows_release_format(""));

        // Re-showing removes the key rather than stacking duplicates.
        cfg.set_release_medium_shown(ReleaseMedium::Cd, true);
        assert!(cfg.shows_release_format("CD, Album"));
        assert_eq!(cfg.hidden_release_mediums, vec!["digital".to_string()]);
    }

    /// Hiding everything would make the picker permanently empty, so it's
    /// treated as no filter at all.
    #[test]
    fn hiding_every_medium_is_ignored_rather_than_hiding_everything() {
        let mut cfg = Config::default();
        for m in ReleaseMedium::ALL {
            cfg.set_release_medium_shown(m, false);
        }
        assert!(cfg.shows_release_format("Vinyl, 12\""));
        assert!(cfg.shows_release_format("CD, Album"));
    }

    /// A config predating the library root loads with none set (music keeps
    /// arriving only via explicit adds), and a chosen root survives the TOML
    /// round trip.
    #[test]
    fn library_root_defaults_to_none_and_round_trips() {
        let old: Config = toml::from_str("").unwrap();
        assert_eq!(old.library_root, None);

        let cfg = Config {
            library_root: Some(PathBuf::from("/Users/dj/Music/seeker")),
            ..Config::default()
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(
            back.library_root,
            Some(PathBuf::from("/Users/dj/Music/seeker"))
        );
    }

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
