//! Transcode / spectral-quality detection — Ordnung's built-in "Spek".
//!
//! A `.flac`/`.aiff`/`.wav` extension only describes the *container*; the audio
//! inside may have been upsampled from a lossy source (MP3/AAC). The discarded
//! high frequencies never come back, so the file is "lossless garbage". Container
//! bitrate can't catch this — an AIFF transcoded from a 128 kbps MP3 still reads a
//! reassuring 1411 kbps. The spectrum can: every lossy encoder imposes a low-pass
//! *brick wall*, a near-vertical cliff above which there is essentially no energy
//! (≈20 kHz for 320 kbps MP3, ≈16 kHz for 128 kbps, ≈16–17 kHz for AAC). Genuine
//! lossless audio rolls off gently and keeps content up toward Nyquist.
//!
//! We measure that cliff from the shared spectrogram (free — it's already computed
//! for tempo/key) and report two raw numbers: the cutoff frequency and how sharp
//! the edge is. The human-facing verdict is *derived* from those (see
//! `model::Analysis::transcode_verdict`) so thresholds can be retuned without
//! re-analysis. This is exactly the picture you'd eyeball in Spek, quantified.

use super::dsp::Spectrogram;

/// Spectral roll-off measurement used to flag lossy transcodes. Both fields are
/// `None` when the spectrum extends cleanly toward Nyquist (no detectable cliff) —
/// i.e. the audio looks genuinely full-band.
pub struct Quality {
    /// Detected low-pass cutoff in Hz: the frequency above which energy collapses
    /// to the noise floor.
    pub cutoff_hz: Option<f32>,
    /// Steepness of the cliff at the cutoff, in dB per kHz. A lossy brick wall is
    /// steep (tens of dB/kHz); a natural roll-off is gentle.
    pub edge_db_per_khz: Option<f32>,
}

const NONE: Quality = Quality {
    cutoff_hz: None,
    edge_db_per_khz: None,
};

/// "Present" level relative to the mid-band reference: bins quieter than this are
/// treated as noise floor when locating the cliff.
const PRESENT_DB: f32 = -60.0;
/// A cutoff this close to Nyquist isn't a lossy wall — it's just the band edge of
/// genuinely full-band audio. Reported as no cutoff.
const FULLBAND_RATIO: f32 = 0.95;

/// Locate the low-pass cutoff (if any) in a magnitude spectrogram.
pub fn detect(spec: &Spectrogram) -> Quality {
    let nyquist = spec.sample_rate as f32 / 2.0;
    if spec.frames.is_empty() {
        return NONE;
    }
    let n_bins = spec.frames[0].len();
    if n_bins < 32 || nyquist <= 0.0 {
        return NONE;
    }

    // --- 1. Average magnitude per bin over the loudest frames -----------------
    // Quiet intros/breakdowns carry no high-frequency content and would drag the
    // estimate down, so we average only over frames at or above the median energy.
    let mut energies: Vec<f32> = spec
        .frames
        .iter()
        .map(|f| f.iter().map(|m| m * m).sum::<f32>())
        .collect();
    let mut sorted = energies.clone();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = sorted[sorted.len() / 2];

    let mut avg = vec![0.0f32; n_bins];
    let mut count = 0usize;
    for (frame, &en) in spec.frames.iter().zip(&energies) {
        if en >= median && en > 0.0 {
            for (b, &m) in frame.iter().enumerate() {
                avg[b] += m;
            }
            count += 1;
        }
    }
    if count == 0 {
        return NONE;
    }
    for v in &mut avg {
        *v /= count as f32;
    }
    energies.clear(); // done with it

    // --- 2. To dB, referenced to the mid-band peak ----------------------------
    let db: Vec<f32> = avg.iter().map(|&m| 20.0 * (m + 1e-12).log10()).collect();
    let bin_of = |hz: f32| ((hz / nyquist) * (n_bins as f32 - 1.0)).round() as usize;
    let hz_of = |bin: usize| bin as f32 / (n_bins as f32 - 1.0) * nyquist;

    // Reference = loudest bin in 200 Hz–6 kHz, the body of almost any track. The
    // cliff is judged relative to this so the test is level-independent.
    let ref_lo = bin_of(200.0).clamp(1, n_bins - 2);
    let ref_hi = bin_of(6_000.0).clamp(ref_lo + 1, n_bins - 1);
    let ref_db = db[ref_lo..=ref_hi]
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    if !ref_db.is_finite() {
        return NONE;
    }
    let rel: Vec<f32> = db.iter().map(|&d| d - ref_db).collect();
    let sm = smooth(&rel, 5);

    // --- 3. Find the cutoff: highest "present" bin above 2 kHz ----------------
    // Nothing below 2 kHz is a lossy wall (that would be telephone-grade); start
    // the search above it so a quiet mid never reads as a cutoff.
    let search_lo = bin_of(2_000.0).clamp(1, n_bins - 1);
    let cutoff_bin = (search_lo..n_bins).rev().find(|&b| sm[b] >= PRESENT_DB);
    let cutoff_bin = match cutoff_bin {
        Some(b) => b,
        None => return NONE, // no energy above 2 kHz at all — leave it unflagged
    };
    let cutoff_hz = hz_of(cutoff_bin);
    if cutoff_hz >= FULLBAND_RATIO * nyquist {
        return NONE; // reaches Nyquist → full-band, no lossy wall
    }

    // --- 4. Edge steepness: shelf level just below vs. floor just above -------
    // Sample the shelf ~500 Hz below the cutoff and the floor ~700 Hz above it.
    // A brick wall drops tens of dB across that ~1.2 kHz; a natural roll-off only
    // a few. This is what separates a transcode from genuinely dull mastering.
    let below_bin = bin_of((cutoff_hz - 500.0).max(0.0)).clamp(1, n_bins - 1);
    let above_bin = bin_of((cutoff_hz + 700.0).min(nyquist)).clamp(1, n_bins - 1);
    let span_khz = (hz_of(above_bin) - hz_of(below_bin)) / 1_000.0;
    let edge = if span_khz > 0.0 {
        ((sm[below_bin] - sm[above_bin]) / span_khz).max(0.0)
    } else {
        0.0
    };

    Quality {
        cutoff_hz: Some(cutoff_hz),
        edge_db_per_khz: Some(edge),
    }
}

/// Moving average over `±(radius)` bins; flattens FFT bin-to-bin ripple so the
/// cliff search reads the envelope, not noise.
fn smooth(v: &[f32], radius: usize) -> Vec<f32> {
    let n = v.len();
    let mut out = vec![0.0f32; n];
    for i in 0..n {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius + 1).min(n);
        let slice = &v[lo..hi];
        out[i] = slice.iter().sum::<f32>() / slice.len() as f32;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SR: u32 = 44_100;
    const N_BINS: usize = super::super::dsp::WINDOW / 2 + 1;

    /// Build a synthetic spectrogram whose magnitude is `1.0` up to `cutoff_hz`
    /// and `floor` above it, with `transition_hz` of linear ramp at the edge.
    fn synth(cutoff_hz: f32, transition_hz: f32, floor: f32) -> Spectrogram {
        let nyq = SR as f32 / 2.0;
        let frame: Vec<f32> = (0..N_BINS)
            .map(|i| {
                let hz = i as f32 / (N_BINS as f32 - 1.0) * nyq;
                if hz <= cutoff_hz {
                    1.0
                } else if hz >= cutoff_hz + transition_hz {
                    floor
                } else {
                    let t = (hz - cutoff_hz) / transition_hz;
                    1.0 + (floor - 1.0) * t
                }
            })
            .collect();
        Spectrogram {
            frames: vec![frame; 8],
            sample_rate: SR,
        }
    }

    #[test]
    fn full_band_has_no_cutoff() {
        // Energy all the way to Nyquist → not a transcode.
        let q = detect(&synth(22_000.0, 200.0, 0.5));
        assert!(q.cutoff_hz.is_none(), "got {:?}", q.cutoff_hz);
    }

    #[test]
    fn sharp_16k_wall_is_detected_steep() {
        // Classic 128 kbps / AAC signature: hard wall near 16 kHz.
        let q = detect(&synth(16_000.0, 150.0, 1e-4));
        let cut = q.cutoff_hz.expect("should detect a cutoff");
        assert!((cut - 16_000.0).abs() < 600.0, "cutoff {cut}");
        assert!(
            q.edge_db_per_khz.unwrap() >= 25.0,
            "edge {:?} should read as a brick wall",
            q.edge_db_per_khz
        );
    }

    #[test]
    fn gentle_rolloff_is_not_steep() {
        // A genuine band-limited master: cutoff present but a soft 4 kHz ramp.
        let q = detect(&synth(15_000.0, 4_000.0, 1e-3));
        if let Some(edge) = q.edge_db_per_khz {
            assert!(edge < 25.0, "gentle roll-off misread as steep: {edge}");
        }
    }

    #[test]
    fn sharp_20k_wall_detected() {
        // 320 kbps MP3 wall sits ~20 kHz — still below Nyquist, still detectable.
        let q = detect(&synth(20_000.0, 150.0, 1e-4));
        let cut = q.cutoff_hz.expect("should detect ~20k cutoff");
        assert!((cut - 20_000.0).abs() < 700.0, "cutoff {cut}");
    }

    #[test]
    fn empty_spectrogram_is_none() {
        let q = detect(&Spectrogram {
            frames: vec![],
            sample_rate: SR,
        });
        assert!(q.cutoff_hz.is_none());
    }
}
