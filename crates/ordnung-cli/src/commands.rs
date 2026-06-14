//! Command implementations: thin policy + presentation over `ordnung-core`.

use anyhow::{bail, Context, Result};
use indicatif::{ProgressBar, ProgressStyle};
use ordnung_core::analysis::{self, AnalysisParams, ANALYZER_VERSION};
use ordnung_core::convert::{self, ConvertSpec};
use ordnung_core::model::Analysis;
use ordnung_core::{scan, tag, Catalog, Key, Mode, PitchClass, Tags};
use rayon::prelude::*;
use std::path::Path;

/// `scan <DIR>` — discover audio files and upsert them into the catalog.
pub fn scan(db: &Path, dir: &Path) -> Result<()> {
    if !dir.is_dir() {
        bail!("{} is not a directory", dir.display());
    }
    let catalog = Catalog::open(db).context("opening catalog")?;

    let files = scan::discover(dir);
    if files.is_empty() {
        println!("No audio files found under {}", dir.display());
        return Ok(());
    }

    let bar = ProgressBar::new(files.len() as u64);
    bar.set_style(
        ProgressStyle::with_template("{bar:40} {pos}/{len} {msg}")
            .unwrap()
            .progress_chars("=> "),
    );

    let (mut added, mut updated, mut failed) = (0u64, 0u64, 0u64);
    for path in &files {
        bar.set_message(
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        match scan::scan_file(path) {
            Ok(scanned) => match catalog.upsert_scanned(&scanned) {
                Ok((_, true)) => added += 1,
                Ok((_, false)) => updated += 1,
                Err(e) => {
                    bar.suspend(|| eprintln!("  catalog error for {}: {e}", path.display()));
                    failed += 1;
                }
            },
            Err(e) => {
                bar.suspend(|| eprintln!("  skipped {}: {e}", path.display()));
                failed += 1;
            }
        }
        bar.inc(1);
    }
    bar.finish_and_clear();

    println!(
        "Scanned {} file(s): {added} added, {updated} updated, {failed} skipped. \
Catalog now holds {} track(s).",
        files.len(),
        catalog.count()?
    );
    Ok(())
}

/// `ls [QUERY]` — print a table of catalog tracks with BPM + Camelot key.
pub fn ls(db: &Path, query: Option<&str>, limit: usize) -> Result<()> {
    let catalog = Catalog::open(db).context("opening catalog")?;
    let tracks = catalog.list_tracks(query, limit)?;
    if tracks.is_empty() {
        println!("No matching tracks. (Run `ordnung scan <DIR>` first.)");
        return Ok(());
    }

    println!(
        "{:>5}  {:<22} {:<30} {:>7} {:>6} {:>4}  GENRE",
        "ID", "ARTIST", "TITLE", "DUR", "BPM", "KEY"
    );
    for t in &tracks {
        let dur = t
            .properties
            .as_ref()
            .map(|p| fmt_duration(p.duration_ms))
            .unwrap_or_else(|| "-".into());
        let analysis = catalog.get_analysis(t.id)?;
        let bpm = analysis
            .as_ref()
            .and_then(|a| a.bpm)
            .map(|b| format!("{b:.0}"))
            .unwrap_or_else(|| "-".into());
        let key = analysis
            .as_ref()
            .and_then(|a| a.key)
            .map(|k| k.camelot().label())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:>5}  {:<22} {:<30} {:>7} {:>6} {:>4}  {}",
            t.id,
            truncate(t.tags.artist.as_deref().unwrap_or("-"), 22),
            truncate(t.tags.title.as_deref().unwrap_or("-"), 30),
            dur,
            bpm,
            key,
            t.tags.genre.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

/// `analyze [QUERY] [--force]` — detect BPM/key/beat/waveform in parallel, cached.
pub fn analyze(db: &Path, query: Option<&str>, force: bool) -> Result<()> {
    let catalog = Catalog::open(db).context("opening catalog")?;
    let tracks = catalog.list_tracks(query, 0)?;
    if tracks.is_empty() {
        println!("No matching tracks to analyze.");
        return Ok(());
    }

    // Decide which tracks need work (cheap stat-based cache check).
    let mut pending = Vec::new();
    for t in &tracks {
        let (size, mtime) = file_stamp(&t.source_path);
        if force || catalog.needs_analysis(t.id, size, mtime, ANALYZER_VERSION)? {
            pending.push((t.id, t.source_path.clone(), size, mtime));
        }
    }
    if pending.is_empty() {
        println!(
            "All {} matching track(s) already analyzed (analyzer v{ANALYZER_VERSION}). \
Use --force to redo.",
            tracks.len()
        );
        return Ok(());
    }

    let bar = ProgressBar::new(pending.len() as u64);
    bar.set_style(
        ProgressStyle::with_template("{bar:40} {pos}/{len} analyzing {msg}")
            .unwrap()
            .progress_chars("=> "),
    );

    // CPU-bound analysis runs in parallel; DB writes happen serially afterward.
    let params = AnalysisParams::default();
    let results: Vec<(u64, u64, i64, Result<Analysis, String>)> = pending
        .par_iter()
        .map(|(id, path, size, mtime)| {
            let r = analysis::analyze_file(path, params).map_err(|e| e.to_string());
            bar.inc(1);
            (*id, *size, *mtime, r)
        })
        .collect();
    bar.finish_and_clear();

    let (mut ok, mut failed) = (0u64, 0u64);
    for (id, size, mtime, result) in results {
        match result {
            Ok(a) => {
                catalog.save_analysis(id, &a, size, mtime)?;
                ok += 1;
            }
            Err(e) => {
                eprintln!("  analysis failed for track {id}: {e}");
                failed += 1;
            }
        }
    }
    println!("Analyzed {ok} track(s), {failed} failed. (analyzer v{ANALYZER_VERSION})");
    Ok(())
}

/// File size + mtime (unix secs) for the cache check; (0, 0) if unavailable.
fn file_stamp(path: &str) -> (u64, i64) {
    match std::fs::metadata(path) {
        Ok(m) => {
            let mtime = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            (m.len(), mtime)
        }
        Err(_) => (0, 0),
    }
}

/// `tag <ID> [--set field=val]... [--write [--art]]`
pub fn tag(db: &Path, id: u64, sets: &[String], write: bool, art: bool) -> Result<()> {
    let catalog = Catalog::open(db).context("opening catalog")?;
    let mut track = catalog.get_track(id)?;

    // Pure read: no edits and nothing to write back — just show the track.
    if sets.is_empty() && !write {
        print_track(&track);
        return Ok(());
    }

    for s in sets {
        let (field, value) = s
            .split_once('=')
            .with_context(|| format!("expected FIELD=VALUE, got `{s}`"))?;
        apply_field(&mut track.tags, field.trim(), value.trim())?;
    }

    if !sets.is_empty() {
        catalog.update_tags(id, &track.tags)?;
        println!("Updated catalog for track {id}.");
    }

    if write {
        // `--art` embeds the full-resolution external artwork the fetcher saved.
        let artwork = if art {
            let bytes = catalog
                .get_external_artwork_full(id)
                .context("reading external artwork")?;
            if bytes.is_none() {
                println!(
                    "(no fetched artwork for track {id} — run the artwork fetcher first; \
                     writing tags without embedding a cover)"
                );
            }
            bytes
        } else {
            None
        };

        tag::write_to_file(&track.source_path, &track.tags, artwork.as_deref())
            .with_context(|| format!("writing tags to {}", track.source_path))?;
        match artwork {
            Some(_) => println!("Wrote tags + cover art into {}", track.source_path),
            None => println!("Wrote tags into {}", track.source_path),
        }
    } else {
        println!("(catalog only — pass --write to update the source file)");
    }
    Ok(())
}

/// `playlist <ACTION>` — create and edit playlists / playlist folders.
pub fn playlist(db: &Path, action: crate::PlaylistCmd) -> Result<()> {
    use crate::PlaylistCmd::*;
    let catalog = Catalog::open(db).context("opening catalog")?;
    match action {
        New { name, folder, parent } => {
            let id = catalog.create_playlist(&name, parent, folder)?;
            let kind = if folder { "folder" } else { "playlist" };
            println!("Created {kind} {id}: {name}");
        }
        Ls => print_playlist_tree(&catalog)?,
        Show { id } => {
            let pl = catalog.get_playlist(id)?;
            if pl.is_folder {
                bail!("{id} is a folder; use `playlist ls` to see its contents");
            }
            println!("Playlist {} — {} ({} track(s))", pl.id, pl.name, pl.track_ids.len());
            if pl.track_ids.is_empty() {
                println!("  (empty — add tracks with `playlist add {id} <TRACK_ID>...`)");
            }
            for (i, tid) in pl.track_ids.iter().enumerate() {
                let t = catalog.get_track(*tid)?;
                println!(
                    "  {:>3}. [{}] {} — {}",
                    i + 1,
                    tid,
                    t.tags.artist.as_deref().unwrap_or("-"),
                    t.tags.title.as_deref().unwrap_or("-"),
                );
            }
        }
        Add { id, tracks } => {
            let n = catalog.add_tracks(id, &as_ids(&tracks))?;
            let skipped = tracks.len() - n;
            print!("Added {n} track(s) to playlist {id}.");
            println!("{}", if skipped > 0 { format!(" ({skipped} already present)") } else { String::new() });
        }
        Rm { id, tracks } => {
            let n = catalog.remove_tracks(id, &as_ids(&tracks))?;
            println!("Removed {n} track(s) from playlist {id}.");
        }
        Reorder { id, tracks } => {
            catalog.reorder_tracks(id, &as_ids(&tracks))?;
            println!("Reordered playlist {id}.");
        }
        Rename { id, name } => {
            catalog.rename_playlist(id, &name)?;
            println!("Renamed {id} to {name}.");
        }
        Mv { id, parent } => {
            catalog.move_playlist(id, parent)?;
            match parent {
                Some(p) => println!("Moved {id} under folder {p}."),
                None => println!("Moved {id} to the top level."),
            }
        }
        Delete { id } => {
            catalog.delete_playlist(id)?;
            println!("Deleted {id}.");
        }
    }
    Ok(())
}

fn as_ids(v: &[u64]) -> Vec<ordnung_core::model::Id> {
    v.iter().map(|&x| x as ordnung_core::model::Id).collect()
}

/// Render playlists/folders as an indented tree (folders first, then playlists).
fn print_playlist_tree(catalog: &Catalog) -> Result<()> {
    let all = catalog.list_playlists()?;
    if all.is_empty() {
        println!("No playlists yet. Create one with `playlist new <NAME>`.");
        return Ok(());
    }
    fn walk(all: &[ordnung_core::Playlist], parent: Option<ordnung_core::model::Id>, depth: usize) {
        for pl in all.iter().filter(|p| p.parent == parent) {
            let indent = "  ".repeat(depth);
            if pl.is_folder {
                println!("{indent}[{}] {}/", pl.id, pl.name);
                walk(all, Some(pl.id), depth + 1);
            } else {
                println!("{indent}[{}] {} ({} track(s))", pl.id, pl.name, pl.track_ids.len());
            }
        }
    }
    walk(&all, None, 0);
    Ok(())
}

/// `convert <ID>... --to <FMT>` — explicit transcode; new files unless --in-place.
#[allow(clippy::too_many_arguments)]
pub fn convert(
    db: &Path,
    ids: &[u64],
    to: &str,
    bitrate: Option<u32>,
    out: Option<&Path>,
    in_place: bool,
    yes: bool,
) -> Result<()> {
    let target = parse_format(to)?;
    if let Some(dir) = out {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let catalog = Catalog::open(db).context("opening catalog")?;
    let spec = ConvertSpec { target, bitrate_kbps: bitrate };

    // Resolve tracks up front so a bad id fails before any file is written.
    let tracks: Vec<_> = ids
        .iter()
        .map(|&id| catalog.get_track(id))
        .collect::<std::result::Result<_, _>>()?;

    let quality = spec
        .effective_bitrate()
        .map(|b| format!("{b} kbps"))
        .unwrap_or_else(|| "lossless".into());
    println!(
        "Converting {} track(s) → {} ({quality}){}.",
        tracks.len(),
        convert::target_extension(target),
        if in_place { ", replacing originals" } else { "" }
    );

    // In-place removes originals — confirm unless --yes.
    if in_place && !yes {
        for t in &tracks {
            println!("  will replace: {}", t.source_path);
        }
        if !confirm("Replace these source files in place?")? {
            println!("Aborted; nothing was changed.");
            return Ok(());
        }
    }

    let (mut ok, mut failed) = (0u64, 0u64);
    for t in &tracks {
        let src = Path::new(&t.source_path);
        let dest = convert::output_path_for(src, target, out);

        // Capture the original cover art before conversion: `ffmpeg -vn` drops the
        // embedded picture, and an in-place convert deletes the source, so we must
        // read it now to re-embed at full quality afterward.
        let cover = tag::read_front_cover_raw(src).unwrap_or(None);

        match convert::convert_file(src, &spec, &dest, in_place) {
            Ok(outcome) => {
                ok += 1;

                // Embed the catalog's metadata (the source of truth) into the new
                // file: full standardized tag set + cover, written ID3v2.3 for CDJ
                // compatibility. Audio already converted, so a tag failure is a
                // warning, not a hard error.
                if let Err(e) = tag::embed_full(&outcome.output_path, &t.tags, cover.as_ref()) {
                    eprintln!(
                        "  [{}] converted but could not embed metadata: {e}",
                        t.id
                    );
                }
                if outcome.replaced_source {
                    // Keep the catalog (source of truth) pointing at the new file.
                    match scan::scan_file(&outcome.output_path) {
                        Ok(scanned) => {
                            catalog.relink_source(
                                t.id,
                                &outcome.output_path.to_string_lossy(),
                                target,
                                &scanned.properties,
                            )?;
                            println!(
                                "  [{}] replaced → {}",
                                t.id,
                                outcome.output_path.display()
                            );
                        }
                        Err(e) => {
                            eprintln!(
                                "  [{}] converted {} but could not re-read properties: {e} \
(run `scan` to refresh)",
                                t.id,
                                outcome.output_path.display()
                            );
                        }
                    }
                } else {
                    println!("  [{}] wrote {}", t.id, outcome.output_path.display());
                }
            }
            Err(e) => {
                eprintln!("  [{}] {e}", t.id);
                failed += 1;
            }
        }
    }

    println!("Converted {ok} track(s), {failed} failed.");
    if ok > 0 && !in_place {
        println!("(new files are not yet cataloged — run `scan` on their folder to add them)");
    }
    Ok(())
}

/// Parse a target format name; rejects unknown formats and `other`.
fn parse_format(s: &str) -> Result<ordnung_core::Format> {
    use ordnung_core::Format::*;
    Ok(match s.to_lowercase().as_str() {
        "mp3" => Mp3,
        "aac" | "m4a" => Aac,
        "wav" => Wav,
        "aiff" | "aif" => Aiff,
        "flac" => Flac,
        other => bail!("unsupported target format `{other}` (mp3|aac|wav|aiff|flac)"),
    })
}

/// Prompt for a yes/no answer on stdin (defaults to no).
fn confirm(question: &str) -> Result<bool> {
    use std::io::Write;
    print!("{question} [y/N] ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    Ok(matches!(line.trim().to_lowercase().as_str(), "y" | "yes"))
}

/// `dupes` — report duplicate tracks (identical audio + same-song variants),
/// marking the best copy of each. Read-only: Ordnung never deletes files.
pub fn dupes(db: &Path) -> Result<()> {
    use ordnung_core::DuplicateKind;
    let catalog = Catalog::open(db).context("opening catalog")?;
    let groups = catalog.find_duplicates()?;
    if groups.is_empty() {
        println!("No duplicates found. ✓");
        return Ok(());
    }

    let identical: Vec<_> = groups.iter().filter(|g| g.kind == DuplicateKind::Identical).collect();
    let variants: Vec<_> = groups.iter().filter(|g| g.kind == DuplicateKind::SameTrack).collect();
    let acoustic: Vec<_> = groups.iter().filter(|g| g.kind == DuplicateKind::Acoustic).collect();

    if !identical.is_empty() {
        println!(
            "Identical audio — {} group(s), the same recording imported more than once:\n",
            identical.len()
        );
        for g in &identical {
            print_dupe_group(&g.tracks);
        }
    }
    if !variants.is_empty() {
        println!(
            "Same track, different files — {} group(s), likely re-encodes or format variants:\n",
            variants.len()
        );
        for g in &variants {
            print_dupe_group(&g.tracks);
        }
    }
    if !acoustic.is_empty() {
        println!(
            "Sounds identical — {} group(s) matched by audio fingerprint despite \
             differing files and tags (the kind you'd only catch on playback):\n",
            acoustic.len()
        );
        for g in &acoustic {
            print_dupe_group(&g.tracks);
        }
    }

    println!(
        "★ marks the best copy (clean audio over detected transcodes, then \
         lossless, then highest bitrate)."
    );
    println!(
        "Ordnung never deletes files — remove the copies you don't want, then run \
         `ordnung scan` (deleted files will otherwise show up under `ordnung missing`)."
    );
    Ok(())
}

/// Print one duplicate group: the artist/title once, then each copy with format,
/// bitrate, and path; the best copy gets a ★.
fn print_dupe_group(tracks: &[ordnung_core::Track]) {
    let head = &tracks[0];
    println!(
        "  {} — {}",
        head.tags.artist.as_deref().unwrap_or("-"),
        head.tags.title.as_deref().unwrap_or("-"),
    );
    let best = ordnung_core::best_copy_index(tracks);
    for (i, t) in tracks.iter().enumerate() {
        let star = if Some(i) == best { "★" } else { " " };
        let fmt = format!("{:?}", t.format).to_lowercase();
        let br = t
            .properties
            .as_ref()
            .and_then(|p| p.bitrate_kbps)
            .map(|b| format!("{b}k"))
            .unwrap_or_else(|| "-".into());
        println!("    {star} [{:>4}] {:>5} {:>6}  {}", t.id, fmt, br, t.source_path);
    }
    println!();
}

/// `missing` — list tracks whose source file is gone and suggest a fix. The
/// rekordbox "!" missing-files view, as a read-only report.
pub fn missing(db: &Path) -> Result<()> {
    let catalog = Catalog::open(db).context("opening catalog")?;
    let missing = catalog.missing_tracks()?;
    if missing.is_empty() {
        println!("All cataloged files are present. ✓");
        return Ok(());
    }

    println!(
        "{} track(s) point at files that no longer exist:\n",
        missing.len()
    );
    for t in &missing {
        println!(
            "  [{}] {} — {}",
            t.id,
            t.tags.artist.as_deref().unwrap_or("-"),
            t.tags.title.as_deref().unwrap_or("-"),
        );
        println!("        {}", t.source_path);
    }

    // If the gone files share a common directory, the user most likely moved
    // that folder — point them straight at `relink` (and the auto-rematch path).
    let paths: Vec<String> = missing.iter().map(|t| t.source_path.clone()).collect();
    match common_dir_prefix(&paths) {
        Some(prefix) => {
            println!("\nIf you moved that folder, repoint everything under it with:");
            println!("  ordnung relink \"{prefix}\" \"<NEW_LOCATION>\"");
            println!("Or rescan the new location to auto-rematch moved files by content:");
            println!("  ordnung scan <NEW_LOCATION>");
        }
        None => {
            println!("\nRepoint a moved folder with `ordnung relink <OLD> <NEW>`, or");
            println!("rescan its new location to auto-rematch moved files by content.");
        }
    }
    Ok(())
}

/// Longest directory path shared by every file in `paths` (component-wise, so it
/// never returns a partial path segment). `None` when the slice is empty or the
/// paths share no common directory. Used to suggest a `relink` prefix.
fn common_dir_prefix(paths: &[String]) -> Option<String> {
    let dirs: Vec<&Path> = paths.iter().filter_map(|p| Path::new(p).parent()).collect();
    let first = dirs.first()?;
    let mut common: Vec<_> = first.components().collect();
    for d in &dirs[1..] {
        let shared = common
            .iter()
            .zip(d.components())
            .take_while(|(a, b)| **a == *b)
            .count();
        common.truncate(shared);
        if common.is_empty() {
            return None;
        }
    }
    if common.is_empty() {
        return None;
    }
    let mut pb = std::path::PathBuf::new();
    for c in &common {
        pb.push(c.as_os_str());
    }
    Some(pb.to_string_lossy().into_owned())
}

/// `relink <FROM> <TO> [--dry-run]` — repoint every track under a moved/renamed
/// source folder. Catalog-only: never touches files on disk.
pub fn relink(db: &Path, from: &Path, to: &Path, dry_run: bool) -> Result<()> {
    let catalog = Catalog::open(db).context("opening catalog")?;
    let from = from.to_string_lossy();
    let to = to.to_string_lossy();
    let report = catalog
        .relink_prefix(&from, &to, dry_run)
        .context("repointing source paths")?;

    if report.moved == 0 && report.skipped == 0 {
        println!("No catalog tracks live under {from} — nothing to relink.");
        return Ok(());
    }

    // Preview the rewrites (cap the list so a huge library doesn't flood stdout).
    const SHOWN: usize = 20;
    for (old, new) in report.changes.iter().take(SHOWN) {
        println!("  {old}\n    → {new}");
    }
    if report.changes.len() > SHOWN {
        println!("  … and {} more", report.changes.len() - SHOWN);
    }

    if dry_run {
        println!(
            "Dry run: {} track(s) would be repointed{}. Re-run without --dry-run to apply.",
            report.moved,
            skipped_note(report.skipped)
        );
    } else {
        println!(
            "Repointed {} track(s) from {from} to {to}{}.",
            report.moved,
            skipped_note(report.skipped)
        );
    }
    Ok(())
}

/// "" or ", N skipped (path already in use)" for the relink summary line.
fn skipped_note(skipped: usize) -> String {
    if skipped == 0 {
        String::new()
    } else {
        format!(", {skipped} skipped (target path already in use)")
    }
}

/// `key <KEY>` — show notations for a musical key.
pub fn key(input: &str) -> Result<()> {
    let (note, mode) = match input.strip_suffix('m') {
        Some(n) => (n, Mode::Minor),
        None => (input, Mode::Major),
    };
    let semitone = parse_note(note)?;
    let k = Key::new(PitchClass::new(semitone), mode);
    println!("Camelot:   {}", k.camelot().label());
    println!("Open Key:  {}", k.open_key());
    println!("Classical: {}", k.classical());
    Ok(())
}

fn apply_field(tags: &mut Tags, field: &str, value: &str) -> Result<()> {
    let v = if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    };
    match field.to_lowercase().as_str() {
        "title" => tags.title = v,
        "artist" => tags.artist = v,
        "album" => tags.album = v,
        "genre" => tags.genre = v,
        "label" => tags.label = v,
        "comment" => tags.comment = v,
        "year" => tags.year = value.parse().ok(),
        "rating" => tags.rating = value.parse().ok(),
        other => bail!("unknown field `{other}` (title|artist|album|genre|label|comment|year|rating)"),
    }
    Ok(())
}

fn print_track(t: &ordnung_core::Track) {
    println!("Track {}", t.id);
    println!("  path:    {}", t.source_path);
    println!("  format:  {:?}", t.format);
    if let Some(p) = &t.properties {
        println!(
            "  audio:   {} Hz, {} ch, {}{}",
            p.sample_rate_hz,
            p.channels,
            fmt_duration(p.duration_ms),
            p.bitrate_kbps
                .map(|b| format!(", {b} kbps"))
                .unwrap_or_default()
        );
    }
    let g = &t.tags;
    let pr = |label: &str, v: &Option<String>| {
        if let Some(s) = v.as_deref().filter(|s| !s.is_empty()) {
            println!("  {label:<22} {s}");
        }
    };
    let prn = |label: &str, v: Option<u16>| {
        if let Some(n) = v {
            println!("  {label:<22} {n}");
        }
    };
    println!("  {:<22} {}", "title:", g.title.as_deref().unwrap_or("-"));
    println!("  {:<22} {}", "artist:", g.artist.as_deref().unwrap_or("-"));
    println!("  {:<22} {}", "album:", g.album.as_deref().unwrap_or("-"));
    pr("album_artist:", &g.album_artist);
    println!("  {:<22} {}", "genre:", g.genre.as_deref().unwrap_or("-"));
    pr("label:", &g.label);
    if let Some(y) = g.year {
        println!("  {:<22} {y}", "year:");
    }
    pr("recording_date:", &g.recording_date);
    pr("release_date:", &g.release_date);
    pr("original_release_date:", &g.original_release_date);
    if let (Some(n), Some(t)) = (g.track_number, g.track_total) {
        println!("  {:<22} {n} of {t}", "track:");
    } else {
        prn("track_number:", g.track_number);
    }
    if let (Some(n), Some(t)) = (g.disc_number, g.disc_total) {
        println!("  {:<22} {n} of {t}", "disc:");
    } else {
        prn("disc_number:", g.disc_number);
    }
    pr("composer:", &g.composer);
    pr("conductor:", &g.conductor);
    pr("remixer:", &g.remixer);
    pr("producer:", &g.producer);
    pr("lyricist:", &g.lyricist);
    pr("arranger:", &g.arranger);
    pr("performer:", &g.performer);
    pr("mix_dj:", &g.mix_dj);
    pr("writer:", &g.writer);
    if let Some(b) = g.bpm_tag {
        println!("  {:<22} {b:.1}", "bpm (file tag):");
    }
    pr("initial_key (file tag):", &g.initial_key_tag);
    pr("mood:", &g.mood);
    pr("grouping:", &g.grouping);
    if let Some(c) = g.compilation {
        println!("  {:<22} {}", "compilation:", if c { "yes" } else { "no" });
    }
    pr("isrc:", &g.isrc);
    pr("barcode:", &g.barcode);
    pr("catalog_number:", &g.catalog_number);
    pr("publisher:", &g.publisher);
    pr("copyright:", &g.copyright);
    pr("release_country:", &g.release_country);
    pr("subtitle:", &g.subtitle);
    pr("description:", &g.description);
    pr("language:", &g.language);
    pr("script:", &g.script);
    pr("work:", &g.work);
    pr("movement:", &g.movement);
    if let (Some(n), Some(t)) = (g.movement_number, g.movement_total) {
        println!("  {:<22} {n} of {t}", "movement_pos:");
    }
    pr("encoded_by:", &g.encoded_by);
    pr("encoder_software:", &g.encoder_software);
    pr("encoder_settings:", &g.encoder_settings);
    pr("original_artist:", &g.original_artist);
    pr("original_album:", &g.original_album);
    pr("mb_recording_id:", &g.musicbrainz_recording_id);
    pr("mb_track_id:", &g.musicbrainz_track_id);
    pr("mb_release_id:", &g.musicbrainz_release_id);
    pr("mb_release_group_id:", &g.musicbrainz_release_group_id);
    pr("mb_artist_id:", &g.musicbrainz_artist_id);
    pr("mb_release_artist_id:", &g.musicbrainz_release_artist_id);
    pr("mb_work_id:", &g.musicbrainz_work_id);
    pr("mb_release_type:", &g.musicbrainz_release_type);
    pr("acoust_id:", &g.acoust_id);
    if let Some(v) = g.replay_gain_track_gain {
        println!("  {:<22} {v:+.2} dB", "replay_gain_track:");
    }
    if let Some(v) = g.replay_gain_album_gain {
        println!("  {:<22} {v:+.2} dB", "replay_gain_album:");
    }
    pr("comment:", &g.comment);
    if g.lyrics.is_some() {
        println!("  {:<22} (present)", "lyrics:");
    }
    if g.has_cover {
        println!("  {:<22} yes", "cover_art:");
    }
}

fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Parse a note name (sharps or flats) into a semitone 0..11 (C = 0).
fn parse_note(s: &str) -> Result<u8> {
    let mut chars = s.chars();
    let letter = chars.next().context("empty key")?;
    let base: i8 = match letter.to_ascii_uppercase() {
        'C' => 0,
        'D' => 2,
        'E' => 4,
        'F' => 5,
        'G' => 7,
        'A' => 9,
        'B' => 11,
        _ => bail!("invalid note letter: {letter}"),
    };
    let accidental: i8 = match chars.next() {
        None => 0,
        Some('#') => 1,
        Some('b') => -1,
        Some(c) => bail!("invalid accidental: {c}"),
    };
    Ok((base + accidental).rem_euclid(12) as u8)
}

#[cfg(test)]
mod tests {
    use super::common_dir_prefix;

    fn p(paths: &[&str]) -> Option<String> {
        common_dir_prefix(&paths.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    }

    #[test]
    fn shared_folder_is_the_relink_prefix() {
        assert_eq!(
            p(&["/Music/Old/x.mp3", "/Music/Old/sub/y.mp3"]),
            Some("/Music/Old".to_string())
        );
    }

    #[test]
    fn divergent_paths_fall_back_to_their_common_ancestor() {
        // Only "/Music" is shared — and never a partial segment like "/Music/Old".
        assert_eq!(
            p(&["/Music/Old/x.mp3", "/Music/OldStuff/z.mp3"]),
            Some("/Music".to_string())
        );
    }

    #[test]
    fn external_volumes_share_their_mount_root() {
        // Two external drives still share /Volumes — relink from there is a
        // sensible (if broad) suggestion. An empty set has no prefix at all.
        assert_eq!(p(&["/Volumes/A/x.mp3", "/Volumes/B/y.mp3"]), Some("/Volumes".to_string()));
        assert_eq!(p(&[]), None);
    }
}
