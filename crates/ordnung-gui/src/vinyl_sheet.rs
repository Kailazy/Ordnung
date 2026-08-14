//! The record sheet: one vinyl record's full tracklist, with a way to hear every
//! position it can play.
//!
//! Two sources feed it. Tracks you own digitally play through the app's own
//! audio engine (lossless, analyzed, in the player bar). Everything else falls
//! back to the YouTube videos the Discogs community attached to the release,
//! played in the native mini-player (see [`crate::webview`]) so a record you own
//! only on wax is still listenable. Records where neither exists say so rather
//! than offering a dead button.
//!
//! The tracklist and videos come from `GET /releases/{id}`, cached in the
//! catalog's `release_cache` — so a record opened once needs no network again.

use super::*;

/// One release detail fetched for the sheet: which record it was for, and either
/// the detail or the error to show in its place.
pub(crate) struct SheetFetched {
    pub release_id: u64,
    pub result: Result<discogs::ReleaseDetail, String>,
}

/// A local catalog track linked to the open record.
pub(crate) struct SheetLocal {
    pub id: Id,
    pub title: String,
    pub path: PathBuf,
    pub bpm: Option<f32>,
    pub camelot: Option<String>,
}

/// What plays one row of the sheet.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SheetSource {
    /// A file in the catalog: index into [`VinylSheet::local`].
    Local(usize),
    /// A YouTube video on the release: index into the detail's `videos`.
    Video(usize),
    /// Neither — the record lists this track but nothing can play it.
    None,
}

/// One line of the sheet: a Discogs tracklist position, or a leftover video
/// (album rip, live set) shown under its own heading.
pub(crate) struct SheetRow {
    pub position: String,
    pub title: String,
    /// Duration as Discogs writes it; blank when it lists none.
    pub duration: String,
    pub source: SheetSource,
    /// A video for a row whose primary source is a local file, so the sheet can
    /// still offer "watch it" alongside "play my copy".
    pub also_video: Option<usize>,
}

/// The open record sheet.
pub(crate) struct VinylSheet {
    pub key: VinylCoverKey,
    pub release_id: u64,
    pub title: String,
    pub artist: String,
    /// Second header line, e.g. `1993 · Vinyl, 12" · Warp WAP42`.
    pub sub: String,
    pub detail: Option<discogs::ReleaseDetail>,
    pub local: Vec<SheetLocal>,
    pub rows: Vec<SheetRow>,
    /// Videos no track claimed, as indices into the detail's `videos`.
    pub extra_videos: Vec<usize>,
    pub loading: bool,
    pub error: Option<String>,
    /// The video index currently loaded in the mini-player, for the row marker.
    pub playing_video: Option<usize>,
    /// Set when the user hit play on the cover rather than opening the sheet:
    /// start the record as soon as there's a tracklist to start it from.
    pub pending_play: bool,
}

impl App {
    /// Open the record sheet for a grid cell, fetching its tracklist and videos
    /// if they aren't cached yet. Re-opening the record that's already open is a
    /// no-op, so a second click doesn't restart a fetch.
    pub(crate) fn open_vinyl_sheet(&mut self, key: VinylCoverKey, ctx: &egui::Context) {
        if self.vinyl_sheet.as_ref().is_some_and(|s| s.key == key) {
            return;
        }
        let Some(record) = self.vinyl_record(key) else {
            return;
        };
        let sub = [
            record.year.map(|y| y.to_string()),
            record.format.clone(),
            match (&record.label, &record.catalog_number) {
                (Some(l), Some(c)) => Some(format!("{l} {c}")),
                (Some(l), None) => Some(l.clone()),
                (None, Some(c)) => Some(c.clone()),
                (None, None) => None,
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        self.vinyl_sheet = Some(VinylSheet {
            key,
            release_id: record.release_id,
            title: record.title.clone(),
            artist: record.artist.clone(),
            sub,
            detail: None,
            local: self.sheet_local_tracks(record.release_id),
            rows: Vec::new(),
            extra_videos: Vec::new(),
            loading: true,
            error: None,
            playing_video: None,
            pending_play: false,
        });
        self.spawn_sheet_fetch(record.release_id, ctx.clone());
    }

    /// The catalog tracks linked to `release_id`, with the analysis figures the
    /// sheet shows. One small read per linked track — a record is a handful of
    /// tracks, so this stays on the UI thread like the other inline reads.
    fn sheet_local_tracks(&self, release_id: u64) -> Vec<SheetLocal> {
        let ids = self.vinyl_links.get(&release_id).cloned().unwrap_or_default();
        let Ok(cat) = Catalog::open(&self.db_path) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| {
                let t = cat.get_track(*id).ok()?;
                let analysis = cat.get_analysis(*id).ok().flatten();
                let title = t.tags.title.clone().filter(|s| !s.trim().is_empty())?;
                Some(SheetLocal {
                    id: *id,
                    title,
                    path: PathBuf::from(&t.source_path),
                    bpm: analysis.as_ref().and_then(|a| a.bpm),
                    camelot: analysis
                        .as_ref()
                        .and_then(|a| a.key)
                        .map(|k| k.camelot().label()),
                })
            })
            .collect()
    }

    /// Resolve one release's detail off the UI thread — cache first, network on
    /// a miss. A missing token is only an error when the release isn't cached,
    /// so a record opened before still works offline.
    fn spawn_sheet_fetch(&mut self, release_id: u64, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.sheet_rx = Some(rx);
        let db = self.db_path.clone();
        let token = self.discogs_token();
        thread::spawn(move || {
            let id = release_id.to_string();
            let result = Catalog::open(&db)
                .map_err(|e| e.to_string())
                .and_then(|cat| {
                    match cat.cached_release(&id) {
                        Ok(Some(d)) => return Ok(d),
                        Ok(None) => {}
                        // A cache read failure isn't fatal — fall through to the
                        // network and let that decide.
                        Err(_) => {}
                    }
                    if token.trim().is_empty() {
                        return Err("No Discogs token set. Add one in Settings to load \
                                    tracklists and videos."
                            .to_string());
                    }
                    let client = discogs::Client::new(
                        token,
                        "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung",
                    );
                    cat.release_cached_or(&id, || client.fetch_release(&id))
                        .map_err(|e| e.to_string())
                });
            let _ = tx.send(SheetFetched { release_id, result });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished release fetch and build the sheet's rows from it.
    pub(crate) fn poll_vinyl_sheet(&mut self) {
        let Some(rx) = &self.sheet_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.sheet_rx = None;
        let Some(sheet) = self.vinyl_sheet.as_mut() else {
            return;
        };
        // The user may have moved on to another record while this was in flight.
        if sheet.release_id != msg.release_id {
            return;
        }
        sheet.loading = false;
        match msg.result {
            Ok(detail) => {
                let local_titles: Vec<String> =
                    sheet.local.iter().map(|l| l.title.clone()).collect();
                let files = detail.file_matches(&local_titles);
                let videos = detail.video_matches();
                sheet.rows = detail
                    .tracklist
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let file = files.get(i).copied().flatten();
                        let video = videos.get(i).copied().flatten();
                        SheetRow {
                            position: t.position.clone(),
                            title: t.title.clone(),
                            duration: t.duration.clone(),
                            // Your own file wins: it's lossless, analyzed, and
                            // plays in the real player bar.
                            source: match (file, video) {
                                (Some(f), _) => SheetSource::Local(f),
                                (None, Some(v)) => SheetSource::Video(v),
                                (None, None) => SheetSource::None,
                            },
                            also_video: file.and(video),
                        }
                    })
                    .collect();
                sheet.extra_videos = detail.unmatched_videos().iter().map(|(i, _)| *i).collect();
                // A release with no tracklist at all (Discogs has plenty) still
                // has its videos — show them as the record's only contents
                // rather than an empty sheet.
                sheet.detail = Some(detail);
            }
            Err(e) => sheet.error = Some(e),
        }
    }

    /// Play the record from `row` onwards. Local rows go to the audio engine;
    /// video rows queue every video from there to the end of the record in the
    /// mini-player, so a side plays through without further clicks.
    fn play_sheet_row(&mut self, row: usize, frame: &eframe::Frame) {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return;
        };
        let Some(source) = sheet.rows.get(row).map(|r| r.source) else {
            return;
        };
        match source {
            SheetSource::Local(i) => {
                let Some(local) = sheet.local.get(i) else { return };
                let (id, path) = (local.id, local.path.clone());
                // Never leave a video playing under the local track.
                self.stop_sheet_video();
                self.play_track(id, path);
            }
            SheetSource::Video(_) => {
                let ids = self.sheet_video_queue(row);
                self.start_sheet_video(ids, row, frame);
            }
            SheetSource::None => {}
        }
    }

    /// The YouTube ids to hand the mini-player when starting at `row`: that row's
    /// video and every later video on the record, so playback continues down the
    /// side. Rows backed by a local file are skipped — mixing the two engines
    /// mid-queue would play two things at once.
    fn sheet_video_queue(&self, row: usize) -> Vec<String> {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return Vec::new();
        };
        let Some(detail) = sheet.detail.as_ref() else {
            return Vec::new();
        };
        sheet.rows[row..]
            .iter()
            .filter_map(|r| match r.source {
                SheetSource::Video(v) => detail.videos.get(v),
                _ => None,
            })
            .filter(|v| v.embeddable)
            .filter_map(|v| v.youtube_id().map(str::to_string))
            .collect()
    }

    /// Hand a video queue to the native mini-player, pausing local audio first.
    /// Falls back to opening the video on youtube.com when the panel isn't
    /// available (non-macOS, or a blocked embed).
    fn start_sheet_video(&mut self, ids: Vec<String>, row: usize, frame: &eframe::Frame) {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return;
        };
        let video = sheet.rows.get(row).and_then(|r| match r.source {
            SheetSource::Video(v) => Some(v),
            _ => r.also_video,
        });
        // An embed the uploader blocked would show a dead player, so send those
        // straight to YouTube instead of pretending.
        let blocked = video
            .and_then(|v| sheet.detail.as_ref()?.videos.get(v))
            .is_some_and(|v| !v.embeddable);
        let uri = video
            .and_then(|v| sheet.detail.as_ref()?.videos.get(v))
            .map(|v| v.uri.clone());
        let title = match sheet.rows.get(row) {
            Some(r) => format!("{} — {}", sheet.artist, r.title),
            None => sheet.title.clone(),
        };

        if ids.is_empty() || blocked {
            if let Some(uri) = uri {
                open_url(&uri);
            }
            return;
        }
        // One sound at a time: the video takes over from the player bar.
        if let Some(a) = self.audio.as_mut() {
            if a.is_active() {
                a.toggle_pause();
            }
        }
        if webview::play(frame, &ids, &title) {
            if let Some(sheet) = self.vinyl_sheet.as_mut() {
                sheet.playing_video = video;
            }
        } else if let Some(uri) = uri {
            open_url(&uri);
        }
    }

    /// Close the mini-player and forget which row it was on.
    pub(crate) fn stop_sheet_video(&mut self) {
        webview::close();
        if let Some(sheet) = self.vinyl_sheet.as_mut() {
            sheet.playing_video = None;
        }
    }

    /// Start the whole record: the first row that can play anything.
    fn play_sheet_from_start(&mut self, frame: &eframe::Frame) {
        let first = self.vinyl_sheet.as_ref().and_then(|s| {
            s.rows
                .iter()
                .position(|r| !matches!(r.source, SheetSource::None))
        });
        if let Some(row) = first {
            self.play_sheet_row(row, frame);
        }
    }

    /// Draw the open record sheet. Returns nothing — every action is applied
    /// before it returns, so the caller just calls it once per frame.
    pub(crate) fn draw_vinyl_sheet(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.vinyl_sheet.is_none() {
            return;
        }
        // The panel can be closed by its own title-bar button, which we only
        // find out about by asking.
        if let Some(sheet) = self.vinyl_sheet.as_mut() {
            if sheet.playing_video.is_some() && !webview::is_open() {
                sheet.playing_video = None;
            }
        }
        // A play started from the grid waits here for the tracklist to arrive.
        let start_now = self.vinyl_sheet.as_mut().is_some_and(|s| {
            let go = s.pending_play && !s.loading;
            if go {
                s.pending_play = false;
            }
            go
        });
        if start_now {
            self.play_sheet_from_start(frame);
        }

        // Snapshot what the closure paints so it never borrows `self` (actions
        // below need it mutably).
        let (key, title, artist, sub, release_id, loading, error, playing_video, video_open) = {
            let s = self.vinyl_sheet.as_ref().unwrap();
            (
                s.key,
                s.title.clone(),
                s.artist.clone(),
                s.sub.clone(),
                s.release_id,
                s.loading,
                s.error.clone(),
                s.playing_video,
                webview::is_open(),
            )
        };
        let cover = match self.vinyl_covers.get(&key) {
            Some(ThumbState::Ready(Some(t))) => Some(t.clone()),
            _ => None,
        };
        let now_playing_id = self.audio.as_ref().and_then(|a| a.current());

        /// What the user clicked this frame, applied after the window closes its
        /// borrow of `self`.
        enum Act {
            Play(usize),
            PlayAll,
            StopVideo,
            PlayExtra(usize),
            Goto,
        }
        let mut act: Option<Act> = None;
        let mut open = true;

        egui::Window::new(format!("{artist} — {title}"))
            .id(egui::Id::new(("vinyl-sheet", key)))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(620.0)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(560.0);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    // Cover.
                    const C: f32 = 120.0;
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(C, C), egui::Sense::hover());
                    match &cover {
                        Some(h) => {
                            egui::Image::new(h)
                                .fit_to_exact_size(egui::vec2(C, C))
                                .rounding(egui::Rounding::same(6.0))
                                .paint_at(ui, rect);
                        }
                        None => {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(6.0),
                                egui::Color32::from_gray(34),
                            );
                        }
                    }
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&artist).size(15.0).strong());
                        ui.label(egui::RichText::new(&title).size(15.0));
                        if !sub.is_empty() {
                            ui.label(egui::RichText::new(&sub).weak());
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            if ui
                                .button("▶  Play record")
                                .on_hover_note("Play from the first track that has a source")
                                .clicked()
                            {
                                act = Some(Act::PlayAll);
                            }
                            if video_open
                                && ui
                                    .button("■  Stop video")
                                    .on_hover_note("Close the video player")
                                    .clicked()
                            {
                                act = Some(Act::StopVideo);
                            }
                            if ui
                                .button("↗  Discogs")
                                .on_hover_note("Open this release on discogs.com")
                                .clicked()
                            {
                                open_url(&format!("https://www.discogs.com/release/{release_id}"));
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                ui.separator();

                if loading {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Loading tracklist…").weak());
                    });
                    ui.add_space(10.0);
                    return;
                }
                if let Some(e) = &error {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(220, 140, 120)));
                    ui.add_space(10.0);
                    return;
                }

                let sheet = self.vinyl_sheet.as_ref().unwrap();
                if sheet.rows.is_empty() && sheet.extra_videos.is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new(
                            "Discogs lists no tracks or videos for this release.",
                        )
                        .weak(),
                    );
                    ui.add_space(10.0);
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        for (i, row) in sheet.rows.iter().enumerate() {
                            let playing = match row.source {
                                SheetSource::Local(l) => sheet
                                    .local
                                    .get(l)
                                    .is_some_and(|t| Some(t.id) == now_playing_id),
                                SheetSource::Video(v) => playing_video == Some(v),
                                SheetSource::None => false,
                            };
                            if sheet_row_ui(ui, sheet, row, playing) {
                                act = Some(Act::Play(i));
                            }
                        }
                        // Leftover videos: album rips, live sets, anything the
                        // tracklist didn't claim.
                        if !sheet.extra_videos.is_empty() {
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new("Other videos").weak());
                            ui.separator();
                            for v in &sheet.extra_videos {
                                let Some(video) =
                                    sheet.detail.as_ref().and_then(|d| d.videos.get(*v))
                                else {
                                    continue;
                                };
                                let playing = playing_video == Some(*v);
                                if extra_video_ui(ui, video, playing) {
                                    act = Some(Act::PlayExtra(*v));
                                }
                            }
                        }
                        ui.add_space(4.0);
                    });

                // Footer: how many tracks you actually own, and a way in.
                let owned = sheet
                    .rows
                    .iter()
                    .filter(|r| matches!(r.source, SheetSource::Local(_)))
                    .count();
                ui.separator();
                ui.horizontal(|ui| {
                    let note = match owned {
                        0 => "No tracks from this record are in your library".to_string(),
                        1 => "1 track from this record is in your library".to_string(),
                        n => format!("{n} tracks from this record are in your library"),
                    };
                    ui.label(egui::RichText::new(note).small().weak());
                    if owned > 0 {
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui
                                    .button("Show in library")
                                    .on_hover_note("Filter the library to this record")
                                    .clicked()
                                {
                                    act = Some(Act::Goto);
                                }
                            },
                        );
                    }
                });
            });

        match act {
            Some(Act::Play(row)) => self.play_sheet_row(row, frame),
            Some(Act::PlayAll) => self.play_sheet_from_start(frame),
            Some(Act::StopVideo) => self.stop_sheet_video(),
            Some(Act::PlayExtra(v)) => {
                let (ids, title) = {
                    let s = self.vinyl_sheet.as_ref().unwrap();
                    let video = s.detail.as_ref().and_then(|d| d.videos.get(v));
                    (
                        video
                            .filter(|v| v.embeddable)
                            .and_then(|v| v.youtube_id().map(str::to_string))
                            .map(|id| vec![id])
                            .unwrap_or_default(),
                        video.map(|v| v.title.clone()).unwrap_or_default(),
                    )
                };
                if ids.is_empty() {
                    let uri = self
                        .vinyl_sheet
                        .as_ref()
                        .and_then(|s| s.detail.as_ref()?.videos.get(v))
                        .map(|v| v.uri.clone());
                    if let Some(uri) = uri {
                        open_url(&uri);
                    }
                } else {
                    if let Some(a) = self.audio.as_mut() {
                        if a.is_active() {
                            a.toggle_pause();
                        }
                    }
                    if webview::play(frame, &ids, &title) {
                        if let Some(s) = self.vinyl_sheet.as_mut() {
                            s.playing_video = Some(v);
                        }
                    }
                }
            }
            Some(Act::Goto) => {
                let (album, tracks) = {
                    let s = self.vinyl_sheet.as_ref().unwrap();
                    (s.title.clone(), s.local.iter().map(|l| l.id).collect())
                };
                self.vinyl_sheet = None;
                self.jump_to_catalog_tracks(album, tracks);
            }
            None => {}
        }
        if !open {
            // Closing the sheet leaves the mini-player alone: a video you started
            // keeps playing in its own window until you close that.
            self.vinyl_sheet = None;
        }
    }
}

/// One tracklist row. Returns true when the user asked to play it.
fn sheet_row_ui(
    ui: &mut egui::Ui,
    sheet: &VinylSheet,
    row: &SheetRow,
    playing: bool,
) -> bool {
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
    let playable = !matches!(row.source, SheetSource::None);
    let mut clicked = false;

    let resp = ui
        .scope(|ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(24.0);
                // Play marker.
                let glyph = if playing { "❚❚" } else if playable { "▶" } else { " " };
                let colour = if playing {
                    ACCENT
                } else if playable {
                    egui::Color32::from_gray(190)
                } else {
                    egui::Color32::from_gray(90)
                };
                ui.allocate_ui(egui::vec2(22.0, 20.0), |ui| {
                    ui.label(egui::RichText::new(glyph).size(11.0).color(colour));
                });
                // Position.
                ui.allocate_ui(egui::vec2(34.0, 20.0), |ui| {
                    ui.label(
                        egui::RichText::new(&row.position)
                            .small()
                            .color(egui::Color32::from_gray(150)),
                    );
                });
                let title = egui::RichText::new(&row.title).color(if playable {
                    egui::Color32::from_gray(230)
                } else {
                    egui::Color32::from_gray(120)
                });
                ui.label(if playing { title.strong() } else { title });

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Source chip: which of the two engines this row uses.
                    match row.source {
                        SheetSource::Local(l) => {
                            let t = &sheet.local[l];
                            let figures = match (t.bpm, &t.camelot) {
                                (Some(b), Some(k)) => format!("{b:.0} · {k}"),
                                (Some(b), None) => format!("{b:.0}"),
                                (None, Some(k)) => k.clone(),
                                (None, None) => String::new(),
                            };
                            if !figures.is_empty() {
                                ui.label(
                                    egui::RichText::new(figures)
                                        .small()
                                        .color(egui::Color32::from_gray(140)),
                                );
                                ui.add_space(8.0);
                            }
                            ui.label(egui::RichText::new("♪ your copy").small().color(ACCENT));
                        }
                        SheetSource::Video(_) => {
                            ui.label(
                                egui::RichText::new("▶ video")
                                    .small()
                                    .color(egui::Color32::from_rgb(190, 130, 130)),
                            );
                        }
                        SheetSource::None => {
                            ui.label(
                                egui::RichText::new("—")
                                    .small()
                                    .color(egui::Color32::from_gray(90)),
                            );
                        }
                    }
                    if !row.duration.is_empty() {
                        ui.add_space(10.0);
                        ui.label(
                            egui::RichText::new(&row.duration)
                                .small()
                                .color(egui::Color32::from_gray(130)),
                        );
                    }
                });
            });
        })
        .response;

    // The whole row is the hit target, so there's no small glyph to aim at.
    let rect = resp.rect;
    let id = ui.id().with(("sheet-row", &row.position, &row.title));
    let hit = ui.interact(rect, id, egui::Sense::click());
    if playable {
        if hit.hovered() {
            ui.painter().rect_filled(
                rect.expand2(egui::vec2(4.0, 1.0)),
                egui::Rounding::same(4.0),
                egui::Color32::from_white_alpha(10),
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if hit.clicked() {
            clicked = true;
        }
    } else {
        hit.on_hover_note("Not in your library, and Discogs lists no video for it");
    }
    clicked
}

/// One "other video" row (album rip, live set). Returns true when clicked.
fn extra_video_ui(ui: &mut egui::Ui, video: &discogs::ReleaseVideo, playing: bool) -> bool {
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
    let resp = ui
        .scope(|ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(22.0);
                ui.allocate_ui(egui::vec2(22.0, 20.0), |ui| {
                    ui.label(
                        egui::RichText::new(if playing { "❚❚" } else { "▶" })
                            .size(11.0)
                            .color(if playing { ACCENT } else { egui::Color32::from_gray(190) }),
                    );
                });
                ui.label(
                    egui::RichText::new(&video.title).color(egui::Color32::from_gray(215)),
                );
                if let Some(d) = video.duration_secs {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(fmt_duration(u64::from(d) * 1000))
                                .small()
                                .color(egui::Color32::from_gray(130)),
                        );
                    });
                }
            });
        })
        .response;
    let hit = ui.interact(
        resp.rect,
        ui.id().with(("sheet-extra", &video.uri)),
        egui::Sense::click(),
    );
    if hit.hovered() {
        ui.painter().rect_filled(
            resp.rect.expand2(egui::vec2(4.0, 1.0)),
            egui::Rounding::same(4.0),
            egui::Color32::from_white_alpha(10),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    hit.clicked()
}
