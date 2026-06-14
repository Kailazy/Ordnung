//! Waveform preview + level metrics from mono samples.
//!
//! The preview is a coarse peak-per-bin overview (handy for a future GUI and a
//! first approximation of the CDJ overview). Loudness here is a simple RMS dBFS
//! estimate for gain hints — not full BS.1770 LUFS (that can come later).

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
