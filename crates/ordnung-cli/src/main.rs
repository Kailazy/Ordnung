//! Ordnung CLI — the only layer that decides policy, prints, and prompts.
//!
//! Phase 1 ships `scan`, `ls`, and `tag` over the SQLite catalog, plus the `key`
//! demo. Later commands (analyze/playlist/convert/export) are stubbed to their
//! phases — see the `ordnung-roadmap` skill.

mod commands;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ordnung",
    version,
    about = "Fast DJ catalog & rekordbox/CDJ USB exporter",
    long_about = "Ordnung catalogs, analyzes, and exports a DJ library to native \
rekordbox/CDJ USBs. Nothing is converted, retagged, or exported unless you ask."
)]
struct Cli {
    /// Path to the catalog database (created if missing).
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Discover audio files in a folder and read tags + properties into the catalog.
    Scan {
        /// Folder to scan recursively.
        dir: PathBuf,
    },
    /// List tracks in the catalog.
    Ls {
        /// Filter by substring across artist/title/album/genre.
        query: Option<String>,
        /// Max rows to show (0 = all).
        #[arg(long, default_value_t = 50)]
        limit: usize,
    },
    /// Show or edit a track's metadata. Edits stay in the catalog unless --write.
    Tag {
        /// Track id (from `ls`).
        id: u64,
        /// Set a field, repeatable: --set artist="Barker" --set genre=Techno
        #[arg(long = "set", value_name = "FIELD=VALUE")]
        set: Vec<String>,
        /// Also write the changes back into the source file's tags.
        #[arg(long)]
        write: bool,
        /// With --write, also embed fetched cover artwork into the source file.
        #[arg(long, requires = "write")]
        art: bool,
    },
    /// Detect BPM, musical key (Camelot), beat anchor, and waveform; cached.
    Analyze {
        /// Re-analyze even if a current cached result exists.
        #[arg(long)]
        force: bool,
        /// Only analyze tracks matching this substring (artist/title/album/genre).
        query: Option<String>,
    },
    /// Show Camelot / Open Key / classical notation for a key, e.g. `key Am`.
    Key { key: String },

    /// List catalog tracks whose source file no longer exists on disk, and
    /// suggest how to repoint them. Read-only — changes nothing.
    Missing,

    /// Find duplicate tracks — byte-identical imports and same-song format
    /// variants — and mark the best copy of each. Read-only; never deletes.
    #[command(visible_alias = "duplicates")]
    Dupes,

    /// Repoint the catalog after you move or rename a source folder, so tracks
    /// keep their playlists, analysis, and ids. Rewrites every source path under
    /// FROM to sit under TO instead. (A plain `scan` also auto-detects individual
    /// moved files by content; use this for a whole folder at once.)
    Relink {
        /// Old folder path (the prefix that currently appears in source paths).
        from: PathBuf,
        /// New folder path to point those tracks at.
        to: PathBuf,
        /// Show what would change without writing anything.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },

    /// Create and edit playlists and playlist folders.
    Playlist {
        #[command(subcommand)]
        action: PlaylistCmd,
    },
    /// Convert tracks to a CDJ-compatible format (explicit; never automatic).
    Convert {
        /// Track id(s) to convert (from `ls`).
        #[arg(required = true, value_name = "TRACK_ID")]
        ids: Vec<u64>,
        /// Target format: mp3 | aac | wav | aiff | flac.
        #[arg(long = "to", value_name = "FORMAT")]
        to: String,
        /// Bitrate (kbps) for lossy targets. Defaults: mp3 320, aac 256.
        #[arg(long)]
        bitrate: Option<u32>,
        /// Write outputs to this directory (default: alongside each source).
        #[arg(long)]
        out: Option<PathBuf>,
        /// Replace each source file with its conversion and repoint the catalog.
        #[arg(long = "in-place")]
        in_place: bool,
        /// Skip the confirmation prompt that --in-place otherwise requires.
        #[arg(long)]
        yes: bool,
    },
    /// [Phase 5] Build a rekordbox/CDJ USB.
    Export,
}

#[derive(Subcommand)]
enum PlaylistCmd {
    /// Create a new playlist (or a folder with --folder).
    New {
        /// Playlist/folder name.
        name: String,
        /// Create a folder instead of a playlist.
        #[arg(long)]
        folder: bool,
        /// Nest under this folder id.
        #[arg(long)]
        parent: Option<u64>,
    },
    /// List all playlists and folders as a tree.
    Ls,
    /// Show a playlist's tracks.
    Show {
        /// Playlist id (from `playlist ls`).
        id: u64,
    },
    /// Add track ids (from `ls`) to a playlist.
    Add {
        id: u64,
        #[arg(required = true, value_name = "TRACK_ID")]
        tracks: Vec<u64>,
    },
    /// Remove track ids from a playlist.
    Rm {
        id: u64,
        #[arg(required = true, value_name = "TRACK_ID")]
        tracks: Vec<u64>,
    },
    /// Reorder a playlist: list ALL its track ids in the desired order.
    Reorder {
        id: u64,
        #[arg(required = true, value_name = "TRACK_ID")]
        tracks: Vec<u64>,
    },
    /// Rename a playlist or folder.
    Rename { id: u64, name: String },
    /// Move a playlist/folder under a folder (omit --parent to move to top level).
    Mv {
        id: u64,
        #[arg(long)]
        parent: Option<u64>,
    },
    /// Delete a playlist or folder (a folder also deletes its contents).
    Delete { id: u64 },
}

fn main() {
    let cli = Cli::parse();
    let db = cli
        .db
        .unwrap_or_else(|| default_db_path().unwrap_or_else(|| PathBuf::from("ordnung.db")));

    let result = match cli.command {
        Command::Scan { dir } => commands::scan(&db, &dir),
        Command::Ls { query, limit } => commands::ls(&db, query.as_deref(), limit),
        Command::Tag { id, set, write, art } => commands::tag(&db, id, &set, write, art),
        Command::Analyze { force, query } => commands::analyze(&db, query.as_deref(), force),
        Command::Key { key } => commands::key(&key),
        Command::Missing => commands::missing(&db),
        Command::Dupes => commands::dupes(&db),
        Command::Relink { from, to, dry_run } => commands::relink(&db, &from, &to, dry_run),
        Command::Playlist { action } => commands::playlist(&db, action),
        Command::Convert { ids, to, bitrate, out, in_place, yes } => {
            commands::convert(&db, &ids, &to, bitrate, out.as_deref(), in_place, yes)
        }
        Command::Export => Err(anyhow::anyhow!(
            "not implemented yet — see the ordnung-roadmap skill"
        )),
    };

    if let Err(e) = result {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

/// Default catalog location: ~/.ordnung/catalog.db
fn default_db_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    let dir = PathBuf::from(home).join(".ordnung");
    std::fs::create_dir_all(&dir).ok()?;
    Some(dir.join("catalog.db"))
}
