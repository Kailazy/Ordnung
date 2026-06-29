//! Domain model — the authoritative shapes for the whole workspace.
//!
//! Extend these types in place; never define parallel structs in `ordnung-cli`
//! or `ordnung-rbdb`. See the `ordnung-architecture` skill.

pub mod key;

use key::Key;

/// A unique identifier for a catalog entity. (Backed by SQLite rowids in Phase 1.)
pub type Id = u64;

/// Audio container/codec families Ordnung recognizes. CDJ-compatible subset is
/// enforced by the export/convert layers, not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Mp3,
    Aac,
    Wav,
    Aiff,
    Flac,
    Other,
}

/// Raw audio properties read during scan.
#[derive(Debug, Clone, PartialEq)]
pub struct AudioProperties {
    pub sample_rate_hz: u32,
    pub bit_depth: Option<u8>,
    pub channels: u8,
    pub duration_ms: u64,
    pub bitrate_kbps: Option<u32>,
}

/// Editable metadata.
///
/// Ordnung tries to capture the full standardized cross-format tag set so the
/// catalog is never less detailed than the file. Most fields are `Option`s
/// because few tracks have every field populated.
///
/// Two write paths, deliberately different in scope (see `tag`):
/// * Writeback to *source files* (`tag --write`) is scoped to the small "core"
///   set — title/artist/album/genre/label/year/comment — so we never accidentally
///   rewrite obscure fields the user didn't touch.
/// * Embedding into a *freshly converted file* (`convert`) writes this full set,
///   since the catalog is the source of truth for a brand-new file. ID3 targets
///   are written as ID3v2.3 for CDJ compatibility.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    // --- core (editable + written back via `tag --write`) -------------------
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub genre: Option<String>,
    pub label: Option<String>,
    pub year: Option<u16>,
    pub comment: Option<String>,
    pub rating: Option<u8>,

    // --- track / disc numbering --------------------------------------------
    pub track_number: Option<u16>,
    pub track_total: Option<u16>,
    pub disc_number: Option<u16>,
    pub disc_total: Option<u16>,

    // --- people / credits ---------------------------------------------------
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub conductor: Option<String>,
    pub remixer: Option<String>,
    pub producer: Option<String>,
    pub lyricist: Option<String>,
    pub arranger: Option<String>,
    pub performer: Option<String>,
    pub mix_dj: Option<String>,
    pub writer: Option<String>,

    // --- dates (full, beyond plain year) -----------------------------------
    pub recording_date: Option<String>,
    pub release_date: Option<String>,
    pub original_release_date: Option<String>,

    // --- release identifiers -----------------------------------------------
    pub isrc: Option<String>,
    pub barcode: Option<String>,
    pub catalog_number: Option<String>,
    pub publisher: Option<String>,
    pub copyright: Option<String>,
    pub release_country: Option<String>,

    // --- DJ-relevant (file-embedded; analysis-derived values are separate) -
    pub bpm_tag: Option<f32>,
    pub initial_key_tag: Option<String>,
    pub mood: Option<String>,
    pub grouping: Option<String>, // ID3 ContentGroup (TIT1)
    pub compilation: Option<bool>,

    // --- content / descriptive ---------------------------------------------
    pub subtitle: Option<String>,
    pub description: Option<String>,
    pub language: Option<String>,
    pub script: Option<String>,
    pub lyrics: Option<String>,
    pub work: Option<String>,
    pub movement: Option<String>,
    pub movement_number: Option<u16>,
    pub movement_total: Option<u16>,

    // --- encoder / origin ---------------------------------------------------
    pub encoded_by: Option<String>,
    pub encoder_software: Option<String>,
    pub encoder_settings: Option<String>,
    pub original_artist: Option<String>,
    pub original_album: Option<String>,

    // --- MusicBrainz IDs ---------------------------------------------------
    pub musicbrainz_recording_id: Option<String>,
    pub musicbrainz_track_id: Option<String>,
    pub musicbrainz_release_id: Option<String>,
    pub musicbrainz_release_group_id: Option<String>,
    pub musicbrainz_artist_id: Option<String>,
    pub musicbrainz_release_artist_id: Option<String>,
    pub musicbrainz_work_id: Option<String>,
    pub musicbrainz_release_type: Option<String>,
    pub acoust_id: Option<String>,

    // --- ReplayGain --------------------------------------------------------
    pub replay_gain_track_gain: Option<f32>,
    pub replay_gain_track_peak: Option<f32>,
    pub replay_gain_album_gain: Option<f32>,
    pub replay_gain_album_peak: Option<f32>,

    // --- cover art ---------------------------------------------------------
    pub has_cover: bool,
}

/// A beat anchor: a beat's number and its position in the track.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Beat {
    pub number: u32,
    pub position_ms: u64,
    pub bpm: f32,
}

/// Beatgrid: anchored beats (constant-tempo material uses few anchors).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Beatgrid {
    pub beats: Vec<Beat>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CueKind {
    Hot,
    Memory,
    Loop,
}

/// A cue point. Positions carry both ms and sample where the export needs them.
#[derive(Debug, Clone, PartialEq)]
pub struct Cue {
    pub kind: CueKind,
    pub position_ms: u64,
    pub label: Option<String>,
    pub color: Option<[u8; 3]>,
}

/// The result of analyzing a track. Cache key = (`content_hash`, `analyzer_version`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Analysis {
    pub bpm: Option<f32>,
    pub key: Option<Key>,
    pub beatgrid: Beatgrid,
    pub cues: Vec<Cue>,
    /// Low-resolution waveform preview bins (CDJ overview).
    pub waveform_preview: Vec<u8>,
    /// Per-bin colored-waveform data: `[low, mid, high, loudness]` quads, one per
    /// `waveform_preview` bin (length `4 × waveform_preview.len()`, time-aligned).
    /// `low`/`mid`/`high` are K-weighted band magnitude (globally normalized
    /// 0–255) — the RGB ratio is the bin's spectral balance, driving the spectrum
    /// color mode. `loudness` is K-weighted (ITU-R BS.1770) RMS in dB, normalized
    /// over a fixed window below the track's loudest bin — driving the energy
    /// color mode so it tracks *perceived* loudness. See `analysis::waveform`.
    /// Empty until (re)analyzed under analyzer v11+ (v10 stored 3 bytes/bin with
    /// no loudness; the GUI treats the wrong stride as "no band data").
    pub waveform_bands: Vec<u8>,
    pub peak: Option<f32>,
    pub integrated_loudness_lufs: Option<f32>,
    pub content_hash: Option<String>,
    /// Perceptual acoustic fingerprint (see `analysis::fingerprint`), little-endian
    /// packed. Identifies the same *recording* across formats/bitrates/tags, so
    /// duplicates that only differ in encoding surface without playback. `None`
    /// until (re)analyzed; empty for audio too short to fingerprint.
    pub audio_fingerprint: Option<Vec<u8>>,
    /// Detected low-pass cutoff in Hz (see `analysis::quality`): the frequency
    /// above which spectral energy collapses to the noise floor. `None` when the
    /// audio reaches toward Nyquist with no brick wall — i.e. looks full-band.
    /// Lossy encoders impose this wall (~20 kHz at 320 kbps, ~16 kHz at 128),
    /// so it flags transcodes that container bitrate (always 1411 for AIFF/WAV)
    /// can't. Paired with `lowpass_edge_db_per_khz`; render via `transcode_verdict`.
    pub lowpass_hz: Option<f32>,
    /// Steepness of the cliff at `lowpass_hz`, in dB/kHz. High = lossy brick wall;
    /// low = a gentle, genuine band-limit. Distinguishes a transcode from dull
    /// mastering so the verdict doesn't cry wolf.
    pub lowpass_edge_db_per_khz: Option<f32>,
    pub analyzer_version: u32,
}

/// Human-facing reading of the spectral roll-off, derived from `Analysis`'s raw
/// `lowpass_*` measurements. Presentation only — never stored (see the
/// "store canonical, derive display" rule).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscodeVerdict {
    /// Spectrum extends toward Nyquist with no lossy brick wall.
    Clean,
    /// A sharp cliff sits high (~20 kHz). Consistent with 320 kbps MP3 — but some
    /// lossless masters also apply a ~20 kHz shelf, so treat it as a hint, not proof.
    Suspect,
    /// A sharp low-pass cliff well below Nyquist: almost certainly upsampled from a
    /// lossy source. The strong flag.
    LikelyLossy,
    /// A cutoff exists but the edge is gradual — genuine band-limited mastering or
    /// an old recording, not a transcode signature. Reported, not flagged.
    Inconclusive,
}

impl Analysis {
    /// Sharpness threshold (dB/kHz) above which a cutoff reads as an encoder wall
    /// rather than a natural roll-off.
    const STEEP_DB_PER_KHZ: f32 = 25.0;

    /// Classify the spectral roll-off. `Clean` also covers "not yet analyzed for
    /// this" (no cutoff recorded), so absence never reads as an accusation.
    pub fn transcode_verdict(&self) -> TranscodeVerdict {
        match self.lowpass_hz {
            None => TranscodeVerdict::Clean,
            Some(cut) => {
                let steep = self
                    .lowpass_edge_db_per_khz
                    .is_some_and(|e| e >= Self::STEEP_DB_PER_KHZ);
                if !steep {
                    TranscodeVerdict::Inconclusive
                } else if cut >= 20_000.0 {
                    TranscodeVerdict::Suspect
                } else {
                    TranscodeVerdict::LikelyLossy
                }
            }
        }
    }

    /// Rough source-bitrate guess from the cutoff, for display next to the verdict.
    /// `None` when there's no cutoff to reason from.
    pub fn estimated_source_kbps(&self) -> Option<&'static str> {
        Some(match self.lowpass_hz? {
            c if c >= 20_000.0 => "~320 kbps (or lossless w/ 20 kHz shelf)",
            c if c >= 18_500.0 => "~256 kbps",
            c if c >= 15_500.0 => "~192 kbps / AAC",
            c if c >= 13_500.0 => "~128 kbps",
            _ => "≤96 kbps",
        })
    }
}

/// A track: the canonical unit of the catalog.
#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: Id,
    pub source_path: String,
    pub format: Format,
    pub properties: Option<AudioProperties>,
    pub tags: Tags,
    pub analysis: Option<Analysis>,
}

/// An ordered playlist; playlists nest under playlist folders (`parent`).
#[derive(Debug, Clone, PartialEq)]
pub struct Playlist {
    pub id: Id,
    pub name: String,
    pub parent: Option<Id>,
    pub is_folder: bool,
    pub track_ids: Vec<Id>,
}

/// One record in the user's Discogs vinyl collection, cached locally so the
/// "My Vinyl Collection" view renders offline and a refresh only fetches what's
/// new. `instance_id` is Discogs's per-copy id (unique even when you own two
/// pressings of the same release), so it's the stable primary key. Cover image
/// bytes are stored separately in the catalog (fetched lazily) and are not
/// carried on this metadata shape.
#[derive(Debug, Clone, PartialEq)]
pub struct VinylRecord {
    pub instance_id: u64,
    pub release_id: u64,
    pub title: String,
    pub artist: String,
    pub year: Option<u16>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    /// Human-readable format summary, e.g. `12", 45 RPM`.
    pub format: Option<String>,
    pub thumb_url: Option<String>,
    /// Full-size cover image URL to download on refresh.
    pub cover_url: Option<String>,
    /// Discogs `date_added` for the collection item, as listed (e.g. ISO 8601).
    pub added: Option<String>,
    /// True once a cover image has been downloaded and cached for this record.
    pub has_cover: bool,
}

/// Conversion target chosen explicitly by the user. Never applied automatically.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertRule {
    pub target: Format,
    pub bitrate_kbps: Option<u32>,
}

/// An export job description: which playlists/tracks go to which device, with an
/// optional explicit conversion rule.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportProfile {
    pub device_path: String,
    pub playlist_ids: Vec<Id>,
    pub convert: Option<ConvertRule>,
}
