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
const DLP_KEY: &str = "r8gddnr4k847830ar6cqzbkk0el6qytmb3trbbx805jm74vez64i5o8fnrqryqls";

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

#[cfg(test)]
mod tests {
    use super::*;

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
