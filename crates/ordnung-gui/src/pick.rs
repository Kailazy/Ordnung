//! File and folder panels, opened off the frame.
//!
//! A panel opened inside `update` runs AppKit's modal loop *inside* winit's
//! event handler, which is still borrowed for the frame. A drag from that
//! panel over our window makes winit queue the hover event as a block on
//! the very loop the panel is spinning; the block runs, winit re-enters
//! its borrowed handler, panics ("tried to handle event while another event
//! is currently being handled") inside an Objective-C block that cannot
//! unwind, and the process aborts. That was the crash when dragging files
//! out of the "Choose files" window into the app.
//!
//! So no panel runs on the frame. [`open`] puts it on its own thread; rfd
//! hops to the main queue for the modal, which the main loop services
//! between frames with the handler free, so events that arrive while the
//! panel is up (the drag, the drop, a repaint) are handled normally. The
//! answer comes back through a channel that [`Pending::poll`] reads on a
//! later frame, and the [`Then`] the opener queued says what to do with it.

use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};

/// Which panel to show.
pub(crate) enum Panel {
    /// A multi-file open panel limited to `exts`, labelled `name`.
    Files {
        name: &'static str,
        exts: &'static [&'static str],
    },
    /// A folder chooser.
    Folder,
    /// A save panel proposing `file_name`.
    Save { file_name: String },
}

/// What the frame does with the paths once the panel settles. Data only:
/// the match lives in the app, where the jobs are.
pub(crate) enum Then {
    /// Import the picked files into the catalog.
    ImportFiles,
    /// Scan the picked folder into the catalog.
    ScanFolder,
    /// Search the picked folder for the catalog's missing files.
    Relocate,
    /// The library root, asked for by a copy off a stick that found none
    /// configured: keep it, then run that copy.
    LibraryRootFor {
        sources: Vec<PathBuf>,
        vol: PathBuf,
        playlist: Option<String>,
    },
    /// The library root, set from settings.
    LibraryRoot,
    /// The library root, chosen on the tour's folder step.
    TourLibraryRoot,
    /// The output folder of the single-track convert window.
    ConvertOutDir,
    /// The output folder of the batch convert window.
    BatchConvertOutDir,
    /// The default convert output folder in settings.
    ConvertSettingOutDir,
    /// Write `text` to the chosen file and report `count` tracks saved.
    SaveTrackList { text: String, count: usize },
}

/// A panel that is up, or whose answer has not been read yet.
pub(crate) struct Pending {
    rx: Receiver<Vec<PathBuf>>,
    pub(crate) then: Then,
}

impl Pending {
    /// The panel's answer once it has one: the picked paths, or an empty
    /// list for a cancelled panel. `None` while the panel is still up.
    pub(crate) fn poll(&self) -> Option<Vec<PathBuf>> {
        self.rx.try_recv().ok()
    }
}

/// Show `panel` on its own thread and hand back the pending answer. The
/// thread asks `ctx` for a repaint when the panel closes, so the frame that
/// reads the answer runs without waiting for the next input event.
pub(crate) fn open(panel: Panel, then: Then, ctx: egui::Context) -> Pending {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let paths = match panel {
            Panel::Files { name, exts } => rfd::FileDialog::new()
                .add_filter(name, exts)
                .pick_files()
                .unwrap_or_default(),
            Panel::Folder => rfd::FileDialog::new().pick_folder().into_iter().collect(),
            Panel::Save { file_name } => rfd::FileDialog::new()
                .set_file_name(file_name)
                .save_file()
                .into_iter()
                .collect(),
        };
        let _ = tx.send(paths);
        ctx.request_repaint();
    });
    Pending { rx, then }
}
