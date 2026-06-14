//! Tag writeback and embedding.
//!
//! Two distinct operations live here, with different boundaries:
//!
//! * [`write_to_file`] — narrow writeback of the managed "core" fields into an
//!   *existing source file* (`tag --write`). Deliberately small so we never
//!   rewrite obscure fields the user didn't touch (the explicit-only rule).
//! * [`embed_full`] — populate a *freshly converted file* from the catalog with
//!   the full standardized tag set + original cover art. There is no "user's
//!   original" to protect here: the catalog is the source of truth and we want
//!   the new file to carry everything the format can hold.
//!
//! CDJ / rekordbox compatibility: ID3-based containers (MP3, AIFF, WAV) are
//! written as **ID3v2.3**, not the lofty default v2.4 — older CDJs (Nexus/Nexus2)
//! read v2.3 reliably and choke on v2.4. Analysis-derived BPM/key/beatgrid/cues
//! reach the CDJ through the rekordbox export (export.pdb/ANLZ), not file tags;
//! here we only embed the file-level metadata the catalog holds.

use crate::error::{Error, Result};
use crate::model::Tags;
use lofty::config::WriteOptions;
use lofty::error::{ErrorKind, LoftyError};
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::ItemKey;
use lofty::tag::{Tag, TagExt, TagType};
use std::path::Path;

/// Cover artwork carried verbatim between files so conversion preserves the
/// original image at full quality (no decode/re-encode). Opaque to callers
/// outside this module — read it from one file with [`read_front_cover_raw`]
/// and hand it to [`embed_full`] for another.
#[derive(Debug, Clone)]
pub struct CoverArt {
    mime: MimeType,
    data: Vec<u8>,
}

impl CoverArt {
    /// Wrap PNG cover bytes (e.g. artwork fetched into the catalog and stored as
    /// PNG) as `CoverArt` so it can be embedded via [`embed_full`]. Mirrors how
    /// [`write_to_file`] treats fetched artwork (always PNG).
    pub fn from_png(data: Vec<u8>) -> Self {
        CoverArt { mime: MimeType::Png, data }
    }
}

/// Write CDJ/rekordbox-friendly options: force ID3v2.3 on ID3-based containers.
/// (No-op for Vorbis/MP4 tags, so it's safe to apply unconditionally.)
fn cdj_write_options() -> WriteOptions {
    WriteOptions::new().use_id3v23(true)
}

/// Read the front cover (or first picture) from `path` as raw bytes + mime,
/// without decoding/re-encoding — so the artwork can be re-embedded into a
/// converted file at its original quality. Returns `None` when the file carries
/// no embedded picture.
pub fn read_front_cover_raw(path: impl AsRef<Path>) -> Result<Option<CoverArt>> {
    let path = path.as_ref();
    let tagged = lofty::read_from_path(path).map_err(|source| Error::Tag {
        path: path.to_path_buf(),
        source,
    })?;
    let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) else {
        return Ok(None);
    };
    // Prefer an explicit front cover; fall back to the first picture present.
    let pic = tag
        .pictures()
        .iter()
        .find(|p| p.pic_type() == PictureType::CoverFront)
        .or_else(|| tag.pictures().first());
    Ok(pic.map(|p| CoverArt {
        mime: p.mime_type().cloned().unwrap_or(MimeType::Unknown(String::new())),
        data: p.data().to_vec(),
    }))
}

/// Populate `path` (a freshly converted file) with the full standardized tag set
/// from the catalog `tags`, plus `cover` artwork when supplied. Unlike
/// [`write_to_file`], this writes every field the catalog models — the catalog is
/// the source of truth for a new file, so we embed everything the format can hold.
///
/// Fields map 1:1 onto the same `ItemKey`s that `scan` reads, so a written file
/// round-trips back into an equivalent catalog entry. ID3 containers are written
/// as ID3v2.3 for CDJ compatibility. Empty/`None` fields are cleared so the
/// output never carries stale values copied across by the transcoder.
pub fn embed_full(path: impl AsRef<Path>, tags: &Tags, cover: Option<&CoverArt>) -> Result<()> {
    let path = path.as_ref();
    // Read only to learn the container's native tag type; we then write a FRESH
    // tag built solely from the catalog and `remove_others(true)`, so the output
    // carries exactly what the catalog says — no stale frames copied across by
    // the transcoder, and no re-encoding of pre-existing frames (which can trip
    // lofty's ID3v2.3 downconversion of multi-value/UTF-16 values).
    let tagged = lofty::read_from_path(path).map_err(|source| Error::Tag {
        path: path.to_path_buf(),
        source,
    })?;
    let tag_type = tagged.primary_tag_type();
    drop(tagged);
    let mut owned = Tag::new(tag_type);
    let tag = &mut owned;

    // --- core ---------------------------------------------------------------
    set_or_clear(tag, ItemKey::TrackTitle, tags.title.as_deref());
    set_or_clear(tag, ItemKey::TrackArtist, tags.artist.as_deref());
    set_or_clear(tag, ItemKey::AlbumTitle, tags.album.as_deref());
    set_or_clear(tag, ItemKey::Genre, tags.genre.as_deref());
    set_or_clear(tag, ItemKey::Label, tags.label.as_deref());
    set_or_clear(tag, ItemKey::Comment, tags.comment.as_deref());
    // Year is written via the recording-date frame, not ItemKey::Year: lofty drops
    // a bare Year on both v2.3 and v2.4, whereas RecordingDate round-trips on both
    // and `scan` derives the year back out of it.

    // --- numbering ----------------------------------------------------------
    set_num(tag, ItemKey::TrackNumber, tags.track_number);
    set_num(tag, ItemKey::TrackTotal, tags.track_total);
    set_num(tag, ItemKey::DiscNumber, tags.disc_number);
    set_num(tag, ItemKey::DiscTotal, tags.disc_total);
    set_num(tag, ItemKey::MovementNumber, tags.movement_number);
    set_num(tag, ItemKey::MovementTotal, tags.movement_total);

    // --- people / credits ---------------------------------------------------
    set_or_clear(tag, ItemKey::AlbumArtist, tags.album_artist.as_deref());
    set_or_clear(tag, ItemKey::Composer, tags.composer.as_deref());
    set_or_clear(tag, ItemKey::Conductor, tags.conductor.as_deref());
    set_or_clear(tag, ItemKey::Remixer, tags.remixer.as_deref());
    set_or_clear(tag, ItemKey::Producer, tags.producer.as_deref());
    set_or_clear(tag, ItemKey::Lyricist, tags.lyricist.as_deref());
    set_or_clear(tag, ItemKey::Arranger, tags.arranger.as_deref());
    set_or_clear(tag, ItemKey::Performer, tags.performer.as_deref());
    set_or_clear(tag, ItemKey::MixDj, tags.mix_dj.as_deref());
    set_or_clear(tag, ItemKey::Writer, tags.writer.as_deref());

    // --- dates --------------------------------------------------------------
    // Fall back to the plain year when no full recording date is present, so the
    // year is never lost. (ReleaseDate/TDRL is a v2.4-only frame and is dropped
    // when writing v2.3 to MP3/AIFF/WAV; FLAC/MP4 keep it.)
    let recording_date = tags
        .recording_date
        .clone()
        .or_else(|| tags.year.map(|y| y.to_string()));
    set_or_clear(tag, ItemKey::RecordingDate, recording_date.as_deref());
    set_or_clear(tag, ItemKey::ReleaseDate, tags.release_date.as_deref());
    set_or_clear(
        tag,
        ItemKey::OriginalReleaseDate,
        tags.original_release_date.as_deref(),
    );

    // --- release identifiers ------------------------------------------------
    set_or_clear(tag, ItemKey::Isrc, tags.isrc.as_deref());
    set_or_clear(tag, ItemKey::Barcode, tags.barcode.as_deref());
    set_or_clear(tag, ItemKey::CatalogNumber, tags.catalog_number.as_deref());
    set_or_clear(tag, ItemKey::Publisher, tags.publisher.as_deref());
    set_or_clear(tag, ItemKey::CopyrightMessage, tags.copyright.as_deref());
    set_or_clear(tag, ItemKey::ReleaseCountry, tags.release_country.as_deref());

    // --- DJ-relevant file-embedded values -----------------------------------
    // BPM: ID3v2 only supports integer BPM (TBPM); FLAC/MP4 also take a decimal
    // value. Write both so every container carries what it can — `scan` reads
    // either. Without IntegerBpm, MP3/AIFF/WAV would silently lose the tag.
    set_or_clear(
        tag,
        ItemKey::IntegerBpm,
        tags.bpm_tag.map(|b| (b.round() as i64).to_string()).as_deref(),
    );
    set_or_clear(tag, ItemKey::Bpm, tags.bpm_tag.map(fmt_num).as_deref());
    set_or_clear(tag, ItemKey::InitialKey, tags.initial_key_tag.as_deref());
    set_or_clear(tag, ItemKey::Mood, tags.mood.as_deref());
    set_or_clear(tag, ItemKey::ContentGroup, tags.grouping.as_deref());
    set_or_clear(
        tag,
        ItemKey::FlagCompilation,
        tags.compilation.and_then(|c| c.then_some("1")),
    );

    // --- content / descriptive ----------------------------------------------
    set_or_clear(tag, ItemKey::TrackSubtitle, tags.subtitle.as_deref());
    set_or_clear(tag, ItemKey::Description, tags.description.as_deref());
    set_or_clear(tag, ItemKey::Language, tags.language.as_deref());
    set_or_clear(tag, ItemKey::Script, tags.script.as_deref());
    set_or_clear(tag, ItemKey::Lyrics, tags.lyrics.as_deref());
    // lofty 0.24 encodes `ItemKey::Work` as an invalid ID3v2 frame (literal id
    // "WORK"), which makes the whole save fail for MP3/AIFF/WAV. Only write it to
    // containers that map it correctly (FLAC/MP4); ID3v2 simply omits it.
    if tag_type != TagType::Id3v2 {
        set_or_clear(tag, ItemKey::Work, tags.work.as_deref());
    }
    set_or_clear(tag, ItemKey::Movement, tags.movement.as_deref());

    // --- encoder / origin ---------------------------------------------------
    set_or_clear(tag, ItemKey::EncodedBy, tags.encoded_by.as_deref());
    set_or_clear(tag, ItemKey::EncoderSoftware, tags.encoder_software.as_deref());
    set_or_clear(tag, ItemKey::EncoderSettings, tags.encoder_settings.as_deref());
    set_or_clear(tag, ItemKey::OriginalArtist, tags.original_artist.as_deref());
    set_or_clear(
        tag,
        ItemKey::OriginalAlbumTitle,
        tags.original_album.as_deref(),
    );

    // --- MusicBrainz / AcoustID ---------------------------------------------
    set_or_clear(
        tag,
        ItemKey::MusicBrainzRecordingId,
        tags.musicbrainz_recording_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzTrackId,
        tags.musicbrainz_track_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzReleaseId,
        tags.musicbrainz_release_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzReleaseGroupId,
        tags.musicbrainz_release_group_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzArtistId,
        tags.musicbrainz_artist_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzReleaseArtistId,
        tags.musicbrainz_release_artist_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzWorkId,
        tags.musicbrainz_work_id.as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::MusicBrainzReleaseType,
        tags.musicbrainz_release_type.as_deref(),
    );
    set_or_clear(tag, ItemKey::AcoustId, tags.acoust_id.as_deref());

    // --- ReplayGain (gains carry a "dB" unit; peaks are bare floats) ---------
    set_or_clear(
        tag,
        ItemKey::ReplayGainTrackGain,
        tags.replay_gain_track_gain.map(fmt_db).as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::ReplayGainTrackPeak,
        tags.replay_gain_track_peak.map(fmt_num).as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::ReplayGainAlbumGain,
        tags.replay_gain_album_gain.map(fmt_db).as_deref(),
    );
    set_or_clear(
        tag,
        ItemKey::ReplayGainAlbumPeak,
        tags.replay_gain_album_peak.map(fmt_num).as_deref(),
    );

    // --- cover art ----------------------------------------------------------
    if let Some(art) = cover {
        tag.remove_picture_type(PictureType::CoverFront);
        let pic = Picture::unchecked(art.data.clone())
            .pic_type(PictureType::CoverFront)
            .mime_type(art.mime.clone())
            // An explicit (empty) description is required, not a workaround we can
            // skip: lofty assigns pictures UTF-8, which ID3v2.3 upgrades to UTF-16,
            // and its writer emits a malformed 1-byte terminator for a `None`
            // description — producing an APIC frame nothing can re-read. A `Some`
            // description routes through the correct BOM+terminator path.
            .description("")
            .build();
        tag.push_picture(pic);
    }

    save_with_stacked_id3_fallback(path, || {
        owned.save_to_path(path, cdj_write_options().remove_others(true))
    })
}

/// Set a numeric tag from an `Option<T: Display>`, clearing it when `None`.
fn set_num<T: std::fmt::Display>(tag: &mut Tag, key: ItemKey, value: Option<T>) {
    set_or_clear(tag, key, value.map(|v| v.to_string()).as_deref());
}

/// Format a float for a tag value, dropping a trailing `.0` (e.g. `128`, not `128.0`).
fn fmt_num(v: f32) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}

/// True if `bytes[at..]` starts with an MPEG audio frame sync (11 set bits).
fn mpeg_sync_at(bytes: &[u8], at: usize) -> bool {
    bytes.len() >= at + 2 && bytes[at] == 0xFF && (bytes[at + 1] & 0xE0) == 0xE0
}

/// Count consecutive leading ID3v2 tags and return `(count, byte offset past
/// them)`. ID3v2 headers are self-describing: `"ID3"`, two version bytes, a
/// flags byte (bit 4 = a 10-byte footer follows the body), then a 28-bit
/// syncsafe size of the body.
fn leading_id3_extent(bytes: &[u8]) -> (usize, usize) {
    let (mut pos, mut count) = (0usize, 0usize);
    while bytes.len() >= pos + 10 && &bytes[pos..pos + 3] == b"ID3" {
        let flags = bytes[pos + 5];
        let body = ((bytes[pos + 6] as usize) << 21)
            | ((bytes[pos + 7] as usize) << 14)
            | ((bytes[pos + 8] as usize) << 7)
            | (bytes[pos + 9] as usize);
        let footer = if flags & 0x10 != 0 { 10 } else { 0 };
        pos += 10 + body + footer;
        count += 1;
    }
    (count, pos)
}

/// Run a lofty save, and if it fails with `UnknownFormat`, retry once after
/// stripping *stacked* leading ID3v2 tags.
///
/// Some files (often from sloppy converters) carry two ID3v2 tags back-to-back
/// — e.g. an ID3v2.3 tag immediately followed by an ID3v2.4 one — ahead of the
/// audio. lofty reads them fine, but its MPEG writer re-sniffs the file's format
/// before writing and the second tag, sitting where it expects an audio frame,
/// makes detection give up with `UnknownFormat` (its source even notes
/// `TODO: Search through junk`). Dropping every leading ID3v2 tag puts the audio
/// at the front so the next save can sniff it; lofty then writes a single clean
/// tag. We only do this for the genuine stacked case (2+ tags) with real MPEG
/// audio after them, so a file that's simply corrupt (no audio) is left as-is
/// and its original error is surfaced unchanged.
fn save_with_stacked_id3_fallback(
    path: &Path,
    mut save: impl FnMut() -> std::result::Result<(), LoftyError>,
) -> Result<()> {
    match save() {
        Ok(()) => Ok(()),
        Err(source) => {
            let stripped = matches!(source.kind(), ErrorKind::UnknownFormat)
                && strip_stacked_leading_id3(path).unwrap_or(false);
            if stripped {
                save().map_err(|source| Error::Tag {
                    path: path.to_path_buf(),
                    source,
                })
            } else {
                Err(Error::Tag {
                    path: path.to_path_buf(),
                    source,
                })
            }
        }
    }
}

/// Strip stacked leading ID3v2 tags if present (see
/// [`save_with_stacked_id3_fallback`]). Returns whether the file was rewritten.
fn strip_stacked_leading_id3(path: &Path) -> std::io::Result<bool> {
    let raw = std::fs::read(path)?;
    let (count, audio_start) = leading_id3_extent(&raw);
    if count < 2 || !mpeg_sync_at(&raw, audio_start) {
        return Ok(false);
    }
    std::fs::write(path, &raw[audio_start..])?;
    Ok(true)
}

/// Whether `path` looks like an MP3 by extension. Used to gate the APE-strip
/// recovery (see [`strip_trailing_ape`]) so it only ever touches files where an
/// APE tag is non-standard junk and ID3v2 is the managed tag — never a file
/// whose APE tag is its primary metadata (`.ape`/`.wv`/`.mpc`).
fn is_mp3(path: &Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mp3"))
}

/// Byte range `[start, end)` of an APE tag sitting at the end of the file, or
/// `None` if there isn't one. Reads only the 32-byte APE footer's fixed-width
/// little-endian fields — never the text items — so a tag that's unreadable to
/// lofty (e.g. non-UTF-8 values) is still located here. A trailing 128-byte
/// ID3v1 tag is skipped over, so the returned range excludes it.
fn trailing_ape_extent(bytes: &[u8]) -> Option<(usize, usize)> {
    let len = bytes.len();
    // An APE tag is followed only by an optional ID3v1 (and ID3v1 enhanced)
    // block at the very end; the APE footer sits just before it.
    let mut end = len;
    if end >= 128 && &bytes[end - 128..end - 125] == b"TAG" {
        end -= 128;
    }
    if end < 32 {
        return None;
    }
    let footer = &bytes[end - 32..end];
    if &footer[0..8] != b"APETAGEX" {
        return None;
    }
    // `tag_size` counts the items plus the 32-byte footer, but not a header.
    let tag_size = u32::from_le_bytes([footer[12], footer[13], footer[14], footer[15]]) as usize;
    let flags = u32::from_le_bytes([footer[20], footer[21], footer[22], footer[23]]);
    let has_header = flags & 0x8000_0000 != 0;
    if tag_size < 32 || tag_size > end {
        return None;
    }
    let mut start = end - tag_size;
    // A header (when present) is another 32 bytes ahead of the items; only step
    // back over it if its preamble is actually there.
    if has_header && start >= 32 && &bytes[start - 32..start - 24] == b"APETAGEX" {
        start -= 32;
    }
    Some((start, end))
}

/// Remove a trailing APE tag from an MP3, preserving the audio and any ID3v1
/// tag after it. Returns whether the file was rewritten. Recovery for files
/// whose APE tag holds non-UTF-8 text that lofty refuses to parse (see the
/// fallback in [`write_to_file`]). Caller must gate this to MP3s ([`is_mp3`]).
fn strip_trailing_ape(path: &Path) -> std::io::Result<bool> {
    let raw = std::fs::read(path)?;
    let Some((start, end)) = trailing_ape_extent(&raw) else {
        return Ok(false);
    };
    let mut out = Vec::with_capacity(raw.len() - (end - start));
    out.extend_from_slice(&raw[..start]);
    out.extend_from_slice(&raw[end..]); // keep a trailing ID3v1, if any
    std::fs::write(path, &out)?;
    Ok(true)
}

/// Format a ReplayGain gain value with its conventional `dB` unit.
fn fmt_db(v: f32) -> String {
    format!("{:.2} dB", v)
}

/// Write the managed tag fields of `tags` into the file at `path`. When
/// `artwork` is `Some`, the PNG bytes are embedded as the file's front-cover
/// picture, replacing any existing cover; `None` leaves embedded pictures
/// untouched. Embedding is opt-in (CLI `tag --write --art`) because it rewrites
/// significantly more of the file than text tags.
pub fn write_to_file(path: impl AsRef<Path>, tags: &Tags, artwork: Option<&[u8]>) -> Result<()> {
    let path = path.as_ref();
    let mut tagged = match lofty::read_from_path(path) {
        Ok(tagged) => tagged,
        // A non-UTF-8 APE tag (APEv2 mandates UTF-8, but some MP3 taggers write
        // Latin-1/Windows-1252) makes lofty bail on the *whole* file — and its
        // own APE-removal API re-reads the items and chokes the same way, so the
        // tag can't be dropped through lofty at all. On an MP3 the APE tag is
        // non-standard and we only manage ID3v2 here, so strip the stray APE
        // block structurally (fixed-width footer fields, no text decoding — it
        // can't hit the same error) and retry, mirroring the stacked-ID3
        // recovery above. Gated to MP3 so we never delete the *primary* APE tag
        // of a Monkey's Audio / WavPack / Musepack file.
        Err(source) => {
            if is_mp3(path) && strip_trailing_ape(path).unwrap_or(false) {
                lofty::read_from_path(path).map_err(|source| Error::Tag {
                    path: path.to_path_buf(),
                    source,
                })?
            } else {
                return Err(Error::Tag {
                    path: path.to_path_buf(),
                    source,
                });
            }
        }
    };

    let tag_type = tagged.primary_tag_type();
    if tagged.primary_tag_mut().is_none() {
        tagged.insert_tag(Tag::new(tag_type));
    }
    let tag = tagged
        .primary_tag_mut()
        .expect("tag was just inserted if missing");

    set_or_clear(tag, ItemKey::TrackTitle, tags.title.as_deref());
    set_or_clear(tag, ItemKey::TrackArtist, tags.artist.as_deref());
    set_or_clear(tag, ItemKey::AlbumArtist, tags.album_artist.as_deref());
    set_or_clear(tag, ItemKey::AlbumTitle, tags.album.as_deref());
    set_or_clear(tag, ItemKey::Genre, tags.genre.as_deref());
    set_or_clear(tag, ItemKey::Label, tags.label.as_deref());
    set_or_clear(tag, ItemKey::Comment, tags.comment.as_deref());
    match tags.year {
        Some(y) => {
            tag.insert_text(ItemKey::Year, y.to_string());
        }
        None => tag.remove_key(ItemKey::Year),
    }

    if let Some(bytes) = artwork {
        // Replace any existing front cover with the supplied PNG. We only touch
        // the front-cover slot; other picture types (back cover, artist, etc.)
        // are left as-is.
        tag.remove_picture_type(PictureType::CoverFront);
        let pic = Picture::unchecked(bytes.to_vec())
            .pic_type(PictureType::CoverFront)
            .mime_type(MimeType::Png)
            .build();
        tag.push_picture(pic);
    }

    // lofty 0.24 *reads* an ID3v2 work tag into `ItemKey::Work` but re-encodes
    // it as a frame with the literal id "WORK" — which isn't a valid ID3v2 text
    // frame (those start with 'T'), so the save aborts and the whole writeback
    // fails. We only manage the core fields here, so for ID3v2 containers
    // (MP3/AIFF/WAV) drop the unwritable item rather than lose every edit. Other
    // containers (FLAC/MP4) map Work correctly and keep it.
    if tag_type == TagType::Id3v2 {
        tag.remove_key(ItemKey::Work);
    }

    match save_with_stacked_id3_fallback(path, || {
        tagged.save_to_path(path, WriteOptions::default())
    }) {
        Err(Error::Tag { source, .. }) if matches!(source.kind(), ErrorKind::BadTimestamp(_)) => {
            // A frame with an out-of-range timestamp (e.g. month 30 in a TDRC)
            // was kept verbatim by lofty on read — invisible to the generic tag
            // API (so we can't drop it above) but re-encoded on save, where it's
            // rejected. Rebuild the tag from just the representable items and
            // pictures, which excludes that retained frame, and write that. The
            // catalog still owns the date via its own year field.
            rewrite_tag_dropping_unrepresentable(path, tag_type, tagged.primary_tag())
        }
        other => other,
    }
}

/// Save a clean copy of `src`'s items + pictures, dropping any frame lofty
/// retained but can't re-encode (see the `BadTimestamp` fallback in
/// [`write_to_file`]). Building a fresh [`Tag`] from the generic view leaves the
/// unrepresentable frames behind; saving it replaces the on-disk tag of the same
/// type.
fn rewrite_tag_dropping_unrepresentable(
    path: &Path,
    tag_type: TagType,
    src: Option<&Tag>,
) -> Result<()> {
    let mut clean = Tag::new(tag_type);
    if let Some(src) = src {
        for item in src.items() {
            clean.insert(item.clone());
        }
        for pic in src.pictures() {
            clean.push_picture(pic.clone());
        }
    }
    clean
        .save_to_path(path, WriteOptions::default())
        .map_err(|source| Error::Tag {
            path: path.to_path_buf(),
            source,
        })
}

fn set_or_clear(tag: &mut Tag, key: ItemKey, value: Option<&str>) {
    match value {
        Some(v) if !v.is_empty() => {
            tag.insert_text(key, v.to_string());
        }
        _ => {
            tag.remove_key(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A real audio file is needed because lofty validates the container on
    /// save. Skip gracefully if the testdata isn't present (e.g. CI without it).
    fn fixture() -> Option<std::path::PathBuf> {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(
            "../../testdata/seeker-sample/function - berghain 07 cd1 - 01. tadeo - requiem.mp3",
        );
        p.exists().then_some(p)
    }

    /// Build a minimal valid PNG (1x1) so the test doesn't depend on `image`.
    fn tiny_png() -> Vec<u8> {
        // 1x1 transparent PNG.
        const PNG: &[u8] = &[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00, 0x0A, 0x49, 0x44, 0x41, 0x54, 0x78,
            0x9C, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00,
            0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
        ];
        PNG.to_vec()
    }

    #[test]
    fn embeds_front_cover_when_artwork_supplied() {
        let Some(src) = fixture() else {
            eprintln!("skipping: testdata fixture missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ordnung-tag-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("fixture.mp3");
        std::fs::copy(&src, &dst).unwrap();

        let tags = Tags {
            title: Some("Requiem".into()),
            artist: Some("Tadeo".into()),
            album_artist: Some("Various Artists".into()),
            ..Default::default()
        };
        let png = tiny_png();
        write_to_file(&dst, &tags, Some(&png)).unwrap();

        // The album artist must round-trip back out of the file.
        {
            use lofty::prelude::ItemKey;
            let tagged = lofty::read_from_path(&dst).unwrap();
            let tag = tagged.primary_tag().expect("primary tag present");
            assert_eq!(
                tag.get_string(ItemKey::AlbumArtist),
                Some("Various Artists"),
                "album artist written back"
            );
        }

        // Re-read: the front cover must be present and exactly our bytes.
        let tagged = lofty::read_from_path(&dst).unwrap();
        let tag = tagged.primary_tag().expect("primary tag present");
        let cover = tag
            .pictures()
            .iter()
            .find(|p| p.pic_type() == PictureType::CoverFront)
            .expect("front cover embedded");
        assert_eq!(cover.data(), png.as_slice());

        // Writing again with no artwork leaves the existing cover untouched.
        write_to_file(&dst, &tags, None).unwrap();
        let tagged = lofty::read_from_path(&dst).unwrap();
        assert_eq!(
            tagged
                .primary_tag()
                .unwrap()
                .pictures()
                .iter()
                .filter(|p| p.pic_type() == PictureType::CoverFront)
                .count(),
            1,
            "passing None must not remove or duplicate the cover"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// `embed_full` writes the broad standardized set, and a `scan` of the result
    /// recovers it — proving conversion preserves metadata into the new file.
    #[test]
    fn embed_full_round_trips_via_scan() {
        let Some(src) = fixture() else {
            eprintln!("skipping: testdata fixture missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ordnung-embed-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("fixture.mp3");
        std::fs::copy(&src, &dst).unwrap();

        let tags = Tags {
            title: Some("Requiem".into()),
            artist: Some("Tadeo".into()),
            album: Some("Berghain 07".into()),
            album_artist: Some("Various Artists".into()),
            genre: Some("Techno".into()),
            label: Some("Ostgut Ton".into()),
            year: Some(2012),
            track_number: Some(1),
            track_total: Some(12),
            remixer: Some("Some Remixer".into()),
            isrc: Some("DEUM71200001".into()),
            catalog_number: Some("OSTGUTCD07".into()),
            bpm_tag: Some(132.0),
            initial_key_tag: Some("8A".into()),
            compilation: Some(true),
            ..Default::default()
        };
        let cover = CoverArt {
            mime: MimeType::Png,
            data: tiny_png(),
        };
        embed_full(&dst, &tags, Some(&cover)).unwrap();

        // Re-scan the written file: the broad set must come back.
        let scanned = crate::scan::scan_file(&dst).unwrap();
        let got = &scanned.tags;
        assert_eq!(got.title.as_deref(), Some("Requiem"));
        assert_eq!(got.album.as_deref(), Some("Berghain 07"));
        assert_eq!(got.album_artist.as_deref(), Some("Various Artists"));
        // ID3 has a single TPUB frame for both label and publisher, so on an MP3
        // round-trip the label value surfaces as either. (FLAC/Vorbis keep them
        // distinct.) Assert the value survived under one of them.
        assert_eq!(
            got.label.as_deref().or(got.publisher.as_deref()),
            Some("Ostgut Ton")
        );
        assert_eq!(got.year, Some(2012));
        assert_eq!(got.track_number, Some(1));
        assert_eq!(got.track_total, Some(12));
        assert_eq!(got.remixer.as_deref(), Some("Some Remixer"));
        assert_eq!(got.isrc.as_deref(), Some("DEUM71200001"));
        assert_eq!(got.catalog_number.as_deref(), Some("OSTGUTCD07"));
        assert_eq!(got.bpm_tag, Some(132.0));
        assert_eq!(got.initial_key_tag.as_deref(), Some("8A"));
        assert_eq!(got.compilation, Some(true));
        assert!(got.has_cover, "cover survived embedding");

        let cover_raw = read_front_cover_raw(&dst).unwrap().expect("cover present");
        assert_eq!(cover_raw.data, tiny_png(), "cover bytes preserved verbatim");

        // CDJ compatibility: the MP3's ID3v2 header must be major version 3
        // (byte 3 of the "ID3" header), not lofty's default v2.4.
        let bytes = std::fs::read(&dst).unwrap();
        assert_eq!(&bytes[0..3], b"ID3", "starts with an ID3v2 header");
        assert_eq!(bytes[3], 3, "ID3v2.3 written for CDJ compatibility");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn leading_id3_extent_counts_stacked_tags() {
        // Two empty, back-to-back ID3v2 tags (10-byte header, zero body) then
        // a fake MPEG frame sync.
        let mut buf = Vec::new();
        buf.extend_from_slice(b"ID3\x03\x00\x00\x00\x00\x00\x00"); // v2.3, body 0
        buf.extend_from_slice(b"ID3\x04\x00\x00\x00\x00\x00\x00"); // v2.4, body 0
        let audio = buf.len();
        buf.extend_from_slice(&[0xFF, 0xFB, 0x90, 0x00]); // MPEG sync
        assert_eq!(leading_id3_extent(&buf), (2, audio));
        assert!(mpeg_sync_at(&buf, audio));

        // A non-syncsafe-aware reader would mis-size; confirm the footer flag is
        // honoured (bit 4 → +10 bytes).
        let mut withf = Vec::new();
        withf.extend_from_slice(b"ID3\x04\x00\x10\x00\x00\x00\x00"); // footer flag set
        withf.extend_from_slice(&[0u8; 10]); // the 10-byte footer
        let audio2 = withf.len();
        withf.extend_from_slice(&[0xFF, 0xF3, 0x00, 0x00]);
        assert_eq!(leading_id3_extent(&withf), (1, audio2));

        // No tag at all: extent is zero, and arbitrary bytes aren't a sync.
        assert_eq!(leading_id3_extent(&[0x00, 0x01, 0x02]), (0, 0));
        assert!(!mpeg_sync_at(&[0x00, 0x00], 0));
    }

    #[test]
    fn write_to_file_recovers_stacked_id3_mp3() {
        // Reproduce the real-world failure: a second ID3v2 tag wedged ahead of a
        // valid MP3 makes lofty's in-place save bail with `UnknownFormat`.
        // `write_to_file` must strip the stray leading tag and still write.
        let Some(src) = fixture() else {
            eprintln!("skipping: testdata fixture missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ordnung-stacked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("stacked.mp3");

        // Prepend an extra empty ID3v2.3 tag to the (already-tagged) fixture, so
        // the file now begins with two stacked ID3 tags before the audio.
        let mut bytes = b"ID3\x03\x00\x00\x00\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&std::fs::read(&src).unwrap());
        std::fs::write(&dst, &bytes).unwrap();
        assert!(leading_id3_extent(&std::fs::read(&dst).unwrap()).0 >= 2);

        let tags = Tags {
            title: Some("Recovered".into()),
            artist: Some("Tadeo".into()),
            ..Default::default()
        };
        write_to_file(&dst, &tags, None).expect("write recovers from stacked tags");

        // The edit landed, the audio survived, and the file is now sniffable.
        let scanned = crate::scan::scan_file(&dst).unwrap();
        assert_eq!(scanned.tags.title.as_deref(), Some("Recovered"));
        assert!(scanned.properties.duration_ms > 0, "audio preserved");
        assert_eq!(leading_id3_extent(&std::fs::read(&dst).unwrap()).0, 1, "single clean tag");

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A minimal APE tag (footer only, no header) carrying one text item whose
    /// value is deliberately not valid UTF-8 — the shape that makes lofty bail
    /// with "Ape: Failed to convert text item into a UTF-8 string".
    fn ape_tag_bad_utf8() -> Vec<u8> {
        let key = b"COMMENT";
        let value = [0xFFu8, 0xFE, 0x80]; // invalid UTF-8
        let mut items = Vec::new();
        items.extend_from_slice(&(value.len() as u32).to_le_bytes()); // value size
        items.extend_from_slice(&0u32.to_le_bytes()); // flags: UTF-8 text, writable
        items.extend_from_slice(key);
        items.push(0); // null-terminated key
        items.extend_from_slice(&value);

        let tag_size = (items.len() + 32) as u32; // items + footer, no header
        let mut footer = Vec::new();
        footer.extend_from_slice(b"APETAGEX");
        footer.extend_from_slice(&2000u32.to_le_bytes()); // APEv2
        footer.extend_from_slice(&tag_size.to_le_bytes());
        footer.extend_from_slice(&1u32.to_le_bytes()); // item count
        footer.extend_from_slice(&0u32.to_le_bytes()); // flags: footer, no header
        footer.extend_from_slice(&[0u8; 8]); // reserved

        let mut tag = items;
        tag.extend_from_slice(&footer);
        tag
    }

    #[test]
    fn trailing_ape_extent_locates_tag() {
        // Bare APE tag at the end of some audio: range covers exactly the tag.
        let mut buf = vec![0xFFu8, 0xFB, 0x90, 0x00]; // fake MPEG frame
        let audio = buf.len();
        let ape = ape_tag_bad_utf8();
        buf.extend_from_slice(&ape);
        assert_eq!(trailing_ape_extent(&buf), Some((audio, audio + ape.len())));

        // Same, but with an ID3v1 tag after the APE tag: the range stops before
        // the ID3v1 so it's preserved on strip.
        let mut withv1 = buf.clone();
        let mut v1 = b"TAG".to_vec();
        v1.extend_from_slice(&[0u8; 125]); // 128-byte ID3v1
        withv1.extend_from_slice(&v1);
        assert_eq!(trailing_ape_extent(&withv1), Some((audio, audio + ape.len())));

        // No APE tag → None.
        assert_eq!(trailing_ape_extent(&[0xFF, 0xFB, 0x00, 0x00]), None);
    }

    #[test]
    fn write_to_file_recovers_non_utf8_ape_mp3() {
        let Some(src) = fixture() else {
            eprintln!("skipping: testdata fixture missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ordnung-ape-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("ape.mp3");

        // Append a non-UTF-8 APE tag to the fixture, reproducing the field report.
        let mut bytes = std::fs::read(&src).unwrap();
        bytes.extend_from_slice(&ape_tag_bad_utf8());
        std::fs::write(&dst, &bytes).unwrap();

        // Confirm the repro: lofty can't even read the doctored file.
        assert!(
            lofty::read_from_path(&dst).is_err(),
            "expected the non-UTF-8 APE tag to break lofty's read"
        );

        let tags = Tags {
            title: Some("Salvaged".into()),
            artist: Some("Tadeo".into()),
            ..Default::default()
        };
        write_to_file(&dst, &tags, None).expect("write recovers by stripping the APE tag");

        // The edit landed, the audio survived, and the stray APE tag is gone.
        let scanned = crate::scan::scan_file(&dst).unwrap();
        assert_eq!(scanned.tags.title.as_deref(), Some("Salvaged"));
        assert!(scanned.properties.duration_ms > 0, "audio preserved");
        assert_eq!(
            trailing_ape_extent(&std::fs::read(&dst).unwrap()),
            None,
            "APE tag removed"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A minimal ID3v2.4 tag holding a single TDRC frame with an out-of-range
    /// timestamp (`2009-30` → month 30). lofty reads it but keeps it as an
    /// un-representable frame that breaks a later save.
    fn id3v24_with_bad_tdrc() -> Vec<u8> {
        let mut body = vec![0x03u8]; // UTF-8 text encoding
        body.extend_from_slice(b"2009-30");
        let syncsafe = |n: usize| {
            [
                (n >> 21) as u8 & 0x7f,
                (n >> 14) as u8 & 0x7f,
                (n >> 7) as u8 & 0x7f,
                n as u8 & 0x7f,
            ]
        };
        let mut frame = b"TDRC".to_vec();
        frame.extend_from_slice(&syncsafe(body.len()));
        frame.extend_from_slice(&[0x00, 0x00]); // frame flags
        frame.extend_from_slice(&body);
        let mut tag = b"ID3".to_vec();
        tag.extend_from_slice(&[0x04, 0x00, 0x00]); // v2.4, rev 0, no flags
        tag.extend_from_slice(&syncsafe(frame.len()));
        tag.extend_from_slice(&frame);
        tag
    }

    #[test]
    fn write_to_file_recovers_invalid_timestamp_frame() {
        let Some(src) = fixture() else {
            eprintln!("skipping: testdata fixture missing");
            return;
        };
        let dir = std::env::temp_dir().join(format!("ordnung-badts-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let dst = dir.join("badts.mp3");

        // Replace the fixture's tag with our bad-timestamp tag (strip the
        // fixture's own leading ID3 first so the result isn't *stacked* — we want
        // to exercise the timestamp path, not the stacked-tag path).
        let fixture_bytes = std::fs::read(&src).unwrap();
        let (_, audio_start) = leading_id3_extent(&fixture_bytes);
        let mut bytes = id3v24_with_bad_tdrc();
        bytes.extend_from_slice(&fixture_bytes[audio_start..]);
        std::fs::write(&dst, &bytes).unwrap();

        // The bad frame is retained, not surfaced as a generic item.
        let tagged = lofty::read_from_path(&dst).unwrap();
        assert!(
            tagged
                .primary_tag()
                .and_then(|t| t.get_string(ItemKey::RecordingDate))
                .is_none(),
            "invalid timestamp is retained, not exposed"
        );
        drop(tagged);

        let tags = Tags {
            artist: Some("Tadeo".into()),
            title: Some("Salvaged".into()),
            ..Default::default()
        };
        write_to_file(&dst, &tags, None).expect("write recovers from bad timestamp");

        let scanned = crate::scan::scan_file(&dst).unwrap();
        assert_eq!(scanned.tags.title.as_deref(), Some("Salvaged"));
        assert!(scanned.properties.duration_ms > 0, "audio preserved");

        std::fs::remove_dir_all(&dir).ok();
    }
}
