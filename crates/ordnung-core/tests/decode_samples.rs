//! Smoke test: decode every file in the local sample dir, if present.
//! Run with: cargo test -p ordnung-core --test decode_samples -- --nocapture

use ordnung_core::analysis::decode_mono;
use std::path::PathBuf;

fn sample_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../testdata/seeker-sample")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("nonexistent"))
}

#[test]
#[ignore = "slow: decodes all sample files; run explicitly with --ignored"]
fn decode_all_samples() {
    let dir = sample_dir();
    if !dir.is_dir() {
        eprintln!("skip: {} not present", dir.display());
        return;
    }
    let mut count = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        match decode_mono(&path) {
            Ok(a) => {
                let secs = a.samples.len() as f64 / a.sample_rate as f64;
                println!(
                    "OK   {:>9} samples @ {} Hz ({:>6.1}s)  {}",
                    a.samples.len(),
                    a.sample_rate,
                    secs,
                    path.file_name().unwrap().to_string_lossy()
                );
                assert!(!a.samples.is_empty());
                count += 1;
            }
            Err(e) => {
                println!("FAIL {}  ({e})", path.file_name().unwrap().to_string_lossy());
                panic!("decode failed for {}", path.display());
            }
        }
    }
    assert!(count > 0, "expected to decode at least one sample");
}
