//! Tempo (BPM) and beat phase from a spectral-flux onset envelope.
//!
//! High-resolution, octave-robust pipeline:
//!
//! 1. **Onset envelope** — log-compressed spectral flux (positive bin-to-bin
//!    increases of `ln(1+magnitude)`), then local-mean subtraction so loud bass
//!    and slow drift don't drown the transients.
//! 2. **Coarse period** — mean-removed autocorrelation scored by a harmonic comb
//!    (a true beat period peaks at its multiples) and weighted by a log-Gaussian
//!    tempo prior centred on club tempi. The comb + prior resolve the ½×/2×
//!    octave ambiguity that fools a bare autocorrelation peak. Parabolic
//!    interpolation lifts the integer-lag peak to a fractional period.
//! 3. **Fine period + phase** — a fractional-period comb is phase-locked against
//!    the whole onset envelope (linearly interpolated) and the period is fine-
//!    searched around the coarse estimate. Aligning hundreds of beats over the
//!    track pins the tempo to well under 0.5 BPM and yields the first downbeat.

use super::dsp::Spectrogram;

/// Plausible BPM search range.
const BPM_MIN: f32 = 70.0;
const BPM_MAX: f32 = 185.0;

/// Log-Gaussian tempo prior: centre and width (in octaves). Disambiguates octave
/// errors by pulling toward the band where most DJ material actually sits, without
/// hard-clamping genuinely fast/slow tracks.
const TEMPO_BIAS_BPM: f32 = 125.0;
const TEMPO_BIAS_OCTAVES: f32 = 0.65;

/// Number of harmonics summed by the comb when scoring a candidate period.
const COMB_HARMONICS: usize = 4;

/// Fine search half-width around the coarse BPM, and its step.
const REFINE_FRAC: f32 = 0.035;
const REFINE_STEPS: usize = 320;

/// Compression applied before the flux: `ln(1 + GAMMA * magnitude)`.
const GAMMA: f32 = 1.0;

pub struct TempoResult {
    pub bpm: f32,
    /// First detected beat position, in milliseconds.
    pub beat_offset_ms: u64,
    /// Salience of the chosen period (peak comb score / median), clamped to [0, 1].
    /// ~0.5+ is a confident lock; near 0 means weak/ambiguous tempo.
    pub confidence: f32,
}

/// Spectral-flux onset strength per frame: positive changes of the log-compressed
/// magnitude, then local-mean subtracted and half-wave rectified.
pub fn onset_envelope(spec: &Spectrogram) -> Vec<f32> {
    if spec.frames.is_empty() {
        return Vec::new();
    }
    let mut env = Vec::with_capacity(spec.frames.len());
    env.push(0.0);
    // Reuse last frame's log magnitudes instead of recomputing the ln twice.
    let mut prev_log: Vec<f32> = spec.frames[0]
        .iter()
        .map(|&m| (1.0 + GAMMA * m).ln())
        .collect();
    for frame in spec.frames.iter().skip(1) {
        let mut flux = 0.0;
        for (b, &m) in frame.iter().enumerate() {
            let c = (1.0 + GAMMA * m).ln();
            let d = c - prev_log[b];
            if d > 0.0 {
                flux += d;
            }
            prev_log[b] = c;
        }
        env.push(flux);
    }
    // Remove DC / slow drift with a simple moving-average subtraction.
    subtract_moving_average(&mut env, 16);
    env
}

pub fn detect(spec: &Spectrogram) -> TempoResult {
    let env = onset_envelope(spec);
    let frame_rate = spec.frame_rate();

    let lag_min = (60.0 / BPM_MAX * frame_rate).floor().max(1.0) as usize;
    let lag_max = (60.0 / BPM_MIN * frame_rate).ceil() as usize;
    if env.len() <= lag_max + 2 {
        // Not enough material to estimate a tempo confidently.
        return TempoResult {
            bpm: 0.0,
            beat_offset_ms: 0,
            confidence: 0.0,
        };
    }

    // Mean-removed autocorrelation (removes the DC pedestal that flattens the ACF
    // of a non-negative envelope), normalized so acf[0] == 1.
    let mean = env.iter().sum::<f32>() / env.len() as f32;
    let zero_mean: Vec<f32> = env.iter().map(|&x| x - mean).collect();
    let max_acf_lag = (lag_max * COMB_HARMONICS).min(zero_mean.len() - 1);
    let acf = autocorrelation(&zero_mean, max_acf_lag);

    // Harmonic-comb score, weighted by the tempo prior, over the candidate periods.
    let hi = lag_max.min(env.len() - 1);
    let mut scores = vec![0.0f32; hi + 1];
    let mut best_lag = lag_min;
    let mut best_score = f32::MIN;
    for lag in lag_min..=hi {
        let bpm = 60.0 * frame_rate / lag as f32;
        let s = comb_score(&acf, lag) * tempo_prior(bpm);
        scores[lag] = s;
        if s > best_score {
            best_score = s;
            best_lag = lag;
        }
    }

    // Sub-lag refinement of the peak by parabolic interpolation on the comb score.
    let coarse_lag = parabolic_peak(&scores, best_lag, lag_min, hi);
    let coarse_bpm = (60.0 * frame_rate / coarse_lag).clamp(BPM_MIN, BPM_MAX);

    // Fine period + phase: phase-lock a fractional comb against the whole envelope.
    let (bpm, phase_frames) = refine(&env, frame_rate, coarse_bpm);
    let beat_offset_ms = (phase_frames / frame_rate * 1000.0).round().max(0.0) as u64;

    TempoResult {
        bpm: (bpm * 100.0).round() / 100.0,
        beat_offset_ms,
        confidence: confidence(&scores, lag_min, hi, best_score),
    }
}

/// Log-Gaussian weight: 1.0 at `TEMPO_BIAS_BPM`, falling off over octaves.
fn tempo_prior(bpm: f32) -> f32 {
    if bpm <= 0.0 {
        return 0.0;
    }
    let z = (bpm / TEMPO_BIAS_BPM).log2() / TEMPO_BIAS_OCTAVES;
    (-0.5 * z * z).exp()
}

/// Sum of the normalized autocorrelation at a period and its first harmonics; the
/// true beat period scores higher than its ½× (which misses the fundamental) or
/// 2× (which only sees a subset of the comb teeth).
fn comb_score(acf: &[f32], period: usize) -> f32 {
    if period == 0 {
        return 0.0;
    }
    let mut s = 0.0;
    for k in 1..=COMB_HARMONICS {
        let lag = period * k;
        if lag >= acf.len() {
            break;
        }
        s += acf[lag] / k as f32;
    }
    s
}

/// Normalized autocorrelation `acf[0..=max_lag]`, with `acf[0] == 1`.
fn autocorrelation(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    let mut acf = vec![0.0f32; max_lag + 1];
    for (lag, slot) in acf.iter_mut().enumerate() {
        let mut sum = 0.0;
        for i in lag..n {
            sum += x[i] * x[i - lag];
        }
        *slot = sum;
    }
    let z = acf[0];
    if z > 0.0 {
        for v in acf.iter_mut() {
            *v /= z;
        }
    }
    acf
}

/// Parabolic interpolation of the peak at `peak` against its neighbours, returning
/// a fractional lag. Falls back to the integer peak at the search boundary.
fn parabolic_peak(scores: &[f32], peak: usize, lo: usize, hi: usize) -> f32 {
    if peak <= lo || peak >= hi {
        return peak as f32;
    }
    let (sm1, s0, sp1) = (scores[peak - 1], scores[peak], scores[peak + 1]);
    let denom = sm1 - 2.0 * s0 + sp1;
    if denom.abs() < f32::EPSILON {
        return peak as f32;
    }
    let delta = (0.5 * (sm1 - sp1) / denom).clamp(-1.0, 1.0);
    peak as f32 + delta
}

/// Fine-search the period around `coarse_bpm` by phase-locking a fractional-period
/// comb to the envelope; returns `(bpm, phase_in_frames)` of the best alignment.
fn refine(env: &[f32], frame_rate: f32, coarse_bpm: f32) -> (f32, f32) {
    let lo = (coarse_bpm * (1.0 - REFINE_FRAC)).max(BPM_MIN);
    let hi = (coarse_bpm * (1.0 + REFINE_FRAC)).min(BPM_MAX);

    let mut best_bpm = coarse_bpm;
    let mut best_energy = f32::MIN;
    for k in 0..=REFINE_STEPS {
        let bpm = lo + (hi - lo) * k as f32 / REFINE_STEPS as f32;
        let period = 60.0 / bpm * frame_rate;
        // Coarse 1-frame phase grid is enough to rank periods.
        let (_, energy) = comb_align(env, period, 1.0);
        if energy > best_energy {
            best_energy = energy;
            best_bpm = bpm;
        }
    }

    // Pin the phase at the winning period with a finer (¼-frame) grid.
    let period = 60.0 / best_bpm * frame_rate;
    let (phase, _) = comb_align(env, period, 0.25);
    (best_bpm, phase)
}

/// Best phase of a comb with `period` frames over the envelope, scanning phases in
/// `phase_step`-frame increments. Returns `(phase_frames, mean_energy_per_tap)`.
fn comb_align(env: &[f32], period: f32, phase_step: f32) -> (f32, f32) {
    let n = env.len();
    if period < 1.0 || n == 0 {
        return (0.0, 0.0);
    }
    let mut best_phase = 0.0f32;
    let mut best_energy = f32::MIN;
    let mut phase = 0.0;
    while phase < period {
        let mut sum = 0.0;
        let mut count = 0u32;
        let mut x = phase;
        while x < (n - 1) as f32 {
            sum += lerp(env, x);
            count += 1;
            x += period;
        }
        let energy = if count > 0 { sum / count as f32 } else { 0.0 };
        if energy > best_energy {
            best_energy = energy;
            best_phase = phase;
        }
        phase += phase_step;
    }
    (best_phase, best_energy)
}

/// Linear interpolation of the envelope at fractional index `x`.
fn lerp(env: &[f32], x: f32) -> f32 {
    if x <= 0.0 {
        return env[0];
    }
    let i = x as usize;
    if i + 1 >= env.len() {
        return *env.last().unwrap_or(&0.0);
    }
    let f = x - i as f32;
    env[i] * (1.0 - f) + env[i + 1] * f
}

/// Peak-to-median comb-score ratio, squashed into [0, 1] as a confidence value.
fn confidence(scores: &[f32], lo: usize, hi: usize, peak: f32) -> f32 {
    if hi <= lo || peak <= 0.0 {
        return 0.0;
    }
    let mut vals: Vec<f32> = scores[lo..=hi].iter().copied().filter(|v| *v > 0.0).collect();
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = vals[vals.len() / 2];
    if median <= 0.0 {
        return 1.0;
    }
    // ratio 1 → 0 confidence, large ratio → ~1. ratio of 3 maps to ~0.5.
    let ratio = peak / median;
    (1.0 - (1.0 / ratio)).clamp(0.0, 1.0)
}

fn subtract_moving_average(env: &mut [f32], radius: usize) {
    if env.is_empty() {
        return;
    }
    let n = env.len();
    let smoothed: Vec<f32> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(radius);
            let hi = (i + radius + 1).min(n);
            let s: f32 = env[lo..hi].iter().sum();
            s / (hi - lo) as f32
        })
        .collect();
    for i in 0..n {
        env[i] = (env[i] - smoothed[i]).max(0.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::dsp::spectrogram;

    /// A click train at `bpm`: 20 ms decaying 1 kHz bursts on each beat.
    fn click_train(sr: u32, bpm: f32, secs: u32) -> Vec<f32> {
        let period = 60.0 / bpm * sr as f32;
        let n = sr as usize * secs as usize;
        let click_len = (sr as f32 * 0.02) as usize;
        let mut s = vec![0.0f32; n];
        let mut beat = 0.0;
        while (beat as usize) < n {
            let start = beat as usize;
            for j in 0..click_len {
                if start + j < n {
                    let t = j as f32 / sr as f32;
                    let env = 1.0 - j as f32 / click_len as f32;
                    s[start + j] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * env;
                }
            }
            beat += period;
        }
        s
    }

    fn detect_bpm(sr: u32, bpm: f32, secs: u32) -> f32 {
        let s = click_train(sr, bpm, secs);
        let spec = spectrogram(&s, sr);
        detect(&spec).bpm
    }

    #[test]
    fn detects_120_bpm_click_train() {
        let got = detect_bpm(44_100, 120.0, 30);
        assert!((got - 120.0).abs() < 1.0, "expected ~120, got {got}");
    }

    #[test]
    fn high_resolution_off_grid_tempo() {
        // 127.3 BPM falls between integer autocorrelation lags (≈126.0 / 129.2);
        // the fine comb search must recover it to well under a BPM.
        let got = detect_bpm(44_100, 127.3, 40);
        assert!((got - 127.3).abs() < 0.6, "expected ~127.3, got {got}");
    }

    #[test]
    fn accurate_across_dj_range() {
        for &target in &[90.0f32, 128.0, 140.0, 174.0] {
            let got = detect_bpm(44_100, target, 40);
            assert!(
                (got - target).abs() < 0.8,
                "expected ~{target}, got {got}"
            );
        }
    }

    #[test]
    fn resists_double_tempo_from_offbeats() {
        // Kicks at 130 BPM with weaker offbeat hats halfway between (which a bare
        // autocorrelation could read as 260). The comb + prior must hold 130.
        let sr = 44_100;
        let mut s = click_train(sr, 130.0, 40);
        let off = click_train(sr, 130.0, 40);
        let shift = (60.0 / 130.0 / 2.0 * sr as f32) as usize;
        for (i, &v) in off.iter().enumerate() {
            if i + shift < s.len() {
                s[i + shift] += 0.5 * v;
            }
        }
        let spec = spectrogram(&s, sr);
        let got = detect(&spec).bpm;
        assert!((got - 130.0).abs() < 1.0, "expected ~130, got {got}");
    }
}
