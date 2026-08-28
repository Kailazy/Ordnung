//! Unified library search: one query, ranked hits across the digital catalog and
//! the Discogs vinyl collection/wantlist.
//!
//! The toolbar's search box narrows the track table, which answers "show me the
//! rows matching this" but not "what *is* this?". A DJ typing an artist, an
//! album or a song name usually means the second question — and the answer may
//! be a file, a record on the shelf, or both. This module resolves a free-text
//! query into a small ranked list of concrete things, so the GUI can offer them
//! as a dropdown that jumps straight to the track or the record.
//!
//! Ranking exists to make the *first* hit the right one. Matches are scored by
//! how completely and how precisely they match rather than by row order, so a
//! query that names one record exactly collapses to a single confident hit
//! instead of burying it under partial matches.

use crate::catalog::{norm_match, Catalog};
use crate::error::Result;
use crate::model::{Id, VinylList};

/// What a hit points at, and what clicking it should do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchHit {
    /// A track in the digital catalog. The GUI selects and scrolls to it.
    Track {
        id: Id,
        title: String,
        artist: String,
        album: String,
    },
    /// A record in the collection or wantlist. The GUI opens its release sheet.
    ///
    /// `matched_track` names the song that matched when the query hit the
    /// record's *tracklist* rather than its artist/title — the answer to "which
    /// record is this song on?", which is otherwise invisible in a hit that
    /// shows only the release name.
    Vinyl {
        list: VinylList,
        instance_id: u64,
        release_id: u64,
        title: String,
        artist: String,
        /// Year/format/label line, pre-joined for display.
        sub: String,
        matched_track: Option<String>,
    },
}

/// One ranked result.
#[derive(Debug, Clone)]
pub struct ScoredHit {
    pub hit: SearchHit,
    /// Higher is better. Only meaningful for ordering within one query.
    pub score: i32,
}

/// How strongly one normalized field matches the normalized query.
///
/// The tiers are deliberately far apart so that a stronger match on any single
/// field always outranks a pile of weak ones: an exact title beats three
/// substring hits, which is what makes a fully-typed name collapse to one hit.
fn field_score(field: &str, query: &str) -> i32 {
    if field.is_empty() || query.is_empty() {
        return 0;
    }
    if field == query {
        return 100;
    }
    if field.starts_with(query) {
        return 60;
    }
    if field.contains(query) {
        return 35;
    }
    // Every query word present somewhere in the field, in any order — this is
    // what lets "lawrence glow" match a title of "Glow" by artist "Lawrence"
    // once the caller concatenates the fields it searches.
    if query.split_whitespace().all(|w| field.contains(w)) {
        return 20;
    }
    0
}

/// Score a hit from its searchable fields. `weights` pairs each field with a
/// multiplier so a match on the thing the user most likely typed (a title)
/// outranks the same match on a supporting field (a label).
fn score_fields(query: &str, weights: &[(&str, i32)]) -> i32 {
    let mut best = 0;
    let mut total = 0;
    for (field, weight) in weights {
        let s = field_score(&norm_match(field), query) * weight;
        best = best.max(s);
        total += s;
    }
    // Dominated by the strongest single field, with a small bonus for matching
    // in several places — so "Lawrence" on both artist and album ranks above
    // "Lawrence" on artist alone, without letting breadth beat an exact hit.
    best + total / 8
}

/// Resolve `query` into at most `limit` ranked hits across both libraries.
///
/// Returns an empty list for a blank query. Digital and vinyl hits compete on
/// one scale, so a query naming a record the user owns on both formats surfaces
/// both — which is the point: "do I have this, and on what?".
pub fn search_library(cat: &Catalog, query: &str, limit: usize) -> Result<Vec<ScoredHit>> {
    let q = norm_match(query);
    if q.is_empty() || limit == 0 {
        return Ok(Vec::new());
    }
    let mut out: Vec<ScoredHit> = Vec::new();

    // ---- Digital catalog ------------------------------------------------
    // The SQL prefilter is the same LIKE search the table uses, so the scan
    // stays proportional to what matched rather than to the whole library.
    for t in cat.list_tracks(Some(query), 400)? {
        let title = t.tags.title.clone().unwrap_or_default();
        let artist = t.tags.artist.clone().unwrap_or_default();
        let album = t.tags.album.clone().unwrap_or_default();
        // "artist title" as one field catches a query that spans both, which is
        // how people actually type a song they're looking for.
        let combined = format!("{artist} {title}");
        let score = score_fields(
            &q,
            &[
                (&title, 3),
                (&artist, 2),
                (&album, 2),
                (&combined, 3),
            ],
        );
        if score > 0 {
            out.push(ScoredHit {
                hit: SearchHit::Track {
                    id: t.id,
                    title,
                    artist,
                    album,
                },
                score,
            });
        }
    }

    // ---- Vinyl: collection and wantlist ---------------------------------
    for list in [VinylList::Collection, VinylList::Wantlist] {
        for rec in cat.list_vinyl(list)? {
            let combined = format!("{} {}", rec.artist, rec.title);
            let mut score = score_fields(
                &q,
                &[
                    (&rec.title, 3),
                    (&rec.artist, 2),
                    (&combined, 3),
                    (rec.label.as_deref().unwrap_or(""), 1),
                    (rec.catalog_number.as_deref().unwrap_or(""), 1),
                ],
            );
            // A song name won't appear in any of those fields, so consult the
            // cached tracklist: this is what answers "which record is this song
            // on?". Only cached details are read — the search box never makes a
            // network call, so a record whose detail was never fetched simply
            // matches on its release fields alone.
            let mut matched_track = None;
            if let Ok(Some(detail)) = cat.cached_release(&rec.release_id.to_string()) {
                for tr in &detail.tracklist {
                    let s = field_score(&norm_match(&tr.title), &q) * 3;
                    if s > 0 && s > score {
                        score = s;
                        matched_track = Some(tr.title.clone());
                    }
                }
            }
            if score > 0 {
                let sub = [
                    rec.year.map(|y| y.to_string()),
                    rec.format.clone(),
                    rec.label.clone(),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                out.push(ScoredHit {
                    hit: SearchHit::Vinyl {
                        list,
                        instance_id: rec.instance_id,
                        release_id: rec.release_id,
                        title: rec.title.clone(),
                        artist: rec.artist.clone(),
                        sub,
                        matched_track,
                    },
                    score,
                });
            }
        }
    }

    // Strongest first; ties broken by a stable label so the list doesn't
    // reshuffle between identical queries.
    out.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then_with(|| hit_sort_key(&a.hit).cmp(&hit_sort_key(&b.hit)))
    });
    out.truncate(limit);
    Ok(out)
}

/// Stable tiebreaker: the text the row displays.
fn hit_sort_key(hit: &SearchHit) -> String {
    match hit {
        SearchHit::Track { artist, title, .. } => format!("{artist} {title}"),
        SearchHit::Vinyl { artist, title, .. } => format!("{artist} {title}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discogs::{ReleaseDetail, ReleaseTrack};
    use crate::model::{AudioProperties, Format, Tags, VinylRecord};
    use crate::ScannedTrack;

    fn track(path: &str, artist: &str, title: &str, album: &str) -> ScannedTrack {
        ScannedTrack {
            source_path: path.into(),
            format: Format::Mp3,
            properties: AudioProperties {
                sample_rate_hz: 44100,
                bit_depth: None,
                channels: 2,
                duration_ms: 1000,
                bitrate_kbps: Some(320),
            },
            tags: Tags {
                artist: Some(artist.into()),
                title: Some(title.into()),
                album: Some(album.into()),
                ..Default::default()
            },
            cover_thumb: None,
            fingerprint: None,
            src_size: None,
            src_mtime: None,
        }
    }

    fn record(instance_id: u64, release_id: u64, artist: &str, title: &str) -> VinylRecord {
        VinylRecord {
            instance_id,
            release_id,
            title: title.into(),
            artist: artist.into(),
            year: Some(2013),
            label: Some("Sistrum".into()),
            catalog_number: Some("SIS-12".into()),
            format: Some("12\"".into()),
            thumb_url: None,
            cover_url: None,
            added: None,
            folder_id: None,
            has_cover: false,
            price: None,
            price_currency: None,
        }
    }

    fn titles(hits: &[ScoredHit]) -> Vec<String> {
        hits.iter()
            .map(|h| match &h.hit {
                SearchHit::Track { title, .. } => format!("track:{title}"),
                SearchHit::Vinyl { title, .. } => format!("vinyl:{title}"),
            })
            .collect()
    }

    #[test]
    fn blank_query_returns_nothing() {
        let cat = Catalog::open(":memory:").unwrap();
        assert!(search_library(&cat, "", 5).unwrap().is_empty());
        assert!(search_library(&cat, "   ", 5).unwrap().is_empty());
    }

    #[test]
    fn searches_digital_and_vinyl_together() {
        let cat = Catalog::open(":memory:").unwrap();
        cat.upsert_scanned(&track("/m/a.mp3", "Lawrence", "Glow", "Pampa Vol 1"))
            .unwrap();
        cat.upsert_vinyl(VinylList::Collection, &record(1, 100, "Lawrence", "Glow"))
            .unwrap();

        let hits = search_library(&cat, "lawrence glow", 5).unwrap();
        let t = titles(&hits);
        assert!(t.contains(&"track:Glow".to_string()), "{t:?}");
        assert!(t.contains(&"vinyl:Glow".to_string()), "{t:?}");
    }

    /// The headline requirement: typing more narrows toward a single answer.
    #[test]
    fn a_more_complete_query_narrows_to_one_confident_hit() {
        let cat = Catalog::open(":memory:").unwrap();
        cat.upsert_scanned(&track("/m/a.mp3", "Lawrence", "Glow", "Pampa"))
            .unwrap();
        cat.upsert_scanned(&track("/m/b.mp3", "Lawrence", "Miles", "Pampa"))
            .unwrap();
        cat.upsert_scanned(&track("/m/c.mp3", "Lawrence", "Rousing", "Pampa"))
            .unwrap();

        // The artist alone is ambiguous: several tracks tie.
        let broad = search_library(&cat, "lawrence", 5).unwrap();
        assert_eq!(broad.len(), 3);

        // Naming the song collapses it to the single answer: the SQL prefilter
        // requires every term, so the album-mates drop out entirely.
        let narrow = search_library(&cat, "lawrence glow", 5).unwrap();
        assert_eq!(narrow.len(), 1, "{:?}", titles(&narrow));
        match &narrow[0].hit {
            SearchHit::Track { title, .. } => assert_eq!(title, "Glow"),
            other => panic!("expected the track first, got {other:?}"),
        }

        // And where several rows *do* survive the prefilter, the best match
        // still leads: "pampa" matches all three albums, ranked above nothing
        // in particular, but adding the song name puts Glow decisively on top.
        let ranked = search_library(&cat, "glow", 5).unwrap();
        match &ranked[0].hit {
            SearchHit::Track { title, .. } => assert_eq!(title, "Glow"),
            other => panic!("expected Glow first, got {other:?}"),
        }
    }

    /// Searching a song name finds the *record* it's pressed on, via the cached
    /// tracklist — the "which vinyl has this song?" case.
    #[test]
    fn a_song_name_matches_a_record_through_its_cached_tracklist() {
        let cat = Catalog::open(":memory:").unwrap();
        cat.upsert_vinyl(VinylList::Collection, &record(1, 4460898, "XDB", "Frocks"))
            .unwrap();
        cat.cache_release(&ReleaseDetail {
            release_id: "4460898".into(),
            title: "Frocks".into(),
            year: None,
            released: None,
            country: None,
            genres: Vec::new(),
            styles: Vec::new(),
            label: None,
            catalog_number: None,
            artist_ids: Vec::new(),
            label_ids: Vec::new(),
            master_id: None,
            tracklist: vec![ReleaseTrack {
                position: "B2".into(),
                title: "Frocks (P.Scott Mix)".into(),
                duration: "6:12".into(),
            }],
            videos: Vec::new(),
        })
        .unwrap();

        // "p scott mix" appears nowhere in the record's own fields.
        let hits = search_library(&cat, "p scott mix", 5).unwrap();
        assert_eq!(hits.len(), 1, "{:?}", titles(&hits));
        match &hits[0].hit {
            SearchHit::Vinyl {
                title,
                matched_track,
                ..
            } => {
                assert_eq!(title, "Frocks");
                assert_eq!(matched_track.as_deref(), Some("Frocks (P.Scott Mix)"));
            }
            other => panic!("expected a vinyl hit, got {other:?}"),
        }
    }

    /// A record whose detail was never cached still matches on its own fields —
    /// the search box must never depend on a network fetch.
    #[test]
    fn an_uncached_record_still_matches_on_its_release_fields() {
        let cat = Catalog::open(":memory:").unwrap();
        cat.upsert_vinyl(VinylList::Wantlist, &record(7, 555, "Theo Parrish", "Sound Sculptures"))
            .unwrap();
        let hits = search_library(&cat, "theo parrish", 5).unwrap();
        assert_eq!(hits.len(), 1);
        match &hits[0].hit {
            SearchHit::Vinyl { list, matched_track, .. } => {
                assert_eq!(*list, VinylList::Wantlist);
                assert!(matched_track.is_none());
            }
            other => panic!("expected a vinyl hit, got {other:?}"),
        }
    }

    #[test]
    fn limit_caps_the_result_list() {
        let cat = Catalog::open(":memory:").unwrap();
        for i in 0..10 {
            cat.upsert_scanned(&track(
                &format!("/m/{i}.mp3"),
                "Lawrence",
                &format!("Track {i}"),
                "Pampa",
            ))
            .unwrap();
        }
        assert_eq!(search_library(&cat, "lawrence", 5).unwrap().len(), 5);
    }
}
