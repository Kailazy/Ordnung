//! The video mini-player: a floating `NSPanel` holding a `WKWebView`, used by the
//! Vinyl view to play a release's Discogs-listed YouTube videos without leaving
//! the app.
//!
//! Pure GUI presentation (per `ordnung-architecture`): nothing here touches the
//! catalog or any engine, and no audio is downloaded or re-hosted — the panel
//! loads youtube.com itself, exactly as a browser would.
//!
//! The panel is an AppKit *child window* of the main window, so it follows it
//! around the screen and stays above it, but is otherwise independent of egui's
//! render loop. That's what makes video playback possible at all: egui has no
//! way to composite a live web view into its own surface.
//!
//! **Why the panel is normally off screen.** The record sheet draws its own
//! transport (play/pause, scrubber, clock), so the panel's only remaining job
//! is the picture — and a 480px window of styled-down watch page is not worth
//! covering the record for. It is therefore parked off screen by default and
//! brought back on demand. Parked, not ordered out: a window AppKit considers
//! hidden has its media throttled or suspended by WebKit, which would stall the
//! audio. Off screen it is an ordinary visible window that nobody can see.
//!
//! **Why the watch page and not an embed.** The obvious implementation — the
//! `youtube.com/embed/…` IFrame player — does not work inside an app web view.
//! Google refuses it there: the player answers with error 150 ("the owner does
//! not allow embedding") for *every* video, including their own IFrame-API
//! sample and Big Buck Bunny, and including when the page is served from a real
//! loopback origin with a Safari user agent. The same page in Safari plays. So
//! the panel loads the ordinary watch page, which plays fine; the cost is that
//! the embed's `playlist=` chaining is gone, and a record's queue is driven from
//! here instead (see [`poll`]).

/// Start playing `youtube_ids` in order, with `title` on the panel's title bar.
/// Reuses the existing panel when one is already open, so moving to the next
/// track doesn't flash a new window. Returns false when the platform has no
/// mini-player (non-macOS) or the window handle wasn't available this frame —
/// callers fall back to opening the video in a browser.
///
/// Must be called on the UI thread — eframe's `update` always is.
#[cfg(target_os = "macos")]
pub fn play(frame: &eframe::Frame, youtube_ids: &[String], title: &str) -> bool {
    imp::play(frame, youtube_ids, title)
}

#[cfg(not(target_os = "macos"))]
pub fn play(_frame: &eframe::Frame, _youtube_ids: &[String], _title: &str) -> bool {
    false
}

/// Hide the mini-player and stop playback. A no-op when nothing is open.
#[cfg(target_os = "macos")]
pub fn close() {
    imp::close();
}

#[cfg(not(target_os = "macos"))]
pub fn close() {}

/// True while the panel is on screen. Goes false on its own when the user closes
/// the panel with its own close button, which is how the UI notices.
#[cfg(target_os = "macos")]
pub fn is_open() -> bool {
    imp::is_open()
}

#[cfg(not(target_os = "macos"))]
pub fn is_open() -> bool {
    false
}

/// Keep the panel's own playback moving: advance to the next video in the queue
/// when the current one ends, and refresh what [`status`] reports. Cheap and
/// idempotent — call it once per frame while the panel is open.
#[cfg(target_os = "macos")]
pub fn poll() {
    imp::poll();
}

#[cfg(not(target_os = "macos"))]
pub fn poll() {}

/// How long until [`poll`] next has work to do, or `None` when the panel is
/// closed. Callers feed this to `Context::request_repaint_after`, since the
/// panel lives outside egui's event loop and an otherwise idle frame loop would
/// leave [`poll`] uncalled for as long as nothing else woke it.
#[cfg(target_os = "macos")]
pub fn next_poll_in() -> Option<std::time::Duration> {
    imp::next_poll_in()
}

#[cfg(not(target_os = "macos"))]
pub fn next_poll_in() -> Option<std::time::Duration> {
    None
}

/// Where the mini-player's video has got to, as of the last [`poll`]. This is
/// what lets the record sheet draw its own transport instead of leaving the
/// user to hit YouTube's controls inside a 480px panel: the page is asked for
/// its `<video>` position on every tick, and [`toggle_pause`] / [`seek`] push
/// the other way.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Transport {
    /// Seconds into the current video.
    pub position: f32,
    /// Total length in seconds, or 0 while the page hasn't reported one — a
    /// live stream, or a video whose metadata hasn't loaded yet.
    pub duration: f32,
    /// True when the video element exists and isn't paused.
    pub playing: bool,
    /// True once a video element has been found at all. Until then the sheet
    /// shows the bar in its loading state rather than a bogus 0:00 / 0:00.
    pub ready: bool,
}

/// The mini-player's playback position, for drawing a scrubber.
#[cfg(target_os = "macos")]
pub fn transport() -> Transport {
    imp::transport()
}

#[cfg(not(target_os = "macos"))]
pub fn transport() -> Transport {
    Transport::default()
}

/// Play or pause the loaded video, whichever it isn't doing now. The state the
/// sheet's button paints comes from the next [`poll`], not from here — the page
/// is the only authority on whether the video actually moved.
#[cfg(target_os = "macos")]
pub fn toggle_pause() {
    imp::toggle_pause();
}

#[cfg(not(target_os = "macos"))]
pub fn toggle_pause() {}

/// Jump the loaded video to `secs`.
#[cfg(target_os = "macos")]
pub fn seek(secs: f32) {
    imp::seek(secs);
}

#[cfg(not(target_os = "macos"))]
pub fn seek(_secs: f32) {}

/// Show or hide the video panel without interrupting playback. Hidden is the
/// default: the record sheet's own transport is the interface, and the panel is
/// only worth looking at when the user wants the picture.
#[cfg(target_os = "macos")]
pub fn set_video_visible(visible: bool) {
    imp::set_video_visible(visible);
}

#[cfg(not(target_os = "macos"))]
pub fn set_video_visible(_visible: bool) {}

/// Whether the video panel is currently on screen.
#[cfg(target_os = "macos")]
pub fn video_visible() -> bool {
    imp::video_visible()
}

#[cfg(not(target_os = "macos"))]
pub fn video_visible() -> bool {
    false
}

/// What the mini-player is doing, as of the last [`poll`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum PlayerStatus {
    /// Nothing loaded, or the page hasn't reported yet.
    Unknown,
    /// A video element exists and is playing, paused or buffering.
    Running,
    /// The page has been up a while with no video on it — a removed video, or a
    /// consent/sign-in wall. The caller hands these to a real browser.
    Stuck,
}

/// Ask the mini-player what became of the video it was given.
#[cfg(target_os = "macos")]
pub fn status() -> PlayerStatus {
    imp::status()
}

#[cfg(not(target_os = "macos"))]
pub fn status() -> PlayerStatus {
    PlayerStatus::Unknown
}

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;
    use std::time::{Duration, Instant};

    use objc2::rc::Retained;
    use objc2::runtime::AnyObject;
    use objc2_app_kit::{
        NSBackingStoreType, NSColor, NSPanel, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
    };
    use objc2_foundation::{
        MainThreadMarker, NSError, NSNumber, NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL,
    };
    use objc2_web_kit::{
        WKAudiovisualMediaTypes, WKUserContentController, WKUserScript,
        WKUserScriptInjectionTime, WKWebView, WKWebViewConfiguration,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::{PlayerStatus, Transport};

    /// Content size of the panel: 16:9, since the page's own chrome is styled
    /// away and only the player is left (see `ISOLATION_CSS`).
    const W: f64 = 480.0;
    const H: f64 = 270.0;
    const MIN_W: f64 = 320.0;
    const MIN_H: f64 = 180.0;
    /// Where the panel parks when it isn't being shown. Far enough left of any
    /// plausible display arrangement that no screen reaches it, while staying an
    /// ordered-in window so WebKit keeps its media running.
    const OFFSCREEN: NSPoint = NSPoint::new(-20000.0, -20000.0);
    /// Inset from the main window's bottom-right corner on first show.
    const MARGIN: f64 = 24.0;
    /// How often the page is asked what it's doing once it's settled. The answer
    /// drives queue advance and the stuck fallback, neither of which needs to be
    /// tighter than this.
    const POLL_EVERY: Duration = Duration::from_millis(900);
    /// How often it's asked while a video is actually rolling. The sheet draws a
    /// scrubber from these answers, so the cadence is the readout's frame rate:
    /// at 900ms the elapsed time visibly stepped, and the playhead crawled in
    /// jumps. Between ticks the position is extrapolated (see [`transport`]), so
    /// this only has to be tight enough to keep that estimate honest.
    const POLL_EVERY_PLAYING: Duration = Duration::from_millis(250);
    /// How often it's asked while a freshly loaded page hasn't produced a video
    /// yet. Much tighter, because this is the window the user is *watching* —
    /// every tick re-applies the styling to whatever DOM now exists, so the page
    /// snaps to the player as soon as it's there rather than at the next second
    /// boundary.
    const POLL_EVERY_SETTLING: Duration = Duration::from_millis(120);
    /// How long after a load the tight cadence applies, if the page hasn't
    /// started playing before then.
    const SETTLING_FOR: Duration = Duration::from_secs(6);
    /// How long a page may sit with no video element before it counts as stuck.
    /// Generous: a cold watch page on a slow link takes a few seconds.
    const STUCK_AFTER: Duration = Duration::from_secs(12);

    thread_local! {
        /// The one mini-player. Held for the process lifetime (the panel is
        /// hidden, never destroyed) so reopening is instant and a user-moved
        /// panel keeps its position. Main-thread-only by construction — every
        /// entry point below takes a `MainThreadMarker` first.
        static PANEL: RefCell<Option<Mini>> = const { RefCell::new(None) };
    }

    impl Mini {
        /// How long until this panel wants asking again. Tight while a fresh
        /// page is still finding its video, relaxed once it's playing — the
        /// value the GUI's repaint scheduling follows too, via [`next_poll_in`].
        fn poll_interval(&self) -> Duration {
            let settling = self.state.is_empty() || self.state == "novideo";
            if settling && self.loaded_at.elapsed() < SETTLING_FOR {
                POLL_EVERY_SETTLING
            } else if self.state == "playing" {
                POLL_EVERY_PLAYING
            } else {
                POLL_EVERY
            }
        }
    }

    struct Mini {
        panel: Retained<NSPanel>,
        web: Retained<WKWebView>,
        /// Whether a video is loaded and this panel owns the current session.
        /// This — not `isVisible` — is what every entry point below gates on,
        /// because the panel is normally parked off screen while playing and
        /// AppKit would still call that visible. Cleared by [`close`].
        live: bool,
        /// Whether the panel is parked on screen rather than off it. Only ever
        /// true because the user asked for the picture.
        visible: bool,
        /// Videos still to play after the current one, in order.
        queue: Vec<String>,
        /// Panel title, reused when the queue advances on its own.
        title: String,
        /// What the page last said (`playing`, `paused`, `ended`, `novideo`).
        state: String,
        /// Position and length the page last reported, and when it did. The
        /// timestamp is what lets [`transport`] run the playhead forward
        /// between polls instead of stepping it once per tick.
        position: f32,
        duration: f32,
        reported_at: Instant,
        /// When the current video was loaded, for the stuck check.
        loaded_at: Instant,
        /// When the page was last asked.
        polled_at: Instant,
    }

    pub fn play(frame: &eframe::Frame, youtube_ids: &[String], title: &str) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        let Some((first, rest)) = youtube_ids.split_first() else {
            return false;
        };
        let Some(parent) = main_window(frame) else {
            return false;
        };

        PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let mini = slot.get_or_insert_with(|| build(mtm));
            let was_live = mini.live;

            unsafe {
                // Re-parent every time: eframe can recreate the window, and
                // AppKit ignores an add for a parent it already has.
                parent.addChildWindow_ordered(&mini.panel, NSWindowOrderingMode::NSWindowAbove);
            }
            // A new session starts hidden — the sheet's transport is the
            // interface — but one already showing the picture keeps showing it
            // across a queue advance rather than blinking away mid-record.
            if !was_live {
                mini.visible = false;
            }
            mini.live = true;
            place(mini, &parent);
            mini.queue = rest.to_vec();
            mini.title = title.to_string();
            load(mini, first);
            true
        })
    }

    pub fn close() {
        if MainThreadMarker::new().is_none() {
            return;
        }
        PANEL.with(|slot| {
            if let Some(mini) = slot.borrow_mut().as_mut() {
                mini.live = false;
                mini.visible = false;
                mini.queue.clear();
                mini.state.clear();
                mini.position = 0.0;
                mini.duration = 0.0;
                // Navigating away is what actually stops the audio — ordering the
                // window out on its own leaves the video playing behind it.
                blank(&mini.web);
                mini.panel.orderOut(None);
            }
        });
    }

    pub fn is_open() -> bool {
        if MainThreadMarker::new().is_none() {
            return false;
        }
        PANEL.with(|slot| slot.borrow().as_ref().is_some_and(|mini| mini.live))
    }

    pub fn status() -> PlayerStatus {
        if MainThreadMarker::new().is_none() {
            return PlayerStatus::Unknown;
        }
        PANEL.with(|slot| {
            let slot = slot.borrow();
            let Some(mini) = slot.as_ref() else {
                return PlayerStatus::Unknown;
            };
            if !mini.live {
                return PlayerStatus::Unknown;
            }
            match mini.state.as_str() {
                "playing" | "paused" | "ended" => PlayerStatus::Running,
                // A page with no video on it is only a problem once it's had
                // time to load one.
                "novideo" if mini.loaded_at.elapsed() > STUCK_AFTER => PlayerStatus::Stuck,
                _ => PlayerStatus::Unknown,
            }
        })
    }

    pub fn transport() -> Transport {
        if MainThreadMarker::new().is_none() {
            return Transport::default();
        }
        PANEL.with(|slot| {
            let slot = slot.borrow();
            let Some(mini) = slot.as_ref() else {
                return Transport::default();
            };
            if !mini.live {
                return Transport::default();
            }
            let playing = mini.state == "playing";
            // Run the clock forward from the last answer while the video rolls,
            // so the playhead moves every frame rather than once per poll. The
            // next answer corrects it, and the drift in between is bounded by
            // the poll interval.
            let position = if playing {
                mini.position + mini.reported_at.elapsed().as_secs_f32()
            } else {
                mini.position
            };
            let duration = mini.duration;
            Transport {
                position: if duration > 0.0 {
                    position.min(duration)
                } else {
                    position
                },
                duration,
                playing,
                ready: matches!(mini.state.as_str(), "playing" | "paused" | "ended"),
            }
        })
    }

    pub fn toggle_pause() {
        // Assume the flip locally so the button responds on the click rather
        // than at the next poll; the page's own answer overwrites it either way.
        with_video("v.paused?v.play():v.pause()", |mini| {
            mini.state = if mini.state == "playing" {
                "paused".into()
            } else {
                "playing".into()
            };
            mini.reported_at = Instant::now();
        });
    }

    pub fn seek(secs: f32) {
        let secs = secs.max(0.0);
        with_video(&format!("v.currentTime={secs}"), move |mini| {
            // Same reason as the pause flip: the scrubber must land where it
            // was dropped, not snap back until the page catches up.
            mini.position = secs;
            mini.reported_at = Instant::now();
        });
    }

    /// Run `body` against the page's `<video>` (bound as `v`), and apply
    /// `optimistic` to the local state so the UI reflects the command now.
    fn with_video(body: &str, optimistic: impl FnOnce(&mut Mini)) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(mini) = slot.as_mut() else { return };
            if !mini.live {
                return;
            }
            let js = format!("(function(){{var v=document.querySelector('video');if(v){{{body}}}}})()");
            unsafe {
                mini.web
                    .evaluateJavaScript_completionHandler(&NSString::from_str(&js), None);
            }
            optimistic(mini);
        });
    }

    pub fn poll() {
        if MainThreadMarker::new().is_none() {
            return;
        }
        // The advance is decided under the borrow and applied after it, since
        // loading the next video borrows the same slot again.
        let next = PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let mini = slot.as_mut()?;
            if !mini.live {
                return None;
            }
            if mini.polled_at.elapsed() >= mini.poll_interval() {
                mini.polled_at = Instant::now();
                ask_state(&mini.web);
            }
            // Take the next video the moment the current one reports it's done.
            (mini.state == "ended" && !mini.queue.is_empty()).then(|| mini.queue.remove(0))
        });
        if let Some(id) = next {
            PANEL.with(|slot| {
                if let Some(mini) = slot.borrow_mut().as_mut() {
                    load(mini, &id);
                }
            });
        }
    }

    /// How long until [`poll`] would next do something, or `None` when there's
    /// nothing on screen to drive. The GUI turns this into a repaint request:
    /// the panel isn't an egui surface, so without one an idle app never calls
    /// `poll` and the queue, the stuck fallback and the styling all stall.
    pub fn next_poll_in() -> Option<Duration> {
        if MainThreadMarker::new().is_none() {
            return None;
        }
        PANEL.with(|slot| {
            let slot = slot.borrow();
            let mini = slot.as_ref()?;
            if !mini.live {
                return None;
            }
            Some(mini.poll_interval().saturating_sub(mini.polled_at.elapsed()))
        })
    }

    /// Create the panel and its web view. Called once, lazily, the first time a
    /// video is played.
    fn build(mtm: MainThreadMarker) -> Mini {
        let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H));
        let style = NSWindowStyleMask::Titled
            | NSWindowStyleMask::Closable
            | NSWindowStyleMask::Resizable
            | NSWindowStyleMask::UtilityWindow;
        let panel: Retained<NSPanel> = unsafe {
            NSPanel::initWithContentRect_styleMask_backing_defer(
                mtm.alloc(),
                content,
                style,
                NSBackingStoreType::NSBackingStoreBuffered,
                false,
            )
        };
        unsafe {
            // We keep the panel across shows, so AppKit must not free it when
            // the user clicks its close button.
            panel.setReleasedWhenClosed(false);
            // Utility panels hide when the app loses focus by default, which
            // would kill the video the moment the user switched to a browser.
            panel.setHidesOnDeactivate(false);
            panel.setFloatingPanel(true);
            panel.setMinSize(NSSize::new(MIN_W, MIN_H));
            // The panel *is* a video frame, so keep it one: dragging it bigger
            // stays 16:9 rather than letterboxing the video inside a shape the
            // page then has to pad out.
            panel.setContentAspectRatio(NSSize::new(16.0, 9.0));
            // Anything the web view hasn't painted yet (a fresh navigation, a
            // live resize) shows the window itself, which is light by default.
            panel.setBackgroundColor(Some(&NSColor::blackColor()));
        }

        let config = unsafe { WKWebViewConfiguration::new() };
        unsafe {
            // Without this, WebKit blocks autoplay and the page opens paused —
            // the click on the track *is* the user's play action.
            config.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::empty());

            // Style the page down to its player *before it first paints*. The
            // poll below re-applies the same snippet for YouTube's own SPA
            // transitions, but by then the first frame is long gone — without a
            // document-start script the panel shows a slab of unstyled watch
            // page (or bare white) until the first poll tick lands.
            let controller = WKUserContentController::new();
            let script = WKUserScript::initWithSource_injectionTime_forMainFrameOnly(
                mtm.alloc(),
                &NSString::from_str(&inject_js()),
                WKUserScriptInjectionTime::AtDocumentStart,
                true,
            );
            controller.addUserScript(&script);
            config.setUserContentController(&controller);
        }
        let web: Retained<WKWebView> =
            unsafe { WKWebView::initWithFrame_configuration(mtm.alloc(), content, &config) };
        unsafe {
            // The web view draws its own opaque white until the page paints, so
            // the panel's black background never gets a chance to show through
            // during a navigation. Turning off `drawsBackground` (the private
            // but long-stable `NSView` value WebKit reads) lets it through, so a
            // loading panel is black rather than a white slab.
            let _: () = objc2::msg_send![&*web, setValue: &*NSNumber::numberWithBool(false),
                forKey: &*NSString::from_str("drawsBackground")];
        }
        panel.setContentView(Some(&web));

        Mini {
            panel,
            web,
            live: false,
            visible: false,
            queue: Vec::new(),
            title: String::new(),
            state: String::new(),
            position: 0.0,
            duration: 0.0,
            reported_at: Instant::now(),
            loaded_at: Instant::now(),
            polled_at: Instant::now(),
        }
    }

    /// Point the panel at one video's watch page.
    fn load(mini: &mut Mini, id: &str) {
        // Ids are `[A-Za-z0-9_-]` (checked by `ReleaseVideo::youtube_id`), so
        // the URL needs no escaping.
        let url = format!("https://www.youtube.com/watch?v={id}");
        mini.state.clear();
        // The old video's clock must not show under the new one's title for the
        // frame or two before the page answers.
        mini.position = 0.0;
        mini.duration = 0.0;
        mini.reported_at = Instant::now();
        mini.loaded_at = Instant::now();
        // Due immediately: the same tick that reads the player's state also
        // injects the styling, and waiting a full period would show YouTube's
        // chrome for most of a second before it collapses to the video.
        mini.polled_at = Instant::now()
            .checked_sub(POLL_EVERY)
            .unwrap_or_else(Instant::now);
        if !mini.title.is_empty() {
            mini.panel.setTitle(&NSString::from_str(&mini.title));
        }
        navigate(&mini.web, &url);
    }

    fn navigate(web: &WKWebView, url: &str) {
        let Some(nsurl) = (unsafe { NSURL::URLWithString(&NSString::from_str(url)) }) else {
            return;
        };
        let req = unsafe { NSURLRequest::requestWithURL(&nsurl) };
        unsafe {
            let _ = web.loadRequest(&req);
        }
    }

    /// Navigate away from the player, which unloads it and stops the sound.
    /// Deliberately *not* `about:blank`: that paints white, so a close (or the
    /// gap before the next video) flashed a bright rectangle inside an otherwise
    /// black panel. A black data page is the same unload with no flash.
    fn blank(web: &WKWebView) {
        navigate(web, "data:text/html,<body style='margin:0;background:%23000'>");
    }

    /// Styling that strips the watch page down to its player: pin the player
    /// containers to the viewport, let the video letterbox inside them with
    /// `object-fit`, and hide the page's chrome, end screens and overlays.
    ///
    /// Measured stable: sampling `#movie_player`'s rect 10x/second shows a
    /// single distinct rect, matching the viewport exactly, both at the default
    /// size and after the panel is resized. (The gentler alternative — hiding
    /// chrome only and letting YouTube lay the player out itself — leaves a
    /// 30px gap under the video, so this one wins.)
    ///
    /// This is YouTube's own DOM, so a redesign can stop it matching. That is a
    /// cosmetic failure by design — nothing here touches playback, so the worst
    /// case is the page's chrome reappearing, never a dead player.
    ///
    /// Must contain no backtick or `${`, since it is embedded in a JS template
    /// literal below.
    const ISOLATION_CSS: &str = "\
        html, body { overflow: hidden !important; background: #000 !important; \
          margin: 0 !important; padding: 0 !important; \
          width: 100vw !important; height: 100vh !important; } \
        #player, #player-container-outer, #player-container-inner, \
        ytd-player, #movie_player, .html5-video-player { \
          position: fixed !important; top: 0 !important; left: 0 !important; \
          width: 100vw !important; height: 100vh !important; \
          max-width: 100vw !important; max-height: 100vh !important; \
          z-index: 999999 !important; margin: 0 !important; padding: 0 !important; } \
        video.video-stream.html5-main-video { width: 100% !important; \
          height: 100% !important; top: 0 !important; left: 0 !important; \
          object-fit: contain !important; } \
        #masthead-container, #masthead, #secondary, #below, #comments, #chat, \
        #related, ytd-miniplayer, .ytp-endscreen-content, .ytp-ce-element, \
        .ytp-pause-overlay-container, .ytp-suggested-action, .ytp-subscribe-card, \
        tp-yt-paper-dialog, ytd-popup-container { \
          display: none !important; visibility: hidden !important; }";

    /// The self-contained snippet that puts [`ISOLATION_CSS`] on the page, used
    /// both as a document-start `WKUserScript` (so the style is up before the
    /// first paint) and on every state poll (so it survives YouTube's own SPA
    /// navigations, which drop the injected node). Idempotent: the `<style>`
    /// carries an id, and a run that finds it does nothing.
    fn inject_js() -> String {
        format!(
            "(function(){{\
               if(document.getElementById('ordnung-player-style'))return;\
               var s=document.createElement('style');\
               s.id='ordnung-player-style';\
               s.textContent=`{ISOLATION_CSS}`;\
               (document.head||document.documentElement).appendChild(s);\
             }})()"
        )
    }

    /// Ask the page what its video element is doing, and re-apply the styling
    /// while we're in there (see [`inject_js`]). The answer lands
    /// asynchronously in `Mini::state`; nothing waits on it.
    fn ask_state(web: &WKWebView) {
        let js = format!(
            "(function(){{\
               {inject};\
               var v=document.querySelector('video');\
               if(!v)return 'novideo';\
               var s=v.ended?'ended':(v.paused?'paused':'playing');\
               var d=isFinite(v.duration)?v.duration:0;\
               return s+'|'+v.currentTime+'|'+d;\
             }})()",
            inject = inject_js()
        );
        let js: &str = &js;
        let handler = block2::RcBlock::new(move |res: *mut AnyObject, _err: *mut NSError| {
            let state = if res.is_null() {
                String::new()
            } else {
                let obj: &AnyObject = unsafe { &*res };
                let s: Retained<NSString> = unsafe { objc2::msg_send_id![obj, description] };
                s.to_string()
            };
            // `state|position|duration` since the transport landed; a bare
            // word (`novideo`, or an empty answer from a page mid-navigation)
            // still parses, leaving the clock where it was.
            let mut parts = state.split('|');
            let word = parts.next().unwrap_or_default().to_string();
            let pos = parts.next().and_then(|s| s.parse::<f32>().ok());
            let dur = parts.next().and_then(|s| s.parse::<f32>().ok());
            // WebKit runs completion handlers on the main thread, which is the
            // thread that owns `PANEL`.
            PANEL.with(|slot| {
                if let Some(mini) = slot.borrow_mut().as_mut() {
                    mini.state = word;
                    if let Some(p) = pos {
                        mini.position = p;
                        mini.reported_at = Instant::now();
                    }
                    if let Some(d) = dur {
                        mini.duration = d;
                    }
                }
            });
        });
        unsafe {
            web.evaluateJavaScript_completionHandler(&NSString::from_str(js), Some(&handler));
        }
    }

    /// Put the panel where its current visibility says it belongs, and order it
    /// in either way — a parked-off-screen panel is still an ordinary visible
    /// window, which is what keeps WebKit playing its media (see the module
    /// note). Shown, it sits in the main window's bottom-right corner; hidden,
    /// it sits far off the left of every screen.
    ///
    /// Screen coordinates are bottom-left origin, so the corner is
    /// `origin + margin` on Y and the far edge less the panel width on X.
    fn place(mini: &Mini, parent: &NSWindow) {
        let f = mini.panel.frame();
        let origin = if mini.visible {
            let p = parent.frame();
            NSPoint::new(
                p.origin.x + p.size.width - f.size.width - MARGIN,
                p.origin.y + MARGIN,
            )
        } else {
            OFFSCREEN
        };
        unsafe {
            mini.panel.setFrameOrigin(origin);
            mini.panel.orderFront(None);
        }
    }

    pub fn set_video_visible(visible: bool) {
        if MainThreadMarker::new().is_none() {
            return;
        }
        PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let Some(mini) = slot.as_mut() else { return };
            if !mini.live || mini.visible == visible {
                return;
            }
            mini.visible = visible;
            // Re-park against the panel's own parent, so showing it lands it on
            // the main window wherever that has since been moved to.
            let parent = unsafe { mini.panel.parentWindow() };
            match parent {
                Some(parent) => place(mini, &parent),
                // No parent this frame (eframe recreated the window): the next
                // `play` re-parents and places it. Order it in regardless, so a
                // requested show isn't silently dropped.
                None => mini.panel.orderFront(None),
            }
        });
    }

    pub fn video_visible() -> bool {
        if MainThreadMarker::new().is_none() {
            return false;
        }
        PANEL.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|mini| mini.live && mini.visible)
        })
    }

    /// The app's main `NSWindow`, via eframe's AppKit handle. Borrowed for the
    /// duration of the call only — never stashed, since eframe may recreate the
    /// view (same rule as `macos_drag`).
    fn main_window(frame: &eframe::Frame) -> Option<Retained<NSWindow>> {
        let handle = frame.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        let view: Retained<objc2_app_kit::NSView> =
            unsafe { Retained::retain(h.ns_view.as_ptr() as *mut objc2_app_kit::NSView)? };
        view.window()
    }
}
