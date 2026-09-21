//! Song identity: the one shape every place a song shows up agrees on.
//!
//! A song turns up as a file in the library ([`super::Track`]), a row of a
//! record's Discogs tracklist ([`crate::discogs::ReleaseTrack`]), a line of
//! a pasted mix ([`super::TracklistEntry`]), a song on the radio, and a
//! like ([`super::LikedSong`]). Those are different things — a file has
//! bytes and an analysis, a like is a bookmark, a tracklist line carries
//! match state — and they stay different structs. What they share is
//! *which song* they are, and that is a [`SongRef`]: artist and title,
//! plus the record and position it was met at, for the titles that name
//! nothing on their own ("Untitled", "B2"). Every one of those shapes
//! answers `song()` with one. The crate of liked songs keys on it, and so
//! does the library index ([`crate::library_index`]), so "is this liked?"
//! and "is this a file I have?" mean the same thing in every view.

/// Which song a row is about. Compare by [`key`](Self::key), never by the
/// raw fields: the key folds case, accents, punctuation, a Discogs
/// `(2)` disambiguator on the artist and an `(Original Mix)` marker on the
/// title, so a tag, a Discogs listing and a pasted line for one song
/// agree.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SongRef {
    pub artist: String,
    pub title: String,
    /// The Discogs release the song was met on, when there was one. Part
    /// of the identity only for a title that names nothing.
    pub release_id: Option<u64>,
    /// The song's position on that release (`A1`), likewise.
    pub position: Option<String>,
}

impl SongRef {
    pub fn new(artist: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            artist: artist.into(),
            title: title.into(),
            release_id: None,
            position: None,
        }
    }

    /// The same song, met on a record. A blank position counts as none.
    pub fn at(mut self, release_id: Option<u64>, position: Option<&str>) -> Self {
        self.release_id = release_id;
        self.position = position
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .map(str::to_string);
        self
    }

    /// The identity key (see [`crate::catalog::song_key`]). Empty when
    /// neither the artist nor the title has a word in it.
    pub fn key(&self) -> String {
        crate::catalog::song_key(
            &self.artist,
            &self.title,
            self.release_id,
            self.position.as_deref(),
        )
    }

    /// Whether the song names nothing at all.
    pub fn is_empty(&self) -> bool {
        self.key().is_empty()
    }

    /// `Artist - Title`, for status lines; the title alone when there is
    /// no artist.
    pub fn label(&self) -> String {
        match (self.artist.trim(), self.title.trim()) {
            ("", t) => t.to_string(),
            (a, "") => a.to_string(),
            (a, t) => format!("{a} - {t}"),
        }
    }
}
