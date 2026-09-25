//! Who is sounding. Split out of `main.rs`; part of the GUI `App`.
//!
//! The app has two engines that make noise: the file player ([`AudioEngine`],
//! behind the now-playing bar) and the video mini-player ([`webview`]), which a
//! record sheet plays through and the radio drives. Each used to silence the
//! other on its own at the spots that happened to remember to, so a start that
//! reached an engine by any other path paid out as two things playing at once:
//! a record's video kept rolling under a library row clicked while it played.
//!
//! This is now the one place that decides. Every path that is about to make
//! sound claims the floor with [`App::claim_sound`] first, and anything that
//! wants to know what is on air asks [`App::sounding`].
use super::*;

/// One of the app's two sound sources.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sound {
    /// The file player: a local track in the now-playing bar.
    Player,
    /// The video mini-player, whether a record sheet's or the radio's.
    Video,
}

impl App {
    /// What is on air right now, if anything. Derived from the engines every
    /// time rather than remembered: the panel can be closed from its own title
    /// bar and a track runs out on its own, so a stored answer would go stale.
    ///
    /// A loaded video answers even while paused, the same way the space bar
    /// has always treated it: it is the sound the listener reached for last,
    /// and its window is parked off screen, so the app's transport is the only
    /// way back to it.
    pub(crate) fn sounding(&self) -> Option<Sound> {
        if webview::is_open() {
            Some(Sound::Video)
        } else if self.audio.as_ref().is_some_and(|a| a.is_active()) {
            Some(Sound::Player)
        } else {
            None
        }
    }

    /// `who` is about to make sound: silence everyone else first, so only one
    /// thing plays at a time. Call it before starting or resuming an engine.
    ///
    /// The player taking over *closes* the video: a video has nowhere to show
    /// paused, and the radio, which owns the panel while it is on, goes off
    /// with it. The video taking over only *pauses* the player: the bar stays
    /// up with the track loaded, so one click brings it back.
    pub(crate) fn claim_sound(&mut self, who: Sound) {
        match who {
            Sound::Player => {
                if self.radio.on {
                    self.radio_stop("Radio off: the player took over");
                }
                // A sheet's video, or one still rolling from a sheet since
                // closed: the panel keeps playing a record after its sheet is
                // gone, so close it whether or not a sheet is up.
                self.stop_sheet_video();
                if webview::is_open() {
                    webview::close();
                }
            }
            Sound::Video => {
                if let Some(a) = self.audio.as_mut() {
                    if a.is_active() {
                        a.toggle_pause();
                    }
                }
            }
        }
    }
}

/// How far into a track "start mid-song" lands, as a fraction of its length.
/// Past a 32-bar intro and the first build on a club record, but before the
/// breakdown: the part a preview is usually skipping forward to find.
pub(crate) const MID_START_FRACTION: f32 = 0.4;

/// How far one arrow or A/D press moves the playhead, in seconds.
pub(crate) const NUDGE_SECS: f32 = 10.0;

/// A video's mid-song jump, from the seek until the page is seen there.
///
/// The mini-player is a web page, so the jump is a seek fired once the page
/// reports a length, and YouTube does not always keep it: a player still
/// bringing up its streams can rewind to the top a moment later, and the
/// listener hears the intro after all. So the jump is watched: until the
/// page reports a position at (or past) the target it counts as unsettled,
/// and if it is seen back near the top once the page has had time to
/// answer, the seek goes out again, a few times at most.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VideoMidStart {
    /// The YouTube id jumped, so the queue's next video gets its own.
    id: String,
    /// Where it was sent, in seconds.
    target: f32,
    /// When the last seek went out.
    seeked_at: Instant,
    /// Seeks sent so far.
    tries: u8,
    /// The page has been seen at the target (or the retries are spent, or
    /// the listener took over): nothing more to do for this video.
    settled: bool,
}

/// A reported position this close under the target (seconds) counts as
/// having arrived: the answer was asked a poll before the clock read it.
const MID_START_ARRIVED_WITHIN: f32 = 3.0;
/// How long after a seek the page gets to answer before a position back
/// near the top is read as YouTube having undone the jump.
const MID_START_RETRY_AFTER: Duration = Duration::from_millis(700);
/// Seeks per video before giving up and leaving it where it is.
const MID_START_TRIES: u8 = 4;
/// How long to wait for the page to answer at all after a seek before the
/// jump is taken as done: a page that has stopped answering is not one to
/// keep seeking.
const MID_START_UNANSWERED_FOR: Duration = Duration::from_secs(4);

/// What the mid-start watcher does on a frame, see [`VideoMidStart::step`].
#[derive(Debug, PartialEq)]
enum MidStep {
    /// Seek the video to this point (seconds).
    Seek(f32),
    /// Nothing to do this frame.
    Hold,
}

impl VideoMidStart {
    /// A record for a video whose jump is over with, or never wanted: the
    /// video already on air when the setting is switched keeps its place.
    pub(crate) fn settled(id: String) -> Self {
        VideoMidStart {
            id,
            target: 0.0,
            seeked_at: Instant::now(),
            tries: 0,
            settled: true,
        }
    }

    /// Decide this frame's move for the video `id`, which the page reports
    /// at `position` of `duration` seconds; `reported` says that position
    /// is the page's own word since the last seek rather than the seek's
    /// assumption (see [`webview::Transport::reported`]). `this` is the
    /// watcher's record so far, for the same video or an earlier one; the
    /// returned record replaces it.
    fn step(
        this: Option<&VideoMidStart>,
        id: &str,
        position: f32,
        duration: f32,
        reported: bool,
        now: Instant,
    ) -> (VideoMidStart, MidStep) {
        let target = duration * MID_START_FRACTION;
        match this {
            Some(v) if v.id == id => {
                if v.settled {
                    return (v.clone(), MidStep::Hold);
                }
                let mut v = v.clone();
                let since_seek = now.duration_since(v.seeked_at);
                if !reported {
                    // Nothing heard back since the seek: the position on
                    // show is the target itself, and proves nothing yet.
                    if since_seek >= MID_START_UNANSWERED_FOR {
                        v.settled = true;
                    }
                    return (v, MidStep::Hold);
                }
                if position >= v.target - MID_START_ARRIVED_WITHIN {
                    v.settled = true;
                    return (v, MidStep::Hold);
                }
                if since_seek < MID_START_RETRY_AFTER {
                    return (v, MidStep::Hold);
                }
                if v.tries >= MID_START_TRIES {
                    v.settled = true;
                    return (v, MidStep::Hold);
                }
                v.tries += 1;
                v.seeked_at = now;
                let target = v.target;
                (v, MidStep::Seek(target))
            }
            _ => (
                VideoMidStart {
                    id: id.to_string(),
                    target,
                    seeked_at: now,
                    tries: 1,
                    settled: false,
                },
                MidStep::Seek(target),
            ),
        }
    }
}

impl App {
    /// Push the mid-start setting to the file player. The video side reads
    /// the config directly (see [`App::mid_start_video`]); the audio engine
    /// starts its sink off the UI thread's poll, so it holds the fraction.
    pub(crate) fn apply_mid_start(&mut self) {
        let fraction = self.config.mid_start_files.then_some(MID_START_FRACTION);
        if let Some(a) = self.audio.as_mut() {
            a.set_start_fraction(fraction);
        }
    }

    /// Jump a freshly loaded video part-way in, when the setting is on, and
    /// see that it stays there (see [`VideoMidStart`]). The mini-player is
    /// a web page, so a start position can't be handed over with the play
    /// call: the video is seeked the first poll that reports it ready with
    /// a length. Keyed on the YouTube id on air, so the queue's next video
    /// gets its own jump.
    pub(crate) fn mid_start_video(&mut self) {
        if !webview::is_open() {
            self.video_mid_start = None;
            return;
        }
        if !self.config.mid_start_videos {
            return;
        }
        let t = webview::transport();
        if !t.ready || t.duration <= 0.0 {
            return;
        }
        let Some(id) = webview::on_air() else {
            return;
        };
        let (state, step) = VideoMidStart::step(
            self.video_mid_start.as_ref(),
            &id,
            t.position,
            t.duration,
            t.reported,
            Instant::now(),
        );
        self.video_mid_start = Some(state);
        if let MidStep::Seek(secs) = step {
            webview::seek(secs);
        }
    }

    /// The listener moved the video themselves: wherever it is now is where
    /// they want it, so the mid-song jump stops watching it.
    pub(crate) fn video_mid_start_settle(&mut self) {
        if let Some(v) = self.video_mid_start.as_mut() {
            v.settled = true;
        }
    }

    /// Where the video on air is on its way to, as a fraction of its
    /// length, while its mid-song jump is still unconfirmed, or while the
    /// page has yet to report a video at all (then the jump is still to
    /// come). `None` once it is playing where it should. A scrubber holds
    /// its playhead here rather than following the page through the jump.
    pub(crate) fn video_held_start(&self) -> Option<f32> {
        if !webview::is_open() {
            return None;
        }
        let t = webview::transport();
        if !t.ready || t.duration <= 0.0 {
            return Some(if self.config.mid_start_videos {
                MID_START_FRACTION
            } else {
                0.0
            });
        }
        if !self.config.mid_start_videos {
            return None;
        }
        let id = webview::on_air()?;
        match self.video_mid_start.as_ref() {
            Some(v) if v.id == id => (!v.settled).then_some(MID_START_FRACTION),
            // Ready with a length, jump not yet sent: this frame's
            // `mid_start_video` sends it.
            _ => Some(MID_START_FRACTION),
        }
    }

    /// Move whatever is loaded by `delta` seconds: the video if the
    /// mini-player is up, else the track in the player bar, paused or not.
    /// Nothing loaded, nothing happens.
    pub(crate) fn nudge_playhead(&mut self, delta: f32) {
        if webview::is_open() {
            let t = webview::transport();
            if !t.ready {
                return;
            }
            let end = if t.duration > 0.0 { t.duration } else { f32::MAX };
            webview::seek((t.position + delta).clamp(0.0, end));
            self.video_mid_start_settle();
            return;
        }
        if let Some(a) = self.audio.as_mut() {
            if a.current().is_some() {
                let target = (a.position() + delta).max(0.0);
                a.seek(target);
            }
        }
    }
}

#[cfg(test)]
mod mid_start_tests {
    use super::*;

    const DUR: f32 = 300.0;

    fn at(secs: f32) -> Instant {
        Instant::now() + Duration::from_secs_f32(secs)
    }

    const TARGET: f32 = DUR * MID_START_FRACTION;

    #[test]
    fn a_new_video_is_seeked_to_the_fraction_once() {
        let (v, step) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        assert_eq!(step, MidStep::Seek(TARGET));
        assert!(!v.settled);
        // Same frame, page still reports the top: too soon to doubt the seek.
        let (v, step) = VideoMidStart::step(Some(&v), "abc", 0.0, DUR, true, at(0.1));
        assert_eq!(step, MidStep::Hold);
        assert!(!v.settled);
    }

    #[test]
    fn the_seeks_own_assumption_does_not_settle_it() {
        let (v, _) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        // The transport shows the target because the seek wrote it there;
        // the page has not answered since.
        let (v, step) = VideoMidStart::step(Some(&v), "abc", TARGET, DUR, false, at(0.5));
        assert_eq!(step, MidStep::Hold);
        assert!(!v.settled);
        // Still unanswered after the grace: no retry either, only patience.
        let (v, step) = VideoMidStart::step(Some(&v), "abc", TARGET, DUR, false, at(1.5));
        assert_eq!(step, MidStep::Hold);
        assert!(!v.settled);
        // A page that never answers is left alone in the end.
        let (v, step) = VideoMidStart::step(Some(&v), "abc", TARGET, DUR, false, at(4.5));
        assert_eq!(step, MidStep::Hold);
        assert!(v.settled);
    }

    #[test]
    fn a_video_reported_at_the_target_settles() {
        let (v, _) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        let (v, step) = VideoMidStart::step(Some(&v), "abc", TARGET - 2.0, DUR, true, at(0.3));
        assert_eq!(step, MidStep::Hold);
        assert!(v.settled);
        // A later scrub back to the top is the listener's business.
        let (v, step) = VideoMidStart::step(Some(&v), "abc", 0.0, DUR, true, at(5.0));
        assert_eq!(step, MidStep::Hold);
        assert!(v.settled);
    }

    #[test]
    fn a_video_reported_back_at_the_top_after_the_grace_is_seeked_again() {
        let (v, _) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        let (v, step) = VideoMidStart::step(Some(&v), "abc", 0.4, DUR, true, at(1.0));
        assert_eq!(step, MidStep::Seek(TARGET));
        assert_eq!(v.tries, 2);
        assert!(!v.settled);
    }

    #[test]
    fn retries_run_out_and_the_video_is_left_alone() {
        let (mut v, _) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        let mut t = 0.0;
        let mut seeks = 1;
        for _ in 0..10 {
            t += 1.0;
            let (next, step) = VideoMidStart::step(Some(&v), "abc", 0.0, DUR, true, at(t));
            if matches!(step, MidStep::Seek(_)) {
                seeks += 1;
            }
            v = next;
        }
        assert_eq!(seeks, MID_START_TRIES);
        assert!(v.settled);
    }

    #[test]
    fn the_next_video_on_air_gets_its_own_jump() {
        let (v, _) = VideoMidStart::step(None, "abc", 0.0, DUR, true, at(0.0));
        let (v, _) = VideoMidStart::step(Some(&v), "abc", TARGET, DUR, true, at(0.3));
        assert!(v.settled);
        let (v, step) = VideoMidStart::step(Some(&v), "def", 0.0, 200.0, true, at(9.0));
        assert_eq!(step, MidStep::Seek(200.0 * MID_START_FRACTION));
        assert_eq!(v.id, "def");
        assert!(!v.settled);
    }
}
