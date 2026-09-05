//! Diagnostic: per-band beat-synchronous energy profiles anchored on
//! *rekordbox's* grid, for tracks on the EYEBAGS reference USB. Shows what the
//! audio does at rekordbox's phase 0 — which element actually marks the beat —
//! when `downbeat_eval` flags a track as offbeat-gridded.
//!
//! Reading the rows (48 bins per beat, levels 0–9 normalized per band):
//! rekordbox stamps its line ~45 ms before the kick's energy foot, so a
//! correctly-understood track shows all bands rising a few bins after 0.
//!
//! Run: cargo test -p ordnung-rbdb --test phase_probe --release -- --ignored --nocapture
//! Set PROBE_TRACKS to a comma-separated list of file-name substrings.

use ordnung_core::analysis::{decode_mono_capped, tempo};
use ordnung_core::analysis::dsp::spectrogram;
use ordnung_rbdb::anlz::{read_beatgrid, read_track_path};
use std::path::{Path, PathBuf};

const USB: &str = "/Volumes/EYEBAGS";

#[test]
#[ignore = "diagnostic; needs the EYEBAGS USB"]
fn beat_profiles_on_rekordbox_grid() {
    let wanted = std::env::var("PROBE_TRACKS")
        .unwrap_or_else(|_| "Two Chords,LawnChair,Scania,Everywhere You Go".into());
    let wanted: Vec<String> = wanted.split(',').map(|s| s.trim().to_string()).collect();
    let usb = Path::new(USB);
    for dat in walk_dats(&usb.join("PIONEER/USBANLZ")) {
        let Some(rel) = read_track_path(&dat) else { continue };
        if !wanted.iter().any(|w| rel.contains(w.as_str())) {
            continue;
        }
        let audio_path = usb.join(rel.trim_start_matches('/'));
        let rb = read_beatgrid(&dat);
        if rb.len() < 8 {
            continue;
        }
        let Ok(audio) = decode_mono_capped(&audio_path, Some(48_000 * 160)) else { continue };
        let sr = audio.sample_rate as f32;

        let spec = spectrogram(&audio.samples, audio.sample_rate);
        let t = tempo::detect(&spec);
        let (bpm, anchor) =
            tempo::lock_grid(&audio.samples, audio.sample_rate, t.bpm, t.beat_offset_ms);

        let rb0 = rb.first().unwrap().position_ms as f64;
        let rbn = rb.last().unwrap();
        let rb_period = (rbn.position_ms as f64 - rb0) / (rb.len() - 1) as f64;
        let hop_ms = 64.0 / sr as f64 * 1000.0;
        let our_phase = (((anchor as f64 - rb0) / rb_period).fract() + 1.0).fract();
        let name = audio_path.file_name().unwrap().to_string_lossy();
        println!("\n{name}  bpm {bpm:.2} anchor {anchor}ms  our phase {our_phase:+.2}bt of rb");

        let fine = |env: &[f32], label: &str| {
            let mut hist = [0.0f64; 48];
            let mut cnt = [0u32; 48];
            for (i, &f) in env.iter().enumerate() {
                let ms = i as f64 * hop_ms;
                if ms < rb0 {
                    continue;
                }
                let ph = ((ms - rb0) / rb_period).fract();
                let b = (ph * 48.0) as usize % 48;
                hist[b] += f as f64;
                cnt[b] += 1;
            }
            for (h, &c) in hist.iter_mut().zip(&cnt) {
                *h /= c.max(1) as f64;
            }
            let (lo, hi) = hist
                .iter()
                .fold((f64::MAX, f64::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            let row: String = hist
                .iter()
                .map(|&h| {
                    char::from_digit((((h - lo) / (hi - lo).max(1e-12)) * 9.0) as u32, 10).unwrap()
                })
                .collect();
            println!("  {label:<12} |{row}|");
        };
        fine(&band_env(&audio.samples, sr, Band::Sub), "energy sub");
        fine(&band_env(&audio.samples, sr, Band::Full), "energy full");
        fine(&band_env(&audio.samples, sr, Band::Hi), "energy hi");
        println!("               |0...........12..........24..........36..........|  (48ths of a beat)");
    }
}

enum Band {
    Sub,
    Full,
    Hi,
}

/// RMS envelope per 64-sample hop for one band (sub: two-pole 60 Hz low-pass,
/// hi: one-pole 4 kHz high-pass).
fn band_env(samples: &[f32], sr: f32, band: Band) -> Vec<f32> {
    const HOP: usize = 64;
    const WIN: usize = 256;
    let filtered: Vec<f32> = match band {
        Band::Full => samples.to_vec(),
        Band::Sub => {
            let a = 1.0 - (-std::f32::consts::TAU * 60.0 / sr).exp();
            let mut out = samples.to_vec();
            for _ in 0..2 {
                let mut lp = 0.0f32;
                for x in &mut out {
                    lp += a * (*x - lp);
                    *x = lp;
                }
            }
            out
        }
        Band::Hi => {
            let a = 1.0 - (-std::f32::consts::TAU * 4000.0 / sr).exp();
            let mut lp = 0.0f32;
            samples
                .iter()
                .map(|&s| {
                    lp += a * (s - lp);
                    s - lp
                })
                .collect()
        }
    };
    let blocks = filtered.len().saturating_sub(WIN) / HOP;
    (0..blocks)
        .map(|b| {
            let s = b * HOP;
            let sum: f64 = filtered[s..s + WIN].iter().map(|&x| (x * x) as f64).sum();
            (sum / WIN as f64).sqrt() as f32
        })
        .collect()
}

fn walk_dats(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk_dats(&p));
        } else if p.file_name().is_some_and(|n| n == "ANLZ0000.DAT") {
            out.push(p);
        }
    }
    out
}
