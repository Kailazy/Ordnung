//! Now-playing player for the catalog.
//!
//! Clicking a track's play control starts a *streaming* decode on a background
//! thread: audio begins after about a second of PCM is buffered, while the
//! rest of the file keeps decoding behind the read cursor — so a long file on
//! a slow USB stick starts in about a second instead of after a full-file
//! decode. The bottom-bar player then shows artwork, title/artist, a
//! play/pause button, and a draggable scrubber — the engine exposes the
//! current position, duration, and a `seek` so the scrubber can drive playback
//! like Spotify's (seeks clamp to what's decoded so far until decode ends).
//!
//! The buffer lives behind an `Arc` and is played through a small custom
//! `Source` that holds a cursor into it — a seek or resume rebuilds the
//! cursor, never re-copies the audio.
//!
//! This is playback-only: it never touches the catalog or the source file
//! beyond reading it to decode.

use ordnung_core::analysis::decode::decode_interleaved_chunks;
use ordnung_core::model::Id;
use rodio::source::Source;
use rodio::{OutputStream, OutputStreamHandle, Sink};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant};

/// What the table needs to render the play control for a given row.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PlayState {
    /// Not the current track (or the current track is paused).
    Idle,
    /// This track is being decoded.
    Loading,
    /// This track is the one actively playing.
    Playing,
}

enum DecodeMsg {
    /// Enough audio is buffered to start playing; decode continues behind it.
    Started {
        id: Id,
        sample_rate: u32,
        channels: u16,
        /// Total frames per the file's header, when it says — the duration
        /// shown until the decode finishes and the exact count is known.
        total_frames: Option<u64>,
        pcm: Arc<StreamingPcm>,
    },
    /// The decode ran to the end (possibly truncated by a mid-file error);
    /// the buffer is complete and the exact duration is knowable.
    Finished { id: Id },
    Failed {
        id: Id,
        error: String,
    },
}

/// Playback PCM that fills in behind the read cursor while the decoder is
/// still working. Writers append under the lock and then publish the new
/// length; readers trust only the published length, so a reader never sees
/// a partially-written tail.
#[derive(Default)]
pub struct StreamingPcm {
    data: RwLock<Vec<f32>>,
    /// Number of interleaved samples currently readable.
    len: AtomicUsize,
    /// Set once the decoder has exited (successfully or not) — after this the
    /// buffer will never grow again.
    done: AtomicBool,
}

impl StreamingPcm {
    fn append(&self, chunk: &[f32]) {
        let mut data = self.data.write().unwrap();
        data.extend_from_slice(chunk);
        self.len.store(data.len(), Ordering::Release);
    }

    fn finish(&self) {
        self.done.store(true, Ordering::Release);
    }

    pub fn published_len(&self) -> usize {
        self.len.load(Ordering::Acquire)
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    /// Run `f` over the samples decoded so far. Callers that need the whole
    /// track (waveform rendering) gate on [`is_done`] first.
    pub fn with<R>(&self, f: impl FnOnce(&[f32]) -> R) -> R {
        let data = self.data.read().unwrap();
        f(&data[..self.published_len().min(data.len())])
    }
}

/// A rodio source that streams interleaved f32 samples out of a
/// [`StreamingPcm`], keeping only a read cursor. Samples are copied out in
/// chunks so the audio thread takes the lock a few times a second, not per
/// sample. Reaching the frontier of a still-running decode plays silence
/// *without advancing*, so no audio is ever skipped; hitting the end of a
/// finished buffer ends the source. Seeking makes a fresh cursor at the
/// target sample; the (potentially large) audio is never copied wholesale.
/// `pcm` holds interleaved frames (L,R,… per frame) so the native channel
/// layout is preserved.
struct BufferSource {
    pcm: Arc<StreamingPcm>,
    pos: usize,
    /// Locally cached run of samples starting at `chunk_start`.
    chunk: Vec<f32>,
    chunk_start: usize,
    sample_rate: u32,
    channels: u16,
}

/// How many samples `BufferSource` copies out per lock. ~0.09 s of 48 kHz
/// stereo: small enough to stay responsive, large enough that locking is noise.
const REFILL_SAMPLES: usize = 16_384;

impl BufferSource {
    fn new(pcm: Arc<StreamingPcm>, pos: usize, sample_rate: u32, channels: u16) -> Self {
        Self {
            pcm,
            pos,
            chunk: Vec::new(),
            chunk_start: pos,
            sample_rate,
            channels,
        }
    }
}

impl Iterator for BufferSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        loop {
            let chunk_end = self.chunk_start + self.chunk.len();
            if self.pos >= self.chunk_start && self.pos < chunk_end {
                let s = self.chunk[self.pos - self.chunk_start];
                self.pos += 1;
                return Some(s);
            }
            let len = self.pcm.published_len();
            if self.pos < len {
                let data = self.pcm.data.read().unwrap();
                let end = len.min(self.pos + REFILL_SAMPLES).min(data.len());
                self.chunk_start = self.pos;
                self.chunk.clear();
                self.chunk.extend_from_slice(&data[self.pos..end]);
                continue;
            }
            if self.pcm.is_done() {
                return None;
            }
            // Starved mid-decode: hold position and emit silence until the
            // decoder catches up. Content is never skipped; the wall-clock
            // position can drift ahead by the (rare, brief) gap.
            return Some(0.0);
        }
    }
}

impl Source for BufferSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        if !self.pcm.is_done() {
            return None;
        }
        let frames = self.pcm.published_len() as f32 / self.channels.max(1) as f32;
        Some(Duration::from_secs_f32(
            frames / self.sample_rate.max(1) as f32,
        ))
    }
}

/// Title/artist/cover the OS "Now Playing" panel is currently advertising. The
/// engine rebuilds a souvlaki `MediaMetadata` from this (plus the live duration)
/// whenever the track or its cover changes.
struct NowPlayingMeta {
    title: String,
    artist: String,
    /// `file://` URL to a cover image written to a temp file, or `None`.
    cover_url: Option<String>,
}

/// Owns the audio output and the currently-loaded track. The `OutputStream` is
/// `!Send`, so this lives on the UI thread for the app's lifetime.
pub struct AudioEngine {
    _stream: OutputStream,
    handle: OutputStreamHandle,
    sink: Option<Sink>,
    /// The decoded track currently loaded (playing or paused). `None` when idle.
    current: Option<Id>,
    /// Track being decoded in the background, if any (playback hasn't begun).
    loading: Option<Id>,
    /// The current track's PCM, shared with the playing `BufferSource` (and
    /// still growing while `decode_done` is false) — a seek spins up a new
    /// cursor without re-decoding or copying.
    pcm_buf: Option<Arc<StreamingPcm>>,
    /// The current track's decode has run to completion; `pcm_buf` is final.
    decode_done: bool,
    /// Cancels the in-flight decode thread when the track is superseded, so a
    /// skipped-past long file doesn't keep a core busy for minutes.
    load_cancel: Option<Arc<AtomicBool>>,
    sample_rate: u32,
    /// Channel count of the loaded track (interleaved in `samples`).
    channels: u16,
    duration: f32,
    /// Playback position (seconds) captured the last time the sink (re)started.
    base_secs: f32,
    /// When the sink last started from `base_secs`. `None` while paused — the
    /// position is then frozen at `base_secs`.
    started_at: Option<Instant>,
    tx: Sender<DecodeMsg>,
    rx: Receiver<DecodeMsg>,
    /// OS media-control bridge (macOS Now Playing / media keys). `None` when the
    /// platform integration is unavailable (then the engine just plays locally).
    controls: Option<MediaControls>,
    /// Remote play/pause/seek commands from the OS, pushed by souvlaki's callback
    /// (off the UI thread) and drained in `poll`.
    cmd_rx: Receiver<MediaControlEvent>,
    /// What we're advertising to the OS, or `None` when nothing is loaded.
    np_meta: Option<NowPlayingMeta>,
    /// Play/paused state last pushed to the OS; `poll` reconciles against the
    /// live state so GUI-driven changes also reach the Now Playing panel.
    reported_playing: Option<bool>,
    /// Set when a seek/toggle/load changed playback so `poll` re-pushes the OS
    /// playback status even if the play/paused flag itself didn't flip.
    status_dirty: bool,
    /// Master output volume as a linear amplitude factor, `0.0`–`1.0`. Held here
    /// rather than only on the sink because `start_sink_at` rebuilds the sink on
    /// every seek and resume — a fresh `Sink` starts at unity, so the level is
    /// re-applied from this field each time.
    volume: f32,
    /// Last decode/output error, surfaced to the status bar by the caller.
    pub last_error: Option<String>,
}

impl AudioEngine {
    /// Open the default output device. Returns `None` on machines with no audio
    /// output (e.g. headless CI) so the GUI still runs — the player just won't
    /// appear.
    ///
    /// `ctx` is the egui context: souvlaki's remote-command callback fires on the
    /// OS run-loop thread, so it nudges a repaint to wake the UI thread, which is
    /// where `poll` actually applies the command.
    pub fn new(ctx: egui::Context) -> Option<Self> {
        let (stream, handle) = OutputStream::try_default().ok()?;
        let (tx, rx) = mpsc::channel();
        let (cmd_tx, cmd_rx) = mpsc::channel();
        let controls = init_media_controls(cmd_tx, ctx);
        Some(Self {
            _stream: stream,
            handle,
            sink: None,
            current: None,
            loading: None,
            pcm_buf: None,
            decode_done: false,
            load_cancel: None,
            sample_rate: 0,
            channels: 1,
            duration: 0.0,
            base_secs: 0.0,
            started_at: None,
            tx,
            rx,
            controls,
            cmd_rx,
            np_meta: None,
            reported_playing: None,
            status_dirty: false,
            volume: 1.0,
            last_error: None,
        })
    }

    /// How the play control for `id` should render right now.
    pub fn state_for(&self, id: Id) -> PlayState {
        if self.loading == Some(id) {
            PlayState::Loading
        } else if self.current == Some(id) && self.is_playing() {
            PlayState::Playing
        } else {
            PlayState::Idle
        }
    }

    /// The track that's loaded in the player (playing or paused), if any.
    pub fn current(&self) -> Option<Id> {
        self.current
    }

    /// True while audio is loading or actively playing — the caller uses this to
    /// keep repainting so `poll` runs, the scrubber animates, and end-of-track is
    /// noticed promptly.
    pub fn is_active(&self) -> bool {
        self.loading.is_some() || self.is_playing()
    }

    /// True when a sink exists and is running (not paused, not finished).
    fn is_playing(&self) -> bool {
        self.started_at.is_some()
            && self
                .sink
                .as_ref()
                .map_or(false, |s| !s.is_paused() && !s.empty())
    }

    /// Current playback position in seconds, clamped to the track length.
    pub fn position(&self) -> f32 {
        let p = match self.started_at {
            Some(t) => self.base_secs + t.elapsed().as_secs_f32(),
            None => self.base_secs,
        };
        p.clamp(0.0, self.duration)
    }

    /// Length of the loaded track in seconds (0 when nothing is loaded).
    pub fn duration(&self) -> f32 {
        self.duration
    }

    /// The loaded track's decoded PCM for high-resolution rendering:
    /// interleaved `f32` samples (read via [`StreamingPcm::with`]), channel
    /// count, and sample rate. `None` while idle or still decoding — same
    /// timing the consumers always had, since the buffer used to exist only
    /// once decode finished. The buffer is shared (`Arc`), so cloning the
    /// handle never copies the audio.
    pub fn pcm(&self) -> Option<(Arc<StreamingPcm>, u16, u32)> {
        if !self.decode_done {
            return None;
        }
        Some((self.pcm_buf.clone()?, self.channels, self.sample_rate))
    }

    /// Play control click for a row: start `id` from the top, or — if it's already
    /// the loaded track — toggle pause/resume.
    pub fn play_or_toggle(&mut self, id: Id, path: PathBuf) {
        if self.current == Some(id) {
            self.toggle_pause();
            return;
        }
        if self.loading == Some(id) {
            // Already decoding this one; a second click cancels the load.
            self.stop();
            return;
        }
        self.stop();
        self.loading = Some(id);
        self.last_error = None;
        let tx = self.tx.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.load_cancel = Some(cancel.clone());
        thread::spawn(move || {
            // Streaming decode: buffer ~1 s of audio, announce `Started` so
            // playback begins, and keep appending until the file ends. The
            // sink reads through the same shared buffer the whole time.
            let pcm = Arc::new(StreamingPcm::default());
            // Cells rather than plain locals: the format is written by the
            // decoder's `on_start` and read by `on_chunk`, and two closures
            // can't share a `&mut`.
            let format = std::cell::Cell::new(None::<(u32, u16, Option<u64>)>);
            let started = std::cell::Cell::new(false);
            let announce = || {
                if let Some((sample_rate, channels, total_frames)) = format.get() {
                    started.set(true);
                    let _ = tx.send(DecodeMsg::Started {
                        id,
                        sample_rate,
                        channels,
                        total_frames,
                        pcm: pcm.clone(),
                    });
                }
            };
            let result = decode_interleaved_chunks(
                &path,
                |start| format.set(Some((start.sample_rate, start.channels, start.total_frames))),
                |chunk| {
                    if cancel.load(Ordering::Relaxed) {
                        return false;
                    }
                    pcm.append(chunk);
                    if !started.get() {
                        if let Some((sr, ch, _)) = format.get() {
                            // One second of prebuffer before the sink starts,
                            // so playback never begins starved.
                            if pcm.published_len() >= sr as usize * ch.max(1) as usize {
                                announce();
                            }
                        }
                    }
                    true
                },
            );
            pcm.finish();
            match result {
                Ok(()) => {
                    // A short track can end before the prebuffer threshold.
                    if !started.get() && pcm.published_len() > 0 {
                        announce();
                    }
                    if started.get() {
                        let _ = tx.send(DecodeMsg::Finished { id });
                    } else {
                        let _ = tx.send(DecodeMsg::Failed {
                            id,
                            error: "track has no audio to play".into(),
                        });
                    }
                }
                Err(e) => {
                    if started.get() {
                        // Mid-file failure after playback began: keep playing
                        // the (truncated) audio we have rather than yanking it.
                        let _ = tx.send(DecodeMsg::Finished { id });
                    } else {
                        let _ = tx.send(DecodeMsg::Failed {
                            id,
                            error: e.to_string(),
                        });
                    }
                }
            }
        });
    }

    /// Pause if playing, resume if paused. Resuming from the very end restarts the
    /// track from the top.
    pub fn toggle_pause(&mut self) {
        if self.is_playing() {
            // Freeze the clock at the current position and pause the sink.
            self.base_secs = self.position();
            self.started_at = None;
            if let Some(s) = &self.sink {
                s.pause();
            }
        } else if self.current.is_some() {
            if self.position() >= self.duration.max(f32::EPSILON) - 0.05 {
                self.start_sink_at(0.0);
            } else if let Some(s) = &self.sink {
                s.play();
                self.started_at = Some(Instant::now());
            } else {
                // Sink was dropped (e.g. ran to the end); rebuild it.
                self.start_sink_at(self.base_secs);
            }
        }
        self.status_dirty = true;
    }

    /// Jump to `secs` and keep the current play/pause state. While the decode
    /// is still running, the target clamps to what's decoded so far — a jump
    /// into the undecoded tail would otherwise sit in silence waiting for the
    /// decoder to reach it.
    pub fn seek(&mut self, secs: f32) {
        if self.current.is_none() {
            return;
        }
        let mut max = self.duration;
        if !self.decode_done {
            if let Some(pcm) = &self.pcm_buf {
                let per_sec = (self.sample_rate.max(1) as usize) * self.channels.max(1) as usize;
                max = max.min(pcm.published_len() as f32 / per_sec as f32);
            }
        }
        let was_playing = self.is_playing();
        self.start_sink_at(secs.clamp(0.0, max));
        if !was_playing {
            if let Some(s) = &self.sink {
                s.pause();
            }
            self.started_at = None;
        }
    }

    /// (Re)build the sink so playback resumes from `secs`. Leaves it playing; the
    /// caller pauses afterward if the player was paused.
    fn start_sink_at(&mut self, secs: f32) {
        let Some(pcm) = self.pcm_buf.clone() else {
            return;
        };
        if let Some(s) = self.sink.take() {
            s.stop();
        }
        match Sink::try_new(&self.handle) {
            Ok(sink) => {
                let ch = self.channels.max(1) as usize;
                // Convert seconds → sample index, snapped to a frame boundary so
                // interleaved channels stay aligned (an odd offset would swap L/R).
                let frame = (secs * self.sample_rate as f32) as usize;
                let pos = (frame * ch).min(pcm.published_len());
                sink.append(BufferSource::new(
                    pcm,
                    pos,
                    self.sample_rate,
                    self.channels.max(1),
                ));
                sink.set_volume(self.volume);
                sink.play();
                self.sink = Some(sink);
                self.base_secs = secs;
                self.started_at = Some(Instant::now());
                self.status_dirty = true;
            }
            Err(e) => self.last_error = Some(format!("audio output error: {e}")),
        }
    }

    /// Set the master output volume. Applies to the live sink immediately and is
    /// remembered for sinks rebuilt by a later seek or resume. Takes effect even
    /// when nothing is loaded, so the knob is meaningful before playback starts.
    pub fn set_volume(&mut self, volume: f32) {
        self.volume = volume.clamp(0.0, 1.0);
        if let Some(s) = &self.sink {
            s.set_volume(self.volume);
        }
    }

    /// Stop playback and clear all player state. Also cancels any in-flight
    /// decode so a superseded long file stops burning a core.
    pub fn stop(&mut self) {
        if let Some(sink) = self.sink.take() {
            sink.stop();
        }
        if let Some(cancel) = self.load_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        self.current = None;
        self.loading = None;
        self.pcm_buf = None;
        self.decode_done = false;
        self.started_at = None;
        self.base_secs = 0.0;
        self.duration = 0.0;
        self.np_meta = None;
        self.status_dirty = true;
    }

    /// Tell the OS "Now Playing" panel what track is loaded. Call when starting a
    /// new track (the cover, which loads asynchronously, arrives later via
    /// [`set_now_playing_cover`]). The duration is filled in once decode finishes.
    pub fn set_now_playing(&mut self, title: String, artist: String) {
        self.np_meta = Some(NowPlayingMeta {
            title,
            artist,
            cover_url: None,
        });
        self.push_metadata();
        self.status_dirty = true;
    }

    /// Attach (or clear) the cover art shown in the OS Now Playing panel for the
    /// loaded track. `cover_url` is a `file://` URL to an image on disk.
    pub fn set_now_playing_cover(&mut self, cover_url: Option<String>) {
        if let Some(meta) = self.np_meta.as_mut() {
            meta.cover_url = cover_url;
            self.push_metadata();
        }
    }

    /// Push the current track's title/artist/duration/cover to the OS panel.
    fn push_metadata(&mut self) {
        let duration = (self.duration > 0.0).then(|| Duration::from_secs_f32(self.duration));
        // Disjoint field borrows: `controls` mutable, `np_meta` shared.
        let (Some(controls), Some(meta)) = (self.controls.as_mut(), self.np_meta.as_ref()) else {
            return;
        };
        let _ = controls.set_metadata(MediaMetadata {
            title: Some(&meta.title),
            artist: (!meta.artist.is_empty()).then_some(meta.artist.as_str()),
            album: None,
            cover_url: meta.cover_url.as_deref(),
            duration,
        });
    }

    /// Push the current play/paused state + position to the OS panel. `set_metadata`
    /// replaces the whole now-playing dict, so this must run *after* it to layer the
    /// playback state back on.
    fn push_playback(&mut self) {
        if self.controls.is_none() {
            return;
        }
        let playback = if self.np_meta.is_none() {
            self.reported_playing = None;
            MediaPlayback::Stopped
        } else {
            let playing = self.is_playing();
            self.reported_playing = Some(playing);
            let progress = Some(MediaPosition(Duration::from_secs_f32(
                self.position().max(0.0),
            )));
            if playing {
                MediaPlayback::Playing { progress }
            } else {
                MediaPlayback::Paused { progress }
            }
        };
        if let Some(c) = self.controls.as_mut() {
            let _ = c.set_playback(playback);
        }
    }

    /// Apply one remote command from the OS media controls. The low-level state
    /// changers it calls mark `status_dirty`, so `poll` re-reports the new state.
    fn handle_media_event(&mut self, event: MediaControlEvent) {
        match event {
            MediaControlEvent::Play => {
                if !self.is_playing() {
                    self.toggle_pause();
                }
            }
            MediaControlEvent::Pause | MediaControlEvent::Stop => {
                if self.is_playing() {
                    self.toggle_pause();
                }
            }
            MediaControlEvent::Toggle => self.toggle_pause(),
            MediaControlEvent::SetPosition(MediaPosition(d)) => self.seek(d.as_secs_f32()),
            MediaControlEvent::SeekBy(dir, d) => self.seek_relative(dir, d.as_secs_f32()),
            MediaControlEvent::Seek(dir) => self.seek_relative(dir, 5.0),
            // No queue (single-track player), so skip/raise/quit/volume are no-ops.
            _ => {}
        }
    }

    /// Seek forward/backward from the current position by `delta` seconds.
    fn seek_relative(&mut self, dir: SeekDirection, delta: f32) {
        let target = match dir {
            SeekDirection::Forward => self.position() + delta,
            SeekDirection::Backward => self.position() - delta,
        };
        self.seek(target);
    }

    /// Drain decode results and detect natural end-of-track. Call once a frame.
    pub fn poll(&mut self) {
        // Apply any remote commands (play/pause/seek) the OS sent since last frame.
        while let Ok(event) = self.cmd_rx.try_recv() {
            self.handle_media_event(event);
        }
        while let Ok(msg) = self.rx.try_recv() {
            match msg {
                DecodeMsg::Started {
                    id,
                    sample_rate,
                    channels,
                    total_frames,
                    pcm,
                } => {
                    // A newer click may have superseded this decode; ignore stale ones.
                    if self.loading != Some(id) {
                        continue;
                    }
                    self.loading = None;
                    self.sample_rate = sample_rate.max(1);
                    self.channels = channels.max(1);
                    // The header's frame count stands in for the duration
                    // until the decode finishes (below); a headerless stream
                    // shows the decoded-so-far length, growing as it loads.
                    self.duration = total_frames
                        .map(|tf| tf as f32 / self.sample_rate as f32)
                        .unwrap_or(0.0);
                    self.pcm_buf = Some(pcm);
                    self.decode_done = false;
                    self.current = Some(id);
                    self.base_secs = 0.0;
                    self.start_sink_at(0.0);
                    // A provisional duration is known now — refresh the OS
                    // panel so its scrubber shows the track length.
                    self.push_metadata();
                }
                DecodeMsg::Finished { id } => {
                    if self.current != Some(id) {
                        continue;
                    }
                    self.decode_done = true;
                    // Exact duration from what actually decoded (headers lie
                    // by a frame or two; a truncated decode by much more).
                    if let Some(pcm) = &self.pcm_buf {
                        let frames = pcm.published_len() as f32 / self.channels.max(1) as f32;
                        self.duration = frames / self.sample_rate.max(1) as f32;
                    }
                    self.push_metadata();
                }
                DecodeMsg::Failed { id, error } => {
                    if self.loading == Some(id) {
                        self.loading = None;
                        self.last_error = Some(format!("couldn't decode track: {error}"));
                    }
                }
            }
        }
        // While decoding, the duration only ever grows toward the decoded
        // frontier — a header-supplied length already exceeds it (no-op), and
        // a headerless stream's scrubber tracks what actually exists.
        if !self.decode_done {
            if let Some(pcm) = &self.pcm_buf {
                let frames = pcm.published_len() as f32 / self.channels.max(1) as f32;
                self.duration = self.duration.max(frames / self.sample_rate.max(1) as f32);
            }
        }
        // Track ran to its end on its own — freeze the scrubber at the end and
        // drop the sink, but keep `current` so the bar still shows what played.
        if self.started_at.is_some() {
            if let Some(sink) = &self.sink {
                if sink.empty() {
                    self.sink = None;
                    self.started_at = None;
                    self.base_secs = self.duration;
                    self.status_dirty = true;
                }
            }
        }
        // Reconcile the OS panel's playback state with reality. This catches every
        // path — GUI clicks, remote commands, and a track ending on its own — so
        // macOS Now Playing always mirrors the in-app player.
        if self.controls.is_some()
            && (self.status_dirty || self.reported_playing != Some(self.is_playing()))
        {
            self.push_playback();
            self.status_dirty = false;
        }
    }
}

/// Wire up the OS media-control bridge. souvlaki registers the system's
/// play/pause/seek handlers; each fires on the OS run-loop thread, so the callback
/// just forwards the command into `cmd_rx` and wakes the UI thread via `ctx`.
/// Returns `None` (engine plays locally only) if the platform integration fails —
/// e.g. an unbundled binary or a platform without media controls.
fn init_media_controls(
    cmd_tx: Sender<MediaControlEvent>,
    ctx: egui::Context,
) -> Option<MediaControls> {
    let config = PlatformConfig {
        // Required on Linux (MPRIS); ignored on macOS.
        dbus_name: "org.ordnung.Ordnung",
        display_name: "Ordnung",
        // Required on Windows (SMTC); we don't pass a window handle, so media
        // controls are macOS/Linux-only.
        hwnd: None,
    };
    let mut controls = MediaControls::new(config).ok()?;
    controls
        .attach(move |event| {
            let _ = cmd_tx.send(event);
            ctx.request_repaint();
        })
        .ok()?;
    Some(controls)
}

/// Format a duration in seconds as `m:ss` for the scrubber labels.
pub fn fmt_time(secs: f32) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "0:00".into();
    }
    let total = secs as u32;
    format!("{}:{:02}", total / 60, total % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_formatting() {
        assert_eq!(fmt_time(0.0), "0:00");
        assert_eq!(fmt_time(5.0), "0:05");
        assert_eq!(fmt_time(65.0), "1:05");
        assert_eq!(fmt_time(605.0), "10:05");
        assert_eq!(fmt_time(-3.0), "0:00");
        assert_eq!(fmt_time(f32::NAN), "0:00");
    }

    fn finished_pcm(samples: Vec<f32>) -> Arc<StreamingPcm> {
        let pcm = Arc::new(StreamingPcm::default());
        pcm.append(&samples);
        pcm.finish();
        pcm
    }

    #[test]
    fn buffer_source_reports_duration_and_drains() {
        let src = BufferSource::new(finished_pcm(vec![0.0; 100]), 0, 50, 1);
        assert_eq!(src.sample_rate(), 50);
        assert_eq!(src.channels(), 1);
        assert_eq!(src.total_duration(), Some(Duration::from_secs_f32(2.0)));
        assert_eq!(src.count(), 100);

        // Stereo: 100 interleaved samples = 50 frames at 50 Hz = 1 s.
        let stereo = BufferSource::new(finished_pcm(vec![0.0; 100]), 0, 50, 2);
        assert_eq!(stereo.channels(), 2);
        assert_eq!(stereo.total_duration(), Some(Duration::from_secs_f32(1.0)));
    }

    /// The streaming contract: a source that reaches the frontier of an
    /// unfinished decode holds its place and plays silence — never skipping
    /// content — then resumes with the real samples once they land, and only
    /// ends when the finished buffer is truly drained.
    #[test]
    fn buffer_source_starves_with_silence_and_resumes_without_skipping() {
        let pcm = Arc::new(StreamingPcm::default());
        pcm.append(&[1.0, 2.0]);
        let mut src = BufferSource::new(pcm.clone(), 0, 50, 1);
        assert_eq!(src.next(), Some(1.0));
        assert_eq!(src.next(), Some(2.0));
        // Starved: silence, but the cursor must not advance…
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(src.next(), Some(0.0));
        assert_eq!(src.total_duration(), None, "length unknown mid-decode");
        // …so when the decoder catches up, nothing was skipped.
        pcm.append(&[3.0, 4.0]);
        assert_eq!(src.next(), Some(3.0));
        assert_eq!(src.next(), Some(4.0));
        assert_eq!(src.next(), Some(0.0));
        pcm.finish();
        assert_eq!(src.next(), None);
        assert_eq!(pcm.with(|s| s.to_vec()), vec![1.0, 2.0, 3.0, 4.0]);
    }
}
