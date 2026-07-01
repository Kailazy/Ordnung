//! Audio analysis: decode → BPM/beatgrid, key (Camelot), waveform, loudness.
//!
//! Pure-Rust DSP. See the `audio-analysis` skill for the algorithms and the cache
//! contract. Bump `ANALYZER_VERSION` whenever an algorithm changes so cached
//! results invalidate.

pub mod decode;
pub mod dsp;
pub mod fingerprint;
pub mod key;
pub mod quality;
pub mod tempo;
pub mod waveform;

pub use decode::{decode_mono, decode_mono_capped, DecodedAudio};

use crate::error::Result;
use crate::model::{Analysis, Beatgrid};
use std::hash::{Hash, Hasher};
use std::path::Path;

/// Increment when any analysis algorithm changes; gates cache reuse.
/// v2: per-frame normalized, log-compressed, band-limited chromagram for key.
/// v3: EDM-tuned `edma` key profiles (Faraldo et al.) + minor mode bias.
/// v4: HPCP-style spectral peak-picking chroma + 4096 FFT (resolves the tonic).
/// v5: perceptual acoustic fingerprint (catches cross-format/-tag duplicates).
/// v6: low-pass cutoff detection (flags lossy transcodes hiding in lossless files).
/// v7: high-resolution tempo — log-flux onset, harmonic-comb ACF with a tempo
///     prior for octave robustness, and a fractional-comb refine for sub-BPM accuracy.
/// v8: BPM/tempo detection disabled — analysis no longer emits a bpm or beatgrid.
/// v9: key accuracy — lower chroma band to 90 Hz (admits bass-root fundamentals),
///     per-track tuning correction, minor mode bias 1.05→1.20. 21%→34% exact,
///     43%→50% harmonically-compatible on the 79-track labelled set (KEY_CHECK.md).
/// v10: colored-waveform band energy — per-bin low/mid/high spectral energy
///     (`waveform_bands`) for the GUI's energy/spectrum waveform colouring.
/// v11: colored-waveform loudness — `waveform_bands` now carries 4 bytes/bin
///     `[low, mid, high, loudness]`; loudness is K-weighted (BS.1770) RMS in dB,
///     so the energy mode tracks *perceived* loudness instead of raw, bass-
///     dominated, linear magnitude.
/// v12: multiband colored waveform — band bytes are now *raw* RMS amplitude
///     (sqrt-companded) at higher time resolution (`WAVE_COLOR_BINS`), drawn as
///     three overlaid per-band waveforms; loudness byte stays K-weighted.
/// v13: finer colored-waveform detail — `WAVE_COLOR_BINS` raised to 4000, drawn
///     peak-preserving so fine transients resolve as thin spikes. (Still the
///     150 s key window — the full-track span change below shipped *without*
///     bumping the version, so v13 ambiguously covers both 150 s and full-track
///     waveforms. v14 exists to disambiguate.)
/// v14: full-track colored waveform — the waveform/preview now span the whole
///     track (decoded up to a ceiling) instead of the 150 s key window, and the
///     color bins scale with duration (streamed STFT). The span change itself
///     landed under v13 but forgot to bump the version, so v13 caches may hold
///     either span; this bump forces re-analysis so every cached waveform is
///     unambiguously full-track and lines up with the player's playhead.
/// v15: hybrid energy byte — `waveform_bands` byte 4 is now K-weighted loudness
///     gated by *spectral occupancy* (the fraction of 30 Hz–15 kHz FFT bins
///     within 60 dB of the track's hottest bin), stored as the cube root of
///     `loud^1.2 · occ^0.6` so the GUI's existing gamma-3 curve reconstructs
///     it. Compressed masters sit within a few dB of peak throughout, so the
///     old loudness-only byte was a structureless wall; occupancy recovers the
///     intro/breakdown/drop contour (see `tests/energy_probe.rs`).
pub const ANALYZER_VERSION: u32 = 15;

/// First analyzer version whose `waveform_preview`/`waveform_bands` span the
/// **full track**. Earlier versions only covered the first 150 s (the key
/// window), so their bins are time-incompatible with the player's full-track
/// playhead (`pos / full_duration`) — drawing them stretches ~150 s of audio
/// across the whole bar, putting the wrong section under the cursor. v13 is
/// ambiguous (the span change shipped without a version bump), so the floor is
/// v14. The GUI must treat pre-`v14` waveform data as absent until re-analyzed.
pub const WAVEFORM_FULLTRACK_VERSION: u32 = 14;

/// How much audio to feed the analyzers. Steady-tempo material needs only a
/// representative window, which keeps decoding fast.
#[derive(Debug, Clone, Copy)]
pub struct AnalysisParams {
    pub max_seconds: u32,
}

impl Default for AnalysisParams {
    fn default() -> Self {
        AnalysisParams { max_seconds: 150 }
    }
}

/// Generous ceiling on how much audio we decode, so the colored waveform can span
/// the whole track without an hour-long file blowing up memory. ~20 min covers
/// essentially every DJ track; longer files have their waveform truncated here.
const DECODE_CEILING_SECS: usize = 20 * 60;

/// Decode and analyze one file into an `Analysis` (BPM, key, beatgrid anchor,
/// waveform preview, peak/loudness, and a content hash for caching).
pub fn analyze_file(path: impl AsRef<Path>, params: AnalysisParams) -> Result<Analysis> {
    // Decode the whole track (capped at a sane ceiling) so the waveform spans the
    // full song. Key/BPM/quality only need a representative window, taken as a
    // slice of the decoded audio below — they don't pay for the full length.
    let ceiling = DECODE_CEILING_SECS
        .saturating_mul(48_000) // upper-bound rate; exact rate known after decode
        .max(1);
    let audio = decode_mono_capped(path, Some(ceiling))?;

    // Window for key/quality/fingerprint: the first `max_seconds` of the decode.
    let key_cap = (params.max_seconds as usize)
        .saturating_mul(audio.sample_rate as usize)
        .max(1);
    let key_slice = &audio.samples[..audio.samples.len().min(key_cap)];
    let spec = dsp::spectrogram(key_slice, audio.sample_rate);
    // BPM/tempo detection is disabled for now. Leave `bpm` empty and the beatgrid
    // anchorless until it's re-enabled. See `tempo::detect`.
    let detected_key = key::detect(&spec);
    let quality = quality::detect(&spec);
    // Waveform + levels span the full decoded track (not just the key window).
    let lv = waveform::levels(&audio.samples);
    let waveform_bands = waveform::color_bands(&audio.samples, audio.sample_rate);

    let beatgrid = Beatgrid { beats: Vec::new() };

    Ok(Analysis {
        bpm: None,
        key: detected_key,
        beatgrid,
        cues: Vec::new(),
        waveform_preview: lv.waveform_preview,
        waveform_bands,
        peak: Some(lv.peak),
        integrated_loudness_lufs: Some(lv.rms_dbfs), // RMS dBFS approximation for now
        content_hash: Some(content_hash(&audio.samples)),
        audio_fingerprint: Some(fingerprint::to_bytes(&fingerprint::fingerprint(&spec))),
        lowpass_hz: quality.cutoff_hz,
        lowpass_edge_db_per_khz: quality.edge_db_per_khz,
        analyzer_version: ANALYZER_VERSION,
    })
}

fn content_hash(samples: &[f32]) -> String {
    // Hash a decimated view of the samples; deterministic and independent of tags.
    let mut h = std::collections::hash_map::DefaultHasher::new();
    samples.len().hash(&mut h);
    for &s in samples.iter().step_by(997) {
        s.to_bits().hash(&mut h);
    }
    format!("{:016x}", h.finish())
}
