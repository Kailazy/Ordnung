//! Discogs API — fetch release artwork for catalog tracks that lack an
//! embedded cover image.
//!
//! Engine-shaped per `ordnung-architecture`: pure library, no UI, no policy,
//! no `println!`. The caller (GUI or CLI) supplies the token and decides which
//! tracks to enrich; the [`Client`] paces its own requests against the Discogs
//! rate limit (60 authenticated req/min) and retries on 429, because only the
//! client knows how many API calls a single track actually fires.
//!
//! Beyond artwork lookup, [`Client::fetch_release`] pulls a chosen release's
//! full detail (genres/styles, label, catalog number, year, country) so the
//! caller can fill in album-level tag fields the track is missing — see
//! [`ReleaseDetail::apply_to_tags`] and `docs/design/discogs-track-inspector.md`.

use crate::error::{Error, Result};
use crate::model::{SellerListing, Tags, VinylRecord};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const SEARCH_URL: &str = "https://api.discogs.com/database/search";
/// Artist and label browse endpoints; `/{id}/releases` is appended. These take
/// a Discogs entity id and list exactly that entity's records — see
/// [`Client::browse_by_id`].
const ARTISTS_URL: &str = "https://api.discogs.com/artists";
const LABELS_URL: &str = "https://api.discogs.com/labels";
/// Master (release-group) endpoint; `/{id}/versions` lists every pressing.
const MASTERS_URL: &str = "https://api.discogs.com/masters";
/// Per-release endpoint; `{id}` is appended for full release detail.
const RELEASE_URL: &str = "https://api.discogs.com/releases";
/// Identity endpoint — resolves the token owner's username so the collection
/// endpoints (which are keyed by username) can be addressed without asking the
/// user to type their handle.
const IDENTITY_URL: &str = "https://api.discogs.com/oauth/identity";
/// Discogs returns at most 100 collection items per page; we walk every page.
const COLLECTION_PER_PAGE: u32 = 100;
/// Discogs's built-in "Uncategorized" collection folder. Folder `0` ("All") is a
/// read-only view — adds must name a real folder — so a record added by Ordnung
/// lands here, exactly where the discogs.com "Add to collection" button puts it.
/// Also the fallback folder for deleting a cached copy whose folder wasn't
/// recorded (rows cached before [`VinylRecord::folder_id`] existed).
pub const UNCATEGORIZED_FOLDER: u32 = 1;
/// Max side of a cached vinyl cover PNG. Bigger than the table thumbnail
/// ([`THUMB_MAX_SIDE`]) because the "Vinyl Collection" grid renders large
/// album icons, but well under [`FULL_MAX_SIDE`] since these are display-only.
const VINYL_COVER_MAX_SIDE: u32 = 400;
/// Minimum spacing between Discogs *API* requests (search + release detail).
/// Discogs allows 60 authenticated requests/minute on a rolling window; ~1.1s
/// per request holds us at ~54/min with headroom. This is enforced per-request
/// inside [`Client`] — not per-track by the caller — because a single track can
/// fire up to four search calls (see [`Client::resolve_hits`]), so pacing tracks
/// undercounts and bursts straight through the limit. CDN image downloads are
/// exempt: they don't count against the API rate limit.
const MIN_API_INTERVAL: Duration = Duration::from_millis(1100);
/// How many times to retry an API request that comes back HTTP 429 before
/// giving up and surfacing the error to the caller.
const MAX_RETRIES: u32 = 3;
/// Max side of the GUI thumbnail PNG, matching `scan`'s embedded-thumb downscale.
const THUMB_MAX_SIDE: u32 = 96;
/// Max side of the full-resolution PNG we keep for embedding into source files
/// (`tag --write --art`). Generous enough to look crisp on a CDJ screen while
/// capping pathological cases; Discogs `cover_image`s are typically well under
/// this, so they pass through untouched (`thumbnail` only downscales).
const FULL_MAX_SIDE: u32 = 1400;

/// A successful artwork lookup — Discogs release the image came from, the
/// original image URL (for refresh / debugging), and two decoded PNGs ready to
/// drop into `Catalog::set_external_artwork`: a small `png_bytes` thumbnail for
/// GUI rendering and a `full_bytes` full-resolution image for tag embedding.
#[derive(Debug, Clone)]
pub struct ArtworkHit {
    pub release_id: String,
    pub thumb_url: String,
    pub png_bytes: Vec<u8>,
    pub full_bytes: Vec<u8>,
}

/// The cheapest copy of a release currently listed on the Discogs marketplace,
/// in whatever currency Discogs quoted it (the token owner's, when it has one).
/// A live market price, not a purchase price — see
/// [`Client::marketplace_price`].
#[derive(Debug, Clone, PartialEq)]
pub struct MarketPrice {
    pub value: f64,
    pub currency: String,
}

/// One Discogs release candidate: metadata + image URLs, with no bytes
/// downloaded yet. Powers the GUI multi-candidate picker so the user can choose
/// among many releases; the caller downloads images on demand via
/// [`Client::fetch_thumb`] / [`Client::fetch_full`].
#[derive(Debug, Clone)]
pub struct ReleaseCandidate {
    pub release_id: String,
    pub title: String,
    pub year: String,
    pub label: String,
    pub country: String,
    pub format: String,
    pub thumb_url: String,
    pub cover_image_url: String,
    /// How many Discogs users have this release in their collection. Comes free
    /// with every search hit, so callers can rank candidates by popularity
    /// without spending further API requests.
    pub in_collection: u32,
    /// How many Discogs users have this release on their wantlist.
    pub in_wantlist: u32,
}

/// One release returned by a free-text record lookup ([`Client::search_records`]).
///
/// Distinct from [`ReleaseCandidate`], which answers "which release is *this
/// track* from" and so carries only what the artwork picker needs. A lookup hit
/// is a record the user is considering on its own terms, so it splits artist
/// from title (Discogs returns them joined as `"Artist - Title"`) and carries
/// the catalog number — the two fields that disambiguate pressings at a glance.
#[derive(Debug, Clone)]
pub struct RecordHit {
    pub release_id: u64,
    /// Credited artist, split off the joined `"Artist - Title"` search label.
    /// Empty when Discogs gave a title with no ` - ` separator.
    pub artist: String,
    pub title: String,
    pub year: String,
    pub label: String,
    pub catno: String,
    pub country: String,
    /// Format summary as Discogs lists it, e.g. `2xLP, Album, Repress`.
    pub format: String,
    pub thumb_url: String,
    pub cover_image_url: String,
}

/// One artist returned by a free-text artist lookup ([`Client::search_artists`]).
///
/// The search box's Discogs mode answers "what exists?" with records; this is
/// the other thing a typed name can mean. An artist hit is a door rather than a
/// destination — it opens the artist's whole discography (browsed by id, see
/// [`Client::browse_by_id`]) instead of one record — so it carries only what a
/// row needs to be recognised: the name and a portrait.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtistHit {
    pub artist_id: u64,
    /// The name as Discogs lists it, disambiguator and all (`Lawrence (2)`).
    /// Kept verbatim because the number is what tells two artists apart; strip
    /// it for display only.
    pub name: String,
    pub thumb_url: String,
}

/// One page of free-text record-lookup results. See [`Client::search_records`].
#[derive(Debug, Clone)]
pub struct RecordSearchPage {
    pub hits: Vec<RecordHit>,
    /// Total pages Discogs reports for this query, so a caller can tell "that's
    /// everything" from "there's more".
    pub pages: u32,
    /// Total matching releases across all pages.
    pub items: u32,
}

/// One pressing of a master — a specific release, with how many people own and
/// want it. See [`Client::master_versions`].
#[derive(Debug, Clone)]
pub struct MasterVersion {
    pub release_id: u64,
    pub title: String,
    /// Format details for this pressing, e.g. `12", White Label, Limited Edition`.
    pub format: String,
    pub label: String,
    pub catno: String,
    pub country: String,
    /// Release date as Discogs lists it; often just a year.
    pub released: String,
    pub thumb_url: String,
    /// How many Discogs users hold this exact pressing. The best single signal
    /// for "the normal one" versus a promo or a limited variant.
    pub in_collection: u32,
    pub in_wantlist: u32,
}

/// Which association a browse follows — the two threads a crate dig can pull.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrowseThread {
    Artist,
    Label,
}

/// One page of browse results, with the shape of the whole result set so the
/// caller can page around it. See [`Client::browse_by_id`].
#[derive(Debug, Clone, Default)]
pub struct BrowsePage {
    /// Total pages available for this query, at least 1.
    pub pages: u32,
    /// Total releases across all pages, as Discogs counts them.
    pub items: u32,
    pub releases: Vec<BrowseRelease>,
}

/// One release from an artist or label browse. Leaner than
/// [`ReleaseCandidate`]: these endpoints return a listing, not a search hit.
#[derive(Debug, Clone)]
pub struct BrowseRelease {
    /// A concrete release id — masters are resolved to their `main_release`.
    pub release_id: u64,
    pub title: String,
    /// Credited artist. On a label browse this is the release's artist; on an
    /// artist browse it's the artist themself (possibly a collaboration string).
    pub artist: String,
    pub year: Option<u16>,
    /// Format summary (`12"`, `2xLP, Album`). Empty on master entries, which
    /// don't carry one — check [`BrowseRelease::format_known`] before reading
    /// this as "not a record".
    pub format: String,
    /// False when Discogs gave no format for this row (every master entry, plus
    /// the occasional bare release). An empty `format` then means "unknown",
    /// not "not vinyl" — resolve it with [`Client::release_format`] rather than
    /// discarding a row that may well be a 12".
    pub format_known: bool,
    /// Label name — only the label browse and release rows carry it.
    pub label: String,
    pub catno: String,
    pub thumb_url: String,
    /// False when the artist is credited as a remixer rather than the main
    /// artist, so a dig can prefer their own records.
    pub main: bool,
}

/// One page of a seller's marketplace inventory
/// (`GET /users/{username}/inventory`). Non-vinyl listings are already filtered
/// out of `listings`, so `items` (Discogs's total across every format) can
/// exceed what paging to the end will actually yield.
#[derive(Debug, Clone, Default)]
pub struct InventoryPage {
    /// Total pages available, at least 1.
    pub pages: u32,
    /// Total for-sale listings Discogs reports for this seller, all formats.
    pub items: u32,
    pub listings: Vec<SellerListing>,
}

/// Full detail for a single Discogs release (`GET /releases/{id}`), carrying the
/// album-level metadata the search endpoint omits. Used to fill in tag fields a
/// track is missing once the user has chosen which release it is.
///
/// `Serialize`/`Deserialize` back the `release_cache` table (see
/// [`Catalog::release_cached_or`](crate::catalog::Catalog::release_cached_or)) so a
/// release fetched once is never re-requested across runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseDetail {
    pub release_id: String,
    /// The release's own title (i.e. the album/EP name).
    pub title: String,
    pub year: Option<u16>,
    /// Full release date as Discogs lists it, e.g. "1995-09-01" or "1995".
    pub released: Option<String>,
    pub country: Option<String>,
    pub genres: Vec<String>,
    /// Discogs sub-genre taxonomy ("Deep House", "Detroit Techno") — the most
    /// DJ-useful field and preferred over `genres` when populating `genre`.
    pub styles: Vec<String>,
    pub label: Option<String>,
    pub catalog_number: Option<String>,
    /// Discogs artist ids credited on this release, in credit order. These name
    /// exactly one artist where a name doesn't — Discogs has four distinct
    /// artists called "Lawrence" — so anything walking the database by artist
    /// should use these. `#[serde(default)]` for `release_cache` rows written
    /// before the field existed; [`DETAIL_SCHEMA_VERSION`](crate::catalog::DETAIL_SCHEMA_VERSION)
    /// re-fetches those.
    #[serde(default)]
    pub artist_ids: Vec<u64>,
    /// Discogs label ids, in release order — the primary label first. Same
    /// reasoning as [`ReleaseDetail::artist_ids`]: "Dial" and "Dial Record" are
    /// different labels that a name match conflates.
    #[serde(default)]
    pub label_ids: Vec<u64>,
    /// The master (release group) this pressing belongs to; `None` when it
    /// stands alone. A promo or white label often has no copies for sale while
    /// another pressing of the same record has plenty — the master is how the
    /// buyable one is found. See [`Client::master_versions`].
    #[serde(default)]
    pub master_id: Option<u64>,
    /// The release's own track listing, in pressing order. Empty when Discogs
    /// lists none. `#[serde(default)]` so a `release_cache` row written before
    /// this field existed still deserializes (the cache's `detail_version`
    /// guard re-fetches it — see [`crate::catalog::DETAIL_SCHEMA_VERSION`]).
    #[serde(default)]
    pub tracklist: Vec<ReleaseTrack>,
    /// YouTube videos the Discogs community attached to this release — how a
    /// record with no digital copy in the library can still be listened to.
    #[serde(default)]
    pub videos: Vec<ReleaseVideo>,
}

/// One entry from a release's track listing.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseTrack {
    /// Side/position as pressed, e.g. `A1`. Empty on releases that don't list one.
    pub position: String,
    pub title: String,
    /// Duration as Discogs writes it (`5:18`), not a parsed count of seconds —
    /// it's display-only and frequently blank or malformed.
    pub duration: String,
    /// Who performed this track, when the track credits someone other than the
    /// release does. Set on compilations and split releases — where the
    /// release-level artist is "Various" and so names nobody — and `None` on a
    /// single-artist album, whose release credit already covers every track.
    ///
    /// Pre-joined for display ("A & B", "A Feat. B"), because Discogs's own
    /// connectors are the only thing that gets multi-artist credits right.
    /// `#[serde(default)]` so a `release_cache` row written before this field
    /// existed still deserializes; [`crate::catalog::DETAIL_SCHEMA_VERSION`]
    /// is what actually re-fetches those.
    #[serde(default)]
    pub artist: Option<String>,
}

/// A YouTube video attached to a Discogs release.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReleaseVideo {
    pub uri: String,
    pub title: String,
    pub duration_secs: Option<u32>,
    /// Discogs's own flag for whether the video may be embedded elsewhere.
    /// Informational: Ordnung plays videos on their YouTube watch page rather
    /// than through the embed player (see the GUI's `webview` module), so this
    /// doesn't gate playback.
    pub embeddable: bool,
}

impl ReleaseVideo {
    /// The YouTube video id from `uri`, for building an embed URL. `None` for
    /// the occasional non-YouTube link (Vimeo, dead shorteners) — Discogs
    /// accepts any URL here, so this can't be assumed.
    pub fn youtube_id(&self) -> Option<&str> {
        let u = self.uri.trim();
        let rest = u
            .strip_prefix("https://")
            .or_else(|| u.strip_prefix("http://"))
            .unwrap_or(u);
        let rest = rest.strip_prefix("www.").unwrap_or(rest);
        // The two forms Discogs stores: watch links and youtu.be shorteners.
        let id = if let Some(q) = rest.strip_prefix("youtube.com/watch?") {
            q.split('&').find_map(|p| p.strip_prefix("v="))?
        } else if let Some(tail) = rest.strip_prefix("youtu.be/") {
            tail.split(['?', '/']).next()?
        } else if let Some(tail) = rest.strip_prefix("youtube.com/embed/") {
            tail.split(['?', '/']).next()?
        } else {
            return None;
        };
        // Ids are fixed-alphabet; anything else is a mangled link we can't play.
        (!id.is_empty()
            && id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
        .then_some(id)
    }
}

/// Which album-level tag field a [`FieldFill`] targets. Kept as an enum (rather
/// than matching on display strings) so [`ReleaseDetail::proposed_fills`] and
/// [`ReleaseDetail::apply_to_tags`] can never drift out of sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FillField {
    Genre,
    Label,
    CatalogNumber,
    Country,
    Album,
    ReleaseDate,
    Year,
}

impl FillField {
    /// Human-readable label for the preview UI.
    pub fn label(self) -> &'static str {
        match self {
            FillField::Genre => "Genre",
            FillField::Label => "Label",
            FillField::CatalogNumber => "Catalog #",
            FillField::Country => "Country",
            FillField::Album => "Album",
            FillField::ReleaseDate => "Release date",
            FillField::Year => "Year",
        }
    }
}

/// One field this release would write into a track, with the value it would
/// write. Returned by [`ReleaseDetail::proposed_fills`] so the caller can show
/// the user exactly what data is about to be added before committing.
#[derive(Debug, Clone)]
pub struct FieldFill {
    pub field: FillField,
    pub value: String,
}

/// What [`ReleaseDetail::match_videos`] found: `tracks[i]` is the index into
/// `videos` playing tracklist position `i`, and `leftover` the videos no
/// track claimed, in release order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoMatches {
    pub tracks: Vec<Option<usize>>,
    pub leftover: Vec<usize>,
}

/// One reading of a video title, normalized. A `literal` reading is the
/// title as typed (or its tail after a separator), and settles first: a video
/// that plainly says `Confusion` beats one that says so only once `(Official
/// Music Video) [HD Upgrade]` is trimmed away. `exact_only` readings — the
/// inside of a quoted or parenthesized run — may equal a track title but never
/// claim one by prefix: "(Be My Love)" names the track, "(Miss Yetti Remix)"
/// merely starts with a word.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Cand {
    text: String,
    exact_only: bool,
    literal: bool,
    /// The pressing position this reading was peeled from, normalized —
    /// `a2` for `Valis 003` read out of `A2 Valis 003`. Only the track at
    /// that position may use it: six tracks all called `Valis 003` are told
    /// apart by nothing else.
    position: Option<String>,
}

impl Cand {
    fn literal(text: String) -> Self {
        Cand {
            text,
            exact_only: false,
            literal: true,
            position: None,
        }
    }
}

/// The matching rules, in the order they're settled (see
/// [`ReleaseDetail::match_videos`]).
#[derive(Clone, Copy)]
enum Rule {
    ExactLiteral,
    ExactDerived,
    Prefix,
    Fuzzy,
}

impl ReleaseDetail {
    /// Which video plays each track: one entry per `tracklist` position, holding
    /// an index into `videos` (or `None` when nothing on the release matches).
    /// [`match_videos`](Self::match_videos) without a release artist — see
    /// there for the rules.
    pub fn video_matches(&self) -> Vec<Option<usize>> {
        self.match_videos("").tracks
    }

    /// Pair this release's videos with its tracklist. `release_artist` is the
    /// name the release is credited to, which Discogs's own detail doesn't
    /// carry; it lets a video titled `Artist Title` (no separator at all) be
    /// read past the artist. Pass `""` when it isn't known.
    ///
    /// Discogs video titles are free text typed by whoever attached them —
    /// `Massive Attack - Safe From Harm`, `A1. Safe From Harm`, `Dntel
    /// "Snowshoe" (Greer 2018)`, `C3D-E – The Perfect Memory B1` — so a video
    /// is read every way it might name a track (see [`video_title_candidates`])
    /// and claimed, at most once, by the first rule that fits. Each rule is
    /// settled across the whole tracklist before the next, looser one runs:
    ///
    /// 1. the same title as typed, ignoring case, punctuation, diacritics, a
    ///    leading "The" and an "(Original Mix)" marker on either side;
    /// 2. a title naming the track's pressing position (`B1`) and no other;
    /// 3. the same title once the release's name, an artist, a position, a
    ///    bracketed catalogue number or upload filler is read past;
    /// 4. a title that *starts with* the track's (a differently-mixed video
    ///    beats none);
    /// 5. a title one or two typos away from the track's, when it's that close
    ///    to no other track on the release.
    ///
    /// Anything looser makes a short title like "Love" swallow the wrong
    /// video. Whatever no track claims (album rips, live sets, interviews)
    /// comes back in `leftover`.
    pub fn match_videos(&self, release_artist: &str) -> VideoMatches {
        let cx = TitleContext::new(self, release_artist);
        let candidates: Vec<Vec<Cand>> = self
            .videos
            .iter()
            .map(|v| video_title_candidates(&v.title, &cx))
            .collect();
        let tracks = self.claim_by_title(&candidates, true);
        let claimed: std::collections::HashSet<usize> = tracks.iter().flatten().copied().collect();
        let leftover = (0..self.videos.len())
            .filter(|i| !claimed.contains(i))
            .collect();
        VideoMatches { tracks, leftover }
    }

    /// Which of `titles` (the track titles of local files linked to this
    /// release) plays each tracklist position. Same conservative matching as
    /// [`match_videos`](Self::match_videos), minus the positional fallback —
    /// a file named `A1` is a filename convention, not a title.
    pub fn file_matches(&self, titles: &[String]) -> Vec<Option<usize>> {
        let candidates: Vec<Vec<Cand>> = titles
            .iter()
            .map(|t| title_forms(t).into_iter().map(Cand::literal).collect())
            .collect();
        self.claim_by_title(&candidates, false)
    }

    /// Assign at most one candidate to each tracklist entry. `candidates[i]` is
    /// the set of normalized forms item `i` may match under; the first item that
    /// matches a track claims it and is not offered to later tracks.
    ///
    /// Each rule is settled across the *whole* tracklist before the next,
    /// looser one runs — run per track instead, an early "Dreamuniverse" would
    /// claim the file for "Dreamuniverse Pt.II" by prefix before that later
    /// track ever got to match it exactly.
    fn claim_by_title(&self, candidates: &[Vec<Cand>], allow_position: bool) -> Vec<Option<usize>> {
        let wants: Vec<Vec<String>> = self
            .tracklist
            .iter()
            .map(|t| title_forms(&t.title))
            .collect();
        let keys: Vec<Vec<String>> = wants
            .iter()
            .map(|ws| ws.iter().map(|w| title_key(w)).collect())
            .collect();
        let mut used = vec![false; candidates.len()];
        let mut out: Vec<Option<usize>> = vec![None; self.tracklist.len()];
        for rule in [
            Rule::ExactLiteral,
            Rule::ExactDerived,
            Rule::Prefix,
            Rule::Fuzzy,
        ] {
            for (ti, t) in self.tracklist.iter().enumerate() {
                if out[ti].is_some() || wants[ti].is_empty() {
                    continue;
                }
                let pos = norm_loose(&t.position);
                // A position match needs the position to be a real side/track
                // marker (`a1`, `aa`), not a bare digit that would collide with
                // any number in a video title.
                let pos_marker = allow_position
                    && pos.len() >= 2
                    && pos.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                    && !pos.contains(' ');
                let same = |c: &Cand| keys[ti].iter().any(|k| title_key(&c.text) == *k);
                // A reading peeled from a position belongs to that position.
                let placed = |c: &Cand| {
                    c.position
                        .as_ref()
                        .is_none_or(|p| pos.is_empty() || same_position(p, &pos))
                };
                let fits = |c: &Cand| match rule {
                    Rule::ExactLiteral => c.literal && same(c),
                    Rule::ExactDerived => placed(c) && same(c),
                    Rule::Prefix => {
                        let by_title = !c.exact_only
                            && placed(c)
                            && wants[ti]
                                .iter()
                                .any(|w| dethe(&c.text).starts_with(&format!("{} ", dethe(w))));
                        by_title || (pos_marker && names_position(&c.text, &pos))
                    }
                    Rule::Fuzzy => {
                        if !placed(c) {
                            return false;
                        }
                        let ck = title_key(&c.text);
                        let near = |k: &String| levenshtein(&ck, k) <= typo_budget(k);
                        // Close to this track only: "Untitled 3" is one typo
                        // from "Untitled 2" as well, and must not settle there.
                        keys[ti].iter().any(near)
                            && !keys
                                .iter()
                                .enumerate()
                                .any(|(o, ks)| o != ti && ks.iter().any(near))
                    }
                };
                let hit = candidates
                    .iter()
                    .enumerate()
                    .position(|(i, cands)| !used[i] && cands.iter().any(fits));
                if let Some(i) = hit {
                    out[ti] = Some(i);
                    used[i] = true;
                }
            }
        }
        out
    }

    /// The videos no track claimed, as `(index, video)` pairs in release order.
    /// These are the full-album rips, live sets and interviews Discogs carries
    /// alongside the per-track links. [`match_videos`](Self::match_videos)
    /// without a release artist.
    pub fn unmatched_videos(&self) -> Vec<(usize, &ReleaseVideo)> {
        self.match_videos("")
            .leftover
            .into_iter()
            .map(|i| (i, &self.videos[i]))
            .collect()
    }

    /// Every genre tag on this release, for display and filtering — see
    /// [`genre_tags`].
    pub fn genre_tags(&self) -> Vec<String> {
        genre_tags(&self.genres, &self.styles)
    }

    /// The album-level fields this release *would* write onto `tags`, with their
    /// values. This is the single source of truth for both the preview UI and
    /// [`apply_to_tags`].
    ///
    /// When `overwrite` is false (the default), only fields currently empty on
    /// the track are proposed. When true, every field this release has a value
    /// for is proposed *except* those already equal to it — so the preview and
    /// the write never list a no-op change.
    ///
    /// Scope is deliberately album-level (`genre`, `label`, `catalog_number`,
    /// `year`, `release_country`, `album`, `release_date`): these are
    /// unambiguous once the release is chosen. Track-level fields (track number,
    /// canonical title) need tracklist-position matching and are out of scope.
    pub fn proposed_fills(&self, tags: &Tags, overwrite: bool) -> Vec<FieldFill> {
        let mut out = Vec::new();
        // Prefer the finer Discogs styles; fall back to coarse genres.
        let genre = if self.styles.is_empty() {
            self.genres.join(", ")
        } else {
            self.styles.join(", ")
        };
        push_fill(&mut out, FillField::Genre, &tags.genre, overwrite, genre);
        push_fill(
            &mut out,
            FillField::Label,
            &tags.label,
            overwrite,
            self.label.clone().unwrap_or_default(),
        );
        push_fill(
            &mut out,
            FillField::CatalogNumber,
            &tags.catalog_number,
            overwrite,
            self.catalog_number.clone().unwrap_or_default(),
        );
        push_fill(
            &mut out,
            FillField::Country,
            &tags.release_country,
            overwrite,
            self.country.clone().unwrap_or_default(),
        );
        push_fill(
            &mut out,
            FillField::Album,
            &tags.album,
            overwrite,
            self.title.clone(),
        );
        push_fill(
            &mut out,
            FillField::ReleaseDate,
            &tags.release_date,
            overwrite,
            self.released.clone().unwrap_or_default(),
        );
        if let Some(y) = self.year {
            // Write when empty, or (overwrite) when it differs from the current year.
            let write = if overwrite {
                tags.year != Some(y)
            } else {
                tags.year.is_none()
            };
            if write {
                out.push(FieldFill {
                    field: FillField::Year,
                    value: y.to_string(),
                });
            }
        }
        out
    }

    /// Write this release's album-level fields onto `tags`. With `overwrite =
    /// false` only empty fields are filled (non-destructive); with `true`,
    /// existing values are replaced too. Returns how many fields were written —
    /// exactly the set [`proposed_fills`] reports for the same `overwrite` flag.
    pub fn apply_to_tags(&self, tags: &mut Tags, overwrite: bool) -> usize {
        let fills = self.proposed_fills(tags, overwrite);
        for f in &fills {
            match f.field {
                FillField::Genre => tags.genre = Some(f.value.clone()),
                FillField::Label => tags.label = Some(f.value.clone()),
                FillField::CatalogNumber => tags.catalog_number = Some(f.value.clone()),
                FillField::Country => tags.release_country = Some(f.value.clone()),
                FillField::Album => tags.album = Some(f.value.clone()),
                FillField::ReleaseDate => tags.release_date = Some(f.value.clone()),
                FillField::Year => tags.year = f.value.parse().ok(),
            }
        }
        fills.len()
    }
}

/// True when an optional tag field is absent or only whitespace.
fn is_empty(slot: &Option<String>) -> bool {
    slot.as_deref().map(str::trim).is_none_or(str::is_empty)
}

/// Record that `field` would be written with `value`, gated on the release
/// actually having a value and on the write being meaningful: when `overwrite`
/// is false, only into an empty slot; when true, into any slot whose trimmed
/// value differs (so an identical value is never reported as a change).
fn push_fill(
    out: &mut Vec<FieldFill>,
    field: FillField,
    slot: &Option<String>,
    overwrite: bool,
    value: String,
) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    let write = if overwrite {
        slot.as_deref().map(str::trim) != Some(value)
    } else {
        is_empty(slot)
    };
    if write {
        out.push(FieldFill {
            field,
            value: value.to_string(),
        });
    }
}

/// Thin wrapper around `ureq::Agent` carrying the Discogs token + User-Agent.
/// Cheap to clone (`ureq::Agent` is `Arc` inside) so it can be moved into
/// background workers.
#[derive(Clone)]
pub struct Client {
    token: String,
    user_agent: String,
    agent: ureq::Agent,
}

/// Timestamp of the last API request, process-wide.
///
/// Discogs rate-limits the *token*, so the clock has to be global to the
/// process rather than owned by a `Client`: callers construct a fresh client
/// per worker thread rather than cloning one, so a per-instance clock gives
/// every concurrent worker its own full allowance and they burst straight
/// through the limit together. That stayed latent while only one worker talked
/// to Discogs at a time, and became a reliable 429 as soon as a second
/// concurrent caller existed.
///
/// `None` until the first request.
static LAST_REQUEST: Mutex<Option<Instant>> = Mutex::new(None);

impl Client {
    /// `token` is a Discogs personal access token (https://www.discogs.com/settings/developers).
    /// `user_agent` must be set — Discogs rejects requests with a default
    /// `ureq` UA. Use something like `"Ordnung/0.1 +https://example.com"`.
    pub fn new(token: impl Into<String>, user_agent: impl Into<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(10))
            .timeout_read(Duration::from_secs(20))
            .build();
        Client {
            token: token.into(),
            user_agent: user_agent.into(),
            agent,
        }
    }

    /// Block until at least [`MIN_API_INTERVAL`] has elapsed since the previous
    /// API request *anywhere in the process*, then stamp "now". Holding the
    /// lock across the sleep is intentional: it serializes concurrent workers
    /// so they share one pace rather than each racing to the limit
    /// independently. See [`LAST_REQUEST`] for why the clock is global.
    fn throttle(&self) {
        let mut last = LAST_REQUEST.lock().expect("discogs throttle lock");
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_API_INTERVAL {
                std::thread::sleep(MIN_API_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    /// Run an API request, throttling before each attempt and retrying the
    /// failures that are worth retrying. `build` is called fresh per attempt (a
    /// `ureq::Request` is consumed by `.call()`, so it can't be reused).
    ///
    /// Three retryable classes, all sharing one widening backoff and the same
    /// [`MAX_RETRIES`] budget:
    ///
    /// * **429** — rate limited. Wait out the server's `Retry-After` when it
    ///   sends one, else back off.
    /// * **5xx** — Discogs having a bad moment (502/503 during a deploy is the
    ///   common one). The request was well-formed, so the same request a few
    ///   seconds later usually succeeds.
    /// * **transport** — a dropped connection or a read timeout.
    ///
    /// Retrying these matters most in a long library-wide sweep: without it a
    /// single blip permanently drops that track from the run, and the user has
    /// no way to tell a real no-match from a transient failure. A 4xx other
    /// than 429 is *not* retried — a malformed query or a bad token fails the
    /// same way however many times we ask.
    fn call_with_retry<F>(&self, build: F) -> Result<ureq::Response>
    where
        F: Fn() -> ureq::Request,
    {
        let mut attempt = 0;
        loop {
            self.throttle();
            let backoff = || Duration::from_secs(2 * (attempt as u64 + 1));
            match build().call() {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(429, resp)) if attempt < MAX_RETRIES => {
                    let wait = retry_after(&resp).unwrap_or_else(backoff);
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                // 5xx: server-side and usually momentary. Honour `Retry-After`
                // here too — 503 is the other status Discogs sends it with.
                Err(ureq::Error::Status(code, resp))
                    if (500..600).contains(&code) && attempt < MAX_RETRIES =>
                {
                    let wait = retry_after(&resp).unwrap_or_else(backoff);
                    std::thread::sleep(wait);
                    attempt += 1;
                }
                // Connection dropped / timed out before any status came back.
                Err(ureq::Error::Transport(_)) if attempt < MAX_RETRIES => {
                    std::thread::sleep(backoff());
                    attempt += 1;
                }
                Err(e) => return Err(map_ureq_err(e)),
            }
        }
    }

    /// Search Discogs for a release matching this track and return the best
    /// thumbnail we can find. `Ok(None)` means "searched and nothing matched
    /// or no result had artwork" — that's a normal outcome, not an error.
    ///
    /// Strategy (see [`Client::resolve_hits`] for the full fallback chain):
    /// album search takes priority over track search, and each search tries the
    /// structured `artist` filter first then a hyphen-safe free-text `q` retry.
    /// We ask Discogs to return releases (not masters / artists) and take the
    /// first hit that has a non-empty `thumb` URL.
    ///
    /// For the multi-candidate picker that lets the user choose among releases,
    /// see [`Client::find_artwork_candidates`] below; this method keeps the
    /// "best single hit" behaviour for callers that just want one cover.
    pub fn find_artwork(
        &self,
        artist: &str,
        title: Option<&str>,
        album: Option<&str>,
    ) -> Result<Option<ArtworkHit>> {
        let artist = artist.trim();
        if artist.is_empty() {
            return Ok(None);
        }

        let hits = self.resolve_hits(artist, title, album)?;

        for hit in hits {
            if hit.thumb.is_empty() {
                continue;
            }
            let thumb_src = match self.download(&hit.thumb) {
                Ok(b) => b,
                // Discogs CDN occasionally 404s a thumb URL — try the next hit.
                Err(_) => continue,
            };
            let Some(thumb_png) = downscale_png(&thumb_src, THUMB_MAX_SIDE) else {
                continue;
            };
            // Full-resolution image for embedding. Prefer the larger
            // `cover_image`; fall back to the thumb source if it's missing or
            // fails to download/decode, so we always have *something* to embed.
            let full_src = if hit.cover_image.is_empty() {
                None
            } else {
                self.download(&hit.cover_image).ok()
            };
            let full_png = full_src
                .as_deref()
                .and_then(|b| downscale_png(b, FULL_MAX_SIDE))
                .or_else(|| downscale_png(&thumb_src, FULL_MAX_SIDE))
                .unwrap_or_else(|| thumb_png.clone());
            return Ok(Some(ArtworkHit {
                release_id: hit.id.to_string(),
                thumb_url: hit.thumb,
                png_bytes: thumb_png,
                full_bytes: full_png,
            }));
        }
        Ok(None)
    }

    /// Free-text record lookup: search all of Discogs for releases matching a
    /// user-typed query, the way the discogs.com search box does.
    ///
    /// This is the general search [`Client::find_artwork`] and
    /// [`Client::find_artwork_candidates`] are not — those anchor on a known
    /// artist to identify *one track's* release and return nothing without one.
    /// Here the query is whatever the user typed ("metro area", "environ 006",
    /// "theo parrish"), so it goes straight to `q` with no fallback ladder.
    ///
    /// Results are releases only (never masters or artists), so every hit has a
    /// concrete `release_id` that can be wanted, collected, or opened. `page` is
    /// 1-based. One API request per call, paced by the shared throttle.
    ///
    /// An empty or whitespace-only query returns an empty page without touching
    /// the network — a caller debouncing keystrokes shouldn't spend a request on
    /// a cleared search box.
    pub fn search_records(
        &self,
        query: &str,
        page: u32,
        per_page: u32,
    ) -> Result<RecordSearchPage> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(RecordSearchPage {
                hits: Vec::new(),
                pages: 0,
                items: 0,
            });
        }
        let page = page.max(1).to_string();
        let per_page = per_page.clamp(1, 100).to_string();
        let resp = self.call_with_retry(|| {
            self.agent
                .get(SEARCH_URL)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
                .query("q", query)
                .query("type", "release")
                .query("per_page", &per_page)
                .query("page", &page)
        })?;
        let body: SearchResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs search response: {e}")))?;
        Ok(RecordSearchPage {
            pages: body.pagination.pages,
            items: body.pagination.items,
            hits: body
                .results
                .into_iter()
                .map(|h| {
                    let (artist, title) = split_artist_title(&h.title);
                    RecordHit {
                        release_id: h.id,
                        artist,
                        title,
                        year: h.year,
                        label: h.label.into_iter().next().unwrap_or_default(),
                        catno: h.catno,
                        country: h.country,
                        format: h.format.join(", "),
                        thumb_url: h.thumb,
                        cover_image_url: h.cover_image,
                    }
                })
                .collect(),
        })
    }

    /// Free-text lookup of **artists** by name, for the search box's Discogs
    /// mode: the rows that let a typed name open a discography rather than
    /// one record.
    ///
    /// Same endpoint as [`Client::search_records`] with `type=artist`, so it
    /// matches as loosely — `Lawrence` returns every Lawrence Discogs knows,
    /// each with its own id, and the caller shows them side by side for the
    /// user to tell apart. Rows without a name are dropped. One API request
    /// per call, paced by the shared throttle; an empty query returns an empty
    /// list without touching the network.
    pub fn search_artists(&self, query: &str, per_page: u32) -> Result<Vec<ArtistHit>> {
        let query = query.trim();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let per_page = per_page.clamp(1, 100).to_string();
        let resp = self.call_with_retry(|| {
            self.agent
                .get(SEARCH_URL)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
                .query("q", query)
                .query("type", "artist")
                .query("per_page", &per_page)
                .query("page", "1")
        })?;
        let body: SearchResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs artist search: {e}")))?;
        Ok(artist_hits(body.results))
    }

    /// Like [`Client::find_artwork`] but returns *every* candidate release
    /// (up to ~10) with metadata and image URLs, leaving image downloads to the
    /// caller. Search strategy mirrors `find_artwork` (album first, then track
    /// title). Candidates without a thumbnail URL are dropped.
    pub fn find_artwork_candidates(
        &self,
        artist: &str,
        title: Option<&str>,
        album: Option<&str>,
    ) -> Result<Vec<ReleaseCandidate>> {
        let artist = artist.trim();
        if artist.is_empty() {
            return Ok(Vec::new());
        }
        let hits = self.resolve_hits(artist, title, album)?;
        Ok(hits
            .into_iter()
            .filter(|h| !h.thumb.is_empty())
            .map(|h| ReleaseCandidate {
                release_id: h.id.to_string(),
                title: h.title,
                year: h.year,
                label: h.label.into_iter().next().unwrap_or_default(),
                country: h.country,
                format: h.format.join(", "),
                thumb_url: h.thumb,
                cover_image_url: h.cover_image,
                in_collection: h.community.have,
                in_wantlist: h.community.want,
            })
            .collect())
    }

    /// Download + downscale a thumbnail URL into a small PNG for GUI preview.
    /// `None` on any network/decode failure.
    pub fn fetch_thumb(&self, url: &str) -> Option<Vec<u8>> {
        let bytes = self.download(url).ok()?;
        downscale_png(&bytes, THUMB_MAX_SIDE)
    }

    /// Download + downscale a full-resolution image URL into a PNG for tag
    /// embedding. `None` on any network/decode failure.
    pub fn fetch_full(&self, url: &str) -> Option<Vec<u8>> {
        let bytes = self.download(url).ok()?;
        downscale_png(&bytes, FULL_MAX_SIDE)
    }

    /// Fetch a single release's full detail (`GET /releases/{id}`) so the caller
    /// can fill in album-level tag fields via [`ReleaseDetail::apply_to_tags`].
    /// One authenticated request — pace alongside the search rate limit.
    pub fn fetch_release(&self, release_id: &str) -> Result<ReleaseDetail> {
        let url = format!("{RELEASE_URL}/{release_id}");
        let resp = self.call_with_retry(|| {
            self.agent
                .get(&url)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
        })?;
        let body: ReleaseResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs release response: {e}")))?;
        Ok(body.into_detail())
    }

    /// The format summary of one release (`Vinyl, 12", 33 ⅓ RPM`), for deciding
    /// whether a browse row that carried no format is actually a record. One
    /// API request; callers should only reach for it on rows where
    /// [`BrowseRelease::format_known`] is false.
    pub fn release_format(&self, release_id: u64) -> Result<String> {
        let url = format!("{RELEASE_URL}/{release_id}");
        let resp = self.call_with_retry(|| {
            self.agent
                .get(&url)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
        })?;
        let body: ReleaseResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs release response: {e}")))?;
        Ok(body.format_summary())
    }

    /// Resolve the token owner's Discogs username (`GET /oauth/identity`). One
    /// authenticated request — the collection endpoints are keyed by username, so
    /// this is the first call [`Client::fetch_collection`] makes.
    pub fn identity(&self) -> Result<String> {
        let resp = self.call_with_retry(|| {
            self.agent
                .get(IDENTITY_URL)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
        })?;
        let body: IdentityResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs identity response: {e}")))?;
        if body.username.trim().is_empty() {
            return Err(Error::Network(
                "Discogs identity returned no username".into(),
            ));
        }
        Ok(body.username)
    }

    /// Fetch the token owner's entire vinyl collection (Discogs folder 0 = "All"),
    /// walking every page and keeping only items pressed on vinyl. Returns the
    /// records as metadata only — cover images are downloaded separately by the
    /// caller via [`Client::fetch_cover`] so a refresh can skip covers it already
    /// has. Each page is one authenticated request, paced by the shared throttle.
    pub fn fetch_collection(&self) -> Result<Vec<VinylRecord>> {
        let username = self.identity()?;
        self.fetch_collection_for(&username)
    }

    /// Fetch the vinyl collection for a known username, skipping the identity
    /// lookup. Use when the caller already resolved the username (e.g. to report
    /// it back to the UI) and doesn't want to spend a second API request on it.
    pub fn fetch_collection_for(&self, username: &str) -> Result<Vec<VinylRecord>> {
        let base =
            format!("https://api.discogs.com/users/{username}/collection/folders/0/releases");
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let per_page = COLLECTION_PER_PAGE.to_string();
            let resp = self.call_with_retry(|| {
                self.agent
                    .get(&base)
                    .set("User-Agent", &self.user_agent)
                    .set("Authorization", &format!("Discogs token={}", self.token))
                    .query("page", &page_str)
                    .query("per_page", &per_page)
                    .query("sort", "added")
                    .query("sort_order", "desc")
            })?;
            let body: CollectionResponse = resp.into_json().map_err(|e| {
                Error::Network(format!("decoding Discogs collection response: {e}"))
            })?;
            for item in body.releases {
                if let Some(rec) = item.into_record() {
                    out.push(rec);
                }
            }
            if page >= body.pagination.pages.max(1) {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// Fetch the token owner's wantlist (`GET /users/{u}/wants`), keeping only
    /// vinyl pressings — the same filter the collection fetch applies, since both
    /// feed the records-only vinyl view. Wantlist items have no per-copy instance
    /// id, so each record's `instance_id` mirrors its `release_id`. Paging and
    /// pacing match [`Client::fetch_collection_for`].
    pub fn fetch_wantlist_for(&self, username: &str) -> Result<Vec<VinylRecord>> {
        let base = format!("https://api.discogs.com/users/{username}/wants");
        let mut out = Vec::new();
        let mut page = 1u32;
        loop {
            let page_str = page.to_string();
            let per_page = COLLECTION_PER_PAGE.to_string();
            let resp = self.call_with_retry(|| {
                self.agent
                    .get(&base)
                    .set("User-Agent", &self.user_agent)
                    .set("Authorization", &format!("Discogs token={}", self.token))
                    .query("page", &page_str)
                    .query("per_page", &per_page)
                    .query("sort", "added")
                    .query("sort_order", "desc")
            })?;
            let body: WantlistResponse = resp
                .into_json()
                .map_err(|e| Error::Network(format!("decoding Discogs wantlist response: {e}")))?;
            for item in body.wants {
                if let Some(rec) = item.into_record() {
                    out.push(rec);
                }
            }
            if page >= body.pagination.pages.max(1) {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    // --- Writes -------------------------------------------------------------
    //
    // Everything above reads. The four methods below are the only calls that
    // change the user's Discogs account, and each maps to exactly one explicit
    // user action in the front-end — nothing here runs as a side effect of a
    // sync. Each returns the metadata the caller needs to update its local cache
    // without re-fetching the whole list.

    /// Add `release_id` to `username`'s wantlist (`PUT /users/{u}/wants/{id}`).
    /// Discogs treats this as idempotent: re-adding a release already wanted
    /// succeeds and simply returns the existing want.
    ///
    /// Returns the created want as a [`VinylRecord`], or `Ok(None)` when the
    /// release isn't a vinyl pressing — the want *was* added to Discogs either
    /// way, but a CD/digital release has no place in the records-only vinyl
    /// view, so the caller must not cache it (and should say so).
    pub fn add_to_wantlist(&self, username: &str, release_id: u64) -> Result<Option<VinylRecord>> {
        let url = format!("https://api.discogs.com/users/{username}/wants/{release_id}");
        let resp = self.call_with_retry(|| self.authed(self.agent.put(&url)))?;
        let item: WantItem = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs wantlist add response: {e}")))?;
        Ok(item.into_record())
    }

    /// Drop `release_id` from `username`'s wantlist
    /// (`DELETE /users/{u}/wants/{id}`). Wants aren't foldered, so the release id
    /// alone addresses the item.
    pub fn remove_from_wantlist(&self, username: &str, release_id: u64) -> Result<()> {
        let url = format!("https://api.discogs.com/users/{username}/wants/{release_id}");
        self.call_with_retry(|| self.authed(self.agent.delete(&url)))?;
        Ok(())
    }

    /// Add `release_id` to `username`'s collection, in the folder that
    /// discogs.com's own "Add to collection" button uses
    /// ([`UNCATEGORIZED_FOLDER`]). Returns the new copy's `instance_id`, which
    /// the caller needs both to key the local cache row and to remove the copy
    /// later. Unlike the wantlist this is *not* idempotent — Discogs happily
    /// records a second copy of a release you already own, so callers should
    /// only offer this for releases not already in the collection.
    pub fn add_to_collection(&self, username: &str, release_id: u64) -> Result<u64> {
        let url = format!(
            "https://api.discogs.com/users/{username}/collection/folders/\
             {UNCATEGORIZED_FOLDER}/releases/{release_id}"
        );
        let resp = self.call_with_retry(|| self.authed(self.agent.post(&url)))?;
        let added: CollectionAdd = resp.into_json().map_err(|e| {
            Error::Network(format!("decoding Discogs collection add response: {e}"))
        })?;
        if added.instance_id == 0 {
            return Err(Error::Network(
                "Discogs accepted the collection add but returned no instance id".into(),
            ));
        }
        Ok(added.instance_id)
    }

    /// Every pressing of one master (`GET /masters/{id}/versions`) — the
    /// original, the repress, the promo, the coloured vinyl.
    ///
    /// This is what makes a dead-end pressing buyable: a promo white label
    /// routinely has nothing for sale while the standard pressing of the same
    /// record has a dozen copies listed. The caller compares the versions and
    /// points the user at one that can actually be bought.
    ///
    /// Ordered by how many people own each pressing, most first — the canonical
    /// pressing is the one most collections hold, and it's also the one most
    /// likely to be for sale. One API request.
    pub fn master_versions(&self, master_id: u64) -> Result<Vec<MasterVersion>> {
        let url = format!("{MASTERS_URL}/{master_id}/versions");
        let resp = self.call_with_retry(|| {
            self.agent
                .get(&url)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
                .query("per_page", "100")
        })?;
        let body: VersionsResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs versions response: {e}")))?;
        let mut out: Vec<MasterVersion> = body
            .versions
            .into_iter()
            .filter(|v| {
                // Records only, matching the rest of the vinyl view.
                v.major_formats
                    .iter()
                    .any(|f| f.eq_ignore_ascii_case("Vinyl"))
            })
            .map(|v| MasterVersion {
                release_id: v.id,
                title: v.title,
                format: v.format,
                label: v.label,
                catno: v.catno,
                country: v.country,
                released: v.released,
                thumb_url: v.thumb,
                in_collection: v.stats.community.in_collection,
                in_wantlist: v.stats.community.in_wantlist,
            })
            .collect();
        out.sort_by(|a, b| b.in_collection.cmp(&a.in_collection));
        Ok(out)
    }

    /// Build the cache row for a release just added to the collection
    /// (`GET /releases/{id}`), since [`Client::add_to_collection`] answers with
    /// an instance id and nothing else. `Ok(None)` when the release isn't a
    /// vinyl pressing — it's in the user's Discogs collection either way, but
    /// the records-only view has nowhere to put it, exactly as
    /// [`Client::add_to_wantlist`] reports.
    ///
    /// The copy is brand new, so it's in the folder the add targeted
    /// ([`UNCATEGORIZED_FOLDER`]) and its `added` date is left for the next sync
    /// to fill from Discogs itself.
    pub fn collection_record(
        &self,
        _username: &str,
        release_id: u64,
        instance_id: u64,
    ) -> Result<Option<VinylRecord>> {
        let url = format!("{RELEASE_URL}/{release_id}");
        let resp = self.call_with_retry(|| {
            self.agent
                .get(&url)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
        })?;
        let body: ReleaseResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs release response: {e}")))?;
        Ok(body.into_vinyl_record(instance_id, Some(UNCATEGORIZED_FOLDER)))
    }

    /// Which collection folder holds a given copy
    /// (`GET /users/{u}/collection/releases/{r}`, which lists every instance of
    /// one release with its folder). `Ok(None)` means Discogs doesn't have that
    /// instance — the copy is already gone.
    ///
    /// Only needed to repair a cache row that predates
    /// [`VinylRecord::folder_id`]: a removal reaches for this rather than
    /// guessing a folder and 404ing on anyone who files records into their own
    /// folders. One extra request, and only in that case.
    pub fn collection_folder_of(
        &self,
        username: &str,
        release_id: u64,
        instance_id: u64,
    ) -> Result<Option<u32>> {
        let url =
            format!("https://api.discogs.com/users/{username}/collection/releases/{release_id}");
        let resp = self.call_with_retry(|| self.authed(self.agent.get(&url)))?;
        let body: CollectionResponse = resp.into_json().map_err(|e| {
            Error::Network(format!("decoding Discogs collection lookup response: {e}"))
        })?;
        Ok(body
            .releases
            .iter()
            .find(|item| item.instance_id == instance_id)
            .map(|item| item.folder_id))
    }

    /// Remove one copy from `username`'s collection
    /// (`DELETE /users/{u}/collection/folders/{f}/releases/{r}/instances/{i}`).
    /// The copy is addressed through the folder that holds it, so pass the
    /// record's own [`VinylRecord::folder_id`]; `None` falls back to
    /// [`UNCATEGORIZED_FOLDER`]. This drops that copy's collection metadata
    /// (date added, rating, notes) on Discogs and cannot be undone from here.
    pub fn remove_from_collection(
        &self,
        username: &str,
        folder_id: Option<u32>,
        release_id: u64,
        instance_id: u64,
    ) -> Result<()> {
        let folder = folder_id.unwrap_or(UNCATEGORIZED_FOLDER);
        let url = format!(
            "https://api.discogs.com/users/{username}/collection/folders/\
             {folder}/releases/{release_id}/instances/{instance_id}"
        );
        self.call_with_retry(|| self.authed(self.agent.delete(&url)))?;
        Ok(())
    }

    /// Current lowest marketplace listing for one release
    /// (`GET /marketplace/stats/{id}`). This is what a copy is going for right
    /// now, not what the user paid — Discogs doesn't expose a purchase price on
    /// collection items, so this is the price the vinyl view can sort by.
    ///
    /// `Ok(None)` is a normal outcome: nothing for sale, or the release is
    /// blocked from sale. One authenticated request per release, paced by the
    /// shared throttle, so callers should fetch these in the background and
    /// cache what comes back.
    pub fn marketplace_price(&self, release_id: u64) -> Result<Option<MarketPrice>> {
        let url = format!("https://api.discogs.com/marketplace/stats/{release_id}");
        let resp = self.call_with_retry(|| self.authed(self.agent.get(&url)))?;
        let body: MarketplaceStats = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs marketplace stats: {e}")))?;
        if body.blocked_from_sale {
            return Ok(None);
        }
        Ok(body.lowest_price.and_then(|p| {
            (p.value > 0.0).then(|| MarketPrice {
                value: p.value,
                currency: p.currency,
            })
        }))
    }

    /// One page of a seller's for-sale inventory
    /// (`GET /users/{username}/inventory`) — the only marketplace direction
    /// Discogs still exposes: seller → stock. (The release → sellers endpoint
    /// was removed; see `docs/design/bulk-sellers-spike.md`.) This is what the
    /// Sellers tab's sweep pages through.
    ///
    /// Newest listings first (`sort=listed`), so a capped sweep keeps the
    /// freshest part of the crates. Non-vinyl listings (CDs, cassettes, files)
    /// are dropped here, matching the records-only vinyl view. `page` is
    /// 1-based; the caller learns the real page count from
    /// [`InventoryPage::pages`]. One rate-limited request per call, and
    /// `per_page` is hard-capped at 100 by Discogs — a large shop takes one
    /// request per hundred listings, which is why sweeps are explicit,
    /// backgrounded and cancellable.
    pub fn seller_inventory(&self, username: &str, page: u32) -> Result<InventoryPage> {
        let page = page.max(1);
        let url = format!("https://api.discogs.com/users/{username}/inventory");
        let resp = self.call_with_retry(|| {
            self.authed(self.agent.get(&url))
                .query("status", "For Sale")
                .query("per_page", "100")
                .query("page", &page.to_string())
                .query("sort", "listed")
                .query("sort_order", "desc")
        })?;
        let body: InventoryResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs inventory response: {e}")))?;
        Ok(InventoryPage {
            pages: body.pagination.pages.max(1),
            items: body.pagination.items,
            listings: body
                .listings
                .into_iter()
                .filter_map(|l| l.into_listing())
                .collect(),
        })
    }

    /// Attach the token + User-Agent every Discogs API request needs. The read
    /// paths above set these inline (alongside their query parameters); the
    /// writes carry no query string, so they share this one helper.
    fn authed(&self, req: ureq::Request) -> ureq::Request {
        req.set("User-Agent", &self.user_agent)
            .set("Authorization", &format!("Discogs token={}", self.token))
    }

    /// Download + downscale a vinyl cover image URL into a display PNG for the
    /// collection grid. `None` on any network/decode failure (the grid then shows
    /// a placeholder). CDN image downloads don't count against the API rate limit.
    pub fn fetch_cover(&self, url: &str) -> Option<Vec<u8>> {
        let bytes = self.download(url).ok()?;
        downscale_png(&bytes, VINYL_COVER_MAX_SIDE)
    }

    /// Resolve the best set of release hits for a track, trying progressively
    /// looser queries so artists whose names confuse Discogs's structured
    /// `artist` index (hyphens / punctuation — e.g. `C3D-E`, whose `artist=`
    /// lookup returns nothing even though the release is plainly credited to it)
    /// still match. Returns the first non-empty result, in this order:
    ///   1. album: `artist` + `release_title`   — most precise
    ///   2. album: `q`=artist + `release_title`  — free-text artist, hyphen-safe
    ///   3. title: `artist` + `track`
    ///   4. title: `q`=artist + `track`
    ///
    /// Album searches take priority over track searches because release-level
    /// matches return canonical artwork, whereas a track-level match can land on
    /// a random compilation cover. The `q` retries only fire when the structured
    /// `artist` filter comes back empty, so names that already match keep their
    /// tighter results.
    fn resolve_hits(
        &self,
        artist: &str,
        title: Option<&str>,
        album: Option<&str>,
    ) -> Result<Vec<SearchHit>> {
        // Search without "(Original Mix)"-style markers: files carry them, the
        // official releases don't, and Discogs still matches titles that do.
        let album = album.map(strip_original_mix).filter(|s| !s.is_empty());
        let title = title.map(strip_original_mix).filter(|s| !s.is_empty());

        if let Some(a) = album {
            for key in ["artist", "q"] {
                let hits = self.search_release(&[
                    (key, artist),
                    ("release_title", a),
                    ("type", "release"),
                    ("per_page", "10"),
                ])?;
                if !hits.is_empty() {
                    return Ok(hits);
                }
            }
        }

        if let Some(t) = title {
            for key in ["artist", "q"] {
                let hits = self.search_release(&[
                    (key, artist),
                    ("track", t),
                    ("type", "release"),
                    ("per_page", "10"),
                ])?;
                if !hits.is_empty() {
                    return Ok(hits);
                }
            }
        }

        Ok(Vec::new())
    }

    /// Browse an artist's or a label's releases by Discogs **id**, for crate
    /// digging — records the user does not already have.
    ///
    /// Ids rather than names: Discogs's search endpoint matches loosely, so
    /// `artist=Lawrence` returns three unrelated Lawrences plus Steve Lawrence,
    /// and `label=Dial` returns the salsa label "Dial Record". An artist id
    /// (`6644`) and a label id (`392`) name exactly one entity, which is what a
    /// dig means by "the same artist". Ids come from
    /// [`ReleaseDetail::artist_ids`] / [`ReleaseDetail::label_ids`].
    ///
    /// `page` is 1-based; the caller varies it for variety and learns the real
    /// page count from [`BrowsePage::pages`]. Vinyl-only filtering is *not*
    /// done here — these endpoints don't take a format filter — so entries
    /// report whatever format Discogs lists and the caller decides.
    ///
    /// One API request per call, paced by the shared throttle.
    pub fn browse_by_id(&self, thread: BrowseThread, id: u64, page: u32) -> Result<BrowsePage> {
        let page = page.max(1);
        let url = match thread {
            BrowseThread::Artist => format!("{ARTISTS_URL}/{id}/releases"),
            BrowseThread::Label => format!("{LABELS_URL}/{id}/releases"),
        };
        let resp = self.call_with_retry(|| {
            self.agent
                .get(&url)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
                .query("per_page", "100")
                .query("page", &page.to_string())
        })?;
        let body: BrowseResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs browse response: {e}")))?;
        Ok(BrowsePage {
            pages: body.pagination.pages.max(1),
            items: body.pagination.items,
            releases: body
                .releases
                .into_iter()
                .filter_map(|r| {
                    // A "master" is an abstract release group; `main_release`
                    // is the concrete pressing to actually show. Entries with
                    // neither id are unusable.
                    let release_id = match r.kind.as_str() {
                        "master" => r.main_release.filter(|id| *id > 0)?,
                        _ => r.id,
                    };
                    let is_master = r.kind == "master";
                    Some(BrowseRelease {
                        release_id,
                        format_known: !is_master && !r.format.trim().is_empty(),
                        title: r.title,
                        artist: r.artist,
                        year: r.year,
                        format: r.format,
                        label: r.label,
                        catno: r.catno,
                        thumb_url: r.thumb,
                        // Only the artist endpoint sets a role; a label's
                        // releases are all "main" as far as a dig cares.
                        main: !r.role.eq_ignore_ascii_case("remix"),
                    })
                })
                .collect(),
        })
    }

    /// Browse releases carrying **every** one of these Discogs style tags
    /// ("Deep House" + "Dub Techno"), for the dig's style thread — records
    /// that sound like this one, from anyone, on any label.
    ///
    /// Unlike the artist/label browses this rides the search endpoint, which
    /// takes a `style` facet and a `format` filter — so the rows come back
    /// vinyl-only and carrying their format string, and the caller's format
    /// resolution has nothing left to do on most pages. Style names come from
    /// [`ReleaseDetail::styles`]; the facet matches each tag exactly, so a
    /// name is as precise as an id is for an artist.
    ///
    /// Several tags go up as one comma-joined facet, which Discogs ANDs: a
    /// "Deep House, Dub Techno" record finds only records tagged with both,
    /// not everything in either bin. (Repeating the parameter instead is
    /// silently collapsed to the first tag.) Blank tags are dropped; with
    /// none left the page comes back empty without a request.
    ///
    /// `page` is 1-based; the caller learns the real page count from
    /// [`BrowsePage::pages`]. One API request per call, paced by the shared
    /// throttle.
    pub fn search_by_style(&self, styles: &[String], page: u32) -> Result<BrowsePage> {
        let style = styles
            .iter()
            .map(|s| s.trim())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(",");
        let style = style.as_str();
        if style.is_empty() {
            return Ok(BrowsePage::default());
        }
        let page = page.max(1).to_string();
        let resp = self.call_with_retry(|| {
            self.agent
                .get(SEARCH_URL)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token))
                .query("style", style)
                .query("format", "Vinyl")
                .query("type", "release")
                .query("per_page", "100")
                .query("page", &page)
        })?;
        let body: SearchResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs search response: {e}")))?;
        Ok(BrowsePage {
            pages: body.pagination.pages.max(1),
            items: body.pagination.items,
            releases: body
                .results
                .into_iter()
                .map(|h| {
                    let (artist, title) = split_artist_title(&h.title);
                    let format = h.format.join(", ");
                    BrowseRelease {
                        release_id: h.id,
                        format_known: !format.trim().is_empty(),
                        title,
                        artist,
                        year: h.year.trim().parse::<u16>().ok().filter(|y| *y > 0),
                        format,
                        label: h.label.into_iter().next().unwrap_or_default(),
                        catno: h.catno,
                        thumb_url: h.thumb,
                        // Search hits carry no credit role; every row is a
                        // full match for the style that found it.
                        main: true,
                    }
                })
                .collect(),
        })
    }

    fn search_release(&self, params: &[(&str, &str)]) -> Result<Vec<SearchHit>> {
        let resp = self.call_with_retry(|| {
            let mut req = self
                .agent
                .get(SEARCH_URL)
                .set("User-Agent", &self.user_agent)
                .set("Authorization", &format!("Discogs token={}", self.token));
            for (k, v) in params {
                req = req.query(k, v);
            }
            req
        })?;
        let body: SearchResponse = resp
            .into_json()
            .map_err(|e| Error::Network(format!("decoding Discogs search response: {e}")))?;
        Ok(body.results)
    }

    fn download(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .agent
            .get(url)
            .set("User-Agent", &self.user_agent)
            .call()
            .map_err(map_ureq_err)?;
        let mut buf = Vec::new();
        resp.into_reader()
            .read_to_end(&mut buf)
            .map_err(|e| Error::Network(format!("reading thumbnail bytes from {url}: {e}")))?;
        Ok(buf)
    }
}

/// The `masters/{id}/versions` response.
#[derive(Debug, Deserialize)]
struct VersionsResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    versions: Vec<VersionEntry>,
}

#[derive(Debug, Deserialize)]
struct VersionEntry {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    #[serde(default, deserialize_with = "null_as_default")]
    format: String,
    #[serde(default, deserialize_with = "null_as_default")]
    label: String,
    #[serde(default, deserialize_with = "null_as_default")]
    catno: String,
    #[serde(default, deserialize_with = "null_as_default")]
    country: String,
    #[serde(default, deserialize_with = "null_as_default")]
    released: String,
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    /// Carrier names (`Vinyl`, `CD`), separate from the detailed `format`.
    #[serde(default, deserialize_with = "null_as_default")]
    major_formats: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    stats: VersionStats,
}

#[derive(Debug, Default, Deserialize)]
struct VersionStats {
    #[serde(default, deserialize_with = "null_as_default")]
    community: VersionCommunity,
}

#[derive(Debug, Default, Deserialize)]
struct VersionCommunity {
    #[serde(default, deserialize_with = "null_as_default")]
    in_collection: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    in_wantlist: u32,
}

/// The artist/label `releases` response.
#[derive(Debug, Deserialize)]
struct BrowseResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    pagination: SearchPagination,
    #[serde(default, deserialize_with = "null_as_default")]
    releases: Vec<BrowseEntry>,
}

/// One raw row of a browse response. The artist and label endpoints return
/// slightly different shapes (only the artist one has `type`/`role`/
/// `main_release`; only the label one has `catno`), so every field that isn't
/// common defaults.
#[derive(Debug, Deserialize)]
struct BrowseEntry {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    /// `"master"` or `"release"`; absent on the label endpoint, where every row
    /// is already a release.
    #[serde(rename = "type", default, deserialize_with = "null_as_default")]
    kind: String,
    /// The concrete release a master stands for.
    #[serde(default)]
    main_release: Option<u64>,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    #[serde(default, deserialize_with = "null_as_default")]
    artist: String,
    #[serde(default)]
    year: Option<u16>,
    #[serde(default, deserialize_with = "null_as_default")]
    format: String,
    #[serde(default, deserialize_with = "null_as_default")]
    label: String,
    #[serde(default, deserialize_with = "null_as_default")]
    catno: String,
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    #[serde(default, deserialize_with = "null_as_default")]
    role: String,
}

/// Split Discogs's joined `"Artist - Title"` release label into its two parts.
///
/// Discogs's search endpoint has no separate artist field — it returns one
/// combined string — so a lookup result has to be split to be rendered as an
/// artist over a title. A label with no ` - ` separator is taken as all title,
/// leaving the artist empty rather than guessing.
fn split_artist_title(combined: &str) -> (String, String) {
    match combined.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), combined.trim().to_string()),
    }
}

/// Shape artist-search rows into [`ArtistHit`]s. An artist row reuses the
/// release row's fields — `title` is the name, `thumb` the portrait — and a
/// row Discogs returns with no name or no id is unusable, so it's dropped
/// rather than shown as a blank chip.
fn artist_hits(results: Vec<SearchHit>) -> Vec<ArtistHit> {
    results
        .into_iter()
        .filter(|h| h.id > 0 && !h.title.trim().is_empty())
        .map(|h| ArtistHit {
            artist_id: h.id,
            name: h.title.trim().to_string(),
            thumb_url: h.thumb,
        })
        .collect()
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    pagination: SearchPagination,
    #[serde(default, deserialize_with = "null_as_default")]
    results: Vec<SearchHit>,
}

/// The `pagination` block Discogs attaches to a search response. Defaults to a
/// single empty page so a response that omits it still deserializes.
#[derive(Debug, Default, Deserialize)]
struct SearchPagination {
    #[serde(default, deserialize_with = "null_as_default")]
    pages: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    items: u32,
}

/// Per-hit community tallies the search endpoint includes with every result:
/// how many users have (`have`) and want (`want`) the release.
#[derive(Debug, Default, Deserialize)]
struct HitCommunity {
    #[serde(default, deserialize_with = "null_as_default")]
    have: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    want: u32,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    /// Full-size release image. Empty when Discogs has no high-res cover.
    #[serde(default, deserialize_with = "null_as_default")]
    cover_image: String,
    /// Catalog number, e.g. `ENV 006`. Empty when Discogs has none.
    #[serde(default, deserialize_with = "null_as_default")]
    catno: String,
    /// "Artist - Title" as Discogs labels the release.
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    #[serde(default, deserialize_with = "null_as_default")]
    year: String,
    #[serde(default, deserialize_with = "null_as_default")]
    country: String,
    #[serde(default, deserialize_with = "null_as_default")]
    label: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    format: Vec<String>,
    #[serde(default)]
    community: HitCommunity,
}

#[derive(Debug, Deserialize)]
struct IdentityResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    username: String,
}

#[derive(Debug, Deserialize)]
struct CollectionResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    pagination: CollectionPagination,
    #[serde(default, deserialize_with = "null_as_default")]
    releases: Vec<CollectionItem>,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionPagination {
    #[serde(default, deserialize_with = "null_as_default")]
    pages: u32,
}

/// `GET /marketplace/stats/{release_id}`. `lowest_price` is null when nothing is
/// for sale, and Discogs also flags releases it won't allow sales of at all.
#[derive(Debug, Default, Deserialize)]
struct MarketplaceStats {
    #[serde(default, deserialize_with = "null_as_default")]
    lowest_price: Option<StatsPrice>,
    #[serde(default, deserialize_with = "null_as_default")]
    blocked_from_sale: bool,
}

#[derive(Debug, Default, Deserialize)]
struct StatsPrice {
    #[serde(default, deserialize_with = "null_as_default")]
    value: f64,
    #[serde(default, deserialize_with = "null_as_default")]
    currency: String,
}

/// One item in a collection folder. The bulk of the metadata lives under
/// `basic_information`; `id`/`instance_id`/`date_added` are on the item itself.
#[derive(Debug, Deserialize)]
struct CollectionItem {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    instance_id: u64,
    /// Which folder holds this copy. Present on every item even though we fetch
    /// through folder 0 ("All"), which is what makes deleting the instance later
    /// possible — the delete endpoint is addressed through the *real* folder.
    #[serde(default, deserialize_with = "null_as_default")]
    folder_id: u32,
    #[serde(default, deserialize_with = "null_as_default")]
    date_added: String,
    #[serde(default, deserialize_with = "null_as_default")]
    basic_information: BasicInformation,
}

#[derive(Debug, Default, Deserialize)]
struct BasicInformation {
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    year: Option<u16>,
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    #[serde(default, deserialize_with = "null_as_default")]
    cover_image: String,
    #[serde(default, deserialize_with = "null_as_default")]
    artists: Vec<CollectionArtist>,
    #[serde(default, deserialize_with = "null_as_default")]
    labels: Vec<ReleaseLabel>,
    #[serde(default, deserialize_with = "null_as_default")]
    formats: Vec<CollectionFormat>,
    #[serde(default, deserialize_with = "null_as_default")]
    genres: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    styles: Vec<String>,
}

/// Response to `POST .../collection/folders/{f}/releases/{r}`. Discogs echoes
/// back only the new copy's identity — no `basic_information` — so the caller
/// rebuilds the cache row from metadata it already has plus this instance id.
#[derive(Debug, Default, Deserialize)]
struct CollectionAdd {
    #[serde(default, deserialize_with = "null_as_default")]
    instance_id: u64,
}

/// One page of `GET /users/{u}/wants`. Same pagination shape as the collection;
/// the items live under `wants` and carry no `instance_id`.
#[derive(Debug, Deserialize)]
struct WantlistResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    pagination: CollectionPagination,
    #[serde(default, deserialize_with = "null_as_default")]
    wants: Vec<WantItem>,
}

#[derive(Debug, Deserialize)]
struct WantItem {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    date_added: String,
    #[serde(default, deserialize_with = "null_as_default")]
    basic_information: BasicInformation,
}

impl WantItem {
    /// Build a [`VinylRecord`], or `None` if the wanted release isn't vinyl.
    /// A want has no per-copy instance, so `instance_id` mirrors the release id
    /// (which is what keys the wantlist cache).
    fn into_record(self) -> Option<VinylRecord> {
        self.basic_information
            .into_record(self.id, self.id, None, self.date_added)
    }
}

/// One page of `GET /users/{username}/inventory`.
#[derive(Debug, Deserialize)]
struct InventoryResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    pagination: SearchPagination,
    #[serde(default, deserialize_with = "null_as_default")]
    listings: Vec<InventoryItem>,
}

/// One marketplace listing in a seller's inventory. Sale terms live on the
/// item; the release being sold is a flat summary (not `basic_information` —
/// this endpoint predates that shape and carries plain strings).
#[derive(Debug, Deserialize)]
struct InventoryItem {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    price: StatsPrice,
    #[serde(default, deserialize_with = "null_as_default")]
    condition: String,
    #[serde(default, deserialize_with = "null_as_default")]
    sleeve_condition: String,
    #[serde(default, deserialize_with = "null_as_default")]
    ships_from: String,
    /// Per-record shipping as Discogs quotes it for the requesting account's
    /// location. Absent when the seller only publishes a free-text policy.
    #[serde(default)]
    shipping_price: Option<StatsPrice>,
    #[serde(default, deserialize_with = "null_as_default")]
    allow_offers: bool,
    #[serde(default, deserialize_with = "null_as_default")]
    uri: String,
    #[serde(default, deserialize_with = "null_as_default")]
    posted: String,
    #[serde(default, deserialize_with = "null_as_default")]
    release: InventoryRelease,
}

#[derive(Debug, Default, Deserialize)]
struct InventoryRelease {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    artist: String,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    year: Option<u16>,
    /// Format summary as a plain string (`12", 45 RPM`) — no `formats` array on
    /// this endpoint, so the vinyl filter has to read this text.
    #[serde(default, deserialize_with = "null_as_default")]
    format: String,
    #[serde(default, deserialize_with = "null_as_default")]
    label: String,
    #[serde(default, deserialize_with = "null_as_default")]
    catalog_number: String,
    #[serde(default, deserialize_with = "null_as_default")]
    thumbnail: String,
}

/// Is this inventory format summary a record? The endpoint gives only a free
/// text like `12"` / `LP, Album` / `CD, Compilation`, so this is a denylist of
/// unambiguous non-vinyl tokens rather than a proof of vinyl — an unknown or
/// empty format is kept, matching how the browse endpoints treat missing
/// formats as "unknown", not "not a record".
fn inventory_format_is_vinyl(format: &str) -> bool {
    const NOT_VINYL: [&str; 14] = [
        "cd", "cdr", "sacd", "hdcd", "cass", "cassette", "dvd", "dvdr", "vhs", "file", "files",
        "minidisc", "dat", "shellac",
    ];
    !format
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .any(|t| NOT_VINYL.iter().any(|n| t.eq_ignore_ascii_case(n)))
}

impl InventoryItem {
    /// Build a [`SellerListing`], or `None` for a listing that isn't a record
    /// (or is malformed — a listing with no id or no release can't be keyed).
    fn into_listing(self) -> Option<SellerListing> {
        if self.id == 0 || self.release.id == 0 {
            return None;
        }
        if !inventory_format_is_vinyl(&self.release.format) {
            return None;
        }
        let r = self.release;
        Some(SellerListing {
            listing_id: self.id,
            release_id: r.id,
            title: r.title,
            artist: strip_discogs_number(&r.artist),
            year: r.year.filter(|y| *y > 0),
            label: none_if_empty(r.label),
            catalog_number: none_if_empty(r.catalog_number),
            format: none_if_empty(r.format),
            thumb_url: none_if_empty(r.thumbnail),
            price: self.price.value,
            currency: self.price.currency,
            condition: none_if_empty(self.condition),
            sleeve_condition: none_if_empty(self.sleeve_condition),
            ships_from: none_if_empty(self.ships_from),
            shipping_price: self.shipping_price.as_ref().map(|p| p.value),
            shipping_currency: self.shipping_price.and_then(|p| none_if_empty(p.currency)),
            allow_offers: self.allow_offers,
            uri: none_if_empty(self.uri),
            posted: none_if_empty(self.posted),
        })
    }
}

#[derive(Debug, Default, Deserialize)]
struct CollectionArtist {
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionFormat {
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    descriptions: Vec<String>,
}

impl CollectionItem {
    /// Build a [`VinylRecord`], or `None` if this item isn't a vinyl pressing.
    fn into_record(self) -> Option<VinylRecord> {
        self.basic_information.into_record(
            self.instance_id,
            self.id,
            Some(self.folder_id),
            self.date_added,
        )
    }
}

impl BasicInformation {
    /// Build a [`VinylRecord`] from the release metadata a collection *or*
    /// wantlist item carries, or `None` if it isn't a vinyl pressing. Discogs
    /// lists CDs, files and cassettes in both; the "Vinyl Collection" view is
    /// records only, so non-vinyl formats are dropped.
    fn into_record(
        self,
        instance_id: u64,
        release_id: u64,
        folder_id: Option<u32>,
        date_added: String,
    ) -> Option<VinylRecord> {
        let bi = self;
        let is_vinyl = bi
            .formats
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("Vinyl"));
        if !is_vinyl {
            return None;
        }
        // Strip Discogs's disambiguation suffix (e.g. "Surgeon (2)") and join
        // multi-artist credits the way the release is billed.
        let artist = bi
            .artists
            .iter()
            .map(|a| strip_discogs_number(&a.name))
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        // Summarize the format as "name, descriptions" (e.g. `Vinyl, 12", 45 RPM`).
        let format = bi.formats.first().map(|f| {
            let mut parts = vec![f.name.clone()];
            parts.extend(f.descriptions.iter().cloned());
            parts.join(", ")
        });
        let (label, catalog_number) = match bi.labels.into_iter().next() {
            Some(l) => (none_if_empty(l.name), none_if_empty(l.catno)),
            None => (None, None),
        };
        Some(VinylRecord {
            instance_id,
            release_id,
            title: bi.title,
            artist,
            year: bi.year.filter(|y| *y > 0),
            label,
            catalog_number,
            format,
            thumb_url: none_if_empty(bi.thumb),
            cover_url: none_if_empty(bi.cover_image),
            added: none_if_empty(date_added),
            folder_id,
            has_cover: false,
            // Neither list endpoint carries a price; it's looked up per release
            // and read back from the cache (see `Catalog::set_vinyl_price`).
            price: None,
            price_currency: None,
            genres: genre_tags(&bi.genres, &bi.styles),
        })
    }
}

/// Case- and punctuation-insensitive form used to compare video titles against
/// track titles. Keeps word order (so `starts_with` stays meaningful) but drops
/// everything that varies between a tag and a YouTube title: case, punctuation,
/// diacritics (`Áttfalt` / `Attfalt`), and the spelling of a few words that
/// uploaders and Discogs never agree on (`&` / `and`, `Pt.` / `Part`).
fn norm_loose(s: &str) -> String {
    let mut folded = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            // `&` and `+` are words, not punctuation: `Bits + Pieces` is
            // `Bits & Pieces` is `Bits And Pieces`.
            '&' | '+' => folded.push_str(" and "),
            c => match fold_diacritic(c) {
                Some(plain) => folded.push_str(plain),
                None => folded.push(c),
            },
        }
    }
    folded
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|w| match w {
            "pt" => "part",
            "vol" => "volume",
            "feat" | "ft" => "featuring",
            "rmx" => "remix",
            w => w,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A Latin letter with its diacritic dropped, or `None` for anything that
/// isn't one. Covers Latin-1 and Latin Extended-A, which is what record
/// titles in the catalogue actually carry (`Í`, `ø`, `ł`).
fn fold_diacritic(c: char) -> Option<&'static str> {
    const TABLE: &[(&str, &str)] = &[
        ("ÀÁÂÃÄÅĀĂĄàáâãäåāăą", "a"),
        ("ÆæǢǣ", "ae"),
        ("ÇĆĈĊČçćĉċč", "c"),
        ("ÐĎĐðďđ", "d"),
        ("ÈÉÊËĒĔĖĘĚèéêëēĕėęě", "e"),
        ("ĜĞĠĢĝğġģ", "g"),
        ("ĤĦĥħ", "h"),
        ("ÌÍÎÏĨĪĬĮİìíîïĩīĭįı", "i"),
        ("Ĵĵ", "j"),
        ("Ķķ", "k"),
        ("ĹĻĽĿŁĺļľŀł", "l"),
        ("ÑŃŅŇñńņňŉ", "n"),
        ("ÒÓÔÕÖØŌŎŐòóôõöøōŏő", "o"),
        ("Œœ", "oe"),
        ("ŔŖŘŕŗř", "r"),
        ("ŚŜŞŠśŝşš", "s"),
        ("ß", "ss"),
        ("ŢŤŦţťŧ", "t"),
        ("ÙÚÛÜŨŪŬŮŰŲùúûüũūŭůűų", "u"),
        ("Ŵŵ", "w"),
        ("ÝŶŸýÿŷ", "y"),
        ("ŹŻŽźżž", "z"),
        ("Þþ", "th"),
    ];
    if c.is_ascii() {
        return None;
    }
    TABLE
        .iter()
        .find(|(from, _)| from.contains(c))
        .map(|(_, to)| *to)
}

/// The normalized forms a title is compared under: as written, and with an
/// "(Original Mix)" marker dropped — Discogs lists the marker on some
/// pressings and uploaders leave it off, or the reverse. Empty forms are
/// dropped, so a title that *is* the marker compares under nothing.
fn title_forms(title: &str) -> Vec<String> {
    let mut out = vec![norm_loose(title)];
    let bare = norm_loose(strip_original_mix(title));
    if bare != out[0] {
        out.push(bare);
    }
    out.retain(|w| !w.is_empty());
    out
}

/// A normalized title without a leading "The": `The Road Of Life` and `Road
/// Of Life` are the same track, whichever side wrote the article.
fn dethe(s: &str) -> &str {
    s.strip_prefix("the ").unwrap_or(s)
}

/// The form two normalized titles are judged equal under: no leading "The",
/// no spaces — `Boz Boz` and `Bozboz` are one title. Also what the typo
/// distance is measured over.
fn title_key(s: &str) -> String {
    dethe(s).replace(' ', "")
}

/// Whether a normalized video title names pressing position `pos` (`b1`) as a
/// word of its own — `[ARMA02] B1 - Djungl - Rakataka`, `The Perfect Memory
/// B1` — and names no *other* position, so `A1 & B2 (mix)` claims neither.
fn names_position(text: &str, pos: &str) -> bool {
    let looks_positional = |w: &str| {
        let letters = w.chars().take_while(|c| c.is_ascii_alphabetic()).count();
        (1..=2).contains(&letters)
            && w.len() > letters
            && w.len() - letters <= 2
            && w[letters..].bytes().all(|b| b.is_ascii_digit())
    };
    let mut named = false;
    for w in text.split(' ') {
        if w == pos {
            named = true;
        } else if looks_positional(w) {
            return false;
        }
    }
    named
}

/// Whether two normalized positions name the same slot: `b1` and `b1`, or
/// `01` and `1` on a release that numbers its tracks.
fn same_position(a: &str, b: &str) -> bool {
    let (a_bare, b_bare) = (a.trim_start_matches('0'), b.trim_start_matches('0'));
    a == b || (!a_bare.is_empty() && a_bare == b_bare)
}

/// How many typos a title of `key.len()` characters may carry and still be
/// read as the same title: none while it's short enough that one changed
/// letter is a different word, then one, then two.
fn typo_budget(key: &str) -> usize {
    match key.len() {
        0..=7 => 0,
        8..=15 => 1,
        _ => 2,
    }
}

/// Edit distance between two short strings — insert, delete or replace one
/// character each. Titles run to a few dozen characters, so the plain
/// quadratic form is fine.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur[j + 1] = sub.min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// What a release knows about itself that a video title might repeat before
/// getting to the track: its own title, and the names it's credited to.
/// [`video_title_candidates`] peels these off the front of a title so
/// `Finale Underground Vol 3 Hakim Murphy Slowdown` reads as `Slowdown`.
struct TitleContext {
    /// The release title, normalized.
    title: String,
    /// The title and every credited name, normalized, longest first, so
    /// `Skudge Presents X` peels before `Skudge`.
    prefixes: Vec<String>,
}

impl TitleContext {
    fn new(detail: &ReleaseDetail, release_artist: &str) -> Self {
        let mut prefixes: Vec<String> = Vec::new();
        let mut add = |name: &str| {
            let n = norm_loose(name);
            if !n.is_empty() && !prefixes.contains(&n) {
                prefixes.push(n);
            }
        };
        add(&detail.title);
        // Credits joined by Discogs connectors name several artists at once;
        // a video usually names one.
        for credit in std::iter::once(release_artist)
            .chain(detail.tracklist.iter().filter_map(|t| t.artist.as_deref()))
        {
            add(credit);
            for name in credit.split([',', '/']).flat_map(|c| c.split(" & ")) {
                add(name);
            }
        }
        prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));
        TitleContext {
            title: norm_loose(&detail.title),
            prefixes,
        }
    }
}

/// The forms a Discogs video title might match a track under.
///
/// Uploaders stack prefixes — `Artist - Title`, `Label • Artist - Release |
/// A1 Title`, `Artist "Title" (Official Video)`, `Release B2` — so a title is
/// read two ways. *Literally*: the whole thing and the tail after each
/// separator, which is how a video that plainly names the track is found.
/// Then *derived*: bracketed runs and a file extension dropped, the tail after
/// each of a wider set of separators, the inside of each quoted or
/// parenthesized run (exact matches only, see [`Cand`]), and each of those
/// with an "(Original Mix)" marker, the release's own title, an artist, or a
/// position marker peeled off the front and filler (`official video`, a
/// year) off the back. Normalizing *after* the split is what makes the
/// separators survive long enough to be useful. The whole title comes first,
/// so a track that really is called `Untitled 1` still wins it outright.
fn video_title_candidates(title: &str, cx: &TitleContext) -> Vec<Cand> {
    const SEPARATORS: [char; 5] = ['-', '–', '—', '|', '•'];
    const WIDER_SEPARATORS: [char; 8] = ['-', '–', '—', '|', '•', '/', ':', '~'];
    const EXTENSIONS: [&str; 7] = [".wmv", ".mp4", ".avi", ".mov", ".flv", ".mkv", ".mp3"];

    let mut out: Vec<Cand> = Vec::new();
    let mut push = |text: String, exact_only: bool, literal: bool, position: Option<String>| {
        if text.is_empty() {
            return;
        }
        match out.iter_mut().find(|c| c.text == text) {
            // Read more than one way, the least constrained reading wins.
            Some(c) => {
                c.exact_only &= exact_only;
                c.literal |= literal;
                if c.position != position {
                    c.position = None;
                }
            }
            None => out.push(Cand {
                text,
                exact_only,
                literal,
                position,
            }),
        }
    };
    for seg in tails(title, &SEPARATORS) {
        push(norm_loose(seg), false, true, None);
    }

    // Bracketed runs are catalogue numbers and labels (`[XOZ010]`, `[Limited
    // Vinyl]`), never the track; drop them before anything else is read.
    let mut raw = String::with_capacity(title.len());
    let mut depth = 0usize;
    for c in title.chars() {
        match c {
            '[' => depth += 1,
            ']' => depth = depth.saturating_sub(1),
            c if depth == 0 => raw.push(c),
            _ => {}
        }
    }
    let raw = raw.trim();
    let raw = EXTENSIONS
        .iter()
        .find_map(|e| {
            let cut = raw.len().checked_sub(e.len())?;
            (raw.is_char_boundary(cut) && raw[cut..].eq_ignore_ascii_case(e)).then(|| &raw[..cut])
        })
        .unwrap_or(raw)
        .trim();

    let mut segments: Vec<(&str, bool)> = tails(raw, &WIDER_SEPARATORS)
        .into_iter()
        .map(|s| (s, false))
        .collect();
    // A run quoted inside the *release's* name — `Loops Of Infinity (A Rave
    // Loveletter)` — is part of that name, not a track called "A Rave
    // Loveletter", so it's read only when the video isn't naming the release.
    let names_release = norm_loose(raw).contains(cx.title.as_str());
    for (open, close) in [('"', '"'), ('“', '”'), ('(', ')'), ('\'', '\'')] {
        let mut rest = raw;
        while let Some(start) = rest.find(open) {
            let inner = &rest[start + open.len_utf8()..];
            let Some(end) = inner.find(close) else { break };
            let quoted = &inner[..end];
            if !(names_release && cx.title.contains(norm_loose(quoted).as_str())) {
                segments.push((quoted, true));
            }
            rest = &inner[end + close.len_utf8()..];
        }
    }

    for (seg, exact_only) in segments {
        for seg in [seg, strip_original_mix(seg)] {
            let full = norm_loose(seg);
            push(full.clone(), exact_only, false, None);
            let mut text = trim_filler(full);
            push(text.clone(), exact_only, false, None);
            let mut position: Option<String> = None;
            // Peel what the release already told us, one layer at a time:
            // `finale underground volume 3 hakim murphy slowdown` reads down to
            // `slowdown`.
            loop {
                let peeled = cx
                    .prefixes
                    .iter()
                    .find_map(|p| {
                        text.strip_prefix(p.as_str())
                            .and_then(|r| r.strip_prefix(' '))
                    })
                    .or_else(|| {
                        let (head, tail) = text.split_once(' ')?;
                        // A leading position or track number: `a1 title`,
                        // `01 title`, `1. title`. What's read past it is
                        // that position's.
                        let positional = head.len() <= 4
                            && head.bytes().any(|b| b.is_ascii_digit())
                            && head.chars().take_while(|c| c.is_ascii_alphabetic()).count() <= 2;
                        if positional && position.is_none() {
                            position = Some(head.to_string());
                        }
                        positional.then_some(tail)
                    })
                    .map(|t| trim_filler(t.to_string()));
                match peeled {
                    Some(p) if !p.is_empty() && p != text => {
                        text = p;
                        push(text.clone(), exact_only, false, position.clone());
                    }
                    _ => break,
                }
            }
        }
    }
    out
}

/// `text` and the tail after each character of `seps` in it: `A - B | C`
/// reads as itself, `B | C` and `C`.
fn tails<'a>(text: &'a str, seps: &[char]) -> Vec<&'a str> {
    let mut out = vec![text];
    let mut rest = text;
    while let Some(i) = rest.find(seps) {
        rest = &rest[i + rest[i..].chars().next().map_or(1, |c| c.len_utf8())..];
        out.push(rest);
    }
    out
}

/// Drop trailing words that describe the upload rather than the track:
/// `official video`, `hd`, a year, `remastered`.
fn trim_filler(text: String) -> String {
    const FILLER: [&str; 18] = [
        "official",
        "video",
        "audio",
        "hd",
        "hq",
        "4k",
        "lyric",
        "lyrics",
        "visualizer",
        "visualiser",
        "remastered",
        "remaster",
        "clip",
        "videoclip",
        "music",
        "promo",
        "vinyl",
        "rip",
    ];
    let mut words: Vec<&str> = text.split(' ').collect();
    while let Some(last) = words.last() {
        let year = last.len() == 4
            && (last.starts_with("19") || last.starts_with("20"))
            && last.bytes().all(|b| b.is_ascii_digit());
        if words.len() > 1 && (year || FILLER.contains(last)) {
            words.pop();
        } else {
            break;
        }
    }
    words.join(" ")
}

/// Drop a trailing "(Original Mix)"-style marker from a title before searching
/// Discogs. Files from digital stores carry these suffixes, but the official
/// releases usually don't, so leaving them in makes otherwise-good queries come
/// back empty. Remix/edit credits are kept — those are part of the real title.
/// Handles `(...)`, `[...]` and the bare `- Original Mix` dash form, stacked.
pub fn strip_original_mix(title: &str) -> &str {
    const MARKERS: [&str; 3] = ["original mix", "original version", "original"];
    let mut t = title.trim();
    'outer: loop {
        for m in MARKERS {
            for suffix in [format!("({m})"), format!("[{m}]"), format!("- {m}")] {
                let Some(cut) = t.len().checked_sub(suffix.len()) else {
                    continue;
                };
                if t.is_char_boundary(cut) && t[cut..].eq_ignore_ascii_case(&suffix) {
                    t = t[..cut].trim_end();
                    continue 'outer;
                }
            }
        }
        return t;
    }
}

/// Drop a trailing Discogs disambiguation number, e.g. `Surgeon (2)` → `Surgeon`.
fn strip_discogs_number(name: &str) -> String {
    let trimmed = name.trim();
    if let Some(open) = trimmed.rfind(" (") {
        let tail = &trimmed[open + 2..];
        if tail.ends_with(')') && tail[..tail.len() - 1].chars().all(|c| c.is_ascii_digit()) {
            return trimmed[..open].trim().to_string();
        }
    }
    trimmed.to_string()
}

#[derive(Debug, Deserialize)]
struct ReleaseResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    year: Option<u16>,
    #[serde(default, deserialize_with = "null_as_default")]
    released: String,
    #[serde(default, deserialize_with = "null_as_default")]
    country: String,
    #[serde(default, deserialize_with = "null_as_default")]
    genres: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    styles: Vec<String>,
    #[serde(default, deserialize_with = "null_as_default")]
    labels: Vec<ReleaseLabel>,
    #[serde(default, deserialize_with = "null_as_default")]
    artists: Vec<ReleaseArtist>,
    /// The master (release group) this pressing belongs to, 0 when it stands
    /// alone. Every other pressing of the same music hangs off it — see
    /// [`Client::master_versions`].
    #[serde(default, deserialize_with = "null_as_default")]
    master_id: u64,
    /// Pressing formats. Only read by [`ReleaseResponse::format_summary`], to
    /// answer "is this actually a record?" for a browse row that carried no
    /// format of its own.
    #[serde(default, deserialize_with = "null_as_default")]
    formats: Vec<CollectionFormat>,
    /// Cover art, for caching a record the user just added to their
    /// collection — the add response itself carries no artwork. Unlike the
    /// collection and wantlist listings, the release endpoint has no
    /// `cover_image`: `thumb` is the 150px preview and the full-size art is
    /// the `images` gallery. See [`ReleaseResponse::cover_url`].
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    #[serde(default, deserialize_with = "null_as_default")]
    images: Vec<ReleaseImage>,
    #[serde(default, deserialize_with = "null_as_default")]
    tracklist: Vec<TracklistEntry>,
    #[serde(default, deserialize_with = "null_as_default")]
    videos: Vec<VideoEntry>,
}

/// One entry of a release's image gallery. Discogs marks the cover
/// `"primary"` and the rest (back sleeve, labels, inserts) `"secondary"`; the
/// listing endpoints' `cover_image` is the first of these, at 600px.
#[derive(Debug, Deserialize)]
struct ReleaseImage {
    #[serde(default, rename = "type", deserialize_with = "null_as_default")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    uri: String,
}

#[derive(Debug, Deserialize)]
struct ReleaseLabel {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    catno: String,
}

/// An artist credit on a release — only the id is used, to browse that exact
/// artist's other records.
///
/// The same shape is reused for a *track's* own credit on a compilation, where
/// `name` and `join` are what matter instead: see [`TracklistEntry::artists`].
#[derive(Debug, Deserialize)]
struct ReleaseArtist {
    #[serde(default, deserialize_with = "null_as_default")]
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    /// How this credit joins to the next one — Discogs writes the literal
    /// connector ("&", "Feat.", ","). Empty on the last credit, and on every
    /// credit of a single-artist track.
    #[serde(default, deserialize_with = "null_as_default")]
    join: String,
}

#[derive(Debug, Deserialize)]
struct TracklistEntry {
    /// `"track"` for a real track; `"heading"` and `"index"` rows are section
    /// titles ("Side A", a medley header) with nothing to play, and are dropped.
    #[serde(default, rename = "type_", deserialize_with = "null_as_default")]
    kind: String,
    #[serde(default, deserialize_with = "null_as_default")]
    position: String,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    #[serde(default, deserialize_with = "null_as_default")]
    duration: String,
    /// Who performed *this track*, when the release itself doesn't say.
    /// Discogs populates it on compilations and split releases — exactly the
    /// records whose release-level artist is "Various" and therefore names
    /// nobody. Absent on a single-artist album, where the release credit
    /// already covers every track.
    #[serde(default, deserialize_with = "null_as_default")]
    artists: Vec<ReleaseArtist>,
}

#[derive(Debug, Deserialize)]
struct VideoEntry {
    #[serde(default, deserialize_with = "null_as_default")]
    uri: String,
    #[serde(default, deserialize_with = "null_as_default")]
    title: String,
    /// Seconds, as Discogs reports it. 0 shows up for videos whose length was
    /// never resolved, and reads the same as absent.
    #[serde(default, deserialize_with = "null_as_default")]
    duration: u32,
    /// Discogs defaults this to true when the uploader never touched it, so an
    /// absent field must not read as "blocked".
    #[serde(default = "yes", deserialize_with = "null_as_yes")]
    embed: bool,
}

fn yes() -> bool {
    true
}

/// Read a field that Discogs may send as an explicit `null`, falling back to the
/// type's default. `#[serde(default)]` only covers an *absent* key: a present
/// `"country": null` (which Discogs sends for releases with no country, and
/// likewise for catalog numbers, video titles and durations) still fails to
/// decode into a `String`, taking the whole release down with it.
fn null_as_default<'de, D, T>(de: D) -> std::result::Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(de)?.unwrap_or_default())
}

/// Same, for `embed`, where the fallback is `true` rather than `false` — a video
/// Discogs says nothing about is playable, not blocked.
fn null_as_yes<'de, D>(de: D) -> std::result::Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<bool>::deserialize(de)?.unwrap_or(true))
}

impl ReleaseResponse {
    /// Build a [`VinylRecord`] straight from a release response, for a copy the
    /// user just added to their collection. `None` when the release isn't
    /// vinyl. Mirrors `BasicInformation::into_record`, but reads the release
    /// endpoint's own shape.
    fn into_vinyl_record(self, instance_id: u64, folder_id: Option<u32>) -> Option<VinylRecord> {
        if !self
            .formats
            .iter()
            .any(|f| f.name.eq_ignore_ascii_case("Vinyl"))
        {
            return None;
        }
        let artist = self
            .artists
            .iter()
            .map(|a| strip_discogs_number(&a.name))
            .filter(|n| !n.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        let (label, catalog_number) = match self.labels.first() {
            Some(l) => (
                none_if_empty(l.name.clone()),
                none_if_empty(l.catno.clone()),
            ),
            None => (None, None),
        };
        let format = none_if_empty(self.format_summary());
        Some(VinylRecord {
            instance_id,
            release_id: self.id,
            title: self.title.clone(),
            artist,
            year: self.year.filter(|y| *y > 0),
            label,
            catalog_number,
            format,
            thumb_url: none_if_empty(self.thumb.clone()),
            cover_url: self.cover_url(),
            // Discogs stamps the add itself; the next sync brings the real date.
            added: None,
            folder_id,
            has_cover: false,
            price: None,
            price_currency: None,
            genres: genre_tags(&self.genres, &self.styles),
        })
    }

    /// The full-size cover, matching the `cover_image` the collection and
    /// wantlist listings carry for the same release: the gallery's primary
    /// image, else its first (some releases mark none primary), else the
    /// thumb. Cached as the collection grid's cover, so anything but the
    /// full-size image would leave the record blurry until a sync replaced
    /// it.
    fn cover_url(&self) -> Option<String> {
        self.images
            .iter()
            .find(|i| i.kind == "primary")
            .or_else(|| self.images.first())
            .and_then(|i| none_if_empty(i.uri.clone()))
            .or_else(|| none_if_empty(self.thumb.clone()))
    }

    /// The release's formats as one comparable string, e.g. `Vinyl, 12", Album`.
    fn format_summary(&self) -> String {
        self.formats
            .iter()
            .map(|f| {
                let mut parts = vec![f.name.clone()];
                parts.extend(f.descriptions.iter().cloned());
                parts.join(", ")
            })
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn into_detail(self) -> ReleaseDetail {
        // Discogs lists labels in release order; the first is the primary one.
        let label_ids: Vec<u64> = self
            .labels
            .iter()
            .map(|l| l.id)
            .filter(|i| *i > 0)
            .collect();
        let (label, catalog_number) = match self.labels.into_iter().next() {
            Some(l) => (none_if_empty(l.name), none_if_empty(l.catno)),
            None => (None, None),
        };
        // "Various" (id 194) is Discogs's placeholder for a compilation, not an
        // artist anyone can browse — dropping it here keeps every consumer from
        // having to know that.
        let artist_ids: Vec<u64> = self
            .artists
            .iter()
            .filter(|a| a.id > 0 && !a.name.eq_ignore_ascii_case("various"))
            .map(|a| a.id)
            .collect();
        ReleaseDetail {
            release_id: self.id.to_string(),
            title: self.title,
            // Discogs uses 0 for "unknown year"; treat it as absent.
            year: self.year.filter(|y| *y > 0),
            released: none_if_empty(self.released),
            country: none_if_empty(self.country),
            genres: self.genres,
            styles: self.styles,
            label,
            catalog_number,
            artist_ids,
            label_ids,
            master_id: (self.master_id > 0).then_some(self.master_id),
            tracklist: self
                .tracklist
                .into_iter()
                // Keep real tracks only. Discogs leaves `type_` empty on some
                // older releases, so an absent kind counts as a track rather
                // than silently emptying those listings.
                .filter(|t| (t.kind.is_empty() || t.kind == "track") && !t.title.trim().is_empty())
                .map(|t| ReleaseTrack {
                    position: t.position,
                    title: t.title,
                    duration: t.duration,
                    artist: join_credits(&t.artists),
                })
                .collect(),
            videos: self
                .videos
                .into_iter()
                .filter(|v| !v.uri.trim().is_empty())
                .map(|v| ReleaseVideo {
                    uri: v.uri,
                    title: v.title,
                    duration_secs: (v.duration > 0).then_some(v.duration),
                    embeddable: v.embed,
                })
                .collect(),
        }
    }
}

/// Render a track's artist credits the way Discogs punctuates them: each name
/// followed by its own `join` connector, which is how "A & B" and "A Feat. B"
/// keep their intended meaning rather than being flattened to a comma list.
///
/// Discogs writes the connector bare ("&"), so it gets spaces around it — but a
/// comma is already attached to the preceding name ("A, B", not "A , B"), so
/// punctuation-only connectors are appended tight. `None` when nothing is
/// credited, which is the common case: only compilations carry these.
fn join_credits(artists: &[ReleaseArtist]) -> Option<String> {
    let mut out = String::new();
    for a in artists {
        let name = a.name.trim();
        if name.is_empty() {
            continue;
        }
        if !out.is_empty() && !out.ends_with(|c: char| c.is_whitespace()) {
            out.push(' ');
        }
        out.push_str(name);
        let join = a.join.trim();
        if !join.is_empty() {
            // A bare comma/semicolon hugs the name it follows; a word or
            // symbol connector ("&", "Feat.") stands on its own with spaces.
            if join.chars().all(|c| matches!(c, ',' | ';')) {
                out.push_str(join);
            } else {
                out.push(' ');
                out.push_str(join);
            }
        }
    }
    // A trailing connector (Discogs sometimes leaves one on the last credit)
    // would otherwise read as a dangling "A &".
    let out = out
        .trim()
        .trim_end_matches([',', ';', '&', '/'])
        .trim()
        .to_string();
    (!out.is_empty()).then_some(out)
}

fn none_if_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Every genre tag on a release as one flat list: Discogs's coarse genres
/// ("Electronic") followed by its finer styles ("Deep House"), trimmed and
/// deduplicated case-insensitively. The one shape the record sheet displays and
/// the vinyl view's genre filter matches on, so the two always agree.
pub fn genre_tags(genres: &[String], styles: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for tag in genres.iter().chain(styles) {
        let tag = tag.trim();
        if !tag.is_empty() && !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            out.push(tag.to_string());
        }
    }
    out
}

/// Parse the `Retry-After` header Discogs sends on a 429 or a 503
/// (delta-seconds form) into a wait duration. `None` if the header is absent or
/// unparseable, leaving the caller to fall back to its own backoff.
fn retry_after(resp: &ureq::Response) -> Option<Duration> {
    resp.header("Retry-After")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

fn map_ureq_err(e: ureq::Error) -> Error {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            Error::Discogs {
                status: code,
                message: discogs_error_message(&body),
            }
        }
        // ureq's transport errors spell out the whole resolver or socket
        // failure. Whatever the cause, what the user can do is the same:
        // check the connection.
        ureq::Error::Transport(_) => {
            Error::Network("Couldn't reach Discogs. Check your connection".into())
        }
    }
}

/// The sentence Discogs puts in an error body, reworded for the person
/// reading it. Error bodies are `{"message": "..."}`; anything else (an HTML
/// error page from the CDN, an empty body) yields an empty string so the
/// caller falls back to a generic line rather than echoing markup.
fn discogs_error_message(body: &str) -> String {
    #[derive(Deserialize)]
    struct ErrorBody {
        message: String,
    }
    let Ok(ErrorBody { message }) = serde_json::from_str::<ErrorBody>(body) else {
        return String::new();
    };
    let m = message.trim().trim_end_matches('.').trim();
    if m.is_empty() {
        return String::new();
    }
    // Discogs writes about "the user"; the reader *is* the user, and the
    // sentence is quoted mid-line, so it opens in lower case.
    let mut out = m
        .replace("the user's", "your")
        .replace("The user's", "Your");
    let first = out
        .chars()
        .next()
        .map(|c| c.to_lowercase().to_string())
        .unwrap_or_default();
    out.replace_range(..out.chars().next().map_or(0, char::len_utf8), &first);
    out
}

/// Decode arbitrary image bytes (Discogs returns JPEG), downscale to a
/// `max_side`-pixel square, re-encode as PNG. Returns `None` on any failure,
/// which the caller treats as "no usable artwork" and moves on.
/// Decode `bytes` and re-encode as PNG, shrinking to fit `max_side` if larger.
/// Never enlarges: `image::thumbnail` scales *up* to fit as readily as down,
/// which would turn a 150px thumb into a 400px blur that looks cached at
/// full size — and can't be told apart from one afterwards.
fn downscale_png(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = if img.width() > max_side || img.height() > max_side {
        img.thumbnail(max_side, max_side)
    } else {
        img
    };
    let mut out = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

// `read_to_end` is from std::io::Read — pull it in for the download path.
use std::io::Read;

#[cfg(test)]
mod throttle_tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let img = image::DynamicImage::ImageRgb8(image::RgbImage::new(w, h));
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    fn dims(png: &[u8]) -> (u32, u32) {
        let img = image::load_from_memory(png).unwrap();
        (img.width(), img.height())
    }

    /// A source smaller than the cap keeps its size; a larger one shrinks to
    /// fit. Upscaling a thumb would only fake a full-size cover.
    #[test]
    fn downscale_png_shrinks_but_never_enlarges() {
        assert_eq!(dims(&downscale_png(&png(150, 150), 400).unwrap()), (150, 150));
        assert_eq!(dims(&downscale_png(&png(400, 400), 400).unwrap()), (400, 400));
        assert_eq!(dims(&downscale_png(&png(600, 300), 400).unwrap()), (400, 200));
    }

    /// The pace must be shared by *separately constructed* clients, not just by
    /// clones of one. Callers build a fresh client per worker thread, so a
    /// per-instance clock would give each concurrent worker its own full
    /// allowance — which is exactly how the dig's prefetch started drawing 429s
    /// the moment it put a second caller on Discogs at the same time.
    #[test]
    fn throttle_is_shared_across_separately_built_clients() {
        let a = Client::new("t", "Ordnung/test");
        let b = Client::new("t", "Ordnung/test");
        let start = Instant::now();
        a.throttle();
        b.throttle();
        assert!(
            start.elapsed() >= MIN_API_INTERVAL,
            "two clients paced independently: {:?} elapsed for two requests,              expected at least {MIN_API_INTERVAL:?}",
            start.elapsed()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discogs_error_message_rewords_the_body_for_the_reader() {
        assert_eq!(
            discogs_error_message(
                r#"{"message":"That release does not exist in the user's wantlist."}"#
            ),
            "that release does not exist in your wantlist"
        );
        assert_eq!(discogs_error_message(""), "");
        assert_eq!(discogs_error_message("<html>502</html>"), "");
        assert_eq!(discogs_error_message(r#"{"message":"  "}"#), "");
    }

    #[test]
    fn discogs_errors_read_as_one_short_line() {
        let e = Error::Discogs {
            status: 404,
            message: "that release does not exist in your wantlist".into(),
        };
        assert_eq!(
            e.to_string(),
            "Discogs says that release does not exist in your wantlist"
        );
        let e = Error::Discogs {
            status: 401,
            message: "invalid consumer key".into(),
        };
        assert_eq!(
            e.to_string(),
            "Discogs doesn't accept your token. Check it in Settings"
        );
        let e = Error::Discogs {
            status: 503,
            message: String::new(),
        };
        assert_eq!(
            e.to_string(),
            "Discogs is having trouble right now. Try again in a few minutes"
        );
        let e = Error::Discogs {
            status: 404,
            message: String::new(),
        };
        assert_eq!(e.to_string(), "Discogs can't find that record");
        // Only an unfamiliar status keeps its code: there it's the one clue.
        let e = Error::Discogs {
            status: 418,
            message: String::new(),
        };
        assert_eq!(e.to_string(), "Discogs couldn't do that (HTTP 418)");
    }

    #[test]
    fn artist_hits_keep_named_rows_and_drop_blank_ones() {
        let body: SearchResponse = serde_json::from_str(
            r#"{"pagination":{"pages":1,"items":3},"results":[
                {"id":6644,"type":"artist","title":"Lawrence","thumb":"https://i/l.jpg","cover_image":"https://i/L.jpg"},
                {"id":9,"type":"artist","title":"   "},
                {"id":0,"type":"artist","title":"Ghost"},
                {"id":12,"type":"artist","title":"Lawrence (2)","thumb":""}
            ]}"#,
        )
        .unwrap();
        let hits = artist_hits(body.results);
        assert_eq!(
            hits,
            vec![
                ArtistHit {
                    artist_id: 6644,
                    name: "Lawrence".into(),
                    thumb_url: "https://i/l.jpg".into(),
                },
                ArtistHit {
                    artist_id: 12,
                    name: "Lawrence (2)".into(),
                    thumb_url: String::new(),
                },
            ]
        );
    }

    #[test]
    fn split_artist_title_separates_on_first_dash() {
        let (a, t) = split_artist_title("Metro Area - Miura");
        assert_eq!(a, "Metro Area");
        assert_eq!(t, "Miura");
    }

    /// Titles routinely contain their own hyphens; only the first ` - ` is the
    /// artist boundary, so the rest has to survive intact.
    #[test]
    fn split_artist_title_keeps_later_dashes_in_the_title() {
        let (a, t) = split_artist_title("Theo Parrish - Summertime Is Here - Remixes");
        assert_eq!(a, "Theo Parrish");
        assert_eq!(t, "Summertime Is Here - Remixes");
    }

    /// No separator means Discogs gave us a bare title (common for untitled
    /// white labels). Guessing an artist would be worse than leaving it blank.
    #[test]
    fn split_artist_title_leaves_artist_empty_without_a_separator() {
        let (a, t) = split_artist_title("Untitled");
        assert_eq!(a, "");
        assert_eq!(t, "Untitled");
    }

    fn detail() -> ReleaseDetail {
        ReleaseDetail {
            release_id: "123".into(),
            title: "Plastikman EP".into(),
            year: Some(1993),
            released: Some("1993-05-01".into()),
            country: Some("Canada".into()),
            genres: vec!["Electronic".into()],
            styles: vec!["Acid".into(), "Techno".into()],
            label: Some("Plus 8".into()),
            catalog_number: Some("PLUS8 024".into()),
            artist_ids: vec![11209],
            label_ids: vec![385],
            master_id: None,
            tracklist: Vec::new(),
            videos: Vec::new(),
        }
    }

    fn video(uri: &str, title: &str) -> ReleaseVideo {
        ReleaseVideo {
            uri: uri.into(),
            title: title.into(),
            duration_secs: Some(300),
            embeddable: true,
        }
    }

    fn track(position: &str, title: &str) -> ReleaseTrack {
        ReleaseTrack {
            position: position.into(),
            title: title.into(),
            duration: "5:00".into(),
            artist: None,
        }
    }

    fn credit(name: &str, join: &str) -> ReleaseArtist {
        ReleaseArtist {
            id: 0,
            name: name.into(),
            join: join.into(),
        }
    }

    /// A single credit is just the name; nothing to join.
    #[test]
    fn join_credits_renders_a_lone_artist() {
        assert_eq!(
            join_credits(&[credit("Herbert", "")]),
            Some("Herbert".into())
        );
    }

    /// Discogs writes the connector bare, so it needs spaces on both sides —
    /// otherwise a collaboration reads "Theo Parrish&Marcellus Pittman".
    #[test]
    fn join_credits_spaces_word_and_symbol_connectors() {
        assert_eq!(
            join_credits(&[credit("Theo Parrish", "&"), credit("Marcellus Pittman", "")]),
            Some("Theo Parrish & Marcellus Pittman".into())
        );
        assert_eq!(
            join_credits(&[credit("Moodymann", "Feat."), credit("Andrés", "")]),
            Some("Moodymann Feat. Andrés".into())
        );
    }

    /// A comma belongs to the name before it, not between two spaces.
    #[test]
    fn join_credits_hugs_comma_connectors() {
        assert_eq!(
            join_credits(&[credit("A", ","), credit("B", "")]),
            Some("A, B".into())
        );
    }

    /// Discogs sometimes leaves a connector on the final credit, which would
    /// otherwise render as a dangling "Artist &".
    #[test]
    fn join_credits_drops_a_trailing_connector() {
        assert_eq!(join_credits(&[credit("Solo", "&")]), Some("Solo".into()));
    }

    /// The common case by far: an ordinary album track credits nobody
    /// separately, and must not produce an empty string to draw.
    #[test]
    fn join_credits_is_none_when_nothing_is_credited() {
        assert_eq!(join_credits(&[]), None);
        assert_eq!(join_credits(&[credit("   ", "")]), None);
    }

    /// The whole point: a compilation's per-track credits survive parsing, so
    /// the sheet can name performers a "Various" header can't.
    #[test]
    fn tracklist_keeps_per_track_artists() {
        let json = r#"{
            "id": 1, "title": "Efflorescence III",
            "tracklist": [
                {"type_": "track", "position": "A1", "title": "Six Castles", "duration": "5:18",
                 "artists": [{"id": 7, "name": "Some Artist", "join": ""}]},
                {"type_": "track", "position": "A2", "title": "Summer Sun", "duration": "4:02"}
            ]
        }"#;
        let resp: ReleaseResponse = serde_json::from_str(json).unwrap();
        let detail = resp.into_detail();
        assert_eq!(detail.tracklist[0].artist.as_deref(), Some("Some Artist"));
        // No `artists` key at all is the single-artist case, not an error.
        assert_eq!(detail.tracklist[1].artist, None);
    }

    /// Discogs sends `"country": null` (and nulls elsewhere) rather than
    /// omitting the key, which `#[serde(default)]` alone rejects — that failure
    /// surfaced as "invalid type: null, expected a string" on the record sheet.
    #[test]
    fn release_decodes_with_explicit_nulls() {
        let json = r#"{
            "id": 32413806,
            "title": "Lastday Cookie",
            "year": 2024,
            "country": null,
            "released": null,
            "genres": null,
            "styles": ["Techno"],
            "labels": [{"name": "Pin", "catno": null}],
            "tracklist": [{"type_": null, "position": "A1", "title": "Ma", "duration": null}],
            "videos": [{"uri": "https://youtu.be/abc", "title": null, "duration": null, "embed": null}]
        }"#;
        let detail: ReleaseDetail = serde_json::from_str::<ReleaseResponse>(json)
            .unwrap()
            .into_detail();
        assert_eq!(detail.country, None);
        assert_eq!(detail.released, None);
        assert_eq!(detail.catalog_number, None);
        assert!(detail.genres.is_empty());
        assert_eq!(detail.label.as_deref(), Some("Pin"));
        assert_eq!(detail.tracklist.len(), 1);
        assert_eq!(detail.tracklist[0].duration, "");
        assert_eq!(detail.videos.len(), 1);
        assert_eq!(detail.videos[0].duration_secs, None);
        // A null `embed` is silence from the uploader, not a block.
        assert!(detail.videos[0].embeddable);
    }

    #[test]
    fn youtube_ids_parse_from_every_form_discogs_stores() {
        let id = |u: &str| video(u, "").youtube_id().map(str::to_string);
        assert_eq!(
            id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        assert_eq!(
            id("http://youtube.com/watch?v=abc_-123&t=42").as_deref(),
            Some("abc_-123")
        );
        // The `v=` parameter isn't always first.
        assert_eq!(
            id("https://www.youtube.com/watch?t=9&v=xyz789").as_deref(),
            Some("xyz789")
        );
        assert_eq!(
            id("https://youtu.be/dQw4w9WgXcQ?t=30").as_deref(),
            Some("dQw4w9WgXcQ")
        );
        // Anything that isn't YouTube has no embeddable id.
        assert_eq!(id("https://vimeo.com/12345"), None);
        assert_eq!(id("https://www.youtube.com/watch?list=PL123"), None);
    }

    #[test]
    fn videos_match_tracks_by_title_position_and_nothing_looser() {
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Safe From Harm"),
            track("A2", "One Love"),
            track("B1", "Lately"),
            track("B2", "Hymn Of The Big Wheel"),
        ];
        d.videos = vec![
            video("https://youtu.be/v1", "Massive Attack - One Love"),
            video("https://youtu.be/v2", "Safe From Harm (Perfecto Mix)"),
            video("https://youtu.be/v3", "B1. Lately"),
            video("https://youtu.be/v4", "Blue Lines - Full Album"),
        ];
        let m = d.video_matches();
        // Title after an "Artist - " prefix, a trailing mix suffix, and a
        // leading pressing position all resolve.
        assert_eq!(m, vec![Some(1), Some(0), Some(2), None]);
        // The album rip claimed by no track stays available on its own.
        let left: Vec<&str> = d
            .unmatched_videos()
            .iter()
            .map(|(_, v)| v.uri.as_str())
            .collect();
        assert_eq!(left, vec!["https://youtu.be/v4"]);
    }

    #[test]
    fn videos_match_through_stacked_uploader_prefixes() {
        // Real shape from a Discogs release: catalogue number, bullet, artist,
        // release title, then the position and track after a pipe.
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Meadow"),
            track("B1", "Break2"),
            track("B2", "Break2 (KW Refix)"),
        ];
        d.videos = vec![
            video(
                "https://youtu.be/v1",
                "DIFF006 • Skudge - Meadow | A1 Meadow",
            ),
            video(
                "https://youtu.be/v2",
                "DIFF006 • Skudge - Meadow | B1 Break2",
            ),
            video(
                "https://youtu.be/v3",
                "DIFF006 • Skudge - Meadow | B2 Break2 KW Refix",
            ),
        ];
        assert_eq!(d.video_matches(), vec![Some(0), Some(1), Some(2)]);
        assert!(d.unmatched_videos().is_empty());
    }

    /// The library case: a record carries "Dreamuniverse" and, later in its
    /// tracklist, "Dreamuniverse Pt.II". With only the Pt.II file on hand, the
    /// earlier track must not grab it by prefix — and with both files, each
    /// track gets its own.
    #[test]
    fn an_exact_title_later_in_the_tracklist_beats_an_earlier_prefix() {
        let mut d = detail();
        d.tracklist = vec![
            track("J2", "Dreamuniverse"),
            track("K1", "Dreamuniverse Pt.II"),
            track("L3", "Dreamuniverse Pt.III"),
        ];
        let only_pt2 = vec!["dreamuniverse pt.ii".to_string()];
        assert_eq!(d.file_matches(&only_pt2), vec![None, Some(0), None]);

        let all = vec![
            "dreamuniverse pt.iii".to_string(),
            "dreamuniverse pt.ii".to_string(),
            "dreamuniverse".to_string(),
        ];
        assert_eq!(d.file_matches(&all), vec![Some(2), Some(1), Some(0)]);

        // The prefix rule still serves when nothing exact exists for a track.
        let mixes = vec!["dreamuniverse (original mix)".to_string()];
        assert_eq!(d.file_matches(&mixes), vec![Some(0), None, None]);
    }

    #[test]
    fn videos_match_through_uploader_noise() {
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Atman (Original Mix)"),
            track("A2", "Áttfalt"),
            track("B1", "Double Jointed Sex Freak (Part 2)"),
            track("B2", "Lost & Found"),
            track("C1", "Snowshoe"),
            track("C2", "Be My Love"),
            track("D1", "Steeler"),
            track("D2", "Lastday Cookie [No Hats]"),
            track("E1", "The Road Of Life"),
        ];
        d.videos = vec![
            video("https://youtu.be/v1", "YokoO & Atish - Atman"),
            video("https://youtu.be/v2", "Exos - Attfalt [XOZ010]"),
            video(
                "https://youtu.be/v3",
                "Levon Vincent - Double Jointed Sex Freak Pt. 2",
            ),
            video("https://youtu.be/v4", "Lost and Found (Official Video)"),
            video("https://youtu.be/v5", "Dntel \"Snowshoe\" (Greer 2018)"),
            video("https://youtu.be/v6", "Frederic Blais (Be My Love) 2000"),
            video("https://youtu.be/v7", "Steeler (original version).wmv"),
            video("https://youtu.be/v8", "SnPLO - Lastday cookie [no hats]"),
            video("https://youtu.be/v9", "Road Of Life"),
        ];
        // An "(Original Mix)" marker on either side, diacritics, `Pt.` for
        // `Part`, `and` for `&`, a quoted or parenthesized title, a file
        // extension, a bracketed catalogue number and a leading "The" all
        // read past.
        assert_eq!(d.video_matches(), (0..9).map(Some).collect::<Vec<_>>());
    }

    #[test]
    fn videos_name_a_track_by_position_anywhere_in_the_title() {
        let mut d = detail();
        d.tracklist = vec![track("A1", "Untitled 1"), track("A2", "Untitled 2")];
        d.videos = vec![
            video("https://youtu.be/v1", "C3D E – The Perfect Memory A2"),
            video("https://youtu.be/v2", "C3D E – The Perfect Memory A1"),
            video(
                "https://youtu.be/v3",
                "C3D E – The Perfect Memory A1 & A2 (full)",
            ),
        ];
        // Each side takes its own; a title naming two positions names neither.
        assert_eq!(d.video_matches(), vec![Some(1), Some(0)]);
    }

    #[test]
    fn identical_titles_are_told_apart_by_the_position_read_off_the_video() {
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Valis 003"),
            track("A2", "Valis 003"),
            track("A3", "Valis 003"),
        ];
        d.videos = vec![
            video("https://youtu.be/v1", "A2 VALIS 003"),
            video("https://youtu.be/v2", "A3 VALIS 003"),
        ];
        // `VALIS 003` read past the `A2` is A2's alone — A1 can't take it
        // just because its title is the same.
        assert_eq!(d.video_matches(), vec![None, Some(0), Some(1)]);
    }

    #[test]
    fn videos_read_past_the_release_and_artist_names() {
        let mut d = detail();
        d.title = "Finale Underground Vol. 3: Future Chicago".into();
        d.tracklist = vec![
            ReleaseTrack {
                artist: Some("Hakim Murphy".into()),
                ..track("B1", "Slowdown")
            },
            track("B2", "Ottagone 016"),
        ];
        d.videos = vec![
            video(
                "https://youtu.be/v1",
                "Finale Underground Vol 3   Future Chicago   Hakim Murphy   Slowdown",
            ),
            video("https://youtu.be/v2", "Ottagone Ottagone 016"),
        ];
        // The per-track credit is known to the detail; the release artist is
        // the sheet's to pass in.
        assert_eq!(d.video_matches(), vec![Some(0), None]);
        assert_eq!(d.match_videos("Ottagone").tracks, vec![Some(0), Some(1)]);
    }

    #[test]
    fn a_typo_away_still_matches_when_no_other_track_is_that_close() {
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Wrapped In Spaces Between Us"),
            track("B1", "Untitled 2"),
            track("B2", "Untitled 3"),
            track("C1", "Love"),
        ];
        d.videos = vec![
            video(
                "https://youtu.be/v1",
                "Dolomea - Wrapped in Space Between Us",
            ),
            video("https://youtu.be/v2", "Untitled 4"),
            video("https://youtu.be/v3", "Lave"),
        ];
        // One letter off a long title is the same title; "Untitled 4" is one
        // off *two* tracks and so belongs to neither; a four-letter title
        // gets no slack at all.
        assert_eq!(d.video_matches(), vec![Some(0), None, None, None]);
    }

    #[test]
    fn a_literal_video_title_beats_a_trimmed_one_and_a_remix_goes_to_the_remix() {
        let mut d = detail();
        d.tracklist = vec![
            track("A1", "Confusion"),
            track("B1", "Resiclaps"),
            track("B2", "Resiclaps (Andrei Ciubuc Remix)"),
        ];
        d.videos = vec![
            video(
                "https://youtu.be/v1",
                "New Order - Confusion (Official Music Video) [HD Upgrade]",
            ),
            video("https://youtu.be/v2", "Confusion"),
            video(
                "https://youtu.be/v3",
                "PREMIERE: Firesc - Resiclaps (Andrei Ciubuc Remix) [_NRV]",
            ),
        ];
        assert_eq!(d.video_matches(), vec![Some(1), None, Some(2)]);
    }

    #[test]
    fn a_run_quoted_in_the_release_name_is_not_a_track() {
        let mut d = detail();
        d.title = "Loops Of Infinity (A Rave Loveletter)".into();
        d.tracklist = vec![track("C1", "A Rave Loveletter")];
        d.videos = vec![video(
            "https://youtu.be/v1",
            "DJ Metatron – Loops Of Infinity (A Rave Loveletter) [APW3]",
        )];
        // The full-album upload names the release, not the track that
        // happens to share the parenthetical.
        assert_eq!(d.match_videos("DJ Metatron").tracks, vec![None]);
        assert_eq!(d.match_videos("DJ Metatron").leftover, vec![0]);
    }

    #[test]
    fn a_short_track_title_does_not_swallow_a_longer_video() {
        let mut d = detail();
        d.tracklist = vec![track("A1", "Love")];
        d.videos = vec![video("https://youtu.be/v1", "One Love")];
        // "One Love" merely *contains* "Love" — claiming it would play the
        // wrong track, so the row stays empty.
        assert_eq!(d.video_matches(), vec![None]);
    }

    #[test]
    fn release_json_parses_tracklist_and_videos() {
        let json = r#"{
            "id": 123,
            "title": "Blue Lines",
            "tracklist": [
                {"type_": "heading", "position": "", "title": "Side A", "duration": ""},
                {"type_": "track", "position": "A1", "title": "Safe From Harm", "duration": "5:18"},
                {"type_": "track", "position": "A2", "title": "One Love", "duration": "4:48"}
            ],
            "videos": [
                {"uri": "https://youtu.be/v1", "title": "Safe From Harm", "duration": 318},
                {"uri": "https://youtu.be/v2", "title": "One Love", "duration": 0, "embed": false}
            ]
        }"#;
        let d: ReleaseResponse = serde_json::from_str(json).unwrap();
        let d = d.into_detail();
        // The "Side A" heading is not a track.
        assert_eq!(d.tracklist.len(), 2);
        assert_eq!(d.tracklist[0].position, "A1");
        assert_eq!(d.tracklist[0].title, "Safe From Harm");
        assert_eq!(d.tracklist[0].duration, "5:18");
        assert_eq!(d.tracklist[1].duration, "4:48");
        // Duration 0 means "unknown", and an absent `embed` is not "blocked".
        assert_eq!(d.videos[0].duration_secs, Some(318));
        assert!(d.videos[0].embeddable);
        assert_eq!(d.videos[1].duration_secs, None);
        assert!(!d.videos[1].embeddable);
    }

    #[test]
    fn fills_only_empty_fields() {
        let mut tags = Tags::default();
        let filled = detail().apply_to_tags(&mut tags, false);
        assert_eq!(filled, 7);
        // Styles win over genres for the DJ-relevant `genre` field.
        assert_eq!(tags.genre.as_deref(), Some("Acid, Techno"));
        assert_eq!(tags.label.as_deref(), Some("Plus 8"));
        assert_eq!(tags.catalog_number.as_deref(), Some("PLUS8 024"));
        assert_eq!(tags.release_country.as_deref(), Some("Canada"));
        assert_eq!(tags.album.as_deref(), Some("Plastikman EP"));
        assert_eq!(tags.release_date.as_deref(), Some("1993-05-01"));
        assert_eq!(tags.year, Some(1993));
    }

    #[test]
    fn never_overwrites_existing_values() {
        let mut tags = Tags {
            genre: Some("House".into()),
            year: Some(2001),
            album: Some("  ".into()), // whitespace counts as empty and gets filled
            ..Tags::default()
        };
        let filled = detail().apply_to_tags(&mut tags, false);
        // genre + year kept; album/label/catno/country/release_date filled.
        assert_eq!(tags.genre.as_deref(), Some("House"));
        assert_eq!(tags.year, Some(2001));
        assert_eq!(tags.album.as_deref(), Some("Plastikman EP"));
        assert_eq!(filled, 5);
    }

    #[test]
    fn overwrite_replaces_existing_values_but_skips_identical() {
        let mut tags = Tags {
            genre: Some("House".into()),         // differs → replaced
            year: Some(2001),                    // differs → replaced
            album: Some("Plastikman EP".into()), // identical → no-op, not counted
            ..Tags::default()
        };
        let filled = detail().apply_to_tags(&mut tags, true);
        assert_eq!(tags.genre.as_deref(), Some("Acid, Techno"));
        assert_eq!(tags.year, Some(1993));
        assert_eq!(tags.album.as_deref(), Some("Plastikman EP"));
        // genre, year, label, catalog_number, country, release_date = 6.
        // Album is unchanged (already equal) so it isn't written.
        assert_eq!(filled, 6);
    }

    #[test]
    fn proposed_fills_lists_only_empty_fields_with_values() {
        let tags = Tags {
            genre: Some("House".into()),
            year: Some(2001),
            ..Tags::default()
        };
        let fills = detail().proposed_fills(&tags, false);
        // Genre + year already set → excluded; the rest are proposed.
        let fields: Vec<_> = fills.iter().map(|f| f.field).collect();
        assert!(!fields.contains(&FillField::Genre));
        assert!(!fields.contains(&FillField::Year));
        assert!(fields.contains(&FillField::Label));
        assert!(fields.contains(&FillField::Album));
        // Values come through for the preview.
        let album = fills.iter().find(|f| f.field == FillField::Album).unwrap();
        assert_eq!(album.value, "Plastikman EP");
        // proposed_fills count matches what apply_to_tags will write.
        let mut t = tags.clone();
        assert_eq!(detail().apply_to_tags(&mut t, false), fills.len());
    }

    #[test]
    fn falls_back_to_genres_when_no_styles() {
        let mut d = detail();
        d.styles.clear();
        let mut tags = Tags::default();
        d.apply_to_tags(&mut tags, false);
        assert_eq!(tags.genre.as_deref(), Some("Electronic"));
    }

    #[test]
    fn strips_original_mix_markers() {
        assert_eq!(
            strip_original_mix("Strings Of Life (Original Mix)"),
            "Strings Of Life"
        );
        assert_eq!(
            strip_original_mix("Strings Of Life [ORIGINAL MIX]"),
            "Strings Of Life"
        );
        assert_eq!(
            strip_original_mix("Strings Of Life - Original Mix"),
            "Strings Of Life"
        );
        assert_eq!(
            strip_original_mix("Voodoo Ray (Original Version)"),
            "Voodoo Ray"
        );
        assert_eq!(strip_original_mix("Voodoo Ray (Original)"), "Voodoo Ray");
        // Stacked markers all come off.
        assert_eq!(
            strip_original_mix("Track (Original Mix) [Original]"),
            "Track"
        );
        // Remix/edit credits are part of the real title and stay.
        assert_eq!(
            strip_original_mix("Age Of Love (Jam & Spoon Remix)"),
            "Age Of Love (Jam & Spoon Remix)"
        );
        assert_eq!(strip_original_mix("Plain Title"), "Plain Title");
        // A title that is nothing but the marker strips to empty (caller skips it).
        assert_eq!(strip_original_mix("(Original Mix)"), "");
    }

    #[test]
    fn strips_discogs_disambiguation_number() {
        assert_eq!(strip_discogs_number("Surgeon (2)"), "Surgeon");
        assert_eq!(strip_discogs_number("Ø (3)"), "Ø");
        // A real parenthetical that isn't a bare number is left intact.
        assert_eq!(strip_discogs_number("Underworld (UK)"), "Underworld (UK)");
        assert_eq!(strip_discogs_number("Aphex Twin"), "Aphex Twin");
    }

    fn vinyl_item() -> CollectionItem {
        CollectionItem {
            id: 42,
            instance_id: 1001,
            folder_id: 3,
            date_added: "2021-03-04T12:00:00-08:00".into(),
            basic_information: BasicInformation {
                title: "Plastikman EP".into(),
                year: Some(1993),
                thumb: "https://img/thumb.jpg".into(),
                cover_image: "https://img/cover.jpg".into(),
                artists: vec![CollectionArtist {
                    name: "Plastikman (2)".into(),
                }],
                labels: vec![ReleaseLabel {
                    id: 385,
                    name: "Plus 8".into(),
                    catno: "PLUS8 024".into(),
                }],
                formats: vec![CollectionFormat {
                    name: "Vinyl".into(),
                    descriptions: vec!["12\"".into(), "45 RPM".into()],
                }],
                genres: vec!["Electronic".into()],
                styles: vec!["Techno".into(), "Acid".into()],
            },
        }
    }

    #[test]
    fn collection_item_builds_vinyl_record() {
        let rec = vinyl_item().into_record().expect("vinyl item -> record");
        assert_eq!(rec.instance_id, 1001);
        assert_eq!(rec.release_id, 42);
        assert_eq!(rec.title, "Plastikman EP");
        assert_eq!(rec.artist, "Plastikman"); // disambiguation number stripped
        assert_eq!(rec.year, Some(1993));
        assert_eq!(rec.label.as_deref(), Some("Plus 8"));
        assert_eq!(rec.catalog_number.as_deref(), Some("PLUS8 024"));
        assert_eq!(rec.format.as_deref(), Some("Vinyl, 12\", 45 RPM"));
        assert_eq!(rec.cover_url.as_deref(), Some("https://img/cover.jpg"));
        assert!(!rec.has_cover);
        // Genres then styles, one flat tag list.
        assert_eq!(rec.genres, vec!["Electronic", "Techno", "Acid"]);
    }

    /// The release endpoint has no `cover_image`; the grid cover must come from
    /// the gallery's primary image, not the 150px thumb.
    #[test]
    fn release_response_caches_the_full_size_cover() {
        let json = r#"{
            "id": 7, "title": "Sheet One", "year": 1993,
            "artists": [{"id": 1, "name": "Plastikman"}],
            "labels": [{"id": 385, "name": "Plus 8", "catno": "PLUS8 024"}],
            "formats": [{"name": "Vinyl", "descriptions": ["12\""]}],
            "thumb": "https://img/h:150/thumb.jpg",
            "images": [
                {"type": "secondary", "uri": "https://img/h:600/back.jpg", "uri150": "https://img/h:150/back.jpg"},
                {"type": "primary", "uri": "https://img/h:600/front.jpg", "uri150": "https://img/h:150/front.jpg"}
            ]
        }"#;
        let rec = serde_json::from_str::<ReleaseResponse>(json)
            .unwrap()
            .into_vinyl_record(1001, Some(UNCATEGORIZED_FOLDER))
            .expect("vinyl release -> record");
        assert_eq!(rec.thumb_url.as_deref(), Some("https://img/h:150/thumb.jpg"));
        assert_eq!(rec.cover_url.as_deref(), Some("https://img/h:600/front.jpg"));

        // No primary marked: the first image is what the listings call the
        // cover. No images at all: the thumb is better than nothing.
        let json = r#"{"id": 7, "formats": [{"name": "Vinyl"}], "thumb": "https://img/t.jpg",
            "images": [{"type": "secondary", "uri": "https://img/first.jpg"}]}"#;
        let rec = serde_json::from_str::<ReleaseResponse>(json)
            .unwrap()
            .into_vinyl_record(1, None)
            .unwrap();
        assert_eq!(rec.cover_url.as_deref(), Some("https://img/first.jpg"));
        let json = r#"{"id": 7, "formats": [{"name": "Vinyl"}], "thumb": "https://img/t.jpg"}"#;
        let rec = serde_json::from_str::<ReleaseResponse>(json)
            .unwrap()
            .into_vinyl_record(1, None)
            .unwrap();
        assert_eq!(rec.cover_url.as_deref(), Some("https://img/t.jpg"));
    }

    #[test]
    fn genre_tags_dedupe_case_insensitively() {
        let tags = genre_tags(
            &["Electronic".into(), "Folk, World, & Country".into()],
            &["Techno".into(), "electronic".into(), " ".into()],
        );
        assert_eq!(tags, vec!["Electronic", "Folk, World, & Country", "Techno"]);
    }

    #[test]
    fn wantlist_item_keys_on_release_id() {
        let item = WantItem {
            id: 42,
            date_added: "2024-01-02T00:00:00-08:00".into(),
            basic_information: vinyl_item().basic_information,
        };
        let rec = item.into_record().expect("want item -> record");
        // A want has no per-copy instance; the release id keys it.
        assert_eq!(rec.release_id, 42);
        assert_eq!(rec.instance_id, 42);
        assert_eq!(rec.artist, "Plastikman");
        assert_eq!(rec.added.as_deref(), Some("2024-01-02T00:00:00-08:00"));
    }

    #[test]
    fn wantlist_item_skips_non_vinyl() {
        let mut bi = vinyl_item().basic_information;
        bi.formats = vec![CollectionFormat {
            name: "File".into(),
            descriptions: vec!["WAV".into()],
        }];
        let item = WantItem {
            id: 42,
            date_added: String::new(),
            basic_information: bi,
        };
        assert!(item.into_record().is_none());
    }

    #[test]
    fn collection_item_skips_non_vinyl() {
        let mut item = vinyl_item();
        item.basic_information.formats = vec![CollectionFormat {
            name: "CD".into(),
            descriptions: vec!["Album".into()],
        }];
        assert!(item.into_record().is_none());
    }
}
