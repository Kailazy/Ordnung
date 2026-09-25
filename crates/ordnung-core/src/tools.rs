//! Helper programs Ordnung manages for the user, so nothing has to be
//! installed by hand. Today that is one program: `ffmpeg`, the converter
//! behind [`crate::convert`] (the single subprocess the engine shells out
//! to, per `ordnung-architecture`). A static build pinned to
//! [`FFMPEG_VERSION`] is fetched from the repo's rolling `ffmpeg` GitHub
//! release into `~/.ordnung/bin` the first time the app runs, and again only
//! when a build ships that pins a newer version. The app bundle is replaced
//! on every update while `~/.ordnung` stays, so one download lasts across
//! updates.
//!
//! Engine-shaped: no UI, no policy. The GUI decides when to call
//! [`install_ffmpeg`] and how to show its progress; this module only knows
//! how to fetch, prove and place the binary. The streaming gunzip download
//! it uses, [`download_gunzip`], is the same one the genre database import
//! runs on.

use crate::error::{Error, Result};
use std::io::{BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

/// `~/.ordnung`: the one directory everything the app owns lives in (the
/// catalog, the analysis cache, settings, managed tools). `None` only when
/// `HOME` is unset.
pub fn data_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".ordnung"))
}

/// `~/.ordnung/bin`: where managed tools are installed.
pub fn tool_dir() -> Option<PathBuf> {
    data_dir().map(|d| d.join("bin"))
}

/// The ffmpeg release this build expects. Bump it together with a run of
/// `make ffmpeg-publish`, which uploads the matching assets; an app that
/// finds an older stamp in `~/.ordnung/bin` downloads the new one.
pub const FFMPEG_VERSION: &str = "9.0.2";

/// Rolling GitHub release carrying the per-architecture ffmpeg builds
/// (`tools/publish-ffmpeg.sh` fills it).
const FFMPEG_RELEASE_URL: &str = "https://github.com/Kailazy/Ordnung/releases/download/ffmpeg";

/// Architecture tag in the asset name for the CPU this process runs on. A
/// universal app binary runs natively on either kind of Mac, so the process
/// architecture is the right one to fetch.
fn arch_tag() -> &'static str {
    match std::env::consts::ARCH {
        "aarch64" => "arm64",
        other => other,
    }
}

/// Asset name of the ffmpeg build for this machine, e.g.
/// `ffmpeg-9.0.2-macos-arm64.gz`: one gzipped static binary.
pub fn ffmpeg_asset_name() -> String {
    format!("ffmpeg-{FFMPEG_VERSION}-macos-{}.gz", arch_tag())
}

/// Download URL of [`ffmpeg_asset_name`]. `ORDNUNG_FFMPEG_URL` overrides it
/// so a test harness can serve the file locally.
pub fn ffmpeg_url() -> String {
    if let Some(url) = std::env::var_os("ORDNUNG_FFMPEG_URL") {
        return url.to_string_lossy().into_owned();
    }
    format!("{FFMPEG_RELEASE_URL}/{}", ffmpeg_asset_name())
}

/// Path of the managed ffmpeg binary, present or not.
pub fn managed_ffmpeg() -> Option<PathBuf> {
    tool_dir().map(|d| d.join("ffmpeg"))
}

/// Path of the stamp file recording which version the managed binary is.
fn version_stamp() -> Option<PathBuf> {
    tool_dir().map(|d| d.join("ffmpeg.version"))
}

/// What is installed under `~/.ordnung/bin` relative to [`FFMPEG_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FfmpegStatus {
    /// The managed binary is present and stamped with [`FFMPEG_VERSION`].
    Current,
    /// A managed binary is present but stamped with another version (or an
    /// unreadable stamp, reported as an empty string).
    Outdated(String),
    /// No managed binary. `convert` may still find a Homebrew one.
    Missing,
}

/// Read the stamp next to the managed binary; no network, no subprocess.
pub fn ffmpeg_status() -> FfmpegStatus {
    let (Some(bin), Some(stamp)) = (managed_ffmpeg(), version_stamp()) else {
        return FfmpegStatus::Missing;
    };
    if !bin.is_file() {
        return FfmpegStatus::Missing;
    }
    match std::fs::read_to_string(&stamp) {
        Ok(v) if v.trim() == FFMPEG_VERSION => FfmpegStatus::Current,
        Ok(v) => FfmpegStatus::Outdated(v.trim().to_string()),
        Err(_) => FfmpegStatus::Outdated(String::new()),
    }
}

/// Bytes seen so far of a download, and its size when the server said.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DownloadProgress {
    pub read_bytes: u64,
    pub total_bytes: u64,
}

/// A reader that counts what passes through it, so a decode loop can report
/// download progress without owning the network stream.
pub(crate) struct CountingReader<R: Read> {
    pub(crate) inner: R,
    pub(crate) read: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Stream `url` (a gzip file) and write its decompressed contents to `dest`,
/// replacing whatever was there. `what` names the download in error
/// messages ("the audio converter"). Progress carries compressed bytes, the
/// only count the server sizes. Returns `Ok(false)` when `cancel` was set:
/// `dest` is removed and nothing else is touched. The caller owns proving
/// the finished file is what it wanted.
pub fn download_gunzip(
    url: &str,
    what: &str,
    dest: &Path,
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &AtomicBool,
) -> Result<bool> {
    let agent = ureq::AgentBuilder::new()
        .timeout_read(std::time::Duration::from_secs(120))
        .user_agent("Ordnung/0.1 +https://kailazy.github.io/Ordnung/")
        .build();
    let resp = agent
        .get(url)
        .call()
        .map_err(|e| Error::Network(format!("downloading {what}: {e}")))?;
    let total_bytes: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: resp.into_reader(),
        read: read.clone(),
    };
    let mut gz = flate2::read::GzDecoder::new(BufReader::with_capacity(1 << 20, counting));

    let _ = std::fs::remove_file(dest);
    let io_err = |source: std::io::Error| Error::Io {
        path: dest.to_path_buf(),
        source,
    };
    let mut out = std::io::BufWriter::new(std::fs::File::create(dest).map_err(io_err)?);
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if cancel.load(Ordering::Relaxed) {
            drop(out);
            let _ = std::fs::remove_file(dest);
            return Ok(false);
        }
        let n = gz
            .read(&mut buf)
            .map_err(|e| Error::Network(format!("downloading {what}: {e}")))?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(io_err)?;
        progress(DownloadProgress {
            read_bytes: read.load(Ordering::Relaxed),
            total_bytes,
        });
    }
    out.flush().map_err(io_err)?;
    Ok(true)
}

/// Fetch the ffmpeg build for this machine into `~/.ordnung/bin`, prove it
/// runs, and stamp it with [`FFMPEG_VERSION`]. The download lands in a
/// temp name and is renamed into place only after `ffmpeg -version` answers,
/// so a half-finished or foreign file never shadows a working converter (a
/// conversion running on the old binary keeps its open file). Returns the
/// installed path, or `None` when cancelled.
pub fn install_ffmpeg(
    progress: &mut dyn FnMut(DownloadProgress),
    cancel: &AtomicBool,
) -> Result<Option<PathBuf>> {
    let (Some(dir), Some(bin), Some(stamp)) = (tool_dir(), managed_ffmpeg(), version_stamp())
    else {
        return Err(Error::Invalid(
            "no home directory to install the audio converter into".into(),
        ));
    };
    std::fs::create_dir_all(&dir).map_err(|source| Error::Io {
        path: dir.clone(),
        source,
    })?;
    let tmp = dir.join("ffmpeg.download");
    if !download_gunzip(&ffmpeg_url(), "the audio converter", &tmp, progress, cancel)? {
        return Ok(None);
    }
    let io_err = |source: std::io::Error| Error::Io {
        path: tmp.clone(),
        source,
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(io_err)?;
    }
    if !runs_as_ffmpeg(&tmp) {
        let _ = std::fs::remove_file(&tmp);
        return Err(Error::Network(
            "the downloaded audio converter does not run on this Mac".into(),
        ));
    }
    std::fs::rename(&tmp, &bin).map_err(io_err)?;
    std::fs::write(&stamp, format!("{FFMPEG_VERSION}\n")).map_err(|source| Error::Io {
        path: stamp.clone(),
        source,
    })?;
    Ok(Some(bin))
}

/// `<bin> -version` prints an `ffmpeg version …` banner and exits 0.
fn runs_as_ffmpeg(bin: &Path) -> bool {
    Command::new(bin)
        .arg("-version")
        .output()
        .map(|o| o.status.success() && String::from_utf8_lossy(&o.stdout).starts_with("ffmpeg version"))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn asset_name_pins_version_and_arch() {
        let name = ffmpeg_asset_name();
        assert!(name.starts_with(&format!("ffmpeg-{FFMPEG_VERSION}-macos-")));
        assert!(name.ends_with(".gz"));
        assert!(ffmpeg_url().ends_with(&name));
        assert!(matches!(arch_tag(), "arm64" | "x86_64"));
    }

    #[test]
    fn status_follows_binary_and_stamp() {
        // Point HOME at a scratch dir; the status reads only the file system.
        let tmp = std::env::temp_dir().join(format!("ordnung-tools-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let old = std::env::var_os("HOME");
        std::env::set_var("HOME", &tmp);
        assert_eq!(ffmpeg_status(), FfmpegStatus::Missing);
        let bin = managed_ffmpeg().unwrap();
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, b"").unwrap();
        assert_eq!(ffmpeg_status(), FfmpegStatus::Outdated(String::new()));
        std::fs::write(version_stamp().unwrap(), "1.0\n").unwrap();
        assert_eq!(ffmpeg_status(), FfmpegStatus::Outdated("1.0".into()));
        std::fs::write(version_stamp().unwrap(), format!("{FFMPEG_VERSION}\n")).unwrap();
        assert_eq!(ffmpeg_status(), FfmpegStatus::Current);
        match old {
            Some(h) => std::env::set_var("HOME", h),
            None => std::env::remove_var("HOME"),
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
