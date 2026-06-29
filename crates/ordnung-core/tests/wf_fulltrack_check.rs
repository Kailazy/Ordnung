use ordnung_core::analysis::{analyze_file, AnalysisParams};

#[test]
#[ignore]
fn waveform_spans_full_track() {
    // Confirm color bands now scale with duration (full track), not the 150s key
    // window. 20 bins/sec, so anything past a couple minutes far exceeds the old
    // fixed counts.
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/seeker-sample/01 - Midtown 120 Intro.aiff");
    if !root.exists() { eprintln!("sample missing at {:?}, skipping", root); return; }
    let a = analyze_file(&root, AnalysisParams::default()).unwrap();
    let quads = a.waveform_bands.len() / 4;
    let dur_s = quads as f32 / 20.0;
    eprintln!("waveform_bands quads={} (~{:.0}s of audio at 20 bins/s)", quads, dur_s);
    assert_eq!(a.waveform_bands.len() % 4, 0);
    assert!(quads >= 400);
}
