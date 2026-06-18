//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    pub(crate) fn is_busy(&self) -> bool {
        self.job_rx.is_some()
    }

    /// Drain any pending worker messages. Returns true if we should reload rows.
    pub(crate) fn poll_worker(&mut self) -> bool {
        let Some(rx) = &self.job_rx else { return false };
        let mut reload = false;
        let mut finished = false;
        loop {
            match rx.try_recv() {
                Ok(JobMsg::Status(s)) => self.status = s,
                Ok(JobMsg::Progress { done, total }) => self.progress = Some((done, total)),
                Ok(JobMsg::Done(s)) => {
                    self.status = s;
                    finished = true;
                    reload = true;
                }
                Ok(JobMsg::Failed(s)) => {
                    self.status = format!("error: {s}");
                    finished = true;
                }
                Ok(JobMsg::Failures { title, items }) => {
                    // Arrives just before Done; pop the report so the user sees
                    // exactly which items failed and why.
                    self.show_failure_report = !items.is_empty();
                    self.failure_report_title = title;
                    self.failure_report = items;
                }
                Ok(JobMsg::ArtworkChoices(c)) => {
                    // Don't save yet — queue the candidates for the user to pick.
                    self.artwork_queue.push_back(c);
                }
                Ok(JobMsg::VinylUsername(u)) => {
                    // Persist the resolved username so the collection link works
                    // across launches. Only write when it actually changed.
                    if self.config.discogs_username != u {
                        self.config.discogs_username = u;
                        let _ = self.config.save();
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            }
        }
        if finished {
            self.job_rx = None;
            self.job_cancel = None;
            self.progress = None;
            // A write-edits job may have embedded fetched cover art into files;
            // the per-id texture cache survives `reload` (ids stay live), so
            // drop it here to force the new covers to re-decode on next render.
            if self.write_edits_running {
                self.write_edits_running = false;
                self.cover_cache.clear();
                self.cover_full_cache.clear();
                self.cover_inflight.clear();
            }
        }
        reload
    }

    pub(crate) fn spawn_scan(&mut self, ctx: egui::Context, dir: PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = format!("Scanning {}…", dir.display());
        let db = self.db_path.clone();
        let auto_analyze = self.config.auto_analyze;
        thread::spawn(move || run_scan(db, dir, cancel, tx, ctx, auto_analyze));
    }

    /// Import paths dropped onto the window from Finder (folders are walked,
    /// individual audio files taken as-is). Behaves exactly like "Add songs…".
    pub(crate) fn spawn_import(&mut self, ctx: egui::Context, paths: Vec<PathBuf>) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = format!("Importing {} dropped item(s)…", paths.len());
        let db = self.db_path.clone();
        let auto_analyze = self.config.auto_analyze;
        thread::spawn(move || run_import(db, paths, cancel, tx, ctx, auto_analyze));
    }

    /// Drop-to-import: shade the window while files hover over it, and scan
    /// anything dropped. A single image dropped directly onto a track row is
    /// instead routed to the cover-art flow (a confirm popup), so it never gets
    /// fed to the importer. Ignored while a job is already running, or while the
    /// cover-drop popup is already open, so a drop can't stomp either.
    pub(crate) fn handle_file_drop(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.is_busy() || self.cover_drop.is_some() {
            return;
        }
        // The row under the cursor right now (used both for the hover hint and to
        // route a dropped image to that track). `None` when the pointer is off any
        // row or outside the table.
        //
        // egui's own `latest_pos()` goes stale during an OS file drag (winit gets
        // no cursor events while a file hovers), so on macOS we poll the live mouse
        // location from AppKit instead and fall back to egui elsewhere.
        let pointer_pos = macos_drag::pointer_pos(frame)
            .or_else(|| ctx.input(|i| i.pointer.latest_pos()));
        let row_under_cursor = pointer_pos.and_then(|p| self.row_at(p));

        // Paths in `hovered_files` aren't always populated until the drop lands,
        // so any hovering file shows a hint. When the pointer is over a track row
        // we treat it as a cover drop (highlight just that row) *unless* the drag
        // is clearly audio — that's the one case we keep the full-screen import
        // overlay. macOS usually withholds the path on hover, so the type often
        // reads as "unknown"; defaulting an over-a-row hover to the cover hint
        // means dragging an image onto a song highlights the song instead of
        // darkening the whole window. The actual action on drop is still decided
        // by the dropped file's real path (image-on-row → cover, else import).
        let hovering = ctx.input(|i| !i.raw.hovered_files.is_empty());
        if hovering {
            // winit fires no events while a file hovers (no `draggingUpdated:`), so
            // keep repainting ourselves — otherwise the frame loop stalls and the
            // highlight freezes at wherever the cursor first entered the window.
            ctx.request_repaint();
            let cover_target = row_under_cursor.is_some() && !hovered_looks_like_audio(ctx);
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop-overlay"),
            ));
            // Highlight the targeted row so it's obvious which track gets the cover.
            if cover_target {
                if let Some((_, rect)) = self
                    .row_screen_rects
                    .iter()
                    .find(|(id, _)| Some(*id) == row_under_cursor)
                {
                    painter.rect_filled(
                        rect.expand(1.0),
                        3.0,
                        egui::Color32::from_rgba_unmultiplied(64, 110, 180, 90),
                    );
                    painter.rect_stroke(
                        rect.expand(1.0),
                        3.0,
                        egui::Stroke::new(2.0, egui::Color32::from_rgb(120, 170, 240)),
                    );
                    // A right-aligned hint inside the row so the gesture reads as
                    // "set this track's cover" rather than a catalog import.
                    painter.text(
                        rect.right_center() - egui::vec2(8.0, 0.0),
                        egui::Align2::RIGHT_CENTER,
                        "Set as cover",
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_rgb(120, 170, 240),
                    );
                }
            } else {
                painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
                painter.text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    "Drop music to add it to your catalog",
                    egui::FontId::proportional(22.0),
                    egui::Color32::WHITE,
                );
            }
        }
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if dropped.is_empty() {
            return;
        }
        // A single image dropped onto a track row → offer to set it as that
        // track's cover (with the option to apply across the album), rather than
        // importing. Anything else falls through to the normal import.
        let images: Vec<&PathBuf> = dropped.iter().filter(|p| is_image_path(p)).collect();
        if images.len() == 1 {
            if let Some(track_id) = row_under_cursor {
                let image = images[0].clone();
                self.open_cover_drop(ctx, track_id, image);
                return;
            }
        }
        self.spawn_import(ctx.clone(), dropped);
    }

    /// The track id of the visible row at `pos`. Reads the row rects recorded by
    /// `draw_table` this frame. Tolerant of the small gaps between rows: a `pos`
    /// inside the list's horizontal span and vertical extent but landing in
    /// inter-row padding snaps to the nearest row, so dragging a cover image down
    /// the list highlights a song continuously instead of flickering to the
    /// full-window import overlay between every row. `None` only when `pos` is
    /// outside the list area entirely.
    pub(crate) fn row_at(&self, pos: egui::Pos2) -> Option<Id> {
        if let Some((id, _)) = self
            .row_screen_rects
            .iter()
            .find(|(_, rect)| rect.contains(pos))
        {
            return Some(*id);
        }
        // No exact hit: snap to the nearest row, but only while the pointer is
        // within the list's horizontal span and vertical extent (so the toolbar,
        // sidebar, and the empty area below the last row still read as "import").
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        let mut top = f32::INFINITY;
        let mut bottom = f32::NEG_INFINITY;
        for (_, rect) in &self.row_screen_rects {
            left = left.min(rect.left());
            right = right.max(rect.right());
            top = top.min(rect.top());
            bottom = bottom.max(rect.bottom());
        }
        if pos.x < left || pos.x > right || pos.y < top || pos.y > bottom {
            return None;
        }
        self.row_screen_rects
            .iter()
            .min_by(|(_, a), (_, b)| {
                let da = (a.center().y - pos.y).abs();
                let db = (b.center().y - pos.y).abs();
                da.total_cmp(&db)
            })
            .map(|(id, _)| *id)
    }

    /// Search `dir` recursively for the source files of every track gone
    /// missing and repoint the catalog at the ones it confidently locates.
    /// Catalog-only; source files are never touched.
    pub(crate) fn spawn_relocate(&mut self, ctx: egui::Context, dir: PathBuf) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // a single directory walk; runs to completion
        self.status = format!("Searching {} for missing files…", dir.display());
        let db = self.db_path.clone();
        thread::spawn(move || run_relocate(db, dir, tx, ctx));
    }

    pub(crate) fn spawn_analyze(&mut self, ctx: egui::Context, force: bool) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // analyze runs in parallel; not interruptible
        self.status = "Analyzing…".into();
        let db = self.db_path.clone();
        let query = if self.filter.trim().is_empty() {
            None
        } else {
            Some(self.filter.clone())
        };
        thread::spawn(move || run_analyze(db, AnalyzeTargets::Query(query), force, tx, ctx));
    }

    /// Analyze a specific set of tracks (the context-menu selection) rather than
    /// the whole filtered view. Skips tracks already analyzed at the current
    /// version unless `force`.
    pub(crate) fn spawn_analyze_ids(&mut self, ctx: egui::Context, ids: Vec<Id>, force: bool) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // analyze runs in parallel; not interruptible
        self.status = "Analyzing…".into();
        let db = self.db_path.clone();
        thread::spawn(move || run_analyze(db, AnalyzeTargets::Ids(ids), force, tx, ctx));
    }

    /// Sync the local vinyl-collection cache from Discogs: pull the user's whole
    /// collection (folder 0), upsert metadata, prune records they've removed, and
    /// download covers we don't already have. Token resolution is policy and lives
    /// here; the worker only talks to Discogs and the catalog.
    pub(crate) fn spawn_refresh_vinyl(&mut self, ctx: egui::Context) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            self.status = "No Discogs token set. Add one in Settings \
                (https://www.discogs.com/settings/developers)."
                .into();
            self.settings_open = true;
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // a collection sync runs to completion
        self.status = "Syncing vinyl collection…".into();
        let db = self.db_path.clone();
        thread::spawn(move || run_refresh_vinyl(db, token, tx, ctx));
    }

    /// Walk every track that has no embedded *and* no external cover, ask
    /// Discogs for a thumbnail, and cache the result in the catalog. Token
    /// resolution and pacing are policy and live here (not in `discogs`).
    /// Launch a Discogs fetch run. `enrich = false` caches cover art only
    /// ("Fetch artwork"); `enrich = true` also fills the track's missing
    /// album-level tag fields from the release the user picks ("Fetch song
    /// data"). Both share the same search + candidate-picker flow; the flag only
    /// changes what happens when a candidate is saved (`save_selected_artwork`).
    pub(crate) fn spawn_fetch_artwork(&mut self, ctx: egui::Context, enrich: bool) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            self.status = "No Discogs token set. Add one in Settings \
                (https://www.discogs.com/settings/developers)."
                .into();
            self.settings_open = true;
            return;
        }
        self.artwork_enrich = enrich;
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = if enrich {
            "Fetching song data…".into()
        } else {
            "Fetching artwork…".into()
        };
        let db = self.db_path.clone();
        thread::spawn(move || run_fetch_artwork(db, token, cancel, tx, ctx, enrich));
    }

    /// Re-open the release picker for a single track (the "Edit release…" menu
    /// action): search Discogs for just this track and queue its candidates, in
    /// song-data mode so committing applies the chosen release's cover + tags.
    /// Bypasses the fetched-marker — this is an explicit, per-track re-pick.
    pub(crate) fn spawn_edit_release(&mut self, ctx: egui::Context, track_id: Id) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            self.status = "No Discogs token set. Add one in Settings \
                (https://www.discogs.com/settings/developers)."
                .into();
            self.settings_open = true;
            return;
        }
        self.artwork_enrich = true;
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = "Searching Discogs for releases…".into();
        let db = self.db_path.clone();
        // Always a song-data re-pick, so a no-match marks the track done.
        thread::spawn(move || run_fetch_tracks(db, token, vec![track_id], cancel, tx, ctx, true));
    }

    /// Fetch from Discogs for an explicit set of tracks (the right-click menu
    /// actions). `enrich = false` caches cover art only ("Fetch artwork");
    /// `enrich = true` also fills each track's empty tag fields ("Fetch song
    /// release details"). Searches every id and queues its candidates for the
    /// picker, ignoring the fetched-marker — these are deliberate per-track
    /// requests. The flag only changes what committing a pick writes.
    pub(crate) fn spawn_fetch_tracks(&mut self, ctx: egui::Context, ids: Vec<Id>, enrich: bool) {
        if ids.is_empty() {
            return;
        }
        let token = self.discogs_token();
        if token.trim().is_empty() {
            self.status = "No Discogs token set. Add one in Settings \
                (https://www.discogs.com/settings/developers)."
                .into();
            self.settings_open = true;
            return;
        }
        self.artwork_enrich = enrich;
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = if enrich {
            "Searching Discogs for song details…".into()
        } else {
            "Searching Discogs for artwork…".into()
        };
        let db = self.db_path.clone();
        thread::spawn(move || run_fetch_tracks(db, token, ids, cancel, tx, ctx, enrich));
    }

    pub(crate) fn spawn_convert(
        &mut self,
        ctx: egui::Context,
        modal: &ConvertModal,
    ) -> Result<(), String> {
        let bitrate_kbps = match modal.target {
            Format::Mp3 | Format::Aac => {
                let s = modal.bitrate_kbps.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(
                        s.parse::<u32>()
                            .map_err(|_| format!("invalid bitrate `{s}` (expected kbps)"))?,
                    )
                }
            }
            _ => None,
        };
        let spec = ConvertSpec {
            target: modal.target,
            bitrate_kbps,
        };
        if let Some(dir) = &modal.out_dir {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }

        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // a single ffmpeg run; not interruptible
        self.status = format!("Converting {}…", modal.track_label);

        let db = self.db_path.clone();
        let track_id = modal.track_id;
        let in_place = modal.in_place;
        let out_dir = modal.out_dir.clone();
        thread::spawn(move || run_convert(db, track_id, spec, out_dir, in_place, tx, ctx));
        Ok(())
    }

    /// Start a background batch conversion of `ids` to `target`. Validates the
    /// bitrate and creates the output folder up front so a bad value surfaces in
    /// the dialog rather than mid-run.
    pub(crate) fn spawn_batch_convert(
        &mut self,
        ctx: egui::Context,
        ids: Vec<Id>,
        target: Format,
        bitrate_raw: &str,
        out_dir: Option<PathBuf>,
        in_place: bool,
    ) -> Result<(), String> {
        let bitrate_kbps = match target {
            Format::Mp3 | Format::Aac => {
                let s = bitrate_raw.trim();
                if s.is_empty() {
                    None
                } else {
                    Some(
                        s.parse::<u32>()
                            .map_err(|_| format!("invalid bitrate `{s}` (expected kbps)"))?,
                    )
                }
            }
            _ => None,
        };
        if let Some(dir) = &out_dir {
            std::fs::create_dir_all(dir).map_err(|e| format!("creating {}: {e}", dir.display()))?;
        }
        let spec = ConvertSpec {
            target,
            bitrate_kbps,
        };

        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = format!("Converting {} track(s)…", ids.len());

        let db = self.db_path.clone();
        thread::spawn(move || run_batch_convert(db, ids, spec, out_dir, in_place, cancel, tx, ctx));
        Ok(())
    }

    /// Background job: write every `user_edited` track's tags into its source
    /// file, clearing the flag as each succeeds. Cancellable; reports progress
    /// and a final summary through the shared job channel.
    pub(crate) fn spawn_write_edits(&mut self, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = "Writing edits to source files…".into();
        self.write_edits_running = true;
        let db = self.db_path.clone();
        thread::spawn(move || run_write_edits(db, cancel, tx, ctx));
    }

    /// Background job: trash a reviewed batch of duplicate copies. `batch` is
    /// `(keeper id, drop id, source path)` per marked copy; each file goes to the
    /// system Trash (recoverable) and, on success, its catalog row is dropped with
    /// its playlist slots handed to the kept copy. Cancellable and non-blocking, so
    /// the Duplicates view stays interactive while it runs; `poll_worker` reloads
    /// (recomputing the groups) when it finishes.
    pub(crate) fn spawn_trash_marked(&mut self, ctx: egui::Context, batch: Vec<(Id, Id, PathBuf)>) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = format!("Trashing {} duplicate(s)…", batch.len());
        let db = self.db_path.clone();
        thread::spawn(move || run_trash_marked(db, batch, cancel, tx, ctx));
    }
}

pub(crate) fn run_scan(
    db: PathBuf,
    dir: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    auto_analyze: bool,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let files = scan::discover(&dir);
    if files.is_empty() {
        let _ = tx.send(JobMsg::Done(format!(
            "No audio files found under {}",
            dir.display()
        )));
        ctx.request_repaint();
        return;
    }
    let outcome = import_files(&catalog, &files, &cancel, &tx, &ctx);
    finish_import(&catalog, outcome, auto_analyze, &tx, &ctx);
}

/// Import a drag-and-drop of paths from Finder: directories are walked for audio
/// files, individual audio files are taken as-is, and anything else is ignored.
/// Shares the scan loop with `run_scan`, so drops behave exactly like "Add songs…".
pub(crate) fn run_import(
    db: PathBuf,
    paths: Vec<PathBuf>,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    auto_analyze: bool,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let mut files = Vec::new();
    for p in &paths {
        if p.is_dir() {
            files.extend(scan::discover(p));
        } else if scan::is_audio_file(p) {
            files.push(p.clone());
        }
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        let _ = tx.send(JobMsg::Done(
            "Nothing to import — the dropped items held no audio files.".into(),
        ));
        ctx.request_repaint();
        return;
    }
    let outcome = import_files(&catalog, &files, &cancel, &tx, &ctx);
    finish_import(&catalog, outcome, auto_analyze, &tx, &ctx);
}

/// What an import run touched, so the caller can report it and (optionally)
/// chain straight into analysis of the freshly added tracks.
pub(crate) struct ImportOutcome {
    /// Catalog ids of every track added or updated this run — the set handed to
    /// the auto-analysis pass. Excludes tracks skipped as unchanged.
    pub touched: Vec<Id>,
    /// Human-readable tally for the status line / final `Done` message.
    pub summary: String,
    /// True if the user cancelled mid-scan; suppresses the analysis chain.
    pub cancelled: bool,
}

/// Scan `files` into the catalog one by one, reporting determinate progress.
/// Honours `cancel`. Shared by "Add songs…" (`run_scan`) and drop-import
/// (`run_import`) so both paths behave identically. Returns the touched ids and
/// a summary without sending a terminal `Done` — `finish_import` owns that, so
/// it can chain analysis onto the same job first.
pub(crate) fn import_files(
    catalog: &Catalog,
    files: &[PathBuf],
    cancel: &AtomicBool,
    tx: &Sender<JobMsg>,
    ctx: &egui::Context,
) -> ImportOutcome {
    let total = files.len();
    let (mut added, mut updated, mut failed, mut unchanged) = (0u64, 0u64, 0u64, 0u64);
    // Ids of tracks added or updated this run, fed to auto-analysis afterward.
    let mut touched: Vec<Id> = Vec::new();
    // Per-file failures, with the reason, so the UI can report exactly what was
    // skipped instead of just a count.
    let mut skips: Vec<(String, String)> = Vec::new();
    let name_of = |path: &Path| {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    for (i, path) in files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            if !skips.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: "Scan".into(),
                    items: skips,
                });
            }
            return ImportOutcome {
                touched,
                summary: format!(
                    "Scan cancelled after {i}/{total}: {added} added, {updated} updated, \
                     {unchanged} unchanged, {failed} skipped."
                ),
                cancelled: true,
            };
        }
        // Skip files already in the catalog and unchanged on disk (same size +
        // mtime) — the expensive part is reading/decoding the file, so this makes
        // re-adding a folder near-instant. Self-healing: a row scanned before the
        // signature existed (NULL) reads as "changed" and is scanned once, which
        // records the signature, so it's skipped on the next pass.
        if let Some((size, mtime)) = scan::fs_signature(path) {
            if catalog
                .track_unchanged(&path.to_string_lossy(), size, mtime)
                .unwrap_or(false)
            {
                unchanged += 1;
                let _ = tx.send(JobMsg::Progress { done: i, total });
                continue;
            }
        }
        let _ = tx.send(JobMsg::Status(format!(
            "Scanning ({i}/{total}) {}",
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )));
        let _ = tx.send(JobMsg::Progress { done: i, total });
        ctx.request_repaint();
        match scan::scan_file(path) {
            Ok(s) => match catalog.upsert_scanned(&s) {
                Ok((id, true)) => {
                    added += 1;
                    touched.push(id);
                }
                Ok((id, false)) => {
                    updated += 1;
                    touched.push(id);
                }
                Err(e) => {
                    failed += 1;
                    skips.push((name_of(path), format!("catalog write failed: {e}")));
                }
            },
            Err(e) => {
                failed += 1;
                skips.push((name_of(path), format!("couldn't read file: {e}")));
            }
        }
    }
    let _ = tx.send(JobMsg::Progress { done: total, total });
    if !skips.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Scan".into(),
            items: skips,
        });
    }
    let unchanged_note = if unchanged > 0 {
        format!(", {unchanged} unchanged")
    } else {
        String::new()
    };
    ImportOutcome {
        touched,
        summary: format!(
            "Scanned {total} file(s): {added} added, {updated} updated{unchanged_note}, {failed} skipped."
        ),
        cancelled: false,
    }
}

/// Close out an import: either report the tally, or — when auto-analysis is on
/// and tracks were added/updated — chain straight into analyzing them on this
/// same job thread (so it stays one progress flow with one terminal `Done`).
/// Auto-analysis is GUI policy, mirroring the explicit "Analyze" action; core
/// stays explicit-only.
fn finish_import(
    catalog: &Catalog,
    outcome: ImportOutcome,
    auto_analyze: bool,
    tx: &Sender<JobMsg>,
    ctx: &egui::Context,
) {
    if outcome.cancelled || !auto_analyze || outcome.touched.is_empty() {
        let _ = tx.send(JobMsg::Done(outcome.summary));
        ctx.request_repaint();
        return;
    }
    // Resolve the touched ids to tracks; skip any that vanished since the scan.
    let tracks: Vec<Track> = outcome
        .touched
        .iter()
        .filter_map(|&id| catalog.get_track(id).ok())
        .collect();
    if tracks.is_empty() {
        let _ = tx.send(JobMsg::Done(outcome.summary));
        ctx.request_repaint();
        return;
    }
    // Lead the analysis tally with what was imported, so the one combined Done
    // reads e.g. "Scanned 5 file(s): … Analyzed 5 track(s), 0 failed."
    let lead = format!("{} ", outcome.summary);
    analyze_tracks(catalog, tracks, false, &lead, tx, ctx);
}

/// Locate moved source files and repoint the catalog at them. Reads the missing
/// tracks, searches `dir` by filename (content-fingerprint to break ties), and
/// relinks each confident match. The relink is a catalog row update — files are
/// never moved or modified.
pub(crate) fn run_relocate(db: PathBuf, dir: PathBuf, tx: Sender<JobMsg>, ctx: egui::Context) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let missing = match catalog.missing_tracks_detailed() {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("listing missing tracks: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    if missing.is_empty() {
        let _ = tx.send(JobMsg::Done("No tracks have a missing source file.".into()));
        ctx.request_repaint();
        return;
    }
    let total = missing.len();
    let _ = tx.send(JobMsg::Status(format!(
        "Searching {} for {total} missing file(s)…",
        dir.display()
    )));
    ctx.request_repaint();

    let found = scan::relocate_missing(&missing, &dir);
    let (mut relinked, mut failed) = (0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();
    let name_of = |path: &Path| {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    for r in &found {
        // Re-scan the located file so the relink refreshes format + audio props
        // (the file may differ from the catalog's stale record).
        match scan::scan_file(&r.new_path) {
            Ok(s) => {
                let new_path = r.new_path.to_string_lossy();
                match catalog.relink_source(r.id, &new_path, s.format, &s.properties) {
                    Ok(()) => relinked += 1,
                    Err(e) => {
                        failed += 1;
                        fails.push((name_of(&r.new_path), format!("couldn't relink: {e}")));
                    }
                }
            }
            Err(e) => {
                failed += 1;
                fails.push((
                    name_of(&r.new_path),
                    format!("couldn't read located file: {e}"),
                ));
            }
        }
    }
    let not_found = total as u64 - found.len() as u64;
    let mut msg = format!("Relocated {relinked} of {total} missing file(s)");
    if not_found > 0 {
        msg.push_str(&format!("; {not_found} not found under {}", dir.display()));
    }
    if failed > 0 {
        msg.push_str(&format!("; {failed} could not be relinked"));
    }
    msg.push('.');
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Relocate".into(),
            items: fails,
        });
    }
    let _ = tx.send(JobMsg::Done(msg));
    ctx.request_repaint();
}

pub(crate) fn run_write_edits(
    db: PathBuf,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let tracks = match catalog.list_edited_tracks() {
        Ok(t) => t,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(e.to_string()));
            ctx.request_repaint();
            return;
        }
    };
    if tracks.is_empty() {
        let _ = tx.send(JobMsg::Done("No edited tracks to write.".into()));
        ctx.request_repaint();
        return;
    }
    let total = tracks.len();
    let (mut written, mut failed) = (0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();
    let name_of = |path: &Path| {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    for (i, t) in tracks.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            if !fails.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: "Write edits".into(),
                    items: fails,
                });
            }
            let _ = tx.send(JobMsg::Done(format!(
                "Write cancelled after {i}/{total}: {written} written, {failed} failed."
            )));
            ctx.request_repaint();
            return;
        }
        let path = PathBuf::from(&t.source_path);
        let _ = tx.send(JobMsg::Status(format!(
            "Writing ({}/{total}) {}",
            i + 1,
            path.file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default()
        )));
        ctx.request_repaint();
        // Embed fetched cover art when this track has some; otherwise pass
        // `None`, which leaves any existing embedded cover untouched. Mirrors
        // the single-track `embed_cover_into_file` path.
        let art = catalog.get_external_artwork_full(t.id).ok().flatten();
        match tag::write_to_file(&path, &t.tags, art.as_deref()) {
            Ok(()) => {
                // Synced: drop the flag so it won't be written again next time.
                let _ = catalog.clear_user_edited(t.id);
                // If we embedded art, re-scan so the catalog's cover_thumb
                // reflects what now lives in the file (same reasoning as the
                // single-track embed path).
                if art.is_some() {
                    if let Ok(scanned) = scan::scan_file(&path) {
                        let _ = catalog.upsert_scanned(&scanned);
                    }
                }
                written += 1;
            }
            Err(e) => {
                failed += 1;
                fails.push((name_of(&path), format!("couldn't write tags: {e}")));
            }
        }
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Write edits".into(),
            items: fails,
        });
    }
    let _ = tx.send(JobMsg::Done(format!(
        "Wrote {written} track(s) to their source files{}.",
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    )));
    ctx.request_repaint();
}

/// Background worker behind "Delete N marked" in the Duplicates view. Trashes
/// every copy in `batch` (`(keeper id, drop id, source path)`) — moving the file
/// to the system Trash, then handing its playlist slots to the kept copy and
/// dropping its catalog row, but only for files that trashed cleanly. Reports
/// per-item progress, a failure report for any that couldn't be trashed, and a
/// final summary. Cancellable between items.
pub(crate) fn run_trash_marked(
    db: PathBuf,
    batch: Vec<(Id, Id, PathBuf)>,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let total = batch.len();
    let mut trashed = 0usize;
    let mut fails: Vec<(String, String)> = Vec::new();
    let name_of = |path: &Path| {
        path.file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string())
    };
    for (i, (keeper, drop, path)) in batch.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            if !fails.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: "Delete duplicates".into(),
                    items: fails,
                });
            }
            let _ = tx.send(JobMsg::Done(format!(
                "Delete cancelled after {i}/{total}: {trashed} trashed."
            )));
            ctx.request_repaint();
            return;
        }
        let _ = tx.send(JobMsg::Progress { done: i, total });
        let _ = tx.send(JobMsg::Status(format!(
            "Trashing ({}/{total}) {}",
            i + 1,
            name_of(path)
        )));
        ctx.request_repaint();
        match trash::delete(path) {
            Ok(()) => {
                // Only after the file is safely in the Trash. When a keeper
                // survives the group, hand the trashed copy's playlist slots to it,
                // then drop its catalog row. When the whole group was marked there's
                // no keeper (`keeper == drop`, the staging sentinel): delete the row
                // outright so its playlist slots and analysis cascade away.
                if keeper == drop {
                    let _ = catalog.delete_tracks(&[*drop]);
                } else {
                    let _ = catalog.replace_tracks(&[(*keeper, *drop)]);
                }
                trashed += 1;
            }
            Err(e) => fails.push((name_of(path), e.to_string())),
        }
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Delete duplicates".into(),
            items: fails,
        });
    }
    let failed = total - trashed;
    let _ = tx.send(JobMsg::Done(format!(
        "Trashed {trashed} duplicate(s){}.",
        if failed > 0 {
            format!(", {failed} failed")
        } else {
            String::new()
        }
    )));
    ctx.request_repaint();
}

/// What `run_analyze` should operate on: the current filtered view (`Query`) or
/// an explicit set of track ids (`Ids`, from the right-click selection).
pub(crate) enum AnalyzeTargets {
    Query(Option<String>),
    Ids(Vec<Id>),
}

pub(crate) fn run_analyze(
    db: PathBuf,
    targets: AnalyzeTargets,
    force: bool,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let tracks = match targets {
        AnalyzeTargets::Query(query) => match catalog.list_tracks(query.as_deref(), 0) {
            Ok(t) => t,
            Err(e) => {
                let _ = tx.send(JobMsg::Failed(e.to_string()));
                ctx.request_repaint();
                return;
            }
        },
        // Resolve each id; silently skip any that vanished since the menu opened.
        AnalyzeTargets::Ids(ids) => ids
            .iter()
            .filter_map(|&id| catalog.get_track(id).ok())
            .collect(),
    };
    if tracks.is_empty() {
        let _ = tx.send(JobMsg::Done("No matching tracks to analyze.".into()));
        ctx.request_repaint();
        return;
    }
    analyze_tracks(&catalog, tracks, force, "", &tx, &ctx);
}

/// Analyze `tracks` in parallel, skipping any already current at this analyzer
/// version (unless `force`), then save each result. Sends progress and exactly
/// one terminal `Done`, whose message is prefixed with `lead` (empty for a
/// standalone analyze; the import tally when chained after a scan). Shared by
/// the explicit "Analyze" action and auto-analysis-on-import.
fn analyze_tracks(
    catalog: &Catalog,
    tracks: Vec<Track>,
    force: bool,
    lead: &str,
    tx: &Sender<JobMsg>,
    ctx: &egui::Context,
) {
    let mut pending = Vec::new();
    for t in &tracks {
        let (size, mtime) = file_stamp(&t.source_path);
        match catalog.needs_analysis(t.id, size, mtime, ANALYZER_VERSION) {
            Ok(true) if !force => pending.push((t.id, t.source_path.clone(), size, mtime)),
            Ok(_) if force => pending.push((t.id, t.source_path.clone(), size, mtime)),
            _ => {}
        }
    }
    if pending.is_empty() {
        let _ = tx.send(JobMsg::Done(format!(
            "{lead}All {} track(s) already analyzed.",
            tracks.len()
        )));
        ctx.request_repaint();
        return;
    }
    let total = pending.len();
    let _ = tx.send(JobMsg::Status(format!("Analyzing {total} track(s)…")));
    let _ = tx.send(JobMsg::Progress { done: 0, total });
    ctx.request_repaint();

    // Analysis runs in parallel; a shared atomic counts completions so the
    // progress bar advances as tracks finish. `map_init` hands each rayon worker
    // its own `Sender` clone (the channel sender isn't `Sync`); the egui context
    // and the counter are `Sync`, so they're shared by reference.
    let params = AnalysisParams::default();
    let done = AtomicUsize::new(0);
    let results: Vec<(u64, u64, i64, Result<Analysis, String>)> = pending
        .par_iter()
        .map_init(
            || tx.clone(),
            |tx_local, (id, path, size, mtime)| {
                let r = analysis::analyze_file(path, params).map_err(|e| e.to_string());
                let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                let _ = tx_local.send(JobMsg::Progress { done: n, total });
                ctx.request_repaint();
                (*id, *size, *mtime, r)
            },
        )
        .collect();

    let (mut ok, mut failed) = (0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();
    // Map id -> source path so a failure can be reported by file name.
    let name_for = |id: u64| -> String {
        pending
            .iter()
            .find(|(pid, ..)| *pid == id)
            .map(|(_, path, ..)| {
                Path::new(path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| path.clone())
            })
            .unwrap_or_else(|| format!("track {id}"))
    };
    for (id, size, mtime, result) in results {
        match result {
            Ok(a) => match catalog.save_analysis(id, &a, size, mtime) {
                Ok(()) => ok += 1,
                Err(e) => {
                    failed += 1;
                    fails.push((name_for(id), format!("couldn't save analysis: {e}")));
                }
            },
            Err(e) => {
                failed += 1;
                fails.push((name_for(id), format!("analysis failed: {e}")));
            }
        }
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Analyze".into(),
            items: fails,
        });
    }
    let _ = tx.send(JobMsg::Done(format!(
        "{lead}Analyzed {ok} track(s), {failed} failed."
    )));
    ctx.request_repaint();
}

/// Sync the local vinyl-collection cache from the user's Discogs collection.
/// Fetches the full collection (paced by the Discogs client), upserts every
/// record, prunes any the user removed since the last sync, then downloads
/// covers for records that don't have one cached yet. Covers stream in with
/// determinate progress so the grid fills as the run proceeds.
pub(crate) fn run_refresh_vinyl(
    db: PathBuf,
    token: String,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung");

    let _ = tx.send(JobMsg::Status("Fetching Discogs collection…".into()));
    ctx.request_repaint();
    // Resolve the username up front so we can report it back for the collection
    // link, then reuse it for the fetch (no second identity request).
    let username = match client.identity() {
        Ok(u) => u,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("resolving Discogs account: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let _ = tx.send(JobMsg::VinylUsername(username.clone()));
    let records = match client.fetch_collection_for(&username) {
        Ok(r) => r,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("fetching collection: {e}")));
            ctx.request_repaint();
            return;
        }
    };

    // Upsert metadata and prune records dropped from the collection, so the
    // cache mirrors Discogs exactly. Cover bytes survive the metadata upsert.
    let mut keep = Vec::with_capacity(records.len());
    for rec in &records {
        let _ = catalog.upsert_vinyl(rec);
        keep.push(rec.instance_id);
    }
    let removed = catalog.prune_vinyl_not_in(&keep).unwrap_or(0);

    // Download covers we don't already have, reporting progress as we go.
    let missing = catalog.vinyl_missing_covers().unwrap_or_default();
    let total = missing.len();
    let mut fetched = 0usize;
    for (i, (instance_id, url)) in missing.iter().enumerate() {
        let _ = tx.send(JobMsg::Status(format!(
            "Downloading vinyl covers… ({}/{total})",
            i + 1
        )));
        let _ = tx.send(JobMsg::Progress { done: i, total });
        ctx.request_repaint();
        if let Some(png) = client.fetch_cover(url) {
            if catalog.set_vinyl_cover(*instance_id, &png).is_ok() {
                fetched += 1;
            }
        }
    }
    if total > 0 {
        let _ = tx.send(JobMsg::Progress { done: total, total });
    }

    let removed_note = if removed > 0 {
        format!(", {removed} removed")
    } else {
        String::new()
    };
    let _ = tx.send(JobMsg::Done(format!(
        "Vinyl collection synced: {} record(s){removed_note}, {fetched} new cover(s).",
        records.len()
    )));
    ctx.request_repaint();
}

/// Discogs artwork lookup for every track that has neither an embedded cover
/// nor a prior Discogs attempt on file. Paced at one request per ~1.1 s to
/// stay comfortably under the 60/min authenticated rate limit. Candidate
/// releases are streamed back to the UI as `ArtworkChoices` for the user to
/// pick from; nothing is written to the catalog here. Honours `cancel`.
/// Search Discogs for an explicit set of tracks (the "Edit release…" and the
/// right-click "Fetch artwork" / "Fetch song release details" actions) and queue
/// each track's candidate releases as `ArtworkChoices`, one queued entry per
/// track, ignoring the fetched-marker. The picker (reading `artwork_enrich`)
/// applies the chosen release's cover and, in song-details mode, its tags.
/// Nothing is written here.
pub(crate) fn run_fetch_tracks(
    db: PathBuf,
    token: String,
    ids: Vec<Id>,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    enrich: bool,
) {
    const MAX_CANDIDATES: usize = 6;
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung");
    let total = ids.len();
    let (mut queued, mut none, mut skipped, mut errored) = (0u64, 0u64, 0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();
    for track_id in ids {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let track = match catalog.get_track(track_id) {
            Ok(t) => t,
            Err(e) => {
                errored += 1;
                fails.push((format!("track {track_id}"), e.to_string()));
                continue;
            }
        };
        let artist = track
            .tags
            .artist
            .as_deref()
            .unwrap_or("")
            .trim()
            .to_string();
        let title = track
            .tags
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let album = track
            .tags
            .album
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let label = format!(
            "{} — {}",
            if artist.is_empty() {
                "Unknown"
            } else {
                &artist
            },
            title.as_deref().unwrap_or("Untitled"),
        );
        if artist.is_empty() && title.is_none() && album.is_none() {
            skipped += 1;
            fails.push((
                label,
                "no artist, title, or album tag to search Discogs with".into(),
            ));
            continue;
        }
        match client.find_artwork_candidates(&artist, title.as_deref(), album.as_deref()) {
            Ok(found) if !found.is_empty() => {
                let candidates: Vec<ArtworkChoice> = found
                    .into_iter()
                    .take(MAX_CANDIDATES)
                    .map(|c| {
                        let thumb_png = client.fetch_thumb(&c.thumb_url).unwrap_or_default();
                        ArtworkChoice {
                            release_id: c.release_id,
                            title: c.title,
                            year: c.year,
                            label: c.label,
                            country: c.country,
                            format: c.format,
                            thumb_url: c.thumb_url,
                            cover_image_url: c.cover_image_url,
                            thumb_png,
                        }
                    })
                    .collect();
                let _ = tx.send(JobMsg::ArtworkChoices(ArtworkChoices {
                    id: track_id,
                    label,
                    candidates,
                }));
                queued += 1;
            }
            Ok(_) => {
                none += 1;
                // On a song-data run, a no-match means Discogs has nothing for
                // this track — mark it fetched so it leaves the "recently added"
                // inbox instead of lingering forever with nothing to populate it.
                // Mirrors the bulk path in `run_fetch_artwork`. (Artwork-only runs
                // leave the marker alone.) Re-runnable via "Edit release…".
                if enrich {
                    let _ = catalog.mark_metadata_fetched(track_id);
                }
            }
            Err(e) => {
                errored += 1;
                fails.push((label, format!("Discogs search failed: {e}")));
            }
        }
        ctx.request_repaint();
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Discogs fetch".into(),
            items: fails,
        });
    }
    // A single-track request gets the original, friendlier wording; a multi-track
    // one gets a roll-up so the user knows how many releases are waiting to pick.
    let done = if total == 1 {
        if queued == 1 {
            "Pick a release.".to_string()
        } else if none == 1 {
            "No Discogs release found.".to_string()
        } else if skipped == 1 {
            "Not enough tags to search Discogs — add an artist or title first.".to_string()
        } else {
            "Couldn't search Discogs for that track.".to_string()
        }
    } else {
        format!(
            "Discogs fetch: {queued} ready to pick, {none} no match, {skipped} skipped, \
             {errored} error(s)."
        )
    };
    let _ = tx.send(JobMsg::Done(done));
    ctx.request_repaint();
}

pub(crate) fn run_fetch_artwork(
    db: PathBuf,
    token: String,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    enrich: bool,
) {
    const MAX_CANDIDATES: usize = 6;
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    // "Fetch song data" targets tracks missing metadata fields (regardless of
    // whether they have a cover); "Fetch artwork" targets tracks missing a cover.
    let pending = if enrich {
        catalog.tracks_missing_metadata()
    } else {
        catalog.tracks_missing_artwork()
    };
    let pending = match pending {
        Ok(p) => p,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(e.to_string()));
            ctx.request_repaint();
            return;
        }
    };
    if pending.is_empty() {
        let _ = tx.send(JobMsg::Done(if enrich {
            "Every track already has the album-level fields Discogs can fill.".into()
        } else {
            "All tracks already have artwork (or a prior Discogs attempt on file).".to_string()
        }));
        ctx.request_repaint();
        return;
    }
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung");
    let total = pending.len();
    let (mut matched, mut none, mut skipped, mut errored) = (0u64, 0u64, 0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();
    // Best name for a track that has no usable label: its source file name.
    let name_for = |id: Id| -> String {
        catalog
            .get_track(id)
            .ok()
            .map(|t| {
                Path::new(&t.source_path)
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or(t.source_path)
            })
            .unwrap_or_else(|| format!("track {id}"))
    };
    for (i, m) in pending.into_iter().enumerate() {
        let artist = m.artist.as_deref().unwrap_or("").trim().to_string();
        let title = m
            .title
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let album = m
            .album
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        if artist.is_empty() && title.is_none() && album.is_none() {
            skipped += 1;
            fails.push((
                name_for(m.id),
                "no artist, title, or album tag to search Discogs with".into(),
            ));
            continue;
        }
        let label = format!(
            "{} — {}",
            if artist.is_empty() {
                "Unknown"
            } else {
                &artist
            },
            title.as_deref().unwrap_or("Untitled"),
        );
        if cancel.load(Ordering::Relaxed) {
            let what = if enrich {
                "Song-data fetch"
            } else {
                "Artwork fetch"
            };
            if !fails.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: what.into(),
                    items: fails,
                });
            }
            let _ = tx.send(JobMsg::Done(format!(
                "{what} cancelled: {matched} track(s) with matches, {none} no match, {skipped} skipped, {errored} error(s)."
            )));
            ctx.request_repaint();
            return;
        }
        let _ = tx.send(JobMsg::Progress { done: i, total });

        // Song-data run on a track that already has artwork from a known
        // Discogs release: pull the tags straight from *that* release instead
        // of making the user re-pick art they already chose. The existing
        // artwork is left untouched (we never call `set_external_artwork`).
        if enrich {
            if let Some(rid) = m
                .release_id
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                let _ = tx.send(JobMsg::Status(format!(
                    "Filling song data ({}/{}) {}",
                    i + 1,
                    total,
                    label
                )));
                ctx.request_repaint();
                // Cache-first: a release pulled on an earlier run (or earlier in
                // this batch) is reused instead of spending another rate-limited
                // round trip on immutable data.
                match catalog.release_cached_or(rid, || client.fetch_release(rid)) {
                    Ok(rel) => {
                        if let Ok(track) = catalog.get_track(m.id) {
                            let mut tags = track.tags;
                            // Silent auto-fill stays non-destructive (empty fields
                            // only); replacing values is an explicit picker choice.
                            if rel.apply_to_tags(&mut tags, false) > 0 {
                                let _ = catalog.update_tags(m.id, &tags);
                            }
                        }
                        // Filled from the known release without prompting — mark
                        // done so we don't re-pull this same release every run.
                        let _ = catalog.mark_metadata_fetched(m.id);
                        matched += 1;
                    }
                    Err(e) => {
                        errored += 1;
                        fails.push((label.clone(), format!("Discogs release fetch failed: {e}")));
                    }
                }
                // No manual pacing here: the Discogs client throttles every API
                // request itself and retries on 429 (see `discogs::Client`).
                continue;
            }
        }

        let _ = tx.send(JobMsg::Status(format!(
            "{} ({}/{}) {}",
            if enrich {
                "Fetching song data"
            } else {
                "Fetching artwork"
            },
            i + 1,
            total,
            label
        )));
        ctx.request_repaint();

        match client.find_artwork_candidates(&artist, title.as_deref(), album.as_deref()) {
            Ok(found) if !found.is_empty() => {
                // Download a small preview thumbnail for each candidate (CDN
                // requests — these don't count against the search rate limit).
                let candidates: Vec<ArtworkChoice> = found
                    .into_iter()
                    .take(MAX_CANDIDATES)
                    .map(|c| {
                        let thumb_png = client.fetch_thumb(&c.thumb_url).unwrap_or_default();
                        ArtworkChoice {
                            release_id: c.release_id,
                            title: c.title,
                            year: c.year,
                            label: c.label,
                            country: c.country,
                            format: c.format,
                            thumb_url: c.thumb_url,
                            cover_image_url: c.cover_image_url,
                            thumb_png,
                        }
                    })
                    .collect();
                matched += 1;
                let _ = tx.send(JobMsg::ArtworkChoices(ArtworkChoices {
                    id: m.id,
                    label,
                    candidates,
                }));
                ctx.request_repaint();
            }
            Ok(_) => {
                none += 1;
                // On a song-data run, a no-match means Discogs has nothing to
                // offer for this track — mark it fetched so it isn't re-presented
                // every run. (Artwork runs don't touch this mark.)
                if enrich {
                    let _ = catalog.mark_metadata_fetched(m.id);
                }
            }
            Err(e) => {
                errored += 1;
                fails.push((label.clone(), format!("Discogs search failed: {e}")));
            }
        }
        // The Discogs client paces its own requests and backs off on 429, so the
        // loop no longer needs to sleep between tracks.
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: if enrich {
                "Song-data fetch"
            } else {
                "Artwork fetch"
            }
            .into(),
            items: fails,
        });
    }
    let _ = tx.send(JobMsg::Done(format!(
        "Discogs {}: {matched} track(s) with matches, {none} no match, {skipped} skipped, {errored} error(s).",
        if enrich { "song data" } else { "artwork" }
    )));
    ctx.request_repaint();
}

/// Convert one cataloged track to `dest` and rehydrate the new file from the
/// catalog: the FULL tag set (original scan + every edit) plus cover art (the
/// source file's own, else artwork fetched into the catalog). Shared by the
/// single and batch converters so they behave identically. Returns the output
/// path and whether metadata embedding fully succeeded (the audio converts
/// regardless — a tag failure is a warning, not an error).
/// Compute the output path for a conversion, naming the file from the track's
/// metadata ("Artist - Title", with fallbacks) rather than keeping the source's
/// filename. Falls back to the source filename only when the track has no usable
/// artist/title tags. The name is made unique so it never clobbers an unrelated
/// file (see [`unique_dest`]).
pub(crate) fn convert_dest_for(track: &Track, target: Format, out_dir: Option<&Path>) -> PathBuf {
    let src = Path::new(&track.source_path);
    let base =
        match convert::metadata_stem(track.tags.artist.as_deref(), track.tags.title.as_deref()) {
            Some(stem) => convert::output_path_with_stem(src, &stem, target, out_dir),
            None => convert::output_path_for(src, target, out_dir),
        };
    unique_dest(base, src)
}

/// Return `base` if it's free (or is the source file itself, which a convert may
/// legitimately replace); otherwise append " (1)", " (2)", … until a free path is
/// found. Prevents a metadata-named output from overwriting a different existing
/// file when two tracks share a name.
pub(crate) fn unique_dest(base: PathBuf, src: &Path) -> PathBuf {
    if !base.exists() || base == src {
        return base;
    }
    let dir = base.parent().map(Path::to_path_buf).unwrap_or_default();
    let stem = base
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = base
        .extension()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut n = 1u32;
    loop {
        let name = if ext.is_empty() {
            format!("{stem} ({n})")
        } else {
            format!("{stem} ({n}).{ext}")
        };
        let cand = dir.join(name);
        if !cand.exists() || cand == src {
            return cand;
        }
        n += 1;
    }
}

pub(crate) fn convert_track(
    catalog: &Catalog,
    track: &Track,
    spec: &ConvertSpec,
    out_dir: Option<&Path>,
    in_place: bool,
) -> Result<(PathBuf, bool), String> {
    let src = PathBuf::from(&track.source_path);
    let dest = convert_dest_for(track, spec.target, out_dir);
    if !in_place && dest.exists() {
        return Err(format!(
            "output already exists: {} (pick a different folder or use In-place)",
            dest.display()
        ));
    }
    // Capture the cover BEFORE converting (in-place deletes the source). Prefer
    // the source's own embedded art (original); else artwork fetched into the
    // catalog (added by the user).
    let cover = tag::read_front_cover_raw(&src).unwrap_or(None).or_else(|| {
        catalog
            .get_external_artwork_full(track.id)
            .ok()
            .flatten()
            .map(tag::CoverArt::from_png)
    });
    let outcome = convert::convert_file(&src, spec, &dest, in_place).map_err(|e| e.to_string())?;
    // embed_full builds a fresh tag, so the output carries exactly the catalog's
    // set (superseding whatever the transcoder copied).
    let embedded = tag::embed_full(&outcome.output_path, &track.tags, cover.as_ref()).is_ok();
    if outcome.replaced_source {
        // Repoint the catalog at the new file; relink leaves the embedded tags.
        let scanned = scan::scan_file(&outcome.output_path).map_err(|e| e.to_string())?;
        catalog
            .relink_source(
                track.id,
                &outcome.output_path.to_string_lossy(),
                spec.target,
                &scanned.properties,
            )
            .map_err(|e| e.to_string())?;
    }
    Ok((outcome.output_path, embedded))
}

pub(crate) fn run_convert(
    db: PathBuf,
    track_id: Id,
    spec: ConvertSpec,
    out_dir: Option<PathBuf>,
    in_place: bool,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let result = Catalog::open(&db)
        .map_err(|e| format!("could not open catalog: {e}"))
        .and_then(|catalog| {
            let track = catalog
                .get_track(track_id)
                .map_err(|e| format!("could not read track {track_id}: {e}"))?;
            convert_track(&catalog, &track, &spec, out_dir.as_deref(), in_place)
        });
    match result {
        Ok((output, embedded)) => {
            let warn = if embedded {
                ""
            } else {
                "  (warning: metadata could not be fully embedded)"
            };
            let msg = if in_place {
                format!("Replaced in place → {}{warn}", output.display())
            } else {
                format!(
                    "Wrote {} (run Scan on its folder to add it to the catalog){warn}",
                    output.display()
                )
            };
            let _ = tx.send(JobMsg::Done(msg));
        }
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(e));
        }
    }
    ctx.request_repaint();
}

/// Convert a whole selection to one target format, one track at a time, on a
/// background thread. Reports per-track progress, is cancellable between tracks,
/// and continues past individual failures (summarized at the end).
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_batch_convert(
    db: PathBuf,
    ids: Vec<Id>,
    spec: ConvertSpec,
    out_dir: Option<PathBuf>,
    in_place: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("could not open catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let total = ids.len();
    let (mut ok, mut failed, mut partial) = (0usize, 0usize, 0usize);
    let mut first_error: Option<String> = None;
    let mut fails: Vec<(String, String)> = Vec::new();
    for (i, id) in ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            if !fails.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: "Convert".into(),
                    items: fails,
                });
            }
            let _ = tx.send(JobMsg::Done(format!("Canceled — converted {ok}/{total}.")));
            ctx.request_repaint();
            return;
        }
        let track = match catalog.get_track(*id) {
            Ok(t) => t,
            Err(e) => {
                failed += 1;
                fails.push((format!("track {id}"), format!("couldn't read track: {e}")));
                first_error.get_or_insert_with(|| format!("track {id}: {e}"));
                continue;
            }
        };
        let label = track
            .tags
            .title
            .clone()
            .unwrap_or_else(|| format!("track {id}"));
        let _ = tx.send(JobMsg::Status(format!(
            "Converting {}/{total}: {label}…",
            i + 1
        )));
        let _ = tx.send(JobMsg::Progress { done: i, total });
        ctx.request_repaint();
        match convert_track(&catalog, &track, &spec, out_dir.as_deref(), in_place) {
            Ok((_, embedded)) => {
                ok += 1;
                if !embedded {
                    partial += 1;
                }
            }
            Err(e) => {
                failed += 1;
                fails.push((label.clone(), format!("conversion failed: {e}")));
                first_error.get_or_insert_with(|| format!("{label}: {e}"));
            }
        }
    }
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Convert".into(),
            items: fails,
        });
    }
    let mut msg = format!("Converted {ok}/{total} → {}", format_label(spec.target));
    if failed > 0 {
        msg.push_str(&format!(", {failed} failed"));
    }
    if partial > 0 {
        msg.push_str(&format!(", {partial} with partial metadata"));
    }
    if !in_place && ok > 0 {
        msg.push_str(" (run Scan on the output folder to catalog the new files)");
    }
    if let Some(err) = first_error {
        msg.push_str(&format!(" — e.g. {err}"));
    }
    let _ = tx.send(JobMsg::Done(msg));
    ctx.request_repaint();
}

pub(crate) fn file_stamp(path: &str) -> (u64, i64) {
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
