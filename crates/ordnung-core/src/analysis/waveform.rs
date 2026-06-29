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

/// Per-bin three-band spectral energy for the colored waveform, derived from the
/// shared spectrogram so we pay for no extra FFT. Returns `3 * PREVIEW_BINS`
/// bytes — `[low, mid, high]` for each of `PREVIEW_BINS` time bins, time-aligned
/// with `preview()`'s peak bins. Bands split at 200 Hz and 2 kHz (bass / mids /
/// presence+air, the split DJ tools colour by). Each band is globally normalized
/// to 0–255 across the track, so a bin's RGB ratio is its spectral balance and
/// the triple's sum is its energy.
pub fn color_bands(spec: &dsp::Spectrogram) -> Vec<u8> {
    let n = spec.frames.len();
    if n == 0 || spec.frames[0].is_empty() {
        return vec![0; 3 * PREVIEW_BINS];
    }
    let n_bins = spec.frames[0].len();
    // Map a cutoff in Hz to an FFT bin index, clamped into range.
    let hz_to_bin = |hz: f32| {
        ((hz * dsp::WINDOW as f32 / spec.sample_rate as f32).round() as usize).min(n_bins)
    };
    let lo_hi = hz_to_bin(200.0);
    let mid_hi = hz_to_bin(2000.0).max(lo_hi);

    // Accumulate band magnitude per time bin (averaged over the frames it spans).
    let mut sums = vec![[0.0f32; 3]; PREVIEW_BINS];
    for (k, slot) in sums.iter_mut().enumerate() {
        let start = k * n / PREVIEW_BINS;
        let end = (((k + 1) * n / PREVIEW_BINS).max(start + 1)).min(n);
        let mut acc = [0.0f64; 3];
        for frame in &spec.frames[start..end] {
            let mut lo = 0.0f64;
            let mut mid = 0.0f64;
            let mut hi = 0.0f64;
            for (i, &m) in frame.iter().enumerate() {
                if i < lo_hi {
                    lo += m as f64;
                } else if i < mid_hi {
                    mid += m as f64;
                } else {
                    hi += m as f64;
                }
            }
            acc[0] += lo;
            acc[1] += mid;
            acc[2] += hi;
        }
        let span = (end - start).max(1) as f64;
        *slot = [
            (acc[0] / span) as f32,
            (acc[1] / span) as f32,
            (acc[2] / span) as f32,
        ];
    }

    // One global scale across all bands and bins: preserves the true relative
    // energy between bands (so summing the triple gives a section's loudness)
    // while the per-bin ratio still yields a meaningful hue.
    let max = sums
        .iter()
        .flat_map(|b| b.iter().copied())
        .fold(0.0f32, f32::max)
        .max(1e-9);
    let mut out = Vec::with_capacity(3 * PREVIEW_BINS);
    for b in &sums {
        for &v in b {
            out.push(((v / max).clamp(0.0, 1.0) * 255.0).round() as u8);
        }
    }
    out
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
