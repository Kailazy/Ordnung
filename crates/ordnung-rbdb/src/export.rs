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
//! /PIONEER/USBANLZ/Pnnn/<8-hex>/ANLZ0000.{DAT,EXT,2EX}         (anlz)
//! /PIONEER/Artwork/nnnnn/{a,b}N[_m].jpg   cover art 80/240 px  (artwork)
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
    #[error(
        "not enough space on the destination: need ~{need_mb} MB, {free_mb} MB free"
    )]
    NotEnoughSpace { need_mb: u64, free_mb: u64 },
    #[error("the export wrote a database that failed validation ({0}); the stick was not left in a half-written state")]
    Validation(String),
}

type Result<T> = std::result::Result<T, ExportError>;

fn io_err(path: impl Into<PathBuf>) -> impl FnOnce(std::io::Error) -> ExportError {
    let path = path.into();
    move |source| ExportError::Io { path, source }
}

/// Write `bytes` to `path` and fsync before returning. Everything the export
/// puts on the stick goes through this (or [`sync_existing`]): a USB pulled
/// without ejecting must never hold a half-flushed database — that reads as
/// "Device library is corrupted" on players.
pub(crate) fn write_synced(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut f = std::fs::File::create(path)?;
    std::io::Write::write_all(&mut f, bytes)?;
    f.sync_all()
}

/// Write `bytes` to `path` **atomically**: fsync a sibling temp file, then
/// rename it over `path`. A reader (a CDJ, rekordbox) therefore only ever sees
/// the complete old file or the complete new one — never a truncated file mid
/// write. Used for the browse databases, which a player reads first and treats
/// as the manifest for the whole stick: a torn `export.pdb` fails the device.
/// The temp sits in the same directory so the rename stays within one
/// filesystem (a cross-device rename would fall back to a non-atomic copy).
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = tmp_sibling(path);
    // A leftover temp from a previously-killed export would fail create; clear it.
    let _ = std::fs::remove_file(&tmp);
    write_synced(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// Atomically place an already-built file (e.g. the DLP built on local disk)
/// onto `dest`: copy to a sibling temp, fsync, rename over `dest`. Same
/// all-or-nothing guarantee as [`write_atomic`] without holding the bytes in
/// memory.
pub(crate) fn place_atomic(src: &Path, dest: &Path) -> std::io::Result<()> {
    let tmp = tmp_sibling(dest);
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(src, &tmp)?;
    sync_existing(&tmp)?;
    std::fs::rename(&tmp, dest)?;
    Ok(())
}

/// A hidden temp path beside `path` (`.foo.pdb.tmp`), on the same filesystem so
/// the rename is atomic.
fn tmp_sibling(path: &Path) -> PathBuf {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    path.with_file_name(format!(".{name}.tmp"))
}

/// fsync a file that was produced by `fs::copy`.
pub(crate) fn sync_existing(path: &Path) -> std::io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

/// fsync a directory so a rename/create *into* it is durable. On FAT (and
/// most filesystems) a file rename isn't persisted until the directory entry
/// itself is flushed — without this a power loss just after export can leave
/// the directory still pointing at the old file even though the new bytes are
/// on disk. Best-effort: platforms/filesystems that reject a directory fsync
/// (some don't support it) simply skip it rather than failing the export.
pub(crate) fn sync_dir(dir: &Path) {
    if let Ok(f) = std::fs::File::open(dir) {
        let _ = f.sync_all();
    }
}

/// Bytes we expect to add to the destination for `sources` not already present
/// at the right size, plus a fractional margin for the databases, ANLZ files
/// and artwork the export also writes. Summed from the true u64 file lengths
/// (the `TrackRow.file_size` field is clamped to u32, so it must not be used
/// here). `dest_contents` is the stick's `/Contents` dir, used to skip files
/// already copied.
fn estimated_bytes_needed(sources: &[(PathBuf, String)], dest_contents: &Path) -> u64 {
    let mut audio: u64 = 0;
    for (src, name) in sources {
        let src_len = std::fs::metadata(src).map(|m| m.len()).unwrap_or(0);
        let already = std::fs::metadata(dest_contents.join(name))
            .map(|m| m.len())
            .unwrap_or(0);
        if already != src_len {
            audio += src_len;
        }
    }
    // ANLZ + artwork + databases run well under a fifth of the audio in
    // practice; a 25% margin comfortably covers them and FAT slack.
    audio + audio / 4 + 8 * 1024 * 1024
}

/// Free bytes on the filesystem holding `path`. `None` when it can't be
/// determined (the precheck then skips rather than blocking a valid export).
#[cfg(unix)]
fn free_space(path: &Path) -> Option<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: statvfs reads into a zeroed struct we own; c is a valid C string.
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut st) } != 0 {
        return None;
    }
    Some(st.f_bavail as u64 * st.f_frsize as u64)
}

#[cfg(not(unix))]
fn free_space(_path: &Path) -> Option<u64> {
    None
}

/// What the export is doing right now; `done`/`total` count within the stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportStage {
    CopyingAudio,
    WritingArtwork,
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

/// Clamp a metadata string for the pdb row. A track row must fit one 4 KB
/// page beside its ~0x88-byte header and paths; a pathological comment or
/// title tag (liner notes pasted into a comment field) would otherwise
/// overflow the page. CDJ browse screens show well under this many chars.
fn clamp(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        s.chars().take(max_chars).collect()
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
        let name = clamp(name, 300);
        if let Some(&id) = self.ids.get(&name) {
            return id;
        }
        let id = self.rows.len() as u32 + 1;
        self.ids.insert(name.clone(), id);
        self.rows.push((id, name));
        id
    }
}

/// How a new export relates to whatever is already on the stick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExportMode {
    /// Rebuild the stick from this selection alone. The browse database ends
    /// up listing exactly `tracks`; any earlier export's extra tracks and
    /// playlists disappear from it (their audio under `/Contents` is left
    /// untouched, just unreferenced).
    #[default]
    Replace,
    /// Add this selection to whatever the stick already carries. Tracks
    /// already present (matched by `/Contents` path) keep their existing id,
    /// filename and ANLZ files; genuinely new tracks are appended. Playlists
    /// merge by name — an incoming playlist with an existing name replaces
    /// that one's membership, a new name is added.
    Merge,
}

/// One track already on the stick, reconstructed from the existing export so a
/// merge can preserve it verbatim (same id, same `/Contents` name, same ANLZ
/// files) without re-copying anything.
struct ExistingTrack {
    id: u32,
    usb_path: String,
    row: TrackRow,
}

/// Read the export already at `dest_root` for a merge. Returns the existing
/// tracks (keyed for path lookup) and playlist nodes. A stick with no export
/// (or an unreadable one) merges as if empty.
fn read_existing(dest_root: &Path) -> (Vec<ExistingTrack>, Vec<PlaylistRow>) {
    let pdb = dest_root
        .join("PIONEER")
        .join("rekordbox")
        .join("export.pdb");
    let Ok(export) = crate::pdb::read_export(&pdb) else {
        return (Vec::new(), Vec::new());
    };
    let mut tracks = Vec::new();
    for (id, t) in &export.tracks {
        // Rebuild just enough of the row to re-emit it. The read-side view is
        // lossy (no file_size/master id/etc.), so fields it can't see are
        // derived the same way a fresh export would derive them, and the audio
        // + ANLZ bytes already on the stick are what actually matter.
        let name = t.file_path.rsplit('/').next().unwrap_or("").to_string();
        let mut row = TrackRow {
            id: *id,
            master_content_id: master_content_id(*id),
            tempo_centi_bpm: t.tempo_centi_bpm,
            duration_s: t.duration_s,
            bitrate_kbps: t.bitrate_kbps,
            sample_rate_hz: t.sample_rate_hz,
            year: t.year,
            file_type: file_type_from_ext(&name),
            analyze_path: t
                .analyze_path
                .clone()
                .unwrap_or_else(|| default_anlz_path(*id)),
            title: t.title.clone(),
            comment: t.comment.clone(),
            filename: name.clone(),
            file_path: t.file_path.clone(),
            sample_depth: 16,
            // date_added is stamped with the export date by the caller.
            ..Default::default()
        };
        // Re-fetch file size from the stick so the row stays truthful.
        let on_disk = dest_root.join(t.file_path.trim_start_matches('/'));
        row.file_size = std::fs::metadata(&on_disk)
            .map(|m| m.len().min(u32::MAX as u64) as u32)
            .unwrap_or(0);
        tracks.push(ExistingTrack {
            id: *id,
            usb_path: t.file_path.clone(),
            row,
        });
    }
    let playlists = export
        .playlists
        .iter()
        .map(|p| PlaylistRow {
            id: p.id,
            parent_id: p.parent_id,
            sort_order: p.sort_order,
            is_folder: p.is_folder,
            name: p.name.clone(),
        })
        .collect();
    (tracks, playlists)
}

/// File-type enum from a filename extension (the read side doesn't store it).
fn file_type_from_ext(name: &str) -> u16 {
    match name.rsplit('.').next().map(|e| e.to_lowercase()).as_deref() {
        Some("mp3") => 1,
        Some("m4a") | Some("aac") => 4,
        Some("flac") => 5,
        Some("wav") => 11,
        Some("aif") | Some("aiff") => 12,
        _ => 0,
    }
}

/// The ANLZ path a fresh export would give track `id` (P-dir + 8-hex).
fn default_anlz_path(id: u32) -> String {
    format!(
        "/PIONEER/USBANLZ/P{:03}/{:08X}/ANLZ0000.DAT",
        (id - 1) / 256,
        id
    )
}

/// Export `tracks` (in the given order) and `playlists` onto `dest_root`.
///
/// Re-read the databases the export just wrote and confirm the stick is
/// internally consistent, so a subtle writer bug surfaces here as a loud
/// failure instead of a stick that looks exported but won't load on a player.
/// Checks: `export.pdb` re-parses; every track row's `analyze_path` names an
/// ANLZ file that exists on the stick; every playlist entry points at a track
/// the database actually contains; and the number of tracks/playlists read
/// back matches what was written. Returns the offending detail on failure.
fn validate_export(
    dest_root: &Path,
    expected_tracks: usize,
    expected_playlists: usize,
) -> std::result::Result<(), String> {
    let pdb = dest_root
        .join("PIONEER")
        .join("rekordbox")
        .join("export.pdb");
    let export = crate::pdb::read_export(&pdb).map_err(|e| format!("export.pdb: {e}"))?;

    if export.tracks.len() != expected_tracks {
        return Err(format!(
            "wrote {expected_tracks} track(s) but read back {}",
            export.tracks.len()
        ));
    }
    if export.playlists.len() != expected_playlists {
        return Err(format!(
            "wrote {expected_playlists} playlist node(s) but read back {}",
            export.playlists.len()
        ));
    }
    // Every track's analysis file must actually be on the stick.
    for t in export.tracks.values() {
        if let Some(rel) = t.analyze_path.as_deref() {
            let on_disk = dest_root.join(rel.trim_start_matches('/'));
            if !on_disk.is_file() {
                return Err(format!(
                    "track {:?} references a missing ANLZ file {rel}",
                    t.title
                ));
            }
        }
    }
    // Every playlist entry must resolve to a real track row.
    for (playlist_id, track_ids) in &export.entries {
        for tid in track_ids {
            if !export.tracks.contains_key(tid) {
                return Err(format!(
                    "playlist {playlist_id} references track id {tid}, which has no row"
                ));
            }
        }
    }
    Ok(())
}

/// The destination is a mounted FAT32 volume root or any directory (a staging
/// folder, an `ORDNUNG_FAKE_USB` root). `mode` decides whether the stick is
/// rebuilt from this selection ([`ExportMode::Replace`]) or the selection is
/// added to what is already there ([`ExportMode::Merge`]). Audio already
/// present in `/Contents` with matching size is not re-copied either way.
///
/// Robustness contract: nothing is written until a free-space precheck passes;
/// the audio, ANLZ and artwork are fsynced as they're written; the browse
/// databases are placed atomically (temp + rename) so an interrupted export
/// never leaves a torn manifest; the containing directory is fsynced; and the
/// written databases are re-read and validated before success is reported.
pub fn export_usb(
    dest_root: &Path,
    tracks: &[Track],
    playlists: &[Playlist],
    mode: ExportMode,
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

    // ---- carry-over from an existing export (Merge only) -----------------
    // In Merge mode the tracks already on the stick are kept verbatim — same
    // id, filename and ANLZ files — and the new selection is layered on top.
    let (existing_tracks, existing_playlists) = if mode == ExportMode::Merge {
        read_existing(dest_root)
    } else {
        (Vec::new(), Vec::new())
    };
    // Path (as stored on the stick) → its existing id, so a re-selected track
    // reuses its slot instead of getting a duplicate.
    let existing_by_path: HashMap<String, u32> = existing_tracks
        .iter()
        .map(|e| (e.usb_path.to_lowercase(), e.id))
        .collect();
    let mut next_id = existing_tracks.iter().map(|e| e.id).max().unwrap_or(0) + 1;

    // ---- resolve tracks: filenames, interned ids, metadata ---------------
    let mut artists = Intern::default();
    let mut albums = Intern::default();
    let mut genres = Intern::default();
    let mut labels = Intern::default();
    let mut keys = Intern::default();
    let mut covers = crate::artwork::ArtworkStore::default();

    // Filenames already taken (existing rows + rows we assign this pass).
    let mut taken: HashSet<String> = existing_tracks
        .iter()
        .map(|e| e.row.filename.to_lowercase())
        .collect();
    let mut resolved: Vec<ExportTrack> = Vec::new();
    // Track ids we've placed this pass, to skip carrying an existing row that
    // the new selection already re-covers.
    let mut placed_ids: HashSet<u32> = HashSet::new();

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

        // Filename first, so a merge can spot a track already on the stick by
        // its `/Contents` path and reuse that id, filename and ANLZ dir.
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
        // Does this exact `/Contents/<base>` already live on the stick?
        let reuse_id = existing_by_path
            .get(&format!("/contents/{}", base.to_lowercase()))
            .copied();
        let (id, name) = match reuse_id {
            Some(id) => {
                taken.insert(base.to_lowercase());
                (id, base.clone())
            }
            None => {
                let mut name = base.clone();
                let mut n = 2;
                while !taken.insert(name.to_lowercase()) {
                    name = format!("{stem} ({n}){ext}");
                    n += 1;
                }
                let id = next_id;
                next_id += 1;
                (id, name)
            }
        };
        placed_ids.insert(id);
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

        // The cover travels from the file's own tags onto the stick as
        // rekordbox artwork; identical covers across an album intern once.
        let artwork_id = ordnung_core::tag::read_front_cover_raw(&source)
            .ok()
            .flatten()
            .map(|c| covers.intern(c.bytes()))
            .unwrap_or(0);

        let anlz_dir = format!("P{:03}/{:08X}", (id - 1) / 256, id);
        let row = TrackRow {
            id,
            sample_rate_hz: props.map(|p| p.sample_rate_hz).unwrap_or(0),
            file_size,
            master_content_id: master_content_id(id),
            artwork_id,
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
            comment: clamp(t.tags.comment.as_deref().unwrap_or(""), 500),
            title: clamp(
                t.tags
                    .title
                    .as_deref()
                    .filter(|s| !s.trim().is_empty())
                    .unwrap_or(&stem),
                300,
            ),
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
    // ---- carry over existing tracks the selection didn't re-cover --------
    // Their audio and ANLZ files already sit on the stick, so they need a row
    // (with re-interned metadata) but no copy and no ANLZ write. Re-interning
    // pulls their artist/album/genre/label names out of the row strings we
    // read back, so the browse tables stay populated for them too.
    let mut carried: Vec<TrackRow> = Vec::new();
    for e in &existing_tracks {
        if placed_ids.contains(&e.id) {
            continue; // the new selection re-covered this track
        }
        // The read side didn't preserve interned *names*, only that the row had
        // them; re-intern from the DLP mirror is out of reach here, so carried
        // rows keep their text fields but resolve id references to 0 (the
        // player still lists them by title/filename). Their date_added is set
        // to this export's date for consistency.
        let mut row = e.row.clone();
        row.date_added = date.clone();
        row.analyze_date = date.clone();
        carried.push(row);
    }

    if resolved.is_empty() && carried.is_empty() {
        return Err(ExportError::NoTracks);
    }

    // ---- free-space precheck ----------------------------------------------
    // Refuse before touching the stick if the selection can't fit, rather than
    // half-writing it. Sum true file lengths of the audio we'd actually copy.
    let sources: Vec<(PathBuf, String)> = resolved
        .iter()
        .map(|t| (t.source.clone(), t.row.filename.clone()))
        .collect();
    let need = estimated_bytes_needed(&sources, &contents);
    if let Some(free) = free_space(dest_root) {
        if free < need {
            return Err(ExportError::NotEnoughSpace {
                need_mb: need / (1024 * 1024),
                free_mb: free / (1024 * 1024),
            });
        }
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
            let n = std::fs::copy(&tr.source, &dest).map_err(io_err(dest.clone()))?;
            sync_existing(&dest).map_err(io_err(dest))?;
            report.bytes_copied += n;
        }
    }

    // ---- artwork ----------------------------------------------------------
    for (i, art) in covers.files.iter().enumerate() {
        check(cancel)?;
        progress(ExportProgress {
            stage: ExportStage::WritingArtwork,
            done: i,
            total: covers.files.len(),
            detail: format!("artwork {}", art.id),
        });
        crate::artwork::write_files(dest_root, art)
            .map_err(io_err(dest_root.join("PIONEER").join("Artwork")))?;
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
        // Decode the audio for the detailed waveforms (PWV3/5/6/7): rekordbox
        // stores linear per-column band peaks at 150 col/s, and the catalog's
        // 20 bins/sec analysis cache can't reconstruct that beat-level pulse.
        // On decode failure the coarse cached data still renders something.
        let scroll = ordnung_core::analysis::decode_mono(&tr.source)
            .map(|a| ordnung_core::analysis::waveform::scroll_bands(&a.samples, a.sample_rate))
            .unwrap_or_default();
        let inp = anlz::AnlzInput {
            usb_path: &tr.usb_path,
            beats: &tr.beats,
            duration_ms: tr.duration_ms,
            preview: &tr.preview,
            bands: &tr.bands,
            scroll: &scroll,
        };
        let dat = dir.join("ANLZ0000.DAT");
        write_synced(&dat, &anlz::build_dat(&inp)).map_err(io_err(dat))?;
        let ext = dir.join("ANLZ0000.EXT");
        write_synced(&ext, &anlz::build_ext(&inp)).map_err(io_err(ext))?;
        let ex2 = dir.join("ANLZ0000.2EX");
        write_synced(&ex2, &anlz::build_2ex(&inp)).map_err(io_err(ex2))?;
    }

    // ---- playlists --------------------------------------------------------
    check(cancel)?;
    progress(ExportProgress {
        stage: ExportStage::WritingDatabase,
        done: 0,
        total: 1,
        detail: "export.pdb".into(),
    });

    // Catalog track id → the pdb track id it landed on (new or reused slot).
    let by_catalog: HashMap<Id, u32> = resolved
        .iter()
        .map(|t| (t.catalog_id, t.row.id))
        .collect();

    // Build the export playlist tree. In Merge mode we start from the existing
    // tree (preserving ids and existing memberships) and layer the incoming
    // playlists on by name: an incoming name that already exists replaces that
    // node's membership; a new name is appended. In Replace mode the existing
    // tree is empty, so this reduces to numbering the incoming playlists 1..M.
    let mut playlist_rows: Vec<PlaylistRow> = existing_playlists.clone();
    let mut entries: Vec<(u32, u32, u32)> = Vec::new();
    // Existing memberships are read straight back onto their (unchanged) track
    // ids, except for any node an incoming playlist is about to replace.
    let existing_export = if mode == ExportMode::Merge {
        crate::pdb::read_export(&rb_dir.join("export.pdb")).ok()
    } else {
        None
    };
    let mut next_pl_id = playlist_rows.iter().map(|p| p.id).max().unwrap_or(0) + 1;
    // Incoming names that will replace an existing node (so we drop its old
    // membership below).
    let incoming_names: HashSet<String> = playlists
        .iter()
        .filter(|p| !p.is_folder)
        .map(|p| clamp(&p.name, 300).to_lowercase())
        .collect();
    if let Some(exp) = &existing_export {
        for p in &playlist_rows {
            if p.is_folder || incoming_names.contains(&p.name.to_lowercase()) {
                continue;
            }
            if let Some(ids) = exp.entries.get(&p.id) {
                for (i, tid) in ids.iter().enumerate() {
                    entries.push((i as u32 + 1, *tid, p.id));
                }
            }
        }
    }
    // Map an incoming catalog playlist id → the export playlist id it becomes.
    let mut pl_ids: HashMap<Id, u32> = HashMap::new();
    for p in playlists {
        let clamped = clamp(&p.name, 300);
        // Reuse an existing node of the same name+kind, else mint a new id.
        let id = playlist_rows
            .iter()
            .find(|e| e.name == clamped && e.is_folder == p.is_folder)
            .map(|e| e.id)
            .unwrap_or_else(|| {
                let id = next_pl_id;
                next_pl_id += 1;
                id
            });
        pl_ids.insert(p.id, id);
    }
    for p in playlists {
        let id = pl_ids[&p.id];
        let parent_id = p
            .parent
            .and_then(|pp| pl_ids.get(&pp).copied())
            .unwrap_or(0);
        let clamped = clamp(&p.name, 300);
        match playlist_rows.iter_mut().find(|e| e.id == id) {
            Some(existing) => {
                existing.parent_id = parent_id;
                existing.name = clamped;
                existing.is_folder = p.is_folder;
            }
            None => playlist_rows.push(PlaylistRow {
                id,
                parent_id,
                sort_order: id,
                is_folder: p.is_folder,
                name: clamped,
            }),
        }
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
    report.tracks_exported = resolved.len() + carried.len();

    // The full track set the database lists: freshly resolved + carried-over.
    let all_track_rows: Vec<TrackRow> = resolved
        .iter()
        .map(|t| t.row.clone())
        .chain(carried.iter().cloned())
        .collect();
    let tables = PdbTables {
        tracks: all_track_rows,
        genres: genres.rows,
        artists: artists.rows,
        albums: albums.rows,
        labels: labels.rows,
        keys: keys.rows,
        artwork: covers
            .files
            .iter()
            .map(|a| (a.id, crate::artwork::pdb_path(a.id)))
            .collect(),
        playlists: playlist_rows,
        playlist_entries: entries,
        created_date: date,
    };

    // ---- databases --------------------------------------------------------
    // The DLP is built first (on local disk, then placed) and the pdbs after,
    // so `export.pdb` — the file a player reads first and treats as the whole
    // stick's manifest — is the LAST thing to land. Each placement is atomic
    // (temp + rename): an interrupted write leaves the previous complete file,
    // never a torn one.
    check(cancel)?;
    let device_name = dest_root
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ORDNUNG".into());
    let dlp = rb_dir.join("exportLibrary.db");
    crate::dlp::write_library(&dlp, &tables, &device_name)
        .map_err(|e| ExportError::Dlp(e.to_string()))?;

    let ext_pdb = rb_dir.join("exportExt.pdb");
    write_atomic(&ext_pdb, &pdbw::build_export_ext_pdb()).map_err(io_err(ext_pdb))?;
    let pdb = rb_dir.join("export.pdb");
    write_atomic(&pdb, &pdbw::build_export_pdb(&tables)).map_err(io_err(pdb))?;

    // Make the renames durable: without a directory fsync a rename isn't
    // persisted on FAT until the dir entry flushes, so a power loss right after
    // export could leave the directory pointing at the old files.
    sync_dir(&rb_dir);

    // ---- validate what we wrote -------------------------------------------
    // Re-read the databases and confirm the stick is internally consistent
    // before reporting success. A writer bug surfaces here as a loud failure,
    // not a stick that looks done but won't load. This runs after the atomic
    // placement, so a failure still leaves a complete (if wrong) database
    // rather than a half-written one — but the caller learns not to trust it.
    validate_export(dest_root, tables.tracks.len(), tables.playlists.len())
        .map_err(ExportError::Validation)?;

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

    #[test]
    fn write_atomic_leaves_no_temp_and_replaces_content() {
        let dir = std::env::temp_dir().join(format!("ordnung-atomic-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("export.pdb");
        write_atomic(&f, b"first").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"first");
        write_atomic(&f, b"second-longer").unwrap();
        assert_eq!(std::fs::read(&f).unwrap(), b"second-longer");
        // No `.export.pdb.tmp` sidecar left behind.
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftover.is_empty(), "temp not cleaned: {leftover:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn estimated_bytes_skips_files_already_present_at_size() {
        let dir = std::env::temp_dir().join(format!("ordnung-space-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let contents = dir.join("Contents");
        std::fs::create_dir_all(&contents).unwrap();
        let src = dir.join("a.mp3");
        std::fs::write(&src, vec![0u8; 1000]).unwrap();
        let sources = vec![(src.clone(), "a.mp3".to_string())];
        // Nothing on the stick yet: the 1000 bytes count (plus margin).
        let need_fresh = estimated_bytes_needed(&sources, &contents);
        assert!(need_fresh >= 1000);
        // Same-size file already present: its bytes drop out of the estimate.
        std::fs::write(contents.join("a.mp3"), vec![0u8; 1000]).unwrap();
        let need_present = estimated_bytes_needed(&sources, &contents);
        assert!(
            need_present < need_fresh,
            "an already-copied file must not be re-counted"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_export_flags_dangling_playlist_entry() {
        // A hand-built stick whose playlist points at a nonexistent track must
        // fail validation — proving the pass actually cross-checks entries.
        let dir = std::env::temp_dir().join(format!("ordnung-val-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let rb = dir.join("PIONEER").join("rekordbox");
        let anlz = dir.join("PIONEER").join("USBANLZ").join("P000").join("00000001");
        std::fs::create_dir_all(&rb).unwrap();
        std::fs::create_dir_all(&anlz).unwrap();
        // Write a pdb with one track (id 1) but a playlist entry for track 99.
        let mut tables = crate::pdbw::PdbTables {
            created_date: "2026-01-01".into(),
            ..Default::default()
        };
        tables.tracks.push(crate::pdbw::TrackRow {
            id: 1,
            title: "T".into(),
            filename: "t.mp3".into(),
            file_path: "/Contents/t.mp3".into(),
            analyze_path: "/PIONEER/USBANLZ/P000/00000001/ANLZ0000.DAT".into(),
            ..Default::default()
        });
        tables.playlists.push(crate::pdbw::PlaylistRow {
            id: 1,
            parent_id: 0,
            sort_order: 1,
            is_folder: false,
            name: "p".into(),
        });
        tables.playlist_entries.push((1, 99, 1)); // track 99 doesn't exist
        std::fs::write(rb.join("export.pdb"), crate::pdbw::build_export_pdb(&tables)).unwrap();
        // Give the referenced ANLZ file so only the dangling entry is wrong.
        std::fs::write(anlz.join("ANLZ0000.DAT"), b"PMAI").unwrap();

        let err = validate_export(&dir, 1, 1).unwrap_err();
        assert!(err.contains("track id 99"), "got: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
