//! Audio analysis: decode → BPM/beatgrid, key (Camelot), waveform, loudness.
//!
//! Pure-Rust DSP. See the `audio-analysis` skill for the algorithms and the cache
//! contract. Bump `ANALYZER_VERSION` whenever an algorithm changes so cached
//! results invalidate.

pub mod decode;
pub mod downbeat;
pub mod dsp;
pub mod fingerprint;
pub mod key;
pub mod quality;
pub mod tempo;
pub mod waveform;

pub use decode::{decode_mono, decode_mono_capped, DecodedAudio};

use crate::error::Result;
use crate::model::{Analysis, Beat, Beatgrid};
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
/// v16: re-enable BPM/tempo (undoes v8). `tempo::detect` runs again and
///     `analyze_file` emits a constant-tempo (static) beatgrid: beats spaced
///     `60000/bpm` ms from the first detected beat, spanning the full track and
///     each carrying the global BPM. Downbeat numbering is provisional (the first
///     detected beat is numbered 1) until dedicated downbeat detection lands.
///     Baseline vs rekordbox on the 79-track set is guarded by `tests/bpm_eval.rs`.
/// v17: downbeat detection — `downbeat::detect_phase` picks which of the four beats
///     starts the bar (backbeat clap on 2 & 4 + harmonic novelty on the "1"), so the
///     grid's beat numbers put the downbeat where rekordbox's red bar marker sits
///     instead of assuming the first detected beat is the "1".
/// v18: metrical tempo correction — `tempo::correct_metrical` promotes a faster
///     metrical relative (3/2, 4/3, 5/4, 2×) when it phase-locks with clearly more
///     energy per tap. Fixes the "3:2"/"5:4" slow folds the autocorrelation comb
///     settled on for fast techno (e.g. 89→134, 107→134), validated against a
///     200-track rekordbox comparison (~15 tracks recovered).
/// v19: persist the downbeat — the catalog now stores the beatgrid's first-beat bar
///     number (`first_beat_number`), so the player can draw downbeat ("1") markers on
///     the moving waveform. No algorithm change; the bump backfills the phase, which
///     earlier versions computed (v17) but discarded on save.
/// v20: grid anchor timing — STFT frame indices were being read as timestamps,
///     but a frame describes the audio at the *centre* of its 4096-sample window,
///     so every beat was reported ~46 ms early; spectral flux peaking mid-attack
///     added ~28 ms more. `dsp::frame_to_ms`/`ms_to_frame` fix the convention (in
///     `tempo::detect`, `downbeat::detect_phase`, and `waveform::color_bands`) and
///     `tempo::snap_anchor` then slides the anchor onto the transient itself at
///     ~1.5 ms resolution. Measured 74 ms early → within ~3 ms
///     (`tests/grid_phase.rs`); tempo and downbeat phase are unchanged.
/// v21: full-track period refine — the static grid's BPM is now fine-tuned
///     against every beat of the whole track at sample resolution
///     (`tempo::lock_grid`), not just the comb's 150 s frame-grid lock, whose
///     ~0.03 BPM search quantization (plus 0.01 BPM rounding) extrapolated into
///     visible creep: lines on the beat at the start of a six-minute track
///     drifted ~50 ms off by the end. The grid also keeps the refined BPM at
///     full precision (display still rounds); residual creep is under ~2 ms.
/// v22: beat-aware anchor snap — graded against rekordbox 7's own grids for 121
///     club tracks (EYEBAGS USB PQTZ ground truth, `ordnung-rbdb`'s
///     `downbeat_eval`), the flux-max snap gridded 93/121 tracks off the beat:
///     it lands mid-bump on soft/sidechained kicks (50–200 ms late) or on
///     louder offbeat percussion outright. The snap now folds sub-, high- and
///     full-band RMS envelopes into one average beat, takes strong rising
///     edges as candidate feet, votes for the beat by percussive sharpness
///     (short-window slope; sub-weighted, so a kick's thump outranks an offbeat
///     hat, and a bare hi click still beats a slow bass swell) with a sustain
///     penalty (ringing offbeat chord stabs fold wide; kicks fold narrow), and
///     anchors at the winning bump's foot. rekordbox parity on the reference
///     set: 28→64 of 121 grids in phase, 22→52 with the downbeat also right
///     (rekordbox's own ~45 ms early stamp convention aside —
///     `tempo::RB_GRID_LEAD_MS`). Downbeat novelty weight retuned (0.5→0.8).
/// v23: grid spans the whole track — the snap could anchor at the beat
///     instance nearest the coarse comb phase, which sits beats into the
///     track; the grid only extrapolates forward, so the track's opening
///     beats got no lines at all and every bar number shifted with them. The
///     anchor is now always the beat phase's first instance (within one
///     period of 0), like rekordbox's own grids.
/// v24: kick-entrance downbeat — the bar "1" is now the beat where the kick
///     first enters (sustained kick-band energy arriving as a jump; see
///     `downbeat::kick_entrance`), which is where a DJ expects it; the
///     backbeat/novelty cues only decide for kickless material. This
///     deliberately diverges from rekordbox, whose pattern-derived bar phase
///     routinely starts tracks on "3"/"4" (its own reference grids do), which
///     is exactly the behavior being overridden.
/// v25: the beat line lands where the visible slope begins — the snap's foot
///     crossing is now read off the full-band folded profile (the amplitude
///     envelope the waveform view draws, threshold 15% of the local rise)
///     instead of the sub-weighted one, whose slow sub swell parked lines
///     mid-bump on soft kicks. Kick-entrance detection relaxed to an
///     active-run rule (25% of median, two beats, preceded by an inactive
///     beat): filtered intro kicks no longer push the "1" a bar in, and the
///     brittle relative-jump test that could reject every beat is gone.
pub const ANALYZER_VERSION: u32 = 25;

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
    let detected_key = key::detect(&spec);
    let quality = quality::detect(&spec);
    // BPM/beatgrid: a constant-tempo (static) grid extrapolated across the whole
    // track from the tempo lock over the key window. Steady 4/4 club material has
    // one tempo, so the window is representative; variable-tempo (dynamic) grids
    // come later. `beat_offset_ms` anchors the phase; the grid spans `duration_ms`.
    let tempo = tempo::detect(&spec);
    let duration_ms = if audio.sample_rate > 0 {
        audio.samples.len() as u64 * 1000 / audio.sample_rate as u64
    } else {
        0
    };
    let (bpm, beatgrid) = if tempo.bpm > 0.0 {
        // Lock the grid against the *full* decoded track at sample resolution:
        // refine the comb's period against every beat (its 150 s frame-grid lock
        // is ~0.02 BPM coarse, which creeps ~50 ms of misalignment by the end of
        // a six-minute track), then slide the anchor onto the actual transient
        // so grid lines sit on the kick.
        let (bpm, beat_offset_ms) = tempo::lock_grid(
            &audio.samples,
            audio.sample_rate,
            tempo.bpm,
            tempo.beat_offset_ms,
        );
        // Which of the four beats starts the bar, so the grid's downbeats ("1")
        // land where rekordbox would put its red bar marker.
        let phase = downbeat::detect_phase(&spec, bpm, beat_offset_ms);
        let first_beat_number = ((BAR - phase % BAR) % BAR) + 1; // bar position of beat 0
        (
            Some(bpm),
            build_static_grid(bpm, beat_offset_ms, duration_ms, first_beat_number),
        )
    } else {
        (None, Beatgrid::default())
    };
    // Waveform + levels span the full decoded track (not just the key window).
    let lv = waveform::levels(&audio.samples);
    let waveform_bands = waveform::color_bands(&audio.samples, audio.sample_rate);

    Ok(Analysis {
        bpm,
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

/// Beats per bar (4/4 assumed for DJ material).
const BAR: u32 = 4;

/// Expand a constant-tempo lock into an anchored beatgrid spanning the track.
///
/// Beats are spaced `60000/bpm` ms starting at `first_beat_ms`, one per beat up to
/// `duration_ms`, each tagged with the global `bpm` — a static grid, which is what
/// steady 4/4 club material wants and what rekordbox emits by default. `number`
/// cycles 1..=4 as the position within the bar, starting from `first_beat_number`
/// (the detected bar position of the first beat) so downbeats land on the true "1".
fn build_static_grid(
    bpm: f32,
    first_beat_ms: u64,
    duration_ms: u64,
    first_beat_number: u32,
) -> Beatgrid {
    if bpm <= 0.0 || duration_ms == 0 {
        return Beatgrid::default();
    }
    let period_ms = 60_000.0 / bpm as f64;
    let mut beats = Vec::new();
    let mut pos = first_beat_ms as f64;
    let mut number: u32 = first_beat_number.clamp(1, BAR);
    while pos.round() as u64 <= duration_ms {
        beats.push(Beat {
            number,
            position_ms: pos.round() as u64,
            bpm,
        });
        number = if number == BAR { 1 } else { number + 1 };
        pos += period_ms;
    }
    Beatgrid { beats }
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    #[test]
    fn static_grid_spacing_and_span() {
        // 120 BPM → 500 ms per beat, phase 250 ms, over a 4 s track, downbeat first.
        let g = build_static_grid(120.0, 250, 4_000, 1);
        assert_eq!(g.beats.len(), 8); // 250,750,...,3750
        assert_eq!(g.beats[0].position_ms, 250);
        assert_eq!(g.beats[1].position_ms, 750);
        assert!(g.beats.iter().all(|b| (b.bpm - 120.0).abs() < 1e-6));
        // number cycles 1..=4 as bar position.
        let nums: Vec<u32> = g.beats.iter().take(5).map(|b| b.number).collect();
        assert_eq!(nums, vec![1, 2, 3, 4, 1]);
    }

    #[test]
    fn static_grid_downbeat_offset() {
        // First beat is bar position 4, so the *second* beat is the downbeat "1".
        let g = build_static_grid(120.0, 250, 4_000, 4);
        let nums: Vec<u32> = g.beats.iter().take(5).map(|b| b.number).collect();
        assert_eq!(nums, vec![4, 1, 2, 3, 4]);
    }

    #[test]
    fn static_grid_degenerate_inputs() {
        assert!(build_static_grid(0.0, 0, 4_000, 1).beats.is_empty());
        assert!(build_static_grid(120.0, 0, 0, 1).beats.is_empty());
    }
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
