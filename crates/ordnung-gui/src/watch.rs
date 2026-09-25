//! Wantlist watch: which records from the wantlist are in stock at the saved
//! sellers, cheapest first.
//!
//! This is the payoff for sweeping shops. Both halves already live in the
//! catalog — the wantlist cache and every seller's swept listings — so the
//! watch is a pure local join (`Catalog::wantlist_offers`): no request is
//! spent, and it refreshes with the same reload every job already triggers.
//! Two layouts of the same rows. **By seller** (the default) folds the offers
//! into one basket per shop (`model::seller_baskets`), ranked by how many
//! wants the shop holds: the point of sweeping several sellers is to find the
//! one order that clears the most of the wantlist while paying shipping once,
//! so each basket says what it comes to and roughly what that is per record.
//! **Cheapest** is the flat list, one row per concrete offer (a record stocked
//! by three shops shows three rows), for comparing the same want across
//! sellers by price and grade. Either way a row opens the ordinary record
//! sheet carrying that seller's offer, exactly as if the listing had been
//! opened from the shop's own crates.

use super::*;
use crate::vinyl_sheet::SellerOffer;
use ordnung_core::model::{seller_baskets, SellerBasket};

/// Window width. Same as the versions panel: rows carry price, grade and a
/// seller name on one line, and wrapping those makes the list unscannable.
const PANEL_W: f32 = 560.0;

/// Side of a row's sleeve thumbnail, matching the versions panel's rows.
const THUMB: f32 = 48.0;

/// What a row asked for, applied after the window releases its borrows.
/// Keyed by listing id so a row means the same offer in either layout.
enum Act {
    /// Open the record sheet, carrying this seller's concrete offer.
    Open(u64),
    /// Open the listing on discogs.com — where the purchase happens.
    Buy(u64),
}

/// One offer as the window draws it, snapshotted so the window closure never
/// borrows the offer list the click handlers need.
struct Row {
    listing_id: u64,
    cover: Option<Tex>,
    artist: String,
    title: String,
    /// Year · format · label catno, whichever exist.
    sub: String,
    /// Price and grading, e.g. `€14.00 · VG+/VG`.
    terms: String,
    seller: String,
}

/// The price a basket comes to, per currency: `€47.00`, or `€47.00 + $12.00`
/// for the rare shop quoting in two.
fn fmt_subtotal(b: &SellerBasket) -> String {
    b.subtotal
        .iter()
        .map(|(currency, value)| {
            crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                value: *value,
                currency: currency.clone(),
            })
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

/// A basket's header: the shop, how much of the wantlist it covers, what the
/// basket comes to with shipping, and the per-record figure that ranks one
/// order against another. The button opens the shop's own "in my wantlist"
/// page on discogs.com, where the basket is put in the cart.
fn draw_basket_header(ui: &mut egui::Ui, b: &SellerBasket) {
    use crate::ui::tokens::color;
    let n = b.wants();
    let mut meta = format!(
        "{n} of your wants · {}",
        fmt_subtotal(b),
    );
    match &b.shipping {
        Some((p, c)) => meta.push_str(&format!(
            " + {} shipping",
            crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                value: *p,
                currency: c.clone(),
            })
        )),
        None => meta.push_str(" · shipping not quoted"),
    }
    if let Some((per, c)) = b.landed_per_record() {
        meta.push_str(&format!(
            " · ≈ {} per record",
            crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                value: per,
                currency: c,
            })
        ));
    }
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.label(egui::RichText::new(&b.seller).strong().color(color::ACCENT_HOVER));
            ui.label(egui::RichText::new(meta).small().weak());
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Shop wants ↗")
                .on_hover_note("Open this seller's stock from your wantlist on discogs.com")
                .clicked()
            {
                open_url(&format!(
                    "https://www.discogs.com/seller/{}/mywants",
                    b.seller
                ));
            }
        });
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
}

/// One offer row: sleeve, credit, and the terms pinned right, with the shop
/// named under them when the row isn't already sitting under its basket's
/// header. Returns what the row asked for.
fn draw_row(ui: &mut egui::Ui, r: &Row, show_seller: bool) -> Option<Act> {
    let mut act = None;
    ui.horizontal(|ui| {
        let (trect, _) = ui.allocate_exact_size(egui::vec2(THUMB, THUMB), egui::Sense::hover());
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
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .button("Buy ↗")
                .on_hover_note("Open this listing on discogs.com")
                .clicked()
            {
                act = Some(Act::Buy(r.listing_id));
            }
            if ui
                .button("Open")
                .on_hover_note("Open the record with this offer")
                .clicked()
            {
                act = Some(Act::Open(r.listing_id));
            }
            ui.vertical(|ui| {
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.label(
                        egui::RichText::new(&r.terms)
                            .color(egui::Color32::from_rgb(120, 200, 140)),
                    );
                });
                if show_seller {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("at {}", r.seller))
                                .small()
                                .weak(),
                        );
                    });
                }
            });
        });
    });
    ui.add_space(4.0);
    ui.separator();
    ui.add_space(4.0);
    act
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
        let swept = self.sellers.iter().any(|s| s.swept_at.is_some());
        // Snapshot the rows before `dig_cover` needs `self` mutably. Both
        // layouts are built up front: the flat list straight off the watch,
        // the baskets through the core fold, each basket's shipping floor
        // patched from the shop-wide quote when none of its own listings
        // carries one.
        let flat: Vec<Row> = self
            .wantlist_watch
            .clone()
            .iter()
            .map(|(seller, l)| self.watch_row(seller, l))
            .collect();
        let baskets: Vec<(SellerBasket, Vec<Row>)> = seller_baskets(&self.wantlist_watch)
            .into_iter()
            .map(|mut b| {
                if b.shipping.is_none() {
                    b.shipping = self
                        .seller_shipping
                        .get(&b.seller)
                        .map(|(p, c)| (*p, c.clone()));
                }
                let rows: Vec<Row> = b
                    .offers
                    .iter()
                    .map(|l| self.watch_row(&b.seller, l))
                    .collect();
                (b, rows)
            })
            .collect();
        let wants_in_stock = self.watch_record_count();
        let by_seller = self.watch_by_seller;

        let mut act: Option<Act> = None;
        let mut pick_layout: Option<usize> = None;
        let mut open = true;
        crate::ui::window::Window::new("Wantlist watch")
            .id(egui::Id::new("wantlist-watch"))
            .open(&mut open)
            .resizable_height()
            .default_width(PANEL_W)
            .show(ctx, |ui| {
                ui.set_min_width(PANEL_W);
                ui.set_max_width(PANEL_W);
                ui.add_space(2.0);
                ui.horizontal(|ui| crate::ui::control_row(ui, |ui| {
                    use crate::ui::button::{segmented, Segment};
                    pick_layout = segmented(
                        ui,
                        Some(if by_seller { 0 } else { 1 }),
                        &[
                            Segment {
                                label: "By seller",
                                tip: "One basket per shop, the shop holding the most of your wants first: one order, one shipping charge",
                            },
                            Segment {
                                label: "Cheapest",
                                tip: "Every offer in one list, cheapest copy first",
                            },
                        ],
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let summary = match (wants_in_stock, baskets.len()) {
                            (0, _) => String::new(),
                            (w, 1) => format!("{w} of your wants in stock at 1 seller"),
                            (w, n) => format!("{w} of your wants in stock at {n} sellers"),
                        };
                        ui.label(egui::RichText::new(summary).weak().small());
                    });
                }));
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(if by_seller {
                        "Records from your wantlist for sale at your saved sellers, grouped \
                         by shop. Per-record cost assumes the whole basket ships as one order. \
                         Update a shop to refresh its stock."
                    } else {
                        "Records from your wantlist for sale at your saved sellers, \
                         cheapest first. Update a shop to refresh its stock."
                    })
                    .weak()
                    .small(),
                );
                ui.add_space(6.0);
                if flat.is_empty() {
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
                        if by_seller {
                            for (b, rows) in &baskets {
                                draw_basket_header(ui, b);
                                for r in rows {
                                    if let Some(a) = draw_row(ui, r, false) {
                                        act = Some(a);
                                    }
                                }
                                ui.add_space(8.0);
                            }
                        } else {
                            for r in &flat {
                                if let Some(a) = draw_row(ui, r, true) {
                                    act = Some(a);
                                }
                            }
                        }
                    });
            });
        match pick_layout {
            Some(0) => self.watch_by_seller = true,
            Some(1) => self.watch_by_seller = false,
            _ => {}
        }
        if !open {
            self.show_watch = false;
        }
        match act {
            Some(Act::Open(id)) => self.open_watch_offer(id, ctx),
            Some(Act::Buy(id)) => {
                if let Some((_, l)) = self.wantlist_watch.iter().find(|(_, l)| l.listing_id == id) {
                    let url = l.uri.clone().unwrap_or_else(|| {
                        format!("https://www.discogs.com/release/{}", l.release_id)
                    });
                    open_url(&url);
                }
            }
            None => {}
        }
    }

    /// One offer as a drawable row: the caption strings plus the sleeve
    /// thumbnail out of the shared URL-keyed cover cache.
    fn watch_row(&mut self, seller: &str, l: &SellerListing) -> Row {
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
        let thumb = l.thumb_url.clone().filter(|u| !u.trim().is_empty());
        Row {
            listing_id: l.listing_id,
            cover: thumb.as_deref().and_then(|u| self.dig_cover(u).cloned()),
            artist: l.artist.clone(),
            title: l.title.clone(),
            sub,
            terms,
            seller: seller.to_string(),
        }
    }

    /// Open the record sheet for one watch row, carrying the seller's concrete
    /// offer — same contract as opening the listing from that shop's crates.
    fn open_watch_offer(&mut self, listing_id: u64, ctx: &egui::Context) {
        let Some((seller, l)) = self
            .wantlist_watch
            .iter()
            .find(|(_, l)| l.listing_id == listing_id)
            .cloned()
        else {
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
                    listing_id: l.listing_id,
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
