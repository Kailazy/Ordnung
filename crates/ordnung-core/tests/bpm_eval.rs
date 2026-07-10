//! BPM-detection accuracy regression test against rekordbox ground truth.
//!
//! Runs the *production* `tempo::detect` over the same 79-track labelled sample set
//! as `key_eval`, grading each detected BPM against the rekordbox value the user
//! transcribed (the `BPM↣rb` column of KEY_CHECK.md). Asserts the within-2-BPM and
//! modulo-octave rates stay at or above the calibrated baseline, so re-enabling and
//! later tuning the tempo path can't silently regress it.
//!
//! Grading mirrors KEY_CHECK.md's BPM flag:
//!   ok  — within `BPM_TOL` of rekordbox
//!   8ve — within tolerance of half or double rekordbox (octave error)
//!   X   — otherwise
//!
//! This covers *tempo*; first-downbeat placement gets its own ground truth and
//! grading once dedicated downbeat detection lands (it needs rekordbox anchor ms,
//! not in KEY_CHECK.md). The grid is spot-checked for consistency below.
//!
//! Run: cargo test -p ordnung-core --test bpm_eval --release -- --ignored --nocapture

use ordnung_core::analysis::{decode_mono_capped, downbeat, dsp, tempo};
use std::path::PathBuf;

/// (filename substring, rekordbox BPM). Needles match `key_eval`; BPM is the value
/// after `↣` in KEY_CHECK.md (rekordbox's own reading — the target we grade against,
/// even where rekordbox itself picked a half/double tempo, e.g. Requiem at 75).
const GROUND_TRUTH: &[(&str, f32)] = &[
    ("(38) [Toasty]", 141.0),
    ("00 - Basso Ostinato", 125.0),
    ("Lime In Da Coconut", 130.0),
    ("303 Views", 125.0),
    ("01 - B-PAX", 124.0),
    ("Birmingham Screwdriver", 167.0),
    ("Cascade Effect", 136.0),
    ("01 - Dimensional", 118.0),
    ("01 - Elevation", 125.0),
    ("Enteroctopus Dofleini", 125.0),
    ("01 - I Love Ya", 122.0),
    ("Midtown 120 Intro", 120.0),
    ("01 - Sentient", 130.0),
    ("Phylyps Trak", 144.0),
    ("Space Jelly", 127.0),
    ("02 - Ifeksa", 146.0),
    ("Midtown 120 Blues", 120.0),
    ("Sistol - Keno", 127.0),
    ("Ben Nevile", 127.0),
    ("cell_out-transcendance", 131.0),
    ("klint-horus", 143.0),
    ("whip_it_good", 134.0),
    ("02. Lava", 130.0),
    ("Mine Has A Shower", 122.0),
    ("basic_math_three", 127.0),
    ("04 - Reptilian", 130.0),
    ("04 Collider", 115.0),
    ("bidoben-unfair", 140.0),
    ("rene_wise-cutting_thick", 133.0),
    ("tekra-ybbob", 131.0),
    ("Kronberg 4", 126.0),
    ("domina (maurizio", 129.0),
    ("regis-point_of_entry", 134.0),
    ("Baby Ford - Monolense", 133.0),
    ("Dorisburg - Gripen", 125.0),
    ("models of wellbeing", 73.0),
    ("06 Lovesick", 104.0),
    ("cabanne-fraisheur", 126.0),
    ("Exit Strategy", 135.0),
    ("07 All That Nothing", 128.0),
    ("graviton", 125.0),
    ("metapattern-pseudo_user", 137.0),
    ("Fresh (Sprinkles Alt", 120.0),
    ("The Etheric Body", 134.0),
    ("Circul Globus", 125.0),
    ("1.02. Oriel", 125.0),
    ("dinky-twelve_to_four", 125.0),
    ("marcel_dettman-scourer", 130.0),
    ("scb-down_moment", 125.0),
    ("wenn_meine_mutti", 123.0),
    ("Deuce-Cue Ed", 130.0),
    ("ABRAX - OCB", 126.0),
    ("trance me up", 128.0),
    ("Made Your Point", 113.0),
    ("Neoclassicdub", 128.0),
    ("Basso Ostinato.mp3", 124.0),
    ("There's Galaxies Better", 126.0),
    ("Dub 22", 150.0),
    ("Double Lardon", 128.0),
    ("Ouane Forzeshow", 124.0),
    ("New Atlantis", 135.0),
    ("Transform Into Glass", 135.0),
    ("Cut 06", 126.0),
    ("EARTH JUMP", 140.0),
    ("Sand Blind", 121.0),
    ("J1. GS", 126.0),
    ("Detune (AWSI", 129.0),
    ("Mullet is in da house", 128.0),
    ("Scania", 133.0),
    ("RB208", 140.0),
    ("Klong", 125.0),
    ("Undertow", 129.0),
    ("Rezzett-Doyce", 129.0),
    ("When Will We Leave", 120.0),
    ("Ultimo Sentenza", 125.0),
    ("Not Your Business", 146.0),
    ("Young Seth - Moment", 122.0),
    ("gadget o'flow", 128.0),
    ("tadeo - requiem", 75.0),
];

/// Tolerance for an "ok" match (rekordbox rounds to whole BPM; our lock is <0.5 off
/// on steady material, so 2 BPM absorbs rounding + genuine near-misses like KEY_CHECK).
const BPM_TOL: f32 = 2.0;

#[derive(PartialEq, Clone, Copy)]
enum Grade {
    Ok,
    Octave,
    Miss,
}

fn grade(got: f32, want: f32) -> Grade {
    if (got - want).abs() <= BPM_TOL {
        Grade::Ok
    } else if (got - want * 2.0).abs() <= BPM_TOL || (got - want * 0.5).abs() <= BPM_TOL {
        Grade::Octave
    } else {
        Grade::Miss
    }
}

/// Calibrated floors. v16 (re-enabled tempo path) measured 72 within 2 BPM / 73
/// modulo octave here; v18 (metrical correction, `tempo::correct_metrical`) lifted it
/// to 75 / 76 by recovering the 3:2 & 5:4 slow folds. Floors sit a few tracks below
/// the v18 baseline (float/decoder cushion); assert `>=` so accuracy can improve
/// freely but a regression fails the build.
const MIN_OK: usize = 72;
const MIN_MODULO_OCTAVE: usize = 74;

#[test]
#[ignore = "decodes the sample set; run with --ignored --nocapture"]
fn bpm_accuracy_regression() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/seeker-sample");
    if !dir.is_dir() {
        eprintln!("skip: no sample dir");
        return;
    }

    let (mut ok, mut octave, mut miss, mut total) = (0, 0, 0, 0);
    let mut rows: Vec<(char, &str, f32, f32)> = Vec::new();

    for &(needle, want) in GROUND_TRUTH {
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().contains(needle)));
        let Some(path) = path else {
            panic!("no file matches ground-truth needle {needle:?}");
        };
        // Match analyze_file's window: the first 150 s of the decode.
        let Ok(audio) = decode_mono_capped(&path, Some(150 * 48_000)) else {
            eprintln!("decode failed: {needle:?}");
            continue;
        };
        let spec = dsp::spectrogram(&audio.samples, audio.sample_rate);
        let got = tempo::detect(&spec).bpm;
        total += 1;
        let g = grade(got, want);
        match g {
            Grade::Ok => ok += 1,
            Grade::Octave => octave += 1,
            Grade::Miss => miss += 1,
        }
        let mark = match g {
            Grade::Ok => 'o',
            Grade::Octave => '8',
            Grade::Miss => 'X',
        };
        rows.push((mark, needle, got, want));
    }

    let modulo_octave = ok + octave;
    println!("\n--- per-track (o within 2 BPM · 8 half/double · X miss) ---");
    for (mark, needle, got, want) in &rows {
        println!("  {} {:<28} {:>6.1} -> {:>5.0}", mark, needle, got, want);
    }
    println!(
        "\nwithin 2 BPM {}/{} ({}%)  modulo octave {} ({}%)  [o{} 8{} X{}]",
        ok,
        total,
        100 * ok / total,
        modulo_octave,
        100 * modulo_octave / total,
        ok,
        octave,
        miss,
    );

    assert!(
        ok >= MIN_OK,
        "BPM within-2 regressed: {ok} < {MIN_OK} (floor); see KEY_CHECK.md"
    );
    assert!(
        modulo_octave >= MIN_MODULO_OCTAVE,
        "BPM modulo-octave regressed: {modulo_octave} < {MIN_MODULO_OCTAVE} (floor)"
    );
}

/// Diagnostic (not a pass/fail gate): print the detected bar phase for a spread of
/// real tracks, so a regression that collapses `detect_phase` to a constant is
/// visible by eye. Real downbeat *accuracy* needs rekordbox anchor ground truth
/// (not in KEY_CHECK.md) — this only sanity-checks that the cue responds to audio.
#[test]
#[ignore = "decodes sample tracks; run with --ignored --nocapture"]
fn downbeat_phase_probe() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/seeker-sample");
    if !dir.is_dir() {
        eprintln!("skip: no sample dir");
        return;
    }
    let mut counts = [0usize; 4];
    println!("\n--- detected bar phase (0 = first detected beat is the downbeat) ---");
    for &(needle, _) in GROUND_TRUTH {
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().contains(needle)));
        let Some(path) = path else { continue };
        let Ok(audio) = decode_mono_capped(&path, Some(150 * 48_000)) else { continue };
        let spec = dsp::spectrogram(&audio.samples, audio.sample_rate);
        let t = tempo::detect(&spec);
        if t.bpm <= 0.0 {
            continue;
        }
        let phase = downbeat::detect_phase(&spec, t.bpm, t.beat_offset_ms);
        counts[phase as usize] += 1;
        println!("  phase {} {:<28} ({:.0} bpm)", phase, needle, t.bpm);
    }
    println!("\nphase distribution: {counts:?}  (a healthy detector is not all one bucket)");
}

/// End-to-end wiring check: `analyze_file` (decode → tempo → grid) emits a BPM and a
/// populated, evenly-spaced static grid that spans the track. Guards the v16 wiring
/// against the v8 regression where the grid was left anchorless.
#[test]
#[ignore = "decodes a sample track; run with --ignored --nocapture"]
fn analyze_file_emits_static_grid() {
    use ordnung_core::analysis::{analyze_file, AnalysisParams};

    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/seeker-sample");
    if !dir.is_dir() {
        eprintln!("skip: no sample dir");
        return;
    }
    // A steady 4/4 track with a confident lock (rekordbox 130, detected 130).
    let path = std::fs::read_dir(&dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().contains("02. Lava")))
        .expect("Lava sample present");

    let a = analyze_file(&path, AnalysisParams::default()).expect("analyze");
    let bpm = a.bpm.expect("bpm emitted");
    assert!((bpm - 130.0).abs() < 2.0, "bpm {bpm}");
    assert!(a.beatgrid.beats.len() > 100, "grid populated: {}", a.beatgrid.beats.len());

    // Consecutive beats are one tempo period apart, all tagged the global BPM.
    let period_ms = 60_000.0 / bpm as f64;
    for w in a.beatgrid.beats.windows(2) {
        let gap = w[1].position_ms as f64 - w[0].position_ms as f64;
        assert!((gap - period_ms).abs() <= 1.0, "gap {gap} vs {period_ms}");
        assert!((w[0].bpm - bpm).abs() < 1e-6);
    }
    // Bar numbers cycle 1..=4.
    assert!(a.beatgrid.beats.iter().all(|b| (1..=4).contains(&b.number)));
    println!(
        "analyze_file: bpm {:.1}, {} beats, last @ {} ms",
        bpm,
        a.beatgrid.beats.len(),
        a.beatgrid.beats.last().unwrap().position_ms
    );
}
