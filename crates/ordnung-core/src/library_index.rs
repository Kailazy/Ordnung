//! The library by song: which file, if any, *is* a given song.
//!
//! Every view that shows a song met outside the library — the crate of
//! liked songs, a pasted mix's lines, a record's sheet, the radio — wants
//! to say whether a track in the library already is that song. Each used
//! to decide for itself, with its own folding and its own search, so one
//! song was "FILE" in one view and not in the next. This is the one
//! answer: an index over the tagged tracks, built in one pass (see
//! [`crate::catalog::Catalog::library_index`]) and asked by
//! [`SongRef`], the identity every shape shares.
//!
//! The rules are deliberately conservative — a wrong FILE tells the user
//! not to buy a song they don't have — and the same in every direction:
//!
//! * artist and title both fold the way [`crate::catalog::song_key`]
//!   folds (case, accents, punctuation, `(2)`, `(Original Mix)`);
//! * a song with an artist matches a track credited to exactly that
//!   artist, or one whose joint credit names them (`A & B`, `A feat. B`)
//!   when the title leaves one such track;
//! * a song with no artist matches a track by title alone, and only when
//!   the library has one track of that title;
//! * a title that names nothing ("Untitled", "B2") never matches: files
//!   carry no side position, so nothing can tell four Untitleds apart.

use std::collections::{HashMap, HashSet};

use crate::catalog::{generic_title, norm_match, song_key};
use crate::discogs::strip_original_mix;
use crate::model::{Id, SongRef};
use crate::tracklist::credit_names;

/// One tagged track, as the index knows it.
#[derive(Debug, Clone)]
struct Entry {
    id: Id,
    /// The artist credit, folded.
    artist: String,
    /// The names in that credit, folded, in order — the lead first.
    credits: Vec<String>,
}

/// The library keyed by song. Cheap to hold and to ask; rebuild it after
/// the library changes (a scan, an import, a tag edit).
#[derive(Debug, Clone, Default)]
pub struct LibraryIndex {
    ids: HashSet<Id>,
    /// Song key (artist and title) → the track. Where two tracks share a
    /// key the lower id wins, which is stable.
    by_song: HashMap<String, Id>,
    /// Folded title → every track with it, lowest id first.
    by_title: HashMap<String, Vec<Entry>>,
}

impl LibraryIndex {
    /// Build from `(id, artist, title)` triples. A track missing either is
    /// skipped: it can't be keyed. Order doesn't matter.
    pub fn build<'a, I>(tracks: I) -> Self
    where
        I: IntoIterator<Item = (Id, &'a str, &'a str)>,
    {
        let mut index = Self::default();
        let mut rows: Vec<(Id, &str, &str)> = tracks.into_iter().collect();
        rows.sort_by_key(|r| r.0);
        for (id, artist, title) in rows {
            let key = song_key(artist, title, None, None);
            if key.is_empty() {
                continue;
            }
            index.ids.insert(id);
            index.by_song.entry(key).or_insert(id);
            let t = fold_title(title);
            if t.is_empty() {
                continue;
            }
            index.by_title.entry(t).or_default().push(Entry {
                id,
                artist: norm_match(artist),
                credits: credit_names(artist)
                    .iter()
                    .map(|n| norm_match(n))
                    .filter(|n| !n.is_empty())
                    .collect(),
            });
        }
        index
    }

    /// How many tracks are keyed.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Whether `id` is a keyed track in the library. A row's remembered
    /// track id is only as good as this: the file may have been removed.
    pub fn has(&self, id: Id) -> bool {
        self.ids.contains(&id)
    }

    /// The track that is `song`, if the library has one.
    pub fn track_for(&self, song: &SongRef) -> Option<Id> {
        let title = fold_title(&song.title);
        if title.is_empty() || generic_title(&title) {
            return None;
        }
        let artist = norm_match(&song.artist);
        if artist.is_empty() {
            // Title alone, and only when it's unambiguous.
            return match self.by_title.get(&title).map(Vec::as_slice) {
                Some([one]) => Some(one.id),
                _ => None,
            };
        }
        if let Some(id) = self.by_song.get(&song_key(&song.artist, &song.title, None, None)) {
            return Some(*id);
        }
        // The credit doesn't match whole: is ours one name of a joint
        // credit, either way round?
        let ours: Vec<String> = credit_names(&song.artist)
            .iter()
            .map(|n| norm_match(n))
            .filter(|n| !n.is_empty())
            .collect();
        let candidates = self.by_title.get(&title)?;
        let hit = candidates.iter().find(|e| {
            e.credits.iter().any(|c| *c == artist)
                || (ours.len() > 1
                    && (e.artist == ours[0]
                        || e.credits.first() == ours.first()
                        || ours.iter().all(|w| e.credits.contains(w))))
        })?;
        Some(hit.id)
    }

    /// The track that is `song`, preferring the one a row already knows
    /// (`known`) while it's still in the library. A remembered id that
    /// points at nothing (the file was removed) falls through to a fresh
    /// look-up, so a liked song re-imported later lights up again.
    pub fn resolve(&self, song: &SongRef, known: Option<Id>) -> Option<Id> {
        known.filter(|id| self.has(*id)).or_else(|| self.track_for(song))
    }
}

/// A title the way the index keys it: the `(Original Mix)` marker off,
/// then folded like a song key.
fn fold_title(title: &str) -> String {
    norm_match(strip_original_mix(title))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> LibraryIndex {
        LibraryIndex::build([
            (1, "Metro Area", "Miura (Original Mix)"),
            (2, "Cajmere ft. Dajae", "Brighter Days"),
            (3, "Björk", "Hyperballad"),
            (4, "Someone", "Untitled"),
            (5, "A", "Same Title"),
            (6, "B", "Same Title"),
            (7, "D. Tiffany & Roza Terenzi", "Tumbler"),
            (8, "", "Only Title"),
            (9, "", ""),
        ])
    }

    #[test]
    fn matches_the_song_key_with_the_mix_marker_and_case_folded() {
        let ix = index();
        assert_eq!(ix.track_for(&SongRef::new("metro area", "MIURA")), Some(1));
        assert_eq!(ix.track_for(&SongRef::new("Metro Area (2)", "Miura (Original)")), Some(1));
        assert_eq!(ix.track_for(&SongRef::new("Bjork", "Hyperballad")), Some(3));
        assert_eq!(ix.track_for(&SongRef::new("Metro Area", "Miura (Carl Craig Remix)")), None);
        assert_eq!(ix.len(), 8);
        assert!(!ix.has(9));
    }

    #[test]
    fn joint_credits_agree_either_way_round() {
        let ix = index();
        // Ours is one name of the file's credit.
        assert_eq!(ix.track_for(&SongRef::new("Cajmere", "Brighter Days")), Some(2));
        assert_eq!(ix.track_for(&SongRef::new("Dajae", "Brighter Days")), Some(2));
        // The file names one of ours as its lead.
        assert_eq!(ix.track_for(&SongRef::new("Cajmere feat. Somebody Else", "Brighter Days")), Some(2));
        // Joint credit in the other order.
        assert_eq!(ix.track_for(&SongRef::new("Roza Terenzi & D. Tiffany", "Tumbler")), Some(7));
        assert_eq!(ix.track_for(&SongRef::new("Nobody", "Brighter Days")), None);
    }

    #[test]
    fn a_song_without_an_artist_matches_only_a_unique_title() {
        let ix = index();
        assert_eq!(ix.track_for(&SongRef::new("", "Brighter Days")), Some(2));
        assert_eq!(ix.track_for(&SongRef::new("", "Same Title")), None);
        assert_eq!(ix.track_for(&SongRef::new("", "Only Title")), Some(8));
        assert_eq!(ix.track_for(&SongRef::new("X", "Same Title")), None);
    }

    #[test]
    fn a_title_that_names_nothing_never_matches() {
        let ix = index();
        assert_eq!(ix.track_for(&SongRef::new("Someone", "Untitled")), None);
        assert_eq!(
            ix.track_for(&SongRef::new("Someone", "Untitled").at(Some(9), Some("A1"))),
            None
        );
        assert_eq!(ix.track_for(&SongRef::new("Someone", "")), None);
        assert_eq!(ix.track_for(&SongRef::new("", "")), None);
    }

    #[test]
    fn resolve_keeps_a_known_track_only_while_it_is_there() {
        let ix = index();
        let song = SongRef::new("Metro Area", "Miura");
        assert_eq!(ix.resolve(&song, Some(3)), Some(3));
        assert_eq!(ix.resolve(&song, Some(999)), Some(1));
        assert_eq!(ix.resolve(&SongRef::new("Nobody", "Nothing"), Some(999)), None);
    }
}
