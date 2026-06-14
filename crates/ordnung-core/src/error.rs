//! Typed errors for the core engines. The CLI maps these to user-facing messages.

use std::path::PathBuf;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("audio tag error on {path}: {source}")]
    Tag {
        path: PathBuf,
        #[source]
        source: lofty::error::LoftyError,
    },

    #[error("could not decode audio at {path}: {msg}")]
    Decode { path: PathBuf, msg: String },

    #[error("conversion failed for {path}: {msg}")]
    Convert { path: PathBuf, msg: String },

    #[error("track not found: {0}")]
    NotFound(String),

    #[error("invalid operation: {0}")]
    Invalid(String),

    #[error("network error: {0}")]
    Network(String),
}

pub type Result<T> = std::result::Result<T, Error>;
