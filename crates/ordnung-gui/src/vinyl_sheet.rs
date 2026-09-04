//! The record sheet: one vinyl record's full tracklist, with a way to hear every
//! position it can play.
//!
//! Two sources feed it. Tracks you own digitally play through the app's own
//! audio engine (lossless, analyzed, in the player bar). Everything else falls
//! back to the YouTube videos the Discogs community attached to the release,
//! played in the native mini-player (see [`crate::webview`]) so a record you own
//! only on wax is still listenable. Records where neither exists say so rather
//! than offering a dead button.
//!
//! The tracklist and videos come from `GET /releases/{id}`, cached in the
//! catalog's `release_cache` — so a record opened once needs no network again.

use super::*;
use crate::dig::strip_disambiguator;

/// One release detail fetched for the sheet: which record it was for, and either
/// the detail or the error to show in its place.
pub(crate) struct SheetFetched {
    pub release_id: u64,
    pub result: Result<discogs::ReleaseDetail, String>,
}

/// A local catalog track linked to the open record.
pub(crate) struct SheetLocal {
    pub id: Id,
    pub title: String,
    pub path: PathBuf,
    pub bpm: Option<f32>,
    pub camelot: Option<String>,
}

/// What plays one row of the sheet.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum SheetSource {
    /// A file in the catalog: index into [`VinylSheet::local`].
    Local(usize),
    /// A YouTube video on the release: index into the detail's `videos`.
    Video(usize),
    /// Neither — the record lists this track but nothing can play it.
    None,
}

/// One line of the sheet: a Discogs tracklist position, or a leftover video
/// (album rip, live set) shown under its own heading.
pub(crate) struct SheetRow {
    pub position: String,
    pub title: String,
    /// Duration as Discogs writes it; blank when it lists none.
    pub duration: String,
    /// Who performed this track, shown when the track credits someone the
    /// release itself doesn't name — a compilation's "Various", or a split.
    /// `None` on a single-artist album, where repeating the release's own
    /// artist on every row would be noise rather than information.
    pub artist: Option<String>,
    pub source: SheetSource,
    /// A video for a row whose primary source is a local file, so the sheet can
    /// still offer "watch it" alongside "play my copy".
    pub also_video: Option<usize>,
}

/// The open record sheet.
pub(crate) struct VinylSheet {
    /// Cover-cache key, when this record is in the collection or wantlist.
    /// `None` for a record opened from a dig: it isn't in either list, so it has
    /// no cached cover to key — `cover_url` carries its art instead.
    pub key: Option<VinylCoverKey>,
    /// Cover thumbnail URL, loaded through [`App::dig_covers`] (which the strip
    /// already fills for the same release). Set for *every* record, keyed or
    /// not: a dug record has no cached cover at all, and a keyed one loses its
    /// cached cover the moment it leaves its list, while its sheet stays open.
    /// This is what the sheet falls back to in both cases, so don't drop it on
    /// the assumption that a key makes it redundant.
    pub cover_url: Option<String>,
    pub release_id: u64,
    pub title: String,
    pub artist: String,
    /// Second header line, e.g. `1993 · Vinyl, 12" · Warp WAP42`.
    pub sub: String,
    pub detail: Option<discogs::ReleaseDetail>,
    pub local: Vec<SheetLocal>,
    pub rows: Vec<SheetRow>,
    /// Videos no track claimed, as indices into the detail's `videos`.
    pub extra_videos: Vec<usize>,
    pub loading: bool,
    pub error: Option<String>,
    /// The video index currently loaded in the mini-player, for the row marker.
    pub playing_video: Option<usize>,
    /// That video's YouTube page, kept so a player error can hand it to a
    /// browser without re-deriving which row it came from.
    pub video_uri: Option<String>,
    /// Fraction the transport scrubber is being dragged to, while a drag is in
    /// flight. The bar paints this instead of the live position, so the
    /// playhead follows the pointer and doesn't fight the poll answers still
    /// arriving from the old spot. The seek fires on release.
    pub video_scrub: Option<f32>,
    /// Set when the user hit play on the cover rather than opening the sheet:
    /// start the record as soon as there's a tracklist to start it from.
    pub pending_play: bool,
    /// Lowest current marketplace listing for this release, once looked up.
    /// `Loading` while the request is out, `Ready(None)` when nothing is for
    /// sale (or Discogs blocks the release from sale).
    pub price: PriceState,
}

/// The sheet's marketplace price lookup.
pub(crate) enum PriceState {
    /// Not asked yet — a record already in a list may have a synced price to
    /// show without spending a request.
    Idle,
    Loading,
    Ready(Option<discogs::MarketPrice>),
    /// Nothing for sale on *this* pressing, but another pressing of the same
    /// record has copies. Promos and white labels are routinely dead ends while
    /// the standard pressing is stocked, and "no copies for sale" would be a
    /// misleading answer to "can I buy this record?".
    Elsewhere {
        version: Box<discogs::MasterVersion>,
        price: discogs::MarketPrice,
    },
    Failed,
}

/// Find a pressing of the same record that can actually be bought, for a
/// release with nothing listed. Returns the pressing and its lowest price.
///
/// Runs on the price worker, so every call here is off the UI thread. Costs at
/// most `1 + MAX_VERSION_PRICES` paced requests, and only for a record that
/// came back with no copies at all — the common case never reaches this.
fn sheet_alternative(
    client: &discogs::Client,
    db: &Path,
    release_id: u64,
) -> Option<(discogs::MasterVersion, discogs::MarketPrice)> {
    /// How many sibling pressings to price before giving up. Versions arrive
    /// most-owned first, so the buyable one is nearly always in the first few,
    /// and each check is a rate-limited request.
    const MAX_VERSION_PRICES: usize = 4;

    // The master id rides along on the release detail the sheet already caches,
    // so this usually costs nothing.
    let id = release_id.to_string();
    let master_id = Catalog::open(db)
        .ok()
        .and_then(|cat| cat.cached_release(&id).ok().flatten())
        .and_then(|d| d.master_id)
        .or_else(|| client.fetch_release(&id).ok().and_then(|d| d.master_id))?;

    let versions = client.master_versions(master_id).ok()?;
    for v in versions
        .into_iter()
        .filter(|v| v.release_id != release_id)
        .take(MAX_VERSION_PRICES)
    {
        if let Ok(Some(price)) = client.marketplace_price(v.release_id) {
            return Some((v, price));
        }
    }
    None
}

/// Render a marketplace price for the sheet header — symbol where there is
/// one, and always the exact figure: this is the number the user decides on.
fn fmt_market_price(p: &discogs::MarketPrice) -> String {
    let code = p.currency.trim().to_uppercase();
    let symbol = match code.as_str() {
        "USD" | "CAD" | "AUD" | "NZD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        _ => "",
    };
    if symbol.is_empty() {
        format!("{:.2} {code}", p.value)
    } else {
        format!("{symbol}{:.2}", p.value)
    }
}

impl App {
    /// Open the record sheet for a grid cell, fetching its tracklist and videos
    /// if they aren't cached yet. Re-opening the record that's already open is a
    /// no-op, so a second click doesn't restart a fetch.
    pub(crate) fn open_vinyl_sheet(&mut self, key: VinylCoverKey, ctx: &egui::Context) {
        if self
            .vinyl_sheet
            .as_ref()
            .is_some_and(|s| s.key == Some(key))
        {
            return;
        }
        let Some(record) = self.vinyl_record(key) else {
            return;
        };
        // Opening a different record replaces the sheet; its video would
        // otherwise play on under a tracklist it doesn't belong to (and
        // `playing_video` indexes the old release's videos).
        self.stop_sheet_video();
        let sub = [
            record.year.map(|y| y.to_string()),
            record.format.clone(),
            match (&record.label, &record.catalog_number) {
                (Some(l), Some(c)) => Some(format!("{l} {c}")),
                (Some(l), None) => Some(l.clone()),
                (None, Some(c)) => Some(c.clone()),
                (None, None) => None,
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");

        self.vinyl_sheet = Some(VinylSheet {
            key: Some(key),
            // Set even though the cache normally serves a keyed record's cover:
            // it's the fallback for when the record leaves its list. See the
            // field's docs.
            cover_url: record.thumb_url.clone(),
            release_id: record.release_id,
            title: record.title.clone(),
            artist: record.artist.clone(),
            sub,
            detail: None,
            local: self.sheet_local_tracks(record.release_id),
            rows: Vec::new(),
            extra_videos: Vec::new(),
            loading: true,
            error: None,
            playing_video: None,
            video_uri: None,
            video_scrub: None,
            pending_play: false,
            // A synced record already has a price on file; anything else asks
            // the marketplace when the sheet opens.
            price: match (record.price, record.price_currency.clone()) {
                (Some(value), Some(currency)) => {
                    PriceState::Ready(Some(discogs::MarketPrice { value, currency }))
                }
                _ => PriceState::Idle,
            },
        });
        self.spawn_sheet_fetch(record.release_id, ctx.clone());
        self.spawn_sheet_price(record.release_id, ctx.clone());
    }

    /// Open the sheet for a bare Discogs release — a record from a dig, which
    /// isn't in the collection or the wantlist and so has no cache key. The
    /// tracklist, videos and "play" behaviour are all driven by the release id,
    /// so everything below works the same; only the cover comes from a URL
    /// instead of the local cache.
    pub(crate) fn open_release_sheet(
        &mut self,
        release_id: u64,
        artist: String,
        title: String,
        sub: String,
        cover_url: Option<String>,
        ctx: &egui::Context,
    ) {
        if self
            .vinyl_sheet
            .as_ref()
            .is_some_and(|s| s.release_id == release_id)
        {
            return;
        }
        self.stop_sheet_video();
        self.vinyl_sheet = Some(VinylSheet {
            key: None,
            cover_url,
            release_id,
            title,
            artist,
            sub,
            detail: None,
            // A dug record can still turn out to be one you have digitally —
            // the link map is by release id, not by list membership.
            local: self.sheet_local_tracks(release_id),
            rows: Vec::new(),
            extra_videos: Vec::new(),
            loading: true,
            error: None,
            playing_video: None,
            video_uri: None,
            video_scrub: None,
            pending_play: false,
            price: PriceState::Idle,
        });
        self.spawn_sheet_fetch(release_id, ctx.clone());
        self.spawn_sheet_price(release_id, ctx.clone());
    }

    /// Open the record sheet for the release a *library track* is matched to —
    /// the inspector's "View release".
    ///
    /// The sheet is the same one the vinyl grid and a dig open, so the answer to
    /// "what else is on this record, and do I own it?" is one view rather than
    /// three. It already draws the tracklist, marks which positions exist in the
    /// catalog, and shows "✓ In collection" / "✓ In wantlist" for the release —
    /// so this only has to supply the identity for its header.
    ///
    /// Identity comes from what's already local, so the sheet opens populated
    /// rather than blank-until-fetched: the release title from the detail cache
    /// when it's warm (falling back to the track's album tag, then its title),
    /// and the artist from the track's own tag, since `ReleaseDetail` carries no
    /// release-level artist. The cover URL recorded with the match feeds the
    /// same lazy image cache a dug record uses. Every one of these is
    /// best-effort — `open_release_sheet` fetches the authoritative tracklist
    /// regardless, and a missing cover just opens the sheet without art.
    pub(crate) fn open_track_release_sheet(&mut self, id: Id, ctx: &egui::Context) {
        let Some(release_id) = self.track_releases.get(&id).copied() else {
            return;
        };
        let cat = Catalog::open(&self.db_path).ok();
        let track = cat.as_ref().and_then(|c| c.get_track(id).ok());
        let artist = track
            .as_ref()
            .and_then(|t| t.tags.artist.clone())
            .unwrap_or_default();
        let cached = cat
            .as_ref()
            .and_then(|c| c.cached_release(&release_id.to_string()).ok())
            .flatten();
        let title = cached
            .as_ref()
            .map(|d| d.title.clone())
            .or_else(|| track.as_ref().and_then(|t| t.tags.album.clone()))
            .or_else(|| track.as_ref().and_then(|t| t.tags.title.clone()))
            .unwrap_or_else(|| format!("Release {release_id}"));
        // Same citation line the record search builds, from whatever the cache
        // has: year · label · catalogue number.
        let sub = cached
            .as_ref()
            .map(|d| {
                let year = d.year.map(|y| y.to_string()).unwrap_or_default();
                let imprint = match (
                    d.label.as_deref().unwrap_or("").trim(),
                    d.catalog_number.as_deref().unwrap_or("").trim(),
                ) {
                    ("", "") => String::new(),
                    (l, "") => l.to_string(),
                    ("", c) => c.to_string(),
                    (l, c) => format!("{l} {c}"),
                };
                [year.as_str(), imprint.as_str()]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" · ")
            })
            .unwrap_or_default();
        let cover_url = cat.as_ref().and_then(|c| c.external_cover_url(id).ok()).flatten();
        self.open_release_sheet(release_id, artist, title, sub, cover_url, ctx);
    }

    /// The catalog tracks linked to `release_id`, with the analysis figures the
    /// sheet shows. One small read per linked track — a record is a handful of
    /// tracks, so this stays on the UI thread like the other inline reads.
    fn sheet_local_tracks(&self, release_id: u64) -> Vec<SheetLocal> {
        let ids = self
            .vinyl_links
            .get(&release_id)
            .cloned()
            .unwrap_or_default();
        let Ok(cat) = Catalog::open(&self.db_path) else {
            return Vec::new();
        };
        ids.iter()
            .filter_map(|id| {
                let t = cat.get_track(*id).ok()?;
                let analysis = cat.get_analysis(*id).ok().flatten();
                let title = t.tags.title.clone().filter(|s| !s.trim().is_empty())?;
                Some(SheetLocal {
                    id: *id,
                    title,
                    path: PathBuf::from(&t.source_path),
                    bpm: analysis.as_ref().and_then(|a| a.bpm),
                    camelot: analysis
                        .as_ref()
                        .and_then(|a| a.key)
                        .map(|k| k.camelot().label()),
                })
            })
            .collect()
    }

    /// Look up the lowest marketplace listing for the open sheet's record, off
    /// the UI thread. One request; a record whose price came from a sync
    /// already shows that and this refreshes it, since a synced price can be
    /// weeks stale and the sheet is where the user decides whether to buy.
    fn spawn_sheet_price(&mut self, release_id: u64, ctx: egui::Context) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            return;
        }
        if let Some(s) = self.vinyl_sheet.as_mut() {
            // Keep a known price on screen while the refresh runs, rather than
            // blanking it — `Loading` only replaces "nothing shown yet".
            if matches!(s.price, PriceState::Idle) {
                s.price = PriceState::Loading;
            }
        }
        let (tx, rx) = mpsc::channel();
        self.sheet_price_rx = Some(rx);
        let db = self.db_path.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let mine = client.marketplace_price(release_id);
            // Priced fine, or the request failed — either way there's nothing
            // more to ask.
            let found = match &mine {
                Ok(Some(_)) | Err(_) => {
                    let _ = tx.send((release_id, mine.map(|p| (p, None))));
                    ctx.request_repaint();
                    return;
                }
                Ok(None) => None,
            };
            // Nothing for sale here. This is often a promo or a white label
            // whose standard pressing is well stocked, so ask the master which
            // other pressings exist and price the most-owned one.
            let alt = sheet_alternative(&client, &db, release_id);
            let _ = tx.send((release_id, Ok((found, alt))));
            ctx.request_repaint();
        });
    }

    /// Adopt a finished price lookup onto the sheet it was asked for.
    pub(crate) fn poll_sheet_price(&mut self) {
        let Some(rx) = &self.sheet_price_rx else {
            return;
        };
        let Ok((release_id, result)) = rx.try_recv() else {
            return;
        };
        self.sheet_price_rx = None;
        let Some(sheet) = self.vinyl_sheet.as_mut() else {
            return;
        };
        // The user opened a different record while this was in flight.
        if sheet.release_id != release_id {
            return;
        }
        sheet.price = match result {
            Ok((Some(p), _)) => PriceState::Ready(Some(p)),
            Ok((None, Some((version, price)))) => PriceState::Elsewhere {
                version: Box::new(version),
                price,
            },
            Ok((None, None)) => PriceState::Ready(None),
            Err(_) => PriceState::Failed,
        };
    }

    /// Resolve one release's detail off the UI thread — cache first, network on
    /// a miss. A missing token is only an error when the release isn't cached,
    /// so a record opened before still works offline.
    fn spawn_sheet_fetch(&mut self, release_id: u64, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.sheet_rx = Some(rx);
        let db = self.db_path.clone();
        let token = self.discogs_token();
        thread::spawn(move || {
            let id = release_id.to_string();
            let result = Catalog::open(&db)
                .map_err(|e| e.to_string())
                .and_then(|cat| {
                    match cat.cached_release(&id) {
                        Ok(Some(d)) => return Ok(d),
                        Ok(None) => {}
                        // A cache read failure isn't fatal — fall through to the
                        // network and let that decide.
                        Err(_) => {}
                    }
                    if token.trim().is_empty() {
                        return Err("No Discogs token set. Add one in Settings to load \
                                    tracklists and videos."
                            .to_string());
                    }
                    let client = discogs::Client::new(
                        token,
                        "Ordnung/0.1 +https://kailazy.github.io/Ordnung/",
                    );
                    cat.release_cached_or(&id, || client.fetch_release(&id))
                        .map_err(|e| e.to_string())
                });
            let _ = tx.send(SheetFetched { release_id, result });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished release fetch and build the sheet's rows from it.
    pub(crate) fn poll_vinyl_sheet(&mut self) {
        let Some(rx) = &self.sheet_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.sheet_rx = None;
        let Some(sheet) = self.vinyl_sheet.as_mut() else {
            return;
        };
        // The user may have moved on to another record while this was in flight.
        if sheet.release_id != msg.release_id {
            return;
        }
        sheet.loading = false;
        match msg.result {
            Ok(detail) => {
                // The name in the sheet's header, to suppress per-track credits
                // that only repeat it.
                let release_artist = sheet.artist.trim().to_string();
                let release_artist = release_artist.as_str();
                let local_titles: Vec<String> =
                    sheet.local.iter().map(|l| l.title.clone()).collect();
                let files = detail.file_matches(&local_titles);
                let videos = detail.video_matches();
                sheet.rows = detail
                    .tracklist
                    .iter()
                    .enumerate()
                    .map(|(i, t)| {
                        let file = files.get(i).copied().flatten();
                        let video = videos.get(i).copied().flatten();
                        SheetRow {
                            position: t.position.clone(),
                            title: t.title.clone(),
                            duration: t.duration.clone(),
                            // Only worth showing when it says something the
                            // header doesn't. On a compilation the header reads
                            // "Various", so every track's credit is news; on a
                            // single-artist album Discogs still sometimes
                            // repeats the release artist per track, and drawing
                            // it down the whole list adds nothing.
                            artist: t
                                .artist
                                .as_deref()
                                .map(str::trim)
                                .filter(|a| {
                                    !a.is_empty() && !a.eq_ignore_ascii_case(release_artist)
                                })
                                .map(str::to_string),
                            // Your own file wins: it's lossless, analyzed, and
                            // plays in the real player bar.
                            source: match (file, video) {
                                (Some(f), _) => SheetSource::Local(f),
                                (None, Some(v)) => SheetSource::Video(v),
                                (None, None) => SheetSource::None,
                            },
                            also_video: file.and(video),
                        }
                    })
                    .collect();
                sheet.extra_videos = detail.unmatched_videos().iter().map(|(i, _)| *i).collect();
                // A release with no tracklist at all (Discogs has plenty) still
                // has its videos — show them as the record's only contents
                // rather than an empty sheet.
                sheet.detail = Some(detail);
            }
            Err(e) => sheet.error = Some(e),
        }
    }

    /// Play the record from `row` onwards. Local rows go to the audio engine;
    /// video rows queue every video from there to the end of the record in the
    /// mini-player, so a side plays through without further clicks.
    fn play_sheet_row(&mut self, row: usize, frame: &eframe::Frame) {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return;
        };
        let Some(source) = sheet.rows.get(row).map(|r| r.source) else {
            return;
        };
        match source {
            SheetSource::Local(i) => {
                let Some(local) = sheet.local.get(i) else {
                    return;
                };
                let (id, path) = (local.id, local.path.clone());
                // Never leave a video playing under the local track.
                self.stop_sheet_video();
                self.play_track(id, path);
            }
            SheetSource::Video(_) => {
                let ids = self.sheet_video_queue(row);
                self.start_sheet_video(ids, row, frame);
            }
            SheetSource::None => {}
        }
    }

    /// The YouTube ids to hand the mini-player when starting at `row`: that row's
    /// video and every later video on the record, so playback continues down the
    /// side. Rows backed by a local file are skipped — mixing the two engines
    /// mid-queue would play two things at once.
    fn sheet_video_queue(&self, row: usize) -> Vec<String> {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return Vec::new();
        };
        let Some(detail) = sheet.detail.as_ref() else {
            return Vec::new();
        };
        sheet.rows[row..]
            .iter()
            .filter_map(|r| match r.source {
                SheetSource::Video(v) => detail.videos.get(v),
                _ => None,
            })
            .filter_map(|v| v.youtube_id().map(str::to_string))
            .collect()
    }

    /// Hand a video queue to the native mini-player, pausing local audio first.
    /// Falls back to opening the video on youtube.com when there's no panel to
    /// play it in (non-macOS, or no window handle this frame).
    fn start_sheet_video(&mut self, ids: Vec<String>, row: usize, frame: &eframe::Frame) {
        let Some(sheet) = self.vinyl_sheet.as_ref() else {
            return;
        };
        let video = sheet.rows.get(row).and_then(|r| match r.source {
            SheetSource::Video(v) => Some(v),
            _ => r.also_video,
        });
        let uri = video
            .and_then(|v| sheet.detail.as_ref()?.videos.get(v))
            .map(|v| v.uri.clone());
        let title = match sheet.rows.get(row) {
            Some(r) => format!("{} — {}", sheet.artist, r.title),
            None => sheet.title.clone(),
        };

        if ids.is_empty() {
            if let Some(uri) = uri {
                open_url(&uri);
            }
            return;
        }
        // One sound at a time: the video takes over from the player bar.
        if let Some(a) = self.audio.as_mut() {
            if a.is_active() {
                a.toggle_pause();
            }
        }
        if webview::play(frame, &ids, &title) {
            if let Some(sheet) = self.vinyl_sheet.as_mut() {
                sheet.playing_video = video;
                sheet.video_uri = uri;
            }
        } else if let Some(uri) = uri {
            open_url(&uri);
        }
    }

    /// Drive the mini-player: let it advance its own queue, and notice a page
    /// that never produced a video — a pulled video, or a consent wall YouTube
    /// wants clicked through. Those go to a real browser rather than sitting
    /// there blank. Called every frame; the panel keeps playing a record even
    /// once the sheet that started it is closed.
    ///
    /// The panel is an AppKit window, not an egui surface, so nothing the user
    /// does *in* it wakes this loop. While it's up we ask for the repaint that
    /// brings us back — otherwise an idle app leaves the queue un-advanced and
    /// the styling un-refreshed until some unrelated event happens to tick.
    pub(crate) fn drive_video_player(&mut self, ctx: &egui::Context) {
        webview::poll();
        if let Some(next) = webview::next_poll_in() {
            ctx.request_repaint_after(next);
        }
        // The sheet's transport runs its playhead forward between polls, so
        // while a video is rolling under an open sheet the bar wants every
        // frame, not just the poll ticks — otherwise the scrubber steps four
        // times a second instead of gliding.
        if self.vinyl_sheet.is_some() && webview::transport().playing {
            ctx.request_repaint();
        }
        if self
            .vinyl_sheet
            .as_ref()
            .is_none_or(|s| s.playing_video.is_none())
        {
            return;
        }
        if webview::status() != webview::PlayerStatus::Stuck {
            return;
        }
        let uri = self.vinyl_sheet.as_ref().and_then(|s| s.video_uri.clone());
        self.stop_sheet_video();
        self.status = "That video wouldn't play here. Opening it on YouTube.".into();
        if let Some(uri) = uri {
            open_url(&uri);
        }
    }

    /// Close the mini-player and forget which row it was on.
    pub(crate) fn stop_sheet_video(&mut self) {
        webview::close();
        if let Some(sheet) = self.vinyl_sheet.as_mut() {
            sheet.playing_video = None;
            sheet.video_uri = None;
            sheet.video_scrub = None;
        }
    }

    /// Start the whole record: the first row that can play anything.
    fn play_sheet_from_start(&mut self, frame: &eframe::Frame) {
        let first = self.vinyl_sheet.as_ref().and_then(|s| {
            s.rows
                .iter()
                .position(|r| !matches!(r.source, SheetSource::None))
        });
        if let Some(row) = first {
            self.play_sheet_row(row, frame);
        }
    }

    /// Draw the open record sheet. Returns nothing — every action is applied
    /// before it returns, so the caller just calls it once per frame.
    pub(crate) fn draw_vinyl_sheet(&mut self, ctx: &egui::Context, frame: &eframe::Frame) {
        if self.vinyl_sheet.is_none() {
            return;
        }
        // The panel can be closed by its own title-bar button, which we only
        // find out about by asking.
        if let Some(sheet) = self.vinyl_sheet.as_mut() {
            if sheet.playing_video.is_some() && !webview::is_open() {
                sheet.playing_video = None;
            }
        }
        // A play started from the grid waits here for the tracklist to arrive.
        let start_now = self.vinyl_sheet.as_mut().is_some_and(|s| {
            let go = s.pending_play && !s.loading;
            if go {
                s.pending_play = false;
            }
            go
        });
        if start_now {
            self.play_sheet_from_start(frame);
        }

        // Snapshot what the closure paints so it never borrows `self` (actions
        // below need it mutably).
        let (
            key,
            cover_url,
            title,
            artist,
            sub,
            release_id,
            loading,
            error,
            playing_video,
            video_open,
            has_video,
        ) = {
            let s = self.vinyl_sheet.as_ref().unwrap();
            (
                s.key,
                s.cover_url.clone(),
                s.title.clone(),
                s.artist.clone(),
                s.sub.clone(),
                s.release_id,
                s.loading,
                s.error.clone(),
                s.playing_video,
                webview::is_open(),
                // Can anything on this record play through the mini-player? Only
                // then does the transport's slot need holding open.
                s.rows
                    .iter()
                    .any(|r| matches!(r.source, SheetSource::Video(_)) || r.also_video.is_some())
                    || !s.extra_videos.is_empty(),
            )
        };
        // Cover from the local cache when it has one, falling back to the URL.
        // The fallback covers a keyed record too, not just a dug one: a key is
        // live only while the record is in a list, and the sheet outlives that.
        // Removing the record evicts its texture (and deletes the row its PNG
        // sits on); leaving the Vinyl view clears the cache outright. Both used
        // to blank an open sheet. `dig_cover` refetches on a miss, so the worst
        // case is the cover returning a moment later rather than vanishing.
        let cached = key.and_then(|k| match self.vinyl_covers.get(&k) {
            Some(ThumbState::Ready(Some(t))) => Some(t.clone()),
            _ => None,
        });
        let cover = match cached {
            Some(t) => Some(t),
            None => cover_url
                .as_deref()
                .and_then(|u| self.dig_cover(u))
                .cloned(),
        };
        let now_playing_id = self.audio.as_ref().and_then(|a| a.current());
        // The track that is actually *sounding*, as against the one merely
        // loaded in the player. `current()` survives a pause and a stop, so a
        // row keyed on it alone keeps showing its pause icon long after the
        // music ended — and a second row lighting up next to it made the sheet
        // claim two tracks were playing at once.
        let sounding_id = self.audio.as_ref().and_then(|a| {
            a.current()
                .filter(|id| a.state_for(*id) == PlayState::Playing)
        });
        // Is *this record* sounding right now, and through which engine? One
        // button covers both, so it has to know which one to talk to.
        let record_play = {
            let s = self.vinyl_sheet.as_ref().unwrap();
            if video_open {
                if webview::transport().playing {
                    RecordPlay::Playing(PlayEngine::Video)
                } else {
                    RecordPlay::Paused(PlayEngine::Video)
                }
            } else {
                // A local track counts only while it's one of this record's own.
                let mine = now_playing_id.is_some_and(|id| s.local.iter().any(|t| t.id == id));
                match (mine, now_playing_id) {
                    (true, Some(id))
                        if self
                            .audio
                            .as_ref()
                            .is_some_and(|a| a.state_for(id) == PlayState::Playing) =>
                    {
                        RecordPlay::Playing(PlayEngine::Audio)
                    }
                    (true, _) => RecordPlay::Paused(PlayEngine::Audio),
                    _ => RecordPlay::Stopped,
                }
            }
        };
        // Digging is offered for a record that's in a list (it's the seed of a
        // dig).
        let can_dig = key.is_some_and(|k| self.can_dig(k));
        // A record reached *by* a dig branches with the strip's own two
        // buttons — but the sheet is drawn over the strip, so it carries its
        // own copy rather than making the user move the window to reach them.
        // Only for the record the dig is actually standing on: taking a thread
        // out of a card you merely walked back to would branch from the head
        // instead of from what you're looking at.
        let branch = {
            let head = self.dig.as_ref().map(|d| d.head());
            match head {
                Some(h) if h.release_id == release_id => Some((
                    strip_disambiguator(&h.artist).to_string(),
                    h.label.clone(),
                    !h.artist_ids.is_empty(),
                    !h.label_ids.is_empty(),
                    self.dig.as_ref().is_some_and(|d| d.pending.is_some()),
                )),
                _ => None,
            }
        };
        // Where this record stands on Discogs right now. Read from the same
        // membership sets the grid uses, so the sheet agrees with the wall
        // behind it, and both update together on the reload an edit triggers.
        let in_collection = self.vinyl_owned.contains(&release_id);
        let in_wantlist = self.vinyl_wanted.contains(&release_id);
        let price_line = match &self.vinyl_sheet.as_ref().map(|s| &s.price) {
            Some(PriceState::Ready(Some(p))) => Some(format!("From {}", fmt_market_price(p))),
            Some(PriceState::Ready(None)) => Some("No copies for sale".to_string()),
            Some(PriceState::Loading) => Some("Checking price…".to_string()),
            _ => None,
        };
        // A different pressing of the same record that *is* for sale. Snapshot
        // what the row needs so the window closure doesn't borrow the sheet.
        let alt = match self.vinyl_sheet.as_ref().map(|s| &s.price) {
            Some(PriceState::Elsewhere { version, price }) => Some((
                version.release_id,
                // The pressing detail is what distinguishes it from the one on
                // screen — "12\", White Label, Limited Edition" against the
                // promo the user is looking at.
                if version.format.trim().is_empty() {
                    version.title.clone()
                } else {
                    version.format.clone()
                },
                version.catno.clone(),
                fmt_market_price(price),
                self.vinyl_owned.contains(&version.release_id),
                self.vinyl_wanted.contains(&version.release_id),
            )),
            _ => None,
        };
        // An edit is already running: the buttons would queue a second job
        // against the same record, so they wait it out rather than misreport.
        let editing = self.is_busy();

        /// What the user clicked this frame, applied after the window closes its
        /// borrow of `self`.
        enum Act {
            Play(usize),
            /// Play the record, or pause/resume whatever of it is loaded.
            TogglePlay,
            PlayExtra(usize),
            Goto,
            Dig,
            /// Take a thread out of the record on screen — the strip's two
            /// branch buttons, mirrored here.
            Branch(crate::dig::DigThread),
            /// Add this record to that list, or take it off if it's there.
            ToggleList(VinylList),
            /// Want a *different* pressing of the same record — the one that
            /// has copies for sale.
            WantAlternative(u64),
        }
        let mut act: Option<Act> = None;
        // The transport bar's own action, kept apart from `act` so a scrub and
        // a row click in the same frame don't shadow each other.
        let mut video_act: Option<VideoAct> = None;
        let mut open = true;

        egui::Window::new(format!("{artist} — {title}"))
            .id(egui::Id::new(("vinyl-sheet", release_id)))
            .open(&mut open)
            .collapsible(false)
            // Fixed width: the sheet's content is a fixed-width layout (see the
            // `set_max_width` note below), so a horizontal drag would only pad
            // it. Height still follows the tracklist.
            .resizable([false, true])
            .default_width(SHEET_W)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                // Pin the content width rather than only flooring it. The sheet
                // auto-sizes, so any child that fills the width it is offered
                // (the transport's scrubber does) turns this frame's layout into
                // next frame's demand, and the window creeps wider every frame
                // until it hits the screen. A max as well as a min removes the
                // feedback path entirely.
                ui.set_min_width(SHEET_W);
                ui.set_max_width(SHEET_W);
                ui.add_space(2.0);
                ui.horizontal(|ui| {
                    // Cover.
                    const C: f32 = 120.0;
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(C, C), egui::Sense::hover());
                    match &cover {
                        Some(h) => {
                            egui::Image::new(h)
                                .fit_to_exact_size(egui::vec2(C, C))
                                .rounding(egui::Rounding::same(6.0))
                                .paint_at(ui, rect);
                        }
                        None => {
                            ui.painter().rect_filled(
                                rect,
                                egui::Rounding::same(6.0),
                                egui::Color32::from_gray(34),
                            );
                        }
                    }
                    ui.add_space(14.0);
                    ui.vertical(|ui| {
                        ui.label(egui::RichText::new(&artist).font(
                            crate::ui::tokens::font::strong(
                                crate::ui::tokens::font::headline().size,
                            ),
                        ));
                        ui.label(
                            egui::RichText::new(&title).font(crate::ui::tokens::font::headline()),
                        );
                        if !sub.is_empty() {
                            ui.label(egui::RichText::new(&sub).weak());
                        }
                        // What it costs, right under what it is — the sheet is
                        // where the buy decision happens.
                        if let Some(line) = &price_line {
                            ui.label(
                                egui::RichText::new(line)
                                    .color(egui::Color32::from_rgb(120, 200, 140)),
                            );
                        }
                        // This pressing is a dead end, but another isn't. Say
                        // which one and offer it, rather than leaving "no copies
                        // for sale" to imply the record can't be bought.
                        if let Some((
                            alt_id,
                            alt_fmt,
                            alt_catno,
                            alt_price,
                            alt_owned,
                            alt_wanted,
                        )) = &alt
                        {
                            ui.horizontal_wrapped(|ui| {
                                ui.spacing_mut().item_spacing.x = 4.0;
                                ui.label(egui::RichText::new("Another pressing:").small().weak());
                                let mut what = alt_fmt.clone();
                                if !alt_catno.trim().is_empty() {
                                    what = format!("{what} · {alt_catno}");
                                }
                                ui.label(egui::RichText::new(what).small());
                                ui.label(
                                    egui::RichText::new(format!("from {alt_price}"))
                                        .small()
                                        .color(egui::Color32::from_rgb(120, 200, 140)),
                                );
                                if ui
                                    .small_button("Open ↗")
                                    .on_hover_note("Open that pressing on discogs.com")
                                    .clicked()
                                {
                                    open_url(&format!("https://www.discogs.com/release/{alt_id}"));
                                }
                                // Want the pressing you can actually buy, not
                                // the promo you happened to land on.
                                let already = *alt_owned || *alt_wanted;
                                let want_tip = if already {
                                    "That pressing is already in one of your lists"
                                } else {
                                    "Add that pressing to your Discogs wantlist"
                                };
                                if ui
                                    .add_enabled(
                                        !already && !editing,
                                        egui::Button::new("＋ Wantlist").small(),
                                    )
                                    .on_hover_note(want_tip)
                                    .on_disabled_hover_text(crate::ui::hover::note(want_tip))
                                    .clicked()
                                {
                                    act = Some(Act::WantAlternative(*alt_id));
                                }
                            });
                        }
                        ui.add_space(10.0);
                        ui.horizontal(|ui| {
                            // One control, two states, drawn rather than
                            // labelled. A "Play"/"Pause" word changes width
                            // when it toggles and shoves the whole row sideways
                            // under the pointer, so the mark swaps inside a
                            // fixed square and everything after it stays put.
                            let playing = matches!(record_play, RecordPlay::Playing(_));
                            let play_tip = match record_play {
                                RecordPlay::Playing(_) => "Pause this record",
                                RecordPlay::Paused(_) => "Resume this record",
                                RecordPlay::Stopped => {
                                    "Play from the first track that has a source"
                                }
                            };
                            // Exactly as tall as the text buttons beside it.
                            // A hardcoded height drifts from whatever the font
                            // and padding actually produce, and a play square
                            // shorter than its neighbours sits inset from them
                            // top and bottom, so the row reads as misaligned.
                            // Measure the way egui sizes a button instead: one
                            // line of button text plus the vertical padding,
                            // floored at the interact height.
                            // Rounded because egui rounds a galley's height to
                            // whole pixels before padding it; the raw text
                            // height lands a fraction of a pixel short of the
                            // real buttons (26.94 against their 27).
                            let btn_h = (ui.text_style_height(&egui::TextStyle::Button).round()
                                + 2.0 * ui.spacing().button_padding.y)
                                .max(ui.spacing().interact_size.y);
                            let (rect, resp) = ui
                                .allocate_exact_size(egui::vec2(44.0, btn_h), egui::Sense::click());
                            let resp = resp.on_hover_note(play_tip);
                            // Same chrome the text buttons beside it wear, so
                            // the row reads as one set of controls.
                            let visuals = ui.style().interact(&resp);
                            ui.painter().rect(
                                rect.expand(visuals.expansion),
                                visuals.rounding,
                                visuals.weak_bg_fill,
                                visuals.bg_stroke,
                            );
                            crate::ui::icon::play_pause(
                                ui.painter(),
                                rect.center(),
                                visuals.fg_stroke.color,
                                playing,
                            );
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                act = Some(Act::TogglePlay);
                            }
                            if ui
                                .button("↗  Discogs")
                                .on_hover_note("Open this release on discogs.com")
                                .clicked()
                            {
                                open_url(&format!("https://www.discogs.com/release/{release_id}"));
                            }
                            // Collection and wantlist, each showing this
                            // record's actual state — so one button always says
                            // what pressing it and clicking it does the opposite
                            // of what's true now.
                            let (col_label, col_tip) = if in_collection {
                                (
                                    "✓ In collection",
                                    "Remove this record from your Discogs collection",
                                )
                            } else {
                                (
                                    "＋ Collection",
                                    "Add this record to your Discogs collection",
                                )
                            };
                            if ui
                                .add_enabled(!editing, egui::Button::new(col_label))
                                .on_hover_note(col_tip)
                                .on_disabled_hover_text(crate::ui::hover::note(
                                    "Wait for the current Discogs job to finish",
                                ))
                                .clicked()
                            {
                                act = Some(Act::ToggleList(VinylList::Collection));
                            }
                            let (want_label, want_tip) = if in_wantlist {
                                (
                                    "✓ In wantlist",
                                    "Remove this record from your Discogs wantlist",
                                )
                            } else {
                                ("＋ Wantlist", "Add this record to your Discogs wantlist")
                            };
                            if ui
                                .add_enabled(!editing, egui::Button::new(want_label))
                                .on_hover_note(want_tip)
                                .on_disabled_hover_text(crate::ui::hover::note(
                                    "Wait for the current Discogs job to finish",
                                ))
                                .clicked()
                            {
                                act = Some(Act::ToggleList(VinylList::Wantlist));
                            }
                            // Dig from here: search Discogs outward from this
                            // record for pressings you don't already have.
                            if can_dig
                                && ui
                                    .button("🔍  Dig")
                                    .on_hover_note(
                                        "Find records like this one on Discogs that aren't \
                                         in your collection",
                                    )
                                    .clicked()
                            {
                                act = Some(Act::Dig);
                            }
                            // The dig's two threads, for the record the dig is
                            // standing on. Same gating and wording as the strip
                            // (see `draw_dig`): both always shown, a disabled
                            // one explaining itself rather than vanishing.
                            if let Some((
                                head_artist,
                                head_label,
                                has_artist_id,
                                has_label_id,
                                busy,
                            )) = &branch
                            {
                                // Set apart from the four buttons before them:
                                // those record what you already decided, these
                                // are how you keep moving. A separator breaks
                                // the run, and the accent fill makes the
                                // forward action the one thing in the row that
                                // reads as a place to press.
                                ui.add_space(crate::ui::tokens::space::S2);
                                ui.separator();
                                ui.add_space(crate::ui::tokens::space::S2);
                                // A disabled button keeps whatever fill it was
                                // given, so an accent one still looks pressable
                                // while the ids resolve. Drop to the muted
                                // surface when it can't be clicked, so the
                                // colour only ever means "this works now".
                                let dig_fill = |on: bool| {
                                    if on {
                                        crate::ui::tokens::color::ACCENT
                                    } else {
                                        crate::ui::tokens::color::SURFACE_HI
                                    }
                                };
                                let dig_text = |on: bool| {
                                    if on {
                                        egui::Color32::WHITE
                                    } else {
                                        crate::ui::tokens::color::LABEL_4
                                    }
                                };
                                let artist_tip = if *has_artist_id {
                                    format!(
                                        "Find another vinyl release by {head_artist} that you \
                                         don't own"
                                    )
                                } else if head_artist.trim().is_empty() {
                                    "Discogs lists no artist for this record".to_string()
                                } else {
                                    format!("Looking up {head_artist} on Discogs…")
                                };
                                if ui
                                    .add_enabled(
                                        *has_artist_id && !busy,
                                        egui::Button::new(
                                            egui::RichText::new("♪  Dig the artist")
                                                .color(dig_text(*has_artist_id && !busy)),
                                        )
                                        .fill(dig_fill(*has_artist_id && !busy)),
                                    )
                                    .on_hover_note(artist_tip.clone())
                                    .on_disabled_hover_text(crate::ui::hover::note(artist_tip))
                                    .clicked()
                                {
                                    act = Some(Act::Branch(crate::dig::DigThread::Artist));
                                }
                                let label_tip = match head_label {
                                    Some(l) if *has_label_id => {
                                        format!(
                                            "Find another vinyl release on {l} that you don't own"
                                        )
                                    }
                                    Some(l) => format!("Looking up {l} on Discogs…"),
                                    None => "Discogs lists no label for this record".to_string(),
                                };
                                if ui
                                    .add_enabled(
                                        *has_label_id && !busy,
                                        egui::Button::new(
                                            egui::RichText::new("⌂  Dig the label")
                                                .color(dig_text(*has_label_id && !busy)),
                                        )
                                        .fill(dig_fill(*has_label_id && !busy)),
                                    )
                                    .on_hover_note(label_tip.clone())
                                    .on_disabled_hover_text(crate::ui::hover::note(label_tip))
                                    .clicked()
                                {
                                    act = Some(Act::Branch(crate::dig::DigThread::Label));
                                }
                            }
                        });
                    });
                });
                ui.add_space(10.0);
                ui.separator();

                // The transport for whatever the mini-player is playing. Sits
                // above the tracklist so it's in reach of the rows that feed it,
                // and only while there's a panel to drive.
                //
                // Its slot is held open whether or not it's there. The window
                // auto-sizes to its content, so a bar that appears on play and
                // vanishes on close would grow and shrink the whole sheet under
                // the pointer — and the rows the user is aiming at would jump by
                // the bar's height at the exact moment they started something
                // playing. Reserving the space costs a strip of empty sheet and
                // keeps the tracklist still.
                //
                // The height is measured from the real bar rather than written
                // down as a constant, so it can't drift out of step if the bar's
                // contents change. Until it has been measured once, the slot is
                // simply absent — one frame, on the first play of a session.
                let bar_h_id = egui::Id::new("vinyl-sheet-transport-h");
                if video_open {
                    ui.add_space(8.0);
                    let before = ui.cursor().top();
                    let mut scrub = self.vinyl_sheet.as_ref().and_then(|s| s.video_scrub);
                    video_act = video_transport_ui(ui, &mut scrub);
                    if let Some(s) = self.vinyl_sheet.as_mut() {
                        s.video_scrub = scrub;
                    }
                    let measured = ui.cursor().top() - before;
                    if measured > 0.0 {
                        ui.ctx().data_mut(|d| d.insert_temp(bar_h_id, measured));
                    }
                    ui.add_space(4.0);
                } else if has_video {
                    // Only on a record that *can* play a video. A record with no
                    // video never shows the bar, so reserving its slot there
                    // would be dead space at the top of every such sheet.
                    if let Some(h) = ui.ctx().data(|d| d.get_temp::<f32>(bar_h_id)) {
                        ui.add_space(8.0 + h + 4.0);
                    }
                }

                if loading {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Loading tracklist…").weak());
                    });
                    ui.add_space(10.0);
                    return;
                }
                if let Some(e) = &error {
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(e).color(egui::Color32::from_rgb(220, 140, 120)));
                    ui.add_space(10.0);
                    return;
                }

                let sheet = self.vinyl_sheet.as_ref().unwrap();
                if sheet.rows.is_empty() && sheet.extra_videos.is_empty() {
                    ui.add_space(10.0);
                    ui.label(
                        egui::RichText::new("Discogs lists no tracks or videos for this release.")
                            .weak(),
                    );
                    ui.add_space(10.0);
                    return;
                }

                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        ui.add_space(4.0);
                        for (i, row) in sheet.rows.iter().enumerate() {
                            let playing = match row.source {
                                SheetSource::Local(l) => sheet
                                    .local
                                    .get(l)
                                    .is_some_and(|t| Some(t.id) == sounding_id),
                                SheetSource::Video(v) => playing_video == Some(v),
                                SheetSource::None => false,
                            };
                            if sheet_row_ui(ui, sheet, row, i, playing) {
                                act = Some(Act::Play(i));
                            }
                        }
                        // Leftover videos: album rips, live sets, anything the
                        // tracklist didn't claim.
                        if !sheet.extra_videos.is_empty() {
                            ui.add_space(10.0);
                            ui.label(egui::RichText::new("Other videos").weak());
                            ui.separator();
                            for (n, v) in sheet.extra_videos.iter().enumerate() {
                                let Some(video) =
                                    sheet.detail.as_ref().and_then(|d| d.videos.get(*v))
                                else {
                                    continue;
                                };
                                let playing = playing_video == Some(*v);
                                if extra_video_ui(ui, video, n, playing) {
                                    act = Some(Act::PlayExtra(*v));
                                }
                            }
                        }
                        ui.add_space(4.0);
                    });

                // Footer: how many tracks you actually own, and a way in.
                let owned = sheet
                    .rows
                    .iter()
                    .filter(|r| matches!(r.source, SheetSource::Local(_)))
                    .count();
                ui.separator();
                ui.horizontal(|ui| {
                    let note = match owned {
                        0 => "No tracks from this record are in your library".to_string(),
                        1 => "1 track from this record is in your library".to_string(),
                        n => format!("{n} tracks from this record are in your library"),
                    };
                    ui.label(egui::RichText::new(note).small().weak());
                    if owned > 0 {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("Show in library")
                                .on_hover_note("Filter the library to this record")
                                .clicked()
                            {
                                act = Some(Act::Goto);
                            }
                        });
                    }
                });
            });

        // The transport talks straight to the panel — nothing here touches the
        // sheet's own state except the stop, which also clears the row marker.
        match video_act {
            Some(VideoAct::TogglePause) => webview::toggle_pause(),
            Some(VideoAct::Seek(secs)) => webview::seek(secs),
            Some(VideoAct::Stop) => self.stop_sheet_video(),
            Some(VideoAct::ToggleVideo) => webview::set_video_visible(!webview::video_visible()),
            None => {}
        }

        match act {
            // Starting a dig closes the sheet: the strip it drives sits behind
            // this window, and the first thing a digger does is look at it.
            Some(Act::WantAlternative(id)) => {
                self.request_vinyl_edit(
                    ctx.clone(),
                    VinylEdit::Want {
                        release_ids: vec![id],
                        label: format!("{artist} — {title}"),
                    },
                );
            }
            Some(Act::ToggleList(list)) => {
                let present = match list {
                    VinylList::Collection => in_collection,
                    VinylList::Wantlist => in_wantlist,
                };
                let label = format!("{artist} — {title}");
                let edit = if present {
                    // Removing needs the cached row: a collection copy is
                    // addressed by folder + instance id, not by release id.
                    // Anything in a list has one, since that's what membership
                    // is read from.
                    let Some(record) = self.vinyl_record_in(list, release_id) else {
                        self.status =
                            "That record isn't in the local cache yet — sync and try again.".into();
                        return;
                    };
                    VinylEdit::Remove {
                        list,
                        record: Box::new(record),
                    }
                } else {
                    match list {
                        VinylList::Collection => VinylEdit::Collect { release_id, label },
                        VinylList::Wantlist => VinylEdit::Want {
                            release_ids: vec![release_id],
                            label,
                        },
                    }
                };
                self.request_vinyl_edit(ctx.clone(), edit);
            }
            Some(Act::Dig) => {
                let Some(k) = key else { return };
                self.start_dig(k);
                self.stop_sheet_video();
                self.vinyl_sheet = None;
                return;
            }
            // Branching moves the dig to a new record, and the sheet rides
            // along: digging from the window is a chain of listens, so it
            // re-points at whatever the step lands on rather than closing and
            // sending the user back to the strip to reopen the next find. The
            // old record's playback stops here — what's on screen is about to
            // be a different record. See `sheet_follows_dig`, spent in
            // `apply_page`.
            Some(Act::Branch(thread)) => {
                self.stop_sheet_video();
                self.sheet_follows_dig = true;
                self.dig_step(thread);
                return;
            }
            Some(Act::Play(row)) => self.play_sheet_row(row, frame),
            Some(Act::TogglePlay) => match record_play {
                RecordPlay::Playing(PlayEngine::Video) | RecordPlay::Paused(PlayEngine::Video) => {
                    webview::toggle_pause()
                }
                RecordPlay::Playing(PlayEngine::Audio) | RecordPlay::Paused(PlayEngine::Audio) => {
                    if let Some(a) = self.audio.as_mut() {
                        a.toggle_pause();
                    }
                }
                RecordPlay::Stopped => self.play_sheet_from_start(frame),
            },
            Some(Act::PlayExtra(v)) => {
                let (ids, title) = {
                    let s = self.vinyl_sheet.as_ref().unwrap();
                    let video = s.detail.as_ref().and_then(|d| d.videos.get(v));
                    (
                        video
                            .filter(|v| v.embeddable)
                            .and_then(|v| v.youtube_id().map(str::to_string))
                            .map(|id| vec![id])
                            .unwrap_or_default(),
                        video.map(|v| v.title.clone()).unwrap_or_default(),
                    )
                };
                if ids.is_empty() {
                    let uri = self
                        .vinyl_sheet
                        .as_ref()
                        .and_then(|s| s.detail.as_ref()?.videos.get(v))
                        .map(|v| v.uri.clone());
                    if let Some(uri) = uri {
                        open_url(&uri);
                    }
                } else {
                    if let Some(a) = self.audio.as_mut() {
                        if a.is_active() {
                            a.toggle_pause();
                        }
                    }
                    if webview::play(frame, &ids, &title) {
                        if let Some(s) = self.vinyl_sheet.as_mut() {
                            s.playing_video = Some(v);
                        }
                    }
                }
            }
            Some(Act::Goto) => {
                let (album, tracks) = {
                    let s = self.vinyl_sheet.as_ref().unwrap();
                    (s.title.clone(), s.local.iter().map(|l| l.id).collect())
                };
                // Same rule as closing it: leaving the record's sheet stops the
                // record's video, and the library is where the files play.
                self.stop_sheet_video();
                self.vinyl_sheet = None;
                self.jump_to_catalog_tracks(album, tracks);
            }
            None => {}
        }
        if !open {
            // Closing the record takes its video with it: the panel is that
            // record's player, so leaving it playing over a closed sheet is
            // sound with nothing on screen to explain it.
            self.stop_sheet_video();
            self.vinyl_sheet = None;
        }
    }
}

/// Which engine is carrying the record right now — the two are never both
/// live, but the play button has to send its click to the right one.
#[derive(Clone, Copy, PartialEq)]
enum PlayEngine {
    Audio,
    Video,
}

/// Whether this record is sounding, loaded-but-paused, or not started.
#[derive(Clone, Copy, PartialEq)]
enum RecordPlay {
    Playing(PlayEngine),
    Paused(PlayEngine),
    Stopped,
}

/// The record sheet's content width. Fixed, and the single source the
/// transport bar sizes against — see the `set_max_width` note in the sheet.
const SHEET_W: f32 = 620.0;

/// The video-window toggle: a small display outline, filled once the window is
/// actually on screen, so the button shows the state rather than only the verb.
fn draw_screen_glyph(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, showing: bool) {
    let screen = egui::Rect::from_center_size(egui::pos2(c.x, c.y - 1.0), egui::vec2(15.0, 11.0));
    if showing {
        p.rect_filled(screen, 2.0, col);
    } else {
        p.rect_stroke(screen, 2.0, egui::Stroke::new(1.5, col));
    }
    // Stand, so the mark reads as a screen at this size instead of a bare box.
    p.line_segment(
        [
            egui::pos2(c.x - 3.5, screen.max.y + 2.5),
            egui::pos2(c.x + 3.5, screen.max.y + 2.5),
        ],
        egui::Stroke::new(1.5, col),
    );
}

/// The tracklist's two fixed left-hand columns: the play marker, then the
/// position. Shared with the transport bar, whose own play button is drawn
/// [`MARKER_W`] wide over the same left edge, so the bar's pause icon sits
/// directly above the triangles in the rows beneath it instead of a few pixels
/// to their right.
const MARKER_W: f32 = 22.0;
const POS_W: f32 = 34.0;
/// The tracklist's duration column, wide enough for a long side ("10:57") so
/// the figures right-align in a straight column instead of each starting
/// wherever its own text happens to begin.
const DUR_W: f32 = 38.0;
/// The BPM/key column, wide enough for the longest reading ("125 · 12A"). The
/// row lays out right-to-left, so without a fixed width a two-character key
/// ("2A") left the "♪ your copy" chip beside it sitting further right than a
/// three-character one ("12A") and the chips read as a ragged column.
const FIG_W: f32 = 58.0;

/// The transport's two clocks. Both are given the same fixed width so the
/// elapsed side can't shove the scrubber sideways as its digits tick over, and
/// wide enough for an hour-long set ("1:07:59") rather than only a track.
const CLOCK_W: f32 = 48.0;

/// What the transport bar's controls asked for this frame.
enum VideoAct {
    TogglePause,
    Seek(f32),
    Stop,
    /// Show the video panel, or park it off screen again.
    ToggleVideo,
}

/// The transport for the video mini-player: a play/pause button, a wide
/// draggable scrubber and the clock, drawn at egui's scale rather than
/// YouTube's.
///
/// The panel's own controls are a few pixels tall inside a 480px window and
/// vanish under the page's chrome styling, so scrubbing there is guesswork.
/// Everything here drives the same `<video>` element through
/// [`crate::webview::seek`] / [`crate::webview::toggle_pause`]; the position it
/// paints comes back from the page on the next poll, so the bar always shows
/// what the video is really doing.
///
/// Drawn in the record sheet, between the record's buttons and its tracklist:
/// the rows that feed the player are right underneath, so the transport sits
/// with what it drives.
///
/// It spans the sheet's full content width. Measuring that off the `Ui` is safe
/// *here specifically* — the sheet pins its content with `set_max_width`, so
/// `available_width` reports that fixed figure rather than anything derived
/// from what's been laid out. Without that pin this would be the classic
/// auto-sizing-window feedback loop, where a child that fills its offered width
/// turns this frame's layout into next frame's demand and walks the window out
/// to the screen edge.
///
/// `scrub` is the in-flight drag fraction, borrowed mutably so the drag can own
/// the playhead until it's released.
fn video_transport_ui(ui: &mut egui::Ui, scrub: &mut Option<f32>) -> Option<VideoAct> {
    use crate::ui::tokens::space;

    const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
    /// Breathing room inside the bar's ends, so the pause button and the close
    /// cross aren't flush against its rounded corners.
    const EDGE: f32 = space::S4;
    let t = webview::transport();
    let mut act = None;

    // The bar fills the sheet, so its own width is the width it's offered.
    let bar_w = ui.available_width();
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .rounding(crate::ui::tokens::radius::SM)
        .inner_margin(egui::Margin::symmetric(EDGE, space::S3))
        .show(ui, |ui| {
            ui.set_width(bar_w - EDGE * 2.0);
            ui.horizontal(|ui| {
                // Play/pause, 28 tall so it stays a comfortable target beside
                // the scrubber rather than a text-height button.
                let (btn_rect, btn) =
                    ui.allocate_exact_size(egui::vec2(MARKER_W, 28.0), egui::Sense::click());
                let col = if btn.hovered() {
                    egui::Color32::WHITE
                } else {
                    egui::Color32::from_gray(190)
                };
                crate::ui::icon::play_pause(ui.painter(), btn_rect.center(), col, t.playing);
                if btn.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                let btn = btn.on_hover_note(if t.playing { "Pause" } else { "Play" });
                if btn.clicked() {
                    act = Some(VideoAct::TogglePause);
                }
                ui.add_space(space::S4);

                // The fraction the bar paints: the drag while one is in flight,
                // the video's real position otherwise.
                let live = if t.duration > 0.0 {
                    (t.position / t.duration).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let shown = scrub.unwrap_or(live);

                // Elapsed. Fixed width, so digits changing mid-scrub can't shift
                // the scrubber that follows them (same reason as the player bar).
                ui.add_sized(
                    egui::vec2(CLOCK_W, 18.0),
                    egui::Label::new(
                        egui::RichText::new(fmt_time(shown * t.duration))
                            .font(crate::ui::tokens::font::mono_small())
                            .color(egui::Color32::from_gray(170)),
                    ),
                );
                ui.add_space(space::S4);

                // Scrubber: the width left over once the controls that follow
                // it have been accounted for. Only the trailing ones need
                // subtracting — `available_width` has already shed the play
                // button and elapsed clock behind it.
                const TRAILING: f32 = space::S4 + CLOCK_W // gap, total clock
                    + space::S4 + 24.0                    // gap, video toggle
                    + space::S3 + 24.0; // gap, close
                let track_w = (ui.available_width() - TRAILING).max(60.0);
                let (rect, resp) = ui
                    .allocate_exact_size(egui::vec2(track_w, 26.0), egui::Sense::click_and_drag());
                let y = rect.center().y;
                let (x0, x1) = (rect.left(), rect.right());
                let knob_x = x0 + shown * (x1 - x0);
                let p = ui.painter();
                p.line_segment(
                    [egui::pos2(x0, y), egui::pos2(x1, y)],
                    egui::Stroke::new(4.0, egui::Color32::from_gray(70)),
                );
                p.line_segment(
                    [egui::pos2(x0, y), egui::pos2(knob_x, y)],
                    egui::Stroke::new(4.0, ACCENT),
                );
                let knob_r = if resp.hovered() || scrub.is_some() {
                    6.5
                } else {
                    5.0
                };
                p.circle_filled(egui::pos2(knob_x, y), knob_r, egui::Color32::WHITE);
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }

                // A live stream (or a page that hasn't reported a length) has
                // nothing to seek within, so the bar stays a readout.
                let seekable = t.duration > 0.0;
                let frac_at = |pos: egui::Pos2| ((pos.x - x0) / (x1 - x0)).clamp(0.0, 1.0);
                if seekable {
                    if resp.dragged() || resp.drag_started() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            *scrub = Some(frac_at(pos));
                        }
                    }
                    if resp.drag_stopped() {
                        if let Some(f) = scrub.take() {
                            act = Some(VideoAct::Seek(f * t.duration));
                        }
                    }
                    if resp.clicked() {
                        if let Some(pos) = resp.interact_pointer_pos() {
                            act = Some(VideoAct::Seek(frac_at(pos) * t.duration));
                        }
                        *scrub = None;
                    }
                }

                // Total. Sized like the elapsed clock beside it: a bare label
                // would be as wide as its text, so an hour-long set ("1:07:59")
                // would take width the scrubber had already been given.
                ui.add_space(space::S4);
                ui.add_sized(
                    egui::vec2(CLOCK_W, 18.0),
                    egui::Label::new(
                        egui::RichText::new(if t.ready && t.duration > 0.0 {
                            fmt_time(t.duration)
                        } else {
                            // A page still finding its video, or a stream with
                            // no end — neither has a total to show.
                            "--:--".to_string()
                        })
                        .font(crate::ui::tokens::font::mono_small())
                        .color(egui::Color32::from_gray(170)),
                    ),
                );

                // The picture, for when the user wants it. The panel plays
                // parked off screen otherwise, since this bar is the interface.
                // Painted rather than a text glyph in button chrome, so both of
                // these sit beside the play triangle as the same kind of mark.
                ui.add_space(space::S4);
                let showing = webview::video_visible();
                let (rect, resp) =
                    ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::click());
                let resp = resp.on_hover_note(if showing {
                    "Hide the video window"
                } else {
                    "Show the video window"
                });
                draw_screen_glyph(
                    ui.painter(),
                    rect.center(),
                    crate::ui::icon::col(&resp),
                    showing,
                );
                if resp.hovered() {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                if resp.clicked() {
                    act = Some(VideoAct::ToggleVideo);
                }

                ui.add_space(space::S3);
                if crate::ui::icon::close_button(ui, "Close the video player") {
                    act = Some(VideoAct::Stop);
                }
            });
        });
    act
}

/// One tracklist row. Returns true when the user asked to play it.
fn sheet_row_ui(
    ui: &mut egui::Ui,
    sheet: &VinylSheet,
    row: &SheetRow,
    index: usize,
    playing: bool,
) -> bool {
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
    let playable = !matches!(row.source, SheetSource::None);
    let mut clicked = false;

    // Reserve a slot underneath the row's content so the hover fill paints
    // behind the text instead of washing over it.
    let bg = ui.painter().add(egui::Shape::Noop);
    let resp = ui
        .scope(|ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(24.0);
                // Play marker.
                let glyph = if playing {
                    "❚❚"
                } else if playable {
                    "▶"
                } else {
                    " "
                };
                let colour = if playing {
                    ACCENT
                } else if playable {
                    egui::Color32::from_gray(190)
                } else {
                    egui::Color32::from_gray(90)
                };
                // Fixed cells, left-aligned: `allocate_ui` shrinks to the
                // content it's given, so a row playing "❚❚" reserved a wider
                // marker than one showing "▶" and shunted the position and
                // title of that one row to the right. These columns hold their
                // width whatever glyph is in them, so the tracklist reads as
                // straight edges down the sheet.
                // A column that holds its width whatever it's given:
                // `allocate_ui` shrinks back to the content it actually used,
                // so the reserved size has to be taken outright and the text
                // painted into it.
                let cell = |ui: &mut egui::Ui, w: f32, text: egui::RichText| {
                    let (rect, _) =
                        ui.allocate_exact_size(egui::vec2(w, 20.0), egui::Sense::hover());
                    ui.put(
                        rect,
                        egui::Label::new(text)
                            .selectable(false)
                            .halign(egui::Align::LEFT),
                    );
                };
                cell(
                    ui,
                    MARKER_W,
                    egui::RichText::new(glyph).size(11.0).color(colour),
                );
                // Position.
                cell(
                    ui,
                    POS_W,
                    egui::RichText::new(&row.position)
                        .small()
                        .color(egui::Color32::from_gray(150)),
                );
                let title = egui::RichText::new(&row.title).color(if playable {
                    egui::Color32::from_gray(230)
                } else {
                    egui::Color32::from_gray(120)
                });
                ui.label(if playing { title.strong() } else { title });
                // Who played it, on the records where the header can't say —
                // a compilation credits "Various", so the per-track artist is
                // the only place the performer's name appears. Dimmer than the
                // title and set after it, so the tracklist still reads as a
                // list of songs with the credit as support.
                if let Some(artist) = &row.artist {
                    ui.label(
                        egui::RichText::new(artist)
                            .small()
                            .color(egui::Color32::from_gray(if playable { 150 } else { 105 })),
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    // Source chip: which of the two engines this row uses.
                    match row.source {
                        SheetSource::Local(l) => {
                            let t = &sheet.local[l];
                            let figures = match (t.bpm, &t.camelot) {
                                (Some(b), Some(k)) => format!("{b:.0} · {k}"),
                                (Some(b), None) => format!("{b:.0}"),
                                (None, Some(k)) => k.clone(),
                                (None, None) => String::new(),
                            };
                            if !figures.is_empty() {
                                let (rect, _) = ui.allocate_exact_size(
                                    egui::vec2(FIG_W, 20.0),
                                    egui::Sense::hover(),
                                );
                                ui.put(
                                    rect,
                                    egui::Label::new(
                                        egui::RichText::new(figures)
                                            .small()
                                            .color(egui::Color32::from_gray(140)),
                                    )
                                    .selectable(false)
                                    .halign(egui::Align::RIGHT),
                                );
                                ui.add_space(8.0);
                            }
                            ui.label(egui::RichText::new("♪ your copy").small().color(ACCENT));
                        }
                        SheetSource::Video(_) => {
                            ui.label(
                                egui::RichText::new("▶ video")
                                    .small()
                                    .color(egui::Color32::from_rgb(190, 130, 130)),
                            );
                        }
                        SheetSource::None => {
                            ui.label(
                                egui::RichText::new("—")
                                    .small()
                                    .color(egui::Color32::from_gray(90)),
                            );
                        }
                    }
                    if !row.duration.is_empty() {
                        ui.add_space(10.0);
                        // Fixed and right-aligned, like the transport's clocks:
                        // a bare label is as wide as its text, so "10:57" and
                        // "9:59" started at different x and the durations read
                        // as a ragged column down the sheet.
                        let (rect, _) =
                            ui.allocate_exact_size(egui::vec2(DUR_W, 20.0), egui::Sense::hover());
                        ui.put(
                            rect,
                            egui::Label::new(
                                egui::RichText::new(&row.duration)
                                    .small()
                                    .color(egui::Color32::from_gray(130)),
                            )
                            .selectable(false)
                            .halign(egui::Align::RIGHT),
                        );
                    }
                });
            });
        })
        .response;

    // The whole row is the hit target, so there's no small glyph to aim at.
    let rect = resp.rect;
    // Keyed by index, not by text: Discogs releases regularly repeat a
    // position or a title, and two rows sharing an id share hover state — one
    // cursor would light up both.
    let id = ui.id().with(("sheet-row", index));
    let hit = ui.interact(rect, id, egui::Sense::click());
    if playable {
        if hit.hovered() {
            ui.painter().set(
                bg,
                egui::epaint::RectShape::filled(
                    rect.expand2(egui::vec2(4.0, 0.0)),
                    egui::Rounding::same(4.0),
                    egui::Color32::from_white_alpha(10),
                ),
            );
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        if hit.clicked() {
            clicked = true;
        }
    } else {
        hit.on_hover_note("Not in your library, and Discogs lists no video for it");
    }
    clicked
}

/// One "other video" row (album rip, live set). Returns true when clicked.
fn extra_video_ui(
    ui: &mut egui::Ui,
    video: &discogs::ReleaseVideo,
    index: usize,
    playing: bool,
) -> bool {
    const ACCENT: egui::Color32 = egui::Color32::from_rgb(90, 200, 120);
    let bg = ui.painter().add(egui::Shape::Noop);
    let resp = ui
        .scope(|ui| {
            ui.horizontal(|ui| {
                ui.set_min_height(22.0);
                ui.allocate_ui(egui::vec2(22.0, 20.0), |ui| {
                    ui.label(
                        egui::RichText::new(if playing { "❚❚" } else { "▶" })
                            .size(11.0)
                            .color(if playing {
                                ACCENT
                            } else {
                                egui::Color32::from_gray(190)
                            }),
                    );
                });
                ui.label(egui::RichText::new(&video.title).color(egui::Color32::from_gray(215)));
                if let Some(d) = video.duration_secs {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(fmt_duration(u64::from(d) * 1000))
                                .small()
                                .color(egui::Color32::from_gray(130)),
                        );
                    });
                }
            });
        })
        .response;
    let hit = ui.interact(
        resp.rect,
        ui.id().with(("sheet-extra", index)),
        egui::Sense::click(),
    );
    if hit.hovered() {
        ui.painter().set(
            bg,
            egui::epaint::RectShape::filled(
                resp.rect.expand2(egui::vec2(4.0, 0.0)),
                egui::Rounding::same(4.0),
                egui::Color32::from_white_alpha(10),
            ),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    hit.clicked()
}
