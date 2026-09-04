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
    #[error("device library plus: {0}")]
    Dlp(String),
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

/// One track row of the export — the full read-side slice: identity, location,
/// the browse metadata a player lists (title/artist/album/genre, duration,
/// bitrate) and the analysis summary it shows (tempo and key). This is what
/// lets a stick open the way it does on a CDJ: everything below comes out of
/// `export.pdb` alone, without decoding a single audio file.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RbTrack {
    /// File path as stored in the export (e.g. `/Contents/Artist/track.mp3`,
    /// absolute from the volume root).
    pub file_path: String,
    /// Tempo as stored: BPM × 100. `0` means never analyzed.
    pub tempo_centi_bpm: u32,
    /// Key name resolved through the Keys table (e.g. "Am" or "8A" — whatever
    /// notation rekordbox exported). `None` when unanalyzed or unresolvable.
    pub key: Option<String>,
    /// Title string from the row; empty when absent.
    pub title: String,
    /// Artist name resolved through the Artists table.
    pub artist: Option<String>,
    /// Album name resolved through the Albums table.
    pub album: Option<String>,
    /// Genre name resolved through the Genres table.
    pub genre: Option<String>,
    /// Comment string from the row; empty when absent.
    pub comment: String,
    /// Cover-art image file resolved through the Artwork table — a small JPEG
    /// under `/PIONEER/ARTWORK/…`, absolute from the volume root.
    pub artwork_path: Option<String>,
    /// Duration in whole seconds, as stored.
    pub duration_s: u16,
    /// Bitrate in kbps, as stored. `0` when unknown.
    pub bitrate_kbps: u32,
    /// Sample rate in Hz, as stored. `0` when unknown.
    pub sample_rate_hz: u32,
    /// Release year, as stored. `0` when unknown.
    pub year: u16,
    /// The track's ANLZ analysis file (beatgrid, cues, waveforms), as stored —
    /// e.g. `/PIONEER/USBANLZ/P016/0000B5/ANLZ0000.DAT`. Not parsed here yet;
    /// carried so a caller (or the Phase 5 round-trip) can find it.
    pub analyze_path: Option<String>,
}

impl RbTrack {
    /// Tempo in BPM, filtered to plausible values; `None` when unanalyzed.
    pub fn bpm(&self) -> Option<f32> {
        (self.tempo_centi_bpm > 0 && self.tempo_centi_bpm < 60_000)
            .then_some(self.tempo_centi_bpm as f32 / 100.0)
    }
}

/// The slice of an `export.pdb` the app reads: playlists, which files (in
/// order) each contains, and each track's location + player-shown analysis
/// summary. Everything else about a track still comes from the audio file.
#[derive(Debug, Default, Clone)]
pub struct RbExport {
    /// Every playlist-tree node, sorted by `sort_order` (group by `parent_id`
    /// to render the tree).
    pub playlists: Vec<RbPlaylist>,
    /// Playlist id → track ids in playlist order.
    pub entries: HashMap<u32, Vec<u32>>,
    /// Track id → the row's read-side slice (path, tempo, key, title).
    pub tracks: HashMap<u32, RbTrack>,
}

// Table/page type ids (DeviceSQL `PageType`).
const TYPE_TRACKS: u32 = 0;
const TYPE_GENRES: u32 = 1;
const TYPE_ARTISTS: u32 = 2;
const TYPE_ALBUMS: u32 = 3;
const TYPE_KEYS: u32 = 5;
const TYPE_PLAYLIST_TREE: u32 = 7;
const TYPE_PLAYLIST_ENTRIES: u32 = 8;
const TYPE_ARTWORK: u32 = 13;

/// Byte size of a page header; the row heap starts right after it.
const PAGE_HEADER: usize = 0x28;
/// Byte size of one row group in the page footer: 16 u16 row offsets, a u16
/// presence bitmask, and a u16 of padding.
const ROW_GROUP: usize = 36;

/// Read everything the app needs from a mounted rekordbox stick, given the
/// volume root: the `export.pdb` tables, and — when the export is a modern
/// (rekordbox 6/7) one whose pdb playlist tables are empty — the playlists
/// out of the encrypted `exportLibrary.db` beside it (see [`crate::dlp`]),
/// joined back onto pdb track ids by file path (case-insensitively; the
/// volume is FAT32). Prefer this over [`read_export`] for anything
/// user-facing, or a modern stick shows a library with no playlists.
pub fn read_stick(volume_root: &Path) -> Result<RbExport, ReadError> {
    let dir = volume_root.join("PIONEER").join("rekordbox");
    let mut export = read_export(&dir.join("export.pdb"))?;
    if !export.playlists.is_empty() {
        return Ok(export);
    }
    let dlp_db = dir.join("exportLibrary.db");
    if !dlp_db.is_file() {
        return Ok(export);
    }
    // A failure here (foreign key, future schema change) degrades to the
    // pdb-only view rather than failing the whole stick.
    let Ok(dlp) = crate::dlp::read_playlists(&dlp_db) else {
        return Ok(export);
    };
    let by_path: HashMap<String, u32> = export
        .tracks
        .iter()
        .map(|(id, t)| (t.file_path.to_lowercase(), *id))
        .collect();
    export.entries = dlp
        .entries_by_path
        .into_iter()
        .map(|(playlist, paths)| {
            let ids: Vec<u32> = paths
                .iter()
                .filter_map(|p| by_path.get(&p.to_lowercase()).copied())
                .collect();
            (playlist, ids)
        })
        .collect();
    export.playlists = dlp.playlists;
    Ok(export)
}

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
    // Name resolution is a post-pass: the interned-name tables (keys, artists,
    // albums, genres, artwork) may come after Tracks in the table list, so
    // rows carry their *ids* until every table is read.
    let mut key_names: HashMap<u32, String> = HashMap::new();
    let mut artist_names: HashMap<u32, String> = HashMap::new();
    let mut album_names: HashMap<u32, String> = HashMap::new();
    let mut genre_names: HashMap<u32, String> = HashMap::new();
    let mut artwork_paths: HashMap<u32, String> = HashMap::new();
    // track id → (key_id, artist_id, album_id, genre_id, artwork_id).
    let mut track_refs: HashMap<u32, [u32; 5]> = HashMap::new();

    for t in 0..num_tables {
        let base = 0x1C + t * 16;
        let Some(page_type) = u32_at(data, base) else {
            break;
        };
        if !matches!(
            page_type,
            TYPE_TRACKS
                | TYPE_GENRES
                | TYPE_ARTISTS
                | TYPE_ALBUMS
                | TYPE_KEYS
                | TYPE_PLAYLIST_TREE
                | TYPE_PLAYLIST_ENTRIES
                | TYPE_ARTWORK
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
                        // Track row (Deep Symmetry layout): key_id u32 @0x20,
                        // tempo (BPM×100) u32 @0x38, id u32 @0x48; then 21
                        // string-offset u16s from @0x5E — analyze_path is
                        // index 14 (@0x7A), title index 17 (@0x80), file_path
                        // the last (@0x86) — each relative to the row start.
                        let (Some(id), Some(rel)) =
                            (u32_at(data, row + 0x48), u16_at(data, row + 0x86))
                        else {
                            continue;
                        };
                        let Some(file_path) = dsql_string(data, row + rel as usize) else {
                            continue;
                        };
                        track_refs.insert(
                            id,
                            [
                                u32_at(data, row + 0x20).unwrap_or(0), // key
                                u32_at(data, row + 0x44).unwrap_or(0), // artist
                                u32_at(data, row + 0x40).unwrap_or(0), // album
                                u32_at(data, row + 0x3C).unwrap_or(0), // genre
                                u32_at(data, row + 0x1C).unwrap_or(0), // artwork
                            ],
                        );
                        let string_at = |idx: usize| {
                            u16_at(data, row + 0x5E + 2 * idx)
                                .and_then(|rel| dsql_string(data, row + rel as usize))
                        };
                        out.tracks.insert(
                            id,
                            RbTrack {
                                file_path,
                                tempo_centi_bpm: u32_at(data, row + 0x38).unwrap_or(0),
                                key: None,
                                title: string_at(17).unwrap_or_default(),
                                artist: None,
                                album: None,
                                genre: None,
                                comment: string_at(16).unwrap_or_default(),
                                artwork_path: None,
                                duration_s: u16_at(data, row + 0x54).unwrap_or(0),
                                bitrate_kbps: u32_at(data, row + 0x30).unwrap_or(0),
                                sample_rate_hz: u32_at(data, row + 0x08).unwrap_or(0),
                                year: u16_at(data, row + 0x50).unwrap_or(0),
                                analyze_path: string_at(14).filter(|s| !s.is_empty()),
                            },
                        );
                    }
                    TYPE_KEYS => {
                        // Key row: id u32 @0, id2 u32 @4 (same), name @8.
                        let Some(id) = u32_at(data, row) else {
                            continue;
                        };
                        if let Some(name) = dsql_string(data, row + 8) {
                            key_names.insert(id, name);
                        }
                    }
                    TYPE_GENRES => {
                        // Genre row: id u32 @0, name @4.
                        let Some(id) = u32_at(data, row) else {
                            continue;
                        };
                        if let Some(name) = dsql_string(data, row + 4) {
                            genre_names.insert(id, name);
                        }
                    }
                    TYPE_ARTISTS => {
                        // Artist row: subtype u16 @0 (0x60 near / 0x64 far),
                        // index_shift u16 @2, id u32 @4, unknown u8 @8, name
                        // offset u8 @9 — or u16 @0xA when subtype is 0x64 and
                        // the name sits beyond a u8's reach.
                        let (Some(subtype), Some(id)) = (u16_at(data, row), u32_at(data, row + 4))
                        else {
                            continue;
                        };
                        let ofs = if subtype == 0x64 {
                            u16_at(data, row + 0x0A).map(|o| o as usize)
                        } else {
                            data.get(row + 9).map(|o| *o as usize)
                        };
                        if let Some(name) = ofs.and_then(|o| dsql_string(data, row + o)) {
                            artist_names.insert(id, name);
                        }
                    }
                    TYPE_ALBUMS => {
                        // Album row: subtype u16 @0, index_shift u16 @2,
                        // unknown u32 @4, artist_id u32 @8, id u32 @0xC,
                        // unknown u32 @0x10, unknown u8 @0x14, name offset
                        // u8 @0x15.
                        let (Some(id), Some(ofs)) = (u32_at(data, row + 0x0C), data.get(row + 0x15))
                        else {
                            continue;
                        };
                        if let Some(name) = dsql_string(data, row + *ofs as usize) {
                            album_names.insert(id, name);
                        }
                    }
                    TYPE_ARTWORK => {
                        // Artwork row: id u32 @0, image path @4.
                        let Some(id) = u32_at(data, row) else {
                            continue;
                        };
                        if let Some(path) = dsql_string(data, row + 4) {
                            artwork_paths.insert(id, path);
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

    // Resolve interned ids into names now that every table is read. A
    // dangling or zero id simply leaves the field empty — same posture as
    // every other bad row. Empty interned names count as absent, so a track
    // with no artist reads as `None` rather than `Some("")`.
    let lookup = |map: &HashMap<u32, String>, id: u32| {
        (id != 0)
            .then(|| map.get(&id))
            .flatten()
            .filter(|s| !s.is_empty())
            .cloned()
    };
    for (track_id, [key, artist, album, genre, artwork]) in track_refs {
        if let Some(track) = out.tracks.get_mut(&track_id) {
            track.key = lookup(&key_names, key);
            track.artist = lookup(&artist_names, artist);
            track.album = lookup(&album_names, album);
            track.genre = lookup(&genre_names, genre);
            track.artwork_path = lookup(&artwork_paths, artwork);
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
