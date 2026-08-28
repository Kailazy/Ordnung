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

impl App {
    /// Recompute the suggestion list for the current query. Cheap enough to run
    /// on the search debounce: the digital side is a bounded SQL prefilter and
    /// the vinyl side scans the cached lists, with no network access.
    pub(crate) fn refresh_search_hits(&mut self) {
        let q = self.filter.trim().to_string();
        if q.is_empty() {
            self.search_hits.clear();
            self.search_popup_open = false;
            self.search_cursor = None;
            return;
        }
        self.search_hits = Catalog::open(&self.db_path)
            .and_then(|c| ordnung_core::search::search_library(&c, &q, MAX_SEARCH_HITS))
            .unwrap_or_default();
        // Keep the highlight in range as the list shrinks under a longer query.
        if let Some(i) = self.search_cursor {
            if i >= self.search_hits.len() {
                self.search_cursor = None;
            }
        }
        self.search_popup_open = !self.search_hits.is_empty();
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
    /// The search text is left in place: it's what the user typed to get here,
    /// and clearing it would yank the surrounding rows out from under the hit
    /// they just picked. The table is already filtered by it, so the selected
    /// row is visible among its matches.
    fn reveal_track(&mut self, id: Id) {
        self.view = LibraryView::Library;
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
        // The grid filters on the same search text as the table. Clearing it
        // here keeps the record visible behind its own sheet rather than
        // leaving the user on an empty-looking grid when they close it.
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
        if !self.search_popup_open || self.search_hits.is_empty() {
            return;
        }
        let ctx = field.ctx.clone();
        let focused = field.has_focus();
        let n = self.search_hits.len();

        if focused {
            let (down, up, enter, esc) = ctx.input_mut(|i| {
                (
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown),
                    i.consume_key(egui::Modifiers::NONE, egui::Key::ArrowUp),
                    i.key_pressed(egui::Key::Enter),
                    i.key_pressed(egui::Key::Escape),
                )
            });
            if esc {
                self.search_popup_open = false;
                self.search_cursor = None;
                return;
            }
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
            // Enter only commits when something is highlighted; otherwise it
            // means "just filter", which is what the box already did.
            if enter {
                if let Some(i) = self.search_cursor {
                    if let Some(h) = self.search_hits.get(i) {
                        let hit = h.hit.clone();
                        self.open_search_hit(hit, &ctx);
                        return;
                    }
                }
            }
        }

        let mut chosen: Option<SearchHit> = None;
        let mut dismiss = false;
        // Cover loads discovered while drawing; issued after the Area closure so
        // no borrow of the caches is live when the loaders mutate them.
        let mut load_covers: Vec<SearchHit> = Vec::new();
        let cursor = self.search_cursor;
        let hits = self.search_hits.clone();

        egui::Area::new(egui::Id::new("search_suggestions"))
            .order(egui::Order::Foreground)
            .fixed_pos(field.rect.left_bottom() + egui::vec2(0.0, 4.0))
            .show(&ctx, |ui| {
                egui::Frame::popup(ui.style())
                    .inner_margin(egui::Margin::symmetric(space::S1, space::S2))
                    .rounding(egui::Rounding::same(radius::MD))
                    .show(ui, |ui| {
                    ui.set_width(field.rect.width().max(320.0));
                    for (i, scored) in hits.iter().enumerate() {
                        let selected = cursor == Some(i);
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
                        if search_hit_row(ui, &scored.hit, selected, tex) {
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

        // A click anywhere else closes the popup — the usual dismiss gesture,
        // and without it the list would hang around over the table.
        if ctx.input(|i| i.pointer.any_click()) && !field.has_focus() && chosen.is_none() {
            dismiss = true;
        }
        if let Some(hit) = chosen {
            self.open_search_hit(hit, &ctx);
        } else if dismiss {
            self.search_popup_open = false;
            self.search_cursor = None;
        }
    }
}

/// The artwork square at the head of each suggestion row.
const HIT_COVER_PX: f32 = 32.0;

/// One suggestion row. Returns true when clicked.
///
/// Each row leads with its cover art, falling back to a glyph naming which
/// library the hit came from — so "the file" and "the record" are never confused
/// at a glance, which is the whole point of searching both at once. The glyph
/// stays visible as a small badge over the artwork for the same reason.
fn search_hit_row(
    ui: &mut egui::Ui,
    hit: &SearchHit,
    selected: bool,
    tex: Option<Tex>,
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
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(ui.available_width(), height),
        egui::Sense::click(),
    );
    if selected || resp.hovered() {
        ui.painter().rect_filled(
            rect,
            ui.visuals().widgets.hovered.rounding,
            ui.visuals().widgets.hovered.weak_bg_fill,
        );
    }
    let pad = space::S3;
    let art = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.center().y - HIT_COVER_PX / 2.0),
        egui::vec2(HIT_COVER_PX, HIT_COVER_PX),
    );
    let rounding = egui::Rounding::same(radius::XS);
    match &tex {
        Some(handle) => {
            egui::Image::new(handle)
                .rounding(rounding)
                .paint_at(ui, art);
        }
        None => {
            // No art (or still decoding): a muted square carrying the kind's
            // mark, so the row keeps its shape and still says where it's from.
            ui.painter().rect_filled(art, rounding, color::SURFACE_HI);
            draw_hit_mark(ui.painter(), art.center(), 9.0, kind, false);
        }
    }
    let painter = ui.painter();
    // With artwork shown, the mark moves to a badge in the corner so the library
    // a hit came from survives the cover taking its place.
    if tex.is_some() {
        let badge = art.right_bottom() + egui::vec2(-7.0, -7.0);
        painter.circle_filled(badge, 8.0, egui::Color32::from_black_alpha(180));
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
fn clipped_line(
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
