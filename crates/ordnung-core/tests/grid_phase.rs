//! Where the beatgrid's *anchor* lands, as opposed to how fast it ticks.
//!
//! The BPM tests in `analysis::tempo` only ever checked the period, which let a
//! systematic ~74 ms phase error survive: STFT frame indices were being read as
//! timestamps, but a frame describes the audio at the centre of its window, and
//! spectral flux peaks while a transient is still climbing into that window. A
//! grid can be perfectly in tempo and still sit visibly before every kick, so
//! the anchor gets its own test.

use ordnung_core::analysis::{dsp::spectrogram, tempo};

/// Kick-like transient train: 60 Hz thump plus a click, first hit at `offset_ms`.
fn kicks(sr: u32, bpm: f32, secs: u32, offset_ms: f32) -> Vec<f32> {
    let period = 60.0 / bpm * sr as f32;
    let n = sr as usize * secs as usize;
    let mut s = vec![0.0f32; n];
    let mut beat = offset_ms / 1000.0 * sr as f32;
    while (beat as usize) < n {
        let start = beat as usize;
        for j in 0..(sr as usize / 5) {
            if start + j >= n {
                break;
            }
            let t = j as f32 / sr as f32;
            s[start + j] += (2.0 * std::f32::consts::PI * 60.0 * t).sin()
                * (-(j as f32) / (sr as f32 * 0.04)).exp()
                * 0.9;
            s[start + j] += (2.0 * std::f32::consts::PI * 1800.0 * t).sin()
                * (-(j as f32) / (sr as f32 * 0.004)).exp()
                * 0.4;
        }
        beat += period;
    }
    s
}

/// Signed distance from `ms` to the nearest true beat, in `(-period/2, period/2]`.
fn phase_error(ms: u64, offset_ms: f32, bpm: f32) -> f32 {
    let period = 60_000.0 / bpm;
    let d = ms as f32 - offset_ms;
    d - (d / period).round() * period
}

#[test]
fn anchor_lands_on_the_transient() {
    let sr = 44_100;
    for &bpm in &[120.0f32, 128.0, 174.0] {
        for &offset in &[0.0f32, 37.0, 123.0, 400.0] {
            let s = kicks(sr, bpm, 40, offset);
            let spec = spectrogram(&s, sr);
            let t = tempo::detect(&spec);
            let anchor = tempo::snap_anchor(&s, sr, t.bpm, t.beat_offset_ms);
            let err = phase_error(anchor, offset, bpm);
            assert!(
                err.abs() < 10.0,
                "bpm {bpm}, offset {offset}ms: anchor {anchor}ms is {err:+.1}ms off the kick"
            );
        }
    }
}

#[test]
fn snap_pulls_a_deliberately_early_anchor_back() {
    let sr = 44_100;
    let (bpm, offset) = (128.0f32, 200.0f32);
    let s = kicks(sr, bpm, 30, offset);
    // Hand the snap an anchor 60 ms early — roughly the error the old frame-index
    // timestamping produced — and it should walk back onto the kick.
    let snapped = tempo::snap_anchor(&s, sr, bpm, (offset - 60.0) as u64);
    assert!(
        phase_error(snapped, offset, bpm).abs() < 10.0,
        "expected ~{offset}ms, got {snapped}ms"
    );
}

#[test]
fn snap_keeps_the_coarse_anchor_when_there_are_no_transients() {
    // A steady tone has no attacks to lock onto; the score curve is flat and the
    // snap must decline rather than pick the noise floor's argmax.
    let sr = 44_100;
    let s: Vec<f32> = (0..sr as usize * 20)
        .map(|i| (2.0 * std::f32::consts::PI * 220.0 * i as f32 / sr as f32).sin() * 0.3)
        .collect();
    assert_eq!(tempo::snap_anchor(&s, sr, 128.0, 350), 350);
}
