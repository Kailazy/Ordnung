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

    /// Something went wrong talking to Discogs that wasn't an HTTP status:
    /// the connection itself, or a response we couldn't read. The message is
    /// a complete sentence, ready to show as-is.
    #[error("{0}")]
    Network(String),

    /// Discogs answered with an HTTP error. `message` is the human sentence
    /// from the response body (already reworded for the user, see
    /// `discogs::map_ureq_err`), or empty when the body carried none.
    #[error("{}", describe_discogs(*status, message))]
    Discogs { status: u16, message: String },
}

/// One plain sentence for a Discogs HTTP failure, written for the person
/// using the app rather than for whoever debugs it: no status codes, no
/// JSON. Each status the API actually sends gets its own wording; the code
/// is only spelled out for a status we don't recognise, where it's the one
/// clue there is.
fn describe_discogs(status: u16, message: &str) -> String {
    let said = |fallback: &str| {
        if message.is_empty() {
            fallback.to_string()
        } else {
            format!("Discogs says {message}")
        }
    };
    match status {
        401 => "Discogs doesn't accept your token. Check it in Settings".to_string(),
        403 => said("Discogs won't allow that"),
        404 => said("Discogs can't find that record"),
        429 => "Discogs asked us to slow down. Try again in a minute".to_string(),
        500..=599 => "Discogs is having trouble right now. Try again in a few minutes".to_string(),
        _ => format!("{} (HTTP {status})", said("Discogs couldn't do that")),
    }
}

pub type Result<T> = std::result::Result<T, Error>;
