//! The catalog: SQLite-backed master pool and source of truth.
//!
//! Phase 1 stores tracks (audio properties + tags) in one table. Analysis,
//! playlists, and cues get their own tables in later phases. The export USB is
//! always derived from this — never the other way around.

use crate::error::{Error, Result};
use crate::model::key::{Key, Mode, PitchClass};
use crate::model::{
    Analysis, AudioProperties, Beat, Beatgrid, Format, Id, Playlist, Tags, Track, TranscodeVerdict,
    VinylRecord,
};
use rusqlite::{params, Connection, OptionalExtension, Row};
use std::path::Path;

fn mode_int(m: Mode) -> i64 {
    match m {
        Mode::Major => 0,
        Mode::Minor => 1,
    }
}

fn mode_from_int(v: i64) -> Mode {
    if v == 1 {
        Mode::Minor
    } else {
        Mode::Major
    }
}

pub struct Catalog {
    conn: Connection,
}

/// One other track sharing an album with a given track. Returned by
/// [`Catalog::album_siblings_detailed`] so the GUI can present album-mates by
/// name (with whether each already has a cover) when applying a dropped or
/// fetched cover across an album.
#[derive(Debug, Clone)]
pub struct AlbumSibling {
    pub id: Id,
    pub artist: Option<String>,
    pub title: Option<String>,
    /// True when the track already has a cover — embedded (`has_cover`) or a
    /// fetched external image. Used to default which mates are pre-selected.
    pub has_art: bool,
}

/// Minimum fields needed to search an external source for a track's artwork —
/// returned by `Catalog::tracks_missing_artwork` and consumed by the
/// `discogs` engine.
#[derive(Debug, Clone)]
pub struct MissingArtwork {
    pub id: Id,
    pub artist: Option<String>,
    pub title: Option<String>,
    pub album: Option<String>,
    /// Discogs release id of the artwork already on file for this track, when
    /// any. Lets the song-data run pull tags straight from the release the user
    /// already picked art from, instead of re-prompting. `None` for tracks with
    /// no external-artwork row (always `None` for `tracks_missing_artwork`).
    pub release_id: Option<String>,
}

/// What scanning produced for one file, ready to upsert.
#[derive(Debug, Clone)]
pub struct ScannedTrack {
    pub source_path: String,
    pub format: Format,
    pub properties: AudioProperties,
    /// Tags read from the file (and/or inferred from the filename).
    pub tags: Tags,
    /// Downscaled cover-art thumbnail (PNG bytes), if the file had embedded art.
    pub cover_thumb: Option<Vec<u8>>,
    /// A cheap, path-independent content fingerprint (see `scan::file_fingerprint`).
    /// Lets a rescan recognize a file that moved or was renamed and repoint the
    /// existing row instead of orphaning it. `None` when it couldn't be computed.
    pub fingerprint: Option<String>,
    /// Source file size in bytes, captured at scan time. Paired with `src_mtime`
    /// as a cheap "has this file changed?" signature so a rescan can skip files
    /// already in the catalog and untouched on disk. `None` if stat failed.
    pub src_size: Option<u64>,
    /// Source file modification time, seconds since the Unix epoch. See `src_size`.
    pub src_mtime: Option<i64>,
}

/// Why a [`DuplicateGroup`]'s tracks are considered duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplicateKind {
    /// Byte-identical audio — the tracks share a content fingerprint.
    Identical,
    /// Same artist + title, but the files differ (e.g. a FLAC and an MP3 of one
    /// song, or two different rips/re-encodes).
    SameTrack,
    /// Same *recording* by acoustic fingerprint — the audio sounds identical on
    /// playback even though the bytes AND tags differ (different format/bitrate,
    /// re-rip, or cleaned-up metadata). Requires both tracks to be analyzed.
    Acoustic,
}

/// A set of catalog tracks that duplicate one another. The catalog never deletes
/// anything — this is a report for the user to act on.
#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub kind: DuplicateKind,
    pub tracks: Vec<Track>,
    /// Stable identity of this group, used to persist a "not a duplicate"
    /// dismissal in `ignored_duplicates`. For `Identical` it's the shared
    /// fingerprint; for `SameTrack` it's the normalized artist+title key; for
    /// `Acoustic` it's the sorted member track ids. The kind is prefixed so the
    /// namespaces can't collide.
    pub key: String,
}

/// Index of the copy to keep in a duplicate group. The ranking, highest first:
///
/// 1. **Spectrally clean** — a copy whose audio is *not* a detected transcode
///    beats one that is. A FLAC re-encoded from a 128 kbps MP3 carries no more
///    real information than that MP3, so a confirmed-lossy copy (verdict
///    `Suspect`/`LikelyLossy`) is demoted below a clean or merely band-limited
///    one (`Clean`/`Inconclusive`), *even when its container is lossless*. A copy
///    with no analysis is treated as clean — absence is never an accusation, so
///    behaviour matches the old format+bitrate pick until tracks are analyzed.
/// 2. **Lossless container** — FLAC/WAV/AIFF over a lossy format.
/// 3. **Highest bitrate** — the tiebreaker within a tier.
///
/// `None` only for an empty slice.
pub fn best_copy_index(tracks: &[Track]) -> Option<usize> {
    tracks
        .iter()
        .enumerate()
        .max_by_key(|(_, t)| {
            // A detected transcode (lossy audio, whatever the container) is not a
            // true high-quality copy; `Clean`/`Inconclusive` and un-analyzed pass.
            let clean = t.analysis.as_ref().map_or(true, |a| {
                !matches!(
                    a.transcode_verdict(),
                    TranscodeVerdict::Suspect | TranscodeVerdict::LikelyLossy
                )
            });
            let lossless = matches!(t.format, Format::Flac | Format::Wav | Format::Aiff);
            let bitrate = t.properties.as_ref().and_then(|p| p.bitrate_kbps).unwrap_or(0);
            (clean as u8, lossless as u8, bitrate)
        })
        .map(|(i, _)| i)
}

/// A track whose source file is absent from disk, carried with just enough to
/// hunt for its moved file: the row `id` to repoint, its old `source_path` (for
/// the filename to look for), and the stored content `fingerprint` used to
/// confirm identity when several files share that name.
#[derive(Debug, Clone)]
pub struct MissingTrack {
    pub id: Id,
    pub source_path: String,
    pub fingerprint: Option<String>,
}

/// Outcome of a [`Catalog::relink_prefix`] call.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct RelinkReport {
    /// Tracks whose path was repointed (or, on a dry run, would be).
    pub moved: usize,
    /// Matches skipped because the new path already belongs to another track.
    pub skipped: usize,
    /// The `(old, new)` rewrites — applied, or previewed on a dry run.
    pub changes: Vec<(String, String)>,
}

/// A track is "metadata-complete" — and so never needs the Discogs picker —
/// once it has the core album-level fields. This is the *one-time*, add-time
/// test: a complete song is marked fetched the moment it's scanned in, and an
/// incomplete one is offered exactly once (then marked, whatever the outcome).
/// After that, `discogs_meta_fetched_at` is the only gate. Used by the add-time
/// mark in `upsert_scanned` and the one-time backfill in `migrate`. Unqualified
/// column names so it drops into an `UPDATE tracks` WHERE clause.
const METADATA_COMPLETE_SQL: &str =
    "TRIM(COALESCE(album, '')) <> '' AND TRIM(COALESCE(genre, '')) <> '' AND year IS NOT NULL";

/// How long a freshly scanned track stays in the "recently added" inbox: one
/// day (in seconds). Past this, it drops out of the view and its sidebar badge
/// count regardless of whether analysis/fetch finished, keeping the inbox a
/// short list of genuinely fresh imports. See [`Catalog::list_recently_added`].
const RECENTLY_ADDED_WINDOW_SECS: i64 = 24 * 60 * 60;

impl Catalog {
    /// Open (creating if needed) a catalog at `path` and ensure the schema exists.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // The GUI runs several connections at once (the cover loader, a
        // long-running Discogs scan worker that writes `discogs_meta_fetched_at`
        // as it goes, and the per-pick save worker). WAL lets readers and a
        // single writer coexist, but two writers still collide. Without a busy
        // timeout the loser fails *instantly* with SQLITE_BUSY — and because the
        // save worker swallows that error, a picked release would silently fail
        // to persist and the track would re-appear in the picker forever. Wait
        // for the lock instead of dropping the write.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let cat = Catalog { conn };
        cat.init_schema()?;
        Ok(cat)
    }

    fn init_schema(&self) -> Result<()> {
        self.conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS tracks (
                id           INTEGER PRIMARY KEY,
                source_path  TEXT NOT NULL UNIQUE,
                format       TEXT NOT NULL,
                sample_rate  INTEGER,
                bit_depth    INTEGER,
                channels     INTEGER,
                duration_ms  INTEGER,
                bitrate_kbps INTEGER,
                -- core editable tags (writeback via `tag --write` is limited to these)
                title        TEXT,
                artist       TEXT,
                album        TEXT,
                genre        TEXT,
                label        TEXT,
                year         INTEGER,
                comment      TEXT,
                rating       INTEGER,
                added_at     INTEGER NOT NULL DEFAULT (unixepoch()),
                -- Set once a user edits tags via the `tag` command. While set, a
                -- rescan refreshes audio properties but does NOT overwrite tag
                -- fields, so catalog edits survive re-scanning.
                user_edited  INTEGER NOT NULL DEFAULT 0,
                -- extended tags read from the source file (read-only in catalog;
                -- never written back by --write unless explicitly added there).
                track_number INTEGER,
                track_total  INTEGER,
                disc_number  INTEGER,
                disc_total   INTEGER,
                album_artist TEXT,
                composer     TEXT,
                conductor    TEXT,
                remixer      TEXT,
                producer     TEXT,
                lyricist     TEXT,
                arranger     TEXT,
                performer    TEXT,
                mix_dj       TEXT,
                writer       TEXT,
                recording_date         TEXT,
                release_date           TEXT,
                original_release_date  TEXT,
                isrc            TEXT,
                barcode         TEXT,
                catalog_number  TEXT,
                publisher       TEXT,
                copyright       TEXT,
                release_country TEXT,
                bpm_tag         REAL,
                initial_key_tag TEXT,
                mood            TEXT,
                grouping        TEXT,
                compilation     INTEGER,
                subtitle        TEXT,
                description     TEXT,
                language        TEXT,
                script          TEXT,
                lyrics          TEXT,
                work            TEXT,
                movement        TEXT,
                movement_number INTEGER,
                movement_total  INTEGER,
                encoded_by      TEXT,
                encoder_software TEXT,
                encoder_settings TEXT,
                original_artist TEXT,
                original_album  TEXT,
                mb_recording_id      TEXT,
                mb_track_id          TEXT,
                mb_release_id        TEXT,
                mb_release_group_id  TEXT,
                mb_artist_id         TEXT,
                mb_release_artist_id TEXT,
                mb_work_id           TEXT,
                mb_release_type      TEXT,
                acoust_id            TEXT,
                rg_track_gain  REAL,
                rg_track_peak  REAL,
                rg_album_gain  REAL,
                rg_album_peak  REAL,
                has_cover      INTEGER NOT NULL DEFAULT 0,
                -- Cheap content fingerprint (see scan::file_fingerprint). Identity
                -- that survives a move/rename, so a rescan can repoint an existing
                -- row instead of orphaning it. NOT unique (legit duplicate copies
                -- share it). NULL for rows scanned before this column existed.
                fingerprint    TEXT
            );
            CREATE INDEX IF NOT EXISTS idx_tracks_artist ON tracks(artist);
            CREATE INDEX IF NOT EXISTS idx_tracks_genre  ON tracks(genre);
            -- Indexes on columns added by migrations live in `migrate()` so they
            -- run AFTER the column has been added to old DBs.

            CREATE TABLE IF NOT EXISTS analysis (
                track_id         INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                bpm              REAL,
                key_tonic        INTEGER,   -- 0..11 (C..B), NULL if undetected
                key_mode         INTEGER,   -- 0 = major, 1 = minor
                beat_offset_ms   INTEGER,
                peak             REAL,
                loudness         REAL,
                waveform         BLOB,
                waveform_bands   BLOB,     -- per-bin low/mid/high energy for colored waveform (analyzer v10+)
                content_hash     TEXT,
                audio_fingerprint BLOB,    -- perceptual fingerprint (see analysis::fingerprint)
                lowpass_hz       REAL,     -- detected low-pass cutoff; flags lossy transcodes
                lowpass_edge     REAL,     -- cliff steepness in dB/kHz (brick wall vs. natural roll-off)
                analyzer_version INTEGER NOT NULL,
                src_size         INTEGER,
                src_mtime        INTEGER,
                analyzed_at      INTEGER NOT NULL DEFAULT (unixepoch())
            );

            -- Playlists and playlist folders. A folder (is_folder=1) holds other
            -- playlists/folders via parent_id; a playlist holds ordered track refs
            -- in playlist_tracks. The flat master pool (tracks) is the source of
            -- truth; playlists are just ordered views over it.
            CREATE TABLE IF NOT EXISTS playlists (
                id         INTEGER PRIMARY KEY,
                name       TEXT NOT NULL,
                parent_id  INTEGER REFERENCES playlists(id) ON DELETE CASCADE,
                is_folder  INTEGER NOT NULL DEFAULT 0,
                position   INTEGER NOT NULL DEFAULT 0,  -- order among siblings
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );
            CREATE INDEX IF NOT EXISTS idx_playlists_parent ON playlists(parent_id);

            CREATE TABLE IF NOT EXISTS playlist_tracks (
                playlist_id INTEGER NOT NULL REFERENCES playlists(id) ON DELETE CASCADE,
                track_id    INTEGER NOT NULL REFERENCES tracks(id) ON DELETE CASCADE,
                position    INTEGER NOT NULL,           -- order within the playlist
                PRIMARY KEY (playlist_id, track_id)
            );
            CREATE INDEX IF NOT EXISTS idx_pltracks_pl ON playlist_tracks(playlist_id);

            -- Cover art fetched from external sources (Discogs today; MusicBrainz/
            -- Beatport later). Kept separate from tracks.cover_thumb so the
            -- embedded-art semantics stay clean: has_cover/cover_thumb mean
            -- \"from the file\", this table means \"fetched on demand\". A row with
            -- NULL png_bytes records a no-match attempt so we don't re-query.
            CREATE TABLE IF NOT EXISTS track_external_artwork (
                track_id    INTEGER PRIMARY KEY REFERENCES tracks(id) ON DELETE CASCADE,
                source      TEXT NOT NULL,
                external_id TEXT,
                url         TEXT,
                png_bytes   BLOB,
                full_bytes  BLOB,
                fetched_at  INTEGER NOT NULL DEFAULT (unixepoch())
            );

            -- Duplicate groups the user has marked \"not a duplicate\" so
            -- find_duplicates stops reporting them. group_key is the stable
            -- identity of the group (see DuplicateGroup::key): the shared
            -- fingerprint for Identical groups, or the normalized artist+title
            -- key for SameTrack groups. Survives recompute and rescans; a group
            -- whose members all leave the catalog simply never recurs.
            CREATE TABLE IF NOT EXISTS ignored_duplicates (
                group_key  TEXT PRIMARY KEY,
                created_at INTEGER NOT NULL DEFAULT (unixepoch())
            );

            -- The user's Discogs vinyl collection, cached locally so the \"My Vinyl
            -- Collection\" view renders offline and a refresh only downloads covers
            -- it doesn't already have. instance_id is Discogs's per-copy id (stable
            -- across refreshes). cover_png is the downscaled grid image, NULL until
            -- fetched. This is reference data about physical records the user owns,
            -- entirely separate from the tracks pool (digital files).
            CREATE TABLE IF NOT EXISTS vinyl_collection (
                instance_id    INTEGER PRIMARY KEY,
                release_id     INTEGER NOT NULL,
                title          TEXT NOT NULL,
                artist         TEXT NOT NULL,
                year           INTEGER,
                label          TEXT,
                catalog_number TEXT,
                format         TEXT,
                thumb_url      TEXT,
                cover_url      TEXT,
                cover_png      BLOB,
                added          TEXT,
                fetched_at     INTEGER NOT NULL DEFAULT (unixepoch())
            );

            -- Parsed Discogs release detail (GET /releases/{id}), cached so song-data
            -- enrichment never re-fetches (and re-parses) a release it already pulled.
            -- The expensive part is the rate-limited round trip — at ~1 req/1.1s a
            -- re-run over a known-release library was spending minutes re-downloading
            -- data that never changes. Release metadata is effectively immutable, so
            -- there's no TTL; detail_json is the serialized ReleaseDetail. Keyed by
            -- Discogs release id (TEXT, matching the external-id plumbing elsewhere).
            CREATE TABLE IF NOT EXISTS release_cache (
                release_id  TEXT PRIMARY KEY,
                detail_json TEXT NOT NULL,
                fetched_at  INTEGER NOT NULL DEFAULT (unixepoch())
            );",
        )?;
        self.migrate()?;
        Ok(())
    }

    /// Forward-only migrations for catalogs created by an older build.
    /// Each entry: (column name, DDL fragment to append after `ADD COLUMN`).
    fn migrate(&self) -> Result<()> {
        // Phase 1.1 — track-level user-edit flag.
        self.add_column_if_missing("tracks", "user_edited", "INTEGER NOT NULL DEFAULT 0")?;

        // Full standardized tag set (added later — DBs created before now lose
        // these by default; rescanning fills them in).
        let cols: &[(&str, &str)] = &[
            ("track_number", "INTEGER"),
            ("track_total", "INTEGER"),
            ("disc_number", "INTEGER"),
            ("disc_total", "INTEGER"),
            ("album_artist", "TEXT"),
            ("composer", "TEXT"),
            ("conductor", "TEXT"),
            ("remixer", "TEXT"),
            ("producer", "TEXT"),
            ("lyricist", "TEXT"),
            ("arranger", "TEXT"),
            ("performer", "TEXT"),
            ("mix_dj", "TEXT"),
            ("writer", "TEXT"),
            ("recording_date", "TEXT"),
            ("release_date", "TEXT"),
            ("original_release_date", "TEXT"),
            ("isrc", "TEXT"),
            ("barcode", "TEXT"),
            ("catalog_number", "TEXT"),
            ("publisher", "TEXT"),
            ("copyright", "TEXT"),
            ("release_country", "TEXT"),
            ("bpm_tag", "REAL"),
            ("initial_key_tag", "TEXT"),
            ("mood", "TEXT"),
            ("grouping", "TEXT"),
            ("compilation", "INTEGER"),
            ("subtitle", "TEXT"),
            ("description", "TEXT"),
            ("language", "TEXT"),
            ("script", "TEXT"),
            ("lyrics", "TEXT"),
            ("work", "TEXT"),
            ("movement", "TEXT"),
            ("movement_number", "INTEGER"),
            ("movement_total", "INTEGER"),
            ("encoded_by", "TEXT"),
            ("encoder_software", "TEXT"),
            ("encoder_settings", "TEXT"),
            ("original_artist", "TEXT"),
            ("original_album", "TEXT"),
            ("mb_recording_id", "TEXT"),
            ("mb_track_id", "TEXT"),
            ("mb_release_id", "TEXT"),
            ("mb_release_group_id", "TEXT"),
            ("mb_artist_id", "TEXT"),
            ("mb_release_artist_id", "TEXT"),
            ("mb_work_id", "TEXT"),
            ("mb_release_type", "TEXT"),
            ("acoust_id", "TEXT"),
            ("rg_track_gain", "REAL"),
            ("rg_track_peak", "REAL"),
            ("rg_album_gain", "REAL"),
            ("rg_album_peak", "REAL"),
            ("has_cover", "INTEGER NOT NULL DEFAULT 0"),
            // Downscaled cover-art thumbnail (PNG bytes), extracted by scan.
            // Kept in the catalog so the GUI can render covers without re-reading
            // each audio file. Empty by default — rescan to populate.
            ("cover_thumb", "BLOB"),
            // Move-resilient content fingerprint. Empty by default — rescan to
            // populate, after which a moved/renamed file rematches its row.
            ("fingerprint", "TEXT"),
            // The single gate for the Discogs song-data picker: non-NULL means
            // "don't offer this track again", NULL means "still pending". Set
            // when the track was already metadata-complete at add time, when the
            // user applies a release, or when a run finds no match — so
            // `tracks_missing_metadata` returns exactly the never-handled tracks.
            // (Discogs rarely fills every field, so completeness alone can't tell
            // "done" from "Discogs has no more to give"; the mark can.)
            ("discogs_meta_fetched_at", "INTEGER"),
            // Source-file change signature (size in bytes, mtime in epoch
            // seconds), recorded at scan time. A rescan skips a file whose
            // signature still matches, so re-adding a folder doesn't re-read
            // every file. NULL on rows scanned before this column existed — those
            // rescan once (which fills the signature), then skip thereafter.
            ("src_size", "INTEGER"),
            ("src_mtime", "INTEGER"),
        ];
        for (name, ddl) in cols {
            self.add_column_if_missing("tracks", name, ddl)?;
        }
        // Indexes that depend on migrated columns — safe to create now.
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_tracks_album_artist ON tracks(album_artist);
             CREATE INDEX IF NOT EXISTS idx_tracks_isrc         ON tracks(isrc);
             CREATE INDEX IF NOT EXISTS idx_tracks_fingerprint  ON tracks(fingerprint);",
        )?;
        // Full-resolution external artwork kept for tag embedding (`tag --write
        // --art`). Older catalogs only had the small `png_bytes` thumbnail.
        self.add_column_if_missing("track_external_artwork", "full_bytes", "BLOB")?;

        // When set, this fetched cover should *supersede* whatever art is embedded
        // in the source file (display and export both prefer it). Default 0 keeps
        // the original semantics — external art only shows for tracks with no
        // embedded cover. Set to 1 when the user deliberately overwrites an
        // album-mate's existing cover to make a whole album match.
        self.add_column_if_missing(
            "track_external_artwork",
            "prefer_external",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        // Perceptual acoustic fingerprint, added in analyzer v5. Empty on older
        // catalogs until tracks are re-analyzed (the version bump invalidates the
        // cache), after which cross-format/-tag duplicates surface.
        self.add_column_if_missing("analysis", "audio_fingerprint", "BLOB")?;

        // Low-pass cutoff detection, added in analyzer v6 (flags lossy transcodes
        // wrapped in lossless containers). Empty until tracks are re-analyzed; the
        // version bump invalidates the cache so the next `analyze` fills them.
        self.add_column_if_missing("analysis", "lowpass_hz", "REAL")?;
        self.add_column_if_missing("analysis", "lowpass_edge", "REAL")?;

        // Per-bin low/mid/high spectral energy for the colored waveform, added in
        // analyzer v10. Empty on older catalogs until re-analyzed; the version
        // bump invalidates the cache so the next `analyze` fills it.
        self.add_column_if_missing("analysis", "waveform_bands", "BLOB")?;

        // One-time data migration (user_version 0 → 1): adopt the "decide once,
        // at add time" model for the Discogs picker. Songs that were already
        // metadata-complete in the catalog never needed Discogs, so mark them
        // fetched now. After this the picker queue is purely "never fetched"
        // (`tracks_missing_metadata` no longer re-derives missing fields each
        // run); newly-scanned songs make the same decision at insert time. The
        // backfill mirrors the old query's exclusion, so the set of songs still
        // pending review is unchanged — it just won't grow back if a field is
        // later cleared.
        let version: i64 = self.conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            self.conn.execute(
                &format!(
                    "UPDATE tracks SET discogs_meta_fetched_at = unixepoch()
                     WHERE discogs_meta_fetched_at IS NULL AND {METADATA_COMPLETE_SQL}"
                ),
                [],
            )?;
            self.conn.pragma_update(None, "user_version", 1)?;
        }
        Ok(())
    }

    fn add_column_if_missing(&self, table: &str, column: &str, ddl: &str) -> Result<()> {
        if !self.has_column(table, column)? {
            self.conn
                .execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {column} {ddl};"))?;
        }
        Ok(())
    }

    fn has_column(&self, table: &str, column: &str) -> Result<bool> {
        let mut stmt = self
            .conn
            .prepare(&format!("PRAGMA table_info({table})"))?;
        let names = stmt.query_map([], |r| r.get::<_, String>(1))?;
        for name in names {
            if name? == column {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Insert a scanned track, or update an already-known one. Returns the row id
    /// and whether it was newly inserted (preserving the id keeps playlists and
    /// analysis linked across rescans).
    ///
    /// A row is matched in two ways, in order:
    /// 1. by `source_path` — the file is in its known place;
    /// 2. by `fingerprint` — the file MOVED or was renamed (its old path no longer
    ///    exists on disk and exactly one orphaned row shares its content
    ///    fingerprint). The row is repointed at the new path, so playlists and
    ///    analysis stay attached instead of being orphaned.
    ///
    /// Audio properties always refresh from the file. Tag fields refresh from the
    /// file too — UNLESS the user has edited this track (`user_edited`), in which
    /// case the catalog wins and the scan leaves tags alone.
    pub fn upsert_scanned(&self, t: &ScannedTrack) -> Result<(Id, bool)> {
        // 1. Known path → in-place update.
        if let Some((id, user_edited)) = self.find_by_path(&t.source_path)? {
            self.refresh_existing(id, user_edited, t)?;
            return Ok((id as Id, false));
        }

        // 2. Unknown path, but the bytes match an orphaned row → it moved.
        if let Some((id, user_edited)) = self.find_moved(t)? {
            self.refresh_existing(id, user_edited, t)?;
            return Ok((id as Id, false));
        }

        // 3. Genuinely new.
        self.conn.execute(
            "INSERT INTO tracks (source_path, format, sample_rate, bit_depth,
               channels, duration_ms, bitrate_kbps, cover_thumb, fingerprint,
               src_size, src_mtime)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",
            params![
                t.source_path,
                format_str(t.format),
                t.properties.sample_rate_hz,
                t.properties.bit_depth,
                t.properties.channels,
                t.properties.duration_ms as i64,
                t.properties.bitrate_kbps,
                t.cover_thumb,
                t.fingerprint,
                t.src_size.map(|s| s as i64),
                t.src_mtime,
            ],
        )?;
        let id = self.conn.last_insert_rowid() as Id;
        self.write_all_tags(id, &t.tags)?;
        // The missing-attributes check runs once — here, at add time. A song
        // that already has the core album-level fields never needs the Discogs
        // picker, so mark it fetched immediately; an incomplete one stays
        // unmarked and is offered exactly once (applying a release, or a
        // no-match run, then marks it). No-op when the song is incomplete.
        self.conn.execute(
            &format!(
                "UPDATE tracks SET discogs_meta_fetched_at = unixepoch()
                 WHERE id = ?1 AND {METADATA_COMPLETE_SQL}"
            ),
            params![id as i64],
        )?;
        Ok((id, true))
    }

    /// Is a track already in the catalog at `source_path` with this exact
    /// `(size, mtime)` signature? When true, a rescan can skip re-reading the file
    /// — the bytes are unchanged since it was last scanned. Returns `false` when
    /// there's no row, or when the stored signature is NULL (rows from before the
    /// signature column existed), so those get rescanned once to populate it.
    pub fn track_unchanged(&self, source_path: &str, size: u64, mtime: i64) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT src_size, src_mtime FROM tracks WHERE source_path = ?1",
                params![source_path],
                |r| {
                    Ok((
                        r.get::<_, Option<i64>>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                    ))
                },
            )
            .optional()?
            .map(|(s, m)| s == Some(size as i64) && m == Some(mtime))
            .unwrap_or(false))
    }

    fn find_by_path(&self, source_path: &str) -> Result<Option<(i64, bool)>> {
        Ok(self
            .conn
            .query_row(
                "SELECT id, user_edited FROM tracks WHERE source_path = ?1",
                params![source_path],
                |r| Ok((r.get(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .optional()?)
    }

    /// Find the row a moved/renamed file belongs to: an existing track with the
    /// same `fingerprint` whose recorded path no longer exists on disk. Requires a
    /// *unique* missing candidate — if several orphans share the fingerprint we
    /// can't tell which one moved, so we decline and let the file be inserted fresh.
    fn find_moved(&self, t: &ScannedTrack) -> Result<Option<(i64, bool)>> {
        let Some(fp) = t.fingerprint.as_deref() else {
            return Ok(None);
        };
        let mut stmt = self.conn.prepare(
            "SELECT id, source_path, user_edited FROM tracks
             WHERE fingerprint = ?1 AND source_path <> ?2",
        )?;
        let rows = stmt
            .query_map(params![fp, t.source_path], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut missing = rows
            .into_iter()
            .filter(|(_, path, _)| !Path::new(path).exists());
        match (missing.next(), missing.next()) {
            (Some((id, _, user_edited)), None) => Ok(Some((id, user_edited))),
            _ => Ok(None), // none missing, or ambiguous (multiple orphans)
        }
    }

    /// Refresh the file-derived columns of an existing row (path, format, audio
    /// properties, cover, fingerprint). Used by both the path-match and the
    /// moved-file branches of `upsert_scanned`; in the path-match case the path
    /// is rewritten to its own current value (a no-op). Tag fields refresh only
    /// when the user hasn't taken the row over.
    fn refresh_existing(&self, id: i64, user_edited: bool, t: &ScannedTrack) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET source_path=?2, format=?3, sample_rate=?4, bit_depth=?5,
               channels=?6, duration_ms=?7, bitrate_kbps=?8, cover_thumb=?9, fingerprint=?10,
               src_size=?11, src_mtime=?12
             WHERE id=?1",
            params![
                id,
                t.source_path,
                format_str(t.format),
                t.properties.sample_rate_hz,
                t.properties.bit_depth,
                t.properties.channels,
                t.properties.duration_ms as i64,
                t.properties.bitrate_kbps,
                t.cover_thumb,
                t.fingerprint,
                t.src_size.map(|s| s as i64),
                t.src_mtime,
            ],
        )?;
        if !user_edited {
            self.write_all_tags(id as Id, &t.tags)?;
        }
        Ok(())
    }

    /// Repoint every track whose `source_path` is, or sits under, the `from`
    /// directory so that prefix becomes `to` — the explicit "I renamed/moved a
    /// source folder" fix. Matching is path-boundary aware: `from = /Music/Old`
    /// repoints `/Music/Old/x.mp3` but never the sibling `/Music/OldStuff/...`.
    /// A rewrite that would collide with another track's existing path is skipped.
    /// With `dry_run`, nothing is written — the report previews what would change.
    pub fn relink_prefix(&self, from: &str, to: &str, dry_run: bool) -> Result<RelinkReport> {
        let from = from.trim_end_matches('/');
        let to = to.trim_end_matches('/');

        let mut stmt = self.conn.prepare("SELECT id, source_path FROM tracks")?;
        let all = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        let planned: Vec<(i64, String, String)> = all
            .into_iter()
            .filter_map(|(id, path)| {
                strip_prefix_boundary(&path, from).map(|rest| {
                    let new = format!("{to}{rest}");
                    (id, path, new)
                })
            })
            .collect();

        let mut report = RelinkReport::default();
        let tx = self.conn.unchecked_transaction()?;
        for (id, old, new) in planned {
            let clash: Option<i64> = tx
                .query_row(
                    "SELECT id FROM tracks WHERE source_path=?1 AND id<>?2",
                    params![new, id],
                    |r| r.get(0),
                )
                .optional()?;
            if clash.is_some() {
                report.skipped += 1;
                continue;
            }
            if !dry_run {
                tx.execute(
                    "UPDATE tracks SET source_path=?2 WHERE id=?1",
                    params![id, new],
                )?;
            }
            report.moved += 1;
            report.changes.push((old, new));
        }
        if dry_run {
            // Leave the DB untouched; the transaction never wrote anything.
            drop(tx);
        } else {
            tx.commit()?;
        }
        Ok(report)
    }

    /// Fetch the stored cover thumbnail (PNG bytes) for a track, if any.
    pub fn get_cover_thumb(&self, id: Id) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT cover_thumb FROM tracks WHERE id=?1",
                params![id as i64],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Record (or replace) the external-artwork row for `track_id`. `png_bytes`
    /// is the small GUI thumbnail; `full_bytes` is the full-resolution image
    /// used for tag embedding. Pass `png_bytes = None` to record a "no match"
    /// attempt so we don't keep re-querying — call `clear_external_artwork` to
    /// allow a retry.
    pub fn set_external_artwork(
        &self,
        track_id: Id,
        source: &str,
        external_id: Option<&str>,
        url: Option<&str>,
        png_bytes: Option<&[u8]>,
        full_bytes: Option<&[u8]>,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO track_external_artwork
                 (track_id, source, external_id, url, png_bytes, full_bytes, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, unixepoch())
             ON CONFLICT(track_id) DO UPDATE SET
                 source      = excluded.source,
                 external_id = excluded.external_id,
                 url         = excluded.url,
                 png_bytes   = excluded.png_bytes,
                 full_bytes  = excluded.full_bytes,
                 fetched_at  = excluded.fetched_at",
            params![track_id as i64, source, external_id, url, png_bytes, full_bytes],
        )?;
        // Embeddable (full-res) artwork is a pending source-file write, just like
        // a tag edit: flag the track so the GUI's bulk "write edits to files"
        // button appears and `write_to_file` imprints the cover. No-match / legacy
        // thumbnail-only rows (no `full_bytes`) carry nothing to embed, so they
        // don't dirty the track.
        if full_bytes.is_some() {
            self.conn
                .execute("UPDATE tracks SET user_edited=1 WHERE id=?1", params![track_id as i64])?;
        }
        Ok(())
    }

    /// External-source artwork (PNG bytes) for `track_id`, if a successful
    /// fetch is on record. Returns `None` for both "no row" and "row records a
    /// no-match attempt".
    pub fn get_external_artwork(&self, track_id: Id) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT png_bytes FROM track_external_artwork WHERE track_id=?1",
                params![track_id as i64],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Full-resolution external artwork (PNG bytes) for `track_id`, suitable for
    /// embedding into the source file via `tag --write --art`. Returns `None`
    /// when there's no row, a no-match row, or only a legacy thumbnail (older
    /// catalogs fetched before full-res storage existed — re-fetch to populate).
    pub fn get_external_artwork_full(&self, track_id: Id) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT full_bytes FROM track_external_artwork WHERE track_id=?1",
                params![track_id as i64],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Discogs release id recorded for `track_id`'s external artwork, if any.
    /// Set when the user fetched cover art from a specific release, so callers
    /// can deep-link back to that exact release page. Returns `None` when there
    /// is no row, or the row logged a no-match attempt (no id).
    pub fn external_release_id(&self, track_id: Id) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT external_id FROM track_external_artwork WHERE track_id=?1",
                params![track_id as i64],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Every catalog track that is linked to a Discogs release (via the release
    /// id stored when its art/metadata was fetched), as `(release_id, track_id)`
    /// pairs. Used to cross-reference the vinyl collection against the catalog so
    /// the grid can show which records you already have a digital copy of. Rows
    /// whose `external_id` isn't a plain release-id number are skipped.
    pub fn release_track_links(&self) -> Result<Vec<(u64, Id)>> {
        let mut stmt = self.conn.prepare(
            "SELECT external_id, track_id FROM track_external_artwork
             WHERE external_id IS NOT NULL AND external_id <> ''",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as Id))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter_map(|(rid, tid)| rid.trim().parse::<u64>().ok().map(|rid| (rid, tid)))
            .collect())
    }

    /// Cross-reference the vinyl collection against the catalog and return every
    /// `(release_id, track_id)` link. The exact **release id** is the primary
    /// signal: a track whose fetched `external_id` equals the record's
    /// `release_id` (see [`Self::release_track_links`]). **Metadata** matching is
    /// only a *fallback*, applied to a record solely when it has no exact link —
    /// a track whose album equals the record's title (the release/album name), or
    /// whose title equals it and whose artist overlaps (catches singles/EPs named
    /// after their lead track). Matching ignores punctuation/spacing/case, so
    /// "Guardwatcher Pt. 1" and "guardwatcher pt 1" link.
    pub fn vinyl_catalog_links(&self, records: &[VinylRecord]) -> Result<Vec<(u64, Id)>> {
        // Primary: exact release-id links. Records already covered here are not
        // re-matched by the softer metadata pass below.
        let id_links = self.release_track_links()?;
        let linked_releases: std::collections::HashSet<u64> =
            id_links.iter().map(|(rid, _)| *rid).collect();

        // Lightweight catalog index: just the fields metadata matching needs.
        let mut stmt = self.conn.prepare(
            "SELECT id, coalesce(artist,''), coalesce(album,''), coalesce(title,'') FROM tracks",
        )?;
        struct Row {
            id: Id,
            artist: String,
            album: String,
            title: String,
        }
        let rows = stmt
            .query_map([], |r| {
                Ok(Row {
                    id: r.get::<_, i64>(0)? as Id,
                    artist: norm_match(&r.get::<_, String>(1)?),
                    album: norm_match(&r.get::<_, String>(2)?),
                    title: norm_match(&r.get::<_, String>(3)?),
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = id_links;
        for rec in records {
            // Fallback only — skip records the exact release id already matched.
            if linked_releases.contains(&rec.release_id) {
                continue;
            }
            let rtitle = norm_match(&rec.title);
            if rtitle.is_empty() {
                continue;
            }
            let rartist = norm_match(&rec.artist);
            for row in &rows {
                let album_hit = !row.album.is_empty() && row.album == rtitle;
                let title_hit = row.title == rtitle && artist_overlaps(&row.artist, &rartist);
                if album_hit || title_hit {
                    out.push((rec.release_id, row.id));
                }
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(out)
    }

    /// Forget any external-artwork row for `track_id`. Lets the next "fetch
    /// missing artwork" run re-attempt the lookup.
    pub fn clear_external_artwork(&self, track_id: Id) -> Result<()> {
        self.conn.execute(
            "DELETE FROM track_external_artwork WHERE track_id=?1",
            params![track_id as i64],
        )?;
        Ok(())
    }

    /// Return a cached Discogs [`ReleaseDetail`] for `release_id`, or `None` if it
    /// hasn't been fetched yet. A corrupt/old cached row (one that no longer
    /// deserializes) is treated as a miss, not an error, so a schema change to
    /// `ReleaseDetail` self-heals on the next fetch rather than wedging enrichment.
    pub fn cached_release(&self, release_id: &str) -> Result<Option<crate::discogs::ReleaseDetail>> {
        let json: Option<String> = self
            .conn
            .query_row(
                "SELECT detail_json FROM release_cache WHERE release_id=?1",
                params![release_id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(json.and_then(|j| serde_json::from_str(&j).ok()))
    }

    /// Store a fetched [`ReleaseDetail`] in the release cache, replacing any prior
    /// row for the same release. Release metadata is effectively immutable, so this
    /// is write-once in practice; the upsert just keeps a manual re-fetch idempotent.
    pub fn cache_release(&self, detail: &crate::discogs::ReleaseDetail) -> Result<()> {
        let json = serde_json::to_string(detail)
            .map_err(|e| Error::Invalid(format!("serializing release detail: {e}")))?;
        self.conn.execute(
            "INSERT INTO release_cache (release_id, detail_json) VALUES (?1, ?2)
             ON CONFLICT(release_id) DO UPDATE SET detail_json=excluded.detail_json,
                                                   fetched_at=unixepoch()",
            params![detail.release_id, json],
        )?;
        Ok(())
    }

    /// Resolve a release's detail from the cache, falling back to `fetch` (the
    /// network call) on a miss and caching the result. Centralizes the
    /// cache-then-fetch policy so every call site shares it without coupling the
    /// persistence layer to the network client — the caller passes the fetch as a
    /// closure (`|| client.fetch_release(id)`). A fetch error is propagated and
    /// nothing is cached, so a failed lookup retries next time.
    pub fn release_cached_or<F>(
        &self,
        release_id: &str,
        fetch: F,
    ) -> Result<crate::discogs::ReleaseDetail>
    where
        F: FnOnce() -> Result<crate::discogs::ReleaseDetail>,
    {
        if let Some(detail) = self.cached_release(release_id)? {
            return Ok(detail);
        }
        let detail = fetch()?;
        // A best-effort cache write must not fail the lookup the user asked for.
        let _ = self.cache_release(&detail);
        Ok(detail)
    }

    /// Track ids for which we have a successfully-fetched external artwork
    /// row (non-NULL bytes). Used by the GUI to decide whether to render a
    /// thumbnail for a track without an embedded cover.
    pub fn external_artwork_ids(&self) -> Result<Vec<Id>> {
        let mut stmt = self
            .conn
            .prepare("SELECT track_id FROM track_external_artwork WHERE png_bytes IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r? as Id);
        }
        Ok(out)
    }

    /// Track ids that have *full-resolution* external artwork on record — i.e.
    /// art the GUI/CLI can imprint into the source file (`tag --write --art`).
    /// Narrower than `external_artwork_ids`, which also counts thumbnail-only
    /// rows fetched before full-res storage existed.
    pub fn external_artwork_full_ids(&self) -> Result<Vec<Id>> {
        let mut stmt = self
            .conn
            .prepare("SELECT track_id FROM track_external_artwork WHERE full_bytes IS NOT NULL")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r? as Id);
        }
        Ok(out)
    }

    /// Tracks with no embedded cover and no external-artwork attempt logged.
    /// Drives the "fetch missing artwork" worker — returns only the fields it
    /// needs to query Discogs.
    pub fn tracks_missing_artwork(&self) -> Result<Vec<MissingArtwork>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.artist, t.title, t.album
             FROM tracks t
             LEFT JOIN track_external_artwork e ON e.track_id = t.id
             WHERE COALESCE(t.has_cover, 0) = 0 AND e.track_id IS NULL
             ORDER BY t.artist, t.title",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MissingArtwork {
                id: r.get::<_, i64>(0)? as Id,
                artist: r.get::<_, Option<String>>(1)?,
                title: r.get::<_, Option<String>>(2)?,
                album: r.get::<_, Option<String>>(3)?,
                // By construction these tracks have no external-artwork row.
                release_id: None,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Other tracks on the *same album* as `track_id` that have no cover of their
    /// own (no embedded art and no external-artwork row). "Same album" means a
    /// matching, non-empty album title under the same album identity — the album
    /// artist, falling back to the track artist when no album artist is set — both
    /// compared case-insensitively and trimmed so tag noise doesn't split a record.
    /// Used to propagate one chosen cover across a whole album: the returned ids
    /// are exactly the siblings that would benefit, leaving any that already have
    /// their own art untouched. Empty when `track_id` has no album, or no needy
    /// siblings exist.
    pub fn album_siblings_missing_art(&self, track_id: Id) -> Result<Vec<Id>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id
             FROM tracks t
             JOIN tracks me ON me.id = ?1
             LEFT JOIN track_external_artwork e ON e.track_id = t.id
             WHERE t.id <> me.id
               AND TRIM(COALESCE(me.album, '')) <> ''
               AND lower(TRIM(t.album)) = lower(TRIM(me.album))
               AND lower(COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist, ''))
                   = lower(COALESCE(NULLIF(TRIM(me.album_artist), ''), me.artist, ''))
               AND COALESCE(t.has_cover, 0) = 0
               AND (e.track_id IS NULL OR e.png_bytes IS NULL)
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![track_id as i64], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r? as Id);
        }
        Ok(out)
    }

    /// *Every* other track on the same album as `track_id`, regardless of whether
    /// it already has a cover. Same "same album" identity as
    /// [`Catalog::album_siblings_missing_art`]; used when the user chooses to
    /// overwrite album-mates' existing covers so the whole album matches.
    pub fn album_siblings(&self, track_id: Id) -> Result<Vec<Id>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id
             FROM tracks t
             JOIN tracks me ON me.id = ?1
             WHERE t.id <> me.id
               AND TRIM(COALESCE(me.album, '')) <> ''
               AND lower(TRIM(t.album)) = lower(TRIM(me.album))
               AND lower(COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist, ''))
                   = lower(COALESCE(NULLIF(TRIM(me.album_artist), ''), me.artist, ''))
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![track_id as i64], |r| r.get::<_, i64>(0))?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r? as Id);
        }
        Ok(out)
    }

    /// Every other track on the same album as `track_id`, with the name fields
    /// and a per-track `has_art` flag. Same "same album" identity as
    /// [`Catalog::album_siblings`]; this richer form lets the GUI list mates by
    /// name and pre-select the cover-less ones when copying a cover across an
    /// album.
    pub fn album_siblings_detailed(&self, track_id: Id) -> Result<Vec<AlbumSibling>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.artist, t.title,
                 (COALESCE(t.has_cover, 0) = 1
                  OR EXISTS (
                      SELECT 1 FROM track_external_artwork e
                      WHERE e.track_id = t.id AND e.png_bytes IS NOT NULL
                  )) AS has_art
             FROM tracks t
             JOIN tracks me ON me.id = ?1
             WHERE t.id <> me.id
               AND TRIM(COALESCE(me.album, '')) <> ''
               AND lower(TRIM(t.album)) = lower(TRIM(me.album))
               AND lower(COALESCE(NULLIF(TRIM(t.album_artist), ''), t.artist, ''))
                   = lower(COALESCE(NULLIF(TRIM(me.album_artist), ''), me.artist, ''))
             ORDER BY t.id",
        )?;
        let rows = stmt.query_map(params![track_id as i64], |r| {
            Ok(AlbumSibling {
                id: r.get::<_, i64>(0)? as Id,
                artist: r.get::<_, Option<String>>(1)?,
                title: r.get::<_, Option<String>>(2)?,
                has_art: r.get::<_, i64>(3)? != 0,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Whether `track_id` already has *some* cover — either an embedded one
    /// (`has_cover`) or a fetched external image. Used by the song-data picker to
    /// default the "set cover" toggle off when art already exists, so enriching a
    /// track's tags doesn't silently clobber a cover it already had.
    pub fn track_has_art(&self, track_id: Id) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT COALESCE(t.has_cover, 0) = 1
                 OR EXISTS (
                     SELECT 1 FROM track_external_artwork e
                     WHERE e.track_id = t.id AND e.png_bytes IS NOT NULL
                 )
             FROM tracks t WHERE t.id = ?1",
            params![track_id as i64],
            |r| r.get::<_, i64>(0),
        )? != 0)
    }

    /// Mark (or unmark) `track_id`'s fetched cover as superseding the file's
    /// embedded art. A no-op when the track has no external-artwork row. Set when
    /// the user overwrites an album-mate's cover so display and export both show
    /// the new art rather than the stale embedded one.
    pub fn set_prefer_external_artwork(&self, track_id: Id, prefer: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE track_external_artwork SET prefer_external = ?2 WHERE track_id = ?1",
            params![track_id as i64, prefer as i64],
        )?;
        Ok(())
    }

    /// Whether `track_id`'s fetched cover should supersede the embedded one — true
    /// only when a usable external image exists *and* it's flagged preferred.
    /// Callers use it to order the cover sources (external first when true).
    pub fn prefers_external_artwork(&self, track_id: Id) -> Result<bool> {
        Ok(self
            .conn
            .query_row(
                "SELECT prefer_external FROM track_external_artwork
                 WHERE track_id = ?1 AND png_bytes IS NOT NULL",
                params![track_id as i64],
                |r| r.get::<_, i64>(0),
            )
            .optional()?
            .map(|v| v != 0)
            .unwrap_or(false))
    }

    /// Tracks that still need a Discogs song-data fetch — i.e. never fetched
    /// (`discogs_meta_fetched_at IS NULL`). The "is it missing core fields?"
    /// decision is made once, when a track is added (see [`METADATA_COMPLETE_SQL`]
    /// in `upsert_scanned`): a complete song is marked fetched at insert and so
    /// never lands here, while an incomplete one is offered exactly once and then
    /// marked whatever the outcome (release applied, or no match). This query no
    /// longer re-derives completeness each run, so a song the user has handled —
    /// or that was complete on import — won't reappear, even if a field is later
    /// cleared. Unlike [`Catalog::tracks_missing_artwork`], having a cover does
    /// NOT exclude a track. Returns the minimum fields needed to search Discogs.
    pub fn tracks_missing_metadata(&self) -> Result<Vec<MissingArtwork>> {
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.artist, t.title, t.album, e.external_id
             FROM tracks t
             LEFT JOIN track_external_artwork e ON e.track_id = t.id
             WHERE t.discogs_meta_fetched_at IS NULL
             ORDER BY t.artist, t.title",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(MissingArtwork {
                id: r.get::<_, i64>(0)? as Id,
                artist: r.get::<_, Option<String>>(1)?,
                title: r.get::<_, Option<String>>(2)?,
                album: r.get::<_, Option<String>>(3)?,
                // The release the on-file artwork came from, if this track has
                // one — lets the worker fill tags without re-prompting.
                release_id: r.get::<_, Option<String>>(4)?,
            })
        })?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    }

    /// Mark a track as having completed a Discogs song-data fetch, so
    /// [`Catalog::tracks_missing_metadata`] no longer re-presents it. Called when
    /// the user applies a release or the run finds no match — not on skip, which
    /// leaves the track re-fetchable. Idempotent; stamps the current time.
    pub fn mark_metadata_fetched(&self, id: Id) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET discogs_meta_fetched_at = unixepoch() WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    /// Clear the song-data fetched mark for `track_ids` so a later run can revisit
    /// them. Empty slice clears every track (a full re-fetch reset). Returns the
    /// number of rows touched.
    pub fn clear_metadata_fetched(&self, track_ids: &[Id]) -> Result<usize> {
        let n = if track_ids.is_empty() {
            self.conn
                .execute("UPDATE tracks SET discogs_meta_fetched_at = NULL", [])?
        } else {
            let mut n = 0;
            for &id in track_ids {
                n += self.conn.execute(
                    "UPDATE tracks SET discogs_meta_fetched_at = NULL WHERE id = ?1",
                    params![id as i64],
                )?;
            }
            n
        };
        Ok(n)
    }

    /// Persist every Tags field to the row. Used by scan upserts (extended set)
    /// and by `update_tags` (which then also flips `user_edited`).
    fn write_all_tags(&self, id: Id, tags: &Tags) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET
                title=?2, artist=?3, album=?4, genre=?5, label=?6, year=?7,
                comment=?8, rating=?9,
                track_number=?10, track_total=?11, disc_number=?12, disc_total=?13,
                album_artist=?14, composer=?15, conductor=?16, remixer=?17,
                producer=?18, lyricist=?19, arranger=?20, performer=?21,
                mix_dj=?22, writer=?23,
                recording_date=?24, release_date=?25, original_release_date=?26,
                isrc=?27, barcode=?28, catalog_number=?29, publisher=?30,
                copyright=?31, release_country=?32,
                bpm_tag=?33, initial_key_tag=?34, mood=?35, grouping=?36,
                compilation=?37,
                subtitle=?38, description=?39, language=?40, script=?41,
                lyrics=?42, work=?43, movement=?44, movement_number=?45, movement_total=?46,
                encoded_by=?47, encoder_software=?48, encoder_settings=?49,
                original_artist=?50, original_album=?51,
                mb_recording_id=?52, mb_track_id=?53, mb_release_id=?54,
                mb_release_group_id=?55, mb_artist_id=?56, mb_release_artist_id=?57,
                mb_work_id=?58, mb_release_type=?59, acoust_id=?60,
                rg_track_gain=?61, rg_track_peak=?62, rg_album_gain=?63, rg_album_peak=?64,
                has_cover=?65
             WHERE id=?1",
            params![
                id as i64,
                tags.title, tags.artist, tags.album, tags.genre, tags.label, tags.year,
                tags.comment, tags.rating,
                tags.track_number, tags.track_total, tags.disc_number, tags.disc_total,
                tags.album_artist, tags.composer, tags.conductor, tags.remixer,
                tags.producer, tags.lyricist, tags.arranger, tags.performer,
                tags.mix_dj, tags.writer,
                tags.recording_date, tags.release_date, tags.original_release_date,
                tags.isrc, tags.barcode, tags.catalog_number, tags.publisher,
                tags.copyright, tags.release_country,
                tags.bpm_tag, tags.initial_key_tag, tags.mood, tags.grouping,
                tags.compilation.map(|b| b as i64),
                tags.subtitle, tags.description, tags.language, tags.script,
                tags.lyrics, tags.work, tags.movement, tags.movement_number, tags.movement_total,
                tags.encoded_by, tags.encoder_software, tags.encoder_settings,
                tags.original_artist, tags.original_album,
                tags.musicbrainz_recording_id, tags.musicbrainz_track_id, tags.musicbrainz_release_id,
                tags.musicbrainz_release_group_id, tags.musicbrainz_artist_id, tags.musicbrainz_release_artist_id,
                tags.musicbrainz_work_id, tags.musicbrainz_release_type, tags.acoust_id,
                tags.replay_gain_track_gain, tags.replay_gain_track_peak,
                tags.replay_gain_album_gain, tags.replay_gain_album_peak,
                tags.has_cover as i64,
            ],
        )?;
        Ok(())
    }

    /// List tracks, optionally filtering by a flexible free-text search across
    /// artist/title/album/genre/album_artist (case-insensitive). See
    /// [`search_filter`] for the term semantics. `limit` of 0 means no limit.
    pub fn list_tracks(&self, query: Option<&str>, limit: usize) -> Result<Vec<Track>> {
        let (filter_sql, filter_params) = search_filter(query, "");
        let limit = if limit == 0 { -1i64 } else { limit as i64 };
        let sql = format!(
            "SELECT {SELECT_COLS} FROM tracks
              WHERE {filter_sql}
              ORDER BY artist, title
              LIMIT {limit}"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let refs: Vec<&dyn rusqlite::ToSql> =
            filter_params.iter().map(|p| p as &dyn rusqlite::ToSql).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Every track id paired with its `added_at` unix timestamp (seconds since
    /// the epoch). `added_at` is catalog bookkeeping — when the row was first
    /// scanned in — not part of the [`Track`] domain model, so it's exposed
    /// separately. One query lets a view show or sort by recency ("recently
    /// added") without an extra lookup per row.
    pub fn added_at_all(&self) -> Result<Vec<(Id, i64)>> {
        let mut stmt = self.conn.prepare("SELECT id, added_at FROM tracks")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, i64>("id")? as Id, r.get::<_, i64>("added_at")?))
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// The "recently added" inbox: tracks that still need finishing work, newest
    /// first. A track appears here until it has been BOTH analyzed at the current
    /// analyzer `version` AND had its Discogs song-data fetched
    /// (`discogs_meta_fetched_at` set) — at which point it "expires" out of the
    /// view automatically. So it's a self-clearing to-do list of fresh imports:
    /// analyze + fetch a track and it leaves on the next reload. `query` filters
    /// the same fields as [`Catalog::list_tracks`].
    ///
    /// It's also time-bounded: a track only counts as "recent" while it was
    /// added within the last [`RECENTLY_ADDED_WINDOW_SECS`] (one day). After
    /// that it drops out regardless of whether the finishing work happened, so
    /// the inbox stays a short, fresh list rather than accumulating stale imports
    /// the user never got around to.
    pub fn list_recently_added(&self, query: Option<&str>, version: u32) -> Result<Vec<Track>> {
        let (filter_sql, filter_params) = search_filter(query, "");
        let sql = format!(
            "SELECT {SELECT_COLS} FROM tracks
              WHERE ({filter_sql})
                AND (unixepoch() - added_at) < {RECENTLY_ADDED_WINDOW_SECS}
                AND (
                  discogs_meta_fetched_at IS NULL
                  OR id NOT IN (SELECT track_id FROM analysis WHERE analyzer_version >= ?)
                )
              ORDER BY added_at DESC, id DESC"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = filter_params
            .into_iter()
            .map(|p| Box::new(p) as Box<dyn rusqlite::ToSql>)
            .collect();
        params.push(Box::new(version as i64));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Load specific tracks by id, optionally narrowed by the same `query` as
    /// [`Catalog::list_tracks`]. Used by the "recently added" view to keep tracks
    /// that have *just* finished (analyzed + fetched) pinned in place until the
    /// user leaves the tab — those rows are no longer "recent" by the inbox query,
    /// so they're re-fetched explicitly by id. Returns whatever subset of `ids`
    /// exists and matches the filter; order is unspecified (the caller re-sorts).
    pub fn list_tracks_by_ids(&self, ids: &[Id], query: Option<&str>) -> Result<Vec<Track>> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        let (filter_sql, filter_params) = search_filter(query, "");
        let placeholders = vec!["?"; ids.len()].join(",");
        let sql = format!(
            "SELECT {SELECT_COLS} FROM tracks
              WHERE id IN ({placeholders})
                AND ({filter_sql})"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        // Bind the id placeholders first, then the search-filter `%term%` params,
        // matching the `?` order in the statement.
        let mut params: Vec<Box<dyn rusqlite::ToSql>> =
            ids.iter().map(|&id| Box::new(id as i64) as Box<dyn rusqlite::ToSql>).collect();
        params.extend(filter_params.into_iter().map(|p| Box::new(p) as Box<dyn rusqlite::ToSql>));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// How many tracks are in the "recently added" inbox — i.e. not yet both
    /// analyzed at `version` and song-data fetched. Cheap (no `Track` building),
    /// so it can drive the sidebar badge on every refresh. See
    /// [`Catalog::list_recently_added`].
    pub fn count_recently_added(&self, version: u32) -> Result<u64> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks
              WHERE (unixepoch() - added_at) < ?1
                AND (
                  discogs_meta_fetched_at IS NULL
                  OR id NOT IN (SELECT track_id FROM analysis WHERE analyzer_version >= ?2)
                )",
            params![RECENTLY_ADDED_WINDOW_SECS, version as i64],
            |r| r.get(0),
        )?;
        Ok(n as u64)
    }

    /// Whether `id` is currently in the "recently added" inbox — the same
    /// predicate as [`Catalog::list_recently_added`], scoped to one track. Lets
    /// callers tailor messaging (e.g. only say "removed from Recently Added"
    /// when the track was actually there).
    pub fn is_recently_added(&self, id: Id, version: u32) -> Result<bool> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tracks
              WHERE id = ?1
                AND (unixepoch() - added_at) < ?2
                AND (
                  discogs_meta_fetched_at IS NULL
                  OR id NOT IN (SELECT track_id FROM analysis WHERE analyzer_version >= ?3)
                )",
            params![id as i64, RECENTLY_ADDED_WINDOW_SECS, version as i64],
            |r| r.get(0),
        )?;
        Ok(n > 0)
    }

    /// Tracks whose recorded `source_path` no longer exists on disk — the file
    /// was moved, renamed, or deleted out from under the catalog. The rows (and
    /// their cues/analysis/playlist links) are intact; only the locator is stale.
    /// Drives the `missing` command and any "missing files" view, and pairs with
    /// `relink_prefix` to repoint a whole moved folder at once.
    pub fn missing_tracks(&self) -> Result<Vec<Track>> {
        Ok(self
            .list_tracks(None, 0)?
            .into_iter()
            .filter(|t| !Path::new(&t.source_path).exists())
            .collect())
    }

    /// Every track whose source file no longer exists on disk, carried as the
    /// lightweight [`MissingTrack`] (id + path + fingerprint) the relocation
    /// search needs — no tags, no audio properties. Read-only.
    pub fn missing_tracks_detailed(&self) -> Result<Vec<MissingTrack>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, source_path, fingerprint FROM tracks")?;
        let all = stmt
            .query_map([], |r| {
                Ok(MissingTrack {
                    id: r.get::<_, i64>(0)? as Id,
                    source_path: r.get::<_, String>(1)?,
                    fingerprint: r.get::<_, Option<String>>(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(all
            .into_iter()
            .filter(|m| !Path::new(&m.source_path).exists())
            .collect())
    }

    /// How many tracks have a missing source file. Cheaper than
    /// [`Catalog::missing_tracks`] — it stats paths without building `Track`s —
    /// so it can drive the toolbar's "relocate" affordance on refresh.
    pub fn count_missing(&self) -> Result<u64> {
        let mut stmt = self.conn.prepare("SELECT source_path FROM tracks")?;
        let paths = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(paths.iter().filter(|p| !Path::new(p).exists()).count() as u64)
    }

    /// Short "Artist — Title" labels for every track whose source file is gone,
    /// in `source_path` order. Reads only the three columns a label needs (no
    /// `Track` building), so it's cheap enough to refresh the toolbar's relocate
    /// hover alongside [`Catalog::count_missing`]. Falls back to the file name
    /// when artist/title are blank so a row is never an empty line.
    pub fn missing_track_labels(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_path, artist, title FROM tracks ORDER BY source_path")?;
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, Option<String>>(1)?,
                    r.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows
            .into_iter()
            .filter(|(path, _, _)| !Path::new(path).exists())
            .map(|(path, artist, title)| {
                let artist = artist.unwrap_or_default();
                let artist = artist.trim();
                let title = title.unwrap_or_default();
                let title = title.trim();
                match (artist.is_empty(), title.is_empty()) {
                    (false, false) => format!("{artist} — {title}"),
                    (true, false) => title.to_string(),
                    (false, true) => artist.to_string(),
                    (true, true) => Path::new(&path)
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or(path),
                }
            })
            .collect())
    }

    /// Find duplicate tracks, in two flavors (see [`DuplicateKind`]):
    /// * **Identical** — tracks sharing a content fingerprint (the same audio,
    ///   imported twice). Always safe to keep one and drop the rest.
    /// * **SameTrack** — tracks with the same artist + title but differing files,
    ///   i.e. the same song in different formats/qualities; review by hand.
    ///
    /// A group that is *purely* identical (same fingerprint AND same artist/title,
    /// with no format diversity) is reported once, as Identical, never twice.
    /// Read-only: it never touches files or catalog rows.
    pub fn find_duplicates(&self) -> Result<Vec<DuplicateGroup>> {
        // Group keys the user dismissed as "not a duplicate" — never reported.
        let ignored = self.ignored_duplicate_keys()?;

        // id -> fingerprint for every track, loaded once.
        let mut fp_stmt = self.conn.prepare("SELECT id, fingerprint FROM tracks")?;
        let fp_of: std::collections::HashMap<i64, Option<String>> = fp_stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        drop(fp_stmt);

        let mut out = Vec::new();

        // --- Identical audio: ids sharing a non-empty fingerprint. ---
        let mut by_fp: std::collections::BTreeMap<String, Vec<i64>> = Default::default();
        for (id, fp) in &fp_of {
            if let Some(fp) = fp.as_deref().filter(|s| !s.is_empty()) {
                by_fp.entry(fp.to_string()).or_default().push(*id);
            }
        }
        for (fp, mut ids) in by_fp {
            if ids.len() < 2 {
                continue;
            }
            let key = format!("identical\u{1f}{fp}");
            if ignored.contains(&key) {
                continue;
            }
            ids.sort_unstable();
            let tracks = ids
                .iter()
                .map(|id| self.get_track_with_analysis(*id as Id))
                .collect::<Result<Vec<_>>>()?;
            out.push(DuplicateGroup { kind: DuplicateKind::Identical, tracks, key });
        }

        // --- Same track, different files: same normalized artist + title. ---
        // char(31) (unit separator) joins the two fields so distinct (artist,
        // title) pairs can't collide through concatenation.
        let mut at_stmt = self.conn.prepare(
            "SELECT lower(trim(artist)) || char(31) || lower(trim(title)) AS k, id
             FROM tracks
             WHERE trim(coalesce(artist,'')) <> '' AND trim(coalesce(title,'')) <> ''
             ORDER BY k, id",
        )?;
        let rows = at_stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(at_stmt);
        let mut by_key: std::collections::BTreeMap<String, Vec<i64>> = Default::default();
        for (k, id) in rows {
            by_key.entry(k).or_default().push(id);
        }
        for (at_key, ids) in by_key {
            if ids.len() < 2 {
                continue;
            }
            let key = format!("sametrack\u{1f}{at_key}");
            if ignored.contains(&key) {
                continue;
            }
            // Skip groups already fully covered by an Identical group: every
            // member carries the one same non-empty fingerprint.
            let fps: Vec<Option<&str>> = ids
                .iter()
                .map(|id| {
                    fp_of
                        .get(id)
                        .and_then(|o| o.as_deref())
                        .filter(|s| !s.is_empty())
                })
                .collect();
            let all_one_fp = fps[0].is_some() && fps.iter().all(|f| *f == fps[0]);
            if all_one_fp {
                continue;
            }
            let tracks = ids
                .iter()
                .map(|id| self.get_track_with_analysis(*id as Id))
                .collect::<Result<Vec<_>>>()?;
            out.push(DuplicateGroup { kind: DuplicateKind::SameTrack, tracks, key });
        }

        // --- Acoustic: same recording by perceptual fingerprint. ---
        // Catches duplicates whose bytes AND tags differ (cross-format re-encodes,
        // re-rips, retagged copies) — the ones that only reveal themselves on
        // playback. Only analyzed tracks carry a fingerprint.
        out.extend(self.acoustic_duplicate_groups(&ignored, &fp_of)?);

        Ok(out)
    }

    /// Cluster analyzed tracks by perceptual acoustic fingerprint (see
    /// [`analysis::fingerprint`](crate::analysis::fingerprint)). Tracks are
    /// candidate-paired only when their durations are within `DUR_WINDOW_MS` (the
    /// same recording keeps nearly the same length), then confirmed by fingerprint
    /// similarity; matches are unioned into groups. A group whose members all share
    /// one file fingerprint is already reported as `Identical`, so it's skipped.
    fn acoustic_duplicate_groups(
        &self,
        ignored: &std::collections::HashSet<String>,
        fp_of: &std::collections::HashMap<i64, Option<String>>,
    ) -> Result<Vec<DuplicateGroup>> {
        use crate::analysis::fingerprint;

        /// Max duration gap (ms) between two tracks still worth comparing. Encoder
        /// delay/padding shifts length by well under a second; this is generous
        /// headroom that still prunes the comparison space hard.
        const DUR_WINDOW_MS: i64 = 15_000;

        // (id, duration_ms, decoded fingerprint) for every analyzed track that has
        // a non-empty fingerprint, sorted by duration so the candidate window is a
        // forward scan.
        let mut stmt = self.conn.prepare(
            "SELECT t.id, t.duration_ms, a.audio_fingerprint
             FROM tracks t JOIN analysis a ON a.track_id = t.id
             WHERE a.audio_fingerprint IS NOT NULL
             ORDER BY t.duration_ms",
        )?;
        let rows: Vec<(i64, i64, Vec<u32>)> = stmt
            .query_map([], |r| {
                let bytes: Vec<u8> = r.get::<_, Option<Vec<u8>>>(2)?.unwrap_or_default();
                Ok((r.get::<_, i64>(0)?, r.get::<_, Option<i64>>(1)?.unwrap_or(0), bytes))
            })?
            .map(|r| r.map(|(id, dur, b)| (id, dur, fingerprint::from_bytes(&b))))
            .collect::<rusqlite::Result<Vec<_>>>()?;
        drop(stmt);

        // Confirm duplicate pairs in parallel. `best_ber` is the hot loop (it slides
        // one fingerprint over the other across ~97 lags), and every pair is
        // independent, so fan the rows out across cores. Each row only scans forward
        // until the duration window closes (rows are sorted by duration), which keeps
        // this from being a true O(n²) even before parallelism. Union-find then runs
        // serially over the confirmed pairs — cheap, and order-independent so the
        // resulting clusters are identical to the old sequential merge.
        use rayon::prelude::*;
        let matches: Vec<(usize, usize)> = (0..rows.len())
            .into_par_iter()
            .flat_map_iter(|i| {
                let mut local = Vec::new();
                for j in (i + 1)..rows.len() {
                    if rows[j].1 - rows[i].1 > DUR_WINDOW_MS {
                        break; // sorted by duration → no later j can be in window
                    }
                    if fingerprint::are_duplicates(&rows[i].2, &rows[j].2) {
                        local.push((i, j));
                    }
                }
                local
            })
            .collect();

        // Union-find over row indices.
        let mut parent: Vec<usize> = (0..rows.len()).collect();
        fn find(parent: &mut [usize], mut x: usize) -> usize {
            while parent[x] != x {
                parent[x] = parent[parent[x]];
                x = parent[x];
            }
            x
        }
        for (i, j) in matches {
            let (ri, rj) = (find(&mut parent, i), find(&mut parent, j));
            if ri != rj {
                parent[ri] = rj;
            }
        }

        // Gather clusters of size >= 2.
        let mut clusters: std::collections::BTreeMap<usize, Vec<i64>> = Default::default();
        for i in 0..rows.len() {
            let root = find(&mut parent, i);
            clusters.entry(root).or_default().push(rows[i].0);
        }

        let mut out = Vec::new();
        for ids in clusters.into_values() {
            if ids.len() < 2 {
                continue;
            }
            let mut ids = ids;
            ids.sort_unstable();
            // Already reported as Identical? (every member shares one file fp.)
            let file_fps: Vec<Option<&str>> = ids
                .iter()
                .map(|id| fp_of.get(id).and_then(|o| o.as_deref()).filter(|s| !s.is_empty()))
                .collect();
            if file_fps[0].is_some() && file_fps.iter().all(|f| *f == file_fps[0]) {
                continue;
            }
            let key = format!(
                "acoustic\u{1f}{}",
                ids.iter().map(|i| i.to_string()).collect::<Vec<_>>().join(",")
            );
            if ignored.contains(&key) {
                continue;
            }
            let tracks = ids
                .iter()
                .map(|id| self.get_track_with_analysis(*id as Id))
                .collect::<Result<Vec<_>>>()?;
            // Already reported as SameTrack? (every member shares one non-empty
            // normalized artist+title.) Acoustic's whole point is the cross-tag
            // case, so cede same-tag clusters to SameTrack and avoid duplicate rows.
            let at_key = |t: &Track| {
                let a = t.tags.artist.as_deref().unwrap_or("").trim().to_lowercase();
                let ti = t.tags.title.as_deref().unwrap_or("").trim().to_lowercase();
                (!a.is_empty() && !ti.is_empty()).then(|| format!("{a}\u{1f}{ti}"))
            };
            let first_at = at_key(&tracks[0]);
            if first_at.is_some() && tracks.iter().all(|t| at_key(t) == first_at) {
                continue;
            }
            out.push(DuplicateGroup { kind: DuplicateKind::Acoustic, tracks, key });
        }
        Ok(out)
    }

    /// The set of group keys the user has marked "not a duplicate".
    fn ignored_duplicate_keys(&self) -> Result<std::collections::HashSet<String>> {
        let mut stmt = self.conn.prepare("SELECT group_key FROM ignored_duplicates")?;
        let keys = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(keys)
    }

    /// Mark a duplicate group as "not a duplicate" (e.g. two distinct songs that
    /// happen to share an artist + title like "Untitled"). It won't be reported
    /// by [`find_duplicates`] again. Idempotent. Pass [`DuplicateGroup::key`].
    pub fn ignore_duplicate_group(&self, group_key: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR IGNORE INTO ignored_duplicates (group_key) VALUES (?1)",
            params![group_key],
        )?;
        Ok(())
    }

    /// Undo [`ignore_duplicate_group`] — the group can be reported again.
    pub fn unignore_duplicate_group(&self, group_key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM ignored_duplicates WHERE group_key = ?1",
            params![group_key],
        )?;
        Ok(())
    }

    /// Tracks in one playlist, in playlist order (`playlist_tracks.position`),
    /// optionally narrowed by the same substring filter `list_tracks` uses.
    /// Folders have no tracks and yield an empty vec. Mirrors `list_tracks` so
    /// the GUI can swap between the whole library and a single playlist.
    pub fn list_playlist_tracks(&self, playlist_id: Id, query: Option<&str>) -> Result<Vec<Track>> {
        let (filter_sql, filter_params) = search_filter(query, "t.");
        let sql = format!(
            "SELECT {SELECT_COLS} FROM tracks t
               JOIN playlist_tracks pt ON pt.track_id = t.id
              WHERE pt.playlist_id = ?
                AND ({filter_sql})
              ORDER BY pt.position, pt.track_id"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        // playlist_id binds first (its `?` leads the statement), then the filter params.
        let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(playlist_id as i64)];
        params.extend(filter_params.into_iter().map(|p| Box::new(p) as Box<dyn rusqlite::ToSql>));
        let refs: Vec<&dyn rusqlite::ToSql> = params.iter().map(|b| b.as_ref()).collect();
        let rows = stmt.query_map(refs.as_slice(), row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    pub fn get_track(&self, id: Id) -> Result<Track> {
        let sql = format!("SELECT {SELECT_COLS} FROM tracks WHERE id = ?1");
        self.conn
            .query_row(&sql, params![id as i64], row_to_track)
            .optional()?
            .ok_or_else(|| crate::error::Error::NotFound(format!("id {id}")))
    }

    /// Like [`get_track`], but also attaches the cached analysis. Used when
    /// building duplicate groups so [`best_copy_index`] can weigh the spectral
    /// transcode verdict, not just container format and bitrate.
    fn get_track_with_analysis(&self, id: Id) -> Result<Track> {
        let mut t = self.get_track(id)?;
        t.analysis = self.get_analysis(id)?;
        Ok(t)
    }

    /// Update tag fields for a track in the catalog and mark it user-edited so
    /// a later rescan won't overwrite the tag fields. Writes the full standardized
    /// tag set (so a UI can rename composer / album_artist / etc. too). Does not
    /// touch the source file — see the `tag` module for opt-in writeback.
    pub fn update_tags(&self, id: Id, tags: &Tags) -> Result<()> {
        self.write_all_tags(id, tags)?;
        self.conn
            .execute("UPDATE tracks SET user_edited=1 WHERE id=?1", params![id as i64])?;
        Ok(())
    }

    /// Number of tracks with catalog edits not yet written to their source file
    /// (`user_edited = 1`). Drives the GUI's bulk "write edits to files" affordance.
    pub fn count_edited(&self) -> Result<u64> {
        Ok(self.conn.query_row(
            "SELECT count(*) FROM tracks WHERE user_edited = 1",
            [],
            |r| r.get::<_, i64>(0),
        )? as u64)
    }

    /// Every track whose catalog tags have been edited but not yet written back
    /// to the source file (`user_edited = 1`), ordered for stable display.
    pub fn list_edited_tracks(&self) -> Result<Vec<Track>> {
        let sql = format!(
            "SELECT {SELECT_COLS} FROM tracks WHERE user_edited = 1 ORDER BY artist, title"
        );
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], row_to_track)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Clear the `user_edited` flag for a track. Call after the catalog's tags
    /// have been written into the source file, so the two are back in sync and
    /// the track no longer counts as "needs writing". A later rescan reads the
    /// file (which now carries the edits), so dropping the protection is safe.
    pub fn clear_user_edited(&self, id: Id) -> Result<()> {
        self.conn.execute(
            "UPDATE tracks SET user_edited = 0 WHERE id = ?1",
            params![id as i64],
        )?;
        Ok(())
    }

    /// Repoint a track at a new source file and format, refreshing audio
    /// properties — used after an in-place conversion changed the file. Tags and
    /// the playlist/analysis links are kept (the analysis row stays but will be
    /// re-run on the next `analyze`, since the file's size/mtime changed). Errors
    /// if the new path already belongs to a different track.
    pub fn relink_source(
        &self,
        id: Id,
        new_path: &str,
        format: Format,
        props: &AudioProperties,
    ) -> Result<()> {
        let clash: Option<i64> = self
            .conn
            .query_row(
                "SELECT id FROM tracks WHERE source_path=?1 AND id<>?2",
                params![new_path, id as i64],
                |r| r.get(0),
            )
            .optional()?;
        if clash.is_some() {
            return Err(Error::Invalid(format!(
                "another track already points at {new_path}"
            )));
        }
        let n = self.conn.execute(
            "UPDATE tracks SET source_path=?2, format=?3, sample_rate=?4, bit_depth=?5,
               channels=?6, duration_ms=?7, bitrate_kbps=?8 WHERE id=?1",
            params![
                id as i64,
                new_path,
                format_str(format),
                props.sample_rate_hz,
                props.bit_depth,
                props.channels,
                props.duration_ms as i64,
                props.bitrate_kbps,
            ],
        )?;
        if n == 0 {
            return Err(Error::NotFound(format!("id {id}")));
        }
        Ok(())
    }

    /// Whether a track needs (re)analysis: missing row, stale analyzer version,
    /// or the source file changed size/mtime since last analysis.
    pub fn needs_analysis(&self, id: Id, src_size: u64, src_mtime: i64, version: u32) -> Result<bool> {
        let row: Option<(i64, Option<i64>, Option<i64>)> = self
            .conn
            .query_row(
                "SELECT analyzer_version, src_size, src_mtime FROM analysis WHERE track_id=?1",
                params![id as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(match row {
            None => true,
            Some((v, size, mtime)) => {
                v as u32 != version
                    || size != Some(src_size as i64)
                    || mtime != Some(src_mtime)
            }
        })
    }

    /// Insert or replace a track's analysis, recording source size/mtime so the
    /// cache can detect file changes.
    pub fn save_analysis(
        &self,
        id: Id,
        a: &Analysis,
        src_size: u64,
        src_mtime: i64,
    ) -> Result<()> {
        let (tonic, mode) = match a.key {
            Some(k) => (Some(k.tonic.0 as i64), Some(mode_int(k.mode))),
            None => (None, None),
        };
        let beat_offset = a.beatgrid.beats.first().map(|b| b.position_ms as i64);
        self.conn.execute(
            "INSERT INTO analysis (track_id, bpm, key_tonic, key_mode, beat_offset_ms,
                 peak, loudness, waveform, content_hash, audio_fingerprint,
                 lowpass_hz, lowpass_edge, analyzer_version, src_size, src_mtime,
                 waveform_bands)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
             ON CONFLICT(track_id) DO UPDATE SET
                 bpm=?2, key_tonic=?3, key_mode=?4, beat_offset_ms=?5, peak=?6,
                 loudness=?7, waveform=?8, content_hash=?9, audio_fingerprint=?10,
                 lowpass_hz=?11, lowpass_edge=?12, analyzer_version=?13,
                 src_size=?14, src_mtime=?15, waveform_bands=?16,
                 analyzed_at=unixepoch()",
            params![
                id as i64,
                a.bpm,
                tonic,
                mode,
                beat_offset,
                a.peak,
                a.integrated_loudness_lufs,
                a.waveform_preview,
                a.content_hash,
                a.audio_fingerprint,
                a.lowpass_hz,
                a.lowpass_edge_db_per_khz,
                a.analyzer_version,
                src_size as i64,
                src_mtime,
                a.waveform_bands,
            ],
        )?;
        Ok(())
    }

    /// Load a track's analysis, if present.
    pub fn get_analysis(&self, id: Id) -> Result<Option<Analysis>> {
        let row = self
            .conn
            .query_row(
                "SELECT bpm, key_tonic, key_mode, beat_offset_ms, peak, loudness,
                     waveform, content_hash, analyzer_version, audio_fingerprint,
                     lowpass_hz, lowpass_edge, waveform_bands
                 FROM analysis WHERE track_id=?1",
                params![id as i64],
                |r| {
                    Ok(Analysis {
                        bpm: r.get::<_, Option<f64>>(0)?.map(|v| v as f32),
                        key: match (r.get::<_, Option<i64>>(1)?, r.get::<_, Option<i64>>(2)?) {
                            (Some(t), Some(m)) => {
                                Some(Key::new(PitchClass::new(t as u8), mode_from_int(m)))
                            }
                            _ => None,
                        },
                        beatgrid: match (r.get::<_, Option<i64>>(3)?, r.get::<_, Option<f64>>(0)?) {
                            (Some(off), Some(bpm)) => Beatgrid {
                                beats: vec![Beat {
                                    number: 1,
                                    position_ms: off as u64,
                                    bpm: bpm as f32,
                                }],
                            },
                            _ => Beatgrid::default(),
                        },
                        cues: Vec::new(),
                        peak: r.get::<_, Option<f64>>(4)?.map(|v| v as f32),
                        integrated_loudness_lufs: r.get::<_, Option<f64>>(5)?.map(|v| v as f32),
                        waveform_preview: r.get::<_, Option<Vec<u8>>>(6)?.unwrap_or_default(),
                        content_hash: r.get(7)?,
                        analyzer_version: r.get::<_, i64>(8)? as u32,
                        audio_fingerprint: r.get::<_, Option<Vec<u8>>>(9)?,
                        lowpass_hz: r.get::<_, Option<f64>>(10)?.map(|v| v as f32),
                        lowpass_edge_db_per_khz: r.get::<_, Option<f64>>(11)?.map(|v| v as f32),
                        waveform_bands: r.get::<_, Option<Vec<u8>>>(12)?.unwrap_or_default(),
                    })
                },
            )
            .optional()?;
        Ok(row)
    }

    pub fn count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM tracks", [], |r| r.get::<_, i64>(0))?
            as u64)
    }

    /// Remove every scanned track from the catalog. `analysis`,
    /// `playlist_tracks`, and `track_external_artwork` rows are deleted via
    /// their `ON DELETE CASCADE` foreign keys, so this empties the music
    /// library in one call. Playlists and folders themselves are kept (they
    /// just become empty). Returns the number of tracks removed. The freed
    /// pages are reclaimed with `VACUUM` so the file shrinks on disk.
    pub fn clear_tracks(&self) -> Result<u64> {
        let removed = self.conn.execute("DELETE FROM tracks", [])? as u64;
        self.conn.execute_batch("VACUUM")?;
        Ok(removed)
    }

    /// Permanently remove tracks from the catalog. Their `analysis`,
    /// `playlist_tracks`, and `track_external_artwork` rows are deleted via
    /// `ON DELETE CASCADE`, so a deleted track also drops out of every playlist
    /// and the analysis cache. Source files on disk are NEVER touched. Unknown
    /// ids are skipped. Returns the number of track rows actually removed.
    pub fn delete_tracks(&self, track_ids: &[Id]) -> Result<usize> {
        let mut removed = 0;
        for &tid in track_ids {
            removed += self
                .conn
                .execute("DELETE FROM tracks WHERE id=?1", params![tid as i64])?;
        }
        Ok(removed)
    }

    /// Delete tracks while handing their playlist memberships off to a kept copy
    /// — the operation behind "keep this duplicate, trash that one". Each
    /// `(keep, drop)` pair makes `keep` take over every playlist slot `drop`
    /// held, at the same position, so order is preserved; if `keep` is already
    /// in one of those playlists, `drop` is simply removed and `keep` stays
    /// where it was (no duplicate entry). The `drop` track is then deleted, so
    /// its `analysis`/artwork rows cascade away as with [`delete_tracks`]. Pairs
    /// where `keep == drop`, or whose `drop` id is unknown, are no-ops. The
    /// whole batch is one transaction. Returns the number of track rows removed.
    pub fn replace_tracks(&self, subs: &[(Id, Id)]) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let mut removed = 0;
        for &(keep, drop) in subs {
            if keep == drop {
                continue;
            }
            // 1. Drop `drop` from any playlist that already contains `keep`,
            //    so the reassignment below can't collide on the
            //    (playlist_id, track_id) primary key.
            tx.execute(
                "DELETE FROM playlist_tracks
                 WHERE track_id=?2
                   AND playlist_id IN (
                       SELECT playlist_id FROM playlist_tracks WHERE track_id=?1
                   )",
                params![keep as i64, drop as i64],
            )?;
            // 2. Hand the remaining slots to `keep`, keeping `drop`'s position.
            tx.execute(
                "UPDATE playlist_tracks SET track_id=?1 WHERE track_id=?2",
                params![keep as i64, drop as i64],
            )?;
            // 3. Remove the dropped track (cascades analysis/artwork rows).
            removed += tx.execute("DELETE FROM tracks WHERE id=?1", params![drop as i64])?;
        }
        tx.commit()?;
        Ok(removed)
    }

    // --- Vinyl collection (Discogs-backed cache) -----------------------------

    /// Insert or update a vinyl-collection record's metadata. Keyed on
    /// `instance_id`; a refresh re-runs this for every item, so the cached
    /// `cover_png` is deliberately left untouched here (covers are downloaded
    /// once and survive metadata refreshes — see [`Catalog::set_vinyl_cover`]).
    pub fn upsert_vinyl(&self, rec: &VinylRecord) -> Result<()> {
        self.conn.execute(
            "INSERT INTO vinyl_collection
                 (instance_id, release_id, title, artist, year, label,
                  catalog_number, format, thumb_url, cover_url, added, fetched_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, unixepoch())
             ON CONFLICT(instance_id) DO UPDATE SET
                 release_id     = excluded.release_id,
                 title          = excluded.title,
                 artist         = excluded.artist,
                 year           = excluded.year,
                 label          = excluded.label,
                 catalog_number = excluded.catalog_number,
                 format         = excluded.format,
                 thumb_url      = excluded.thumb_url,
                 cover_url      = excluded.cover_url,
                 added          = excluded.added,
                 fetched_at     = excluded.fetched_at",
            params![
                rec.instance_id as i64,
                rec.release_id as i64,
                rec.title,
                rec.artist,
                rec.year.map(|y| y as i64),
                rec.label,
                rec.catalog_number,
                rec.format,
                rec.thumb_url,
                rec.cover_url,
                rec.added,
            ],
        )?;
        Ok(())
    }

    /// Every cached vinyl record, ordered for the collection grid (artist, then
    /// title). `has_cover` reflects whether a cover image is cached.
    pub fn list_vinyl(&self) -> Result<Vec<VinylRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT instance_id, release_id, title, artist, year, label,
                    catalog_number, format, thumb_url, cover_url, added,
                    cover_png IS NOT NULL
             FROM vinyl_collection
             ORDER BY artist COLLATE NOCASE, title COLLATE NOCASE, instance_id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok(VinylRecord {
                    instance_id: r.get::<_, i64>(0)? as u64,
                    release_id: r.get::<_, i64>(1)? as u64,
                    title: r.get(2)?,
                    artist: r.get(3)?,
                    year: r.get::<_, Option<i64>>(4)?.map(|y| y as u16),
                    label: r.get(5)?,
                    catalog_number: r.get(6)?,
                    format: r.get(7)?,
                    thumb_url: r.get(8)?,
                    cover_url: r.get(9)?,
                    added: r.get(10)?,
                    has_cover: r.get::<_, i64>(11)? != 0,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Number of cached vinyl records (drives the sidebar count).
    pub fn vinyl_count(&self) -> Result<u64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM vinyl_collection", [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }

    /// `(instance_id, cover_url)` for every record that has a cover URL but no
    /// cached image yet — the work list a refresh downloads covers for.
    pub fn vinyl_missing_covers(&self) -> Result<Vec<(u64, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT instance_id, cover_url FROM vinyl_collection
             WHERE cover_png IS NULL AND cover_url IS NOT NULL AND cover_url <> ''",
        )?;
        let rows = stmt
            .query_map([], |r| {
                Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Store a downloaded cover image (downscaled PNG) for one record.
    pub fn set_vinyl_cover(&self, instance_id: u64, png: &[u8]) -> Result<()> {
        self.conn.execute(
            "UPDATE vinyl_collection SET cover_png=?2 WHERE instance_id=?1",
            params![instance_id as i64, png],
        )?;
        Ok(())
    }

    /// Cached cover image bytes for one record, if downloaded.
    pub fn vinyl_cover(&self, instance_id: u64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row(
                "SELECT cover_png FROM vinyl_collection WHERE instance_id=?1",
                params![instance_id as i64],
                |r| r.get::<_, Option<Vec<u8>>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Drop cached records whose `instance_id` isn't in `keep` — i.e. items the
    /// user removed from their Discogs collection since the last refresh. Call
    /// after upserting a fresh fetch so the local cache mirrors Discogs exactly.
    pub fn prune_vinyl_not_in(&self, keep: &[u64]) -> Result<usize> {
        // No fetched items → the collection is empty (or the fetch returned
        // nothing); clear the cache wholesale.
        if keep.is_empty() {
            let n = self.conn.execute("DELETE FROM vinyl_collection", [])?;
            return Ok(n);
        }
        let list = keep
            .iter()
            .map(|id| (*id as i64).to_string())
            .collect::<Vec<_>>()
            .join(",");
        let n = self.conn.execute(
            &format!("DELETE FROM vinyl_collection WHERE instance_id NOT IN ({list})"),
            [],
        )?;
        Ok(n)
    }

    // --- Playlists (Phase 3) -------------------------------------------------

    /// Create a playlist (or folder with `is_folder`) under an optional parent
    /// folder. New entries are appended after existing siblings. Returns the id.
    pub fn create_playlist(&self, name: &str, parent: Option<Id>, is_folder: bool) -> Result<Id> {
        if let Some(p) = parent {
            self.expect_folder(p)?;
        }
        let position = self.next_sibling_position(parent)?;
        self.conn.execute(
            "INSERT INTO playlists (name, parent_id, is_folder, position)
             VALUES (?1, ?2, ?3, ?4)",
            params![name, parent.map(|p| p as i64), is_folder as i64, position],
        )?;
        Ok(self.conn.last_insert_rowid() as Id)
    }

    /// All playlists and folders, each with ordered `track_ids`, sorted by
    /// (parent, sibling position). Callers render the tree from `parent`.
    pub fn list_playlists(&self) -> Result<Vec<Playlist>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, name, parent_id, is_folder FROM playlists
             ORDER BY parent_id IS NOT NULL, parent_id, position, id",
        )?;
        let metas = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)? as Id,
                    r.get::<_, String>(1)?,
                    r.get::<_, Option<i64>>(2)?.map(|v| v as Id),
                    r.get::<_, i64>(3)? != 0,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;

        let mut out = Vec::with_capacity(metas.len());
        for (id, name, parent, is_folder) in metas {
            let track_ids = if is_folder {
                Vec::new()
            } else {
                self.playlist_track_ids(id)?
            };
            out.push(Playlist { id, name, parent, is_folder, track_ids });
        }
        Ok(out)
    }

    /// One playlist (or folder) with its ordered `track_ids`.
    pub fn get_playlist(&self, id: Id) -> Result<Playlist> {
        let (name, parent, is_folder) = self
            .conn
            .query_row(
                "SELECT name, parent_id, is_folder FROM playlists WHERE id=?1",
                params![id as i64],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, Option<i64>>(1)?.map(|v| v as Id),
                        r.get::<_, i64>(2)? != 0,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("playlist {id}")))?;
        let track_ids = if is_folder {
            Vec::new()
        } else {
            self.playlist_track_ids(id)?
        };
        Ok(Playlist { id, name, parent, is_folder, track_ids })
    }

    /// Rename a playlist or folder.
    pub fn rename_playlist(&self, id: Id, name: &str) -> Result<()> {
        let n = self
            .conn
            .execute("UPDATE playlists SET name=?2 WHERE id=?1", params![id as i64, name])?;
        if n == 0 {
            return Err(Error::NotFound(format!("playlist {id}")));
        }
        Ok(())
    }

    /// Move a playlist/folder under a new parent folder (`None` = top level).
    /// Rejects moves that would create a cycle or nest under a non-folder.
    pub fn move_playlist(&self, id: Id, parent: Option<Id>) -> Result<()> {
        self.get_playlist(id)?; // existence
        if let Some(p) = parent {
            if p == id {
                return Err(Error::Invalid("a playlist cannot be its own parent".into()));
            }
            self.expect_folder(p)?;
            if self.is_descendant(p, id)? {
                return Err(Error::Invalid(
                    "cannot move a folder into one of its own descendants".into(),
                ));
            }
        }
        let position = self.next_sibling_position(parent)?;
        self.conn.execute(
            "UPDATE playlists SET parent_id=?2, position=?3 WHERE id=?1",
            params![id as i64, parent.map(|p| p as i64), position],
        )?;
        Ok(())
    }

    /// Delete a playlist or folder. Folders cascade to descendants and all
    /// track links (ON DELETE CASCADE). Source tracks are never touched.
    pub fn delete_playlist(&self, id: Id) -> Result<()> {
        let n = self
            .conn
            .execute("DELETE FROM playlists WHERE id=?1", params![id as i64])?;
        if n == 0 {
            return Err(Error::NotFound(format!("playlist {id}")));
        }
        Ok(())
    }

    /// Append tracks to a playlist, skipping any already present. Returns how
    /// many were newly added. Errors if the target is a folder or a track id is
    /// unknown.
    pub fn add_tracks(&self, id: Id, track_ids: &[Id]) -> Result<usize> {
        self.expect_playlist(id)?;
        let mut position = self.next_track_position(id)?;
        let mut added = 0;
        for &tid in track_ids {
            self.expect_track(tid)?;
            let n = self.conn.execute(
                "INSERT OR IGNORE INTO playlist_tracks (playlist_id, track_id, position)
                 VALUES (?1, ?2, ?3)",
                params![id as i64, tid as i64, position],
            )?;
            if n > 0 {
                position += 1;
                added += 1;
            }
        }
        Ok(added)
    }

    /// Remove tracks from a playlist. Returns how many links were removed.
    pub fn remove_tracks(&self, id: Id, track_ids: &[Id]) -> Result<usize> {
        self.expect_playlist(id)?;
        let mut removed = 0;
        for &tid in track_ids {
            removed += self.conn.execute(
                "DELETE FROM playlist_tracks WHERE playlist_id=?1 AND track_id=?2",
                params![id as i64, tid as i64],
            )?;
        }
        Ok(removed)
    }

    /// Reorder a playlist's tracks. `ordered` must be a permutation of the
    /// playlist's current members (same set, any order); positions are rewritten
    /// to match.
    pub fn reorder_tracks(&self, id: Id, ordered: &[Id]) -> Result<()> {
        self.expect_playlist(id)?;
        let current = self.playlist_track_ids(id)?;
        let cur_set: std::collections::BTreeSet<Id> = current.iter().copied().collect();
        let new_set: std::collections::BTreeSet<Id> = ordered.iter().copied().collect();
        if ordered.len() != current.len() || cur_set != new_set {
            return Err(Error::Invalid(
                "reorder list must be a permutation of the playlist's current tracks".into(),
            ));
        }
        for (pos, &tid) in ordered.iter().enumerate() {
            self.conn.execute(
                "UPDATE playlist_tracks SET position=?3 WHERE playlist_id=?1 AND track_id=?2",
                params![id as i64, tid as i64, pos as i64],
            )?;
        }
        Ok(())
    }

    fn playlist_track_ids(&self, id: Id) -> Result<Vec<Id>> {
        let mut stmt = self.conn.prepare(
            "SELECT track_id FROM playlist_tracks WHERE playlist_id=?1 ORDER BY position, track_id",
        )?;
        let ids = stmt
            .query_map(params![id as i64], |r| r.get::<_, i64>(0).map(|v| v as Id))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(ids)
    }

    fn next_sibling_position(&self, parent: Option<Id>) -> Result<i64> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT max(position) FROM playlists WHERE parent_id IS ?1",
            params![parent.map(|p| p as i64)],
            |r| r.get(0),
        )?;
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }

    fn next_track_position(&self, id: Id) -> Result<i64> {
        let max: Option<i64> = self.conn.query_row(
            "SELECT max(position) FROM playlist_tracks WHERE playlist_id=?1",
            params![id as i64],
            |r| r.get(0),
        )?;
        Ok(max.map(|m| m + 1).unwrap_or(0))
    }

    fn playlist_is_folder(&self, id: Id) -> Result<bool> {
        self.conn
            .query_row(
                "SELECT is_folder FROM playlists WHERE id=?1",
                params![id as i64],
                |r| r.get::<_, i64>(0).map(|v| v != 0),
            )
            .optional()?
            .ok_or_else(|| Error::NotFound(format!("playlist {id}")))
    }

    fn expect_folder(&self, id: Id) -> Result<()> {
        if self.playlist_is_folder(id)? {
            Ok(())
        } else {
            Err(Error::Invalid(format!("{id} is a playlist, not a folder")))
        }
    }

    fn expect_playlist(&self, id: Id) -> Result<()> {
        if self.playlist_is_folder(id)? {
            Err(Error::Invalid(format!(
                "{id} is a folder; tracks belong to playlists, not folders"
            )))
        } else {
            Ok(())
        }
    }

    fn expect_track(&self, id: Id) -> Result<()> {
        let exists: bool = self.conn.query_row(
            "SELECT 1 FROM tracks WHERE id=?1",
            params![id as i64],
            |_| Ok(true),
        ).optional()?.unwrap_or(false);
        if exists {
            Ok(())
        } else {
            Err(Error::NotFound(format!("track {id}")))
        }
    }

    /// Whether `candidate` is `ancestor` or nested anywhere beneath it.
    fn is_descendant(&self, candidate: Id, ancestor: Id) -> Result<bool> {
        let mut cur = Some(candidate);
        while let Some(c) = cur {
            if c == ancestor {
                return Ok(true);
            }
            cur = self
                .conn
                .query_row(
                    "SELECT parent_id FROM playlists WHERE id=?1",
                    params![c as i64],
                    |r| r.get::<_, Option<i64>>(0),
                )
                .optional()?
                .flatten()
                .map(|v| v as Id);
        }
        Ok(false)
    }
}

/// If `path` is exactly `prefix` or sits beneath it at a path boundary, return
/// the remainder (including the leading separator, e.g. `/x.mp3`). Otherwise
/// `None`. `prefix` is assumed already stripped of any trailing separator.
/// Keeps `relink_prefix` from matching `/Music/OldStuff` against `/Music/Old`.
fn strip_prefix_boundary(path: &str, prefix: &str) -> Option<String> {
    let rest = path.strip_prefix(prefix)?;
    if rest.is_empty() || rest.starts_with('/') {
        Some(rest.to_string())
    } else {
        None
    }
}

/// Normalize a title/album/artist for fuzzy linking: lowercase, split on any
/// non-alphanumeric run, canonicalize each token, and rejoin with single spaces.
/// Collapses punctuation and spacing differences so "Guardwatcher Pt. 1" ==
/// "guardwatcher pt 1", while keeping word boundaries (so unrelated strings don't
/// accidentally merge). Token canonicalization also bridges the abbreviation and
/// numeral spellings common in compilation titles, so a downloaded "Club Styling
/// Vol. 2" links to the Discogs release "Club Styling Volume Two".
fn norm_match(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(canon_token)
        .collect::<Vec<_>>()
        .join(" ")
}

/// Canonicalize a single normalized token to a common form so that equivalent
/// spellings compare equal: expand the abbreviations that show up in release
/// titles ("volume"→"vol", "part"→"pt") and fold the small number words used for
/// volumes/parts to their digit ("two"→"2"). Anything else passes through.
fn canon_token(t: &str) -> String {
    match t {
        "volume" => "vol".to_string(),
        "part" => "pt".to_string(),
        "zero" => "0".to_string(),
        "one" => "1".to_string(),
        "two" => "2".to_string(),
        "three" => "3".to_string(),
        "four" => "4".to_string(),
        "five" => "5".to_string(),
        "six" => "6".to_string(),
        "seven" => "7".to_string(),
        "eight" => "8".to_string(),
        "nine" => "9".to_string(),
        "ten" => "10".to_string(),
        "eleven" => "11".to_string(),
        "twelve" => "12".to_string(),
        _ => t.to_string(),
    }
}

/// Whether two already-normalized artist strings plausibly refer to the same act:
/// equal, or one contained in the other (handles "A" vs "A, B" multi-artist
/// credits). Empty on either side is never a match — we don't link on no evidence.
fn artist_overlaps(a: &str, b: &str) -> bool {
    !a.is_empty() && !b.is_empty() && (a == b || a.contains(b) || b.contains(a))
}

fn format_str(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "mp3",
        Format::Aac => "aac",
        Format::Wav => "wav",
        Format::Aiff => "aiff",
        Format::Flac => "flac",
        Format::Other => "other",
    }
}

fn format_from_str(s: &str) -> Format {
    match s {
        "mp3" => Format::Mp3,
        "aac" => Format::Aac,
        "wav" => Format::Wav,
        "aiff" => Format::Aiff,
        "flac" => Format::Flac,
        _ => Format::Other,
    }
}

/// Every column row_to_track reads, in one place so list_tracks/get_track stay in sync.
const SELECT_COLS: &str = "id, source_path, format, sample_rate, bit_depth, channels,
    duration_ms, bitrate_kbps,
    title, artist, album, genre, label, year, comment, rating,
    track_number, track_total, disc_number, disc_total,
    album_artist, composer, conductor, remixer, producer, lyricist, arranger,
    performer, mix_dj, writer,
    recording_date, release_date, original_release_date,
    isrc, barcode, catalog_number, publisher, copyright, release_country,
    bpm_tag, initial_key_tag, mood, grouping, compilation,
    subtitle, description, language, script, lyrics, work, movement,
    movement_number, movement_total,
    encoded_by, encoder_software, encoder_settings, original_artist, original_album,
    mb_recording_id, mb_track_id, mb_release_id, mb_release_group_id,
    mb_artist_id, mb_release_artist_id, mb_work_id, mb_release_type, acoust_id,
    rg_track_gain, rg_track_peak, rg_album_gain, rg_album_peak, has_cover";

/// Build a SQL WHERE fragment (and its bound `%term%` params) for a flexible
/// free-text search across the human-visible text columns
/// (artist/title/album/genre/album_artist, case-insensitive).
///
/// The query is split on whitespace into terms; every term must match at least
/// one column, AND-ed across terms and OR-ed across columns within a term. So
/// `"apple dj pear"` finds the track titled "Apple" by artist "DJ Pear" even
/// though no single column holds the whole phrase — order doesn't matter either.
/// A blank/`None` query yields the always-true `"1"` fragment and no params.
///
/// `prefix` is a table alias plus dot (e.g. `"t."` for a join) or `""`. The
/// fragment uses positional `?` placeholders, so its params must be bound in
/// order ahead of any params that follow it in the statement.
fn search_filter(query: Option<&str>, prefix: &str) -> (String, Vec<String>) {
    let terms: Vec<String> = query
        .into_iter()
        .flat_map(|q| q.split_whitespace())
        .map(|t| format!("%{}%", t.to_lowercase()))
        .collect();
    if terms.is_empty() {
        return ("1".to_string(), Vec::new());
    }
    let clause = format!(
        "(lower(coalesce({p}artist,'')) LIKE ? \
          OR lower(coalesce({p}title,'')) LIKE ? \
          OR lower(coalesce({p}album,'')) LIKE ? \
          OR lower(coalesce({p}genre,'')) LIKE ? \
          OR lower(coalesce({p}album_artist,'')) LIKE ?)",
        p = prefix
    );
    let sql = vec![clause; terms.len()].join(" AND ");
    // Each term binds its `%term%` once per column (5 columns).
    let params = terms.into_iter().flat_map(|t| std::iter::repeat(t).take(5)).collect();
    (sql, params)
}

fn row_to_track(r: &Row) -> rusqlite::Result<Track> {
    let format: String = r.get("format")?;
    Ok(Track {
        id: r.get::<_, i64>("id")? as Id,
        source_path: r.get("source_path")?,
        format: format_from_str(&format),
        properties: Some(AudioProperties {
            sample_rate_hz: r.get::<_, Option<i64>>("sample_rate")?.unwrap_or(0) as u32,
            bit_depth: r.get::<_, Option<i64>>("bit_depth")?.map(|v| v as u8),
            channels: r.get::<_, Option<i64>>("channels")?.unwrap_or(0) as u8,
            duration_ms: r.get::<_, Option<i64>>("duration_ms")?.unwrap_or(0) as u64,
            bitrate_kbps: r.get::<_, Option<i64>>("bitrate_kbps")?.map(|v| v as u32),
        }),
        tags: Tags {
            title: r.get("title")?,
            artist: r.get("artist")?,
            album: r.get("album")?,
            genre: r.get("genre")?,
            label: r.get("label")?,
            year: r.get::<_, Option<i64>>("year")?.map(|v| v as u16),
            comment: r.get("comment")?,
            rating: r.get::<_, Option<i64>>("rating")?.map(|v| v as u8),
            track_number: r.get::<_, Option<i64>>("track_number")?.map(|v| v as u16),
            track_total: r.get::<_, Option<i64>>("track_total")?.map(|v| v as u16),
            disc_number: r.get::<_, Option<i64>>("disc_number")?.map(|v| v as u16),
            disc_total: r.get::<_, Option<i64>>("disc_total")?.map(|v| v as u16),
            album_artist: r.get("album_artist")?,
            composer: r.get("composer")?,
            conductor: r.get("conductor")?,
            remixer: r.get("remixer")?,
            producer: r.get("producer")?,
            lyricist: r.get("lyricist")?,
            arranger: r.get("arranger")?,
            performer: r.get("performer")?,
            mix_dj: r.get("mix_dj")?,
            writer: r.get("writer")?,
            recording_date: r.get("recording_date")?,
            release_date: r.get("release_date")?,
            original_release_date: r.get("original_release_date")?,
            isrc: r.get("isrc")?,
            barcode: r.get("barcode")?,
            catalog_number: r.get("catalog_number")?,
            publisher: r.get("publisher")?,
            copyright: r.get("copyright")?,
            release_country: r.get("release_country")?,
            bpm_tag: r.get::<_, Option<f64>>("bpm_tag")?.map(|v| v as f32),
            initial_key_tag: r.get("initial_key_tag")?,
            mood: r.get("mood")?,
            grouping: r.get("grouping")?,
            compilation: r.get::<_, Option<i64>>("compilation")?.map(|v| v != 0),
            subtitle: r.get("subtitle")?,
            description: r.get("description")?,
            language: r.get("language")?,
            script: r.get("script")?,
            lyrics: r.get("lyrics")?,
            work: r.get("work")?,
            movement: r.get("movement")?,
            movement_number: r.get::<_, Option<i64>>("movement_number")?.map(|v| v as u16),
            movement_total: r.get::<_, Option<i64>>("movement_total")?.map(|v| v as u16),
            encoded_by: r.get("encoded_by")?,
            encoder_software: r.get("encoder_software")?,
            encoder_settings: r.get("encoder_settings")?,
            original_artist: r.get("original_artist")?,
            original_album: r.get("original_album")?,
            musicbrainz_recording_id: r.get("mb_recording_id")?,
            musicbrainz_track_id: r.get("mb_track_id")?,
            musicbrainz_release_id: r.get("mb_release_id")?,
            musicbrainz_release_group_id: r.get("mb_release_group_id")?,
            musicbrainz_artist_id: r.get("mb_artist_id")?,
            musicbrainz_release_artist_id: r.get("mb_release_artist_id")?,
            musicbrainz_work_id: r.get("mb_work_id")?,
            musicbrainz_release_type: r.get("mb_release_type")?,
            acoust_id: r.get("acoust_id")?,
            replay_gain_track_gain: r.get::<_, Option<f64>>("rg_track_gain")?.map(|v| v as f32),
            replay_gain_track_peak: r.get::<_, Option<f64>>("rg_track_peak")?.map(|v| v as f32),
            replay_gain_album_gain: r.get::<_, Option<f64>>("rg_album_gain")?.map(|v| v as f32),
            replay_gain_album_peak: r.get::<_, Option<f64>>("rg_album_peak")?.map(|v| v as f32),
            has_cover: r.get::<_, Option<i64>>("has_cover")?.unwrap_or(0) != 0,
        },
        analysis: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(path: &str, artist: &str, genre: &str, duration_ms: u64) -> ScannedTrack {
        ScannedTrack {
            source_path: path.to_string(),
            format: Format::Mp3,
            properties: AudioProperties {
                sample_rate_hz: 44100,
                bit_depth: None,
                channels: 2,
                duration_ms,
                bitrate_kbps: Some(320),
            },
            tags: Tags {
                artist: Some(artist.into()),
                genre: Some(genre.into()),
                title: Some("T".into()),
                ..Default::default()
            },
            cover_thumb: None,
            fingerprint: None,
            src_size: None,
            src_mtime: None,
        }
    }

    /// Build a copy for `best_copy_index` tests: a format, a bitrate, and an
    /// optional spectral verdict expressed via the raw lowpass measurements.
    fn copy(format: Format, bitrate: u32, verdict: Option<TranscodeVerdict>) -> Track {
        let analysis = verdict.map(|v| {
            let (lowpass_hz, lowpass_edge_db_per_khz) = match v {
                TranscodeVerdict::Clean => (None, None),
                TranscodeVerdict::Inconclusive => (Some(16_000.0), Some(5.0)), // gentle roll-off
                TranscodeVerdict::Suspect => (Some(20_000.0), Some(40.0)),     // steep, high
                TranscodeVerdict::LikelyLossy => (Some(16_000.0), Some(40.0)), // steep, low
            };
            Analysis { lowpass_hz, lowpass_edge_db_per_khz, ..Default::default() }
        });
        Track {
            id: 0,
            source_path: String::new(),
            format,
            properties: Some(AudioProperties {
                sample_rate_hz: 44_100,
                bit_depth: None,
                channels: 2,
                duration_ms: 0,
                bitrate_kbps: Some(bitrate),
            }),
            tags: Tags::default(),
            analysis,
        }
    }

    #[test]
    fn best_copy_prefers_lossless_then_bitrate_when_unanalyzed() {
        // No analysis on any copy → falls back to the old format+bitrate pick.
        let tracks = [
            copy(Format::Mp3, 320, None),
            copy(Format::Flac, 1000, None),
            copy(Format::Mp3, 128, None),
        ];
        assert_eq!(best_copy_index(&tracks), Some(1)); // the FLAC
    }

    #[test]
    fn best_copy_demotes_detected_transcode_below_clean_lossy() {
        // A FLAC re-encoded from a lossy source (LikelyLossy) must lose to a
        // genuinely clean MP3, even though the FLAC's container is lossless.
        let tracks = [
            copy(Format::Flac, 1000, Some(TranscodeVerdict::LikelyLossy)),
            copy(Format::Mp3, 320, Some(TranscodeVerdict::Clean)),
        ];
        assert_eq!(best_copy_index(&tracks), Some(1)); // the clean MP3
    }

    #[test]
    fn best_copy_treats_clean_and_ltd_as_one_tier_above_lossy() {
        // Clean and Inconclusive ("ltd") both outrank Suspect/LikelyLossy; within
        // the clean tier the lossless container still wins.
        let tracks = [
            copy(Format::Mp3, 320, Some(TranscodeVerdict::Suspect)),
            copy(Format::Mp3, 320, Some(TranscodeVerdict::Inconclusive)),
            copy(Format::Flac, 1000, Some(TranscodeVerdict::Inconclusive)),
        ];
        assert_eq!(best_copy_index(&tracks), Some(2)); // the band-limited FLAC
    }

    #[test]
    fn track_unchanged_tracks_the_stored_size_and_mtime() {
        let cat = Catalog::open(":memory:").unwrap();
        let mut a = scanned("/lib/a.mp3", "A", "House", 1000);
        a.src_size = Some(4242);
        a.src_mtime = Some(1_700_000_000);
        cat.upsert_scanned(&a).unwrap();

        // Exact signature → unchanged (a rescan would skip it).
        assert!(cat.track_unchanged("/lib/a.mp3", 4242, 1_700_000_000).unwrap());
        // A different size or mtime → changed (rescan).
        assert!(!cat.track_unchanged("/lib/a.mp3", 4243, 1_700_000_000).unwrap());
        assert!(!cat.track_unchanged("/lib/a.mp3", 4242, 1_700_000_001).unwrap());
        // Unknown path → not unchanged.
        assert!(!cat.track_unchanged("/lib/missing.mp3", 4242, 1_700_000_000).unwrap());

        // A row whose signature was never recorded (NULL) reads as changed, so it
        // gets rescanned once to backfill the signature.
        let b = scanned("/lib/b.mp3", "B", "Techno", 1000); // src_size/mtime = None
        cat.upsert_scanned(&b).unwrap();
        assert!(!cat.track_unchanged("/lib/b.mp3", 1, 1).unwrap());
    }

    #[test]
    fn moved_file_rematches_by_fingerprint_keeping_id_and_playlists() {
        let cat = Catalog::open(":memory:").unwrap();
        // Path under a directory that does not exist on disk, so the original
        // counts as "missing" once we rescan it at a new location.
        let mut a = scanned("/no/such/old/a.mp3", "A", "House", 1000);
        a.fingerprint = Some("fp-a".into());
        let (id, inserted) = cat.upsert_scanned(&a).unwrap();
        assert!(inserted);
        let pl = cat.create_playlist("set", None, false).unwrap();
        cat.add_tracks(pl, &[id]).unwrap();

        // Same audio, new path; old path is gone → detected as a move.
        let mut moved = scanned("/no/such/new/a.mp3", "A", "House", 1000);
        moved.fingerprint = Some("fp-a".into());
        let (id2, inserted2) = cat.upsert_scanned(&moved).unwrap();
        assert_eq!(id, id2, "same row id");
        assert!(!inserted2, "treated as a move, not a new insert");
        assert_eq!(cat.count().unwrap(), 1, "no duplicate row");
        assert_eq!(cat.get_track(id).unwrap().source_path, "/no/such/new/a.mp3");
        assert_eq!(
            cat.get_playlist(pl).unwrap().track_ids,
            vec![id],
            "playlist link survived the move"
        );
    }

    #[test]
    fn duplicate_copy_is_not_a_move_while_original_exists() {
        let cat = Catalog::open(":memory:").unwrap();
        // A real file so its recorded path genuinely exists on disk.
        let dir = std::env::temp_dir().join(format!("ordnung-dup-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let orig = dir.join("a.mp3");
        std::fs::write(&orig, b"x").unwrap();
        let mut a = scanned(orig.to_str().unwrap(), "A", "House", 1000);
        a.fingerprint = Some("fp".into());
        let (id, _) = cat.upsert_scanned(&a).unwrap();

        // Second path, same fingerprint, original still present → a copy. Insert.
        let mut b = scanned("/no/such/copy/a.mp3", "A", "House", 1000);
        b.fingerprint = Some("fp".into());
        let (id2, inserted2) = cat.upsert_scanned(&b).unwrap();
        assert_ne!(id, id2);
        assert!(inserted2);
        assert_eq!(cat.count().unwrap(), 2, "copy inserted, original untouched");
        std::fs::remove_dir_all(&dir).ok();
    }

    /// Make a real temp dir for duplicate tests — the files must actually exist
    /// on disk so two same-fingerprint imports aren't mistaken for a moved file.
    fn dupe_dir(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("ordnung-dupe-{tag}-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn touch(dir: &std::path::Path, name: &str) -> String {
        let p = dir.join(name);
        std::fs::write(&p, b"x").unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn find_duplicates_separates_identical_audio_from_same_track_variants() {
        let cat = Catalog::open(":memory:").unwrap();
        let dir = dupe_dir("variants");

        // Two byte-identical imports (shared fingerprint), both present on disk.
        let mut a1 = scanned(&touch(&dir, "a1.mp3"), "Artist", "House", 1000);
        a1.tags.title = Some("Song".into());
        a1.fingerprint = Some("fp1".into());
        let mut a2 = scanned(&touch(&dir, "a2.mp3"), "Artist", "House", 1000);
        a2.tags.title = Some("Song".into());
        a2.fingerprint = Some("fp1".into());
        // A lossless re-encode of the SAME song — same artist/title, different bytes.
        let mut a3 = scanned(&touch(&dir, "a3.flac"), "Artist", "House", 1000);
        a3.tags.title = Some("Song".into());
        a3.format = Format::Flac;
        a3.fingerprint = Some("fp2".into());
        // An unrelated track.
        let mut b = scanned(&touch(&dir, "b.mp3"), "Other", "Techno", 2000);
        b.tags.title = Some("Different".into());
        b.fingerprint = Some("fp9".into());
        for t in [&a1, &a2, &a3, &b] {
            cat.upsert_scanned(t).unwrap();
        }

        let groups = cat.find_duplicates().unwrap();
        let identical: Vec<_> = groups.iter().filter(|g| g.kind == DuplicateKind::Identical).collect();
        let same: Vec<_> = groups.iter().filter(|g| g.kind == DuplicateKind::SameTrack).collect();

        assert_eq!(identical.len(), 1, "the two fp1 files are one identical group");
        assert_eq!(identical[0].tracks.len(), 2);
        assert_eq!(same.len(), 1, "all three 'Artist - Song' files are a same-track group");
        assert_eq!(same[0].tracks.len(), 3);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn find_duplicates_does_not_double_report_a_pure_identical_pair() {
        let cat = Catalog::open(":memory:").unwrap();
        let dir = dupe_dir("pure");
        // Identical audio AND identical artist/title, no format diversity → it
        // should surface once (Identical), not again as SameTrack.
        let mut a1 = scanned(&touch(&dir, "a1.mp3"), "Artist", "House", 1000);
        a1.tags.title = Some("Song".into());
        a1.fingerprint = Some("fp".into());
        let mut a2 = scanned(&touch(&dir, "a2.mp3"), "Artist", "House", 1000);
        a2.tags.title = Some("Song".into());
        a2.fingerprint = Some("fp".into());
        cat.upsert_scanned(&a1).unwrap();
        cat.upsert_scanned(&a2).unwrap();

        let groups = cat.find_duplicates().unwrap();
        assert_eq!(groups.len(), 1, "reported once");
        assert_eq!(groups[0].kind, DuplicateKind::Identical);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ignore_duplicate_group_hides_it_and_unignore_restores_it() {
        let cat = Catalog::open(":memory:").unwrap();
        let dir = dupe_dir("ignore");
        // Two distinct songs that collide only on artist + title ("Untitled") —
        // a false positive the user wants to dismiss.
        let mut a = scanned(&touch(&dir, "a.mp3"), "Artist", "House", 1000);
        a.tags.title = Some("Untitled".into());
        a.fingerprint = Some("fpA".into());
        let mut b = scanned(&touch(&dir, "b.mp3"), "Artist", "House", 1000);
        b.tags.title = Some("Untitled".into());
        b.fingerprint = Some("fpB".into());
        cat.upsert_scanned(&a).unwrap();
        cat.upsert_scanned(&b).unwrap();

        let groups = cat.find_duplicates().unwrap();
        assert_eq!(groups.len(), 1, "the two 'Untitled' files report as one group");
        let key = groups[0].key.clone();

        cat.ignore_duplicate_group(&key).unwrap();
        assert!(cat.find_duplicates().unwrap().is_empty(), "dismissed group is hidden");
        // Idempotent.
        cat.ignore_duplicate_group(&key).unwrap();
        assert!(cat.find_duplicates().unwrap().is_empty());

        cat.unignore_duplicate_group(&key).unwrap();
        assert_eq!(cat.find_duplicates().unwrap().len(), 1, "un-dismiss restores it");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_tracks_lists_only_absent_files() {
        let cat = Catalog::open(":memory:").unwrap();
        let dir = std::env::temp_dir().join(format!("ordnung-missing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let present = dir.join("here.mp3");
        std::fs::write(&present, b"x").unwrap();

        let (pid, _) = cat
            .upsert_scanned(&scanned(present.to_str().unwrap(), "P", "House", 1000))
            .unwrap();
        let (mid, _) = cat
            .upsert_scanned(&scanned("/no/such/gone.mp3", "G", "House", 1000))
            .unwrap();

        let missing: Vec<Id> = cat.missing_tracks().unwrap().into_iter().map(|t| t.id).collect();
        assert!(missing.contains(&mid), "absent file reported");
        assert!(!missing.contains(&pid), "present file not reported");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn relink_prefix_repoints_a_moved_folder_at_boundaries() {
        let cat = Catalog::open(":memory:").unwrap();
        let (a, _) = cat.upsert_scanned(&scanned("/Music/Old/x.mp3", "A", "House", 1000)).unwrap();
        let (b, _) = cat.upsert_scanned(&scanned("/Music/Old/sub/y.mp3", "B", "House", 1000)).unwrap();
        // Sibling whose name merely starts with "Old" — must NOT be touched.
        let (c, _) = cat.upsert_scanned(&scanned("/Music/OldStuff/z.mp3", "C", "House", 1000)).unwrap();

        let report = cat.relink_prefix("/Music/Old", "/Library/New", false).unwrap();
        assert_eq!(report.moved, 2);
        assert_eq!(report.skipped, 0);
        assert_eq!(cat.get_track(a).unwrap().source_path, "/Library/New/x.mp3");
        assert_eq!(cat.get_track(b).unwrap().source_path, "/Library/New/sub/y.mp3");
        assert_eq!(
            cat.get_track(c).unwrap().source_path, "/Music/OldStuff/z.mp3",
            "sibling prefix left alone"
        );
    }

    #[test]
    fn relink_prefix_dry_run_previews_without_writing() {
        let cat = Catalog::open(":memory:").unwrap();
        let (a, _) = cat.upsert_scanned(&scanned("/Music/Old/x.mp3", "A", "House", 1000)).unwrap();
        let report = cat.relink_prefix("/Music/Old", "/New", true).unwrap();
        assert_eq!(report.moved, 1);
        assert_eq!(
            report.changes,
            vec![("/Music/Old/x.mp3".to_string(), "/New/x.mp3".to_string())]
        );
        assert_eq!(
            cat.get_track(a).unwrap().source_path, "/Music/Old/x.mp3",
            "dry run changed nothing"
        );
    }

    #[test]
    fn relink_prefix_skips_path_collisions() {
        let cat = Catalog::open(":memory:").unwrap();
        // Repointing /A/x.mp3 → /B/x.mp3 would collide with the existing /B/x.mp3.
        let (_, _) = cat.upsert_scanned(&scanned("/A/x.mp3", "A", "House", 1000)).unwrap();
        let (keep, _) = cat.upsert_scanned(&scanned("/B/x.mp3", "B", "House", 1000)).unwrap();
        let report = cat.relink_prefix("/A", "/B", false).unwrap();
        assert_eq!(report.moved, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(cat.get_track(keep).unwrap().source_path, "/B/x.mp3", "collision target intact");
    }

    #[test]
    fn rescan_preserves_user_edits_but_refreshes_properties() {
        let cat = Catalog::open(":memory:").unwrap();

        // First scan.
        let (id, inserted) = cat.upsert_scanned(&scanned("/a.mp3", "FromFile", "House", 1000)).unwrap();
        assert!(inserted);

        // User edits genre in the catalog (marks user_edited).
        let mut tags = cat.get_track(id).unwrap().tags;
        tags.genre = Some("Minimal".into());
        cat.update_tags(id, &tags).unwrap();

        // Rescan with different file tags AND a different duration.
        let (id2, inserted2) = cat
            .upsert_scanned(&scanned("/a.mp3", "ChangedInFile", "Techno", 2000))
            .unwrap();
        assert_eq!(id, id2);
        assert!(!inserted2);

        let t = cat.get_track(id).unwrap();
        assert_eq!(t.tags.genre.as_deref(), Some("Minimal"), "user edit preserved");
        assert_eq!(t.tags.artist.as_deref(), Some("FromFile"), "user-edited row keeps catalog tags");
        assert_eq!(t.properties.unwrap().duration_ms, 2000, "properties always refresh");
    }

    #[test]
    fn tracks_missing_metadata_finds_gaps_even_with_cover() {
        let cat = Catalog::open(":memory:").unwrap();
        // Missing album, label, catalog #, year, etc. — qualifies.
        let (gappy, _) = cat
            .upsert_scanned(&scanned("/gappy.mp3", "DJ Sprinkles", "House", 1000))
            .unwrap();
        // A track with all fillable album-level fields populated — excluded.
        let mut full = scanned("/full.mp3", "Ben Kaczor", "Techno", 1000);
        full.tags.album = Some("Sun Chapter One".into());
        full.tags.label = Some("Label".into());
        full.tags.catalog_number = Some("CAT 1".into());
        full.tags.release_country = Some("DE".into());
        full.tags.release_date = Some("2020-01-01".into());
        full.tags.year = Some(2020);
        let (complete, _) = cat.upsert_scanned(&full).unwrap();

        // A song with core data (album/genre/year) but blank niche fields
        // (label, catalog #, country, date) is considered populated and must
        // NOT re-appear — those fields are rarely fillable and previously kept
        // complete-looking songs in the queue with nothing to add.
        let mut core_only = scanned("/core.mp3", "Roman Flügel", "House", 1000);
        core_only.tags.album = Some("Eat Your Heart Out".into());
        core_only.tags.year = Some(2004);
        let (populated, _) = cat.upsert_scanned(&core_only).unwrap();

        // Having a cover must NOT exclude a metadata-gap track (unlike the
        // artwork query) — this is the bug the second button fixes.
        cat.set_external_artwork(gappy, "discogs", None, None, Some(&[1, 2, 3]), None)
            .unwrap();

        let ids: Vec<Id> = cat
            .tracks_missing_metadata()
            .unwrap()
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert!(ids.contains(&gappy), "track missing album/year is included");
        assert!(!ids.contains(&complete), "fully-tagged track is excluded");
        assert!(
            !ids.contains(&populated),
            "song with core fields but blank niche fields is left alone"
        );
    }

    #[test]
    fn metadata_fetch_decision_is_made_once_at_add_time() {
        let cat = Catalog::open(":memory:").unwrap();

        // Complete at add (album + genre + year) → marked done at insert, so
        // it's never offered by the picker.
        let mut whole = scanned("/whole.mp3", "Artist", "Techno", 1000);
        whole.tags.album = Some("Album".into());
        whole.tags.year = Some(2020);
        let (complete, _) = cat.upsert_scanned(&whole).unwrap();

        // Incomplete at add (no album, no year) → stays pending until fetched.
        let (gappy, _) = cat
            .upsert_scanned(&scanned("/gappy.mp3", "Artist", "House", 1000))
            .unwrap();

        let pending = |c: &Catalog| -> Vec<Id> {
            c.tracks_missing_metadata().unwrap().into_iter().map(|m| m.id).collect()
        };
        assert!(!pending(&cat).contains(&complete), "complete-at-add track is never queued");
        assert!(pending(&cat).contains(&gappy), "incomplete-at-add track is queued");

        // Clearing a field after the fact must NOT re-queue the track: the
        // missing-attributes check ran once, at add time. (The old query, which
        // re-derived completeness every run, would have pulled it back in.)
        let mut t = cat.get_track(complete).unwrap();
        t.tags.album = None;
        cat.update_tags(complete, &t.tags).unwrap();
        assert!(
            !pending(&cat).contains(&complete),
            "a later-cleared field does not re-queue a track decided complete at add"
        );

        // Any fetch outcome (here: a release applied) marks the pending track
        // done — even though it's still missing year, it won't be offered again.
        cat.mark_metadata_fetched(gappy).unwrap();
        assert!(pending(&cat).is_empty(), "fetched track is no longer pending");
    }

    #[test]
    fn clear_tracks_empties_catalog_and_cascades() {
        let cat = Catalog::open(":memory:").unwrap();
        let (id1, _) = cat.upsert_scanned(&scanned("/a.mp3", "A1", "House", 1000)).unwrap();
        let (id2, _) = cat.upsert_scanned(&scanned("/b.mp3", "A2", "Techno", 1000)).unwrap();

        // Children that should cascade away with their tracks.
        cat.set_external_artwork(id1, "discogs", None, None, Some(&[1, 2, 3]), None)
            .unwrap();
        let pl = cat.create_playlist("set", None, false).unwrap();
        cat.add_tracks(pl, &[id1, id2]).unwrap();

        let removed = cat.clear_tracks().unwrap();
        assert_eq!(removed, 2, "both tracks removed");
        assert_eq!(cat.count().unwrap(), 0, "catalog empty");
        assert!(cat.get_external_artwork(id1).unwrap().is_none(), "artwork cascaded");
        // Playlist row survives but holds no tracks.
        assert!(cat.get_playlist(pl).unwrap().track_ids.is_empty(), "playlist emptied");
    }

    #[test]
    fn edited_tracks_are_tracked_then_cleared() {
        let cat = Catalog::open(":memory:").unwrap();
        let (id, _) = cat.upsert_scanned(&scanned("/a.mp3", "A", "House", 1000)).unwrap();
        let (other, _) = cat.upsert_scanned(&scanned("/b.mp3", "B", "Techno", 1000)).unwrap();

        // Fresh scans aren't "edited".
        assert_eq!(cat.count_edited().unwrap(), 0);

        // Editing one track's tags flags it.
        let mut tags = cat.get_track(id).unwrap().tags;
        tags.album = Some("New Album".into());
        cat.update_tags(id, &tags).unwrap();
        assert_eq!(cat.count_edited().unwrap(), 1);
        let edited = cat.list_edited_tracks().unwrap();
        assert_eq!(edited.len(), 1);
        assert_eq!(edited[0].id, id);
        assert_ne!(edited[0].id, other);

        // Clearing the flag (after a successful file write) syncs it back.
        cat.clear_user_edited(id).unwrap();
        assert_eq!(cat.count_edited().unwrap(), 0);
        assert!(cat.list_edited_tracks().unwrap().is_empty());
    }

    #[test]
    fn embeddable_artwork_flags_track_edited() {
        let cat = Catalog::open(":memory:").unwrap();
        let (id, _) = cat.upsert_scanned(&scanned("/a.mp3", "A", "House", 1000)).unwrap();
        let (other, _) = cat.upsert_scanned(&scanned("/b.mp3", "B", "Techno", 1000)).unwrap();
        assert_eq!(cat.count_edited().unwrap(), 0);

        // A thumbnail-only row (no full_bytes) carries nothing to embed → not dirty.
        cat.set_external_artwork(other, "discogs", None, None, Some(&[1, 2, 3]), None)
            .unwrap();
        assert_eq!(cat.count_edited().unwrap(), 0, "thumbnail-only art is not a pending write");

        // Full-res (embeddable) art flags the track so the bulk-write button picks it up.
        cat.set_external_artwork(id, "discogs", None, None, Some(&[1, 2, 3]), Some(&[4, 5, 6]))
            .unwrap();
        assert_eq!(cat.count_edited().unwrap(), 1);
        assert_eq!(cat.list_edited_tracks().unwrap()[0].id, id);
    }

    #[test]
    fn album_siblings_group_by_album_and_artist() {
        let cat = Catalog::open(":memory:").unwrap();
        // Helper: scan a track on a given album with an album artist and an
        // optional cover flag.
        let scan = |path: &str, album: &str, album_artist: &str, has_cover: bool| {
            let mut s = scanned(path, "Someone", "House", 1000);
            s.tags.album = Some(album.into());
            s.tags.album_artist = Some(album_artist.into());
            s.tags.has_cover = has_cover;
            cat.upsert_scanned(&s).unwrap().0
        };
        let a = scan("/a.mp3", "X", "AA", true); // has its own cover
        let b = scan("/b.mp3", "X", "AA", false); // cover-less mate
        let c = scan("/c.mp3", "X", "AA", true); // has its own cover
        let _d = scan("/d.mp3", "Y", "AA", false); // different album
        let _e = scan("/e.mp3", "X", "BB", false); // same album title, other artist

        // All mates on album X / AA, excluding A itself (D and E differ).
        assert_eq!(cat.album_siblings(a).unwrap(), vec![b, c]);
        // Only the cover-less mate is "missing art".
        assert_eq!(cat.album_siblings_missing_art(a).unwrap(), vec![b]);

        // A track with no album has no mates.
        let lone = scanned("/lone.mp3", "Z", "Ambient", 1000);
        let lone = cat.upsert_scanned(&lone).unwrap().0;
        assert!(cat.album_siblings(lone).unwrap().is_empty());
    }

    #[test]
    fn prefer_external_artwork_round_trips() {
        let cat = Catalog::open(":memory:").unwrap();
        let (id, _) = cat.upsert_scanned(&scanned("/a.mp3", "A", "House", 1000)).unwrap();

        // No row yet → not preferred.
        assert!(!cat.prefers_external_artwork(id).unwrap());

        // A fetched cover defaults to *not* superseding the embedded art.
        cat.set_external_artwork(id, "discogs", None, None, Some(&[1, 2, 3]), Some(&[4, 5, 6]))
            .unwrap();
        assert!(!cat.prefers_external_artwork(id).unwrap());

        // Flagging it preferred (the overwrite path) flips it; clearing flips back.
        cat.set_prefer_external_artwork(id, true).unwrap();
        assert!(cat.prefers_external_artwork(id).unwrap());
        cat.set_prefer_external_artwork(id, false).unwrap();
        assert!(!cat.prefers_external_artwork(id).unwrap());
    }

    #[test]
    fn track_has_art_sees_embedded_and_external() {
        let cat = Catalog::open(":memory:").unwrap();

        // No embedded cover, no fetched art → no art.
        let (bare, _) = cat.upsert_scanned(&scanned("/bare.mp3", "A", "House", 1000)).unwrap();
        assert!(!cat.track_has_art(bare).unwrap());

        // Embedded cover counts.
        let mut s = scanned("/embed.mp3", "B", "House", 1000);
        s.tags.has_cover = true;
        let (embed, _) = cat.upsert_scanned(&s).unwrap();
        assert!(cat.track_has_art(embed).unwrap());

        // A fetched external cover counts even with no embedded art.
        cat.set_external_artwork(bare, "discogs", None, None, Some(&[1, 2, 3]), Some(&[4, 5, 6]))
            .unwrap();
        assert!(cat.track_has_art(bare).unwrap());
    }

    #[test]
    fn rescan_refreshes_tags_when_not_user_edited() {
        let cat = Catalog::open(":memory:").unwrap();
        let (id, _) = cat.upsert_scanned(&scanned("/b.mp3", "A1", "House", 1000)).unwrap();
        cat.upsert_scanned(&scanned("/b.mp3", "A2", "Techno", 1000)).unwrap();
        let t = cat.get_track(id).unwrap();
        assert_eq!(t.tags.genre.as_deref(), Some("Techno"), "non-edited tags refresh from file");
        assert_eq!(t.tags.artist.as_deref(), Some("A2"));
    }

    /// A unique temp DB path for reload tests (no tempfile dependency).
    fn temp_db_path(tag: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        std::env::temp_dir().join(format!(
            "ordnung-test-{tag}-{}-{n}.db",
            std::process::id()
        ))
    }

    fn three_tracks(cat: &Catalog) -> (Id, Id, Id) {
        let (a, _) = cat.upsert_scanned(&scanned("/1.mp3", "A", "House", 1000)).unwrap();
        let (b, _) = cat.upsert_scanned(&scanned("/2.mp3", "B", "House", 1000)).unwrap();
        let (c, _) = cat.upsert_scanned(&scanned("/3.mp3", "C", "House", 1000)).unwrap();
        (a, b, c)
    }

    #[test]
    fn delete_tracks_cascades_to_playlists_and_analysis() {
        let cat = Catalog::open(":memory:").unwrap();
        let (t1, t2, t3) = three_tracks(&cat);
        let pl = cat.create_playlist("Set", None, false).unwrap();
        cat.add_tracks(pl, &[t1, t2, t3]).unwrap();

        // Deleting two tracks removes them from the catalog and the playlist;
        // the survivor stays. Unknown ids are skipped, not counted.
        assert_eq!(cat.delete_tracks(&[t1, t2, 9999]).unwrap(), 2);
        assert_eq!(cat.count().unwrap(), 1);
        assert_eq!(cat.get_playlist(pl).unwrap().track_ids, vec![t3]);
        assert!(cat.get_track(t1).is_err());
        assert!(cat.get_track(t3).is_ok());
    }

    #[test]
    fn replace_tracks_hands_playlist_slots_to_the_keeper() {
        let cat = Catalog::open(":memory:").unwrap();
        let (mp3, t2, t3) = three_tracks(&cat);
        // The kept copy (e.g. an AIFF of the same song).
        let (aiff, _) = cat.upsert_scanned(&scanned("/keep.aiff", "A", "House", 1000)).unwrap();

        // Playlist A holds the mp3 but not the aiff.
        let a = cat.create_playlist("A", None, false).unwrap();
        cat.add_tracks(a, &[t2, mp3, t3]).unwrap();
        // Playlist B already holds the aiff *and* the mp3.
        let b = cat.create_playlist("B", None, false).unwrap();
        cat.add_tracks(b, &[mp3, aiff, t2]).unwrap();

        assert_eq!(cat.replace_tracks(&[(aiff, mp3)]).unwrap(), 1);

        // A: the aiff inherits the mp3's exact slot (position preserved).
        assert_eq!(cat.get_playlist(a).unwrap().track_ids, vec![t2, aiff, t3]);
        // B: the aiff was already present, so the mp3 is just dropped and the
        // aiff keeps its original position — no duplicate entry.
        assert_eq!(cat.get_playlist(b).unwrap().track_ids, vec![aiff, t2]);
        // The mp3 is gone from the catalog; the aiff survives.
        assert!(cat.get_track(mp3).is_err());
        assert!(cat.get_track(aiff).is_ok());
    }

    #[test]
    fn playlists_nest_and_hold_ordered_tracks() {
        let cat = Catalog::open(":memory:").unwrap();
        let (t1, t2, t3) = three_tracks(&cat);

        let folder = cat.create_playlist("Sets", None, true).unwrap();
        let pl = cat.create_playlist("Warmup", Some(folder), false).unwrap();

        assert_eq!(cat.add_tracks(pl, &[t1, t2, t3]).unwrap(), 3);
        // Duplicates are skipped.
        assert_eq!(cat.add_tracks(pl, &[t2]).unwrap(), 0);
        assert_eq!(cat.get_playlist(pl).unwrap().track_ids, vec![t1, t2, t3]);

        // Reorder to a permutation.
        cat.reorder_tracks(pl, &[t3, t1, t2]).unwrap();
        assert_eq!(cat.get_playlist(pl).unwrap().track_ids, vec![t3, t1, t2]);

        // Remove one.
        assert_eq!(cat.remove_tracks(pl, &[t1]).unwrap(), 1);
        assert_eq!(cat.get_playlist(pl).unwrap().track_ids, vec![t3, t2]);

        // The playlist is nested under the folder.
        assert_eq!(cat.get_playlist(pl).unwrap().parent, Some(folder));
    }

    #[test]
    fn list_playlist_tracks_respects_order_and_filter() {
        let cat = Catalog::open(":memory:").unwrap();
        let (t1, t2, t3) = three_tracks(&cat); // artists A, B, C
        let folder = cat.create_playlist("F", None, true).unwrap();
        let pl = cat.create_playlist("P", None, false).unwrap();
        cat.add_tracks(pl, &[t1, t2, t3]).unwrap();
        cat.reorder_tracks(pl, &[t3, t1, t2]).unwrap();

        // Returned in playlist position order, not catalog order.
        let ids: Vec<Id> = cat
            .list_playlist_tracks(pl, None)
            .unwrap()
            .iter()
            .map(|t| t.id)
            .collect();
        assert_eq!(ids, vec![t3, t1, t2]);

        // The same substring filter as `list_tracks` applies within the playlist.
        let filtered = cat.list_playlist_tracks(pl, Some("b")).unwrap();
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, t2);

        // A folder holds no tracks.
        assert!(cat.list_playlist_tracks(folder, None).unwrap().is_empty());
    }

    #[test]
    fn search_matches_terms_across_fields() {
        let cat = Catalog::open(":memory:").unwrap();
        let mut s = scanned("/apple.mp3", "DJ Pear", "House", 1000);
        s.tags.title = Some("Apple".into());
        let (apple, _) = cat.upsert_scanned(&s).unwrap();
        // A decoy that shares the title word but not the artist.
        let mut other = scanned("/apple2.mp3", "Someone Else", "House", 1000);
        other.tags.title = Some("Apple".into());
        cat.upsert_scanned(&other).unwrap();

        // Each whitespace term may match a different field, in any order: "apple"
        // is the title, "dj"/"pear" the artist. The old single-substring filter
        // failed this because no one column held the whole phrase.
        for q in ["apple dj pear", "pear apple", "DJ APPLE"] {
            let hits = cat.list_tracks(Some(q), 0).unwrap();
            assert_eq!(hits.len(), 1, "query {q:?} should match only the Pear track");
            assert_eq!(hits[0].id, apple, "query {q:?}");
        }

        // A term that matches nothing rules the row out (terms are AND-ed).
        assert!(cat.list_tracks(Some("apple banana"), 0).unwrap().is_empty());
        // A blank / whitespace-only query still returns everything.
        assert_eq!(cat.list_tracks(Some("   "), 0).unwrap().len(), 2);
    }

    #[test]
    fn folder_and_track_rules_are_enforced() {
        let cat = Catalog::open(":memory:").unwrap();
        let (t1, ..) = three_tracks(&cat);
        let folder = cat.create_playlist("F", None, true).unwrap();
        let pl = cat.create_playlist("P", None, false).unwrap();

        // Can't put tracks in a folder.
        assert!(cat.add_tracks(folder, &[t1]).is_err());
        // Can't nest under a non-folder.
        assert!(cat.create_playlist("X", Some(pl), false).is_err());
        // Can't add an unknown track.
        assert!(cat.add_tracks(pl, &[9999]).is_err());
        // Reorder must be a permutation of current members.
        cat.add_tracks(pl, &[t1]).unwrap();
        assert!(cat.reorder_tracks(pl, &[t1, 9999]).is_err());
        // Can't move a folder into its own descendant.
        let sub = cat.create_playlist("Sub", Some(folder), true).unwrap();
        assert!(cat.move_playlist(folder, Some(sub)).is_err());
    }

    #[test]
    fn playlists_survive_reload() {
        let path = temp_db_path("playlists");
        let (folder, pl, t1, t2);
        {
            let cat = Catalog::open(&path).unwrap();
            (t1, t2, _) = three_tracks(&cat);
            folder = cat.create_playlist("Crate", None, true).unwrap();
            pl = cat.create_playlist("Night", Some(folder), false).unwrap();
            cat.add_tracks(pl, &[t2, t1]).unwrap();
        } // drop closes the connection

        let cat = Catalog::open(&path).unwrap();
        let all = cat.list_playlists().unwrap();
        assert_eq!(all.len(), 2);
        let reloaded = cat.get_playlist(pl).unwrap();
        assert_eq!(reloaded.name, "Night");
        assert_eq!(reloaded.parent, Some(folder));
        assert_eq!(reloaded.track_ids, vec![t2, t1], "order preserved across reload");

        // Deleting the folder cascades to the nested playlist.
        cat.delete_playlist(folder).unwrap();
        assert!(cat.list_playlists().unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    fn vinyl(instance_id: u64, artist: &str, title: &str) -> VinylRecord {
        VinylRecord {
            instance_id,
            release_id: instance_id + 9000,
            title: title.into(),
            artist: artist.into(),
            year: Some(1999),
            label: Some("Plus 8".into()),
            catalog_number: Some("PLUS8 024".into()),
            format: Some("Vinyl, 12\"".into()),
            thumb_url: Some("https://img/thumb.jpg".into()),
            cover_url: Some("https://img/cover.jpg".into()),
            added: Some("2021-03-04".into()),
            has_cover: false,
        }
    }

    #[test]
    fn vinyl_cache_roundtrips_upsert_cover_and_prune() {
        let cat = Catalog::open(":memory:").unwrap();
        cat.upsert_vinyl(&vinyl(1, "Plastikman", "Sheet One")).unwrap();
        cat.upsert_vinyl(&vinyl(2, "Surgeon", "Force + Form")).unwrap();
        assert_eq!(cat.vinyl_count().unwrap(), 2);

        // Ordered by artist, then title.
        let list = cat.list_vinyl().unwrap();
        assert_eq!(list.iter().map(|v| v.instance_id).collect::<Vec<_>>(), vec![1, 2]);
        assert!(!list[0].has_cover);

        // Both records start out needing a cover; storing one flips its flag and
        // drops it from the missing-cover work list.
        assert_eq!(cat.vinyl_missing_covers().unwrap().len(), 2);
        cat.set_vinyl_cover(1, &[1, 2, 3]).unwrap();
        assert_eq!(cat.vinyl_cover(1).unwrap().as_deref(), Some(&[1, 2, 3][..]));
        assert_eq!(cat.vinyl_missing_covers().unwrap().len(), 1);
        assert!(cat.list_vinyl().unwrap()[0].has_cover);

        // A metadata refresh (re-upsert) must not wipe the cached cover.
        let mut updated = vinyl(1, "Plastikman", "Sheet One");
        updated.year = Some(1993);
        cat.upsert_vinyl(&updated).unwrap();
        assert_eq!(cat.vinyl_cover(1).unwrap().as_deref(), Some(&[1, 2, 3][..]));

        // Pruning to the set still in the Discogs collection drops the rest.
        let removed = cat.prune_vinyl_not_in(&[1]).unwrap();
        assert_eq!(removed, 1);
        assert_eq!(cat.vinyl_count().unwrap(), 1);

        // An empty keep-set clears the cache wholesale.
        assert_eq!(cat.prune_vinyl_not_in(&[]).unwrap(), 1);
        assert_eq!(cat.vinyl_count().unwrap(), 0);
    }

    #[test]
    fn release_track_links_maps_numeric_release_ids_only() {
        let cat = Catalog::open(":memory:").unwrap();
        let (a, _) = cat.upsert_scanned(&scanned("/m/a.mp3", "A", "Techno", 1000)).unwrap();
        let (b, _) = cat.upsert_scanned(&scanned("/m/b.mp3", "B", "House", 1000)).unwrap();
        let (c, _) = cat.upsert_scanned(&scanned("/m/c.mp3", "C", "House", 1000)).unwrap();

        // a + b link to Discogs release 555; c has a non-numeric id (skipped).
        cat.set_external_artwork(a, "discogs", Some("555"), None, Some(&[1]), None).unwrap();
        cat.set_external_artwork(b, "discogs", Some("555"), None, Some(&[1]), None).unwrap();
        cat.set_external_artwork(c, "discogs", Some("master-1"), None, Some(&[1]), None).unwrap();

        let mut links = cat.release_track_links().unwrap();
        links.sort();
        let mut expected = vec![(555u64, a), (555u64, b)];
        expected.sort();
        assert_eq!(links, expected);
    }

    #[test]
    fn norm_match_collapses_punctuation_and_case() {
        assert_eq!(norm_match("Guardwatcher Pt. 1"), "guardwatcher pt 1");
        assert_eq!(norm_match("guardwatcher pt 1"), "guardwatcher pt 1");
        assert_eq!(norm_match("  Ø  [Phase] "), "ø phase");
        assert_eq!(norm_match("A&B - C"), "a b c");
        // Abbreviation + numeral spellings canonicalize together so a downloaded
        // "Club Styling Vol. 2" links to the release "Club Styling Volume Two".
        assert_eq!(
            norm_match("Club Styling Vol. 2"),
            norm_match("Club Styling Volume Two")
        );
        assert_eq!(norm_match("Greatest Hits Part Three"), "greatest hits pt 3");
    }

    #[test]
    fn vinyl_links_use_metadata_only_as_release_id_fallback() {
        let cat = Catalog::open(":memory:").unwrap();
        // Track A: album matches a record's title, no Discogs id → metadata link.
        let mut a = scanned("/m/a.mp3", "Lakker", "Techno", 1000);
        a.tags.album = Some("Guardwatcher Pt 1".into());
        a.tags.title = Some("Guardwatcher".into());
        let (a, _) = cat.upsert_scanned(&a).unwrap();
        // Track B: title matches the record title and artist overlaps → metadata.
        let mut b = scanned("/m/b.mp3", "Lakker", "Techno", 1000);
        b.tags.title = Some("Guardwatcher Pt. 1".into());
        let (b, _) = cat.upsert_scanned(&b).unwrap();
        // Track C: unrelated.
        let mut c = scanned("/m/c.mp3", "Surgeon", "Techno", 1000);
        c.tags.album = Some("Force + Form".into());
        let (_c, _) = cat.upsert_scanned(&c).unwrap();
        // Track D: exact release-id link to record 7000.
        let (d, _) = cat.upsert_scanned(&scanned("/m/d.mp3", "Lakker", "Techno", 1000)).unwrap();
        cat.set_external_artwork(d, "discogs", Some("7000"), None, Some(&[1]), None).unwrap();

        let rec_meta = vinyl(1, "Lakker", "Guardwatcher Pt 1"); // release_id 9001, no id link
        let mut rec_id = vinyl(2, "Lakker", "Some EP"); // matched by exact id only
        rec_id.release_id = 7000;

        let mut links = cat.vinyl_catalog_links(&[rec_meta.clone(), rec_id]).unwrap();
        links.sort();
        let mut expected = vec![
            (rec_meta.release_id, a), // album == title
            (rec_meta.release_id, b), // title == title + artist overlap
            (7000u64, d),             // exact release-id link
        ];
        expected.sort();
        assert_eq!(links, expected);
    }

    fn release_detail(id: &str, title: &str) -> crate::discogs::ReleaseDetail {
        crate::discogs::ReleaseDetail {
            release_id: id.into(),
            title: title.into(),
            year: Some(1995),
            released: Some("1995-09-01".into()),
            country: Some("UK".into()),
            genres: vec!["Electronic".into()],
            styles: vec!["Techno".into()],
            label: Some("Downwards".into()),
            catalog_number: Some("DN-01".into()),
        }
    }

    #[test]
    fn release_cache_round_trips_and_serves_hits_without_fetching() {
        let cat = Catalog::open(":memory:").unwrap();

        // Miss on an empty cache.
        assert!(cat.cached_release("42").unwrap().is_none());

        // First lookup fetches once and caches.
        let calls = std::cell::Cell::new(0);
        let got = cat
            .release_cached_or("42", || {
                calls.set(calls.get() + 1);
                Ok(release_detail("42", "Force + Form"))
            })
            .unwrap();
        assert_eq!(got.title, "Force + Form");
        assert_eq!(calls.get(), 1);

        // Round-trips through SQLite with every field intact.
        let cached = cat.cached_release("42").unwrap().expect("now cached");
        assert_eq!(cached.styles, vec!["Techno".to_string()]);
        assert_eq!(cached.year, Some(1995));
        assert_eq!(cached.catalog_number.as_deref(), Some("DN-01"));

        // Second lookup is served from cache — the fetch closure must not run again.
        let again = cat
            .release_cached_or("42", || {
                calls.set(calls.get() + 1);
                Ok(release_detail("42", "SHOULD NOT BE USED"))
            })
            .unwrap();
        assert_eq!(again.title, "Force + Form");
        assert_eq!(calls.get(), 1, "cache hit must not re-fetch");
    }

    #[test]
    fn release_cache_fetch_error_is_not_cached() {
        let cat = Catalog::open(":memory:").unwrap();
        // A failed fetch propagates and stores nothing, so the next call retries.
        let err = cat.release_cached_or("7", || Err(Error::Network("boom".into())));
        assert!(err.is_err());
        assert!(cat.cached_release("7").unwrap().is_none(), "failures aren't cached");
    }
}
