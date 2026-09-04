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
            // An automatic write has finished (successfully or not). Hand the
            // stall check to the caller, which re-counts pending edits after the
            // reload this poll triggers. Done here rather than on the reload path
            // because a failed job reports `Failed` without asking for a reload.
            if std::mem::take(&mut self.auto_write_job) {
                self.auto_write_pending_latch = true;
                reload = true;
            }
            if self.write_edits_running {
                self.write_edits_running = false;
                self.cover_cache.clear();
                self.cover_full_cache.clear();
                self.cover_inflight.clear();
            }
            // A finished USB export changed the stick's playlists/tracks on
            // disk. If the device view is showing that same stick, drop
            // `usb_loaded_for` so `poll_usb` re-scans it next frame and the new
            // playlists appear — otherwise the view keeps the pre-export tree
            // until a manual rescan or app restart.
            if let Some(dest) = self.export_running_to.take() {
                if self.usb_loaded_for.as_deref() == Some(dest.as_path()) {
                    self.usb_loaded_for = None;
                }
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

    /// Transfer device tracks into the local library: copy the files off the
    /// stick into `dest` (mirroring the stick's own folder layout, minus a
    /// rekordbox export's `Contents/` wrapper), then run the normal import —
    /// so the copies land in the catalog and, with auto-analyze on, the
    /// analysis chain. Explicit-only: reachable solely from the USB rows'
    /// "Add to Library" menu and a drag onto the Library tab. Source files on
    /// the stick are never touched.
    /// `playlist`: when set, the transferred tracks are additionally gathered
    /// into a new local playlist of that name, in source order — the
    /// "import a device playlist" gesture.
    pub(crate) fn spawn_usb_transfer(
        &mut self,
        ctx: egui::Context,
        sources: Vec<PathBuf>,
        vol: PathBuf,
        dest: PathBuf,
        playlist: Option<String>,
    ) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = format!(
            "Copying {} track(s) from {} to the library…",
            sources.len(),
            vol.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| vol.display().to_string())
        );
        let db = self.db_path.clone();
        let auto_analyze = self.config.auto_analyze;
        thread::spawn(move || {
            run_usb_transfer(db, sources, vol, dest, playlist, cancel, tx, ctx, auto_analyze)
        });
    }

    /// Export the whole catalog (tracks + playlists) as a native rekordbox
    /// USB onto `dest` — the flow behind the USB view's Export button, always
    /// via its confirmation modal. Explicit-only: rewrites the destination's
    /// `PIONEER/rekordbox` databases and adds under `/Contents`, never touches
    /// library source files.
    pub(crate) fn spawn_export(
        &mut self,
        ctx: egui::Context,
        dest: PathBuf,
        playlist_ids: Vec<Id>,
        scope: String,
        replace: bool,
    ) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        let verb = if replace { "Exporting" } else { "Adding" };
        self.status = format!("{verb} {scope} to {}…", dest.display());
        // Remember where we're exporting so the completion handler can refresh
        // the device view (the stick's on-disk playlists just changed).
        self.export_running_to = Some(dest.clone());
        let db = self.db_path.clone();
        thread::spawn(move || run_export(db, dest, playlist_ids, replace, cancel, tx, ctx));
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
        let pointer_pos =
            macos_drag::pointer_pos(frame).or_else(|| ctx.input(|i| i.pointer.latest_pos()));
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
                        crate::ui::tokens::font::callout(),
                        egui::Color32::from_rgb(120, 170, 240),
                    );
                }
            } else {
                painter.rect_filled(screen, 0.0, egui::Color32::from_black_alpha(160));
                painter.text(
                    screen.center(),
                    egui::Align2::CENTER_CENTER,
                    "Drop music to add it to your catalog",
                    crate::ui::tokens::font::title(),
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
        // Cancellable: a whole-library sweep is thousands of tracks, and the flag
        // is checked per track, so Abort stops the queue without discarding the
        // analyses that already landed.
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = "Analyzing…".into();
        let db = self.db_path.clone();
        let query = if self.filter.trim().is_empty() {
            None
        } else {
            Some(self.filter.clone())
        };
        thread::spawn(move || {
            run_analyze(db, AnalyzeTargets::Query(query), force, cancel, tx, ctx)
        });
    }

    /// Analyze a specific set of tracks (the context-menu selection) rather than
    /// the whole filtered view. Skips tracks already analyzed at the current
    /// version unless `force`.
    pub(crate) fn spawn_analyze_ids(&mut self, ctx: egui::Context, ids: Vec<Id>, force: bool) {
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = "Analyzing…".into();
        let db = self.db_path.clone();
        thread::spawn(move || run_analyze(db, AnalyzeTargets::Ids(ids), force, cancel, tx, ctx));
    }

    /// Sync the local vinyl-collection cache from Discogs: pull the user's whole
    /// collection (folder 0), upsert metadata, prune records they've removed, and
    /// Kick off the background refreshes we always want current at launch. The
    /// single home for "keep this fresh on startup" work — today that's the
    /// Discogs vinyl collection (new records + any missing covers); add future
    /// always-up-to-date syncs here. Unlike an explicit Sync click, this
    /// silently no-ops when no Discogs token is configured, so a tokenless
    /// launch is never nagged with the Settings modal.
    pub(crate) fn spawn_startup_refresh(&mut self, ctx: egui::Context) {
        if self.discogs_token().trim().is_empty() {
            return;
        }
        self.spawn_refresh_vinyl_inner(ctx, true);
    }

    /// download covers we don't already have. Token resolution is policy and lives
    /// here; the worker only talks to Discogs and the catalog.
    pub(crate) fn spawn_refresh_vinyl(&mut self, ctx: egui::Context) {
        self.spawn_refresh_vinyl_inner(ctx, false);
    }

    /// Shared body of the two vinyl-sync entry points. `quiet` keeps the run
    /// out of the status bar entirely — no phase lines, no progress, no closing
    /// tally: nobody asked for the startup sync, and narrating it makes a launch
    /// that's otherwise ready read as busy. An explicit Sync click still reports
    /// every phase and what it did — that one the user is waiting on.
    fn spawn_refresh_vinyl_inner(&mut self, ctx: egui::Context, quiet: bool) {
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
        // Cancellable because the price phase is one rate-limited request per
        // record — minutes on a first sync. Stopping keeps everything fetched so
        // far; the next refresh resumes with what's still unpriced.
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        if !quiet {
            self.status = "Syncing vinyl collection and wantlist…".into();
        }
        let db = self.db_path.clone();
        thread::spawn(move || run_refresh_vinyl(db, token, cancel, quiet, tx, ctx));
    }

    /// Run one user-requested change to a Discogs list — the vinyl grid's
    /// move/remove actions and the library's "Add to Discogs wantlist". Writes
    /// to the user's Discogs account off the UI thread, then mirrors the change
    /// into the local cache so the grid updates on the reload `Done` triggers.
    ///
    /// Edits that destroy a collection copy are routed through a confirmation
    /// first (see [`VinylEdit::destroys_collection_copy`]) — call this only once
    /// the user has agreed, or for edits that don't need it.
    pub(crate) fn spawn_vinyl_edit(&mut self, ctx: egui::Context, edit: VinylEdit) {
        // One job channel serves the whole app, so starting an edit mid-job
        // would orphan the running one. These are one-click actions from a menu
        // (nothing queues them), so declining is enough.
        if self.is_busy() {
            self.status = "Busy — wait for the current job to finish.".into();
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
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        self.job_cancel = None; // a handful of API calls; runs to completion
        self.status = match &edit {
            VinylEdit::Want { release_ids, .. } if release_ids.len() > 1 => {
                format!("Adding {} releases to your wantlist…", release_ids.len())
            }
            VinylEdit::Want { .. } => "Adding to your wantlist…".into(),
            VinylEdit::Move { from, .. } => match from {
                VinylList::Collection => "Moving to your wantlist…".into(),
                VinylList::Wantlist => "Moving to your collection…".into(),
            },
            VinylEdit::Collect { .. } => "Adding to your collection…".into(),
            VinylEdit::Remove { list, .. } => match list {
                VinylList::Collection => "Removing from your collection…".into(),
                VinylList::Wantlist => "Removing from your wantlist…".into(),
            },
            VinylEdit::Swap { .. } => "Swapping the pressing…".into(),
        };
        let db = self.db_path.clone();
        // The username keys every collection/wantlist endpoint. Reuse the one a
        // previous sync resolved when we have it; the worker falls back to an
        // identity lookup (one extra request) when it's still blank.
        let username = self.config.discogs_username.trim().to_string();
        thread::spawn(move || run_vinyl_edit(db, token, username, edit, tx, ctx));
    }

    /// Fetch from Discogs for an explicit set of tracks (the right-click menu's
    /// "Find Discogs release" entry and its ↻ re-pick). Searches every id and
    /// queues its candidates for the picker, ignoring the fetched-marker —
    /// these are deliberate per-track requests. Always a song-data run: the
    /// chosen release supplies the cover *and* fills empty tag fields, since a
    /// cover-only mode meant a second trip through the same picker.
    pub(crate) fn spawn_fetch_tracks(&mut self, ctx: egui::Context, ids: Vec<Id>) {
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
        self.artwork_enrich = true;
        let (tx, rx) = mpsc::channel();
        self.job_rx = Some(rx);
        let cancel = Arc::new(AtomicBool::new(false));
        self.job_cancel = Some(cancel.clone());
        self.status = "Searching Discogs for releases…".into();
        let db = self.db_path.clone();
        // Snapshot the medium filter for the worker: the user's picker
        // preferences can't be read off `self` from another thread.
        let hidden_mediums = self.config.hidden_release_mediums.clone();
        thread::spawn(move || {
            run_fetch_tracks(db, token, ids, cancel, tx, ctx, true, hidden_mediums)
        });
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
    finish_import(&catalog, outcome, auto_analyze, &cancel, &tx, &ctx);
}

/// Build a native rekordbox export of the whole catalog onto `dest`.
/// See [`App::spawn_export`]. Audio is copied under `/Contents` (unchanged
/// files skip the copy), analysis is serialized to ANLZ files, and playlists
/// land in both `export.pdb` and `exportLibrary.db` so every CDJ generation
/// sees them. Library sources are read, never written.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_export(
    db: PathBuf,
    dest: PathBuf,
    playlist_ids: Vec<Id>,
    replace: bool,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    use ordnung_rbdb::export::{export_usb, ExportError, ExportMode, ExportStage};

    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    // Whole library (empty ids) or the chosen playlist/folder subtree, with
    // analyses attached — see Catalog::export_selection.
    let (tracks, playlists) = match catalog.export_selection(&playlist_ids) {
        Ok(v) => v,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("reading catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    if tracks.is_empty() {
        let _ = tx.send(JobMsg::Failed(
            "nothing to export — the selection has no tracks".into(),
        ));
        ctx.request_repaint();
        return;
    }
    let name_by_id: std::collections::HashMap<u64, String> = tracks
        .iter()
        .map(|t| {
            let label = match (&t.tags.artist, &t.tags.title) {
                (Some(a), Some(ti)) => format!("{a} — {ti}"),
                _ => t.source_path.clone(),
            };
            (t.id, label)
        })
        .collect();

    let mode = if replace {
        ExportMode::Replace
    } else {
        ExportMode::Merge
    };
    let result = export_usb(
        &dest,
        &tracks,
        &playlists,
        mode,
        &mut |p| {
            let stage = match p.stage {
                ExportStage::CopyingAudio => "Copying",
                ExportStage::WritingArtwork => "Writing",
                ExportStage::WritingAnalysis => "Writing analysis for",
                ExportStage::WritingDatabase => "Writing databases",
            };
            let msg = if p.stage == ExportStage::WritingDatabase {
                format!("{stage}…")
            } else {
                format!("{stage} {}…", p.detail)
            };
            let _ = tx.send(JobMsg::Status(msg));
            let _ = tx.send(JobMsg::Progress {
                done: p.done,
                total: p.total,
            });
            ctx.request_repaint();
        },
        &cancel,
    );
    match result {
        Ok(report) => {
            if !report.skipped.is_empty() {
                let items = report
                    .skipped
                    .iter()
                    .map(|(id, why)| {
                        (
                            name_by_id
                                .get(id)
                                .cloned()
                                .unwrap_or_else(|| format!("track {id}")),
                            why.clone(),
                        )
                    })
                    .collect();
                let _ = tx.send(JobMsg::Failures {
                    title: "Export".into(),
                    items,
                });
            }
            let _ = tx.send(JobMsg::Done(format!(
                "Exported {} track(s), {} playlist node(s) to {} ({:.1} MB copied). \
Eject before unplugging.",
                report.tracks_exported,
                report.playlists_exported,
                dest.file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| dest.display().to_string()),
                report.bytes_copied as f64 / 1_048_576.0,
            )));
        }
        Err(ExportError::Canceled) => {
            let _ = tx.send(JobMsg::Done(
                "Export aborted — the stick may hold a partial export; run Export again \
to finish it."
                    .into(),
            ));
        }
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("export: {e}")));
        }
    }
    ctx.request_repaint();
}

/// Copy device tracks into the local library folder, then import the copies.
/// See [`App::spawn_usb_transfer`]. A file already present at its destination
/// with the same size is not re-copied (it still gets imported, so "transfer"
/// always ends with the tracks in the catalog); a same-name file of a
/// *different* size keeps both — the copy lands under a numbered name, since
/// silently overwriting a local file with a device file would destroy data.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_usb_transfer(
    db: PathBuf,
    sources: Vec<PathBuf>,
    vol: PathBuf,
    dest: PathBuf,
    playlist: Option<String>,
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
    let total = sources.len();
    let mut copied = 0usize;
    let mut already = 0usize;
    let mut failures: Vec<(String, String)> = Vec::new();
    let mut to_import: Vec<PathBuf> = Vec::new();
    for (i, src) in sources.iter().enumerate() {
        if cancel.load(std::sync::atomic::Ordering::Relaxed) {
            break;
        }
        let _ = tx.send(JobMsg::Progress { done: i, total });
        let _ = tx.send(JobMsg::Status(format!(
            "Copying to library ({} of {total})…",
            i + 1
        )));
        ctx.request_repaint();
        // Mirror the stick's own layout, minus the export's Contents/ wrapper,
        // so an artist/album tree lands as an artist/album tree.
        let rel = src
            .strip_prefix(&vol)
            .unwrap_or_else(|_| Path::new(src.file_name().unwrap_or_default()));
        let rel = rel.strip_prefix("Contents").unwrap_or(rel);
        let mut dst = dest.join(rel);
        if let Some(parent) = dst.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                failures.push((src.display().to_string(), e.to_string()));
                continue;
            }
        }
        let src_size = std::fs::metadata(src).map(|m| m.len()).ok();
        if let Ok(existing) = std::fs::metadata(&dst) {
            if src_size == Some(existing.len()) {
                // Same file, already local: nothing to copy, still import it.
                already += 1;
                to_import.push(dst);
                continue;
            }
            dst = unique_destination(&dst);
        }
        match std::fs::copy(src, &dst) {
            Ok(_) => {
                copied += 1;
                to_import.push(dst);
            }
            Err(e) => failures.push((src.display().to_string(), e.to_string())),
        }
    }
    if !failures.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Copy to library".into(),
            items: failures,
        });
    }
    if to_import.is_empty() {
        let _ = tx.send(JobMsg::Done("Nothing was copied.".into()));
        ctx.request_repaint();
        return;
    }
    let _ = tx.send(JobMsg::Status(format!(
        "Copied {copied} track(s){}; importing…",
        if already > 0 {
            format!(" ({already} already in the library folder)")
        } else {
            String::new()
        }
    )));
    let outcome = import_files(&catalog, &to_import, &cancel, &tx, &ctx);
    // Recreate the device playlist locally, in transfer order. Unchanged files
    // (already in the library) resolve through the same path lookup, so the
    // playlist is complete even when nothing needed copying. Runs before the
    // analysis chain so the playlist exists the moment the import lands.
    if let Some(name) = playlist {
        if !outcome.cancelled {
            let ids: Vec<Id> = to_import
                .iter()
                .filter_map(|p| {
                    catalog
                        .track_id_by_path(&p.to_string_lossy())
                        .ok()
                        .flatten()
                })
                .collect();
            if !ids.is_empty() {
                // A same-named playlist may already exist; number the copy the
                // way file copies are numbered rather than merging into it.
                let existing: Vec<String> = catalog
                    .list_playlists()
                    .map(|all| all.iter().map(|p| p.name.clone()).collect())
                    .unwrap_or_default();
                let mut final_name = name.clone();
                let mut n = 2;
                while existing.contains(&final_name) {
                    final_name = format!("{name} ({n})");
                    n += 1;
                }
                match catalog
                    .create_playlist(&final_name, None, false)
                    .and_then(|pid| catalog.add_tracks(pid, &ids))
                {
                    Ok(_) => {
                        let _ = tx.send(JobMsg::Status(format!(
                            "Created playlist \u{201C}{final_name}\u{201D} with {} track(s).",
                            ids.len()
                        )));
                    }
                    Err(e) => {
                        let _ = tx.send(JobMsg::Failures {
                            title: "Import playlist".into(),
                            items: vec![(name.clone(), e.to_string())],
                        });
                    }
                }
            }
        }
    }
    finish_import(&catalog, outcome, auto_analyze, &cancel, &tx, &ctx);
}

/// `song.mp3` → `song (2).mp3`, `song (3).mp3`, … — the first name that
/// doesn't collide with an existing file.
fn unique_destination(dst: &Path) -> PathBuf {
    let stem = dst
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let ext = dst
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    let dir = dst.parent().unwrap_or(Path::new(""));
    for n in 2.. {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
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
    finish_import(&catalog, outcome, auto_analyze, &cancel, &tx, &ctx);
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
    cancel: &AtomicBool,
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
    // The import's cancel flag stays live into the chained analysis, so one
    // Abort covers the whole scan → analyze run.
    analyze_tracks(catalog, tracks, false, &lead, cancel, tx, ctx);
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
                // reflects what now lives in the file, then drop the fetched
                // row — the cover now lives in the file, so keeping it around
                // would leave the inspector's "Embed fetched cover into file"
                // button offering a write that already happened. Same reasoning
                // (and same order) as the single-track embed path.
                if art.is_some() {
                    if let Ok(scanned) = scan::scan_file(&path) {
                        let _ = catalog.upsert_scanned(&scanned);
                    }
                    let _ = catalog.clear_external_artwork(t.id);
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
    analyze_tracks(&catalog, tracks, force, "", &cancel, &tx, &ctx);
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
    cancel: &AtomicBool,
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
    //
    // Run it on a pool sized for *memory*, not just cores: each worker holds a
    // whole decoded track plus its spectrogram, so on a small-RAM machine the
    // default one-thread-per-core pool is what pushes the app into swap.
    let params = AnalysisParams::default();
    let done = AtomicUsize::new(0);
    let pool = analysis_pool();

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

    // Results stream back to *this* thread and are saved as they land, rather
    // than being collected and written after the whole fan-out finishes. The
    // catalog connection isn't shareable across rayon workers, so it stays here
    // and the workers only send.
    //
    // Why it matters: a full-library sweep runs for hours. Collecting first
    // meant a crash, a force-quit, or a power loss at hour three threw away
    // every track analysed in that run — `needs_analysis` would re-derive the
    // whole pending set on the next launch. Saving per track makes the run
    // resumable at whatever point it stopped, and the SQLite write is trivial
    // next to the decode+FFT that produced it.
    let (res_tx, res_rx) = mpsc::channel::<(u64, u64, i64, Option<Result<Analysis, String>>)>();
    let (mut ok, mut failed, mut skipped) = (0u64, 0u64, 0u64);
    let mut fails: Vec<(String, String)> = Vec::new();

    // The fan-out borrows `pending`/`cancel`/`ctx`; the drain borrows `catalog`.
    // A scoped thread lets both run concurrently without moving either.
    thread::scope(|scope| {
        scope.spawn(|| {
            let run = || {
                pending.par_iter().for_each_init(
                    || (tx.clone(), res_tx.clone()),
                    |(tx_local, res_local), (id, path, size, mtime)| {
                        // Abort can't interrupt a decode already in flight, but
                        // it stops every track rayon hasn't started yet — with a
                        // queue of thousands that's the difference between
                        // seconds and hours. `None` marks a skipped track: it's
                        // neither saved nor counted as a failure.
                        if cancel.load(Ordering::Relaxed) {
                            // Still tick progress: the bar drains quickly to its
                            // total instead of freezing where the user clicked.
                            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                            let _ = tx_local.send(JobMsg::Progress { done: n, total });
                            ctx.request_repaint();
                            let _ = res_local.send((*id, *size, *mtime, None));
                            return;
                        }
                        let r = analysis::analyze_file(path, params).map_err(|e| e.to_string());
                        let n = done.fetch_add(1, Ordering::Relaxed) + 1;
                        let _ = tx_local.send(JobMsg::Progress { done: n, total });
                        ctx.request_repaint();
                        let _ = res_local.send((*id, *size, *mtime, Some(r)));
                    },
                );
            };
            // `install` runs the fan-out on the sized pool; without a pool we're
            // on rayon's global one, which is the pre-clamp behavior.
            match &pool {
                Some(p) => p.install(run),
                None => run(),
            }
            // Drop this thread's handle so the drain below sees the channel
            // close once every worker clone is gone.
            drop(res_tx);
        });

        // Drain and persist as each track finishes. Ends when the fan-out
        // thread and all its worker clones have dropped their senders.
        for (id, size, mtime, result) in res_rx {
            let Some(result) = result else {
                skipped += 1;
                continue;
            };
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
    });
    if !fails.is_empty() {
        let _ = tx.send(JobMsg::Failures {
            title: "Analyze".into(),
            items: fails,
        });
    }
    // A cancelled run reports what it managed to keep, so the user knows the
    // finished analyses were saved and only the remainder was dropped.
    let _ = tx.send(JobMsg::Done(if skipped > 0 {
        format!(
            "{lead}Analysis cancelled: {ok} of {total} track(s) analyzed, \
             {failed} failed, {skipped} skipped."
        )
    } else {
        format!("{lead}Analyzed {ok} track(s), {failed} failed.")
    }));
    ctx.request_repaint();
}

/// How long a cached marketplace price stays fresh (30 days). Prices are the one
/// volatile thing in the vinyl cache, but each costs a rate-limited request, so a
/// routine sync only re-prices what's aged out. See
/// [`Catalog::vinyl_prices_to_refresh`].
const VINYL_PRICE_MAX_AGE_SECS: i64 = 30 * 24 * 60 * 60;

/// Sync the local vinyl-collection cache from the user's Discogs collection.
/// Fetches the full collection (paced by the Discogs client), upserts every
/// record, prunes any the user removed since the last sync, then downloads
/// covers for records that don't have one cached yet. Covers stream in with
/// determinate progress so the grid fills as the run proceeds.
///
/// The last phase looks up each record's lowest marketplace price (what the
/// vinyl view's price sort reads), skipping any priced recently enough. That's
/// one rate-limited request per record, so it's the long pole on a first sync —
/// hence `cancel`: stopping there keeps everything already fetched, and the next
/// refresh picks up the records that never got a price.
///
/// `quiet` runs the whole sync silently: no phase lines, no progress bar, no
/// closing tally (the job still reports `Done` so the grid reloads) — used by
/// the startup sync, which runs unasked and shouldn't make an idle launch look
/// like it's working.
pub(crate) fn run_refresh_vinyl(
    db: PathBuf,
    token: String,
    cancel: Arc<AtomicBool>,
    quiet: bool,
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
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");

    if !quiet {
        let _ = tx.send(JobMsg::Status("Fetching Discogs collection…".into()));
        ctx.request_repaint();
    }
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

    if !quiet {
        let _ = tx.send(JobMsg::Status("Fetching Discogs wantlist…".into()));
        ctx.request_repaint();
    }
    // The wantlist is a bonus section, not the point of the sync: if it fails
    // (or the account has none), keep the collection we just fetched rather than
    // failing the whole run.
    let wants = client.fetch_wantlist_for(&username).unwrap_or_default();

    // Upsert metadata and prune records dropped from each list, so the caches
    // mirror Discogs exactly. Cover bytes survive the metadata upsert.
    let mut removed = 0usize;
    for (list, recs) in [
        (VinylList::Collection, &records),
        (VinylList::Wantlist, &wants),
    ] {
        let mut keep = Vec::with_capacity(recs.len());
        for rec in recs.iter() {
            let _ = catalog.upsert_vinyl(list, rec);
            keep.push(rec.instance_id);
        }
        removed += catalog.prune_vinyl_not_in(list, &keep).unwrap_or(0);
    }

    // Download covers we don't already have, reporting progress across both
    // lists as one run so the grid fills top to bottom.
    let missing: Vec<(VinylList, u64, String)> = [VinylList::Collection, VinylList::Wantlist]
        .into_iter()
        .flat_map(|list| {
            catalog
                .vinyl_missing_covers(list)
                .unwrap_or_default()
                .into_iter()
                .map(move |(id, url)| (list, id, url))
        })
        .collect();
    let total = missing.len();
    let mut fetched = 0usize;
    for (i, (list, instance_id, url)) in missing.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !quiet {
            let _ = tx.send(JobMsg::Status(format!(
                "Downloading vinyl covers… ({}/{total})",
                i + 1
            )));
            let _ = tx.send(JobMsg::Progress { done: i, total });
            ctx.request_repaint();
        }
        if let Some(png) = client.fetch_cover(url) {
            if catalog.set_vinyl_cover(*list, *instance_id, &png).is_ok() {
                fetched += 1;
            }
        }
    }
    if total > 0 && !quiet {
        let _ = tx.send(JobMsg::Progress { done: total, total });
    }

    // Tracklists next: the search box matches song titles out of the cached
    // release detail, so a record whose detail was never fetched can only be
    // found by its release fields. Warming every uncached release here is what
    // makes "which record is that song on?" answerable for records the user has
    // never opened. One rate-limited request each, but release metadata doesn't
    // change, so the cost is paid once per record and never again.
    let uncached = catalog.vinyl_releases_missing_detail().unwrap_or_default();
    let detail_total = uncached.len();
    let mut detailed = 0usize;
    for (i, release_id) in uncached.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !quiet {
            let _ = tx.send(JobMsg::Status(format!(
                "Caching song lists… ({}/{detail_total})",
                i + 1
            )));
            let _ = tx.send(JobMsg::Progress {
                done: i,
                total: detail_total,
            });
            ctx.request_repaint();
        }
        // Best-effort: a release that won't fetch (deleted, private, a transient
        // network error) is left uncached and retried on the next sync.
        let id = release_id.to_string();
        if let Ok(detail) = client.fetch_release(&id) {
            if catalog.cache_release(&detail).is_ok() {
                detailed += 1;
            }
        }
    }
    if detail_total > 0 && !quiet {
        let _ = tx.send(JobMsg::Progress {
            done: detail_total,
            total: detail_total,
        });
    }

    // Prices last: one rate-limited request each, and everything above is
    // already usable without them.
    let stale: Vec<(VinylList, u64, u64)> = [VinylList::Collection, VinylList::Wantlist]
        .into_iter()
        .flat_map(|list| {
            catalog
                .vinyl_prices_to_refresh(list, VINYL_PRICE_MAX_AGE_SECS)
                .unwrap_or_default()
                .into_iter()
                .map(move |(instance_id, release_id)| (list, instance_id, release_id))
        })
        .collect();
    let price_total = stale.len();
    let mut priced = 0usize;
    for (i, (list, instance_id, release_id)) in stale.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        if !quiet {
            let _ = tx.send(JobMsg::Status(format!(
                "Checking Discogs prices… ({}/{price_total})",
                i + 1
            )));
            let _ = tx.send(JobMsg::Progress {
                done: i,
                total: price_total,
            });
            ctx.request_repaint();
        }
        // A failed lookup is left unstamped so the next refresh retries it; a
        // successful one that found nothing for sale is stamped as checked.
        if let Ok(price) = client.marketplace_price(*release_id) {
            let ok = catalog
                .set_vinyl_price(
                    *list,
                    *instance_id,
                    price.as_ref().map(|p| p.value),
                    price.as_ref().map(|p| p.currency.as_str()),
                )
                .is_ok();
            if ok && price.is_some() {
                priced += 1;
            }
        }
    }
    if price_total > 0 && !quiet {
        let _ = tx.send(JobMsg::Progress {
            done: price_total,
            total: price_total,
        });
    }

    let removed_note = if removed > 0 {
        format!(", {removed} removed")
    } else {
        String::new()
    };
    let stopped = if cancel.load(Ordering::Relaxed) {
        " (stopped early)"
    } else {
        ""
    };
    let done = if quiet {
        String::new()
    } else {
        format!(
            "Vinyl synced: {} record(s), {} wantlisted{removed_note}, \
             {fetched} new cover(s), {detailed} song list(s), {priced} price(s){stopped}.",
            records.len(),
            wants.len()
        )
    };
    let _ = tx.send(JobMsg::Done(done));
    ctx.request_repaint();
}

/// Apply one [`VinylEdit`] to the user's Discogs account, then mirror it into
/// the local cache so the grid reflects it immediately instead of waiting for
/// the next full sync.
///
/// Discogs is always written first: if a call fails, the local cache is left
/// untouched and still matches the account. Moves add to the destination list
/// *before* removing from the source, so a half-failed move leaves the record in
/// both lists (visible, and fixable) rather than in neither.
pub(crate) fn run_vinyl_edit(
    db: PathBuf,
    token: String,
    username: String,
    edit: VinylEdit,
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
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");

    // Every collection/wantlist endpoint is keyed by username. Resolve it once
    // if a previous sync hasn't already, and report it back so the next edit
    // skips the lookup.
    let username = if username.is_empty() {
        match client.identity() {
            Ok(u) => {
                let _ = tx.send(JobMsg::VinylUsername(u.clone()));
                u
            }
            Err(e) => {
                let _ = tx.send(JobMsg::Failed(format!("resolving Discogs account: {e}")));
                ctx.request_repaint();
                return;
            }
        }
    } else {
        username
    };

    let done = match edit {
        VinylEdit::Want { release_ids, label } => {
            let total = release_ids.len();
            // Added to Discogs but not cached locally: the vinyl view is
            // records only, so a CD/digital release the user wanted from the
            // library has nowhere to show. Counted so we can say so plainly
            // rather than looking like the add silently did nothing.
            let mut non_vinyl = 0usize;
            let mut wanted = 0usize;
            let mut failures: Vec<(String, String)> = Vec::new();
            for (i, release_id) in release_ids.into_iter().enumerate() {
                if total > 1 {
                    let _ = tx.send(JobMsg::Progress { done: i, total });
                    let _ = tx.send(JobMsg::Status(format!(
                        "Adding to your wantlist… ({}/{total})",
                        i + 1
                    )));
                    ctx.request_repaint();
                }
                match client.add_to_wantlist(&username, release_id) {
                    Ok(Some(rec)) => {
                        let _ = catalog.upsert_vinyl(VinylList::Wantlist, &rec);
                        // Pull the cover now so the record isn't a blank tile
                        // until the next sync. Best-effort: a failed image
                        // download doesn't fail the want.
                        if let Some(url) = rec.cover_url.as_deref() {
                            if let Some(png) = client.fetch_cover(url) {
                                let _ = catalog.set_vinyl_cover(
                                    VinylList::Wantlist,
                                    rec.instance_id,
                                    &png,
                                );
                            }
                        }
                        // Warm the tracklist too, so the record is findable by
                        // song name right away instead of at the next sync.
                        cache_release_detail(&catalog, &client, rec.release_id);
                        // …and its price, so the grid's price column isn't
                        // blank on the record you just added.
                        cache_release_price(
                            &catalog,
                            &client,
                            VinylList::Wantlist,
                            rec.instance_id,
                            rec.release_id,
                        );
                        wanted += 1;
                    }
                    Ok(None) => non_vinyl += 1,
                    Err(e) => failures.push((format!("Release {release_id}"), e.to_string())),
                }
            }
            if total > 1 {
                let _ = tx.send(JobMsg::Progress { done: total, total });
            }
            if !failures.is_empty() {
                let _ = tx.send(JobMsg::Failures {
                    title: "Add to wantlist".into(),
                    items: failures,
                });
            }
            let non_vinyl_note = match non_vinyl {
                0 => String::new(),
                1 => " 1 isn't a vinyl pressing, so it won't show in the grid.".into(),
                n => format!(" {n} aren't vinyl pressings, so they won't show in the grid."),
            };
            match (wanted, non_vinyl, total) {
                // Every one failed; the failure report says why.
                (0, 0, _) => "Nothing added to your wantlist.".to_string(),
                // The common case: one release, wanted. Name it.
                (1, 0, 1) => format!("Added {label} to your wantlist."),
                _ => format!(
                    "Added {} of {total} to your wantlist.{non_vinyl_note}",
                    wanted + non_vinyl
                ),
            }
        }

        VinylEdit::Collect { release_id, label } => {
            // `add_to_collection` answers with an instance id only, so the row
            // to cache comes from the release itself. A non-vinyl release is
            // added on Discogs but has no place in the records-only view — the
            // same rule `Want` follows.
            let instance_id = match client.add_to_collection(&username, release_id) {
                Ok(id) => id,
                Err(e) => {
                    let _ = tx.send(JobMsg::Failed(format!(
                        "adding {label} to your collection: {e}"
                    )));
                    ctx.request_repaint();
                    return;
                }
            };
            match client.collection_record(&username, release_id, instance_id) {
                Ok(Some(rec)) => {
                    let _ = catalog.upsert_vinyl(VinylList::Collection, &rec);
                    // Pull the cover now so the record isn't a blank tile until
                    // the next sync; a failed download doesn't fail the add.
                    if let Some(url) = rec.cover_url.as_deref() {
                        if let Some(png) = client.fetch_cover(url) {
                            let _ = catalog.set_vinyl_cover(
                                VinylList::Collection,
                                rec.instance_id,
                                &png,
                            );
                        }
                    }
                    cache_release_detail(&catalog, &client, rec.release_id);
                    cache_release_price(
                        &catalog,
                        &client,
                        VinylList::Collection,
                        rec.instance_id,
                        rec.release_id,
                    );
                    format!("Added {label} to your collection.")
                }
                Ok(None) => format!(
                    "Added {label} to your Discogs collection. It isn't a vinyl \
                     pressing, so it won't show in this view."
                ),
                // On Discogs either way — the local cache just misses it until
                // the next sync, which is worth saying plainly.
                Err(e) => format!(
                    "Added {label} to your Discogs collection, but couldn't cache \
                     it locally: {e}"
                ),
            }
        }

        VinylEdit::Move { from, record } => {
            let to = match from {
                VinylList::Collection => VinylList::Wantlist,
                VinylList::Wantlist => VinylList::Collection,
            };
            // Build the destination row first: the two lists key rows
            // differently, so the record is re-keyed as part of the move. The
            // `added` date belongs to the list it's leaving, so drop it — the
            // next sync fills in the real one.
            let mut moved = (*record).clone();
            moved.added = None;
            let result =
                match to {
                    VinylList::Wantlist => client
                        .add_to_wantlist(&username, record.release_id)
                        .map(|_| {
                            moved.instance_id = record.release_id;
                            moved.folder_id = None;
                        }),
                    VinylList::Collection => client
                        .add_to_collection(&username, record.release_id)
                        .map(|instance_id| {
                            moved.instance_id = instance_id;
                            moved.folder_id = Some(discogs::UNCATEGORIZED_FOLDER);
                        }),
                };
            if let Err(e) = result {
                let _ = tx.send(JobMsg::Failed(format!(
                    "adding {} to your {}: {e}",
                    record.title,
                    list_name(to)
                )));
                ctx.request_repaint();
                return;
            }
            // Now drop the source copy. If this fails the record is in both
            // lists on Discogs — say so, and leave the local cache alone so a
            // sync shows the user exactly that state.
            if let Err(e) = remove_from(&client, &username, from, &record) {
                let _ = tx.send(JobMsg::Failed(format!(
                    "{} is now in your {}, but removing it from your {} failed: {e}",
                    record.title,
                    list_name(to),
                    list_name(from)
                )));
                ctx.request_repaint();
                return;
            }
            let _ = catalog.move_vinyl(from, record.instance_id, to, &moved);
            // A no-op if the release was already warmed on the list it left.
            cache_release_detail(&catalog, &client, record.release_id);
            // The price does *not* survive the move: `upsert_vinyl` doesn't
            // write that column (it's set separately by the price pass), so the
            // re-keyed row starts blank and has to be priced again.
            cache_release_price(&catalog, &client, to, moved.instance_id, moved.release_id);
            format!("Moved {} to your {}.", record.title, list_name(to))
        }

        VinylEdit::Remove { list, record } => {
            if let Err(e) = remove_from(&client, &username, list, &record) {
                let _ = tx.send(JobMsg::Failed(format!(
                    "removing {} from your {}: {e}",
                    record.title,
                    list_name(list)
                )));
                ctx.request_repaint();
                return;
            }
            let _ = catalog.delete_vinyl(list, record.instance_id);
            format!("Removed {} from your {}.", record.title, list_name(list))
        }

        VinylEdit::Swap {
            list,
            record,
            to_release,
            to_label,
        } => {
            // Add the replacement first. If this fails nothing has changed, so
            // the user still owns the pressing they started with.
            let added = match list {
                VinylList::Wantlist => client
                    .add_to_wantlist(&username, to_release)
                    .map(|rec| rec.map(|r| (r.instance_id, Some(r)))),
                VinylList::Collection => client
                    .add_to_collection(&username, to_release)
                    .map(|instance_id| Some((instance_id, None))),
            };
            let added = match added {
                Ok(a) => a,
                Err(e) => {
                    let _ = tx.send(JobMsg::Failed(format!(
                        "adding {to_label} to your {}: {e}",
                        list_name(list)
                    )));
                    ctx.request_repaint();
                    return;
                }
            };
            // Now drop the pressing being replaced. A failure here leaves both
            // pressings in the list on Discogs — say exactly that, and leave the
            // cache alone so the next sync shows the user that real state.
            if let Err(e) = remove_from(&client, &username, list, &record) {
                let _ = tx.send(JobMsg::Failed(format!(
                    "{to_label} is now in your {}, but removing {} failed: {e}",
                    list_name(list),
                    record.title
                )));
                ctx.request_repaint();
                return;
            }
            let _ = catalog.delete_vinyl(list, record.instance_id);
            // Cache the incoming row so the grid shows the new pressing straight
            // away rather than a gap until the next sync. The wantlist add hands
            // back the row itself; a collection add answers with an instance id
            // only, so that one is looked up.
            let fetched = match added {
                Some((instance_id, Some(rec))) => Some((instance_id, Some(rec))),
                Some((instance_id, None)) => Some((
                    instance_id,
                    client
                        .collection_record(&username, to_release, instance_id)
                        .ok()
                        .flatten(),
                )),
                None => None,
            };
            if let Some((instance_id, Some(rec))) = fetched {
                let _ = catalog.upsert_vinyl(list, &rec);
                if let Some(url) = rec.cover_url.as_deref() {
                    if let Some(png) = client.fetch_cover(url) {
                        let _ = catalog.set_vinyl_cover(list, rec.instance_id, &png);
                    }
                }
                cache_release_detail(&catalog, &client, to_release);
                cache_release_price(&catalog, &client, list, instance_id, to_release);
            }
            format!(
                "Swapped {} for {to_label} in your {}.",
                record.title,
                list_name(list)
            )
        }
    };

    let _ = tx.send(JobMsg::Done(done));
    ctx.request_repaint();
}

/// Cache a newly-added release's Discogs detail so its tracklist is searchable
/// straight away, rather than only after the next vinyl sync's warming pass.
///
/// Best-effort and already-cached-aware: `release_cached_or` serves a hit without
/// a request, so re-adding a record the user previously owned costs nothing. A
/// failure leaves the release uncached and the next sync retries it.
fn cache_release_detail(catalog: &Catalog, client: &discogs::Client, release_id: u64) {
    let id = release_id.to_string();
    let _ = catalog.release_cached_or(&id, || client.fetch_release(&id));
}

/// Price a newly-added record so it lands complete rather than showing a blank
/// price until the next sync.
///
/// Prices are otherwise filled only by the sync's price pass, so without this an
/// add left the grid's price column blank on exactly the record the user just
/// chose — until they happened to run a sync. That's the one column that says
/// whether a record is worth anything, so a fresh add showing nothing there
/// reads as broken rather than as pending.
///
/// One request, and best-effort like the cover and tracklist beside it: a record
/// that can't be priced (blocked from sale, or a failed call) simply stays
/// unpriced and the next sync retries it, rather than failing the add that
/// already succeeded on Discogs.
fn cache_release_price(
    catalog: &Catalog,
    client: &discogs::Client,
    list: VinylList,
    instance_id: u64,
    release_id: u64,
) {
    if let Ok(price) = client.marketplace_price(release_id) {
        let _ = catalog.set_vinyl_price(
            list,
            instance_id,
            price.as_ref().map(|p| p.value),
            price.as_ref().map(|p| p.currency.as_str()),
        );
    }
}

/// Drop one record from `list` on Discogs. A want is addressed by release id; a
/// collection copy by the folder that holds it plus its instance id.
///
/// Rows cached before folders were recorded have no folder on file, so those ask
/// Discogs which folder holds the copy rather than guessing — guessing would fail
/// for anyone who files records into folders of their own. Everything synced
/// since already knows its folder and removes in one call.
fn remove_from(
    client: &discogs::Client,
    username: &str,
    list: VinylList,
    record: &VinylRecord,
) -> Result<(), ordnung_core::Error> {
    match list {
        VinylList::Wantlist => client.remove_from_wantlist(username, record.release_id),
        VinylList::Collection => {
            let folder = match record.folder_id {
                Some(f) => Some(f),
                None => {
                    client.collection_folder_of(username, record.release_id, record.instance_id)?
                }
            };
            client.remove_from_collection(username, folder, record.release_id, record.instance_id)
        }
    }
}

/// How to name a Discogs list in a status message ("Moved X to your collection").
fn list_name(list: VinylList) -> &'static str {
    match list {
        VinylList::Collection => "collection",
        VinylList::Wantlist => "wantlist",
    }
}

/// Discogs artwork lookup for every track that has neither an embedded cover
/// nor a prior Discogs attempt on file. Paced at one request per ~1.1 s to
/// stay comfortably under the 60/min authenticated rate limit. Candidate
/// releases are streamed back to the UI as `ArtworkChoices` for the user to
/// pick from; nothing is written to the catalog here. Honours `cancel`.
/// Search Discogs for an explicit set of tracks (the right-click menu's
/// "Find Discogs release" / ↻ re-pick action) and queue
/// each track's candidate releases as `ArtworkChoices`, one queued entry per
/// track, ignoring the fetched-marker. The picker (reading `artwork_enrich`)
/// applies the chosen release's cover and, in song-details mode, its tags.
/// Nothing is written here.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_fetch_tracks(
    db: PathBuf,
    token: String,
    ids: Vec<Id>,
    cancel: Arc<AtomicBool>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
    enrich: bool,
    hidden_mediums: Vec<String>,
) {
    const MAX_CANDIDATES: usize = 6;
    // The medium filter travels as bare keys; rebuild the tiny bit of `Config`
    // that answers "show this format?" rather than shipping the whole struct.
    let medium_filter = config::Config {
        hidden_release_mediums: hidden_mediums,
        ..config::Config::default()
    };
    let catalog = match Catalog::open(&db) {
        Ok(c) => c,
        Err(e) => {
            let _ = tx.send(JobMsg::Failed(format!("opening catalog: {e}")));
            ctx.request_repaint();
            return;
        }
    };
    let client = discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
    let total = ids.len();
    // Every Discogs search is paced to ~1.1 s (and a track can cost up to four
    // of them), so a multi-track fetch runs for minutes. Report determinate
    // progress like every other long job here does, rather than leaving the
    // user on an indeterminate spinner with no idea how far along it is.
    let _ = tx.send(JobMsg::Progress { done: 0, total });
    ctx.request_repaint();
    let (mut queued, mut none, mut skipped, mut errored) = (0u64, 0u64, 0u64, 0u64);
    // Tracks whose only releases were hidden by the medium filter — reported
    // apart from a true no-match, since the fix is a setting, not more tagging.
    let mut filtered = 0u64;
    let mut fails: Vec<(String, String)> = Vec::new();
    for (i, track_id) in ids.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let _ = tx.send(JobMsg::Progress { done: i, total });
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
        // Name the track in flight: at ~1.1 s per search the bar alone moves
        // too slowly to read as progress.
        let _ = tx.send(JobMsg::Status(format!(
            "Searching Discogs ({}/{total}) {label}",
            i + 1
        )));
        ctx.request_repaint();
        if artist.is_empty() && title.is_none() && album.is_none() {
            skipped += 1;
            fails.push((
                label,
                "no artist, title, or album tag to search Discogs with".into(),
            ));
            continue;
        }
        match client
            .find_artwork_candidates(&artist, title.as_deref(), album.as_deref())
            .map(|found| {
                // Drop the carriers the user doesn't collect *before* the
                // candidate cap, so a CD-heavy result can't crowd the vinyl
                // pressings out of the six slots the picker shows. `hit_any`
                // remembers that Discogs *did* know this track, so a list
                // emptied purely by the filter isn't mistaken for a no-match.
                let hit_any = !found.is_empty();
                let kept: Vec<_> = found
                    .into_iter()
                    .filter(|c| medium_filter.shows_release_format(&c.format))
                    .collect();
                (kept, hit_any)
            }) {
            Ok((found, _)) if !found.is_empty() => {
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
            Ok((_, hit_any)) => {
                if hit_any {
                    // Discogs had releases; the medium filter hid all of them.
                    // Deliberately *not* marked fetched — nothing was reviewed,
                    // and widening the filter should bring this track back.
                    filtered += 1;
                    fails.push((
                        label,
                        "every release Discogs found is on a format hidden in \
                         Settings › Discogs"
                            .into(),
                    ));
                } else {
                    none += 1;
                    // A real no-match: Discogs has nothing for this track. Mark
                    // it fetched so it leaves the "recently added" inbox instead
                    // of lingering forever with nothing to populate it.
                    // (Artwork-only runs leave the marker alone.) Re-runnable
                    // via the menu's ↻ re-pick.
                    if enrich {
                        let _ = catalog.mark_metadata_fetched(track_id);
                    }
                }
            }
            Err(e) => {
                errored += 1;
                fails.push((label, format!("Discogs search failed: {e}")));
            }
        }
        ctx.request_repaint();
    }
    let _ = tx.send(JobMsg::Progress { done: total, total });
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
        } else if filtered == 1 {
            "Only hidden formats found. Check Settings › Discogs.".to_string()
        } else if none == 1 {
            "No Discogs release found.".to_string()
        } else if skipped == 1 {
            "Not enough tags to search Discogs — add an artist or title first.".to_string()
        } else {
            "Couldn't search Discogs for that track.".to_string()
        }
    } else {
        // Only name the filter when it actually cost the run something, so the
        // usual roll-up doesn't grow a permanent "0 hidden".
        let hidden_note = if filtered > 0 {
            format!(", {filtered} hidden by format")
        } else {
            String::new()
        };
        format!(
            "Discogs fetch: {queued} ready to pick, {none} no match{hidden_note}, \
             {skipped} skipped, {errored} error(s)."
        )
    };
    let _ = tx.send(JobMsg::Done(done));
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

/// Physical RAM in bytes, or `None` if the platform won't say.
///
/// Reads `hw.memsize` via sysctl — always present on macOS, and cheaper than
/// pulling in a system-info crate for one number.
fn physical_memory_bytes() -> Option<u64> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", "hw.memsize"])
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// A rayon pool sized so a full-library analysis doesn't drive a small-RAM Mac
/// into swap, or `None` to use rayon's default (one worker per logical core).
///
/// Each analysis worker holds a whole decoded track plus its spectrogram — call it
/// ~350 MB of headroom apiece at the 20-minute decode ceiling. The default pool
/// ignores memory entirely, so a 2015 8 GB MacBook Pro (4c/8t) would start eight of
/// those at once and thrash. Budget a quarter of physical RAM for analysis and
/// derive the worker count from that, always leaving at least one worker and never
/// exceeding what the default pool would have used.
///
/// A quarter (not half) is what actually binds where it matters: analysis runs
/// *alongside* the OS, the catalog, and the GUI's cover textures, and the app has
/// to stay responsive while it works. In practice this clamps 8 GB machines to
/// 4-5 workers and leaves 16 GB and up at full core count.
pub(crate) fn analysis_pool() -> Option<rayon::ThreadPool> {
    /// Rough peak footprint of one in-flight analysis worker.
    const BYTES_PER_WORKER: u64 = 350 * 1024 * 1024;

    let cores = std::thread::available_parallelism().map(|n| n.get()).ok()?;
    let budget = physical_memory_bytes()? / 4;
    let affordable = (budget / BYTES_PER_WORKER).max(1) as usize;
    let workers = affordable.min(cores);
    // Nothing to gain from a custom pool when memory isn't the binding constraint.
    if workers >= cores {
        return None;
    }
    rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .build()
        .ok()
}

#[cfg(test)]
mod usb_transfer_tests {
    use super::*;

    /// The whole point of the transfer: files leave the stick's layout
    /// (minus the export's Contents/ wrapper) and land in the library
    /// mirror-structured; an identical file already there isn't re-copied;
    /// a same-name different file keeps both instead of overwriting.
    #[test]
    fn transfer_copies_with_layout_dedupe_and_no_overwrites() {
        let base = std::env::temp_dir().join(format!("ordnung-transfer-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let vol = base.join("STICK");
        let dest = base.join("library");
        let a = vol.join("Contents/Artist/Album/a.mp3");
        let b = vol.join("Contents/Artist/Album/b.mp3");
        let c = vol.join("loose.mp3"); // plain stick file, no Contents wrapper
        for f in [&a, &b, &c] {
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        }
        std::fs::write(&a, b"aaaa").unwrap();
        std::fs::write(&b, b"bbbb").unwrap();
        std::fs::write(&c, b"cccc").unwrap();
        // b already exists locally, identical: must be skipped, not re-copied.
        std::fs::create_dir_all(dest.join("Artist/Album")).unwrap();
        std::fs::write(dest.join("Artist/Album/b.mp3"), b"bbbb").unwrap();
        // a exists locally with DIFFERENT bytes: both must survive.
        std::fs::write(dest.join("Artist/Album/a.mp3"), b"local-version").unwrap();

        let db = base.join("catalog.db");
        let (tx, _rx) = mpsc::channel();
        run_usb_transfer(
            db,
            vec![a, b, c],
            vol,
            dest.clone(),
            None,
            Arc::new(AtomicBool::new(false)),
            tx,
            egui::Context::default(),
            false,
        );

        assert_eq!(
            std::fs::read(dest.join("Artist/Album/a.mp3")).unwrap(),
            b"local-version",
            "an existing local file must never be overwritten"
        );
        assert_eq!(
            std::fs::read(dest.join("Artist/Album/a (2).mp3")).unwrap(),
            b"aaaa",
            "the device copy lands under a numbered name"
        );
        assert_eq!(std::fs::read(dest.join("Artist/Album/b.mp3")).unwrap(), b"bbbb");
        assert!(
            !dest.join("Artist/Album/b (2).mp3").exists(),
            "an identical file must not be duplicated"
        );
        assert_eq!(std::fs::read(dest.join("loose.mp3")).unwrap(), b"cccc");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn unique_destination_counts_up_from_two() {
        let dir = std::env::temp_dir().join(format!("ordnung-uniq-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("song.mp3");
        std::fs::write(&f, b"x").unwrap();
        assert_eq!(unique_destination(&f), dir.join("song (2).mp3"));
        std::fs::write(dir.join("song (2).mp3"), b"x").unwrap();
        assert_eq!(unique_destination(&f), dir.join("song (3).mp3"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
