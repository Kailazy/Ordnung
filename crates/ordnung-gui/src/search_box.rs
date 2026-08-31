//! The toolbar search box's suggestion dropdown.
//!
//! The box has always filtered the track table. This adds the other half: a
//! ranked list of concrete things the query names — songs in the digital
//! catalog and records in the Discogs collection/wantlist — so a search can
//! answer "what is this and where is it?" rather than only narrowing rows.
//!
//! Picking a hit navigates: a song selects and scrolls to it in the Library; a
//! record switches to the Vinyl view and opens its release sheet. Ranking and
//! matching live in `ordnung_core::search`; this module is presentation and
//! navigation only, per `ordnung-architecture`.

use super::*;
use crate::ui::tokens::{color, radius, space};

/// How many suggestions the popup shows.
pub(crate) const MAX_SEARCH_HITS: usize = 5;

/// Motion for the popup itself: how long the panel takes to settle open or fade
/// away. Short enough to feel immediate under fast typing, long enough that the
/// list reads as *arriving* rather than blinking into place.
const POPUP_ANIM: f32 = 0.14;

/// How far above its resting position the panel starts, in points. Small: the
/// list should look like it slid out from under the field, not flown in.
const POPUP_RISE: f32 = 6.0;

/// Motion for one row's entrance, and the delay between consecutive rows. The
/// stagger is what turns five simultaneous fades into a cascade; keep the total
/// (`ROW_ANIM + MAX_SEARCH_HITS * ROW_STAGGER`) well under a comfortable read so
/// the last row is settled by the time the eye reaches it.
const ROW_ANIM: f32 = 0.16;
const ROW_STAGGER: f32 = 0.028;

/// How far right of its resting position a row starts, in points.
const ROW_SLIDE: f32 = 10.0;

/// Motion for a row's hover/selection wash and its cover art fading up.
const ROW_HIGHLIGHT_ANIM: f32 = 0.11;
const COVER_FADE_ANIM: f32 = 0.22;

impl App {
    /// Recompute the suggestion list for the current query. Cheap enough to run
    /// on the search debounce: the digital side is a bounded SQL prefilter and
    /// the vinyl side scans the cached lists, with no network access.
    pub(crate) fn refresh_search_hits(&mut self) {
        let q = self.search_query.trim().to_string();
        if q.is_empty() {
            self.search_hits.clear();
            self.search_popup_open = false;
            self.search_cursor = None;
            return;
        }
        let hits = Catalog::open(&self.db_path)
            .and_then(|c| ordnung_core::search::search_library(&c, &q, MAX_SEARCH_HITS))
            .unwrap_or_default();
        // Restart the row cascade only when the list actually changed. A query
        // that narrows to the same five hits (or a debounce tick that lands on
        // an unchanged result) leaves the rows where they are — re-running the
        // entrance on every keystroke would read as flicker, not motion.
        if !same_hits(&self.search_hits, &hits) {
            self.search_row_shown_at = Some(std::time::Instant::now());
        }
        self.search_hits = hits;
        // Keep the highlight in range as the list shrinks under a longer query.
        if let Some(i) = self.search_cursor {
            if i >= self.search_hits.len() {
                self.search_cursor = None;
            }
        }
        // Open even with nothing to show: typing no longer empties the table, so
        // an unmatched query would otherwise give no feedback at all. The popup
        // says "No matches" instead of going quiet.
        self.search_popup_open = true;
    }

    /// Clear every active filter *and* the search box that may have produced
    /// one, then rebuild the rows.
    ///
    /// One place for it because "clear filters" is offered from four: the
    /// toolbar button, the macOS menu command, the empty-state escape hatch, and
    /// the table's own header UI. Leaving the search box populated while the
    /// filter it set goes away would be a lie about the current state.
    pub(crate) fn clear_all_filters(&mut self) {
        self.filter.clear();
        self.col_filters.clear();
        self.filter_apply_at = None;
        self.search_query.clear();
        self.search_apply_at = None;
        self.search_hits.clear();
        self.search_popup_open = false;
        self.search_cursor = None;
        self.reload();
    }

    /// Act on a chosen suggestion: go to where the thing actually lives.
    pub(crate) fn open_search_hit(&mut self, hit: SearchHit, ctx: &egui::Context) {
        self.search_popup_open = false;
        self.search_cursor = None;
        match hit {
            SearchHit::Track { id, .. } => self.reveal_track(id),
            SearchHit::Vinyl {
                list, instance_id, ..
            } => self.reveal_vinyl(list, instance_id, ctx),
        }
    }

    /// Show one catalog track in the Library, selected and scrolled into view.
    ///
    /// *This* is where the library gets filtered — typing alone never does.
    /// The query the user typed becomes the table filter, so the chosen track
    /// lands among its near matches rather than in the full catalog, and the
    /// toolbar's "Clear filters" button is the way back out.
    fn reveal_track(&mut self, id: Id) {
        self.view = LibraryView::Library;
        self.filter = self.search_query.trim().to_string();
        self.filter_apply_at = None;
        self.col_filters.clear();
        // Rebuild rows for the (possibly new) view first: `reload` prunes the
        // selection to live rows, so seed the selection after it.
        self.reload();
        self.selection = std::iter::once(id).collect();
        self.selected = Some(id);
        self.select_anchor = Some(id);
        self.scroll_to_track = Some(id);
        self.refresh_selected();
    }

    /// Show one record: switch to the Vinyl view and open its release sheet
    /// (tracklist, videos, price, and any digital copies on file).
    ///
    /// The view switch has to happen *and* reload before the sheet opens —
    /// `open_vinyl_sheet` resolves the record out of `self.vinyl` /
    /// `self.wantlist`, which are only populated while the Vinyl view is the
    /// active one (see `App::reload`).
    fn reveal_vinyl(&mut self, list: VinylList, instance_id: u64, ctx: &egui::Context) {
        self.view = LibraryView::Vinyl;
        // The record is addressed directly by key, so the grid behind the sheet
        // is left unfiltered — closing the sheet lands on the whole collection
        // rather than a one-record grid.
        self.filter.clear();
        self.filter_apply_at = None;
        self.col_filters.clear();
        self.search_hits.clear();
        self.reload();
        self.open_vinyl_sheet((list, instance_id), ctx);
    }
}

impl App {
    /// Draw the suggestion popup under the search field, if it's open.
    ///
    /// Keyboard first: ↑/↓ move the highlight, Enter opens it, Esc dismisses the
    /// popup while leaving the typed query (and the table filter it drives)
    /// alone. Keys are read only while the field has focus, so the arrows still
    /// belong to the track table the rest of the time.
    pub(crate) fn draw_search_popup(&mut self, field: &egui::Response) {
        let ctx = field.ctx.clone();
        // Whether the popup *wants* to be up. The panel keeps drawing past a
        // `false` here until its fade-out finishes, so dismissing is as smooth
        // as opening — an instant `return` would snap the list out of existence.
        let want_open = self.search_popup_open && !self.search_query.trim().is_empty();
        let open_t = ctx.animate_bool_with_time_and_easing(
            egui::Id::new("search_popup_open"),
            want_open,
            POPUP_ANIM,
            egui::emath::easing::cubic_out,
        );
        if open_t <= 0.0 {
            // Fully closed and settled: nothing to paint, and no leftover row
            // animations to keep alive for the next query.
            self.search_row_shown_at = None;
            return;
        }
        let focused = field.has_focus();
        // Arrow/Enter navigate whichever list is on screen.
        let n = if self.searching_discogs() {
            self.record_hit_count()
        } else {
            self.search_hits.len()
        };

        // Esc always dismisses, list or no list. Only the flag is flipped here:
        // this frame still paints the popup at its current `open_t`, and the
        // fade-out starts from there. Returning early instead would blank the
        // list for a frame before the animation ever got to run.
        let dismissing = want_open && focused && ctx.input(|i| i.key_pressed(egui::Key::Escape));
        if dismissing {
            self.search_popup_open = false;
            self.search_cursor = None;
            ctx.request_repaint();
        }
        // Arrow/Enter handling only applies when there's a list to move through;
        // with the popup showing "No matches" the keys still belong to the table.
        if want_open && !dismissing && focused && n > 0 {
            let (down, up, enter) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                )
            });
            if down {
                self.search_cursor = Some(match self.search_cursor {
                    Some(i) if i + 1 < n => i + 1,
                    Some(i) => i,
                    None => 0,
                });
            }
            if up {
                self.search_cursor = match self.search_cursor {
                    Some(0) | None => None,
                    Some(i) => Some(i - 1),
                };
            }
            // Enter opens the highlighted hit, or the top one when the user
            // hasn't moved the cursor — typing a name and pressing Enter should
            // go there, not sit inert. Typing no longer filters on its own, so
            // there's no other meaning left for the key.
            if enter {
                let i = self.search_cursor.unwrap_or(0);
                if self.searching_discogs() {
                    if let Some(hit) = self.record_hit_at(i) {
                        self.open_record_hit(hit, &ctx);
                        return;
                    }
                } else if let Some(h) = self.search_hits.get(i) {
                    let hit = h.hit.clone();
                    self.open_search_hit(hit, &ctx);
                    return;
                }
            }
        }

        let mut chosen: Option<SearchHit> = None;
        let mut record_chosen: Option<ordnung_core::discogs::RecordHit> = None;
        let discogs_mode = self.searching_discogs();
        let mut dismiss = false;
        // Cover loads discovered while drawing; issued after the Area closure so
        // no borrow of the caches is live when the loaders mutate them.
        let mut load_covers: Vec<SearchHit> = Vec::new();
        let cursor = self.search_cursor;
        // `want_open` is this frame's intent; Esc above may have just revoked it.
        let want_open = want_open && !dismissing;
        let hits = self.search_hits.clone();

        // When the current list of hits first went up, so each row can time its
        // own entrance off a shared origin. Re-stamped by `refresh_search_hits`
        // whenever the results actually change, which is what makes a new query
        // cascade in rather than swapping contents behind a static frame.
        let shown_at = *self
            .search_row_shown_at
            .get_or_insert_with(std::time::Instant::now);
        let since_shown = shown_at.elapsed().as_secs_f32();

        // The panel rises the last few points into place and fades as it goes;
        // reversing on close. Both are driven by the one `open_t` so the two
        // halves of the motion can never drift apart.
        let rise = POPUP_RISE * (1.0 - open_t);
        egui::Area::new(egui::Id::new("search_suggestions"))
            .order(egui::Order::Foreground)
            // Non-interactive while it is still materialising or on its way out,
            // so a click during the fade lands on whatever the user aimed at
            // rather than on a ghost row.
            .interactable(open_t > 0.99 && want_open)
            .fixed_pos(field.rect.left_bottom() + egui::vec2(0.0, 4.0 - rise))
            .show(&ctx, |ui| {
                ui.multiply_opacity(open_t);
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(space::S1, space::S2))
                    .rounding(egui::Rounding::same(radius::MD))
                    .show(ui, |ui| {
                    // The Discogs list is wider: it carries a third line of
                    // pressing detail and a membership pill that a library hit
                    // doesn't have.
                    let min_w = if discogs_mode { 460.0 } else { 320.0 };
                    ui.set_width(field.rect.width().max(min_w));
                    if discogs_mode {
                        // The remote list owns the whole popup in this mode —
                        // its own states (loading, empty, failed) are rendered
                        // by `draw_record_results`.
                        record_chosen = self.draw_record_results(ui);
                        return;
                    }
                    if hits.is_empty() {
                        ui.add_space(space::S2);
                        ui.horizontal(|ui| {
                            ui.add_space(space::S3);
                            ui.label(
                                egui::RichText::new("No matches in your library")
                                    .color(color::LABEL_3),
                            );
                        });
                        ui.add_space(space::S2);
                        return;
                    }
                    for (i, scored) in hits.iter().enumerate() {
                        let selected = cursor == Some(i);
                        // Each row trails the one above it, so the list unrolls
                        // top-down instead of five things appearing at once.
                        let enter = row_enter_t(since_shown, i);
                        // Resolve the row's artwork from whichever cache owns it,
                        // and note a miss so the loader is asked *after* the
                        // closure (it needs `&mut self`, borrowed here).
                        let (tex, wants_load) = match &scored.hit {
                            SearchHit::Track { id, has_cover, .. } => {
                                match self.cover_cache.get(id) {
                                    Some(ThumbState::Ready(Some(h))) => (Some(h.clone()), false),
                                    Some(_) => (None, false),
                                    None => (None, *has_cover),
                                }
                            }
                            SearchHit::Vinyl {
                                list,
                                instance_id,
                                has_cover,
                                ..
                            } => match self.search_vinyl_covers.get(&(*list, *instance_id)) {
                                Some(ThumbState::Ready(Some(h))) => (Some(h.clone()), false),
                                Some(_) => (None, false),
                                None => (None, *has_cover),
                            },
                        };
                        if wants_load {
                            load_covers.push(scored.hit.clone());
                        }
                        if search_hit_row(ui, &scored.hit, selected, tex, enter) {
                            chosen = Some(scored.hit.clone());
                        }
                    }
                });
            });

        for hit in load_covers {
            match hit {
                SearchHit::Track { id, .. } => self.request_thumb(id),
                SearchHit::Vinyl {
                    list, instance_id, ..
                } => self.request_search_vinyl_cover((list, instance_id)),
            }
        }

        // Keep frames coming while anything is still in motion: the panel's own
        // fade, and the row cascade that outlasts it. egui only repaints on
        // demand, so without this the animation would advance a frame at a time
        // as incidental events happened to land.
        if since_shown < ROW_ANIM + ROW_STAGGER * n as f32 {
            ctx.request_repaint();
        }

        // A click anywhere else closes the popup — the usual dismiss gesture,
        // and without it the list would hang around over the table.
        //
        // A click that landed *inside* the popup is not "anywhere else": the
        // membership buttons on a result deliberately leave the list up so you
        // can work down it, and dismissing here would close the popup out from
        // under the record you just wanted.
        let clicked_inside = ctx.input(|i| i.pointer.interact_pos()).is_some_and(|p| {
            ctx.memory(|m| m.area_rect(egui::Id::new("search_suggestions")))
                .is_some_and(|r| r.contains(p))
        });
        if want_open
            && ctx.input(|i| i.pointer.any_click())
            && !field.has_focus()
            && !clicked_inside
            && chosen.is_none()
            && record_chosen.is_none()
        {
            dismiss = true;
        }
        if let Some(hit) = record_chosen {
            self.open_record_hit(hit, &ctx);
        } else if let Some(hit) = chosen {
            self.open_search_hit(hit, &ctx);
        } else if dismiss {
            self.search_popup_open = false;
            self.search_cursor = None;
        }
    }
}

/// A stable identity for one hit, independent of the score that ranked it.
/// Two queries that surface the same record should not restage the list.
fn hit_key(h: &SearchHit) -> (u8, u64) {
    match h {
        SearchHit::Track { id, .. } => (0, *id),
        SearchHit::Vinyl {
            list, instance_id, ..
        } => (
            match list {
                VinylList::Collection => 1,
                VinylList::Wantlist => 2,
            },
            *instance_id,
        ),
    }
}

/// Whether two result lists name the same things in the same order.
fn same_hits(a: &[ScoredHit], b: &[ScoredHit]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| hit_key(&x.hit) == hit_key(&y.hit))
}

/// How far along its entrance row `i` is, `since` seconds after the list went
/// up. Each row waits out its share of the stagger, then eases in over
/// [`ROW_ANIM`]; the result is clamped, so a row that has finished simply
/// reports 1.0 forever after.
fn row_enter_t(since: f32, i: usize) -> f32 {
    let t = ((since - ROW_STAGGER * i as f32) / ROW_ANIM).clamp(0.0, 1.0);
    egui::emath::easing::cubic_out(t)
}

/// The artwork square at the head of each suggestion row.
const HIT_COVER_PX: f32 = 32.0;

/// One suggestion row. Returns true when clicked.
///
/// Each row leads with its cover art, falling back to a glyph naming which
/// library the hit came from — so "the file" and "the record" are never confused
/// at a glance, which is the whole point of searching both at once. The glyph
/// stays visible as a small badge over the artwork for the same reason.
///
/// `enter` is the row's entrance progress (0→1, see [`row_enter_t`]): the row
/// fades up and slides the last few points left into place, staggered behind the
/// rows above it. It scales *opacity and offset only* — the row always occupies
/// its full height from the first frame, so the popup never resizes mid-cascade
/// and the rows below don't shuffle underneath a moving cursor.
fn search_hit_row(
    ui: &mut egui::Ui,
    hit: &SearchHit,
    selected: bool,
    tex: Option<Tex>,
    enter: f32,
) -> bool {
    let (kind, primary, secondary) = match hit {
        SearchHit::Track {
            title,
            artist,
            album,
            ..
        } => {
            let sub = [artist.as_str(), album.as_str()]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            (HitKind::Track, title.clone(), sub)
        }
        SearchHit::Vinyl {
            list,
            title,
            artist,
            sub,
            matched_track,
            ..
        } => {
            // Name the song when the query matched the tracklist rather than the
            // release, so "which record is this on?" shows its own answer.
            let mut line = [artist.as_str(), sub.as_str()]
                .into_iter()
                .filter(|s| !s.trim().is_empty())
                .collect::<Vec<_>>()
                .join(" · ");
            if let Some(t) = matched_track {
                line = format!("{t} — {line}");
            }
            let kind = match list {
                VinylList::Collection => HitKind::Record,
                VinylList::Wantlist => HitKind::Wanted,
            };
            (kind, title.clone(), line)
        }
    };

    // Tall enough for the artwork plus breathing room, so the two text lines
    // sit centred against it rather than crowding the square. One S3 gutter
    // above and below keeps the popup on the same 8-pt rhythm as the toolbar.
    let height = HIT_COVER_PX + space::S3 * 2.0;
    let (slot, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    // Everything the row paints is offset by the remaining slide and dimmed by
    // the remaining fade. The allocated slot above is untouched, so layout is
    // stable while the contents move.
    let rect = slot.translate(egui::vec2(ROW_SLIDE * (1.0 - enter), 0.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect));
    let ui = &mut ui;
    ui.multiply_opacity(enter);

    // The hover/selection wash fades rather than switching on, and widens the
    // last couple of points into the row — the same "settling" motion as the
    // panel, at row scale.
    let hot = ui.ctx().animate_bool_with_time(
        resp.id.with("hot"),
        selected || resp.hovered(),
        ROW_HIGHLIGHT_ANIM,
    );
    if hot > 0.0 {
        let inset = 2.0 * (1.0 - hot);
        ui.painter().rect_filled(
            rect.shrink2(egui::vec2(inset, inset * 0.5)),
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
        egui::pos2(rect.left() + pad, rect.center().y - HIT_COVER_PX / 2.0),
        egui::vec2(HIT_COVER_PX, HIT_COVER_PX),
    );
    let rounding = egui::Rounding::same(radius::XS);
    // Covers arrive from a decode thread, whenever they arrive. Cross-fading the
    // artwork up over its placeholder keeps a late texture from punching a hole
    // in an otherwise settled list.
    let art_t = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("art"), tex.is_some(), COVER_FADE_ANIM);
    // The placeholder holds underneath for the whole cross-fade, so the square
    // is never briefly transparent between the two.
    if art_t < 1.0 {
        // No art (or still decoding): a muted square carrying the kind's
        // mark, so the row keeps its shape and still says where it's from.
        ui.painter().rect_filled(art, rounding, color::SURFACE_HI);
        draw_hit_mark(ui.painter(), art.center(), 9.0, kind, false);
    }
    if let Some(handle) = &tex {
        egui::Image::new(handle)
            .rounding(rounding)
            .tint(egui::Color32::WHITE.gamma_multiply(art_t))
            .paint_at(ui, art);
    }
    let painter = ui.painter();
    // With artwork shown, the mark moves to a badge in the corner so the library
    // a hit came from survives the cover taking its place. It fades in with the
    // artwork it sits on.
    if tex.is_some() && art_t > 0.0 {
        let badge = art.right_bottom() + egui::vec2(-7.0, -7.0);
        painter.circle_filled(
            badge,
            8.0,
            egui::Color32::from_black_alpha(180).gamma_multiply(art_t),
        );
        draw_hit_mark(painter, badge, 6.0, kind, true);
    }
    let text_x = art.right() + space::S4 - 2.0;
    // Both lines are truncated to the space actually left in the row.
    // `painter.text` neither wraps nor clips, so a long "artist · year · format
    // · label" ran straight out of the popup and over the table behind it.
    let avail = (rect.right() - pad - text_x).max(0.0);
    // Two lines stacked around the row's centre. A single-line hit (no artist
    // or album to show) centres on its own instead of floating high with an
    // empty gap beneath it.
    if secondary.is_empty() {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, rect.center().y),
            avail,
            primary,
            egui::TextStyle::Body,
            color::LABEL,
        );
    } else {
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, rect.center().y - 9.0),
            avail,
            primary,
            egui::TextStyle::Body,
            color::LABEL,
        );
        clipped_line(
            ui,
            painter,
            egui::pos2(text_x, rect.center().y + 9.0),
            avail,
            secondary,
            egui::TextStyle::Small,
            color::LABEL_3,
        );
    }
    resp.clicked()
}

impl App {
    /// Ask the vinyl-cover worker for one record's art on the popup's behalf.
    ///
    /// Mirrors `request_vinyl_cover` but lands in `search_vinyl_covers`, which
    /// survives the view-scoped eviction that clears `vinyl_covers` outside the
    /// Vinyl view. The worker is shared — it reads by key straight from the
    /// catalog — so this adds a cache, not a second loader thread.
    pub(crate) fn request_search_vinyl_cover(&mut self, key: VinylCoverKey) {
        if self.search_vinyl_covers.contains_key(&key) {
            return;
        }
        self.search_vinyl_covers.insert(key, ThumbState::Loading);
        let _ = self.search_cover_req_tx.send(key);
    }

    /// Drain finished popup cover decodes, uploading each to a texture. Called
    /// once per frame alongside the other cover polls.
    pub(crate) fn poll_search_covers(&mut self, ctx: &egui::Context) {
        while let Ok(msg) = self.search_cover_rx.try_recv() {
            let (list, id) = msg.key;
            let tex = msg.image.map(|img| {
                let name = match list {
                    VinylList::Collection => format!("search-vinyl-{id}"),
                    VinylList::Wantlist => format!("search-vinyl-want-{id}"),
                };
                self.tex_graveyard
                    .wrap(ctx.load_texture(name, img, egui::TextureOptions::LINEAR))
            });
            self.search_vinyl_covers.insert(msg.key, ThumbState::Ready(tex));
        }
    }
}

/// Paint one line of text left-aligned at `pos`, ellipsized to `width`.
///
/// egui's `Painter::text` neither wraps nor clips, so anything too long simply
/// overflows its container. Laying the galley out with an explicit wrap width
/// and `truncate` gives the usual single-line "…" instead.
#[allow(clippy::too_many_arguments)]
pub(crate) fn clipped_line(
    ui: &egui::Ui,
    painter: &egui::Painter,
    pos: egui::Pos2,
    width: f32,
    text: String,
    style: egui::TextStyle,
    color: egui::Color32,
) {
    let galley = ui.fonts(|f| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            text,
            style.resolve(ui.style()),
            color,
        );
        job.wrap.max_width = width;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = false;
        f.layout_job(job)
    });
    // `LEFT_CENTER` by hand: the galley is positioned from its top-left.
    painter.galley(
        egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// Which library a suggestion came from, and therefore which mark it wears.
#[derive(Clone, Copy, PartialEq, Eq)]
enum HitKind {
    /// A file in the digital catalog.
    Track,
    /// A record in the Discogs collection.
    Record,
    /// A record on the Discogs wantlist — one you don't own yet.
    Wanted,
}

/// Paint the small mark that says where a hit came from, centred on `c` with
/// radius `r`.
///
/// The vinyl marks are drawn rather than set in type: no glyph in a UI font
/// reads as a *record*, and "◉" landed somewhere between a bullseye and a radio
/// button. A dark disc with a light centre label and a spindle hole is
/// unmistakable even at badge size, and the wantlist variant is the same disc
/// left as an outline — "the shape of a record you don't have yet".
///
/// `on_art` brightens the mark for the badge that sits over cover artwork, where
/// it needs to hold up against an arbitrary image rather than a flat surface.
fn draw_hit_mark(
    painter: &egui::Painter,
    c: egui::Pos2,
    r: f32,
    kind: HitKind,
    on_art: bool,
) {
    let ink = if on_art {
        egui::Color32::from_white_alpha(235)
    } else {
        color::LABEL_3
    };
    match kind {
        HitKind::Track => {
            // A minim: filled notehead with a stem, which reads as "audio file"
            // at any size a glyph would.
            let head = egui::pos2(c.x - r * 0.25, c.y + r * 0.45);
            painter.circle_filled(head, r * 0.42, ink);
            painter.line_segment(
                [
                    egui::pos2(head.x + r * 0.38, head.y),
                    egui::pos2(head.x + r * 0.38, c.y - r * 0.75),
                ],
                egui::Stroke::new((r * 0.18).max(1.0), ink),
            );
        }
        HitKind::Record | HitKind::Wanted => {
            let owned = kind == HitKind::Record;
            if owned {
                painter.circle_filled(c, r, ink.gamma_multiply(0.85));
            } else {
                // Wanted: an outline of the same disc, so owning and wanting
                // read as the same object in two states rather than two symbols.
                painter.circle_stroke(c, r, egui::Stroke::new((r * 0.2).max(1.0), ink));
            }
            // Centre label, then the spindle hole punched through it — the two
            // details that make a plain circle read as a record.
            let label_r = r * 0.42;
            let label = if on_art {
                egui::Color32::from_black_alpha(190)
            } else {
                color::SURFACE
            };
            if owned {
                painter.circle_filled(c, label_r, label);
            } else {
                painter.circle_stroke(c, label_r, egui::Stroke::new((r * 0.14).max(0.8), ink));
            }
            painter.circle_filled(c, (r * 0.1).max(0.7), ink);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_enter_in_order_and_settle() {
        // At t=0 nothing has moved yet.
        assert_eq!(row_enter_t(0.0, 0), 0.0);
        assert_eq!(row_enter_t(0.0, 3), 0.0);
        // Mid-cascade, earlier rows are always further along than later ones.
        let mid = ROW_STAGGER * 2.0 + ROW_ANIM * 0.5;
        for i in 0..MAX_SEARCH_HITS - 1 {
            assert!(
                row_enter_t(mid, i) >= row_enter_t(mid, i + 1),
                "row {i} should lead row {}",
                i + 1
            );
        }
        // Every row is settled once the whole cascade has had time to run.
        let done = ROW_ANIM + ROW_STAGGER * MAX_SEARCH_HITS as f32;
        for i in 0..MAX_SEARCH_HITS {
            assert_eq!(row_enter_t(done, i), 1.0, "row {i} should be settled");
        }
    }

    fn track(id: Id) -> ScoredHit {
        ScoredHit {
            hit: SearchHit::Track {
                id,
                title: String::new(),
                artist: String::new(),
                album: String::new(),
                has_cover: false,
            },
            score: 0,
        }
    }

    #[test]
    fn same_hits_ignores_score_but_not_identity() {
        // Identity, not rank, decides whether the list restages: re-scoring the
        // same hits under a longer query must not replay the cascade.
        let mut rescored = track(1);
        rescored.score = 99;
        assert!(same_hits(&[track(1), track(2)], &[rescored, track(2)]));
        // Different members, different order, and different lengths all count
        // as a new list.
        assert!(!same_hits(&[track(1)], &[track(2)]));
        assert!(!same_hits(&[track(1), track(2)], &[track(2), track(1)]));
        assert!(!same_hits(&[track(1)], &[track(1), track(2)]));
    }
}
