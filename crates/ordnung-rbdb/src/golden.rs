//! Golden-fixture diffing: compare an `export.pdb` Ordnung wrote against one
//! rekordbox wrote for the same tracks, field by field, and say for every
//! difference whether it is *expected* (ids, dates, random per-export
//! values) or a real divergence from rekordbox's output.
//!
//! This is Phase 6's "round-trip diffs are explained" tool. The walk uses
//! the reader's page and row helpers (`pdb`) and the row map in
//! `docs/rekordbox-export-structure.md` §2.3; a field is compared raw at
//! its documented offset, and interned references are compared by the
//! *name* they resolve to rather than the id, since id assignment is
//! per-export.

use std::collections::BTreeMap;

use crate::pdb::{self, dsql_string, page_rows, table_pages, u16_at, u32_at, ReadError};

/// One field that differs between the two files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delta {
    pub field: String,
    pub golden: String,
    pub ours: String,
    /// Why this difference is expected, or `None` if it isn't.
    pub explained: Option<&'static str>,
}

/// Page and row accounting for one table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDiff {
    pub table_type: u32,
    pub name: &'static str,
    pub golden_pages: usize,
    pub ours_pages: usize,
    pub golden_rows: usize,
    pub ours_rows: usize,
    pub deltas: Vec<Delta>,
}

/// One track's row compared across the two files (matched by filename).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackDiff {
    pub filename: String,
    pub deltas: Vec<Delta>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdbDiff {
    pub header: Vec<Delta>,
    pub tables: Vec<TableDiff>,
    pub tracks: Vec<TrackDiff>,
    /// Filenames present only in the golden file / only in ours.
    pub only_golden: Vec<String>,
    pub only_ours: Vec<String>,
}

impl PdbDiff {
    /// Every difference with no explanation, as `"where: field golden→ours"`.
    pub fn unexplained(&self) -> Vec<String> {
        let mut out = Vec::new();
        for d in &self.header {
            if d.explained.is_none() {
                out.push(format!("header: {} {} → {}", d.field, d.golden, d.ours));
            }
        }
        for t in &self.tables {
            for d in t.deltas.iter().filter(|d| d.explained.is_none()) {
                out.push(format!("table {}: {} {} → {}", t.name, d.field, d.golden, d.ours));
            }
        }
        for t in &self.tracks {
            for d in t.deltas.iter().filter(|d| d.explained.is_none()) {
                out.push(format!(
                    "track {}: {} {} → {}",
                    t.filename, d.field, d.golden, d.ours
                ));
            }
        }
        for f in &self.only_golden {
            out.push(format!("track only in golden: {f}"));
        }
        for f in &self.only_ours {
            out.push(format!("track only in ours: {f}"));
        }
        out
    }

    /// A human report: header, per-table accounting, then per-track deltas,
    /// each marked `ok` (explained) or `!!` (not).
    pub fn render(&self) -> String {
        let mut s = String::new();
        let mark = |d: &Delta| if d.explained.is_some() { "ok" } else { "!!" };
        s.push_str("header\n");
        for d in &self.header {
            s.push_str(&format!(
                "  {} {:<22} {:>14} → {:<14} {}\n",
                mark(d),
                d.field,
                d.golden,
                d.ours,
                d.explained.unwrap_or("")
            ));
        }
        s.push_str("tables (pages golden/ours, rows golden/ours)\n");
        for t in &self.tables {
            s.push_str(&format!(
                "  {:>2} {:<18} pages {}/{}  rows {}/{}\n",
                t.table_type, t.name, t.golden_pages, t.ours_pages, t.golden_rows, t.ours_rows
            ));
            for d in &t.deltas {
                s.push_str(&format!(
                    "     {} {:<22} {:>14} → {:<14} {}\n",
                    mark(d),
                    d.field,
                    d.golden,
                    d.ours,
                    d.explained.unwrap_or("")
                ));
            }
        }
        s.push_str("tracks\n");
        for t in &self.tracks {
            s.push_str(&format!("  {}\n", t.filename));
            for d in &t.deltas {
                s.push_str(&format!(
                    "     {} {:<22} {:>14} → {:<14} {}\n",
                    mark(d),
                    d.field,
                    trunc(&d.golden),
                    trunc(&d.ours),
                    d.explained.unwrap_or("")
                ));
            }
        }
        for f in &self.only_golden {
            s.push_str(&format!("  !! only in golden: {f}\n"));
        }
        for f in &self.only_ours {
            s.push_str(&format!("  !! only in ours: {f}\n"));
        }
        let n = self.unexplained().len();
        s.push_str(&format!("{n} unexplained difference(s)\n"));
        s
    }
}

fn trunc(s: &str) -> String {
    if s.chars().count() > 14 {
        let mut t: String = s.chars().take(13).collect();
        t.push('…');
        t
    } else {
        s.to_string()
    }
}

/// Table names by type id (§2.1).
pub const TABLE_NAMES: [&str; 20] = [
    "tracks",
    "genres",
    "artists",
    "albums",
    "labels",
    "keys",
    "colors",
    "playlist_tree",
    "playlist_entries",
    "unknown9",
    "unknown10",
    "history_playlists",
    "history_entries",
    "artwork",
    "unknown14",
    "unknown15",
    "menu_columns",
    "browse_categories",
    "sort_menu",
    "export_summary",
];

/// Fixed-width track row fields (§2.3): offset, width in bytes, name, and
/// why a difference is expected (`None` = must match).
const ROW_FIELDS: &[(usize, usize, &str, Option<&str>)] = &[
    (0x00, 2, "magic", None),
    (0x04, 4, "content_link", None),
    (0x08, 4, "sample_rate", None),
    (0x0c, 4, "composer_id", Some("interned per export")),
    (0x10, 4, "file_size", Some("audio bytes are the fixture's, not the real file's")),
    (0x14, 4, "master_content_id", Some("random 28-bit per export")),
    (0x18, 2, "const_2fdb", Some("rekordbox-version constant; ours is rekordbox 7's")),
    (0x1a, 2, "const_8f45", Some("rekordbox-version constant; ours is rekordbox 7's")),
    (0x1c, 4, "artwork_id", Some("interned per export")),
    (0x20, 4, "key_id", Some("interned per export (name compared instead)")),
    (0x24, 4, "original_artist_id", Some("interned per export")),
    (0x28, 4, "label_id", Some("interned per export (name compared instead)")),
    (0x2c, 4, "remixer_id", Some("interned per export")),
    (0x30, 4, "bitrate", None),
    (0x34, 4, "track_number", None),
    (0x38, 4, "tempo", None),
    (0x3c, 4, "genre_id", Some("interned per export (name compared instead)")),
    (0x40, 4, "album_id", Some("interned per export (name compared instead)")),
    (0x44, 4, "artist_id", Some("interned per export (name compared instead)")),
    (0x48, 4, "id", Some("dense per export")),
    (0x4c, 2, "disc_number", None),
    (0x4e, 2, "play_count", Some("player-side counter")),
    (0x50, 2, "year", None),
    (0x52, 2, "sample_depth", None),
    (0x54, 2, "duration", None),
    (0x56, 2, "const_41", None),
    (0x58, 1, "color_id", None),
    (0x59, 1, "rating", None),
    (0x5a, 2, "file_type", None),
    (0x5c, 2, "const_3", None),
];

/// The 21 string slots (§2.3), with the same explanation column.
const ROW_STRINGS: [(&str, Option<&str>); 21] = [
    ("isrc", None),
    ("texter", None),
    ("information_update_count", Some("rekordbox edit counter")),
    ("analysis_update_count", Some("rekordbox edit counter")),
    ("cue_update_count", Some("rekordbox edit counter")),
    ("message", None),
    ("kuvo_public", Some("older rekordbox left it empty; rekordbox 7 writes ON")),
    ("autoload_hotcues", Some("older rekordbox left it empty; rekordbox 7 writes ON")),
    ("unknown8", None),
    ("unknown9", None),
    ("date_added", Some("stamped with the export date")),
    ("release_date", None),
    ("mix_name", None),
    ("unknown13", None),
    ("analyze_path", Some("follows the per-export id")),
    ("analyze_date", Some("stamped with the export date")),
    ("comment", None),
    ("title", None),
    ("unknown18", None),
    ("filename", None),
    ("file_path", Some("Ordnung keeps /Contents flat; rekordbox mirrors the source tree")),
];

struct Tables {
    page_size: usize,
    /// type → (first_page, last_page)
    dir: Vec<(u32, u32, u32)>,
}

fn header(data: &[u8]) -> Result<Tables, ReadError> {
    if u32_at(data, 0) != Some(0) {
        return Err(ReadError::Format("bad signature"));
    }
    let page_size = u32_at(data, 4).ok_or(ReadError::Format("truncated header"))? as usize;
    let n = u32_at(data, 8).ok_or(ReadError::Format("truncated header"))? as usize;
    if page_size == 0 || n > 64 {
        return Err(ReadError::Format("implausible header"));
    }
    let mut dir = Vec::new();
    for t in 0..n {
        let base = 0x1C + t * 16;
        let (Some(ty), Some(first), Some(last)) =
            (u32_at(data, base), u32_at(data, base + 8), u32_at(data, base + 12))
        else {
            return Err(ReadError::Format("truncated table directory"));
        };
        dir.push((ty, first, last));
    }
    Ok(Tables { page_size, dir })
}

/// Data pages (sentinels skipped) and present-row offsets of one table.
fn table_rows(data: &[u8], t: &Tables, ty: u32) -> (Vec<usize>, Vec<usize>) {
    let Some(&(_, first, last)) = t.dir.iter().find(|(x, _, _)| *x == ty) else {
        return (Vec::new(), Vec::new());
    };
    let mut pages = Vec::new();
    let mut rows = Vec::new();
    for off in table_pages(data, t.page_size, first, last) {
        let flags = data.get(off + 0x1B).copied().unwrap_or(0);
        if flags & 0x40 != 0 {
            continue;
        }
        pages.push(off);
        rows.extend(page_rows(data, t.page_size, off, ty));
    }
    (pages, rows)
}

fn raw(data: &[u8], off: usize, width: usize) -> u64 {
    match width {
        1 => data.get(off).copied().unwrap_or(0) as u64,
        2 => u16_at(data, off).unwrap_or(0) as u64,
        _ => u32_at(data, off).unwrap_or(0) as u64,
    }
}

fn delta(field: &str, golden: impl ToString, ours: impl ToString, explained: Option<&'static str>) -> Delta {
    Delta {
        field: field.to_string(),
        golden: golden.to_string(),
        ours: ours.to_string(),
        explained,
    }
}

/// Diff two `export.pdb` images. `golden` is rekordbox's, `ours` Ordnung's.
pub fn diff_pdb(golden: &[u8], ours: &[u8]) -> Result<PdbDiff, ReadError> {
    let g = header(golden)?;
    let o = header(ours)?;
    let mut out = PdbDiff::default();

    // ---- header -------------------------------------------------------
    if g.page_size != o.page_size {
        out.header.push(delta("page_size", g.page_size, o.page_size, None));
    }
    if g.dir.len() != o.dir.len() {
        out.header.push(delta("num_tables", g.dir.len(), o.dir.len(), None));
    }
    let g_types: Vec<u32> = g.dir.iter().map(|d| d.0).collect();
    let o_types: Vec<u32> = o.dir.iter().map(|d| d.0).collect();
    if g_types != o_types {
        out.header.push(delta("table_order", format!("{g_types:?}"), format!("{o_types:?}"), None));
    }
    for (field, off, why) in [
        ("next_unused_page", 0x0c, Some("allocator slack")),
        ("const_5", 0x10, None),
        ("sequence", 0x14, Some("transaction counter")),
    ] {
        let (a, b) = (u32_at(golden, off).unwrap_or(0), u32_at(ours, off).unwrap_or(0));
        if a != b {
            out.header.push(delta(field, a, b, why));
        }
    }

    // ---- tables -------------------------------------------------------
    // Intern tables may legitimately carry rows nothing exported references
    // (rekordbox never garbage-collects them).
    let intern_table = |ty: u32| matches!(ty, 1..=5 | 13);
    for ty in 0..20u32 {
        let (gp, gr) = table_rows(golden, &g, ty);
        let (op, or) = table_rows(ours, &o, ty);
        let mut deltas = Vec::new();
        // A golden page that has seen deletes/rewrites (flag bit 0x10) keeps
        // its deleted slots and their heap bytes; a fresh export has neither.
        let rewritten = |data: &[u8], off: usize| data.get(off + 0x1B).is_some_and(|f| f & 0x10 != 0);
        let golden_rewritten = gp.iter().any(|&a| rewritten(golden, a));
        if gp.len() != op.len() {
            let why = golden_rewritten.then_some("golden chain grew through rewrites");
            deltas.push(delta("pages", gp.len(), op.len(), why));
        }
        if gr.len() != or.len() {
            let why = (intern_table(ty) && gr.len() > or.len())
                .then_some("golden intern table carries rows no exported track references");
            deltas.push(delta("rows", gr.len(), or.len(), why));
        }
        // Per data page: flags and the heap accounting, in chain order.
        for (i, (a, b)) in gp.iter().zip(op.iter()).enumerate() {
            let g_slots = raw(golden, a + 0x18, 1) as usize;
            let g_present = page_rows(golden, g.page_size, *a, ty).len();
            let o_present = page_rows(ours, o.page_size, *b, ty).len();
            let history = rewritten(golden, *a) || g_slots > g_present;
            for (field, off, width) in [
                ("flags", 0x1b, 1usize),
                ("num_row_slots", 0x18, 1),
                ("used_size", 0x1e, 2),
                ("free_size", 0x1c, 2),
            ] {
                let (x, y) = (raw(golden, a + off, width), raw(ours, b + off, width));
                if x != y {
                    let why = match field {
                        "flags" if (x ^ y) == 0x10 && rewritten(golden, *a) => {
                            Some("golden page has seen deletes/rewrites; a fresh export is 0x24")
                        }
                        "num_row_slots" if g_slots > g_present && o_present + (g_slots - g_present) == x as usize => {
                            Some("golden slot count includes deleted rows")
                        }
                        "num_row_slots" if g_present != o_present => Some("row counts differ (see rows)"),
                        "used_size" | "free_size" if history => {
                            Some("deleted golden rows still occupy the heap")
                        }
                        "used_size" | "free_size" if g_present != o_present => {
                            Some("row counts differ (see rows)")
                        }
                        "used_size" | "free_size" if ty == pdb::TYPE_TRACKS => {
                            Some("track rows differ in string lengths (paths, dates)")
                        }
                        _ => None,
                    };
                    deltas.push(delta(&format!("page{i}.{field}"), x, y, why));
                }
            }
        }
        out.tables.push(TableDiff {
            table_type: ty,
            name: TABLE_NAMES[ty as usize],
            golden_pages: gp.len(),
            ours_pages: op.len(),
            golden_rows: gr.len(),
            ours_rows: or.len(),
            deltas,
        });
    }

    // ---- track rows, matched by filename --------------------------------
    let (_, g_rows) = table_rows(golden, &g, pdb::TYPE_TRACKS);
    let (_, o_rows) = table_rows(ours, &o, pdb::TYPE_TRACKS);
    let string_at = |data: &[u8], row: usize, idx: usize| -> String {
        u16_at(data, row + 0x5E + 2 * idx)
            .and_then(|rel| dsql_string(data, row + rel as usize))
            .unwrap_or_default()
    };
    let key_by_name = |data: &[u8], rows: &[usize]| -> BTreeMap<String, usize> {
        rows.iter()
            .map(|&r| (string_at(data, r, 19).to_lowercase(), r))
            .collect()
    };
    let g_by = key_by_name(golden, &g_rows);
    let o_by = key_by_name(ours, &o_rows);
    // Resolved interned names via the reader (id → name is per file).
    let g_read = pdb::read_export_bytes(golden)?;
    let o_read = pdb::read_export_bytes(ours)?;
    let named = |ex: &pdb::RbExport, row_id: u32| -> [String; 5] {
        let t = ex.tracks.get(&row_id);
        let f = |v: Option<&String>| v.cloned().unwrap_or_default();
        [
            f(t.and_then(|t| t.artist.as_ref())),
            f(t.and_then(|t| t.album.as_ref())),
            f(t.and_then(|t| t.genre.as_ref())),
            f(t.and_then(|t| t.label.as_ref())),
            f(t.and_then(|t| t.key.as_ref())),
        ]
    };
    for (name, &gr) in &g_by {
        let Some(&or) = o_by.get(name) else {
            out.only_golden.push(name.clone());
            continue;
        };
        let mut deltas = Vec::new();
        for &(off, width, field, why) in ROW_FIELDS {
            let (x, y) = (raw(golden, gr + off, width), raw(ours, or + off, width));
            if x != y {
                deltas.push(delta(field, x, y, why));
            }
        }
        for (i, (field, why)) in ROW_STRINGS.iter().enumerate() {
            let (x, y) = (string_at(golden, gr, i), string_at(ours, or, i));
            if x != y {
                deltas.push(delta(field, x, y, *why));
            }
        }
        let gid = u32_at(golden, gr + 0x48).unwrap_or(0);
        let oid = u32_at(ours, or + 0x48).unwrap_or(0);
        let (gn, on) = (named(&g_read, gid), named(&o_read, oid));
        for (i, field) in ["artist", "album", "genre", "label", "key"].iter().enumerate() {
            if gn[i] != on[i] {
                let why = (*field == "key").then_some("key notation is a rekordbox display setting; Ordnung writes Camelot");
                deltas.push(delta(field, &gn[i], &on[i], why));
            }
        }
        out.tracks.push(TrackDiff {
            filename: name.clone(),
            deltas,
        });
    }
    for name in o_by.keys() {
        if !g_by.contains_key(name) {
            out.only_ours.push(name.clone());
        }
    }
    Ok(out)
}
