//! Wantlist watch: which records from the wantlist are in stock at the saved
//! sellers, cheapest first.
//!
//! This is the payoff for sweeping shops. Both halves already live in the
//! catalog — the wantlist cache and every seller's swept listings — so the
//! watch is a pure local join (`Catalog::wantlist_offers`): no request is
//! spent, and it refreshes with the same reload every job already triggers.
//! Each row is one concrete offer (a record stocked by three shops shows
//! three rows), so the same want can be compared across sellers by price and
//! grade. A row opens the ordinary record sheet carrying that seller's offer,
//! exactly as if the listing had been opened from the shop's own crates.

use super::*;
use crate::vinyl_sheet::SellerOffer;

/// Window width. Same as the versions panel: rows carry price, grade and a
/// seller name on one line, and wrapping those makes the list unscannable.
const PANEL_W: f32 = 560.0;

/// Side of a row's sleeve thumbnail, matching the versions panel's rows.
const THUMB: f32 = 48.0;

/// What a row asked for, applied after the window releases its borrows.
enum Act {
    /// Open the record sheet, carrying this seller's concrete offer.
    Open(usize),
    /// Open the listing on discogs.com — where the purchase happens.
    Buy(usize),
}

/// One offer as the window draws it, snapshotted so the window closure never
/// borrows the offer list the click handlers need.
struct Row {
    cover: Option<Tex>,
    artist: String,
    title: String,
    /// Year · format · label catno, whichever exist.
    sub: String,
    /// Price and grading, e.g. `€14.00 · VG+/VG`.
    terms: String,
    seller: String,
}

impl App {
    /// The toolbar button's count: how many distinct wanted records are in
    /// stock somewhere. Offers are rows-per-listing, so this folds them.
    pub(crate) fn watch_record_count(&self) -> usize {
        let mut seen = HashSet::new();
        for (_, l) in &self.wantlist_watch {
            seen.insert(l.release_id);
        }
        seen.len()
    }

    /// The wantlist watch window. Drawn every frame the flag is up; the data
    /// is refreshed by `reload`, so a sweep or a wantlist edit landing while
    /// the window is open updates it in place.
    pub(crate) fn draw_wantlist_watch(&mut self, ctx: &egui::Context) {
        if !self.show_watch || self.view != LibraryView::Vinyl {
            return;
        }
        // Snapshot the rows before `dig_cover` needs `self` mutably.
        let specs: Vec<(Option<String>, String, String, String, String, String)> = self
            .wantlist_watch
            .iter()
            .map(|(seller, l)| {
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
                let mut terms = crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                    value: l.price,
                    currency: l.currency.clone(),
                });
                if let Some(c) = l.condition.as_deref() {
                    terms.push_str(&format!(" · {}", crate::sellers::cond_short(c)));
                    if let Some(s) = l.sleeve_condition.as_deref() {
                        terms.push_str(&format!("/{}", crate::sellers::cond_short(s)));
                    }
                }
                (
                    l.thumb_url.clone().filter(|u| !u.trim().is_empty()),
                    l.artist.clone(),
                    l.title.clone(),
                    sub,
                    terms,
                    seller.clone(),
                )
            })
            .collect();
        let rows: Vec<Row> = specs
            .into_iter()
            .map(|(thumb, artist, title, sub, terms, seller)| Row {
                cover: thumb.as_deref().and_then(|u| self.dig_cover(u).cloned()),
                artist,
                title,
                sub,
                terms,
                seller,
            })
            .collect();
        let swept = self.sellers.iter().any(|s| s.swept_at.is_some());

        let mut act: Option<Act> = None;
        let mut open = true;
        egui::Window::new("Wantlist watch")
            .id(egui::Id::new("wantlist-watch"))
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
                        "Records from your wantlist for sale at your saved sellers, \
                         cheapest first. Update a shop to refresh its stock.",
                    )
                    .weak()
                    .small(),
                );
                ui.add_space(6.0);
                if rows.is_empty() {
                    let msg = if !swept {
                        "No updated sellers yet. Save a seller in the Sellers tab and \
                         update their inventory — anything they stock from your \
                         wantlist lands here."
                    } else {
                        "None of your saved sellers currently stock a record from \
                         your wantlist."
                    };
                    ui.label(egui::RichText::new(msg).weak());
                    ui.add_space(6.0);
                    return;
                }
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        for (i, r) in rows.iter().enumerate() {
                            ui.horizontal(|ui| {
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
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.label(egui::RichText::new(&r.artist).strong());
                                    ui.label(&r.title);
                                    if !r.sub.is_empty() {
                                        ui.label(egui::RichText::new(&r.sub).small().weak());
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .button("Buy ↗")
                                            .on_hover_note("Open this listing on discogs.com")
                                            .clicked()
                                        {
                                            act = Some(Act::Buy(i));
                                        }
                                        if ui
                                            .button("Open")
                                            .on_hover_note("Open the record with this offer")
                                            .clicked()
                                        {
                                            act = Some(Act::Open(i));
                                        }
                                        ui.vertical(|ui| {
                                            ui.spacing_mut().item_spacing.y = 1.0;
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(egui::RichText::new(&r.terms).color(
                                                        egui::Color32::from_rgb(120, 200, 140),
                                                    ));
                                                },
                                            );
                                            ui.with_layout(
                                                egui::Layout::right_to_left(egui::Align::Center),
                                                |ui| {
                                                    ui.label(
                                                        egui::RichText::new(format!(
                                                            "at {}",
                                                            r.seller
                                                        ))
                                                        .small()
                                                        .weak(),
                                                    );
                                                },
                                            );
                                        });
                                    },
                                );
                            });
                            ui.add_space(4.0);
                            ui.separator();
                            ui.add_space(4.0);
                        }
                    });
            });
        if !open {
            self.show_watch = false;
        }
        match act {
            Some(Act::Open(i)) => self.open_watch_offer(i, ctx),
            Some(Act::Buy(i)) => {
                if let Some((_, l)) = self.wantlist_watch.get(i) {
                    let url = l.uri.clone().unwrap_or_else(|| {
                        format!("https://www.discogs.com/release/{}", l.release_id)
                    });
                    open_url(&url);
                }
            }
            None => {}
        }
    }

    /// Open the record sheet for one watch row, carrying the seller's concrete
    /// offer — same contract as opening the listing from that shop's crates.
    fn open_watch_offer(&mut self, idx: usize, ctx: &egui::Context) {
        let Some((seller, l)) = self.wantlist_watch.get(idx).cloned() else {
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
                    shipping: l.shipping_price.map(|value| discogs::MarketPrice {
                        value,
                        currency: l
                            .shipping_currency
                            .clone()
                            .unwrap_or_else(|| l.currency.clone()),
                    }),
                    uri: l.uri.clone(),
                });
            }
        }
    }
}
