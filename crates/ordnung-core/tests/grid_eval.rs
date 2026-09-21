//! Beatgrid parity against rekordbox 7, fully local — no USB needed.
//!
//! `testdata/rekordbox-grids/*.tsv` hold rekordbox 7's own PQTZ beatgrids
//! (position, BPM, bar number per beat) for the tracks in
//! `testdata/seeker-sample` that also live on the EYEBAGS reference export —
//! the 17-track *dev* set the v26 fixes were tuned on. `rekordbox-grids-holdout`
//! is a further 50 drawn at random (2026-09-21) that no tuning has seen; it is
//! graded separately so a change that only fits the dev set shows. For each
//! track this runs the production grid pipeline (`tempo::detect` →
//! `lock_grid` → `downbeat::detect_phase`) and grades:
//!
//!   * **BPM** — ours vs rekordbox's (within 0.5%).
//!   * **Phase** — the median position of rekordbox's beats relative to our
//!     nearest grid line, in beats. rekordbox stamps its line at the kick's
//!     attack, which is where the snap's foot lands, so the target is zero.
//!     In phase = within [`PHASE_TOL_BT`] of it.
//!   * **Downbeat** — informational only (see `downbeat_eval` in ordnung-rbdb:
//!     Ordnung deliberately puts the "1" at the kick's entrance).
//!
//! The asserted floors are the calibrated baseline; a change that drops
//! either is a regression, not noise. `GRID_EVAL_TRACKS=a,b` restricts the run
//! to file-name substrings; for a miss, `tempo`'s `debug_snap_on_real_track`
//! prints the folded beat profiles the snap voted on.
//!
//! Run: cargo test -p ordnung-core --test grid_eval --release -- --ignored --nocapture

use ordnung_core::analysis::{decode_mono_capped, downbeat, dsp::spectrogram, tempo};
use std::path::{Path, PathBuf};

/// Decode cap: the production key/tempo window is 150 s; a little slack past it.
const DECODE_CAP_SECS: usize = 160;
const BAR: u32 = 4;

/// In-phase tolerance, in beats (~70 ms at club tempo).
const PHASE_TOL_BT: f64 = 0.15;
/// Calibrated floors per set: (fixture dir, tracks in phase, right BPM).
const SETS: &[(&str, u32, u32)] = &[("rekordbox-grids", 12, 17), ("rekordbox-grids-holdout", 44, 50)];

struct RbBeat {
    position_ms: u64,
    bpm: f32,
    number: u32,
}

fn read_grid(path: &Path) -> Vec<RbBeat> {
    let text = std::fs::read_to_string(path).unwrap_or_default();
    text.lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split('\t');
            Some(RbBeat {
                position_ms: it.next()?.parse().ok()?,
                bpm: it.next()?.parse().ok()?,
                number: it.next()?.parse().ok()?,
            })
        })
        .collect()
}

fn audio_for(stem: &str, sample_dir: &Path) -> Option<PathBuf> {
    ["mp3", "flac", "aiff", "aif", "wav", "m4a"]
        .iter()
        .map(|ext| sample_dir.join(format!("{stem}.{ext}")))
        .find(|p| p.exists())
}

#[test]
#[ignore = "decodes 67 real tracks; run in release"]
fn grid_matches_rekordbox() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../testdata");
    let only: Vec<String> = std::env::var("GRID_EVAL_TRACKS")
        .map(|s| s.split(',').map(|s| s.trim().to_lowercase()).collect())
        .unwrap_or_default();
    let mut failures = Vec::new();
    for &(set, min_in_phase, min_bpm_ok) in SETS {
        let Some((n_bpm_ok, n_in_phase)) = eval_set(&root, set, &only) else { continue };
        if only.is_empty() {
            if n_bpm_ok < min_bpm_ok {
                failures.push(format!("{set}: BPM regressed: {n_bpm_ok} < {min_bpm_ok}"));
            }
            if n_in_phase < min_in_phase {
                failures.push(format!("{set}: phase regressed: {n_in_phase} < {min_in_phase}"));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Grade one fixture set; returns `(bpm ok, in phase)`, or `None` when the
/// set has no gradable tracks.
fn eval_set(root: &Path, set: &str, only: &[String]) -> Option<(u32, u32)> {
    let sample_dir = root.join("seeker-sample");
    let Ok(dir) = std::fs::read_dir(root.join(set)) else {
        eprintln!("no fixture dir testdata/{set}");
        return None;
    };
    let mut grids: Vec<PathBuf> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "tsv"))
        .collect();
    grids.sort();
    println!("\n--- {set} ({} grids) ---", grids.len());

    let mut n_tracks = 0u32;
    let mut n_bpm_ok = 0u32;
    let mut n_in_phase = 0u32;
    let mut n_downbeat_ok = 0u32;
    let mut misses: Vec<String> = Vec::new();

    for grid in &grids {
        let stem = grid.file_stem().unwrap().to_string_lossy().to_string();
        if !only.is_empty() && !only.iter().any(|w| stem.to_lowercase().contains(w.as_str())) {
            continue;
        }
        let Some(audio_path) = audio_for(&stem, &sample_dir) else {
            eprintln!("{stem}: no audio in testdata/seeker-sample");
            continue;
        };
        let rb = read_grid(grid);
        if rb.len() < 16 {
            continue;
        }
        let name: String = stem.chars().take(44).collect();
        let audio = decode_mono_capped(&audio_path, Some(48_000 * DECODE_CAP_SECS)).expect("decode");
        let spec = spectrogram(&audio.samples, audio.sample_rate);
        let t = tempo::detect(&spec);
        assert!(t.bpm > 0.0, "{name}: no tempo");
        let (bpm, anchor_ms) =
            tempo::lock_grid(&audio.samples, audio.sample_rate, t.bpm, t.beat_offset_ms);
        let phase = downbeat::detect_phase(&spec, bpm, anchor_ms);
        let first_beat_number = ((BAR - phase % BAR) % BAR) + 1;

        let mut rb_bpms: Vec<f32> = rb.iter().map(|b| b.bpm).collect();
        rb_bpms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rb_bpm = rb_bpms[rb_bpms.len() / 2];
        let bpm_ok = (bpm - rb_bpm).abs() / rb_bpm < 0.005;

        let dur_ms = audio.samples.len() as u64 * 1000 / audio.sample_rate.max(1) as u64;
        let period = 60_000.0 / bpm as f64;
        let mut agree = 0u32;
        let mut total = 0u32;
        let mut fracs: Vec<f64> = Vec::new();
        for b in rb.iter().filter(|b| b.position_ms < dur_ms) {
            let raw = (b.position_ms as f64 - anchor_ms as f64) / period;
            let i = raw.round() as i64;
            fracs.push(raw - i as f64);
            total += 1;
            let ours = ((first_beat_number as i64 - 1 + i).rem_euclid(BAR as i64)) as u32 + 1;
            if b.number == ours {
                agree += 1;
            }
        }
        if total == 0 {
            continue;
        }
        fracs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med_frac = fracs[fracs.len() / 2];
        let in_phase = med_frac.abs() <= PHASE_TOL_BT;
        let agree_pct = agree as f32 * 100.0 / total as f32;

        n_tracks += 1;
        let mut flag = String::new();
        if !bpm_ok {
            flag = "  <-- BPM".into();
        } else {
            n_bpm_ok += 1;
            if in_phase {
                n_in_phase += 1;
                if agree_pct >= 75.0 {
                    n_downbeat_ok += 1;
                }
            } else {
                flag = "  <-- OFFBEAT".into();
            }
        }
        if !flag.is_empty() {
            misses.push(format!("{name} ({med_frac:+.2}bt, bpm {bpm:.2} vs {rb_bpm:.2})"));
        }
        println!(
            "{name:<46} bpm {bpm:>7.3} (rb {rb_bpm:>6.2})  phase {med_frac:>+5.2}bt ({:>+6.1}ms)  \
             anchor {anchor_ms:>4}ms  downbeat {agree_pct:>3.0}%{flag}",
            med_frac * period
        );
    }

    println!(
        "=== {set}: {n_tracks} tracks: bpm ok {n_bpm_ok}, in phase {n_in_phase}/{n_bpm_ok}, \
         downbeat ok {n_downbeat_ok}/{n_in_phase} ==="
    );
    if !misses.is_empty() {
        println!("misses:\n  {}", misses.join("\n  "));
    }
    if n_tracks == 0 {
        None
    } else {
        Some((n_bpm_ok, n_in_phase))
    }
}
