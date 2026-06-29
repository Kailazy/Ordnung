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
pub const ANALYZER_VERSION: u32 = 11;

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

/// Decode and analyze one file into an `Analysis` (BPM, key, beatgrid anchor,
/// waveform preview, peak/loudness, and a content hash for caching).
pub fn analyze_file(path: impl AsRef<Path>, params: AnalysisParams) -> Result<Analysis> {
    let cap = (params.max_seconds as usize)
        .saturating_mul(48_000) // upper-bound rate; trimmed precisely below
        .max(1);
    let audio = decode_mono_capped(path, Some(cap))?;

    let spec = dsp::spectrogram(&audio.samples, audio.sample_rate);
    // BPM/tempo detection is disabled for now. Leave `bpm` empty and the beatgrid
    // anchorless until it's re-enabled. See `tempo::detect`.
    let detected_key = key::detect(&spec);
    let quality = quality::detect(&spec);
    let lv = waveform::levels(&audio.samples);
    let waveform_bands = waveform::color_bands(&spec);

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
