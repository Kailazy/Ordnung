//! Split out of `main.rs`; part of the GUI `App`.
use super::*;
use crate::ui::tokens::space;

/// How long the search box waits for typing to stop before rebuilding the rows
/// (see `App::filter_apply_at`). Short enough to feel immediate — comfortably
/// under the ~200 ms gap that reads as a pause — while collapsing the keystrokes
/// within a typed word into a single reload.
const SEARCH_DEBOUNCE: Duration = Duration::from_millis(150);

impl App {
    pub(crate) fn new(db_path: PathBuf, egui_ctx: egui::Context) -> Self {
        // Install the Inter font stack and push the design tokens into egui's
        // global style before any text is laid out, so every stock widget already
        // matches the Ordnung visual language (see `ui::theme`). DejaVu Sans stays
        // in the fallback chain for the wide-Unicode glyphs Inter lacks.
        crate::ui::theme::install(&egui_ctx);
        // Dig cover downloads: many small CDN fetches, each answering on this
        // one channel (the dig is a single path, so there's no per-request
        // routing to do).
        let (dig_cover_tx, dig_cover_rx) = mpsc::channel();
        let (dig_prime_tx, dig_prime_rx) = mpsc::channel();
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
        // A second long-lived loader for vinyl cover art (collection + wantlist),
        // keyed by list and that list's row id.
        let (vinyl_cover_req_tx, vinyl_cover_req_rx) = mpsc::channel::<VinylCoverKey>();
        let (vinyl_cover_tx, vinyl_cover_rx) = mpsc::channel();
        spawn_vinyl_cover_loader(
            db_path.clone(),
            egui_ctx.clone(),
            vinyl_cover_req_rx,
            vinyl_cover_tx,
        );
        // A third loader serving the search popup's vinyl rows. Its own channel
        // (and cache) because the popup shows records from outside the Vinyl
        // view, where `vinyl_covers` is cleared on every reload.
        let (search_cover_req_tx, search_cover_req_rx) = mpsc::channel::<VinylCoverKey>();
        let (search_cover_tx, search_cover_rx) = mpsc::channel();
        let (record_tx, record_rx) = mpsc::channel();
        spawn_vinyl_cover_loader(
            db_path.clone(),
            egui_ctx.clone(),
            search_cover_req_rx,
            search_cover_tx,
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
            filter_apply_at: None,
            search_hits: Vec::new(),
            search_query: String::new(),
            search_apply_at: None,
            search_popup_open: false,
            search_row_shown_at: None,
            focus_search: false,
            search_cursor: None,
            search_vinyl_covers: HashMap::new(),
            search_cover_req_tx,
            search_cover_rx,
            search_scope: SearchScope::default(),
            record_search: RecordSearch::Idle,
            record_generation: 0,
            record_cache: HashMap::new(),
            record_tx,
            record_rx,
            record_apply_at: None,
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
            wantlist: Vec::new(),
            vinyl_count: 0,
            vinyl_tab: VinylList::Collection,
            vinyl_filter: String::new(),
            vinyl_covers: HashMap::new(),
            vinyl_cover_req_tx,
            vinyl_cover_rx,
            vinyl_links: HashMap::new(),
            track_releases: HashMap::new(),
            vinyl_owned: HashSet::new(),
            vinyl_wanted: HashSet::new(),
            vinyl_owned_tracks: HashSet::new(),
            vinyl_wanted_tracks: HashSet::new(),
            confirm_vinyl_edit: None,
            vinyl_sheet: None,
            sheet_follows_dig: false,
            sheet_rx: None,
            sheet_price_rx: None,
            versions: None,
            versions_rx: None,
            egui_ctx: egui_ctx.clone(),
            dig: None,
            dig_seed: 0x853C_49E6_748F_EA9B,
            dig_rx: None,
            dig_prime_tx,
            dig_prime_rx,
            dig_ids_rx: None,
            dig_covers: HashMap::new(),
            dig_cover_tx,
            dig_cover_rx,
            dig_start_keys: HashMap::new(),
            scroll_to_track: None,
            row_screen_rects: Vec::new(),
            cover_drop: None,
            tags_editing: false,
            job_cancel: None,
            artwork_queue: VecDeque::new(),
            artwork_enrich: false,
            wantlist_after_fetch: Vec::new(),
            wantlist_after_fetch_label: String::new(),
            pending_wantlist_releases: Vec::new(),
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
            tex_graveyard: TexGraveyard::default(),
            inspector_open: true,
            menu_installed: false,
            tour: None,
            settings_open: false,
            settings_tab: SettingsTab::default(),
            token_input: String::new(),
            discogs_auth: DiscogsAuth::default(),
            discogs_auth_rx: None,
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
            auto_write_stalled_at: None,
            auto_write_job: false,
            auto_write_pending_latch: false,
            playlists: Vec::new(),
            dup_groups: Vec::new(),
            dup_dirty: false,
            dup_loading: false,
            dup_rx: None,
            dup_decisions: HashMap::new(),
            dup_pending_bulk: None,
            dup_confirm_pos: None,
            missing_list: Vec::new(),
            health_tab: LibraryView::Duplicates,
            missing_pending_remove: None,
            usb_volumes: ordnung_core::usb::detect_volumes(),
            usb_last_poll: 0.0,
            usb_tracks: Vec::new(),
            usb_loaded_for: None,
            usb_playlists: Vec::new(),
            usb_playlist_tracks: HashMap::new(),
            usb_loading: false,
            usb_rx: None,
            usb_eject_rx: None,
            usb_selected: None,
            usb_edit: UsbEdit::default(),
            usb_edit_saved: UsbEdit::default(),
            nav_density: NavDensity::Narrow,
            nav_drag: None,
            view: LibraryView::Library,
            renaming: None,
            sort: None,
            now_playing: None,
            player_native_drag: None,
            scrub: None,
            volume_dirty: false,
            wave_zoom_secs: crate::player::DEFAULT_ZOOM_SECS,
            wave_lane_h: crate::player::DEFAULT_LANE_H,
            grid_edit_open: false,
            grid_nudge_held: false,
            update_rx: None,
            update_available: None,
        };
        let config = Config::load();
        app.token_input = config.discogs_token.clone();
        app.config = config;
        // The engine is built before the config is read, so hand it the saved
        // level now — otherwise the first track plays at unity while the knob
        // shows whatever the user left it at.
        if let Some(a) = &mut app.audio {
            a.set_volume(app.config.volume);
        }
        // Same for the video player. It has no panel yet — that's built on the
        // first video — but the level is recorded now so the first one to play
        // comes in at the saved volume rather than at full.
        webview::set_volume(app.config.volume);
        // A token on disk isn't proof it still works, so start at "unverified"
        // and let the Discogs tab verify on open rather than checking every
        // launch — that would spend a rate-limited request nobody asked for.
        if !app.discogs_token().trim().is_empty() {
            app.discogs_auth = DiscogsAuth::Unverified;
        }
        // Open on whichever section the user set as their home. A vinyl-first
        // collector lands on the shelf instead of the digital catalog.
        app.view = match StartupView::from_key(&app.config.startup_view) {
            StartupView::Library => LibraryView::Library,
            StartupView::Vinyl => LibraryView::Vinyl,
            StartupView::Recent => LibraryView::RecentlyAdded,
        };
        // Restore the sidebar to the width tier it was left at.
        app.nav_density = NavDensity::from_key(&app.config.nav_density);
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
        // Last, so the tour draws over a fully built window rather than an
        // empty one: a new user should see what they're being told about.
        app.maybe_open_tour();
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

    /// Verify the saved token against `GET /oauth/identity` on a worker thread.
    /// This is the whole point of the Discogs tab: it answers "am I actually
    /// signed in?" up front instead of letting a bad token surface as a failed
    /// artwork fetch hours later. A confirmed username is persisted to config,
    /// which also saves the separate lookup the vinyl collection used to need.
    pub(crate) fn spawn_discogs_identity_check(&mut self, ctx: egui::Context) {
        let token = self.discogs_token().trim().to_string();
        if token.is_empty() {
            self.discogs_auth = DiscogsAuth::SignedOut;
            self.discogs_auth_rx = None;
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.discogs_auth_rx = Some(rx);
        self.discogs_auth = DiscogsAuth::Checking;
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let outcome = match client.identity() {
                Ok(username) => DiscogsAuth::Connected { username },
                Err(e) => {
                    let msg = e.to_string();
                    // `map_ureq_err` folds every HTTP status into Error::Network,
                    // so the status code has to be read back out of the message
                    // to tell "token is bad" from "network is down" — the two
                    // need very different wording in the UI.
                    if msg.contains("HTTP 401") || msg.contains("HTTP 403") {
                        DiscogsAuth::Rejected
                    } else {
                        DiscogsAuth::Unreachable { detail: msg }
                    }
                }
            };
            let _ = tx.send(outcome);
            ctx.request_repaint();
        });
    }

    /// Drain the identity check's result. Persists a confirmed username so the
    /// collection views can address the user's Discogs account across launches.
    pub(crate) fn poll_discogs_identity(&mut self) {
        let Some(rx) = &self.discogs_auth_rx else {
            return;
        };
        match rx.try_recv() {
            Ok(auth) => {
                if let DiscogsAuth::Connected { username } = &auth {
                    if &self.config.discogs_username != username {
                        self.config.discogs_username = username.clone();
                        let _ = self.config.save();
                    }
                }
                self.discogs_auth = auth;
                self.discogs_auth_rx = None;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => {
                // Worker died without sending; don't strand the UI on a spinner.
                self.discogs_auth = DiscogsAuth::Unreachable {
                    detail: "the check didn't finish".into(),
                };
                self.discogs_auth_rx = None;
            }
        }
    }

    pub(crate) fn reload(&mut self) {
        // Rows are about to be rebuilt from the catalog, so any waveform bytes a
        // (re)analysis rewrote are now stale in the smoothing cache — and its key
        // can't see a change that kept the envelope's length. Reload is the one
        // choke point every catalog write funnels through, so drop it here.
        crate::player::clear_smooth_cache();
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
        // USB views build rows from the scanned device (no catalog involved);
        // everything else queries the catalog. Both feed the same
        // post-processing below so filters/selection/covers behave uniformly.
        let loaded = if matches!(self.view, LibraryView::Usb(..)) {
            Ok(self.usb_rows())
        } else {
            load_rows(&self.db_path, &self.filter, &self.view, &self.recent_pinned)
        };
        match loaded {
            Ok(rows) => {
                // Narrow to the rows passing every active per-column filter before
                // computing the live set, so the selection/cover bookkeeping below
                // only ever references rows the user can actually see.
                let rows = self.apply_col_filters(rows);
                // Evict cover textures for tracks that are no longer in the
                // visible set; keep the ones still present (the texture id is
                // stable since track ids don't change). Safe mid-frame: `Tex`
                // defers the actual GPU frees to the next frame (see `tex.rs`).
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
            .and_then(|c| c.vinyl_count(VinylList::Collection))
            .unwrap_or(0);
        // track → Discogs release, so the library's right-click menu knows which
        // tracks have a release worth wantlisting. Loaded in every view (unlike
        // the grid's reverse map below) because that menu is the library's.
        self.track_releases = Catalog::open(&self.db_path)
            .and_then(|c| c.release_track_links())
            .map(|pairs| pairs.into_iter().map(|(rid, tid)| (tid, rid)).collect())
            .unwrap_or_default();
        // …and which records are already yours, so the menu can say where one
        // already is instead of offering to want it again. Two views of the same
        // membership: by release (the grid's both-lists check) and by track (the
        // library's, which needs the metadata fallback since most tracks carry
        // no Discogs release id).
        for (list, releases, tracks) in [
            (
                VinylList::Collection,
                &mut self.vinyl_owned,
                &mut self.vinyl_owned_tracks,
            ),
            (
                VinylList::Wantlist,
                &mut self.vinyl_wanted,
                &mut self.vinyl_wanted_tracks,
            ),
        ] {
            *releases = Catalog::open(&self.db_path)
                .and_then(|c| c.vinyl_release_ids(list))
                .map(|ids| ids.into_iter().collect())
                .unwrap_or_default();
            *tracks = Catalog::open(&self.db_path)
                .and_then(|c| c.vinyl_tracks_in(list))
                .map(|ids| ids.into_iter().collect())
                .unwrap_or_default();
        }
        if self.view == LibraryView::Vinyl {
            self.vinyl = Catalog::open(&self.db_path)
                .and_then(|c| c.list_vinyl(VinylList::Collection))
                .unwrap_or_default();
            self.wantlist = Catalog::open(&self.db_path)
                .and_then(|c| c.list_vinyl(VinylList::Wantlist))
                .unwrap_or_default();
            // Evict cover textures for records no longer present (`Tex` makes
            // the mid-frame eviction safe — see `tex.rs`).
            let live: HashSet<VinylCoverKey> = self
                .vinyl
                .iter()
                .map(|v| (VinylList::Collection, v.instance_id))
                .chain(
                    self.wantlist
                        .iter()
                        .map(|v| (VinylList::Wantlist, v.instance_id)),
                )
                .collect();
            self.vinyl_covers.retain(|key, _| live.contains(key));
            // Cross-reference the catalog: which records do we already own a
            // digital copy of? Build release_id → [track_id] once for the grid.
            // Exact release-id links first, metadata matching as a fallback.
            // Both lists are cross-referenced: a wanted record you already have
            // digitally is worth flagging too.
            let records: Vec<VinylRecord> = self
                .vinyl
                .iter()
                .chain(self.wantlist.iter())
                .cloned()
                .collect();
            self.vinyl_links = Catalog::open(&self.db_path)
                .and_then(|c| c.vinyl_catalog_links(&records))
                .map(|pairs| {
                    let mut m: HashMap<u64, Vec<Id>> = HashMap::new();
                    for (rid, tid) in pairs {
                        m.entry(rid).or_default().push(tid);
                    }
                    m
                })
                .unwrap_or_default();
        } else if !self.vinyl.is_empty() || !self.wantlist.is_empty() {
            self.vinyl = Vec::new();
            self.wantlist = Vec::new();
            // Runs mid-frame when the grid's "in catalog" badge jumps to the
            // Library after painting these covers; safe because `Tex` defers
            // the frees to the next frame (see `tex.rs`).
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

    /// Keep the sidebar's device list live and the USB view's track list
    /// loaded. Volumes are re-detected every couple of seconds (a cheap
    /// `/Volumes` readdir); the per-volume track scan walks the whole device
    /// reading tags, so it runs on a worker thread like the duplicate scan.
    /// Pulling the viewed stick falls the view back to the Library.
    fn poll_usb(&mut self, ctx: &egui::Context) {
        let now = ctx.input(|i| i.time);
        if now - self.usb_last_poll >= 2.0 || self.usb_last_poll == 0.0 {
            self.usb_last_poll = now;
            self.usb_volumes = ordnung_core::usb::detect_volumes();
        }
        // Keep a frame scheduled ~2s out even when the app is idle, so a
        // plugged-in stick appears without the user having to wiggle the mouse
        // to force a frame. Re-armed unconditionally every frame (egui
        // coalesces these): if it were only re-armed when a poll runs, the
        // scheduled frame could land a hair before the 2s threshold, skip the
        // poll, and the wake-up chain would die until the next input event.
        ctx.request_repaint_after(std::time::Duration::from_secs(2));
        // Surface a finished eject's outcome, and re-detect right away so a
        // successful eject drops the volume from the sidebar this frame.
        if let Some(rx) = &self.usb_eject_rx {
            if let Ok(msg) = rx.try_recv() {
                self.status = msg;
                self.usb_eject_rx = None;
                self.usb_volumes = ordnung_core::usb::detect_volumes();
            }
        }
        // Adopt a finished scan; results are tagged with their volume so a
        // stale scan (user already switched sticks) can't fill the wrong view.
        if let Some(rx) = &self.usb_rx {
            if let Ok(scan) = rx.try_recv() {
                self.usb_loading = false;
                self.usb_rx = None;
                if self.usb_loaded_for.as_deref() == Some(scan.vol.as_path()) {
                    self.usb_tracks = scan.tracks;
                    self.usb_playlists = scan.playlists;
                    self.usb_playlist_tracks = scan.playlist_tracks;
                    // Build the table rows for whatever USB view is showing.
                    self.reload();
                }
            }
        }
        let LibraryView::Usb(vol, _) = &self.view else {
            // Free the device's track list once the view moves elsewhere.
            if !self.usb_tracks.is_empty() {
                self.usb_tracks = Vec::new();
            }
            self.usb_playlists = Vec::new();
            self.usb_playlist_tracks = HashMap::new();
            self.usb_loaded_for = None;
            self.usb_selected = None;
            return;
        };
        let vol = vol.clone();
        if !self.usb_volumes.iter().any(|v| v.path == vol) {
            self.view = LibraryView::Library;
            self.reload();
            return;
        }
        if self.usb_loading || self.usb_loaded_for.as_deref() == Some(vol.as_path()) {
            return;
        }
        self.usb_loaded_for = Some(vol.clone());
        self.usb_tracks = Vec::new();
        self.usb_playlists = Vec::new();
        self.usb_playlist_tracks = HashMap::new();
        self.usb_selected = None;
        self.usb_loading = true;
        let (tx, rx) = std::sync::mpsc::channel();
        self.usb_rx = Some(rx);
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let _ = tx.send(scan_usb_volume(vol));
            ctx.request_repaint();
        });
    }

    /// Build table rows for the active USB view straight from the scanned
    /// device tracks (synthetic ids — see [`usb_track_id`]). Mirrors
    /// `load_rows` field-for-field, but analysis-derived columns (waveform,
    /// quality, Added) stay empty: these files aren't in the catalog. BPM and
    /// key come from the files' own tags — on a rekordbox export those carry
    /// what rekordbox analyzed.
    pub(crate) fn usb_rows(&self) -> Vec<TrackRow> {
        let LibraryView::Usb(_, playlist) = &self.view else {
            return Vec::new();
        };
        let indices: Vec<usize> = match playlist {
            Some(pid) => self
                .usb_playlist_tracks
                .get(pid)
                .cloned()
                .unwrap_or_default(),
            None => (0..self.usb_tracks.len()).collect(),
        };
        let filter = self.filter.trim().to_lowercase();
        indices
            .into_iter()
            .filter_map(|i| {
                let t = self.usb_tracks.get(i)?;
                let artist = t.tags.artist.clone().unwrap_or_default();
                let title = t.tags.title.clone().unwrap_or_default();
                let album = t.tags.album.clone().unwrap_or_default();
                let genre = t.tags.genre.clone().unwrap_or_default();
                if !filter.is_empty() {
                    let hay = format!(
                        "{artist} {title} {album} {genre} {}",
                        t.source_path.to_lowercase()
                    )
                    .to_lowercase();
                    if !hay.contains(&filter) {
                        return None;
                    }
                }
                let key = t.tags.initial_key_tag.clone().unwrap_or_else(|| "—".into());
                let camelot = parse_camelot_label(&key);
                let bpm_val = t.tags.bpm_tag;
                Some(TrackRow {
                    id: usb_track_id(i),
                    artist,
                    title,
                    album,
                    genre,
                    duration: fmt_duration(t.properties.duration_ms),
                    bpm: bpm_val
                        .map(|b| format!("{b:.2}"))
                        .unwrap_or_else(|| "—".into()),
                    key,
                    format: t.format,
                    format_label: format_label(t.format).into(),
                    bitrate: t
                        .properties
                        .bitrate_kbps
                        .map(|b| b.to_string())
                        .unwrap_or_else(|| "—".into()),
                    notes: t.tags.comment.clone().unwrap_or_default(),
                    added: "—".into(),
                    added_at: 0,
                    waveform: Vec::new(),
                    waveform_bands: Vec::new(),
                    source_path: PathBuf::from(&t.source_path),
                    // The scan already extracted the file's embedded art into
                    // `cover_thumb`; the cover cell decodes it straight from
                    // there (see `load_usb_thumb`) instead of the catalog.
                    has_cover: t.cover_thumb.is_some(),
                    has_external_cover: false,
                    dur_ms: Some(t.properties.duration_ms),
                    bpm_val,
                    bitrate_val: t.properties.bitrate_kbps,
                    key_sort: camelot.map(|c| u16::from(c.number) * 2 + u16::from(c.major)),
                    camelot,
                    quality: None,
                    quality_cut_hz: None,
                    quality_src: None,
                    quality_sort: None,
                })
            })
            .collect()
    }

    /// Select every row currently visible (`self.rows` is already narrowed to
    /// the active view and column filters), promoting the first row to primary
    /// when nothing was focused. Shared by ⌘A and the Edit ▸ Select All item.
    pub(crate) fn select_all_visible(&mut self) {
        self.selection = self.rows.iter().map(|r| r.id).collect();
        if self.selected.is_none() {
            if let Some(first) = self.rows.first().map(|r| r.id) {
                self.set_primary(Some(first));
            }
        }
    }

    /// Play/pause whatever is currently sounding. Shared by the space bar and
    /// the Playback menu.
    ///
    /// A record's video answers first while one is loaded: it's the sound the
    /// user is hearing, and its own window is parked off screen, so this is the
    /// only way to reach it without the pointer. Without that branch the
    /// request would fall through and start an unrelated local track *over* the
    /// video.
    pub(crate) fn toggle_play_pause(&mut self) {
        if webview::is_open() {
            webview::toggle_pause();
        } else if self.now_playing.is_some() {
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
}

/// Parse a Camelot label from a file's key tag ("8A", "12b") so the Key column
/// gets its coloured chip. Other notations (classical "Am", open key) simply
/// render as plain text. `None` for anything that isn't `<1-12><A|B>`.
fn parse_camelot_label(s: &str) -> Option<Camelot> {
    let s = s.trim();
    let letter = s.chars().last()?;
    let major = match letter {
        'B' | 'b' => true,
        'A' | 'a' => false,
        _ => return None,
    };
    let number: u8 = s[..s.len() - 1].parse().ok()?;
    (1..=12)
        .contains(&number)
        .then_some(Camelot { number, major })
}

impl eframe::App for App {
    // TEMP DEBUG: inject a synthetic hover at ORDNUNG_CURSOR_PROBE="x,y".
    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(path) = std::env::var_os("ORDNUNG_CURSOR_PROBE") {
            let v = std::fs::read_to_string(path).unwrap_or_default();
            if let Some((x, y)) = v.trim().split_once(',') {
                if let (Ok(x), Ok(y)) = (x.trim().parse::<f32>(), y.trim().parse::<f32>()) {
                    raw_input
                        .events
                        .push(egui::Event::PointerMoved(egui::pos2(x, y)));
                }
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        // Install the native menu bar on the first frame. It has to wait for a
        // frame rather than happen before `run_native`: the `NSApplication`
        // whose main menu we replace doesn't exist until eframe/winit has
        // created it, and AppKit would otherwise install its own stub over ours.
        if !self.menu_installed {
            self.menu_installed = true;
            crate::macos_menu::install();
        }
        // Drop cover textures evicted during the PREVIOUS frame. Doing it here —
        // before anything paints or uploads — guarantees the frame that painted
        // them has already been submitted to the GPU (see `tex_graveyard`).
        self.tex_graveyard.clear();
        if self.poll_worker() {
            self.reload();
            self.refresh_selected();
            self.recount_missing();
            // `reload` has just refreshed `edited_count`. If an automatic write
            // ran and tracks are still pending, those files can't be written —
            // latch the count so auto-write doesn't spin on them every frame.
            if self.auto_write_pending_latch {
                self.auto_write_pending_latch = false;
                self.auto_write_stalled_at = (self.edited_count > 0).then_some(self.edited_count);
            }
        }
        // Auto-write: with the setting on, edits that landed outside the
        // inspector (Discogs enrichment, bulk fetches) still leave tracks
        // pending a file write. Flush them as soon as the worker channel is
        // free — one job at a time, and never on top of a running job, since
        // `job_rx` is shared. `edited_count` is refreshed by `reload`, so this
        // sees a fresh number and settles back to zero once the write lands.
        if self.config.auto_write_tags
            && self.edited_count > 0
            && !self.is_busy()
            && self.auto_write_stalled_at != Some(self.edited_count)
        {
            self.auto_write_job = true;
            self.spawn_write_edits(ctx.clone());
        }
        self.poll_covers(ctx);
        self.poll_thumbs(ctx);
        self.poll_vinyl_covers(ctx);
        self.poll_search_covers(ctx);
        self.poll_artwork_save(ctx);
        // Settle a pending "wantlist it, matching first" request. This is the
        // path that actually fires it in the normal case: the flush inside
        // `poll_artwork_save` runs while the fetch job still owns the shared job
        // channel, so it defers, and this retries once `poll_worker` has drained
        // `Done` and freed it. It also covers a fetch that never queued a picker
        // at all (no candidates, or cancelled). No-op when nothing is pending.
        if !self.is_busy() && !self.artwork_saving && self.artwork_queue.is_empty() {
            self.flush_wantlist_after_fetch(ctx);
        } else if !self.wantlist_after_fetch.is_empty() {
            // Still blocked (the fetch's `Done` hasn't drained, or auto-write
            // took the channel first). Keep the frames coming so the retry above
            // gets its chance without waiting on the next mouse move.
            ctx.request_repaint();
        }
        self.poll_discogs_identity();
        self.poll_metadata_preview();
        self.poll_vinyl_sheet();
        self.poll_sheet_price();
        self.poll_versions();
        self.poll_dig();
        self.poll_records();
        self.poll_dig_primed();
        self.poll_dig_ids();
        self.poll_dig_covers(ctx);
        self.drive_video_player(ctx);

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
            self.select_all_visible();
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

        // Cmd/Ctrl+F puts the caret in the toolbar search box — the standard
        // "Find" chord, and the fastest way into the one control that answers
        // "where is this?" for both halves of the library.
        //
        // Unlike ⌘A this is *not* gated on `wants_keyboard_input`: ⌘F has no
        // in-field meaning to protect, and pressing it while some other field
        // has focus should still take you to the search box. The flag is read
        // by the toolbar a few lines later, once the field exists.
        //
        // On macOS the Edit ▸ Find menu item owns this chord and gets here via
        // `MenuCommand::FocusSearch` instead; the branch is kept for platforms
        // with no native menu bar.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F)) {
            self.focus_search = true;
        }

        // Cmd/Ctrl+R reloads the table from the catalog. This used to be a
        // toolbar button, but it's a rare manual escape hatch (jobs reload on
        // their own), so it lives as a shortcut instead of taking up chrome.
        // Ignored while a job runs, which is when the old button was disabled.
        //
        // On macOS the Library ▸ Reload menu item owns this chord and gets there
        // first, so this branch only fires off-macOS. Kept rather than gated so
        // the shortcut still exists on platforms with no native menu bar.
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::R))
            && !self.is_busy()
        {
            self.reload();
            self.recount_missing();
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
            self.toggle_play_pause();
        }

        // Menu-bar commands. Each arm runs the same code the in-app shortcut
        // runs, so the menu is a second door onto one implementation rather
        // than a parallel one. Drained once per frame; see `macos_menu`.
        if let Some(cmd) = macos_menu::take_command() {
            match cmd {
                macos_menu::MenuCommand::Reload => {
                    if !self.is_busy() {
                        self.reload();
                        self.recount_missing();
                    }
                }
                macos_menu::MenuCommand::Settings => {
                    self.token_input = self.config.discogs_token.clone();
                    self.settings_open = true;
                }
                macos_menu::MenuCommand::SelectAll => self.select_all_visible(),
                macos_menu::MenuCommand::ClearFilters => self.clear_all_filters(),
                // Re-injected as the OS copy event the table already listens
                // for, so the menu and ⌘C land in the identical handler (which
                // also decides between selection and primary row).
                macos_menu::MenuCommand::Copy => {
                    ctx.input_mut(|i| i.events.push(egui::Event::Copy));
                }
                macos_menu::MenuCommand::AddFolder => {
                    if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                        self.spawn_scan(ctx.clone(), dir);
                    }
                }
                macos_menu::MenuCommand::PlayPause => self.toggle_play_pause(),
                macos_menu::MenuCommand::FocusSearch => self.focus_search = true,
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

        // Apply a parked search edit once typing has settled. Repainting on the
        // deadline is what makes it fire while the user sits still — without it the
        // rows would wait for whatever incidental event repainted next.
        if let Some(at) = self.filter_apply_at {
            let now = std::time::Instant::now();
            if now >= at {
                self.filter_apply_at = None;
                self.reload();
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        // The search box's own debounce. Separate from the table filter's above:
        // this one only rebuilds the suggestion list, so it never touches the
        // rows on screen.
        if let Some(at) = self.search_apply_at {
            let now = std::time::Instant::now();
            if now >= at {
                self.search_apply_at = None;
                self.refresh_search_hits();
            } else {
                ctx.request_repaint_after(at - now);
            }
        }
        // The Discogs lookup's own, much longer debounce. Separate from the
        // local one above because this one spends a rate-limited network
        // request, so it waits for a real pause in typing rather than a gap
        // between keystrokes.
        if let Some(at) = self.record_apply_at {
            let now = std::time::Instant::now();
            if now >= at {
                self.record_apply_at = None;
                self.start_record_search();
            } else {
                ctx.request_repaint_after(at - now);
            }
        }

        // Attach the off-thread hi-res zoom envelope to the now-playing track,
        // dropping results for a track the user has since moved on from.
        while let Ok((id, hires)) = self.hires_rx.try_recv() {
            if let Some(n) = self.now_playing.as_mut() {
                if n.id == id {
                    // A recompute triggered by new crossover frequencies (Settings →
                    // Frequency bands, or a loaded preset) yields a *different*
                    // envelope of the *same* length for the same track — which the
                    // smoothing cache's key can't distinguish from the one it already
                    // holds. Drop it here, where every hi-res result lands, rather
                    // than at each of the settings paths that clears `hires_bands`.
                    crate::player::clear_smooth_cache();
                    n.hires_bands = Some(Arc::new(hires));
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
                            egui::RichText::new(format!(
                                "(you have {})",
                                env!("CARGO_PKG_VERSION")
                            ))
                            .color(egui::Color32::from_rgb(220, 230, 245)),
                        );
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icon::close_button(ui, "Dismiss until the next launch") {
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

        // Vertical padding is stated once, in the frame, rather than as
        // `add_space` before and after the row. Trailing `item_spacing.y` is
        // appended *after* a widget but never before one, so a leading and
        // trailing spacer of the same size do not produce equal margins — the
        // bottom gap silently inherits an extra `item_spacing.y`. A symmetric
        // frame margin has no such asymmetry, and puts the button row on the
        // 8-pt grid with equal optical weight above and below.
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(space::S3, space::S3)),
            )
            .show(ctx, |ui| {
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
                            "Add files or a folder to the catalog. Source files are never modified",
                        );
                        ui.separator();
                        // When rows are selected, the toolbar buttons act on just that
                        // selection (in visible order); otherwise they fall back to the
                        // whole filtered view. The label reflects which, so a user who
                        // picked a few tracks isn't surprised by a full-library run.
                        // USB rows have synthetic non-catalog ids, so a device-view
                        // selection is ignored here — the buttons keep their
                        // whole-catalog fallback meaning instead of no-op'ing.
                        let usb_view = matches!(self.view, LibraryView::Usb(..));
                        let sel_ids: Vec<Id> = if usb_view {
                            Vec::new()
                        } else {
                            self.rows
                                .iter()
                                .filter(|x| self.selection.contains(&x.id))
                                .map(|x| x.id)
                                .collect()
                        };
                        // Analysis: one button. Force re-analyze and bulk Discogs
                        // fetches were dropped — the per-track ↻ re-pick covers the
                        // metadata case, and re-analysis is rarely wanted in bulk.
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
                        // Batch convert: enabled whenever tracks are selected. Opens a
                        // dialog to pick one target format for all of them.
                        if !self.selection.is_empty() && !usb_view {
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
                            | LibraryView::Vinyl
                            | LibraryView::Usb(..) => None,
                        };
                        if let Some(pid) = playlist_view {
                            if !self.selection.is_empty() {
                                let n = self.selection.len();
                                if ui
                                    .button(format!("Remove {n} from playlist"))
                                    .on_hover_note(
                                        "Remove from this playlist. Tracks stay in the catalog",
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
                    // The vinyl view has no table rows, so it counts records instead
                    // — filtered by its own search box, the way the grid is.
                    let counts = if self.view == LibraryView::Vinyl {
                        let query = self.vinyl_filter.trim().to_lowercase();
                        let n = |recs: &[VinylRecord]| {
                            if query.is_empty() {
                                recs.len()
                            } else {
                                recs.iter()
                                    .filter(|v| crate::views::vinyl_matches(v, &query))
                                    .count()
                            }
                        };
                        format!(
                            "{} in collection · {} in wantlist",
                            n(&self.vinyl),
                            n(&self.wantlist)
                        )
                    } else {
                        let mut counts = format!("{} tracks", self.rows.len());
                        if !self.selection.is_empty() {
                            counts.push_str(&format!(" · {} selected", self.selection.len()));
                        }
                        if self.missing_count > 0 {
                            counts.push_str(&format!(" · {} missing", self.missing_count));
                        }
                        counts
                    };
                    // Right-aligned utility group: counts and Settings live away from
                    // the left-edge library actions so the toolbar reads "do work …
                    // status & config". Laying this out right-to-left FIRST reserves
                    // its width, so the left-aligned filter group nested inside
                    // shrinks to fit instead of overdrawing the counts when the
                    // window is narrow. Visual order on the right: counts · Settings.
                    // Reloading the table is a Cmd/Ctrl+R shortcut rather than a
                    // button: it's rarely needed, since every job reloads on finish.
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // Master volume sits at the very top right, the corner
                        // rekordbox puts its master level in — a global control
                        // that isn't tied to whatever the current view is, so it
                        // stays out of the per-track player bar at the bottom.
                        // The engine is `None` on a machine with no audio output;
                        // showing a dead knob there would be a control that lies.
                        if self.audio.is_some() {
                            if let Some(v) = crate::ui::knob::volume(ui, self.config.volume) {
                                self.config.volume = v;
                                if let Some(a) = &mut self.audio {
                                    a.set_volume(v);
                                }
                                // The knob is the master level, so it rules the
                                // video player too — a record's YouTube tracks
                                // and your own files are the same record, and
                                // one of them ignoring the knob reads as broken.
                                webview::set_volume(v);
                                // Persist on release rather than every drag frame,
                                // so a gesture writes the file once.
                                self.volume_dirty = true;
                            }
                            if self.volume_dirty && !ui.ctx().input(|i| i.pointer.any_down()) {
                                self.volume_dirty = false;
                                let _ = self.config.save();
                            }
                            ui.separator();
                        }
                        // Settings stays reachable even while a job runs.
                        if ui
                            .button("⚙ Settings")
                            .on_hover_note("Discogs token and app options")
                            .clicked()
                        {
                            self.token_input = self.config.discogs_token.clone();
                            self.settings_open = true;
                        }
                        ui.separator();
                        ui.label(counts);
                        ui.separator();
                        // The filter group fills whatever horizontal space the utility
                        // group left over. Rendered left-to-right inside the reserved
                        // remainder so it can never collide with the counts.
                        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                            // Reserve room for the Clear-filters button so the text
                            // field shrinks rather than pushing it past the edge of
                            // this remainder.
                            // Room for the Clear-filters button *and* the
                            // scope toggle, so the field shrinks rather than
                            // pushing either past the edge of this remainder.
                            let reserved = (if has_filters { 140.0 } else { 0.0 }) + SCOPE_TOGGLE_W;
                            let w = (ui.available_width() - reserved).clamp(120.0, 320.0);
                            // egui's TextEdit defaults to a 4×2 inner margin, which
                            // leaves the caret and hint text jammed against the
                            // frame. Give the field real breathing room and a
                            // comfortable hit height — it's the most-used control in
                            // the toolbar, so it earns the space.
                            // Read before the field borrows `search_query`
                            // mutably, so the hint can depend on the scope.
                            let hint = if self.searching_discogs() {
                                "Search all of Discogs"
                            } else {
                                "Search songs and records"
                            };
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut self.search_query)
                                    .desired_width(w)
                                    .margin(egui::Margin::symmetric(space::S3, space::S2 + 1.0))
                                    .min_size(egui::vec2(0.0, 26.0))
                                    .hint_text(hint),
                            );
                            // ⌘F (or Edit ▸ Find) asked for this field. Focus
                            // it and select whatever's already typed, so the
                            // chord means "search for something else" as well
                            // as "search" — the same as every other Find box.
                            if std::mem::take(&mut self.focus_search) {
                                resp.request_focus();
                                let chars = self.search_query.chars().count();
                                if let Some(mut state) =
                                    egui::TextEdit::load_state(ui.ctx(), resp.id)
                                {
                                    state.cursor.set_char_range(Some(
                                        egui::text::CCursorRange::two(
                                            egui::text::CCursor::new(0),
                                            egui::text::CCursor::new(chars),
                                        ),
                                    ));
                                    state.store(ui.ctx(), resp.id);
                                }
                                // Bring back the suggestions the box last
                                // showed, rather than an empty popup over a
                                // query that's still there.
                                if !self.search_hits.is_empty() {
                                    self.search_popup_open = true;
                                }
                            }
                            if resp.changed() {
                                // Typing only recomputes the dropdown — the table is
                                // left alone until a hit is chosen. Parked behind the
                                // same debounce so a fast typist isn't re-querying
                                // the catalog on every keystroke.
                                self.search_apply_at =
                                    Some(std::time::Instant::now() + SEARCH_DEBOUNCE);
                                if self.searching_discogs() {
                                    // The remote lookup rides its own, longer
                                    // debounce; until it fires the popup keeps
                                    // showing the last answer rather than
                                    // blanking on every keystroke.
                                    self.record_apply_at =
                                        Some(std::time::Instant::now() + RECORD_DEBOUNCE);
                                }
                            }
                            // Re-opening on focus lets a user who dismissed the
                            // popup get it back by clicking into the box, without
                            // retyping.
                            if resp.gained_focus() && !self.search_hits.is_empty() {
                                self.search_popup_open = true;
                            }
                            // The scope switch sits immediately right of the
                            // field, reading as part of the same control: it
                            // says what the box you just typed in will search.
                            self.draw_scope_toggle(ui);
                            self.draw_search_popup(&resp);
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
                                    self.clear_all_filters();
                                }
                            }
                        });
                    });
                });
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
                        .button("✖ Cancel")
                        .on_hover_note("Stop after the items already running")
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

        // The inspector is the single surface for per-track detail and tag
        // editing (the convert dialog no longer duplicates name editing), so it
        // is always present rather than behind a toolbar toggle.
        let mut inspector_action: Option<InspectorAction> = None;
        // A fixed-width drawer, not a splitter: the inspector is either open at
        // its designed width or fully out of the way, the way Spotify's right
        // sidebar behaves. Dragging an edge to some in-between width only ever
        // produced a cramped, half-truncated panel, so that affordance is gone
        // and the pull tab below is the single way to show/hide it.
        const INSPECTOR_W: f32 = 360.0;
        // Animate the drawer's width instead of snapping it: egui eases this
        // 0→1 over the given duration and repaints until it settles, so the
        // panel slides out and the table reflows with it. Short enough to feel
        // like a direct response to the click rather than a transition.
        // The inspector describes one selected track, which the vinyl view has
        // no concept of — it's a grid of Discogs releases, not catalog rows. So
        // the drawer (and its tab) are digital-only: force it shut there rather
        // than leaving an empty panel with nothing to inspect.
        let inspector_applies = self.view != LibraryView::Vinyl;
        let t = ctx.animate_bool_with_time(
            egui::Id::new("inspector_slide"),
            self.inspector_open && inspector_applies,
            0.12,
        );
        let width = INSPECTOR_W * t;
        if width > 0.5 {
            egui::SidePanel::right("inspector")
                .resizable(false)
                .exact_width(width)
                // Same fill as the pull tab (see `color::DRAWER`), so the
                // handle reads as the edge of this panel rather than a lighter
                // shape stuck to a darker one.
                // egui's own divider is off (`show_separator_line`); the edge is
                // instead a hairline drawn as the frame's left stroke, which is
                // what keeps the drawer from bleeding into the table now that
                // both are dark. A stroke on `Frame` insets the content by its
                // width on every side, so it is applied as a painted line below
                // rather than here.
                .show_separator_line(false)
                .frame(
                    egui::Frame::side_top_panel(&ctx.style())
                        .fill(crate::ui::tokens::color::DRAWER)
                        .stroke(egui::Stroke::NONE)
                        // Content inset. The panel had none, so captions and
                        // values ran flush into the panel edge; a utility panel
                        // needs a consistent gutter on both sides for its
                        // label/value columns to read as a column at all.
                        .inner_margin(egui::Margin {
                            left: crate::ui::tokens::space::S5,
                            right: crate::ui::tokens::space::S4,
                            top: 0.0,
                            bottom: 0.0,
                        }),
                )
                .show(ctx, |ui| {
                    // The hairline between drawer and table, painted at the
                    // panel's own left edge so the inner margin doesn't push it
                    // inward the way a `Frame` stroke would.
                    let edge = ui.max_rect().left() - crate::ui::tokens::space::S5;
                    ui.painter().vline(
                        edge,
                        ctx.screen_rect().y_range(),
                        egui::Stroke::new(1.0, crate::ui::tokens::color::SEPARATOR_OPAQUE),
                    );
                    // The inspector's text is laid out at its natural width, and
                    // a label wider than the panel would otherwise push the
                    // panel's width back out and paint over the table. Clip to
                    // the frame so the panel's width is the panel's alone.
                    ui.set_clip_rect(ui.max_rect().intersect(ui.clip_rect()));
                    ui.set_max_width(ui.available_width());
                    inspector_action = self.draw_inspector(ui, ctx);
                });
        }
        // Pull tab: a slim half-rounded handle pinned to the inner edge of the
        // drawer, vertically centred over whatever is beside it. It rides along
        // with the panel so the same control both opens and closes it, and the
        // chevron always points the direction the panel will move.
        if inspector_applies {
            self.draw_inspector_tab(ctx, width);
        }
        match inspector_action {
            Some(InspectorAction::EmbedCover(id, path)) => self.embed_cover_into_file(id, path),
            Some(InspectorAction::SaveToCatalog(id)) => self.save_tags(id, None),
            Some(InspectorAction::WriteToFile(id, path)) => self.save_tags(id, Some(path)),
            None => {}
        }

        // Left sidebar: "Library" (the whole catalog) plus the playlist/folder
        // tree. Plain-field state (`view`, `renaming`) is edited in place; catalog
        // mutations are raised as a `SidebarAction` and applied after the panel so
        // nothing borrows `self` while the tree renders. A view change triggers a
        // reload so the table follows the sidebar.
        let prev_view = self.view.clone();
        let mut sidebar_action: Option<SidebarAction> = None;
        // The sidebar snaps between three designed layouts (see `NavDensity`)
        // rather than resizing freely, and the layout it shows is *frozen* for
        // the whole of a drag. Committing the tier live meant the labels
        // rewrapped under the pointer on the way past every boundary — the
        // sidebar flickering through layouts you were only travelling over, not
        // choosing. So the edge drag no longer moves the panel at all: a ghost
        // line follows the pointer, and the tier it implies is applied once, on
        // release. One layout change per drag, at the moment you commit to it.
        let drag_id = egui::Id::new("library_nav").with("__resize");
        if ctx.is_being_dragged(drag_id) {
            if let Some(pos) = ctx.pointer_interact_pos() {
                let w = pos.x - ctx.screen_rect().left();
                // Hysteresis is applied against the tier in force (see
                // `NavDensity::dragged_to`), which is the tier the panel is
                // still showing — so the ghost snaps to the same tier the drop
                // will pick, and never previews a landing the release refuses.
                self.nav_drag = Some(crate::NavDrag {
                    x: pos.x,
                    target: self.nav_density.dragged_to(w),
                });
                // The panel is frozen for the duration, so nothing else is
                // asking for frames — without this the ghost would only advance
                // when some other part of the UI happened to repaint, and the
                // line would visibly lag the cursor.
                ctx.request_repaint();
            }
        } else if let Some(drag) = self.nav_drag.take() {
            // Released: this is the only place the tier changes.
            if drag.target != self.nav_density {
                self.nav_density = drag.target;
                self.config.nav_density = drag.target.key().to_string();
                let _ = self.config.save();
            }
        }
        // Naming a playlist needs a text field, and the rail has no room for
        // one — a new playlist created there would open an editor you cannot
        // read or type into. So a rename temporarily promotes the sidebar out
        // of the rail; it drops back the moment the edit resolves. The stored
        // tier is untouched, so this borrows the width rather than changing the
        // user's choice.
        let density = if self.renaming.is_some() && self.nav_density.icons_only() {
            NavDensity::Narrow
        } else {
            self.nav_density
        };
        let target = density.width();
        // Ease between tiers so the change of layout reads as a deliberate
        // lock-into-place rather than a hard cut. `animate_value` repaints until
        // it settles; the width it produces is only ever *travelling between*
        // two tiers, never a width the user can hold it at.
        let settled = ctx.animate_value_with_time(egui::Id::new("nav_snap"), target, 0.13);
        egui::SidePanel::left("library_nav")
            .resizable(true)
            .default_width(target)
            // Pinned to the snapped width at all times — a collapsed range is
            // what stops egui's own resize from writing an arbitrary width back
            // into the panel. The drag above is the only thing that changes it.
            .width_range(settled..=settled)
            .show(ctx, |ui| {
                // Header for a section: a small dimmed all-caps caption that sets
                // the playlist / collection groups apart without competing with
                // the big nav tiles below it.
                let section_caption = |ui: &mut egui::Ui, text: &str| {
                    // An all-caps caption has no legible short form, so the
                    // icon tier drops it entirely and lets the spacing and
                    // rules do the grouping.
                    if density.icons_only() {
                        return;
                    }
                    ui.label(
                        egui::RichText::new(text)
                            .font(crate::ui::tokens::font::footnote())
                            .color(egui::Color32::from_gray(140))
                            .strong(),
                    );
                };

                // Which library leads the sidebar. A vinyl-first collector puts
                // the shelf on top and the digital catalog underneath it; the
                // default keeps the digital catalog as the home base.
                let nav_primary = NavPrimary::from_key(&self.config.nav_primary);
                // Copied out so the section closures below don't borrow `self`
                // while the panel is drawing (`view` is threaded in explicitly).
                let recent_count = self.recent_count;
                // Whether the Recent tab currently has anything on screen —
                // pinned rows included. Guards the empty-inbox eviction below.
                let rows_empty = self.rows.is_empty();
                // Library health only earns sidebar space when something is
                // actually wrong; the tab under "Library" appears with the
                // first missing file and vanishes once the catalog is clean.
                let missing_count = self.missing_count;

                // The digital-library group: the "Library" / "New" tile pair
                // and the PLAYLISTS header (the tree itself scrolls in the middle
                // panel below). Drawn wherever `nav_primary` puts it, so the same
                // code serves both the top slot and the vinyl-first layout.
                let draw_digital_group =
                    |ui: &mut egui::Ui,
                     view: &mut LibraryView,
                     sidebar_action: &mut Option<SidebarAction>| {
                        // "Library" is the home base — the big tile — and
                        // fresh imports live *inside* it: a small "New" pill on
                        // the tile's right edge, present only while something is
                        // actually waiting on analysis or a Discogs fetch. They
                        // are a subset of the catalog rather than a sibling
                        // library, so an empty inbox leaves no tile behind and
                        // the sidebar's top row stays a single clear target.
                        const RECENT_NOTE: &str = "New imports awaiting analysis or a \
                                                   Discogs fetch. They drop off once both \
                                                   are done.";
                        // Icon tier has no room for a pill beside the glyph, so
                        // the inbox keeps its own stacked tile there.
                        let inline_badge = recent_count > 0 && density != NavDensity::Icon;
                        // The inbox has no permanent tile any more, so an empty
                        // one must not leave the user parked on a view they
                        // can't navigate back to. Only an inbox that is empty
                        // *on screen* ejects, though: while tracks finish under
                        // the pin, `recent_count` is already zero and the rows
                        // are deliberately still there (see `recent_pinned`),
                        // so eviction waits until nothing is left to look at.
                        if recent_count == 0 && *view == LibraryView::RecentlyAdded && rows_empty {
                            *view = LibraryView::Library;
                        }
                        // One size at every remaining tier: the taller 46pt
                        // variant existed for the retired wide layout, where the
                        // tile shared its row with "New".
                        let tile = nav_button_painted(
                            ui,
                            density,
                            crate::ui::icon::library,
                            "Library",
                            *view == LibraryView::Library,
                            44.0,
                            16.5,
                        );
                        let mut tile_clicked = tile.clicked();
                        if inline_badge {
                            let badge = crate::sidebar::nav_tile_badge(
                                ui,
                                tile.rect,
                                &format!("✦ {recent_count}"),
                                *view == LibraryView::RecentlyAdded,
                            )
                            .on_hover_note(RECENT_NOTE);
                            if badge.clicked() {
                                *view = LibraryView::RecentlyAdded;
                                // The tile underneath reports the same click, so
                                // swallow it or the catalog would win the race.
                                tile_clicked = false;
                            }
                        } else {
                            tile.on_hover_note("Every track in the catalog");
                        }
                        if tile_clicked {
                            *view = LibraryView::Library;
                        }
                        // At the icon tier the pill has nowhere to go, so the
                        // inbox falls back to its own glyph tile below.
                        if recent_count > 0 && density == NavDensity::Icon {
                            ui.add_space(4.0);
                            if nav_button_dense(
                                ui,
                                density,
                                "✦",
                                &format!("New  {recent_count}"),
                                *view == LibraryView::RecentlyAdded,
                                36.0,
                                14.0,
                            )
                            .on_hover_note(RECENT_NOTE)
                            .clicked()
                            {
                                *view = LibraryView::RecentlyAdded;
                            }
                        }
                        // A slim strip under the tile pair, shown only when the
                        // catalog has something wrong with it: files the catalog
                        // points at that are no longer on disk. Clicking it opens
                        // the Library Health window (Duplicates / Missing tabs).
                        // A clean catalog gets no row at all.
                        if missing_count > 0 {
                            ui.add_space(4.0);
                            if nav_button_dense(
                                ui,
                                density,
                                "⚠",
                                &format!("{missing_count} missing"),
                                *view == LibraryView::Duplicates || *view == LibraryView::Missing,
                                24.0,
                                12.0,
                            )
                            .on_hover_note("Library health: missing files and duplicate copies")
                            .clicked()
                            {
                                *sidebar_action = Some(SidebarAction::OpenHealth);
                            }
                        }
                        ui.add_space(10.0);
                        if density.icons_only() {
                            // In the rail the caption is gone, so right-aligning
                            // the "+" left it floating in an empty row with
                            // nothing to align against. It becomes a rail tile
                            // like every other target instead — same square, same
                            // column, so it reads as "add to this list" rather
                            // than as a stray button.
                            if rail_tile(ui, "+", false)
                                .on_hover_text("New playlist")
                                .clicked()
                            {
                                *sidebar_action = Some(SidebarAction::NewPlaylist(None));
                            }
                            ui.add_space(6.0);
                        } else {
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
                                            *sidebar_action =
                                                Some(SidebarAction::NewPlaylist(None));
                                        }
                                    },
                                );
                            });
                            ui.add_space(4.0);
                        }
                    };

                // The vinyl tile. Like the digital group, it's drawn from one
                // place into whichever slot `nav_primary` assigns it — big and
                // leading when vinyl is primary, a compact pinned row otherwise.
                let draw_vinyl_tile = |ui: &mut egui::Ui, view: &mut LibraryView, lead: bool| {
                    // The vinyl shelf is a top-level library whether or not it
                    // leads the sidebar, so at the icon tier it keeps the taller
                    // tile that earns the bigger glyph; only the captioned tiers
                    // shrink it when it sits below the digital group.
                    let (h, size) = match (lead, density) {
                        (true, _) => (46.0, 17.0),
                        (false, NavDensity::Icon) => (40.0, 14.0),
                        (false, _) => (34.0, 14.0),
                    };
                    if nav_button_dense(
                        ui,
                        density,
                        "💿",
                        // Just "Vinyl", at every tier and with no count. The
                        // shelf is a place you go, not a number you track, and
                        // dropping the count also retires the longest string
                        // the sidebar had to fit.
                        "Vinyl",
                        *view == LibraryView::Vinyl,
                        h,
                        size,
                    )
                    .on_hover_note("Your Discogs vinyl collection")
                    .clicked()
                    {
                        *view = LibraryView::Vinyl;
                    }
                };

                // ── Leading section (top) ─────────────────────────────────────
                // Whichever library the user leads with sits here: the digital
                // catalog (default) or the vinyl shelf. When vinyl leads, the
                // digital group follows under its own caption so the playlist
                // tree below it still reads as belonging to the catalog.
                egui::TopBottomPanel::top("nav_library")
                    .frame(egui::Frame::none())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(8.0);
                        match nav_primary {
                            NavPrimary::Digital => {
                                draw_digital_group(ui, &mut self.view, &mut sidebar_action);
                            }
                            NavPrimary::Vinyl => {
                                draw_vinyl_tile(ui, &mut self.view, true);
                                ui.add_space(10.0);
                                ui.separator();
                                ui.add_space(8.0);
                                section_caption(ui, "DIGITAL LIBRARY");
                                ui.add_space(4.0);
                                draw_digital_group(ui, &mut self.view, &mut sidebar_action);
                            }
                        }
                    });

                // ── Pinned bottom views (no captions) ─────────────────────────
                // External sources — mounted USB devices and the Discogs vinyl
                // collection — separated from the playlist tree by spacing and a
                // rule rather than a text header. Library health isn't here: it
                // only surfaces as a small tab under "Library", and only when
                // the catalog actually has something wrong with it.
                egui::TopBottomPanel::bottom("nav_collections")
                    .frame(egui::Frame::none())
                    .show_separator_line(false)
                    .show_inside(ui, |ui| {
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(6.0);
                        // ── Devices ──
                        // Mounted removable volumes, rekordbox-style: plug a
                        // stick in and it appears here, pull it and it's gone
                        // (kept live by `poll_usb`). Hidden when nothing is
                        // mounted so the sidebar doesn't carry an empty header.
                        if !self.usb_volumes.is_empty() {
                            section_caption(ui, "DEVICES");
                            ui.add_space(4.0);
                            for v in self.usb_volumes.clone() {
                                // The "(rekordbox)" suffix only ever fit the
                                // retired wide tier; the volume's own name is
                                // what identifies it, and the hover note still
                                // says whether it holds a rekordbox export.
                                let label = v.name.clone();
                                if nav_button_dense(
                                    ui,
                                    density,
                                    "⏏",
                                    &label,
                                    self.view == LibraryView::Usb(v.path.clone(), None),
                                    34.0,
                                    14.0,
                                )
                                .on_hover_note(if v.is_rekordbox_export {
                                    "Removable volume with a rekordbox export. \
                                     Browse and edit its files directly."
                                } else {
                                    "Removable volume. Browse and edit its files directly."
                                })
                                .clicked()
                                {
                                    self.view = LibraryView::Usb(v.path.clone(), None);
                                }
                                // The active device's rekordbox playlist tree,
                                // indented under its tile like the catalog's
                                // own playlist tree. Only the scanned device
                                // has this data (the pdb is read by its scan).
                                if self.usb_loaded_for.as_deref() == Some(v.path.as_path())
                                    && !self.usb_playlists.is_empty()
                                {
                                    ui.indent(("usb-pl-tree", &v.path), |ui| {
                                        draw_usb_playlist_nodes(
                                            ui,
                                            density,
                                            &self.usb_playlists.clone(),
                                            &self.usb_playlist_tracks,
                                            0,
                                            &v.path,
                                            &mut self.view,
                                        );
                                    });
                                }
                                ui.add_space(3.0);
                            }
                            ui.add_space(3.0);
                            ui.separator();
                            ui.add_space(6.0);
                        }
                        // ── Sources ──
                        // Only when the digital library leads; if vinyl is the
                        // primary library its tile lives at the top instead.
                        if nav_primary == NavPrimary::Digital {
                            draw_vinyl_tile(ui, &mut self.view, false);
                            ui.add_space(6.0);
                            ui.separator();
                            ui.add_space(6.0);
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
                                    density,
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
            Some(SidebarAction::OpenHealth) => {
                let tab = self.health_tab.clone();
                self.open_health_tab(tab, ctx);
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
            // A record sheet belongs to the Vinyl section. Leaving that section
            // closes it rather than leaving it stranded over the Library — and,
            // as everywhere else the sheet closes, takes its video with it so
            // nothing keeps playing with no sheet on screen to explain it.
            if prev_view == LibraryView::Vinyl && self.vinyl_sheet.is_some() {
                self.stop_sheet_video();
                self.vinyl_sheet = None;
            }
            self.reload();
        }
        // Kick off / adopt the off-thread duplicate scan after the view switch is
        // settled, so clicking the Duplicates tab starts the scan this same frame
        // (the view shows a spinner instead of a stale "no duplicates" flash).
        self.poll_duplicates(ctx);
        // Same deal for the USB device list and the viewed volume's track scan.
        self.poll_usb(ctx);

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
                } else if let LibraryView::Usb(vol, _) = self.view.clone() {
                    native_drag = self.draw_usb(ui, &vol);
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
                                    egui::RichText::new("  Clear filters  ")
                                        .font(crate::ui::tokens::font::headline()),
                                ))
                                .clicked()
                            {
                                self.clear_all_filters();
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
                                ui.label("Drag tracks here from “Library” to add them.");
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
                                        egui::RichText::new("  Add songs…  ")
                                            .font(crate::ui::tokens::font::headline()),
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

        // The resize ghost. While the edge is held the panel itself does not
        // move, so this line is the entire feedback for the drag: it tracks the
        // pointer freely, and a wider marker sits at the tier the drop would
        // land on. Painted in a foreground layer after the panel so it reads on
        // top of the sidebar's own content rather than being clipped by it.
        // The panel is pinned to a single width (`settled..=settled`), so egui
        // reads it as already at its minimum and offers a one-way "resize east"
        // cursor — implying the sidebar can only be widened. It snaps both ways,
        // so say so, on hover as well as mid-drag.
        if self.nav_drag.is_some()
            || ctx.is_pointer_over_area() && {
                let r = ctx.read_response(drag_id);
                r.map(|r| r.hovered()).unwrap_or(false)
            }
        {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if let Some(drag) = self.nav_drag {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("nav_resize_ghost"),
            ));
            // Where the panel would settle if released now. Drawn solid, in the
            // nav accent, so the eye reads the landing rather than the pointer.
            let snap_x = screen.left() + drag.target.width();
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(snap_x - 1.5, screen.top()),
                    egui::pos2(snap_x + 1.5, screen.bottom()),
                ),
                egui::Rounding::ZERO,
                crate::sidebar::NAV_ACCENT,
            );
            // The pointer's own position, dimmer and hairline: it explains why
            // the snap marker sits where it does while the two are apart, and
            // is redundant (so unobtrusive) once the drag settles onto a tier.
            if (drag.x - snap_x).abs() > 2.0 {
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(drag.x - 0.5, screen.top()),
                        egui::pos2(drag.x + 0.5, screen.bottom()),
                    ),
                    egui::Rounding::ZERO,
                    egui::Color32::from_white_alpha(60),
                );
            }
        }

        // Native drag-out to rekordbox/Finder. A ⌥-drag begun in the table this
        // frame (`draw_table` returned its files) starts an `NSDraggingSession`
        // *now* — while the initiating mouse event is still live and the cursor is
        // inside the view, the only moment AppKit accepts it. The session then
        // tracks the drag itself all the way to the drop, with no dependence on
        // egui noticing the cursor leave the window (the old, race-prone trigger).
        // `begin_file_drag` blocks on AppKit's nested loop until the drop completes.
        // A plain (non-⌥) drag never reaches here: it carries an egui payload for
        // in-window reorder / drop onto a sidebar playlist instead.
        // The now-playing bar's artwork drag, taken from where `draw_player`
        // parked it earlier this frame. Same dispatch point as the table's, so
        // AppKit's nested loop is entered once, with no borrows outstanding.
        let from_player = self.player_native_drag.take().map(|p| vec![p]);
        let native_drag = native_drag.or(from_player);
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
        if let Some(modal) = self.convert_modal.as_mut() {
            egui::Window::new("Convert track")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center())
                .show(ctx, |ui| {
                    // Fixed, narrow width: without a cap the long source path
                    // used to dictate how wide the dialog opened.
                    ui.set_width(400.0);
                    // Which track this acts on. Name/tag editing lives in the
                    // inspector (right panel) — this dialog is transcode only,
                    // so the header is identification, not another edit surface.
                    ui.label(egui::RichText::new(&modal.track_label).strong());
                    // File name only, full path on hover: the path is long
                    // enough to set the dialog's whole width on its own.
                    let src = modal.source_path.clone();
                    let name = src
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_else(|| src.display().to_string());
                    ui.label(egui::RichText::new(name).small().weak())
                        .on_hover_note(src.display().to_string());
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);

                    egui::Grid::new("convert_grid")
                        .num_columns(2)
                        .spacing([12.0, 7.0])
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new("Source").weak().small());
                            ui.label(format_label(modal.source_format));
                            ui.end_row();

                            ui.label(egui::RichText::new("Target").weak().small());
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

                            ui.label(egui::RichText::new("Bitrate").weak().small());
                            let lossy = matches!(modal.target, Format::Mp3 | Format::Aac);
                            ui.horizontal(|ui| {
                                ui.add_enabled(
                                    lossy,
                                    egui::TextEdit::singleline(&mut modal.bitrate_kbps)
                                        .hint_text(default_bitrate_hint(modal.target))
                                        .desired_width(70.0),
                                );
                                ui.label(
                                    egui::RichText::new(if lossy { "kbps" } else { "lossless" })
                                        .weak()
                                        .small(),
                                );
                            });
                            ui.end_row();

                            ui.label(egui::RichText::new("Output").weak().small());
                            ui.horizontal(|ui| {
                                let text = match &modal.out_dir {
                                    Some(p) => p.display().to_string(),
                                    None => "(alongside source)".into(),
                                };
                                ui.label(egui::RichText::new(text).monospace().small());
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

                            ui.label(egui::RichText::new("In place").weak().small());
                            ui.checkbox(&mut modal.in_place, "Replace the source file");
                            ui.end_row();
                        });

                    if modal.in_place {
                        ui.add_space(2.0);
                        ui.colored_label(
                            egui::Color32::LIGHT_YELLOW,
                            "⚠ Original file removed, catalog repointed.",
                        );
                    }

                    if let Some(err) = &modal.error {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(6.0);
                    // Primary action right-aligned, Cancel beside it — the
                    // standard dialog footer shape. The row is allocated at an
                    // explicit height: a bare right-to-left layout claimed all
                    // the window's remaining height, leaving a tall dead gap
                    // above the buttons.
                    let footer = egui::Layout::right_to_left(egui::Align::Center);
                    ui.allocate_ui_with_layout(
                        egui::vec2(ui.available_width(), 26.0),
                        footer,
                        |ui| {
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
                        },
                    );
                });
        }
        // Apply deferred modal actions to satisfy the borrow checker.
        if start_convert.is_some() {
            let modal_clone = self.convert_modal.as_ref().map(|m| ConvertModal {
                track_id: m.track_id,
                track_label: m.track_label.clone(),
                source_path: m.source_path.clone(),
                source_format: m.source_format,
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
        self.draw_vinyl_edit_confirm(ctx);
        self.draw_vinyl_sheet(ctx, frame);
        self.draw_versions(ctx);
        self.draw_failure_report(ctx);
        // Drawn last so the welcome tour sits above every other window on a
        // first launch.
        self.draw_tour(ctx);

        // Keep the UI moving while a worker thread is active, or while there are
        // still fetched covers queued for the user to review.
        if self.is_busy() || !self.artwork_queue.is_empty() || self.artwork_saving {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }

        // TEMP DEBUG: trace the cursor icon egui asks for, frame by frame.
        if std::env::var_os("ORDNUNG_CURSOR_DEBUG").is_some() {
            let icon = ctx.output(|o| o.cursor_icon);
            let pos = ctx.input(|i| i.pointer.latest_pos());
            let screen = ctx.screen_rect();
            let over = ctx.is_pointer_over_area();
            let id = ctx.input(|i| i.pointer.interact_pos());
            eprintln!(
                "cursor: {icon:?} at {pos:?} interact={id:?} over_area={over} screen={screen:?}"
            );
            ctx.request_repaint();
        }
    }
}

/// The USB device scan, run on a worker thread: find every audio file on the
/// volume and read its tags, then — when the volume carries a rekordbox
/// export — read the export's playlist tree and resolve each playlist entry's
/// pdb path to a scanned track. FAT32 is case-insensitive, so paths match on
/// the lowercased volume-relative form; entries whose file wasn't found (or
/// failed to scan) drop out of the playlist rather than showing as dead rows.
pub(crate) fn scan_usb_volume(vol: PathBuf) -> UsbScan {
    let files = scan::discover(&vol);
    // Tag reads are per-file and independent; rayon keeps a big stick from
    // taking minutes. Unreadable files are skipped, not fatal.
    let mut tracks: Vec<ScannedTrack> = files
        .par_iter()
        .filter_map(|p| scan::scan_file(p).ok())
        .collect();
    tracks.sort_by(|a, b| a.source_path.cmp(&b.source_path));

    let mut playlists = Vec::new();
    let mut playlist_tracks: HashMap<u32, Vec<usize>> = HashMap::new();
    let pdb = vol.join("PIONEER").join("rekordbox").join("export.pdb");
    if pdb.is_file() {
        if let Ok(export) = ordnung_rbdb::pdb::read_export(&pdb) {
            let by_rel_path: HashMap<String, usize> = tracks
                .iter()
                .enumerate()
                .filter_map(|(i, t)| {
                    let rel = Path::new(&t.source_path)
                        .strip_prefix(&vol)
                        .ok()?
                        .to_string_lossy()
                        .to_lowercase();
                    Some((rel, i))
                })
                .collect();
            for (playlist, track_ids) in &export.entries {
                let indices: Vec<usize> = track_ids
                    .iter()
                    .filter_map(|tid| {
                        let rel = export
                            .track_paths
                            .get(tid)?
                            .trim_start_matches('/')
                            .to_lowercase();
                        by_rel_path.get(&rel).copied()
                    })
                    .collect();
                playlist_tracks.insert(*playlist, indices);
            }
            // Playlists with no resolvable tracks still show (empty), so the
            // tree mirrors what the player would list.
            for p in &export.playlists {
                if !p.is_folder {
                    playlist_tracks.entry(p.id).or_default();
                }
            }
            playlists = export.playlists;
        }
    }
    UsbScan {
        vol,
        tracks,
        playlists,
        playlist_tracks,
    }
}

#[cfg(test)]
mod usb_scan_tests {
    use super::*;

    /// End-to-end device scan against a synthetic rekordbox stick: the real
    /// `num_rows` export.pdb fixture (104 playlists / 3886 tracks) plus audio
    /// files placed at the paths of playlist 11's first three entries. The
    /// scan must find the files, read the playlist tree, and resolve exactly
    /// those entries — in playlist order — to scanned-track indices.
    #[test]
    fn fake_stick_resolves_playlists() {
        let vol = std::env::temp_dir().join("ordnung-fake-usb-test");
        let _ = std::fs::remove_dir_all(&vol);
        let rb = vol.join("PIONEER/rekordbox");
        std::fs::create_dir_all(&rb).unwrap();
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../ordnung-rbdb/tests/fixtures/num_rows_export.pdb");
        std::fs::copy(fixture, rb.join("export.pdb")).unwrap();
        // Playlist 11 ("2.1 VIBEY TECHNO DEEPER") entries 1–3 in the pdb.
        let paths = [
            "Contents/Aleksi Perala/CBS024X/Aleksi Perala - A2 - 128C70 123.7.mp3",
            "Contents/Anthony Rother/Mistress 12/A1  Anthony Rother - Heaven To Heaven_MMM.wav",
            "Contents/Anthony Rother/Mistress 12/B1  Anthony Rother - Ab Ab Ab_MMM.wav",
        ];
        let sample = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../testdata/seeker-sample/Rezzett-Doyce.mp3");
        for p in paths {
            let dst = vol.join(p);
            std::fs::create_dir_all(dst.parent().unwrap()).unwrap();
            std::fs::copy(&sample, dst).unwrap();
        }

        let scan = scan_usb_volume(vol.clone());
        assert_eq!(scan.tracks.len(), 3, "all three files scanned");
        assert_eq!(scan.playlists.len(), 104, "full playlist tree read");
        let pl11 = scan.playlists.iter().find(|p| p.id == 11).unwrap();
        assert_eq!(pl11.name, "2.1 VIBEY TECHNO DEEPER");
        assert!(!pl11.is_folder);
        // Only the three planted files resolve; they come back in playlist
        // (entry-index) order, which here is the pdb path order above.
        let got: Vec<String> = scan.playlist_tracks[&11]
            .iter()
            .map(|&i| scan.tracks[i].source_path.clone())
            .collect();
        let want: Vec<String> = paths
            .iter()
            .map(|p| vol.join(p).to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, want);
        // A playlist whose files aren't on the stick is present but empty.
        assert_eq!(scan.playlist_tracks[&48], Vec::<usize>::new());
        let _ = std::fs::remove_dir_all(&vol);
    }
}
