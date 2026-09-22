//! Editing an existing rekordbox stick in place.
//!
//! The export command rebuilds a whole stick; this module instead applies one
//! user gesture to whatever export is already mounted. Playlist edits —
//! create, rename, delete a playlist, or add tracks to one — touch *only*
//! the playlist data; per-track edits ([`write_stick_cues`],
//! [`write_stick_beatgrid`]) touch only that track's ANLZ files (plus its
//! tempo in the databases for a grid change), the way rekordbox's own device
//! view saves a cue or a grid nudge straight onto the stick.
//!
//! * `export.pdb` — the PlaylistTree and PlaylistEntries tables are rewritten
//!   in place (surgically: every other table's pages are left byte-identical).
//!   Freed pages are blanked, extra pages are appended at the end of the file,
//!   and the table directory / page chains are relinked — see
//!   `docs/rekordbox-export-structure.md` for the bookkeeping being honored.
//! * `exportLibrary.db` — when the stick carries a Device Library Plus
//!   database (every rekordbox 6/7 export), its `playlist` /
//!   `playlist_content` tables are replaced to mirror the same tree, joining
//!   pdb track ids onto DLP content ids by file path (the only shared key).
//!
//! Audio files and every other table are never touched. Before the first
//! edit ever made to a stick (or to a track's analysis), the pristine files
//! are copied to `*.orig` alongside themselves, so the pre-Ordnung state is
//! always recoverable.

use std::collections::HashMap;
use std::path::Path;

use ordnung_core::model::{Beat, Cue};

use crate::pdb::{RbExport, RbPlaylist, ReadError};
use crate::pdbw;

/// One user-level playlist edit. Paths are relative to the volume root with
/// no leading slash (e.g. `Contents/Artist/track.mp3`), matched
/// case-insensitively against the export's stored paths (the volume is FAT32).
#[derive(Debug, Clone)]
pub enum PlaylistOp {
    /// Create an empty playlist under `parent_id` (`0` = top level).
    Create { name: String, parent_id: u32 },
    /// Rename a playlist or folder.
    Rename { id: u32, name: String },
    /// Delete a playlist, or a folder and everything under it.
    Delete { id: u32 },
    /// Append tracks (by path) to a playlist, skipping ones already in it and
    /// paths the export doesn't know.
    AddTracks { id: u32, rel_paths: Vec<String> },
}

/// What an edit produced: the id of a created playlist, and the stick's
/// post-edit state so the caller can refresh without re-reading the device.
#[derive(Debug)]
pub struct EditOutcome {
    /// The new playlist's id for `Create`; `None` for every other op.
    pub new_id: Option<u32>,
    /// The export as it now stands (playlists and entries reflect the edit;
    /// tracks are as read).
    pub export: RbExport,
}

/// Apply one playlist edit to the mounted stick at `volume_root` and write it
/// back to both databases. Returns the post-edit state.
pub fn edit_stick_playlists(
    volume_root: &Path,
    op: &PlaylistOp,
) -> Result<EditOutcome, ReadError> {
    let mut export = crate::pdb::read_stick(volume_root)?;
    let new_id = apply_op(&mut export, op)?;

    let dir = volume_root.join("PIONEER").join("rekordbox");
    let pdb_path = dir.join("export.pdb");
    backup_once(&pdb_path);
    rewrite_pdb_playlist_tables(&pdb_path, &export.playlists, &export.entries)?;

    let dlp_path = dir.join("exportLibrary.db");
    if dlp_path.is_file() {
        backup_once(&dlp_path);
        sync_dlp_playlists(&dlp_path, &export)?;
    }
    Ok(EditOutcome { new_id, export })
}

/// Copy `path` to `path.orig` the first time a stick is ever edited, so the
/// pre-Ordnung database can always be restored. Best-effort: a failed backup
/// (full stick) doesn't block the edit.
fn backup_once(path: &Path) {
    let mut orig = path.as_os_str().to_owned();
    orig.push(".orig");
    let orig = std::path::PathBuf::from(orig);
    if !orig.exists() {
        let _ = std::fs::copy(path, &orig);
    }
}

/// Mutate the in-memory export per `op`. Returns the created id for `Create`.
fn apply_op(export: &mut RbExport, op: &PlaylistOp) -> Result<Option<u32>, ReadError> {
    match op {
        PlaylistOp::Create { name, parent_id } => {
            let id = export.playlists.iter().map(|p| p.id).max().unwrap_or(0) + 1;
            let sort = export
                .playlists
                .iter()
                .map(|p| p.sort_order)
                .max()
                .unwrap_or(0)
                + 1;
            export.playlists.push(RbPlaylist {
                id,
                parent_id: *parent_id,
                sort_order: sort,
                is_folder: false,
                name: name.clone(),
            });
            export.entries.insert(id, Vec::new());
            Ok(Some(id))
        }
        PlaylistOp::Rename { id, name } => {
            let Some(p) = export.playlists.iter_mut().find(|p| p.id == *id) else {
                return Err(ReadError::Format("no such playlist"));
            };
            p.name = name.clone();
            Ok(None)
        }
        PlaylistOp::Delete { id } => {
            // A folder takes its whole subtree with it, exactly like the
            // desktop app: collect ids transitively, then drop nodes+entries.
            let mut doomed = vec![*id];
            let mut i = 0;
            while i < doomed.len() {
                let parent = doomed[i];
                doomed.extend(
                    export
                        .playlists
                        .iter()
                        .filter(|p| p.parent_id == parent && p.id != parent)
                        .map(|p| p.id),
                );
                i += 1;
            }
            export.playlists.retain(|p| !doomed.contains(&p.id));
            for d in &doomed {
                export.entries.remove(d);
            }
            Ok(None)
        }
        PlaylistOp::AddTracks { id, rel_paths } => {
            if !export.playlists.iter().any(|p| p.id == *id && !p.is_folder) {
                return Err(ReadError::Format("no such playlist"));
            }
            let by_path: HashMap<String, u32> = export
                .tracks
                .iter()
                .map(|(tid, t)| (t.file_path.trim_start_matches('/').to_lowercase(), *tid))
                .collect();
            let list = export.entries.entry(*id).or_default();
            for rel in rel_paths {
                let Some(tid) = by_path.get(&rel.trim_start_matches('/').to_lowercase()) else {
                    continue;
                };
                if !list.contains(tid) {
                    list.push(*tid);
                }
            }
            Ok(None)
        }
    }
}

// ---------------------------------------------------------------------------
// Per-track edits — cues and beatgrid, straight onto the stick
// ---------------------------------------------------------------------------

/// Write a track's cue set onto the stick it lives on, the way rekordbox's
/// own device view does: its `ANLZ0000.DAT` / `.EXT` cue lists are replaced
/// in place, nothing else on the stick changes. `dat_path` is the absolute
/// path of the track's `ANLZ0000.DAT` on the mounted volume. The pristine
/// analysis files are kept as `*.orig` siblings from the first edit on.
pub fn write_stick_cues(dat_path: &Path, cues: &[Cue]) -> Result<(), ReadError> {
    backup_anlz(dat_path);
    crate::anlz::write_cues(dat_path, cues).map_err(|e| ReadError::Io {
        path: dat_path.to_path_buf(),
        source: e,
    })
}

/// Write a track's beatgrid onto the stick it lives on: the ANLZ `PQTZ`
/// (and the `.EXT`'s extended grid, emptied) are replaced in place, the
/// tempo of the track's `export.pdb` row is patched to the grid's BPM, and
/// the Device Library Plus row follows suit. `track_id` is the pdb track id.
/// The pristine analysis files and databases are kept as `*.orig` siblings
/// from the first edit on.
pub fn write_stick_beatgrid(
    volume_root: &Path,
    track_id: u32,
    dat_path: &Path,
    beats: &[Beat],
) -> Result<(), ReadError> {
    backup_anlz(dat_path);
    crate::anlz::write_beatgrid(dat_path, beats).map_err(|e| ReadError::Io {
        path: dat_path.to_path_buf(),
        source: e,
    })?;
    let Some(bpm) = beats.first().map(|b| b.bpm).filter(|b| *b > 0.0) else {
        return Ok(());
    };
    let centi = (bpm * 100.0).round().clamp(0.0, u32::MAX as f32) as u32;
    let dir = volume_root.join("PIONEER").join("rekordbox");
    let pdb_path = dir.join("export.pdb");
    backup_once(&pdb_path);
    let file_path = patch_track_tempo(&pdb_path, track_id, centi)?;
    let dlp_path = dir.join("exportLibrary.db");
    if dlp_path.is_file() {
        backup_once(&dlp_path);
        with_local_dlp(&dlp_path, |conn| {
            let key = file_path.trim_start_matches('/').to_lowercase();
            let mut stmt = conn.prepare("SELECT content_id, path FROM content")?;
            let ids: Vec<i64> = stmt
                .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                .flatten()
                .filter(|(_, p)| p.trim_start_matches('/').to_lowercase() == key)
                .map(|(id, _)| id)
                .collect();
            for id in ids {
                conn.execute(
                    "UPDATE content SET bpmx100 = ?1 WHERE content_id = ?2",
                    rusqlite::params![centi as i64, id],
                )?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// The beatgrid a track had before Ordnung first edited it on this stick —
/// read from the `ANLZ0000.DAT.orig` backup. `None` when the track was
/// never edited here (no backup) or the backup carries no grid.
pub fn original_stick_beatgrid(dat_path: &Path) -> Option<Vec<Beat>> {
    let orig = orig_path(dat_path);
    let beats = crate::anlz::read_beatgrid(&orig);
    (!beats.is_empty()).then_some(beats)
}

fn orig_path(path: &Path) -> std::path::PathBuf {
    let mut orig = path.as_os_str().to_owned();
    orig.push(".orig");
    std::path::PathBuf::from(orig)
}

/// Keep the pristine `.DAT` and `.EXT` beside themselves before the first
/// edit to a track's analysis (the `.2EX` carries no cues or grid).
fn backup_anlz(dat_path: &Path) {
    backup_once(dat_path);
    let ext = dat_path.with_extension("EXT");
    if ext.is_file() {
        backup_once(&ext);
    }
}

/// Patch the `tempo` field (BPM × 100, u32 at row offset 0x38) of one track
/// row of the DeviceSQL database, in place — the row keeps its size, so no
/// page bookkeeping changes. Returns the row's file path (the key the DLP
/// shares). Atomic (temp file + rename) like every database write here.
fn patch_track_tempo(pdb_path: &Path, track_id: u32, centi_bpm: u32) -> Result<String, ReadError> {
    use crate::pdb::{dsql_string, page_rows, table_pages, u16_at, u32_at, TYPE_TRACKS};
    let mut data = std::fs::read(pdb_path).map_err(|e| ReadError::Io {
        path: pdb_path.to_path_buf(),
        source: e,
    })?;
    let page_size = u32_at(&data, 4).ok_or(ReadError::Format("truncated header"))? as usize;
    let num_tables = u32_at(&data, 8).ok_or(ReadError::Format("truncated header"))? as usize;
    if !(512..=65536).contains(&page_size) || num_tables > 64 {
        return Err(ReadError::Format("implausible header"));
    }
    let mut hit: Option<(usize, String)> = None;
    for t in 0..num_tables {
        let base = 0x1C + t * 16;
        if u32_at(&data, base) != Some(TYPE_TRACKS) {
            continue;
        }
        let (Some(first), Some(last)) = (u32_at(&data, base + 8), u32_at(&data, base + 12)) else {
            break;
        };
        for page_off in table_pages(&data, page_size, first, last) {
            for row in page_rows(&data, page_size, page_off, TYPE_TRACKS) {
                if u32_at(&data, row + 0x48) != Some(track_id) {
                    continue;
                }
                let path = u16_at(&data, row + 0x86)
                    .and_then(|rel| dsql_string(&data, row + rel as usize))
                    .unwrap_or_default();
                hit = Some((row, path));
            }
        }
    }
    let Some((row, path)) = hit else {
        return Err(ReadError::Format("no such track row"));
    };
    data[row + 0x38..row + 0x3C].copy_from_slice(&centi_bpm.to_le_bytes());
    crate::export::write_atomic(pdb_path, &data).map_err(|e| ReadError::Io {
        path: pdb_path.to_path_buf(),
        source: e,
    })?;
    Ok(path)
}

/// Run `f` against a local copy of the stick's Device Library Plus database
/// and copy the result back whole — SQLite cannot write in place on macOS's
/// msdos (FAT32) driver (see [`crate::dlp::write_library`]).
fn with_local_dlp(
    db_path: &Path,
    f: impl FnOnce(&rusqlite::Connection) -> rusqlite::Result<()>,
) -> Result<(), ReadError> {
    let err_io = |e: std::io::Error| ReadError::Dlp(e.to_string());
    let err = |e: rusqlite::Error| ReadError::Dlp(e.to_string());
    let tmp = crate::dlp::scratch_db_path("ordnung-dlp-edit");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(db_path, &tmp).map_err(err_io)?;
    let result = (|| {
        let conn = rusqlite::Connection::open(&tmp).map_err(err)?;
        conn.execute_batch(&format!(
            "PRAGMA key = '{}'; PRAGMA cipher_compatibility = 4;",
            crate::dlp::DLP_KEY
        ))
        .map_err(err)?;
        conn.execute_batch("BEGIN").map_err(err)?;
        f(&conn).map_err(err)?;
        conn.execute_batch("COMMIT").map_err(err)?;
        Ok(())
    })();
    if result.is_ok() {
        std::fs::copy(&tmp, db_path).map_err(err_io)?;
        crate::export::sync_existing(db_path).map_err(err_io)?;
    }
    let _ = std::fs::remove_file(&tmp);
    result
}

// ---------------------------------------------------------------------------
// export.pdb surgery
// ---------------------------------------------------------------------------

const TYPE_PLAYLIST_TREE: u32 = 7;
const TYPE_PLAYLIST_ENTRIES: u32 = 8;

/// Rewrite exactly the PlaylistTree and PlaylistEntries tables of the
/// DeviceSQL database at `pdb_path`, leaving every other byte of every other
/// table untouched. The new row set may need more pages than the old one, in
/// which case fresh pages are appended at the end of the file; pages freed by
/// shrinkage are blanked so no reader can pick up stale rows. The write is
/// atomic (temp file + rename).
fn rewrite_pdb_playlist_tables(
    pdb_path: &Path,
    playlists: &[RbPlaylist],
    entries: &HashMap<u32, Vec<u32>>,
) -> Result<(), ReadError> {
    let io_err = |source| ReadError::Io {
        path: pdb_path.to_path_buf(),
        source,
    };
    let mut data = std::fs::read(pdb_path).map_err(io_err)?;

    let page_size = u32_at(&data, 4).ok_or(ReadError::Format("truncated header"))? as usize;
    if page_size != pdbw::PAGE {
        // Never seen in the wild (rekordbox always writes 4096); refuse
        // rather than build pages of the wrong size.
        return Err(ReadError::Format("unsupported page size"));
    }
    let num_tables = u32_at(&data, 8).ok_or(ReadError::Format("truncated header"))? as usize;
    if num_tables > 64 {
        return Err(ReadError::Format("implausible table count"));
    }

    // Encode the replacement rows. Entries are grouped per playlist in tree
    // order with 0-based indices — the shape rekordbox writes and the shape
    // the full-export writer produces.
    let tree_rows: Vec<Vec<u8>> = playlists
        .iter()
        .map(|p| {
            pdbw::pad4(pdbw::playlist_tree_row(&pdbw::PlaylistRow {
                id: p.id,
                parent_id: p.parent_id,
                sort_order: p.sort_order,
                is_folder: p.is_folder,
                name: p.name.clone(),
            }))
        })
        .collect();
    let mut entry_rows: Vec<Vec<u8>> = Vec::new();
    for p in playlists.iter().filter(|p| !p.is_folder) {
        if let Some(tracks) = entries.get(&p.id) {
            for (i, tid) in tracks.iter().enumerate() {
                entry_rows.push(pdbw::pad4(pdbw::playlist_entry_row(i as u32, *tid, p.id)));
            }
        }
    }

    // Fresh pages go at the end of the file; the header's next_unused_page
    // may carry slack beyond the file, so allocate from whichever is larger.
    let mut next_free = (data.len() / page_size) as u32;
    next_free = next_free.max(u32_at(&data, 0x0C).unwrap_or(0));

    let mut plan: Vec<(usize, Vec<Vec<Vec<u8>>>)> = Vec::new(); // (dir slot, chunks)
    for (ty, rows) in [
        (TYPE_PLAYLIST_TREE, &tree_rows),
        (TYPE_PLAYLIST_ENTRIES, &entry_rows),
    ] {
        let slot = (0..num_tables)
            .find(|t| u32_at(&data, 0x1C + t * 16) == Some(ty))
            .ok_or(ReadError::Format("playlist table missing from directory"))?;
        plan.push((slot, pdbw::paginate(rows)));
    }

    // First pass: settle page assignments for both tables so every chain's
    // terminator can point one past the final file size.
    struct TablePlan {
        slot: usize,
        ty: u32,
        keep_first: Option<u32>, // the sentinel page, kept in place
        reusable: Vec<u32>,      // old data pages, rewritten or blanked
        chunks: Vec<Vec<Vec<u8>>>,
        fresh: Vec<u32>, // appended pages
    }
    let mut tables: Vec<TablePlan> = Vec::new();
    for (slot, chunks) in plan {
        let base = 0x1C + slot * 16;
        let ty = u32_at(&data, base).unwrap_or(0);
        let first = u32_at(&data, base + 8).ok_or(ReadError::Format("truncated directory"))?;
        let last = u32_at(&data, base + 12).ok_or(ReadError::Format("truncated directory"))?;
        let chain = walk_chain(&data, page_size, first, last);
        if chain.is_empty() {
            return Err(ReadError::Format("broken playlist table chain"));
        }
        // The first page is normally the table's sentinel ("strange") page,
        // which stays; a chain that starts on a data page has no sentinel and
        // every page is reusable.
        let first_flags = data
            .get(chain[0] as usize * page_size + 0x1B)
            .copied()
            .unwrap_or(0);
        let (keep_first, reusable) = if first_flags & 0x40 != 0 {
            (Some(chain[0]), chain[1..].to_vec())
        } else {
            (None, chain)
        };
        let need = chunks.len();
        let fresh: Vec<u32> = (reusable.len()..need)
            .map(|i| next_free + (i - reusable.len()) as u32)
            .collect();
        next_free += fresh.len() as u32;
        tables.push(TablePlan {
            slot,
            ty,
            keep_first,
            reusable,
            chunks,
            fresh,
        });
    }
    let total = next_free;
    if data.len() < total as usize * page_size {
        data.resize(total as usize * page_size, 0);
    }

    // Second pass: write pages, relink chains, update the directory.
    for t in &tables {
        let need = t.chunks.len();
        let used: Vec<u32> = t
            .reusable
            .iter()
            .copied()
            .take(need)
            .chain(t.fresh.iter().copied())
            .collect();
        for (i, chunk) in t.chunks.iter().enumerate() {
            let idx = used[i];
            let next = if i + 1 < need { used[i + 1] } else { total };
            let page = pdbw::data_page(idx, t.ty, next, chunk);
            let o = idx as usize * page_size;
            data[o..o + page_size].copy_from_slice(&page);
        }
        // Pages the shrunken table no longer needs: blank them (zero rows) so
        // a reader that overruns the chain finds nothing, not stale rows.
        for &idx in t.reusable.iter().skip(need) {
            let page = pdbw::data_page(idx, t.ty, total, &[]);
            let o = idx as usize * page_size;
            data[o..o + page_size].copy_from_slice(&page);
        }
        // Relink the kept sentinel to the first data page (or a blanked one,
        // mirroring how rekordbox leaves an empty table).
        let first_data = used.first().copied().or_else(|| t.reusable.first().copied());
        if let (Some(sentinel), Some(first_data)) = (t.keep_first, first_data) {
            let o = sentinel as usize * page_size + 0x0C;
            data[o..o + 4].copy_from_slice(&first_data.to_le_bytes());
        }
        // Directory: `first` is unchanged; `last` is the final live page (the
        // sentinel again when the table emptied); `empty_candidate` may point
        // at any allocatable page — one past the end, like the full writer.
        let base = 0x1C + t.slot * 16;
        let last = used
            .last()
            .copied()
            .or(t.keep_first)
            .or_else(|| t.reusable.first().copied())
            .unwrap_or(total);
        data[base + 4..base + 8].copy_from_slice(&total.to_le_bytes());
        data[base + 12..base + 16].copy_from_slice(&last.to_le_bytes());
    }

    // Header: next_unused_page covers the appended pages; the sequence number
    // records that the database changed, as rekordbox does on every write.
    let seq = u32_at(&data, 0x14).unwrap_or(0).wrapping_add(1);
    data[0x0C..0x10].copy_from_slice(&total.to_le_bytes());
    data[0x14..0x18].copy_from_slice(&seq.to_le_bytes());

    // Atomic replace: a yanked stick mid-write leaves either the old database
    // or the new one, never a torn page.
    let tmp = pdb_path.with_extension("pdb.tmp");
    crate::export::write_synced(&tmp, &data).map_err(io_err)?;
    std::fs::rename(&tmp, pdb_path).map_err(io_err)?;
    Ok(())
}

/// Follow one table's page chain from `first` to `last`, returning page
/// indices. Same defensive walk as the reader: cycles and out-of-file links
/// end the walk.
fn walk_chain(data: &[u8], page_size: usize, first: u32, last: u32) -> Vec<u32> {
    let max_pages = data.len() / page_size + 1;
    let mut pages = Vec::new();
    let mut index = first;
    for _ in 0..max_pages {
        let off = index as usize * page_size;
        if off + page_size > data.len() {
            break;
        }
        if u32_at(data, off) != Some(0) || u32_at(data, off + 4) != Some(index) {
            break;
        }
        if pages.contains(&index) {
            break;
        }
        pages.push(index);
        if index == last {
            break;
        }
        match u32_at(data, off + 0x0C) {
            Some(next) if next != index => index = next,
            _ => break,
        }
    }
    pages
}

fn u32_at(data: &[u8], pos: usize) -> Option<u32> {
    data.get(pos..pos + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

// ---------------------------------------------------------------------------
// exportLibrary.db sync
// ---------------------------------------------------------------------------

/// Replace the DLP database's `playlist` / `playlist_content` tables with the
/// export's current tree. Content ids are the DLP's own — resolved by file
/// path, the only key shared with the pdb; entries whose path the DLP doesn't
/// know are skipped rather than failing the edit.
///
/// SQLite cannot write in place on macOS's msdos (FAT32) driver (see
/// [`crate::dlp::write_library`]), so the database is copied to local disk,
/// edited there, and copied back whole.
fn sync_dlp_playlists(db_path: &Path, export: &RbExport) -> Result<(), ReadError> {
    let err_io = |e: std::io::Error| ReadError::Dlp(e.to_string());
    let tmp = crate::dlp::scratch_db_path("ordnung-dlp-sync");
    let _ = std::fs::remove_file(&tmp);
    std::fs::copy(db_path, &tmp).map_err(err_io)?;
    let result = sync_dlp_playlists_at(&tmp, export);
    if result.is_ok() {
        std::fs::copy(&tmp, db_path).map_err(err_io)?;
        crate::export::sync_existing(db_path).map_err(err_io)?;
    }
    let _ = std::fs::remove_file(&tmp);
    result
}

/// The actual sync, run against a database on a journal-friendly filesystem.
fn sync_dlp_playlists_at(db_path: &Path, export: &RbExport) -> Result<(), ReadError> {
    let err = |e: rusqlite::Error| ReadError::Dlp(e.to_string());
    let conn = rusqlite::Connection::open(db_path).map_err(err)?;
    conn.execute_batch(&format!(
        "PRAGMA key = '{}'; PRAGMA cipher_compatibility = 4;",
        crate::dlp::DLP_KEY
    ))
    .map_err(err)?;

    let mut content_by_path: HashMap<String, i64> = HashMap::new();
    {
        let mut stmt = conn
            .prepare("SELECT content_id, path FROM content")
            .map_err(err)?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))
            })
            .map_err(err)?;
        for (id, path) in rows.flatten() {
            content_by_path.insert(path.trim_start_matches('/').to_lowercase(), id);
        }
    }

    conn.execute_batch("BEGIN").map_err(err)?;
    conn.execute("DELETE FROM playlist", []).map_err(err)?;
    conn.execute("DELETE FROM playlist_content", [])
        .map_err(err)?;
    for p in &export.playlists {
        conn.execute(
            "INSERT INTO playlist VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
            rusqlite::params![
                p.id as i64,
                p.sort_order as i64,
                p.name,
                p.is_folder as i64,
                p.parent_id as i64,
            ],
        )
        .map_err(err)?;
    }
    for p in export.playlists.iter().filter(|p| !p.is_folder) {
        let Some(tracks) = export.entries.get(&p.id) else {
            continue;
        };
        for (i, tid) in tracks.iter().enumerate() {
            let Some(track) = export.tracks.get(tid) else {
                continue;
            };
            let key = track.file_path.trim_start_matches('/').to_lowercase();
            let Some(content_id) = content_by_path.get(&key) else {
                continue;
            };
            conn.execute(
                "INSERT INTO playlist_content VALUES (?1, ?2, ?3)",
                rusqlite::params![p.id as i64, content_id, i as i64],
            )
            .map_err(err)?;
        }
    }
    conn.execute_batch("COMMIT").map_err(err)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anlz::AnlzInput;
    use crate::pdb::read_export;

    fn temp_root(tag: &str) -> std::path::PathBuf {
        let root = std::env::temp_dir().join(format!("ordnung-edit-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("PIONEER").join("rekordbox")).unwrap();
        root
    }

    fn beats(bpm: f32) -> Vec<Beat> {
        (0..8)
            .map(|i| Beat {
                number: (i % 4) + 1,
                position_ms: 100 + i as u64 * 500,
                bpm,
            })
            .collect()
    }

    #[test]
    fn grid_edit_patches_only_that_row_s_tempo() {
        let root = temp_root("grid");
        let pdb = root.join("PIONEER/rekordbox/export.pdb");
        std::fs::copy(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo_tracks_export.pdb"),
            &pdb,
        )
        .unwrap();
        let before = read_export(&pdb).unwrap();
        let (&id, _) = before.tracks.iter().next().unwrap();

        let old = beats(120.0);
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &old,
            duration_ms: 4_100,
            preview: &[128; 400],
            bands: &vec![64; 4 * 82],
            scroll: &[],
            cues: &[],
        };
        let dat = root.join("PIONEER/USBANLZ/P001/00000001/ANLZ0000.DAT");
        std::fs::create_dir_all(dat.parent().unwrap()).unwrap();
        std::fs::write(&dat, crate::anlz::build_dat(&inp)).unwrap();
        std::fs::write(dat.with_extension("EXT"), crate::anlz::build_ext(&inp)).unwrap();

        write_stick_beatgrid(&root, id, &dat, &beats(133.37)).unwrap();

        let after = read_export(&pdb).unwrap();
        assert_eq!(after.tracks[&id].tempo_centi_bpm, 13337);
        for (tid, t) in &before.tracks {
            if *tid != id {
                assert_eq!(&after.tracks[tid], t, "other rows must not change");
            }
        }
        assert_eq!(after.playlists, before.playlists);
        // Pristine copies exist, and the original grid reads back from them.
        assert!(pdb.with_extension("pdb.orig").is_file());
        assert!(dat.with_extension("DAT.orig").is_file());
        assert!(dat.with_extension("EXT.orig").is_file());
        assert_eq!(original_stick_beatgrid(&dat).unwrap(), old);
        assert_eq!(crate::anlz::read_beatgrid(&dat)[0].bpm, 133.37);

        // A second edit keeps the first backup.
        write_stick_beatgrid(&root, id, &dat, &beats(140.0)).unwrap();
        assert_eq!(original_stick_beatgrid(&dat).unwrap(), old);
        assert_eq!(read_export(&pdb).unwrap().tracks[&id].tempo_centi_bpm, 14000);

        assert!(matches!(
            write_stick_beatgrid(&root, 999_999, &dat, &beats(1.0)),
            Err(ReadError::Format("no such track row"))
        ));
    }

    #[test]
    fn cue_edit_lands_in_both_files_and_backs_up_once() {
        let root = temp_root("cues");
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &[],
            duration_ms: 4_100,
            preview: &[],
            bands: &[],
            scroll: &[],
            cues: &[],
        };
        let dat = root.join("ANLZ0000.DAT");
        std::fs::write(&dat, crate::anlz::build_dat(&inp)).unwrap();
        std::fs::write(dat.with_extension("EXT"), crate::anlz::build_ext(&inp)).unwrap();
        let pristine = std::fs::read(&dat).unwrap();
        let cue = Cue {
            hot_slot: Some(1),
            position_ms: 2_000,
            loop_end_ms: None,
            label: Some("B".into()),
            color: Some([1, 2, 3]),
        };
        write_stick_cues(&dat, std::slice::from_ref(&cue)).unwrap();
        assert_eq!(crate::anlz::read_cues(&dat), vec![cue.clone()]);
        assert_eq!(std::fs::read(dat.with_extension("DAT.orig")).unwrap(), pristine);
        write_stick_cues(&dat, &[]).unwrap();
        assert!(crate::anlz::read_cues(&dat).is_empty());
        assert_eq!(std::fs::read(dat.with_extension("DAT.orig")).unwrap(), pristine);
    }
}
