//! DeviceSQL `export.pdb` — write side.
//!
//! Serializes a full 20-table export database byte-for-byte in the shape
//! rekordbox 7 writes (validated against the EYEBAGS golden reference; see
//! `docs/rekordbox-export-structure.md` for every field's provenance). The
//! write is "one-shot": every page carries the one-shot bookkeeping signature
//! rekordbox uses for tables it writes in a single pass.
//!
//! Layout contract (mirrors rekordbox exactly):
//! * page 0 = file header + table directory, 20 tables in fixed type order;
//! * table *t* gets a sentinel page at index `2t+1` (flags 0x64, no rows) and
//!   its first data page at `2t+2`; overflow data pages are appended from
//!   page 41 on, chained via `next_page`;
//! * an empty table keeps its (unlinked) blank even page, with the directory's
//!   `first == last == sentinel` — exactly how rekordbox 7 leaves the
//!   playlist tables.

use std::collections::HashMap;

pub(crate) const PAGE: usize = 4096;
const PAGE_HEADER: usize = 0x28;
/// Row-index cost at the page tail: 4 bytes per group of 16 + 2 per row slot.
fn index_bytes(rows: usize) -> usize {
    rows.div_ceil(16) * 4 + rows * 2
}

// ---------------------------------------------------------------------------
// DeviceSQLString
// ---------------------------------------------------------------------------

/// Encode a DeviceSQLString: short ASCII when it fits (≤126 chars), long
/// ASCII (0x40) when pure-ASCII but longer, UTF-16LE (0x90) otherwise.
pub(crate) fn dsql_string(s: &str) -> Vec<u8> {
    if s.is_ascii() && s.len() <= 126 {
        let mut v = Vec::with_capacity(1 + s.len());
        v.push((((s.len() + 1) as u8) << 1) | 1);
        v.extend_from_slice(s.as_bytes());
        v
    } else if s.is_ascii() {
        let total = (s.len() + 4) as u16;
        let mut v = Vec::with_capacity(total as usize);
        v.push(0x40);
        v.extend_from_slice(&total.to_le_bytes());
        v.push(0);
        v.extend_from_slice(s.as_bytes());
        v
    } else {
        let units: Vec<u16> = s.encode_utf16().collect();
        let total = (units.len() * 2 + 4) as u16;
        let mut v = Vec::with_capacity(total as usize);
        v.push(0x90);
        v.extend_from_slice(&total.to_le_bytes());
        v.push(0);
        for u in units {
            v.extend_from_slice(&u.to_le_bytes());
        }
        v
    }
}

/// The ISRC oddity: a 0x90 long string whose body is `0x03` + ASCII + NUL.
fn dsql_isrc(s: &str) -> Vec<u8> {
    if s.is_empty() {
        return dsql_string("");
    }
    let total = (4 + 1 + s.len() + 1) as u16;
    let mut v = Vec::with_capacity(total as usize);
    v.push(0x90);
    v.extend_from_slice(&total.to_le_bytes());
    v.push(0);
    v.push(0x03);
    v.extend_from_slice(s.as_bytes());
    v.push(0);
    v
}

// ---------------------------------------------------------------------------
// Input shape
// ---------------------------------------------------------------------------

/// One track row, fully resolved: interned ids assigned, strings final.
/// Field meanings follow `docs/rekordbox-export-structure.md` §2.5.
#[derive(Debug, Clone, Default)]
pub(crate) struct TrackRow {
    pub id: u32,
    pub sample_rate_hz: u32,
    pub file_size: u32,
    /// rekordbox-style random content id (constant 28-bit); any nonzero value.
    pub master_content_id: u32,
    pub artwork_id: u32,
    pub key_id: u32,
    pub label_id: u32,
    pub bitrate_kbps: u32,
    pub track_number: u32,
    pub tempo_centi_bpm: u32,
    pub genre_id: u32,
    pub album_id: u32,
    pub artist_id: u32,
    pub disc_number: u16,
    pub year: u16,
    pub sample_depth: u16,
    pub duration_s: u16,
    /// File type enum: 1 mp3, 4 m4a/aac, 5 flac, 11 wav, 12 aiff, 0 unknown.
    pub file_type: u16,
    pub rating: u8,
    pub isrc: String,
    pub date_added: String,
    pub analyze_path: String,
    pub analyze_date: String,
    pub comment: String,
    pub title: String,
    pub filename: String,
    pub file_path: String,
}

#[derive(Debug, Clone)]
pub(crate) struct PlaylistRow {
    pub id: u32,
    pub parent_id: u32,
    pub sort_order: u32,
    pub is_folder: bool,
    pub name: String,
}

/// Everything `build_export_pdb` needs. Interned tables are `(id, name)` in
/// id order; ids are 1-based and dense.
#[derive(Debug, Clone, Default)]
pub(crate) struct PdbTables {
    pub tracks: Vec<TrackRow>,
    pub genres: Vec<(u32, String)>,
    pub artists: Vec<(u32, String)>,
    pub albums: Vec<(u32, String)>,
    pub labels: Vec<(u32, String)>,
    pub keys: Vec<(u32, String)>,
    pub artwork: Vec<(u32, String)>,
    pub playlists: Vec<PlaylistRow>,
    /// `(entry_index, track_id, playlist_id)` — entry_index is 0-based within
    /// its playlist, matching rekordbox.
    pub playlist_entries: Vec<(u32, u32, u32)>,
    /// `YYYY-MM-DD` stamp for the type-19 summary row.
    pub created_date: String,
}

// ---------------------------------------------------------------------------
// Row encoders
// ---------------------------------------------------------------------------

fn track_row(t: &TrackRow, index_shift: u16) -> Vec<u8> {
    let mut r = Vec::with_capacity(0x120);
    let u16le = |r: &mut Vec<u8>, v: u16| r.extend_from_slice(&v.to_le_bytes());
    let u32le = |r: &mut Vec<u8>, v: u32| r.extend_from_slice(&v.to_le_bytes());
    u16le(&mut r, 0x0024); // magic
    u16le(&mut r, index_shift);
    u32le(&mut r, 0x000C_0700); // contentLink (constant in rekordbox 7 exports)
    u32le(&mut r, t.sample_rate_hz);
    u32le(&mut r, 0); // composer artist id
    u32le(&mut r, t.file_size);
    u32le(&mut r, t.master_content_id);
    u16le(&mut r, 0x2FDB); // constant in every golden row
    u16le(&mut r, 0x8F45); // constant in every golden row
    u32le(&mut r, t.artwork_id);
    u32le(&mut r, t.key_id);
    u32le(&mut r, 0); // original-artist id
    u32le(&mut r, t.label_id);
    u32le(&mut r, 0); // remixer id
    u32le(&mut r, t.bitrate_kbps);
    u32le(&mut r, t.track_number);
    u32le(&mut r, t.tempo_centi_bpm);
    u32le(&mut r, t.genre_id);
    u32le(&mut r, t.album_id);
    u32le(&mut r, t.artist_id);
    u32le(&mut r, t.id);
    u16le(&mut r, t.disc_number);
    u16le(&mut r, 0); // play count
    u16le(&mut r, t.year);
    u16le(&mut r, t.sample_depth);
    u16le(&mut r, t.duration_s);
    u16le(&mut r, 41); // constant in every golden row
    r.push(0); // color id
    r.push(t.rating);
    u16le(&mut r, t.file_type);
    u16le(&mut r, 3); // constant in every golden row
    debug_assert_eq!(r.len(), 0x5E);

    // 21 strings; offsets are relative to row start, strings packed after the
    // 0x88-byte header. Update-counter slots get the fresh-analysis values.
    let strings: [Vec<u8>; 21] = [
        dsql_isrc(&t.isrc),         // 0 isrc
        dsql_string(""),            // 1 texter
        dsql_string("1"),           // 2 informationUpdateCount
        dsql_string("1"),           // 3 analysisDataUpdateCount
        dsql_string(""),            // 4 cueUpdateCount
        dsql_string(""),            // 5 message
        dsql_string("ON"),          // 6 kuvo_public
        dsql_string("ON"),          // 7 autoload_hotcues
        dsql_string(""),            // 8
        dsql_string(""),            // 9
        dsql_string(&t.date_added), // 10 date_added
        dsql_string(""),            // 11 release_date
        dsql_string(""),            // 12 mix_name
        dsql_string(""),            // 13
        dsql_string(&t.analyze_path), // 14
        dsql_string(&t.analyze_date), // 15
        dsql_string(&t.comment),    // 16
        dsql_string(&t.title),      // 17
        dsql_string(""),            // 18
        dsql_string(&t.filename),   // 19
        dsql_string(&t.file_path),  // 20
    ];
    let mut off = 0x88u16;
    for s in &strings {
        u16le(&mut r, off);
        off += s.len() as u16;
    }
    debug_assert_eq!(r.len(), 0x88);
    for s in &strings {
        r.extend_from_slice(s);
    }
    r
}

fn genre_row(id: u32, name: &str) -> Vec<u8> {
    let mut r = id.to_le_bytes().to_vec();
    r.extend_from_slice(&dsql_string(name));
    r
}

fn artist_row(id: u32, name: &str, index_shift: u16) -> Vec<u8> {
    // subtype 0x60: the name offset always fits a u8 (header is 10 bytes).
    let mut r = Vec::new();
    r.extend_from_slice(&0x0060u16.to_le_bytes());
    r.extend_from_slice(&index_shift.to_le_bytes());
    r.extend_from_slice(&id.to_le_bytes());
    r.push(0x03);
    r.push(0x0A); // name offset
    r.extend_from_slice(&dsql_string(name));
    r
}

fn album_row(id: u32, name: &str, index_shift: u16) -> Vec<u8> {
    let mut r = Vec::new();
    r.extend_from_slice(&0x0080u16.to_le_bytes());
    r.extend_from_slice(&index_shift.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes()); // album-artist id (0 = unset)
    r.extend_from_slice(&id.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.push(0x03);
    r.push(0x16); // name offset
    r.extend_from_slice(&dsql_string(name));
    r
}

fn key_row(id: u32, name: &str) -> Vec<u8> {
    let mut r = id.to_le_bytes().to_vec();
    r.extend_from_slice(&id.to_le_bytes()); // id2 == id
    r.extend_from_slice(&dsql_string(name));
    r
}

fn color_row(id: u16, name: &str) -> Vec<u8> {
    let mut r = 0u32.to_le_bytes().to_vec();
    r.push(id as u8);
    r.extend_from_slice(&id.to_le_bytes());
    r.push(0);
    r.extend_from_slice(&dsql_string(name));
    r
}

fn artwork_row(id: u32, path: &str) -> Vec<u8> {
    let mut r = id.to_le_bytes().to_vec();
    r.extend_from_slice(&dsql_string(path));
    r
}

fn playlist_tree_row(p: &PlaylistRow) -> Vec<u8> {
    let mut r = p.parent_id.to_le_bytes().to_vec();
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&p.sort_order.to_le_bytes());
    r.extend_from_slice(&p.id.to_le_bytes());
    r.extend_from_slice(&(p.is_folder as u32).to_le_bytes());
    r.extend_from_slice(&dsql_string(&p.name));
    r
}

fn playlist_entry_row(entry_index: u32, track_id: u32, playlist_id: u32) -> Vec<u8> {
    let mut r = entry_index.to_le_bytes().to_vec();
    r.extend_from_slice(&track_id.to_le_bytes());
    r.extend_from_slice(&playlist_id.to_le_bytes());
    r
}

/// The type-19 export-summary row: track count, created date, dbVersion
/// "1000". The `0x19 0x1e` pair is copied verbatim from rekordbox 7 output
/// (meaning unknown; see the format doc §2.7).
fn summary_row(num_contents: u32, created_date: &str, index_shift: u16) -> Vec<u8> {
    let mut r = Vec::with_capacity(40);
    r.extend_from_slice(&0x0280u16.to_le_bytes());
    r.extend_from_slice(&index_shift.to_le_bytes());
    r.extend_from_slice(&num_contents.to_le_bytes());
    r.extend_from_slice(&0u32.to_le_bytes());
    r.extend_from_slice(&dsql_string(created_date));
    r.push(0x19);
    r.push(0x1E);
    r.extend_from_slice(&dsql_string("1000"));
    r.push(0x03);
    while r.len() < 40 {
        r.push(0);
    }
    r
}

/// The eight fixed rekordbox colors (always written, always these ids).
const COLORS: [&str; 8] = [
    "Pink", "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple",
];

// Static browse-menu tables (16 columns / 17 categories / 18 sort), copied
// byte-verbatim from the rekordbox 7.2.2 golden reference. They describe the
// player's browse menus and never vary with library content.
const MENU_COLUMNS: &[&[u8]] = &[
    b"\x01\x00\x80\x00\x90\x12\x00\x00\xfa\xff\x47\x00\x45\x00\x4e\x00\x52\x00\x45\x00\xfb\xff\x00\x00",
    b"\x02\x00\x81\x00\x90\x14\x00\x00\xfa\xff\x41\x00\x52\x00\x54\x00\x49\x00\x53\x00\x54\x00\xfb\xff",
    b"\x03\x00\x82\x00\x90\x12\x00\x00\xfa\xff\x41\x00\x4c\x00\x42\x00\x55\x00\x4d\x00\xfb\xff\x00\x00",
    b"\x04\x00\x83\x00\x90\x12\x00\x00\xfa\xff\x54\x00\x52\x00\x41\x00\x43\x00\x4b\x00\xfb\xff\x00\x00",
    b"\x05\x00\x85\x00\x90\x0e\x00\x00\xfa\xff\x42\x00\x50\x00\x4d\x00\xfb\xff\x00\x00",
    b"\x06\x00\x86\x00\x90\x14\x00\x00\xfa\xff\x52\x00\x41\x00\x54\x00\x49\x00\x4e\x00\x47\x00\xfb\xff",
    b"\x07\x00\x87\x00\x90\x10\x00\x00\xfa\xff\x59\x00\x45\x00\x41\x00\x52\x00\xfb\xff",
    b"\x08\x00\x88\x00\x90\x16\x00\x00\xfa\xff\x52\x00\x45\x00\x4d\x00\x49\x00\x58\x00\x45\x00\x52\x00\xfb\xff\x00\x00",
    b"\x09\x00\x89\x00\x90\x12\x00\x00\xfa\xff\x4c\x00\x41\x00\x42\x00\x45\x00\x4c\x00\xfb\xff\x00\x00",
    b"\x0a\x00\x8a\x00\x90\x26\x00\x00\xfa\xff\x4f\x00\x52\x00\x49\x00\x47\x00\x49\x00\x4e\x00\x41\x00\x4c\x00\x20\x00\x41\x00\x52\x00\x54\x00\x49\x00\x53\x00\x54\x00\xfb\xff\x00\x00",
    b"\x0b\x00\x8b\x00\x90\x0e\x00\x00\xfa\xff\x4b\x00\x45\x00\x59\x00\xfb\xff\x00\x00",
    b"\x0c\x00\x8d\x00\x90\x0e\x00\x00\xfa\xff\x43\x00\x55\x00\x45\x00\xfb\xff\x00\x00",
    b"\x0d\x00\x8e\x00\x90\x12\x00\x00\xfa\xff\x43\x00\x4f\x00\x4c\x00\x4f\x00\x52\x00\xfb\xff\x00\x00",
    b"\x0e\x00\x92\x00\x90\x10\x00\x00\xfa\xff\x54\x00\x49\x00\x4d\x00\x45\x00\xfb\xff",
    b"\x0f\x00\x93\x00\x90\x16\x00\x00\xfa\xff\x42\x00\x49\x00\x54\x00\x52\x00\x41\x00\x54\x00\x45\x00\xfb\xff\x00\x00",
    b"\x10\x00\x94\x00\x90\x1a\x00\x00\xfa\xff\x46\x00\x49\x00\x4c\x00\x45\x00\x20\x00\x4e\x00\x41\x00\x4d\x00\x45\x00\xfb\xff\x00\x00",
    b"\x11\x00\x84\x00\x90\x18\x00\x00\xfa\xff\x50\x00\x4c\x00\x41\x00\x59\x00\x4c\x00\x49\x00\x53\x00\x54\x00\xfb\xff",
    b"\x12\x00\x98\x00\x90\x20\x00\x00\xfa\xff\x48\x00\x4f\x00\x54\x00\x20\x00\x43\x00\x55\x00\x45\x00\x20\x00\x42\x00\x41\x00\x4e\x00\x4b\x00\xfb\xff",
    b"\x13\x00\x95\x00\x90\x16\x00\x00\xfa\xff\x48\x00\x49\x00\x53\x00\x54\x00\x4f\x00\x52\x00\x59\x00\xfb\xff\x00\x00",
    b"\x14\x00\x91\x00\x90\x14\x00\x00\xfa\xff\x53\x00\x45\x00\x41\x00\x52\x00\x43\x00\x48\x00\xfb\xff",
    b"\x15\x00\x96\x00\x90\x18\x00\x00\xfa\xff\x43\x00\x4f\x00\x4d\x00\x4d\x00\x45\x00\x4e\x00\x54\x00\x53\x00\xfb\xff",
    b"\x16\x00\x8c\x00\x90\x1c\x00\x00\xfa\xff\x44\x00\x41\x00\x54\x00\x45\x00\x20\x00\x41\x00\x44\x00\x44\x00\x45\x00\x44\x00\xfb\xff",
    b"\x17\x00\x97\x00\x90\x22\x00\x00\xfa\xff\x44\x00\x4a\x00\x20\x00\x50\x00\x4c\x00\x41\x00\x59\x00\x20\x00\x43\x00\x4f\x00\x55\x00\x4e\x00\x54\x00\xfb\xff\x00\x00",
    b"\x18\x00\x90\x00\x90\x14\x00\x00\xfa\xff\x46\x00\x4f\x00\x4c\x00\x44\x00\x45\x00\x52\x00\xfb\xff",
    b"\x19\x00\xa1\x00\x90\x16\x00\x00\xfa\xff\x44\x00\x45\x00\x46\x00\x41\x00\x55\x00\x4c\x00\x54\x00\xfb\xff\x00\x00",
    b"\x1a\x00\xa2\x00\x90\x18\x00\x00\xfa\xff\x41\x00\x4c\x00\x50\x00\x48\x00\x41\x00\x42\x00\x45\x00\x54\x00\xfb\xff",
    b"\x1b\x00\xaa\x00\x90\x18\x00\x00\xfa\xff\x4d\x00\x41\x00\x54\x00\x43\x00\x48\x00\x49\x00\x4e\x00\x47\x00\xfb\xff",
];

const MENU_CATEGORIES: &[&[u8]] = &[
    b"\x01\x00\x01\x00\x63\x01\x00\x00",
    b"\x05\x00\x06\x00\x05\x01\x00\x00",
    b"\x06\x00\x07\x00\x63\x01\x00\x00",
    b"\x07\x00\x08\x00\x63\x01\x00\x00",
    b"\x08\x00\x09\x00\x63\x01\x00\x00",
    b"\x09\x00\x0a\x00\x63\x01\x00\x00",
    b"\x0a\x00\x0b\x00\x63\x01\x00\x00",
    b"\x0d\x00\x0f\x00\x63\x01\x00\x00",
    b"\x0e\x00\x13\x00\x04\x01\x00\x00",
    b"\x0f\x00\x14\x00\x06\x01\x00\x00",
    b"\x10\x00\x15\x00\x63\x01\x00\x00",
    b"\x12\x00\x17\x00\x63\x01\x00\x00",
    b"\x02\x00\x02\x00\x02\x00\x01\x00",
    b"\x03\x00\x03\x00\x03\x00\x02\x00",
    b"\x04\x00\x04\x00\x01\x00\x03\x00",
    b"\x0b\x00\x0c\x00\x63\x00\x04\x00",
    b"\x11\x00\x05\x00\x63\x00\x05\x00",
    b"\x13\x00\x16\x00\x63\x00\x06\x00",
    b"\x14\x00\x12\x00\x63\x00\x07\x00",
    b"\x1b\x00\x1a\x00\x63\x02\x08\x00",
    b"\x18\x00\x11\x00\x63\x00\x09\x00",
    b"\x16\x00\x1b\x00\x63\x00\x0a\x00",
];

const MENU_SORTS: &[&[u8]] = &[
    b"\x01\x00\x06\x00\x01\x00\x00\x00",
    b"\x15\x00\x07\x00\x01\x00\x00\x00",
    b"\x0e\x00\x08\x00\x01\x00\x00\x00",
    b"\x08\x00\x09\x00\x01\x00\x00\x00",
    b"\x09\x00\x0a\x00\x01\x00\x00\x00",
    b"\x0a\x00\x0b\x00\x01\x00\x00\x00",
    b"\x0f\x00\x0d\x00\x01\x00\x00\x00",
    b"\x0d\x00\x0f\x00\x01\x00\x00\x00",
    b"\x17\x00\x10\x00\x01\x00\x00\x00",
    b"\x16\x00\x11\x00\x01\x00\x00\x00",
    b"\x19\x00\x00\x00\x00\x01\x00\x00",
    b"\x1a\x00\x01\x00\x00\x02\x00\x00",
    b"\x02\x00\x02\x00\x00\x03\x00\x00",
    b"\x03\x00\x03\x00\x00\x04\x00\x00",
    b"\x05\x00\x04\x00\x00\x05\x00\x00",
    b"\x06\x00\x05\x00\x00\x06\x00\x00",
    b"\x0b\x00\x0c\x00\x00\x07\x00\x00",
];

// ---------------------------------------------------------------------------
// Page assembly
// ---------------------------------------------------------------------------

fn pad4(mut row: Vec<u8>) -> Vec<u8> {
    while row.len() % 4 != 0 {
        row.push(0);
    }
    row
}

/// Split rows into page-sized chunks (each row already padded).
fn paginate(rows: &[Vec<u8>]) -> Vec<Vec<Vec<u8>>> {
    let mut chunks: Vec<Vec<Vec<u8>>> = Vec::new();
    let mut cur: Vec<Vec<u8>> = Vec::new();
    let mut used = 0usize;
    for row in rows {
        let fits = PAGE_HEADER + used + row.len() + index_bytes(cur.len() + 1) <= PAGE
            && cur.len() < 255;
        if !fits && !cur.is_empty() {
            chunks.push(std::mem::take(&mut cur));
            used = 0;
        }
        used += row.len();
        cur.push(row.clone());
    }
    if !cur.is_empty() {
        chunks.push(cur);
    }
    chunks
}

/// Emit one data page with the one-shot bookkeeping signature.
fn data_page(page_index: u32, ty: u32, next_page: u32, rows: &[Vec<u8>]) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    let used: usize = rows.iter().map(|r| r.len()).sum();
    let free = PAGE - PAGE_HEADER - used - index_bytes(rows.len());
    p[0x04..0x08].copy_from_slice(&page_index.to_le_bytes());
    p[0x08..0x0C].copy_from_slice(&ty.to_le_bytes());
    p[0x0C..0x10].copy_from_slice(&next_page.to_le_bytes());
    p[0x10..0x14].copy_from_slice(&1u32.to_le_bytes()); // page tx id
    p[0x18] = rows.len() as u8;
    // Unaligned u16 at 0x19: 32 × present rows.
    let cnt = (rows.len() as u16) * 32;
    p[0x19..0x1B].copy_from_slice(&cnt.to_le_bytes());
    p[0x1B] = 0x24;
    p[0x1C..0x1E].copy_from_slice(&(free as u16).to_le_bytes());
    p[0x1E..0x20].copy_from_slice(&(used as u16).to_le_bytes());
    p[0x20..0x22].copy_from_slice(&(rows.len() as u16).to_le_bytes()); // last batch = all
    p[0x22..0x24].copy_from_slice(&0u16.to_le_bytes()); // last-written slot (one-shot: 0)

    // Heap.
    let mut off = 0usize;
    let mut offsets = Vec::with_capacity(rows.len());
    for row in rows {
        p[PAGE_HEADER + off..PAGE_HEADER + off + row.len()].copy_from_slice(row);
        offsets.push(off as u16);
        off += row.len();
    }
    // Row index, groups of 16 from the page end down.
    for g in 0..rows.len().div_ceil(16) {
        let end = PAGE - 36 * g;
        let in_group = (rows.len() - g * 16).min(16);
        let flags: u16 = if in_group == 16 {
            0xFFFF
        } else {
            (1u16 << in_group) - 1
        };
        // Last-batch bitmask mirrors the presence flags on a one-shot write.
        p[end - 2..end].copy_from_slice(&flags.to_le_bytes());
        p[end - 4..end - 2].copy_from_slice(&flags.to_le_bytes());
        for r in 0..in_group {
            let o = end - 6 - 2 * r;
            p[o..o + 2].copy_from_slice(&offsets[g * 16 + r].to_le_bytes());
        }
    }
    p
}

/// Emit a sentinel ("strange") page — the first page of every table.
fn sentinel_page(page_index: u32, ty: u32, next_page: u32) -> Vec<u8> {
    let mut p = vec![0u8; PAGE];
    p[0x04..0x08].copy_from_slice(&page_index.to_le_bytes());
    p[0x08..0x0C].copy_from_slice(&ty.to_le_bytes());
    p[0x0C..0x10].copy_from_slice(&next_page.to_le_bytes());
    p[0x10..0x14].copy_from_slice(&1u32.to_le_bytes());
    p[0x1B] = 0x64;
    p[0x20..0x22].copy_from_slice(&0x1FFFu16.to_le_bytes());
    p[0x22..0x24].copy_from_slice(&0x1FFFu16.to_le_bytes());
    p[0x24..0x26].copy_from_slice(&1004u16.to_le_bytes());
    p
}

/// Assemble a complete DeviceSQL database. `tables[t]` = the encoded rows of
/// table type `t`. Returns the file bytes.
fn build_dsql(tables: &[Vec<Vec<u8>>]) -> Vec<u8> {
    let n = tables.len();
    // Chunked rows per table.
    let chunked: Vec<Vec<Vec<Vec<u8>>>> = tables.iter().map(|t| paginate(t)).collect();

    // Assign page indices: sentinel 2t+1, first data page 2t+2 (blank when the
    // table is empty), overflow appended after 2n+1 in table order.
    let mut next_free = (2 * n + 1) as u32;
    let mut overflow: HashMap<usize, Vec<u32>> = HashMap::new();
    for (t, chunks) in chunked.iter().enumerate() {
        let extra = chunks.len().saturating_sub(1);
        let ids: Vec<u32> = (0..extra).map(|i| next_free + i as u32).collect();
        next_free += extra as u32;
        overflow.insert(t, ids);
    }
    let total_pages = next_free;

    let mut out = vec![0u8; total_pages as usize * PAGE];
    let put = |out: &mut Vec<u8>, idx: u32, page: Vec<u8>| {
        let o = idx as usize * PAGE;
        out[o..o + PAGE].copy_from_slice(&page);
    };

    // File header.
    {
        let h = &mut out[..PAGE];
        h[0x04..0x08].copy_from_slice(&(PAGE as u32).to_le_bytes());
        h[0x08..0x0C].copy_from_slice(&(n as u32).to_le_bytes());
        h[0x0C..0x10].copy_from_slice(&total_pages.to_le_bytes());
        h[0x10..0x14].copy_from_slice(&5u32.to_le_bytes());
        h[0x14..0x18].copy_from_slice(&2u32.to_le_bytes()); // sequence
    }

    for (t, chunks) in chunked.iter().enumerate() {
        let sentinel = (2 * t + 1) as u32;
        let first_data = (2 * t + 2) as u32;
        let ov = &overflow[&t];
        let last = if chunks.is_empty() {
            sentinel
        } else if chunks.len() == 1 {
            first_data
        } else {
            ov[chunks.len() - 2]
        };
        let empty_candidate = if chunks.is_empty() {
            first_data
        } else {
            total_pages
        };
        // Directory entry.
        {
            let o = 0x1C + t * 16;
            let h = &mut out[o..o + 16];
            h[0..4].copy_from_slice(&(t as u32).to_le_bytes());
            h[4..8].copy_from_slice(&empty_candidate.to_le_bytes());
            h[8..12].copy_from_slice(&sentinel.to_le_bytes());
            h[12..16].copy_from_slice(&last.to_le_bytes());
        }
        put(&mut out, sentinel, sentinel_page(sentinel, t as u32, first_data));
        if chunks.is_empty() {
            // Blank, unlinked data page — mirrors rekordbox's empty tables.
            put(&mut out, first_data, data_page(first_data, t as u32, total_pages, &[]));
        } else {
            for (i, chunk) in chunks.iter().enumerate() {
                let idx = if i == 0 { first_data } else { ov[i - 1] };
                let next = if i + 1 < chunks.len() {
                    if i == 0 {
                        ov[0]
                    } else {
                        ov[i]
                    }
                } else {
                    total_pages
                };
                put(&mut out, idx, data_page(idx, t as u32, next, chunk));
            }
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Public builders
// ---------------------------------------------------------------------------

/// Build a complete `export.pdb`.
pub(crate) fn build_export_pdb(t: &PdbTables) -> Vec<u8> {
    let ishift = |i: usize| (i as u16 % 16) * 32; // per-page slot × 32; recomputed per chunk below

    // NOTE on index_shift: rekordbox stores 32 × the row's slot *within its
    // page*. Rows that overflow onto later pages restart at 0. `paginate`
    // splits after encoding, so encode with the within-page slot by chunking
    // tracks/artists/albums manually at the same capacity rule.
    let _ = ishift;

    let mut tables: Vec<Vec<Vec<u8>>> = vec![Vec::new(); 20];

    // Encode with correct per-page index_shift: simulate pagination as we go.
    fn encode_paged<T>(items: &[T], mut enc: impl FnMut(&T, u16) -> Vec<u8>) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::with_capacity(items.len());
        let mut used = 0usize;
        let mut slot = 0usize;
        for it in items {
            // Encode assuming current slot; check fit, else restart page.
            let mut row = pad4(enc(it, (slot as u16) * 32));
            let fits =
                PAGE_HEADER + used + row.len() + index_bytes(slot + 1) <= PAGE && slot < 255;
            if !fits {
                slot = 0;
                used = 0;
                row = pad4(enc(it, 0));
            }
            used += row.len();
            slot += 1;
            out.push(row);
        }
        out
    }

    tables[0] = encode_paged(&t.tracks, |tr, sh| track_row(tr, sh));
    tables[1] = t
        .genres
        .iter()
        .map(|(id, n)| pad4(genre_row(*id, n)))
        .collect();
    tables[2] = encode_paged(&t.artists, |(id, n), sh| artist_row(*id, n, sh));
    tables[3] = encode_paged(&t.albums, |(id, n), sh| album_row(*id, n, sh));
    tables[4] = t
        .labels
        .iter()
        .map(|(id, n)| pad4(genre_row(*id, n)))
        .collect();
    tables[5] = t.keys.iter().map(|(id, n)| pad4(key_row(*id, n))).collect();
    tables[6] = (1u16..=8)
        .map(|i| pad4(color_row(i, COLORS[i as usize - 1])))
        .collect();
    tables[7] = t.playlists.iter().map(|p| pad4(playlist_tree_row(p))).collect();
    tables[8] = t
        .playlist_entries
        .iter()
        .map(|(e, tr, pl)| pad4(playlist_entry_row(*e, *tr, *pl)))
        .collect();
    tables[13] = t
        .artwork
        .iter()
        .map(|(id, p)| pad4(artwork_row(*id, p)))
        .collect();
    tables[16] = MENU_COLUMNS.iter().map(|r| pad4(r.to_vec())).collect();
    tables[17] = MENU_CATEGORIES.iter().map(|r| pad4(r.to_vec())).collect();
    tables[18] = MENU_SORTS.iter().map(|r| pad4(r.to_vec())).collect();
    tables[19] = vec![pad4(summary_row(
        t.tracks.len() as u32,
        &t.created_date,
        0,
    ))];

    build_dsql(&tables)
}

/// Build a minimal `exportExt.pdb`: the 9-table My Tag database, empty —
/// exactly the skeleton rekordbox writes for a library with no My Tags.
pub(crate) fn build_export_ext_pdb() -> Vec<u8> {
    build_dsql(&vec![Vec::new(); 9])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dsql_string_forms() {
        assert_eq!(dsql_string(""), vec![0x03]);
        assert_eq!(dsql_string("ON"), vec![0x07, b'O', b'N']);
        // 126 ASCII chars still short-form; 127 flips to long ASCII.
        let s126 = "a".repeat(126);
        assert_eq!(dsql_string(&s126)[0], 0xFF);
        let s127 = "a".repeat(127);
        let enc = dsql_string(&s127);
        assert_eq!(enc[0], 0x40);
        assert_eq!(u16::from_le_bytes([enc[1], enc[2]]) as usize, 127 + 4);
        // Non-ASCII goes UTF-16LE.
        let enc = dsql_string("Pärt");
        assert_eq!(enc[0], 0x90);
        assert_eq!(u16::from_le_bytes([enc[1], enc[2]]) as usize, 4 * 2 + 4);
    }

    #[test]
    fn free_size_accounting_matches_formula() {
        let rows: Vec<Vec<u8>> = (0..20).map(|i| pad4(genre_row(i + 1, "House"))).collect();
        let p = data_page(4, 1, 5, &rows);
        let used = u16::from_le_bytes([p[0x1E], p[0x1F]]) as usize;
        let free = u16::from_le_bytes([p[0x1C], p[0x1D]]) as usize;
        assert_eq!(
            free,
            PAGE - PAGE_HEADER - used - (2 * 4 + 20 * 2),
            "free_size must satisfy the golden-reference formula"
        );
        assert_eq!(p[0x18] as usize, 20);
        assert_eq!(u16::from_le_bytes([p[0x19], p[0x1A]]), 20 * 32);
    }

    #[test]
    fn track_row_header_is_0x88_bytes() {
        let r = track_row(
            &TrackRow {
                id: 1,
                title: "T".into(),
                filename: "t.mp3".into(),
                file_path: "/Contents/t.mp3".into(),
                ..Default::default()
            },
            0,
        );
        // First string offset must be 0x88.
        assert_eq!(u16::from_le_bytes([r[0x5E], r[0x5F]]), 0x88);
        assert_eq!(&r[0x00..0x02], &[0x24, 0x00]);
        assert_eq!(&r[0x04..0x08], &0x000C_0700u32.to_le_bytes());
    }
}
