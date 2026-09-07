//! CI builder for the prebuilt genre database (see `.github/workflows/genredb.yml`):
//! streams the latest Discogs monthly dump through `genredb::import_latest` and
//! writes the finished SQLite table to the given path. The workflow gzips and
//! publishes it as a release asset so the app can download ~0.5 GB instead of
//! the ~10 GB dump. Run by hand with:
//!
//!     cargo run --release -p ordnung-core --example build_genredb -- out.db [stamp url]
//!
//! With `stamp` (YYYYMMDD) and `url` given, discovery is skipped — CI resolves
//! the dump in shell, where a failed probe leaves readable diagnostics.

use std::path::Path;
use std::sync::atomic::AtomicBool;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out = args
        .get(1)
        .expect("usage: build_genredb <out.db> [stamp url]");
    let cancel = AtomicBool::new(false);
    let mut last_mb = 0;
    let mut progress = |p: ordnung_core::genredb::ImportProgress| {
        // One log line per ~250 MB keeps CI logs readable.
        let mb = p.read_bytes / (1024 * 1024);
        if mb >= last_mb + 250 {
            last_mb = mb;
            eprintln!(
                "{mb} / {} MB downloaded, {} releases seen, {} kept",
                p.total_bytes / (1024 * 1024),
                p.seen,
                p.kept
            );
        }
    };
    let result = match (args.get(2), args.get(3)) {
        (Some(stamp), Some(url)) => ordnung_core::genredb::import_from(
            Path::new(out),
            stamp,
            url,
            &mut progress,
            &cancel,
        ),
        _ => ordnung_core::genredb::import_latest(Path::new(out), &mut progress, &cancel),
    };
    let stats = result.unwrap_or_else(|e| {
        eprintln!("import failed: {e}");
        std::process::exit(1);
    });
    println!(
        "dump={} seen={} kept={} completed={}",
        stats.dump, stats.seen, stats.kept, stats.completed
    );
}
