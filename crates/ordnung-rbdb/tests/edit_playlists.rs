//! In-place playlist editing against real rekordbox-produced exports.
//!
//! Each test copies a golden fixture into a temp volume layout, applies edits
//! through `edit_stick_playlists`, and re-reads the stick with the crate's own
//! validated readers — proving the surgical pdb rewrite round-trips and that
//! everything *outside* the playlist tables survives byte-level untouched
//! semantics (same tracks, same metadata).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ordnung_rbdb::edit::{edit_stick_playlists, PlaylistOp};
use ordnung_rbdb::pdb::{read_export, read_stick};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// Build a temp volume root carrying `fixture_name` as its export.pdb.
fn temp_volume(tag: &str, fixture_name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "ordnung-edit-test-{tag}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    let dir = root.join("PIONEER").join("rekordbox");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::copy(fixture(fixture_name), dir.join("export.pdb")).unwrap();
    root
}

fn pdb_path(root: &Path) -> PathBuf {
    root.join("PIONEER").join("rekordbox").join("export.pdb")
}

#[test]
fn create_rename_add_delete_on_playlistless_export() {
    // The demo export is exactly what rekordbox 7 leaves for a library with
    // no playlists: empty pdb playlist tables (first == last == sentinel).
    // This exercises the empty → non-empty transition on foreign bytes.
    let root = temp_volume("demo", "demo_tracks_export.pdb");
    let pristine = std::fs::read(pdb_path(&root)).unwrap();

    let out = edit_stick_playlists(
        &root,
        &PlaylistOp::Create {
            name: "warmup".into(),
            parent_id: 0,
        },
    )
    .expect("create");
    let id = out.new_id.expect("new id");

    // The pre-edit database was preserved.
    let orig = std::fs::read(pdb_path(&root).with_extension("pdb.orig")).unwrap();
    assert_eq!(orig, pristine, "first edit must back up the pristine pdb");

    let ex = read_export(&pdb_path(&root)).expect("re-parse after create");
    assert_eq!(ex.playlists.len(), 1);
    assert_eq!(ex.playlists[0].name, "warmup");
    assert!(!ex.playlists[0].is_folder);
    // An empty playlist writes no entry rows, so the raw reader reports it
    // with no entries at all (callers normalize this to an empty list).
    assert!(ex.entries.get(&id).map_or(true, Vec::is_empty));

    edit_stick_playlists(
        &root,
        &PlaylistOp::AddTracks {
            id,
            rel_paths: vec![
                "Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3".into(),
                // Case-insensitive: FAT32 paths must still resolve.
                "contents/loopmasters/unknownalbum/demo track 1.MP3".into(),
                // Unknown paths are skipped, not fatal.
                "Contents/Nope/missing.mp3".into(),
                // Duplicates are skipped.
                "Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3".into(),
            ],
        },
    )
    .expect("add tracks");
    edit_stick_playlists(
        &root,
        &PlaylistOp::Rename {
            id,
            name: "peak time".into(),
        },
    )
    .expect("rename");

    let ex = read_export(&pdb_path(&root)).expect("re-parse after edits");
    assert_eq!(ex.playlists[0].name, "peak time");
    assert_eq!(ex.entries[&id], vec![2, 1], "order and dedupe preserved");

    // Everything outside the playlist tables is intact: same tracks, same
    // stamped analysis, same ANLZ paths.
    assert_eq!(ex.tracks.len(), 2);
    assert_eq!(ex.tracks[&1].bpm(), Some(128.0));
    assert_eq!(ex.tracks[&2].bpm(), Some(120.0));
    for t in ex.tracks.values() {
        assert_eq!(t.key.as_deref(), Some("Fm"));
        assert!(t.analyze_path.as_deref().unwrap().starts_with("/PIONEER/"));
    }

    edit_stick_playlists(&root, &PlaylistOp::Delete { id }).expect("delete");
    let ex = read_export(&pdb_path(&root)).expect("re-parse after delete");
    assert!(ex.playlists.is_empty());
    assert!(ex.entries.is_empty());
    assert_eq!(ex.tracks.len(), 2);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn edits_on_large_multi_page_export() {
    // 104 playlist nodes / 6637 entries span multiple pages in both tables,
    // so this exercises chain reuse, blanking on shrink, and appending fresh
    // pages past the end of a real rekordbox file.
    let root = temp_volume("large", "num_rows_export.pdb");
    let before = read_export(&pdb_path(&root)).expect("parse fixture");
    let track74_path = before.tracks[&74].file_path.clone();

    // Append a track to playlist 11 (65 entries) by its stored path; one
    // duplicate of the playlist's own first entry must be skipped. The new
    // track is picked deterministically from outside the playlist.
    let pl11_before: std::collections::HashSet<u32> =
        before.entries[&11].iter().copied().collect();
    let new_tid = before
        .tracks
        .keys()
        .copied()
        .filter(|t| !pl11_before.contains(t))
        .min()
        .expect("a track outside playlist 11");
    let new_track_path = before.tracks[&new_tid].file_path.clone();
    edit_stick_playlists(
        &root,
        &PlaylistOp::AddTracks {
            id: 11,
            rel_paths: vec![
                new_track_path.trim_start_matches('/').to_string(),
                track74_path.trim_start_matches('/').to_string(), // already first
            ],
        },
    )
    .expect("add to large playlist");

    let ex = read_export(&pdb_path(&root)).expect("re-parse after add");
    let pl11 = &ex.entries[&11];
    assert_eq!(pl11.len(), 66);
    assert_eq!(&pl11[..3], &[74, 79, 80], "existing order untouched");
    assert_eq!(*pl11.last().unwrap(), new_tid);
    // Every other playlist is byte-for-byte the same list it was.
    for (pid, tracks) in &before.entries {
        if *pid != 11 {
            assert_eq!(ex.entries.get(pid), Some(tracks), "playlist {pid}");
        }
    }
    assert_eq!(ex.tracks.len(), 3886);

    // Delete the "HOUSE PLAYLISTS" folder (id 56): its whole subtree and all
    // of the subtree's entries must go; unrelated nodes stay.
    let mut doomed: Vec<u32> = vec![56];
    let mut i = 0;
    while i < doomed.len() {
        let parent = doomed[i];
        doomed.extend(
            before
                .playlists
                .iter()
                .filter(|p| p.parent_id == parent)
                .map(|p| p.id),
        );
        i += 1;
    }
    edit_stick_playlists(&root, &PlaylistOp::Delete { id: 56 }).expect("delete folder");

    let ex = read_export(&pdb_path(&root)).expect("re-parse after folder delete");
    assert_eq!(ex.playlists.len(), 104 - doomed.len());
    for d in &doomed {
        assert!(!ex.playlists.iter().any(|p| p.id == *d));
        assert!(!ex.entries.contains_key(d) || ex.entries[d].is_empty());
    }
    // Survivors keep their exact membership.
    let leaf = ex.playlists.iter().find(|p| p.id == 48).expect("node 48");
    assert_eq!(leaf.name, "2 - START BEATs 1");
    assert_eq!(ex.entries[&11].len(), 66);
    assert_eq!(ex.tracks.len(), 3886);

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn dlp_database_mirrors_every_edit() {
    // A stick with a Device Library Plus database beside the pdb: edits must
    // land in both stores, joined by file path onto the DLP's own content ids.
    let root = temp_volume("dlp", "demo_tracks_export.pdb");
    let db = root
        .join("PIONEER")
        .join("rekordbox")
        .join("exportLibrary.db");
    {
        let conn = rusqlite::Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA key = 'r8gddnr4k847830ar6cqzbkk0el6qytmb3trbbx805jm74vez64i5o8fnrqryqls';
             PRAGMA cipher_compatibility = 4;
             CREATE TABLE playlist(playlist_id integer primary key, sequenceNo integer, \
                 name varchar, image_id integer, attribute integer, playlist_id_parent integer);
             CREATE TABLE playlist_content(playlist_id integer, content_id integer, \
                 sequenceNo integer);
             CREATE TABLE content(content_id integer primary key, path varchar);
             -- DLP content ids deliberately differ from the pdb's track ids.
             INSERT INTO content VALUES \
                 (901, '/Contents/Loopmasters/UnknownAlbum/Demo Track 1.mp3'), \
                 (902, '/Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3');",
        )
        .unwrap();
    }

    let out = edit_stick_playlists(
        &root,
        &PlaylistOp::Create {
            name: "both stores".into(),
            parent_id: 0,
        },
    )
    .expect("create");
    let id = out.new_id.unwrap();
    edit_stick_playlists(
        &root,
        &PlaylistOp::AddTracks {
            id,
            rel_paths: vec!["Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3".into()],
        },
    )
    .expect("add");

    // The DLP side reads back through the production reader, mapped to paths.
    let dlp = ordnung_rbdb::dlp::read_playlists(&db).expect("dlp read");
    assert_eq!(dlp.playlists.len(), 1);
    assert_eq!(dlp.playlists[0].name, "both stores");
    assert_eq!(
        dlp.entries_by_path[&id],
        vec!["/Contents/Loopmasters/UnknownAlbum/Demo Track 2.mp3".to_string()]
    );

    // And the joined view (what the GUI reads) agrees with both.
    let stick = read_stick(&root).expect("read stick");
    assert_eq!(stick.playlists.len(), 1);
    assert_eq!(stick.entries[&id], vec![2]);

    let mut expected: HashMap<u32, Vec<u32>> = HashMap::new();
    expected.insert(id, vec![2]);
    assert_eq!(stick.entries, expected);

    let _ = std::fs::remove_dir_all(&root);
}
