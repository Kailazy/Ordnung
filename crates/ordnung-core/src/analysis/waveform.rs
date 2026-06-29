//! Waveform preview + level metrics from mono samples.
//!
//! The preview is a coarse peak-per-bin overview (handy for a future GUI and a
//! first approximation of the CDJ overview). Loudness here is a simple RMS dBFS
//! estimate for gain hints — not full BS.1770 LUFS (that can come later).

use super::dsp;

/// Number of bins in the preview waveform.
pub const PREVIEW_BINS: usize = 400;

pub struct Levels {
    pub waveform_preview: Vec<u8>,
    pub peak: f32,
    pub rms_dbfs: f32,
}

pub fn levels(samples: &[f32]) -> Levels {
    let mut peak = 0.0f32;
    let mut sum_sq = 0.0f64;
    for &s in samples {
        let a = s.abs();
        if a > peak {
            peak = a;
        }
        sum_sq += (s as f64) * (s as f64);
    }
    let rms = if samples.is_empty() {
        0.0
    } else {
        (sum_sq / samples.len() as f64).sqrt() as f32
    };
    let rms_dbfs = if rms > 0.0 {
        20.0 * rms.log10()
    } else {
        -120.0
    };

    Levels {
        waveform_preview: preview(samples),
        peak,
        rms_dbfs,
    }
}

/// Bytes per output bin in [`color_bands`]: `[low, mid, high, loudness]`.
pub const COLOR_STRIDE: usize = 4;
/// dB window below the track's loudest bin that the loudness byte spans. Anything
/// quieter than `max - LOUDNESS_RANGE_DB` clamps to 0 (coolest). ~45 dB covers a
/// track's musical dynamic range without wasting resolution on the noise floor.
const LOUDNESS_RANGE_DB: f64 = 45.0;

/// Per-bin colored-waveform data, derived from the shared spectrogram so we pay
/// for no extra FFT. Returns `COLOR_STRIDE * PREVIEW_BINS` bytes —
/// `[low, mid, high, loudness]` per time bin, time-aligned with `preview()`'s
/// peak bins:
///
/// * `low`/`mid`/`high` — K-weighted band *magnitude* (split at 200 Hz / 2 kHz),
///   globally normalized to 0–255. Drive the spectrum mode's hue (the per-bin
///   RGB ratio is the section's spectral balance).
/// * `loudness` — **K-weighted RMS in dB** (ITU-R BS.1770 / LUFS-style perceptual
///   weighting), normalized over a `LOUDNESS_RANGE_DB` window below the track's
///   loudest bin to 0–255. Drives the energy mode so colour tracks *perceived
///   loudness*. (Earlier it summed raw FFT magnitude — linear and bass-dominated,
///   so quiet bass read as "hot"; loud and quiet sections looked alike.)
pub fn color_bands(spec: &dsp::Spectrogram) -> Vec<u8> {
    let out_len = COLOR_STRIDE * PREVIEW_BINS;
    let n = spec.frames.len();
    if n == 0 || spec.frames[0].is_empty() {
        return vec![0; out_len];
    }
    let n_bins = spec.frames[0].len();

    // K-weighting (ITU-R BS.1770) as a per-FFT-bin *power* gain: the product of
    // the two stage biquads' magnitude responses. Approximates the ear's
    // frequency sensitivity — trims sub-bass, lifts presence — so the loudness
    // below is perceptual, not flat-spectrum RMS. Coefficients are the standard
    // 48 kHz set; evaluated at our bin frequencies they're a close-enough
    // approximation for colouring (this is not a certified LUFS meter).
    let denom = (n_bins - 1).max(1) as f32;
    let kgain: Vec<f32> = (0..n_bins)
        .map(|i| {
            let w = std::f32::consts::PI * i as f32 / denom;
            // Stage 1: high-shelf (+~4 dB above ~1.5 kHz). Stage 2: RLB high-pass.
            let s1 = biquad_mag2(
                w, 1.53512485958697, -2.69169618940638, 1.19839281085285,
                -1.69065929318241, 0.73248077421585,
            );
            let s2 = biquad_mag2(w, 1.0, -2.0, 1.0, -1.99004745483398, 0.99007225036621);
            s1 * s2
        })
        .collect();

    let hz_to_bin = |hz: f32| {
        ((hz * dsp::WINDOW as f32 / spec.sample_rate as f32).round() as usize).min(n_bins)
    };
    let lo_hi = hz_to_bin(200.0);
    let mid_hi = hz_to_bin(2000.0).max(lo_hi);

    // Mean K-weighted power per band, per time bin (averaged over its frames).
    let mut bins = vec![[0.0f64; 3]; PREVIEW_BINS];
    for (k, slot) in bins.iter_mut().enumerate() {
        let start = k * n / PREVIEW_BINS;
        let end = (((k + 1) * n / PREVIEW_BINS).max(start + 1)).min(n);
        let mut acc = [0.0f64; 3];
        for frame in &spec.frames[start..end] {
            for (i, &m) in frame.iter().enumerate() {
                let p = (kgain[i] * m * m) as f64; // K-weighted power
                if i < lo_hi {
                    acc[0] += p;
                } else if i < mid_hi {
                    acc[1] += p;
                } else {
                    acc[2] += p;
                }
            }
        }
        let span = (end - start).max(1) as f64;
        *slot = [acc[0] / span, acc[1] / span, acc[2] / span];
    }

    // Hue: global magnitude scale (sqrt of power). Loudness: dB of total power,
    // normalized to a fixed window below the loudest bin.
    let max_mag = bins
        .iter()
        .flat_map(|b| b.iter().map(|&p| p.max(0.0).sqrt()))
        .fold(0.0f64, f64::max)
        .max(1e-12);
    let total: Vec<f64> = bins.iter().map(|b| b[0] + b[1] + b[2]).collect();
    let max_db = total
        .iter()
        .map(|&p| 10.0 * p.max(1e-12).log10())
        .fold(f64::NEG_INFINITY, f64::max);
    let floor_db = max_db - LOUDNESS_RANGE_DB;

    let mut out = Vec::with_capacity(out_len);
    for (k, b) in bins.iter().enumerate() {
        let q = |mag: f64| ((mag / max_mag).clamp(0.0, 1.0) * 255.0).round() as u8;
        out.push(q(b[0].max(0.0).sqrt()));
        out.push(q(b[1].max(0.0).sqrt()));
        out.push(q(b[2].max(0.0).sqrt()));
        let db = 10.0 * total[k].max(1e-12).log10();
        let t = ((db - floor_db) / LOUDNESS_RANGE_DB).clamp(0.0, 1.0);
        out.push((t * 255.0).round() as u8);
    }
    out
}

/// Squared magnitude response `|H(e^jw)|²` of a normalized biquad at digital
/// angular frequency `w`. Used to evaluate the K-weighting filter per FFT bin.
fn biquad_mag2(w: f32, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) -> f32 {
    let (cw, c2w) = (w.cos(), (2.0 * w).cos());
    let (sw, s2w) = (w.sin(), (2.0 * w).sin());
    let br = b0 + b1 * cw + b2 * c2w;
    let bi = -(b1 * sw + b2 * s2w);
    let ar = 1.0 + a1 * cw + a2 * c2w;
    let ai = -(a1 * sw + a2 * s2w);
    (br * br + bi * bi) / (ar * ar + ai * ai).max(1e-12)
}

fn preview(samples: &[f32]) -> Vec<u8> {
    if samples.is_empty() {
        return vec![0; PREVIEW_BINS];
    }
    let bin = samples.len().div_ceil(PREVIEW_BINS).max(1);
    let mut out = Vec::with_capacity(PREVIEW_BINS);
    let mut i = 0;
    while i < samples.len() {
        let end = (i + bin).min(samples.len());
        let p = samples[i..end].iter().fold(0.0f32, |m, &s| m.max(s.abs()));
        out.push((p.clamp(0.0, 1.0) * 255.0).round() as u8);
        i = end;
    }
    out.resize(PREVIEW_BINS, 0);
    out
}
