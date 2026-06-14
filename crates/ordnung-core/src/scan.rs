//! Scanning: discover audio files and read their properties + tags.
//!
//! For DJ libraries, tags are often missing but the filename carries
//! "Artist - Title". When a file lacks an artist/title tag, we infer it from the
//! filename so the catalog is useful immediately. Inference only FILLS missing
//! fields — it never overwrites real tags (the explicit-only rule).

use crate::catalog::{MissingTrack, ScannedTrack};
use crate::error::{Error, Result};
use crate::model::{AudioProperties, Format, Id, Tags};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use std::path::Path;
use unicode_normalization::UnicodeNormalization;
use walkdir::WalkDir;

/// Audio extensions Ordnung ingests.
const AUDIO_EXTS: &[&str] = &["mp3", "flac", "aiff", "aif", "wav", "m4a", "aac", "ogg"];

/// True if `path` has an extension Ordnung ingests. Lets callers (e.g. the GUI's
/// drag-and-drop import) accept individual dropped files without listing a dir.
pub fn is_audio_file(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| AUDIO_EXTS.contains(&x.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// Walk `dir` recursively and return the paths of audio files, sorted.
pub fn discover(dir: impl AsRef<Path>) -> Vec<std::path::PathBuf> {
    let mut paths: Vec<_> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.into_path())
        .filter(|p| is_audio_file(p))
        .collect();
    paths.sort();
    paths
}

/// One missing track resolved to a file found on disk: the row `id` to repoint,
/// its `old_path`, and the `new_path` where the source now lives.
#[derive(Debug, Clone)]
pub struct Relocation {
    pub id: Id,
    pub old_path: String,
    pub new_path: std::path::PathBuf,
}

/// Hunt under `search_root` for the source files of tracks that have gone
/// missing, matching by filename and — when several files share that name —
/// confirming identity with the stored content fingerprint. Returns one
/// [`Relocation`] per confidently located file; a track with no name match, or
/// an ambiguous name that the fingerprint can't settle, is left out (better to
/// skip than to repoint at the wrong file).
///
/// Pure: it reads the filesystem but never touches the catalog or any file, so
/// the caller decides whether and how to apply the relinks.
pub fn relocate_missing(missing: &[MissingTrack], search_root: impl AsRef<Path>) -> Vec<Relocation> {
    // Index every audio file under the root by its lowercased filename.
    let mut by_name: std::collections::HashMap<String, Vec<std::path::PathBuf>> =
        std::collections::HashMap::new();
    for p in discover(search_root) {
        if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
            by_name.entry(name.to_lowercase()).or_default().push(p);
        }
    }

    let mut out = Vec::new();
    for m in missing {
        let Some(name) = Path::new(&m.source_path)
            .file_name()
            .and_then(|s| s.to_str())
        else {
            continue;
        };
        let Some(candidates) = by_name.get(&name.to_lowercase()) else {
            continue; // no file by that name lives under the search root
        };
        let chosen = match candidates.as_slice() {
            [only] => Some(only.clone()),
            many => disambiguate_by_fingerprint(many, m.fingerprint.as_deref()),
        };
        if let Some(new_path) = chosen {
            out.push(Relocation {
                id: m.id,
                old_path: m.source_path.clone(),
                new_path,
            });
        }
    }
    out
}

/// From several same-named candidates, pick the one whose content fingerprint
/// matches the missing track's stored fingerprint. Returns `None` if the stored
/// fingerprint is unknown, or if zero/multiple candidates match — an unsettled
/// tie is left for the user rather than guessed.
fn disambiguate_by_fingerprint(
    candidates: &[std::path::PathBuf],
    want: Option<&str>,
) -> Option<std::path::PathBuf> {
    let want = want?;
    let mut matches = candidates.iter().filter(|p| {
        scan_file(p)
            .ok()
            .and_then(|s| s.fingerprint)
            .as_deref()
            == Some(want)
    });
    match (matches.next(), matches.next()) {
        (Some(p), None) => Some(p.clone()),
        _ => None,
    }
}

/// Read one file into a `ScannedTrack`, filling missing artist/title from the
/// filename.
pub fn scan_file(path: impl AsRef<Path>) -> Result<ScannedTrack> {
    let path = path.as_ref();
    // The tag reader is strict: one malformed ID3 frame (odd-length UTF-16, a bad
    // timestamp) or a codec it doesn't tag (Opus-in-Ogg) makes it reject the whole
    // file. That audio is usually perfectly good, so on a tag failure we fall back
    // to a decoder-only scan rather than dropping the track entirely.
    let tagged = match lofty::read_from_path(path) {
        Ok(t) => t,
        Err(tag_err) => return scan_untagged(path, tag_err),
    };

    let props = tagged.properties();
    let properties = AudioProperties {
        sample_rate_hz: props.sample_rate().unwrap_or(0),
        bit_depth: props.bit_depth(),
        channels: props.channels().unwrap_or(0),
        duration_ms: props.duration().as_millis() as u64,
        bitrate_kbps: props.audio_bitrate(),
    };

    let mut tags = Tags::default();
    let mut cover_thumb: Option<Vec<u8>> = None;
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        // Core fields (used by ls, search, writeback)
        tags.title = tag.title().map(|c| nfc(c.as_ref()));
        tags.artist = tag.artist().map(|c| nfc(c.as_ref()));
        tags.album = tag.album().map(|c| nfc(c.as_ref()));
        tags.genre = tag.genre().map(|c| nfc(c.as_ref()));
        tags.comment = tag.comment().map(|c| nfc(c.as_ref()));
        tags.label = str_of(tag, ItemKey::Label);
        tags.year = tag
            .get_string(ItemKey::RecordingDate)
            .or_else(|| tag.get_string(ItemKey::Year))
            .or_else(|| tag.get_string(ItemKey::ReleaseDate))
            .or_else(|| tag.get_string(ItemKey::OriginalReleaseDate))
            .and_then(parse_year);

        // Numbering
        tags.track_number = u16_of(tag, ItemKey::TrackNumber);
        tags.track_total = u16_of(tag, ItemKey::TrackTotal);
        tags.disc_number = u16_of(tag, ItemKey::DiscNumber);
        tags.disc_total = u16_of(tag, ItemKey::DiscTotal);
        tags.movement_number = u16_of(tag, ItemKey::MovementNumber);
        tags.movement_total = u16_of(tag, ItemKey::MovementTotal);

        // People / credits
        tags.album_artist = str_of(tag, ItemKey::AlbumArtist);
        tags.composer = str_of(tag, ItemKey::Composer);
        tags.conductor = str_of(tag, ItemKey::Conductor);
        tags.remixer = str_of(tag, ItemKey::Remixer);
        tags.producer = str_of(tag, ItemKey::Producer);
        tags.lyricist = str_of(tag, ItemKey::Lyricist);
        tags.arranger = str_of(tag, ItemKey::Arranger);
        tags.performer = str_of(tag, ItemKey::Performer);
        tags.mix_dj = str_of(tag, ItemKey::MixDj);
        tags.writer = str_of(tag, ItemKey::Writer);

        // Dates
        tags.recording_date = str_of(tag, ItemKey::RecordingDate);
        tags.release_date = str_of(tag, ItemKey::ReleaseDate);
        tags.original_release_date = str_of(tag, ItemKey::OriginalReleaseDate);

        // Release IDs
        tags.isrc = str_of(tag, ItemKey::Isrc);
        tags.barcode = str_of(tag, ItemKey::Barcode);
        tags.catalog_number = str_of(tag, ItemKey::CatalogNumber);
        tags.publisher = str_of(tag, ItemKey::Publisher);
        tags.copyright = str_of(tag, ItemKey::CopyrightMessage);
        tags.release_country = str_of(tag, ItemKey::ReleaseCountry);

        // DJ-relevant file-embedded values
        tags.bpm_tag = tag
            .get_string(ItemKey::Bpm)
            .or_else(|| tag.get_string(ItemKey::IntegerBpm))
            .and_then(|s| s.trim().parse::<f32>().ok())
            .filter(|b| (1.0..=400.0).contains(b));
        tags.initial_key_tag = str_of(tag, ItemKey::InitialKey);
        tags.mood = str_of(tag, ItemKey::Mood);
        tags.grouping = str_of(tag, ItemKey::ContentGroup);
        tags.compilation = tag
            .get_string(ItemKey::FlagCompilation)
            .map(|s| matches!(s.trim(), "1" | "true" | "True"));

        // Content / descriptive
        tags.subtitle = str_of(tag, ItemKey::TrackSubtitle);
        tags.description = str_of(tag, ItemKey::Description);
        tags.language = str_of(tag, ItemKey::Language);
        tags.script = str_of(tag, ItemKey::Script);
        tags.lyrics = tag
            .get_string(ItemKey::Lyrics)
            .or_else(|| tag.get_string(ItemKey::UnsyncLyrics))
            .map(nfc);
        tags.work = str_of(tag, ItemKey::Work);
        tags.movement = str_of(tag, ItemKey::Movement);

        // Encoder / origin
        tags.encoded_by = str_of(tag, ItemKey::EncodedBy);
        tags.encoder_software = str_of(tag, ItemKey::EncoderSoftware);
        tags.encoder_settings = str_of(tag, ItemKey::EncoderSettings);
        tags.original_artist = str_of(tag, ItemKey::OriginalArtist);
        tags.original_album = str_of(tag, ItemKey::OriginalAlbumTitle);

        // MusicBrainz / AcoustID
        tags.musicbrainz_recording_id = str_of(tag, ItemKey::MusicBrainzRecordingId);
        tags.musicbrainz_track_id = str_of(tag, ItemKey::MusicBrainzTrackId);
        tags.musicbrainz_release_id = str_of(tag, ItemKey::MusicBrainzReleaseId);
        tags.musicbrainz_release_group_id = str_of(tag, ItemKey::MusicBrainzReleaseGroupId);
        tags.musicbrainz_artist_id = str_of(tag, ItemKey::MusicBrainzArtistId);
        tags.musicbrainz_release_artist_id = str_of(tag, ItemKey::MusicBrainzReleaseArtistId);
        tags.musicbrainz_work_id = str_of(tag, ItemKey::MusicBrainzWorkId);
        tags.musicbrainz_release_type = str_of(tag, ItemKey::MusicBrainzReleaseType);
        tags.acoust_id = str_of(tag, ItemKey::AcoustId);

        // ReplayGain — strip trailing "dB" and parse
        tags.replay_gain_track_gain = f32_db(tag, ItemKey::ReplayGainTrackGain);
        tags.replay_gain_track_peak = f32_of(tag, ItemKey::ReplayGainTrackPeak);
        tags.replay_gain_album_gain = f32_db(tag, ItemKey::ReplayGainAlbumGain);
        tags.replay_gain_album_peak = f32_of(tag, ItemKey::ReplayGainAlbumPeak);

        // Cover art — flag + downscaled thumbnail for the catalog/GUI.
        tags.has_cover = tag.picture_count() > 0;
        if let Some(pic) = tag.pictures().first() {
            cover_thumb = thumbnail_png(pic.data(), 96);
        }
    }

    // Fill missing artist/title from the filename — never overwrite real tags.
    // NFC the stem: macOS returns filenames decomposed, so an accented title
    // parsed from the name would otherwise be stored (and shown) as tofu.
    let stem = nfc(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
    let stem = stem.as_str();
    if tags.artist.is_none() || tags.title.is_none() {
        let (artist, title) = parse_filename(stem);
        if tags.artist.is_none() {
            tags.artist = artist;
        }
        if tags.title.is_none() {
            tags.title = title.or_else(|| Some(stem.to_string()));
        }
    }

    let fingerprint = file_fingerprint(path, properties.duration_ms);
    let (src_size, src_mtime) = match fs_signature(path) {
        Some((s, m)) => (Some(s), Some(m)),
        None => (None, None),
    };

    Ok(ScannedTrack {
        source_path: path.to_string_lossy().into_owned(),
        format: format_from_ext(path),
        properties,
        tags,
        cover_thumb,
        fingerprint,
        src_size,
        src_mtime,
    })
}

/// Fallback when the tag reader can't parse a file. If the decoder can still read
/// real audio samples, it's a tag-level defect (a corrupt frame, a codec lofty
/// doesn't tag) on otherwise-good audio — import it with filename-derived
/// artist/title and decoder-probed properties, rather than dropping it. If the
/// decoder also fails, the file itself is broken (a truncated or empty download),
/// and we surface a clear error instead of the cryptic tag-parser message.
fn scan_untagged(path: &Path, tag_err: lofty::error::LoftyError) -> Result<ScannedTrack> {
    let properties = match crate::analysis::decode::probe_for_scan(path) {
        Ok(p) => p,
        Err(_) => {
            return Err(Error::Decode {
                path: path.to_path_buf(),
                msg: format!(
                    "tag read failed ({tag_err}) and no decodable audio found — \
                     file is likely a truncated or incomplete download"
                ),
            })
        }
    };

    // No tags survived the parse failure; derive what we can from the filename.
    let stem = nfc(path.file_stem().and_then(|s| s.to_str()).unwrap_or(""));
    let (artist, title) = parse_filename(&stem);
    let tags = Tags {
        artist,
        title: title.or_else(|| Some(stem.clone())),
        ..Tags::default()
    };

    let fingerprint = file_fingerprint(path, properties.duration_ms);
    let (src_size, src_mtime) = match fs_signature(path) {
        Some((s, m)) => (Some(s), Some(m)),
        None => (None, None),
    };
    Ok(ScannedTrack {
        source_path: path.to_string_lossy().into_owned(),
        format: format_from_ext(path),
        properties,
        tags,
        cover_thumb: None,
        fingerprint,
        src_size,
        src_mtime,
    })
}

/// The source file's `(size_bytes, mtime_seconds)` — a cheap "has this file
/// changed?" signature. A rescan stores this and skips a file whose signature
/// still matches the catalog, so re-adding a folder doesn't re-read every file.
/// `mtime` is whole seconds since the Unix epoch (matching what the catalog
/// stores), so the GUI's pre-check and `scan_file`'s stored value compare equal.
/// Returns `None` if the file can't be stat'd.
pub fn fs_signature(path: impl AsRef<Path>) -> Option<(u64, i64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Some((meta.len(), mtime))
}

/// A cheap, path-independent content fingerprint used to recognize a file that
/// moved or was renamed (see `Catalog::upsert_scanned`).
///
/// It hashes the track duration plus a window of bytes near the END of the file
/// (skipping a possible 128-byte ID3v1 trailer). Two properties matter:
/// * **Move-stable** — renaming or relocating a file doesn't touch its bytes, so
///   the fingerprint is unchanged.
/// * **Retag-tolerant** — ID3v2 header tags and embedded cover art live at the
///   START of the file; hashing the tail keeps the fingerprint stable when those
///   change, and deliberately excludes the file *size* (which a retag shifts).
///
/// This is identity for rematching, not a cryptographic digest — collisions are
/// harmless because a move is only inferred when the old path is also gone and
/// exactly one orphaned row matches. Returns `None` if the file can't be read.
fn file_fingerprint(path: &Path, duration_ms: u64) -> Option<String> {
    use std::hash::{Hash, Hasher};
    use std::io::{Read, Seek, SeekFrom};

    const WINDOW: u64 = 256 * 1024; // bytes of audio tail to hash
    const ID3V1: u64 = 128; // possible trailer to skip

    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let audio_end = len.saturating_sub(ID3V1);
    let start = audio_end.saturating_sub(WINDOW);

    let mut h = std::collections::hash_map::DefaultHasher::new();
    duration_ms.hash(&mut h);
    f.seek(SeekFrom::Start(start)).ok()?;
    let mut buf = Vec::new();
    f.take(audio_end - start).read_to_end(&mut buf).ok()?;
    buf.hash(&mut h);
    Some(format!("{:016x}", h.finish()))
}

/// Largest side (px) we render the inspector cover preview at. The panel only
/// shows it ~256px wide, so anything beyond this is wasted decode/encode/upload
/// work — embedded covers run up to 3000px, which would otherwise re-encode to
/// a ~27-megapixel PNG and stall the UI thread on every track selection.
const PREVIEW_MAX_SIDE: u32 = 1024;

/// Read the front cover embedded in `path` as a PNG sized for the inspector
/// preview (downscaled to at most `PREVIEW_MAX_SIDE` per side; smaller covers
/// are left as-is). Higher quality than the 96px `cover_thumb` stored at scan
/// time, but capped so a giant source image can't jank the UI. Re-encoding to
/// PNG keeps it decodable by PNG-only consumers (the GUI's `image` build).
/// Returns `None` when the file carries no embedded picture or it can't decode.
pub fn read_front_cover_png(path: impl AsRef<Path>) -> Result<Option<Vec<u8>>> {
    let path = path.as_ref();
    let tagged = lofty::read_from_path(path).map_err(|source| Error::Tag {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };
    let Some(pic) = tag.pictures().first() else {
        return Ok(None);
    };
    Ok(thumbnail_png(pic.data(), PREVIEW_MAX_SIDE))
}

/// Decode the embedded cover bytes (typically JPEG or PNG), downscale to a
/// `max_side`-pixel square thumbnail, and re-encode as PNG. Returns `None` if
/// the bytes don't decode as any supported image format.
fn thumbnail_png(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
    use std::io::Cursor;
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(max_side, max_side);
    let mut out = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

fn format_from_ext(path: &Path) -> Format {
    match path
        .extension()
        .and_then(|x| x.to_str())
        .map(|x| x.to_lowercase())
        .as_deref()
    {
        Some("mp3") => Format::Mp3,
        Some("flac") => Format::Flac,
        Some("aiff") | Some("aif") => Format::Aiff,
        Some("wav") => Format::Wav,
        Some("m4a") | Some("aac") => Format::Aac,
        _ => Format::Other,
    }
}

/// Infer (artist, title) from a DJ-style filename stem.
///
/// Handles common shapes:
///   "Artist - Title"
///   "01 - Artist - Title"   / "01 Artist - Title"
///   "01 - 03 - Artist - Title"   (disc/track prefixes)
///   "01 - Title"            -> (None, "Title")
///   "Title"                 -> (None, "Title")
pub fn parse_filename(stem: &str) -> (Option<String>, Option<String>) {
    let cleaned = stem.replace('\u{2010}', "-"); // normalize unicode hyphen
    let parts: Vec<&str> = cleaned.split(" - ").map(str::trim).collect();

    // Drop leading pure-number segments (track/disc numbers).
    let mut segs: Vec<&str> = parts
        .iter()
        .copied()
        .skip_while(|s| is_track_number(s))
        .collect();

    // A bare leading "NN Title" with no " - " split: strip the number prefix.
    if segs.len() == 1 {
        let one = strip_leading_number(segs[0]);
        return (None, non_empty(one));
    }

    if segs.len() >= 2 {
        let artist = segs.remove(0);
        let title = segs.join(" - ");
        return (non_empty(strip_leading_number(artist)), non_empty(title));
    }

    (None, non_empty(cleaned))
}

fn is_track_number(s: &str) -> bool {
    let t = s.trim();
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

/// Strip a leading "NN " or "NN. " track-number prefix from a segment.
fn strip_leading_number(s: &str) -> String {
    let t = s.trim_start();
    let rest = t.trim_start_matches(|c: char| c.is_ascii_digit());
    let rest = rest.trim_start_matches(['.', ')', '-']).trim_start();
    if rest.is_empty() || rest.len() == t.len() {
        t.to_string()
    } else {
        rest.to_string()
    }
}

/// Parse a year from a tag value that may be a bare year or an ISO date.
fn parse_year(s: &str) -> Option<u16> {
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    digits.parse().ok().filter(|&y| (1900..=2200).contains(&y))
}

/// Read a string ItemKey, trimming whitespace and treating empty as absent.
/// Normalized to NFC like the core fields (see `nfc`).
fn str_of(tag: &lofty::tag::Tag, key: ItemKey) -> Option<String> {
    tag.get_string(key)
        .map(|s| nfc(s.trim()))
        .filter(|s| !s.is_empty())
}

/// Normalize text to NFC (precomposed form). macOS hands back filenames — and
/// some embedded tags — in NFD, where e.g. `é` is `e` + a combining accent. The
/// UI font has no glyph for the combining marks (they render as tofu boxes), and
/// NFD also breaks byte-wise search and artist/title de-duplication. Storing NFC
/// fixes all three. Never applied to `source_path` — the filesystem needs the
/// original bytes to open the file.
fn nfc(s: &str) -> String {
    s.nfc().collect()
}

/// Parse a numeric ItemKey as a u16. Handles "3", "3/12" (track), and " 3 ".
fn u16_of(tag: &lofty::tag::Tag, key: ItemKey) -> Option<u16> {
    let s = tag.get_string(key)?;
    let head = s.split(['/', '-']).next()?.trim();
    head.parse().ok()
}

/// Parse a plain float (no unit).
fn f32_of(tag: &lofty::tag::Tag, key: ItemKey) -> Option<f32> {
    tag.get_string(key).and_then(|s| s.trim().parse::<f32>().ok())
}

/// Parse a ReplayGain gain value — these are often `-7.23 dB`; strip the unit.
fn f32_db(tag: &lofty::tag::Tag, key: ItemKey) -> Option<f32> {
    let s = tag.get_string(key)?;
    let cleaned = s
        .trim()
        .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c == ' ');
    cleaned.parse::<f32>().ok()
}

fn non_empty(s: impl Into<String>) -> Option<String> {
    let s = s.into();
    if s.trim().is_empty() {
        None
    } else {
        Some(s.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::{nfc, parse_filename, relocate_missing, scan_file};
    use crate::catalog::MissingTrack;

    fn p(s: &str) -> (Option<String>, Option<String>) {
        parse_filename(s)
    }

    /// A file the tag reader can't parse AND the decoder can't read (empty or just
    /// garbage bytes) is a broken/incomplete download — it must be rejected, not
    /// imported as a phantom track. The error names the real cause.
    #[test]
    fn unreadable_file_is_rejected_with_clear_error() {
        let dir = std::env::temp_dir().join(format!("ordnung-scan-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("00 - Truncated.aif");
        std::fs::write(&empty, b"").unwrap();
        let garbage = dir.join("01 - Garbage.mp3");
        std::fs::write(&garbage, vec![0u8; 4096]).unwrap();

        for f in [&empty, &garbage] {
            let err = scan_file(f).unwrap_err().to_string();
            assert!(
                err.contains("truncated")
                    || err.contains("no decodable audio")
                    || err.contains("incomplete"),
                "expected a broken-download error, got: {err}"
            );
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A track whose file moved to a new subfolder is found by filename and
    /// repointed; a track whose file isn't under the root is left untouched.
    #[test]
    fn relocate_finds_moved_file_by_name_and_skips_absent_ones() {
        let root = std::env::temp_dir().join(format!("ordnung-relocate-{}", std::process::id()));
        let moved_to = root.join("New Location");
        std::fs::create_dir_all(&moved_to).unwrap();
        // The real file now lives in a different subfolder than the catalog records.
        let real = moved_to.join("A2 Zenk - Nairobi Market [PRP017].flac");
        std::fs::write(&real, b"not real audio, just a name to match").unwrap();

        let missing = vec![
            MissingTrack {
                id: 1,
                // Old path: same filename, a folder that no longer exists.
                source_path: "/old/place/A2 Zenk - Nairobi Market [PRP017].flac".into(),
                fingerprint: None,
            },
            MissingTrack {
                id: 2,
                source_path: "/old/place/Nonexistent Song.flac".into(),
                fingerprint: None,
            },
        ];

        let found = relocate_missing(&missing, &root);

        assert_eq!(found.len(), 1, "only the present file is relocated");
        assert_eq!(found[0].id, 1);
        assert_eq!(found[0].new_path, real, "repointed at the file's new home");

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn nfc_recomposes_decomposed_accents() {
        // "Liège Trinité" in NFD: base letters + combining grave/acute marks.
        let nfd = "Lie\u{0300}ge Trinite\u{0301}";
        let got = nfc(nfd);
        assert_eq!(got, "Li\u{00e8}ge Trinit\u{00e9}", "decomposed accents recomposed");
        assert!(!got.contains('\u{0300}') && !got.contains('\u{0301}'), "no combining marks left");
        // Already-NFC text is unchanged.
        assert_eq!(nfc("Liège"), "Liège");
    }

    #[test]
    fn artist_title() {
        assert_eq!(
            p("Barker - Birmingham Screwdriver"),
            (Some("Barker".into()), Some("Birmingham Screwdriver".into()))
        );
    }

    #[test]
    fn track_number_prefixes() {
        assert_eq!(
            p("01 - Barker - Cascade Effect"),
            (Some("Barker".into()), Some("Cascade Effect".into()))
        );
        assert_eq!(
            p("01 - 07 - SCSI-9 - 303 Views"),
            (Some("SCSI-9".into()), Some("303 Views".into()))
        );
    }

    #[test]
    fn title_only() {
        assert_eq!(p("01 - Elevation"), (None, Some("Elevation".into())));
        assert_eq!(p("01 Destiny"), (None, Some("Destiny".into())));
        assert_eq!(p("Sentient"), (None, Some("Sentient".into())));
    }
}
