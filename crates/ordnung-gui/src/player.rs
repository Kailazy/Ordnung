//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// Start playing `id` in the now-playing bar, or toggle pause if it's already
    /// the loaded track. Captures the title/artist (from the visible row, falling
    /// back to the file name) so the bar can render even when the track later
    /// scrolls out of, or isn't in, the table.
    pub(crate) fn play_track(&mut self, id: Id, path: PathBuf) {
        let toggling = self.audio.as_ref().and_then(|a| a.current()) == Some(id);
        let display = self
            .rows
            .iter()
            .find(|r| r.id == id)
            .map(|r| (r.artist.clone(), r.title.clone(), r.album.clone()));
        if let Some(a) = self.audio.as_mut() {
            a.play_or_toggle(id, path.clone());
        }
        // Only (re)seed the bar when switching to a different track; a same-track
        // click is just a pause/resume and keeps the existing display + scrub.
        if !toggling {
            let (artist, title, album) = display.unwrap_or_else(|| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (String::new(), stem, String::new())
            });
            let source_path = path.to_string_lossy().into_owned();
            // Advertise the track to the OS Now Playing panel right away; its cover
            // resolves asynchronously and is attached when ready (see below).
            if let Some(a) = self.audio.as_mut() {
                a.set_now_playing(title.clone(), artist.clone());
            }
            // Resolve the cover off-thread (catalog read + temp-file write) so the
            // play click never blocks; the result comes back over `media_cover_rx`.
            let cover_tx = self.media_cover_tx.clone();
            let db = self.db_path.clone();
            let cover_src = source_path.clone();
            thread::spawn(move || {
                let url = now_playing_cover_url(&db, id, &cover_src);
                let _ = cover_tx.send((id, url));
            });
            // Load the waveform for the bar. One small catalog row read, like the
            // other inline reads in the GUI; cover art is the only thing worth
            // offloading. Empty vecs (unanalyzed track) just render a flat line.
            let analysis = Catalog::open(&self.db_path)
                .and_then(|c| c.get_analysis(id))
                .ok()
                .flatten();
            let (waveform, waveform_bands) = analysis
                .as_ref()
                .map(|a| {
                    // Only the v11+ 4-byte stride is what the renderer expects.
                    let bands = if a.analyzer_version >= 11 {
                        a.waveform_bands.clone()
                    } else {
                        Vec::new()
                    };
                    (a.waveform_preview.clone(), bands)
                })
                .unwrap_or_default();
            // Beatgrid for the moving lane: bpm + first-beat position + downbeat
            // phase (from the anchor beat's bar number). `get_analysis` returns the
            // grid as one anchor beat, so the phase is `1 - number` (mod 4).
            let grid = analysis.as_ref().and_then(|a| {
                let bpm = a.bpm?;
                let b0 = a.beatgrid.beats.first()?;
                Some(PlayerGrid {
                    bpm,
                    first_beat_ms: b0.position_ms as f64,
                    downbeat_phase: (1 - b0.number as i64).rem_euclid(4) as u32,
                })
            });
            self.now_playing = Some(NowPlaying {
                id,
                artist,
                title,
                album,
                source_path,
                waveform,
                waveform_bands,
                hires_bands: None,
                hires_requested: false,
                grid,
            });
            self.scrub = None;
        }
    }

    /// Start a random next track, for the player bar's shuffle buttons. The
    /// candidate pool is the current table rows, so shuffle respects the active
    /// view and filters. With `smart` the pool narrows to tracks harmonically
    /// compatible with the playing key on the Camelot wheel; when the current
    /// key is unknown or nothing compatible exists it falls back to plain
    /// shuffle rather than going silent.
    fn shuffle_next(&mut self, current: Id, smart: bool) {
        let current_camelot = self
            .rows
            .iter()
            .find(|r| r.id == current)
            .and_then(|r| r.camelot)
            .or_else(|| {
                // Playing track not in the visible rows (filtered out): read its
                // key from the catalog instead.
                Catalog::open(&self.db_path)
                    .and_then(|c| c.get_analysis(current))
                    .ok()
                    .flatten()
                    .and_then(|a| a.key)
                    .map(|k| k.camelot())
            });
        let pick = |compatible_only: bool| -> Option<(Id, PathBuf)> {
            let pool: Vec<&TrackRow> = self
                .rows
                .iter()
                .filter(|r| r.id != current)
                .filter(|r| {
                    !compatible_only
                        || matches!(
                            (current_camelot, r.camelot),
                            (Some(a), Some(b)) if a.compatible_with(b)
                        )
                })
                .collect();
            if pool.is_empty() {
                return None;
            }
            let r = pool[random_index(pool.len())];
            Some((r.id, r.source_path.clone()))
        };
        let next = if smart {
            pick(true).or_else(|| pick(false))
        } else {
            pick(false)
        };
        if let Some((id, path)) = next {
            self.play_track(id, path);
        }
    }

    /// Render the bottom now-playing bar: artwork, title/artist, a play/pause
    /// button and a draggable scrubber. Shown only while the engine still has the
    /// `now_playing` track loaded (or decoding). The seek fires on scrub release so
    /// the audio sink is rebuilt once per gesture, not every frame.
    pub(crate) fn draw_player(&mut self, ctx: &egui::Context) {
        let Some(np_id) = self.now_playing.as_ref().map(|n| n.id) else {
            return;
        };
        // Hide the bar once the engine has dropped this track (e.g. a load the user
        // cancelled). A naturally-finished track stays loaded, so it lingers paused.
        let visible = self.audio.as_ref().map_or(false, |a| {
            a.current() == Some(np_id) || matches!(a.state_for(np_id), PlayState::Loading)
        });
        if !visible {
            return;
        }

        let np_path = self.now_playing.as_ref().unwrap().source_path.clone();
        let art = self.cover_full_texture(ctx, np_id, &np_path);
        let (pos, dur, loading, playing) = {
            let a = self.audio.as_ref().unwrap();
            (
                a.position(),
                a.duration(),
                matches!(a.state_for(np_id), PlayState::Loading),
                a.state_for(np_id) == PlayState::Playing,
            )
        };

        // Kick off the high-res zoom envelope once, the first frame the decoded PCM
        // is available. This is full-resolution sample analysis of the actual audio
        // (every sample, peak-preserving) — the zoom lane's "rekordbox-level" detail.
        // It runs off-thread (a long track is millions of samples) and comes back
        // over `hires_rx`; until then the lane falls back to the coarse preview.
        if self
            .now_playing
            .as_ref()
            .map_or(false, |n| !n.hires_requested && n.hires_bands.is_none())
        {
            if let Some((samples, ch, sr)) = self.audio.as_ref().and_then(|a| a.pcm()) {
                self.now_playing.as_mut().unwrap().hires_requested = true;
                let tx = self.hires_tx.clone();
                let ctx = ctx.clone();
                let low_hz = self.config.waveform_low_hz;
                let mid_hz = self.config.waveform_mid_hz;
                thread::spawn(move || {
                    let hires = compute_hires_bands(&samples, ch, sr, low_hz, mid_hz);
                    let _ = tx.send((np_id, hires));
                    ctx.request_repaint();
                });
            }
        }

        let np = self.now_playing.as_ref().unwrap();
        let title = if np.title.trim().is_empty() {
            "Unknown title".to_string()
        } else {
            np.title.clone()
        };
        let artist = np.artist.clone();
        let album = np.album.clone();
        // Clone the waveform out so the panel closure (which mutably borrows
        // `self.scrub`) doesn't also need to borrow `self.now_playing`.
        let waveform = np.waveform.clone();
        let bands = np.waveform_bands.clone();
        // High-res bands for the zoom lane; fall back to the coarse preview until
        // the PCM has been analyzed (or for tracks the engine never decoded).
        let hires = np.hires_bands.clone().unwrap_or_default();
        let grid = np.grid;
        let wave_style = WaveformStyle::from_config(&self.config);

        const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
        let mut toggle = false;
        let mut close = false;
        let mut shuffle = false;
        let mut smart_shuffle = false;
        let mut seek_to: Option<f32> = None;
        // Set by clicking the title/artist labels; applied after the panel closure
        // (jumping rebuilds rows and selection, which needs `&mut self`).
        let mut filter_album = false;
        let mut filter_artist = false;

        // Fraction the bar reflects: the live position, or the in-progress scrub
        // before release. Shared by the zoom lane and the overview strip so both
        // track the same playhead. Computed once up front (the panel closure below
        // mutably borrows `self.scrub`).
        let shown_frac = self
            .scrub
            .unwrap_or(if dur > 0.0 { pos / dur } else { 0.0 })
            .clamp(0.0, 1.0);

        let lane_h = self.wave_lane_h.clamp(MIN_LANE_H, MAX_LANE_H);
        // The panel grows with the (resizable) lane; the controls row below is a
        // fixed base. Only reserve the lane's extra height when there's a waveform
        // to show — unanalyzed tracks have no lane.
        let panel_h = if waveform.is_empty() {
            150.0
        } else {
            PANEL_BASE_H + lane_h
        };
        egui::TopBottomPanel::bottom("player")
            .exact_height(panel_h)
            .show(ctx, |ui| {
                ui.add_space(8.0);

                // Zoomed detail lane — a window of `wave_zoom_secs` centered on the
                // playhead, scrolling under it during playback. Wheel to zoom,
                // click/drag to seek. Skipped for unanalyzed tracks (no waveform).
                if !waveform.is_empty() {
                    // Prefer the high-res envelope; fall back to the coarse preview
                    // bands while the PCM is still decoding.
                    let detail = if hires.is_empty() { &bands } else { &hires };
                    self.draw_zoom_lane(
                        ui,
                        &waveform,
                        detail,
                        &wave_style,
                        shown_frac,
                        dur,
                        grid,
                        &mut seek_to,
                    );
                    ui.add_space(8.0);
                }

                ui.horizontal(|ui| {
                    ui.add_space(10.0);

                    // Artwork.
                    let art_sz = egui::vec2(56.0, 56.0);
                    let (art_rect, _) = ui.allocate_exact_size(art_sz, egui::Sense::hover());
                    match &art {
                        Some(h) => {
                            egui::Image::new(h)
                                .fit_to_exact_size(art_sz)
                                .paint_at(ui, art_rect);
                        }
                        None => {
                            ui.painter().rect_filled(
                                art_rect,
                                egui::Rounding::same(6.0),
                                egui::Color32::from_gray(38),
                            );
                            ui.painter().text(
                                art_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "♪",
                                egui::FontId::proportional(22.0),
                                egui::Color32::from_gray(110),
                            );
                        }
                    }
                    ui.add_space(12.0);

                    // Title / artist — a fixed-width column so the title's
                    // length never shifts the waveform that follows it. Titles
                    // wider than the column slow-scroll horizontally like
                    // Spotify; shorter ones sit left-aligned.
                    const LABEL_W: f32 = 220.0;
                    let (block, _) =
                        ui.allocate_exact_size(egui::vec2(LABEL_W, 56.0), egui::Sense::hover());
                    let now = ui.input(|i| i.time);
                    let (anim_title, title_size) = draw_scrolling_line(
                        ui,
                        egui::pos2(block.left(), block.top() + 8.0),
                        LABEL_W,
                        &title,
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_gray(240),
                        now,
                    );
                    let (anim_artist, artist_size) = draw_scrolling_line(
                        ui,
                        egui::pos2(block.left(), block.top() + 32.0),
                        LABEL_W,
                        &artist,
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_gray(165),
                        now,
                    );
                    if anim_title || anim_artist {
                        ui.ctx().request_repaint();
                    }

                    // Title → album filter, artist → artist filter. Only the text
                    // itself is the hit target; hover shows a pointer + underline
                    // (link affordance). Skipped when the tag is empty (nothing to
                    // filter by).
                    let link = |pos: egui::Pos2,
                                    size: egui::Vec2,
                                    salt: &str,
                                    color: egui::Color32,
                                    note: &str,
                                    hit: &mut bool| {
                        let rect = egui::Rect::from_min_size(pos, size);
                        let resp = ui
                            .interact(rect, ui.id().with(salt), egui::Sense::click())
                            .on_hover_note(note);
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            ui.painter().line_segment(
                                [
                                    egui::pos2(rect.left(), rect.bottom()),
                                    egui::pos2(rect.right(), rect.bottom()),
                                ],
                                egui::Stroke::new(1.0, color),
                            );
                        }
                        if resp.clicked() {
                            *hit = true;
                        }
                    };
                    if !album.trim().is_empty() {
                        link(
                            egui::pos2(block.left(), block.top() + 8.0),
                            title_size,
                            "np_title_link",
                            egui::Color32::from_gray(240),
                            "Show this album",
                            &mut filter_album,
                        );
                    }
                    if !artist.trim().is_empty() {
                        link(
                            egui::pos2(block.left(), block.top() + 32.0),
                            artist_size,
                            "np_artist_link",
                            egui::Color32::from_gray(165),
                            "Show this artist",
                            &mut filter_artist,
                        );
                    }
                    ui.add_space(16.0);

                    // Play / pause button — a white disc with a hand-drawn glyph so
                    // it renders identically regardless of which fonts are present.
                    let (btn_rect, btn) =
                        ui.allocate_exact_size(egui::vec2(36.0, 36.0), egui::Sense::click());
                    let c = btn_rect.center();
                    let disc = if btn.hovered() {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_gray(210)
                    };
                    ui.painter().circle_filled(c, 17.0, disc);
                    let fg = egui::Color32::from_gray(18);
                    if loading {
                        ui.painter().text(
                            c,
                            egui::Align2::CENTER_CENTER,
                            "…",
                            egui::FontId::proportional(16.0),
                            fg,
                        );
                    } else if playing {
                        for dx in [-3.5_f32, 3.5] {
                            ui.painter().rect_filled(
                                egui::Rect::from_center_size(
                                    c + egui::vec2(dx, 0.0),
                                    egui::vec2(3.5, 13.0),
                                ),
                                egui::Rounding::same(1.0),
                                fg,
                            );
                        }
                    } else {
                        let r = 7.0;
                        ui.painter().add(egui::Shape::convex_polygon(
                            vec![
                                c + egui::vec2(-r * 0.55, -r),
                                c + egui::vec2(-r * 0.55, r),
                                c + egui::vec2(r, 0.0),
                            ],
                            fg,
                            egui::Stroke::NONE,
                        ));
                    }
                    if btn.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if btn.clicked() && !loading {
                        toggle = true;
                    }
                    ui.add_space(10.0);

                    // Shuffle + smart-shuffle: plain glyphs beside the play disc,
                    // hand-drawn like the play glyph so no icon font is needed.
                    // Shuffle jumps to a random track from the current view; the
                    // vinyl "DJ" button restricts the pool to tracks harmonically
                    // compatible with the playing key on the Camelot wheel.
                    let (sh_rect, sh) =
                        ui.allocate_exact_size(egui::vec2(28.0, 36.0), egui::Sense::click());
                    let sh = sh.on_hover_note("Play a random track");
                    let col = if sh.hovered() {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_gray(170)
                    };
                    draw_shuffle_glyph(ui.painter(), sh_rect.center(), col);
                    if sh.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if sh.clicked() {
                        shuffle = true;
                    }
                    ui.add_space(6.0);

                    let (dj_rect, dj) =
                        ui.allocate_exact_size(egui::vec2(30.0, 36.0), egui::Sense::click());
                    let dj = dj.on_hover_note("Play a random track in a compatible key");
                    let col = if dj.hovered() {
                        egui::Color32::WHITE
                    } else {
                        egui::Color32::from_gray(170)
                    };
                    draw_vinyl_glyph(ui.painter(), dj_rect.center(), col, ACCENT);
                    if dj.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if dj.clicked() {
                        smart_shuffle = true;
                    }
                    ui.add_space(12.0);

                    // Elapsed time. Fixed-width so the digits changing during a
                    // scrub (e.g. "0:05" → "0:00", or crossing "10:00") can't shift
                    // the waveform that follows it — that shift was the scrub jitter.
                    ui.add_sized(
                        egui::vec2(46.0, 18.0),
                        egui::Label::new(
                            egui::RichText::new(fmt_time(shown_frac * dur))
                                .monospace()
                                .size(11.0)
                                .color(egui::Color32::from_gray(170)),
                        ),
                    );

                    // Scrubber — fills the space left after the trailing time label
                    // and close button.
                    let track_w = (ui.available_width() - 86.0).max(60.0);
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(track_w, 40.0),
                        egui::Sense::click_and_drag(),
                    );
                    let y = rect.center().y;
                    let (x0, x1) = (rect.left(), rect.right());
                    let knob_x = x0 + shown_frac * (x1 - x0);
                    let painter = ui.painter();
                    if waveform.is_empty() {
                        // Unanalyzed track: keep the original flat progress line.
                        painter.line_segment(
                            [egui::pos2(x0, y), egui::pos2(x1, y)],
                            egui::Stroke::new(4.0, egui::Color32::from_gray(70)),
                        );
                        painter.line_segment(
                            [egui::pos2(x0, y), egui::pos2(knob_x, y)],
                            egui::Stroke::new(4.0, ACCENT),
                        );
                    } else {
                        draw_waveform(
                            painter,
                            rect,
                            &waveform,
                            &bands,
                            &wave_style,
                            Some(shown_frac),
                            (0.0, 1.0),
                        );
                    }
                    let knob_r = if resp.hovered() || self.scrub.is_some() {
                        6.5
                    } else {
                        5.0
                    };
                    ui.painter()
                        .circle_filled(egui::pos2(knob_x, y), knob_r, egui::Color32::WHITE);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    let frac_at = |p: egui::Pos2| ((p.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                    if (resp.dragged() || resp.drag_started()) && dur > 0.0 {
                        if let Some(p) = resp.interact_pointer_pos() {
                            self.scrub = Some(frac_at(p));
                        }
                    }
                    if resp.drag_stopped() {
                        if let Some(f) = self.scrub.take() {
                            seek_to = Some(f * dur);
                        }
                    }
                    if resp.clicked() && dur > 0.0 {
                        if let Some(p) = resp.interact_pointer_pos() {
                            seek_to = Some(frac_at(p) * dur);
                        }
                        self.scrub = None;
                    }

                    // Total time.
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new(fmt_time(dur))
                            .monospace()
                            .size(11.0)
                            .color(egui::Color32::from_gray(170)),
                    );

                    // Dismiss the player.
                    ui.add_space(4.0);
                    if ui.small_button("✕").on_hover_note("Close player").clicked() {
                        close = true;
                    }
                });
            });

        if let Some(s) = seek_to {
            if let Some(a) = self.audio.as_mut() {
                a.seek(s);
            }
        }
        if toggle {
            if let Some(a) = self.audio.as_mut() {
                a.toggle_pause();
            }
        }
        if shuffle || smart_shuffle {
            self.shuffle_next(np_id, smart_shuffle);
        }
        if close {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            self.now_playing = None;
            self.scrub = None;
        }

        // Title/artist label click: show the full library narrowed to the playing
        // track's album or artist (a per-column filter, so an artist named like a
        // song title doesn't over-match), then select and reveal the track. Same
        // shape as `jump_to_catalog_tracks`. The album needle is quoted — exact
        // whole-cell match (see `apply_col_filters`) — so a short album name
        // doesn't also pull in every longer album containing it. Artist stays
        // substring so "A feat. B" tracks still show under artist A.
        if filter_album || filter_artist {
            let np = self.now_playing.as_ref().unwrap();
            let (col, needle) = if filter_album {
                (TableColumn::Album, format!("\"{}\"", np.album))
            } else {
                (TableColumn::Artist, np.artist.clone())
            };
            self.view = LibraryView::Library;
            self.filter.clear();
            self.col_filters.clear();
            self.col_filters.insert(col, needle);
            // `reload` prunes the selection to live rows, so seed it after.
            self.reload();
            self.selection = std::iter::once(np_id).collect();
            self.selected = Some(np_id);
            self.select_anchor = Some(np_id);
            self.scroll_to_track = Some(np_id);
            self.refresh_selected();
        }

        // Drive the playhead: while audio is rolling, keep repainting so the zoom
        // lane scrolls and the knob advances between user input.
        if playing {
            ctx.request_repaint();
        }
    }

    /// Draw the zoomed detail lane: a `wave_zoom_secs`-wide window of the track,
    /// centered on the playhead and scrolling under it during playback. The window
    /// is clamped to the track bounds, so near the ends the playhead drifts off
    /// center rather than the lane showing empty runway. Wheel over the lane zooms;
    /// click/drag seeks (writing `seek_to` on release, like the overview strip).
    fn draw_zoom_lane(
        &mut self,
        ui: &mut egui::Ui,
        waveform: &[u8],
        bands: &[u8],
        wave_style: &WaveformStyle,
        shown_frac: f32,
        dur: f32,
        grid: Option<PlayerGrid>,
        seek_to: &mut Option<f32>,
    ) {
        const MARGIN: f32 = 10.0;
        let zoom = self
            .wave_zoom_secs
            .clamp(MIN_ZOOM_SECS, MAX_ZOOM_SECS);
        let lane_h = self.wave_lane_h.clamp(MIN_LANE_H, MAX_LANE_H);
        let lane_w = (ui.available_width() - 2.0 * MARGIN).max(60.0);

        // The smoothing slider and the resize grip used to sit in their own rows
        // above the lane, leaving a dead band of empty panel between them and the
        // waveform. Smoothing now lives in Settings → Waveform; the grip floats
        // directly over the lane's top edge (below), so the lane fills the panel.

        ui.horizontal(|ui| {
            ui.add_space(MARGIN);
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(lane_w, lane_h),
                egui::Sense::click_and_drag(),
            );

            // Visible window in track-fraction, width `zoom` seconds, slid to stay
            // inside `[0, 1]`.
            let span = if dur > 0.0 {
                (zoom / dur).clamp(0.0, 1.0)
            } else {
                1.0
            };
            let w0 = (shown_frac - span / 2.0).clamp(0.0, (1.0 - span).max(0.0));
            let w1 = w0 + span;

            let painter = ui.painter();
            painter.rect_filled(
                rect,
                egui::Rounding::same(4.0),
                egui::Color32::from_gray(22),
            );
            let draw_rect = rect.shrink2(egui::vec2(3.0, 4.0));
            if bands.is_empty() {
                // No band data (unanalyzed / pre-v11): coarse envelope, no scroll
                // smoothing to apply anyway.
                draw_waveform(
                    painter,
                    draw_rect,
                    waveform,
                    bands,
                    wave_style,
                    Some(shown_frac),
                    (w0, w1),
                );
            } else {
                // The lane draws whichever envelope the caller picked (hi-res, or
                // the coarse preview until PCM analysis lands) — derive its actual
                // bin rate so the time-based smoothing spans the same audio either
                // way.
                let bins_per_sec = if dur > 0.0 {
                    (bands.len() / 4) as f32 / dur
                } else {
                    HIRES_BINS_PER_SEC
                };
                draw_waveform_scrolling(
                    painter,
                    draw_rect,
                    bands,
                    bins_per_sec,
                    wave_style,
                    shown_frac,
                    (w0, w1),
                );
            }

            // Beatgrid overlay: vertical lines at each beat in the visible window,
            // downbeats (bar "1") drawn brighter and thicker like rekordbox's red
            // bar markers. Drawn over the waveform, under the playhead.
            if let Some(g) = grid {
                if g.bpm > 0.0 && dur > 0.0 {
                    draw_beatgrid(painter, draw_rect, g, dur, (w0, w1));
                }
            }

            // Fixed playhead line at the window's mapping of the live position.
            let play_x =
                rect.left() + ((shown_frac - w0) / span.max(f32::EPSILON)) * rect.width();
            painter.line_segment(
                [
                    egui::pos2(play_x, rect.top()),
                    egui::pos2(play_x, rect.bottom()),
                ],
                egui::Stroke::new(1.5, egui::Color32::from_rgb(255, 80, 80)),
            );

            // Resize grip, floating opaquely over the lane's top edge instead of
            // taking its own row above it. Drag up to grow the lane (and the
            // panel), down to shrink. Registered after the lane so it sits on top
            // and captures the drag there rather than seeking.
            let grip_w = 48.0;
            let grip_center = egui::pos2(rect.center().x, rect.top() + 9.0);
            let grip_hit =
                egui::Rect::from_center_size(grip_center, egui::vec2(grip_w + 16.0, 16.0));
            let grip = ui.interact(
                grip_hit,
                ui.id().with("wave_resize_grip"),
                egui::Sense::drag(),
            );
            if grip.dragged() {
                // Dragging up is negative dy; growing the lane means subtracting it.
                self.wave_lane_h =
                    (lane_h - grip.drag_delta().y).clamp(MIN_LANE_H, MAX_LANE_H);
                ui.ctx().request_repaint();
            }
            if grip.hovered() || grip.dragged() {
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
            }
            let grip_painter = ui.painter();
            // Just the gray pill, floating see-through over the lane (no backing box).
            // Semi-transparent so the waveform shows through; opaquer on hover.
            let pill_color = if grip.hovered() || grip.dragged() {
                egui::Color32::from_rgba_unmultiplied(220, 220, 220, 200)
            } else {
                egui::Color32::from_rgba_unmultiplied(150, 150, 150, 90)
            };
            grip_painter.rect_filled(
                egui::Rect::from_center_size(grip_center, egui::vec2(grip_w, 4.0)),
                egui::Rounding::same(2.0),
                pill_color,
            );

            // Wheel to zoom (multiplicative, so each notch is a constant ratio).
            if resp.hovered() {
                let scroll = ui.input(|i| i.smooth_scroll_delta.y);
                if scroll != 0.0 {
                    self.wave_zoom_secs =
                        (zoom * (-scroll * 0.004).exp()).clamp(MIN_ZOOM_SECS, MAX_ZOOM_SECS);
                    ui.ctx().request_repaint();
                }
                ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
            }

            // Click/drag to seek — map pointer x back through the window.
            let frac_at = |p: egui::Pos2| {
                (w0 + ((p.x - rect.left()) / rect.width()).clamp(0.0, 1.0) * span)
                    .clamp(0.0, 1.0)
            };
            if (resp.dragged() || resp.drag_started()) && dur > 0.0 {
                if let Some(p) = resp.interact_pointer_pos() {
                    self.scrub = Some(frac_at(p));
                }
            }
            if resp.drag_stopped() {
                if let Some(f) = self.scrub.take() {
                    *seek_to = Some(f * dur);
                }
            }
            if resp.clicked() && dur > 0.0 {
                if let Some(p) = resp.interact_pointer_pos() {
                    *seek_to = Some(frac_at(p) * dur);
                }
                self.scrub = None;
            }
        });
    }
}

/// Draw the beatgrid over the zoom lane: a vertical line at every beat inside the
/// visible window `(w0, w1)` (track-fraction), with downbeats (bar "1") brighter and
/// thicker — rekordbox's white-beats / accented-bar look. Beat `i` sits at
/// `first_beat_ms/1000 + i·60/bpm` seconds (any integer `i`, so the grid fills the
/// lane even before the anchor beat). To stay readable when zoomed out, plain beats
/// drop out once they'd crowd below a few pixels apart; downbeats persist longer.
fn draw_beatgrid(
    painter: &egui::Painter,
    rect: egui::Rect,
    grid: PlayerGrid,
    dur: f32,
    window: (f32, f32),
) {
    let beat_col = egui::Color32::from_rgba_unmultiplied(255, 255, 255, 55);
    let downbeat_col = egui::Color32::from_rgba_unmultiplied(255, 120, 40, 210);
    for (x, is_downbeat) in beat_lines(grid, dur, window, rect.left(), rect.width()) {
        let (col, w) = if is_downbeat {
            (downbeat_col, 1.6)
        } else {
            (beat_col, 1.0)
        };
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(w, col),
        );
    }
}

/// Pure geometry for [`draw_beatgrid`]: the `(x, is_downbeat)` of every grid line
/// inside the visible window `(w0, w1)` (track-fraction), mapped onto `[left,
/// left+width]`. Plain beats drop out once they'd crowd below ~6 px apart; below
/// ~2 px even bars are omitted (too dense to read). Beat `i` is at
/// `first_beat_ms/1000 + i·60/bpm` seconds and is a downbeat when
/// `(i - downbeat_phase).rem_euclid(4) == 0`.
fn beat_lines(
    grid: PlayerGrid,
    dur: f32,
    window: (f32, f32),
    left: f32,
    width: f32,
) -> Vec<(f32, bool)> {
    let (w0, w1) = window;
    let (t0, t1) = (w0 * dur, w1 * dur);
    let span = (t1 - t0).max(f32::EPSILON);
    let period = 60.0 / grid.bpm.max(1.0);
    if grid.bpm <= 0.0 || dur <= 0.0 {
        return Vec::new();
    }
    let px_per_beat = period / span * width;
    if px_per_beat < 2.0 {
        return Vec::new();
    }
    let beats_visible = px_per_beat >= 6.0;

    let first = grid.first_beat_ms as f32 / 1000.0;
    let i0 = ((t0 - first) / period).floor() as i64;
    let i1 = ((t1 - first) / period).ceil() as i64;

    let mut out = Vec::new();
    for i in i0..=i1 {
        let is_downbeat = (i - grid.downbeat_phase as i64).rem_euclid(4) == 0;
        if !is_downbeat && !beats_visible {
            continue;
        }
        let t = first + i as f32 * period;
        if t < t0 || t > t1 {
            continue;
        }
        out.push((left + (t - t0) / span * width, is_downbeat));
    }
    out
}

#[cfg(test)]
mod grid_tests {
    use super::*;

    #[test]
    fn beat_lines_spacing_downbeats_and_window() {
        // 120 BPM → 0.5 s/beat. First beat at 0.25 s, downbeat phase 1 (the 2nd
        // detected beat is the "1"). Window 0..4 s over a 4 s track, 400 px wide.
        let g = PlayerGrid { bpm: 120.0, first_beat_ms: 250.0, downbeat_phase: 1 };
        let lines = beat_lines(g, 4.0, (0.0, 1.0), 0.0, 400.0);
        // Beats at 0.25,0.75,...,3.75 → 8 lines across 4 s.
        assert_eq!(lines.len(), 8);
        // 0.5 s = 50 px at 100 px/s; first at 0.25 s = 25 px.
        assert!((lines[0].0 - 25.0).abs() < 0.5);
        assert!((lines[1].0 - 75.0).abs() < 0.5);
        // Downbeats: i where (i-1)%4==0 → i=1,5 → the 2nd and 6th lines.
        let downs: Vec<usize> = lines.iter().enumerate().filter(|(_, l)| l.1).map(|(k, _)| k).collect();
        assert_eq!(downs, vec![1, 5]);
    }

    #[test]
    fn beat_lines_density_gating() {
        let g = PlayerGrid { bpm: 128.0, first_beat_ms: 0.0, downbeat_phase: 0 };
        // Whole 600 s track in 400 px: ~0.03 px/beat → nothing drawn.
        assert!(beat_lines(g, 600.0, (0.0, 1.0), 0.0, 400.0).is_empty());
        // Zoomed to ~8 s window: beats ~10 px apart → beats show, incl. non-downbeats.
        let win = beat_lines(g, 600.0, (0.10, 0.10 + 8.0 / 600.0), 0.0, 400.0);
        assert!(win.iter().any(|l| !l.1), "plain beats visible when zoomed in");
    }

    #[test]
    fn beat_lines_absent_without_tempo() {
        let g = PlayerGrid { bpm: 0.0, first_beat_ms: 0.0, downbeat_phase: 0 };
        assert!(beat_lines(g, 300.0, (0.0, 1.0), 0.0, 400.0).is_empty());
    }
}

/// Paint one line of text left-aligned within `width`, clipped to it. If the
/// text is wider than `width`, scroll it horizontally Spotify-style: hold at the
/// start, glide left to reveal the tail, hold at the end, then loop. Returns
/// `true` while the line is animating (so the caller can request a repaint) plus
/// the displayed size — text size clamped to `width` — for hit-testing the text
/// as a link.
fn draw_scrolling_line(
    ui: &egui::Ui,
    top_left: egui::Pos2,
    width: f32,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    time: f64,
) -> (bool, egui::Vec2) {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size();
    let shown = egui::vec2(size.x.min(width), size.y);
    let clip = egui::Rect::from_min_size(top_left, egui::vec2(width, size.y));
    let painter = ui.painter_at(clip);
    if size.x <= width {
        painter.galley(top_left, galley, color);
        return (false, shown);
    }
    // Overflowing: hold, scroll left at a constant pace, hold, then repeat.
    const SPEED: f64 = 28.0; // px/s
    const HOLD: f64 = 2.0; // s paused at each end
    let overflow = (size.x - width) as f64;
    let scroll_t = overflow / SPEED;
    let cycle = HOLD + scroll_t + HOLD;
    let t = time % cycle;
    let offset = if t < HOLD {
        0.0
    } else if t < HOLD + scroll_t {
        (t - HOLD) * SPEED
    } else {
        overflow
    };
    painter.galley(top_left - egui::vec2(offset as f32, 0.0), galley, color);
    (true, shown)
}

pub(crate) fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// Random index in `0..len` without pulling in an RNG crate: each
/// `RandomState` is seeded from per-process entropy plus a per-instance
/// counter, so one fresh hasher's output is unpredictable enough for
/// picking a shuffle track. `len` must be non-zero.
fn random_index(len: usize) -> usize {
    use std::hash::{BuildHasher, Hasher};
    let h = std::collections::hash_map::RandomState::new().build_hasher();
    h.finish() as usize % len
}

/// Hand-drawn shuffle glyph (two crossing arrows), sized for the 28px player
/// buttons. Drawn like the play/pause glyph so it renders identically
/// regardless of which fonts are present.
fn draw_shuffle_glyph(painter: &egui::Painter, c: egui::Pos2, color: egui::Color32) {
    let (w, h) = (8.0, 5.0);
    let stroke = egui::Stroke::new(1.8, color);
    for dir in [-1.0_f32, 1.0] {
        let from = egui::pos2(c.x - w, c.y - dir * h);
        let to = egui::pos2(c.x + w, c.y + dir * h);
        painter.line_segment([from, to], stroke);
        let v = (to - from).normalized();
        let n = egui::vec2(-v.y, v.x);
        painter.add(egui::Shape::convex_polygon(
            vec![
                to + v * 2.5,
                to - v * 3.5 + n * 3.0,
                to - v * 3.5 - n * 3.0,
            ],
            color,
            egui::Stroke::NONE,
        ));
    }
}

/// Hand-drawn vinyl-record glyph with an accent sparkle for the smart-shuffle
/// (DJ) button: outer platter, groove ring, spindle dot.
fn draw_vinyl_glyph(
    painter: &egui::Painter,
    c: egui::Pos2,
    color: egui::Color32,
    accent: egui::Color32,
) {
    painter.circle_stroke(c, 8.0, egui::Stroke::new(1.6, color));
    painter.circle_stroke(c, 4.6, egui::Stroke::new(1.0, color.gamma_multiply(0.55)));
    painter.circle_filled(c, 1.7, color);
    // Sparkle at the platter's top-right corner marks the pick as "smart".
    let s = egui::pos2(c.x + 8.0, c.y - 8.0);
    let r = 3.4;
    painter.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(s.x, s.y - r),
            egui::pos2(s.x + r * 0.55, s.y),
            egui::pos2(s.x, s.y + r),
            egui::pos2(s.x - r * 0.55, s.y),
        ],
        accent,
        egui::Stroke::NONE,
    ));
}

/// Live, user-tunable rendering parameters for the waveform, built each frame
/// from [`config::Config`] (the Waveform settings tab writes them). Held by value
/// at the call sites and passed by reference into [`draw_waveform`], so slider
/// moves take effect on the very next frame with no re-analysis.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WaveformStyle {
    /// Energy vs. spectrum coloring.
    pub mode: config::WaveformColorMode,
    /// Render-time height companding (see [`wave_height`]).
    pub height_exp: f32,
    /// Per-band height gain `[low, mid, high]`, spectrum mode only.
    pub band_gain: [f32; 3],
    /// Envelope height gain for energy mode (the spectrum `band_gain` analogue).
    pub energy_gain: f32,
    /// Per-band colors `[low, mid, high]` for spectrum mode.
    pub band_colors: [egui::Color32; 3],
    /// The five cool→hot gradient stops for energy mode (quiet → loudest).
    pub energy_colors: [egui::Color32; 5],
    /// Smoothing strength `[0, 1]`: scales `attack_secs`/`release_secs` from `0`
    /// (raw envelope) to their full values. From the Waveform settings tab
    /// (`config::waveform_smoothing`).
    pub smoothing: f32,
    /// Attack time constant (seconds of audio) at full smoothing — how much a
    /// rising edge is rounded (`config::waveform_smooth_attack_ms`).
    pub attack_secs: f32,
    /// Release time constant (seconds of audio) at full smoothing — how long a
    /// falling tail rings out (`config::waveform_smooth_release_ms`).
    pub release_secs: f32,
    /// Bass floor threshold `[0, 1]`: low-band content quieter than this is
    /// dimmed by `bass_floor_amount` (sustained sub under a kick), louder bass
    /// kept. Spectrum mode only. See [`bass_floor_gain`].
    pub bass_floor_threshold: f32,
    /// How hard to dim sub below `bass_floor_threshold` (`0` off, `1` removes it).
    pub bass_floor_amount: f32,
}

impl WaveformStyle {
    /// Build the live style from the current config.
    pub(crate) fn from_config(cfg: &config::Config) -> Self {
        let rgb = |c: [u8; 3]| egui::Color32::from_rgb(c[0], c[1], c[2]);
        Self {
            mode: config::WaveformColorMode::from_key(&cfg.waveform_color_mode),
            height_exp: cfg.waveform_height_exp,
            band_gain: cfg.waveform_band_gain,
            energy_gain: cfg.waveform_energy_gain,
            band_colors: [
                rgb(cfg.waveform_band_colors[0]),
                rgb(cfg.waveform_band_colors[1]),
                rgb(cfg.waveform_band_colors[2]),
            ],
            energy_colors: [
                rgb(cfg.waveform_energy_colors[0]),
                rgb(cfg.waveform_energy_colors[1]),
                rgb(cfg.waveform_energy_colors[2]),
                rgb(cfg.waveform_energy_colors[3]),
                rgb(cfg.waveform_energy_colors[4]),
            ],
            smoothing: cfg.waveform_smoothing.clamp(0.0, 1.0),
            attack_secs: (cfg.waveform_smooth_attack_ms / 1000.0).clamp(0.0, 0.1),
            release_secs: (cfg.waveform_smooth_release_ms / 1000.0).clamp(0.0, 2.0),
            bass_floor_threshold: cfg.waveform_bass_floor_threshold.clamp(0.0, 1.0),
            bass_floor_amount: cfg.waveform_bass_floor_amount.clamp(0.0, 1.0),
        }
    }
}

/// Shape a normalized band height `[0,1]` by `height_exp` so loud sections don't
/// max out. The stored band byte is already sqrt-companded amplitude
/// (`color_bands` lifts quiet detail toward the top), so loud sections slam the
/// ceiling and lose internal contrast. Raising the normalized height by an
/// exponent (>1) walks it back toward linear amplitude: the bulk of the waveform
/// drops off the ceiling and regains dynamics (rekordbox-style), while the single
/// loudest moment still reaches full height. `1.0` = stored sqrt as-is (most
/// compressed); `2.0` exactly cancels the sqrt → linear amplitude (least
/// compressed, rekordbox-like).
fn wave_height(v: f32, height_exp: f32) -> f32 {
    v.clamp(0.0, 1.0).powf(height_exp)
}

/// Visualization-only height multiplier for the bass band. A kick's sharp attack
/// peaks loud, but the sub under it lingers quietly after the transient; both read
/// as bass and clutter the lane. `low_norm` is the band's normalized amplitude
/// `[0, 1]`. Content at/above `threshold` (the kick transient) keeps full height
/// (gain `1.0`); content well below it (sustained sub) is scaled toward
/// `1 - amount`. A soft knee below the threshold ramps between the two so the cut
/// reads as a gentle dip rather than a hard horizontal clip line. `amount` `0`
/// disables (always `1.0`).
fn bass_floor_gain(low_norm: f32, threshold: f32, amount: f32) -> f32 {
    if amount <= 0.0 {
        return 1.0;
    }
    // Soft knee width below the threshold; the gain ramps from `1 - amount` at
    // `threshold - KNEE` up to `1.0` at `threshold`.
    const KNEE: f32 = 0.18;
    let t = ((low_norm - (threshold - KNEE)) / KNEE).clamp(0.0, 1.0);
    (1.0 - amount) + amount * t
}

/// Slope-aware smoothing of the *source* envelope, run once over the stored bins
/// before anything is sampled for the screen — so the smoothing is anchored to the
/// audio, not to the on-screen bars. A given slider setting irons out the same
/// small details at every zoom; zooming out just reveals more of the already-
/// smoothed curve instead of re-smoothing it at the coarser bar scale (which is
/// what made the slider bite differently depending on how far in you were).
///
/// `data` is interleaved `stride`-byte samples (`[low, mid, high, loudness]` for
/// the band envelope, a single byte for the coarse fallback); each channel is
/// smoothed independently. A symmetric box blur would round the leading edge of a
/// kick as hard as its tail, mushing the transients that make a waveform readable,
/// so this runs a one-pole follower whose coefficient depends on the local slope:
///
/// * **Rising** samples — the front of a transient — track with a fast constant
///   (`style.attack_secs`) so the edge stays reasonably crisp.
/// * **Falling** samples — the tail — smooth with a far slower constant
///   (`style.release_secs`) so the decay reads as a clean ring-out.
///
/// The follower's reach is defined in *seconds of audio* (the style's
/// attack/release constants, from Settings → Waveform) and converted to per-bin
/// coefficients via `bins_per_sec`, so the coarse ~20/sec preview and the
/// ~2000/sec hi-res envelope smooth over the same span of *time* — a per-bin
/// coefficient that rounded the hi-res envelope nicely would smear the preview
/// across a minute. The default release span is beat-scale on purpose: a tail
/// must survive to the next kick or every beat pinches back to the centerline
/// and the lane reads as a row of separate petals (the "ripple") instead of a
/// connected silhouette.
///
/// `style.smoothing` `[0, 1]` scales the time constants linearly from `0` (raw)
/// to the full spans — time-space scaling, because scaling the *coefficients*
/// toward 1.0 left the slider perceptually dead until the very top. O(n) per
/// channel.
fn smooth_source(data: &[u8], stride: usize, style: &WaveformStyle, bins_per_sec: f32) -> Vec<u8> {
    let mut out = data.to_vec();
    let n = data.len() / stride;
    let s = style.smoothing.clamp(0.0, 1.0);
    if s == 0.0 || n == 0 || bins_per_sec <= 0.0 {
        return out;
    }
    // One-pole coefficient for a time constant of `tau_secs * s`: after that span
    // the follower has covered ~63% of a step. `exp` keeps it exact even when the
    // constant spans less than one bin (coarse preview), degrading to ~raw.
    let alpha = |tau_secs: f32| {
        let tau_bins = tau_secs * s * bins_per_sec;
        if tau_bins <= f32::EPSILON {
            1.0
        } else {
            1.0 - (-1.0 / tau_bins).exp()
        }
    };
    let attack = alpha(style.attack_secs);
    let release = alpha(style.release_secs);
    for k in 0..stride {
        let mut prev = data[k] as f32;
        for i in 1..n {
            let x = data[stride * i + k] as f32;
            let alpha = if x >= prev { attack } else { release };
            prev += alpha * (x - prev);
            out[stride * i + k] = prev.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// Default span (seconds) shown in the zoomed detail lane.
pub(crate) const DEFAULT_ZOOM_SECS: f32 = 16.0;
/// Tightest zoom. The zoom lane draws the [`HIRES_BINS_PER_SEC`] envelope, so this
/// is the point where its bins stretch past ~1 per pixel and there's no more
/// detail to reveal — half a second is roughly a single beat, transients spread
/// right across the lane.
const MIN_ZOOM_SECS: f32 = 0.5;
/// Widest zoom before the lane is essentially the full-track overview again.
const MAX_ZOOM_SECS: f32 = 90.0;

/// Default pixel height of the moving zoomed detail lane.
pub(crate) const DEFAULT_LANE_H: f32 = 46.0;
/// Shortest the lane can be dragged.
const MIN_LANE_H: f32 = 46.0;
/// Tallest the lane can be dragged (keeps the lane from swallowing the screen).
const MAX_LANE_H: f32 = 460.0;
/// Base height of the player panel excluding the resizable lane (artwork/controls
/// row plus the spacing around the lane). The grip floats over the lane and the
/// smoothing slider moved to Settings, so neither reserves a row here any more.
/// Panel height = this + the lane height.
const PANEL_BASE_H: f32 = 100.0;

/// Buckets per second for the high-res zoom envelope. ~100× the stored preview's
/// ~20/sec — well past rekordbox's detailed waveform, so even the tightest
/// [`MIN_ZOOM_SECS`] view stays ~1 bin/pixel and resolves individual transients.
/// Cost is a one-off pass over the PCM; memory is ~`secs * this * 4` bytes/track
/// (~5 MB for a 10-min track), freed when the track changes.
const HIRES_BINS_PER_SEC: f32 = 2000.0;

/// Transient sharpening strength for the hi-res envelope (see [`sharpen_peaks`]).
/// A percussive hit's decay bleeds into the bins after the attack, so a kick or
/// snare reads as a rounded hump. An unsharp mask pushes each band's peak up where
/// it stands above its local neighbors and eases the shoulders down, so the attack
/// rises to a sharper point. `0` disables; higher is pointier.
const TRANSIENT_SHARPEN: f32 = 0.85;
/// Transient sharpening for the **low** band specifically. Nudged above the shared
/// [`TRANSIENT_SHARPEN`] so a kick's attack spikes harder and its sustained tail —
/// lingering sub/bass — gets pulled down faster: the unsharp mask lifts the peak
/// where it stands above the local mean and eases the shoulders down more, which
/// reads as a sharper attack with a faster release than the mid/high bands.
const LOW_TRANSIENT_SHARPEN: f32 = 1.15;
/// Half-window (in hi-res bins) of the unsharp-mask reference blur. At
/// [`HIRES_BINS_PER_SEC`] this spans ~3 ms each side — the timescale of a
/// percussive attack's shoulders, so the blur tracks the hump the peak sits on
/// without reaching across to neighboring hits.
const SHARPEN_RADIUS: usize = 6;

/// Sharpen a band's per-bin peak track in place with an unsharp mask:
/// `out = peak + amount·(peak − local_mean)`, clamped at 0. Where a bin stands
/// above the local mean (a transient attack) it's pushed higher and its lower
/// shoulders are pulled down, so the peak narrows to a point; flat sustained
/// sections (peak ≈ mean) are left essentially unchanged. The reference mean is an
/// O(n) prefix-sum moving average of half-window `radius`.
fn sharpen_peaks(peaks: &mut [f32], radius: usize, amount: f32) {
    let n = peaks.len();
    if n == 0 || radius == 0 || amount <= 0.0 {
        return;
    }
    let mut prefix = vec![0f32; n + 1];
    for i in 0..n {
        prefix[i + 1] = prefix[i] + peaks[i];
    }
    let mut out = vec![0f32; n];
    for i in 0..n {
        let lo = i.saturating_sub(radius);
        let hi = (i + radius + 1).min(n);
        let mean = (prefix[hi] - prefix[lo]) / (hi - lo) as f32;
        out[i] = (peaks[i] + amount * (peaks[i] - mean)).max(0.0);
    }
    peaks.copy_from_slice(&out);
}

/// Build a high-resolution `[low, mid, high, loudness]` band envelope (4 bytes per
/// bucket — the same layout as core `color_bands`/`waveform_bands`, so the normal
/// [`draw_waveform`] renders it unchanged) directly from the decoded PCM. One
/// streaming pass mixes to mono, splits it into three bands with one-pole filters,
/// and records each bucket's per-band peak plus its RMS loudness. At
/// [`HIRES_BINS_PER_SEC`] the zoom lane resolves individual transients; the column
/// view keeps scaling down the coarse stored preview (it's only a few px tall).
pub(crate) fn compute_hires_bands(
    samples: &[f32],
    channels: u16,
    sample_rate: u32,
    low_hz: f32,
    mid_hz: f32,
) -> Vec<u8> {
    let ch = channels.max(1) as usize;
    let total_frames = samples.len() / ch;
    if total_frames == 0 || sample_rate == 0 {
        return Vec::new();
    }
    let sr = sample_rate as f32;
    let secs = total_frames as f32 / sr;
    let bins = ((secs * HIRES_BINS_PER_SEC).ceil() as usize)
        .max(1)
        .min(total_frames);

    // One-pole low-pass coefficients (a = 1 - e^{-2π fc/sr}). The `low_hz` pole peels
    // off the lows; the `mid_hz` pole peels off everything below the highs; the gap
    // between the two poles is the mid band. The low pole defaults to 120 Hz (kick
    // fundamental + sub) so low-mid energy that isn't part of a DJ's kick/bass cue
    // stays out of the low band; both are settable from the waveform settings.
    let low_hz = low_hz.clamp(20.0, sr * 0.5);
    let mid_hz = mid_hz.clamp(low_hz, sr * 0.5);
    let a_low = 1.0 - (-std::f32::consts::TAU * low_hz / sr).exp();
    let a_mid = 1.0 - (-std::f32::consts::TAU * mid_hz / sr).exp();

    let mut peak_lo = vec![0f32; bins];
    let mut peak_md = vec![0f32; bins];
    let mut peak_hi = vec![0f32; bins];
    let mut sumsq = vec![0f32; bins];
    let mut count = vec![0u32; bins];

    let (mut lp_low, mut lp_mid) = (0f32, 0f32);
    for f in 0..total_frames {
        let base = f * ch;
        let mut s = 0.0;
        for c in 0..ch {
            s += samples[base + c];
        }
        s /= ch as f32;

        lp_low += a_low * (s - lp_low);
        lp_mid += a_mid * (s - lp_mid);
        let lo = lp_low;
        let md = lp_mid - lp_low;
        let hi = s - lp_mid;

        let b = (((f as u64) * bins as u64) / total_frames as u64) as usize;
        let b = b.min(bins - 1);
        peak_lo[b] = peak_lo[b].max(lo.abs());
        peak_md[b] = peak_md[b].max(md.abs());
        peak_hi[b] = peak_hi[b].max(hi.abs());
        sumsq[b] += s * s;
        count[b] += 1;
    }

    // Sharpen each band's peak track so percussive attacks read as pointed spikes
    // rather than the rounded humps their decay would otherwise smear them into.
    sharpen_peaks(&mut peak_lo, SHARPEN_RADIUS, LOW_TRANSIENT_SHARPEN);
    sharpen_peaks(&mut peak_md, SHARPEN_RADIUS, TRANSIENT_SHARPEN);
    sharpen_peaks(&mut peak_hi, SHARPEN_RADIUS, TRANSIENT_SHARPEN);

    // Quantize to bytes with sqrt companding — lifts the quiet detail off the floor
    // the same way core's preview does, so the existing render gains look right.
    let q = |v: f32| (v.clamp(0.0, 1.0).sqrt() * 255.0) as u8;
    let mut out = vec![0u8; bins * 4];
    for i in 0..bins {
        let rms = if count[i] > 0 {
            (sumsq[i] / count[i] as f32).sqrt()
        } else {
            0.0
        };
        out[i * 4] = q(peak_lo[i]);
        out[i * 4 + 1] = q(peak_md[i]);
        out[i * 4 + 2] = q(peak_hi[i]);
        out[i * 4 + 3] = q(rms);
    }
    out
}

/// Paint a colored waveform: one vertical bar per screen column. With
/// `played_frac = Some(f)` the portion left of `f` is full brightness and the
/// rest is dimmed (the player's playhead); `None` paints every bar full
/// brightness (table cells, no playhead).
///
/// `window` is the visible span in track-fraction `(start, end)`: `(0.0, 1.0)`
/// fills `rect` with the whole track (overview strip and table cells); a narrow
/// window like `(0.40, 0.55)` zooms into that slice, so the same `rect` shows
/// fewer bins stretched wider — the zoomed detail lane. `played_frac` stays in
/// whole-track fraction regardless of the window.
///
/// In **spectrum** mode each of the three bands (`[low, mid, high]`, RMS heights
/// from core `color_bands`) is drawn as its own waveform in a fixed back-to-front
/// order (low body behind, highs overlaid in the centre) — bass shows big, a
/// hi-hat shows as a small high-band spike. In **energy** mode a single envelope (the
/// loudest band) is colored by the energy byte (K-weighted loudness × spectral
/// occupancy, see core `color_bands`). Without band data both fall back to the
/// peak envelope (`waveform`) on a height ramp.
pub(crate) fn draw_waveform(
    painter: &egui::Painter,
    rect: egui::Rect,
    waveform: &[u8],
    bands: &[u8],
    style: &WaveformStyle,
    played_frac: Option<f32>,
    window: (f32, f32),
) {
    // Bands are `[low, mid, high, loudness]` per bin (see core `color_bands`).
    const STRIDE: usize = 4;
    let nb = bands.len() / STRIDE;
    let has_bands = nb > 0 && bands.len() % STRIDE == 0;
    let n = waveform.len();

    // Smooth the source envelope once, up front, so the slider irons out the same
    // small details at every zoom (see `smooth_source`). Everything below samples
    // these smoothed bins; sampling raw and blending the drawn bars instead tied
    // the smoothing to the zoom.
    // Both inputs here are the stored preview envelopes, so the time constants
    // convert through the preview's fixed bin rate.
    let rate = analysis::waveform::COLOR_BINS_PER_SEC;
    let sbands = smooth_source(bands, STRIDE, style, rate);
    let bands = sbands.as_slice();
    let swave = smooth_source(waveform, 1, style, rate);
    let waveform = swave.as_slice();

    let y = rect.center().y;
    let (x0, x1) = (rect.left(), rect.right());
    let half = rect.height() / 2.0 - 1.0;
    let cols = (x1 - x0).floor().max(1.0) as usize;

    // Accumulate every bar into a single mesh and emit one draw call. Painting
    // each column (×3 bands in Spectrum mode) as its own `line_segment` shape
    // tessellates and anti-alias-feathers thousands of separate shapes every
    // frame — the dominant per-frame cost, felt as choppy framerate whenever the
    // whole UI repaints (e.g. scrolling the Settings window over the player bar).
    // One mesh of axis-aligned colored rects is a single primitive, already
    // triangulated, with no per-shape overhead.
    let mut mesh = egui::epaint::Mesh::default();
    // Columns are 1px wide and their peaks are joined into a filled envelope below
    // (see `fill_envelope`), so the waveform reads as one continuous silhouette.
    // Visible track-fraction span. `(0, 1)` is the whole track; a narrower window
    // stretches its slice across `rect` (the zoom lane). The column→track-fraction
    // map below routes through it, so the bin sampling and the played/dimmed split
    // both follow the zoom with no other changes.
    let (w0, w1) = window;
    let wspan = (w1 - w0).max(f32::EPSILON);

    // How many stored bins fall under one pixel column. Below 1 a single bin is
    // stretched across multiple pixels (zoomed in past the stored resolution):
    // point-sampling would stair-step, so there we interpolate between adjacent
    // bins for a smooth ramp. When more bins than pixels (zoomed out / overview /
    // table cells) we take the peak-preserving MAX across the column's bin span.
    let src_bins = if has_bands { nb } else { n };
    let bins_per_col = src_bins as f32 * wspan / cols as f32;
    let smooth = bins_per_col < 1.0;

    // First pass: reduce each pixel column to its band aggregate (or, without band
    // data, its peak amplitude packed into channel 0). The second pass blurs that
    // bar sequence by the smoothing radius and draws — so the slider blends each
    // bar with its neighbors (rekordbox's continuous envelope) without changing how
    // any single column is sampled.
    let mut col_agg: Vec<[u8; 4]> = Vec::with_capacity(cols);
    for cx in 0..cols {
        let frac = (w0 + (cx as f32 + 0.5) / cols as f32 * wspan).clamp(0.0, 1.0);
        if has_bands {
            // Map this pixel column to its span of band bins. Zoomed out we take the
            // per-band MAX across the span (peak-preserving — fine transients show as
            // thin spikes instead of being sampled away). Zoomed in (one bin spread
            // over several pixels) we instead lerp between the two nearest bins so the
            // envelope ramps smoothly between samples rather than stair-stepping.
            let agg = if smooth {
                let fpos = (frac * nb as f32 - 0.5).clamp(0.0, (nb - 1) as f32);
                let i0 = fpos.floor() as usize;
                let i1 = (i0 + 1).min(nb - 1);
                let t = fpos - i0 as f32;
                let q0 = &bands[STRIDE * i0..STRIDE * i0 + 4];
                let q1 = &bands[STRIDE * i1..STRIDE * i1 + 4];
                let mut agg = [0u8; 4];
                for k in 0..4 {
                    agg[k] = (q0[k] as f32 + (q1[k] as f32 - q0[k] as f32) * t).round() as u8;
                }
                agg
            } else {
                let tf_lo = (w0 + (cx as f32 / cols as f32) * wspan).clamp(0.0, 1.0);
                let tf_hi = (w0 + ((cx + 1) as f32 / cols as f32) * wspan).clamp(0.0, 1.0);
                let b0 = ((tf_lo * nb as f32) as usize).min(nb - 1);
                let b1 = ((tf_hi * nb as f32).ceil() as usize).clamp(b0 + 1, nb);
                let mut agg = [0u8; 4];
                for j in b0..b1 {
                    let q = &bands[STRIDE * j..STRIDE * j + 4];
                    for t in 0..4 {
                        agg[t] = agg[t].max(q[t]);
                    }
                }
                agg
            };
            col_agg.push(agg);
        } else if n > 0 {
            // No band data: peak envelope on a height ramp. Lerp between samples when
            // zoomed in so it tapers smoothly instead of stepping. Pack the amplitude
            // byte into channel 0 so it rides through the same smoothing pass.
            let amp = if smooth {
                let fpos = (frac * n as f32 - 0.5).clamp(0.0, (n - 1) as f32);
                let i0 = fpos.floor() as usize;
                let i1 = (i0 + 1).min(n - 1);
                let t = fpos - i0 as f32;
                waveform[i0] as f32 + (waveform[i1] as f32 - waveform[i0] as f32) * t
            } else {
                let i = ((frac * n as f32) as usize).min(n - 1);
                waveform[i] as f32
            };
            col_agg.push([amp.round().clamp(0.0, 255.0) as u8, 0, 0, 0]);
        } else {
            col_agg.push([0; 4]);
        }
    }

    // Smoothing already happened in bin space (`smooth_source`); connect the
    // per-column heights into a filled, centre-mirrored envelope so the peaks join
    // into a continuous silhouette (rekordbox's look) rather than separated bars.
    let played_at = |cx: usize| {
        let frac = (w0 + (cx as f32 + 0.5) / cols as f32 * wspan).clamp(0.0, 1.0);
        played_frac.map_or(true, |p| frac <= p)
    };
    let lit = |c: egui::Color32, played: bool| if played { c } else { dim(c, 0.4) };

    if has_bands {
        match style.mode {
            config::WaveformColorMode::Spectrum => {
                // One filled strip per band in a fixed back-to-front order (low body,
                // highs overlaid in the centre). Sorting by per-column height instead
                // made whichever band was shortest sit on top, and that flips column
                // to column — the "tiger stripes"; a fixed z-order keeps the centre
                // colour consistent between neighbours.
                for b in 0..3 {
                    let mut layer: Vec<(f32, f32, egui::Color32)> = Vec::with_capacity(cols);
                    for (cx, a) in col_agg.iter().enumerate() {
                        let mut hh =
                            wave_height(a[b] as f32 / 255.0, style.height_exp) * style.band_gain[b];
                        if b == 0 {
                            hh *= bass_floor_gain(
                                a[0] as f32 / 255.0,
                                style.bass_floor_threshold,
                                style.bass_floor_amount,
                            );
                        }
                        let h = (hh.min(1.0) * half).max(0.4);
                        layer.push((x0 + cx as f32, h, lit(style.band_colors[b], played_at(cx))));
                    }
                    fill_envelope(&mut mesh, &layer, y);
                }
            }
            config::WaveformColorMode::Energy => {
                // Envelope = loudest band; colour = the hybrid energy byte.
                let mut layer: Vec<(f32, f32, egui::Color32)> = Vec::with_capacity(cols);
                for (cx, a) in col_agg.iter().enumerate() {
                    let env = a[0].max(a[1]).max(a[2]) as f32 / 255.0;
                    let loud = a[3] as f32 / 255.0;
                    let h = ((wave_height(env, style.height_exp) * style.energy_gain).min(1.0)
                        * half)
                        .max(0.5);
                    let c = energy_color(energy_curve(loud), &style.energy_colors);
                    layer.push((x0 + cx as f32, h, lit(c, played_at(cx))));
                }
                fill_envelope(&mut mesh, &layer, y);
            }
        }
    } else if n > 0 {
        let mut layer: Vec<(f32, f32, egui::Color32)> = Vec::with_capacity(cols);
        for (cx, a) in col_agg.iter().enumerate() {
            let amp = a[0] as f32 / 255.0;
            let h = ((amp * style.energy_gain).min(1.0) * half).max(1.0);
            let c = energy_color(energy_curve(amp), &style.energy_colors);
            layer.push((x0 + cx as f32, h, lit(c, played_at(cx))));
        }
        fill_envelope(&mut mesh, &layer, y);
    }

    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Like [`draw_waveform`] but for the *moving* zoom lane: each bar is anchored to
/// its absolute position in the track and placed at a sub-pixel x, so as the
/// window scrolls under the playhead the whole waveform glides continuously
/// instead of snapping a whole bin at a time. (`draw_waveform` samples fixed pixel
/// columns tied to the window's left edge — right for the static overview/table,
/// but it stair-steps the content while scrolling, which reads as choppy even at
/// full frame rate.) Spectrum/energy drawing matches `draw_waveform`; callers fall
/// back to it when only the coarse envelope (no band data) is available.
fn draw_waveform_scrolling(
    painter: &egui::Painter,
    rect: egui::Rect,
    bands: &[u8],
    bins_per_sec: f32,
    style: &WaveformStyle,
    played_frac: f32,
    window: (f32, f32),
) {
    const STRIDE: usize = 4;
    let nb = bands.len() / STRIDE;
    if nb == 0 || bands.len() % STRIDE != 0 {
        return;
    }
    // Smooth the source bins once, up front, so the slider irons out the same small
    // details no matter how far the lane is zoomed (see `smooth_source`); the bars
    // below are sampled from these smoothed bins with no further per-bar blend.
    // `bins_per_sec` is the caller's actual envelope rate (hi-res or the coarse
    // preview fallback), so the smoothing spans the same time either way.
    let sbands = smooth_source(bands, STRIDE, style, bins_per_sec);
    let bands = sbands.as_slice();
    let y = rect.center().y;
    let x0 = rect.left();
    let half = rect.height() / 2.0 - 1.0;
    let width = rect.width().max(1.0);
    let (w0, w1) = window;
    let wspan = (w1 - w0).max(f32::EPSILON);

    // Visible span measured in bins, and how many screen pixels one bin spans.
    let vis_bins = wspan * nb as f32;
    let px_per_bin = width / vis_bins.max(f32::EPSILON);

    // Group bins into bars ~1.4 px apart when zoomed out (one bar per bin once a
    // bin is wider than that). The groups sit on a fixed absolute bin grid, so a
    // given bar keeps its identity and just translates left as the window scrolls
    // — that's what makes the motion continuous rather than stepped. Sample one
    // point per pixel when a bin is at least a pixel wide (zoomed in), else one
    // point per group of bins.
    let group = (1.4 / px_per_bin).ceil().max(1.0) as i64;
    let gf = group as f32;
    let solid = px_per_bin >= 1.0;

    let mut mesh = egui::epaint::Mesh::default();

    // First pass: walk the absolute bin grid from the group at/just before the
    // left edge to just past the right edge, reducing each group to one point
    // (centre x + band aggregate). The points come out ordered along x, so the
    // second pass can join their peaks into a filled envelope (`fill_envelope`) —
    // one continuous silhouette — without disturbing the scroll-stable bin grid.
    let mut bars: Vec<(f32, bool, [u8; 4])> = Vec::new();
    if solid {
        // Zoomed in past one bin per pixel. Drawing one flat-topped rectangle per
        // bin (each `bin_w_px` wide) reads as a blocky staircase — adjacent bins
        // jump to their own height with no ramp between them. Instead sample one
        // bar per screen pixel and lerp the band aggregate between the two nearest
        // bins, so the envelope ramps continuously: the most-zoomed-in view is the
        // smooth baseline, and wider zooms scale down from it via the MAX grouping
        // below. Pixel columns map through the absolute window (`w0`), so the curve
        // still glides as the lane scrolls.
        let cols = width.ceil().max(1.0) as usize;
        for cx in 0..cols {
            let center_frac = w0 + (cx as f32 + 0.5) / cols as f32 * wspan;
            let fpos = (center_frac * nb as f32 - 0.5).clamp(0.0, (nb - 1) as f32);
            let i0 = fpos.floor() as usize;
            let i1 = (i0 + 1).min(nb - 1);
            let t = fpos - i0 as f32;
            let q0 = &bands[STRIDE * i0..STRIDE * i0 + 4];
            let q1 = &bands[STRIDE * i1..STRIDE * i1 + 4];
            let mut agg = [0u8; 4];
            for k in 0..4 {
                agg[k] = (q0[k] as f32 + (q1[k] as f32 - q0[k] as f32) * t).round() as u8;
            }
            let played = center_frac <= played_frac;
            bars.push((x0 + cx as f32 + 0.5, played, agg));
        }
    } else {
        let bf0 = (w0 * nb as f32) as i64;
        let mut gb = bf0 - bf0.rem_euclid(group);
        let stop = w1 * nb as f32 + gf;
        while (gb as f32) < stop {
            let b0 = gb.clamp(0, nb as i64) as usize;
            let b1 = (gb + group).clamp(0, nb as i64) as usize;
            if b1 > b0 {
                let mut agg = [0u8; 4];
                for j in b0..b1 {
                    let q = &bands[STRIDE * j..STRIDE * j + 4];
                    for t in 0..4 {
                        agg[t] = agg[t].max(q[t]);
                    }
                }
                let center_frac = (gb as f32 + gf * 0.5) / nb as f32;
                let x_center = x0 + (center_frac - w0) / wspan * width;
                let played = center_frac <= played_frac;
                bars.push((x_center, played, agg));
            }
            gb += group;
        }
    }

    // Smoothing already happened in bin space (`smooth_source`); join the points'
    // peaks into a filled, centre-mirrored envelope (`fill_envelope`) so the lane
    // reads as one continuous silhouette instead of separated bars.
    let lit = |c: egui::Color32, played: bool| if played { c } else { dim(c, 0.4) };
    match style.mode {
        config::WaveformColorMode::Spectrum => {
            // One strip per band in a fixed back-to-front order (low body, highs
            // overlaid in the centre) so the centre colour stays consistent between
            // neighbours instead of flipping to the shortest band ("tiger stripes").
            // See the matching note in `draw_waveform`.
            for b in 0..3 {
                let mut layer: Vec<(f32, f32, egui::Color32)> = Vec::with_capacity(bars.len());
                for &(x, played, a) in bars.iter() {
                    let mut hh =
                        wave_height(a[b] as f32 / 255.0, style.height_exp) * style.band_gain[b];
                    if b == 0 {
                        hh *= bass_floor_gain(
                            a[0] as f32 / 255.0,
                            style.bass_floor_threshold,
                            style.bass_floor_amount,
                        );
                    }
                    let h = (hh.min(1.0) * half).max(0.4);
                    layer.push((x, h, lit(style.band_colors[b], played)));
                }
                fill_envelope(&mut mesh, &layer, y);
            }
        }
        config::WaveformColorMode::Energy => {
            let mut layer: Vec<(f32, f32, egui::Color32)> = Vec::with_capacity(bars.len());
            for &(x, played, a) in bars.iter() {
                let env = a[0].max(a[1]).max(a[2]) as f32 / 255.0;
                let loud = a[3] as f32 / 255.0;
                let h = ((wave_height(env, style.height_exp) * style.energy_gain).min(1.0) * half)
                    .max(0.5);
                let c = energy_color(energy_curve(loud), &style.energy_colors);
                layer.push((x, h, lit(c, played)));
            }
            fill_envelope(&mut mesh, &layer, y);
        }
    }

    if !mesh.is_empty() {
        // Clip to the lane: the grid overshoots both edges by up to one group.
        painter.with_clip_rect(rect).add(egui::Shape::mesh(mesh));
    }
}

/// Raise the threshold for reading as "high energy". Analyzer v15+ stores the
/// energy byte as the cube root of the loudness × spectral-occupancy hybrid
/// (see core `color_bands`), so this cube reconstructs the intended curve. For
/// older cached loudness-only bytes (and the hires lane's RMS byte) it doubles
/// as the contrast fix: compressed music lives within a few dB of its peak and
/// would otherwise map almost entirely to the hot end; the gamma pushes the
/// bulk down into the cool/mid range so only sections near the track's loudest
/// moment read hot.
fn energy_curve(t: f32) -> f32 {
    t.clamp(0.0, 1.0).powf(3.0)
}

/// Cool→hot gradient for the energy color mode: five `colors` stops (quiet →
/// loudest) interpolated at the fixed positions below. Defaults are deep blue →
/// teal → green → amber → red, but the Waveform settings tab can recolor them.
/// `t` is clamped to `[0, 1]`.
fn energy_color(t: f32, colors: &[egui::Color32; 5]) -> egui::Color32 {
    const POS: [f32; 5] = [0.0, 0.3, 0.55, 0.8, 1.0];
    let t = t.clamp(0.0, 1.0);
    let (mut lo, mut hi) = (0usize, POS.len() - 1);
    for i in 0..POS.len() - 1 {
        if t >= POS[i] && t <= POS[i + 1] {
            lo = i;
            hi = i + 1;
            break;
        }
    }
    let span = (POS[hi] - POS[lo]).max(1e-6);
    let f = ((t - POS[lo]) / span).clamp(0.0, 1.0);
    let lerp = |a: u8, b: u8| (a as f32 + (b as f32 - a as f32) * f) as u8;
    egui::Color32::from_rgb(
        lerp(colors[lo].r(), colors[hi].r()),
        lerp(colors[lo].g(), colors[hi].g()),
        lerp(colors[lo].b(), colors[hi].b()),
    )
}

/// Scale a color toward black by `f` (used to dim the not-yet-played portion).
fn dim(c: egui::Color32, f: f32) -> egui::Color32 {
    egui::Color32::from_rgb(
        (c.r() as f32 * f) as u8,
        (c.g() as f32 * f) as u8,
        (c.b() as f32 * f) as u8,
    )
}

/// Push a single flat-shaded quad (two triangles) into the mesh.
fn add_quad(
    mesh: &mut egui::epaint::Mesh,
    a: egui::Pos2,
    b: egui::Pos2,
    c: egui::Pos2,
    d: egui::Pos2,
    color: egui::Color32,
) {
    let i = mesh.vertices.len() as u32;
    mesh.colored_vertex(a, color);
    mesh.colored_vertex(b, color);
    mesh.colored_vertex(c, color);
    mesh.colored_vertex(d, color);
    mesh.add_triangle(i, i + 1, i + 2);
    mesh.add_triangle(i, i + 2, i + 3);
}

/// Draw one waveform layer as a *filled, centre-mirrored envelope* instead of
/// separate bars: consecutive points are joined by a quad running from the top
/// envelope (`y - h`) down to the bottom (`y + h`), so the peaks connect into a
/// continuous silhouette the way rekordbox fills between the waves. Where the
/// height dips toward zero between cycles the fill pinches to the centre line, so
/// quiet passages still read as separated "leaves" rather than one solid block.
/// Each point is `(x, height, color)`; the segment takes its left point's colour
/// (which carries the played/dimmed split).
fn fill_envelope(mesh: &mut egui::epaint::Mesh, pts: &[(f32, f32, egui::Color32)], y: f32) {
    for w in pts.windows(2) {
        let (x0, h0, c) = w[0];
        let (x1, h1, _) = w[1];
        add_quad(
            mesh,
            egui::pos2(x0, y - h0),
            egui::pos2(x1, y - h1),
            egui::pos2(x1, y + h1),
            egui::pos2(x0, y + h0),
            c,
        );
    }
}
