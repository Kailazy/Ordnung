//! The Sellers tab of the vinyl view: dig through a saved Discogs seller's
//! crates the way you'd flip through the bins in their shop.
//!
//! Sellers are saved by username (Discogs removed the release→sellers
//! direction, so the shop is the only way in — see
//! `docs/design/bulk-sellers-spike.md`). An explicit Update pages the shop's
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
    /// Open the label page for the listing's release.
    LabelPage(usize),
    /// Open the record sheet, carrying this seller's concrete offer.
    Open(usize),
    /// Open the listing itself on discogs.com — where the purchase happens.
    Buy(usize),
    /// Add the listing's release to the Discogs wantlist.
    Want(usize),
    /// Start a crate dig from the listing's release — see [`crate::dig`].
    Dig(usize),
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

/// Rank of a media grade for the condition-floor filter, higher = better:
/// P=0, F=1, G=2, G+=3, VG=4, VG+=5, NM=6, M=7. `None` when the string isn't
/// a recognizable grade — a floor filter drops those, since it can't vouch
/// for them.
pub(crate) fn grade_rank(condition: &str) -> Option<u8> {
    match cond_short(condition) {
        "M" => Some(7),
        s if s.starts_with("NM") => Some(6), // Discogs writes "NM or M-"
        "VG+" => Some(5),
        "VG" => Some(4),
        "G+" => Some(3),
        "G" => Some(2),
        "F" => Some(1),
        "P" => Some(0),
        _ => None,
    }
}

/// The condition floors the Filters popup offers: label + minimum rank.
pub(crate) const GRADE_FLOORS: [(&str, u8); 5] = [
    ("G+ or better", 3),
    ("VG or better", 4),
    ("VG+ or better", 5),
    ("NM or better", 6),
    ("M only", 7),
];

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
        // The bulk source: the imported Discogs genre database covers whatever
        // the release-detail cache doesn't, which after one import is nearly
        // everything.
        if let Ok(Some(gdb)) = genredb::GenreDb::open(&genredb::default_path(&self.db_path)) {
            let missing: Vec<u64> = ids
                .iter()
                .copied()
                .filter(|id| !self.seller_genres.contains_key(id))
                .collect();
            if let Ok(map) = gdb.genres_for(&missing) {
                for (id, tags) in map {
                    self.seller_genres.entry(id).or_insert(tags);
                }
            }
        }
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

        // --- Shop row: add, pick a shop, find a record, then the current
        // shop's actions pinned right. The shops live in an alphabetical
        // dropdown rather than a strip of chips: a strip is scanned by eye
        // and stops working past a couple of dozen names, a sorted list
        // scrolls and can be lettered. The find box searches the *records*
        // across every saved shop and names the shop on each hit, so "who
        // has this?" is one lookup instead of a shop-by-shop tour.
        let mut switch_to: Option<String> = None;
        let mut remove: Option<String> = None;
        let mut sweep: Option<String> = None;
        let mut add_clicked = false;
        // A find-box hit picked this frame: `(seller, listing_id)`.
        let mut pick: Option<(String, u64)> = None;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            // The add box lives in a popup so the row stays a row of
            // buttons; the button toggles it, Enter or Add inside submits.
            let add_btn = ui
                .small_button("＋ Add seller")
                .on_hover_note("Save a Discogs seller to dig through, by username or shop URL");
            let popup_id = ui.make_persistent_id("seller-add-popup");
            if add_btn.clicked() {
                ui.memory_mut(|m| m.toggle_popup(popup_id));
            }
            let just_opened = add_btn.clicked() && ui.memory(|m| m.is_popup_open(popup_id));
            egui::popup::popup_below_widget(
                ui,
                popup_id,
                &add_btn,
                egui::PopupCloseBehavior::CloseOnClickOutside,
                |ui| {
                    ui.set_min_width(220.0);
                    ui.horizontal(|ui| {
                        let edit = ui.add(
                            egui::TextEdit::singleline(&mut self.seller_add)
                                .desired_width(180.0)
                                .hint_text("Seller username or URL"),
                        );
                        if just_opened {
                            edit.request_focus();
                        }
                        let submitted =
                            edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                        if ui
                            .small_button("Add")
                            .on_hover_note("Save this seller")
                            .clicked()
                            || submitted
                        {
                            add_clicked = true;
                        }
                    });
                },
            );
            if !self.sellers.is_empty() {
                ui.add_space(6.0);
                self.draw_seller_picker(ui, &mut switch_to);
                ui.add_space(6.0);
                self.draw_seller_find(ui, ctx, &mut pick);
            } else {
                self.seller_find.clear();
                self.seller_find_hits.clear();
                self.seller_find_for.clear();
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if let Some(cur) = self.seller_current.clone() {
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui
                            .button("⟲ Update")
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
                    if ui
                        .button("✖ Remove")
                        .on_hover_note("Forget this seller and their cached crates")
                        .clicked()
                    {
                        remove = Some(cur.clone());
                    }
                }
            });
        });
        // A find-box pick: land in that shop with the record ringed and
        // scrolled to. Done before the generic switch below so the highlight
        // set here survives (a hand-picked shop clears it instead).
        if let Some((seller, listing_id)) = pick {
            if self.seller_current.as_deref() != Some(seller.as_str()) {
                self.seller_current = Some(seller);
                self.ensure_seller_listings();
            }
            self.seller_highlight = Some(listing_id);
            self.seller_scroll_to = true;
            ctx.memory_mut(|m| m.close_popup());
        }

        // Apply the shop-row asks now that `self.sellers` is free again.
        if let Some(u) = switch_to {
            self.seller_current = Some(u);
            self.seller_highlight = None;
            self.seller_scroll_to = false;
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
                            ctx.memory_mut(|m| m.close_popup());
                            self.ensure_seller_listings();
                        }
                        Err(e) => self.status = format!("Couldn't save seller: {e}"),
                    }
                }
                None => {
                    self.status = "That doesn't look like a Discogs username or seller URL.".into();
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

        // The dig strip works from the crates too: a listing seeds a dig the
        // same way a shelf record does, and the web stays on screen while the
        // tab underneath changes. Same placement contract as the shelf view —
        // above the scrolling grid, so it never scrolls away under what it's
        // steering. A record the strip asks to open goes through the ordinary
        // release sheet.
        if let Some(o) = self.draw_dig(ui) {
            self.open_release_sheet(o.release_id, o.artist, o.title, o.sub, o.cover_url, ctx);
        }
        if self.dig.is_some() {
            ui.add_space(8.0);
        }

        // --- Empty states. ----------------------------------------------------
        if self.sellers.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("Dig through a seller's crates");
                ui.add_space(6.0);
                ui.label("Save a Discogs seller above, then update their inventory.");
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
        // genre facet reads `seller_genres`; a listing with no known tags
        // can't match a tag, so it drops out (the Filters popup says how many
        // records have known tags at all). Same rule for a year, format or
        // condition bound the listing can't answer — a filter never vouches
        // for a row it can't check.
        let flt = self.vinyl_flt.clone();
        let active = flt.active(true);
        let unfiltered = query.is_empty() && active == 0;
        let price_cap = flt.price_cap();
        let filtered: Vec<usize> = self
            .seller_listings
            .iter()
            .enumerate()
            .filter(|(i, l)| {
                (query.is_empty() || listing_matches(&self.seller_hay[*i], query))
                    && (flt.genres.is_empty()
                        || self
                            .seller_genres
                            .get(&l.release_id)
                            .is_some_and(|tags| flt.genres_keep(tags)))
                    && flt.year_keep(l.year)
                    && flt.format_keep(l.format.as_deref())
                    && price_cap.is_none_or(|cap| l.price <= cap)
                    && flt.min_grade.is_none_or(|floor| {
                        l.condition
                            .as_deref()
                            .and_then(grade_rank)
                            .is_some_and(|r| r >= floor)
                    })
                    && !(flt.hide_owned && self.vinyl_owned.contains(&l.release_id))
                    && !(flt.hide_wanted && self.vinyl_wanted.contains(&l.release_id))
                    && !(flt.hide_viewed && self.viewed_releases.contains(&l.release_id))
            })
            .map(|(i, _)| i)
            .collect();
        // Where the find box's pick sits in the visible crates, if it does:
        // what the scroll-to aims at, and what the meta line explains when
        // the current search or filters hide it.
        let highlight_pos = self.seller_highlight.and_then(|id| {
            filtered
                .iter()
                .position(|&i| self.seller_listings[i].listing_id == id)
        });
        let highlight_hidden = highlight_pos.is_none()
            && self.seller_highlight.is_some_and(|id| {
                self.seller_listings.iter().any(|l| l.listing_id == id)
            });
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let mut meta = match (unfiltered, shop.cached) {
                (true, n) => format!("{n} records in the crates"),
                (false, _) => format!("{} of {} records match", filtered.len(), shop.cached),
            };
            match shop.swept_at {
                Some(t) => meta.push_str(&format!(" · updated {}", fmt_ago(t))),
                None => meta.push_str(" · never updated"),
            }
            if highlight_hidden {
                meta.push_str(" · the record you picked is hidden by the search or filters");
            }
            ui.label(egui::RichText::new(meta).weak());
        });
        ui.add_space(4.0);

        if self.seller_listings.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                if shop.swept_at.is_none() {
                    ui.label(format!(
                        "Nothing cached for {} yet — update to pull their crates.",
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
                // Name the tags when they're the whole filter ("Techno and
                // Ambient" — the AND actually applied); with year/price/
                // condition bounds in play a generic line beats a paragraph.
                let only_tags = active == flt.genres.len() && !flt.genres.is_empty();
                let msg = {
                    match (only_tags, active > 0, query.is_empty()) {
                        (true, _, true) => {
                            let tags_named = match flt.genres.as_slice() {
                                [one] => one.clone(),
                                [head @ .., last] => {
                                    format!("{} and {last}", head.join(", "))
                                }
                                [] => unreachable!(),
                            };
                            format!("Nothing in the crates is tagged {tags_named}.")
                        }
                        (_, true, _) => "Nothing in the crates matches those filters.".to_string(),
                        (_, false, _) => "Nothing in the crates matches that search.".to_string(),
                    }
                };
                ui.label(egui::RichText::new(msg).weak());
            });
            return;
        }

        // --- The crates: a virtualized cover grid, or compact rows. -----------
        // Same sizing rules as the vinyl wall, but drawn through `show_rows` —
        // a swept shop can be tens of thousands of cards, and only the visible
        // rows should cost layout (or a cover download). The toolbar's grid/
        // list toggle applies here too: rows put price and grade in a column,
        // which is how a shop's crates get compared.
        let mut act: Option<SellerAct> = None;
        if self.config.vinyl_view == "list" {
            const ROW_H: f32 = 54.0;
            let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(p) = highlight_pos.filter(|_| self.seller_scroll_to) {
                // One row of context above the ringed one, so it doesn't sit
                // flush against the top edge.
                area = area.vertical_scroll_offset((p as f32 - 1.0).max(0.0) * ROW_H);
                self.seller_scroll_to = false;
            }
            area.show_rows(ui, ROW_H, filtered.len(), |ui, rows| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for i in rows {
                        let idx = filtered[i];
                        if let Some(a) = self.seller_row(ui, idx, ROW_H) {
                            act = Some(a);
                        }
                    }
                });
        } else {
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

            let mut area = egui::ScrollArea::vertical().auto_shrink([false, false]);
            if let Some(p) = highlight_pos.filter(|_| self.seller_scroll_to) {
                area = area.vertical_scroll_offset((p / cols) as f32 * row_h);
                self.seller_scroll_to = false;
            }
            area.show_rows(ui, row_h, n_rows, |ui, rows| {
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
        }

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
            Some(SellerAct::Dig(idx)) => {
                if let Some(l) = self.seller_listings.get(idx).cloned() {
                    let sub = match (l.year, l.format.as_deref()) {
                        (Some(y), Some(f)) => format!("{y} · {f}"),
                        (Some(y), None) => y.to_string(),
                        (None, Some(f)) => f.to_string(),
                        (None, None) => String::new(),
                    };
                    self.start_dig_release(
                        l.release_id,
                        l.artist,
                        l.title,
                        l.label,
                        sub,
                        l.thumb_url,
                    );
                }
            }
            Some(SellerAct::LabelPage(idx)) => {
                if let Some(l) = self.seller_listings.get(idx).cloned() {
                    self.open_label_page(l.release_id, l.label);
                }
            }
            None => {}
        }
    }

    /// The shop picker: an alphabetical dropdown of every saved seller, the
    /// current one named on the button. Past a dozen shops the list gets
    /// letter headings so a long roster scans the way a strip of chips
    /// never could. A pick lands in `switch_to`, applied once the borrows
    /// on `self.sellers` are released.
    fn draw_seller_picker(&self, ui: &mut egui::Ui, switch_to: &mut Option<String>) {
        use crate::ui::tokens::color;
        const LETTERS_AT: usize = 12;
        let mut sorted: Vec<&SellerShop> = self.sellers.iter().collect();
        sorted.sort_by_key(|s| s.username.to_lowercase());
        let current = self
            .seller_current
            .clone()
            .unwrap_or_else(|| "Pick a seller".to_string());
        let lettered = sorted.len() >= LETTERS_AT;
        let resp = egui::ComboBox::from_id_salt("seller-pick")
            .selected_text(egui::RichText::new(current).strong())
            .width(220.0)
            .height(420.0)
            .show_ui(ui, |ui| {
                ui.set_min_width(240.0);
                let mut last_letter: Option<char> = None;
                for shop in &sorted {
                    if lettered {
                        let letter = shop
                            .username
                            .chars()
                            .next()
                            .map(|c| c.to_uppercase().next().unwrap_or(c))
                            .map(|c| if c.is_alphabetic() { c } else { '#' })
                            .unwrap_or('#');
                        if last_letter != Some(letter) {
                            if last_letter.is_some() {
                                ui.add_space(4.0);
                            }
                            ui.label(
                                egui::RichText::new(letter.to_string())
                                    .small()
                                    .color(color::LABEL_3),
                            );
                            last_letter = Some(letter);
                        }
                    }
                    let active = self.seller_current.as_deref() == Some(shop.username.as_str());
                    let text = match shop.cached {
                        0 => shop.username.clone(),
                        n => format!("{}  ·  {n}", shop.username),
                    };
                    let row = ui.selectable_label(active, text);
                    // The row's hover popup is the shop's info card, led by
                    // the shipping floor. Quotes are per record and
                    // location-specific; a seller publishing only a free-text
                    // policy has none, and that absence never reads as free.
                    let mut note = String::from("Browse this seller's crates");
                    match self.seller_shipping.get(&shop.username) {
                        Some((price, currency)) => note.push_str(&format!(
                            ". Shipping from {} per record",
                            crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                                value: *price,
                                currency: currency.clone(),
                            })
                        )),
                        None => note.push_str(
                            ". Shipping not quoted yet, update the crates to fetch it",
                        ),
                    }
                    if row.on_hover_note(note).clicked() {
                        *switch_to = Some(shop.username.clone());
                    }
                }
            });
        resp.response
            .on_hover_note(format!("Pick one of {} saved sellers, A to Z", sorted.len()));
    }

    /// The find box: a record search across every saved seller's crates,
    /// with the hits in a dropdown under the field, each naming the shop that
    /// has it. Typing re-runs the query (a bounded SQL scan, no network);
    /// a click lands in `pick` as `(seller, listing_id)` for the caller to
    /// switch shops and ring the record.
    fn draw_seller_find(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        pick: &mut Option<(String, u64)>,
    ) {
        use crate::ui::tokens::color;
        /// How many hits the dropdown shows; past this, keep typing.
        const MAX_HITS: usize = 8;
        const ROW_H: f32 = 40.0;
        const POPUP_W: f32 = 460.0;

        let n_sellers = self.sellers.len();
        let edit = ui.add(
            egui::TextEdit::singleline(&mut self.seller_find)
                .desired_width(240.0)
                .hint_text(format!("Find a record across {n_sellers} sellers")),
        );
        let edit = edit.on_hover_note(
            "Search every saved seller's crates by artist, title or label; \
             each hit names the shop",
        );
        let popup_id = ui.make_persistent_id("seller-find-popup");
        let query = self.seller_find.trim().to_string();
        if query != self.seller_find_for {
            self.seller_find_hits = if query.is_empty() {
                Vec::new()
            } else {
                Catalog::open(&self.db_path)
                    .and_then(|c| c.find_seller_listings(&query, MAX_HITS))
                    .unwrap_or_default()
            };
            self.seller_find_for = query.clone();
            ui.memory_mut(|m| {
                if query.is_empty() {
                    if m.is_popup_open(popup_id) {
                        m.close_popup();
                    }
                } else {
                    m.open_popup(popup_id);
                }
            });
        } else if edit.gained_focus() && !query.is_empty() {
            ui.memory_mut(|m| m.open_popup(popup_id));
        }
        if edit.has_focus() && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            ui.memory_mut(|m| {
                if m.is_popup_open(popup_id) {
                    m.close_popup();
                }
            });
        }
        if !query.is_empty()
            && ui
                .small_button("✖")
                .on_hover_note("Clear the search")
                .clicked()
        {
            self.seller_find.clear();
        }

        let hits = &self.seller_find_hits;
        let highlight = self.seller_highlight;
        egui::popup::popup_below_widget(
            ui,
            popup_id,
            &edit,
            egui::PopupCloseBehavior::CloseOnClickOutside,
            |ui| {
                ui.set_min_width(POPUP_W);
                ui.spacing_mut().item_spacing.y = 0.0;
                if hits.is_empty() {
                    ui.add_space(6.0);
                    ui.label(
                        egui::RichText::new("No record in the crates matches that").weak(),
                    );
                    ui.add_space(6.0);
                    return;
                }
                for (seller, l) in hits.iter() {
                    let (rect, resp) = ui.allocate_exact_size(
                        egui::vec2(ui.available_width().max(POPUP_W), ROW_H),
                        egui::Sense::click(),
                    );
                    let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                    let is_current = highlight == Some(l.listing_id);
                    if resp.hovered() || is_current {
                        ui.painter().rect_filled(
                            rect,
                            egui::Rounding::same(5.0),
                            if resp.hovered() {
                                color::SURFACE_HOVER
                            } else {
                                color::SURFACE_HI
                            },
                        );
                    }
                    // Right column: the shop, then the offer under it. The
                    // shop is the answer the box exists for, so it takes
                    // the stronger colour.
                    let mut terms = crate::vinyl_sheet::fmt_market_price(&discogs::MarketPrice {
                        value: l.price,
                        currency: l.currency.clone(),
                    });
                    if let Some(c) = l.condition.as_deref() {
                        terms.push_str(&format!(" · {}", cond_short(c)));
                    }
                    let right_x = rect.right() - 10.0;
                    let seller_rect = ui.painter().text(
                        egui::pos2(right_x, rect.top() + 6.0),
                        egui::Align2::RIGHT_TOP,
                        seller,
                        crate::ui::tokens::font::callout(),
                        color::ACCENT_HOVER,
                    );
                    let terms_rect = ui.painter().text(
                        egui::pos2(right_x, rect.bottom() - 6.0),
                        egui::Align2::RIGHT_BOTTOM,
                        &terms,
                        crate::ui::tokens::font::footnote(),
                        egui::Color32::from_rgb(120, 200, 140),
                    );
                    let right_w = seller_rect.width().max(terms_rect.width()) + 16.0;
                    // Left column: artist over title · year, truncated so
                    // a long credit never runs under the shop name.
                    let text_rect = egui::Rect::from_min_max(
                        egui::pos2(rect.left() + 10.0, rect.top() + 5.0),
                        egui::pos2(rect.right() - right_w - 6.0, rect.bottom() - 4.0),
                    );
                    let mut text_ui = ui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(text_rect)
                            .layout(egui::Layout::top_down(egui::Align::Min))
                            .id_salt(("seller-find-row", l.listing_id)),
                    );
                    text_ui.spacing_mut().item_spacing.y = 0.0;
                    text_ui.add(
                        egui::Label::new(egui::RichText::new(&l.artist).strong()).truncate(),
                    );
                    let line2 = match l.year {
                        Some(y) => format!("{} · {y}", l.title),
                        None => l.title.clone(),
                    };
                    text_ui.add(
                        egui::Label::new(egui::RichText::new(line2).small().weak()).truncate(),
                    );
                    if resp
                        .on_hover_note(format!("Show this record in {seller}'s crates"))
                        .clicked()
                    {
                        *pick = Some((seller.clone(), l.listing_id));
                    }
                }
                if hits.len() >= MAX_HITS {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!(
                            "First {MAX_HITS} matches, keep typing to narrow"
                        ))
                        .small()
                        .weak(),
                    );
                    ui.add_space(4.0);
                }
            },
        );
    }

    /// One card of the crates: cover, credit, and the sale terms — price and
    /// grade are what separate flipping through a shop from browsing a
    /// discography, so they get the caption's third line. Returns what the
    /// user asked of it, if anything.
    fn seller_card(&mut self, ui: &mut egui::Ui, idx: usize, cover_side: f32) -> Option<SellerAct> {
        use crate::ui::tokens::color;

        // Snapshot the card's strings before `dig_cover` needs `self` mutably.
        let (listing_id, release_id, artist, title, sub, price_line, thumb, owned, wanted, viewed) = {
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
                self.viewed_releases.contains(&l.release_id),
            )
        };
        let tex = thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());
        let highlighted = self.seller_highlight == Some(listing_id);

        let mut act: Option<SellerAct> = None;
        ui.allocate_ui_with_layout(
            egui::vec2(cover_side, cover_side + 58.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let (rect, resp) = ui
                    .allocate_exact_size(egui::vec2(cover_side, cover_side), egui::Sense::click());
                let resp = resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_note("Open the record — listen, wantlist, or buy");
                // The dig disc's hit area is claimed before anything paints, so
                // the cover's hover frame can see the disc's hover and the pair
                // reveals together (same reasoning as the shelf grid).
                const D: f32 = 30.0;
                let dig_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - D - 6.0, rect.bottom() - D - 6.0),
                    egui::vec2(D, D),
                );
                let dig_hit = ui.interact(
                    dig_rect,
                    ui.id().with(("seller-dig", listing_id)),
                    egui::Sense::click(),
                );
                let dig_hovered = dig_hit.hovered();
                // Cart disc to the dig disc's left: straight to the listing
                // on discogs.com, whose Add to Cart button is the real cart —
                // the API has no cart endpoint, so there's no local stand-in.
                let cart_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - 2.0 * D - 12.0, rect.bottom() - D - 6.0),
                    egui::vec2(D, D),
                );
                let cart_hit = ui.interact(
                    cart_rect,
                    ui.id().with(("seller-cart", listing_id)),
                    egui::Sense::click(),
                );
                let cart_hovered = cart_hit.hovered();
                let card_hovered = resp.hovered() || dig_hovered || cart_hovered;
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
                if card_hovered {
                    ui.painter().rect_stroke(
                        rect,
                        egui::Rounding::same(6.0),
                        egui::Stroke::new(2.0, color::ACCENT),
                    );
                }
                // The find box's pick: a ring just outside the cover, so it
                // reads at a glance among a wall of cards.
                if highlighted {
                    ui.painter().rect_stroke(
                        rect.expand(3.0),
                        egui::Rounding::same(8.0),
                        egui::Stroke::new(3.0, color::ACCENT_HOVER),
                    );
                }
                // Membership chips: a record already on a shelf is the one
                // thing worth knowing before the price — you either have it
                // or you're already hunting it.
                let mut chip_x = rect.left() + 5.0;
                for (show, list) in [
                    (owned, VinylList::Collection),
                    (!owned && wanted, VinylList::Wantlist),
                ] {
                    if !show {
                        continue;
                    }
                    let chip = crate::ui::icon::shelf_chip(
                        ui.painter(),
                        egui::pos2(chip_x, rect.top() + 5.0),
                        list,
                    );
                    chip_x = chip.right() + 4.0;
                }
                // Viewed eye, top-right: you already pulled this record out
                // and listened — the mark that stops a long dig from
                // re-auditioning the same crates.
                if viewed {
                    let c = egui::pos2(rect.right() - 13.0, rect.top() + 13.0);
                    ui.painter()
                        .circle_filled(c, 10.0, egui::Color32::from_black_alpha(170));
                    crate::records::draw_eye(
                        ui.painter(),
                        c,
                        5.0,
                        egui::Color32::from_gray(235),
                        true,
                    );
                }
                // Dig disc, bottom-right: start a crate dig from this listing.
                // Hover-revealed like the shelf grid's, and its click never
                // falls through to the cover underneath.
                let mut dig_clicked = false;
                if card_hovered {
                    let bg = if dig_hovered {
                        egui::Color32::from_rgb(120, 220, 150)
                    } else {
                        egui::Color32::from_black_alpha(190)
                    };
                    let fg = if dig_hovered {
                        egui::Color32::from_gray(20)
                    } else {
                        egui::Color32::from_gray(240)
                    };
                    ui.painter().circle_filled(dig_rect.center(), D / 2.0, bg);
                    ui.painter().text(
                        dig_rect.center(),
                        egui::Align2::CENTER_CENTER,
                        "🔍",
                        egui::FontId::proportional(14.0),
                        fg,
                    );
                    let dig_hit = dig_hit.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if dig_hit
                        .on_hover_note(
                            "Dig from here: records like this on Discogs that \
                             aren't in your collection",
                        )
                        .clicked()
                    {
                        dig_clicked = true;
                        act = Some(SellerAct::Dig(idx));
                    }
                }
                // Cart disc: hover-revealed like the dig disc; opens the
                // listing page, whose Add to Cart button is the real cart.
                let mut cart_clicked = false;
                if card_hovered {
                    let bg = if cart_hovered {
                        egui::Color32::from_rgb(110, 160, 235)
                    } else {
                        egui::Color32::from_black_alpha(190)
                    };
                    let fg = if cart_hovered {
                        egui::Color32::from_gray(20)
                    } else {
                        egui::Color32::from_gray(240)
                    };
                    ui.painter().circle_filled(cart_rect.center(), D / 2.0, bg);
                    crate::records::draw_cart(ui.painter(), cart_rect.center(), 6.0, fg);
                    let cart_hit = cart_hit.on_hover_cursor(egui::CursorIcon::PointingHand);
                    if cart_hit
                        .on_hover_note("Add to cart on discogs.com (opens the listing)")
                        .clicked()
                    {
                        cart_clicked = true;
                        act = Some(SellerAct::Buy(idx));
                    }
                }
                if resp.clicked() && !dig_clicked && !cart_clicked {
                    act = Some(SellerAct::Open(idx));
                }
                resp.context_menu(|ui| {
                    if ui
                        .button("💰 Add to cart on Discogs ↗")
                        .on_hover_note("Opens the listing; cart and checkout live on discogs.com")
                        .clicked()
                    {
                        act = Some(SellerAct::Buy(idx));
                        ui.close_menu();
                    }
                    if ui
                        .button("🔍  Dig from here")
                        .on_hover_note("Walk Discogs outward from this record, by artist or label")
                        .clicked()
                    {
                        act = Some(SellerAct::Dig(idx));
                        ui.close_menu();
                    }
                    let already = owned || wanted;
                    if crate::ui::icon::shelf_button(
                        ui,
                        VinylList::Wantlist,
                        false,
                        "Add to wantlist",
                        !already,
                    )
                    .clicked()
                    {
                        act = Some(SellerAct::Want(idx));
                        ui.close_menu();
                    }
                    if ui
                        .button("⌂  Browse the label")
                        .on_hover_note("The label's whole run as a list, your shelves marked")
                        .clicked()
                    {
                        act = Some(SellerAct::LabelPage(idx));
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
                        egui::Label::new(egui::RichText::new(clip(&artist)).small().strong())
                            .truncate(),
                    );
                    let title_text = if sub.is_empty() {
                        title.clone()
                    } else {
                        format!("{title} · {sub}")
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(title_text).small().weak()).truncate(),
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

    /// One row of the crates in list layout: thumb, credit, and the sale
    /// terms pinned to the right edge, where a column of prices and grades
    /// scans the way a wall of cards can't. Same actions as the card — click
    /// opens the record, right-click buys, wantlists or digs.
    fn seller_row(&mut self, ui: &mut egui::Ui, idx: usize, row_h: f32) -> Option<SellerAct> {
        const THUMB: f32 = 44.0;
        /// Right-edge budget for the price + grade column.
        const TERMS_W: f32 = 150.0;

        // Snapshot the row's strings before `dig_cover` needs `self` mutably
        // (same dance as the card).
        let (listing_id, release_id, artist, title, sub, price_line, thumb, owned, wanted, viewed) = {
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
                self.viewed_releases.contains(&l.release_id),
            )
        };
        let tex = thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());

        let mut act: Option<SellerAct> = None;
        let avail = ui.available_width();
        let (rect, resp) = ui.allocate_exact_size(egui::vec2(avail, row_h), egui::Sense::click());
        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            ui.painter().rect_filled(
                rect,
                egui::Rounding::same(6.0),
                crate::ui::tokens::color::SURFACE,
            );
        }
        // The find box's pick: a wash and a ring on the row, matching the
        // card grid's treatment.
        if self.seller_highlight == Some(listing_id) {
            ui.painter().rect_filled(
                rect.shrink(1.0),
                egui::Rounding::same(6.0),
                crate::ui::tokens::color::ACCENT_SOFT.gamma_multiply(0.45),
            );
            ui.painter().rect_stroke(
                rect.shrink(1.0),
                egui::Rounding::same(6.0),
                egui::Stroke::new(2.0, crate::ui::tokens::color::ACCENT_HOVER),
            );
        }
        ui.painter().line_segment(
            [
                egui::pos2(rect.left() + THUMB + 14.0, rect.bottom()),
                egui::pos2(rect.right(), rect.bottom()),
            ],
            egui::Stroke::new(1.0, egui::Color32::from_gray(38)),
        );
        let thumb_rect = egui::Rect::from_min_size(
            egui::pos2(rect.left() + 4.0, rect.center().y - THUMB / 2.0),
            egui::vec2(THUMB, THUMB),
        );
        match &tex {
            Some(h) => {
                egui::Image::new(h)
                    .fit_to_exact_size(egui::vec2(THUMB, THUMB))
                    .rounding(egui::Rounding::same(4.0))
                    .paint_at(ui, thumb_rect);
            }
            None => {
                ui.painter().rect_filled(
                    thumb_rect,
                    egui::Rounding::same(4.0),
                    egui::Color32::from_gray(34),
                );
                ui.painter().text(
                    thumb_rect.center(),
                    egui::Align2::CENTER_CENTER,
                    "♪",
                    egui::FontId::proportional(18.0),
                    egui::Color32::from_gray(70),
                );
            }
        }
        // Sale terms on the right edge, then the membership chip to their
        // left — a record already on a shelf changes what the price means.
        ui.painter().text(
            egui::pos2(rect.right() - 10.0, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            &price_line,
            crate::ui::tokens::font::callout(),
            egui::Color32::from_rgb(120, 200, 140),
        );
        let mut right_edge = rect.right() - TERMS_W;
        for (show, list) in [
            (!owned && wanted, VinylList::Wantlist),
            (owned, VinylList::Collection),
        ] {
            if !show {
                continue;
            }
            let chip = crate::ui::icon::shelf_chip(
                ui.painter(),
                egui::pos2(right_edge - 20.0, rect.center().y - 8.0),
                list,
            );
            right_edge = chip.left() - 8.0;
        }
        // Viewed eye, left of the chip column: same audition marker the card
        // wears in its corner.
        if viewed {
            let c = egui::pos2(right_edge - 9.0, rect.center().y);
            crate::records::draw_eye(ui.painter(), c, 5.5, egui::Color32::from_gray(150), true);
            right_edge = c.x - 9.0 - 8.0;
        }
        // Cart disc, left of the marker column: the same shortcut the card
        // carries, straight to the listing page whose Add to Cart button is
        // the real cart. Revealed on hover.
        const CART_D: f32 = 22.0;
        let cart_rect = egui::Rect::from_min_size(
            egui::pos2(right_edge - CART_D, rect.center().y - CART_D / 2.0),
            egui::vec2(CART_D, CART_D),
        );
        let cart_hit = ui.interact(
            cart_rect,
            ui.id().with(("seller-row-cart", listing_id)),
            egui::Sense::click(),
        );
        let cart_hovered = cart_hit.hovered();
        let mut cart_clicked = false;
        if resp.hovered() || cart_hovered {
            let bg = if cart_hovered {
                egui::Color32::from_rgb(110, 160, 235)
            } else {
                egui::Color32::from_black_alpha(190)
            };
            let fg = if cart_hovered {
                egui::Color32::from_gray(20)
            } else {
                egui::Color32::from_gray(240)
            };
            ui.painter()
                .circle_filled(cart_rect.center(), CART_D / 2.0, bg);
            crate::records::draw_cart(ui.painter(), cart_rect.center(), 5.0, fg);
            let cart_hit = cart_hit.on_hover_cursor(egui::CursorIcon::PointingHand);
            if cart_hit
                .on_hover_note("Add to cart on discogs.com (opens the listing)")
                .clicked()
            {
                cart_clicked = true;
                act = Some(SellerAct::Buy(idx));
            }
        }
        right_edge = cart_rect.left() - 8.0;
        // Artist over title · year · format, truncated between the thumb and
        // the terms column.
        let text_rect = egui::Rect::from_min_max(
            egui::pos2(thumb_rect.right() + 10.0, rect.top() + 8.0),
            egui::pos2(right_edge - 6.0, rect.bottom() - 6.0),
        );
        let mut text_ui = ui.new_child(
            egui::UiBuilder::new()
                .max_rect(text_rect)
                .layout(egui::Layout::top_down(egui::Align::Min))
                .id_salt(("seller-row-text", listing_id)),
        );
        text_ui.spacing_mut().item_spacing.y = 1.0;
        text_ui.add(egui::Label::new(egui::RichText::new(&artist).strong()).truncate());
        let line2 = if sub.is_empty() {
            title.clone()
        } else {
            format!("{title} · {sub}")
        };
        text_ui.add(egui::Label::new(egui::RichText::new(line2).weak()).truncate());

        let resp = resp.on_hover_note("Open the record — listen, wantlist, or buy");
        if resp.clicked() && !cart_clicked {
            act = Some(SellerAct::Open(idx));
        }
        resp.context_menu(|ui| {
            if ui
                .button("💰 Add to cart on Discogs ↗")
                .on_hover_note("Opens the listing; cart and checkout live on discogs.com")
                .clicked()
            {
                act = Some(SellerAct::Buy(idx));
                ui.close_menu();
            }
            if ui
                .button("🔍  Dig from here")
                .on_hover_note("Walk Discogs outward from this record, by artist or label")
                .clicked()
            {
                act = Some(SellerAct::Dig(idx));
                ui.close_menu();
            }
            let already = owned || wanted;
            if crate::ui::icon::shelf_button(ui, VinylList::Wantlist, false, "Add to wantlist", !already)
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
