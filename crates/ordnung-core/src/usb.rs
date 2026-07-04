//! Removable-volume detection for the USB devices view.
//!
//! Pure discovery: enumerate mounted USB sticks / SD cards / external drives
//! and note whether each carries a rekordbox export. No mounting, ejecting, or
//! file mutation — callers (GUI/CLI) decide what to do with a volume.

use std::path::{Path, PathBuf};

/// A mounted removable or external volume — a USB stick, SD card, or external
/// drive. Classified via DiskArbitration's device description, so mounted disk
/// images, network shares, and other internal-disk volumes don't qualify
/// (matching how rekordbox lists devices).
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

/// Enumerate mounted USB / SD / external-drive volumes, sorted by name.
/// Errors (no `/Volumes`, unreadable entries) yield an empty/partial list
/// rather than failing — the caller polls this, and a transient stat error
/// shouldn't drop the section.
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
        // Classify by the backing device, per DiskArbitration: keep real
        // sticks/cards ("USB", "Secure Digital" — including SD slots that sit
        // on an internal reader) and any other non-internal device (e.g. a
        // Thunderbolt SSD). Drop network shares, mounted disk images
        // ("Virtual Interface"), and other internal-disk volumes. A failed
        // lookup falls through to listing — better to show a stick we
        // couldn't classify than to hide it.
        if let Some(info) = da::device_info(&path) {
            if info.network == Some(true) {
                continue;
            }
            match info.protocol.as_deref() {
                Some("USB") | Some("Secure Digital") => {}
                Some("Virtual Interface") => continue,
                _ if info.internal == Some(false) => {}
                _ => continue,
            }
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

/// Minimal DiskArbitration FFI: ask the OS what device a mount point lives on.
/// Bound by hand (no crate) — three DA calls plus the CF accessors they need.
#[cfg(target_os = "macos")]
mod da {
    use std::ffi::c_void;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    type CFRef = *const c_void;

    #[link(name = "CoreFoundation", kind = "framework")]
    extern "C" {
        fn CFURLCreateFromFileSystemRepresentation(
            alloc: CFRef,
            buffer: *const u8,
            buf_len: isize,
            is_directory: bool,
        ) -> CFRef;
        fn CFDictionaryGetValue(dict: CFRef, key: CFRef) -> CFRef;
        fn CFStringGetCString(s: CFRef, buf: *mut u8, buf_size: isize, encoding: u32) -> bool;
        fn CFBooleanGetValue(b: CFRef) -> bool;
        fn CFRelease(r: CFRef);
    }

    #[link(name = "DiskArbitration", kind = "framework")]
    extern "C" {
        fn DASessionCreate(alloc: CFRef) -> CFRef;
        fn DADiskCreateFromVolumePath(alloc: CFRef, session: CFRef, url: CFRef) -> CFRef;
        fn DADiskCopyDescription(disk: CFRef) -> CFRef;
        static kDADiskDescriptionDeviceProtocolKey: CFRef;
        static kDADiskDescriptionDeviceInternalKey: CFRef;
        static kDADiskDescriptionVolumeNetworkKey: CFRef;
    }

    const CF_STRING_ENCODING_UTF8: u32 = 0x0800_0100;

    /// What the mount's backing device is. Each field is `None` when the
    /// description simply doesn't carry that key.
    pub(super) struct DeviceInfo {
        /// Bus the device hangs off: "USB", "Secure Digital", "Virtual
        /// Interface" (disk image), "PCI-Express", "Apple Fabric", …
        pub protocol: Option<String>,
        /// True for devices inside the machine (the internal SSD).
        pub internal: Option<bool>,
        /// True for network mounts (SMB/AFP/NFS).
        pub network: Option<bool>,
    }

    pub(super) fn device_info(mount: &Path) -> Option<DeviceInfo> {
        unsafe {
            let session = DASessionCreate(std::ptr::null());
            if session.is_null() {
                return None;
            }
            // Every create below is released before return; dictionary VALUES
            // are borrowed from `desc` (CF get rule) and must not be released.
            let bytes = mount.as_os_str().as_bytes();
            let url = CFURLCreateFromFileSystemRepresentation(
                std::ptr::null(),
                bytes.as_ptr(),
                bytes.len() as isize,
                true,
            );
            if url.is_null() {
                CFRelease(session);
                return None;
            }
            let disk = DADiskCreateFromVolumePath(std::ptr::null(), session, url);
            if disk.is_null() {
                CFRelease(url);
                CFRelease(session);
                return None;
            }
            let desc = DADiskCopyDescription(disk);
            let info = if desc.is_null() {
                None
            } else {
                let protocol = {
                    let v = CFDictionaryGetValue(desc, kDADiskDescriptionDeviceProtocolKey);
                    cf_string(v)
                };
                let internal = {
                    let v = CFDictionaryGetValue(desc, kDADiskDescriptionDeviceInternalKey);
                    (!v.is_null()).then(|| CFBooleanGetValue(v))
                };
                let network = {
                    let v = CFDictionaryGetValue(desc, kDADiskDescriptionVolumeNetworkKey);
                    (!v.is_null()).then(|| CFBooleanGetValue(v))
                };
                CFRelease(desc);
                Some(DeviceInfo {
                    protocol,
                    internal,
                    network,
                })
            };
            CFRelease(disk);
            CFRelease(url);
            CFRelease(session);
            info
        }
    }

    /// Copy a borrowed CFString into a Rust `String`. Protocol names are short
    /// ASCII, so a fixed buffer comfortably covers them.
    unsafe fn cf_string(s: CFRef) -> Option<String> {
        if s.is_null() {
            return None;
        }
        let mut buf = [0u8; 128];
        if !CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, CF_STRING_ENCODING_UTF8) {
            return None;
        }
        let end = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
        Some(String::from_utf8_lossy(&buf[..end]).into_owned())
    }
}
