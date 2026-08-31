//! Looking up records that aren't yours yet — the Discogs half of the search box.
//!
//! The search box has always answered "what do I have?", over the digital
//! catalog and the cached collection/wantlist (`search_box.rs`). This adds the
//! other question a digger asks — "what exists?" — by pointing the same box at
//! Discogs's release database.
//!
//! The two are deliberately *modes*, not one blended list, chosen by a toggle
//! on the right of the field. Blending them would make every keystroke a
//! network request and leave the user unsure whether an absent record means
//! "you don't own it" or "it isn't there". A mode makes the scope explicit,
//! keeps ordinary library filtering entirely offline, and gives the remote
//! results the whole popup to show the fields that actually disambiguate a
//! pressing.
//!
//! Responsiveness is the design constraint, because Discogs is a blocking API
//! paced at ~1 request/1.1s (`MIN_API_INTERVAL`). Three things keep the UI from
//! ever waiting on it:
//!
//! - **The fetch is off the UI thread.** One-shot `thread::spawn` + `mpsc`,
//!   polled with `try_recv`, exactly as `dig.rs` does. The frame never blocks.
//! - **Stale answers are dropped.** Each request carries the `generation` it was
//!   issued for; a reply whose generation isn't current is discarded, so a slow
//!   response for "met" can't overwrite the results for "metro area".
//! - **Repeat queries never hit the network.** Answered queries are memoised in
//!   `record_cache`, so backspacing to a previous query is instant.
//!
//! Per `ordnung-architecture`, the search itself lives in
//! `ordnung_core::discogs::search_records`; this module is state, threading and
//! presentation only.

use super::*;
use crate::ui::tokens::{color, radius, space};
use crate::search_box::clipped_line;
use ordnung_core::discogs::{self, RecordHit};
use std::thread;

/// How long the box waits after the last keystroke before asking Discogs.
///
/// Much longer than the local `SEARCH_DEBOUNCE` (150ms) on purpose: a local
/// query is a bounded SQL prefilter, while this one is a rate-limited network
/// round trip. 450ms is past the pause that ends a typed word, so a normal
/// typist spends one request per query rather than one per keystroke.
pub(crate) const RECORD_DEBOUNCE: Duration = Duration::from_millis(450);

/// How many results one lookup asks Discogs for. Only [`RECORD_HITS_SHOWN`] are
/// ever drawn, but the extras cost nothing (one request either way) and cover
/// the case where the first few are dropped as unusable.
const RECORD_PER_PAGE: u32 = 25;

/// How many results the popup shows. Five, with the total count in the footer
/// standing in for the rest — a lookup popup is a quick answer, not a browser,
/// and five large rows read at a glance where a scrolling list of twenty-five
/// has to be worked through.
const RECORD_HITS_SHOWN: usize = 5;

/// Cap on the memoised query cache. Small: entries are only worth keeping for
/// the backspace-and-retype case within a session.
const RECORD_CACHE_MAX: usize = 32;

/// Height of one result row and the cover square inside it.
///
/// Considerably taller than a library hit row: a lookup result is four stacked
/// lines (format eyebrow, title, artist, year · country) against a large cover,
/// which is what makes a list of near-identical pressings scannable. With only
/// [`RECORD_HITS_SHOWN`] rows on screen there's room to spend on it.
const ROW_H: f32 = 84.0;
const ROW_COVER_PX: f32 = 68.0;

/// Radius of the two membership buttons on the right of each row.
const LIST_BTN_R: f32 = 9.0;

/// Motion for a row's hover wash and the cover fading in.
const ROW_HIGHLIGHT_ANIM: f32 = 0.11;
const COVER_FADE_ANIM: f32 = 0.22;

/// What the search box is currently searching. Toggled by the control on the
/// right of the field.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum SearchScope {
    /// The digital catalog plus the cached Discogs collection/wantlist. Local,
    /// instant, offline.
    #[default]
    Library,
    /// All of Discogs. Networked, debounced, rate-limited.
    Discogs,
}

/// Where a lookup stands, as far as the popup needs to know.
///
/// `Idle` and `Done(empty)` are deliberately different states: one means "we
/// haven't asked", the other "we asked and Discogs has nothing", and a lookup UI
/// that conflates them leaves the user unsure whether to wait.
#[derive(Clone, Debug)]
pub(crate) enum RecordSearch {
    /// Nothing typed, or the query is still inside the debounce window.
    Idle,
    /// A request is in flight. Carries the query so the popup can say what it's
    /// waiting on rather than showing a bare spinner.
    Loading(String),
    /// Results for the query, possibly empty.
    Done {
        hits: Vec<RecordHit>,
        /// Total matches Discogs reports, which is usually far more than the
        /// page we fetched — worth saying so the count doesn't read as "that's
        /// all there is".
        items: u32,
    },
    /// The lookup failed. Carries a plain-language reason.
    Failed(String),
}

/// A finished lookup, handed back from the worker thread.
pub(crate) struct RecordFetched {
    /// Which request this answers. Compared against `record_generation` to drop
    /// replies for queries the user has already typed past.
    pub(crate) generation: u64,
    pub(crate) query: String,
    pub(crate) result: std::result::Result<(Vec<RecordHit>, u32), String>,
}

impl App {
    /// Is the search box pointed at Discogs right now?
    pub(crate) fn searching_discogs(&self) -> bool {
        self.search_scope == SearchScope::Discogs
    }

    /// Flip the search box between the library and Discogs.
    ///
    /// Switching *to* Discogs with a query already typed runs it immediately
    /// rather than waiting for another keystroke — the user just asked for
    /// these results by flipping the switch, so making them type a character to
    /// get them would be a dead control.
    pub(crate) fn set_search_scope(&mut self, scope: SearchScope) {
        if self.search_scope == scope {
            return;
        }
        self.search_scope = scope;
        self.search_cursor = None;
        match scope {
            SearchScope::Discogs => {
                self.search_popup_open = true;
                self.start_record_search();
            }
            SearchScope::Library => {
                // Drop any in-flight lookup's claim on the UI. The generation
                // bump means a reply still on its way is ignored on arrival.
                self.record_generation += 1;
                self.record_search = RecordSearch::Idle;
                self.refresh_search_hits();
            }
        }
    }

    /// Kick off a Discogs lookup for the current query, unless the answer is
    /// already known.
    ///
    /// Called from the debounce tick and from the scope toggle. Safe to call
    /// repeatedly: an unchanged query that's already loading or already answered
    /// from cache costs nothing.
    pub(crate) fn start_record_search(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.record_search = RecordSearch::Idle;
            return;
        }
        // Already answered this exact query in this session: show it now and
        // spend no request. This is what makes backspacing feel instant.
        if let Some((hits, items)) = self.record_cache.get(&q) {
            self.record_search = RecordSearch::Done {
                hits: hits.clone(),
                items: *items,
            };
            return;
        }
        // Already waiting on this same query — don't stack a second request on
        // top of it.
        if matches!(&self.record_search, RecordSearch::Loading(inflight) if inflight == &q) {
            return;
        }
        let token = self.discogs_token();
        if token.trim().is_empty() {
            self.record_search = RecordSearch::Failed(
                "Add a Discogs token in Settings to look up records.".into(),
            );
            return;
        }
        self.record_generation += 1;
        let generation = self.record_generation;
        self.record_search = RecordSearch::Loading(q.clone());
        let (tx, ctx) = (self.record_tx.clone(), self.egui_ctx.clone());
        let query = q;
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let result = client
                .search_records(&query, 1, RECORD_PER_PAGE)
                .map(|page| (page.hits, page.items))
                .map_err(|e| e.to_string());
            let _ = tx.send(RecordFetched {
                generation,
                query,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Drain finished lookups. Called each frame beside the other polls.
    pub(crate) fn poll_records(&mut self) {
        while let Ok(msg) = self.record_rx.try_recv() {
            // The user has typed on since this went out — the answer is about a
            // query they're no longer asking.
            if msg.generation != self.record_generation {
                continue;
            }
            match msg.result {
                Ok((hits, items)) => {
                    if self.record_cache.len() >= RECORD_CACHE_MAX {
                        self.record_cache.clear();
                    }
                    self.record_cache
                        .insert(msg.query, (hits.clone(), items));
                    self.record_search = RecordSearch::Done { hits, items };
                }
                Err(e) => self.record_search = RecordSearch::Failed(e),
            }
        }
    }

    /// Draw the Discogs results list inside the already-open popup frame.
    ///
    /// Returns the release the user clicked, if any — applied by the caller so
    /// the list doesn't mutate `self` mid-render.
    pub(crate) fn draw_record_results(&mut self, ui: &mut egui::Ui) -> Option<RecordHit> {
        // Snapshot what we're drawing so the row loop can borrow `self` for
        // cover lookups without fighting the borrow on `record_search`.
        let state = self.record_search.clone();
        let query = self.search_query.trim().to_string();
        match state {
            RecordSearch::Idle => {
                note(ui, "Type to search Discogs");
                None
            }
            RecordSearch::Loading(q) => {
                ui.add_space(space::S2);
                ui.horizontal(|ui| {
                    ui.add_space(space::S3);
                    ui.spinner();
                    ui.label(
                        egui::RichText::new(format!("Searching Discogs for “{q}”…"))
                            .color(color::LABEL_3),
                    );
                });
                ui.add_space(space::S2);
                // A spinner only reads as motion if frames keep coming.
                ui.ctx().request_repaint();
                None
            }
            RecordSearch::Failed(e) => {
                note(ui, &e);
                None
            }
            RecordSearch::Done { hits, items } => {
                if hits.is_empty() {
                    note(ui, "No records on Discogs match that");
                    return None;
                }
                let mut chosen = None;
                let mut toggled: Option<(VinylList, RecordHit, bool, bool)> = None;
                let mut want_covers: Vec<String> = Vec::new();
                // Only the first few are drawn. The popup is a quick answer, so
                // it shows a glanceable handful and lets the footer account for
                // the rest, rather than becoming a list to scroll through.
                let shown = hits.len().min(RECORD_HITS_SHOWN);
                for (i, hit) in hits.iter().take(shown).enumerate() {
                    let selected = self.search_cursor == Some(i);
                    let tex = match self.dig_covers.get(&hit.thumb_url) {
                        Some(ThumbState::Ready(t)) => t.clone(),
                        Some(_) => None,
                        None => {
                            if !hit.thumb_url.is_empty() {
                                want_covers.push(hit.thumb_url.clone());
                            }
                            None
                        }
                    };
                    // Owning or wanting a record is the single most useful thing
                    // to know while digging, and it's free: both id sets are
                    // already in memory.
                    let owned = self.vinyl_owned.contains(&hit.release_id);
                    let wanted = self.vinyl_wanted.contains(&hit.release_id);
                    match record_row(ui, hit, selected, tex, owned, wanted) {
                        Some(RowAct::Open) => chosen = Some(hit.clone()),
                        // Wanting or collecting leaves the popup up: the whole
                        // point of a lookup list is to work down it, and
                        // dismissing after each add would mean re-running the
                        // search for every record.
                        Some(RowAct::Toggle(list)) => {
                            toggled = Some((list, hit.clone(), owned, wanted))
                        }
                        None => {}
                    }
                    // Hairlines between rows, not around them — the list reads
                    // as one block the way the rows on discogs.com do.
                    if i + 1 < shown {
                        let y = ui.min_rect().bottom();
                        ui.painter().hline(
                            ui.min_rect().x_range(),
                            y,
                            egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
                        );
                    }
                }
                // Account for everything the five rows didn't show, so the list
                // never implies it's the whole answer.
                if items as usize > shown {
                    if footer(ui, items).clicked() {
                        crate::util::open_url(&crate::util::discogs_url(None, &query));
                    }
                }
                // Cover requests and the membership edit are both issued after
                // the render borrow ends.
                for url in want_covers {
                    self.dig_cover(&url);
                }
                if let Some((list, hit, owned, wanted)) = toggled {
                    self.toggle_record_list(list, &hit, owned, wanted);
                }
                chosen
            }
        }
    }

    /// Add a looked-up record to a list, or take it back out.
    ///
    /// Mirrors the release sheet's own membership toggle so both surfaces
    /// behave identically — including the confirmation `request_vinyl_edit`
    /// interposes when an edit would destroy a collection copy (its date added,
    /// rating and notes can't be restored).
    ///
    /// Adding is by bare release id, which is all a lookup result has. Removing
    /// needs the *cached* row, since a collection copy is addressed by folder and
    /// instance id rather than release id; anything actually in a list has one,
    /// because that's where membership is read from.
    fn toggle_record_list(
        &mut self,
        list: VinylList,
        hit: &RecordHit,
        owned: bool,
        wanted: bool,
    ) {
        // One background job at a time — the shared worker channel is
        // single-slot, and a second edit would silently displace the first.
        if self.is_busy() {
            self.status = "Still working on the last change — one moment.".into();
            return;
        }
        let label = edit_label(hit);
        let present = match list {
            VinylList::Collection => owned,
            VinylList::Wantlist => wanted,
        };
        let edit = if present {
            let Some(record) = self.vinyl_record_in(list, hit.release_id) else {
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
                VinylList::Collection => VinylEdit::Collect {
                    release_id: hit.release_id,
                    label,
                },
                VinylList::Wantlist => VinylEdit::Want {
                    release_ids: vec![hit.release_id],
                    label,
                },
            }
        };
        let ctx = self.egui_ctx.clone();
        self.request_vinyl_edit(ctx, edit);
    }

    /// Act on a chosen lookup result: open the record's sheet.
    ///
    /// A looked-up record is by definition one you may not own, so this goes
    /// through `open_release_sheet` — the bare-release path a dig uses — rather
    /// than the collection-keyed one. The sheet then does the rest of the work
    /// the design brief asks of a confirmed match: tracklist, videos, "other
    /// versions", marketplace price, and links to any copies already in the
    /// catalog.
    pub(crate) fn open_record_hit(&mut self, hit: RecordHit, ctx: &egui::Context) {
        self.search_popup_open = false;
        self.search_cursor = None;
        let cover = if hit.cover_image_url.is_empty() {
            Some(hit.thumb_url.clone()).filter(|u| !u.is_empty())
        } else {
            Some(hit.cover_image_url.clone())
        };
        self.open_release_sheet(
            hit.release_id,
            hit.artist.clone(),
            hit.title.clone(),
            sheet_subtitle(&hit),
            cover,
            ctx,
        );
    }

    /// How many rows the Discogs list currently has, for keyboard navigation.
    ///
    /// Counts what's *drawn*, not what was fetched: arrowing past the last
    /// visible row would otherwise highlight nothing and Enter would open a
    /// record the user can't see.
    pub(crate) fn record_hit_count(&self) -> usize {
        match &self.record_search {
            RecordSearch::Done { hits, .. } => hits.len().min(RECORD_HITS_SHOWN),
            _ => 0,
        }
    }

    /// The record at `i` in the current results, if any.
    pub(crate) fn record_hit_at(&self, i: usize) -> Option<RecordHit> {
        match &self.record_search {
            RecordSearch::Done { hits, .. } => hits.get(i).cloned(),
            _ => None,
        }
    }
}

/// Total width the scope toggle occupies, reserved by the toolbar so the search
/// field shrinks around it instead of pushing it off the edge.
pub(crate) const SCOPE_TOGGLE_W: f32 = 132.0;

/// Motion for the selected segment sliding between the two halves.
const SCOPE_SLIDE_ANIM: f32 = 0.16;

impl App {
    /// The two-position switch on the right of the search field: **Library** or
    /// **Discogs**.
    ///
    /// A segmented control rather than a checkbox or a menu because the choice
    /// is a scope with exactly two values, both worth naming — the user needs to
    /// read "what am I about to search" without opening anything. The selected
    /// half slides rather than blinking across, which is what makes it read as
    /// one switch with a position instead of two buttons that light up.
    pub(crate) fn draw_scope_toggle(&mut self, ui: &mut egui::Ui) {
        let h = 26.0;
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(SCOPE_TOGGLE_W, h),
            egui::Sense::hover(),
        );
        let painter = ui.painter();
        painter.rect_filled(rect, egui::Rounding::same(radius::SM), color::FIELD);

        let half = rect.width() / 2.0;
        let on_discogs = self.searching_discogs();
        // Animate the *position*, so switching slides the pill across rather
        // than teleporting it.
        let t = ui.ctx().animate_bool_with_time_and_easing(
            egui::Id::new("search_scope_pos"),
            on_discogs,
            SCOPE_SLIDE_ANIM,
            egui::emath::easing::cubic_out,
        );
        let pill = egui::Rect::from_min_size(
            egui::pos2(rect.left() + half * t, rect.top()),
            egui::vec2(half, h),
        )
        .shrink(2.0);
        painter.rect_filled(pill, egui::Rounding::same(radius::XS), color::SURFACE_HOVER);

        let mut clicked: Option<SearchScope> = None;
        for (i, (scope, label)) in [
            (SearchScope::Library, "Library"),
            (SearchScope::Discogs, "Discogs"),
        ]
        .into_iter()
        .enumerate()
        {
            let seg = egui::Rect::from_min_size(
                egui::pos2(rect.left() + half * i as f32, rect.top()),
                egui::vec2(half, h),
            );
            let resp = ui.interact(
                seg,
                ui.id().with(("search_scope", i)),
                egui::Sense::click(),
            );
            if resp.clicked() {
                clicked = Some(scope);
            }
            let selected = self.search_scope == scope;
            // The unselected half brightens on hover so the control advertises
            // that both sides are live, not just the lit one.
            let ink = if selected {
                color::LABEL
            } else if resp.hovered() {
                color::LABEL_2
            } else {
                color::LABEL_3
            };
            ui.painter().text(
                seg.center(),
                egui::Align2::CENTER_CENTER,
                label,
                egui::TextStyle::Small.resolve(ui.style()),
                ink,
            );
        }
        if let Some(scope) = clicked {
            self.set_search_scope(scope);
        }
    }
}

/// The strip under the five rows: how many matches there are in total, and a
/// way out to the full result set on discogs.com.
///
/// The count is the honest part — a lookup that found 1,204 matches and drew
/// five of them should say so, or the list quietly misrepresents itself as the
/// whole answer. The click-through goes to the browser rather than paginating
/// in-app: past the first handful the user is browsing, and the browser is
/// better at that than a popup under a search field.
fn footer(ui: &mut egui::Ui, items: u32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), 30.0),
        egui::Sense::click(),
    );
    let hot = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hot"), resp.hovered(), ROW_HIGHLIGHT_ANIM);
    if hot > 0.0 {
        ui.painter().rect_filled(
            rect,
            ui.visuals().widgets.hovered.rounding,
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(hot),
        );
    }
    ui.painter().hline(
        rect.x_range(),
        rect.top(),
        egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
    );
    let style = egui::TextStyle::Small.resolve(ui.style());
    ui.painter().text(
        egui::pos2(rect.left() + space::S3, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{} matches on Discogs", thousands(items)),
        style.clone(),
        color::LABEL_3,
    );
    // The affordance brightens with the row, so it reads as the thing the click
    // does rather than as decoration.
    ui.painter().text(
        egui::pos2(rect.right() - space::S3, rect.center().y),
        egui::Align2::RIGHT_CENTER,
        "View all  →",
        style,
        if hot > 0.0 { color::LABEL } else { color::LABEL_3 },
    );
    resp
}

/// Group a count with thin separators: `1204` reads as `1,204`. Big numbers are
/// the normal case here (a common word matches thousands of releases), and an
/// ungrouped five-digit run is genuinely hard to size up at a glance.
fn thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// One line of muted text in the popup, for every state that has no rows.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add_space(space::S2);
    ui.horizontal(|ui| {
        ui.add_space(space::S3);
        ui.label(egui::RichText::new(text).color(color::LABEL_3));
    });
    ui.add_space(space::S2);
}

/// What a click on a result row asked for.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowAct {
    /// The row itself — open this record's sheet.
    Open,
    /// One of the two membership buttons. Adds when the record isn't in that
    /// list, removes when it is.
    Toggle(VinylList),
}

/// Paint one Discogs result. Returns what the user clicked, if anything.
///
/// Four stacked lines against a large cover, in the order a digger reads them:
/// the **format** as a small eyebrow, the **title** in bold, the **artist**, and
/// finally `year · country`. Identity first, provenance last — the eyebrow says
/// what kind of object this is before you've read a word of it, which is the
/// question that decides whether a row is worth your attention at all.
///
/// The label and catalog number are deliberately *not* here. They're what
/// separates two pressings of the same record, which matters once you've picked
/// a record and are choosing between its versions — the release sheet's job.
/// In a five-row lookup they'd crowd out the identity the list exists to show.
fn record_row(
    ui: &mut egui::Ui,
    hit: &RecordHit,
    selected: bool,
    tex: Option<Tex>,
    owned: bool,
    wanted: bool,
) -> Option<RowAct> {
    let (slot, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), ROW_H),
        egui::Sense::click(),
    );
    let hot = ui.ctx().animate_bool_with_time(
        resp.id.with("hot"),
        selected || resp.hovered(),
        ROW_HIGHLIGHT_ANIM,
    );
    if hot > 0.0 {
        let inset = 2.0 * (1.0 - hot);
        ui.painter().rect_filled(
            slot.shrink2(egui::vec2(inset, inset * 0.5)),
            ui.visuals().widgets.hovered.rounding,
            ui.visuals()
                .widgets
                .hovered
                .weak_bg_fill
                .gamma_multiply(hot),
        );
    }
    let pad = space::S3;
    let art = egui::Rect::from_min_size(
        egui::pos2(slot.left() + pad, slot.center().y - ROW_COVER_PX / 2.0),
        egui::vec2(ROW_COVER_PX, ROW_COVER_PX),
    );
    let rounding = egui::Rounding::same(radius::XS);
    // Covers arrive whenever the CDN answers; cross-fade so a late texture
    // doesn't punch a hole in a settled list.
    let art_t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("art"), tex.is_some(), COVER_FADE_ANIM);
    if art_t < 1.0 {
        ui.painter().rect_filled(art, rounding, color::SURFACE_HI);
    }
    if let Some(handle) = &tex {
        egui::Image::new(handle)
            .rounding(rounding)
            .tint(egui::Color32::WHITE.gamma_multiply(art_t))
            .paint_at(ui, art);
    }

    // The two membership buttons, laid out from the right edge inward. Done
    // before the text so the lines know where to stop rather than running
    // underneath them.
    let mut act = None;
    let mut right_edge = slot.right() - pad;
    for (list, present) in [
        (VinylList::Collection, owned),
        (VinylList::Wantlist, wanted),
    ] {
        let c = egui::pos2(right_edge - LIST_BTN_R, slot.center().y);
        let hit_rect =
            egui::Rect::from_center_size(c, egui::vec2(LIST_BTN_R * 2.2, LIST_BTN_R * 2.2));
        let br = ui.interact(
            hit_rect,
            resp.id.with(("list", list == VinylList::Collection)),
            egui::Sense::click(),
        );
        if br.clicked() {
            act = Some(RowAct::Toggle(list));
        }
        draw_list_button(ui, c, list, present, br.hovered(), hot);
        // A tooltip, because a symbol alone can't say that clicking a filled one
        // takes the record back out again.
        br.on_hover_text(match (list, present) {
            (VinylList::Collection, false) => "Add to your collection",
            (VinylList::Collection, true) => "Remove from your collection",
            (VinylList::Wantlist, false) => "Add to your wantlist",
            (VinylList::Wantlist, true) => "Remove from your wantlist",
        });
        right_edge -= LIST_BTN_R * 2.0 + space::S3;
    }
    // Clicking a button must not also open the sheet behind it.
    if act.is_none() && resp.clicked() {
        act = Some(RowAct::Open);
    }

    let text_x = art.right() + space::S4;
    let avail = (right_edge - space::S2 - text_x).max(0.0);
    let painter = ui.painter();

    // Four baselines measured off the row centre, so the block stays vertically
    // centred against the cover however tall the row is.
    let eyebrow_y = slot.center().y - 27.0;
    let title_y = slot.center().y - 9.0;
    let artist_y = slot.center().y + 10.0;
    let meta_y = slot.center().y + 27.0;

    // The format eyebrow: letter-spaced small caps, the way a category label
    // reads on a record sleeve rather than as another line of prose.
    let eyebrow = format_eyebrow(&hit.format);
    if !eyebrow.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, eyebrow_y),
            avail,
            eyebrow,
            egui::TextStyle::Small,
            color::LABEL_3,
        );
    }

    let title = if hit.title.is_empty() {
        format!("Release {}", hit.release_id)
    } else {
        hit.title.clone()
    };
    clipped_line(
        ui,
        painter,
        egui::pos2(text_x, title_y),
        avail,
        title,
        egui::TextStyle::Body,
        color::LABEL,
    );
    if !hit.artist.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, artist_y),
            avail,
            hit.artist.clone(),
            egui::TextStyle::Small,
            color::LABEL_2,
        );
    }
    let meta = pressing_line(hit);
    if !meta.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, meta_y),
            avail,
            meta,
            egui::TextStyle::Small,
            color::LABEL_3,
        );
    }
    act
}

/// How a record names itself in the status line and the confirmation dialog for
/// a membership edit.
///
/// Discogs regularly returns a release with no parsed artist (an untitled white
/// label, or a title with no ` - ` separator). Formatting unconditionally would
/// leave a dangling "— Title" in the status bar, so a missing artist collapses
/// to the title alone.
fn edit_label(hit: &RecordHit) -> String {
    if hit.artist.is_empty() {
        hit.title.clone()
    } else {
        format!("{} — {}", hit.artist, hit.title)
    }
}

/// Paint one membership button: a record in its sleeve for the collection, an
/// eye for the wantlist.
///
/// The two metaphors are about *where a record is*, not about generic approval:
/// a collection is the record on your shelf, and a wantlist is the set you're
/// keeping an eye on. Both read at 18px without a label, which a checkmark
/// only manages by convention.
///
/// **Filled means you have it.** The same symbol carries both the state and the
/// action, so a row needs no separate badge — an outline is an invitation, a
/// solid one is a fact.
///
/// The buttons stay dim until the row is hovered, then rise to full strength —
/// five rows each showing two lit controls would fight the titles for
/// attention, and you only need them on the row you're pointing at.
fn draw_list_button(
    ui: &egui::Ui,
    c: egui::Pos2,
    list: VinylList,
    present: bool,
    hovered: bool,
    row_hot: f32,
) {
    let p = ui.painter();
    // A present mark stays legible on an unhovered row — it's reporting a fact
    // the user should see without pointing at anything. An absent one is only
    // an offer, so it fades up with the row.
    let base = if present {
        color::LABEL_2
    } else {
        color::LABEL_3.gamma_multiply(0.35 + 0.65 * row_hot)
    };
    let ink = if hovered { color::LABEL } else { base };
    if hovered {
        p.circle_filled(c, LIST_BTN_R + 3.0, color::SURFACE_HI);
    }
    match list {
        VinylList::Collection => draw_sleeve(p, c, LIST_BTN_R, ink, present),
        VinylList::Wantlist => draw_eye(p, c, LIST_BTN_R, ink, present),
    }
}

/// A record half out of its sleeve: a rounded square with a disc emerging from
/// its right edge. Solid when the record is in the collection, outlined when
/// it isn't.
///
/// Went through a stack of slabs first, which rendered as a hamburger menu —
/// three equal bars is far too overloaded a glyph to mean "records", and the
/// taper that would have distinguished it disappears at 18px. A sleeve with a
/// disc sliding out is unmistakable at this size, and one record is the right
/// unit anyway: the row *is* one record, and the question the button answers is
/// whether **it** is on your shelf.
fn draw_sleeve(p: &egui::Painter, c: egui::Pos2, r: f32, ink: egui::Color32, filled: bool) {
    let w = r * 0.92;
    // The sleeve sits left of centre so the disc has somewhere to emerge to,
    // keeping the pair balanced on `c` rather than hanging off it.
    let sleeve = egui::Rect::from_min_max(
        egui::pos2(c.x - w * 1.02, c.y - w),
        egui::pos2(c.x + w * 0.30, c.y + w),
    );
    let rounding = egui::Rounding::same(1.3);
    let disc_c = egui::pos2(c.x + w * 0.34, c.y);
    let disc_r = w * 0.86;
    if filled {
        p.rect_filled(sleeve, rounding, ink);
        p.circle_filled(disc_c, disc_r, ink);
        // The spindle hole is punched in the row's own ground, which is what
        // keeps a solid disc reading as a record rather than as a dot.
        p.circle_filled(disc_c, disc_r * 0.24, color::SURFACE);
    } else {
        p.rect_stroke(sleeve, rounding, egui::Stroke::new(1.3, ink));
        p.circle_stroke(disc_c, disc_r, egui::Stroke::new(1.3, ink));
        p.circle_filled(disc_c, disc_r * 0.22, ink);
    }
}

/// An eye: a pointed-oval outline with a pupil, filled when the record is on
/// the wantlist.
///
/// The lid is two mirrored quadratic arcs meeting at sharp corners — a plain
/// ellipse reads as a coin, and the corners are what make it an eye. Sampled
/// into one closed path so the outline and the fill describe the same shape.
fn draw_eye(p: &egui::Painter, c: egui::Pos2, r: f32, ink: egui::Color32, filled: bool) {
    let hw = r * 1.15; // half-width, corner to corner
    let hh = r * 0.72; // how far the lids bow from the centre line
    const STEPS: usize = 14;
    let mut pts: Vec<egui::Pos2> = Vec::with_capacity(STEPS * 2 + 2);
    // Upper lid left→right, then lower lid back, giving one closed outline.
    for side in [-1.0_f32, 1.0] {
        for i in 0..=STEPS {
            // Traverse the lower lid in reverse so the path stays continuous.
            let t = if side < 0.0 {
                i as f32 / STEPS as f32
            } else {
                1.0 - i as f32 / STEPS as f32
            };
            let x = -hw + 2.0 * hw * t;
            // Quadratic bow: zero at the corners, `hh` at the centre.
            let bow = (1.0 - (2.0 * t - 1.0).powi(2)) * hh;
            pts.push(egui::pos2(c.x + x, c.y + side * bow));
        }
    }
    if filled {
        p.add(egui::Shape::convex_polygon(pts, ink, egui::Stroke::NONE));
        // The pupil is punched back out in the row's own background so a solid
        // eye still reads as an eye rather than as a filled leaf.
        p.circle_filled(c, r * 0.34, color::SURFACE);
    } else {
        p.add(egui::Shape::closed_line(pts, egui::Stroke::new(1.3, ink)));
        p.circle_filled(c, r * 0.30, ink);
    }
}

/// The small category label above the title: the *carrier*, not the full format
/// string.
///
/// Discogs hands back `Vinyl, 12", 33 ⅓ RPM, Album` — accurate but far too long
/// to sit above a title. What a digger reads at this position is only "is this a
/// record?", so this reduces to the carrier and spaces it out as small caps.
/// The sizes and speeds still appear on the meta line via [`pressing_line`].
fn format_eyebrow(format: &str) -> String {
    let f = format.to_ascii_lowercase();
    // Order matters: the non-vinyl carriers are checked first so a "CD, Comp"
    // can't match on the `lp` inside a word like "sampler".
    let carrier = if f.contains("cassette") {
        "CASSETTE"
    } else if f.contains("dvd") {
        "DVD"
    } else if f.contains("shellac") {
        "SHELLAC"
    } else if f.contains("cd") {
        "CD"
    } else if f.contains("file") {
        "DIGITAL"
    } else if f.contains("vinyl")
        || f.contains("lp")
        || f.contains("12\"")
        || f.contains("10\"")
        || f.contains("7\"")
    {
        "VINYL"
    } else {
        return String::new();
    };
    // Letter-spaced, since egui has no tracking control and the eyebrow needs
    // to read as a label rather than a shouted word.
    carrier
        .chars()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join("\u{2009}")
}

/// The row's last line: `year · country`, the two facts that place a pressing
/// in time and space.
///
/// Label and catalog number are left out on purpose — see [`record_row`]. They
/// still reach the user through the release sheet, whose subtitle is built by
/// [`sheet_subtitle`] and has room for the full citation.
fn pressing_line(hit: &RecordHit) -> String {
    [hit.year.trim(), hit.country.trim()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The fuller citation handed to the release sheet when a lookup result is
/// opened: everything that identifies this exact pressing, including the format
/// details and the label/catalog number the row omits.
///
/// Label and catalog number are joined with a space rather than a separator —
/// "Environ ENV 006", the way a record is actually cited, not "Environ · ENV 006".
fn sheet_subtitle(hit: &RecordHit) -> String {
    let (label, catno) = (hit.label.trim(), hit.catno.trim());
    let imprint = match (label.is_empty(), catno.is_empty()) {
        (false, false) => format!("{label} {catno}"),
        (false, true) => label.to_string(),
        (true, false) => catno.to_string(),
        (true, true) => String::new(),
    };
    [
        hit.year.trim(),
        hit.format.trim(),
        imprint.as_str(),
        hit.country.trim(),
    ]
    .into_iter()
    .filter(|s| !s.is_empty())
    .collect::<Vec<_>>()
    .join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit() -> RecordHit {
        RecordHit {
            release_id: 1,
            artist: "Drexciya".into(),
            title: "Deep Sea Dweller".into(),
            year: "1992".into(),
            label: "Shockwave Records".into(),
            catno: "SW1007".into(),
            country: "US".into(),
            format: "Vinyl, 12\", 33 ⅓ RPM".into(),
            thumb_url: String::new(),
            cover_image_url: String::new(),
        }
    }

    #[test]
    fn meta_line_is_year_and_country() {
        assert_eq!(pressing_line(&hit()), "1992 · US");
    }

    /// Discogs leaves plenty of these blank; a missing field should vanish
    /// rather than leave a dangling separator.
    #[test]
    fn meta_line_drops_empty_fields() {
        let mut h = hit();
        h.country = String::new();
        assert_eq!(pressing_line(&h), "1992");
        h.year = String::new();
        assert_eq!(pressing_line(&h), "");
    }

    /// The sheet gets the full citation the compact row leaves out.
    #[test]
    fn sheet_subtitle_carries_the_full_citation() {
        assert_eq!(
            sheet_subtitle(&hit()),
            "1992 · Vinyl, 12\", 33 ⅓ RPM · Shockwave Records SW1007 · US"
        );
    }

    #[test]
    fn sheet_subtitle_joins_label_and_catno_without_a_separator() {
        let mut h = hit();
        h.format = String::new();
        h.country = String::new();
        assert_eq!(sheet_subtitle(&h), "1992 · Shockwave Records SW1007");
    }

    /// The eyebrow reduces a long format string to just the carrier.
    #[test]
    fn eyebrow_reduces_format_to_the_carrier() {
        assert_eq!(format_eyebrow("Vinyl, 12\", 33 ⅓ RPM, Album"), spaced("VINYL"));
        assert_eq!(format_eyebrow("LP, Album, Reissue"), spaced("VINYL"));
        assert_eq!(format_eyebrow("CD, Compilation"), spaced("CD"));
        assert_eq!(format_eyebrow("Cassette, Album"), spaced("CASSETTE"));
        assert_eq!(format_eyebrow("File, FLAC, Album"), spaced("DIGITAL"));
    }

    /// A CD compilation must not match on the `lp` hiding inside "Sampler".
    #[test]
    fn eyebrow_does_not_read_a_cd_as_vinyl() {
        assert_eq!(format_eyebrow("CD, Sampler"), spaced("CD"));
    }

    /// Discogs sometimes gives no format at all; the eyebrow then says nothing
    /// rather than guessing a carrier.
    #[test]
    fn eyebrow_is_empty_when_the_format_is_unknown() {
        assert_eq!(format_eyebrow(""), "");
        assert_eq!(format_eyebrow("Box Set"), "");
    }

    #[test]
    fn edit_label_names_artist_and_title() {
        assert_eq!(edit_label(&hit()), "Drexciya — Deep Sea Dweller");
    }

    /// A white label with no parsed artist must not produce a dangling dash.
    #[test]
    fn edit_label_drops_the_dash_without_an_artist() {
        let mut h = hit();
        h.artist = String::new();
        assert_eq!(edit_label(&h), "Deep Sea Dweller");
    }

    fn spaced(s: &str) -> String {
        s.chars()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join("\u{2009}")
    }
}
