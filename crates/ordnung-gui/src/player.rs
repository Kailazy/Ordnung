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
            .map(|r| (r.artist.clone(), r.title.clone()));
        if let Some(a) = self.audio.as_mut() {
            a.play_or_toggle(id, path.clone());
        }
        // Only (re)seed the bar when switching to a different track; a same-track
        // click is just a pause/resume and keeps the existing display + scrub.
        if !toggling {
            let (artist, title) = display.unwrap_or_else(|| {
                let stem = path
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                (String::new(), stem)
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
            let (waveform, waveform_bands) = Catalog::open(&self.db_path)
                .and_then(|c| c.get_analysis(id))
                .ok()
                .flatten()
                .map(|a| {
                    // Only the v11+ 4-byte stride is what the renderer expects.
                    let bands = if a.analyzer_version >= 11 {
                        a.waveform_bands
                    } else {
                        Vec::new()
                    };
                    (a.waveform_preview, bands)
                })
                .unwrap_or_default();
            self.now_playing = Some(NowPlaying {
                id,
                artist,
                title,
                source_path,
                waveform,
                waveform_bands,
                hires_bands: None,
                hires_requested: false,
            });
            self.scrub = None;
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
                thread::spawn(move || {
                    let hires = compute_hires_bands(&samples, ch, sr);
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
        // Clone the waveform out so the panel closure (which mutably borrows
        // `self.scrub`) doesn't also need to borrow `self.now_playing`.
        let waveform = np.waveform.clone();
        let bands = np.waveform_bands.clone();
        // High-res bands for the zoom lane; fall back to the coarse preview until
        // the PCM has been analyzed (or for tracks the engine never decoded).
        let hires = np.hires_bands.clone().unwrap_or_default();
        let wave_style = WaveformStyle::from_config(&self.config);

        const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
        let mut toggle = false;
        let mut close = false;
        let mut seek_to: Option<f32> = None;

        // Fraction the bar reflects: the live position, or the in-progress scrub
        // before release. Shared by the zoom lane and the overview strip so both
        // track the same playhead. Computed once up front (the panel closure below
        // mutably borrows `self.scrub`).
        let shown_frac = self
            .scrub
            .unwrap_or(if dur > 0.0 { pos / dur } else { 0.0 })
            .clamp(0.0, 1.0);

        egui::TopBottomPanel::bottom("player")
            .exact_height(150.0)
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
                    let mut animating = draw_scrolling_line(
                        ui,
                        egui::pos2(block.left(), block.top() + 8.0),
                        LABEL_W,
                        &title,
                        egui::FontId::proportional(14.0),
                        egui::Color32::from_gray(240),
                        now,
                    );
                    animating |= draw_scrolling_line(
                        ui,
                        egui::pos2(block.left(), block.top() + 32.0),
                        LABEL_W,
                        &artist,
                        egui::FontId::proportional(12.0),
                        egui::Color32::from_gray(165),
                        now,
                    );
                    if animating {
                        ui.ctx().request_repaint();
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
                    ui.add_space(14.0);

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
        if close {
            if let Some(a) = self.audio.as_mut() {
                a.stop();
            }
            self.now_playing = None;
            self.scrub = None;
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
        seek_to: &mut Option<f32>,
    ) {
        const MARGIN: f32 = 10.0;
        let zoom = self
            .wave_zoom_secs
            .clamp(MIN_ZOOM_SECS, MAX_ZOOM_SECS);
        let lane_w = (ui.available_width() - 2.0 * MARGIN).max(60.0);
        ui.horizontal(|ui| {
            ui.add_space(MARGIN);
            let (rect, resp) = ui.allocate_exact_size(
                egui::vec2(lane_w, 46.0),
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
            draw_waveform(
                painter,
                rect.shrink2(egui::vec2(3.0, 4.0)),
                waveform,
                bands,
                wave_style,
                Some(shown_frac),
                (w0, w1),
            );

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

/// Paint one line of text left-aligned within `width`, clipped to it. If the
/// text is wider than `width`, scroll it horizontally Spotify-style: hold at the
/// start, glide left to reveal the tail, hold at the end, then loop. Returns
/// `true` while the line is animating so the caller can request a repaint.
fn draw_scrolling_line(
    ui: &egui::Ui,
    top_left: egui::Pos2,
    width: f32,
    text: &str,
    font: egui::FontId,
    color: egui::Color32,
    time: f64,
) -> bool {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), font, color);
    let size = galley.size();
    let clip = egui::Rect::from_min_size(top_left, egui::vec2(width, size.y));
    let painter = ui.painter_at(clip);
    if size.x <= width {
        painter.galley(top_left, galley, color);
        return false;
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
    true
}

pub(crate) fn fmt_duration(ms: u64) -> String {
    let secs = ms / 1000;
    format!("{}:{:02}", secs / 60, secs % 60)
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

/// Default span (seconds) shown in the zoomed detail lane.
pub(crate) const DEFAULT_ZOOM_SECS: f32 = 16.0;
/// Tightest zoom. The zoom lane draws the [`HIRES_BINS_PER_SEC`] envelope, so this
/// is the point where its bins stretch past ~1 per pixel and there's no more
/// detail to reveal — a couple of seconds is already a very tight, per-beat view.
const MIN_ZOOM_SECS: f32 = 2.0;
/// Widest zoom before the lane is essentially the full-track overview again.
const MAX_ZOOM_SECS: f32 = 90.0;

/// Buckets per second for the high-res zoom envelope. ~25× the stored preview's
/// ~20/sec — denser than rekordbox's detailed waveform, so even the tightest
/// [`MIN_ZOOM_SECS`] view stays ~1 bin/pixel and resolves individual transients.
/// Cost is a one-off pass over the PCM; memory is ~`secs * this * 4` bytes/track.
const HIRES_BINS_PER_SEC: f32 = 500.0;

/// Build a high-resolution `[low, mid, high, loudness]` band envelope (4 bytes per
/// bucket — the same layout as core `color_bands`/`waveform_bands`, so the normal
/// [`draw_waveform`] renders it unchanged) directly from the decoded PCM. One
/// streaming pass mixes to mono, splits it into three bands with one-pole filters,
/// and records each bucket's per-band peak plus its RMS loudness. At
/// [`HIRES_BINS_PER_SEC`] the zoom lane resolves individual transients; the column
/// view keeps scaling down the coarse stored preview (it's only a few px tall).
pub(crate) fn compute_hires_bands(samples: &[f32], channels: u16, sample_rate: u32) -> Vec<u8> {
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

    // One-pole low-pass coefficients (a = 1 - e^{-2π fc/sr}). The 250 Hz pole peels
    // off the lows; the 2.5 kHz pole peels off everything below the highs; the gap
    // between the two poles is the mid band.
    let a_low = 1.0 - (-std::f32::consts::TAU * 250.0 / sr).exp();
    let a_mid = 1.0 - (-std::f32::consts::TAU * 2500.0 / sr).exp();

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
/// from core `color_bands`) is drawn as its own waveform, overlaid tallest-first
/// so the shorter bands sit visibly in the centre — bass shows big, a hi-hat
/// shows as a small high-band spike. In **energy** mode a single envelope (the
/// loudest band) is colored by K-weighted loudness. Without band data both fall
/// back to the peak envelope (`waveform`) on a height ramp.
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
    // Each column is 1px wide; draw the bar a touch narrower and centered so a thin
    // gap separates neighboring bars (the rekordbox "individual sample bands" look)
    // instead of a solid fill.
    const BAR_W: f32 = 0.45;
    let pad = (1.0 - BAR_W) / 2.0;
    // A really light feather: a faint half-alpha strip on each side of the solid
    // bar softens the sub-pixel edges so neighboring bars alias gently into the gap
    // instead of hard-clipping (which shimmers as the mesh snaps to the pixel grid).
    const FEATHER_W: f32 = 0.15;
    let bar = |mesh: &mut egui::epaint::Mesh, x: f32, h: f32, played: bool, c: egui::Color32| {
        let c = if played { c } else { dim(c, 0.4) };
        let l = x + pad;
        let r = x + pad + BAR_W;
        mesh.add_colored_rect(
            egui::Rect::from_min_max(egui::pos2(l, y - h), egui::pos2(r, y + h)),
            c,
        );
        let edge = egui::Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), c.a() / 3);
        mesh.add_colored_rect(
            egui::Rect::from_min_max(egui::pos2(l - FEATHER_W, y - h), egui::pos2(l, y + h)),
            edge,
        );
        mesh.add_colored_rect(
            egui::Rect::from_min_max(egui::pos2(r, y - h), egui::pos2(r + FEATHER_W, y + h)),
            edge,
        );
    };

    // Visible track-fraction span. `(0, 1)` is the whole track; a narrower window
    // stretches its slice across `rect` (the zoom lane). The column→track-fraction
    // map below routes through it, so the bin sampling and the played/dimmed split
    // both follow the zoom with no other changes.
    let (w0, w1) = window;
    let wspan = (w1 - w0).max(f32::EPSILON);

    for cx in 0..cols {
        let frac = (w0 + (cx as f32 + 0.5) / cols as f32 * wspan).clamp(0.0, 1.0);
        let played = played_frac.map_or(true, |p| frac <= p);
        let x = x0 + cx as f32;

        if has_bands {
            // Map this pixel column to its span of band bins and take the per-band
            // MAX across them (peak-preserving). With far more bins than pixels the
            // fine transients then show as thin spikes instead of being sampled
            // away; when zoomed past 1 bin/pixel it degrades to a point sample.
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
            match style.mode {
                config::WaveformColorMode::Spectrum => {
                    // Draw the three bands tallest-first so the shortest ends up
                    // on top, visible in the centre of the taller ones.
                    let h = |v: u8, b: usize| {
                        wave_height(v as f32 / 255.0, style.height_exp) * style.band_gain[b]
                    };
                    let mut layers = [
                        (h(agg[0], 0), style.band_colors[0]),
                        (h(agg[1], 1), style.band_colors[1]),
                        (h(agg[2], 2), style.band_colors[2]),
                    ];
                    layers.sort_by(|a, c| c.0.total_cmp(&a.0));
                    for (h, col) in layers {
                        bar(&mut mesh, x, (h.min(1.0) * half).max(0.4), played, col);
                    }
                }
                config::WaveformColorMode::Energy => {
                    // Envelope = loudest band; colour = K-weighted loudness.
                    let env = agg[0].max(agg[1]).max(agg[2]) as f32 / 255.0;
                    let loud = agg[3] as f32 / 255.0;
                    bar(
                        &mut mesh,
                        x,
                        ((wave_height(env, style.height_exp) * style.energy_gain).min(1.0) * half)
                            .max(0.5),
                        played,
                        energy_color(energy_curve(loud), &style.energy_colors),
                    );
                }
            }
        } else if n > 0 {
            // No band data: peak envelope on a height ramp.
            let i = ((frac * n as f32) as usize).min(n - 1);
            let amp = waveform[i] as f32 / 255.0;
            bar(
                &mut mesh,
                x,
                ((amp * style.energy_gain).min(1.0) * half).max(1.0),
                played,
                energy_color(energy_curve(amp), &style.energy_colors),
            );
        }
    }

    if !mesh.is_empty() {
        painter.add(egui::Shape::mesh(mesh));
    }
}

/// Raise the threshold for reading as "high energy". The loudness byte is
/// normalized per-track over a wide (45 dB) window below the track's own peak,
/// so compressed music — which lives within a few dB of its peak — would
/// otherwise map almost entirely to the hot (amber/red) end and show no
/// contrast. This gamma curve pushes the bulk down into the cool/mid range so
/// only sections genuinely near the track's loudest moment read hot, giving the
/// waveform usable structure (intro/breakdown cool, drops hot).
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
