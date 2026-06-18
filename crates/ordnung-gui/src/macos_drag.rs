//! Native macOS drag-out — start an `NSDraggingSession` carrying the dragged
//! tracks' source files as file URLs, so they can be dropped straight into
//! rekordbox (or Finder) and imported without a Finder round-trip.
//!
//! Pure GUI presentation (per `ordnung-architecture`): nothing here touches the
//! catalog or any engine. The audio files are referenced by path — AppKit copies
//! the file URLs onto the drag pasteboard; the source files are never moved or
//! modified. The drop side (rekordbox/Finder) decides what to do with them.

use std::path::Path;

/// Begin a native drag carrying `paths` as file URLs. Returns true if a session
/// actually started; false if the window handle or the initiating mouse event
/// wasn't available this frame (the caller can simply let the user drag again).
///
/// Must be called on the UI thread — eframe's `update` always is.
#[cfg(target_os = "macos")]
pub fn begin_file_drag(frame: &eframe::Frame, paths: &[&Path]) -> bool {
    imp::begin_file_drag(frame, paths)
}

#[cfg(not(target_os = "macos"))]
pub fn begin_file_drag(_frame: &eframe::Frame, _paths: &[&Path]) -> bool {
    false
}

/// The live mouse position in egui points (top-left origin), queried straight
/// from AppKit. egui's own `pointer.latest_pos()` goes stale during an OS file
/// drag because winit's macOS backend implements `draggingEntered:` but not
/// `draggingUpdated:`, so no cursor events arrive while a file hovers — meaning
/// the catalog can't tell which row a dragged cover image is over. Polling the
/// window's mouse location sidesteps that. `None` off-macOS or if the window
/// handle isn't available this frame.
#[cfg(target_os = "macos")]
pub fn pointer_pos(frame: &eframe::Frame) -> Option<egui::Pos2> {
    imp::pointer_pos(frame)
}

#[cfg(not(target_os = "macos"))]
pub fn pointer_pos(_frame: &eframe::Frame) -> Option<egui::Pos2> {
    None
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, ProtocolObject};
    use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
    use objc2_app_kit::{
        NSApplication, NSDragOperation, NSDraggingContext, NSDraggingItem, NSDraggingSession,
        NSDraggingSource, NSEventType, NSImage, NSPasteboardWriting, NSView, NSWorkspace,
    };
    use objc2_foundation::{
        MainThreadMarker, NSArray, NSObject, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
        NSURL,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    /// Read the current mouse location from the window and convert it to egui
    /// points (top-left origin). AppKit window/view coordinates are bottom-left
    /// origin and already in logical points (matching egui's point space), so the
    /// only conversion needed is flipping Y against the content view's height.
    pub fn pointer_pos(frame: &eframe::Frame) -> Option<egui::Pos2> {
        let handle = frame.window_handle().ok()?;
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return None;
        };
        let view: Retained<NSView> =
            unsafe { Retained::retain(h.ns_view.as_ptr() as *mut NSView)? };
        let window = view.window()?;

        // Mouse location in the window's base (bottom-left) coordinate system,
        // available even while no events flow (i.e. during an OS drag).
        let win_pt = unsafe { window.mouseLocationOutsideOfEventStream() };
        // Window base → content-view coordinates (passing None means "from window").
        let view_pt = view.convertPoint_fromView(win_pt, None);
        let bounds = view.bounds();
        Some(egui::pos2(
            view_pt.x as f32,
            (bounds.size.height - view_pt.y) as f32,
        ))
    }

    pub fn begin_file_drag(frame: &eframe::Frame, paths: &[&Path]) -> bool {
        if paths.is_empty() {
            return false;
        }
        // objc2 requires proof we're on the main thread for these AppKit calls;
        // eframe's update() runs there, so this succeeds. Bail rather than risk
        // UB if it ever doesn't.
        let Some(mtm) = MainThreadMarker::new() else {
            return false;
        };

        // Pull the NSView out of eframe's AppKit window handle, adopting the
        // borrowed (+0) pointer for the duration of this call only. Never stash
        // it across frames — eframe may recreate the surface/view.
        let Ok(handle) = frame.window_handle() else {
            return false;
        };
        let RawWindowHandle::AppKit(h) = handle.as_raw() else {
            return false;
        };
        let view: Retained<NSView> = unsafe {
            match Retained::retain(h.ns_view.as_ptr() as *mut NSView) {
                Some(v) => v,
                None => return false,
            }
        };

        // beginDraggingSession requires the live mouse event that started the
        // drag. eframe fires our drag_started() in the same frame it processed
        // that event, so currentEvent is almost always the matching
        // LeftMouseDragged; guard on the type so a rare miss is a harmless no-op
        // (the user just keeps dragging and the next frame carries a valid event).
        let app = NSApplication::sharedApplication(mtm);
        let Some(event) = app.currentEvent() else {
            return false;
        };
        let ty = unsafe { event.r#type() };
        if ty != NSEventType::LeftMouseDown && ty != NSEventType::LeftMouseDragged {
            return false;
        }

        let items = build_drag_items(paths);
        if items.is_empty() {
            return false;
        }
        let items = NSArray::from_vec(items);

        let source = DragSource::new(mtm);
        let source: &ProtocolObject<dyn NSDraggingSource> = ProtocolObject::from_ref(&*source);

        // Blocks on a nested AppKit modal loop until the drop completes, then
        // returns. AppKit retains `source` for the session's lifetime, so it's
        // fine that our local drops right after.
        unsafe {
            view.beginDraggingSessionWithItems_event_source(&items, &event, source);
        }
        true
    }

    /// One `NSDraggingItem` per file: each writes a file URL to the drag
    /// pasteboard and shows the file's Finder icon. Items are staggered so a
    /// multi-file drag reads as a small stack under the cursor.
    fn build_drag_items(paths: &[&Path]) -> Vec<Retained<NSDraggingItem>> {
        let workspace = unsafe { NSWorkspace::sharedWorkspace() };
        let mut items = Vec::with_capacity(paths.len());
        for (i, path) in paths.iter().enumerate() {
            let path_str = NSString::from_str(&path.to_string_lossy());

            // The file URL is the payload rekordbox/Finder read on drop.
            let url: Retained<NSURL> = unsafe { NSURL::fileURLWithPath(&path_str) };
            let writer: &ProtocolObject<dyn NSPasteboardWriting> = ProtocolObject::from_ref(&*url);
            let item: Retained<NSDraggingItem> = unsafe {
                NSDraggingItem::initWithPasteboardWriter(NSDraggingItem::alloc(), writer)
            };

            // Finder icon as the drag image, staggered per item.
            let icon: Retained<NSImage> = unsafe { workspace.iconForFile(&path_str) };
            let size: NSSize = unsafe { icon.size() };
            let off = (i as f64) * 16.0;
            let frame = NSRect::new(NSPoint::new(off, -off), size);
            let contents: &AnyObject = &icon;
            unsafe { item.setDraggingFrame_contents(frame, Some(contents)) };

            items.push(item);
        }
        items
    }

    declare_class!(
        struct DragSource;

        unsafe impl ClassType for DragSource {
            type Super = NSObject;
            type Mutability = mutability::MainThreadOnly;
            const NAME: &'static str = "OrdnungDragSource";
        }

        impl DeclaredClass for DragSource {
            type Ivars = ();
        }

        unsafe impl NSObjectProtocol for DragSource {}

        unsafe impl NSDraggingSource for DragSource {
            // The one required protocol method. A drop outside our app reports
            // the "outside" context; returning Copy is what makes rekordbox /
            // Finder import the dragged files.
            #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
            unsafe fn source_operation_mask(
                &self,
                _session: &NSDraggingSession,
                _context: NSDraggingContext,
            ) -> NSDragOperation {
                NSDragOperation::Copy
            }
        }
    );

    impl DragSource {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = mtm.alloc().set_ivars(());
            unsafe { msg_send_id![super(this), init] }
        }
    }
}
