//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    pub(crate) fn new(db_path: PathBuf, egui_ctx: egui::Context) -> Self {
        // Install the Inter font stack and push the design tokens into egui's
        // global style before any text is laid out, so every stock widget already
        // matches the Ordnung visual language (see `ui::theme`). DejaVu Sans stays
        // in the fallback chain for the wide-Unicode glyphs Inter lacks.
        crate::ui::theme::install(&egui_ctx);
        // Clone the context before it's moved into the audio engine below, so we
        // can hand it to the startup background refresh once the app is built.
        let startup_ctx = egui_ctx.clone();
        let (cover_tx, cover_rx) = mpsc::channel();
        let (art_save_tx, art_save_rx) = mpsc::channel();
        let (preview_tx, preview_rx) = mpsc::channel();
        // Persistent thumbnail loader: one long-lived catalog connection serves
        // every visible-row cover, so we never pay a fresh `Catalog::open` per
        // thumbnail and the disk read + PNG decode stay off the UI thread.
        let (thumb_req_tx, thumb_req_rx) = mpsc::channel::<Id>();
        let (thumb_tx, thumb_rx) = mpsc::channel();
        spawn_thumb_loader(db_path.clone(), egui_ctx.clone(), thumb_req_rx, thumb_tx);
        // A second long-lived loader for vinyl-collection cover art, keyed by
        // Discogs instance_id (reusing the `CoverLoaded` carrier).
        let (vinyl_cover_req_tx, vinyl_cover_req_rx) = mpsc::channel::<u64>();
        let (vinyl_cover_tx, vinyl_cover_rx) = mpsc::channel();
        spawn_vinyl_cover_loader(
            db_path.clone(),
            egui_ctx.clone(),
            vinyl_cover_req_rx,
            vinyl_cover_tx,
        );
        // Resolves the now-playing cover to a temp file off-thread (see
        // `now_playing_cover_url`) so the OS Now Playing panel can show artwork
        // without blocking the UI on a catalog read when a track starts.
        let (media_cover_tx, media_cover_rx) = mpsc::channel::<(Id, Option<String>)>();
        let (hires_tx, hires_rx) = mpsc::channel::<(Id, Vec<u8>)>();
        let mut app = App {
            db_path,
            rows: Vec::new(),
            filter: String::new(),
            selected: None,
            selection: HashSet::new(),
            select_anchor: None,
            selected_track: None,
            selected_has_external_art: false,
            convert_modal: None,
            batch_convert: None,
            job_rx: None,
            status: String::new(),
            status_last: String::new(),
            status_shown_at: 0.0,
            progress: None,
            load_error: None,
            cover_cache: HashMap::new(),
            thumb_req_tx,
            thumb_rx,
            cover_full_cache: HashMap::new(),
            cover_inflight: HashSet::new(),
            cover_tx,
            cover_rx,
            vinyl: Vec::new(),
            vinyl_count: 0,
            vinyl_covers: HashMap::new(),
            vinyl_cover_req_tx,
            vinyl_cover_rx,
            vinyl_links: HashMap::new(),
            scroll_to_track: None,
            row_screen_rects: Vec::new(),
            cover_drop: None,
            show_inspector: false,
            job_cancel: None,
            artwork_queue: VecDeque::new(),
            artwork_enrich: false,
            artwork_overwrite: false,
            artwork_set_cover: true,
            artwork_apply_album: true,
            artwork_album_overwrite: false,
            artwork_album_count: None,
            artwork_album_siblings: None,
            artwork_selected: 0,
            artwork_previews: None,
            artwork_saving: false,
            art_save_tx,
            art_save_rx,
            preview_tx,
            preview_rx,
            preview_cache: HashMap::new(),
            preview_inflight: HashSet::new(),
            release_detail_cache: HashMap::new(),
            config: Config::default(),
            column_order: TableColumn::DEFAULT_ORDER.to_vec(),
            hidden_columns: HashSet::new(),
            column_widths: HashMap::new(),
            column_widths_dirty: false,
            reset_column_widths: false,
            column_menu: None,
            col_filters: HashMap::new(),
            col_filter_open: None,
            tex_graveyard: Vec::new(),
            settings_open: false,
            settings_tab: SettingsTab::default(),
            token_input: String::new(),
            confirm_clear_db: false,
            failure_report_title: String::new(),
            failure_report: Vec::new(),
            show_failure_report: false,
            audio: AudioEngine::new(egui_ctx),
            media_cover_tx,
            media_cover_rx,
            hires_tx,
            hires_rx,
            tag_edit: TagEdit::default(),
            tag_edit_saved: TagEdit::default(),
            edited_count: 0,
            missing_count: 0,
            recent_count: 0,
            recent_pinned: HashSet::new(),
            missing_labels: Vec::new(),
            confirm_bulk_write: false,
            confirm_delete: None,
            write_edits_running: false,
            playlists: Vec::new(),
            dup_groups: Vec::new(),
            dup_dirty: false,
            dup_loading: false,
            dup_rx: None,
            dup_decisions: HashMap::new(),
            dup_pending_bulk: None,
            dup_confirm_pos: None,
            missing_list: Vec::new(),
            missing_pending_remove: None,
            view: LibraryView::Library,
            renaming: None,
            sort: None,
            now_playing: None,
            scrub: None,
            wave_zoom_secs: crate::player::DEFAULT_ZOOM_SECS,
            wave_lane_h: crate::player::DEFAULT_LANE_H,
            update_rx: None,
            update_available: None,
        };
        let config = Config::load();
        app.token_input = config.discogs_token.clone();
        app.config = config;
        app.load_column_layout();
        // Seed the initial sort from the user's saved default (e.g. "Added,
        // newest first") before the first load so it's applied on launch.
        app.sort = app.default_sort();
        app.reload();
        app.recount_missing();
        // Refresh anything we always want current (Discogs vinyl collection)
        // in the background as soon as the catalog is loaded.
        app.spawn_startup_refresh(startup_ctx.clone());
        // Ask GitHub once, off-thread, whether a newer release is out. The result
        // drives a dismissible banner; a network failure is swallowed (no banner).
        app.spawn_update_check(startup_ctx);
        app
    }

    /// Fire the one-shot "is there a newer release?" check on a background
    /// thread, handing the result back through `update_rx`. Best-effort: any
    /// transport error resolves to `None`, so a flaky network never nags. The
    /// running version is the GUI crate's compile-time `CARGO_PKG_VERSION`, which
    /// inherits the workspace version stamped into each release build.
    pub(crate) fn spawn_update_check(&mut self, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.update_rx = Some(rx);
        thread::spawn(move || {
            let current = env!("CARGO_PKG_VERSION");
            let found = match ordnung_core::update::check_latest(current) {
                Ok(ordnung_core::update::UpdateOutcome::Update(info)) => Some(info),
                _ => None,
            };
            // Ignore send errors — the app may have closed before the check returned.
            let _ = tx.send(found);
            ctx.request_repaint();
        });
    }

    /// The Discogs token to use: the saved config value wins; if unset, fall
    /// back to the `DISCOGS_TOKEN` environment variable (so existing setups keep
    /// working). Returns an empty string when neither is set.
    pub(crate) fn discogs_token(&self) -> String {
        let saved = self.config.discogs_token.trim();
        if !saved.is_empty() {
            saved.to_string()
        } else {
            std::env::var("DISCOGS_TOKEN").unwrap_or_default()
        }
    }

    pub(crate) fn reload(&mut self) {
        // Refresh the sidebar's playlist tree first. If the viewed playlist was
        // deleted (or turned out to be a folder), fall back to the Library so the
        // table never queries a playlist that no longer exists.
        self.playlists = Catalog::open(&self.db_path)
            .and_then(|c| c.list_playlists())
            .unwrap_or_default();
        if let LibraryView::Playlist(id) = self.view {
            let still_valid = self.playlists.iter().any(|p| p.id == id && !p.is_folder);
            if !still_valid {
                self.view = LibraryView::Library;
            }
        }
        match load_rows(&self.db_path, &self.filter, &self.view, &self.recent_pinned) {
            Ok(rows) => {
                // Narrow to the rows passing every active per-column filter before
                // computing the live set, so the selection/cover bookkeeping below
                // only ever references rows the user can actually see.
                let rows = self.apply_col_filters(rows);
                // Evict cover textures for tracks that are no longer in the
                // visible set; keep the ones still present (the texture id is
                // stable since track ids don't change). Evicted handles go to
                // the graveyard (dropped next frame), not straight to drop —
                // `reload` runs mid-frame and a same-frame free panics wgpu
                // (see `tex_graveyard`).
                let live: std::collections::BTreeSet<Id> = rows.iter().map(|r| r.id).collect();
                let dead: Vec<Id> =
                    self.cover_cache.keys().filter(|id| !live.contains(id)).copied().collect();
                for id in dead {
                    if let Some(ThumbState::Ready(Some(tex))) = self.cover_cache.remove(&id) {
                        self.tex_graveyard.push(tex);
                    }
                }
                let dead: Vec<Id> = self
                    .cover_full_cache
                    .keys()
                    .filter(|id| !live.contains(id))
                    .copied()
                    .collect();
                for id in dead {
                    if let Some(Some(tex)) = self.cover_full_cache.remove(&id) {
                        self.tex_graveyard.push(tex);
                    }
                }
                self.cover_inflight.retain(|id| live.contains(id));
                // Drop any selected/anchor ids that filtered out of the view so a
                // drag-out never references a row the user can't see.
                self.selection.retain(|id| live.contains(id));
                if self.select_anchor.is_some_and(|id| !live.contains(&id)) {
                    self.select_anchor = None;
                }
                self.rows = rows;
                self.apply_sort();
                self.load_error = None;
                // Pin whatever Recent currently shows so a track that finishes
                // (analyzed + fetched) on the next reload stays put instead of
                // disappearing mid-glance. Entering/leaving the tab resets this
                // (see the view-change handler), which is what eventually expires
                // the completed tracks.
                if self.view == LibraryView::RecentlyAdded {
                    self.recent_pinned = self.rows.iter().map(|r| r.id).collect();
                }
            }
            Err(e) => {
                self.rows.clear();
                // Park the cleared textures until next frame — same mid-frame
                // free hazard as the eviction above (see `tex_graveyard`).
                self.tex_graveyard.extend(
                    self.cover_cache
                        .drain()
                        .filter_map(|(_, s)| match s {
                            ThumbState::Ready(tex) => tex,
                            ThumbState::Loading => None,
                        }),
                );
                self.tex_graveyard
                    .extend(self.cover_full_cache.drain().filter_map(|(_, tex)| tex));
                self.cover_inflight.clear();
                self.selection.clear();
                self.select_anchor = None;
                self.load_error = Some(e);
            }
        }
        // Refresh the count of tracks pending a source-file write (drives the
        // bulk-write button). Independent of the visible filter — it reflects the
        // whole catalog. A failure here just leaves the button hidden.
        self.edited_count = Catalog::open(&self.db_path)
            .and_then(|c| c.count_edited())
            .unwrap_or(0);

        // The "recently added" inbox count drives the sidebar badge. It's a cheap
        // count (no Track building) and view-independent, so refresh it on every
        // reload — that's what makes tracks visibly drop off as they're analyzed
        // and fetched. A failure just hides the badge.
        self.recent_count = Catalog::open(&self.db_path)
            .and_then(|c| c.count_recently_added(ANALYZER_VERSION))
            .unwrap_or(0);

        // The duplicate finder is a full-catalog scan (the acoustic pass decodes and
        // slides every fingerprint against its duration neighbours), so it must not
        // run synchronously here — `reload` is on the UI thread and called for
        // unrelated refreshes. Just flag the cache stale; `poll_duplicates` (which
        // has the egui `Context`) runs the scan off-thread. Clear it when the view
        // isn't showing to free the held Tracks.
        if self.view == LibraryView::Duplicates {
            self.dup_dirty = true;
        } else if !self.dup_groups.is_empty() {
            self.dup_groups = Vec::new();
        }

        // Likewise, only stat the catalog for the Missing view while it's showing;
        // keep the toolbar/sidebar count in sync with what the view displays.
        if self.view == LibraryView::Missing {
            self.missing_list = Catalog::open(&self.db_path)
                .and_then(|c| c.missing_tracks())
                .unwrap_or_default();
            self.missing_count = self.missing_list.len() as u64;
            // Keep the relocate-button hover list in step with the view (e.g. as
            // rows are removed) without a second catalog round-trip.
            self.missing_labels = self
                .missing_list
                .iter()
                .map(|t| {
                    let artist = t.tags.artist.as_deref().unwrap_or("").trim();
                    let title = t.tags.title.as_deref().unwrap_or("").trim();
                    match (artist.is_empty(), title.is_empty()) {
                        (false, false) => format!("{artist} — {title}"),
                        (true, false) => title.to_string(),
                        (false, true) => artist.to_string(),
                        (true, true) => Path::new(&t.source_path)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| t.source_path.clone()),
                    }
                })
                .collect();
        } else if !self.missing_list.is_empty() {
            self.missing_list = Vec::new();
        }

        // Keep the sidebar's vinyl badge current from the cache count regardless
        // of the active view; only hold the full record list (and its cover
        // textures) while the grid is actually showing.
        self.vinyl_count = Catalog::open(&self.db_path)
            .and_then(|c| c.vinyl_count())
            .unwrap_or(0);
        if self.view == LibraryView::Vinyl {
            self.vinyl = Catalog::open(&self.db_path)
                .and_then(|c| c.list_vinyl())
                .unwrap_or_default();
            // Drop cover textures for records no longer present.
            let live: std::collections::BTreeSet<u64> =
                self.vinyl.iter().map(|v| v.instance_id).collect();
            self.vinyl_covers.retain(|id, _| live.contains(id));
            // Cross-reference the catalog: which records do we already own a
            // digital copy of? Build release_id → [track_id] once for the grid.
            // Exact release-id links first, metadata matching as a fallback.
            self.vinyl_links = Catalog::open(&self.db_path)
                .and_then(|c| c.vinyl_catalog_links(&self.vinyl))
                .map(|pairs| {
                    let mut m: HashMap<u64, Vec<Id>> = HashMap::new();
                    for (rid, tid) in pairs {
                        m.entry(rid).or_default().push(tid);
                    }
                    m
                })
                .unwrap_or_default();
        } else if !self.vinyl.is_empty() {
            self.vinyl = Vec::new();
            self.vinyl_covers.clear();
            self.vinyl_links = HashMap::new();
        }
    }

    /// Adopt a finished off-thread duplicate scan and start a fresh one when the
    /// cache is stale. The acoustic-fingerprint pass is a full-catalog scan, so it
    /// runs on a worker thread (which holds the egui `Context` to wake the UI when
    /// done) rather than blocking the frame the user clicks the tab. `dup_dirty`
    /// bursts from successive `reload`s coalesce: only one scan runs at a time, and
    /// a dirty flag set during a scan triggers exactly one rescan when it lands.
    fn poll_duplicates(&mut self, ctx: &egui::Context) {
        if let Some(rx) = &self.dup_rx {
            if let Ok(groups) = rx.try_recv() {
                self.dup_groups = groups;
                self.dup_loading = false;
                self.dup_rx = None;
            }
        }
        if self.view != LibraryView::Duplicates || !self.dup_dirty || self.dup_loading {
            return;
        }
        self.dup_dirty = false;
        self.dup_loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.dup_rx = Some(rx);
        let db = self.db_path.clone();
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let groups = Catalog::open(&db)
                .and_then(|c| c.find_duplicates())
                .unwrap_or_default();
            let _ = tx.send(groups);
            ctx.request_repaint();
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Drop cover textures evicted during the PREVIOUS frame. Doing it here —
        // before anything paints or uploads — guarantees the frame that painted
        // them has already been submitted to the GPU (see `tex_graveyard`).
        self.tex_graveyard.clear();
        if self.poll_worker() {
            self.reload();
            self.refresh_selected();
            self.recount_missing();
        }
        self.poll_covers(ctx);
        self.poll_thumbs(ctx);
        self.poll_vinyl_covers(ctx);
        self.poll_artwork_save();
        self.poll_metadata_preview();

        // Fade an idle status message out of the bottom-left bar after a short
        // while, so a one-off "Synced…/Done…" note doesn't linger forever. We
        // never expire it mid-job (the running status is live state); the timer
        // restarts whenever the message text changes. A repaint is scheduled so
        // the bar clears on its own even when the app is otherwise idle.
        const STATUS_TTL: f64 = 15.0;
        if self.status != self.status_last {
            self.status_last = self.status.clone();
            self.status_shown_at = ctx.input(|i| i.time);
        }
        if !self.status.is_empty() && !self.is_busy() {
            let age = ctx.input(|i| i.time) - self.status_shown_at;
            if age >= STATUS_TTL {
                self.status.clear();
                self.status_last.clear();
            } else {
                ctx.request_repaint_after(std::time::Duration::from_secs_f64(STATUS_TTL - age));
            }
        }

        // Cmd/Ctrl+A selects every visible row in the current view or playlist
        // (`self.rows` is already narrowed to the active tab and column filters).
        // Skip it while a text field — search, per-column filter — owns the
        // keyboard so the shortcut keeps its in-field "select all text" meaning.
        if !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::A))
        {
            self.selection = self.rows.iter().map(|r| r.id).collect();
            if self.selected.is_none() {
                if let Some(first) = self.rows.first().map(|r| r.id) {
                    self.set_primary(Some(first));
                }
            }
        }

        // Cmd/Ctrl+W: close the frontmost "window" — the floating Settings window
        // if it's open, otherwise the app window itself (Ordnung is single-window,
        // so that quits like the red traffic-light button). Transient confirmation
        // dialogs already close with Escape.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::W)) {
            if self.settings_open {
                self.settings_open = false;
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }

        // Drive the snippet-preview engine: pick up finished decodes, notice when
        // a clip ends, and keep animating the button while audio is active.
        if let Some(a) = &mut self.audio {
            a.poll();
            if let Some(err) = a.last_error.take() {
                self.status = err;
            }
            if a.is_active() {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
        }

        // Space bar = play/pause. Toggle the loaded track if one is in the bar;
        // otherwise start the selected row. Skipped while a text field has focus so
        // typing a space in the filter/edit fields doesn't hijack playback.
        if !ctx.wants_keyboard_input()
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Space))
        {
            if self.now_playing.is_some() {
                if let Some(a) = &mut self.audio {
                    a.toggle_pause();
                }
            } else if let Some(id) = self.selected {
                if let Some(path) = self
                    .rows
                    .iter()
                    .find(|r| r.id == id)
                    .map(|r| r.source_path.clone())
                {
                    self.play_track(id, path);
                }
            }
        }

        // Attach any resolved now-playing cover to the OS panel, ignoring results
        // for a track the user has since moved on from.
        while let Ok((id, url)) = self.media_cover_rx.try_recv() {
            if let Some(a) = &mut self.audio {
                if a.current() == Some(id) {
                    a.set_now_playing_cover(url);
                }
            }
        }

        // Attach the off-thread hi-res zoom envelope to the now-playing track,
        // dropping results for a track the user has since moved on from.
        while let Ok((id, hires)) = self.hires_rx.try_recv() {
            if let Some(n) = self.now_playing.as_mut() {
                if n.id == id {
                    n.hires_bands = Some(hires);
                }
            }
        }

        // Pick up the startup update check's verdict (once). A hit populates the
        // banner below; `None` (up to date or check failed) leaves it hidden.
        if let Some(rx) = &self.update_rx {
            if let Ok(found) = rx.try_recv() {
                self.update_available = found;
                self.update_rx = None;
            }
        }

        // "New version available" strip, above the toolbar. Shown only while an
        // update is pending; the user can open the download page or dismiss it.
        if let Some(info) = self.update_available.clone() {
            egui::TopBottomPanel::top("update_banner")
                .frame(
                    egui::Frame::none()
                        .fill(egui::Color32::from_rgb(64, 110, 180))
                        .inner_margin(egui::Margin::symmetric(10.0, 6.0)),
                )
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            egui::RichText::new(format!("Ordnung {} is available", info.version))
                                .color(egui::Color32::WHITE)
                                .strong(),
                        );
                        ui.label(
                            egui::RichText::new(format!("(you have {})", env!("CARGO_PKG_VERSION")))
                                .color(egui::Color32::from_rgb(220, 230, 245)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("✕")
                                .on_hover_note("Dismiss until the next launch")
                                .clicked()
                            {
                                self.update_available = None;
                            }
                            if ui
                                .button(egui::RichText::new("Download").strong())
                                .on_hover_note("Open the release page in your browser")
                                .clicked()
                            {
                                open_url(&info.url);
                            }
                        });
                    });
                });
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                let busy = self.is_busy();
                ui.add_enabled_ui(!busy, |ui| {
                    // "Add songs…" opens a small menu: pick individual files, or a
                    // whole folder. Both import into the catalog; source files are
                    // never moved or modified, and unchanged files are skipped on a
                    // re-add (same size + mtime), so it's never a full re-read.
                    // Primary action: an accent fill marks it as the toolbar's
                    // main entry point (it's the only action that grows the library).
                    let add_btn = egui::Button::new(
                        egui::RichText::new("Add songs…").color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(64, 110, 180));
                    let add = egui::menu::menu_custom_button(ui, add_btn, |ui| {
                        if ui
                            .button("🎵  Choose files…")
                            .on_hover_note("Add audio files")
                            .clicked()
                        {
                            let picked = rfd::FileDialog::new()
                                .add_filter(
                                    "Audio",
                                    &["mp3", "flac", "aiff", "aif", "wav", "m4a", "aac", "ogg"],
                                )
                                .pick_files();
                            if let Some(files) = picked {
                                if !files.is_empty() {
                                    self.spawn_import(ctx.clone(), files);
                                }
                            }
                            ui.close_menu();
                        }
                        if ui
                            .button("📁  Choose folder…")
                            .on_hover_note("Add a folder, subfolders included")
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.spawn_scan(ctx.clone(), dir);
                            }
                            ui.close_menu();
                        }
                    });
                    add.response.on_hover_note(
                        "Add files or a folder to the catalog. Source files are never modified.",
                    );
                    ui.separator();
                    // When rows are selected, the toolbar buttons act on just that
                    // selection (in visible order); otherwise they fall back to the
                    // whole filtered view. The label reflects which, so a user who
                    // picked a few tracks isn't surprised by a full-library run.
                    let sel_ids: Vec<Id> = self
                        .rows
                        .iter()
                        .filter(|x| self.selection.contains(&x.id))
                        .map(|x| x.id)
                        .collect();
                    // Analysis cluster: the common "Analyze" stays one click; its
                    // less-frequent siblings (force re-analyze, Discogs metadata)
                    // tuck into an adjacent ▾ menu so the toolbar stays lean.
                    let analyze_label = if sel_ids.is_empty() {
                        "⚡ Analyze".to_string()
                    } else {
                        format!("⚡ Analyze {} selected", sel_ids.len())
                    };
                    if ui
                        .button(analyze_label)
                        .on_hover_note(
                            "Detect BPM, key, beatgrid, and quality. Skips tracks \
                             already analyzed.",
                        )
                        .clicked()
                    {
                        if sel_ids.is_empty() {
                            self.spawn_analyze(ctx.clone(), false);
                        } else {
                            self.spawn_analyze_ids(ctx.clone(), sel_ids.clone(), false);
                        }
                    }
                    let more = ui.menu_button("▾", |ui| {
                        let reanalyze_label = if sel_ids.is_empty() {
                            "Re-analyze (force)".to_string()
                        } else {
                            format!("Re-analyze {} selected (force)", sel_ids.len())
                        };
                        if ui
                            .button(reanalyze_label)
                            .on_hover_note("Re-analyze, including tracks already analyzed")
                            .clicked()
                        {
                            if sel_ids.is_empty() {
                                self.spawn_analyze(ctx.clone(), true);
                            } else {
                                self.spawn_analyze_ids(ctx.clone(), sel_ids.clone(), true);
                            }
                            ui.close_menu();
                        }
                        if ui
                            .button("Fetch song data…")
                            .on_hover_note(
                                "Pick a Discogs release to cache the cover and fill \
                                 empty fields. Never overwrites tags or files. Needs \
                                 a Discogs token (see Settings).",
                            )
                            .clicked()
                        {
                            self.spawn_fetch_artwork(ctx.clone(), true);
                            ui.close_menu();
                        }
                    });
                    more.response
                        .on_hover_note("More analysis & metadata actions");
                    // Batch convert: enabled whenever tracks are selected. Opens a
                    // dialog to pick one target format for all of them.
                    if !self.selection.is_empty() {
                        let n = self.selection.len();
                        let noun = if n == 1 { "track" } else { "tracks" };
                        if ui
                            .button(format!("Convert {n} {noun}…"))
                            .on_hover_note(
                                "Convert selected tracks to one format, keeping \
                                 metadata and cover.",
                            )
                            .clicked()
                        {
                            let ids: Vec<Id> = self
                                .rows
                                .iter()
                                .filter(|r| self.selection.contains(&r.id))
                                .map(|r| r.id)
                                .collect();
                            let (target, bitrate_kbps, out_dir, in_place) =
                                convert_defaults(&self.config);
                            self.batch_convert = Some(BatchConvert {
                                ids,
                                target,
                                bitrate_kbps,
                                out_dir,
                                in_place,
                                error: None,
                            });
                        }
                    }
                    // When viewing a playlist with a selection, offer to drop those
                    // tracks from it. Only unlinks the playlist membership — the
                    // tracks stay in the catalog (and in any other playlists).
                    let playlist_view = match &self.view {
                        LibraryView::Playlist(pid) => Some(*pid),
                        LibraryView::Library
                        | LibraryView::RecentlyAdded
                        | LibraryView::Duplicates
                        | LibraryView::Missing
                        | LibraryView::Vinyl => None,
                    };
                    if let Some(pid) = playlist_view {
                        if !self.selection.is_empty() {
                            let n = self.selection.len();
                            if ui
                                .button(format!("Remove {n} from playlist"))
                                .on_hover_note(
                                    "Remove from this playlist. Tracks stay in the catalog.",
                                )
                                .clicked()
                            {
                                let ids: Vec<Id> = self
                                    .rows
                                    .iter()
                                    .filter(|r| self.selection.contains(&r.id))
                                    .map(|r| r.id)
                                    .collect();
                                if let Ok(cat) = Catalog::open(&self.db_path) {
                                    let _ = cat.remove_tracks(pid, &ids);
                                }
                                self.reload();
                            }
                        }
                    }
                    // Deleting from the catalog lives in the right-click context
                    // menu (per-row), not the toolbar — it's a destructive action
                    // that should be reached deliberately on a selection.
                    // Bulk writeback: only shown when some tracks have catalog
                    // edits not yet written to their files. Mutates source files,
                    // so it's visually distinct and gated behind a confirmation.
                    if self.edited_count > 0 {
                        let label = format!("⬇ Write {} edited to files", self.edited_count);
                        let btn = egui::Button::new(
                            egui::RichText::new(label).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(70, 110, 70));
                        if ui
                            .add(btn)
                            .on_hover_note("Write edited tags into the source files")
                            .clicked()
                        {
                            self.confirm_bulk_write = true;
                        }
                    }
                    // Relocate: only shown when some tracks' source files are
                    // missing from disk. Pick a folder to search; files matched
                    // by name (and content fingerprint) are repointed in the
                    // catalog. Catalog-only — never moves or modifies files.
                    if self.missing_count > 0 {
                        let label = format!("🔗 Relocate {} missing", self.missing_count);
                        let btn = egui::Button::new(
                            egui::RichText::new(label).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(150, 90, 40));
                        let labels = &self.missing_labels;
                        let count = self.missing_count;
                        if ui
                            .add(btn)
                            .on_hover_ui(|ui| {
                                ui.set_max_width(420.0);
                                ui.label(
                                    crate::ui::hover::note(format!(
                                        "{count} track(s) point at a file that's gone"
                                    ))
                                    .strong(),
                                );
                                ui.separator();
                                // Cap the list so a huge backlog can't grow the
                                // tooltip off-screen; note the overflow instead.
                                const MAX: usize = 20;
                                for label in labels.iter().take(MAX) {
                                    ui.label(crate::ui::hover::note(label.as_str()));
                                }
                                if labels.len() > MAX {
                                    ui.add_space(2.0);
                                    ui.label(
                                        crate::ui::hover::note(format!(
                                            "…and {} more",
                                            labels.len() - MAX
                                        ))
                                        .weak(),
                                    );
                                }
                                ui.separator();
                                ui.weak(
                                    "Pick a folder to search; every file found there \
                                     by name (confirmed by content when names collide) \
                                     is repointed in the catalog. Your files are never \
                                     moved or modified.",
                                );
                            })
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.spawn_relocate(ctx.clone(), dir);
                            }
                        }
                    }
                });
                ui.separator();
                // Number of active column filters drives both the "Clear filters"
                // label and whether that button (and the inline ×) show at all.
                let active_col_filters =
                    self.col_filters.values().filter(|v| !v.is_empty()).count();
                let has_filters = active_col_filters > 0 || !self.filter.is_empty();
                // Live counts: total visible tracks, plus selection and missing
                // when they apply, so the toolbar always reflects current state.
                let mut counts = format!("{} tracks", self.rows.len());
                if !self.selection.is_empty() {
                    counts.push_str(&format!(" · {} selected", self.selection.len()));
                }
                if self.missing_count > 0 {
                    counts.push_str(&format!(" · {} missing", self.missing_count));
                }
                // Right-aligned utility group: counts and the non-workflow actions
                // (Refresh, Settings) live away from the left-edge library actions
                // so the toolbar reads "do work … status & config". Laying this out
                // right-to-left FIRST reserves its width, so the left-aligned filter
                // group nested inside shrinks to fit instead of overdrawing the
                // counts when the window is narrow. Visual order on the right:
                // counts · Refresh · Settings · Info.
                let busy = self.is_busy();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.show_inspector, "Info")
                        .on_hover_note("Show/hide the track info panel");
                    ui.separator();
                    // Settings stays reachable even while a job runs.
                    if ui
                        .button("⚙ Settings")
                        .on_hover_note("Discogs token and app options")
                        .clicked()
                    {
                        self.token_input = self.config.discogs_token.clone();
                        self.settings_open = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("↻ Refresh"))
                        .on_hover_note("Reload the table from the catalog")
                        .clicked()
                    {
                        self.reload();
                        self.recount_missing();
                    }
                    ui.separator();
                    ui.label(counts);
                    ui.separator();
                    // The filter group fills whatever horizontal space the utility
                    // group left over. Rendered left-to-right inside the reserved
                    // remainder so it can never collide with the counts.
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                        ui.label("Filter:");
                        // Reserve room for the inline × and the Clear-filters button
                        // so the text field shrinks rather than pushing them past
                        // the edge of this remainder.
                        let mut reserved = 0.0;
                        if !self.filter.is_empty() {
                            reserved += 28.0;
                        }
                        if has_filters {
                            reserved += 140.0;
                        }
                        let w = (ui.available_width() - reserved).clamp(120.0, 320.0);
                        let resp = ui.add(
                            egui::TextEdit::singleline(&mut self.filter)
                                .hint_text("artist / title / album / genre")
                                .desired_width(w),
                        );
                        if resp.changed() {
                            self.reload();
                        }
                        if !self.filter.is_empty() && ui.small_button("×").clicked() {
                            self.filter.clear();
                            self.reload();
                        }
                        // A prominent "clear all filters" button, shown only while a
                        // filter is actually hiding rows. This rescues the case where
                        // a forgotten column filter leaves the table looking empty.
                        if has_filters {
                            let label = if active_col_filters > 0 {
                                format!("⊘ Clear filters ({active_col_filters})")
                            } else {
                                "⊘ Clear filters".to_string()
                            };
                            if ui
                                .button(label)
                                .on_hover_note("Clear search and filters")
                                .clicked()
                            {
                                self.filter.clear();
                                self.col_filters.clear();
                                self.reload();
                            }
                        }
                    });
                });
            });
            ui.add_space(4.0);
        });

        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(2.0);
            let mut do_abort = false;
            ui.horizontal(|ui| {
                // Determinate bar when the job reports item counts; otherwise a
                // plain spinner for work whose length we can't measure.
                match self.progress {
                    Some((done, total)) if total > 0 => {
                        let frac = (done as f32 / total as f32).clamp(0.0, 1.0);
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(180.0)
                                .text(format!("{done}/{total}")),
                        );
                    }
                    _ if self.is_busy() => {
                        ui.spinner();
                    }
                    _ => {}
                }
                if self.job_cancel.is_some()
                    && ui
                        .button("Abort")
                        .on_hover_note("Stop after the current item")
                        .clicked()
                {
                    do_abort = true;
                }
                if let Some(err) = &self.load_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("catalog error: {err}"));
                } else {
                    // Always render a label, falling back to a blank space when
                    // there's no message, so the status bar keeps a constant
                    // height — only the text changes, the layout never shifts.
                    let text = if self.status.is_empty() {
                        " ".to_string()
                    } else {
                        self.status.clone()
                    };
                    ui.label(text);
                }
            });
            if do_abort {
                if let Some(cancel) = &self.job_cancel {
                    cancel.store(true, Ordering::Relaxed);
                }
                self.status = "Cancelling…".into();
            }
            ui.add_space(2.0);
        });

        // Spotify-style now-playing bar: artwork, title/artist, play-pause and a
        // draggable scrubber. Sits just above the status bar; only shown while a
        // track is loaded in (or decoding for) the player.
        self.draw_player(ctx);

        let mut inspector_action: Option<InspectorAction> = None;
        if self.show_inspector {
            egui::SidePanel::right("inspector")
                .resizable(true)
                .default_width(340.0)
                .width_range(220.0..=560.0)
                .show(ctx, |ui| {
                    inspector_action = self.draw_inspector(ui, ctx);
                });
        }
        match inspector_action {
            Some(InspectorAction::EmbedCover(id, path)) => self.embed_cover_into_file(id, path),
            Some(InspectorAction::SaveToCatalog(id)) => self.save_tags(id, None),
            Some(InspectorAction::WriteToFile(id, path)) => self.save_tags(id, Some(path)),
            None => {}
        }

        // Left sidebar: "All songs" (the whole catalog) plus the playlist/folder
        // tree. Plain-field state (`view`, `renaming`) is edited in place; catalog
        // mutations are raised as a `SidebarAction` and applied after the panel so
        // nothing borrows `self` while the tree renders. A view change triggers a
        // reload so the table follows the sidebar.
        let prev_view = self.view.clone();
        let mut sidebar_action: Option<SidebarAction> = None;
        egui::SidePanel::left("library_nav")
            .resizable(true)
            .default_width(200.0)
            .width_range(150.0..=360.0)
            .show(ctx, |ui| {
                // Header for a section: a small dimmed all-caps caption that sets
                // the playlist / collection groups apart without competing with
                // the big nav tiles below it.
                let section_caption = |ui: &mut egui::Ui, text: &str| {
                    ui.label(
                        egui::RichText::new(text)
                            .size(11.0)
                            .color(egui::Color32::from_gray(140))
                            .strong(),
                    );
                };

                // ── Library (top) ─────────────────────────────────────────────
                // The whole catalog. Biggest tile in the sidebar — it's the home
                // base every other view branches off from.
                egui::TopBottomPanel::top("nav_library")
                    .frame(egui::Frame::none())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(8.0);
                        // "All songs" is the home base — the big tile — paired on the
                        // same row with a smaller "Recent" tile: the self-clearing
                        // inbox of fresh imports still awaiting analysis + a Discogs
                        // fetch. Recent gets a fixed, narrower width; All songs flexes
                        // to fill the rest so the pair always spans the sidebar.
                        ui.horizontal(|ui| {
                            const RECENT_W: f32 = 76.0;
                            let gap = ui.spacing().item_spacing.x;
                            let all_w = (ui.available_width() - RECENT_W - gap).max(60.0);
                            if nav_button_sized(
                                ui,
                                "♪  All songs",
                                self.view == LibraryView::Library,
                                all_w,
                                46.0,
                                17.0,
                            )
                            .on_hover_note("Every track in the catalog")
                            .clicked()
                            {
                                self.view = LibraryView::Library;
                            }
                            let recent_label = if self.recent_count > 0 {
                                format!("✦ {}", self.recent_count)
                            } else {
                                "✦".to_string()
                            };
                            if nav_button_sized(
                                ui,
                                &recent_label,
                                self.view == LibraryView::RecentlyAdded,
                                RECENT_W,
                                46.0,
                                17.0,
                            )
                            .on_hover_note(
                                "New imports awaiting analysis or a Discogs fetch. \
                                 They drop off once both are done.",
                            )
                            .clicked()
                            {
                                self.view = LibraryView::RecentlyAdded;
                            }
                        });
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            section_caption(ui, "PLAYLISTS");
                            ui.with_layout(
                                egui::Layout::right_to_left(egui::Align::Center),
                                |ui| {
                                    // Hold the button off the panel's right clip
                                    // edge so its hover outline isn't cut off.
                                    ui.add_space(3.0);
                                    // Compact square button — without an explicit
                                    // min_size the "+" reads as a stretched pill.
                                    if ui
                                        .add(
                                            egui::Button::new("+")
                                                .min_size(egui::vec2(22.0, 22.0))
                                                .rounding(egui::Rounding::same(6.0)),
                                        )
                                        .on_hover_note("New playlist")
                                        .clicked()
                                    {
                                        sidebar_action = Some(SidebarAction::NewPlaylist(None));
                                    }
                                },
                            );
                        });
                        ui.add_space(4.0);
                    });

                // ── Pinned bottom views (no captions) ─────────────────────────
                // Two distinct groups, separated by spacing/rule rather than text
                // headers: external sources (the Discogs vinyl collection) on top,
                // then library-health diagnostics (Duplicates / Missing) below.
                // They read as their own group, set off from the playlist tree by
                // living in a separate pinned section.
                egui::TopBottomPanel::bottom("nav_collections")
                    .frame(egui::Frame::none())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        // ── Sources ──
                        let vinyl_label = if self.vinyl_count > 0 {
                            format!("💿  My Vinyl Collection ({})", self.vinyl_count)
                        } else {
                            "💿  My Vinyl Collection".to_string()
                        };
                        if nav_button(
                            ui,
                            &vinyl_label,
                            self.view == LibraryView::Vinyl,
                            34.0,
                            14.0,
                        )
                        .on_hover_note("Your Discogs vinyl collection")
                        .clicked()
                        {
                            self.view = LibraryView::Vinyl;
                        }
                        ui.add_space(6.0);
                        ui.separator();
                        ui.add_space(6.0);
                        // ── Library health ──
                        if nav_button(
                            ui,
                            "⧉  Duplicates",
                            self.view == LibraryView::Duplicates,
                            34.0,
                            14.0,
                        )
                        .on_hover_note("Find identical imports and same-song format variants")
                        .clicked()
                        {
                            self.view = LibraryView::Duplicates;
                        }
                        ui.add_space(4.0);
                        let missing_label = if self.missing_count > 0 {
                            format!("⚠  Missing ({})", self.missing_count)
                        } else {
                            "⚠  Missing".to_string()
                        };
                        if nav_button(
                            ui,
                            &missing_label,
                            self.view == LibraryView::Missing,
                            34.0,
                            14.0,
                        )
                        .on_hover_note(
                            "Tracks whose source file is gone. Relocate or remove them.",
                        )
                        .clicked()
                        {
                            self.view = LibraryView::Missing;
                        }
                        ui.add_space(8.0);
                    });

                // ── Playlist tree (middle, scrolls) ───────────────────────────
                egui::CentralPanel::default()
                    .frame(egui::Frame::none())
                    .show_inside(ui, |ui| {
                        egui::ScrollArea::vertical()
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                let all = self.playlists.clone();
                                draw_playlist_nodes(
                                    ui,
                                    &all,
                                    None,
                                    &mut self.view,
                                    &mut self.renaming,
                                    &mut sidebar_action,
                                );
                            });
                    });
            });
        match sidebar_action {
            Some(SidebarAction::NewPlaylist(parent)) => {
                if let Ok(cat) = Catalog::open(&self.db_path) {
                    if let Ok(id) = cat.create_playlist("New playlist", parent, false) {
                        self.view = LibraryView::Playlist(id);
                        // Start with an empty buffer (hint text shows the
                        // placeholder) so the user just types the real name and
                        // an empty name on blur means "discard this entry".
                        self.renaming = Some(Renaming {
                            id,
                            buf: String::new(),
                            is_new: true,
                            needs_focus: true,
                        });
                    }
                }
                self.reload();
            }
            Some(SidebarAction::Rename(id, name)) => {
                if let Ok(cat) = Catalog::open(&self.db_path) {
                    let _ = cat.rename_playlist(id, &name);
                }
                self.reload();
            }
            Some(SidebarAction::Delete(id)) => {
                if let Ok(cat) = Catalog::open(&self.db_path) {
                    let _ = cat.delete_playlist(id);
                }
                if self.view == LibraryView::Playlist(id) {
                    self.view = LibraryView::Library;
                }
                self.reload();
            }
            Some(SidebarAction::AddTracks(pid, ids)) => {
                if let Ok(cat) = Catalog::open(&self.db_path) {
                    match cat.add_tracks(pid, &ids) {
                        Ok(n) => self.status = format!("Added {n} track(s) to playlist."),
                        Err(e) => self.status = format!("error: {e}"),
                    }
                }
                self.reload();
            }
            None => {}
        }
        if self.view != prev_view {
            // Switching tabs resets the Recent pin: a fresh entry starts from the
            // live inbox (nothing pinned), and leaving drops the pin so finished
            // tracks expire. `reload` then re-pins whatever Recent shows.
            if prev_view == LibraryView::RecentlyAdded || self.view == LibraryView::RecentlyAdded {
                self.recent_pinned.clear();
            }
            self.reload();
        }
        // Kick off / adopt the off-thread duplicate scan after the view switch is
        // settled, so clicking the Duplicates tab starts the scan this same frame
        // (the view shows a spinner instead of a stale "no duplicates" flash).
        self.poll_duplicates(ctx);

        // Source files for a ⌥-drag started this frame inside the table (see
        // `draw_table`); the native drag-out is begun after the panel closes.
        let mut native_drag: Option<Vec<PathBuf>> = None;
        // The songs/content area sits a shade lighter than its default panel fill
        // so it reads as raised above the nav sidebar and the top/bottom bars
        // (which keep the darker `BG`). Otherwise this is the default central frame.
        let content_frame =
            egui::Frame::central_panel(&ctx.style()).fill(crate::ui::tokens::color::CONTENT_BG);
        egui::CentralPanel::default()
            .frame(content_frame)
            .show(ctx, |ui| {
                if self.view == LibraryView::Duplicates {
                    self.draw_duplicates(ui);
                } else if self.view == LibraryView::Missing {
                    self.draw_missing(ui);
                } else if self.view == LibraryView::Vinyl {
                    self.draw_vinyl(ui, ctx);
                } else if self.rows.is_empty()
                    && self.load_error.is_none()
                    && (!self.filter.trim().is_empty()
                        || self.col_filters.values().any(|v| !v.trim().is_empty()))
                {
                    // A filter — the global search or a per-column header filter —
                    // hid every row. The per-column filter UI lives in the table
                    // header, which isn't drawn when there are no rows, so without an
                    // escape hatch here the user is trapped: they can't reach a header
                    // to clear the filter, and the "catalog is empty" screen below
                    // would wrongly imply their library is gone. Offer a one-click
                    // clear of every active filter.
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            ui.heading("No tracks match the active filter");
                            ui.add_space(6.0);
                            ui.label("Clear the filter to see your full catalog again.");
                            ui.add_space(14.0);
                            if ui
                                .add(egui::Button::new(
                                    egui::RichText::new("  Clear filters  ").size(15.0),
                                ))
                                .clicked()
                            {
                                self.filter.clear();
                                self.col_filters.clear();
                                self.reload();
                            }
                        });
                    });
                } else if self.rows.is_empty() && self.load_error.is_none() {
                    let in_playlist = matches!(self.view, LibraryView::Playlist(_));
                    let is_recent = self.view == LibraryView::RecentlyAdded;
                    ui.centered_and_justified(|ui| {
                        ui.vertical_centered(|ui| {
                            if is_recent {
                                ui.heading("All caught up");
                                ui.add_space(6.0);
                                ui.label(
                                    "New imports show here until they're analyzed and \
                                 song-data fetched.",
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Add some songs, then analyze and fetch their data — \
                                     they'll appear here and clear themselves as you go.",
                                    )
                                    .weak(),
                                );
                            } else if in_playlist {
                                ui.heading("Empty playlist");
                                ui.add_space(6.0);
                                ui.label("Drag tracks here from “All songs” to add them.");
                                ui.label(
                                    "Hold ⌥ Option while dragging to drop straight into rekordbox.",
                                );
                            } else {
                                ui.heading("Your catalog is empty");
                                ui.add_space(6.0);
                                ui.label("Drag a folder of music anywhere onto this window,");
                                ui.label("or pick one to scan into your catalog.");
                                ui.add_space(4.0);
                                ui.label(
                                    egui::RichText::new(
                                        "Source files are never moved or modified.",
                                    )
                                    .weak(),
                                );
                                ui.add_space(14.0);
                                if ui
                                    .add(egui::Button::new(
                                        egui::RichText::new("  Add songs…  ").size(15.0),
                                    ))
                                    .clicked()
                                {
                                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                        self.spawn_scan(ctx.clone(), dir);
                                    }
                                }
                            }
                        });
                    });
                } else {
                    native_drag = self.draw_table(ui);
                }
            });

        // Native drag-out to rekordbox/Finder. A ⌥-drag begun in the table this
        // frame (`draw_table` returned its files) starts an `NSDraggingSession`
        // *now* — while the initiating mouse event is still live and the cursor is
        // inside the view, the only moment AppKit accepts it. The session then
        // tracks the drag itself all the way to the drop, with no dependence on
        // egui noticing the cursor leave the window (the old, race-prone trigger).
        // `begin_file_drag` blocks on AppKit's nested loop until the drop completes.
        // A plain (non-⌥) drag never reaches here: it carries an egui payload for
        // in-window reorder / drop onto a sidebar playlist instead.
        if let Some(paths) = native_drag {
            let refs: Vec<&Path> = paths.iter().map(PathBuf::as_path).collect();
            if !refs.is_empty() {
                macos_drag::begin_file_drag(frame, &refs);
            }
        }

        // Drop a folder / audio files from Finder anywhere on the window to import.
        self.handle_file_drop(ctx, frame);

        // Modal-style window — draw last so it floats on top.
        let mut open = self.convert_modal.is_some();
        let mut close_modal = false;
        let mut start_convert: Option<()> = None;
        let mut do_save_name = false;
        if let Some(modal) = self.convert_modal.as_mut() {
            egui::Window::new("Track actions")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center())
                .show(ctx, |ui| {
                    ui.set_min_width(500.0);
                    ui.label(egui::RichText::new(&modal.track_label).strong());
                    ui.label(
                        egui::RichText::new(modal.source_path.display().to_string())
                            .small()
                            .weak(),
                    );
                    ui.separator();

                    // --- Edit name (catalog tags) -------------------------------
                    ui.label(egui::RichText::new("Edit name").strong());
                    egui::Grid::new("name_grid")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Title:");
                            ui.add(
                                egui::TextEdit::singleline(&mut modal.edit_title)
                                    .desired_width(360.0),
                            );
                            ui.end_row();
                            ui.label("Artist:");
                            ui.add(
                                egui::TextEdit::singleline(&mut modal.edit_artist)
                                    .desired_width(360.0),
                            );
                            ui.end_row();
                            ui.label("Album:");
                            ui.add(
                                egui::TextEdit::singleline(&mut modal.edit_album)
                                    .desired_width(360.0),
                            );
                            ui.end_row();
                        });
                    ui.horizontal(|ui| {
                        if ui.button("Save name").clicked() {
                            do_save_name = true;
                        }
                        if let Some(msg) = &modal.name_status {
                            let color = if modal.name_is_error {
                                egui::Color32::LIGHT_RED
                            } else {
                                egui::Color32::LIGHT_GREEN
                            };
                            ui.colored_label(color, msg);
                        } else {
                            ui.label(
                                egui::RichText::new("Catalog-only — source file is not touched.")
                                    .small()
                                    .weak(),
                            );
                        }
                    });
                    ui.add_space(6.0);
                    ui.separator();

                    ui.label(egui::RichText::new("Convert").strong());

                    egui::Grid::new("convert_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Source format:");
                            ui.label(format_label(modal.source_format));
                            ui.end_row();

                            ui.label("Target format:");
                            egui::ComboBox::from_id_salt("target_format")
                                .selected_text(format_label(modal.target))
                                .show_ui(ui, |ui| {
                                    for &f in &[
                                        Format::Mp3,
                                        Format::Aac,
                                        Format::Flac,
                                        Format::Wav,
                                        Format::Aiff,
                                    ] {
                                        ui.selectable_value(&mut modal.target, f, format_label(f));
                                    }
                                });
                            ui.end_row();

                            ui.label("Bitrate (kbps):");
                            let lossy = matches!(modal.target, Format::Mp3 | Format::Aac);
                            ui.add_enabled(
                                lossy,
                                egui::TextEdit::singleline(&mut modal.bitrate_kbps)
                                    .hint_text(default_bitrate_hint(modal.target))
                                    .desired_width(80.0),
                            );
                            ui.end_row();

                            ui.label("Output folder:");
                            ui.horizontal(|ui| {
                                let text = match &modal.out_dir {
                                    Some(p) => p.display().to_string(),
                                    None => "(alongside source)".into(),
                                };
                                ui.label(egui::RichText::new(text).monospace());
                                if ui.small_button("Pick…").clicked() {
                                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                        modal.out_dir = Some(d);
                                    }
                                }
                                if modal.out_dir.is_some() && ui.small_button("Clear").clicked() {
                                    modal.out_dir = None;
                                }
                            });
                            ui.end_row();

                            ui.label("In-place:");
                            ui.checkbox(&mut modal.in_place, "Replace the source file");
                            ui.end_row();
                        });

                    if modal.in_place {
                        ui.colored_label(
                            egui::Color32::LIGHT_YELLOW,
                            "Warning: the original file will be removed and the catalog repointed.",
                        );
                    }

                    if let Some(err) = &modal.error {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }

                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let busy = self.job_rx.is_some();
                        if ui
                            .add_enabled(!busy, egui::Button::new("Convert"))
                            .clicked()
                        {
                            start_convert = Some(());
                        }
                        if ui.button("Cancel").clicked() {
                            close_modal = true;
                        }
                    });
                });
        }
        // Apply deferred modal actions to satisfy the borrow checker.
        if do_save_name {
            // Move the modal out to call save_name (which needs &mut self).
            if let Some(mut modal) = self.convert_modal.take() {
                self.save_name(&mut modal);
                let renamed_ok = !modal.name_is_error;
                self.convert_modal = Some(modal);
                if renamed_ok {
                    self.reload();
                    self.refresh_selected();
                }
            }
        }
        if start_convert.is_some() {
            let modal_clone = self.convert_modal.as_ref().map(|m| ConvertModal {
                track_id: m.track_id,
                track_label: m.track_label.clone(),
                source_path: m.source_path.clone(),
                source_format: m.source_format,
                edit_title: m.edit_title.clone(),
                edit_artist: m.edit_artist.clone(),
                edit_album: m.edit_album.clone(),
                name_status: None,
                name_is_error: false,
                target: m.target,
                bitrate_kbps: m.bitrate_kbps.clone(),
                out_dir: m.out_dir.clone(),
                in_place: m.in_place,
                error: None,
            });
            if let Some(m) = modal_clone {
                match self.spawn_convert(ctx.clone(), &m) {
                    Ok(()) => close_modal = true,
                    Err(e) => {
                        if let Some(cur) = self.convert_modal.as_mut() {
                            cur.error = Some(e);
                        }
                    }
                }
            }
        }
        if close_modal || !open {
            self.convert_modal = None;
        }

        self.draw_cover_drop(ctx);
        self.draw_batch_convert(ctx);
        self.draw_artwork_review(ctx);
        self.draw_settings(ctx);
        self.draw_clear_db_confirm(ctx);
        self.draw_bulk_write_confirm(ctx);
        self.draw_delete_confirm(ctx);
        self.draw_failure_report(ctx);

        // Keep the UI moving while a worker thread is active, or while there are
        // still fetched covers queued for the user to review.
        if self.is_busy() || !self.artwork_queue.is_empty() || self.artwork_saving {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }
}
