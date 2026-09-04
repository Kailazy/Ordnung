//! USB export orchestration — Phase 5's engine.
//!
//! Takes catalog tracks + playlists and assembles a native rekordbox stick
//! under a destination root:
//!
//! ```text
//! /Contents/<file>                        audio (flat pool, copied)
//! /PIONEER/rekordbox/export.pdb           DeviceSQL database   (pdbw)
//! /PIONEER/rekordbox/exportExt.pdb        My Tag skeleton      (pdbw)
//! /PIONEER/rekordbox/exportLibrary.db     Device Library Plus  (dlp)
//! /PIONEER/USBANLZ/Pnnn/<8-hex>/ANLZ0000.{DAT,EXT}             (anlz)
//! ```
//!
//! Playlists go into *both* databases: export.pdb's tree for CDJ-2000/nxs2
//! players and exportLibrary.db for OPUS-QUAD/OMNIS-DUO/XDJ-AZ-class players.
//! Engine only: no printing, no prompting — progress flows through a callback
//! and cancellation through an [`AtomicBool`], per the architecture rules.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use ordnung_core::model::{Format, Id, Playlist, Track};

use crate::anlz;
use crate::pdbw::{self, PdbTables, PlaylistRow, TrackRow};

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("{path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("export canceled")]
    Canceled,
    #[error("nothing to export (no tracks with a readable source file)")]
    NoTracks,
    #[error("destination is not a directory: {0}")]
    BadDestination(PathBuf),
    #[error("device library: {0}")]
    Dlp(String),
}

type Result<T> = std::result::Result<T, ExportError>;

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> ExportError {
    let path = path.into();
    move |source| ExportError::Io { path, source }
}

/// What the export is doing right now; `done`/`total` count within the stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStage {
    CopyingAudio,
    WritingAnalysis,
    WritingDatabase,
}

#[derive(Debug, Clone)]
pub struct ExportProgress {
    pub stage: ExportStage,
    pub done: usize,
    pub total: usize,
    /// Current file/track label, for status lines.
    pub detail: String,
}

#[derive(Debug, Clone, Default)]
pub struct ExportReport {
    pub tracks_exported: usize,
    pub playlists_exported: usize,
    /// Tracks left off the stick, with the reason (missing source, format).
    pub skipped: Vec<(Id, String)>,
    pub bytes_copied: u64,
}

/// One track fully resolved for the stick.
struct ExportTrack {
    catalog_id: Id,
    row: TrackRow,
    source: PathBuf,
    /// `/Contents/<name>` — volume-absolute, forward slashes.
    usb_path: String,
    duration_ms: u64,
    beats: Vec<ordnung_core::model::Beat>,
    preview: Vec<u8>,
    bands: Vec<u8>,
    copy_needed: bool,
}

/// rekordbox file-type enum (master.db `FileType`).
fn file_type(f: Format) -> u16 {
    match f {
        Format::Mp3 => 1,
        Format::Aac => 4,
        Format::Flac => 5,
        Format::Wav => 11,
        Format::Aiff => 12,
        Format::Other => 0,
    }
}

/// Replace FAT-hostile characters and trim what FAT dislikes at the edges.
fn sanitize_fat(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if (c as u32) < 0x20 => '_',
            c => c,
        })
        .collect();
    let trimmed = cleaned.trim().trim_end_matches('.').to_string();
    if trimmed.is_empty() {
        "track".to_string()
    } else {
        trimmed
    }
}

/// Today as `YYYY-MM-DD` (UTC) without pulling in a date crate.
fn today() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    // Howard Hinnant's civil-from-days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

/// Deterministic rekordbox-style content id: nonzero, < 2^28, stable per
/// export slot so a re-export writes identical bytes.
fn master_content_id(id: u32) -> u32 {
    ((id as u64).wrapping_mul(2_654_435_761) % 0x0FFF_FFF7 + 1) as u32
}

/// String interner preserving first-seen order with 1-based dense ids —
/// exactly how rekordbox fills its name tables.
#[derive(Default)]
struct Intern {
    ids: HashMap<String, u32>,
    rows: Vec<(u32, String)>,
}

impl Intern {
    fn get(&mut self, name: Option<&str>) -> u32 {
        let Some(name) = name.map(str::trim).filter(|s| !s.is_empty()) else {
            return 0;
        };
        if let Some(&id) = self.ids.get(name) {
            return id;
        }
        let id = self.rows.len() as u32 + 1;
        self.ids.insert(name.to_string(), id);
        self.rows.push((id, name.to_string()));
        id
    }
}

/// Export `tracks` (in the given order) and `playlists` onto `dest_root`.
///
/// The destination is a mounted FAT32 volume root or any directory (a staging
/// folder, an `ORDNUNG_FAKE_USB` root). Existing rekordbox files there are
/// overwritten; audio already present in `/Contents` with matching size is
/// not re-copied, so re-exports are incremental.
pub fn export_usb(
    dest_root: &Path,
    tracks: &[Track],
    playlists: &[Playlist],
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &AtomicBool,
) -> Result<ExportReport> {
    if !dest_root.is_dir() {
        return Err(ExportError::BadDestination(dest_root.to_path_buf()));
    }
    let check = |c: &AtomicBool| {
        if c.load(Ordering::Relaxed) {
            Err(ExportError::Canceled)
        } else {
            Ok(())
        }
    };

    let contents = dest_root.join("Contents");
    let rb_dir = dest_root.join("PIONEER").join("rekordbox");
    let anlz_root = dest_root.join("PIONEER").join("USBANLZ");
    for d in [&contents, &rb_dir, &anlz_root] {
        std::fs::create_dir_all(d).map_err(io_err(d.clone()))?;
    }

    let mut report = ExportReport::default();
    let date = today();

    // ---- resolve tracks: filenames, interned ids, metadata ---------------
    let mut artists = Intern::default();
    let mut albums = Intern::default();
    let mut genres = Intern::default();
    let mut labels = Intern::default();
    let mut keys = Intern::default();

    let mut taken: HashSet<String> = HashSet::new();
    let mut resolved: Vec<ExportTrack> = Vec::new();

    for t in tracks {
        let source = PathBuf::from(&t.source_path);
        if !source.is_file() {
            report.skipped.push((t.id, "source file missing".into()));
            continue;
        }
        if t.format == Format::Other {
            report
                .skipped
                .push((t.id, "format not CDJ-playable".into()));
            continue;
        }
        let id = resolved.len() as u32 + 1;

        // Unique FAT name (FAT is case-insensitive — dedupe accordingly).
        let base = sanitize_fat(
            source
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
                .as_str(),
        );
        let (stem, ext) = match base.rsplit_once('.') {
            Some((s, e)) => (s.to_string(), format!(".{e}")),
            None => (base.clone(), String::new()),
        };
        let mut name = base.clone();
        let mut n = 2;
        while !taken.insert(name.to_lowercase()) {
            name = format!("{stem} ({n}){ext}");
            n += 1;
        }
        let usb_path = format!("/Contents/{name}");

        let file_size = std::fs::metadata(&source)
            .map(|m| m.len().min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        let props = t.properties.as_ref();
        let duration_ms = props.map(|p| p.duration_ms).unwrap_or(0);
        let analysis = t.analysis.as_ref();
        let beats = analysis
            .map(|a| a.beatgrid.expand_to(duration_ms))
            .unwrap_or_default();
        let key_id = keys.get(
            analysis
                .and_then(|a| a.key)
                .map(|k| k.camelot().label())
                .as_deref(),
        );

        let anlz_dir = format!("P{:03}/{:08X}", (id - 1) / 256, id);
        let row = TrackRow {
            id,
            sample_rate_hz: props.map(|p| p.sample_rate_hz).unwrap_or(0),
            file_size,
            master_content_id: master_content_id(id),
            artwork_id: 0,
            key_id,
            label_id: labels.get(t.tags.label.as_deref()),
            bitrate_kbps: props.and_then(|p| p.bitrate_kbps).unwrap_or(0),
            track_number: t.tags.track_number.unwrap_or(0) as u32,
            tempo_centi_bpm: analysis
                .and_then(|a| a.bpm)
                .map(|b| (b * 100.0).round() as u32)
                .unwrap_or(0),
            genre_id: genres.get(t.tags.genre.as_deref()),
            album_id: albums.get(t.tags.album.as_deref()),
            artist_id: artists.get(t.tags.artist.as_deref()),
            disc_number: t.tags.disc_number.unwrap_or(0),
            year: t.tags.year.unwrap_or(0),
            sample_depth: props.and_then(|p| p.bit_depth).unwrap_or(16) as u16,
            duration_s: (duration_ms / 1000).min(u16::MAX as u64) as u16,
            file_type: file_type(t.format),
            rating: t.tags.rating.unwrap_or(0).min(5),
            isrc: t.tags.isrc.clone().unwrap_or_default(),
            date_added: date.clone(),
            analyze_path: format!("/PIONEER/USBANLZ/{anlz_dir}/ANLZ0000.DAT"),
            analyze_date: date.clone(),
            comment: t.tags.comment.clone().unwrap_or_default(),
            title: t
                .tags
                .title
                .clone()
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| stem.clone()),
            filename: name.clone(),
            file_path: usb_path.clone(),
        };
        let dest = contents.join(&name);
        let copy_needed = std::fs::metadata(&dest)
            .map(|m| m.len() != file_size as u64)
            .unwrap_or(true);
        resolved.push(ExportTrack {
            catalog_id: t.id,
            row,
            source,
            usb_path,
            duration_ms,
            beats,
            preview: analysis.map(|a| a.waveform_preview.clone()).unwrap_or_default(),
            bands: analysis.map(|a| a.waveform_bands.clone()).unwrap_or_default(),
            copy_needed,
        });
    }
    if resolved.is_empty() {
        return Err(ExportError::NoTracks);
    }

    // ---- copy audio -------------------------------------------------------
    let total = resolved.len();
    for (i, tr) in resolved.iter().enumerate() {
        check(cancel)?;
        progress(ExportProgress {
            stage: ExportStage::CopyingAudio,
            done: i,
            total,
            detail: tr.row.filename.clone(),
        });
        if tr.copy_needed {
            let dest = contents.join(&tr.row.filename);
            let n = std::fs::copy(&tr.source, &dest).map_err(io_err(dest))?;
            report.bytes_copied += n;
        }
    }

    // ---- ANLZ files -------------------------------------------------------
    for (i, tr) in resolved.iter().enumerate() {
        check(cancel)?;
        progress(ExportProgress {
            stage: ExportStage::WritingAnalysis,
            done: i,
            total,
            detail: tr.row.filename.clone(),
        });
        let dir = anlz_root
            .join(format!("P{:03}", (tr.row.id - 1) / 256))
            .join(format!("{:08X}", tr.row.id));
        std::fs::create_dir_all(&dir).map_err(io_err(dir.clone()))?;
        let inp = anlz::AnlzInput {
            usb_path: &tr.usb_path,
            beats: &tr.beats,
            duration_ms: tr.duration_ms,
            preview: &tr.preview,
            bands: &tr.bands,
        };
        let dat = dir.join("ANLZ0000.DAT");
        std::fs::write(&dat, anlz::build_dat(&inp)).map_err(io_err(dat))?;
        let ext = dir.join("ANLZ0000.EXT");
        std::fs::write(&ext, anlz::build_ext(&inp)).map_err(io_err(ext))?;
    }

    // ---- playlists --------------------------------------------------------
    check(cancel)?;
    progress(ExportProgress {
        stage: ExportStage::WritingDatabase,
        done: 0,
        total: 1,
        detail: "export.pdb".into(),
    });

    let by_catalog: HashMap<Id, u32> = resolved
        .iter()
        .map(|t| (t.catalog_id, t.row.id))
        .collect();
    // Export playlist ids: 1..M in listing order; parents remapped (a parent
    // outside the exported set becomes top-level rather than dangling).
    let pl_ids: HashMap<Id, u32> = playlists
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id, i as u32 + 1))
        .collect();
    let mut playlist_rows = Vec::new();
    let mut entries = Vec::new();
    for (i, p) in playlists.iter().enumerate() {
        let id = i as u32 + 1;
        playlist_rows.push(PlaylistRow {
            id,
            parent_id: p
                .parent
                .and_then(|pp| pl_ids.get(&pp).copied())
                .unwrap_or(0),
            sort_order: id,
            is_folder: p.is_folder,
            name: p.name.clone(),
        });
        if !p.is_folder {
            let mut idx = 1u32;
            for tid in &p.track_ids {
                if let Some(&track) = by_catalog.get(tid) {
                    entries.push((idx, track, id));
                    idx += 1;
                }
            }
        }
    }
    report.playlists_exported = playlist_rows.len();
    report.tracks_exported = resolved.len();

    let tables = PdbTables {
        tracks: resolved.iter().map(|t| t.row.clone()).collect(),
        genres: genres.rows,
        artists: artists.rows,
        albums: albums.rows,
        labels: labels.rows,
        keys: keys.rows,
        artwork: Vec::new(),
        playlists: playlist_rows,
        playlist_entries: entries,
        created_date: date,
    };

    // ---- databases --------------------------------------------------------
    let pdb = rb_dir.join("export.pdb");
    std::fs::write(&pdb, pdbw::build_export_pdb(&tables)).map_err(io_err(pdb))?;
    let ext_pdb = rb_dir.join("exportExt.pdb");
    std::fs::write(&ext_pdb, pdbw::build_export_ext_pdb()).map_err(io_err(ext_pdb))?;

    check(cancel)?;
    let device_name = dest_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ORDNUNG".into());
    let dlp = rb_dir.join("exportLibrary.db");
    crate::dlp::write_library(&dlp, &tables, &device_name)
        .map_err(|e| ExportError::Dlp(e.to_string()))?;

    progress(ExportProgress {
        stage: ExportStage::WritingDatabase,
        done: 1,
        total: 1,
        detail: "done".into(),
    });
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fat_sanitizer_neutralizes_reserved_chars() {
        assert_eq!(sanitize_fat("a/b:c*d?.mp3"), "a_b_c_d_.mp3");
        assert_eq!(sanitize_fat("  spaced.aif  "), "spaced.aif");
        assert_eq!(sanitize_fat("dots..."), "dots");
        assert_eq!(sanitize_fat(""), "track");
    }

    #[test]
    fn content_ids_are_nonzero_28bit_and_stable() {
        for id in 1..1000 {
            let m = master_content_id(id);
            assert!(m > 0 && m < (1 << 28));
            assert_eq!(m, master_content_id(id));
        }
    }

    #[test]
    fn date_is_iso_shaped() {
        let d = today();
        assert_eq!(d.len(), 10);
        assert_eq!(&d[4..5], "-");
        assert!(d.starts_with("20"));
    }
}
