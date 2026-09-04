//! Device Library Plus — the `exportLibrary.db` half of a modern export.
//!
//! rekordbox 6/7 exports write a second database next to `export.pdb`: a
//! SQLCipher-encrypted SQLite file that newer players (OPUS-QUAD, OMNIS-DUO,
//! XDJ-AZ, CDJ-3000 firmware) read instead of the DeviceSQL one. Crucially,
//! such exports leave `export.pdb`'s playlist-tree and playlist-entry tables
//! **empty** — the playlists exist only here. So a stick exported by
//! rekordbox 7 shows its full library through [`crate::pdb`] but zero
//! playlists, until this module fills them in (see [`crate::pdb::read_stick`]).
//!
//! The encryption key is a static constant rekordbox embeds (obfuscated) in
//! its own binary, identical for every export, and publicly documented by the
//! reverse-engineering community — see
//! <https://gist.github.com/0xdevalias/b803476793b56f7c45e6361799168eb0>.
//! Reading it here only ever decrypts the user's own export, on their own
//! stick, read-only.
//!
//! Same defensive posture as the pdb reader: a corrupt or unreadable database
//! returns an error; a malformed row is skipped, never a panic.

use std::collections::HashMap;
use std::path::Path;

use crate::pdb::{RbPlaylist, ReadError};

/// The static Device Library Plus key (64 ASCII bytes, passed as a SQLCipher
/// passphrase with v4 compatibility). Distinct from the `master.db` key.
pub(crate) const DLP_KEY: &str =
    "r8gddnr4k847830ar6cqzbkk0el6qytmb3trbbx805jm74vez64i5o8fnrqryqls";

/// The playlists of one `exportLibrary.db`, in terms the pdb side can join
/// on: tree nodes plus each playlist's member *file paths* in play order
/// (paths as stored, `/Contents/...`, absolute from the volume root).
#[derive(Debug, Default, Clone)]
pub struct DlpPlaylists {
    /// Every playlist/folder node, sorted by sort order.
    pub playlists: Vec<RbPlaylist>,
    /// Playlist id → member file paths in playlist order.
    pub entries_by_path: HashMap<u32, Vec<String>>,
}

/// Open `db_path` (an `exportLibrary.db`) read-only and pull the playlist
/// tree and memberships out of it.
pub fn read_playlists(db_path: &Path) -> Result<DlpPlaylists, ReadError> {
    let conn = rusqlite::Connection::open_with_flags(
        db_path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .map_err(|e| ReadError::Dlp(e.to_string()))?;
    conn.execute_batch(&format!(
        "PRAGMA key = '{DLP_KEY}'; PRAGMA cipher_compatibility = 4;"
    ))
    .map_err(|e| ReadError::Dlp(e.to_string()))?;

    let mut out = DlpPlaylists::default();

    // The playlist tree. `attribute` follows the desktop database's
    // convention: 0 = playlist, 1 = folder (4, a smart playlist, can't occur
    // on an export — rekordbox freezes them into plain lists).
    let mut stmt = conn
        .prepare(
            "SELECT playlist_id, sequenceNo, name, attribute, playlist_id_parent \
             FROM playlist",
        )
        .map_err(|e| ReadError::Dlp(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(RbPlaylist {
                id: r.get::<_, i64>(0)? as u32,
                sort_order: r.get::<_, i64>(1).unwrap_or(0) as u32,
                name: r.get::<_, String>(2)?,
                is_folder: r.get::<_, i64>(3).unwrap_or(0) == 1,
                parent_id: r.get::<_, i64>(4).unwrap_or(0) as u32,
            })
        })
        .map_err(|e| ReadError::Dlp(e.to_string()))?;
    for row in rows.flatten() {
        out.playlists.push(row);
    }
    out.playlists.sort_by_key(|p| p.sort_order);

    // Memberships, joined straight to each member's file path — the only key
    // both databases share (content ids are not the pdb's track ids).
    let mut stmt = conn
        .prepare(
            "SELECT pc.playlist_id, c.path \
             FROM playlist_content pc \
             JOIN content c ON c.content_id = pc.content_id \
             ORDER BY pc.playlist_id, pc.sequenceNo",
        )
        .map_err(|e| ReadError::Dlp(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((r.get::<_, i64>(0)? as u32, r.get::<_, String>(1)?))
        })
        .map_err(|e| ReadError::Dlp(e.to_string()))?;
    for (playlist, path) in rows.flatten() {
        out.entries_by_path.entry(playlist).or_default().push(path);
    }
    // Empty playlists still get an entry so the tree mirrors the player.
    for p in &out.playlists {
        if !p.is_folder {
            out.entries_by_path.entry(p.id).or_default();
        }
    }

    Ok(out)
}

// ---------------------------------------------------------------------------
// Write side (Phase 5)
// ---------------------------------------------------------------------------

/// Full 22-table Device Library Plus schema, as rekordbox 7.2.2 writes it
/// (column list captured from the EYEBAGS golden reference — see
/// `docs/rekordbox-export-structure.md` §4).
const DLP_SCHEMA: &str = "
CREATE TABLE album(album_id INTEGER PRIMARY KEY, name varchar, artist_id INTEGER,
    image_id INTEGER, isComplation INTEGER, nameForSearch varchar);
CREATE TABLE artist(artist_id INTEGER PRIMARY KEY, name varchar, nameForSearch varchar);
CREATE TABLE category(category_id INTEGER PRIMARY KEY, menuItem_id INTEGER,
    sequenceNo INTEGER, isVisible INTEGER);
CREATE TABLE color(color_id INTEGER PRIMARY KEY, name varchar);
CREATE TABLE content(content_id INTEGER PRIMARY KEY, title varchar, titleForSearch varchar,
    subtitle varchar, bpmx100 INTEGER, length INTEGER, trackNo INTEGER, discNo INTEGER,
    artist_id_artist INTEGER, artist_id_remixer INTEGER, artist_id_originalArtist INTEGER,
    artist_id_composer INTEGER, artist_id_lyricist INTEGER, album_id INTEGER,
    genre_id INTEGER, label_id INTEGER, key_id INTEGER, color_id INTEGER, image_id INTEGER,
    djComment varchar, rating INTEGER, releaseYear INTEGER, releaseDate varchar,
    dateCreated varchar, dateAdded varchar, path varchar, fileName varchar,
    fileSize INTEGER, fileType INTEGER, bitrate INTEGER, bitDepth INTEGER,
    samplingRate INTEGER, isrc varchar, djPlayCount INTEGER, isHotCueAutoLoadOn INTEGER,
    isKuvoDeliverStatusOn INTEGER, kuvoDeliveryComment varchar, masterDbId INTEGER,
    masterContentId INTEGER, analysisDataFilePath varchar, analysedBits INTEGER,
    contentLink INTEGER, hasModified INTEGER, cueUpdateCount INTEGER,
    analysisDataUpdateCount INTEGER, informationUpdateCount INTEGER);
CREATE TABLE cue(cue_id INTEGER PRIMARY KEY, content_id INTEGER, kind INTEGER,
    colorTableIndex INTEGER, cueComment varchar, isActiveLoop INTEGER,
    beatLoopNumerator INTEGER, beatLoopDenominator INTEGER, inUsec INTEGER, outUsec INTEGER,
    in150FramePerSec INTEGER, out150FramePerSec INTEGER, inMpegFrameNumber INTEGER,
    outMpegFrameNumber INTEGER, inMpegAbs INTEGER, outMpegAbs INTEGER,
    inDecodingStartFramePosition INTEGER, outDecodingStartFramePosition INTEGER,
    inFileOffsetInBlock INTEGER, OutFileOffsetInBlock INTEGER,
    inNumberOfSampleInBlock INTEGER, outNumberOfSampleInBlock INTEGER);
CREATE TABLE genre(genre_id INTEGER PRIMARY KEY, name varchar);
CREATE TABLE history(history_id INTEGER PRIMARY KEY, sequenceNo INTEGER, name varchar,
    attribute INTEGER, history_id_parent INTEGER);
CREATE TABLE history_content(history_id INTEGER, content_id INTEGER, sequenceNo INTEGER);
CREATE TABLE hotCueBankList(hotCueBankList_id INTEGER PRIMARY KEY, sequenceNo INTEGER,
    name varchar, image_id INTEGER, attribute INTEGER, hotCueBankList_id_parent INTEGER);
CREATE TABLE hotCueBankList_cue(hotCueBankList_id INTEGER, cue_id INTEGER, sequenceNo INTEGER);
CREATE TABLE image(image_id INTEGER PRIMARY KEY, path varchar);
CREATE TABLE key(key_id INTEGER PRIMARY KEY, name varchar);
CREATE TABLE label(label_id INTEGER PRIMARY KEY, name varchar);
CREATE TABLE menuItem(menuItem_id INTEGER PRIMARY KEY, kind INTEGER, name varchar);
CREATE TABLE myTag(myTag_id INTEGER PRIMARY KEY, sequenceNo INTEGER, name varchar,
    attribute INTEGER, myTag_id_parent INTEGER);
CREATE TABLE myTag_content(myTag_id INTEGER, content_id INTEGER);
CREATE TABLE playlist(playlist_id INTEGER PRIMARY KEY, sequenceNo INTEGER, name varchar,
    image_id INTEGER, attribute INTEGER, playlist_id_parent INTEGER);
CREATE TABLE playlist_content(playlist_id INTEGER, content_id INTEGER, sequenceNo INTEGER);
CREATE TABLE property(deviceName varchar, dbVersion varchar, numberOfContents INTEGER,
    createdDate varchar, backGroundColorType INTEGER, myTagMasterDBID INTEGER);
CREATE TABLE recommendedLike(content_id_1 INTEGER, content_id_2 INTEGER, rating INTEGER,
    createdDate INTEGER);
CREATE TABLE sort(sort_id INTEGER PRIMARY KEY, menuItem_id INTEGER, sequenceNo INTEGER,
    isVisible INTEGER, isSelectedAsSubColumn INTEGER);
";

/// Player browse-menu definitions, verbatim from the golden reference. The
/// `\u{FFFA}`/`\u{FFFB}` wrappers (interlinear annotation anchors) are part of
/// the names as rekordbox stores them.
const MENU_ITEMS: &[(i64, i64, &str)] = &[
    (1, 128, "GENRE"), (2, 129, "ARTIST"), (3, 130, "ALBUM"), (4, 131, "TRACK"),
    (5, 133, "BPM"), (6, 134, "RATING"), (7, 135, "YEAR"), (8, 136, "REMIXER"),
    (9, 137, "LABEL"), (10, 138, "ORIGINAL ARTIST"), (11, 139, "KEY"), (12, 141, "CUE"),
    (13, 142, "COLOR"), (14, 146, "TIME"), (15, 147, "BITRATE"), (16, 148, "FILE NAME"),
    (17, 132, "PLAYLIST"), (18, 152, "HOT CUE BANK"), (19, 149, "HISTORY"),
    (20, 145, "SEARCH"), (21, 150, "COMMENTS"), (22, 140, "DATE ADDED"),
    (23, 151, "DJ PLAY COUNT"), (24, 144, "FOLDER"), (25, 161, "DEFAULT"),
    (26, 162, "ALPHABET"), (27, 170, "MATCHING"),
];

const CATEGORIES: &[(i64, i64, i64, i64)] = &[
    (1, 1, 0, 0), (2, 2, 1, 1), (3, 3, 2, 1), (4, 4, 3, 1), (5, 17, 5, 1),
    (6, 5, 0, 0), (7, 6, 0, 0), (8, 7, 0, 0), (9, 8, 0, 0), (10, 9, 0, 0),
    (11, 10, 0, 0), (12, 11, 4, 1), (15, 13, 0, 0), (17, 24, 9, 1), (18, 20, 7, 1),
    (19, 14, 0, 0), (20, 15, 0, 0), (21, 16, 0, 0), (22, 19, 6, 1), (23, 18, 0, 0),
    (26, 27, 8, 1), (27, 22, 10, 1),
];

const SORTS: &[(i64, i64, i64, i64, i64)] = &[
    (0, 25, 1, 1, 0), (1, 26, 2, 1, 0), (2, 2, 3, 1, 0), (3, 3, 4, 1, 0),
    (4, 5, 5, 1, 0), (5, 6, 6, 1, 0), (6, 1, 0, 0, 0), (7, 21, 0, 0, 0),
    (8, 14, 0, 0, 0), (9, 8, 0, 0, 0), (10, 9, 0, 0, 0), (11, 10, 0, 0, 0),
    (12, 11, 7, 1, 0), (13, 15, 0, 0, 0), (15, 13, 0, 0, 0), (16, 23, 0, 0, 0),
    (17, 22, 0, 0, 0),
];

const COLOR_NAMES: [&str; 8] = [
    "Pink", "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple",
];

/// Fixed masterDbId stamped on every row (rekordbox uses its install's random
/// id; any consistent nonzero value serves).
const MASTER_DB_ID: i64 = 715_983_263;

/// Write a complete `exportLibrary.db` beside an `export.pdb`, mirroring the
/// same resolved library. Overwrites any existing database (and removes stale
/// WAL sidecars). Written fully checkpointed in rollback-journal mode so
/// read-only players never need to recover a WAL.
///
/// SQLite cannot write in place on macOS's msdos (FAT32) driver — the second
/// transaction dies with "attempt to write a readonly database", leaving a
/// one-table stub that players reject as a corrupted device library. So the
/// database is built on local disk and the finished bytes are copied over.
pub(crate) fn write_library(
    db_path: &Path,
    t: &crate::pdbw::PdbTables,
    device_name: &str,
) -> Result<(), ReadError> {
    let err_io = |e: std::io::Error| ReadError::Dlp(e.to_string());
    let tmp = scratch_db_path("ordnung-dlp-write");
    let _ = std::fs::remove_file(&tmp);
    build_library(&tmp, t, device_name)?;
    for suffix in ["", "-wal", "-shm"] {
        let p = db_path.with_file_name(format!("exportLibrary.db{suffix}"));
        let _ = std::fs::remove_file(p);
    }
    std::fs::copy(&tmp, db_path).map_err(err_io)?;
    let _ = std::fs::remove_file(&tmp);
    Ok(())
}

/// Create and populate a fresh Device Library Plus database at `db_path`
/// (which must be on a filesystem SQLite can journal on — i.e. local disk).
fn build_library(
    db_path: &Path,
    t: &crate::pdbw::PdbTables,
    device_name: &str,
) -> Result<(), ReadError> {
    let conn =
        rusqlite::Connection::open(db_path).map_err(|e| ReadError::Dlp(e.to_string()))?;
    conn.execute_batch(&format!(
        "PRAGMA key = '{DLP_KEY}'; PRAGMA cipher_compatibility = 4;"
    ))
    .map_err(|e| ReadError::Dlp(e.to_string()))?;
    conn.execute_batch(DLP_SCHEMA)
        .map_err(|e| ReadError::Dlp(e.to_string()))?;

    let err = |e: rusqlite::Error| ReadError::Dlp(e.to_string());
    conn.execute_batch("BEGIN").map_err(err)?;
    {
        for (id, name) in &t.artists {
            conn.execute(
                "INSERT INTO artist VALUES (?1, ?2, ?3)",
                rusqlite::params![*id as i64, name, name.to_lowercase()],
            )
            .map_err(err)?;
        }
        for (id, name) in &t.albums {
            conn.execute(
                "INSERT INTO album VALUES (?1, ?2, NULL, NULL, 0, ?3)",
                rusqlite::params![*id as i64, name, name.to_lowercase()],
            )
            .map_err(err)?;
        }
        for (id, name) in &t.genres {
            conn.execute(
                "INSERT INTO genre VALUES (?1, ?2)",
                rusqlite::params![*id as i64, name],
            )
            .map_err(err)?;
        }
        for (id, name) in &t.labels {
            conn.execute(
                "INSERT INTO label VALUES (?1, ?2)",
                rusqlite::params![*id as i64, name],
            )
            .map_err(err)?;
        }
        for (id, name) in &t.keys {
            conn.execute(
                "INSERT INTO key VALUES (?1, ?2)",
                rusqlite::params![*id as i64, name],
            )
            .map_err(err)?;
        }
        for (i, name) in COLOR_NAMES.iter().enumerate() {
            conn.execute(
                "INSERT INTO color VALUES (?1, ?2)",
                rusqlite::params![i as i64 + 1, name],
            )
            .map_err(err)?;
        }
        for (id, kind, name) in MENU_ITEMS {
            conn.execute(
                "INSERT INTO menuItem VALUES (?1, ?2, ?3)",
                rusqlite::params![id, kind, format!("\u{FFFA}{name}\u{FFFB}")],
            )
            .map_err(err)?;
        }
        for (id, mi, seq, vis) in CATEGORIES {
            conn.execute(
                "INSERT INTO category VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![id, mi, seq, vis],
            )
            .map_err(err)?;
        }
        for (id, mi, seq, vis, sub) in SORTS {
            conn.execute(
                "INSERT INTO sort VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![id, mi, seq, vis, sub],
            )
            .map_err(err)?;
        }
        let mut content = conn
            .prepare(
                "INSERT INTO content VALUES (?1, ?2, ?3, NULL, ?4, ?5, ?6, ?7, \
                 ?8, 0, 0, 0, 0, ?9, ?10, ?11, ?12, 0, NULL, \
                 ?13, ?14, ?15, NULL, ?16, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, \
                 0, 1, 1, NULL, ?25, ?26, ?27, 41, 788224, 0, NULL, 1, 1)",
            )
            .map_err(err)?;
        for tr in &t.tracks {
            content
                .execute(rusqlite::params![
                    tr.id as i64,
                    tr.title,
                    tr.title.to_lowercase(),
                    tr.tempo_centi_bpm as i64,
                    tr.duration_s as i64,
                    tr.track_number as i64,
                    tr.disc_number as i64,
                    zero_null(tr.artist_id),
                    zero_null(tr.album_id),
                    zero_null(tr.genre_id),
                    zero_null(tr.label_id),
                    zero_null(tr.key_id),
                    tr.comment,
                    tr.rating as i64,
                    zero_null(tr.year as u32),
                    tr.date_added,
                    tr.file_path,
                    tr.filename,
                    tr.file_size as i64,
                    tr.file_type as i64,
                    tr.bitrate_kbps as i64,
                    tr.sample_depth as i64,
                    tr.sample_rate_hz as i64,
                    tr.isrc,
                    MASTER_DB_ID,
                    tr.master_content_id as i64,
                    tr.analyze_path,
                ])
                .map_err(err)?;
        }
        drop(content);
        for p in &t.playlists {
            conn.execute(
                "INSERT INTO playlist VALUES (?1, ?2, ?3, NULL, ?4, ?5)",
                rusqlite::params![
                    p.id as i64,
                    p.sort_order as i64,
                    p.name,
                    p.is_folder as i64,
                    p.parent_id as i64,
                ],
            )
            .map_err(err)?;
        }
        for (idx, track, playlist) in &t.playlist_entries {
            conn.execute(
                "INSERT INTO playlist_content VALUES (?1, ?2, ?3)",
                rusqlite::params![*playlist as i64, *track as i64, *idx as i64],
            )
            .map_err(err)?;
        }
        conn.execute(
            "INSERT INTO property VALUES (?1, '1000', ?2, ?3, 0, ?4)",
            rusqlite::params![
                device_name,
                t.tracks.len() as i64,
                t.created_date,
                MASTER_DB_ID,
            ],
        )
        .map_err(err)?;
    }
    conn.execute_batch("COMMIT").map_err(err)?;
    Ok(())
}

/// rekordbox leaves absent interned refs NULL rather than 0.
fn zero_null(v: u32) -> Option<i64> {
    (v != 0).then_some(v as i64)
}

/// A collision-free scratch path on local disk for building a database that
/// will be copied onto the stick whole (process id alone isn't unique enough:
/// parallel test threads share one process).
pub(crate) fn scratch_db_path(prefix: &str) -> std::path::PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(0);
    std::env::temp_dir().join(format!(
        "{prefix}-{}-{}.db",
        std::process::id(),
        SEQ.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The full production writer must persist the complete 22-table schema
    /// and its rows — a partially written exportLibrary.db is exactly what a
    /// player rejects as "Device library is corrupted".
    #[test]
    fn write_library_persists_every_table() {
        let dir = std::env::temp_dir().join(format!("ordnung-dlpw-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("exportLibrary.db");

        let tables = crate::pdbw::PdbTables {
            tracks: vec![crate::pdbw::TrackRow {
                id: 1,
                title: "One".into(),
                filename: "one.mp3".into(),
                file_path: "/Contents/one.mp3".into(),
                ..Default::default()
            }],
            genres: vec![(1, "House".into())],
            artists: vec![(1, "A".into())],
            albums: vec![(1, "B".into())],
            labels: vec![],
            keys: vec![(1, "8A".into())],
            artwork: vec![],
            playlists: vec![crate::pdbw::PlaylistRow {
                id: 1,
                parent_id: 0,
                sort_order: 1,
                is_folder: false,
                name: "set".into(),
            }],
            playlist_entries: vec![(1, 1, 1)],
            created_date: "2026-09-04".into(),
        };
        write_library(&db, &tables, "TEST").expect("write");

        let conn = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        conn.execute_batch(&format!(
            "PRAGMA key = '{DLP_KEY}'; PRAGMA cipher_compatibility = 4;"
        ))
        .unwrap();
        let n_tables: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE type='table'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n_tables, 22, "full DLP schema must be present");
        for (table, want) in [
            ("content", 1i64),
            ("playlist", 1),
            ("playlist_content", 1),
            ("menuItem", 27),
            ("category", 22),
            ("sort", 17),
            ("property", 1),
        ] {
            let n: i64 = conn
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, want, "{table} row count");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Round-trip through a real SQLCipher database: write a miniature
    /// Device Library Plus export with the production key, read it back
    /// through the production reader. Proves the key handling, the schema
    /// queries and the folder/ordering semantics in one go.
    #[test]
    fn reads_playlists_out_of_an_encrypted_device_library() {
        let dir = std::env::temp_dir().join(format!("ordnung-dlp-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("exportLibrary.db");

        {
            let conn = rusqlite::Connection::open(&db).unwrap();
            conn.execute_batch(&format!(
                "PRAGMA key = '{DLP_KEY}'; PRAGMA cipher_compatibility = 4;
                 CREATE TABLE playlist(playlist_id integer primary key, sequenceNo integer, \
                     name varchar, image_id integer, attribute integer, playlist_id_parent integer);
                 CREATE TABLE playlist_content(playlist_id integer, content_id integer, \
                     sequenceNo integer);
                 CREATE TABLE content(content_id integer primary key, path varchar);
                 INSERT INTO playlist VALUES (1, 1, 'CRATES', NULL, 1, 0);
                 INSERT INTO playlist VALUES (2, 2, 'warmup', NULL, 0, 1);
                 INSERT INTO playlist VALUES (3, 3, 'empty one', NULL, 0, 0);
                 INSERT INTO content VALUES (10, '/Contents/A/a.mp3');
                 INSERT INTO content VALUES (11, '/Contents/B/b.mp3');
                 -- Deliberately inserted out of order; sequenceNo decides.
                 INSERT INTO playlist_content VALUES (2, 11, 2);
                 INSERT INTO playlist_content VALUES (2, 10, 1);"
            ))
            .unwrap();
        }

        let got = read_playlists(&db).expect("read back");
        assert_eq!(got.playlists.len(), 3);
        let folder = &got.playlists[0];
        assert!(folder.is_folder);
        assert_eq!(folder.name, "CRATES");
        let leaf = got.playlists.iter().find(|p| p.id == 2).unwrap();
        assert_eq!(leaf.parent_id, 1);
        assert!(!leaf.is_folder);
        assert_eq!(
            got.entries_by_path[&2],
            vec!["/Contents/A/a.mp3".to_string(), "/Contents/B/b.mp3".into()]
        );
        // Present-but-empty, so the sidebar mirrors the player.
        assert_eq!(got.entries_by_path[&3], Vec::<String>::new());

        // Without the key the file must be unreadable — i.e. it really is
        // encrypted, not a plain SQLite file with a wishful pragma.
        let plain = rusqlite::Connection::open_with_flags(
            &db,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        assert!(plain
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0))
            .is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
