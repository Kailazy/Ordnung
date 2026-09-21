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
