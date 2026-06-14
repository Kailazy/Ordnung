//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    pub(crate) fn new(db_path: PathBuf, egui_ctx: egui::Context) -> Self {
        // Swap egui's Latin-only default face for a broad-Unicode one before any
        // text is laid out, so accented / Cyrillic / Greek / symbol characters in
        // track metadata render instead of tofu boxes.
        install_fonts(&egui_ctx);
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
        spawn_vinyl_cover_loader(db_path.clone(), egui_ctx.clone(), vinyl_cover_req_rx, vinyl_cover_tx);
        // Resolves the now-playing cover to a temp file off-thread (see
        // `now_playing_cover_url`) so the OS Now Playing panel can show artwork
        // without blocking the UI on a catalog read when a track starts.
        let (media_cover_tx, media_cover_rx) = mpsc::channel::<(Id, Option<String>)>();
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
            show_inspector: false,
            job_cancel: None,
            artwork_queue: VecDeque::new(),
            artwork_enrich: false,
            artwork_overwrite: false,
            artwork_apply_album: true,
            artwork_album_overwrite: false,
            artwork_album_count: None,
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
            column_menu: None,
            col_filters: HashMap::new(),
            col_filter_open: None,
            settings_open: false,
            token_input: String::new(),
            confirm_clear_db: false,
            failure_report_title: String::new(),
            failure_report: Vec::new(),
            show_failure_report: false,
            audio: AudioEngine::new(egui_ctx),
            media_cover_tx,
            media_cover_rx,
            tag_edit: TagEdit::default(),
            tag_edit_saved: TagEdit::default(),
            edited_count: 0,
            missing_count: 0,
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
        };
        let config = Config::load();
        app.token_input = config.discogs_token.clone();
        app.config = config;
        app.load_column_layout();
        app.reload();
        app.recount_missing();
        app
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
            let still_valid = self
                .playlists
                .iter()
                .any(|p| p.id == id && !p.is_folder);
            if !still_valid {
                self.view = LibraryView::Library;
            }
        }
        match load_rows(&self.db_path, &self.filter, &self.view) {
            Ok(rows) => {
                // Narrow to the rows passing every active per-column filter before
                // computing the live set, so the selection/cover bookkeeping below
                // only ever references rows the user can actually see.
                let rows = self.apply_col_filters(rows);
                // Drop cover textures for tracks that are no longer in the
                // visible set; keep the ones still present (the texture id is
                // stable since track ids don't change).
                let live: std::collections::BTreeSet<Id> = rows.iter().map(|r| r.id).collect();
                self.cover_cache.retain(|id, _| live.contains(id));
                self.cover_full_cache.retain(|id, _| live.contains(id));
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
            }
            Err(e) => {
                self.rows.clear();
                self.cover_cache.clear();
                self.cover_full_cache.clear();
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

        // Attach any resolved now-playing cover to the OS panel, ignoring results
        // for a track the user has since moved on from.
        while let Ok((id, url)) = self.media_cover_rx.try_recv() {
            if let Some(a) = &mut self.audio {
                if a.current() == Some(id) {
                    a.set_now_playing_cover(url);
                }
            }
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
                            .on_hover_text("Pick one or more audio files to add")
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
                            .on_hover_text("Scan a whole folder of music (subfolders included)")
                            .clicked()
                        {
                            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                                self.spawn_scan(ctx.clone(), dir);
                            }
                            ui.close_menu();
                        }
                    });
                    add.response.on_hover_text(
                        "Add music to the catalog (your master library) — pick individual \
                         files or a whole folder. Source files are never moved or modified.",
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
                        .on_hover_text(
                            "Detect BPM, key, beatgrid, and transcode quality for these \
                             tracks (skips ones already analyzed at the current version)",
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
                            .on_hover_text("Re-run analysis even for tracks already analyzed")
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
                            .on_hover_text(
                                "Query Discogs and, for the release you pick, cache the \
                                 cover and fill in the track's missing fields (genre/style, \
                                 label, catalog #, year, country, album, date). Only fills \
                                 empty fields — never overwrites existing tags, and only \
                                 edits the catalog, not your files. Requires a Discogs \
                                 token (set it in Settings). For cover art alone, right-click \
                                 a track and choose “Fetch artwork”.",
                            )
                            .clicked()
                        {
                            self.spawn_fetch_artwork(ctx.clone(), true);
                            ui.close_menu();
                        }
                    });
                    more.response
                        .on_hover_text("More analysis & metadata actions");
                    // Batch convert: enabled whenever tracks are selected. Opens a
                    // dialog to pick one target format for all of them.
                    if !self.selection.is_empty() {
                        let n = self.selection.len();
                        if ui
                            .button(format!("Convert {n}…"))
                            .on_hover_text(
                                "Convert all selected tracks to one target format. \
                                 New files keep the full catalog metadata + cover.",
                            )
                            .clicked()
                        {
                            let ids: Vec<Id> = self
                                .rows
                                .iter()
                                .filter(|r| self.selection.contains(&r.id))
                                .map(|r| r.id)
                                .collect();
                            self.batch_convert = Some(BatchConvert {
                                ids,
                                target: Format::Mp3,
                                bitrate_kbps: String::new(),
                                out_dir: None,
                                in_place: true,
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
                        | LibraryView::Duplicates
                        | LibraryView::Missing
                        | LibraryView::Vinyl => None,
                    };
                    if let Some(pid) = playlist_view {
                        if !self.selection.is_empty() {
                            let n = self.selection.len();
                            if ui
                                .button(format!("Remove {n} from playlist"))
                                .on_hover_text(
                                    "Remove the selected track(s) from this playlist. \
                                     They stay in the catalog.",
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
                            .on_hover_text(
                                "Write the edited tags of every changed track into its \
                                 source file on disk, then mark them as synced.",
                            )
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
                                ui.strong(format!(
                                    "{count} track(s) point at a file that's gone"
                                ));
                                ui.separator();
                                // Cap the list so a huge backlog can't grow the
                                // tooltip off-screen; note the overflow instead.
                                const MAX: usize = 20;
                                for label in labels.iter().take(MAX) {
                                    ui.label(label);
                                }
                                if labels.len() > MAX {
                                    ui.add_space(2.0);
                                    ui.weak(format!("…and {} more", labels.len() - MAX));
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
                ui.label("Filter:");
                let resp = ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("artist / title / album / genre")
                        .desired_width(260.0),
                );
                if resp.changed() {
                    self.reload();
                }
                if !self.filter.is_empty() && ui.small_button("×").clicked() {
                    self.filter.clear();
                    self.reload();
                }
                // A prominent "clear all filters" button, shown only while a
                // filter is actually hiding rows. This rescues the case where a
                // forgotten column filter leaves the table looking empty.
                let active_col_filters = self
                    .col_filters
                    .values()
                    .filter(|v| !v.is_empty())
                    .count();
                if active_col_filters > 0 || !self.filter.is_empty() {
                    let label = if active_col_filters > 0 {
                        format!("⊘ Clear filters ({active_col_filters})")
                    } else {
                        "⊘ Clear filters".to_string()
                    };
                    if ui
                        .button(label)
                        .on_hover_text("Clear the search and all column filters")
                        .clicked()
                    {
                        self.filter.clear();
                        self.col_filters.clear();
                        self.reload();
                    }
                }
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
                // so the toolbar reads "do work … status & config". Added
                // right-to-left, so the visual order is counts · Refresh · Settings · Info.
                let busy = self.is_busy();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.toggle_value(&mut self.show_inspector, "Info")
                        .on_hover_text("Show/hide the track info panel");
                    ui.separator();
                    // Settings stays reachable even while a job runs.
                    if ui
                        .button("⚙ Settings")
                        .on_hover_text("Set your Discogs token (saved to ~/.ordnung/config.toml)")
                        .clicked()
                    {
                        self.token_input = self.config.discogs_token.clone();
                        self.settings_open = true;
                    }
                    if ui
                        .add_enabled(!busy, egui::Button::new("↻ Refresh"))
                        .on_hover_text("Reload the table from the catalog")
                        .clicked()
                    {
                        self.reload();
                        self.recount_missing();
                    }
                    ui.separator();
                    ui.label(counts);
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
                        .on_hover_text(
                            "Stop the running scan / artwork fetch after the current item.",
                        )
                        .clicked()
                {
                    do_abort = true;
                }
                if let Some(err) = &self.load_error {
                    ui.colored_label(egui::Color32::LIGHT_RED, format!("catalog error: {err}"));
                } else {
                    ui.label(if self.status.is_empty() {
                        format!("catalog: {}", self.db_path.display())
                    } else {
                        self.status.clone()
                    });
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
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.heading("Library");
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("+").on_hover_text("New playlist").clicked() {
                            sidebar_action = Some(SidebarAction::NewPlaylist(None));
                        }
                    });
                });
                ui.separator();
                if ui
                    .selectable_label(self.view == LibraryView::Library, "♪ All songs")
                    .on_hover_text("Every track in the catalog")
                    .clicked()
                {
                    self.view = LibraryView::Library;
                }
                if ui
                    .selectable_label(self.view == LibraryView::Duplicates, "⧉ Duplicates")
                    .on_hover_text("Find identical imports and same-song format variants")
                    .clicked()
                {
                    self.view = LibraryView::Duplicates;
                }
                let missing_label = if self.missing_count > 0 {
                    format!("⚠ Missing ({})", self.missing_count)
                } else {
                    "⚠ Missing".to_string()
                };
                if ui
                    .selectable_label(self.view == LibraryView::Missing, missing_label)
                    .on_hover_text("Tracks whose source file is no longer on disk — relocate or remove them")
                    .clicked()
                {
                    self.view = LibraryView::Missing;
                }
                ui.add_space(4.0);
                egui::ScrollArea::vertical().show(ui, |ui| {
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
                // "My Vinyl Collection" — the user's Discogs records, cached
                // locally. Pinned below the playlist tree so it reads as its own
                // section (physical records, separate from the digital catalog).
                ui.add_space(6.0);
                ui.separator();
                let vinyl_label = if self.vinyl_count > 0 {
                    format!("💿 My Vinyl Collection ({})", self.vinyl_count)
                } else {
                    "💿 My Vinyl Collection".to_string()
                };
                if ui
                    .selectable_label(self.view == LibraryView::Vinyl, vinyl_label)
                    .on_hover_text("Your Discogs vinyl collection — refresh to sync new records")
                    .clicked()
                {
                    self.view = LibraryView::Vinyl;
                }
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
            self.reload();
        }
        // Kick off / adopt the off-thread duplicate scan after the view switch is
        // settled, so clicking the Duplicates tab starts the scan this same frame
        // (the view shows a spinner instead of a stale "no duplicates" flash).
        self.poll_duplicates(ctx);

        // Source files for a ⌥-drag started this frame inside the table (see
        // `draw_table`); the native drag-out is begun after the panel closes.
        let mut native_drag: Option<Vec<PathBuf>> = None;
        egui::CentralPanel::default().show(ctx, |ui| {
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
                ui.centered_and_justified(|ui| {
                    ui.vertical_centered(|ui| {
                        if in_playlist {
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
                                egui::RichText::new("Source files are never moved or modified.")
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
        self.handle_file_drop(ctx);

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
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
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
                                egui::RichText::new(
                                    "Catalog-only — source file is not touched.",
                                )
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
                                        ui.selectable_value(
                                            &mut modal.target,
                                            f,
                                            format_label(f),
                                        );
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
