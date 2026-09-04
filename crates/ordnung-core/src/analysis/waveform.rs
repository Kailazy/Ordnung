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
/// Colored-waveform time resolution, in bins per second of audio. The bin count
/// scales with track length so a 10-min track is as detailed *per second* as a
/// 3-min one (the renderer takes the per-pixel max over the bins it spans, so the
/// detail shows as thin spikes). Clamped to `[MIN_COLOR_BINS, MAX_COLOR_BINS]`.
/// Public so renderers can convert time-based smoothing constants into per-bin
/// coefficients for this envelope.
pub const COLOR_BINS_PER_SEC: f32 = 20.0;
const MIN_COLOR_BINS: usize = 400;
const MAX_COLOR_BINS: usize = 24_000;
/// dB window below the track's loudest bin that the loudness byte spans. Anything
/// quieter than `max - LOUDNESS_RANGE_DB` clamps to 0 (coolest). ~45 dB covers a
/// track's musical dynamic range without wasting resolution on the noise floor.
const LOUDNESS_RANGE_DB: f64 = 45.0;
/// An FFT bin counts as "active" for spectral occupancy when its power is within
/// this many dB of the track's single hottest bin.
const OCCUPANCY_FLOOR_DB: f64 = 60.0;
/// Occupancy is measured over this band: above the DC/sub rumble, below the
/// region lossy encoders low-pass away (an mp3's 16 kHz cutoff would otherwise
/// deflate every frame equally).
const OCCUPANCY_LO_HZ: f32 = 30.0;
const OCCUPANCY_HI_HZ: f32 = 15_000.0;

/// Number of color time-bins for a track of `n_samples` (see `COLOR_BINS_PER_SEC`).
fn color_bin_count(n_samples: usize, sample_rate: u32) -> usize {
    let secs = n_samples as f32 / sample_rate.max(1) as f32;
    ((secs * COLOR_BINS_PER_SEC).round() as usize).clamp(MIN_COLOR_BINS, MAX_COLOR_BINS)
}

/// Per-bin colored-waveform data, streamed over the **full track** (a fresh STFT
/// via `dsp::for_each_frame`, so no full spectrogram is held). Returns
/// `COLOR_STRIDE * bins` bytes — `[low, mid, high, loudness]` per time bin, where
/// `bins` scales with duration (`color_bin_count`):
///
/// * `low`/`mid`/`high` — **raw** band RMS amplitude (split at 120 Hz / 2 kHz),
///   sqrt-companded then globally normalized to 0–255. These are the per-band
///   waveform heights drawn overlaid (Serato/rekordbox style), so bass reads as
///   tall as it sounds and a hi-hat shows as a smaller high-band spike. RMS, not
///   peak, so loud sections still fluctuate instead of flat-lining at full scale.
/// * `loudness` — hybrid **energy**: K-weighted RMS dB (ITU-R BS.1770,
///   normalized over a `LOUDNESS_RANGE_DB` window below the track's loudest bin)
///   gated by **spectral occupancy** — the fraction of `OCCUPANCY_LO_HZ`–
///   `OCCUPANCY_HI_HZ` FFT bins within `OCCUPANCY_FLOOR_DB` of the track's
///   hottest bin. Modern masters sit within a few dB of peak for the whole
///   track, so loudness alone is a structureless wall; occupancy recovers the
///   intro/breakdown/drop shape (validated in `tests/energy_probe.rs`). Stored
///   as the cube root of `loud^1.2 · occ^0.6` (normalized to the track max) so
///   the GUI's gamma-3 energy curve reconstructs the validated hybrid at draw
///   time — and pre-v15 cached loudness bytes keep rendering as before.
pub fn color_bands(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let total_frames = dsp::frame_count(samples.len());
    // Cap bins to the frame count so every bin gets at least one frame (no comb
    // gaps on very short clips).
    let bins = color_bin_count(samples.len(), sample_rate).min(total_frames.max(1));
    let out_len = COLOR_STRIDE * bins;
    if total_frames == 0 {
        return vec![0; out_len];
    }
    let n_bins = dsp::WINDOW / 2 + 1;

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
        ((hz * dsp::WINDOW as f32 / sample_rate as f32).round() as usize).min(n_bins)
    };
    // Low band caps at 120 Hz (kick fundamental + sub) so low-mid energy that isn't
    // really part of a DJ's bass cue stays out of it; mid runs up to 2 kHz.
    let lo_hi = hz_to_bin(120.0);
    let mid_hi = hz_to_bin(2000.0).max(lo_hi);
    // Occupancy band, clamped inside the spectrum.
    let occ_lo = hz_to_bin(OCCUPANCY_LO_HZ).max(1).min(n_bins - 1);
    let occ_hi = hz_to_bin(OCCUPANCY_HI_HZ).clamp(occ_lo + 1, n_bins);

    // Which time bin a frame belongs to. A frame describes the audio at the centre
    // of its window (`t·HOP + WINDOW/2`), so binning it by `t/total_frames` would
    // draw every transient half a window (~46 ms) early — enough to visibly
    // disagree with the sample-accurate zoom lane and the beatgrid over it.
    let n_samples = samples.len().max(1);
    let frame_bin = |t: usize| {
        let centre = t * dsp::HOP + dsp::WINDOW / 2;
        (centre * bins / n_samples).min(bins - 1)
    };

    // Pass 1: accumulate raw band power + K-weighted power per time bin, streaming
    // the STFT frame-by-frame and assigning each frame to its bin by position.
    // Also track the hottest single-bin power — the occupancy threshold reference.
    let mut band_pow = vec![[0.0f64; 3]; bins];
    let mut kw_pow = vec![0.0f64; bins];
    let mut counts = vec![0u32; bins];
    let mut max_bin_pow = 0.0f64;
    let mut t = 0usize;
    dsp::for_each_frame(samples, |frame| {
        let k = frame_bin(t);
        let (mut lo, mut mid, mut hi, mut kw) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        for (i, &m) in frame.iter().enumerate() {
            let p = (m * m) as f64;
            kw += (kgain[i] * m * m) as f64;
            if i < lo_hi {
                lo += p;
            } else if i < mid_hi {
                mid += p;
            } else {
                hi += p;
            }
            if i >= occ_lo && i < occ_hi && p > max_bin_pow {
                max_bin_pow = p;
            }
        }
        band_pow[k][0] += lo;
        band_pow[k][1] += mid;
        band_pow[k][2] += hi;
        kw_pow[k] += kw;
        counts[k] += 1;
        t += 1;
    });
    // Mean power per bin.
    for k in 0..bins {
        let c = counts[k].max(1) as f64;
        band_pow[k][0] /= c;
        band_pow[k][1] /= c;
        band_pow[k][2] /= c;
        kw_pow[k] /= c;
    }

    // Pass 2: spectral occupancy — the active fraction of the occupancy band,
    // averaged per time bin. The threshold is relative to the track's hottest
    // bin, so this needs a second streamed STFT (memory stays flat; it costs one
    // extra FFT pass over the track).
    let occ_thresh = max_bin_pow * 10f64.powf(-OCCUPANCY_FLOOR_DB / 10.0);
    let occ_width = (occ_hi - occ_lo) as f64;
    let mut occupancy = vec![0.0f64; bins];
    let mut t = 0usize;
    dsp::for_each_frame(samples, |frame| {
        let k = frame_bin(t);
        let active = frame[occ_lo..occ_hi]
            .iter()
            .filter(|&&m| (m * m) as f64 > occ_thresh)
            .count();
        occupancy[k] += active as f64 / occ_width;
        t += 1;
    });
    for k in 0..bins {
        occupancy[k] /= counts[k].max(1) as f64;
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

    // Energy byte: hybrid of loudness and occupancy, as the cube root of the
    // validated curve `loud^1.2 · occ^0.6` (each factor normalized to its track
    // max) so the renderer's gamma-3 lands back on it.
    let occ_max = occupancy.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
    let energy: Vec<f64> = kw_pow
        .iter()
        .zip(&occupancy)
        .map(|(&p, &o)| {
            let db = 10.0 * p.max(1e-12).log10();
            let loud = ((db - floor_db) / LOUDNESS_RANGE_DB).clamp(0.0, 1.0);
            loud.powf(0.4) * (o / occ_max).powf(0.2)
        })
        .collect();
    let energy_max = energy.iter().cloned().fold(0.0f64, f64::max).max(1e-9);

    let mut out = Vec::with_capacity(out_len);
    for k in 0..bins {
        let q = |p: f64| {
            let mag = p.max(0.0).sqrt();
            ((mag / max_rms).clamp(0.0, 1.0).sqrt() * 255.0).round() as u8
        };
        out.push(q(band_pow[k][0]));
        out.push(q(band_pow[k][1]));
        out.push(q(band_pow[k][2]));
        out.push(((energy[k] / energy_max).clamp(0.0, 1.0) * 255.0).round() as u8);
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

/// Bytes per column in [`scroll_bands`]: `[low, mid, high, amp]`.
pub const SCROLL_STRIDE: usize = 4;
/// Column rate of [`scroll_bands`] — rekordbox's detailed-waveform rate.
pub const SCROLL_COLS_PER_SEC: u32 = 150;

/// RBJ-cookbook biquad, direct form 1.
struct Biquad {
    b0: f32,
    b1: f32,
    b2: f32,
    a1: f32,
    a2: f32,
    x1: f32,
    x2: f32,
    y1: f32,
    y2: f32,
}

impl Biquad {
    fn new(b0: f32, b1: f32, b2: f32, a0: f32, a1: f32, a2: f32) -> Self {
        Self {
            b0: b0 / a0,
            b1: b1 / a0,
            b2: b2 / a0,
            a1: a1 / a0,
            a2: a2 / a0,
            x1: 0.0,
            x2: 0.0,
            y1: 0.0,
            y2: 0.0,
        }
    }
    fn lowpass(hz: f32, sample_rate: u32) -> Self {
        let w = 2.0 * std::f32::consts::PI * hz / sample_rate.max(1) as f32;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / std::f32::consts::SQRT_2; // Q = 0.707
        let b1 = 1.0 - cw;
        Self::new(b1 / 2.0, b1, b1 / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha)
    }
    fn highpass(hz: f32, sample_rate: u32) -> Self {
        let w = 2.0 * std::f32::consts::PI * hz / sample_rate.max(1) as f32;
        let (sw, cw) = w.sin_cos();
        let alpha = sw / std::f32::consts::SQRT_2;
        let b1 = 1.0 + cw;
        Self::new(b1 / 2.0, -b1, b1 / 2.0, 1.0 + alpha, -2.0 * cw, 1.0 - alpha)
    }
    #[inline]
    fn step(&mut self, x: f32) -> f32 {
        let y = self.b0 * x + self.b1 * self.x1 + self.b2 * self.x2
            - self.a1 * self.y1
            - self.a2 * self.y2;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }
}

/// Rekordbox-style detailed 3-band waveform: `[low, mid, high, amp]` per
/// column at [`SCROLL_COLS_PER_SEC`]. `low`/`mid`/`high` are per-column peak
/// amplitudes of the band-filtered signal (split at 200 Hz / 2 kHz — low sits
/// a little wider than [`color_bands`]' 120 Hz so the blue band carries the
/// kick's punch, matching golden rekordbox files' fat low band), 0–127 like
/// rekordbox's `PWV7`; `amp` is the unfiltered
/// column peak, 0–255. All linear (no companding) and normalized to the
/// track's overall peak, so a kick pulses per beat and breakdowns dip —
/// matching what a golden rekordbox export stores. Time-domain single pass;
/// meant for export, where the audio is already being read anyway.
pub fn scroll_bands(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let sr = sample_rate.max(1) as u64;
    let cols = ((samples.len() as u64 * SCROLL_COLS_PER_SEC as u64) / sr).max(1) as usize;
    let mut peaks = vec![[0.0f32; 4]; cols];
    let mut lp = Biquad::lowpass(200.0, sample_rate);
    let mut hp = Biquad::highpass(2000.0, sample_rate);
    let mut mid_hp = Biquad::highpass(200.0, sample_rate);
    let mut mid_lp = Biquad::lowpass(2000.0, sample_rate);
    for (i, &s) in samples.iter().enumerate() {
        let col = ((i as u64 * SCROLL_COLS_PER_SEC as u64) / sr) as usize;
        let p = &mut peaks[col.min(cols - 1)];
        let vals = [
            lp.step(s).abs(),
            mid_lp.step(mid_hp.step(s)).abs(),
            hp.step(s).abs(),
            s.abs(),
        ];
        for (pk, v) in p.iter_mut().zip(vals) {
            *pk = pk.max(v);
        }
    }
    let max = peaks
        .iter()
        .flat_map(|p| p.iter().copied())
        .fold(0.0f32, f32::max)
        .max(1e-6);
    let mut out = Vec::with_capacity(cols * SCROLL_STRIDE);
    for p in &peaks {
        for (k, &v) in p.iter().enumerate() {
            let full = if k == 3 { 255.0 } else { 127.0 };
            out.push(((v / max) * full).round().min(full) as u8);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(hz: f32, secs: f32, sr: u32) -> Vec<f32> {
        (0..(secs * sr as f32) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * hz * i as f32 / sr as f32).sin() * 0.8)
            .collect()
    }

    #[test]
    fn scroll_bands_split_by_frequency() {
        let sr = 44_100;
        // 60 Hz belongs to the low band, 5 kHz to the high band.
        for (hz, band) in [(60.0, 0), (5_000.0, 2)] {
            let cols = scroll_bands(&sine(hz, 2.0, sr), sr);
            assert_eq!(cols.len(), 300 * SCROLL_STRIDE);
            // Steady-state columns (skip the filter warm-up at the start).
            let steady = &cols[100 * SCROLL_STRIDE..];
            let mean = |k: usize| {
                steady.iter().skip(k).step_by(SCROLL_STRIDE).map(|&v| v as u32).sum::<u32>()
                    / (steady.len() / SCROLL_STRIDE) as u32
            };
            for other in 0..3 {
                if other != band {
                    assert!(
                        mean(band) > mean(other) * 4,
                        "{hz} Hz: band {band} ({}) should dominate band {other} ({})",
                        mean(band),
                        mean(other)
                    );
                }
            }
            // The unfiltered amp channel tracks the signal at full scale.
            assert!(mean(3) > 200, "amp channel too small: {}", mean(3));
        }
    }

    #[test]
    fn scroll_bands_empty_input_is_one_zero_column() {
        assert_eq!(scroll_bands(&[], 44_100), vec![0; SCROLL_STRIDE]);
    }
}
