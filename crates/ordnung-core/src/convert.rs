//! Explicit audio conversion via `ffmpeg`. **Never invoked automatically** — only
//! the `convert` (and later `export`) command paths call this. Conversions write a
//! NEW file by default; in-place replacement is opt-in and handled by the caller.
//!
//! `ffmpeg` is the one subprocess Ordnung shells out to (see `ordnung-architecture`);
//! all DSP/format parsing elsewhere is pure Rust. Engine functions take inputs +
//! config and return data — they never print or prompt.

use crate::error::{Error, Result};
use crate::model::Format;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A user-chosen conversion target. Bitrate applies only to lossy codecs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConvertSpec {
    pub target: Format,
    /// Target bitrate (kbps) for lossy formats; `None` uses the format default.
    pub bitrate_kbps: Option<u32>,
}

impl ConvertSpec {
    /// Effective bitrate for lossy targets (mp3 → 320, aac → 256 by default).
    /// Returns `None` for lossless targets, where bitrate is meaningless.
    pub fn effective_bitrate(&self) -> Option<u32> {
        match self.target {
            Format::Mp3 => Some(self.bitrate_kbps.unwrap_or(320)),
            Format::Aac => Some(self.bitrate_kbps.unwrap_or(256)),
            _ => None,
        }
    }
}

/// The result of converting one file.
#[derive(Debug, Clone)]
pub struct ConvertOutcome {
    /// Where the converted audio now lives.
    pub output_path: PathBuf,
    /// True if the original source file was replaced (in-place).
    pub replaced_source: bool,
}

/// Canonical file extension for a target format.
pub fn target_extension(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "mp3",
        Format::Aac => "m4a", // AAC in an MP4/M4A container — what CDJs expect
        Format::Wav => "wav",
        Format::Aiff => "aiff",
        Format::Flac => "flac",
        Format::Other => "bin",
    }
}

/// The ffprobe `codec_name` we expect in a correctly-converted file. Used to
/// verify the output actually matches the requested target.
fn expected_codec(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "mp3",
        Format::Aac => "aac",
        Format::Wav => "pcm_s16le",
        Format::Aiff => "pcm_s16be",
        Format::Flac => "flac",
        Format::Other => "",
    }
}

/// ffmpeg encoder arguments for a target (codec + quality preset).
fn encoder_args(spec: &ConvertSpec) -> Vec<String> {
    match spec.target {
        Format::Mp3 => vec![
            "-c:a".into(),
            "libmp3lame".into(),
            "-b:a".into(),
            format!("{}k", spec.effective_bitrate().unwrap()),
        ],
        Format::Aac => vec![
            "-c:a".into(),
            "aac".into(),
            "-b:a".into(),
            format!("{}k", spec.effective_bitrate().unwrap()),
        ],
        Format::Flac => vec!["-c:a".into(), "flac".into()],
        Format::Wav => vec!["-c:a".into(), "pcm_s16le".into()],
        Format::Aiff => vec!["-c:a".into(), "pcm_s16be".into()],
        Format::Other => Vec::new(),
    }
}

/// Compute the default output path for a source: same directory (or `out_dir`
/// when given), same stem, with the target extension.
pub fn output_path_for(src: &Path, target: Format, out_dir: Option<&Path>) -> PathBuf {
    let stem = src.file_stem().unwrap_or_default();
    let dir = out_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| src.parent().map(Path::to_path_buf).unwrap_or_default());
    let mut name = PathBuf::from(stem);
    name.set_extension(target_extension(target));
    dir.join(name)
}

/// Like [`output_path_for`], but names the file with an explicit `stem` (e.g. one
/// derived from a track's metadata) instead of the source's filename. The stem
/// must already be a single, filesystem-safe path component — see
/// [`metadata_stem`], which produces exactly that.
pub fn output_path_with_stem(
    src: &Path,
    stem: &str,
    target: Format,
    out_dir: Option<&Path>,
) -> PathBuf {
    let dir = out_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| src.parent().map(Path::to_path_buf).unwrap_or_default());
    let mut name = PathBuf::from(stem);
    name.set_extension(target_extension(target));
    dir.join(name)
}

/// Build a filesystem-safe filename stem from a track's artist/title tags, in the
/// "Artist - Title" convention with graceful fallback: both present → `Artist -
/// Title`; only one present → that one; neither → `None` (the caller then keeps
/// the original filename). The result is sanitized into a single path component
/// (path separators and other reserved characters become `-`), so it is always
/// safe to drop straight into [`output_path_with_stem`]. Returns `None` if the
/// tags exist but sanitize to nothing.
pub fn metadata_stem(artist: Option<&str>, title: Option<&str>) -> Option<String> {
    let artist = artist.map(str::trim).filter(|s| !s.is_empty());
    let title = title.map(str::trim).filter(|s| !s.is_empty());
    let raw = match (artist, title) {
        (Some(a), Some(t)) => format!("{a} - {t}"),
        (Some(a), None) => a.to_string(),
        (None, Some(t)) => t.to_string(),
        (None, None) => return None,
    };
    let cleaned = sanitize_filename(&raw);
    // Reject a name with no alphanumeric content (e.g. tags that were only
    // reserved characters): there's nothing meaningful to name the file, so the
    // caller falls back to the source filename.
    if cleaned.chars().any(char::is_alphanumeric) {
        Some(cleaned)
    } else {
        None
    }
}

/// Replace characters that can't (or shouldn't) appear in a filename with `-`,
/// fold control characters and whitespace runs into single spaces, and strip
/// leading/trailing dots and spaces (which trip up Finder, FAT exports, and
/// Windows). Covers the reserved set across macOS, Windows, and FAT so a name
/// is portable to a CDJ USB.
fn sanitize_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('-'),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c| c == '.' || c == ' ')
        .to_string()
}

/// Convert `src` to `spec`.
///
/// - When `in_place` is false, writes a new file at `dest` and leaves the source
///   untouched. Refuses to overwrite an existing `dest` or the source itself.
/// - When `in_place` is true, encodes to a temp file in the source's directory,
///   then atomically replaces: the new file takes `dest`'s path and the original
///   source is removed if its path differs. The catalog is the caller's concern.
///
/// The output is verified with `ffprobe` to actually carry the target codec.
pub fn convert_file(
    src: &Path,
    spec: &ConvertSpec,
    dest: &Path,
    in_place: bool,
) -> Result<ConvertOutcome> {
    if spec.target == Format::Other {
        return Err(Error::Convert {
            path: src.to_path_buf(),
            msg: "cannot convert to an unknown/unsupported format".into(),
        });
    }
    if !src.exists() {
        return Err(Error::Convert {
            path: src.to_path_buf(),
            msg: "source file does not exist".into(),
        });
    }

    // Where ffmpeg actually writes: a sibling temp file for in-place, else dest.
    let write_to: PathBuf = if in_place {
        // In-place may rename (e.g. metadata-based output names): refuse to clobber
        // an unrelated existing file at `dest`. Replacing the source itself is fine.
        if dest != src && dest.exists() {
            return Err(Error::Convert {
                path: dest.to_path_buf(),
                msg: "in-place output would overwrite a different existing file".into(),
            });
        }
        // Keep the real extension LAST so ffmpeg infers the muxer from it.
        let ext = target_extension(spec.target);
        let stem = dest.file_stem().unwrap_or_default().to_string_lossy().into_owned();
        dest.with_file_name(format!("{stem}.ordnung-tmp.{ext}"))
    } else {
        if dest == src {
            return Err(Error::Convert {
                path: src.to_path_buf(),
                msg: "output path equals source; use --in-place to replace it".into(),
            });
        }
        if dest.exists() {
            return Err(Error::Convert {
                path: dest.to_path_buf(),
                msg: "output already exists (refusing to overwrite)".into(),
            });
        }
        dest.to_path_buf()
    };

    run_ffmpeg(src, spec, &write_to)?;
    verify_codec(&write_to, spec.target)?;

    if in_place {
        // Replace the source: drop the original, then move the temp into place.
        if src != dest && src.exists() {
            std::fs::remove_file(src).map_err(|e| Error::Convert {
                path: src.to_path_buf(),
                msg: format!("could not remove original after conversion: {e}"),
            })?;
        }
        std::fs::rename(&write_to, dest).map_err(|e| Error::Convert {
            path: dest.to_path_buf(),
            msg: format!("could not finalize in-place output: {e}"),
        })?;
        Ok(ConvertOutcome {
            output_path: dest.to_path_buf(),
            replaced_source: true,
        })
    } else {
        Ok(ConvertOutcome {
            output_path: write_to,
            replaced_source: false,
        })
    }
}

fn run_ffmpeg(src: &Path, spec: &ConvertSpec, dest: &Path) -> Result<()> {
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
        .arg(src)
        // Audio only (drop attached cover-art video streams that break some
        // container conversions); carry text metadata across.
        .args(["-vn", "-map_metadata", "0"])
        .args(encoder_args(spec))
        .arg(dest);

    let output = cmd.output().map_err(|e| Error::Convert {
        path: src.to_path_buf(),
        msg: format!("could not run ffmpeg (is it installed?): {e}"),
    })?;

    if !output.status.success() {
        let _ = std::fs::remove_file(dest); // don't leave a partial file
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::Convert {
            path: src.to_path_buf(),
            msg: format!(
                "ffmpeg exited with {}: {}",
                output.status,
                stderr.trim().lines().last().unwrap_or("").trim()
            ),
        });
    }
    Ok(())
}

/// Confirm the produced file carries an audio stream of the expected codec.
fn verify_codec(path: &Path, target: Format) -> Result<()> {
    let output = Command::new("ffprobe")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-select_streams",
            "a:0",
            "-show_entries",
            "stream=codec_name",
            "-of",
            "default=nw=1:nk=1",
        ])
        .arg(path)
        .output()
        .map_err(|e| Error::Convert {
            path: path.to_path_buf(),
            msg: format!("could not run ffprobe to verify output: {e}"),
        })?;

    let codec = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if codec.is_empty() {
        return Err(Error::Convert {
            path: path.to_path_buf(),
            msg: "converted file has no audio stream".into(),
        });
    }
    let want = expected_codec(target);
    if codec != want {
        return Err(Error::Convert {
            path: path.to_path_buf(),
            msg: format!("expected codec `{want}` but produced `{codec}`"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn extensions_and_default_bitrates() {
        assert_eq!(target_extension(Format::Aac), "m4a");
        assert_eq!(target_extension(Format::Flac), "flac");
        let mp3 = ConvertSpec { target: Format::Mp3, bitrate_kbps: None };
        assert_eq!(mp3.effective_bitrate(), Some(320));
        let aac = ConvertSpec { target: Format::Aac, bitrate_kbps: Some(192) };
        assert_eq!(aac.effective_bitrate(), Some(192));
        let flac = ConvertSpec { target: Format::Flac, bitrate_kbps: Some(999) };
        assert_eq!(flac.effective_bitrate(), None, "bitrate is meaningless for lossless");
    }

    #[test]
    fn output_path_swaps_extension_and_honors_out_dir() {
        let src = Path::new("/music/01 - Artist - Title.flac");
        let same_dir = output_path_for(src, Format::Mp3, None);
        assert_eq!(same_dir, Path::new("/music/01 - Artist - Title.mp3"));
        let out = output_path_for(src, Format::Aac, Some(Path::new("/tmp/conv")));
        assert_eq!(out, Path::new("/tmp/conv/01 - Artist - Title.m4a"));
    }

    #[test]
    fn metadata_stem_follows_artist_title_with_fallbacks() {
        // Both tags → "Artist - Title".
        assert_eq!(
            metadata_stem(Some("Zenk"), Some("Nairobi Market")).as_deref(),
            Some("Zenk - Nairobi Market")
        );
        // Only one present → that one (the "use whatever exists" fallback).
        assert_eq!(metadata_stem(None, Some("Nairobi Market")).as_deref(), Some("Nairobi Market"));
        assert_eq!(metadata_stem(Some("Zenk"), None).as_deref(), Some("Zenk"));
        // Blank/whitespace tags count as absent.
        assert_eq!(metadata_stem(Some("  "), Some("")), None);
        assert_eq!(metadata_stem(None, None), None);
    }

    #[test]
    fn metadata_stem_sanitizes_path_illegal_characters() {
        // A slash would otherwise split the path; reserved chars become `-`.
        assert_eq!(
            metadata_stem(Some("AC/DC"), Some("Who Made Who?")).as_deref(),
            Some("AC-DC - Who Made Who-")
        );
        // A name that sanitizes to nothing falls back to None.
        assert_eq!(metadata_stem(Some("/"), Some(":")), None);
    }

    #[test]
    fn output_path_with_stem_renames_to_the_given_stem() {
        let src = Path::new("/music/01 - raw filename [PRP017].flac");
        let dest = output_path_with_stem(src, "Zenk - Nairobi Market", Format::Mp3, None);
        assert_eq!(dest, Path::new("/music/Zenk - Nairobi Market.mp3"));
    }
}
