//! Golden-fixture diff: rebuild the tracks of a real rekordbox export from
//! its own rows, export them through Ordnung, and diff the two `export.pdb`
//! files field by field. Every difference must be one the differ can explain
//! (ids, dates, per-export interning, fixture audio) — anything else is a
//! writer divergence from rekordbox and fails the test.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;

use ordnung_core::model::key::Key;
use ordnung_core::model::{Analysis, AudioProperties, Beat, Beatgrid, Format, Tags, Track};
use ordnung_rbdb::export::{export_usb, ExportMode};
use ordnung_rbdb::golden::diff_pdb;
use ordnung_rbdb::pdb::{read_export, RbTrack};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// A catalog track carrying exactly what the golden row says about it, with
/// a dummy audio file under the golden filename.
fn track_from_row(id: u64, dir: &std::path::Path, t: &RbTrack) -> Track {
    let name = t.file_path.rsplit('/').next().unwrap().to_string();
    let path = dir.join(&name);
    std::fs::write(&path, vec![0u8; 4096]).unwrap();
    let format = match name.rsplit('.').next().map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("mp3") => Format::Mp3,
        Some("flac") => Format::Flac,
        Some("wav") => Format::Wav,
        Some("aiff") | Some("aif") => Format::Aiff,
        Some("m4a") => Format::Aac,
        _ => Format::Other,
    };
    Track {
        id,
        source_path: path.to_string_lossy().into_owned(),
        format,
        properties: Some(AudioProperties {
            sample_rate_hz: t.sample_rate_hz,
            bit_depth: Some(16),
            channels: 2,
            duration_ms: t.duration_s as u64 * 1000,
            bitrate_kbps: Some(t.bitrate_kbps),
        }),
        tags: Tags {
            title: Some(t.title.clone()),
            artist: t.artist.clone(),
            album: t.album.clone(),
            genre: t.genre.clone(),
            label: t.label.clone(),
            year: (t.year != 0).then_some(t.year),
            comment: (!t.comment.is_empty()).then(|| t.comment.clone()),
            ..Default::default()
        },
        analysis: Some(Analysis {
            bpm: t.bpm(),
            key: t.key.as_deref().and_then(Key::parse),
            beatgrid: Beatgrid {
                beats: vec![Beat {
                    number: 1,
                    position_ms: 0,
                    bpm: t.bpm().unwrap_or(0.0),
                }],
            },
            waveform_preview: vec![100; 400],
            ..Default::default()
        }),
        cues: Vec::new(),
    }
}

#[test]
fn demo_export_diff_against_rekordbox_is_fully_explained() {
    let golden_path = fixture("demo_tracks_export.pdb");
    let golden = read_export(&golden_path).expect("golden parses");
    let src = std::env::temp_dir().join(format!("ordnung-golden-src-{}", std::process::id()));
    let usb = std::env::temp_dir().join(format!("ordnung-golden-usb-{}", std::process::id()));
    for d in [&src, &usb] {
        let _ = std::fs::remove_dir_all(d);
        std::fs::create_dir_all(d).unwrap();
    }
    let mut ids: Vec<&u32> = golden.tracks.keys().collect();
    ids.sort();
    let tracks: Vec<Track> = ids
        .iter()
        .map(|id| track_from_row(**id as u64, &src, &golden.tracks[id]))
        .collect();

    let cancel = AtomicBool::new(false);
    export_usb(&usb, &tracks, &[], ExportMode::Replace, &mut |_| {}, &cancel).expect("export");

    let g = std::fs::read(&golden_path).unwrap();
    let o = std::fs::read(usb.join("PIONEER/rekordbox/export.pdb")).unwrap();
    let diff = diff_pdb(&g, &o).expect("diff");
    println!("{}", diff.render());

    // Shape: same tables in the same order, every track matched.
    assert!(diff.header.iter().all(|d| d.explained.is_some()), "{:?}", diff.header);
    assert!(diff.only_golden.is_empty() && diff.only_ours.is_empty());
    assert_eq!(diff.tracks.len(), golden.tracks.len());
    let tracks_tbl = &diff.tables[0];
    assert_eq!(tracks_tbl.golden_rows, tracks_tbl.ours_rows);

    let bad = diff.unexplained();
    assert!(bad.is_empty(), "unexplained differences:\n{}", bad.join("\n"));

    // And the differ does catch a real divergence: bump one track's tempo
    // in our file and it must surface as unexplained, by name.
    let mut broken = o.clone();
    let tracks_first_page = 1usize; // type 0's sentinel is page 1; data page follows
    let page = (tracks_first_page + 1) * 4096;
    // First row's heap offset from the footer (slot 0 of group 0).
    let rel = u16::from_le_bytes([broken[page + 4096 - 6], broken[page + 4096 - 5]]) as usize;
    let row = page + 0x28 + rel;
    let tempo = u32::from_le_bytes(broken[row + 0x38..row + 0x3C].try_into().unwrap());
    assert!(tempo == 12_800 || tempo == 12_000, "sanity: found a demo tempo, got {tempo}");
    broken[row + 0x38..row + 0x3C].copy_from_slice(&(tempo + 100).to_le_bytes());
    let diff2 = diff_pdb(&g, &broken).unwrap();
    let bad2 = diff2.unexplained();
    assert_eq!(bad2.len(), 1, "{bad2:?}");
    assert!(bad2[0].contains("tempo"), "{bad2:?}");

    let _ = std::fs::remove_dir_all(&src);
    let _ = std::fs::remove_dir_all(&usb);
}
