fn main() {
    let arg = std::env::args().nth(1).expect("path to export.pdb");
    match ordnung_rbdb::pdb::read_export(std::path::Path::new(&arg)) {
        Ok(e) => println!(
            "PDB OK: {} tracks, {} playlists",
            e.tracks.len(),
            e.playlists.len()
        ),
        Err(err) => println!("PDB ERROR: {err}"),
    }
}
