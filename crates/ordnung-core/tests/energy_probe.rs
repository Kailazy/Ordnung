//! Diagnostic: compare candidate per-track "energy" curves against the current
//! K-weighted-loudness one, printed as aligned ASCII strips so structure
//! (intro / breakdown / drop) can be eyeballed per metric.
//! Run: cargo test -p ordnung-core --test energy_probe --release -- --ignored --nocapture

use ordnung_core::analysis::{decode_mono, dsp, waveform};
use std::path::PathBuf;

/// Time bins per track (one output char each).
const BINS: usize = 100;
/// Loudness window matching waveform::LOUDNESS_RANGE_DB.
const LOUDNESS_RANGE_DB: f64 = 45.0;
/// A bin counts as "active" for occupancy if within this many dB of the
/// track's single loudest FFT bin.
const OCCUPANCY_FLOOR_DB: f64 = 60.0;

fn shade(t: f64) -> char {
    const RAMP: [char; 10] = [' ', '.', ':', '-', '=', '+', '*', '#', '%', '@'];
    RAMP[((t.clamp(0.0, 1.0) * 9.0).round()) as usize]
}

fn strip(vals: &[f64]) -> String {
    vals.iter().map(|&v| shade(v)).collect()
}

/// Population std dev — a crude "contrast" score for a normalized curve.
fn contrast(vals: &[f64]) -> f64 {
    let n = vals.len().max(1) as f64;
    let mean = vals.iter().sum::<f64>() / n;
    (vals.iter().map(|v| (v - mean) * (v - mean)).sum::<f64>() / n).sqrt()
}

/// |H|^2 of a normalized biquad at digital angular frequency w (copy of the
/// private helper in analysis::waveform, for the K-weighting gain).
fn biquad_mag2(w: f32, b0: f32, b1: f32, b2: f32, a1: f32, a2: f32) -> f32 {
    let (cw, c2w) = (w.cos(), (2.0 * w).cos());
    let (sw, s2w) = (w.sin(), (2.0 * w).sin());
    let br = b0 + b1 * cw + b2 * c2w;
    let bi = -(b1 * sw + b2 * s2w);
    let ar = 1.0 + a1 * cw + a2 * c2w;
    let ai = -(a1 * sw + a2 * s2w);
    (br * br + bi * bi) / (ar * ar + ai * ai).max(1e-12)
}

#[test]
#[ignore = "diagnostic: decodes samples and prints energy-curve candidates; run with --ignored --nocapture"]
fn print_energy_curves() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/seeker-sample");
    if !dir.is_dir() {
        eprintln!("skip: no sample dir");
        return;
    }
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    entries.sort();
    // A handful is enough to compare curve shapes; full sweep is slow.
    let picks: Vec<_> = entries.iter().step_by(entries.len().max(1) / 6 + 1).collect();

    for path in picks {
        let audio = match decode_mono(path) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let total_frames = dsp::frame_count(audio.samples.len());
        if total_frames < BINS {
            continue;
        }
        let n_bins = dsp::WINDOW / 2 + 1;
        let sr = audio.sample_rate as f32;
        let hz_to_bin = |hz: f32| ((hz * dsp::WINDOW as f32 / sr).round() as usize).min(n_bins);
        // Occupancy counts bins in the audible, non-lowpassed band so an mp3's
        // 16 kHz cutoff doesn't deflate every frame equally.
        let (occ_lo, occ_hi) = (hz_to_bin(30.0).max(1), hz_to_bin(15_000.0));

        let denom = (n_bins - 1).max(1) as f32;
        let kgain: Vec<f32> = (0..n_bins)
            .map(|i| {
                let w = std::f32::consts::PI * i as f32 / denom;
                let s1 = biquad_mag2(
                    w, 1.53512485958697, -2.69169618940638, 1.19839281085285,
                    -1.69065929318241, 0.73248077421585,
                );
                let s2 = biquad_mag2(w, 1.0, -2.0, 1.0, -1.99004745483398, 0.99007225036621);
                s1 * s2
            })
            .collect();

        // Pass 1: global max single-bin power (occupancy threshold reference).
        let mut max_bin_pow = 0.0f64;
        dsp::for_each_frame(&audio.samples, |frame| {
            for &m in &frame[occ_lo..occ_hi] {
                max_bin_pow = max_bin_pow.max((m * m) as f64);
            }
        });
        let occ_thresh = max_bin_pow * 10f64.powf(-OCCUPANCY_FLOOR_DB / 10.0);

        // Pass 2: accumulate all candidate metrics per time bin.
        let mut kw_pow = vec![0.0f64; BINS];
        let mut occupancy = vec![0.0f64; BINS];
        let mut flux = vec![0.0f64; BINS];
        let mut flatness = vec![0.0f64; BINS];
        let mut counts = vec![0u32; BINS];
        let mut prev = vec![0.0f32; n_bins];
        let mut t = 0usize;
        dsp::for_each_frame(&audio.samples, |frame| {
            let k = (t * BINS / total_frames).min(BINS - 1);
            let mut kw = 0.0f64;
            let mut active = 0usize;
            let mut fx = 0.0f64;
            let mut log_sum = 0.0f64;
            let mut lin_sum = 0.0f64;
            for (i, &m) in frame.iter().enumerate() {
                let p = (m * m) as f64;
                kw += (kgain[i] as f64) * p;
                if i >= occ_lo && i < occ_hi {
                    if p > occ_thresh {
                        active += 1;
                    }
                    log_sum += p.max(1e-20).ln();
                    lin_sum += p;
                }
                let d = (m - prev[i]).max(0.0) as f64;
                fx += d;
                prev[i] = m;
            }
            let nb = (occ_hi - occ_lo) as f64;
            kw_pow[k] += kw;
            occupancy[k] += active as f64 / nb;
            flux[k] += fx;
            // Spectral flatness: geometric / arithmetic mean of power. ~1 = noisy
            // /broadband, ~0 = tonal or sparse.
            flatness[k] += (log_sum / nb).exp() / (lin_sum / nb).max(1e-20);
            counts[k] += 1;
            t += 1;
        });
        for k in 0..BINS {
            let c = counts[k].max(1) as f64;
            kw_pow[k] /= c;
            occupancy[k] /= c;
            flux[k] /= c;
            flatness[k] /= c;
        }

        // Current pipeline: K-weighted dB over a 45 dB window under the max,
        // then the GUI's gamma-3 energy_curve.
        let max_db = kw_pow
            .iter()
            .map(|&p| 10.0 * p.max(1e-12).log10())
            .fold(f64::NEG_INFINITY, f64::max);
        let loud: Vec<f64> = kw_pow
            .iter()
            .map(|&p| {
                let db = 10.0 * p.max(1e-12).log10();
                ((db - (max_db - LOUDNESS_RANGE_DB)) / LOUDNESS_RANGE_DB).clamp(0.0, 1.0)
            })
            .map(|t| t.powi(3))
            .collect();
        // Candidates, each normalized to its own track max.
        let norm = |v: &[f64]| {
            let m = v.iter().cloned().fold(0.0f64, f64::max).max(1e-12);
            v.iter().map(|&x| x / m).collect::<Vec<_>>()
        };
        let occ_n = norm(&occupancy);
        let flux_n = norm(&flux);
        let flat_n = norm(&flatness);
        // Hybrid: loudness gated by how much of the spectrum is lit.
        let hybrid: Vec<f64> = loud
            .iter()
            .zip(&occ_n)
            .map(|(&l, &o)| l.max(1e-9).powf(0.4) * o.powf(0.6))
            .collect();
        let hybrid = norm(&hybrid);

        // Production path: what analyzer v15 stores, rendered as the GUI does
        // (energy byte cubed by `energy_curve`). Should match the hybrid row.
        let bands = waveform::color_bands(&audio.samples, audio.sample_rate);
        let nb2 = bands.len() / waveform::COLOR_STRIDE;
        let mut shipped = vec![0.0f64; BINS];
        let mut scount = vec![0u32; BINS];
        for (j, q) in bands.chunks(waveform::COLOR_STRIDE).enumerate() {
            let k = (j * BINS / nb2.max(1)).min(BINS - 1);
            shipped[k] += (q[3] as f64 / 255.0).powi(3);
            scount[k] += 1;
        }
        for k in 0..BINS {
            shipped[k] /= scount[k].max(1) as f64;
        }
        let shipped = norm(&shipped);

        let name = path.file_name().unwrap().to_string_lossy();
        let secs = audio.samples.len() as f32 / sr;
        println!("\n== {name} ({:.0}s, {} Hz)", secs, audio.sample_rate);
        println!("  loud (v14)     c={:.3} |{}|", contrast(&loud), strip(&loud));
        println!("  occupancy      c={:.3} |{}|", contrast(&occ_n), strip(&occ_n));
        println!("  flux           c={:.3} |{}|", contrast(&flux_n), strip(&flux_n));
        println!("  flatness       c={:.3} |{}|", contrast(&flat_n), strip(&flat_n));
        println!("  hybrid l*occ   c={:.3} |{}|", contrast(&hybrid), strip(&hybrid));
        println!("  shipped (v15)  c={:.3} |{}|", contrast(&shipped), strip(&shipped));
    }
}
