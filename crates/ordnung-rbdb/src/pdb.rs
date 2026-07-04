//! DeviceSQL `export.pdb` — read side.
//!
//! A minimal, self-contained parser for the parts of a rekordbox export the
//! app consumes today: the playlist tree, playlist entries, and each track's
//! id → file path mapping. Field offsets follow the Deep Symmetry reverse
//! engineering (<https://djl-analysis.deepsymmetry.org/>) and were validated
//! against rekordcrate's parser on real exports (see `tests/pdb_read.rs`).
//!
//! The writer half (building the full track/artist/…/playlist tables CDJs
//! read) is Phase 5 and still to come; see the `rekordbox-format` skill.
//!
//! Everything here is defensive: a corrupt or truncated database returns an
//! error or simply yields fewer rows — it must never panic, since the GUI
//! points this at whatever `export.pdb` happens to sit on a mounted stick.

use std::collections::HashMap;
use std::path::Path;

/// Reading `export.pdb` failed outright. Structural oddities *within* a page
/// (bad row offset, malformed string) skip that row instead of erroring.
#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("couldn't read {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("not a DeviceSQL export.pdb: {0}")]
    Format(&'static str),
}

/// One node of the export's playlist tree — a playlist or a folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RbPlaylist {
    pub id: u32,
    /// Parent folder id; `0` = top level.
    pub parent_id: u32,
    /// Sibling sort position, as shown on the player.
    pub sort_order: u32,
    pub is_folder: bool,
    pub name: String,
}

/// The slice of an `export.pdb` the app reads: playlists and which files (in
/// order) each contains. Track metadata itself comes from the audio files.
#[derive(Debug, Default, Clone)]
pub struct RbExport {
    /// Every playlist-tree node, sorted by `sort_order` (group by `parent_id`
    /// to render the tree).
    pub playlists: Vec<RbPlaylist>,
    /// Playlist id → track ids in playlist order.
    pub entries: HashMap<u32, Vec<u32>>,
    /// Track id → file path as stored in the export (e.g.
    /// `/Contents/Artist/Album/track.mp3`, absolute from the volume root).
    pub track_paths: HashMap<u32, String>,
}

// Table/page type ids (DeviceSQL `PageType`).
const TYPE_TRACKS: u32 = 0;
const TYPE_PLAYLIST_TREE: u32 = 7;
const TYPE_PLAYLIST_ENTRIES: u32 = 8;

/// Byte size of a page header; the row heap starts right after it.
const PAGE_HEADER: usize = 0x28;
/// Byte size of one row group in the page footer: 16 u16 row offsets, a u16
/// presence bitmask, and a u16 of padding.
const ROW_GROUP: usize = 36;

/// Parse the export database at `pdb_path` (the `PIONEER/rekordbox/export.pdb`
/// file on a stick).
pub fn read_export(pdb_path: &Path) -> Result<RbExport, ReadError> {
    let data = std::fs::read(pdb_path).map_err(|source| ReadError::Io {
        path: pdb_path.to_path_buf(),
        source,
    })?;
    parse_export(&data)
}

fn parse_export(data: &[u8]) -> Result<RbExport, ReadError> {
    // Header: u32 0, page_size, num_tables, next_unused_page, unknown,
    // sequence, u32 0, then `num_tables` 16-byte table pointers.
    if u32_at(data, 0) != Some(0) {
        return Err(ReadError::Format("bad signature"));
    }
    let page_size = u32_at(data, 4).ok_or(ReadError::Format("truncated header"))? as usize;
    if !(512..=65536).contains(&page_size) {
        return Err(ReadError::Format("implausible page size"));
    }
    let num_tables = u32_at(data, 8).ok_or(ReadError::Format("truncated header"))? as usize;
    if num_tables > 64 {
        return Err(ReadError::Format("implausible table count"));
    }

    let mut out = RbExport::default();
    // playlist id → (entry_index, track_id), sorted after collection.
    let mut raw_entries: HashMap<u32, Vec<(u32, u32)>> = HashMap::new();

    for t in 0..num_tables {
        let base = 0x1C + t * 16;
        let Some(page_type) = u32_at(data, base) else {
            break;
        };
        if !matches!(
            page_type,
            TYPE_TRACKS | TYPE_PLAYLIST_TREE | TYPE_PLAYLIST_ENTRIES
        ) {
            continue;
        }
        let Some(first_page) = u32_at(data, base + 8) else {
            break;
        };
        let Some(last_page) = u32_at(data, base + 12) else {
            break;
        };
        for page_off in table_pages(data, page_size, first_page, last_page) {
            for row in page_rows(data, page_size, page_off, page_type) {
                match page_type {
                    TYPE_TRACKS => {
                        // Track row: id u32 @0x48; 21 string-offset u16s start
                        // @0x5E, file_path is the last (@0x86), relative to
                        // the row start.
                        let (Some(id), Some(rel)) =
                            (u32_at(data, row + 0x48), u16_at(data, row + 0x86))
                        else {
                            continue;
                        };
                        if let Some(path) = dsql_string(data, row + rel as usize) {
                            out.track_paths.insert(id, path);
                        }
                    }
                    TYPE_PLAYLIST_TREE => {
                        // parent u32 @0, sort_order u32 @8, id u32 @12,
                        // is_folder u32 @16 (non-zero = folder), inline name @20.
                        let (Some(parent_id), Some(sort_order), Some(id), Some(folder)) = (
                            u32_at(data, row),
                            u32_at(data, row + 8),
                            u32_at(data, row + 12),
                            u32_at(data, row + 16),
                        ) else {
                            continue;
                        };
                        let Some(name) = dsql_string(data, row + 20) else {
                            continue;
                        };
                        out.playlists.push(RbPlaylist {
                            id,
                            parent_id,
                            sort_order,
                            is_folder: folder != 0,
                            name,
                        });
                    }
                    TYPE_PLAYLIST_ENTRIES => {
                        // entry_index u32 @0, track_id u32 @4, playlist_id u32 @8.
                        let (Some(idx), Some(track), Some(playlist)) = (
                            u32_at(data, row),
                            u32_at(data, row + 4),
                            u32_at(data, row + 8),
                        ) else {
                            continue;
                        };
                        raw_entries.entry(playlist).or_default().push((idx, track));
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    out.playlists.sort_by_key(|p| p.sort_order);
    for (playlist, mut list) in raw_entries {
        list.sort_by_key(|(idx, _)| *idx);
        out.entries
            .insert(playlist, list.into_iter().map(|(_, track)| track).collect());
    }
    Ok(out)
}

/// Follow one table's linked list of pages, returning each page's byte offset.
/// Cycles and out-of-file links terminate the walk instead of hanging it.
fn table_pages(data: &[u8], page_size: usize, first: u32, last: u32) -> Vec<usize> {
    let max_pages = data.len() / page_size + 1;
    let mut pages = Vec::new();
    let mut index = first;
    for _ in 0..max_pages {
        let off = index as usize * page_size;
        if off + page_size > data.len() {
            break;
        }
        // Page header sanity: leading u32 is 0 and the stored index matches.
        if u32_at(data, off) != Some(0) || u32_at(data, off + 4) != Some(index) {
            break;
        }
        pages.push(off);
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

/// Yield the absolute byte offset of every present row on a data page. Row
/// offsets live in 36-byte groups at the page's end: 16 u16 offsets (relative
/// to the heap at `page + 0x28`), then a presence bitmask.
fn page_rows(data: &[u8], page_size: usize, page_off: usize, want_type: u32) -> Vec<usize> {
    let mut rows = Vec::new();
    if u32_at(data, page_off + 8) != Some(want_type) {
        return rows;
    }
    let Some(flags) = data.get(page_off + 0x1B) else {
        return rows;
    };
    // flags & 0x40 set marks a strange/empty page with no row data.
    if flags & 0x40 != 0 {
        return rows;
    }
    let small = data.get(page_off + 0x18).copied().unwrap_or(0) as u16;
    let large = u16_at(data, page_off + 0x22).unwrap_or(0);
    let num_rows = if large > small && large != 0x1FFF {
        large
    } else {
        small
    };
    if num_rows == 0 {
        return rows;
    }
    let groups = (num_rows as usize).div_ceil(16);
    for g in 0..groups {
        // Group g's footer block ends 36*g bytes above the page end.
        let end = page_off + page_size - ROW_GROUP * g;
        let Some(present) = u16_at(data, end.wrapping_sub(4)) else {
            continue;
        };
        for slot in 0..16 {
            if present & (1 << slot) == 0 {
                continue;
            }
            let Some(rel) = u16_at(data, end.wrapping_sub(4 + 2 * (slot + 1))) else {
                continue;
            };
            let row = page_off + PAGE_HEADER + rel as usize;
            // A row must start inside this page's heap.
            if row < page_off + page_size {
                rows.push(row);
            }
        }
    }
    rows
}

/// Decode a DeviceSQLString at `pos`. Two forms: short ASCII (odd first byte:
/// `(len+1)*2+1` header, bytes follow) and long (`0x40` ASCII / `0x90`
/// UTF-16LE or ISRC, u16 total length including the 4-byte header).
/// Malformed → `None`.
fn dsql_string(data: &[u8], pos: usize) -> Option<String> {
    let b0 = *data.get(pos)?;
    if b0 & 1 == 1 {
        let len = ((b0 >> 1) as usize).checked_sub(1)?;
        let body = data.get(pos + 1..pos + 1 + len)?;
        return Some(String::from_utf8_lossy(body).into_owned());
    }
    let total = u16_at(data, pos + 1)? as usize;
    if total < 4 {
        return None;
    }
    let body = data.get(pos + 4..pos + total)?;
    match b0 {
        0x40 => Some(String::from_utf8_lossy(body).into_owned()),
        0x90 => {
            if body.first() == Some(&0x03) {
                // ISRC quirk: 0x03 magic then a NUL-terminated ASCII string.
                let s = body[1..].split(|&b| b == 0).next().unwrap_or(&[]);
                Some(String::from_utf8_lossy(s).into_owned())
            } else {
                let units: Vec<u16> = body
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Some(String::from_utf16_lossy(&units))
            }
        }
        _ => None,
    }
}

fn u16_at(data: &[u8], pos: usize) -> Option<u16> {
    data.get(pos..pos + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
}

fn u32_at(data: &[u8], pos: usize) -> Option<u32> {
    data.get(pos..pos + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}
