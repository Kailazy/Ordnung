//! Removable-volume detection for the USB devices view.
//!
//! Pure discovery: enumerate mounted external volumes and note whether each
//! one carries a rekordbox export. No mounting, ejecting, or file mutation —
//! callers (GUI/CLI) decide what to do with a volume.

use std::path::{Path, PathBuf};

/// A mounted volume that isn't the boot disk — a USB stick, SD card, external
/// drive, or network share. Whether it's *physically* removable isn't knowable
/// from the mount point alone, so anything under `/Volumes` except the boot
/// volume qualifies (matching how rekordbox lists devices).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsbVolume {
    /// The volume's display name (its mount-point directory name).
    pub name: String,
    /// Absolute mount path, e.g. `/Volumes/EYEBAGS`.
    pub path: PathBuf,
    /// True when the volume carries a rekordbox export
    /// (`/PIONEER/rekordbox/export.pdb` exists). Such a volume holds derived
    /// metadata (PDB rows + ANLZ files) that goes stale if the audio files are
    /// modified directly — callers should surface that before editing.
    pub is_rekordbox_export: bool,
}

/// Does `root` hold a rekordbox/CDJ export? Players read track metadata from
/// this database (not from the audio files' tags), so its presence means
/// direct file edits won't be visible on a CDJ until the USB is re-exported.
pub fn is_rekordbox_export(root: &Path) -> bool {
    root.join("PIONEER").join("rekordbox").join("export.pdb").is_file()
}

/// Enumerate mounted non-boot volumes, sorted by name. Errors (no `/Volumes`,
/// unreadable entries) yield an empty/partial list rather than failing — the
/// caller polls this, and a transient stat error shouldn't drop the section.
#[cfg(target_os = "macos")]
pub fn detect_volumes() -> Vec<UsbVolume> {
    use std::os::unix::fs::MetadataExt;
    let root_dev = std::fs::metadata("/").map(|m| m.dev()).ok();
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir("/Volumes") else {
        return out;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        // metadata() follows symlinks, so the boot volume's `/Volumes/<name>`
        // symlink resolves to `/` and is skipped by the device-id comparison.
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || root_dev == Some(meta.dev()) {
            continue;
        }
        out.push(UsbVolume {
            name,
            is_rekordbox_export: is_rekordbox_export(&path),
            path,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

#[cfg(not(target_os = "macos"))]
pub fn detect_volumes() -> Vec<UsbVolume> {
    Vec::new()
}
