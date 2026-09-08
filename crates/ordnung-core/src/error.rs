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

    /// Discogs answered with an HTTP error. `message` is the human sentence
    /// from the response body (already reworded for the user, see
    /// `discogs::map_ureq_err`), or empty when the body carried none.
    #[error("{}", describe_discogs(*status, message))]
    Discogs { status: u16, message: String },
}

/// One short sentence for a Discogs HTTP failure. The status codes the API
/// actually sends each get their own wording, so the status bar reads
/// "Discogs rejected your token" rather than a status code and a JSON blob.
fn describe_discogs(status: u16, message: &str) -> String {
    let said = |fallback: &str| {
        if message.is_empty() {
            fallback.to_string()
        } else {
            format!("Discogs says {message}")
        }
    };
    match status {
        401 => "Discogs rejected your token (HTTP 401)".to_string(),
        403 => format!("{} (HTTP 403)", said("Discogs refused the request")),
        404 => format!("{} (HTTP 404)", said("Discogs has no such record")),
        429 => "Discogs is rate limiting requests, try again in a minute (HTTP 429)".to_string(),
        500..=599 => {
            format!("Discogs is having trouble right now, try again later (HTTP {status})")
        }
        _ => format!(
            "{} (HTTP {status})",
            said("Discogs turned the request down")
        ),
    }
}

pub type Result<T> = std::result::Result<T, Error>;
