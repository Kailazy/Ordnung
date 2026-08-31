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

/// How many results one lookup asks Discogs for. The popup is scrollable, so
/// this is about how much a single request is worth fetching, not how much fits.
const RECORD_PER_PAGE: u32 = 25;

/// Cap on the memoised query cache. Small: entries are only worth keeping for
/// the backspace-and-retype case within a session.
const RECORD_CACHE_MAX: usize = 32;

/// Height of one result row. Taller than a local hit row — a record carries a
/// third line (label · catalog number · country) that a library hit doesn't.
const ROW_H: f32 = 62.0;
const ROW_COVER_PX: f32 = 46.0;

/// Motion for a row's hover wash and the "in your library" pill fading in.
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
                let mut want_covers: Vec<String> = Vec::new();
                // Cap the popup's height and let it scroll: a lookup returns far
                // more than the five hits the local list is built around, and a
                // popup taller than the window would be unreachable.
                egui::ScrollArea::vertical()
                    .max_height(360.0)
                    .show(ui, |ui| {
                        for (i, hit) in hits.iter().enumerate() {
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
                            // Owning or wanting a record is the single most
                            // useful thing to know while digging, and it's free:
                            // both id sets are already in memory.
                            let owned = self.vinyl_owned.contains(&hit.release_id);
                            let wanted = self.vinyl_wanted.contains(&hit.release_id);
                            if record_row(ui, hit, selected, tex, owned, wanted) {
                                chosen = Some(hit.clone());
                            }
                        }
                        // Say how much more there is, so a scrolled-to-bottom
                        // list doesn't imply it's the whole answer.
                        let shown = hits.len() as u32;
                        if items > shown {
                            ui.add_space(space::S2);
                            ui.horizontal(|ui| {
                                ui.add_space(space::S3);
                                ui.label(
                                    egui::RichText::new(format!(
                                        "Showing {shown} of {items} matches"
                                    ))
                                    .small()
                                    .color(color::LABEL_3),
                                );
                            });
                            ui.add_space(space::S1);
                        }
                    });
                // Cover requests are issued after the render borrow ends.
                for url in want_covers {
                    self.dig_cover(&url);
                }
                chosen
            }
        }
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
            pressing_line(&hit),
            cover,
            ctx,
        );
    }

    /// How many rows the Discogs list currently has, for keyboard navigation.
    pub(crate) fn record_hit_count(&self) -> usize {
        match &self.record_search {
            RecordSearch::Done { hits, .. } => hits.len(),
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

/// One line of muted text in the popup, for every state that has no rows.
fn note(ui: &mut egui::Ui, text: &str) {
    ui.add_space(space::S2);
    ui.horizontal(|ui| {
        ui.add_space(space::S3);
        ui.label(egui::RichText::new(text).color(color::LABEL_3));
    });
    ui.add_space(space::S2);
}

/// Paint one Discogs result. Returns true when clicked.
///
/// Three lines, densest at the bottom: title, then artist, then the pressing
/// details (year · format · label · catalog number · country). That order is
/// deliberate — the first two identify the record, the third is what you read
/// only when you're deciding *which pressing*, which is exactly the job the
/// design brief calls out as the picker's whole purpose.
fn record_row(
    ui: &mut egui::Ui,
    hit: &RecordHit,
    selected: bool,
    tex: Option<Tex>,
    owned: bool,
    wanted: bool,
) -> bool {
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

    let text_x = art.right() + space::S4 - 2.0;
    // The membership pill is laid out first so the text lines know to stop
    // short of it rather than running underneath.
    let pill = if owned {
        Some(("In your collection", color::LABEL_3))
    } else if wanted {
        Some(("On your wantlist", color::LABEL_3))
    } else {
        None
    };
    let mut right_edge = slot.right() - pad;
    if let Some((label, tint)) = pill {
        let galley = ui.painter().layout_no_wrap(
            label.to_string(),
            egui::TextStyle::Small.resolve(ui.style()),
            tint,
        );
        let w = galley.size().x + space::S2 * 2.0;
        let pill_rect = egui::Rect::from_min_size(
            egui::pos2(right_edge - w, slot.center().y - 9.0),
            egui::vec2(w, 18.0),
        );
        ui.painter().rect_filled(
            pill_rect,
            egui::Rounding::same(9.0),
            color::SURFACE_HI,
        );
        ui.painter().galley(
            egui::pos2(pill_rect.left() + space::S2, pill_rect.center().y - galley.size().y / 2.0),
            galley,
            tint,
        );
        right_edge = pill_rect.left() - space::S2;
    }
    let avail = (right_edge - text_x).max(0.0);
    let painter = ui.painter();

    let title = if hit.title.is_empty() {
        format!("Release {}", hit.release_id)
    } else {
        hit.title.clone()
    };
    clipped_line(
        ui,
        painter,
        egui::pos2(text_x, slot.center().y - 16.0),
        avail,
        title,
        egui::TextStyle::Body,
        color::LABEL,
    );
    if !hit.artist.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, slot.center().y),
            avail,
            hit.artist.clone(),
            egui::TextStyle::Small,
            color::LABEL_2,
        );
    }
    let details = pressing_line(hit);
    if !details.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, slot.center().y + 16.0),
            avail,
            details,
            egui::TextStyle::Small,
            color::LABEL_3,
        );
    }
    resp.clicked()
}

/// The disambiguating third line: everything that separates one pressing of a
/// record from another, in the order a digger reads them. Empty fields drop out
/// rather than leaving stray separators.
fn pressing_line(hit: &RecordHit) -> String {
    let catno = hit.catno.trim();
    let label = hit.label.trim();
    // Label and catalog number belong together — "Environ ENV 006", not
    // "Environ · ENV 006" — because that's how a record is actually cited.
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
            artist: "Metro Area".into(),
            title: "Metro Area".into(),
            year: "2001".into(),
            label: "Environ".into(),
            catno: "ENV 006".into(),
            country: "US".into(),
            format: "2xLP, Album".into(),
            thumb_url: String::new(),
            cover_image_url: String::new(),
        }
    }

    #[test]
    fn pressing_line_reads_label_and_catno_as_one_citation() {
        assert_eq!(
            pressing_line(&hit()),
            "2001 · 2xLP, Album · Environ ENV 006 · US"
        );
    }

    /// Discogs leaves plenty of these blank; a missing field should vanish
    /// rather than leave a dangling separator.
    #[test]
    fn pressing_line_drops_empty_fields() {
        let mut h = hit();
        h.year = String::new();
        h.country = String::new();
        h.catno = String::new();
        assert_eq!(pressing_line(&h), "2xLP, Album · Environ");
    }

    #[test]
    fn pressing_line_is_empty_when_nothing_is_known() {
        let mut h = hit();
        h.year = String::new();
        h.country = String::new();
        h.catno = String::new();
        h.label = String::new();
        h.format = String::new();
        assert_eq!(pressing_line(&h), "");
    }
}
