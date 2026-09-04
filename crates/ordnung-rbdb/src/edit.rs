//! Playlist editing on an existing rekordbox stick.
//!
//! The export command rebuilds a whole stick; this module instead applies one
//! user gesture — create, rename, delete a playlist, or add tracks to one —
//! to whatever export is already mounted, touching *only* the playlist data:
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
//! Audio files, ANLZ analysis, and every non-playlist table are never
//! touched. Before the first edit ever made to a stick, the pristine
//! databases are copied to `*.orig` alongside themselves, so the pre-Ordnung
//! state is always recoverable.

use std::collections::HashMap;
use std::path::Path;

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
    std::fs::write(&tmp, &data).map_err(io_err)?;
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
