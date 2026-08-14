//! The video mini-player: a floating `NSPanel` holding a `WKWebView`, used by the
//! Vinyl view to play a release's Discogs-listed YouTube videos without leaving
//! the app.
//!
//! Pure GUI presentation (per `ordnung-architecture`): nothing here touches the
//! catalog or any engine, and no audio is downloaded or re-hosted — the panel
//! loads YouTube's own embedded player, exactly as a browser would.
//!
//! The panel is an AppKit *child window* of the main window, so it follows it
//! around the screen and stays above it, but is otherwise independent of egui's
//! render loop. That's what makes video playback possible at all: egui has no
//! way to composite a live web view into its own surface.

/// Start playing `youtube_ids` in order, with `title` on the panel's title bar.
/// Reuses the existing panel when one is already open, so switching tracks
/// doesn't flash a new window. Returns false when the platform has no
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

#[cfg(target_os = "macos")]
mod imp {
    use std::cell::RefCell;

    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSBackingStoreType, NSPanel, NSWindow, NSWindowOrderingMode, NSWindowStyleMask,
    };
    use objc2_foundation::{
        MainThreadMarker, NSPoint, NSRect, NSSize, NSString, NSURLRequest, NSURL,
    };
    use objc2_web_kit::{WKAudiovisualMediaTypes, WKWebView, WKWebViewConfiguration};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// Content size of the panel: 16:9 at a size that reads as a video without
    /// covering the record grid it was opened from.
    const W: f64 = 480.0;
    const H: f64 = 270.0;
    /// Inset from the main window's bottom-right corner on first show.
    const MARGIN: f64 = 24.0;

    thread_local! {
        /// The one mini-player. Held for the process lifetime (the panel is
        /// hidden, never destroyed) so reopening is instant and a user-moved
        /// panel keeps its position. Main-thread-only by construction — every
        /// entry point below takes a `MainThreadMarker` first.
        static PANEL: RefCell<Option<Mini>> = const { RefCell::new(None) };
    }

    struct Mini {
        panel: Retained<NSPanel>,
        web: Retained<WKWebView>,
    }

    pub fn play(frame: &eframe::Frame, youtube_ids: &[String], title: &str) -> bool {
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };
        if youtube_ids.is_empty() {
            return false;
        }
        let Some(parent) = main_window(frame) else {
            return false;
        };

        PANEL.with(|slot| {
            let mut slot = slot.borrow_mut();
            let mini = slot.get_or_insert_with(|| build(mtm));
            let was_visible = mini.panel.isVisible();

            unsafe {
                mini.panel.setTitle(&NSString::from_str(title));
                // Re-parent every time: eframe can recreate the window, and
                // AppKit ignores an add for a parent it already has.
                parent.addChildWindow_ordered(&mini.panel, NSWindowOrderingMode::NSWindowAbove);
            }
            // Only place the panel when it isn't already up, so a panel the user
            // dragged somewhere stays where they put it.
            if !was_visible {
                position_over(&mini.panel, &parent);
            }
            load(&mini.web, youtube_ids);
            mini.panel.orderFront(None);
            true
        })
    }

    pub fn close() {
        if MainThreadMarker::new().is_none() {
            return;
        }
        PANEL.with(|slot| {
            if let Some(mini) = slot.borrow().as_ref() {
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
        PANEL.with(|slot| {
            slot.borrow()
                .as_ref()
                .is_some_and(|mini| mini.panel.isVisible())
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
            panel.setMinSize(NSSize::new(320.0, 180.0));
        }

        let config = unsafe { WKWebViewConfiguration::new() };
        unsafe {
            // Without this, WebKit blocks autoplay and the embed opens paused —
            // the click on the track *is* the user's play action.
            config.setMediaTypesRequiringUserActionForPlayback(WKAudiovisualMediaTypes::empty());
        }
        let web: Retained<WKWebView> =
            unsafe { WKWebView::initWithFrame_configuration(mtm.alloc(), content, &config) };
        panel.setContentView(Some(&web));

        Mini { panel, web }
    }

    /// Point the web view at YouTube's embedded player for `ids`, playing the
    /// first and queueing the rest. The `playlist` parameter is what makes a
    /// record play through side by side without us driving it.
    fn load(web: &WKWebView, ids: &[String]) {
        let mut url = format!(
            "https://www.youtube.com/embed/{}?autoplay=1&rel=0&playsinline=1",
            ids[0]
        );
        if ids.len() > 1 {
            url.push_str("&playlist=");
            url.push_str(&ids[1..].join(","));
        }
        let Some(nsurl) = (unsafe { NSURL::URLWithString(&NSString::from_str(&url)) }) else {
            return;
        };
        let req = unsafe { NSURLRequest::requestWithURL(&nsurl) };
        unsafe {
            let _ = web.loadRequest(&req);
        }
    }

    /// Navigate to a blank page, which unloads the YouTube player and stops the
    /// sound.
    fn blank(web: &WKWebView) {
        if let Some(nsurl) = unsafe { NSURL::URLWithString(&NSString::from_str("about:blank")) } {
            let req = unsafe { NSURLRequest::requestWithURL(&nsurl) };
            unsafe {
                let _ = web.loadRequest(&req);
            }
        }
    }

    /// Park the panel in the main window's bottom-right corner. Screen
    /// coordinates are bottom-left origin, so the corner is `origin + margin` on
    /// Y and the far edge less the panel width on X.
    fn position_over(panel: &NSPanel, parent: &NSWindow) {
        let p = parent.frame();
        let f = panel.frame();
        let x = p.origin.x + p.size.width - f.size.width - MARGIN;
        let y = p.origin.y + MARGIN;
        unsafe { panel.setFrameOrigin(NSPoint::new(x, y)) };
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
