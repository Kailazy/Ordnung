//! The Sellers tab of the vinyl view: dig through a saved Discogs seller's
//! crates the way you'd flip through the bins in their shop.
//!
//! Sellers are saved by username (Discogs removed the release→sellers
//! direction, so the shop is the only way in — see
//! `docs/design/bulk-sellers-spike.md`). An explicit Sweep pages the shop's
//! for-sale inventory into the `seller_listings` cache; from then on the
//! crates browse offline — filtered, price/condition on every card, records
//! you already own or want badged from the same membership sets the shelves
//! use. A card opens the ordinary record sheet, so listening (YouTube via the
//! mini-player), wantlisting and digging all come along for free; buying
//! happens on discogs.com, one click from the card or the sheet.

use super::*;
use crate::vinyl_sheet::SellerOffer;

/// What a card click or context-menu pick asked for, applied after the grid
/// releases its borrows.
enum SellerAct {
    /// Open the record sheet, carrying this seller's concrete offer.
    Open(usize),
    /// Open the listing itself on discogs.com — where the purchase happens.
    Buy(usize),
    /// Add the listing's release to the Discogs wantlist.
    Want(usize),
}

/// A seller username, out of either a bare name or a pasted discogs.com URL
/// (`…/seller/NAME/profile`, `…/user/NAME`). `None` when nothing usable is in
/// the box.
fn parse_seller_input(input: &str) -> Option<String> {
    let s = input.trim().trim_end_matches('/');
    if s.is_empty() {
        return None;
    }
    for marker in ["/seller/", "/user/"] {
        if let Some(i) = s.find(marker) {
            let tail = &s[i + marker.len()..];
            let name = tail.split(['/', '?', '#']).next().unwrap_or("");
            return (!name.is_empty()).then(|| name.to_string());
        }
    }
    // A bare username: Discogs allows letters, digits, dot, underscore and
    // hyphen. Anything else in the box is a typo or an unrelated URL.
    let ok = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    (ok && !s.contains("discogs")).then(|| s.to_string())
}

/// `2h ago` / `3d ago` for the sweep stamp.
fn fmt_ago(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(unix);
    let secs = (now - unix).max(0);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => format!("{}m ago", secs / 60),
        3600..=86_399 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

/// The short grade out of Discogs's long form: `Very Good Plus (VG+)` → `VG+`.
/// Falls back to the full text when there are no parentheses (e.g. `Generic`).
pub(crate) fn cond_short(s: &str) -> &str {
    match (s.rfind('('), s.rfind(')')) {
        (Some(a), Some(b)) if a + 1 < b => &s[a + 1..b],
        _ => s,
    }
}

/// Does a listing match the crates search? Same contract as
/// [`vinyl_matches`](crate::views::vinyl_matches): every whitespace-separated
/// term must appear somewhere in the folded haystack.
fn listing_matches(hay: &str, query: &str) -> bool {
    query.split_whitespace().all(|term| hay.contains(term))
}

/// The search haystack for one listing, built once per load rather than per
/// frame — a swept mega-shop is tens of thousands of rows, and re-formatting
/// them every keystroke is what this avoids.
fn listing_hay(l: &SellerListing) -> String {
    format!(
        "{} {} {} {} {} {}",
        l.artist,
        l.title,
        l.label.as_deref().unwrap_or(""),
        l.year.map(|y| y.to_string()).unwrap_or_default(),
        l.format.as_deref().unwrap_or(""),
        l.condition.as_deref().unwrap_or(""),
    )
    .to_lowercase()
}

impl App {
    /// Make sure the current seller's cached listings are in memory. Lazy
    /// (rather than part of `reload`, which runs on every search keystroke)
    /// because a swept shop can be tens of thousands of rows; a finished job
    /// clears `seller_listings_for` so a fresh sweep shows up next frame.
    fn ensure_seller_listings(&mut self) {
        let Some(cur) = self.seller_current.clone() else {
            self.seller_listings = Vec::new();
            self.seller_hay = Vec::new();
            self.seller_genres = HashMap::new();
            self.seller_listings_for = None;
            return;
        };
        if self.seller_listings_for.as_deref() == Some(cur.as_str()) {
            return;
        }
        self.seller_listings = Catalog::open(&self.db_path)
            .and_then(|c| c.list_seller_listings(&cur))
            .unwrap_or_default();
        self.seller_hay = self.seller_listings.iter().map(listing_hay).collect();
        // Genre tags for the crates. The inventory endpoint carries none, so
        // mine the release-detail cache, then the user's own shelves for
        // anything not fetched yet: a listed record you own or want is tagged
        // by your own sync. Listings in neither stay unknown and drop out of a
        // genre filter.
        let mut ids: Vec<u64> = self.seller_listings.iter().map(|l| l.release_id).collect();
        ids.sort_unstable();
        ids.dedup();
        self.seller_genres = Catalog::open(&self.db_path)
            .and_then(|c| c.release_genres(&ids))
            .unwrap_or_default();
        for r in self.vinyl.iter().chain(self.wantlist.iter()) {
            if !r.genres.is_empty() {
                self.seller_genres
                    .entry(r.release_id)
                    .or_insert_with(|| r.genres.clone());
            }
        }
        self.seller_listings_for = Some(cur);
    }

    /// The Sellers tab body: shop picker + sweep controls, then the crates as
    /// a virtualized card grid. `query` is the shared vinyl search, already
    /// trimmed and lowercased.
    pub(crate) fn draw_sellers(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, query: &str) {
        let busy = self.is_busy();
        self.ensure_seller_listings();

        // --- Shop row: saved sellers as chips, plus the add box. -------------
        let mut switch_to: Option<String> = None;
        let mut remove: Option<String> = None;
        let mut sweep: Option<String> = None;
        let mut add_clicked = false;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            for shop in &self.sellers {
                let active = self.seller_current.as_deref() == Some(shop.username.as_str());
                let chip = ui.selectable_label(active, &shop.username);
                let chip = chip.on_hover_note("Browse this seller's crates");
                if chip.clicked() {
                    switch_to = Some(shop.username.clone());
                }
                chip.context_menu(|ui| {
                    if ui.button("↗ Open shop on Discogs").clicked() {
                        open_url(&format!(
                            "https://www.discogs.com/seller/{}/profile",
                            shop.username
                        ));
                        ui.close_menu();
                    }
                    if ui.button("✖ Remove seller").clicked() {
                        remove = Some(shop.username.clone());
                        ui.close_menu();
                    }
                });
            }
            if !self.sellers.is_empty() {
                ui.add_space(6.0);
            }
            let add = ui
                .add(
                    egui::TextEdit::singleline(&mut self.seller_add)
                        .desired_width(180.0)
                        .hint_text("Seller username or URL"),
                )
                .on_hover_note("Save a Discogs seller to dig through, by username or shop URL");
            let submitted =
                add.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui
                .small_button("＋ Add")
                .on_hover_note("Save this seller")
                .clicked()
                || submitted
            {
                add_clicked = true;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(cur) = self.seller_current.clone() {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui
                            .button("⟲ Sweep")
                            .on_hover_note(
                                "Pull this seller's for-sale inventory from Discogs \
                                 (runs in the background; large shops take minutes)",
                            )
                            .clicked()
                        {
                            sweep = Some(cur.clone());
                        }
                    });
                    if ui
                        .button("↗ Open shop")
                        .on_hover_note("Open this seller's shop on discogs.com")
                        .clicked()
                    {
                        open_url(&format!("https://www.discogs.com/seller/{cur}/profile"));
                    }
                }
            });
        });

        // Apply the shop-row asks now that `self.sellers` is free again.
        if let Some(u) = switch_to {
            self.seller_current = Some(u);
            self.ensure_seller_listings();
        }
        if add_clicked {
            match parse_seller_input(&self.seller_add) {
                Some(name) => {
                    let saved = Catalog::open(&self.db_path).and_then(|c| {
                        c.add_seller(&name)?;
                        c.list_sellers()
                    });
                    match saved {
                        Ok(sellers) => {
                            self.sellers = sellers;
                            self.seller_current = Some(name);
                            self.seller_add.clear();
                            self.ensure_seller_listings();
                        }
                        Err(e) => self.status = format!("Couldn't save seller: {e}"),
                    }
                }
                None => {
                    self.status =
                        "That doesn't look like a Discogs username or seller URL.".into();
                }
            }
        }
        if let Some(name) = remove {
            match Catalog::open(&self.db_path).and_then(|c| {
                c.remove_seller(&name)?;
                c.list_sellers()
            }) {
                Ok(sellers) => {
                    self.sellers = sellers;
                    if self.seller_current.as_deref() == Some(name.as_str()) {
                        self.seller_current = self.sellers.first().map(|s| s.username.clone());
                        self.seller_listings_for = None;
                        self.ensure_seller_listings();
                    }
                }
                Err(e) => self.status = format!("Couldn't remove seller: {e}"),
            }
        }
        if let Some(u) = sweep {
            self.spawn_sweep_seller(ctx.clone(), u);
        }

        // --- Empty states. ----------------------------------------------------
        if self.sellers.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("Dig through a seller's crates");
                ui.add_space(6.0);
                ui.label("Save a Discogs seller above, then sweep their inventory.");
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Every for-sale record lands here with price and condition, \
                         cross-referenced against your collection and wantlist — \
                         so you can flip through the bins without the browser.",
                    )
                    .weak(),
                );
            });
            return;
        }
        let Some(shop) = self
            .seller_current
            .as_deref()
            .and_then(|cur| self.sellers.iter().find(|s| s.username == cur))
            .cloned()
        else {
            return;
        };

        // --- Meta line: how fresh these crates are. ---------------------------
        // Filter up front so the count in the meta line matches the grid. The
        // genre filter reads `seller_genres`; a listing with no known tags
        // can't match a tag, so it drops out (the genre menu says how many
        // records have known tags at all).
        let genres_sel = self.vinyl_genres.clone();
        let unfiltered = query.is_empty() && genres_sel.is_empty();
        let filtered: Vec<usize> = self
            .seller_listings
            .iter()
            .enumerate()
            .filter(|(i, l)| {
                (query.is_empty() || listing_matches(&self.seller_hay[*i], query))
                    && (genres_sel.is_empty()
                        || self.seller_genres.get(&l.release_id).is_some_and(|tags| {
                            tags.iter().any(|t| {
                                genres_sel.iter().any(|g| t.eq_ignore_ascii_case(g))
                            })
                        }))
            })
            .map(|(i, _)| i)
            .collect();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut meta = match (unfiltered, shop.cached) {
                (true, n) => format!("{n} records in the crates"),
                (false, _) => format!("{} of {} records match", filtered.len(), shop.cached),
            };
            match shop.swept_at {
                Some(t) => meta.push_str(&format!(" · swept {}", fmt_ago(t))),
                None => meta.push_str(" · never swept"),
            }
            ui.label(egui::RichText::new(meta).weak());
        });
        ui.add_space(4.0);

        if self.seller_listings.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                if shop.swept_at.is_none() {
                    ui.label(format!(
                        "Nothing cached for {} yet — sweep to pull their crates.",
                        shop.username
                    ));
                } else {
                    ui.label("No vinyl in this seller's inventory.");
                }
            });
            return;
        }
        if filtered.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                // "Techno, House or IDM" — the OR the filter actually applies.
                let tags_named = match genres_sel.as_slice() {
                    [] => String::new(),
                    [one] => one.clone(),
                    [head @ .., last] => format!("{} or {last}", head.join(", ")),
                };
                let msg = match (genres_sel.is_empty(), query.is_empty()) {
                    (false, true) => format!("Nothing in the crates is tagged {tags_named}."),
                    (false, false) => format!(
                        "Nothing tagged {tags_named} in the crates matches that search."
                    ),
                    (true, _) => "Nothing in the crates matches that search.".to_string(),
                };
                ui.label(egui::RichText::new(msg).weak());
            });
            return;
        }

        // --- The crates: a virtualized cover grid. ----------------------------
        // Same sizing rules as the vinyl wall, but drawn through `show_rows` —
        // a swept shop can be tens of thousands of cards, and only the visible
        // rows should cost layout (or a cover download).
        const GAP: f32 = 14.0;
        const MIN_COVER: f32 = 132.0;
        const MAX_COVER: f32 = 170.0;
        /// Caption budget under each cover: artist, title, price line.
        const CAPTION_H: f32 = 58.0;
        let avail = ui.available_width();
        let cols = (((avail + GAP) / (MIN_COVER + GAP)).floor().max(1.0)) as usize;
        let cover_side = ((avail - GAP * (cols as f32 - 1.0)) / cols as f32)
            .floor()
            .clamp(MIN_COVER.min(avail.max(1.0)), MAX_COVER);
        let row_h = cover_side + CAPTION_H + GAP;
        let n_rows = filtered.len().div_ceil(cols);

        let mut act: Option<SellerAct> = None;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, row_h, n_rows, |ui, rows| {
                ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                for row in rows {
                    ui.horizontal_top(|ui| {
                        for slot in 0..cols {
                            let Some(&idx) = filtered.get(row * cols + slot) else {
                                break;
                            };
                            if let Some(a) = self.seller_card(ui, idx, cover_side) {
                                act = Some(a);
                            }
                        }
                    });
                }
                ui.add_space(8.0);
            });

        match act {
            Some(SellerAct::Open(idx)) => self.open_seller_listing(idx, ctx),
            Some(SellerAct::Buy(idx)) => {
                if let Some(l) = self.seller_listings.get(idx) {
                    let url = l.uri.clone().unwrap_or_else(|| {
                        format!("https://www.discogs.com/release/{}", l.release_id)
                    });
                    open_url(&url);
                }
            }
            Some(SellerAct::Want(idx)) => {
                if let Some(l) = self.seller_listings.get(idx) {
                    let edit = VinylEdit::Want {
                        release_ids: vec![l.release_id],
                        label: format!("{} — {}", l.artist, l.title),
                    };
                    self.request_vinyl_edit(ctx.clone(), edit);
                }
            }
            None => {}
        }
    }

    /// One card of the crates: cover, credit, and the sale terms — price and
    /// grade are what separate flipping through a shop from browsing a
    /// discography, so they get the caption's third line. Returns what the
    /// user asked of it, if anything.
    fn seller_card(&mut self, ui: &mut egui::Ui, idx: usize, cover_side: f32) -> Option<SellerAct> {
        use crate::ui::tokens::color;

        // Snapshot the card's strings before `dig_cover` needs `self` mutably.
        let (listing_id, release_id, artist, title, sub, price_line, thumb, owned, wanted) = {
            let l = self.seller_listings.get(idx)?;
            let sub = match (l.year, l.format.as_deref()) {
                (Some(y), Some(f)) => format!("{y} · {f}"),
                (Some(y), None) => y.to_string(),
                (None, Some(f)) => f.to_string(),
                (None, None) => String::new(),
            };
            let mut price_line = crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                value: l.price,
                currency: l.currency.clone(),
            });
            if let Some(c) = l.condition.as_deref() {
                price_line.push_str(&format!(" · {}", cond_short(c)));
                if let Some(s) = l.sleeve_condition.as_deref() {
                    price_line.push_str(&format!("/{}", cond_short(s)));
                }
            }
            (
                l.listing_id,
                l.release_id,
                l.artist.clone(),
                l.title.clone(),
                sub,
                price_line,
                l.thumb_url.clone(),
                self.vinyl_owned.contains(&l.release_id),
                self.vinyl_wanted.contains(&l.release_id),
            )
        };
        let tex = thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());

        let mut act: Option<SellerAct> = None;
        ui.allocate_ui_with_layout(
            egui::vec2(cover_side, cover_side + 58.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let (rect, resp) = ui.allocate_exact_size(
                    egui::vec2(cover_side, cover_side),
                    egui::Sense::click(),
                );
                let resp = resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_note("Open the record — listen, wantlist, or buy");
                match &tex {
                    Some(h) => {
                        egui::Image::new(h)
                            .fit_to_exact_size(egui::vec2(cover_side, cover_side))
                            .rounding(egui::Rounding::same(6.0))
                            .paint_at(ui, rect);
                    }
                    None => {
                        ui.painter().rect_filled(
                            rect,
                            egui::Rounding::same(6.0),
                            egui::Color32::from_gray(34),
                        );
                        ui.painter().text(
                            rect.center(),
                            egui::Align2::CENTER_CENTER,
                            "♪",
                            egui::FontId::proportional(28.0),
                            egui::Color32::from_gray(70),
                        );
                    }
                }
                if resp.hovered() {
                    ui.painter().rect_stroke(
                        rect,
                        egui::Rounding::same(6.0),
                        egui::Stroke::new(2.0, color::ACCENT),
                    );
                }
                // Membership chip: a record already on a shelf is the one
                // thing worth knowing before the price — you either have it
                // or you're already hunting it.
                if owned || wanted {
                    let text = if owned { "OWNED" } else { "WANT" };
                    let font = egui::FontId::proportional(10.0);
                    let galley =
                        ui.painter()
                            .layout_no_wrap(text.into(), font, egui::Color32::WHITE);
                    let pad = egui::vec2(5.0, 3.0);
                    let chip = egui::Rect::from_min_size(
                        rect.min + egui::vec2(5.0, 5.0),
                        galley.size() + pad * 2.0,
                    );
                    let fill = if owned {
                        egui::Color32::from_rgb(40, 120, 70)
                    } else {
                        egui::Color32::from_rgb(120, 90, 30)
                    };
                    ui.painter()
                        .rect_filled(chip, egui::Rounding::same(4.0), fill);
                    ui.painter().galley(chip.min + pad, galley, egui::Color32::WHITE);
                }
                if resp.clicked() {
                    act = Some(SellerAct::Open(idx));
                }
                resp.context_menu(|ui| {
                    if ui.button("🛒 Buy on Discogs ↗").clicked() {
                        act = Some(SellerAct::Buy(idx));
                        ui.close_menu();
                    }
                    let already = owned || wanted;
                    if ui
                        .add_enabled(!already, egui::Button::new("＋ Add to wantlist"))
                        .clicked()
                    {
                        act = Some(SellerAct::Want(idx));
                        ui.close_menu();
                    }
                    if ui.button("↗ Open release page").clicked() {
                        open_url(&format!("https://www.discogs.com/release/{release_id}"));
                        ui.close_menu();
                    }
                });

                // Caption: artist (strong), title, then the sale terms.
                let clip = |s: &str| s.to_string();
                ui.add_space(4.0);
                ui.scope(|ui| {
                    ui.spacing_mut().item_spacing.y = 1.0;
                    ui.set_max_width(cover_side);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(clip(&artist)).small().strong(),
                        )
                        .truncate(),
                    );
                    let title_text = if sub.is_empty() {
                        title.clone()
                    } else {
                        format!("{title} · {sub}")
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(title_text).small().weak())
                            .truncate(),
                    );
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&price_line)
                                .small()
                                .color(egui::Color32::from_rgb(120, 200, 140)),
                        )
                        .truncate(),
                    );
                });
                // Keep the ui id stable per listing so egui state (context
                // menu) doesn't jump between cards as the grid scrolls.
                let _ = listing_id;
            },
        );
        act
    }

    /// Open the record sheet for a seller listing, carrying the seller's
    /// concrete offer (price, grading, listing link) so the sheet shows what
    /// *this copy* costs alongside the market floor.
    fn open_seller_listing(&mut self, idx: usize, ctx: &egui::Context) {
        let Some(l) = self.seller_listings.get(idx).cloned() else {
            return;
        };
        let Some(seller) = self.seller_current.clone() else {
            return;
        };
        let sub = [
            l.year.map(|y| y.to_string()),
            l.format.clone(),
            match (&l.label, &l.catalog_number) {
                (Some(a), Some(c)) => Some(format!("{a} {c}")),
                (Some(a), None) => Some(a.clone()),
                (None, Some(c)) => Some(c.clone()),
                (None, None) => None,
            },
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        self.open_release_sheet(
            l.release_id,
            l.artist.clone(),
            l.title.clone(),
            sub,
            l.thumb_url.clone(),
            ctx,
        );
        if let Some(sheet) = self.vinyl_sheet.as_mut() {
            if sheet.release_id == l.release_id {
                sheet.offer = Some(SellerOffer {
                    seller,
                    price: discogs::MarketPrice {
                        value: l.price,
                        currency: l.currency.clone(),
                    },
                    condition: l.condition.clone(),
                    sleeve_condition: l.sleeve_condition.clone(),
                    uri: l.uri.clone(),
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seller_input_parses_names_and_urls() {
        assert_eq!(parse_seller_input(" hardwax "), Some("hardwax".into()));
        assert_eq!(
            parse_seller_input("https://www.discogs.com/seller/juno_records/profile"),
            Some("juno_records".into())
        );
        assert_eq!(
            parse_seller_input("https://www.discogs.com/user/some-shop"),
            Some("some-shop".into())
        );
        assert_eq!(parse_seller_input("https://www.discogs.com/"), None);
        assert_eq!(parse_seller_input(""), None);
        assert_eq!(parse_seller_input("not a name"), None);
    }

    #[test]
    fn condition_grades_shorten() {
        assert_eq!(cond_short("Very Good Plus (VG+)"), "VG+");
        assert_eq!(cond_short("Mint (M)"), "M");
        assert_eq!(cond_short("Generic"), "Generic");
    }
}
