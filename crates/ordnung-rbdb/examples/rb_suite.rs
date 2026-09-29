//! rekordbox reference suite: replay on Ordnung every song operation a
//! rekordbox reference export carries, export the result, and diff the two
//! USB images byte by byte.
//!
//! A reference is a verbatim copy of a stick rekordbox wrote, kept under
//! `testdata/rekordbox-reference/<name>/` (local only; the audio is too big
//! for git). Its tracked sidecar, `testdata/rekordbox-suite/<name>/`, holds
//! the reference's SHA-256 manifest and the accepted baseline. Legs:
//!
//! * **mirror** — scan and analyze the reference's own audio into a scratch
//!   catalog, then apply every user operation the reference shows (tags,
//!   rating, key, BPM, beatgrid, hot and memory cues and loops with their
//!   colours and comments, playlists and folders) through the same `Catalog`
//!   calls the GUI makes, export, and diff the stick against rekordbox's.
//! * **analysis** — export Ordnung's own detection untouched and grade BPM,
//!   key, grid phase and downbeat against rekordbox's.
//! * **edit** — apply cue, grid and playlist edits in place to a copy of the
//!   reference stick, read them back, and prove nothing else changed.
//! * **edit-mirror** — for a `before/` + `after/` reference pair, replay the
//!   before→after changes with the in-place editor and diff against `after`.
//!
//! A run compares its counts with `accepted.tsv` and fails on any new or
//! grown divergence (or a dropped analysis score). See
//! `testdata/rekordbox-suite/README.md`.
//!
//!   rb_suite run [NAME…] [--leg mirror|analysis|edit|edit-mirror] [--examples N]
//!   rb_suite capture STICK_ROOT NAME [--stage before|after] [--no-audio] [--replace]
//!   rb_suite accept [NAME…]
//!   rb_suite diff GOLDEN_ROOT OURS_ROOT
//!   rb_suite coverage STICK_ROOT

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ordnung_core::analysis::{self, ANALYZER_VERSION};
use ordnung_core::catalog::Catalog;
use ordnung_core::model::{Beat, Cue, Id};
use ordnung_core::{scan, Key};
use ordnung_rbdb::anlz;
use ordnung_rbdb::edit::{self, PlaylistOp};
use ordnung_rbdb::export::{export_usb_with, ExportMode, ExportOptions, PlayerTarget};
use ordnung_rbdb::golden::changed_pages;
use ordnung_rbdb::golden::stick::{coverage, diff_stick, Finding, StickDiff};
use ordnung_rbdb::pdb::{read_export, read_stick, RbExport, RbPlaylist, RbTrack};

type Res<T> = Result<T, String>;

/// A grid is in phase when our beats sit this close to rekordbox's (median).
const PHASE_MS: u64 = 10;

// ---------------------------------------------------------------------------
// Paths
// ---------------------------------------------------------------------------

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}
fn refs_dir() -> PathBuf {
    repo().join("testdata/rekordbox-reference")
}
fn suite_dir(name: &str) -> PathBuf {
    repo().join("testdata/rekordbox-suite").join(name)
}
fn work_dir(name: &str) -> PathBuf {
    let target = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from).unwrap_or_else(|| repo().join("target"));
    target.join("rb-suite").join(name)
}

fn pdb_of(root: &Path) -> PathBuf {
    root.join("PIONEER/rekordbox/export.pdb")
}

fn dat_of(root: &Path, t: &RbTrack) -> Option<PathBuf> {
    t.analyze_path.as_deref().map(|p| root.join(p.trim_start_matches('/')))
}

fn basename(p: &str) -> String {
    p.rsplit('/').next().unwrap_or(p).to_lowercase()
}

fn stamp(p: &Path) -> (u64, i64) {
    let m = std::fs::metadata(p).ok();
    let size = m.as_ref().map(|m| m.len()).unwrap_or(0);
    let mtime = m
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    (size, mtime)
}

/// Copy a tree file by file (APFS clones, so a same-volume copy of the
/// audio costs nothing). macOS AppleDouble `._*` files and `.DS_Store` are
/// Finder litter, not part of the image, and are skipped.
fn copy_tree(from: &Path, to: &Path) -> Res<u64> {
    let mut bytes = 0;
    std::fs::create_dir_all(to).map_err(|e| format!("{}: {e}", to.display()))?;
    for e in std::fs::read_dir(from).map_err(|e| format!("{}: {e}", from.display()))?.flatten() {
        let name = e.file_name();
        let n = name.to_string_lossy();
        if n.starts_with("._") || n == ".DS_Store" {
            continue;
        }
        let (src, dst) = (e.path(), to.join(&name));
        if src.is_dir() {
            bytes += copy_tree(&src, &dst)?;
        } else {
            bytes += std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
    }
    Ok(bytes)
}

fn fresh_dir(p: &Path) -> Res<()> {
    if p.exists() {
        std::fs::remove_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))?;
    }
    std::fs::create_dir_all(p).map_err(|e| format!("{}: {e}", p.display()))
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

enum Scenario {
    /// One rekordbox export: mirror, analysis and edit legs.
    Export(PathBuf),
    /// A stick before and after the user changed it in rekordbox.
    Edit { before: PathBuf, after: PathBuf },
}

fn scenario(name: &str) -> Res<Scenario> {
    let root = refs_dir().join(name);
    if pdb_of(&root).is_file() {
        Ok(Scenario::Export(root))
    } else if pdb_of(&root.join("before")).is_file() && pdb_of(&root.join("after")).is_file() {
        Ok(Scenario::Edit {
            before: root.join("before"),
            after: root.join("after"),
        })
    } else {
        Err(format!(
            "{}: no reference (needs PIONEER/rekordbox/export.pdb, or before/ and after/ stick images)",
            root.display()
        ))
    }
}

fn all_scenarios() -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(refs_dir())
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// `shasum -c` the reference against its tracked manifest(s). `Ok(false)`
/// when there is no manifest yet.
fn verify_manifest(name: &str) -> Res<bool> {
    let dir = suite_dir(name);
    let mut any = false;
    for (file, root) in [
        ("MANIFEST.sha256", refs_dir().join(name)),
        ("MANIFEST.before.sha256", refs_dir().join(name).join("before")),
        ("MANIFEST.after.sha256", refs_dir().join(name).join("after")),
    ] {
        let m = dir.join(file);
        if !m.is_file() {
            continue;
        }
        any = true;
        let out = std::process::Command::new("shasum")
            .args(["-a", "256", "-c", "--quiet"])
            .arg(&m)
            .current_dir(&root)
            .output()
            .map_err(|e| format!("shasum: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "reference {} no longer matches {}:\n{}",
                root.display(),
                m.display(),
                String::from_utf8_lossy(&out.stdout)
            ));
        }
    }
    Ok(any)
}

// ---------------------------------------------------------------------------
// Tally and baseline
// ---------------------------------------------------------------------------

/// One measured number. Divergence counts (every leg but analysis) must not
/// grow; analysis scores must not drop.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    leg: &'static str,
    key: String,
    value: usize,
}

fn higher_is_better(leg: &str) -> bool {
    leg == "analysis"
}

fn diff_rows(leg: &'static str, d: &StickDiff) -> Vec<Row> {
    d.tally()
        .into_iter()
        .filter(|(_, (_, bad))| *bad > 0)
        .map(|(key, (_, bad))| Row { leg, key, value: bad })
        .collect()
}

fn read_tsv(p: &Path) -> BTreeMap<(String, String), usize> {
    std::fs::read_to_string(p)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
        .filter_map(|l| {
            let mut it = l.split('\t');
            let (leg, key, v) = (it.next()?, it.next()?, it.next()?);
            Some(((leg.to_string(), key.to_string()), v.trim().parse().ok()?))
        })
        .collect()
}

fn write_tsv(p: &Path, rows: &[Row], head: &str) -> Res<()> {
    let mut s = String::from(head);
    for r in rows {
        s.push_str(&format!("{}\t{}\t{}\n", r.leg, r.key, r.value));
    }
    std::fs::write(p, s).map_err(|e| format!("{}: {e}", p.display()))
}

/// Regressions and improvements against the accepted baseline.
fn compare(rows: &[Row], accepted: &BTreeMap<(String, String), usize>, legs_run: &BTreeSet<&str>) -> (Vec<String>, Vec<String>) {
    let (mut worse, mut better) = (Vec::new(), Vec::new());
    let now: BTreeMap<(String, String), usize> =
        rows.iter().map(|r| ((r.leg.to_string(), r.key.clone()), r.value)).collect();
    let keys: BTreeSet<&(String, String)> = now.keys().chain(accepted.keys()).collect();
    for k in keys {
        if !legs_run.contains(k.0.as_str()) {
            continue;
        }
        let (a, n) = (accepted.get(k).copied().unwrap_or(0), now.get(k).copied().unwrap_or(0));
        let line = format!("{:<12} {:<44} {a} → {n}", k.0, k.1);
        let hib = higher_is_better(&k.0);
        if (hib && n < a) || (!hib && n > a) {
            worse.push(line);
        } else if n != a {
            better.push(line);
        }
    }
    (worse, better)
}

// ---------------------------------------------------------------------------
// The scratch library: the reference's audio, scanned and analyzed
// ---------------------------------------------------------------------------

struct Library {
    /// Catalog with scan + Ordnung's own analysis, never edited.
    analysis_db: PathBuf,
    /// Reference filename (lowercased) → catalog id and scratch audio path.
    by_file: BTreeMap<String, (Id, PathBuf)>,
    missing: Vec<String>,
}

/// Clone the reference's audio into the work dir (a user edit must never
/// reach the reference) and bring its catalog's analysis up to date.
fn build_library(work: &Path, reference: &Path, ex: &RbExport) -> Res<Library> {
    let audio = work.join("audio");
    let extra = std::env::var_os("RB_SUITE_AUDIO").map(PathBuf::from);
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    let mut missing = Vec::new();
    for t in ex.tracks.values() {
        let rel = t.file_path.trim_start_matches('/');
        let mut src = reference.join(rel);
        if !src.is_file() {
            if let Some(dir) = &extra {
                let alt = dir.join(rel.rsplit('/').next().unwrap_or(rel));
                if alt.is_file() {
                    src = alt;
                }
            }
        }
        if !src.is_file() {
            missing.push(basename(&t.file_path));
            continue;
        }
        let dst = audio.join(rel);
        if std::fs::metadata(&dst).map(|m| m.len()).ok() != std::fs::metadata(&src).map(|m| m.len()).ok() {
            std::fs::create_dir_all(dst.parent().unwrap()).map_err(|e| e.to_string())?;
            std::fs::copy(&src, &dst).map_err(|e| format!("{}: {e}", src.display()))?;
        }
        files.push((basename(&t.file_path), dst));
    }

    let analysis_db = work.join("analysis.db");
    let cat = Catalog::open(&analysis_db).map_err(|e| e.to_string())?;
    let mut by_file = BTreeMap::new();
    let mut pending = Vec::new();
    for (name, path) in &files {
        let scanned = scan::scan_file(path).map_err(|e| format!("scan {}: {e}", path.display()))?;
        let (id, _) = cat.upsert_scanned(&scanned).map_err(|e| e.to_string())?;
        let (size, mtime) = stamp(path);
        if cat.needs_analysis(id, size, mtime, ANALYZER_VERSION).map_err(|e| e.to_string())? {
            pending.push((id, path.clone(), size, mtime));
        }
        by_file.insert(name.clone(), (id, path.clone()));
    }
    if !pending.is_empty() {
        eprintln!("  analyzing {} track(s)…", pending.len());
        let workers = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4);
        let queue = std::sync::Mutex::new(pending.into_iter());
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::scope(|s| {
            for _ in 0..workers {
                let (queue, tx) = (&queue, tx.clone());
                s.spawn(move || loop {
                    let Some((id, path, size, mtime)) = queue.lock().unwrap().next() else { break };
                    let r = analysis::analyze_file(&path);
                    let _ = tx.send((id, path, size, mtime, r));
                });
            }
            drop(tx);
            for (id, path, size, mtime, r) in rx {
                match r {
                    Ok(a) => cat.save_analysis(id, &a, size, mtime).map_err(|e| e.to_string())?,
                    Err(e) => eprintln!("  analysis failed: {}: {e}", path.display()),
                }
            }
            Ok::<(), String>(())
        })?;
    }
    Ok(Library {
        analysis_db,
        by_file,
        missing,
    })
}

fn export_catalog(db: &Path, stick: &Path) -> Res<()> {
    let cat = Catalog::open(db).map_err(|e| e.to_string())?;
    let (tracks, playlists) = cat.export_selection(&[]).map_err(|e| e.to_string())?;
    fresh_dir(stick)?;
    let opts = ExportOptions {
        mode: ExportMode::Replace,
        player: PlayerTarget::Modern,
    };
    let report = export_usb_with(stick, &tracks, &playlists, opts, &mut |_| {}, &AtomicBool::new(false))
        .map_err(|e| format!("export: {e}"))?;
    for (id, why) in &report.skipped {
        eprintln!("  export skipped track {id}: {why}");
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Mirror leg
// ---------------------------------------------------------------------------

fn nonempty(s: &str) -> Option<String> {
    (!s.trim().is_empty()).then(|| s.to_string())
}

/// Playlist nodes parents-first, siblings by sort order.
fn tree_order(nodes: &[RbPlaylist]) -> Vec<&RbPlaylist> {
    let mut out = Vec::new();
    let mut frontier = vec![0u32];
    while let Some(parent) = frontier.pop() {
        let mut kids: Vec<&RbPlaylist> = nodes.iter().filter(|p| p.parent_id == parent && p.id != parent).collect();
        kids.sort_by_key(|p| p.sort_order);
        for k in kids {
            out.push(k);
            if k.is_folder {
                frontier.push(k.id);
            }
        }
    }
    out
}

/// Replay the reference's user data onto a copy of the analyzed catalog
/// through the GUI's own catalog operations. Returns what Ordnung can't
/// express (by feature, with counts).
fn apply_reference(cat: &Catalog, lib: &Library, reference: &Path, ex: &RbExport) -> Res<BTreeMap<&'static str, usize>> {
    let mut gaps: BTreeMap<&'static str, usize> = BTreeMap::new();
    let e = |e: ordnung_core::Error| e.to_string();
    let mut catalog_of: HashMap<u32, Id> = HashMap::new();
    for (rid, rt) in &ex.tracks {
        let Some((id, path)) = lib.by_file.get(&basename(&rt.file_path)) else { continue };
        catalog_of.insert(*rid, *id);
        let mut t = cat.get_track(*id).map_err(e)?;
        // Tags: exactly what the reference row shows; an empty field there is
        // one the user cleared (or never had), so it's cleared here too.
        let tags = &mut t.tags;
        tags.title = nonempty(&rt.title);
        tags.artist = rt.artist.clone();
        tags.album = rt.album.clone();
        tags.genre = rt.genre.clone();
        tags.label = rt.label.clone();
        // The export falls back to the publisher when there's no label.
        tags.publisher = rt.label.clone();
        tags.year = (rt.year > 0).then_some(rt.year);
        tags.comment = nonempty(&rt.comment);
        tags.rating = (rt.rating > 0).then_some(rt.rating);
        tags.track_number = (rt.track_number > 0).then_some(rt.track_number.min(u16::MAX as u32) as u16);
        tags.disc_number = (rt.disc_number > 0).then_some(rt.disc_number);
        tags.remixer = rt.remixer.clone();
        tags.composer = rt.composer.clone();
        tags.original_artist = rt.original_artist.clone();
        cat.update_tags(*id, &t.tags).map_err(e)?;
        if rt.color_id > 0 {
            *gaps.entry("track colour (no catalog field)").or_default() += 1;
        }

        // Key and BPM as analysis values, then the grid as a manual edit.
        let (size, mtime) = stamp(path);
        if let Some(mut a) = cat.get_analysis(*id).map_err(e)? {
            a.key = rt.key.as_deref().and_then(Key::parse);
            a.bpm = rt.bpm();
            cat.save_analysis(*id, &a, size, mtime).map_err(e)?;
        }
        let dat = dat_of(reference, rt);
        let grid = dat.as_deref().map(anlz::read_beatgrid).unwrap_or_default();
        if let Some(first) = grid.first() {
            let bpm = rt.bpm().unwrap_or(first.bpm);
            cat.set_manual_beatgrid(*id, first.position_ms, first.number, bpm).map_err(e)?;
            let tempos: BTreeSet<u16> = grid.iter().map(|b| (b.bpm * 100.0).round() as u16).collect();
            if tempos.len() > 1 {
                *gaps.entry("variable-tempo grid (catalog keeps one tempo)").or_default() += 1;
            }
        }

        let cues = dat.as_deref().map(anlz::read_cues).unwrap_or_default();
        if let Err(err) = cat.set_cues(*id, &cues) {
            eprintln!("  set_cues {}: {err}", rt.title);
            *gaps.entry("cue set the catalog rejects").or_default() += 1;
        }
    }

    // Playlists and folders, in tree order.
    for p in cat.list_playlists().map_err(e)? {
        cat.delete_playlist(p.id).map_err(e)?;
    }
    let mut ids: HashMap<u32, Id> = HashMap::new();
    for p in tree_order(&ex.playlists) {
        let parent = (p.parent_id != 0).then(|| ids.get(&p.parent_id).copied()).flatten();
        let id = cat.create_playlist(&p.name, parent, p.is_folder).map_err(e)?;
        ids.insert(p.id, id);
        if !p.is_folder {
            let members: Vec<Id> = ex
                .entries
                .get(&p.id)
                .map(|v| v.iter().filter_map(|t| catalog_of.get(t).copied()).collect())
                .unwrap_or_default();
            cat.add_tracks(id, &members).map_err(e)?;
        }
    }
    Ok(gaps)
}

fn mirror_leg(lib: &Library, reference: &Path, work: &Path, examples: usize, report: &mut String) -> Res<Vec<Row>> {
    let ex = read_stick(reference).map_err(|e| e.to_string())?;
    let db = work.join("mirror.db");
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
    }
    std::fs::copy(&lib.analysis_db, &db).map_err(|e| e.to_string())?;
    let gaps = {
        let cat = Catalog::open(&db).map_err(|e| e.to_string())?;
        apply_reference(&cat, lib, reference, &ex)?
    };
    let stick = work.join("mirror-stick");
    export_catalog(&db, &stick)?;
    let d = diff_stick(reference, &stick).map_err(|e| e.to_string())?;

    report.push_str(&format!("## mirror: {} of {} tracks matched\n\n", d.matched, ex.tracks.len()));
    if !gaps.is_empty() {
        report.push_str("Operations Ordnung cannot express (the diff shows their effect):\n");
        for (what, n) in &gaps {
            report.push_str(&format!("  {n:>4}  {what}\n"));
        }
        report.push('\n');
    }
    report.push_str(&coverage_table(reference, &stick));
    report.push_str(&shape_table(&d));
    report.push_str(&d.render(examples));
    report.push('\n');
    Ok(diff_rows("mirror", &d))
}

fn coverage_table(golden: &Path, ours: &Path) -> String {
    let (Ok(g), Ok(o)) = (coverage(golden), coverage(ours)) else {
        return String::new();
    };
    let mut s = String::from("coverage                                  rekordbox  ordnung\n");
    for ((k, a), (_, b)) in g.iter().zip(&o) {
        let mark = if a != b { " <" } else { "" };
        s.push_str(&format!("  {k:<38} {a:>9} {b:>8}{mark}\n"));
    }
    s.push('\n');
    s
}

fn shape_table(d: &StickDiff) -> String {
    let mut by: BTreeMap<&str, Vec<(f32, f32, bool)>> = BTreeMap::new();
    for (_, sh) in &d.shapes {
        by.entry(sh.section.as_str()).or_default().push((sh.r, sh.level, sh.ok()));
    }
    if by.is_empty() {
        return String::new();
    }
    let med = |mut v: Vec<f32>| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[v.len() / 2]
    };
    let mut s = String::from("waveforms                ok     median r  median level\n");
    for (sec, v) in by {
        let ok = v.iter().filter(|x| x.2).count();
        s.push_str(&format!(
            "  {sec:<20} {ok:>3}/{:<4} {:>8.3} {:>12.2}\n",
            v.len(),
            med(v.iter().map(|x| x.0).collect()),
            med(v.iter().map(|x| x.1).collect())
        ));
    }
    s.push('\n');
    s
}

// ---------------------------------------------------------------------------
// Analysis leg
// ---------------------------------------------------------------------------

/// Median |Δ| from each of our beats to rekordbox's nearest, and the share
/// of those pairs that agree on the bar position.
fn grid_fit(golden: &[Beat], ours: &[Beat]) -> Option<(u64, f32)> {
    if golden.is_empty() || ours.is_empty() {
        return None;
    }
    let (lo, hi) = (golden[0].position_ms, golden[golden.len() - 1].position_ms);
    let mut deltas = Vec::new();
    let mut agree = 0;
    for b in ours.iter().filter(|b| b.position_ms >= lo && b.position_ms <= hi) {
        let i = golden.partition_point(|g| g.position_ms < b.position_ms);
        let near = [i.checked_sub(1), Some(i)]
            .into_iter()
            .flatten()
            .filter_map(|j| golden.get(j))
            .min_by_key(|g| g.position_ms.abs_diff(b.position_ms))?;
        deltas.push(near.position_ms.abs_diff(b.position_ms));
        agree += (near.number == b.number) as usize;
    }
    if deltas.is_empty() {
        return None;
    }
    let n = deltas.len();
    deltas.sort();
    Some((deltas[n / 2], agree as f32 / n as f32))
}

fn camelot(name: &str) -> Option<(u8, bool)> {
    let c = Key::parse(name)?.camelot();
    Some((c.number, c.major))
}

fn analysis_leg(lib: &Library, reference: &Path, work: &Path, report: &mut String) -> Res<Vec<Row>> {
    let stick = work.join("analysis-stick");
    export_catalog(&lib.analysis_db, &stick)?;
    let g = read_export(&pdb_of(reference)).map_err(|e| e.to_string())?;
    let o = read_export(&pdb_of(&stick)).map_err(|e| e.to_string())?;
    let o_by: HashMap<String, &RbTrack> = o.tracks.values().map(|t| (basename(&t.file_path), t)).collect();

    let mut score: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut misses: Vec<String> = Vec::new();
    for gt in g.tracks.values() {
        let name = basename(&gt.file_path);
        let Some(ot) = o_by.get(&name) else { continue };
        *score.entry("tracks graded").or_default() += 1;
        let (gb, ob) = (gt.tempo_centi_bpm, ot.tempo_centi_bpm);
        if gb > 0 {
            *score.entry("bpm rekordbox has").or_default() += 1;
            *score.entry("bpm exact").or_default() += (gb == ob) as usize;
            *score.entry("bpm within 0.05").or_default() += (gb.abs_diff(ob) <= 5) as usize;
            let ratio = ob as f32 / gb as f32;
            *score.entry("bpm half/double").or_default() += ((ratio - 2.0).abs() < 0.02 || (ratio - 0.5).abs() < 0.005) as usize;
        }
        if let Some(gk) = gt.key.as_deref().and_then(camelot) {
            *score.entry("key rekordbox has").or_default() += 1;
            let ok = ot.key.as_deref().and_then(camelot);
            *score.entry("key exact").or_default() += (ok == Some(gk)) as usize;
            let near = ok.is_some_and(|(n, m)| (n == gk.0) || (m == gk.1 && (n % 12 + 1 == gk.0 || gk.0 % 12 + 1 == n)));
            *score.entry("key exact or neighbour").or_default() += near as usize;
            if ok != Some(gk) {
                misses.push(format!("  key  {name}: {} → {}", gt.key.as_deref().unwrap_or(""), ot.key.as_deref().unwrap_or("-")));
            }
        }
        let grids = (dat_of(reference, gt), dat_of(&stick, ot));
        if let (Some(a), Some(b)) = grids {
            let (ga, oa) = (anlz::read_beatgrid(&a), anlz::read_beatgrid(&b));
            if !ga.is_empty() {
                *score.entry("grid rekordbox has").or_default() += 1;
            }
            if let Some((med, agree)) = grid_fit(&ga, &oa) {
                let phase = med <= PHASE_MS;
                *score.entry("grid in phase").or_default() += phase as usize;
                *score.entry("grid in phase, downbeat right").or_default() += (phase && agree >= 0.9) as usize;
                if !phase || agree < 0.9 {
                    misses.push(format!("  grid {name}: median {med} ms off, downbeat agrees {:.0}%", agree * 100.0));
                }
            }
        }
    }
    report.push_str("## analysis: Ordnung's own detection vs rekordbox's\n\n");
    for (k, v) in &score {
        report.push_str(&format!("  {k:<32} {v}\n"));
    }
    report.push('\n');
    misses.sort();
    for m in misses {
        report.push_str(&m);
        report.push('\n');
    }
    report.push('\n');
    Ok(score
        .into_iter()
        .filter(|(k, _)| !k.ends_with("rekordbox has"))
        .map(|(k, v)| Row {
            leg: "analysis",
            key: k.to_string(),
            value: v,
        })
        .collect())
}

// ---------------------------------------------------------------------------
// Edit leg: in-place edits on a copy of the reference
// ---------------------------------------------------------------------------

/// A full cue set spread over the track: pads A–H (C a 4-beat loop), three
/// memory cues (one an 8-beat loop), each colour and label distinct.
fn synthetic_cues(grid: &[Beat], duration_ms: u64) -> Vec<Cue> {
    let beat_ms = grid.first().map(|b| 60_000.0 / b.bpm.max(1.0) as f64).unwrap_or(500.0);
    let at = |k: usize| -> u64 {
        match grid.get(k) {
            Some(b) => b.position_ms,
            None => ((k as f64 * beat_ms) as u64).min(duration_ms.saturating_sub(1000)),
        }
    };
    const RGB: [[u8; 3]; 8] = [
        [0xe6, 0x28, 0x28],
        [0xff, 0x8c, 0x00],
        [0xe6, 0xc8, 0x00],
        [0x28, 0xe2, 0x14],
        [0x00, 0xe0, 0xff],
        [0x14, 0x32, 0xff],
        [0xaa, 0x72, 0xff],
        [0xff, 0x12, 0x7b],
    ];
    let mut cues: Vec<Cue> = (0..8u8)
        .map(|slot| {
            let pos = at(16 + slot as usize * 32);
            Cue {
                hot_slot: Some(slot),
                position_ms: pos,
                loop_end_ms: (slot == 2).then(|| pos + (4.0 * beat_ms) as u64),
                label: Some(format!("Pad {}", (b'A' + slot) as char)),
                color: Some(RGB[slot as usize]),
            }
        })
        .collect();
    for (i, k) in [0usize, 64, 128].into_iter().enumerate() {
        let pos = at(k);
        cues.push(Cue {
            hot_slot: None,
            position_ms: pos,
            loop_end_ms: (i == 1).then(|| pos + (8.0 * beat_ms) as u64),
            label: Some(format!("Memory {}", i + 1)),
            color: None,
        });
    }
    cues
}

fn sorted_cues(mut v: Vec<Cue>) -> Vec<Cue> {
    v.sort_by_key(|c| (c.hot_slot.is_none(), c.hot_slot.unwrap_or(0), c.position_ms));
    v
}

/// Whether an in-place edit is allowed to have caused this finding.
fn edit_allows(f: &Finding, cue_files: &BTreeSet<String>, grid_files: &BTreeSet<String>) -> bool {
    let track = f.track.as_deref().unwrap_or("");
    let field = f.delta.field.as_str();
    let (cue, grid) = (cue_files.contains(track), grid_files.contains(track));
    let suite = |s: &str| s.contains("RB Suite");
    match f.area {
        "anlz" => {
            (cue && ["DAT.PCOB", "EXT.PCOB", "EXT.PCO2"].iter().any(|p| field.starts_with(p)))
                || (grid && ["DAT.PQTZ", "EXT.PQT2"].iter().any(|p| field.starts_with(p)))
        }
        "pdb.track" => (cue && field == "cue_update_count") || (grid && matches!(field, "analysis_update_count" | "tempo")),
        "pdb" => ["playlist_tree.", "playlist_entries.", "header.next_unused_page", "header.sequence", "tracks.page"]
            .iter()
            .any(|p| field.starts_with(p)),
        "playlists.pdb" | "playlists.dlp" => suite(&f.delta.golden) || suite(&f.delta.ours),
        "dlp.content" => (cue && field == "cueUpdateCount") || (grid && matches!(field, "analysisDataUpdateCount" | "bpmx100")),
        "dlp" => matches!(field, "playlist.rows" | "playlist_content.rows"),
        _ => false,
    }
}

fn edit_leg(reference: &Path, work: &Path, examples: usize, report: &mut String) -> Res<Vec<Row>> {
    let stick = work.join("edit-stick");
    fresh_dir(&stick)?;
    copy_tree(&reference.join("PIONEER"), &stick.join("PIONEER"))?;
    let ex = read_export(&pdb_of(&stick)).map_err(|e| e.to_string())?;
    let mut ids: Vec<u32> = ex.tracks.keys().copied().collect();
    ids.sort();

    let e = |e: ordnung_rbdb::pdb::ReadError| e.to_string();
    let (mut want_cues, mut want_grid) = (BTreeMap::new(), BTreeMap::new());
    let (mut cue_files, mut grid_files) = (BTreeSet::new(), BTreeSet::new());
    for (i, id) in ids.iter().enumerate() {
        let t = &ex.tracks[id];
        let Some(dat) = dat_of(&stick, t) else { continue };
        let grid = anlz::read_beatgrid(&dat);
        match i % 3 {
            0 => {
                let cues = synthetic_cues(&grid, t.duration_s as u64 * 1000);
                edit::write_stick_cues(&stick, *id, &dat, &cues).map_err(e)?;
                want_cues.insert(*id, (dat, sorted_cues(cues)));
                cue_files.insert(basename(&t.file_path));
            }
            1 if !grid.is_empty() => {
                let moved: Vec<Beat> = grid.iter().map(|b| Beat { position_ms: b.position_ms + 20, ..*b }).collect();
                edit::write_stick_beatgrid(&stick, *id, &dat, &moved).map_err(e)?;
                want_grid.insert(*id, (dat, moved));
                grid_files.insert(basename(&t.file_path));
            }
            _ => {}
        }
    }
    let paths: Vec<String> = ids.iter().take(10).map(|id| ex.tracks[id].file_path.trim_start_matches('/').to_string()).collect();
    let created = edit::edit_stick_playlists(&stick, &PlaylistOp::Create { name: "RB Suite".into(), parent_id: 0 }).map_err(e)?;
    let pid = created.new_id.ok_or("Create returned no id")?;
    edit::edit_stick_playlists(&stick, &PlaylistOp::AddTracks { id: pid, rel_paths: paths.clone() }).map_err(e)?;
    edit::edit_stick_playlists(&stick, &PlaylistOp::Rename { id: pid, name: "RB Suite renamed".into() }).map_err(e)?;
    let temp = edit::edit_stick_playlists(&stick, &PlaylistOp::Create { name: "RB Suite temp".into(), parent_id: 0 }).map_err(e)?;
    edit::edit_stick_playlists(&stick, &PlaylistOp::Delete { id: temp.new_id.ok_or("Create returned no id")? }).map_err(e)?;

    // ---- read-back --------------------------------------------------------
    let mut readback: Vec<String> = Vec::new();
    for (id, (dat, want)) in &want_cues {
        let got = anlz::read_cues(dat);
        if &got != want {
            readback.push(format!("cues of track {id}: wrote {} got {}", want.len(), got.len()));
        }
    }
    for (id, (dat, want)) in &want_grid {
        if &anlz::read_beatgrid(dat) != want {
            readback.push(format!("grid of track {id} did not read back"));
        }
    }
    let want_members: Vec<String> = paths.iter().map(|p| basename(p)).collect();
    for (view, got) in [
        ("pdb", read_export(&pdb_of(&stick)).map_err(e)?),
        ("stick", read_stick(&stick).map_err(e)?),
    ] {
        let find = |n: &str| got.playlists.iter().find(|p| p.name == n);
        match find("RB Suite renamed") {
            Some(p) => {
                let members: Vec<String> = got
                    .entries
                    .get(&p.id)
                    .map(|v| v.iter().filter_map(|t| got.tracks.get(t)).map(|t| basename(&t.file_path)).collect())
                    .unwrap_or_default();
                if members != want_members {
                    readback.push(format!("{view}: RB Suite renamed holds {} of 10 tracks in order", members.iter().zip(&want_members).filter(|(a, b)| a == b).count()));
                }
            }
            None => readback.push(format!("{view}: renamed playlist missing")),
        }
        if find("RB Suite temp").is_some() {
            readback.push(format!("{view}: deleted playlist still present"));
        }
    }

    // ---- nothing else moved -----------------------------------------------
    let d = diff_stick(reference, &stick).map_err(|e| e.to_string())?;
    let stray: Vec<&Finding> = d.findings.iter().filter(|f| !edit_allows(f, &cue_files, &grid_files)).collect();
    let before = std::fs::read(pdb_of(reference)).map_err(|e| e.to_string())?;
    let after = std::fs::read(pdb_of(&stick)).map_err(|e| e.to_string())?;
    let pages: Vec<(u32, u32)> = changed_pages(&before, &after)
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|(_, ty)| !matches!(*ty, 0 | 7 | 8 | u32::MAX))
        .collect();

    report.push_str(&format!(
        "## edit: {} cue sets, {} grid shifts, 5 playlist ops, in place on a copy\n\n",
        want_cues.len(),
        want_grid.len()
    ));
    for r in &readback {
        report.push_str(&format!("  !! read-back: {r}\n"));
    }
    for (page, ty) in &pages {
        report.push_str(&format!("  !! export.pdb page {page} (table {ty}) changed but the edit doesn't own it\n"));
    }
    let mut stray_diff = StickDiff::default();
    stray_diff.findings = stray.into_iter().cloned().collect();
    if stray_diff.findings.is_empty() && readback.is_empty() && pages.is_empty() {
        report.push_str("  every edit read back; nothing outside the edited sections changed\n");
    } else {
        report.push_str(&stray_diff.render(examples));
    }
    report.push('\n');

    let mut rows = vec![
        Row { leg: "edit", key: "read-back failures".into(), value: readback.len() },
        Row { leg: "edit", key: "foreign pdb pages changed".into(), value: pages.len() },
    ];
    for (key, (ok, bad)) in stray_diff.tally() {
        rows.push(Row { leg: "edit", key: format!("untouched {key}"), value: ok + bad });
    }
    rows.retain(|r| r.value > 0);
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Edit-mirror leg: before → after, replayed in place
// ---------------------------------------------------------------------------

fn edit_mirror_leg(before: &Path, after: &Path, work: &Path, examples: usize, report: &mut String) -> Res<Vec<Row>> {
    let stick = work.join("edit-mirror-stick");
    fresh_dir(&stick)?;
    copy_tree(&before.join("PIONEER"), &stick.join("PIONEER"))?;
    let e = |e: ordnung_rbdb::pdb::ReadError| e.to_string();
    let b = read_stick(&stick).map_err(e)?;
    let a = read_stick(after).map_err(e)?;
    let a_by: HashMap<String, (&u32, &RbTrack)> = a.tracks.iter().map(|(id, t)| (basename(&t.file_path), (id, t))).collect();
    let mut gaps: BTreeMap<String, usize> = BTreeMap::new();
    let mut gap = |s: String| *gaps.entry(s).or_default() += 1;

    for (bid, bt) in &b.tracks {
        let Some((_, at)) = a_by.get(&basename(&bt.file_path)) else {
            gap("track removed from the stick".into());
            continue;
        };
        let (Some(bd), Some(ad)) = (dat_of(&stick, bt), dat_of(after, at)) else { continue };
        let ac = anlz::read_cues(&ad);
        if anlz::read_cues(&bd) != ac {
            edit::write_stick_cues(&stick, *bid, &bd, &ac).map_err(e)?;
        }
        let ag = anlz::read_beatgrid(&ad);
        if anlz::read_beatgrid(&bd) != ag && !ag.is_empty() {
            edit::write_stick_beatgrid(&stick, *bid, &bd, &ag).map_err(e)?;
        }
        for (field, x, y) in [
            ("title", bt.title.clone(), at.title.clone()),
            ("artist", bt.artist.clone().unwrap_or_default(), at.artist.clone().unwrap_or_default()),
            ("album", bt.album.clone().unwrap_or_default(), at.album.clone().unwrap_or_default()),
            ("genre", bt.genre.clone().unwrap_or_default(), at.genre.clone().unwrap_or_default()),
            ("label", bt.label.clone().unwrap_or_default(), at.label.clone().unwrap_or_default()),
            ("key", bt.key.clone().unwrap_or_default(), at.key.clone().unwrap_or_default()),
            ("comment", bt.comment.clone(), at.comment.clone()),
            ("rating", bt.rating.to_string(), at.rating.to_string()),
            ("colour", bt.color_id.to_string(), at.color_id.to_string()),
            ("year", bt.year.to_string(), at.year.to_string()),
        ] {
            if x != y {
                gap(format!("in-place {field} edit (no Ordnung API)"));
            }
        }
    }
    let b_names: BTreeSet<String> = b.tracks.values().map(|t| basename(&t.file_path)).collect();
    for name in a_by.keys().filter(|n| !b_names.contains(*n)) {
        let _ = name;
        gap("track added to the stick (use a Merge export)".into());
    }

    // Playlists by name: delete what's gone, create what's new, append what
    // grew. Anything else (reorder, removal, folders) has no in-place op.
    let paths_of = |ex: &RbExport, pid: u32| -> Vec<String> {
        ex.entries
            .get(&pid)
            .map(|v| v.iter().filter_map(|t| ex.tracks.get(t)).map(|t| t.file_path.trim_start_matches('/').to_string()).collect())
            .unwrap_or_default()
    };
    for bp in b.playlists.iter().filter(|p| !p.is_folder) {
        if !a.playlists.iter().any(|p| p.name == bp.name && !p.is_folder) {
            edit::edit_stick_playlists(&stick, &PlaylistOp::Delete { id: bp.id }).map_err(e)?;
        }
    }
    for ap in tree_order(&a.playlists) {
        if ap.is_folder {
            if !b.playlists.iter().any(|p| p.name == ap.name && p.is_folder) {
                gap("new playlist folder (no in-place op)".into());
            }
            continue;
        }
        let want = paths_of(&a, ap.id);
        let now = read_stick(&stick).map_err(e)?;
        match now.playlists.iter().find(|p| p.name == ap.name && !p.is_folder) {
            None => {
                let parent = now.playlists.iter().find(|p| p.is_folder && a.playlists.iter().any(|q| q.id == ap.parent_id && q.name == p.name)).map(|p| p.id).unwrap_or(0);
                let c = edit::edit_stick_playlists(&stick, &PlaylistOp::Create { name: ap.name.clone(), parent_id: parent }).map_err(e)?;
                let id = c.new_id.ok_or("Create returned no id")?;
                edit::edit_stick_playlists(&stick, &PlaylistOp::AddTracks { id, rel_paths: want }).map_err(e)?;
            }
            Some(bp) => {
                let have = paths_of(&now, bp.id);
                if have == want {
                } else if want.starts_with(&have) {
                    edit::edit_stick_playlists(&stick, &PlaylistOp::AddTracks { id: bp.id, rel_paths: want[have.len()..].to_vec() }).map_err(e)?;
                } else {
                    gap("playlist reorder or track removal (no in-place op)".into());
                }
            }
        }
    }

    let d = diff_stick(after, &stick).map_err(|e| e.to_string())?;
    report.push_str("## edit-mirror: rekordbox's before→after changes, replayed in place\n\n");
    for (what, n) in &gaps {
        report.push_str(&format!("  {n:>4}  {what}\n"));
    }
    report.push('\n');
    report.push_str(&d.render(examples));
    report.push('\n');
    let mut rows = diff_rows("edit-mirror", &d);
    rows.extend(gaps.into_iter().map(|(k, v)| Row { leg: "edit-mirror", key: format!("unsupported {k}"), value: v }));
    Ok(rows)
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

fn run(names: Vec<String>, legs: Option<String>, examples: usize) -> Res<bool> {
    let names = if names.is_empty() { all_scenarios() } else { names };
    if names.is_empty() {
        return Err(format!(
            "no references under {} — capture one first (see testdata/rekordbox-suite/README.md)",
            refs_dir().display()
        ));
    }
    let mut ok = true;
    for name in names {
        eprintln!("== {name}");
        let sc = scenario(&name)?;
        let manifest = verify_manifest(&name)?;
        let work = work_dir(&name);
        std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
        let mut report = format!("# rb_suite {name}\n\n");
        if !manifest {
            report.push_str("(no MANIFEST.sha256 yet: the reference's bytes are not pinned)\n\n");
        }
        let want = |leg: &str| legs.as_deref().is_none_or(|l| l == leg);
        let mut rows = Vec::new();
        let mut legs_run: BTreeSet<&str> = BTreeSet::new();
        match &sc {
            Scenario::Export(root) => {
                if want("mirror") || want("analysis") {
                    let ex = read_export(&pdb_of(root)).map_err(|e| e.to_string())?;
                    let lib = build_library(&work, root, &ex)?;
                    if !lib.missing.is_empty() {
                        report.push_str(&format!("{} track(s) have no audio in the reference; skipped: {}\n\n", lib.missing.len(), lib.missing.join(", ")));
                    }
                    if want("analysis") {
                        eprintln!("  analysis leg");
                        rows.extend(analysis_leg(&lib, root, &work, &mut report)?);
                        legs_run.insert("analysis");
                    }
                    if want("mirror") {
                        eprintln!("  mirror leg");
                        rows.extend(mirror_leg(&lib, root, &work, examples, &mut report)?);
                        legs_run.insert("mirror");
                    }
                }
                if want("edit") {
                    eprintln!("  edit leg");
                    rows.extend(edit_leg(root, &work, examples, &mut report)?);
                    legs_run.insert("edit");
                }
            }
            Scenario::Edit { before, after } => {
                if want("edit-mirror") {
                    eprintln!("  edit-mirror leg");
                    rows.extend(edit_mirror_leg(before, after, &work, examples, &mut report)?);
                    legs_run.insert("edit-mirror");
                }
                if want("edit") {
                    eprintln!("  edit leg (on before/)");
                    rows.extend(edit_leg(before, &work, examples, &mut report)?);
                    legs_run.insert("edit");
                }
            }
        }
        write_tsv(&work.join("tally.tsv"), &rows, "# leg\tkey\tvalue (written by the last run)\n")?;
        std::fs::write(work.join("report.txt"), &report).map_err(|e| e.to_string())?;

        let accepted_path = suite_dir(&name).join("accepted.tsv");
        let (worse, better) = compare(&rows, &read_tsv(&accepted_path), &legs_run);
        println!("{name}: report {}", work.join("report.txt").display());
        for r in &rows {
            println!("  {:<12} {:<44} {}", r.leg, r.key, r.value);
        }
        if !accepted_path.is_file() {
            println!("  no accepted baseline yet: `rb_suite accept {name}` pins these numbers");
        }
        for l in &better {
            println!("  better  {l}");
        }
        for l in &worse {
            println!("  WORSE   {l}");
        }
        if !better.is_empty() && worse.is_empty() {
            println!("  improved: `rb_suite accept {name}` locks it in");
        }
        ok &= worse.is_empty();
    }
    Ok(ok)
}

fn capture(stick: &Path, name: &str, stage: Option<&str>, audio: bool, replace: bool) -> Res<()> {
    if !pdb_of(stick).is_file() {
        return Err(format!(
            "{}: no PIONEER/rekordbox/export.pdb here. Point STICK at the folder that holds \
             PIONEER/ (a mounted stick or a copy of one)",
            stick.display()
        ));
    }
    let running = std::process::Command::new("pgrep").args(["-x", "rekordbox"]).output().map(|o| o.status.success()).unwrap_or(false);
    if running {
        eprintln!("warning: rekordbox is running; it can rewrite a mounted stick's databases mid-copy");
    }
    let mut dest = refs_dir().join(name);
    if let Some(s) = stage {
        dest = dest.join(s);
    }
    if dest.exists() {
        if !replace {
            return Err(format!("{} exists; pass --replace to overwrite it", dest.display()));
        }
        std::fs::remove_dir_all(&dest).map_err(|e| e.to_string())?;
    }
    let n = copy_tree(&stick.join("PIONEER"), &dest.join("PIONEER"))?;
    eprintln!("copied PIONEER ({:.1} MB)", n as f64 / 1e6);
    if audio && stick.join("Contents").is_dir() {
        let n = copy_tree(&stick.join("Contents"), &dest.join("Contents"))?;
        eprintln!("copied Contents ({:.1} GB)", n as f64 / 1e9);
    }
    // Pin every PIONEER byte: a later run refuses a reference that changed.
    let sdir = suite_dir(name);
    std::fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
    let mut files = Vec::new();
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }
    walk(&dest.join("PIONEER"), &dest, &mut files);
    files.sort();
    let out = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .args(&files)
        .current_dir(&dest)
        .output()
        .map_err(|e| format!("shasum: {e}"))?;
    let manifest = sdir.join(match stage {
        Some(s) => format!("MANIFEST.{s}.sha256"),
        None => "MANIFEST.sha256".into(),
    });
    std::fs::write(&manifest, &out.stdout).map_err(|e| e.to_string())?;
    eprintln!("pinned {} files in {}", files.len(), manifest.display());
    print_coverage(&dest)
}

fn print_coverage(root: &Path) -> Res<()> {
    for (k, v) in coverage(root).map_err(|e| e.to_string())? {
        println!("  {k:<38} {v}");
    }
    Ok(())
}

fn accept(names: Vec<String>) -> Res<()> {
    let names = if names.is_empty() { all_scenarios() } else { names };
    for name in names {
        let tally = work_dir(&name).join("tally.tsv");
        let rows: Vec<Row> = read_tsv(&tally)
            .into_iter()
            .filter_map(|((leg, key), value)| {
                let leg = ["mirror", "analysis", "edit", "edit-mirror"].into_iter().find(|l| *l == leg)?;
                Some(Row { leg, key, value })
            })
            .collect();
        if !tally.is_file() {
            return Err(format!("{name}: no run to accept ({} missing)", tally.display()));
        }
        let sdir = suite_dir(&name);
        std::fs::create_dir_all(&sdir).map_err(|e| e.to_string())?;
        write_tsv(
            &sdir.join("accepted.tsv"),
            &rows,
            "# Accepted rb_suite baseline. analysis rows are scores (a drop fails);\n\
             # every other row counts unexplained divergences (a rise or a new row fails).\n\
             # Rewrite with `make rb-accept NAME=<name>` after a run you've reviewed.\n",
        )?;
        println!("{name}: accepted {} row(s)", rows.len());
    }
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    let value = |f: &str| args.iter().position(|a| a == f).and_then(|i| args.get(i + 1)).cloned();
    // Paths arrive unexpanded from `make STICK=~/…` (zsh leaves a `~` after
    // `=` alone, and the Makefile quotes it), so expand a leading `~` here.
    let home = std::env::var("HOME").unwrap_or_default();
    let expand = |a: &str| match a.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') => format!("{home}{rest}"),
        _ => a.to_string(),
    };
    let positional: Vec<String> = {
        let mut v = Vec::new();
        let mut skip = false;
        for a in args.iter().skip(1) {
            if skip {
                skip = false;
                continue;
            }
            if matches!(a.as_str(), "--leg" | "--stage" | "--examples") {
                skip = true;
                continue;
            }
            if !a.starts_with("--") {
                v.push(expand(a));
            }
        }
        v
    };
    let result = match args.first().map(String::as_str) {
        Some("run") => {
            let examples = value("--examples").and_then(|v| v.parse().ok()).unwrap_or(3);
            run(positional, value("--leg"), examples).map(|ok| if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE })
        }
        Some("capture") if positional.len() == 2 => capture(
            Path::new(&positional[0]),
            &positional[1],
            value("--stage").as_deref(),
            !flag("--no-audio"),
            flag("--replace"),
        )
        .map(|_| ExitCode::SUCCESS),
        Some("accept") => accept(positional).map(|_| ExitCode::SUCCESS),
        Some("diff") if positional.len() == 2 => diff_stick(Path::new(&positional[0]), Path::new(&positional[1]))
            .map_err(|e| e.to_string())
            .map(|d| {
                print!("{}", coverage_table(Path::new(&positional[0]), Path::new(&positional[1])));
                print!("{}", shape_table(&d));
                print!("{}", d.render(0));
                if d.unexplained().next().is_some() { ExitCode::FAILURE } else { ExitCode::SUCCESS }
            }),
        Some("coverage") if positional.len() == 1 => print_coverage(Path::new(&positional[0])).map(|_| ExitCode::SUCCESS),
        _ => {
            eprintln!(
                "usage:\n  rb_suite run [NAME…] [--leg mirror|analysis|edit|edit-mirror] [--examples N]\n  \
                 rb_suite capture STICK_ROOT NAME [--stage before|after] [--no-audio] [--replace]\n  \
                 rb_suite accept [NAME…]\n  rb_suite diff GOLDEN_ROOT OURS_ROOT\n  rb_suite coverage STICK_ROOT"
            );
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(code) => code,
        Err(e) => {
            eprintln!("rb_suite: {e}");
            ExitCode::FAILURE
        }
    }
}
