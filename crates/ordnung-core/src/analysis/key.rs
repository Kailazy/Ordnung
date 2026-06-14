//! Musical key detection: fold the spectrogram into a 12-bin chromagram, then
//! correlate against Krumhansl-Schmuckler major/minor key profiles. Returns the
//! canonical `Key`, which renders Camelot by default (see the `audio-analysis` skill).

use super::dsp::Spectrogram;
use crate::model::key::{Key, Mode, PitchClass};

// `edma` key profiles from Faraldo et al., "Key Estimation in Electronic Dance
// Music" (ECIR 2016) — derived from corpus analysis of EDM and shown to beat the
// generic Krumhansl/Sha'ath profiles on this repertoire. Generic (classical)
// profiles barely distinguish a key from its relative minor; these encode EDM's
// tonal statistics, which fixes most of the major/minor side errors.
const MAJOR: [f32; 12] = [
    1.0, 0.2875, 0.5020, 0.4048, 0.6050, 0.5614, 0.3205, 0.7966, 0.3159, 0.4506, 0.4202, 0.3889,
];
const MINOR: [f32; 12] = [
    1.0, 0.3096, 0.4415, 0.5827, 0.3262, 0.4948, 0.2889, 0.7804, 0.4328, 0.2903, 0.5331, 0.3217,
];

/// Multiplies minor-key scores before the major/minor decision. EDM skews minor,
/// and a (>1) bias corrects residual parallel-/relative-major errors. 1.0 = neutral.
/// (Faraldo et al.'s tunable "mode bias".) Calibrated to 1.20 on the 79-track
/// labelled set (see KEY_CHECK.md): it recovers the parallel-major flips (e.g. Fm
/// read as F major) without over-calling minor (74/79 vs rekordbox's 71/79).
const MINOR_MODE_BIAS: f32 = 1.20;

/// Pitched band for key detection. The low edge is the critical tuning knob: 90 Hz
/// (≈F♯2) admits the bass-root fundamentals in the F2–A2 octave where much techno
/// sits — the old 110 Hz floor cut them, leaving only the fifth/harmonics so the
/// detector latched onto the wrong tonic. Going lower (≤70 Hz) re-admits kick/sub
/// transients that smear across pitch classes. Dropping 110→90 was the single
/// biggest accuracy gain on the labelled set (21%→34% exact). The high edge stays
/// below the noisy top end.
const F_MIN: f32 = 90.0;
const F_MAX: f32 = 4000.0;

/// Build a normalized 12-bin chromagram (index 0 = C) using HPCP-style spectral
/// peak-picking: only local spectral maxima above a per-frame threshold contribute.
///
/// This is the crucial step — summing *every* bin lets broadband/percussive energy
/// smear across pitch classes and flattens the profile, which makes the tonic
/// unresolvable. Peaks isolate tonal content. Each peak is interpolated to a
/// precise frequency (parabolic) for accurate pitch-class assignment, the binning
/// is shifted by the track's estimated tuning so off-A440 masters don't leak into
/// adjacent pitch classes, and each frame is L1-normalized so loud frames don't
/// dominate.
pub fn chromagram(spec: &Spectrogram) -> [f32; 12] {
    // Parabolic peak interpolation + pitch mapping is the expensive part, and the
    // tuning estimate and the chroma fold need exactly the same peaks. Pick them
    // once and reuse — bit-identical to the old two-pass form, half the spectral
    // scans (each peak's `peak_freq`/`log2` ran twice before).
    let frames = collect_peaks(spec);
    let tuning = estimate_tuning(&frames); // semitone fraction to subtract before binning

    let mut chroma = [0.0f32; 12];
    for peaks in &frames {
        let mut frame_chroma = [0.0f32; 12];
        for &(midi, mag) in peaks {
            let pc = ((midi - tuning).round() as i32).rem_euclid(12) as usize;
            frame_chroma[pc] += mag;
        }

        let fsum: f32 = frame_chroma.iter().sum();
        if fsum > 0.0 {
            for pc in 0..12 {
                chroma[pc] += frame_chroma[pc] / fsum;
            }
        }
    }
    let sum: f32 = chroma.iter().sum();
    if sum > 0.0 {
        for c in &mut chroma {
            *c /= sum;
        }
    }
    chroma
}

/// Pick each frame's tonal peaks: spectral local maxima above a per-frame noise
/// floor, in the pitched band, mapped to a (MIDI, magnitude) pair. `midi` carries
/// no tuning offset yet — the caller subtracts the global tuning before binning.
fn collect_peaks(spec: &Spectrogram) -> Vec<Vec<(f32, f32)>> {
    let mut frames = Vec::with_capacity(spec.frames.len());
    for frame in &spec.frames {
        let frame_max = frame.iter().cloned().fold(0.0f32, f32::max);
        let mut peaks = Vec::new();
        if frame_max > 0.0 {
            let threshold = 0.1 * frame_max; // ignore the noise floor
            for i in 1..frame.len() - 1 {
                let mag = frame[i];
                // Local maximum above threshold = a tonal peak.
                if mag < threshold || mag < frame[i - 1] || mag < frame[i + 1] {
                    continue;
                }
                let f = peak_freq(spec, frame, i);
                if !(F_MIN..=F_MAX).contains(&f) {
                    continue;
                }
                peaks.push((69.0 + 12.0 * (f / 440.0).log2(), mag));
            }
        }
        frames.push(peaks);
    }
    frames
}

/// Estimate the track's global tuning deviation from equal temperament, as a
/// fraction of a semitone in [-0.5, 0.5]. Computed as the magnitude-weighted
/// *circular* mean of every tonal peak's distance from the nearest equal-tempered
/// pitch (period = one semitone), so a master tuned a few cents off A440 (or pitched
/// for a remix) doesn't smear each note across two pitch-class bins and shift the
/// detected tonic. Mirrors what rekordbox/Essentia do before chroma binning.
fn estimate_tuning(frames: &[Vec<(f32, f32)>]) -> f32 {
    let (mut sin, mut cos) = (0.0f64, 0.0f64);
    for peaks in frames {
        for &(midi, mag) in peaks {
            let frac = (midi - midi.round()) as f64; // [-0.5, 0.5] semitone
            let theta = 2.0 * std::f64::consts::PI * frac;
            sin += mag as f64 * theta.sin();
            cos += mag as f64 * theta.cos();
        }
    }
    if sin == 0.0 && cos == 0.0 {
        0.0
    } else {
        (sin.atan2(cos) / (2.0 * std::f64::consts::PI)) as f32
    }
}

/// Parabolic interpolation around bin `i` for a sub-bin-accurate peak frequency.
fn peak_freq(spec: &Spectrogram, frame: &[f32], i: usize) -> f32 {
    let (a, b, c) = (frame[i - 1], frame[i], frame[i + 1]);
    let denom = a - 2.0 * b + c;
    let delta = if denom != 0.0 {
        0.5 * (a - c) / denom
    } else {
        0.0
    };
    spec.bin_hz(i) + delta * spec.sample_rate as f32 / super::dsp::WINDOW as f32
}

/// Detect the key by best profile correlation over all 12 tonics × {major, minor}.
pub fn detect(spec: &Spectrogram) -> Option<Key> {
    let chroma = chromagram(spec);
    if chroma.iter().all(|&c| c == 0.0) {
        return None;
    }

    let mut best: Option<(f32, PitchClass, Mode)> = None;
    for tonic in 0..12 {
        let maj = correlation(&chroma, &MAJOR, tonic);
        let min_raw = correlation(&chroma, &MINOR, tonic);
        // Bias only positive scores so a negative correlation isn't "rewarded".
        let min = if min_raw > 0.0 {
            min_raw * MINOR_MODE_BIAS
        } else {
            min_raw
        };
        for (score, mode) in [(maj, Mode::Major), (min, Mode::Minor)] {
            if best.is_none_or(|(b, _, _)| score > b) {
                best = Some((score, PitchClass::new(tonic as u8), mode));
            }
        }
    }
    best.map(|(_, tonic, mode)| Key::new(tonic, mode))
}

/// Pearson correlation between the chromagram and a key profile rotated so its
/// tonic sits at pitch class `tonic`.
fn correlation(chroma: &[f32; 12], profile: &[f32; 12], tonic: usize) -> f32 {
    let rotated: Vec<f32> = (0..12).map(|i| profile[(i + 12 - tonic) % 12]).collect();
    pearson(chroma, &rotated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::dsp::spectrogram;

    fn sine(sr: u32, freq: f32, secs: u32) -> Vec<f32> {
        let n = sr as usize * secs as usize;
        (0..n)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * 0.5)
            .collect()
    }

    #[test]
    fn chromagram_peaks_at_played_pitch() {
        // A4 = 440 Hz → pitch class 9 (A).
        let sr = 44_100;
        let spec = spectrogram(&sine(sr, 440.0, 5), sr);
        let chroma = chromagram(&spec);
        let argmax = (0..12)
            .max_by(|&a, &b| chroma[a].partial_cmp(&chroma[b]).unwrap())
            .unwrap();
        assert_eq!(argmax, 9, "expected pitch class A, chroma = {chroma:?}");
    }
}

fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len() as f32;
    let ma = a.iter().sum::<f32>() / n;
    let mb = b.iter().sum::<f32>() / n;
    let mut num = 0.0;
    let mut da = 0.0;
    let mut db = 0.0;
    for i in 0..a.len() {
        let xa = a[i] - ma;
        let xb = b[i] - mb;
        num += xa * xb;
        da += xa * xa;
        db += xb * xb;
    }
    let den = (da * db).sqrt();
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}
