//! The versions panel: every other pressing of the record you're looking at, and
//! the swap that trades your copy for one of them.
//!
//! A record on Discogs is a *release* — one specific pressing — hanging off a
//! *master* that gathers every pressing of the same music. Which pressing you
//! own is the thing a record collector actually cares about: the original German
//! 12" and a 2014 repress are the same music and very different records. This
//! panel lists the siblings from `GET /masters/{id}/versions`, and lets one of
//! them replace the copy in your list without a round trip through discogs.com.
//!
//! The list is ordered the way the API returns it — most-owned first — which
//! puts the canonical pressing at the top and the promos and test pressings
//! below, the same signal [`crate::vinyl_sheet`] uses to find a buyable copy.

use super::*;

/// How many versions to show. The API pages at 100 and a busy master can run to
/// several hundred pressings, most of them regional repress noise; the panel is
/// for picking a pressing, not for auditing a discography.
const MAX_VERSIONS: usize = 100;

/// The versions panel's window width. Wider than the record sheet: each row
/// carries format, label, catalog number and country on one line, and wrapping
/// those turns a scannable list into a wall.
const PANEL_W: f32 = 560.0;

/// Side of a row's sleeve thumbnail. Sized to the two text lines beside it, so
/// the covers read as a scannable column rather than dominating the row — you
/// pick a pressing by its stamp and catalog number, with the sleeve confirming
/// it's the right record.
const THUMB: f32 = 48.0;

/// The open versions panel — the record it was opened from, and the pressings
/// found for it.
pub(crate) struct VersionsPanel {
    /// The list the record sits in, and its row id there. A swap has to know
    /// which list to write, and needs the cached row to remove the old copy.
    /// `None` for a record reached without a cache key (from the record sheet on
    /// a dug release), which can still browse but has nothing to swap.
    pub key: Option<VinylCoverKey>,
    /// The pressing the panel was opened from, marked "this one" in the list.
    pub release_id: u64,
    pub artist: String,
    pub title: String,
    /// The pressings found, once the lookup lands. Excludes nothing — the
    /// record's own pressing stays in the list, marked, so the user can see
    /// where their copy sits among the others.
    pub versions: Vec<discogs::MasterVersion>,
    pub loading: bool,
    /// Why the list is empty, when it is. A record with no master on Discogs is
    /// the common case and reads as an explanation, not a failure.
    pub error: Option<String>,
}

/// One finished versions lookup: which record it was for, and what came back.
pub(crate) struct VersionsFetched {
    pub release_id: u64,
    pub result: Result<Vec<discogs::MasterVersion>, String>,
}

/// What the user clicked in the panel, applied after the window releases its
/// borrow of `self`.
enum Act {
    /// Open this pressing's sheet — read the tracklist before committing to it.
    Open(u64),
    /// Trade the record's copy for this pressing.
    Swap(u64),
    /// Open this pressing on discogs.com, where the condition and the seller
    /// notes live — the things that decide between two copies of one pressing.
    Web(u64),
}

/// One pressing as the panel draws it. Snapshotted out of the panel before the
/// window opens, so its closure never borrows state the click handlers need to
/// mutate.
struct Row {
    release_id: u64,
    /// This pressing's sleeve, once its thumbnail has downloaded. `None` while
    /// it's still in flight, or when Discogs lists no image for the pressing —
    /// both draw the same placeholder, since a sleeve arriving a moment later
    /// shouldn't make the row jump.
    cover: Option<Tex>,
    title: String,
    format: String,
    label: String,
    catno: String,
    /// Year and country, pre-joined — `1993 Germany`, or either alone.
    place: String,
    /// How many Discogs users hold this exact pressing.
    held: u32,
    owned: bool,
    wanted: bool,
}

/// The year from Discogs's `released` string, which is `1993`, `1993-04-01` or
/// blank depending on how the release was catalogued.
fn year_of(released: &str) -> &str {
    released.split('-').next().unwrap_or("").trim()
}

impl App {
    /// Open the versions panel for a record in one of the lists.
    pub(crate) fn open_versions(&mut self, key: VinylCoverKey, ctx: &egui::Context) {
        let Some(record) = self.vinyl_record(key) else {
            return;
        };
        self.versions = Some(VersionsPanel {
            key: Some(key),
            release_id: record.release_id,
            artist: record.artist.clone(),
            title: record.title.clone(),
            versions: Vec::new(),
            loading: true,
            error: None,
        });
        self.spawn_versions_fetch(record.release_id, ctx.clone());
    }

    /// Look up every pressing of the open record's master, off the UI thread.
    ///
    /// Two requests at worst: the release (to learn its master id) and the
    /// master's versions. The release is nearly always already in the catalog's
    /// `release_cache` — the vinyl sync warms it — so the common cost is one.
    fn spawn_versions_fetch(&mut self, release_id: u64, ctx: egui::Context) {
        let (tx, rx) = mpsc::channel();
        self.versions_rx = Some(rx);
        let db = self.db_path.clone();
        let token = self.discogs_token();
        thread::spawn(move || {
            let result = versions_for(&db, &token, release_id);
            let _ = tx.send(VersionsFetched { release_id, result });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished versions lookup onto the panel that asked for it.
    pub(crate) fn poll_versions(&mut self) {
        let Some(rx) = &self.versions_rx else {
            return;
        };
        let Ok(msg) = rx.try_recv() else { return };
        self.versions_rx = None;
        let Some(panel) = self.versions.as_mut() else {
            return;
        };
        // The user opened the panel on a different record while this was out.
        if panel.release_id != msg.release_id {
            return;
        }
        panel.loading = false;
        match msg.result {
            Ok(v) => panel.versions = v,
            Err(e) => panel.error = Some(e),
        }
    }

    /// Draw the versions panel, if one is open.
    pub(crate) fn draw_versions(&mut self, ctx: &egui::Context) {
        let Some(panel) = self.versions.as_ref() else {
            return;
        };
        let (artist, title, current, loading) = (
            panel.artist.clone(),
            panel.title.clone(),
            panel.release_id,
            panel.loading,
        );
        let error = panel.error.clone();
        let key = panel.key;
        // Snapshot the rows the window draws, so its closure doesn't borrow the
        // panel while the click handlers below need `self`. Built in two passes
        // because the sleeves come from `dig_cover`, which takes `&mut self` to
        // start a download on a miss and so can't run while `panel` is borrowed.
        let mut rows: Vec<Row> = panel
            .versions
            .iter()
            .take(MAX_VERSIONS)
            .map(|v| Row {
                release_id: v.release_id,
                cover: None,
                title: v.title.clone(),
                format: v.format.clone(),
                label: v.label.clone(),
                catno: v.catno.clone(),
                place: format!("{} {}", year_of(&v.released), v.country)
                    .trim()
                    .to_string(),
                held: v.in_collection,
                owned: self.vinyl_owned.contains(&v.release_id),
                wanted: self.vinyl_wanted.contains(&v.release_id),
            })
            .collect();
        // Thumbnails, keyed by URL in the same cache the dig strip uses — a
        // pressing already seen there (or listed twice) costs no second
        // download. Only the rows actually on screen are fetched; the panel caps
        // at `MAX_VERSIONS`, so this is a bounded set either way.
        let thumbs: Vec<Option<String>> = panel
            .versions
            .iter()
            .take(MAX_VERSIONS)
            .map(|v| Some(v.thumb_url.clone()).filter(|u| !u.trim().is_empty()))
            .collect();
        for (row, url) in rows.iter_mut().zip(thumbs) {
            if let Some(u) = url {
                row.cover = self.dig_cover(&u).cloned();
            }
        }
        // A swap writes the user's Discogs account through the one job channel,
        // so it waits out any running job rather than queueing behind it.
        let busy = self.is_busy();
        let mut act: Option<Act> = None;
        let mut open = true;
        egui::Window::new(format!("Other pressings — {artist} — {title}"))
            .id(egui::Id::new(("versions", current)))
            .open(&mut open)
            .collapsible(false)
            .resizable([false, true])
            .default_width(PANEL_W)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(PANEL_W);
                ui.set_max_width(PANEL_W);
                ui.add_space(2.0);
                ui.label(
                    egui::RichText::new(
                        "Every pressing Discogs lists of this record, most widely held first.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(6.0);
                if loading {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Looking up other pressings…").weak());
                    });
                    return;
                }
                if let Some(e) = &error {
                    ui.label(egui::RichText::new(e).weak());
                    return;
                }
                if rows.is_empty() {
                    ui.label(
                        egui::RichText::new("Discogs lists no other pressing of this record.")
                            .weak(),
                    );
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        for r in &rows {
                            let is_current = r.release_id == current;
                            ui.horizontal(|ui| {
                                // Sleeve first, so the eye can run down the
                                // column of covers and stop at the one it
                                // recognises before reading a word.
                                let (trect, _) = ui.allocate_exact_size(
                                    egui::vec2(THUMB, THUMB),
                                    egui::Sense::hover(),
                                );
                                match &r.cover {
                                    Some(t) => {
                                        egui::Image::new(t)
                                            .fit_to_exact_size(egui::vec2(THUMB, THUMB))
                                            .rounding(egui::Rounding::same(4.0))
                                            .paint_at(ui, trect);
                                    }
                                    // Still downloading, or Discogs has no image
                                    // for this pressing. A quiet plate keeps the
                                    // rows aligned either way.
                                    None => {
                                        ui.painter().rect_filled(
                                            trect,
                                            egui::Rounding::same(4.0),
                                            egui::Color32::from_gray(34),
                                        );
                                    }
                                }
                                ui.add_space(8.0);
                                ui.vertical(|ui| {
                                    ui.set_width(PANEL_W - 160.0 - THUMB - 8.0);
                                    // The format is what tells two pressings
                                    // apart, so it leads. The title only earns a
                                    // line when it differs from the record's own
                                    // (reissues get retitled, most don't).
                                    let head = if r.format.trim().is_empty() {
                                        r.title.clone()
                                    } else {
                                        r.format.clone()
                                    };
                                    ui.horizontal_wrapped(|ui| {
                                        ui.spacing_mut().item_spacing.x = 5.0;
                                        ui.label(egui::RichText::new(head).strong());
                                        if is_current {
                                            ui.label(
                                                egui::RichText::new("· your copy")
                                                    .small()
                                                    .color(egui::Color32::from_rgb(120, 200, 140)),
                                            );
                                        } else if r.owned {
                                            ui.label(
                                                egui::RichText::new("· in your collection")
                                                    .small()
                                                    .weak(),
                                            );
                                        } else if r.wanted {
                                            ui.label(
                                                egui::RichText::new("· in your wantlist")
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                    });
                                    let mut line = Vec::new();
                                    if !r.label.trim().is_empty() {
                                        line.push(r.label.clone());
                                    }
                                    if !r.catno.trim().is_empty() {
                                        line.push(r.catno.clone());
                                    }
                                    if !r.place.is_empty() {
                                        line.push(r.place.clone());
                                    }
                                    if r.held > 0 {
                                        line.push(format!("{} own it", r.held));
                                    }
                                    if !line.is_empty() {
                                        ui.label(
                                            egui::RichText::new(line.join(" · ")).weak().small(),
                                        );
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        // The pressing you already have needs no
                                        // action offered against it.
                                        if !is_current {
                                            let can_swap = key.is_some() && !busy && !r.owned;
                                            let tip = if key.is_none() {
                                                "Open this record from your collection or \
                                                 wantlist to swap it"
                                            } else if r.owned {
                                                "You already have this pressing"
                                            } else if busy {
                                                "Wait for the current job to finish"
                                            } else {
                                                "Trade your copy for this pressing on Discogs"
                                            };
                                            if ui
                                                .add_enabled(can_swap, egui::Button::new("Swap in"))
                                                .on_hover_note(tip)
                                                .on_disabled_hover_text(crate::ui::hover::note(tip))
                                                .clicked()
                                            {
                                                act = Some(Act::Swap(r.release_id));
                                            }
                                        }
                                        if ui
                                            .button("↗")
                                            .on_hover_note("Open this pressing on discogs.com")
                                            .clicked()
                                        {
                                            act = Some(Act::Web(r.release_id));
                                        }
                                        if !is_current
                                            && ui
                                                .button("Tracklist")
                                                .on_hover_note(
                                                    "Open this pressing's sheet without swapping",
                                                )
                                                .clicked()
                                        {
                                            act = Some(Act::Open(r.release_id));
                                        }
                                    },
                                );
                            });
                            ui.separator();
                        }
                    });
            });

        match act {
            Some(Act::Web(id)) => open_url(&format!("https://www.discogs.com/release/{id}")),
            Some(Act::Open(id)) => {
                // Hand the sheet this pressing's own sleeve. It isn't in either
                // list, so it has no cached cover to key on — without the URL
                // the sheet falls back to a blank plate, and the pressing you
                // just picked out by its cover opens with no cover at all. The
                // panel has already downloaded this thumbnail, and the sheet
                // reads the same URL-keyed cache, so it paints immediately.
                let (v_title, sub, cover) = self
                    .versions
                    .as_ref()
                    .and_then(|p| p.versions.iter().find(|v| v.release_id == id))
                    .map(|v| {
                        (
                            v.title.clone(),
                            version_sub(v),
                            Some(v.thumb_url.clone()).filter(|u| !u.trim().is_empty()),
                        )
                    })
                    .unwrap_or_else(|| (title.clone(), String::new(), None));
                self.open_release_sheet(id, artist.clone(), v_title, sub, cover, ctx);
            }
            Some(Act::Swap(id)) => {
                if let Some(k) = key {
                    if let Some(record) = self.vinyl_record(k) {
                        let to_label = self
                            .versions
                            .as_ref()
                            .and_then(|p| p.versions.iter().find(|v| v.release_id == id))
                            .map(swap_label)
                            .unwrap_or_else(|| format!("release {id}"));
                        self.request_vinyl_edit(
                            ctx.clone(),
                            VinylEdit::Swap {
                                list: k.0,
                                record: Box::new(record),
                                to_release: id,
                                to_label,
                            },
                        );
                        // The panel is keyed to the row that's about to be
                        // deleted, so it closes with the swap rather than
                        // lingering over a record that no longer exists.
                        self.versions = None;
                    }
                }
            }
            None => {}
        }
        if !open {
            self.versions = None;
        }
    }
}

/// The caption under a pressing's name in the record sheet — the same
/// `year · format · label catno` shape the grid's cells use.
fn version_sub(v: &discogs::MasterVersion) -> String {
    let mut parts = Vec::new();
    let year = year_of(&v.released);
    if !year.is_empty() {
        parts.push(year.to_string());
    }
    if !v.format.trim().is_empty() {
        parts.push(v.format.clone());
    }
    match (v.label.trim(), v.catno.trim()) {
        ("", "") => {}
        ("", c) => parts.push(c.to_string()),
        (l, "") => parts.push(l.to_string()),
        (l, c) => parts.push(format!("{l} {c}")),
    }
    parts.join(" · ")
}

/// How to name an incoming pressing in the swap's confirmation and status line.
/// The format is what distinguishes it from the copy being given up, so that
/// leads; the catalog number pins it when two pressings share a format.
fn swap_label(v: &discogs::MasterVersion) -> String {
    let head = if v.format.trim().is_empty() {
        v.title.clone()
    } else {
        v.format.clone()
    };
    if v.catno.trim().is_empty() {
        head
    } else {
        format!("{head} ({})", v.catno.trim())
    }
}

/// Resolve one release's sibling pressings — cache first for the master id,
/// network for the versions themselves.
///
/// The error strings are what the panel shows in place of a list, so they say
/// what the user can do about it rather than naming the endpoint that failed.
fn versions_for(
    db: &Path,
    token: &str,
    release_id: u64,
) -> Result<Vec<discogs::MasterVersion>, String> {
    let id = release_id.to_string();
    let cached_master = Catalog::open(db)
        .ok()
        .and_then(|cat| cat.cached_release(&id).ok().flatten())
        .and_then(|d| d.master_id);
    if cached_master.is_none() && token.trim().is_empty() {
        return Err(
            "No Discogs token set. Add one in Settings to look up other \
                    pressings."
                .to_string(),
        );
    }
    let client = discogs::Client::new(
        token.to_string(),
        "Ordnung/0.1 +https://kailazy.github.io/Ordnung/",
    );
    let master_id = match cached_master {
        Some(m) => Some(m),
        None => {
            client
                .fetch_release(&id)
                .map_err(|e| format!("Couldn't reach Discogs: {e}"))?
                .master_id
        }
    };
    // No master means Discogs files this release on its own — a one-off with no
    // siblings to list. That's a fact about the record, not a failure.
    let Some(master_id) = master_id else {
        return Ok(Vec::new());
    };
    client
        .master_versions(master_id)
        .map_err(|e| format!("Couldn't load other pressings: {e}"))
}
