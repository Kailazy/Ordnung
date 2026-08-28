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
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    ui.set_width(field.rect.width().max(300.0));
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
    let (glyph, primary, secondary) = match hit {
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
            ("♪", title.clone(), sub)
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
            let glyph = match list {
                VinylList::Collection => "◉",
                VinylList::Wantlist => "☆",
            };
            (glyph, title.clone(), line)
        }
    };

    // Tall enough for the artwork plus breathing room, so the two text lines
    // sit centred against it rather than crowding the square.
    let height = HIT_COVER_PX + 12.0;
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
    let pad = ui.spacing().button_padding.x + 2.0;
    let art = egui::Rect::from_min_size(
        egui::pos2(rect.left() + pad, rect.center().y - HIT_COVER_PX / 2.0),
        egui::vec2(HIT_COVER_PX, HIT_COVER_PX),
    );
    let rounding = egui::Rounding::same(3.0);
    match &tex {
        Some(handle) => {
            egui::Image::new(handle)
                .rounding(rounding)
                .paint_at(ui, art);
        }
        None => {
            // No art (or still decoding): a muted square carrying the source
            // glyph, so the row keeps its shape and still says where it's from.
            ui.painter()
                .rect_filled(art, rounding, egui::Color32::from_gray(40));
            ui.painter().text(
                art.center(),
                egui::Align2::CENTER_CENTER,
                glyph,
                egui::FontId::proportional(14.0),
                ui.visuals().weak_text_color(),
            );
        }
    }
    let painter = ui.painter();
    // With artwork shown, the glyph moves to a small badge in the corner so the
    // library a hit came from survives the cover taking its place.
    if tex.is_some() {
        let badge = art.right_bottom() + egui::vec2(-6.0, -6.0);
        // A dark disc behind it: the glyph would otherwise vanish into a pale
        // sleeve, and plenty of covers are near-white.
        painter.circle_filled(badge, 7.0, egui::Color32::from_black_alpha(170));
        painter.text(
            badge,
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::FontId::proportional(10.0),
            egui::Color32::from_white_alpha(235),
        );
    }
    let text_x = art.right() + 10.0;
    painter.text(
        egui::pos2(text_x, rect.center().y - 8.0),
        egui::Align2::LEFT_CENTER,
        primary,
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().strong_text_color(),
    );
    if !secondary.is_empty() {
        painter.text(
            egui::pos2(text_x, rect.center().y + 8.0),
            egui::Align2::LEFT_CENTER,
            secondary,
            egui::TextStyle::Small.resolve(ui.style()),
            ui.visuals().weak_text_color(),
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
