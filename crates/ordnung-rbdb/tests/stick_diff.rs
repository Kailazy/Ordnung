//! The whole-stick differ the rb_suite runs: a stick diffed against itself
//! is clean, and a real change surfaces where it happened and nowhere else.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use ordnung_core::model::{Analysis, AudioProperties, Beat, Beatgrid, Cue, Format, Playlist, Tags, Track};
use ordnung_rbdb::edit;
use ordnung_rbdb::export::{export_usb, ExportMode};
use ordnung_rbdb::golden::stick::{coverage, diff_stick};
use ordnung_rbdb::pdb;

fn temp_root(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("ordnung-stick-diff-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn track(id: u64, dir: &Path, name: &str) -> Track {
    let path = dir.join(name);
    std::fs::write(&path, vec![0xA5u8; 8192]).unwrap();
    Track {
        id,
        source_path: path.to_string_lossy().into_owned(),
        format: Format::Aiff,
        properties: Some(AudioProperties {
            sample_rate_hz: 44_100,
            bit_depth: Some(16),
            channels: 2,
            duration_ms: 120_000,
            bitrate_kbps: Some(1_411),
        }),
        tags: Tags {
            title: Some(format!("Title {id}")),
            artist: Some("Artist".into()),
            genre: Some("Techno".into()),
            rating: Some(3),
            ..Default::default()
        },
        analysis: Some(Analysis {
            bpm: Some(128.0),
            beatgrid: Beatgrid {
                beats: vec![Beat { number: 1, position_ms: 100, bpm: 128.0 }],
            },
            waveform_preview: vec![100; 400],
            ..Default::default()
        }),
        cues: vec![Cue {
            hot_slot: Some(0),
            position_ms: 1_000,
            loop_end_ms: None,
            label: Some("Drop".into()),
            color: Some([0xe6, 0x28, 0x28]),
        }],
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap().flatten() {
        let dst = to.join(e.file_name());
        if e.path().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

#[test]
fn a_stick_diffed_against_itself_is_clean_and_an_edit_shows_only_where_it_landed() {
    let src = temp_root("src");
    let usb = temp_root("usb");
    let tracks: Vec<Track> = (1..=3).map(|i| track(i, &src, &format!("t{i}.aiff"))).collect();
    let playlists = vec![Playlist {
        id: 10,
        name: "Set".into(),
        parent: None,
        is_folder: false,
        track_ids: vec![3, 1, 2],
        icon: None,
        color: None,
    }];
    export_usb(&usb, &tracks, &playlists, ExportMode::Replace, &mut |_| {}, &AtomicBool::new(false)).expect("export");

    let same = diff_stick(&usb, &usb).expect("diff");
    assert_eq!(same.matched, 3);
    assert!(same.findings.is_empty(), "{}", same.render(0));

    let cov: std::collections::HashMap<_, _> = coverage(&usb).unwrap().into_iter().collect();
    assert_eq!(cov["tracks"], 3);
    assert_eq!(cov["hot cues"], 3);
    assert_eq!(cov["cue comments"], 3);
    assert_eq!(cov["with rating"], 3);
    assert_eq!(cov["playlist entries"], 3);

    // Move one track's hot cue in place on a copy: the diff names that
    // track's cue lists and its cue counter, and nothing else.
    let edited = temp_root("edited");
    copy_tree(&usb.join("PIONEER"), &edited.join("PIONEER"));
    let ex = pdb::read_export(&edited.join("PIONEER/rekordbox/export.pdb")).unwrap();
    let (id, t) = ex.tracks.iter().find(|(_, t)| t.file_path.ends_with("t2.aiff")).unwrap();
    let dat = edited.join(t.analyze_path.as_deref().unwrap().trim_start_matches('/'));
    let moved = vec![Cue { position_ms: 2_000, ..tracks[1].cues[0].clone() }];
    edit::write_stick_cues(&edited, *id, &dat, &moved).unwrap();

    let d = diff_stick(&usb, &edited).expect("diff");
    let keys: std::collections::BTreeSet<String> = d.findings.iter().map(|f| f.key()).collect();
    assert!(d.findings.iter().all(|f| f.track.as_deref().is_none_or(|t| t == "t2.aiff")), "{}", d.render(0));
    assert!(keys.contains("anlz DAT.PCOB.cues"), "{keys:?}");
    assert!(keys.contains("anlz EXT.PCO2.cues"), "{keys:?}");
    assert!(keys.contains("dlp.content cueUpdateCount"), "{keys:?}");
    let unexpected: Vec<&String> = keys
        .iter()
        .filter(|k| !k.contains("PCOB") && !k.contains("PCO2") && !k.contains("cue_update_count") && !k.contains("cueUpdateCount") && !k.starts_with("pdb tracks.page"))
        .collect();
    assert!(unexpected.is_empty(), "{unexpected:?}\n{}", d.render(0));

    for dir in [&src, &usb, &edited] {
        let _ = std::fs::remove_dir_all(dir);
    }
}
