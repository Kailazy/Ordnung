//! Diagnostic: print the chromagram + detected key for each sample, so we can see
//! whether the chroma has a clear tonal peak or is too flat to resolve the tonic.
//! Run: cargo test -p ordnung-core --test chroma_debug --release -- --nocapture

use ordnung_core::analysis::{decode_mono_capped, dsp, key};
use std::path::PathBuf;

const PC: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

#[test]
#[ignore = "diagnostic: decodes all samples and prints chroma; run with --ignored --nocapture"]
fn print_chroma() {
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

    for path in entries {
        let audio = match decode_mono_capped(&path, Some(150 * 48_000)) {
            Ok(a) => a,
            Err(_) => continue,
        };
        let spec = dsp::spectrogram(&audio.samples, audio.sample_rate);
        let chroma = key::chromagram(&spec);
        let detected = key::detect(&spec);

        // Peakedness: ratio of top pitch class to mean (1.0 = flat).
        let max = chroma.iter().cloned().fold(0.0f32, f32::max);
        let mean = chroma.iter().sum::<f32>() / 12.0;
        let peakedness = if mean > 0.0 { max / mean } else { 0.0 };
        let top = (0..12)
            .max_by(|&a, &b| chroma[a].partial_cmp(&chroma[b]).unwrap())
            .unwrap();

        let name = path.file_name().unwrap().to_string_lossy();
        let bars: String = chroma
            .iter()
            .enumerate()
            .map(|(i, &c)| format!("{}:{:.2}", PC[i], c / max.max(1e-9)))
            .collect::<Vec<_>>()
            .join(" ");
        println!(
            "peak={:.2} top={} key={} | {}\n   {}",
            peakedness,
            PC[top],
            detected.map(|k| k.camelot().label()).unwrap_or("-".into()),
            name.chars().take(40).collect::<String>(),
            bars
        );
    }
}
