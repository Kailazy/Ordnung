//! The radio: sit back and let the map dig for you.
//!
//! Switched on, it stands the dig on a record, plays that record's first
//! Discogs video in the mini-player, and when the video ends takes one of
//! the record's threads (artist, label or style, never the same one twice
//! running when there's a choice) to land on a record you don't own. That
//! record plays next, and so on. Each landing lights up on the map, joined
//! to the record before it by a trail, and the camera leans in on it. Skip
//! moves on at once; Want puts the record on the wantlist without stopping
//! the music.
//!
//! Every step is the ordinary dig machinery in `dig.rs`, so what the radio
//! walks is the same web the strip shows, primed and paced the same way. The
//! only thing here is the clock: what to do when a video ends, a thread runs
//! dry, or Discogs has nothing new down it.
//!
//! Pure GUI orchestration: nothing here touches the catalog beyond reading
//! the release cache, and playback is the same YouTube panel the sheet uses.

use super::*;
use crate::dig::DigThread;
use std::sync::mpsc::Receiver;

/// The record on air, as the map's bar shows it.
#[derive(Clone)]
pub(crate) struct RadioNow {
    pub release_id: u64,
    pub artist: String,
    pub title: String,
    pub sub: String,
    pub thumb_url: Option<String>,
    /// The shelf key when the record is one you own (a seed), so its cover
    /// comes from the local cache.
    pub key: Option<VinylCoverKey>,
    /// The thread that found it, and what was matched.
    pub via: Option<(DigThread, String)>,
    /// A wantlist add has been asked for; the button waits on Discogs.
    pub want_sent: bool,
}

#[derive(Clone)]
enum Phase {
    /// Find a record to start from.
    Seed,
    /// Fetch the record's detail for its videos.
    Loading { release_id: u64, since: Instant },
    /// A video is on; wait for it to end.
    Playing { release_id: u64, since: Instant },
    /// Pick a thread out of the record and take it.
    Stepping {
        from: u64,
        /// Threads already tried from this record that came back empty.
        tried: Vec<DigThread>,
        since: Instant,
    },
    /// The step is out; wait for the find.
    Landing {
        from: u64,
        thread: DigThread,
        tried: Vec<DigThread>,
        since: Instant,
    },
}

pub(crate) struct Radio {
    pub on: bool,
    phase: Phase,
    pub now: Option<RadioNow>,
    /// The detail fetch for the record being readied.
    detail_rx: Option<Receiver<(u64, Option<discogs::ReleaseDetail>)>>,
    /// Raised by the bar's Skip: move on at the next tick.
    pub skip: bool,
    /// The thread the last landing came down, so the next pick prefers
    /// another: three artist steps running is a discography, not a dig.
    last_thread: Option<DigThread>,
    /// Fresh starts taken since the last landing. Too many in a row and the
    /// radio gives up rather than churning requests.
    reseeds: u32,
    /// Records played since the radio came on.
    pub played: usize,
}

impl Default for Radio {
    fn default() -> Self {
        Self {
            on: false,
            phase: Phase::Seed,
            now: None,
            detail_rx: None,
            skip: false,
            last_thread: None,
            reseeds: 0,
            played: 0,
        }
    }
}

/// How long a record may sit waiting for its ids, a step for its find, or a
/// video for its page, before the radio moves on without it.
const WAIT_IDS: Duration = Duration::from_secs(20);
const WAIT_LAND: Duration = Duration::from_secs(75);
const WAIT_VIDEO: Duration = Duration::from_secs(25);
/// The longest one record is left on: a live set on a release page shouldn't
/// hold the radio for an hour.
const MAX_PLAY: Duration = Duration::from_secs(20 * 60);
/// Fresh starts in a row before the radio gives up.
const MAX_RESEEDS: u32 = 4;

impl App {
    /// The toolbar's switch.
    pub(crate) fn radio_toggle(&mut self) {
        if self.radio.on {
            self.radio_stop("Radio off");
        } else {
            self.radio = Radio {
                on: true,
                ..Radio::default()
            };
            webview::prewarm();
            self.status = "Radio on: finding a record to start from".to_string();
        }
    }

    pub(crate) fn radio_stop(&mut self, why: &str) {
        let was_on = self.radio.on;
        self.radio.on = false;
        self.radio.now = None;
        self.radio.detail_rx = None;
        self.radio.phase = Phase::Seed;
        if was_on {
            webview::close();
            self.status = why.to_string();
        }
    }

    /// One tick of the radio's clock. Called every frame.
    pub(crate) fn drive_radio(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if !self.radio.on {
            return;
        }
        // The panel is not an egui surface and Discogs answers off-thread:
        // keep frames coming so a video's end or a landing is noticed.
        ctx.request_repaint_after(Duration::from_millis(300));
        // The sheet took the player: the listener is doing their own thing.
        if self
            .vinyl_sheet
            .as_ref()
            .is_some_and(|s| s.playing_video.is_some())
        {
            self.radio_stop("Radio off: the record sheet took over the player");
            return;
        }
        let phase = self.radio.phase.clone();
        match phase {
            Phase::Seed => {
                let Some(id) = self.radio_seed(false) else {
                    self.radio_stop(
                        "Nothing to start the radio from. Sync your shelves, or dig a record first.",
                    );
                    return;
                };
                self.radio_ready(id);
            }
            Phase::Loading { release_id, since } => {
                if self.radio.detail_rx.is_none() {
                    self.radio_fetch_detail(release_id, ctx.clone());
                }
                let answer = self
                    .radio
                    .detail_rx
                    .as_ref()
                    .and_then(|rx| rx.try_recv().ok());
                match answer {
                    Some((id, detail)) if id == release_id => {
                        self.radio.detail_rx = None;
                        let video = detail.as_ref().and_then(|d| {
                            d.videos
                                .iter()
                                .find_map(|v| v.youtube_id().map(str::to_string))
                        });
                        match video {
                            Some(v) => self.radio_play(release_id, v, frame),
                            None => {
                                if let Some(n) = &self.radio.now {
                                    self.status = format!(
                                        "No video on Discogs for {} – {}. Digging on.",
                                        n.artist, n.title
                                    );
                                }
                                self.radio_step_from(release_id, Vec::new());
                            }
                        }
                    }
                    Some(_) => {
                        // An answer for a record we've already left.
                        self.radio.detail_rx = None;
                    }
                    None if since.elapsed() > WAIT_VIDEO => {
                        self.radio.detail_rx = None;
                        self.radio_step_from(release_id, Vec::new());
                    }
                    None => {}
                }
            }
            Phase::Playing { release_id, since } => {
                if std::mem::take(&mut self.radio.skip) {
                    self.radio_step_from(release_id, Vec::new());
                    return;
                }
                if !webview::is_open() {
                    // Closed from the panel itself, or by a stop elsewhere.
                    self.radio_stop("Radio off");
                    return;
                }
                if webview::status() == webview::PlayerStatus::Stuck {
                    self.status = "That video wouldn't play. Digging on.".to_string();
                    self.radio_step_from(release_id, Vec::new());
                    return;
                }
                if webview::ended() || since.elapsed() > MAX_PLAY {
                    self.radio_step_from(release_id, Vec::new());
                }
            }
            Phase::Stepping { from, tried, since } => {
                // The dig must stand on the record. If the listener moved
                // the dig elsewhere meanwhile, follow them.
                let Some(dig) = self.dig.as_ref() else {
                    self.radio_reseed();
                    return;
                };
                if dig.pending.is_some() {
                    return;
                }
                let head = dig.head();
                let from = if head.release_id != from {
                    head.release_id
                } else {
                    from
                };
                let open: Vec<DigThread> = [DigThread::Artist, DigThread::Label, DigThread::Style]
                    .into_iter()
                    .filter(|t| !tried.contains(t) && head.query(*t).is_some())
                    .collect();
                if open.is_empty() {
                    if head.detail_resolved || since.elapsed() > WAIT_IDS {
                        self.radio_reseed();
                    }
                    return;
                }
                // Prefer a different thread from the one that got us here.
                let fresh: Vec<DigThread> = open
                    .iter()
                    .copied()
                    .filter(|t| Some(*t) != self.radio.last_thread)
                    .collect();
                let pool = if fresh.is_empty() { open } else { fresh };
                let thread = pool[self.radio_roll(pool.len())];
                self.dig_step(thread);
                self.radio.phase = Phase::Landing {
                    from,
                    thread,
                    tried,
                    since: Instant::now(),
                };
            }
            Phase::Landing {
                from,
                thread,
                mut tried,
                since,
            } => {
                let Some(dig) = self.dig.as_ref() else {
                    self.radio_reseed();
                    return;
                };
                if dig.pending.is_some() {
                    if since.elapsed() > WAIT_LAND {
                        tried.push(thread);
                        self.radio_step_from(from, tried);
                    }
                    return;
                }
                if dig.head().release_id != from {
                    // Landed. Light it, lean in, and ready its video.
                    let id = dig.head().release_id;
                    self.radio.last_thread = Some(thread);
                    self.radio.reseeds = 0;
                    self.dig_evict();
                    self.dig_prime();
                    self.radio_ready(id);
                } else {
                    // Nothing new down that thread: try another, then a
                    // fresh start.
                    tried.push(thread);
                    self.radio_step_from(from, tried);
                }
            }
        }
    }

    /// Point the radio at the record the dig now stands on: light it on the
    /// map, lean the camera in, and go fetch its videos.
    fn radio_ready(&mut self, release_id: u64) {
        let key = format!("r:{release_id}");
        self.graph.focus = Some(key.clone());
        self.graph.lean = Some((key, true));
        self.graph.wake();
        let now = self.dig.as_ref().and_then(|d| {
            d.steps.iter().find(|s| s.release_id == release_id).map(|s| RadioNow {
                release_id,
                artist: s.artist.clone(),
                title: s.title.clone(),
                sub: s.sub.clone(),
                thumb_url: s.thumb_url.clone(),
                key: self.dig_start_keys.get(&release_id).copied(),
                via: s.via.clone(),
                want_sent: false,
            })
        });
        self.radio.now = now;
        self.radio.detail_rx = None;
        self.radio.phase = Phase::Loading {
            release_id,
            since: Instant::now(),
        };
    }

    /// Move on from `from`, with `tried` the threads that already came back
    /// empty out of it.
    fn radio_step_from(&mut self, from: u64, tried: Vec<DigThread>) {
        self.radio.phase = Phase::Stepping {
            from,
            tried,
            since: Instant::now(),
        };
    }

    /// Start over from a record you own, when the current record has no
    /// thread left to take.
    fn radio_reseed(&mut self) {
        self.radio.reseeds += 1;
        if self.radio.reseeds > MAX_RESEEDS {
            self.radio_stop("Radio off: ran out of threads to follow");
            return;
        }
        match self.radio_seed(true) {
            Some(id) => {
                if let Some(n) = &self.radio.now {
                    self.status = format!("Nothing new near {} – {}. Fresh start.", n.artist, n.title);
                }
                self.radio_ready(id);
            }
            None => self.radio_stop("Radio off: nothing left to start from"),
        }
    }

    /// Stand the dig on a starting record and return its release id. The
    /// record the map is standing on comes first, then the dig's head, then
    /// a record off your shelves at random. `fresh` skips the first two: a
    /// restart wants somewhere new.
    fn radio_seed(&mut self, fresh: bool) -> Option<u64> {
        if !fresh {
            if let Some(rel) = self.graph.standing_on() {
                self.map_stand_on(&rel);
                return self.dig.as_ref().map(|d| d.head().release_id);
            }
            if let Some(d) = &self.dig {
                return Some(d.head().release_id);
            }
        }
        let current = self.radio.now.as_ref().map(|n| n.release_id);
        let pool_of = |list: VinylList, recs: &[VinylRecord]| -> Vec<VinylCoverKey> {
            recs.iter()
                .filter(|r| Some(r.release_id) != current)
                .filter(|r| {
                    !r.artist.trim().is_empty()
                        || r.label.as_deref().is_some_and(|l| !l.trim().is_empty())
                })
                .map(|r| (list, r.instance_id))
                .collect()
        };
        let mut pool = pool_of(VinylList::Collection, &self.vinyl);
        if pool.is_empty() {
            pool = pool_of(VinylList::Wantlist, &self.wantlist);
        }
        if pool.is_empty() {
            return None;
        }
        let key = pool[self.radio_roll(pool.len())];
        self.start_dig(key);
        self.dig.as_ref().map(|d| d.head().release_id)
    }

    /// A varying index in `0..len`, off the dig's rolling seed and the
    /// clock, so two radios started the same way don't walk the same path.
    fn radio_roll(&mut self, len: usize) -> usize {
        let t = Instant::now().elapsed().as_nanos() as u64
            ^ std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
        self.dig_seed = self.dig_seed.wrapping_mul(6364136223846793005).wrapping_add(t | 1);
        ((self.dig_seed >> 33) as usize) % len.max(1)
    }

    /// Fetch the record's detail, cache first, for its videos.
    fn radio_fetch_detail(&mut self, release_id: u64, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.radio.detail_rx = Some(rx);
        let db = self.db_path.clone();
        let token = self.discogs_token();
        thread::spawn(move || {
            let id = release_id.to_string();
            let detail = Catalog::open(&db).ok().and_then(|cat| {
                if let Ok(Some(d)) = cat.cached_release(&id) {
                    return Some(d);
                }
                if token.trim().is_empty() {
                    return None;
                }
                let client =
                    discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
                cat.release_cached_or(&id, || client.fetch_release(&id)).ok()
            });
            let _ = tx.send((release_id, detail));
            ctx.request_repaint();
        });
    }

    /// Put `video` on in the mini-player for `release_id`.
    fn radio_play(&mut self, release_id: u64, video: String, frame: &eframe::Frame) {
        let title = match &self.radio.now {
            Some(n) => format!("{} — {}", n.artist, n.title),
            None => "Radio".to_string(),
        };
        // One sound at a time: the radio takes over from the player bar.
        if let Some(a) = self.audio.as_mut() {
            if a.is_active() {
                a.toggle_pause();
            }
        }
        if !webview::play(frame, &[video], &title) {
            self.radio_stop("The radio needs the video player, which isn't available here.");
            return;
        }
        self.radio.played += 1;
        if let Some(n) = &self.radio.now {
            self.status = match &n.via {
                Some((t, m)) => format!("Radio: {} – {} (via {} {})", n.artist, n.title, t.label(), m),
                None => format!("Radio: {} – {}", n.artist, n.title),
            };
        }
        self.radio.phase = Phase::Playing {
            release_id,
            since: Instant::now(),
        };
    }

    /// The bar over the map while the radio is on: cover, record, how it was
    /// found, the clock, and the controls.
    pub(crate) fn draw_radio_bar(&mut self, ui: &mut egui::Ui, canvas: egui::Rect, ctx: &egui::Context) {
        use crate::ui::tokens::{color, font, radius};
        if !self.radio.on {
            return;
        }
        const W: f32 = 660.0;
        const H: f32 = 64.0;
        const PAD: f32 = 10.0;
        let w = W.min(canvas.width() - 24.0);
        let bar = egui::Rect::from_min_size(
            egui::pos2(canvas.center().x - w * 0.5, canvas.top() + 12.0),
            egui::vec2(w, H),
        );
        ui.painter().rect(
            bar,
            radius::LG,
            color::SURFACE.gamma_multiply(0.96),
            egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
        );
        let now = self.radio.now.clone();
        let cover = egui::Rect::from_min_size(
            bar.min + egui::vec2(PAD, PAD),
            egui::Vec2::splat(H - PAD * 2.0),
        );
        let tex = now.as_ref().and_then(|n| match n.key {
            Some(k) => {
                self.request_vinyl_cover(k);
                match self.vinyl_covers.get(&k) {
                    Some(ThumbState::Ready(Some(t))) => Some(t.id()),
                    _ => None,
                }
            }
            None => n
                .thumb_url
                .as_deref()
                .and_then(|u| self.dig_cover(u).map(|t| t.id())),
        });
        let painter = ui.painter();
        match tex {
            Some(id) => {
                painter.add(egui::Shape::Rect(egui::epaint::RectShape {
                    rect: cover,
                    rounding: radius::XS.into(),
                    fill: egui::Color32::WHITE,
                    stroke: egui::Stroke::NONE,
                    blur_width: 0.0,
                    fill_texture_id: id,
                    uv: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                }));
            }
            None => {
                painter.rect_filled(cover, radius::XS, color::SURFACE_HI);
                crate::ui::icon::record(painter, cover.center(), color::LABEL_4, cover.width() * 0.3);
            }
        }
        // The controls take the right end, as much as they need; the words
        // get what's left.
        let ctl = egui::Rect::from_min_max(egui::pos2(cover.right() + PAD, bar.top()), bar.max);
        let mut cui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(ctl)
                .layout(egui::Layout::right_to_left(egui::Align::Center)),
        );
        let mut stop = false;
        let mut skip = false;
        let mut want: Option<u64> = None;
        crate::ui::control_row(&mut cui, |ui| {
            ui.add_space(PAD);
            if ui.button("Stop").on_hover_note("Switch the radio off").clicked() {
                stop = true;
            }
            if let Some(n) = &now {
                let owned = self.vinyl_owned.contains(&n.release_id);
                let wanted = self.vinyl_wanted.contains(&n.release_id) || n.want_sent;
                if !owned {
                    let label = if wanted { "Wanted" } else { "Want" };
                    let tip = if wanted {
                        "On your Discogs wantlist"
                    } else {
                        "Put this record on your Discogs wantlist"
                    };
                    if ui
                        .add_enabled(!wanted, egui::Button::new(label))
                        .on_hover_note(tip)
                        .clicked()
                    {
                        want = Some(n.release_id);
                    }
                }
            }
            if ui.button("Next").on_hover_note("Skip to the next record").clicked() {
                skip = true;
            }
            let tr = webview::transport();
            let playing_phase = matches!(self.radio.phase, Phase::Playing { .. });
            let (label, tip) = if tr.playing {
                ("Pause", "Pause the record")
            } else {
                ("Play", "Play the record")
            };
            if ui
                .add_enabled(playing_phase && tr.ready, egui::Button::new(label))
                .on_hover_note(tip)
                .clicked()
            {
                webview::toggle_pause();
            }
        });
        let words = egui::Rect::from_min_max(
            egui::pos2(cover.right() + PAD, bar.top()),
            egui::pos2(cui.min_rect().left() - PAD, bar.bottom()),
        );
        let (line1, line2) = match &now {
            Some(n) => {
                let mut sub = n.sub.clone();
                if !sub.is_empty() {
                    sub.push_str(" · ");
                }
                sub.push_str(&match &n.via {
                    Some((t, m)) => format!("via {} {}", t.label(), m),
                    None => "where the radio started".to_string(),
                });
                let tr = webview::transport();
                if tr.ready && tr.duration > 0.0 {
                    sub.push_str(&format!(
                        " · {} / {}",
                        crate::audio::fmt_time(tr.position),
                        crate::audio::fmt_time(tr.duration)
                    ));
                } else if matches!(self.radio.phase, Phase::Loading { .. }) {
                    sub.push_str(" · loading");
                } else if matches!(self.radio.phase, Phase::Stepping { .. } | Phase::Landing { .. }) {
                    sub.push_str(" · digging for the next record");
                }
                (format!("{} – {}", n.artist, n.title), sub)
            }
            None => ("Finding a record to start from".to_string(), String::new()),
        };
        let painter = ui.painter();
        let clip = painter.with_clip_rect(words);
        let g1 = clip.layout_no_wrap(line1, font::strong(font::body().size), color::LABEL);
        let g2 = clip.layout_no_wrap(line2, font::caption(), color::LABEL_3);
        let total = g1.size().y + 2.0 + g2.size().y;
        let y0 = words.center().y - total * 0.5;
        clip.galley(egui::pos2(words.left(), y0), g1.clone(), color::LABEL);
        clip.galley(egui::pos2(words.left(), y0 + g1.size().y + 2.0), g2, color::LABEL_3);

        if stop {
            self.radio_stop("Radio off");
        }
        if skip {
            self.radio.skip = true;
        }
        if let Some(id) = want {
            if let Some(n) = self.radio.now.as_mut() {
                n.want_sent = true;
                let label = format!("{} — {}", n.artist, n.title);
                self.request_vinyl_edit(
                    ctx.clone(),
                    VinylEdit::Want {
                        release_ids: vec![id],
                        label,
                    },
                );
            }
        }
    }
}
