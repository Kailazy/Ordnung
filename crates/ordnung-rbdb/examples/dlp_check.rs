fn main() {
    let arg = std::env::args().nth(1).expect("path to exportLibrary.db");
    match ordnung_rbdb::dlp::read_playlists(std::path::Path::new(&arg)) {
        Ok(pl) => {
            let s = format!("{pl:?}");
            println!("DLP OK: {}", &s[..s.len().min(400)]);
        }
        Err(e) => println!("DLP ERROR: {e}"),
    }
}
