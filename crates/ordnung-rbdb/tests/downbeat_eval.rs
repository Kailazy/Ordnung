//! Downbeat placement against rekordbox ground truth — does our "1" land on
//! rekordbox's "1"?
//!
//! The EYEBAGS USB (a verified rekordbox 7 export) carries both the audio
//! (`/Contents`) and rekordbox's own beatgrids (`PIONEER/USBANLZ/**/ANLZ0000.DAT`,
//! `PQTZ` sections with per-beat bar numbers). For every track this runs the
//! production grid pipeline (`tempo::detect` → `lock_grid` → `downbeat::detect_phase`)
//! and grades three things against the rekordbox grid:
//!
//!   * **BPM** — ours vs the rekordbox median (metrical confusions flagged).
//!   * **Anchor** — median signed distance from rekordbox beats to ours.
//!   * **Downbeat** — over rekordbox beats that align with one of ours, how often
//!     the bar numbers agree, plus the modal `(rb − ours) mod 4` offset so a
//!     systematic flip (e.g. backbeat picked as the "1") is visible.
//!
//! Needs the mounted EYEBAGS volume, so it's `#[ignore]`d like the other evals.
//!
//! Run: cargo test -p ordnung-rbdb --test downbeat_eval --release -- --ignored --nocapture

use ordnung_core::analysis::{decode_mono_capped, downbeat, dsp::spectrogram, tempo};
use ordnung_rbdb::anlz::{read_beatgrid, read_track_path};
use std::path::{Path, PathBuf};

const USB: &str = "/Volumes/EYEBAGS";
/// Decode cap: the production key/tempo window is 150 s; a little slack past it.
const DECODE_CAP_SECS: usize = 160;
const BAR: u32 = 4;

#[test]
#[ignore = "needs the EYEBAGS reference USB mounted"]
fn downbeat_matches_rekordbox() {
    let usb = Path::new(USB);
    let mut dats: Vec<PathBuf> = walk_dats(&usb.join("PIONEER/USBANLZ"));
    dats.sort();
    if dats.is_empty() {
        eprintln!("no ANLZ files under {USB}");
        return;
    }

    let mut n_tracks = 0u32;
    let mut n_bpm_ok = 0u32;
    let mut n_anchor_ok = 0u32; // grids in phase (|offset| < 0.15 beat)
    let mut n_phase_ok = 0u32; // downbeat agreement over in-phase tracks
    let mut offset_hist = [0u32; BAR as usize]; // modal (rb - ours) mod 4 per track
    let mut misses: Vec<String> = Vec::new();
    let mut offbeat: Vec<String> = Vec::new();

    for dat in &dats {
        let Some(rel) = read_track_path(dat) else { continue };
        let audio_path = usb.join(rel.trim_start_matches('/'));
        let rb = read_beatgrid(dat);
        if rb.len() < 16 || !audio_path.exists() {
            continue;
        }
        let name: String = audio_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .chars()
            .take(44)
            .collect();

        let Ok(audio) = decode_mono_capped(&audio_path, Some(48_000 * DECODE_CAP_SECS)) else {
            eprintln!("{name:<46} decode failed");
            continue;
        };
        let spec = spectrogram(&audio.samples, audio.sample_rate);
        let t = tempo::detect(&spec);
        if t.bpm <= 0.0 {
            eprintln!("{name:<46} no tempo");
            continue;
        }
        let (bpm, anchor_ms) = tempo::lock_grid(&audio.samples, audio.sample_rate, t.bpm, t.beat_offset_ms);
        let phase = downbeat::detect_phase(&spec, bpm, anchor_ms);
        let first_beat_number = ((BAR - phase % BAR) % BAR) + 1;

        // Rekordbox reference: median BPM over its beats (grids can have segments).
        let mut rb_bpms: Vec<f32> = rb.iter().map(|b| b.bpm).collect();
        rb_bpms.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let rb_bpm = rb_bpms[rb_bpms.len() / 2];
        let bpm_ok = (bpm - rb_bpm).abs() / rb_bpm < 0.005;

        // Sub-beat phase offset (ours vs rekordbox) and downbeat agreement over
        // rekordbox beats inside our analyzed window. `frac` is where each rb
        // beat falls relative to our nearest grid line, in beats (−0.5, 0.5];
        // its median is the grids' phase offset — near 0 when our anchor sits on
        // rekordbox's beat, near ±0.5 when we've gridded the offbeat.
        let dur_ms = audio.samples.len() as u64 * 1000 / audio.sample_rate.max(1) as u64;
        let period = 60_000.0 / bpm as f64;
        let mut agree = 0u32;
        let mut total = 0u32;
        let mut fracs: Vec<f64> = Vec::new();
        let mut offs = [0u32; BAR as usize];
        for b in rb.iter().filter(|b| b.position_ms < dur_ms) {
            let raw = (b.position_ms as f64 - anchor_ms as f64) / period;
            let i = raw.round() as i64;
            fracs.push(raw - i as f64);
            total += 1;
            let ours = ((first_beat_number as i64 - 1 + i).rem_euclid(BAR as i64)) as u32 + 1;
            let off = ((b.number as i64 - ours as i64).rem_euclid(BAR as i64)) as usize;
            offs[off] += 1;
            if off == 0 {
                agree += 1;
            }
        }
        if total == 0 {
            continue;
        }
        let agree_pct = agree as f32 * 100.0 / total as f32;
        fracs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let med_frac = fracs[fracs.len() / 2];
        // rekordbox stamps lines ~45 ms before the kick's energy foot, where
        // our anchor sits (see tempo::RB_GRID_LEAD_MS); compare around that.
        let expected = -ordnung_core::analysis::tempo::RB_GRID_LEAD_MS / period;
        let anchor_ok = (med_frac - expected).abs() < 0.15;
        let modal_off = offs
            .iter()
            .enumerate()
            .max_by_key(|(_, &c)| c)
            .map(|(o, _)| o)
            .unwrap_or(0);

        n_tracks += 1;
        if bpm_ok {
            n_bpm_ok += 1;
            if anchor_ok {
                n_anchor_ok += 1;
                offset_hist[modal_off] += 1;
                let ok = agree_pct >= 75.0;
                if ok {
                    n_phase_ok += 1;
                } else {
                    misses.push(format!("{name} (off {modal_off}, {agree_pct:.0}%)"));
                }
                println!(
                    "{name:<46} bpm {bpm:>6.2} (rb {rb_bpm:>6.2})  phase {:>+5.2}bt ({:>+6.1}ms)  \
                     downbeat {agree_pct:>3.0}% off={modal_off} {}",
                    med_frac,
                    med_frac * period,
                    if ok { "" } else { "  <-- MISS" }
                );
            } else {
                offbeat.push(format!("{name} ({med_frac:+.2}bt)"));
                println!(
                    "{name:<46} bpm {bpm:>6.2} (rb {rb_bpm:>6.2})  phase {:>+5.2}bt ({:>+6.1}ms)  \
                     <-- OFFBEAT GRID",
                    med_frac,
                    med_frac * period,
                );
            }
        } else {
            println!(
                "{name:<46} bpm {bpm:>6.2} (rb {rb_bpm:>6.2})  BPM MISMATCH — downbeat not graded"
            );
        }
    }

    println!(
        "\n=== {n_tracks} tracks: bpm ok {n_bpm_ok}, in phase {n_anchor_ok}/{n_bpm_ok}, \
         downbeat ok {n_phase_ok}/{n_anchor_ok} ==="
    );
    println!(
        "modal (rb - ours) mod 4 over in-phase tracks: 0:{} 1:{} 2:{} 3:{}",
        offset_hist[0], offset_hist[1], offset_hist[2], offset_hist[3]
    );
    if !offbeat.is_empty() {
        println!("offbeat grids:\n  {}", offbeat.join("\n  "));
    }
    if !misses.is_empty() {
        println!("downbeat misses:\n  {}", misses.join("\n  "));
    }
}

fn walk_dats(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
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
