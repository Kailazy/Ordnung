//! Downbeat (bar "1") detection.
//!
//! Tempo + phase (`tempo::detect`) pin *where the beats are*; this decides *which*
//! beat starts the bar. Assumes 4/4 (the overwhelming default for DJ material), so
//! the answer is a phase in `0..4`: the index, within the detected beat sequence,
//! of the first beat that is a downbeat.
//!
//! For steady four-on-the-floor the kick lands on every beat, so kick energy can't
//! disambiguate. Two cues that *do*:
//!
//! 1. **Backbeat** — claps/snares sit on beats 2 & 4. The band around 2–8 kHz is
//!    louder on the off-beats than on 1 & 3, so the phase whose beats 2 & 4 carry
//!    the most high-band energy is the bar. This is the strong, reliable cue for
//!    house/techno.
//! 2. **Harmonic novelty** — bass-note / chord changes concentrate on the downbeat.
//!    Beat-synchronous spectral flux over the 150 Hz–2 kHz band peaks on the "1".
//!    Weaker and genre-dependent, so it only breaks ties the backbeat leaves.
//!
//! Both cues are computed per beat and averaged by bar position, so the decision is
//! robust to the odd missing clap or passing chord. When neither cue commits (no
//! backbeat, no harmonic motion) the phase falls back to 0 — the first detected beat
//! is the downbeat — which is what a bare grid would assume anyway.

use super::dsp::{Spectrogram, WINDOW};

const BAR: usize = 4;

// Analysis bands (Hz). Kick is unused for scoring (it's on every beat in 4/4) but
// documents the split; clap/snare and the harmonic band drive the two cues.
const CLAP_LO: f32 = 2_000.0;
const CLAP_HI: f32 = 8_000.0;
const HARM_LO: f32 = 150.0;
const HARM_HI: f32 = 2_000.0;

/// Relative cue weights. Backbeat leads; novelty only tips close calls.
const W_BACKBEAT: f32 = 1.0;
const W_NOVELTY: f32 = 0.5;

/// Fewest beats we'll decide a downbeat from (two bars); below this, default to 0.
const MIN_BEATS: usize = 8;

/// Bar phase in `0..4`: the index within the detected beat run of the first beat
/// that is a downbeat. Beat `i` then has bar position `((i - phase).rem_euclid(4))`
/// (0 = the "1"). `first_beat_ms` is the tempo phase; `bpm` the constant tempo.
pub fn detect_phase(spec: &Spectrogram, bpm: f32, first_beat_ms: u64) -> u32 {
    if bpm <= 0.0 || spec.frames.is_empty() {
        return 0;
    }
    let frame_rate = spec.frame_rate();
    let beat_frames = 60.0 / bpm * frame_rate;
    if beat_frames < 1.0 {
        return 0;
    }

    let (clap_lo, clap_hi) = bin_range(spec, CLAP_LO, CLAP_HI);
    let (harm_lo, harm_hi) = bin_range(spec, HARM_LO, HARM_HI);
    // Energy is sampled over the first ~half-beat after each beat, where transients
    // (kick/clap) and note onsets live.
    let win = (beat_frames * 0.5).max(1.0) as usize;
    let first_frame = first_beat_ms as f32 / 1000.0 * frame_rate;
    let n_frames = spec.frames.len();

    let mut clap: Vec<f32> = Vec::new();
    let mut harm_vecs: Vec<Vec<f32>> = Vec::new();
    let mut i = 0;
    loop {
        let f0 = (first_frame + i as f32 * beat_frames).round();
        if f0 < 0.0 {
            i += 1;
            continue;
        }
        let f0 = f0 as usize;
        if f0 >= n_frames {
            break;
        }
        let f1 = (f0 + win).min(n_frames);
        clap.push(band_energy(spec, f0, f1, clap_lo, clap_hi));
        harm_vecs.push(band_profile(spec, f0, f1, harm_lo, harm_hi));
        i += 1;
    }

    let n = clap.len();
    if n < MIN_BEATS {
        return 0;
    }

    // Beat-synchronous harmonic flux: L1 change from the previous beat's profile.
    let mut novelty = vec![0.0f32; n];
    for b in 1..n {
        novelty[b] = l1_dist(&harm_vecs[b], &harm_vecs[b - 1]);
    }

    let clap_scale = mean(&clap).max(f32::EPSILON);
    let nov_scale = mean(&novelty).max(f32::EPSILON);

    let mut best_phase = 0u32;
    let mut best_score = f32::MIN;
    for p in 0..BAR as u32 {
        // Backbeat: high-band energy on 2 & 4 (bar positions 1,3) over 1 & 3 (0,2).
        let on_24 = mean_at(&clap, p, &[1, 3]);
        let on_13 = mean_at(&clap, p, &[0, 2]);
        let backbeat = (on_24 - on_13) / clap_scale;

        // Novelty: harmonic change concentrated on the "1" (bar position 0).
        let nov_on_1 = mean_at(&novelty, p, &[0]);
        let novelty_lift = (nov_on_1 - nov_scale) / nov_scale;

        let score = W_BACKBEAT * backbeat + W_NOVELTY * novelty_lift;
        if score > best_score {
            best_score = score;
            best_phase = p;
        }
    }
    best_phase
}

/// Inclusive bin range covering `[lo_hz, hi_hz]`, clamped to the spectrum.
fn bin_range(spec: &Spectrogram, lo_hz: f32, hi_hz: f32) -> (usize, usize) {
    let n_bins = spec.frames[0].len();
    let sr = spec.sample_rate as f32;
    let k = |hz: f32| ((hz * WINDOW as f32 / sr).round() as usize).min(n_bins.saturating_sub(1));
    (k(lo_hz), k(hi_hz))
}

/// Mean total magnitude in `[lo, hi]` bins over frames `[f0, f1)`.
fn band_energy(spec: &Spectrogram, f0: usize, f1: usize, lo: usize, hi: usize) -> f32 {
    if f1 <= f0 {
        return 0.0;
    }
    let mut sum = 0.0;
    for frame in &spec.frames[f0..f1] {
        for &m in &frame[lo..=hi.min(frame.len() - 1)] {
            sum += m;
        }
    }
    sum / (f1 - f0) as f32
}

/// Per-bin mean magnitude in `[lo, hi]` over frames `[f0, f1)` — a beat-synchronous
/// spectral profile whose frame-to-frame change tracks harmonic motion.
fn band_profile(spec: &Spectrogram, f0: usize, f1: usize, lo: usize, hi: usize) -> Vec<f32> {
    let hi = hi.min(spec.frames[0].len() - 1);
    let mut prof = vec![0.0f32; hi.saturating_sub(lo) + 1];
    if f1 <= f0 {
        return prof;
    }
    for frame in &spec.frames[f0..f1] {
        for (j, slot) in prof.iter_mut().enumerate() {
            *slot += frame[lo + j];
        }
    }
    let inv = 1.0 / (f1 - f0) as f32;
    for v in &mut prof {
        *v *= inv;
    }
    prof
}

fn l1_dist(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum()
}

fn mean(xs: &[f32]) -> f32 {
    if xs.is_empty() {
        0.0
    } else {
        xs.iter().sum::<f32>() / xs.len() as f32
    }
}

/// Mean of `feat` over beats whose bar position (`(i - phase).rem_euclid(4)`) is one
/// of `positions`.
fn mean_at(feat: &[f32], phase: u32, positions: &[i64]) -> f32 {
    let mut sum = 0.0;
    let mut count = 0u32;
    for (i, &v) in feat.iter().enumerate() {
        let pos = (i as i64 - phase as i64).rem_euclid(BAR as i64);
        if positions.contains(&pos) {
            sum += v;
            count += 1;
        }
    }
    if count == 0 {
        0.0
    } else {
        sum / count as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::dsp::spectrogram;

    /// Synthesize a 4/4 loop: kick on every beat, a clap (bright noise burst) on
    /// beats 2 & 4, and a mid "chord" tone (in the 150 Hz–2 kHz harmonic band) that
    /// changes each bar on the downbeat. The downbeat is beat 0, so a correctly-
    /// phased detector must return 0.
    fn four_four(sr: u32, bpm: f32, bars: u32) -> Vec<f32> {
        let beat = (60.0 / bpm * sr as f32) as usize;
        let n = beat * 4 * bars as usize;
        let mut s = vec![0.0f32; n];
        // deterministic pseudo-noise for the clap
        let mut seed = 0x1234_5678u32;
        let mut rng = || {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            (seed >> 8) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
        };
        for b in 0..(4 * bars as usize) {
            let start = b * beat;
            let bar_pos = b % 4;
            // Kick: 60 Hz decaying sine on every beat.
            for j in 0..(beat / 2).min(n - start) {
                let t = j as f32 / sr as f32;
                let env = (-(j as f32) / (sr as f32 * 0.05)).exp();
                s[start + j] += (2.0 * std::f32::consts::PI * 60.0 * t).sin() * env * 0.8;
            }
            // Chord tone per bar in the harmonic band, sustained from the downbeat so
            // its change lands on the "1". Fundamental sits inside 150 Hz–2 kHz.
            let bar = b / 4;
            let chord_hz = [220.0f32, 277.0, 330.0][bar % 3];
            if bar_pos == 0 {
                for j in 0..beat * 4 {
                    if start + j >= n {
                        break;
                    }
                    let t = j as f32 / sr as f32;
                    s[start + j] += (2.0 * std::f32::consts::PI * chord_hz * t).sin() * 0.15;
                }
            }
            // Clap: bright noise burst on beats 2 & 4.
            if bar_pos == 1 || bar_pos == 3 {
                for j in 0..(beat / 4).min(n - start) {
                    let env = 1.0 - j as f32 / (beat / 4) as f32;
                    s[start + j] += rng() * env * 0.5;
                }
            }
        }
        s
    }

    #[test]
    fn finds_downbeat_on_beat_zero() {
        let sr = 44_100;
        let bpm = 128.0;
        let s = four_four(sr, bpm, 8);
        let spec = spectrogram(&s, sr);
        // Beats start at frame 0 → first_beat_ms = 0.
        assert_eq!(detect_phase(&spec, bpm, 0), 0);
    }

    #[test]
    fn finds_shifted_downbeat() {
        // Drop the first beat so the detected run starts on bar position 1 (beat
        // "2"): positions become 1,2,3,0,1,2,3,0,… and the first downbeat is the
        // *fourth* detected beat → phase 3.
        let sr = 44_100;
        let bpm = 128.0;
        let full = four_four(sr, bpm, 8);
        let drop = (60.0 / bpm * sr as f32) as usize; // one beat
        let s = full[drop..].to_vec();
        let spec = spectrogram(&s, sr);
        // First detected beat sits at ~0 ms; its bar position is 1, so phase = 3.
        assert_eq!(detect_phase(&spec, bpm, 0), 3);
    }

    #[test]
    fn degenerate_inputs_default_to_zero() {
        let spec = Spectrogram {
            frames: Vec::new(),
            sample_rate: 44_100,
        };
        assert_eq!(detect_phase(&spec, 128.0, 0), 0);
    }
}
