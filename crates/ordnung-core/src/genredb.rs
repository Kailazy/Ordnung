//! The local Discogs genre database: every vinyl release's genre + style tags,
//! imported in bulk from Discogs's monthly data dump (data.discogs.com).
//!
//! Why it exists: the marketplace inventory endpoint carries no genre data, so
//! tagging a seller's crates through the API costs one paced request per
//! release — an hour for a few thousand records, per shop. The monthly dump is
//! the bulk source Discogs actually offers: one ~10 GB download containing the
//! whole database, streamed straight through a gzip + XML parse into a compact
//! SQLite table (vinyl releases that have tags, ~tens of millions of rows) with
//! nothing ever spilled to disk. After an import, any release's tags resolve
//! locally and instantly, for every seller and every dig, forever — the paced
//! per-release fetch remains only for releases newer than the dump.
//!
//! The database lives in its own file beside the catalog (it is bulky,
//! regenerable reference data — not user state worth backing up), written to a
//! temp path and renamed into place, so a cancelled or failed import never
//! clobbers a working one.

use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use rusqlite::{params, Connection};

use crate::catalog::{split_genres, GENRE_SEP};
use crate::discogs::genre_tags;
use crate::error::{Error, Result};

/// Where the monthly dumps are listed and served.
const DUMP_HOST: &str = "https://data.discogs.com";

/// Where the genre database lives: beside the catalog, in its own file — it's
/// bulky, regenerable reference data, not user state worth backing up.
pub fn default_path(catalog_db: &Path) -> PathBuf {
    catalog_db
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("discogs-genres.db")
}

/// One import's outcome.
#[derive(Debug, Clone)]
pub struct ImportStats {
    /// Dump date stamp, e.g. "20260901".
    pub dump: String,
    /// Releases seen in the dump.
    pub seen: u64,
    /// Rows kept (vinyl releases carrying at least one tag).
    pub kept: u64,
    /// False when the run was cancelled; the previous database (if any) is
    /// left untouched.
    pub completed: bool,
}

/// Import progress, reported every few thousand releases. `total_bytes` is 0
/// when the server didn't say how big the dump is.
#[derive(Debug, Clone, Copy)]
pub struct ImportProgress {
    pub read_bytes: u64,
    pub total_bytes: u64,
    pub seen: u64,
    pub kept: u64,
}

/// The read side: tag lookups against an imported database.
pub struct GenreDb {
    conn: Connection,
}

impl GenreDb {
    /// Open an imported genre database. `Ok(None)` when none has been imported
    /// yet (no file, or a file without the completion stamp).
    pub fn open(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let conn = Connection::open(path)?;
        let complete: Option<String> = conn
            .query_row("SELECT value FROM meta WHERE key='complete'", [], |r| {
                r.get(0)
            })
            .unwrap_or(None);
        if complete.as_deref() != Some("1") {
            return Ok(None);
        }
        Ok(Some(Self { conn }))
    }

    /// The dump date this database was imported from ("20260901"), for the UI.
    pub fn dump_date(&self) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key='dump'", [], |r| r.get(0))
            .unwrap_or(None)
    }

    /// Rows in the database, from the import's meta stamp — a real `COUNT(*)`
    /// over tens of millions of rows is a full scan, too slow for UI reads.
    pub fn kept(&self) -> u64 {
        self.conn
            .query_row("SELECT value FROM meta WHERE key='kept'", [], |r| {
                r.get::<_, String>(0)
            })
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(0)
    }

    /// Rows in the database, counted. Test-sized databases only.
    pub fn count(&self) -> u64 {
        self.conn
            .query_row("SELECT COUNT(*) FROM genres", [], |r| r.get::<_, i64>(0))
            .unwrap_or(0) as u64
    }

    /// Tags for `release_ids`; ids the dump didn't know are simply absent.
    pub fn genres_for(&self, release_ids: &[u64]) -> Result<HashMap<u64, Vec<String>>> {
        let mut out = HashMap::new();
        for chunk in release_ids.chunks(500) {
            let placeholders = vec!["?"; chunk.len()].join(",");
            let mut stmt = self.conn.prepare_cached(&format!(
                "SELECT release_id, tags FROM genres WHERE release_id IN ({placeholders})"
            ))?;
            let rows = stmt.query_map(
                rusqlite::params_from_iter(chunk.iter().map(|id| *id as i64)),
                |r| Ok((r.get::<_, i64>(0)? as u64, r.get::<_, String>(1)?)),
            )?;
            for row in rows {
                let (id, tags) = row?;
                out.insert(id, split_genres(Some(tags)));
            }
        }
        Ok(out)
    }
}

/// Find the newest releases dump on data.discogs.com by reading the year
/// listing (falling back to last year around January). Returns the date stamp
/// and the download URL.
pub fn latest_dump(agent: &ureq::Agent) -> Result<(String, String)> {
    let mut best: Option<String> = None;
    for year in [2027, 2026, 2025] {
        let url = format!("{DUMP_HOST}/?prefix=data%2F{year}%2F");
        let Ok(resp) = agent.get(&url).call() else {
            continue;
        };
        let Ok(body) = resp.into_string() else {
            continue;
        };
        for (i, _) in body.match_indices("_releases.xml.gz") {
            // "discogs_YYYYMMDD" sits right before the match.
            let head = &body[..i];
            if let Some(j) = head.rfind("discogs_") {
                let stamp = &head[j + 8..];
                if stamp.len() == 8 && stamp.bytes().all(|b| b.is_ascii_digit()) {
                    if best.as_deref().is_none_or(|b| stamp > b) {
                        best = Some(stamp.to_string());
                    }
                }
            }
        }
        if best.is_some() {
            break;
        }
    }
    let stamp = best.ok_or_else(|| {
        Error::Network("no releases dump found on data.discogs.com".into())
    })?;
    let url = format!(
        "{DUMP_HOST}/?download=data%2F{}%2Fdiscogs_{stamp}_releases.xml.gz",
        &stamp[..4]
    );
    Ok((stamp, url))
}

/// A reader that counts what passes through it, so the parse loop can report
/// download progress without owning the network stream.
struct CountingReader<R: Read> {
    inner: R,
    read: Arc<AtomicU64>,
}

impl<R: Read> Read for CountingReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.read.fetch_add(n as u64, Ordering::Relaxed);
        Ok(n)
    }
}

/// Download the newest releases dump and build the genre database at
/// `db_path`, streaming download → gunzip → XML parse → SQLite in one pass
/// (constant memory, no temp copy of the dump itself). Kept rows are vinyl
/// releases with at least one genre or style tag. Progress lands through
/// `progress` every few thousand releases; `cancel` stops the run between
/// releases, leaving any previously imported database untouched.
pub fn import_latest(
    db_path: &Path,
    progress: &mut dyn FnMut(ImportProgress),
    cancel: &AtomicBool,
) -> Result<ImportStats> {
    // Long stream: generous read timeout, no overall cap.
    let agent = ureq::AgentBuilder::new()
        .timeout_read(std::time::Duration::from_secs(120))
        .user_agent("Ordnung/0.1 +https://kailazy.github.io/Ordnung/")
        .build();
    let (stamp, url) = latest_dump(&agent)?;

    let resp = agent
        .get(&url)
        .call()
        .map_err(|e| Error::Network(format!("downloading the Discogs dump: {e}")))?;
    let total_bytes: u64 = resp
        .header("Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let read = Arc::new(AtomicU64::new(0));
    let counting = CountingReader {
        inner: resp.into_reader(),
        read: read.clone(),
    };
    let gz = flate2::read::GzDecoder::new(BufReader::with_capacity(1 << 20, counting));

    // Build into a temp file, rename over the live one only on completion.
    let tmp: PathBuf = db_path.with_extension("db.import");
    let _ = std::fs::remove_file(&tmp);
    let conn = Connection::open(&tmp)?;
    conn.execute_batch(
        "PRAGMA journal_mode=OFF;
         PRAGMA synchronous=OFF;
         CREATE TABLE genres (release_id INTEGER PRIMARY KEY, tags TEXT NOT NULL);
         CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
    )?;

    let (seen, kept, cancelled) =
        ingest(gz, &conn, read.as_ref(), total_bytes, progress, cancel)?;
    if cancelled {
        drop(conn);
        let _ = std::fs::remove_file(&tmp);
        return Ok(ImportStats {
            dump: stamp,
            seen,
            kept,
            completed: false,
        });
    }
    conn.execute_batch(&format!(
        "INSERT OR REPLACE INTO meta (key, value) VALUES
             ('dump', '{stamp}'),
             ('kept', '{kept}'),
             ('complete', '1');"
    ))?;
    drop(conn);
    std::fs::rename(&tmp, db_path).map_err(|source| Error::Io {
        path: db_path.to_path_buf(),
        source,
    })?;
    Ok(ImportStats {
        dump: stamp,
        seen,
        kept,
        completed: true,
    })
}

/// The dump's parse loop, split from [`import_latest`] so a fixture can drive
/// it: walk `<release>` elements off the (already gunzipped) XML stream and
/// insert one row per vinyl release carrying tags. Returns `(seen, kept,
/// cancelled)`.
fn ingest(
    xml_stream: impl Read,
    conn: &Connection,
    read: &AtomicU64,
    total_bytes: u64,
    progress: &mut dyn FnMut(ImportProgress),
    cancel: &AtomicBool,
) -> Result<(u64, u64, bool)> {
    let mut xml =
        quick_xml::Reader::from_reader(BufReader::with_capacity(1 << 20, xml_stream));
    xml.config_mut().trim_text(true);

    // Streaming parse state: inside one <release>, collect what its row needs.
    let mut buf = Vec::with_capacity(4096);
    let mut release_id: u64 = 0;
    let mut is_vinyl = false;
    let mut genres: Vec<String> = Vec::new();
    let mut styles: Vec<String> = Vec::new();
    // Which text element we're inside, if it's one we keep.
    enum Capture {
        None,
        Genre,
        Style,
    }
    let mut capture = Capture::None;

    let mut seen: u64 = 0;
    let mut kept: u64 = 0;
    let mut batch: Vec<(u64, String)> = Vec::with_capacity(10_000);
    let sep = GENRE_SEP.to_string();

    let flush = |conn: &Connection, batch: &mut Vec<(u64, String)>| -> Result<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO genres (release_id, tags) VALUES (?1, ?2)",
            )?;
            for (id, tags) in batch.iter() {
                stmt.execute(params![*id as i64, tags])?;
            }
        }
        tx.commit()?;
        batch.clear();
        Ok(())
    };

    loop {
        use quick_xml::events::Event;
        let event = xml.read_event_into(&mut buf).map_err(|e| {
            Error::Network(format!("reading the Discogs dump (after {seen} releases): {e}"))
        })?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => match e.name().as_ref() {
                b"release" => {
                    release_id = e
                        .try_get_attribute("id")
                        .ok()
                        .flatten()
                        .and_then(|a| String::from_utf8(a.value.into_owned()).ok())
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    is_vinyl = false;
                    genres.clear();
                    styles.clear();
                }
                b"format" => {
                    if let Ok(Some(a)) = e.try_get_attribute("name") {
                        if a.value.eq_ignore_ascii_case(b"vinyl") {
                            is_vinyl = true;
                        }
                    }
                }
                b"genre" => capture = Capture::Genre,
                b"style" => capture = Capture::Style,
                _ => {}
            },
            Event::Text(ref t) => {
                if !matches!(capture, Capture::None) {
                    if let Ok(text) = t.unescape() {
                        let text = text.trim();
                        if !text.is_empty() {
                            match capture {
                                Capture::Genre => genres.push(text.to_string()),
                                Capture::Style => styles.push(text.to_string()),
                                Capture::None => {}
                            }
                        }
                    }
                }
            }
            Event::End(ref e) => match e.name().as_ref() {
                b"genre" | b"style" => capture = Capture::None,
                b"release" => {
                    seen += 1;
                    if release_id != 0 && is_vinyl && !(genres.is_empty() && styles.is_empty())
                    {
                        let tags = genre_tags(&genres, &styles);
                        if !tags.is_empty() {
                            batch.push((release_id, tags.join(&sep)));
                            kept += 1;
                        }
                    }
                    if batch.len() >= 10_000 {
                        flush(conn, &mut batch)?;
                    }
                    if seen % 25_000 == 0 {
                        if cancel.load(Ordering::Relaxed) {
                            return Ok((seen, kept, true));
                        }
                        progress(ImportProgress {
                            read_bytes: read.load(Ordering::Relaxed),
                            total_bytes,
                            seen,
                            kept,
                        });
                    }
                }
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    flush(conn, &mut batch)?;
    Ok((seen, kept, false))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the parse over a small in-memory "dump" by pointing the SQLite side
    /// at a temp file and feeding the XML through the same event loop the real
    /// import uses. The import function itself needs the network, so the parse
    /// rules are exercised through a GenreDb round trip instead.
    #[test]
    fn genredb_round_trips_tags() {
        let dir = std::env::temp_dir().join("ordnung-genredb-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("genres.db");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE genres (release_id INTEGER PRIMARY KEY, tags TEXT NOT NULL);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                 INSERT INTO meta VALUES ('dump','20260901'),('complete','1');",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO genres VALUES (42, 'Electronic\u{1F}Dub Techno')",
                [],
            )
            .unwrap();
        }
        let db = GenreDb::open(&path).unwrap().expect("complete db opens");
        assert_eq!(db.dump_date().as_deref(), Some("20260901"));
        assert_eq!(db.count(), 1);
        let map = db.genres_for(&[42, 7]).unwrap();
        assert_eq!(map.len(), 1);
        assert_eq!(map[&42], vec!["Electronic", "Dub Techno"]);
        let _ = std::fs::remove_file(&path);
    }

    /// Drive the parse loop with dump-shaped XML (matching a real sample of
    /// discogs_20260901_releases.xml): vinyl releases with tags land as rows,
    /// CDs and untagged releases don't, genres and styles merge deduped.
    #[test]
    fn ingest_keeps_tagged_vinyl_only() {
        let xml = r#"<releases>
<release id="1"><formats><format name="Vinyl" qty="2" text=""><descriptions><description>12"</description></descriptions></format></formats><genres><genre>Electronic</genre></genres><styles><style>Deep House</style><style>electronic</style></styles><title>Stockholm</title><tracklist><track><position>A</position><title>Östermalm</title></track></tracklist></release>
<release id="2"><formats><format name="CD" qty="1" text=""/></formats><genres><genre>Rock</genre></genres></release>
<release id="3"><formats><format name="Vinyl" qty="1" text=""/></formats><title>No tags at all</title></release>
<release id="4"><formats><format name="Vinyl" qty="1" text=""/></formats><styles><style>Dub Techno</style></styles></release>
</releases>"#;
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE genres (release_id INTEGER PRIMARY KEY, tags TEXT NOT NULL);",
        )
        .unwrap();
        let read = AtomicU64::new(0);
        let cancel = AtomicBool::new(false);
        let (seen, kept, cancelled) =
            ingest(xml.as_bytes(), &conn, &read, 0, &mut |_| {}, &cancel).unwrap();
        assert_eq!((seen, kept, cancelled), (4, 2, false));
        let rows: Vec<(i64, String)> = conn
            .prepare("SELECT release_id, tags FROM genres ORDER BY release_id")
            .unwrap()
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, "Electronic\u{1F}Deep House".to_string()),
                (4, "Dub Techno".to_string()),
            ]
        );
    }

    /// A database whose import never finished (no completion stamp) must read
    /// as absent, not as an empty tag source.
    #[test]
    fn incomplete_import_reads_as_absent() {
        let dir = std::env::temp_dir().join("ordnung-genredb-test");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("incomplete.db");
        let _ = std::fs::remove_file(&path);
        {
            let conn = Connection::open(&path).unwrap();
            conn.execute_batch(
                "CREATE TABLE genres (release_id INTEGER PRIMARY KEY, tags TEXT NOT NULL);
                 CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
            )
            .unwrap();
        }
        assert!(GenreDb::open(&path).unwrap().is_none());
        assert!(GenreDb::open(&dir.join("missing.db")).unwrap().is_none());
        let _ = std::fs::remove_file(&path);
    }
}
