//! The radio: sit back and let the map dig for you.
//!
//! Switched on, it stands the dig on a record, plays that record's first
//! Discogs video in the mini-player, and when the video ends takes one of
//! the record's threads (artist or label, never the same one twice running
//! when there's a choice; style only when a record has neither, since a
//! style tag is too broad to steer by) to land on a record you don't own. That
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
//! The clock runs ahead of the music. As soon as a record is on, the finder
//! (see [`Find`]) steps out of it, lands the next record, fetches its videos
//! and readies its first one in the player behind the one playing, so when
//! the song ends the next starts at once rather than after a step, a detail
//! fetch and a page load. The finder is one record ahead, never more: the
//! next record is chosen from the one on air, as a walk should be.
//!
//! Pure GUI orchestration: nothing here touches the catalog beyond reading
//! the release cache, and playback is the same YouTube panel the sheet uses.

use super::*;
use crate::dig::{DigQuery, DigThread, VARIOUS_ARTIST_ID};
use ordnung_core::discogs::BrowseThread;
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
    /// The record's songs that have a video, tracklist order first, then
    /// the videos no track claimed. Empty until the detail is in.
    pub songs: Vec<RadioSong>,
    /// Which of `songs` is on.
    pub song: usize,
}

/// One playable song of the record on the radio.
#[derive(Clone)]
pub(crate) struct RadioSong {
    pub youtube_id: String,
    /// Side and position as pressed (`A2`); empty for a video no track claimed.
    pub position: String,
    /// Who performs this track, when the record itself doesn't say: set on
    /// compilations and splits, empty on a single-artist record.
    pub artist: String,
    pub title: String,
    /// As Discogs writes it (`5:18`), or empty.
    pub duration: String,
}

/// The record's songs, for the bar's list: every tracklist position whose
/// video was matched, in order, then the videos no track claimed under
/// their own titles.
fn radio_songs(d: &discogs::ReleaseDetail, artist: &str) -> Vec<RadioSong> {
    let m = d.match_videos(artist);
    let mut out = Vec::new();
    for (i, t) in d.tracklist.iter().enumerate() {
        let Some(v) = m.tracks.get(i).copied().flatten().and_then(|v| d.videos.get(v)) else {
            continue;
        };
        if let Some(id) = v.youtube_id() {
            out.push(RadioSong {
                youtube_id: id.to_string(),
                position: t.position.clone(),
                artist: t.artist.clone().unwrap_or_default(),
                title: t.title.clone(),
                duration: t.duration.clone(),
            });
        }
    }
    // A video no track claimed is often a second upload of one that was
    // ("Artist - Title [CAT001]" beside "Title"): it says nothing new, so
    // the list leaves it out. What's left is another thing entirely, a full
    // side or a mix, which is worth a row.
    let matched: Vec<String> = out
        .iter()
        .filter(|s| s.title.len() >= 4)
        .map(|s| s.title.to_lowercase())
        .collect();
    let repeats = |title: &str| {
        let t = title.to_lowercase();
        matched.iter().any(|m| t.contains(m.as_str()))
    };
    for v in m.leftover.iter().filter_map(|&v| d.videos.get(v)) {
        if repeats(&v.title) {
            continue;
        }
        if let Some(id) = v.youtube_id() {
            out.push(RadioSong {
                youtube_id: id.to_string(),
                position: String::new(),
                artist: String::new(),
                title: v.title.clone(),
                duration: v
                    .duration_secs
                    .map(|s| format!("{}:{:02}", s / 60, s % 60))
                    .unwrap_or_default(),
            });
        }
    }
    out
}

/// One record the radio has stood on, in the order it came: the walk the
/// numbers on the map and the row under the bar both read from.
#[derive(Clone)]
pub(crate) struct RadioStop {
    pub release_id: u64,
    pub artist: String,
    pub title: String,
    pub thumb_url: Option<String>,
    pub key: Option<VinylCoverKey>,
    /// The thread that led here from the stop before, and what was matched.
    pub via: Option<(DigThread, String)>,
    /// A fresh start: nothing led here from the stop before it. The first
    /// stop, or a restart when a record ran dry. (A record you point the
    /// radio at yourself starts a new walk instead.)
    pub fresh: bool,
}

/// What is on air.
#[derive(Clone)]
enum Phase {
    /// Nothing yet: find a record to start from.
    Seed,
    /// Nothing: the last record ended, or was skipped, before the next was
    /// ready. Goes on air the moment the finder has one.
    Waiting,
    /// A video is on; wait for it to end. The finder readies the next
    /// record meanwhile.
    Playing { release_id: u64, since: Instant },
}

/// The finder: the walk to the next record, run while the current one
/// plays so the switch doesn't wait on Discogs or YouTube.
#[derive(Clone)]
enum Find {
    /// Nothing sought. While a record plays this lasts one tick: the finder
    /// steps out of it at once.
    Idle,
    /// Pick a thread out of `from` and take it.
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
    /// Landed: the record's detail is being fetched for its videos.
    Loading {
        now: RadioNow,
        fresh: bool,
        since: Instant,
    },
    /// The next record, songs in hand, its first video readied in the
    /// player behind the one on air. Waits there until the record on air
    /// ends.
    Ready { now: RadioNow, fresh: bool },
    /// The walk ran out. The radio goes off with this message once the
    /// record on air has ended, rather than cutting it short.
    Dry(String),
}

/// Where the radio bar sits on the map. Along the top or bottom it lies
/// flat; dragged to a side it stands upright, cover on top.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RadioDock {
    Top,
    Bottom,
    Left,
    Right,
}

impl RadioDock {
    pub(crate) fn key(self) -> &'static str {
        match self {
            RadioDock::Top => "top",
            RadioDock::Bottom => "bottom",
            RadioDock::Left => "left",
            RadioDock::Right => "right",
        }
    }

    pub(crate) fn from_key(key: &str) -> Self {
        match key {
            "bottom" => RadioDock::Bottom,
            "left" => RadioDock::Left,
            "right" => RadioDock::Right,
            _ => RadioDock::Top,
        }
    }

    fn upright(self) -> bool {
        matches!(self, RadioDock::Left | RadioDock::Right)
    }
}

/// The radio bar away from its slot. While held, `rect` is where the hand
/// has it; let go, each corner of `rect` is carried to the slot's by its
/// own spring, so a bar dropped anywhere simply moves its corners to
/// where they belong, whatever shape it changes on the way.
#[derive(Clone, Copy)]
pub(crate) struct RadioFly {
    pub rect: egui::Rect,
    /// Velocity of the top-left and bottom-right corners.
    pub v_min: egui::Vec2,
    pub v_max: egui::Vec2,
    /// The dock the bar left, so its old layout can fade out under the new.
    pub from: RadioDock,
    /// Its size when it was picked up: the old layout keeps it while fading.
    pub from_size: egui::Vec2,
    /// When it was let go; `None` while it's still held.
    pub released: Option<Instant>,
}

/// The bar's measurements, shared by its two layouts.
struct BarGeo {
    pad: f32,
    btn: f32,
    scrub_h: f32,
    row_h: f32,
    flat_cover: f32,
    tall_cover: f32,
    tall_words_h: f32,
    /// How much of the song list is open, in points.
    list_h: f32,
}

pub(crate) struct Radio {
    pub on: bool,
    phase: Phase,
    /// The next record, in the making.
    find: Find,
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
    /// Every record the radio has stood on since it came on, oldest first.
    /// Kept after the radio goes off, so the map still reads; a fresh
    /// switch-on starts a new walk.
    pub walk: Vec<RadioStop>,
    /// A drag on the bar's scrubber in flight: the fraction under the
    /// pointer, which the bar paints instead of the live position until
    /// the drag lands and seeks.
    pub scrub: Option<f32>,
    /// A drag of the bar in flight: the pointer's offset from the bar's
    /// top-left corner and the bar's size when it was picked up, so it
    /// rides under the hand as it was.
    pub drag: Option<(egui::Vec2, egui::Vec2)>,
    /// The bar away from its slot: held, or on its way home after a drop.
    /// `None` once it's settled.
    pub fly: Option<RadioFly>,
    /// The bar's song list is open.
    pub expanded: bool,
    /// A song picked from the list, for the next tick to put on (the tick
    /// has the window handle the player needs; the bar doesn't).
    pub pick: Option<usize>,
}

impl Default for Radio {
    fn default() -> Self {
        Self {
            on: false,
            phase: Phase::Seed,
            find: Find::Idle,
            now: None,
            detail_rx: None,
            skip: false,
            last_thread: None,
            reseeds: 0,
            played: 0,
            walk: Vec::new(),
            scrub: None,
            drag: None,
            fly: None,
            expanded: false,
            pick: None,
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

/// A cover in `rect`, or a blank sleeve when the image isn't in yet.
fn paint_cover(painter: &egui::Painter, rect: egui::Rect, tex: Option<egui::TextureId>) {
    use crate::ui::tokens::{color, radius};
    match tex {
        Some(id) => {
            painter.add(egui::Shape::Rect(egui::epaint::RectShape {
                rect,
                rounding: radius::XS.into(),
                fill: egui::Color32::WHITE,
                stroke: egui::Stroke::NONE,
                blur_width: 0.0,
                fill_texture_id: id,
                uv: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
            }));
        }
        None => {
            painter.rect_filled(rect, radius::XS, color::SURFACE_HI);
            crate::ui::icon::record(painter, rect.center(), color::LABEL_4, rect.width() * 0.3);
        }
    }
}

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

    /// Start the radio on `rel`, from its node on the map. Pointing the
    /// radio at a record by hand begins a new journey: a radio already
    /// playing drops its walk and starts over here, so a path you left is
    /// not counted as the start of the one you chose instead.
    pub(crate) fn radio_start_from(&mut self, rel: &crate::graph::Release) {
        self.map_stand_on(rel);
        let Some(id) = self.dig.as_ref().map(|d| d.head().release_id) else {
            return;
        };
        let was_on = self.radio.on;
        self.radio = Radio {
            on: true,
            ..Radio::default()
        };
        if !was_on {
            webview::prewarm();
        }
        self.radio.phase = Phase::Waiting;
        self.radio_seek(id, true);
    }

    /// The record on air, or one on the walk, was traded for another
    /// pressing: same music, new id and sleeve. A record still readying its
    /// video is readied again as the new pressing; one already playing plays
    /// on, since restarting the track for a sleeve would be a bad trade.
    pub(crate) fn radio_remap(
        &mut self,
        from: u64,
        to: u64,
        title: &str,
        sub: &str,
        thumb_url: Option<String>,
    ) {
        for stop in self.radio.walk.iter_mut().filter(|s| s.release_id == from) {
            stop.release_id = to;
            if !title.is_empty() {
                stop.title = title.to_string();
            }
            stop.thumb_url = thumb_url.clone();
        }
        if let Some(n) = self.radio.now.as_mut() {
            if n.release_id == from {
                n.release_id = to;
                if !title.is_empty() {
                    n.title = title.to_string();
                }
                if !sub.is_empty() {
                    n.sub = sub.to_string();
                }
                n.thumb_url = thumb_url.clone();
            }
        }
        if let Phase::Playing { release_id, .. } = &mut self.radio.phase {
            if *release_id == from {
                *release_id = to;
            }
        }
        // A record being readied is readied again as the new pressing, which
        // carries its own videos.
        let retrace = |n: &mut RadioNow| {
            n.release_id = to;
            if !title.is_empty() {
                n.title = title.to_string();
            }
            if !sub.is_empty() {
                n.sub = sub.to_string();
            }
            n.thumb_url = thumb_url.clone();
            n.songs.clear();
            n.song = 0;
        };
        match &mut self.radio.find {
            Find::Stepping { from: f, .. } | Find::Landing { from: f, .. } if *f == from => *f = to,
            Find::Loading { now, since, .. } if now.release_id == from => {
                retrace(now);
                *since = Instant::now();
                self.radio.detail_rx = None;
            }
            Find::Ready { now, fresh } if now.release_id == from => {
                let mut now = now.clone();
                retrace(&mut now);
                self.radio.find = Find::Loading {
                    now,
                    fresh: *fresh,
                    since: Instant::now(),
                };
                self.radio.detail_rx = None;
            }
            _ => {}
        }
        let key = format!("r:{to}");
        if self.graph.focus.as_deref() == Some(&format!("r:{from}")) {
            self.graph.focus = Some(key);
        }
    }

    pub(crate) fn radio_stop(&mut self, why: &str) {
        let was_on = self.radio.on;
        self.radio.on = false;
        self.radio.now = None;
        self.radio.detail_rx = None;
        self.radio.phase = Phase::Seed;
        self.radio.find = Find::Idle;
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
                self.radio.phase = Phase::Waiting;
                self.radio_seek(id, true);
            }
            Phase::Waiting => {}
            Phase::Playing { release_id, since } => {
                // The file player started: one sound at a time, and the
                // listener chose that one.
                if self.audio.as_ref().is_some_and(|a| a.is_active()) {
                    self.radio_stop("Radio off: the player took over");
                    return;
                }
                if std::mem::take(&mut self.radio.skip) {
                    self.radio_advance(frame);
                    return;
                }
                // A song picked from the bar's list: the same record, another
                // of its videos.
                if let Some(i) = self.radio.pick.take() {
                    let id = self
                        .radio
                        .now
                        .as_ref()
                        .and_then(|n| n.songs.get(i))
                        .map(|s| s.youtube_id.clone());
                    if let Some(id) = id {
                        if let Some(n) = self.radio.now.as_mut() {
                            n.song = i;
                        }
                        self.radio_play(release_id, id, false, frame);
                        return;
                    }
                }
                if !webview::is_open() {
                    // Closed from the panel itself, or by a stop elsewhere.
                    self.radio_stop("Radio off");
                    return;
                }
                if webview::status() == webview::PlayerStatus::Stuck {
                    self.status = "That video wouldn't play. Digging on.".to_string();
                    self.radio_advance(frame);
                    return;
                }
                if webview::ended() || since.elapsed() > MAX_PLAY {
                    self.radio_advance(frame);
                    return;
                }
            }
        }
        self.drive_find(ctx, frame);
    }

    /// One tick of the finder. Runs whether or not a record is on air: with
    /// one on, it works ahead; with none, what it finds goes on at once.
    fn drive_find(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        let find = self.radio.find.clone();
        match find {
            Find::Idle => {
                // A record is on: start on the next straight away.
                if let Phase::Playing { release_id, .. } = self.radio.phase {
                    self.radio_step_from(release_id, Vec::new());
                }
            }
            Find::Stepping { from, tried, since } => {
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
                // A compilation names no artist to follow ("Various" is a
                // placeholder), so its artist thread is closed, unless the
                // walk came onto it down a real artist: that artist is on
                // the record, so the thread carries on as it was.
                let own_artist = head.query(DigThread::Artist).is_some();
                let carry: Option<u64> = if !own_artist
                    && matches!(head.via, Some((DigThread::Artist, _)))
                {
                    head.parent
                        .and_then(|p| dig.steps.get(p))
                        .and_then(|p| p.artist_ids.iter().copied().find(|id| *id != VARIOUS_ARTIST_ID))
                } else {
                    None
                };
                let open: Vec<DigThread> = [DigThread::Artist, DigThread::Label, DigThread::Style]
                    .into_iter()
                    .filter(|t| {
                        !tried.contains(t)
                            && (head.query(*t).is_some() || (*t == DigThread::Artist && carry.is_some()))
                    })
                    .collect();
                if open.is_empty() {
                    if head.detail_resolved || since.elapsed() > WAIT_IDS {
                        self.radio_reseed();
                    }
                    return;
                }
                // Artist and label first: a style tag is broad and subjective,
                // and a walk down it wanders off into anything Discogs files
                // under the same word. Style is only for a record with
                // neither of the other two.
                let firm: Vec<DigThread> = open
                    .iter()
                    .copied()
                    .filter(|t| *t != DigThread::Style)
                    .collect();
                let open = if firm.is_empty() { open } else { firm };
                // Prefer a different thread from the one that got us here.
                let fresh: Vec<DigThread> = open
                    .iter()
                    .copied()
                    .filter(|t| Some(*t) != self.radio.last_thread)
                    .collect();
                let pool = if fresh.is_empty() { open } else { fresh };
                let thread = pool[self.radio_roll(pool.len())];
                match (thread, carry) {
                    (DigThread::Artist, Some(id)) if !own_artist => {
                        self.dig_take(
                            DigThread::Artist,
                            DigQuery::Browse(BrowseThread::Artist, id),
                            None,
                        );
                    }
                    _ => self.dig_step(thread),
                }
                self.radio.find = Find::Landing {
                    from,
                    thread,
                    tried,
                    since: Instant::now(),
                };
            }
            Find::Landing {
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
                    // Landed. Ready its video; it's lit and leaned on when
                    // it comes on air.
                    let id = dig.head().release_id;
                    self.radio.last_thread = Some(thread);
                    self.radio.reseeds = 0;
                    self.dig_evict();
                    self.dig_prime();
                    self.radio_seek(id, false);
                } else {
                    // Nothing new down that thread: try another, then a
                    // fresh start.
                    tried.push(thread);
                    self.radio_step_from(from, tried);
                }
            }
            Find::Loading { now, fresh, since } => {
                let release_id = now.release_id;
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
                        let songs = detail
                            .as_ref()
                            .map(|d| radio_songs(d, &now.artist))
                            .unwrap_or_default();
                        if songs.is_empty() {
                            // Said only when the bar is waiting on it: while a
                            // record plays, a record never heard is no news.
                            if !matches!(self.radio.phase, Phase::Playing { .. }) {
                                self.status = format!(
                                    "No video on Discogs for {} – {}. Digging on.",
                                    now.artist, now.title
                                );
                            }
                            self.radio_step_from(release_id, Vec::new());
                            return;
                        }
                        // One song of the record, at random: a walk that
                        // always opened at A1 would hear every record's
                        // lead cut and nothing else. A pressed track over a
                        // stray upload (a full side, a mix) when there is one.
                        let tracks: Vec<usize> = (0..songs.len())
                            .filter(|&i| !songs[i].position.is_empty())
                            .collect();
                        let pick = if tracks.is_empty() {
                            self.radio_roll(songs.len())
                        } else {
                            tracks[self.radio_roll(tracks.len())]
                        };
                        let mut now = now;
                        now.songs = songs;
                        now.song = pick;
                        self.radio.find = Find::Ready { now, fresh };
                        self.radio_offer(frame);
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
            Find::Ready { .. } | Find::Dry(_) => self.radio_offer(frame),
        }
    }

    /// The finder has an answer. With nothing on air it goes on now; with a
    /// record playing, a found record's first video is readied behind it
    /// and a dry walk waits for the song to end.
    fn radio_offer(&mut self, frame: &eframe::Frame) {
        match (&self.radio.find, &self.radio.phase) {
            (Find::Ready { now, .. }, Phase::Playing { .. }) => {
                if let Some(song) = now.songs.get(now.song) {
                    webview::preload(&song.youtube_id);
                }
            }
            (Find::Ready { .. }, _) => self.radio_on_air(frame),
            (Find::Dry(why), Phase::Seed | Phase::Waiting) => {
                let why = why.clone();
                self.radio_stop(&why);
            }
            _ => {}
        }
    }

    /// The record on air is done with: skipped, ended, or stuck. The next
    /// goes on if the finder has it; otherwise the radio waits on the
    /// finder, and the sound stays up meanwhile rather than going quiet.
    fn radio_advance(&mut self, frame: &eframe::Frame) {
        match &self.radio.find {
            Find::Ready { .. } => self.radio_on_air(frame),
            Find::Dry(why) => {
                let why = why.clone();
                self.radio_stop(&why);
            }
            Find::Idle => {
                if let Phase::Playing { release_id, .. } = self.radio.phase {
                    self.radio_step_from(release_id, Vec::new());
                }
                self.radio.phase = Phase::Waiting;
            }
            _ => self.radio.phase = Phase::Waiting,
        }
    }

    /// Put the finder's record on air: light it on the map, lean the camera
    /// in, add it to the walk, and play its first song.
    fn radio_on_air(&mut self, frame: &eframe::Frame) {
        let Find::Ready { now, fresh } = std::mem::replace(&mut self.radio.find, Find::Idle) else {
            return;
        };
        let release_id = now.release_id;
        let key = format!("r:{release_id}");
        self.graph.focus = Some(key.clone());
        self.graph.lean = Some((key, true));
        self.graph.wake();
        let last = self.radio.walk.last().map(|s| s.release_id);
        if last != Some(release_id) {
            self.radio.walk.push(RadioStop {
                release_id,
                artist: now.artist.clone(),
                title: now.title.clone(),
                thumb_url: now.thumb_url.clone(),
                key: now.key,
                // A landing is joined to the stop before it by the thread it
                // came down; a start stands alone.
                via: if fresh { None } else { now.via.clone() },
                fresh: fresh || self.radio.walk.is_empty(),
            });
        }
        let video = now.songs.get(now.song).map(|s| s.youtube_id.clone());
        self.radio.now = Some(now);
        match video {
            Some(v) => self.radio_play(release_id, v, true, frame),
            None => {
                self.radio.phase = Phase::Waiting;
                self.radio_step_from(release_id, Vec::new());
            }
        }
    }

    /// Point the finder at the record the dig now stands on: take down what
    /// the bar will show of it, and go fetch its videos. `fresh` says nothing
    /// led here from the stop before.
    fn radio_seek(&mut self, release_id: u64, fresh: bool) {
        let now = self
            .dig
            .as_ref()
            .and_then(|d| d.steps.iter().find(|s| s.release_id == release_id))
            .map(|s| RadioNow {
                release_id,
                artist: s.artist.clone(),
                title: s.title.clone(),
                sub: s.sub.clone(),
                thumb_url: s.thumb_url.clone(),
                key: self.dig_start_keys.get(&release_id).copied(),
                via: s.via.clone(),
                want_sent: false,
                songs: Vec::new(),
                song: 0,
            })
            .unwrap_or(RadioNow {
                release_id,
                artist: String::new(),
                title: String::new(),
                sub: String::new(),
                thumb_url: None,
                key: None,
                via: None,
                want_sent: false,
                songs: Vec::new(),
                song: 0,
            });
        self.radio.detail_rx = None;
        self.radio.find = Find::Loading {
            now,
            fresh,
            since: Instant::now(),
        };
    }

    /// Move on from `from`, with `tried` the threads that already came back
    /// empty out of it.
    fn radio_step_from(&mut self, from: u64, tried: Vec<DigThread>) {
        self.radio.find = Find::Stepping {
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
            self.radio.find = Find::Dry("Radio off: ran out of threads to follow".to_string());
            return;
        }
        match self.radio_seed(true) {
            Some(id) => {
                if let (Some(n), false) = (&self.radio.now, matches!(self.radio.phase, Phase::Playing { .. })) {
                    self.status = format!("Nothing new near {} – {}. Fresh start.", n.artist, n.title);
                }
                self.radio_seek(id, true);
            }
            None => {
                self.radio.find = Find::Dry("Radio off: nothing left to start from".to_string());
            }
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

    /// Put `video` on in the mini-player for `release_id`. `new_record` is
    /// false for another song of the record already on, which doesn't count
    /// as a record played.
    fn radio_play(&mut self, release_id: u64, video: String, new_record: bool, frame: &eframe::Frame) {
        let title = match &self.radio.now {
            Some(n) => match n.songs.get(n.song).filter(|s| !s.artist.is_empty()) {
                Some(s) => format!("{} — {}", s.artist, s.title),
                None => format!("{} — {}", n.artist, n.title),
            },
            None => "Radio".to_string(),
        };
        // One sound at a time: the radio takes over from the player bar.
        self.claim_sound(Sound::Video);
        if !webview::play(frame, &[video], &title) {
            self.radio_stop("The radio needs the video player, which isn't available here.");
            return;
        }
        if new_record {
            self.radio.played += 1;
        }
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
    /// found, the clock, the controls, and a scrubber along the foot.
    ///
    /// It docks to one of four slots. Pick it up anywhere that isn't a
    /// control and it rides under the hand, with the slot it would take
    /// outlined; let go and it slides home. Flat along the top or bottom,
    /// upright at a side with the cover on top and the words beneath.
    pub(crate) fn draw_radio_bar(&mut self, ui: &mut egui::Ui, canvas: egui::Rect, ctx: &egui::Context) {
        use crate::ui::tokens::{color, font, radius, space};
        if !self.radio.on {
            return;
        }
        const PAD: f32 = 10.0;
        /// The strip the scrubber lives in: a hairline with a comfortable
        /// hit area around it.
        const SCRUB_H: f32 = 16.0;
        /// The painted controls' square.
        const BTN: f32 = 28.0;
        /// A docked bar's inset from the map's edge.
        const EDGE: f32 = 12.0;
        /// The spring that carries a dropped bar to its slot: stiffness and,
        /// at critical damping, the matching drag, so it arrives without a
        /// wobble. The same figure the map uses for a record and its ring slot.
        const SLOT_K: f32 = 70.0;
        /// How far off the map's centre line a drop must land to take a side.
        const SIDE_PULL: f32 = 0.22;
        /// One song in the list.
        const ROW_H: f32 = 22.0;

        let dock = RadioDock::from_key(&self.config.radio_dock);
        let now = self.radio.now.clone();
        let tr = webview::transport();
        let body_h = ui.fonts(|f| f.row_height(&font::body()));
        let cap_h = ui.fonts(|f| f.row_height(&font::caption()));

        // The song list, opening and closing under the scrubber. A record
        // with one song has nothing to choose between, so no list.
        let songs_n = now.as_ref().map_or(0, |n| n.songs.len());
        let listable = songs_n > 1;
        let open = ctx.animate_bool_with_time(
            ui.id().with("radio_songs_open"),
            self.radio.expanded && listable,
            0.2,
        );
        let list_full = songs_n as f32 * ROW_H + space::S2;
        let list_h = list_full * open;

        // Each slot, sized for the bar's format there.
        let flat_cover = 44.0;
        let flat_h = PAD + flat_cover + SCRUB_H + list_h + PAD * 0.4;
        let tall_w = 232.0_f32.min((canvas.width() * 0.4).max(160.0));
        let tall_cover = tall_w - PAD * 2.0;
        let tall_words_h = body_h * 2.0 + 2.0 + cap_h;
        let tall_h = PAD + tall_cover + space::S3 + tall_words_h + space::S3 + BTN + space::S2
            + SCRUB_H
            + list_h
            + PAD * 0.4;
        let slot = |d: RadioDock| -> egui::Rect {
            match d {
                RadioDock::Top | RadioDock::Bottom => {
                    let w = 660.0_f32.min(canvas.width() - EDGE * 2.0);
                    let y = if d == RadioDock::Top {
                        canvas.top() + EDGE
                    } else {
                        canvas.bottom() - EDGE - flat_h
                    };
                    egui::Rect::from_min_size(
                        egui::pos2(canvas.center().x - w * 0.5, y),
                        egui::vec2(w, flat_h),
                    )
                }
                RadioDock::Left => egui::Rect::from_min_size(
                    egui::pos2(canvas.left() + EDGE, canvas.top() + EDGE),
                    egui::vec2(tall_w, tall_h),
                ),
                RadioDock::Right => egui::Rect::from_min_size(
                    egui::pos2(canvas.right() - EDGE - tall_w, canvas.top() + EDGE),
                    egui::vec2(tall_w, tall_h),
                ),
            }
        };
        // The slot a bar centred at `c` would take when dropped.
        let slot_for = |c: egui::Pos2| -> RadioDock {
            let dx = (c.x - canvas.center().x) / canvas.width().max(1.0);
            let dy = (c.y - canvas.center().y) / canvas.height().max(1.0);
            if dx.abs() > SIDE_PULL {
                if dx < 0.0 {
                    RadioDock::Left
                } else {
                    RadioDock::Right
                }
            } else if dy > 0.0 {
                RadioDock::Bottom
            } else {
                RadioDock::Top
            }
        };
        let home = slot(dock);

        // Where the bar is this frame: under the hand while it's held, on
        // springs home after a drop, else home. Like a record on the map it
        // follows the pointer directly; let go, each corner is carried to
        // the slot's own corner by a critically damped spring, so the bar
        // simply moves from where it was dropped, every edge to its new
        // place, with no reframing in between. The layout it left in fades
        // out under the one it arrives in while it travels.
        let dt = ctx.input(|i| i.stable_dt).clamp(1.0 / 240.0, 1.0 / 30.0);
        let drag_pos = self
            .radio
            .drag
            .and_then(|(off, size)| ctx.pointer_latest_pos().map(|p| (p - off, size)));
        let bar = if let Some((min, size)) = drag_pos {
            let r = egui::Rect::from_min_size(min, size);
            let vel = match self.radio.fly {
                Some(f) => f.v_min * 0.5 + (min - f.rect.min) / dt * 0.5,
                None => egui::Vec2::ZERO,
            };
            self.radio.fly = Some(RadioFly {
                rect: r,
                v_min: vel,
                v_max: vel,
                from: dock,
                from_size: size,
                released: None,
            });
            r
        } else if let Some(mut f) = self.radio.fly {
            let k = SLOT_K;
            let d = 2.0 * k.sqrt();
            f.v_min += ((home.min - f.rect.min) * k - f.v_min * d) * dt;
            f.v_max += ((home.max - f.rect.max) * k - f.v_max * d) * dt;
            let min = f.rect.min + f.v_min * dt;
            let max = f.rect.max + f.v_max * dt;
            let settled = (home.min - min).length() < 0.3
                && (home.max - max).length() < 0.3
                && f.v_min.length() < 4.0
                && f.v_max.length() < 4.0;
            if settled {
                self.radio.fly = None;
                home
            } else {
                ctx.request_repaint();
                f.rect = egui::Rect::from_min_max(min, max);
                self.radio.fly = Some(f);
                f.rect
            }
        } else {
            home
        };
        // The arriving layout: laid out for home, anchored where the bar is.
        let lay = egui::Rect::from_min_size(bar.min, home.size());

        // The bar shadows the map: a pointer on it is on the bar, not on the
        // record under it, so nothing behind lights, drags or opens. The
        // controls and scrubber register after this and so sit on top of it;
        // anywhere else on the bar picks it up.
        let shade = ui.interact(bar, ui.id().with("radio_bar_shade"), egui::Sense::click_and_drag());
        if shade.drag_started() {
            if let Some(p) = shade.interact_pointer_pos() {
                self.radio.drag = Some((p - bar.min, bar.size()));
            }
        }
        if self.radio.drag.is_some() {
            ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
            ctx.request_repaint();
        } else if shade.hovered() {
            ctx.set_cursor_icon(egui::CursorIcon::Grab);
        }
        if shade.drag_stopped() && self.radio.drag.take().is_some() {
            let new = slot_for(bar.center());
            if new != dock {
                self.config.radio_dock = new.key().to_string();
                if let Err(e) = self.config.save() {
                    self.status = format!("Couldn't save settings: {e}");
                }
            }
            // A released bar keeps a little of the hand's motion and is
            // drawn home from there, rather than flying off like a slingshot.
            if let Some(f) = self.radio.fly.as_mut() {
                f.v_min *= 0.3;
                f.v_max = f.v_min;
                f.released = Some(Instant::now());
            }
            ctx.request_repaint();
        }

        ui.painter().rect(
            bar,
            radius::LG,
            color::SURFACE.gamma_multiply(0.96),
            egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
        );
        let tex = now
            .as_ref()
            .and_then(|n| self.radio_cover(n.key, n.thumb_url.as_deref()));
        // Everything inside is clipped to the bar as it is this frame.
        let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(lay));
        bui.set_clip_rect(bar);
        let seekable = tr.ready && tr.duration > 0.0;
        let live = if seekable {
            (tr.position / tr.duration).clamp(0.0, 1.0)
        } else {
            0.0
        };
        // The fraction the bar shows: the drag in flight, else the live one.
        let shown = self.radio.scrub.unwrap_or(live);

        let g = BarGeo {
            pad: PAD,
            btn: BTN,
            scrub_h: SCRUB_H,
            row_h: ROW_H,
            flat_cover,
            tall_cover,
            tall_words_h,
            list_h,
        };
        // On the way home from another format, the layout it left in fades
        // out at its old size under the arriving one.
        let crossfade = self.radio.fly.and_then(|f| {
            let t0 = f.released?;
            (f.from.upright() != dock.upright()).then(|| (f, (t0.elapsed().as_secs_f32() / 0.3).min(1.0)))
        });
        if let Some((f, t)) = crossfade {
            let mut dui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(egui::Rect::from_min_size(bar.min, f.from_size))
                    .id_salt("radio_departing"),
            );
            dui.set_clip_rect(bar);
            dui.set_opacity(1.0 - t);
            let _ = self.radio_bar_body(
                &mut dui,
                egui::Rect::from_min_size(bar.min, f.from_size),
                f.from.upright(),
                &g,
                &now,
                tr,
                tex,
                seekable,
                shown,
                listable,
                songs_n,
            );
            bui.set_opacity(t);
            ctx.request_repaint();
        }
        let (stop, skip, want, toggle) = self.radio_bar_body(
            &mut bui,
            lay,
            dock.upright(),
            &g,
            &now,
            tr,
            tex,
            seekable,
            shown,
            listable,
            songs_n,
        );
        if toggle {
            self.radio.expanded = !self.radio.expanded;
        }

        self.draw_radio_walk(ui, bar, dock == RadioDock::Bottom);

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

    /// The bar's contents, laid out `upright` or flat in `lay`: cover, words,
    /// transport, scrubber and the song list. Returns what the transport
    /// asked for: stop, skip, the record to want, and the list toggled.
    #[allow(clippy::too_many_arguments)]
    fn radio_bar_body(
        &mut self,
        ui: &mut egui::Ui,
        lay: egui::Rect,
        upright: bool,
        g: &BarGeo,
        now: &Option<RadioNow>,
        tr: webview::Transport,
        tex: Option<egui::TextureId>,
        seekable: bool,
        shown: f32,
        listable: bool,
        songs_n: usize,
    ) -> (bool, bool, Option<u64>, bool) {
        use crate::ui::tokens::space;
        let (stop, skip, want, toggle, strip) = if upright {
            let cover = egui::Rect::from_min_size(lay.min + egui::vec2(g.pad, g.pad), egui::Vec2::splat(g.tall_cover));
            paint_cover(ui.painter(), cover, tex);
            let words = egui::Rect::from_min_size(
                egui::pos2(cover.left(), cover.bottom() + space::S3),
                egui::vec2(g.tall_cover, g.tall_words_h),
            );
            self.radio_words(&ui, words, &now, tr, seekable, shown, true);
            let ctl = egui::Rect::from_min_size(
                egui::pos2(cover.left(), words.bottom() + space::S3),
                egui::vec2(g.tall_cover, g.btn),
            );
            let mut cui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(ctl)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            // Centred: the row is as wide as its marks, and starts half the
            // slack in from the right.
            let n = 3
                + usize::from(now.as_ref().is_some_and(|n| !self.vinyl_owned.contains(&n.release_id)))
                + usize::from(listable);
            let row_w = n as f32 * g.btn + (n - 1) as f32 * cui.spacing().item_spacing.x;
            let (stop, skip, want, toggle) =
                self.radio_controls(&mut cui, &now, tr, ((g.tall_cover - row_w) * 0.5).max(0.0), g.btn);
            let strip = egui::Rect::from_min_size(
                egui::pos2(cover.left(), ctl.bottom() + space::S2),
                egui::vec2(g.tall_cover, g.scrub_h),
            );
            (stop, skip, want, toggle, strip)
        } else {
            let cover = egui::Rect::from_min_size(lay.min + egui::vec2(g.pad, g.pad), egui::Vec2::splat(g.flat_cover));
            paint_cover(ui.painter(), cover, tex);
            // The top row, level with the cover: the controls take the right
            // end, as much as they need; the words get what's left.
            let row = egui::Rect::from_min_max(
                egui::pos2(cover.right() + g.pad, cover.top()),
                egui::pos2(lay.right(), cover.bottom()),
            );
            let mut cui = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(row)
                    .layout(egui::Layout::right_to_left(egui::Align::Center)),
            );
            let (stop, skip, want, toggle) = self.radio_controls(&mut cui, &now, tr, g.pad, g.btn);
            let words = egui::Rect::from_min_max(
                row.min,
                egui::pos2(cui.min_rect().left() - g.pad, row.bottom()),
            );
            self.radio_words(&ui, words, &now, tr, seekable, shown, false);
            let strip = egui::Rect::from_min_max(
                egui::pos2(cover.left(), cover.bottom()),
                egui::pos2(lay.right() - g.pad, cover.bottom() + g.scrub_h),
            );
            (stop, skip, want, toggle, strip)
        };
        self.radio_scrubber(ui, strip, tr, seekable, shown);
        // The list shows through exactly as far as it's open, however tall
        // the bar happens to be (in flight it can be taller than home), and
        // rows only answer the pointer through that part: egui clips a
        // widget's hit area to its ui's clip rect.
        if listable && g.list_h > space::S2 {
            let list = egui::Rect::from_min_size(
                egui::pos2(strip.left(), strip.bottom() + space::S2),
                egui::vec2(strip.width(), songs_n as f32 * g.row_h),
            );
            let open = egui::Rect::from_min_size(list.min, egui::vec2(list.width(), g.list_h - space::S2));
            let mut lui = ui.new_child(egui::UiBuilder::new().max_rect(list).id_salt("radio_list"));
            lui.set_clip_rect(open.intersect(ui.clip_rect()));
            self.radio_song_rows(&mut lui, list, now, g.row_h);
        }
        (stop, skip, want, toggle)
    }

    /// The bar's transport, right to left in `cui`: play/pause, skip, the
    /// wantlist heart, stop. Marks, not words: a "Play" that turns into
    /// "Pause" shoves the row sideways under the pointer, and four labels of
    /// differing width in button chrome never read as one transport. `lead`
    /// is the space before the first (rightmost) mark. A record with songs
    /// to choose between gets a chevron last, for the list. Returns what was
    /// asked for: stop, skip, the record to want, and the list toggled.
    fn radio_controls(
        &self,
        cui: &mut egui::Ui,
        now: &Option<RadioNow>,
        tr: webview::Transport,
        lead: f32,
        btn: f32,
    ) -> (bool, bool, Option<u64>, bool) {
        use crate::ui::icon;
        let mut stop = false;
        let mut skip = false;
        let mut want: Option<u64> = None;
        let mut toggle = false;
        cui.add_space(lead);
        if icon::mark_button(cui, btn, true, "Switch the radio off", |p, c, ink| {
            icon::stop(p, c, ink, 5.5)
        })
        .clicked()
        {
            stop = true;
        }
        if let Some(n) = now {
            let owned = self.vinyl_owned.contains(&n.release_id);
            let wanted = self.vinyl_wanted.contains(&n.release_id) || n.want_sent;
            if !owned {
                let tip = if wanted {
                    "On your Discogs wantlist"
                } else {
                    "Put this record on your Discogs wantlist"
                };
                // Once it's wanted the heart fills in the wantlist's own
                // amber and stops answering the pointer.
                let resp = icon::mark_button(cui, btn, !wanted, tip, |p, c, ink| {
                    let ink = if wanted {
                        icon::shelf_fill(VinylList::Wantlist)
                    } else {
                        ink
                    };
                    icon::wantlist(p, c, 7.0, ink, wanted)
                });
                if resp.clicked() {
                    want = Some(n.release_id);
                }
            }
        }
        if icon::mark_button(cui, btn, true, "Skip to the next record", |p, c, ink| {
            icon::skip_next(p, c, ink, 6.0)
        })
        .clicked()
        {
            skip = true;
        }
        let playing_phase = matches!(self.radio.phase, Phase::Playing { .. });
        let tip = if tr.playing {
            "Pause the record"
        } else {
            "Play the record"
        };
        if icon::mark_button(cui, btn, playing_phase && tr.ready, tip, |p, c, ink| {
            icon::play_pause(p, c, ink, tr.playing)
        })
        .clicked()
        {
            webview::toggle_pause();
        }
        if now.as_ref().is_some_and(|n| n.songs.len() > 1) {
            let open = self.radio.expanded;
            let tip = if open {
                "Hide the record's songs"
            } else {
                "Show the record's songs"
            };
            if icon::mark_button(cui, btn, true, tip, |p, c, ink| {
                icon::chevron(p, c, ink, 5.0, !open)
            })
            .clicked()
            {
                toggle = true;
            }
        }
        (stop, skip, want, toggle)
    }

    /// The record's songs, one row each in `list`: position, title, length,
    /// the one that's on marked in the accent. Click a row to put that song
    /// on. Kept to the caption size: it's a list to pick from, not a sheet.
    fn radio_song_rows(&mut self, ui: &mut egui::Ui, list: egui::Rect, now: &Option<RadioNow>, row_h: f32) {
        use crate::ui::tokens::{color, font, radius, space};
        let Some(n) = now else {
            return;
        };
        const POS_W: f32 = 26.0;
        let mut pick: Option<usize> = None;
        let mut like: Option<usize> = None;
        // The like mark's square: the row less a hair, so it never paints
        // over the row above or below.
        let like_w = (row_h - 2.0).max(12.0);
        for (i, song) in n.songs.iter().enumerate() {
            let rect = egui::Rect::from_min_size(
                egui::pos2(list.left(), list.top() + row_h * i as f32),
                egui::vec2(list.width(), row_h),
            );
            let resp = ui.interact(rect, ui.id().with(("radio_song", i)), egui::Sense::click());
            let on = i == n.song;
            let p = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
            if resp.hovered() {
                p.rect_filled(rect, radius::XS, color::SURFACE_HOVER);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let cy = rect.center().y;
            let x0 = rect.left() + space::S2;
            // Position, or the playing mark where there's none to show.
            if on {
                p.circle_filled(egui::pos2(x0 + 4.0, cy), 2.5, color::ACCENT);
            }
            if !song.position.is_empty() {
                p.text(
                    egui::pos2(x0 + POS_W, cy),
                    egui::Align2::RIGHT_CENTER,
                    &song.position,
                    font::caption(),
                    if on { color::ACCENT } else { color::LABEL_4 },
                );
            }
            let dur = p.layout_no_wrap(song.duration.clone(), font::caption(), color::LABEL_4);
            let dur_w = dur.size().x;
            // The like mark takes the row's edge; the length sits left of it.
            let like_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - space::S2 - like_w * 0.5, cy),
                egui::vec2(like_w, like_w),
            );
            let right = like_rect.left() - space::S2;
            p.galley(
                egui::pos2(right - dur_w, cy - dur.size().y * 0.5),
                dur,
                color::LABEL_4,
            );
            let title_left = x0 + POS_W + space::S3;
            let title_clip = p.with_clip_rect(egui::Rect::from_min_max(
                egui::pos2(title_left, rect.top()),
                egui::pos2(right - dur_w - space::S3, rect.bottom()),
            ));
            // The performer leads on a compilation, where the record's own
            // credit says "Various" and the song's says who.
            let words = if song.artist.is_empty() {
                song.title.clone()
            } else {
                format!("{} – {}", song.artist, song.title)
            };
            title_clip.text(
                egui::pos2(title_left, cy),
                egui::Align2::LEFT_CENTER,
                words,
                font::caption(),
                if on { color::LABEL } else { color::LABEL_2 },
            );
            if resp.clicked() && !on {
                pick = Some(i);
            }
            // Registered after the row, so the mark is on top and the click
            // is its own, not the row's.
            let artist = if song.artist.is_empty() { &n.artist } else { &song.artist };
            let liked = self.is_liked(artist, &song.title, Some(n.release_id), Some(&song.position));
            if crate::ui::button::like_mark_at(ui, like_rect, ui.id().with(("radio_like", i)), liked)
                .clicked()
            {
                like = Some(i);
            }
        }
        if pick.is_some() {
            self.radio.pick = pick;
        }
        if let Some(i) = like {
            let song = &n.songs[i];
            let artist = if song.artist.is_empty() { n.artist.clone() } else { song.artist.clone() };
            // The library track that is this song, if one is: the crate
            // shows FILE from the like on, not only after a look-up.
            self.ensure_library_index();
            let local = self.library_index.track_for(
                &ordnung_core::model::SongRef::new(artist.clone(), song.title.clone())
                    .at(Some(n.release_id), Some(&song.position)),
            );
            self.toggle_like(crate::liked::LikeSpec {
                artist,
                title: song.title.clone(),
                release_id: Some(n.release_id),
                position: Some(song.position.clone()),
                rel_artist: Some(n.artist.clone()),
                rel_title: Some(n.title.clone()),
                rel_label: None,
                rel_catno: None,
                rel_year: None,
                rel_thumb: n.thumb_url.clone(),
                local_track_id: local,
            });
        }
    }

    /// The record's name and its caption in `rect`: the pressing, how it was
    /// found, and the clock. Flat, both lines run on one row each, centred
    /// on the rect and cut at its edge; upright (`wrap`), the name may take
    /// two rows and the caption follows beneath.
    #[allow(clippy::too_many_arguments)]
    fn radio_words(
        &self,
        ui: &egui::Ui,
        rect: egui::Rect,
        now: &Option<RadioNow>,
        tr: webview::Transport,
        seekable: bool,
        shown: f32,
        wrap: bool,
    ) {
        use crate::ui::tokens::{color, font};
        let (line1, line2) = match now {
            Some(n) => {
                let mut parts: Vec<String> = Vec::new();
                // The song, when the record has more than one to tell apart.
                if n.songs.len() > 1 {
                    if let Some(s) = n.songs.get(n.song) {
                        parts.push(if s.artist.is_empty() {
                            s.title.clone()
                        } else {
                            format!("{} – {}", s.artist, s.title)
                        });
                    }
                }
                if !n.sub.is_empty() {
                    parts.push(n.sub.clone());
                }
                parts.push(match &n.via {
                    Some((t, m)) => format!("via {} {}", t.label(), m),
                    None => "where the radio started".to_string(),
                });
                let mut sub = parts.join(" · ");
                if seekable {
                    sub.push_str(&format!(
                        " · {} / {}",
                        crate::audio::fmt_time(shown * tr.duration),
                        crate::audio::fmt_time(tr.duration)
                    ));
                } else if matches!(self.radio.phase, Phase::Waiting) {
                    sub.push_str(match self.radio.find {
                        Find::Loading { .. } | Find::Ready { .. } => " · loading",
                        _ => " · digging for the next record",
                    });
                }
                (format!("{} – {}", n.artist, n.title), sub)
            }
            None => ("Finding a record to start from".to_string(), String::new()),
        };
        let clip = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
        let title_font = font::strong(font::body().size);
        if wrap {
            let g1 = clip.layout(line1, title_font, color::LABEL, rect.width());
            let two_rows = ui.fonts(|f| f.row_height(&font::body())) * 2.0 + 1.0;
            let title_h = g1.size().y.min(two_rows);
            let title_clip = clip.with_clip_rect(egui::Rect::from_min_size(
                rect.min,
                egui::vec2(rect.width(), title_h),
            ));
            title_clip.galley(rect.min, g1, color::LABEL);
            let g2 = clip.layout_no_wrap(line2, font::caption(), color::LABEL_3);
            clip.galley(egui::pos2(rect.left(), rect.top() + title_h + 2.0), g2, color::LABEL_3);
        } else {
            let g1 = clip.layout_no_wrap(line1, title_font, color::LABEL);
            let g2 = clip.layout_no_wrap(line2, font::caption(), color::LABEL_3);
            let total = g1.size().y + 2.0 + g2.size().y;
            let y0 = rect.center().y - total * 0.5;
            let h1 = g1.size().y;
            clip.galley(egui::pos2(rect.left(), y0), g1, color::LABEL);
            clip.galley(egui::pos2(rect.left(), y0 + h1 + 2.0), g2, color::LABEL_3);
        }
    }

    /// The scrubber in `strip`: a hairline, the played part in the accent,
    /// a knob only once the pointer is on it. It reads as a progress line
    /// until you reach for it; a click or a drag seeks the record.
    fn radio_scrubber(
        &mut self,
        ui: &mut egui::Ui,
        strip: egui::Rect,
        tr: webview::Transport,
        seekable: bool,
        shown: f32,
    ) {
        use crate::ui::tokens::color;
        let sense = if seekable {
            egui::Sense::click_and_drag()
        } else {
            egui::Sense::hover()
        };
        let resp = ui.interact(strip, ui.id().with("radio_scrub"), sense);
        let hot = seekable && (resp.hovered() || resp.dragged() || self.radio.scrub.is_some());
        let y = strip.center().y + 1.0;
        let (x0, x1) = (strip.left(), strip.right());
        let track = if hot { 3.0 } else { 2.0 };
        let p = ui.painter();
        p.line_segment(
            [egui::pos2(x0, y), egui::pos2(x1, y)],
            egui::Stroke::new(track, color::SEPARATOR_OPAQUE),
        );
        if seekable {
            let kx = x0 + shown * (x1 - x0);
            p.line_segment(
                [egui::pos2(x0, y), egui::pos2(kx, y)],
                egui::Stroke::new(track, if hot { color::ACCENT_HOVER } else { color::ACCENT }),
            );
            if hot {
                p.circle_filled(egui::pos2(kx, y), 5.0, egui::Color32::WHITE);
                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
            }
            let frac_at = |pos: egui::Pos2| ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
            if resp.dragged() || resp.drag_started() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    self.radio.scrub = Some(frac_at(pos));
                }
            }
            if resp.drag_stopped() {
                if let Some(f) = self.radio.scrub.take() {
                    webview::seek(f * tr.duration);
                }
            }
            if resp.clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    webview::seek(frac_at(pos) * tr.duration);
                }
                self.radio.scrub = None;
            }
        } else {
            self.radio.scrub = None;
        }
    }

    /// A radio record's cover: the local cache for a shelf record, the
    /// dig's cover cache for one Discogs found.
    fn radio_cover(&mut self, key: Option<VinylCoverKey>, thumb_url: Option<&str>) -> Option<egui::TextureId> {
        match key {
            Some(k) => {
                self.request_vinyl_cover(k);
                match self.vinyl_covers.get(&k) {
                    Some(ThumbState::Ready(Some(t))) => Some(t.id()),
                    _ => None,
                }
            }
            None => thumb_url.and_then(|u| self.dig_cover(u).map(|t| t.id())),
        }
    }

    /// The walk so far, in a row under the bar: every record the radio has
    /// stood on, oldest left, newest right, each numbered as it is on the
    /// map and joined to the next by the thread that led there. A fresh
    /// start breaks the row. Hover a stop for how it was found; click it to
    /// lean the map in on it.
    fn draw_radio_walk(&mut self, ui: &mut egui::Ui, bar: egui::Rect, above: bool) {
        use crate::ui::tokens::{color, font, radius};
        let walk = self.radio.walk.clone();
        if walk.len() < 2 {
            return;
        }
        const THUMB: f32 = 30.0;
        const LINK: f32 = 26.0;
        const PAD: f32 = 10.0;
        const NUM_H: f32 = 13.0;
        let pitch = THUMB + LINK;
        let room = bar.width() - PAD * 2.0;
        // The last stops that fit; a count of the ones cut off leads the row.
        let fit = (((room + LINK) / pitch).floor() as usize).max(2);
        let (skipped, shown): (usize, &[RadioStop]) = if walk.len() > fit {
            let k = walk.len() - (fit - 1);
            (k, &walk[k..])
        } else {
            (0, &walk[..])
        };
        let more = (skipped > 0).then(|| format!("+{skipped}"));
        let more_w = more.as_ref().map(|m| {
            ui.painter()
                .layout_no_wrap(m.clone(), font::caption(), color::LABEL_3)
                .size()
                .x
                + LINK
        });
        let w = PAD * 2.0 + shown.len() as f32 * THUMB + (shown.len() - 1) as f32 * LINK
            + more_w.unwrap_or(0.0);
        let h = PAD + THUMB + 2.0 + NUM_H + PAD * 0.6;
        let row = egui::Rect::from_min_size(
            egui::pos2(
                bar.center().x - w * 0.5,
                if above { bar.top() - 8.0 - h } else { bar.bottom() + 8.0 },
            ),
            egui::vec2(w, h),
        );
        // Shadows the map the same way the bar does.
        ui.interact(row, ui.id().with("radio_walk_shade"), egui::Sense::click_and_drag());
        ui.painter().rect(
            row,
            radius::LG,
            color::SURFACE.gamma_multiply(0.96),
            egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
        );
        let mut x = row.left() + PAD;
        let cy = row.top() + PAD + THUMB * 0.5;
        if let Some(m) = &more {
            let p = ui.painter();
            let galley = p.layout_no_wrap(m.clone(), font::caption(), color::LABEL_3);
            p.galley(egui::pos2(x, cy - galley.size().y * 0.5), galley, color::LABEL_3);
            x += more_w.unwrap_or(0.0);
        }
        let on_air = self.radio.now.as_ref().map(|n| n.release_id);
        let first_num = skipped + 1;
        let mut lean: Option<u64> = None;
        for (k, stop) in shown.iter().enumerate() {
            let num = first_num + k;
            let thumb = egui::Rect::from_center_size(
                egui::pos2(x + THUMB * 0.5, cy),
                egui::Vec2::splat(THUMB),
            );
            // The link from the stop before, in the colour of the thread
            // that led here. A fresh start gets a gap and a tick instead.
            if k > 0 || more.is_some() {
                let p = ui.painter();
                let a = egui::pos2(x - LINK + 3.0, cy);
                let b = egui::pos2(x - 3.0, cy);
                match (&stop.via, stop.fresh) {
                    (Some((t, _)), false) => {
                        let tint = crate::graph::thread_tint(*t);
                        p.line_segment([a, b], egui::Stroke::new(2.0, tint));
                        p.add(egui::Shape::convex_polygon(
                            vec![b, b + egui::vec2(-5.0, -3.0), b + egui::vec2(-5.0, 3.0)],
                            tint,
                            egui::Stroke::NONE,
                        ));
                    }
                    _ => {
                        let m = egui::pos2(x - LINK * 0.5, cy);
                        p.line_segment(
                            [m - egui::vec2(0.0, 6.0), m + egui::vec2(0.0, 6.0)],
                            egui::Stroke::new(1.0, color::LABEL_4),
                        );
                    }
                }
            }
            let tex = self.radio_cover(stop.key, stop.thumb_url.as_deref());
            let p = ui.painter();
            paint_cover(p, thumb, tex);
            let live = on_air == Some(stop.release_id);
            let resp = ui.interact(
                thumb,
                ui.id().with(("radio_walk", k, stop.release_id)),
                egui::Sense::click(),
            );
            let p = ui.painter();
            let (sw, sc) = if live {
                (2.0, color::ACCENT)
            } else if resp.hovered() {
                (1.5, color::LABEL_2)
            } else {
                (1.0, color::SEPARATOR_OPAQUE)
            };
            p.rect_stroke(thumb, radius::XS, egui::Stroke::new(sw, sc));
            let galley = p.layout_no_wrap(
                num.to_string(),
                font::strong(10.0),
                if live { color::ACCENT_HOVER } else { color::LABEL_3 },
            );
            p.galley(
                egui::pos2(thumb.center().x - galley.size().x * 0.5, thumb.bottom() + 2.0),
                galley,
                color::LABEL_3,
            );
            let mut note = format!("{num}. {} – {}", stop.artist, stop.title);
            note.push('\n');
            note.push_str(&match (&stop.via, stop.fresh) {
                (Some((t, m)), false) => format!("via {} {}", t.label(), m),
                _ if num == 1 => "where the radio started".to_string(),
                _ => "a fresh start".to_string(),
            });
            if resp.on_hover_note(note).clicked() {
                lean = Some(stop.release_id);
            }
            x += pitch;
        }
        if let Some(id) = lean {
            self.graph.lean = Some((format!("r:{id}"), true));
            self.graph.wake();
        }
    }
}
