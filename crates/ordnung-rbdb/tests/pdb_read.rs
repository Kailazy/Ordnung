//! Golden-reference tests for the `export.pdb` reader.
//!
//! Fixtures are real rekordbox-produced exports taken from rekordcrate's test
//! data (<https://github.com/Holzhaus/rekordcrate>, MPL-2.0):
//! * `demo_tracks_export.pdb` — a tiny fresh export (2 tracks, no playlists).
//! * `num_rows_export.pdb` — a large real-world library (3886 tracks, 104
//!   playlist-tree nodes, 6637 playlist entries).
//!
//! The expected values below were produced by parsing the same files with
//! rekordcrate itself, so this asserts our hand-rolled reader agrees with the
//! reference implementation.

use ordnung_rbdb::pdb::read_export;
use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

#[test]
fn demo_export_tracks_no_playlists() {
    let ex = read_export(&fixture("demo_tracks_export.pdb")).expect("parse");
    assert!(ex.playlists.is_empty());
    assert!(ex.entries.is_empty());
    assert_eq!(ex.track_paths.len(), 2);
    assert_eq!(
        ex.track_paths.get(&1).map(String::as_str),
        Some("/Contents/Loopmasters/UnknownAlbum/Demo Track 1.mp3")
    );
    assert_eq!(
        ex.track_paths.get(&2).map(String::as_str),
        Some("/Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3")
    );
}

#[test]
fn large_export_matches_rekordcrate() {
    let ex = read_export(&fixture("num_rows_export.pdb")).expect("parse");

    // Aggregate shape.
    assert_eq!(ex.playlists.len(), 104);
    assert_eq!(ex.playlists.iter().filter(|p| p.is_folder).count(), 10);
    assert_eq!(ex.track_paths.len(), 3886);
    let total_entries: usize = ex.entries.values().map(Vec::len).sum();
    assert_eq!(total_entries, 6637);

    // A folder node parsed with its tree links intact.
    let folder = ex.playlists.iter().find(|p| p.id == 56).expect("node 56");
    assert!(folder.is_folder);
    assert_eq!(folder.name, "HOUSE PLAYLISTS");
    assert_eq!(folder.parent_id, 71);
    assert_eq!(folder.sort_order, 16);

    // A playlist leaf.
    let leaf = ex.playlists.iter().find(|p| p.id == 48).expect("node 48");
    assert!(!leaf.is_folder);
    assert_eq!(leaf.name, "2 - START BEATs 1");
    assert_eq!(leaf.parent_id, 5);

    // Playlist 11: 65 entries, ordered by entry index.
    let pl11 = ex.entries.get(&11).expect("playlist 11");
    assert_eq!(pl11.len(), 65);
    assert_eq!(&pl11[..3], &[74, 79, 80]);

    // Track path lookup, including a long UTF-16 path with non-ASCII chars.
    assert_eq!(
        ex.track_paths.get(&250).map(String::as_str),
        Some("/Contents/Jonas Kopp/EVD043+044 - Fear Factory Failing/EVD043-13_Jonas_Kopp-Reporter_From_the_Futur.mp3")
    );
    assert!(ex
        .track_paths
        .values()
        .any(|p| p == "/Contents/DJ Plant Texture/1\u{d8}PILLS003 MASTER MP3s/1\u{d8}PILLS003_mastered_04.mp3"));
}

#[test]
fn garbage_input_is_rejected_not_panicking() {
    // Truncated / corrupt data must error out or yield partial data, never panic.
    let dir = std::env::temp_dir().join("ordnung-pdb-read-test");
    std::fs::create_dir_all(&dir).unwrap();
    let junk = dir.join("junk.pdb");
    std::fs::write(&junk, [0u8; 16]).unwrap();
    assert!(read_export(&junk).is_err());
    let text = dir.join("text.pdb");
    std::fs::write(&text, b"this is not a database").unwrap();
    assert!(read_export(&text).is_err());
}
