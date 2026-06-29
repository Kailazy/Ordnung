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
/// Time resolution of the colored-waveform data — far higher than `PREVIEW_BINS`
/// so the player's wide waveform resolves fine transients (the renderer takes the
/// per-pixel max over the bins it spans, so detail shows as thin spikes); the
/// inline table thumbnail just downsamples it. At ~4 KB/track this is cheap.
pub const WAVE_COLOR_BINS: usize = 4000;
/// dB window below the track's loudest bin that the loudness byte spans. Anything
/// quieter than `max - LOUDNESS_RANGE_DB` clamps to 0 (coolest). ~45 dB covers a
/// track's musical dynamic range without wasting resolution on the noise floor.
const LOUDNESS_RANGE_DB: f64 = 45.0;

/// Per-bin colored-waveform data, derived from the shared spectrogram so we pay
/// for no extra FFT. Returns `COLOR_STRIDE * WAVE_COLOR_BINS` bytes —
/// `[low, mid, high, loudness]` per time bin:
///
/// * `low`/`mid`/`high` — **raw** band RMS amplitude (split at 200 Hz / 2 kHz),
///   sqrt-companded then globally normalized to 0–255. These are the per-band
///   waveform heights drawn overlaid (Serato/rekordbox style), so bass reads as
///   tall as it sounds and a hi-hat shows as a smaller high-band spike. RMS, not
///   peak, so loud sections still fluctuate instead of flat-lining at full scale.
/// * `loudness` — **K-weighted RMS in dB** (ITU-R BS.1770 / LUFS-style perceptual
///   weighting), normalized over a `LOUDNESS_RANGE_DB` window below the track's
///   loudest bin. Drives the energy color mode so colour tracks *perceived*
///   loudness rather than raw, bass-dominated magnitude.
pub fn color_bands(spec: &dsp::Spectrogram) -> Vec<u8> {
    let out_len = COLOR_STRIDE * WAVE_COLOR_BINS;
    let n = spec.frames.len();
    if n == 0 || spec.frames[0].is_empty() {
        return vec![0; out_len];
    }
    let n_bins = spec.frames[0].len();

    // K-weighting (ITU-R BS.1770) as a per-FFT-bin *power* gain, used only for the
    // loudness byte: the product of the two stage biquads' magnitude responses.
    // Approximates the ear's frequency sensitivity — trims sub-bass, lifts
    // presence. Coefficients are the standard 48 kHz set; evaluated at our bin
    // frequencies they're a close-enough approximation (not a certified LUFS
    // meter). The band heights deliberately stay un-weighted (raw spectral
    // energy) so the bass shows big.
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

    // Per time bin: mean raw power per band, and mean K-weighted power (loudness),
    // averaged over the frames the bin spans.
    let mut band_pow = vec![[0.0f64; 3]; WAVE_COLOR_BINS];
    let mut kw_pow = vec![0.0f64; WAVE_COLOR_BINS];
    for k in 0..WAVE_COLOR_BINS {
        let start = k * n / WAVE_COLOR_BINS;
        let end = (((k + 1) * n / WAVE_COLOR_BINS).max(start + 1)).min(n);
        let mut acc = [0.0f64; 3];
        let mut kacc = 0.0f64;
        for frame in &spec.frames[start..end] {
            for (i, &m) in frame.iter().enumerate() {
                let p = (m * m) as f64;
                kacc += (kgain[i] * m * m) as f64;
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
        band_pow[k] = [acc[0] / span, acc[1] / span, acc[2] / span];
        kw_pow[k] = kacc / span;
    }

    // Band heights: RMS magnitude, globally normalized (so bass stays tallest),
    // then sqrt-companded so the quieter bands and low-level detail are visible.
    let max_rms = band_pow
        .iter()
        .flat_map(|b| b.iter().map(|&p| p.max(0.0).sqrt()))
        .fold(0.0f64, f64::max)
        .max(1e-12);
    // Loudness: K-weighted dB over a fixed window below the loudest bin.
    let max_db = kw_pow
        .iter()
        .map(|&p| 10.0 * p.max(1e-12).log10())
        .fold(f64::NEG_INFINITY, f64::max);
    let floor_db = max_db - LOUDNESS_RANGE_DB;

    let mut out = Vec::with_capacity(out_len);
    for k in 0..WAVE_COLOR_BINS {
        let q = |p: f64| {
            let mag = p.max(0.0).sqrt();
            ((mag / max_rms).clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
        };
        out.push(q(band_pow[k][0]));
        out.push(q(band_pow[k][1]));
        out.push(q(band_pow[k][2]));
        let db = 10.0 * kw_pow[k].max(1e-12).log10();
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
