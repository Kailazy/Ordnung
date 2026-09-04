//! Temporary diagnostic: run a miniature export_usb into the given directory
//! and report how the databases came out. Usage: dlprepro <dest-dir>.

use std::sync::atomic::AtomicBool;

fn main() {
    let dest = std::path::PathBuf::from(
        std::env::args().nth(1).expect("usage: dlprepro <dest-dir>"),
    );
    std::fs::create_dir_all(&dest).unwrap();

    let srcdir = std::env::temp_dir().join(format!("dlprepro-src-{}", std::process::id()));
    std::fs::create_dir_all(&srcdir).unwrap();
    let mut tracks = Vec::new();
    for name in ["one", "two"] {
        let p = srcdir.join(format!("{name}.mp3"));
        std::fs::write(&p, b"not really audio").unwrap();
        let mut tags = ordnung_core::model::Tags::default();
        tags.title = Some(name.to_string());
        tags.artist = Some("An Artist".into());
        tags.album = Some("An Album".into());
        tags.genre = Some("House".into());
        tracks.push(ordnung_core::model::Track {
            id: if name == "one" { 1 } else { 2 },
            source_path: p.to_string_lossy().into_owned(),
            format: ordnung_core::model::Format::Mp3,
            properties: None,
            tags,
            analysis: None,
        });
    }
    let playlists = vec![ordnung_core::model::Playlist {
        id: 1,
        name: "repro".into(),
        parent: None,
        is_folder: false,
        track_ids: vec![1, 2],
    }];

    let cancel = AtomicBool::new(false);
    let mut progress = |_p: ordnung_rbdb::export::ExportProgress| {};
    match ordnung_rbdb::export::export_usb(
        &dest,
        &tracks,
        &playlists,
        ordnung_rbdb::export::ExportMode::Replace,
        &mut progress,
        &cancel,
    ) {
        Ok(r) => println!(
            "export OK: {} tracks, {} playlists",
            r.tracks_exported, r.playlists_exported
        ),
        Err(e) => println!("export FAILED: {e}"),
    }
}
