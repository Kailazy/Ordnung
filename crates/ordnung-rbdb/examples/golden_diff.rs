//! Diff an Ordnung-written `export.pdb` against a rekordbox-written one for
//! the same tracks, and report which differences are expected.
//!
//! ```text
//! cargo run -p ordnung-rbdb --example golden_diff -- GOLDEN.pdb OURS.pdb
//! ```
fn main() {
    let mut args = std::env::args().skip(1);
    let (Some(golden), Some(ours)) = (args.next(), args.next()) else {
        eprintln!("usage: golden_diff GOLDEN.pdb OURS.pdb");
        std::process::exit(2);
    };
    let g = std::fs::read(&golden).expect("read golden");
    let o = std::fs::read(&ours).expect("read ours");
    match ordnung_rbdb::golden::diff_pdb(&g, &o) {
        Ok(d) => {
            print!("{}", d.render());
            if !d.unexplained().is_empty() {
                std::process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::exit(2);
        }
    }
}
