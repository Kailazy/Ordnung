//! Whole-stick golden diff: everything a player reads off a rekordbox USB,
//! compared between a stick rekordbox wrote and one Ordnung wrote for the
//! same songs.
//!
//! [`diff_pdb`](super::diff_pdb) covers `export.pdb`; this adds the rest of
//! the image — each track's ANLZ files ([`super::anlz`]), the playlist tree
//! as both databases carry it, the Device Library Plus `content` rows column
//! by column, My Tags, `exportExt.pdb`'s tables, artwork presence and size,
//! and the file inventory under `/PIONEER`. Tracks are matched by filename,
//! interned references by the name they resolve to, and every difference is
//! either explained (per-export ids, dates, Ordnung's flat `/Contents`,
//! waveforms that follow rekordbox's shape) or left for a person to look at.
//!
//! [`coverage`] counts what a stick exercises (hot cues, loops, colours,
//! ratings, My Tags, folders, …) so a reference with gaps shows them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use rusqlite::types::Value;

use super::anlz::{diff_anlz, Shape};
use super::{delta, header, table_rows, Delta, PdbDiff};
use crate::pdb::{read_export, RbExport, RbPlaylist, ReadError};

/// One difference, located.
#[derive(Debug, Clone, PartialEq)]
pub struct Finding {
    /// `pdb`, `pdb.track`, `anlz`, `playlists.pdb`, `playlists.dlp`, `dlp`,
    /// `dlp.content`, `ext`, `artwork`, `files`, `match`.
    pub area: &'static str,
    /// The track's filename, for per-track findings.
    pub track: Option<String>,
    pub delta: Delta,
}

impl Finding {
    /// `area field`: what the tally and an accepted baseline count by.
    pub fn key(&self) -> String {
        format!("{} {}", self.area, self.delta.field)
    }
}

#[derive(Debug, Clone, Default)]
pub struct StickDiff {
    pub findings: Vec<Finding>,
    /// Every waveform section's score, by track.
    pub shapes: Vec<(String, Shape)>,
    /// Tracks present on both sticks.
    pub matched: usize,
}

impl StickDiff {
    pub fn unexplained(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.delta.explained.is_none())
    }

    /// Per `area field` key: (explained, unexplained) counts.
    pub fn tally(&self) -> BTreeMap<String, (usize, usize)> {
        let mut out: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        for f in &self.findings {
            let e = out.entry(f.key()).or_default();
            if f.delta.explained.is_some() {
                e.0 += 1;
            } else {
                e.1 += 1;
            }
        }
        out
    }

    /// Full listing: every finding, grouped by key, marked `ok`/`!!`, with
    /// up to `per_key` examples each (0 = all).
    pub fn render(&self, per_key: usize) -> String {
        let mut by_key: BTreeMap<String, Vec<&Finding>> = BTreeMap::new();
        for f in &self.findings {
            by_key.entry(f.key()).or_default().push(f);
        }
        let mut s = String::new();
        for (key, list) in by_key {
            let bad = list.iter().filter(|f| f.delta.explained.is_none()).count();
            let mark = if bad > 0 { "!!" } else { "ok" };
            let why = list.iter().find_map(|f| f.delta.explained).unwrap_or("");
            s.push_str(&format!("{mark} {key}  ({bad} unexplained / {})  {why}\n", list.len()));
            let shown = if per_key == 0 { list.len() } else { per_key };
            // Unexplained examples first.
            let mut ordered = list.clone();
            ordered.sort_by_key(|f| f.delta.explained.is_some());
            for f in ordered.iter().take(shown) {
                s.push_str(&format!(
                    "     {}{}  →  {}\n        {}\n",
                    f.track.as_deref().map(|t| format!("[{t}] ")).unwrap_or_default(),
                    f.delta.golden,
                    f.delta.ours,
                    if f.delta.explained.is_some() { "" } else { "(unexplained)" },
                ));
            }
            if list.len() > shown {
                s.push_str(&format!("     … {} more\n", list.len() - shown));
            }
        }
        s
    }
}

fn push(out: &mut StickDiff, area: &'static str, track: Option<&str>, d: Delta) {
    out.findings.push(Finding {
        area,
        track: track.map(str::to_string),
        delta: d,
    });
}

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> ReadError + '_ {
    move |source| ReadError::Io {
        path: path.to_path_buf(),
        source,
    }
}

fn rb_dir(root: &Path) -> PathBuf {
    root.join("PIONEER").join("rekordbox")
}

fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_lowercase()
}

/// Filename (lowercased) → track id, for matching the two sticks.
fn by_filename(ex: &RbExport) -> BTreeMap<String, u32> {
    ex.tracks.iter().map(|(id, t)| (basename(&t.file_path), *id)).collect()
}

/// Diff the stick image rekordbox wrote at `golden` against Ordnung's at
/// `ours`. Both are volume roots (the folder holding `PIONEER/`).
pub fn diff_stick(golden: &Path, ours: &Path) -> Result<StickDiff, ReadError> {
    let mut out = StickDiff::default();
    let (gp, op) = (rb_dir(golden).join("export.pdb"), rb_dir(ours).join("export.pdb"));
    let g_pdb = std::fs::read(&gp).map_err(io(&gp))?;
    let o_pdb = std::fs::read(&op).map_err(io(&op))?;
    pdb_findings(&super::diff_pdb(&g_pdb, &o_pdb)?, &mut out);

    let g_ex = read_export(&gp)?;
    let o_ex = read_export(&op)?;
    let (g_by, o_by) = (by_filename(&g_ex), by_filename(&o_ex));

    // ---- per-track ANLZ and artwork -------------------------------------
    for (name, gid) in &g_by {
        let Some(oid) = o_by.get(name) else { continue };
        out.matched += 1;
        let (gt, ot) = (&g_ex.tracks[gid], &o_ex.tracks[oid]);
        let dat = |root: &Path, t: &crate::pdb::RbTrack| {
            t.analyze_path.as_deref().map(|p| root.join(p.trim_start_matches('/')))
        };
        match (dat(golden, gt), dat(ours, ot)) {
            (Some(g), Some(o)) => {
                for kind in ["DAT", "EXT", "2EX"] {
                    let (g, o) = (g.with_extension(kind), o.with_extension(kind));
                    match (std::fs::read(&g), std::fs::read(&o)) {
                        (Ok(a), Ok(b)) => {
                            let d = diff_anlz(kind, &a, &b);
                            for x in d.deltas {
                                push(&mut out, "anlz", Some(name), x);
                            }
                            out.shapes.extend(d.shapes.into_iter().map(|s| (name.clone(), s)));
                        }
                        (Ok(_), Err(_)) => push(&mut out, "anlz", Some(name), delta(&format!("{kind}.file"), "present", "absent", None)),
                        (Err(_), Ok(_)) => push(&mut out, "anlz", Some(name), delta(&format!("{kind}.file"), "absent", "present", None)),
                        (Err(_), Err(_)) => {}
                    }
                }
            }
            (g, o) => push(
                &mut out,
                "anlz",
                Some(name),
                delta("analyze_path", g.is_some(), o.is_some(), None),
            ),
        }
        artwork(golden, gt, ours, ot, name, &mut out);
    }

    // ---- playlists, as each database carries them ------------------------
    let tree = |ex: &RbExport| playlist_view(&ex.playlists, |id| {
        ex.entries
            .get(&id)
            .map(|ids| ids.iter().filter_map(|t| ex.tracks.get(t)).map(|t| basename(&t.file_path)).collect())
            .unwrap_or_default()
    });
    compare_playlists("playlists.pdb", &tree(&g_ex), &tree(&o_ex), &mut out);
    let dlp_tree = |root: &Path| -> Option<Vec<PlaylistNode>> {
        let d = crate::dlp::read_playlists(&rb_dir(root).join("exportLibrary.db")).ok()?;
        Some(playlist_view(&d.playlists, |id| {
            d.entries_by_path
                .get(&id)
                .map(|v| v.iter().map(|p| basename(p)).collect())
                .unwrap_or_default()
        }))
    };
    match (dlp_tree(golden), dlp_tree(ours)) {
        (Some(g), Some(o)) => compare_playlists("playlists.dlp", &g, &o, &mut out),
        (g, o) => push(&mut out, "dlp", None, delta("readable", g.is_some(), o.is_some(), None)),
    }

    // ---- Device Library Plus -------------------------------------------
    dlp_findings(golden, ours, &mut out);

    // ---- exportExt.pdb (My Tag tables) ---------------------------------
    ext_findings(golden, ours, &mut out);

    // ---- everything else under /PIONEER ----------------------------------
    file_findings(golden, ours, &mut out);
    Ok(out)
}

fn pdb_findings(d: &PdbDiff, out: &mut StickDiff) {
    for x in &d.header {
        push(out, "pdb", None, Delta { field: format!("header.{}", x.field), ..x.clone() });
    }
    for t in &d.tables {
        for x in &t.deltas {
            push(out, "pdb", None, Delta { field: format!("{}.{}", t.name, x.field), ..x.clone() });
        }
    }
    for t in &d.tracks {
        for x in &t.deltas {
            push(out, "pdb.track", Some(&t.filename), x.clone());
        }
    }
    for f in &d.only_golden {
        push(out, "match", Some(f), delta("track", "present", "absent", None));
    }
    for f in &d.only_ours {
        push(out, "match", Some(f), delta("track", "absent", "present", None));
    }
}

// ---------------------------------------------------------------------------
// Playlists
// ---------------------------------------------------------------------------

/// One tree node as a player shows it: its path from the root, folder or
/// list, and its tracks by filename in play order.
#[derive(Debug, Clone, PartialEq)]
struct PlaylistNode {
    path: String,
    folder: bool,
    tracks: Vec<String>,
}

/// Depth-first by sibling sort order: the order a player lists the tree in.
fn playlist_view(nodes: &[RbPlaylist], tracks_of: impl Fn(u32) -> Vec<String>) -> Vec<PlaylistNode> {
    fn walk(
        nodes: &[RbPlaylist],
        parent: u32,
        prefix: &str,
        depth: usize,
        tracks_of: &dyn Fn(u32) -> Vec<String>,
        out: &mut Vec<PlaylistNode>,
    ) {
        if depth > 32 {
            return;
        }
        let mut kids: Vec<&RbPlaylist> = nodes.iter().filter(|p| p.parent_id == parent && p.id != parent).collect();
        kids.sort_by_key(|p| p.sort_order);
        for p in kids {
            let path = if prefix.is_empty() { p.name.clone() } else { format!("{prefix}/{}", p.name) };
            out.push(PlaylistNode {
                path: path.clone(),
                folder: p.is_folder,
                tracks: if p.is_folder { Vec::new() } else { tracks_of(p.id) },
            });
            if p.is_folder {
                walk(nodes, p.id, &path, depth + 1, tracks_of, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, "", 0, &tracks_of, &mut out);
    out
}

fn compare_playlists(area: &'static str, g: &[PlaylistNode], o: &[PlaylistNode], out: &mut StickDiff) {
    let paths = |v: &[PlaylistNode]| v.iter().map(|n| n.path.clone()).collect::<Vec<_>>();
    let (gp, op) = (paths(g), paths(o));
    for p in &gp {
        if !op.contains(p) {
            push(out, area, None, delta("node", p, "absent", None));
        }
    }
    for p in &op {
        if !gp.contains(p) {
            push(out, area, None, delta("node", "absent", p, None));
        }
    }
    let common = |a: &[String], b: &[String]| a.iter().filter(|p| b.contains(p)).cloned().collect::<Vec<_>>();
    if common(&gp, &op) != common(&op, &gp) {
        push(out, area, None, delta("order", gp.join(" | "), op.join(" | "), None));
    }
    for gn in g {
        let Some(on) = o.iter().find(|n| n.path == gn.path) else { continue };
        if gn.folder != on.folder {
            push(out, area, None, delta("folder", format!("{} {}", gn.path, gn.folder), on.folder, None));
        }
        if gn.tracks != on.tracks {
            let i = gn.tracks.iter().zip(&on.tracks).position(|(a, b)| a != b).unwrap_or(gn.tracks.len().min(on.tracks.len()));
            let at = |v: &[String]| v.get(i).cloned().unwrap_or_else(|| "-".into());
            push(
                out,
                area,
                None,
                delta(
                    "entries",
                    format!("{}: {} tracks, #{i} {}", gn.path, gn.tracks.len(), at(&gn.tracks)),
                    format!("{} tracks, #{i} {}", on.tracks.len(), at(&on.tracks)),
                    None,
                ),
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Device Library Plus
// ---------------------------------------------------------------------------

fn val(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(n) => n.to_string(),
        Value::Real(f) => f.to_string(),
        Value::Text(s) => s.clone(),
        Value::Blob(b) => format!("<blob {}>", b.len()),
    }
}

type Rows = (Vec<String>, Vec<Vec<Value>>);

fn rows(conn: &rusqlite::Connection, table: &str) -> Rows {
    let Ok(mut stmt) = conn.prepare(&format!("SELECT * FROM \"{table}\"")) else {
        return (Vec::new(), Vec::new());
    };
    let cols: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let n = cols.len();
    let rows = stmt
        .query_map([], |r| (0..n).map(|i| r.get::<_, Value>(i)).collect::<rusqlite::Result<Vec<_>>>())
        .map(|it| it.flatten().collect())
        .unwrap_or_default();
    (cols, rows)
}

fn tables(conn: &rusqlite::Connection) -> BTreeSet<String> {
    conn.prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .and_then(|mut s| s.query_map([], |r| r.get::<_, String>(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default()
}

/// id → name for one interned table.
fn names(conn: &rusqlite::Connection, table: &str, id_col: &str, name_col: &str) -> HashMap<i64, String> {
    conn.prepare(&format!("SELECT {id_col}, {name_col} FROM \"{table}\""))
        .and_then(|mut s| {
            s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
                .map(|it| it.flatten().collect())
        })
        .unwrap_or_default()
}

/// Content columns whose difference is expected, and why.
fn content_why(col: &str) -> Option<&'static str> {
    Some(match col {
        "masterDbId" => "one constant per rekordbox library",
        "masterContentId" => "random per export",
        "dateCreated" | "dateAdded" => "stamped with the export date",
        "analysisDataFilePath" => "follows the per-export id",
        "hasModified" | "cueUpdateCount" | "analysisDataUpdateCount" | "informationUpdateCount" => {
            "rekordbox edit counter"
        }
        "djPlayCount" => "player-side counter",
        _ => return None,
    })
}

/// Tables a player or rekordbox fills at runtime, not at export.
fn runtime_table(t: &str) -> bool {
    matches!(t, "history" | "history_content" | "recommendedLike")
}

fn dlp_findings(golden: &Path, ours: &Path, out: &mut StickDiff) {
    let open = |root: &Path| crate::dlp::open_read_only(&rb_dir(root).join("exportLibrary.db")).ok();
    let (Some(g), Some(o)) = (open(golden), open(ours)) else {
        return; // readability is already reported with the playlists
    };
    let (gt, ot) = (tables(&g), tables(&o));
    for t in gt.union(&ot) {
        if gt.contains(t) != ot.contains(t) {
            push(out, "dlp", None, delta(&format!("{t}.table"), gt.contains(t), ot.contains(t), None));
            continue;
        }
        if t == "content" {
            continue;
        }
        let ((gc, gr), (oc, or)) = (rows(&g, t), rows(&o, t));
        if gc != oc {
            push(out, "dlp", None, delta(&format!("{t}.columns"), gc.join(","), oc.join(","), None));
        }
        if gr.len() != or.len() {
            let why = runtime_table(t).then_some("filled by players at runtime");
            push(out, "dlp", None, delta(&format!("{t}.rows"), gr.len(), or.len(), why));
        } else if t == "key" {
            // Interned per export in first-seen order: the names must match,
            // not the ids.
            let names = |r: &[Vec<Value>]| r.iter().map(|x| x.get(1).map(val).unwrap_or_default()).collect::<BTreeSet<_>>();
            let (a, b) = (names(&gr), names(&or));
            if a != b {
                let j = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(" ");
                push(out, "dlp", None, delta("key.names", j(&a), j(&b), None));
            }
        } else if matches!(t.as_str(), "menuItem" | "category" | "sort" | "color") {
            // Fixed tables: rekordbox's rows verbatim, order included.
            let flat = |r: &[Vec<Value>]| r.iter().map(|x| x.iter().map(val).collect::<Vec<_>>().join(",")).collect::<Vec<_>>();
            let (a, b) = (flat(&gr), flat(&or));
            let mut sa = a.clone();
            let mut sb = b.clone();
            sa.sort();
            sb.sort();
            if sa != sb {
                let i = a.iter().zip(&b).position(|(x, y)| x != y).unwrap_or(0);
                push(out, "dlp", None, delta(&format!("{t}.values"), &a[i], &b[i], None));
            }
        }
    }
    // property: one row.
    let ((pc, pr), (_, por)) = (rows(&g, "property"), rows(&o, "property"));
    if let (Some(a), Some(b)) = (pr.first(), por.first()) {
        for (i, c) in pc.iter().enumerate() {
            let (x, y) = (a.get(i).map(val).unwrap_or_default(), b.get(i).map(val).unwrap_or_default());
            if x != y {
                let why = match c.as_str() {
                    "deviceName" => Some("volume name"),
                    "createdDate" => Some("stamped with the export date"),
                    "myTagMasterDBID" => Some("one constant per rekordbox library"),
                    _ => None,
                };
                push(out, "dlp", None, delta(&format!("property.{c}"), x, y, why));
            }
        }
    }
    content_findings(golden, &g, &o, out);
    my_tag_findings(&g, &o, out);
}

/// One content row per filename, id columns resolved to names.
fn content_by_file(conn: &rusqlite::Connection) -> BTreeMap<String, BTreeMap<String, String>> {
    let artist = names(conn, "artist", "artist_id", "name");
    let album = names(conn, "album", "album_id", "name");
    let genre = names(conn, "genre", "genre_id", "name");
    let label = names(conn, "label", "label_id", "name");
    let key = names(conn, "key", "key_id", "name");
    let (cols, rs) = rows(conn, "content");
    let mut out = BTreeMap::new();
    for r in rs {
        let mut m = BTreeMap::new();
        for (c, v) in cols.iter().zip(&r) {
            let id = if let Value::Integer(n) = v { *n } else { 0 };
            let resolved = |map: &HashMap<i64, String>| map.get(&id).cloned().unwrap_or_default();
            let s = match c.as_str() {
                "content_id" => continue,
                c if c.starts_with("artist_id_") => resolved(&artist),
                "album_id" => resolved(&album),
                "genre_id" => resolved(&genre),
                "label_id" => resolved(&label),
                "key_id" => resolved(&key),
                // Artwork ids are per export; what matters is having one.
                "image_id" => (id != 0).to_string(),
                _ => val(v),
            };
            m.insert(c.clone(), s);
        }
        let file = m.get("fileName").cloned().unwrap_or_default().to_lowercase();
        out.insert(file, m);
    }
    out
}

fn content_findings(golden: &Path, g: &rusqlite::Connection, o: &rusqlite::Connection, out: &mut StickDiff) {
    let (gc, oc) = (content_by_file(g), content_by_file(o));
    for (file, grow) in &gc {
        let Some(orow) = oc.get(file) else { continue };
        for (col, x) in grow {
            let y = orow.get(col).cloned().unwrap_or_default();
            if *x == y {
                continue;
            }
            let why = match col.as_str() {
                "path" if basename(x) == basename(&y) => Some("Ordnung keeps /Contents flat; rekordbox mirrors the source tree"),
                // rekordbox records its collection's size, which goes stale
                // when the library file is retagged; the copy on its own
                // stick is what Ordnung measured.
                "fileSize"
                    if grow.get("path").and_then(|p| std::fs::metadata(golden.join(p.trim_start_matches('/'))).ok()).map(|m| m.len().to_string())
                        == Some(y.clone()) =>
                {
                    Some("rekordbox recorded a stale size; ours is the file on its own stick")
                }
                "key_id" if super::key_notation_differs(x, &y) => {
                    Some("key notation is a rekordbox display setting; Ordnung writes Camelot")
                }
                c => content_why(c),
            };
            push(out, "dlp.content", Some(file), delta(col, x, y, why));
        }
    }
}

/// Each track's My Tags as "Category/Tag" names, by filename.
fn my_tags(conn: &rusqlite::Connection) -> BTreeMap<String, BTreeSet<String>> {
    let (_, tags) = rows(conn, "myTag");
    // myTag: id, sequenceNo, name, attribute, parent.
    let mut name_of: HashMap<i64, (String, i64)> = HashMap::new();
    for r in &tags {
        if let (Some(Value::Integer(id)), Some(name), Some(Value::Integer(parent))) = (r.first(), r.get(2), r.get(4)) {
            name_of.insert(*id, (val(name), *parent));
        }
    }
    let full = |id: i64| match name_of.get(&id) {
        Some((n, p)) => match name_of.get(p) {
            Some((pn, _)) => format!("{pn}/{n}"),
            None => n.clone(),
        },
        None => format!("#{id}"),
    };
    let files: HashMap<i64, String> = conn
        .prepare("SELECT content_id, fileName FROM content")
        .and_then(|mut s| s.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let (_, links) = rows(conn, "myTag_content");
    for r in links {
        if let (Some(Value::Integer(tag)), Some(Value::Integer(cid))) = (r.first(), r.get(1)) {
            let file = files.get(cid).cloned().unwrap_or_else(|| format!("#{cid}")).to_lowercase();
            out.entry(file).or_default().insert(full(*tag));
        }
    }
    out
}

fn my_tag_findings(g: &rusqlite::Connection, o: &rusqlite::Connection, out: &mut StickDiff) {
    let (gt, ot) = (my_tags(g), my_tags(o));
    let files: BTreeSet<&String> = gt.keys().chain(ot.keys()).collect();
    for f in files {
        let (a, b) = (gt.get(f).cloned().unwrap_or_default(), ot.get(f).cloned().unwrap_or_default());
        if a != b {
            let j = |s: &BTreeSet<String>| s.iter().cloned().collect::<Vec<_>>().join(", ");
            push(out, "dlp.myTag", Some(f), delta("tags", j(&a), j(&b), None));
        }
    }
}

// ---------------------------------------------------------------------------
// exportExt.pdb, artwork, files
// ---------------------------------------------------------------------------

fn ext_findings(golden: &Path, ours: &Path, out: &mut StickDiff) {
    let read = |root: &Path| std::fs::read(rb_dir(root).join("exportExt.pdb")).ok();
    let (g, o) = match (read(golden), read(ours)) {
        (Some(g), Some(o)) => (g, o),
        (g, o) => {
            if g.is_some() || o.is_some() {
                push(out, "ext", None, delta("file", g.is_some(), o.is_some(), None));
            }
            return;
        }
    };
    let (Ok(gh), Ok(oh)) = (header(&g), header(&o)) else {
        push(out, "ext", None, delta("header", "parsed", "unparseable", None));
        return;
    };
    if gh.dir.len() != oh.dir.len() {
        push(out, "ext", None, delta("num_tables", gh.dir.len(), oh.dir.len(), None));
    }
    for (i, &(ty, _, _)) in gh.dir.iter().enumerate() {
        let (_, gr) = table_rows(&g, &gh, ty);
        let (_, or) = table_rows(&o, &oh, ty);
        if gr.len() != or.len() {
            push(out, "ext", None, delta(&format!("table{i}.rows"), gr.len(), or.len(), None));
        }
    }
}

fn artwork(golden: &Path, gt: &crate::pdb::RbTrack, ours: &Path, ot: &crate::pdb::RbTrack, name: &str, out: &mut StickDiff) {
    match (&gt.artwork_path, &ot.artwork_path) {
        (None, None) => {}
        (Some(g), Some(o)) => {
            // rekordbox's JPEG encoder isn't ours, so the bytes never match;
            // the sizes a player expects must.
            for suffix in ["", "_m"] {
                let file = |root: &Path, p: &str| {
                    let p = p.trim_start_matches('/');
                    let p = match p.rsplit_once('.') {
                        Some((stem, ext)) => format!("{stem}{suffix}.{ext}"),
                        None => p.to_string(),
                    };
                    image::image_dimensions(root.join(p)).ok()
                };
                let (a, b) = (file(golden, g), file(ours, o));
                if a != b {
                    push(out, "artwork", Some(name), delta(&format!("size{suffix}"), format!("{a:?}"), format!("{b:?}"), None));
                }
            }
        }
        (g, o) => push(out, "artwork", Some(name), delta("present", g.is_some(), o.is_some(), None)),
    }
}

/// Files under `/PIONEER` outside the per-track folders (USBANLZ, Artwork)
/// and Ordnung's own `.orig` backups, relative and lowercased.
fn inventory(root: &Path) -> BTreeSet<String> {
    fn walk(dir: &Path, rel: &str, out: &mut BTreeSet<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with("._") || name == ".DS_Store" {
                continue;
            }
            let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
            let lower = r.to_lowercase();
            if lower == "usbanlz" || lower == "artwork" || lower.ends_with(".orig") {
                continue;
            }
            if e.path().is_dir() {
                out.insert(format!("{lower}/"));
                walk(&e.path(), &r, out);
            } else {
                out.insert(lower);
            }
        }
    }
    let mut out = BTreeSet::new();
    walk(&root.join("PIONEER"), "", &mut out);
    out
}

fn file_why(rel: &str) -> Option<&'static str> {
    let top = rel.split('/').next().unwrap_or("");
    match top {
        "devsetting.dat" | "mysetting.dat" | "mysetting2.dat" | "djmmysetting.dat" | "djprofile.nxs" => {
            Some("rekordbox My Settings (player preferences), not library data")
        }
        "cdj" | "log" | "mpj" | "extracted" => Some("written by players/rekordbox at runtime"),
        _ if rel.ends_with(".db-wal") || rel.ends_with(".db-shm") => {
            Some("SQLite WAL sidecar rekordbox leaves behind; Ordnung copies a checkpointed database")
        }
        _ => None,
    }
}

fn file_findings(golden: &Path, ours: &Path, out: &mut StickDiff) {
    let (g, o) = (inventory(golden), inventory(ours));
    for f in g.difference(&o) {
        push(out, "files", None, delta("present", f, "absent", file_why(f)));
    }
    for f in o.difference(&g) {
        push(out, "files", None, delta("present", "absent", f, None));
    }
}

// ---------------------------------------------------------------------------
// Coverage
// ---------------------------------------------------------------------------

/// What one stick exercises, as named counts in a fixed order: the feature
/// matrix a reference export should fill before its diff means much.
pub fn coverage(root: &Path) -> Result<Vec<(&'static str, usize)>, ReadError> {
    let ex = crate::pdb::read_stick(root)?;
    let mut c: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut add = |k: &'static str, n: usize| *c.entry(k).or_default() += n;
    for t in ex.tracks.values() {
        add("tracks", 1);
        let ext = t.file_path.rsplit('.').next().unwrap_or("").to_lowercase();
        add(
            match ext.as_str() {
                "mp3" => "format mp3",
                "aif" | "aiff" => "format aiff",
                "wav" => "format wav",
                "flac" => "format flac",
                "m4a" | "mp4" | "aac" => "format m4a",
                _ => "format other",
            },
            1,
        );
        add("with bpm", (t.tempo_centi_bpm > 0) as usize);
        add("with key", t.key.is_some() as usize);
        add("with genre", t.genre.is_some() as usize);
        add("with label", t.label.is_some() as usize);
        add("with year", (t.year > 0) as usize);
        add("with comment", (!t.comment.is_empty()) as usize);
        add("with rating", (t.rating > 0) as usize);
        add("with colour", (t.color_id > 0) as usize);
        add("with track number", (t.track_number > 0) as usize);
        add("with remixer/composer/orig. artist", (t.remixer.is_some() || t.composer.is_some() || t.original_artist.is_some()) as usize);
        add("with artwork", t.artwork_path.is_some() as usize);
        let Some(dat) = t.analyze_path.as_deref().map(|p| root.join(p.trim_start_matches('/'))) else { continue };
        let grid = crate::anlz::read_beatgrid(&dat);
        add("with beatgrid", (!grid.is_empty()) as usize);
        let tempos: BTreeSet<u32> = grid.iter().map(|b| (b.bpm * 100.0).round() as u32).collect();
        add("variable-tempo grid", (tempos.len() > 1) as usize);
        let cues = crate::anlz::read_cues(&dat);
        add("with hot cues", cues.iter().any(|q| q.is_hot()) as usize);
        add("with memory cues", cues.iter().any(|q| !q.is_hot()) as usize);
        for q in &cues {
            add(if q.is_hot() { "hot cues" } else { "memory cues" }, 1);
            add(if q.is_hot() { "hot loops" } else { "memory loops" }, q.is_loop() as usize);
            add("cue comments", q.label.is_some() as usize);
            add("cue colours", q.color.is_some() as usize);
        }
    }
    let nodes = &ex.playlists;
    add("playlists", nodes.iter().filter(|p| !p.is_folder).count());
    add("folders", nodes.iter().filter(|p| p.is_folder).count());
    add("nested nodes", nodes.iter().filter(|p| p.parent_id != 0).count());
    add("playlist entries", ex.entries.values().map(Vec::len).sum());
    if let Ok(conn) = crate::dlp::open_read_only(&rb_dir(root).join("exportLibrary.db")) {
        let tags = my_tags(&conn);
        add("tracks with My Tags", tags.len());
        add("My Tag links", tags.values().map(BTreeSet::len).sum());
    }
    const ORDER: &[&str] = &[
        "tracks", "format mp3", "format aiff", "format wav", "format flac", "format m4a", "format other",
        "with bpm", "with beatgrid", "variable-tempo grid", "with key",
        "with hot cues", "hot cues", "hot loops", "with memory cues", "memory cues", "memory loops",
        "cue comments", "cue colours",
        "with genre", "with label", "with year", "with comment", "with rating", "with colour",
        "with track number", "with remixer/composer/orig. artist", "with artwork",
        "tracks with My Tags", "My Tag links",
        "playlists", "folders", "nested nodes", "playlist entries",
    ];
    Ok(ORDER.iter().map(|k| (*k, c.get(k).copied().unwrap_or(0))).collect())
}
