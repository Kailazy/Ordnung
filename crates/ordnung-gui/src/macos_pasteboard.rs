//! Native macOS clipboard write carrying both the selected tracks' files and
//! their "Artist – Title" text on the same pasteboard. The paste target picks
//! its representation: Finder pastes the audio files, a text field pastes the
//! Soulseek-style lines.
//!
//! Pure GUI presentation (per `ordnung-architecture`): nothing here touches the
//! catalog or any engine. Files are referenced by URL only — pasting in Finder
//! makes Finder copy them; the sources are never moved or modified.

use std::path::Path;

/// Put `paths` (as file URLs) and `text` (as plain text) on the general
/// pasteboard in one write. Returns true when the pasteboard was written;
/// false off-macOS or if no path produced a valid file URL, in which case the
/// caller should fall back to a plain text copy.
#[cfg(target_os = "macos")]
pub fn copy_files_and_text(paths: &[&Path], text: &str) -> bool {
    imp::copy_files_and_text(paths, text)
}

#[cfg(not(target_os = "macos"))]
pub fn copy_files_and_text(_paths: &[&Path], _text: &str) -> bool {
    false
}

#[cfg(target_os = "macos")]
mod imp {
    use std::path::Path;

    use objc2::rc::Retained;
    use objc2::runtime::ProtocolObject;
    use objc2_app_kit::{
        NSPasteboard, NSPasteboardItem, NSPasteboardTypeFileURL, NSPasteboardTypeString,
        NSPasteboardWriting,
    };
    use objc2_foundation::{NSArray, NSString, NSURL};

    pub fn copy_files_and_text(paths: &[&Path], text: &str) -> bool {
        // One pasteboard item per file, each carrying its file URL. The plain
        // text rides on the *first* item: NSPasteboard's string readers take
        // the first item that offers the type, so text consumers see the
        // Artist – Title lines while Finder gathers file URLs from all items.
        let mut items: Vec<Retained<ProtocolObject<dyn NSPasteboardWriting>>> = Vec::new();
        for path in paths {
            let path_str = NSString::from_str(&path.to_string_lossy());
            let url: Retained<NSURL> = unsafe { NSURL::fileURLWithPath(&path_str) };
            let Some(url_str) = (unsafe { url.absoluteString() }) else {
                continue;
            };
            let item = unsafe { NSPasteboardItem::new() };
            unsafe { item.setString_forType(&url_str, NSPasteboardTypeFileURL) };
            if items.is_empty() {
                unsafe {
                    item.setString_forType(&NSString::from_str(text), NSPasteboardTypeString)
                };
            }
            items.push(ProtocolObject::from_retained(item));
        }
        if items.is_empty() {
            return false;
        }
        let items = NSArray::from_vec(items);
        unsafe {
            let pb = NSPasteboard::generalPasteboard();
            pb.clearContents();
            pb.writeObjects(&items)
        }
    }
}
