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
use crate::model::{Tags, VinylRecord};
use serde::{Deserialize, Serialize};
use std::io::Cursor;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const SEARCH_URL: &str = "https://api.discogs.com/database/search";
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

impl ReleaseDetail {
    /// Which video plays each track: one entry per `tracklist` position, holding
    /// an index into `videos` (or `None` when nothing on the release matches).
    ///
    /// Discogs video titles are free text typed by whoever attached them —
    /// `"Massive Attack - Safe From Harm"`, `"A1. Safe From Harm"`, or just the
    /// track name — so matching is deliberately conservative: a video is claimed
    /// only when its title (or the part after an `Artist -` prefix) *starts with*
    /// the track's title, or when it opens with the track's pressing position.
    /// Anything looser makes a short title like "Love" swallow the wrong video.
    /// Each video is claimed at most once; whatever is left over (album rips,
    /// live sets) stays available via [`unmatched_videos`](Self::unmatched_videos).
    pub fn video_matches(&self) -> Vec<Option<usize>> {
        let candidates: Vec<Vec<String>> = self
            .videos
            .iter()
            .map(|v| video_title_candidates(&v.title))
            .collect();
        self.claim_by_title(&candidates, true)
    }

    /// Which of `titles` (the track titles of local files linked to this
    /// release) plays each tracklist position. Same conservative matching as
    /// [`video_matches`](Self::video_matches), minus the positional fallback —
    /// a file named `A1` is a filename convention, not a title.
    pub fn file_matches(&self, titles: &[String]) -> Vec<Option<usize>> {
        let candidates: Vec<Vec<String>> = titles.iter().map(|t| vec![norm_loose(t)]).collect();
        self.claim_by_title(&candidates, false)
    }

    /// Assign at most one candidate to each tracklist entry. `candidates[i]` is
    /// the set of normalized forms item `i` may match under; the first item that
    /// matches a track claims it and is not offered to later tracks.
    fn claim_by_title(
        &self,
        candidates: &[Vec<String>],
        allow_position: bool,
    ) -> Vec<Option<usize>> {
        let mut used = vec![false; candidates.len()];
        let mut out = Vec::with_capacity(self.tracklist.len());
        for t in &self.tracklist {
            let want = norm_loose(&t.title);
            let pos = norm_loose(&t.position);
            let mut hit = None;
            if !want.is_empty() {
                // Two passes so an exact title always wins over a positional
                // guess, even when the positional video comes first in the list.
                'search: for exact_only in [true, false] {
                    for (i, cands) in candidates.iter().enumerate() {
                        if used[i] {
                            continue;
                        }
                        let title_hit = cands.iter().any(|c| {
                            c == &want || (!exact_only && c.starts_with(&format!("{want} ")))
                        });
                        // A position match needs the position to be a real
                        // side/track marker (`a1`), not a bare digit that would
                        // collide with any number in a video title.
                        let pos_hit = allow_position
                            && !exact_only
                            && pos.len() >= 2
                            && pos.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                            && cands.iter().any(|c| {
                                c == &pos || c.starts_with(&format!("{pos} "))
                            });
                        if title_hit || pos_hit {
                            hit = Some(i);
                            used[i] = true;
                            break 'search;
                        }
                    }
                }
            }
            out.push(hit);
        }
        out
    }

    /// The videos no track claimed, as `(index, video)` pairs in release order.
    /// These are the full-album rips, live sets and interviews Discogs carries
    /// alongside the per-track links.
    pub fn unmatched_videos(&self) -> Vec<(usize, &ReleaseVideo)> {
        let claimed: std::collections::HashSet<usize> =
            self.video_matches().into_iter().flatten().collect();
        self.videos
            .iter()
            .enumerate()
            .filter(|(i, _)| !claimed.contains(i))
            .collect()
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
        push_fill(&mut out, FillField::Album, &tags.album, overwrite, self.title.clone());
        push_fill(
            &mut out,
            FillField::ReleaseDate,
            &tags.release_date,
            overwrite,
            self.released.clone().unwrap_or_default(),
        );
        if let Some(y) = self.year {
            // Write when empty, or (overwrite) when it differs from the current year.
            let write = if overwrite { tags.year != Some(y) } else { tags.year.is_none() };
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
    /// Timestamp of the last API request, shared across clones so every search /
    /// release call — whichever worker fires it, however many a single track
    /// triggers — is paced against one global clock. Discogs rate-limits per
    /// *request*, not per track. `None` until the first request.
    last_request: Arc<Mutex<Option<Instant>>>,
}

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
            last_request: Arc::new(Mutex::new(None)),
        }
    }

    /// Block until at least [`MIN_API_INTERVAL`] has elapsed since the previous
    /// API request, then stamp "now". Holding the lock across the sleep is
    /// intentional: it serializes concurrent workers so they share one pace
    /// rather than each racing to the limit independently.
    fn throttle(&self) {
        let mut last = self.last_request.lock().expect("discogs throttle lock");
        if let Some(prev) = *last {
            let elapsed = prev.elapsed();
            if elapsed < MIN_API_INTERVAL {
                std::thread::sleep(MIN_API_INTERVAL - elapsed);
            }
        }
        *last = Some(Instant::now());
    }

    /// Run an API request, throttling before each attempt and retrying on HTTP
    /// 429. `build` is called fresh per attempt (a `ureq::Request` is consumed
    /// by `.call()`, so it can't be reused). On a 429 we wait out the server's
    /// `Retry-After` when present, else a widening backoff, then retry up to
    /// [`MAX_RETRIES`] times before surfacing the error.
    fn call_with_retry<F>(&self, build: F) -> Result<ureq::Response>
    where
        F: Fn() -> ureq::Request,
    {
        let mut attempt = 0;
        loop {
            self.throttle();
            match build().call() {
                Ok(resp) => return Ok(resp),
                Err(ureq::Error::Status(429, resp)) if attempt < MAX_RETRIES => {
                    let wait = retry_after(&resp)
                        .unwrap_or_else(|| Duration::from_secs(2 * (attempt as u64 + 1)));
                    std::thread::sleep(wait);
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
        let body: ReleaseResponse = resp.into_json().map_err(|e| {
            Error::Network(format!("decoding Discogs release response: {e}"))
        })?;
        Ok(body.into_detail())
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
        let body: IdentityResponse = resp.into_json().map_err(|e| {
            Error::Network(format!("decoding Discogs identity response: {e}"))
        })?;
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
        let base = format!(
            "https://api.discogs.com/users/{username}/collection/folders/0/releases"
        );
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
            let body: WantlistResponse = resp.into_json().map_err(|e| {
                Error::Network(format!("decoding Discogs wantlist response: {e}"))
            })?;
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
        let album = album.map(str::trim).filter(|s| !s.is_empty());
        let title = title.map(str::trim).filter(|s| !s.is_empty());

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
        let body: SearchResponse = resp.into_json().map_err(|e| {
            Error::Network(format!("decoding Discogs search response: {e}"))
        })?;
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

#[derive(Debug, Deserialize)]
struct SearchResponse {
    #[serde(default, deserialize_with = "null_as_default")]
    results: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    id: u64,
    #[serde(default, deserialize_with = "null_as_default")]
    thumb: String,
    /// Full-size release image. Empty when Discogs has no high-res cover.
    #[serde(default, deserialize_with = "null_as_default")]
    cover_image: String,
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

#[derive(Debug, Deserialize)]
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
        self.basic_information
            .into_record(self.instance_id, self.id, Some(self.folder_id), self.date_added)
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
        })
    }
}

/// Case- and punctuation-insensitive form used to compare video titles against
/// track titles. Keeps word order (so `starts_with` stays meaningful) but drops
/// everything that varies between a tag and a YouTube title.
fn norm_loose(s: &str) -> String {
    s.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// The forms a Discogs video title might match a track under: the whole title,
/// and the tail after each separator. Uploaders stack prefixes — `Artist -
/// Title`, `Label • Artist - Release | A1 Title` — so every tail is a candidate,
/// and one of them is usually the bare track. Normalizing *after* the split is
/// what makes the separators survive long enough to be useful.
fn video_title_candidates(title: &str) -> Vec<String> {
    const SEPARATORS: [char; 5] = ['-', '–', '—', '|', '•'];
    let mut out = vec![norm_loose(title)];
    let mut rest = title;
    while let Some(i) = rest.find(SEPARATORS) {
        rest = &rest[i + rest[i..].chars().next().map_or(1, |c| c.len_utf8())..];
        let cand = norm_loose(rest);
        if !cand.is_empty() && !out.contains(&cand) {
            out.push(cand);
        }
    }
    out.retain(|c| !c.is_empty());
    out
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
    tracklist: Vec<TracklistEntry>,
    #[serde(default, deserialize_with = "null_as_default")]
    videos: Vec<VideoEntry>,
}

#[derive(Debug, Deserialize)]
struct ReleaseLabel {
    #[serde(default, deserialize_with = "null_as_default")]
    name: String,
    #[serde(default, deserialize_with = "null_as_default")]
    catno: String,
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
    fn into_detail(self) -> ReleaseDetail {
        // Discogs lists labels in release order; the first is the primary one.
        let (label, catalog_number) = match self.labels.into_iter().next() {
            Some(l) => (none_if_empty(l.name), none_if_empty(l.catno)),
            None => (None, None),
        };
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

fn none_if_empty(s: String) -> Option<String> {
    if s.trim().is_empty() {
        None
    } else {
        Some(s)
    }
}

/// Parse the `Retry-After` header Discogs sends on a 429 (delta-seconds form)
/// into a wait duration. `None` if the header is absent or unparseable, leaving
/// the caller to fall back to its own backoff.
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
            // 429 deserves a recognizable message so the caller can back off.
            let body = resp.into_string().unwrap_or_default();
            if code == 429 {
                Error::Network(format!("Discogs rate limited (HTTP 429): {body}"))
            } else {
                Error::Network(format!("Discogs HTTP {code}: {body}"))
            }
        }
        ureq::Error::Transport(t) => Error::Network(format!("transport: {t}")),
    }
}

/// Decode arbitrary image bytes (Discogs returns JPEG), downscale to a
/// `max_side`-pixel square, re-encode as PNG. Returns `None` on any failure,
/// which the caller treats as "no usable artwork" and moves on.
fn downscale_png(bytes: &[u8], max_side: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(bytes).ok()?;
    let thumb = img.thumbnail(max_side, max_side);
    let mut out = Vec::new();
    thumb
        .write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
        .ok()?;
    Some(out)
}

// `read_to_end` is from std::io::Read — pull it in for the download path.
use std::io::Read;

#[cfg(test)]
mod tests {
    use super::*;

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
        }
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
        let detail: ReleaseDetail = serde_json::from_str::<ReleaseResponse>(json).unwrap().into_detail();
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
        assert_eq!(id("https://www.youtube.com/watch?v=dQw4w9WgXcQ").as_deref(), Some("dQw4w9WgXcQ"));
        assert_eq!(id("http://youtube.com/watch?v=abc_-123&t=42").as_deref(), Some("abc_-123"));
        // The `v=` parameter isn't always first.
        assert_eq!(id("https://www.youtube.com/watch?t=9&v=xyz789").as_deref(), Some("xyz789"));
        assert_eq!(id("https://youtu.be/dQw4w9WgXcQ?t=30").as_deref(), Some("dQw4w9WgXcQ"));
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
        let left: Vec<&str> = d.unmatched_videos().iter().map(|(_, v)| v.uri.as_str()).collect();
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
            video("https://youtu.be/v1", "DIFF006 • Skudge - Meadow | A1 Meadow"),
            video("https://youtu.be/v2", "DIFF006 • Skudge - Meadow | B1 Break2"),
            video("https://youtu.be/v3", "DIFF006 • Skudge - Meadow | B2 Break2 KW Refix"),
        ];
        assert_eq!(d.video_matches(), vec![Some(0), Some(1), Some(2)]);
        assert!(d.unmatched_videos().is_empty());
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
            genre: Some("House".into()),       // differs → replaced
            year: Some(2001),                  // differs → replaced
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
                artists: vec![CollectionArtist { name: "Plastikman (2)".into() }],
                labels: vec![ReleaseLabel {
                    name: "Plus 8".into(),
                    catno: "PLUS8 024".into(),
                }],
                formats: vec![CollectionFormat {
                    name: "Vinyl".into(),
                    descriptions: vec!["12\"".into(), "45 RPM".into()],
                }],
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
