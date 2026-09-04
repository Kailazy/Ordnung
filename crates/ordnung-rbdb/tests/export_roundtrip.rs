//! Phase 5 round-trip: export a library onto a directory (a stand-in USB
//! root), then read the stick back through the crate's own validated readers
//! and check the whole surface — tracks, metadata, playlists in *both*
//! databases, ANLZ files — survives intact.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ordnung_core::model::{
    Analysis, AudioProperties, Beat, Beatgrid, Format, Playlist, Tags, Track,
};
use ordnung_core::model::key::{Key, Mode, PitchClass};
use ordnung_rbdb::export::{export_usb, ExportError, ExportMode};
use ordnung_rbdb::{dlp, pdb};

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ordnung-export-test-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn audio_file(dir: &Path, name: &str, bytes: usize) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, vec![0xA5u8; bytes]).unwrap();
    p
}

fn track(id: u64, path: &Path, format: Format, title: &str, artist: &str) -> Track {
    let bands = vec![90u8; 4 * 20 * 120]; // 120 s at 20 bins/s
    Track {
        id,
        source_path: path.to_string_lossy().into_owned(),
        format,
        properties: Some(AudioProperties {
            sample_rate_hz: 44_100,
            bit_depth: Some(16),
            channels: 2,
            duration_ms: 120_000,
            bitrate_kbps: Some(1_411),
        }),
        tags: Tags {
            title: Some(title.into()),
            artist: Some(artist.into()),
            album: Some("Test Album".into()),
            genre: Some("Techno".into()),
            label: Some("Test Label".into()),
            year: Some(2024),
            comment: Some("hello".into()),
            ..Default::default()
        },
        analysis: Some(Analysis {
            bpm: Some(128.0),
            key: Some(Key::new(PitchClass::new(9), Mode::Minor)), // A minor = 8A
            beatgrid: Beatgrid {
                beats: vec![Beat {
                    number: 1,
                    position_ms: 250,
                    bpm: 128.0,
                }],
            },
            waveform_preview: (0..400).map(|i| (i % 256) as u8).collect(),
            waveform_bands: bands,
            ..Default::default()
        }),
    }
}

/// Minimal big-endian ANLZ walker for verifying our own output.
fn anlz_sections(data: &[u8]) -> Vec<(String, usize, usize)> {
    assert_eq!(&data[0..4], b"PMAI");
    let file_len = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
    assert_eq!(file_len, data.len(), "PMAI len_file must match the file");
    let mut off = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
    let mut out = Vec::new();
    while off + 12 <= data.len() {
        let tag = String::from_utf8_lossy(&data[off..off + 4]).into_owned();
        let lt = u32::from_be_bytes(data[off + 8..off + 12].try_into().unwrap()) as usize;
        out.push((tag, off, lt));
        off += lt;
    }
    assert_eq!(off, data.len(), "sections must tile the file");
    out
}

#[test]
fn export_then_read_back_full_surface() {
    let src = temp_root("src");
    let usb = temp_root("usb");
    let a = audio_file(&src, "a side.aiff", 9_000);
    let b = audio_file(&src, "b_side.mp3", 7_000);
    let missing = src.join("gone.flac");

    let tracks = vec![
        track(101, &a, Format::Aiff, "Alpha", "Artist One"),
        track(102, &b, Format::Mp3, "Beta", "Artist Two"),
        track(103, &missing, Format::Flac, "Ghost", "Nobody"),
    ];
    let playlists = vec![
        Playlist {
            id: 10,
            name: "CRATES".into(),
            parent: None,
            is_folder: true,
            track_ids: vec![],
        },
        Playlist {
            id: 11,
            name: "warmup".into(),
            parent: Some(10),
            is_folder: false,
            track_ids: vec![102, 101, 103], // order matters; 103 is skipped
        },
    ];

    let cancel = AtomicBool::new(false);
    let mut stages = Vec::new();
    let report = export_usb(&usb, &tracks, &playlists, ExportMode::Replace, &mut |p| stages.push(p.stage), &cancel)
        .expect("export succeeds");
    assert_eq!(report.tracks_exported, 2);
    assert_eq!(report.playlists_exported, 2);
    assert_eq!(report.skipped.len(), 1, "missing source must be skipped");
    assert_eq!(report.skipped[0].0, 103);
    assert!(report.bytes_copied >= 16_000);

    // --- files on the stick ------------------------------------------------
    assert!(usb.join("Contents/a side.aiff").is_file());
    assert!(usb.join("Contents/b_side.mp3").is_file());
    assert!(usb.join("PIONEER/rekordbox/export.pdb").is_file());
    assert!(usb.join("PIONEER/rekordbox/exportExt.pdb").is_file());
    assert!(usb.join("PIONEER/rekordbox/exportLibrary.db").is_file());

    // --- export.pdb structural invariants ----------------------------------
    let raw = std::fs::read(usb.join("PIONEER/rekordbox/export.pdb")).unwrap();
    assert_eq!(raw.len() % 4096, 0);
    assert_eq!(u32::from_le_bytes(raw[8..12].try_into().unwrap()), 20);
    for t in 0..20usize {
        let sentinel = (2 * t + 1) * 4096;
        assert_eq!(raw[sentinel + 0x1B], 0x64, "table {t} sentinel flags");
        assert_eq!(
            u32::from_le_bytes(raw[sentinel + 8..sentinel + 12].try_into().unwrap()),
            t as u32
        );
    }

    // --- read back through the validated reader ----------------------------
    let export = pdb::read_export(&usb.join("PIONEER/rekordbox/export.pdb")).unwrap();
    assert_eq!(export.tracks.len(), 2);
    let alpha = export
        .tracks
        .values()
        .find(|t| t.title == "Alpha")
        .expect("Alpha present");
    assert_eq!(alpha.file_path, "/Contents/a side.aiff");
    assert_eq!(alpha.tempo_centi_bpm, 12_800);
    assert_eq!(alpha.key.as_deref(), Some("8A"));
    assert_eq!(alpha.artist.as_deref(), Some("Artist One"));
    assert_eq!(alpha.album.as_deref(), Some("Test Album"));
    assert_eq!(alpha.genre.as_deref(), Some("Techno"));
    assert_eq!(alpha.comment, "hello");
    assert_eq!(alpha.duration_s, 120);
    assert_eq!(alpha.bitrate_kbps, 1_411);
    assert_eq!(alpha.sample_rate_hz, 44_100);
    assert_eq!(alpha.year, 2024);

    // Playlists live in the pdb (older CDJs)…
    assert_eq!(export.playlists.len(), 2);
    let folder = export.playlists.iter().find(|p| p.name == "CRATES").unwrap();
    assert!(folder.is_folder);
    let warmup = export.playlists.iter().find(|p| p.name == "warmup").unwrap();
    assert!(!warmup.is_folder);
    assert_eq!(warmup.parent_id, folder.id);
    let entry_titles: Vec<&str> = export.entries[&warmup.id]
        .iter()
        .map(|tid| export.tracks[tid].title.as_str())
        .collect();
    assert_eq!(entry_titles, ["Beta", "Alpha"], "playlist order preserved");

    // …and in Device Library Plus (XDJ-AZ-class players).
    let dlp = dlp::read_playlists(&usb.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    assert_eq!(dlp.playlists.len(), 2);
    let warmup_dlp = dlp.playlists.iter().find(|p| p.name == "warmup").unwrap();
    assert_eq!(
        dlp.entries_by_path[&warmup_dlp.id],
        vec!["/Contents/b_side.mp3".to_string(), "/Contents/a side.aiff".into()]
    );

    // read_stick (the GUI entry point) sees the same library.
    let stick = pdb::read_stick(&usb).unwrap();
    assert_eq!(stick.tracks.len(), 2);
    assert_eq!(stick.playlists.len(), 2);

    // --- ANLZ files --------------------------------------------------------
    let anlz_path = alpha.analyze_path.as_deref().expect("analyze path set");
    let dat = std::fs::read(usb.join(&anlz_path[1..])).expect("DAT exists at analyze_path");
    let tags: Vec<String> = anlz_sections(&dat).iter().map(|(t, _, _)| t.clone()).collect();
    assert_eq!(tags, ["PPTH", "PVBR", "PQTZ", "PWAV", "PWV2", "PCOB", "PCOB"]);
    let (_, qoff, _) = anlz_sections(&dat)
        .into_iter()
        .find(|(t, _, _)| t == "PQTZ")
        .unwrap();
    let nbeats = u32::from_be_bytes(dat[qoff + 0x14..qoff + 0x18].try_into().unwrap());
    // 128 BPM anchor at 250 ms over 120 s ⇒ ~255 beats, bar numbers cycling.
    assert!((250..=257).contains(&nbeats), "got {nbeats} beats");
    let first_num = u16::from_be_bytes(dat[qoff + 0x18..qoff + 0x1A].try_into().unwrap());
    let first_bpm = u16::from_be_bytes(dat[qoff + 0x1A..qoff + 0x1C].try_into().unwrap());
    let first_ms = u32::from_be_bytes(dat[qoff + 0x1C..qoff + 0x20].try_into().unwrap());
    assert_eq!((first_num, first_bpm, first_ms), (1, 12_800, 250));

    let ext = std::fs::read(usb.join(anlz_path[1..].replace(".DAT", ".EXT"))).unwrap();
    let tags: Vec<String> = anlz_sections(&ext).iter().map(|(t, _, _)| t.clone()).collect();
    assert_eq!(
        tags,
        ["PPTH", "PWV3", "PCOB", "PCOB", "PCO2", "PCO2", "PQT2", "PWV5", "PWV4"]
    );

    // --- re-export is incremental ------------------------------------------
    let report2 = export_usb(&usb, &tracks, &playlists, ExportMode::Replace, &mut |_| {}, &cancel).unwrap();
    assert_eq!(report2.bytes_copied, 0, "unchanged audio must not re-copy");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&usb);
}

#[test]
fn merge_adds_to_an_existing_export_without_clobbering_it() {
    let src = temp_root("merge-src");
    let usb = temp_root("merge-usb");
    let a = audio_file(&src, "alpha.mp3", 5_000);
    let b = audio_file(&src, "beta.mp3", 6_000);
    let c = audio_file(&src, "gamma.mp3", 7_000);
    let cancel = AtomicBool::new(false);

    // First export: playlist "set A" with alpha + beta.
    let set_a = vec![Playlist {
        id: 1,
        name: "set A".into(),
        parent: None,
        is_folder: false,
        track_ids: vec![10, 11],
    }];
    let tracks_a = vec![
        track(10, &a, Format::Mp3, "Alpha", "AA"),
        track(11, &b, Format::Mp3, "Beta", "BB"),
    ];
    export_usb(&usb, &tracks_a, &set_a, ExportMode::Replace, &mut |_| {}, &cancel).unwrap();

    // Merge a second playlist "set B" with gamma (a new track) and alpha
    // (already on the stick — must reuse its file, not duplicate it).
    let set_b = vec![Playlist {
        id: 2,
        name: "set B".into(),
        parent: None,
        is_folder: false,
        track_ids: vec![12, 10],
    }];
    let tracks_b = vec![
        track(12, &c, Format::Mp3, "Gamma", "CC"),
        track(10, &a, Format::Mp3, "Alpha", "AA"),
    ];
    let report =
        export_usb(&usb, &tracks_b, &set_b, ExportMode::Merge, &mut |_| {}, &cancel).unwrap();
    // The stick now carries all three tracks (beta was carried over untouched).
    assert_eq!(report.tracks_exported, 3, "merge keeps the earlier tracks");

    let export = pdb::read_export(&usb.join("PIONEER/rekordbox/export.pdb")).unwrap();
    assert_eq!(export.tracks.len(), 3);
    let titles: std::collections::HashSet<&str> =
        export.tracks.values().map(|t| t.title.as_str()).collect();
    assert!(titles.contains("Alpha") && titles.contains("Beta") && titles.contains("Gamma"));

    // alpha.mp3 exists exactly once — the merge reused it rather than writing
    // an "alpha (2).mp3".
    let contents: Vec<String> = std::fs::read_dir(usb.join("Contents"))
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        contents.iter().filter(|n| n.starts_with("alpha")).count(),
        1,
        "alpha must not be duplicated on merge; got {contents:?}"
    );

    // Both playlists survive, each with its own membership.
    assert_eq!(export.playlists.len(), 2);
    let a_pl = export.playlists.iter().find(|p| p.name == "set A").unwrap();
    let b_pl = export.playlists.iter().find(|p| p.name == "set B").unwrap();
    let a_titles: Vec<&str> = export.entries[&a_pl.id]
        .iter()
        .map(|id| export.tracks[id].title.as_str())
        .collect();
    let b_titles: Vec<&str> = export.entries[&b_pl.id]
        .iter()
        .map(|id| export.tracks[id].title.as_str())
        .collect();
    assert_eq!(a_titles, ["Alpha", "Beta"], "set A membership preserved");
    assert_eq!(b_titles, ["Gamma", "Alpha"], "set B membership added");

    // DLP mirrors the merged tree too.
    let dlp = dlp::read_playlists(&usb.join("PIONEER/rekordbox/exportLibrary.db")).unwrap();
    assert_eq!(dlp.playlists.len(), 2);

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&usb);
}

#[test]
fn export_refuses_empty_and_missing_dest() {
    let usb = temp_root("empty");
    let cancel = AtomicBool::new(false);
    let err = export_usb(&usb, &[], &[], ExportMode::Replace, &mut |_| {}, &cancel).unwrap_err();
    assert!(matches!(err, ExportError::NoTracks));
    let err = export_usb(Path::new("/nonexistent-ordnung"), &[], &[], ExportMode::Replace, &mut |_| {}, &cancel)
        .unwrap_err();
    assert!(matches!(err, ExportError::BadDestination(_)));
    let _ = std::fs::remove_dir_all(&usb);
}

#[test]
fn cancel_aborts_before_completion() {
    let src = temp_root("cancel-src");
    let usb = temp_root("cancel-usb");
    let a = audio_file(&src, "x.mp3", 2_000);
    let tracks = vec![track(1, &a, Format::Mp3, "X", "Y")];
    let cancel = AtomicBool::new(true); // canceled from the start
    let err = export_usb(&usb, &tracks, &[], ExportMode::Replace, &mut |_| {}, &cancel).unwrap_err();
    assert!(matches!(err, ExportError::Canceled));
    assert!(
        !usb.join("PIONEER/rekordbox/export.pdb").exists(),
        "no database written after cancel"
    );
    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&usb);
}
