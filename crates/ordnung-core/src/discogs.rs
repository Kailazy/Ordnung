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
/// Max side of a cached vinyl cover PNG. Bigger than the table thumbnail
/// ([`THUMB_MAX_SIDE`]) because the "My Vinyl Collection" grid renders large
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
    #[serde(default)]
    results: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    id: u64,
    #[serde(default)]
    thumb: String,
    /// Full-size release image. Empty when Discogs has no high-res cover.
    #[serde(default)]
    cover_image: String,
    /// "Artist - Title" as Discogs labels the release.
    #[serde(default)]
    title: String,
    #[serde(default)]
    year: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    label: Vec<String>,
    #[serde(default)]
    format: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct IdentityResponse {
    #[serde(default)]
    username: String,
}

#[derive(Debug, Deserialize)]
struct CollectionResponse {
    #[serde(default)]
    pagination: CollectionPagination,
    #[serde(default)]
    releases: Vec<CollectionItem>,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionPagination {
    #[serde(default)]
    pages: u32,
}

/// One item in a collection folder. The bulk of the metadata lives under
/// `basic_information`; `id`/`instance_id`/`date_added` are on the item itself.
#[derive(Debug, Deserialize)]
struct CollectionItem {
    #[serde(default)]
    id: u64,
    #[serde(default)]
    instance_id: u64,
    #[serde(default)]
    date_added: String,
    #[serde(default)]
    basic_information: BasicInformation,
}

#[derive(Debug, Default, Deserialize)]
struct BasicInformation {
    #[serde(default)]
    title: String,
    year: Option<u16>,
    #[serde(default)]
    thumb: String,
    #[serde(default)]
    cover_image: String,
    #[serde(default)]
    artists: Vec<CollectionArtist>,
    #[serde(default)]
    labels: Vec<ReleaseLabel>,
    #[serde(default)]
    formats: Vec<CollectionFormat>,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionArtist {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Default, Deserialize)]
struct CollectionFormat {
    #[serde(default)]
    name: String,
    #[serde(default)]
    descriptions: Vec<String>,
}

impl CollectionItem {
    /// Build a [`VinylRecord`], or `None` if this item isn't a vinyl pressing.
    /// Discogs lists CDs, files and cassettes in the same collection; the "My
    /// Vinyl Collection" view is records only, so non-vinyl formats are dropped.
    fn into_record(self) -> Option<VinylRecord> {
        let bi = self.basic_information;
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
            instance_id: self.instance_id,
            release_id: self.id,
            title: bi.title,
            artist,
            year: bi.year.filter(|y| *y > 0),
            label,
            catalog_number,
            format,
            thumb_url: none_if_empty(bi.thumb),
            cover_url: none_if_empty(bi.cover_image),
            added: none_if_empty(self.date_added),
            has_cover: false,
        })
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
    #[serde(default)]
    id: u64,
    #[serde(default)]
    title: String,
    year: Option<u16>,
    #[serde(default)]
    released: String,
    #[serde(default)]
    country: String,
    #[serde(default)]
    genres: Vec<String>,
    #[serde(default)]
    styles: Vec<String>,
    #[serde(default)]
    labels: Vec<ReleaseLabel>,
}

#[derive(Debug, Deserialize)]
struct ReleaseLabel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    catno: String,
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
        }
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
    fn collection_item_skips_non_vinyl() {
        let mut item = vinyl_item();
        item.basic_information.formats = vec![CollectionFormat {
            name: "CD".into(),
            descriptions: vec!["Album".into()],
        }];
        assert!(item.into_record().is_none());
    }
}
