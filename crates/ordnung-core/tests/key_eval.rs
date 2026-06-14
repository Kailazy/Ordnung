//! Key-detection accuracy regression test against rekordbox ground truth.
//!
//! Runs the *production* `key::detect` over the 79-track labelled sample set and
//! grades each call against the rekordbox Camelot the user transcribed (KEY_CHECK.md).
//! Asserts the exact/compatible rates stay at or above the calibrated baseline, so a
//! future chroma/profile change can't silently regress key accuracy.
//!
//! Run: cargo test -p ordnung-core --test key_eval --release -- --ignored --nocapture

use ordnung_core::analysis::{decode_mono_capped, dsp, key};
use ordnung_core::model::key::{Camelot, Mode};
use std::path::PathBuf;

/// (filename substring, rekordbox Camelot). Transcribed from KEY_CHECK.md.
const GROUND_TRUTH: &[(&str, &str)] = &[
    ("(38) [Toasty]", "11B"),
    ("00 - Basso Ostinato", "10A"),
    ("Lime In Da Coconut", "1A"),
    ("303 Views", "12A"),
    ("01 - B-PAX", "11A"),
    ("Birmingham Screwdriver", "1A"),
    ("Cascade Effect", "8A"),
    ("01 - Dimensional", "10A"),
    ("01 - Elevation", "9A"),
    ("Enteroctopus Dofleini", "6A"),
    ("01 - I Love Ya", "1A"),
    ("Midtown 120 Intro", "4A"),
    ("01 - Sentient", "11A"),
    ("Phylyps Trak", "6B"),
    ("Space Jelly", "3A"),
    ("02 - Ifeksa", "6A"),
    ("Midtown 120 Blues", "2A"),
    ("Sistol - Keno", "1B"),
    ("Ben Nevile", "3A"),
    ("cell_out-transcendance", "5A"),
    ("klint-horus", "6A"),
    ("whip_it_good", "2A"),
    ("02. Lava", "6A"),
    ("Mine Has A Shower", "6B"),
    ("basic_math_three", "2A"),
    ("04 - Reptilian", "4A"),
    ("04 Collider", "1A"),
    ("bidoben-unfair", "1A"),
    ("rene_wise-cutting_thick", "11A"),
    ("tekra-ybbob", "8A"),
    ("Kronberg 4", "9A"),
    ("domina (maurizio", "1A"),
    ("regis-point_of_entry", "10A"),
    ("Baby Ford - Monolense", "9A"),
    ("Dorisburg - Gripen", "6A"),
    ("models of wellbeing", "8A"),
    ("06 Lovesick", "8A"),
    ("cabanne-fraisheur", "1A"),
    ("Exit Strategy", "1A"),
    ("07 All That Nothing", "1A"),
    ("graviton", "7A"),
    ("metapattern-pseudo_user", "1A"),
    ("Fresh (Sprinkles Alt", "8A"),
    ("The Etheric Body", "4A"),
    ("Circul Globus", "10A"),
    ("1.02. Oriel", "10B"),
    ("dinky-twelve_to_four", "8A"),
    ("marcel_dettman-scourer", "11A"),
    ("scb-down_moment", "11A"),
    ("wenn_meine_mutti", "11A"),
    ("Deuce-Cue Ed", "10A"),
    ("ABRAX - OCB", "1A"),
    ("trance me up", "1A"),
    ("Made Your Point", "3A"),
    ("Neoclassicdub", "5B"),
    ("Basso Ostinato.mp3", "10A"),
    ("There's Galaxies Better", "10A"),
    ("Dub 22", "10A"),
    ("Double Lardon", "6A"),
    ("Ouane Forzeshow", "11A"),
    ("New Atlantis", "3A"),
    ("Transform Into Glass", "4A"),
    ("Cut 06", "6A"),
    ("EARTH JUMP", "8B"),
    ("Sand Blind", "1A"),
    ("J1. GS", "4B"),
    ("Detune (AWSI", "1A"),
    ("Mullet is in da house", "5A"),
    ("Scania", "11A"),
    ("RB208", "6A"),
    ("Klong", "1A"),
    ("Undertow", "3A"),
    ("Rezzett-Doyce", "1A"),
    ("When Will We Leave", "7A"),
    ("Ultimo Sentenza", "2A"),
    ("Not Your Business", "5A"),
    ("Young Seth - Moment", "1A"),
    ("gadget o'flow", "7A"),
    ("tadeo - requiem", "1A"),
];

fn parse_camelot(s: &str) -> Camelot {
    let major = s.ends_with('B');
    let number = s[..s.len() - 1].parse::<u8>().unwrap();
    Camelot { number, major }
}

#[derive(PartialEq, Clone, Copy)]
enum Grade {
    Exact,
    Rel, // relative major/minor (same Camelot number)
    Adj, // adjacent number, same side
    X,
}

fn grade(got: Camelot, want: Camelot) -> Grade {
    if got.number == want.number && got.major == want.major {
        Grade::Exact
    } else if got.number == want.number {
        Grade::Rel
    } else if got.major == want.major {
        let d = (got.number as i8 - want.number as i8).rem_euclid(12);
        if d == 1 || d == 11 {
            Grade::Adj
        } else {
            Grade::X
        }
    } else {
        Grade::X
    }
}

/// Calibrated floors on analyzer v9 (see KEY_CHECK.md). Assert >= so accuracy can
/// improve freely but a regression below the established baseline fails the build.
const MIN_EXACT: usize = 27;
const MIN_COMPATIBLE: usize = 40;

#[test]
#[ignore = "decodes the sample set; run with --ignored --nocapture"]
fn key_accuracy_regression() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../testdata/seeker-sample");
    if !dir.is_dir() {
        eprintln!("skip: no sample dir");
        return;
    }

    let (mut exact, mut rel, mut adj, mut x, mut none, mut minor, mut total) =
        (0, 0, 0, 0, 0, 0, 0);
    let mut rows: Vec<(char, &str, String, &str)> = Vec::new();

    for &(needle, gt) in GROUND_TRUTH {
        let path = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .find(|p| p.file_name().map_or(false, |n| n.to_string_lossy().contains(needle)));
        let Some(path) = path else {
            panic!("no file matches ground-truth needle {needle:?}");
        };
        let Ok(audio) = decode_mono_capped(&path, Some(150 * 48_000)) else {
            eprintln!("decode failed: {needle:?}");
            continue;
        };
        let spec = dsp::spectrogram(&audio.samples, audio.sample_rate);
        total += 1;
        let want = parse_camelot(gt);
        let (label, g) = match key::detect(&spec) {
            Some(k) => {
                if k.mode == Mode::Minor {
                    minor += 1;
                }
                (k.camelot().label(), grade(k.camelot(), want))
            }
            None => {
                none += 1;
                ("-".to_string(), Grade::X)
            }
        };
        match g {
            Grade::Exact => exact += 1,
            Grade::Rel => rel += 1,
            Grade::Adj => adj += 1,
            Grade::X => x += 1,
        }
        let mark = match g {
            Grade::Exact => 'E',
            Grade::Rel => 'r',
            Grade::Adj => 'a',
            Grade::X => 'X',
        };
        rows.push((mark, needle, label, gt));
    }

    let compat = exact + rel + adj;
    println!("\n--- per-track (E exact · r relative · a adjacent · X miss) ---");
    for (mark, needle, got, gt) in &rows {
        println!("  {} {:<28} {:>4} -> {:>3}", mark, needle, got, gt);
    }
    println!(
        "\nexact {}/{} ({}%)  compatible {} ({}%)  [E{} r{} a{} X{} none{}]  minor {}/{}",
        exact, total, 100 * exact / total, compat, 100 * compat / total,
        exact, rel, adj, x, none, minor, total,
    );

    assert!(
        exact >= MIN_EXACT,
        "key exact regressed: {exact} < {MIN_EXACT} (floor); see KEY_CHECK.md"
    );
    assert!(
        compat >= MIN_COMPATIBLE,
        "key compatibility regressed: {compat} < {MIN_COMPATIBLE} (floor)"
    );
}
