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
use ordnung_core::stretch::{hermite, Block, Stretcher};
use rodio::source::Source;
use rodio::{OutputStream, OutputStreamHandle, Sink};
use souvlaki::{
    MediaControlEvent, MediaControls, MediaMetadata, MediaPlayback, MediaPosition, PlatformConfig,
    SeekDirection,
};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
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
    Finished {
        id: Id,
    },
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
    /// The engine's active loop, read at every chunk refill (see [`LoopRegion`]).
    looping: Arc<LoopRegion>,
    pos: usize,
    /// Locally cached run of samples starting at `chunk_start`.
    chunk: Vec<f32>,
    chunk_start: usize,
    sample_rate: u32,
    channels: u16,
    /// The engine's scrub target, read every frame while a scrub is active
    /// (see [`ScrubTarget`]).
    scrub: Arc<ScrubTarget>,
    /// A scrub is in progress: `fpos` is the cursor, not `pos`.
    scrubbing: bool,
    /// Fractional frame position of the scrub cursor.
    fpos: f64,
    /// The interpolated frame being emitted, one sample per channel, and how
    /// many of its samples have gone out. Shared by the scrub and the
    /// varispeed paths, which never run at once.
    frame: Vec<f32>,
    frame_idx: usize,
    /// The engine's pitch fader, read at every frame (see [`PitchState`]).
    pitch: Arc<PitchState>,
    /// How the source is currently reading the buffer (see [`RateMode`]).
    mode: RateMode,
    /// The rate the audio actually runs at. Chases the fader's rate with a
    /// platter's inertia rather than jumping to it (see [`RATE_SLEW_SECS`]).
    cur_rate: f64,
    /// Fractional frame position of the varispeed cursor.
    vpos: f64,
}

/// Which read path a [`BufferSource`] is on. The plain path copies samples
/// out one to one and is bit-exact; the other two are the pitch fader's.
enum RateMode {
    /// Fader at zero: straight copy through `pos`.
    Plain,
    /// Fader off zero, key lock off: a turntable. The cursor `vpos` walks the
    /// buffer at the rate, interpolating between frames, so tempo and pitch
    /// move together.
    Vari,
    /// Fader off zero, key lock on: the WSOLA stretcher changes the tempo and
    /// keeps the pitch.
    Stretch(Stretcher),
}

/// The pitch fader as the audio thread sees it, shared between the engine
/// (set from the player bar) and every [`BufferSource`] the engine builds.
/// `rate` is the playback speed as a multiple of normal (`1.08` is +8%),
/// stored as `f32` bits; `key_lock` chooses between the turntable and the
/// time-stretch paths when the rate is off unity.
pub struct PitchState {
    rate: AtomicU32,
    key_lock: AtomicBool,
}

impl Default for PitchState {
    fn default() -> Self {
        Self {
            rate: AtomicU32::new(1.0f32.to_bits()),
            key_lock: AtomicBool::new(false),
        }
    }
}

impl PitchState {
    fn rate(&self) -> f64 {
        f32::from_bits(self.rate.load(Ordering::Acquire)) as f64
    }
    fn set_rate(&self, rate: f32) {
        self.rate.store(rate.to_bits(), Ordering::Release);
    }
    fn key_lock(&self) -> bool {
        self.key_lock.load(Ordering::Acquire)
    }
}

/// Reach of the pitch fader either side of zero, in percent. A Technics
/// SL-1200's fader: ±8%, which is the range key lock stays clean over too.
pub const PITCH_RANGE_PCT: f32 = 8.0;
/// How long the platter takes to close most of the gap to a moved fader. A
/// quartz-locked direct drive follows the fader quickly but not instantly;
/// the short glide is what makes a nudge sound like a record and not a
/// sample-rate switch, and it keeps a dragged fader free of zipper noise.
const RATE_SLEW_SECS: f64 = 0.08;
/// Within this of unity the slewed rate snaps to exactly 1.0 and the source
/// returns to the bit-exact plain path.
const RATE_UNITY_EPS: f64 = 1e-4;

/// Where the user is holding the record, shared between the engine (set from
/// the zoom lane every frame of a drag) and the playing [`BufferSource`] (which
/// chases it on the audio thread). While `active`, the source stops advancing
/// on its own and instead plays toward `target` at a speed proportional to the
/// gap, in either direction: a quick pull is a chirp, a slow one a growl, a
/// still hand silence, the way a platter in vinyl mode behaves. `target` is an
/// interleaved sample index, frame-aligned.
#[derive(Default)]
pub struct ScrubTarget {
    active: AtomicBool,
    target: AtomicUsize,
}

/// How long the scrub cursor takes to close most of the gap to the hand. Shorter
/// is snappier; longer smooths a jittery pointer into a steadier pitch.
const SCRUB_FOLLOW_SECS: f64 = 0.04;
/// Fastest the scrub plays, as a multiple of normal speed. Caps a flick across
/// the lane at a chirp rather than a burst of noise.
const SCRUB_MAX_RATE: f64 = 6.0;
/// Below this speed a scrub is sub-bass rumble; play silence instead, the
/// record is as good as held still.
const SCRUB_MIN_AUDIBLE_RATE: f64 = 0.05;

/// The active loop, shared between the engine (which sets it from the cue
/// panel) and the playing [`BufferSource`] (which honours it on the audio
/// thread). Bounds are interleaved sample indices, frame-aligned; `end == 0`
/// means no loop. The source clamps each refill chunk to `end` and jumps
/// back to `start` when it gets there, so the loop point is sample-exact and
/// a change takes effect within one chunk (~90 ms).
#[derive(Default)]
pub struct LoopRegion {
    start: AtomicUsize,
    end: AtomicUsize,
}

impl LoopRegion {
    fn get(&self) -> Option<(usize, usize)> {
        let end = self.end.load(Ordering::Acquire);
        (end > 0).then(|| (self.start.load(Ordering::Acquire), end))
    }

    fn set(&self, region: Option<(usize, usize)>) {
        let (start, end) = region.unwrap_or((0, 0));
        self.start.store(start, Ordering::Release);
        self.end.store(end, Ordering::Release);
    }
}

/// How many samples `BufferSource` copies out per lock. ~0.09 s of 48 kHz
/// stereo: small enough to stay responsive, large enough that locking is noise.
const REFILL_SAMPLES: usize = 16_384;

impl BufferSource {
    fn new(
        pcm: Arc<StreamingPcm>,
        looping: Arc<LoopRegion>,
        scrub: Arc<ScrubTarget>,
        pitch: Arc<PitchState>,
        pos: usize,
        sample_rate: u32,
        channels: u16,
    ) -> Self {
        Self {
            pcm,
            looping,
            pos,
            chunk: Vec::new(),
            chunk_start: pos,
            sample_rate,
            channels,
            scrub,
            scrubbing: false,
            fpos: 0.0,
            frame: Vec::new(),
            frame_idx: 0,
            pitch,
            mode: RateMode::Plain,
            cur_rate: 1.0,
            vpos: 0.0,
        }
    }

    /// Make sure the local chunk covers interleaved samples `need0..need1`
    /// (both within `published`), refilling a window centred on `need0` when
    /// it doesn't, so a cursor moving in either direction reads locally for a
    /// while before locking again. Shared by the scrub and varispeed paths.
    fn ensure_window(&mut self, need0: usize, need1: usize, published: usize) {
        let chunk_end = self.chunk_start + self.chunk.len();
        if need0 >= self.chunk_start && need1 <= chunk_end {
            return;
        }
        let ch = self.channels.max(1) as usize;
        let start = need0.saturating_sub(REFILL_SAMPLES / 2) / ch * ch;
        let data = self.pcm.data.read().unwrap();
        let end = (start + REFILL_SAMPLES).max(need1).min(published).min(data.len());
        self.chunk_start = start;
        self.chunk.clear();
        self.chunk.extend_from_slice(&data[start..end]);
    }

    /// Sample `idx` from the local chunk, silence outside it.
    #[inline]
    fn at(&self, idx: usize) -> f32 {
        idx.checked_sub(self.chunk_start)
            .and_then(|k| self.chunk.get(k))
            .copied()
            .unwrap_or(0.0)
    }

    /// Move the fader's rate toward the target with the platter's inertia,
    /// snapping to exactly unity when it gets there.
    fn slew_rate(&mut self, frames: f64) {
        let target = self.pitch.rate();
        let k = (frames / (self.sample_rate.max(1) as f64 * RATE_SLEW_SECS)).min(1.0);
        self.cur_rate += (target - self.cur_rate) * k;
        if target == 1.0 && (self.cur_rate - 1.0).abs() < RATE_UNITY_EPS {
            self.cur_rate = 1.0;
        }
    }

    /// Leave whatever rate path is active, putting its position into `pos`
    /// so the plain path (or the scrub) carries on from the same frame.
    fn sync_to_plain(&mut self) {
        let ch = self.channels.max(1) as usize;
        let frame = match &self.mode {
            RateMode::Plain => return,
            RateMode::Vari => self.vpos,
            RateMode::Stretch(st) => st.input_position(),
        };
        self.pos = (frame.max(0.0).round() as usize) * ch;
        self.mode = RateMode::Plain;
    }

    /// Pick the read path for the current rate and key-lock setting, carrying
    /// the position across so a switch is heard as a change of speed, not a
    /// jump. Returns the mode to use this sample.
    fn choose_mode(&mut self) {
        let ch = self.channels.max(1) as usize;
        let key_lock = self.pitch.key_lock();
        let want_plain = self.cur_rate == 1.0 && self.pitch.rate() == 1.0;
        match (&self.mode, want_plain, key_lock) {
            (RateMode::Plain, true, _) | (RateMode::Vari, false, false) => {}
            (RateMode::Stretch(_), false, true) => {}
            (_, true, _) => self.sync_to_plain(),
            (_, false, false) => {
                let frame = match &self.mode {
                    RateMode::Plain => (self.pos / ch) as f64,
                    RateMode::Vari => self.vpos,
                    RateMode::Stretch(st) => st.input_position(),
                };
                self.vpos = frame;
                self.frame_idx = ch;
                self.mode = RateMode::Vari;
            }
            (_, false, true) => {
                let frame = match &self.mode {
                    RateMode::Plain => (self.pos / ch) as f64,
                    RateMode::Vari => self.vpos,
                    RateMode::Stretch(st) => st.input_position(),
                };
                self.mode =
                    RateMode::Stretch(Stretcher::new(self.sample_rate, ch, frame));
            }
        }
    }

    /// Loop bounds in frames, when a loop is set and well-formed.
    fn loop_frames(&self) -> Option<(usize, usize)> {
        let ch = self.channels.max(1) as usize;
        self.looping
            .get()
            .map(|(a, b)| (a / ch, b / ch))
            .filter(|(a, b)| b > a)
    }

    /// One sample of varispeed audio: the record read at `cur_rate`, a frame
    /// interpolated between its neighbours at each frame boundary.
    fn vari_sample(&mut self) -> Option<f32> {
        let ch = self.channels.max(1) as usize;
        if self.frame_idx >= ch {
            match self.fill_vari_frame() {
                Some(true) => self.frame_idx = 0,
                // Starved: silence, cursor held.
                Some(false) => return Some(0.0),
                None => return None,
            }
        }
        let s = self.frame.get(self.frame_idx).copied().unwrap_or(0.0);
        self.frame_idx += 1;
        Some(s)
    }

    /// Advance the varispeed cursor one output frame and interpolate the
    /// frame there. `Some(true)` filled `frame`; `Some(false)` means the
    /// decoder hasn't reached it yet (nothing advanced); `None` is the end.
    fn fill_vari_frame(&mut self) -> Option<bool> {
        let ch = self.channels.max(1) as usize;
        self.slew_rate(1.0);
        let published = self.pcm.published_len() / ch * ch;
        let frames = published / ch;
        // Need frames i-1..=i+2 around the cursor for the cubic.
        let i = self.vpos.max(0.0) as usize;
        if i + 2 >= frames {
            if self.pcm.is_done() {
                return None;
            }
            return Some(false);
        }
        let t = (self.vpos - i as f64) as f32;
        let i0 = i.saturating_sub(1);
        self.ensure_window(i0 * ch, (i + 3) * ch, published);
        self.frame.clear();
        for c in 0..ch {
            let y0 = self.at(i0 * ch + c);
            let y1 = self.at(i * ch + c);
            let y2 = self.at((i + 1) * ch + c);
            let y3 = self.at((i + 2) * ch + c);
            self.frame.push(hermite(y0, y1, y2, y3, t));
        }
        self.vpos += self.cur_rate;
        if let Some((a, b)) = self.loop_frames() {
            if self.vpos >= b as f64 {
                self.vpos -= (b - a) as f64;
            }
        }
        Some(true)
    }

    /// One sample of key-locked audio out of the stretcher, building the next
    /// block when the current one is spent.
    fn stretch_sample(&mut self) -> Option<f32> {
        let ch = self.channels.max(1) as usize;
        let (hop, at_frame) = match &mut self.mode {
            RateMode::Stretch(st) => {
                if let Some(s) = st.pop() {
                    return Some(s);
                }
                (st.hop() as f64, st.input_position())
            }
            _ => return Some(0.0),
        };
        let published = self.pcm.published_len() / ch * ch;
        let frames = published / ch;
        let done = self.pcm.is_done();
        if done && at_frame >= frames as f64 {
            return None;
        }
        let looping = self.loop_frames();
        // Slew once per block by the frames the block spans.
        self.slew_rate(hop);
        let rate = self.cur_rate;
        let pcm = &self.pcm;
        let RateMode::Stretch(st) = &mut self.mode else {
            return Some(0.0);
        };
        let block = st.next_block(rate, |first, buf| {
            let n = buf.len() / ch;
            // Frames past the loop end read from its start; frames past the
            // decoded frontier of an unfinished decode mean "not yet".
            let map = |f: i64| -> Option<usize> {
                if f < 0 {
                    return None;
                }
                let mut f = f as usize;
                if let Some((a, b)) = looping {
                    if f >= b {
                        f = a + (f - b) % (b - a);
                    }
                }
                Some(f)
            };
            if !done {
                let last = (0..n).filter_map(|i| map(first + i as i64)).max().unwrap_or(0);
                if last + 1 > frames {
                    return false;
                }
            }
            let data = pcm.data.read().unwrap();
            for i in 0..n {
                let dst = &mut buf[i * ch..(i + 1) * ch];
                match map(first + i as i64) {
                    Some(f) if (f + 1) * ch <= published.min(data.len()) => {
                        dst.copy_from_slice(&data[f * ch..(f + 1) * ch]);
                    }
                    _ => dst.fill(0.0),
                }
            }
            true
        });
        match block {
            Block::Ready => {
                // Fold the stretcher's cursor at the loop end so its nominal
                // position circles the loop with the audio (the reads already
                // wrapped through `map`).
                if let Some((a, b)) = looping {
                    st.wrap(b as f64, (b - a) as f64);
                }
                Some(st.pop().unwrap_or(0.0))
            }
            Block::Starved => Some(0.0),
        }
    }

    /// One sample of scrub audio: the next channel of the current interpolated
    /// frame, computing a fresh frame at each frame boundary.
    fn scrub_sample(&mut self) -> f32 {
        let ch = self.channels.max(1) as usize;
        if !self.scrubbing {
            // Grabbed: the scrub cursor picks up where normal playback was.
            self.scrubbing = true;
            self.fpos = (self.pos / ch) as f64;
            self.frame_idx = ch;
        }
        if self.frame_idx >= ch {
            self.fill_scrub_frame();
            self.frame_idx = 0;
        }
        let s = self.frame.get(self.frame_idx).copied().unwrap_or(0.0);
        self.frame_idx += 1;
        s
    }

    /// Advance the scrub cursor one output frame toward the target and fill
    /// `frame` with the linearly interpolated samples there (silence when the
    /// cursor is still or barely moving).
    fn fill_scrub_frame(&mut self) {
        let ch = self.channels.max(1) as usize;
        self.frame.clear();
        self.frame.resize(ch, 0.0);
        let target = (self.scrub.target.load(Ordering::Acquire) / ch) as f64;
        let dist = target - self.fpos;
        if dist.abs() < 1.0 {
            self.fpos = target;
            return;
        }
        let follow = (self.sample_rate.max(1) as f64 * SCRUB_FOLLOW_SECS).max(1.0);
        let rate = (dist / follow).clamp(-SCRUB_MAX_RATE, SCRUB_MAX_RATE);
        self.fpos += rate;
        if rate.abs() < SCRUB_MIN_AUDIBLE_RATE {
            return;
        }
        let published = self.pcm.published_len() / ch * ch;
        if published < 2 * ch {
            return;
        }
        let max_frame = (published / ch - 2) as f64;
        let fpos = self.fpos.clamp(0.0, max_frame);
        let i = fpos as usize;
        let t = (fpos - i as f64) as f32;
        let (need0, need1) = (i * ch, (i + 2) * ch);
        self.ensure_window(need0, need1, published);
        for c in 0..ch {
            let a = self.at(need0 + c);
            let b = self.at(need0 + ch + c);
            self.frame[c] = a + (b - a) * t;
        }
    }
}

impl Iterator for BufferSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        if self.scrub.active.load(Ordering::Acquire) {
            // The hand overrides the fader: the scrub picks up from wherever
            // the rate path had got to.
            self.sync_to_plain();
            return Some(self.scrub_sample());
        }
        if self.scrubbing {
            // Released: the plain cursor carries on from where the scrub got
            // to. The engine rebuilds the sink at the exact release point
            // anyway; this only covers the frames until it does.
            self.scrubbing = false;
            let ch = self.channels.max(1) as usize;
            self.pos = (self.fpos.max(0.0) as usize) * ch;
        }
        // The pitch fader: off zero, the record is read at a rate, either as
        // a turntable would or through the key-locked stretcher. At zero the
        // plain path below copies samples through untouched.
        self.choose_mode();
        match self.mode {
            RateMode::Plain => {}
            RateMode::Vari => return self.vari_sample(),
            RateMode::Stretch(_) => return self.stretch_sample(),
        }
        loop {
            let chunk_end = self.chunk_start + self.chunk.len();
            if self.pos >= self.chunk_start && self.pos < chunk_end {
                let s = self.chunk[self.pos - self.chunk_start];
                self.pos += 1;
                return Some(s);
            }
            // At the loop's end, wrap to its start before refilling; the
            // chunk below then stops exactly at the end so the wrap lands
            // on the sample.
            let looping = self.looping.get();
            if let Some((start, end)) = looping {
                if self.pos >= end && start < end {
                    self.pos = start;
                }
            }
            let len = self.pcm.published_len();
            if self.pos < len {
                let data = self.pcm.data.read().unwrap();
                let mut end = len.min(self.pos + REFILL_SAMPLES).min(data.len());
                if let Some((_, loop_end)) = looping {
                    if loop_end > self.pos {
                        end = end.min(loop_end);
                    }
                }
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
    /// Where a new track starts, as a fraction of its length: `None` for
    /// the top. Set from the "start mid-song" setting; a load may name its
    /// own point instead ([`play_or_toggle_from`](Self::play_or_toggle_from)).
    start_fraction: Option<f32>,
    /// The point the track now loading (or held back, see `pending_start`)
    /// was asked to start at, as a fraction of its length. Fixed at the
    /// load so a settings change mid-decode doesn't move it.
    load_start: Option<f32>,
    /// A start point (seconds) the decoder hasn't reached yet. The sink is
    /// held back until it has, so the listener hears the drop and not a
    /// blip of intro first; `poll` starts it the moment the frontier passes.
    /// The track reads as loading meanwhile.
    pending_start: Option<f32>,
    /// Cancels the in-flight decode thread when the track is superseded, so a
    /// skipped-past long file doesn't keep a core busy for minutes.
    load_cancel: Option<Arc<AtomicBool>>,
    /// The active loop as the audio thread sees it (sample bounds), shared
    /// with every `BufferSource` the engine builds.
    loop_region: Arc<LoopRegion>,
    /// The active loop in seconds, `(start, end)`, for the wall-clock
    /// position and the UI. `None` when playback runs straight through.
    loop_secs: Option<(f32, f32)>,
    /// The scrub target as the audio thread sees it, shared with every
    /// `BufferSource` the engine builds.
    scrub: Arc<ScrubTarget>,
    /// The pitch fader as the audio thread sees it, shared with every
    /// `BufferSource` the engine builds.
    pitch: Arc<PitchState>,
    /// The fader's position in percent, `-PITCH_RANGE_PCT..=PITCH_RANGE_PCT`.
    pitch_pct: f32,
    /// `Some` while the user holds the waveform: whether playback resumes on
    /// release (it was playing at the grab, and no pause toggled since). The
    /// sink runs throughout to voice the scrub, so this, not the sink, is the
    /// play state the UI and the OS panel see meanwhile.
    scrub_resume: Option<bool>,
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
            loop_region: Arc::new(LoopRegion::default()),
            loop_secs: None,
            scrub: Arc::new(ScrubTarget::default()),
            pitch: Arc::new(PitchState::default()),
            pitch_pct: 0.0,
            scrub_resume: None,
            start_fraction: None,
            load_start: None,
            pending_start: None,
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
        if self.loading == Some(id) || (self.current == Some(id) && self.pending_start.is_some()) {
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

    /// The track being decoded, before it has begun to play, if any.
    pub fn loading(&self) -> Option<Id> {
        self.loading
    }

    /// Where the track on its way in will start, as a fraction of its
    /// length, while it is still loading or held back for the decoder to
    /// reach that point. `None` once it plays (or nothing is loading). A
    /// scrubber holds its playhead here meanwhile instead of showing the
    /// top, which is where the clock sits until the sink comes up.
    pub fn held_start(&self) -> Option<f32> {
        if self.loading.is_some() || self.pending_start.is_some() {
            Some(self.load_start.unwrap_or(0.0))
        } else {
            None
        }
    }

    /// True while audio is loading or actively playing — the caller uses this to
    /// keep repainting so `poll` runs, the scrubber animates, and end-of-track is
    /// noticed promptly.
    pub fn is_active(&self) -> bool {
        self.loading.is_some() || self.pending_start.is_some() || self.is_playing()
    }

    /// Where new tracks start: `Some(fraction)` of the length, `None` for
    /// the top. Takes effect from the next track loaded.
    pub fn set_start_fraction(&mut self, fraction: Option<f32>) {
        self.start_fraction = fraction.map(|f| f.clamp(0.0, 0.95));
    }

    /// Give up on a held-back start: the sink comes up at the requested
    /// point if the decoder has reached it, else at the top. Any control
    /// that needs a running sink (pause, seek, scrub) calls this first.
    fn settle_pending_start(&mut self) {
        if let Some(t) = self.pending_start.take() {
            let at = if self.decoded_secs() >= t { t } else { 0.0 };
            self.start_sink_at(at);
        }
    }

    /// True when a sink exists and is running (not paused, not finished). While
    /// the waveform is held, the sink runs to voice the scrub whatever the play
    /// state, so this reports the state playback returns to on release instead.
    fn is_playing(&self) -> bool {
        if let Some(resume) = self.scrub_resume {
            return resume;
        }
        self.started_at.is_some()
            && self
                .sink
                .as_ref()
                .map_or(false, |s| !s.is_paused() && !s.empty())
    }

    /// Current playback position in seconds, clamped to the track length.
    pub fn position(&self) -> f32 {
        // The clock runs at the fader's rate: +8% covers 8% more of the
        // record per second. `set_pitch` rebases the clock on every change so
        // the rate only ever applies to the time since it was set.
        let mut p = match self.started_at {
            Some(t) => self.base_secs + t.elapsed().as_secs_f32() * self.rate(),
            None => self.base_secs,
        };
        // The audio thread wraps at the loop end; fold the wall clock the
        // same way so the playhead circles the loop with it. Not while the
        // record is held: a scrub can be dragged straight out of the loop.
        if let (Some((a, b)), None) = (self.loop_secs, self.scrub_resume) {
            if b > a && p >= b {
                p = a + (p - a) % (b - a);
            }
        }
        p.clamp(0.0, self.duration)
    }

    /// Grab the record at the current position. Playback stops advancing on
    /// its own; until [`end_scrub`], the audio thread instead plays toward
    /// wherever [`scrub_to`] points, at the speed the hand moves, so a drag is
    /// heard as the waveform passing under the playhead. The play/pause state
    /// at the grab is remembered and restored on release.
    pub fn begin_scrub(&mut self) {
        if self.current.is_none() || self.pcm_buf.is_none() || self.scrub_resume.is_some() {
            return;
        }
        self.settle_pending_start();
        let was_playing = self.is_playing();
        let now = self.position();
        self.scrub_resume = Some(was_playing);
        self.scrub.target.store(self.sample_index(now), Ordering::Release);
        self.scrub.active.store(true, Ordering::Release);
        // The source needs a running sink to voice the scrub; a paused or
        // finished one is rebuilt at the grab point.
        let running = self
            .sink
            .as_ref()
            .map_or(false, |s| !s.is_paused() && !s.empty());
        if !running {
            self.start_sink_at(now);
        }
        // The clock freezes: the position is wherever the hand puts it.
        self.base_secs = now;
        self.started_at = None;
        self.status_dirty = true;
    }

    /// Move the held record to `secs`. The reported position follows at once;
    /// the audio chases it within a few tens of milliseconds.
    pub fn scrub_to(&mut self, secs: f32) {
        if self.scrub_resume.is_none() {
            return;
        }
        let secs = secs.clamp(0.0, self.decoded_secs());
        self.base_secs = secs;
        self.scrub.target.store(self.sample_index(secs), Ordering::Release);
    }

    /// Let go of the record at `secs`. Playback resumes from there if it was
    /// playing at the grab, or sits there paused if it wasn't.
    pub fn end_scrub(&mut self, secs: f32) {
        let Some(resume) = self.scrub_resume.take() else {
            return;
        };
        self.scrub.active.store(false, Ordering::Release);
        // `seek` keeps the play state it finds, so restore it first: the sink
        // has been running for the scrub whatever the state was.
        self.started_at = resume.then(Instant::now);
        self.seek(secs);
    }

    /// Seconds of audio playable right now: the whole track once decoded, else
    /// what the decoder has published so far.
    fn decoded_secs(&self) -> f32 {
        let mut max = self.duration;
        if !self.decode_done {
            if let Some(pcm) = &self.pcm_buf {
                let per_sec = (self.sample_rate.max(1) as usize) * self.channels.max(1) as usize;
                max = max.min(pcm.published_len() as f32 / per_sec as f32);
            }
        }
        max
    }

    /// `secs` as a frame-aligned interleaved sample index into the loaded PCM.
    fn sample_index(&self, secs: f32) -> usize {
        let ch = self.channels.max(1) as usize;
        let frame = (secs.max(0.0) * self.sample_rate as f32) as usize;
        let published = self.pcm_buf.as_ref().map_or(0, |p| p.published_len());
        (frame * ch).min(published / ch * ch)
    }

    /// The active loop `(start, end)` in seconds, if playback is looping.
    pub fn active_loop(&self) -> Option<(f32, f32)> {
        self.loop_secs
    }

    /// Start looping between `start` and `end` seconds, or stop looping with
    /// `None`. Playback that is past the loop's end jumps to its start; before
    /// the start, it runs into the loop. Clearing keeps the current position.
    pub fn set_loop(&mut self, region: Option<(f32, f32)>) {
        if self.current.is_none() {
            return;
        }
        // Rebase the clock on the position as the old loop folded it, so the
        // new fold (or none) starts from where the playhead actually is.
        let now = self.position();
        self.base_secs = now;
        if self.started_at.is_some() {
            self.started_at = Some(Instant::now());
        }
        let region = region.filter(|(a, b)| b > a && *a >= 0.0);
        match region {
            Some((a, b)) => {
                let b = b.min(self.duration.max(a + 0.01));
                self.loop_secs = Some((a, b));
                let ch = self.channels.max(1) as usize;
                let frame = |secs: f32| (secs * self.sample_rate as f32) as usize * ch;
                self.loop_region.set(Some((frame(a), frame(b))));
                if now >= b {
                    self.seek(a);
                }
            }
            None => {
                self.loop_secs = None;
                self.loop_region.set(None);
            }
        }
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

    /// Play control click for a row: start `id` (at the engine's start
    /// point, see [`set_start_fraction`](Self::set_start_fraction)), or — if
    /// it's already the loaded track — toggle pause/resume.
    pub fn play_or_toggle(&mut self, id: Id, path: PathBuf) {
        let start = self.start_fraction;
        self.play_or_toggle_from(id, path, start);
    }

    /// [`play_or_toggle`](Self::play_or_toggle) with the start point named
    /// by the caller: `Some(fraction)` of the length, `None` for the top.
    /// This load alone; the engine's own setting stands for the next.
    pub fn play_or_toggle_from(&mut self, id: Id, path: PathBuf, start: Option<f32>) {
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
        self.load_start = start.map(|f| f.clamp(0.0, 0.95));
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
                |start| {
                    format.set(Some((
                        start.sample_rate,
                        start.channels,
                        start.total_frames,
                    )))
                },
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
        if let Some(resume) = self.scrub_resume.as_mut() {
            // Pausing mid-scrub decides what happens on release; the sink
            // keeps running to voice the hand until then.
            *resume = !*resume;
            self.status_dirty = true;
            return;
        }
        self.settle_pending_start();
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
        if self.scrub_resume.is_some() {
            self.scrub_to(secs);
            return;
        }
        // A seek while the start is still held back is the listener taking
        // over: the track was on its way to playing, so it plays from here.
        let was_playing = self.is_playing() || self.pending_start.take().is_some();
        let target = secs.clamp(0.0, self.decoded_secs());
        if let Some((a, b)) = self.loop_secs {
            if target < a || target >= b {
                self.loop_secs = None;
                self.loop_region.set(None);
            }
        }
        self.start_sink_at(target);
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
                    self.loop_region.clone(),
                    self.scrub.clone(),
                    self.pitch.clone(),
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

    /// Playback speed as a multiple of normal, from the fader's percent.
    fn rate(&self) -> f32 {
        1.0 + self.pitch_pct / 100.0
    }

    /// The pitch fader's position in percent, `-8..=8`.
    pub fn pitch(&self) -> f32 {
        self.pitch_pct
    }

    /// Move the pitch fader to `percent` (clamped to ±[`PITCH_RANGE_PCT`]).
    /// The audio thread picks the new rate up within a frame and glides to it;
    /// the wall clock is rebased so the position keeps tracking the audio.
    /// Takes effect with nothing loaded too, so a fader left off zero applies
    /// to the next record like a turntable's would.
    pub fn set_pitch(&mut self, percent: f32) {
        let percent = if percent.is_finite() {
            percent.clamp(-PITCH_RANGE_PCT, PITCH_RANGE_PCT)
        } else {
            0.0
        };
        if percent == self.pitch_pct {
            return;
        }
        let now = self.position();
        self.pitch_pct = percent;
        self.pitch.set_rate(self.rate());
        if self.current.is_some() {
            self.base_secs = now;
            if self.started_at.is_some() {
                self.started_at = Some(Instant::now());
            }
        }
    }

    /// Whether key lock (master tempo) is on: the fader then changes the
    /// tempo and leaves the pitch alone.
    pub fn key_lock(&self) -> bool {
        self.pitch.key_lock()
    }

    /// Switch key lock on or off. Applies to the running audio at once.
    pub fn set_key_lock(&mut self, on: bool) {
        self.pitch.key_lock.store(on, Ordering::Release);
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
        self.pending_start = None;
        self.load_start = None;
        self.started_at = None;
        self.base_secs = 0.0;
        self.duration = 0.0;
        self.loop_secs = None;
        self.loop_region.set(None);
        self.scrub.active.store(false, Ordering::Release);
        self.scrub_resume = None;
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
                    self.loop_secs = None;
                    self.loop_region.set(None);
                    self.scrub.active.store(false, Ordering::Release);
                    self.scrub_resume = None;
                    // Mid-song start: only with a header-supplied length to
                    // take the fraction of. A headerless stream starts at
                    // the top, as it can't know where its middle is.
                    let start = match self.load_start {
                        Some(f) if self.duration > 0.0 => self.duration * f,
                        _ => 0.0,
                    };
                    if start <= self.decoded_secs() {
                        self.pending_start = None;
                        self.start_sink_at(start);
                    } else {
                        self.pending_start = Some(start);
                        self.base_secs = start;
                    }
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
        // The decoder has reached a held-back start point (or finished short
        // of it): the sink comes up there now.
        if let Some(t) = self.pending_start {
            if self.decode_done || self.decoded_secs() >= t {
                self.pending_start = None;
                self.start_sink_at(t.min(self.decoded_secs()));
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
        let src = BufferSource::new(finished_pcm(vec![0.0; 100]), Arc::new(LoopRegion::default()), Arc::new(ScrubTarget::default()), Arc::new(PitchState::default()), 0, 50, 1);
        assert_eq!(src.sample_rate(), 50);
        assert_eq!(src.channels(), 1);
        assert_eq!(src.total_duration(), Some(Duration::from_secs_f32(2.0)));
        assert_eq!(src.count(), 100);

        // Stereo: 100 interleaved samples = 50 frames at 50 Hz = 1 s.
        let stereo = BufferSource::new(finished_pcm(vec![0.0; 100]), Arc::new(LoopRegion::default()), Arc::new(ScrubTarget::default()), Arc::new(PitchState::default()), 0, 50, 2);
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
        let mut src = BufferSource::new(
            pcm.clone(),
            Arc::new(LoopRegion::default()),
            Arc::new(ScrubTarget::default()),
            Arc::new(PitchState::default()),
            0,
            50,
            1,
        );
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

    /// A loop wraps on the sample: reaching `end` continues from `start`,
    /// however the refill chunks fall, and clearing it lets the source run
    /// out to the real end.
    #[test]
    fn buffer_source_loops_between_bounds_until_cleared() {
        let pcm = finished_pcm((0..10).map(|i| i as f32).collect());
        let region = Arc::new(LoopRegion::default());
        region.set(Some((2, 5)));
        let mut src = BufferSource::new(pcm, region.clone(), Arc::new(ScrubTarget::default()), Arc::new(PitchState::default()), 0, 50, 1);
        let first: Vec<f32> = (0..9).map(|_| src.next().unwrap()).collect();
        assert_eq!(first, vec![0.0, 1.0, 2.0, 3.0, 4.0, 2.0, 3.0, 4.0, 2.0]);
        region.set(None);
        let rest: Vec<f32> = src.by_ref().collect();
        assert_eq!(rest, vec![3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
    }

    /// Grabbing the record stops it: with the target where the cursor is, the
    /// source emits silence and stays put, so a held hand is heard as a hold.
    #[test]
    fn scrub_holds_still_in_silence() {
        let pcm = finished_pcm((0..100).map(|i| 1.0 + i as f32).collect());
        let scrub = Arc::new(ScrubTarget::default());
        let mut src = BufferSource::new(pcm, Arc::new(LoopRegion::default()), scrub.clone(), Arc::new(PitchState::default()), 10, 50, 1);
        scrub.target.store(10, Ordering::Release);
        scrub.active.store(true, Ordering::Release);
        for _ in 0..200 {
            assert_eq!(src.next(), Some(0.0));
        }
        // Released without moving: playback carries on from the grab point.
        scrub.active.store(false, Ordering::Release);
        assert_eq!(src.next(), Some(11.0));
        assert_eq!(src.next(), Some(12.0));
    }

    /// Pulling the target ahead plays forward through the audio toward it,
    /// pulling it back plays the same audio in reverse, and the cursor settles
    /// at the target on release. Samples are the ramp `i`, so the emitted
    /// values are the positions passed through.
    #[test]
    fn scrub_plays_toward_the_target_in_either_direction() {
        let pcm = finished_pcm((0..20_000).map(|i| i as f32).collect());
        let scrub = Arc::new(ScrubTarget::default());
        let mut src = BufferSource::new(
            pcm,
            Arc::new(LoopRegion::default()),
            scrub.clone(),
            Arc::new(PitchState::default()),
            1_000,
            48_000,
            1,
        );
        scrub.active.store(true, Ordering::Release);
        scrub.target.store(10_000, Ordering::Release);
        let fwd: Vec<f32> = (0..4_000).filter_map(|_| src.next()).filter(|s| *s != 0.0).collect();
        assert!(fwd.len() > 100, "a pull forward is audible");
        assert!(fwd.windows(2).all(|w| w[1] > w[0]), "forward means rising positions");
        assert!(*fwd.first().unwrap() > 1_000.0 && *fwd.last().unwrap() < 10_000.0);
        assert!(fwd.windows(2).all(|w| w[1] - w[0] <= SCRUB_MAX_RATE as f32 + 1e-3), "rate is capped");

        // Now pull back past where we started.
        scrub.target.store(500, Ordering::Release);
        let back: Vec<f32> = (0..20_000).filter_map(|_| src.next()).filter(|s| *s != 0.0).collect();
        assert!(back.len() > 100, "a pull back is audible");
        assert!(back.windows(2).all(|w| w[1] < w[0]), "reverse means falling positions");
        // The audible tail ends where the crawl drops under the audible floor
        // (`SCRUB_MIN_AUDIBLE_RATE` × the follow window); the cursor then
        // settles the rest of the way in silence.
        let floor = SCRUB_MIN_AUDIBLE_RATE * 48_000.0 * SCRUB_FOLLOW_SECS;
        let last = *back.last().unwrap();
        assert!(last > 500.0 && last - 500.0 < floor as f32 + 2.0, "fades out just short of the target: {last}");

        scrub.active.store(false, Ordering::Release);
        let resumed = src.next().unwrap();
        assert!((resumed - 500.0).abs() < 2.0, "plain playback resumes where the scrub settled: {resumed}");
    }

    /// The fader off zero with key lock off is a turntable: the ramp `i`
    /// comes out climbing ~1.08 per sample once the platter has caught up,
    /// so tempo and pitch both rose 8%. Back at zero the plain path resumes
    /// where the varispeed cursor left off, bit-exact.
    #[test]
    fn vari_reads_the_record_faster_and_hands_back_to_plain() {
        let pcm = finished_pcm((0..200_000).map(|i| i as f32).collect());
        let pitch = Arc::new(PitchState::default());
        let mut src = BufferSource::new(
            pcm,
            Arc::new(LoopRegion::default()),
            Arc::new(ScrubTarget::default()),
            pitch.clone(),
            0,
            48_000,
            1,
        );
        assert_eq!(src.next(), Some(0.0));
        pitch.set_rate(1.08);
        // Past the slew (~0.4 s), the step settles at the rate.
        let settled: Vec<f32> = (0..30_000).filter_map(|_| src.next()).collect();
        let tail = &settled[25_000..];
        for w in tail.windows(2) {
            assert!((w[1] - w[0] - 1.08).abs() < 1e-2, "step {}", w[1] - w[0]);
        }
        pitch.set_rate(1.0);
        let back: Vec<f32> = (0..30_000).filter_map(|_| src.next()).collect();
        let last = *back.last().unwrap();
        assert!(matches!(src.mode, RateMode::Plain), "snapped back to the plain path");
        assert_eq!(src.next(), Some(last + 1.0), "plain path continues on the sample");
    }

    /// Key lock on: a 200 Hz tone stretched 8% faster keeps its 200 Hz.
    #[test]
    fn key_lock_keeps_the_pitch() {
        let sr = 8_000u32;
        let pcm = finished_pcm(
            (0..sr * 6)
                .map(|i| (i as f32 * 200.0 * std::f32::consts::TAU / sr as f32).sin())
                .collect(),
        );
        let pitch = Arc::new(PitchState::default());
        pitch.set_rate(1.08);
        pitch.key_lock.store(true, Ordering::Release);
        let mut src = BufferSource::new(
            pcm,
            Arc::new(LoopRegion::default()),
            Arc::new(ScrubTarget::default()),
            pitch,
            0,
            sr,
            1,
        );
        let out: Vec<f32> = (0..sr * 2).filter_map(|_| src.next()).collect();
        assert!(matches!(src.mode, RateMode::Stretch(_)));
        let second = &out[sr as usize..];
        let rising = second.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((195..=205).contains(&rising), "{rising} cycles/s, want ~200");
    }

    /// A rate off unity still honours the loop: the varispeed cursor folds at
    /// the loop's end and never plays past it.
    #[test]
    fn vari_wraps_at_the_loop() {
        let pcm = finished_pcm((0..100_000).map(|i| i as f32).collect());
        let region = Arc::new(LoopRegion::default());
        region.set(Some((1_000, 2_000)));
        let pitch = Arc::new(PitchState::default());
        pitch.set_rate(0.92);
        let mut src = BufferSource::new(
            pcm,
            region,
            Arc::new(ScrubTarget::default()),
            pitch,
            1_000,
            48_000,
            1,
        );
        let out: Vec<f32> = (0..20_000).filter_map(|_| src.next()).collect();
        assert!(out.iter().all(|s| *s >= 999.0 && *s < 2_001.0), "stayed in the loop");
        assert!(out.windows(2).filter(|w| w[1] < w[0]).count() >= 10, "wrapped several times");
    }

    /// Scrubbing an interleaved stereo buffer keeps channels aligned: each
    /// emitted frame is a left sample followed by the matching right one.
    #[test]
    fn scrub_keeps_stereo_frames_aligned() {
        // L = frame index, R = frame index + 0.5.
        let mut data = Vec::new();
        for i in 0..5_000 {
            data.push(i as f32);
            data.push(i as f32 + 0.5);
        }
        let pcm = finished_pcm(data);
        let scrub = Arc::new(ScrubTarget::default());
        let mut src = BufferSource::new(pcm, Arc::new(LoopRegion::default()), scrub.clone(), Arc::new(PitchState::default()), 0, 48_000, 2);
        scrub.active.store(true, Ordering::Release);
        scrub.target.store(4_000 * 2, Ordering::Release);
        let out: Vec<f32> = (0..2_000).filter_map(|_| src.next()).collect();
        for pair in out.chunks(2).filter(|p| p[0] != 0.0) {
            assert!((pair[1] - pair[0] - 0.5).abs() < 1e-3, "frame {pair:?} is misaligned");
        }
    }
}
