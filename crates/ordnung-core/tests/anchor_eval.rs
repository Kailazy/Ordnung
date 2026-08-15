//! Beatgrid *anchor* placement against real records — does the first beat land on
//! a kick?
//!
//! `grid_phase.rs` proves the anchor on synthesized kicks, where the truth is
//! known exactly. This grades the same thing on actual club material, where it
//! can't be: for each track it measures low-band attack energy (200 Hz one-pole,
//! ~1.5 ms blocks, positive RMS change) sampled at every beat of the produced
//! grid, as a multiple of the track's average. A grid sitting on the kicks scores
//! well above 1×; one sitting between them scores at or below it.
//!
//! Needs the local `testdata/seeker-sample` folder (not in the repo), so it's
//! `#[ignore]`d like the other eval harnesses.
//!
//! Run: cargo test -p ordnung-core --test anchor_eval --release -- --ignored --nocapture

use ordnung_core::analysis::{self, AnalysisParams};
use std::path::Path;

/// The anchor must collect clearly more kick energy than the track's average.
const MIN_LIFT: f32 = 1.2;

#[test]
#[ignore = "needs the local testdata/seeker-sample library"]
fn anchor_sits_on_the_kick() {
    let dir = Path::new("../../testdata/seeker-sample");
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("no sample library at {}", dir.display());
        return;
    };
    let mut files: Vec<_> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|s| s.to_str()),
                Some("mp3") | Some("aiff") | Some("aif") | Some("wav") | Some("flac")
            )
        })
        .collect();
    files.sort();

    let mut weak = Vec::new();
    for p in files.iter().take(12) {
        let a = analysis::analyze_file(p, AnalysisParams::default()).unwrap();
        let (Some(bpm), Some(b0)) = (a.bpm, a.beatgrid.beats.first().map(|b| b.position_ms)) else {
            continue;
        };
        let audio = analysis::decode_mono_capped(p, Some(44_100 * 150)).unwrap();
        let lift = kick_lift(&audio.samples, audio.sample_rate, bpm, b0 as f32);
        // Half a beat off is the classic failure; report it as the control.
        let control = kick_lift(&audio.samples, audio.sample_rate, bpm, b0 as f32 + 30_000.0 / bpm);
        let name: String = p.file_name().unwrap().to_string_lossy().chars().take(48).collect();
        println!("{name:<50} bpm={bpm:>6.2} anchor={b0:>5}ms  kick {lift:.2}× (off-beat {control:.2}×)");
        if lift < MIN_LIFT {
            weak.push(format!("{name} ({lift:.2}×)"));
        }
    }
    assert!(
        weak.is_empty(),
        "anchors not landing on the kick: {}",
        weak.join(", ")
    );
}

/// Mean low-band attack energy at the beats of a grid anchored at `offset_ms`,
/// as a multiple of the track's mean.
fn kick_lift(samples: &[f32], sample_rate: u32, bpm: f32, offset_ms: f32) -> f32 {
    const HOP: usize = 64;
    const WIN: usize = 256;
    let sr = sample_rate as f32;
    let a = 1.0 - (-std::f32::consts::TAU * 200.0 / sr).exp();
    let mut lp = 0.0f32;
    let low: Vec<f32> = samples
        .iter()
        .map(|&s| {
            lp += a * (s - lp);
            lp
        })
        .collect();
    let blocks = low.len().saturating_sub(WIN) / HOP;
    let rms: Vec<f32> = (0..blocks)
        .map(|k| {
            let s: f32 = low[k * HOP..k * HOP + WIN].iter().map(|v| v * v).sum();
            (s / WIN as f32).sqrt()
        })
        .collect();
    let flux: Vec<f32> = (0..rms.len())
        .map(|i| if i == 0 { 0.0 } else { (rms[i] - rms[i - 1]).max(0.0) })
        .collect();
    let mean = flux.iter().sum::<f32>() / flux.len().max(1) as f32;
    if mean <= 0.0 || bpm <= 0.0 {
        return 0.0;
    }
    let period = 60_000.0 / bpm;
    let (mut t, mut sum, mut n) = (offset_ms.max(0.0), 0.0f32, 0u32);
    loop {
        let i = (t / 1000.0 * sr / HOP as f32) as usize;
        if i + 1 >= flux.len() {
            break;
        }
        sum += flux[i];
        n += 1;
        t += period;
    }
    sum / n.max(1) as f32 / mean
}
