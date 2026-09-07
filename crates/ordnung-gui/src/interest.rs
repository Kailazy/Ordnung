//! The crate of interest: records dug up and set aside while you decide.
//!
//! A dig turns up more promising records than anyone actually wants, and the
//! wantlist is the wrong place to park them — it's a synced, curated list of
//! records you've committed to hunting. The crate sits between: one click
//! (☆ on a record sheet, or a card's context menu) drops a find here with the
//! thread that found it, and triage happens later with fresh ears. Purely
//! local, never synced to Discogs. Promoting a record to the wantlist takes
//! it out of the crate; so does deciding against it.
//!
//! Records here are excluded from future dig finds the same way the shelves
//! are — a find already set aside isn't a discovery twice.

use super::*;

/// What a card click or context-menu pick asked for, applied after the grid
/// releases its borrows.
enum InterestAct {
    /// Open the record sheet — listen before deciding.
    Open(usize),
    /// Start a crate dig from this record.
    Dig(usize),
    /// Promote to the Discogs wantlist (and take it out of the crate —
    /// decided).
    Want(usize),
    /// Decided against: drop it from the crate.
    Remove(usize),
}

/// Does a crated record match the vinyl view's search box? Same contract as
/// the shelves: every whitespace-separated term must appear somewhere.
pub(crate) fn interest_matches(r: &InterestRecord, query: &str) -> bool {
    let hay = format!(
        "{} {} {} {} {} {}",
        r.artist,
        r.title,
        r.label.as_deref().unwrap_or(""),
        r.year.map(|y| y.to_string()).unwrap_or_default(),
        r.format.as_deref().unwrap_or(""),
        r.via.as_deref().unwrap_or(""),
    )
    .to_lowercase();
    query.split_whitespace().all(|term| hay.contains(term))
}

/// The caption line under a crated cover, e.g. `1994 · 12"`.
fn interest_sub(r: &InterestRecord) -> String {
    match (r.year, r.format.as_deref()) {
        (Some(y), Some(f)) => format!("{y} · {f}"),
        (Some(y), None) => y.to_string(),
        (None, Some(f)) => f.to_string(),
        (None, None) => String::new(),
    }
}

impl App {
    /// Put a record in the crate (or refresh it there), reload the list, and
    /// say so in the status line.
    pub(crate) fn crate_record(&mut self, rec: InterestRecord) {
        let label = format!("{} — {}", rec.artist, rec.title);
        match Catalog::open(&self.db_path).and_then(|c| {
            c.add_interest(&rec)?;
            c.list_interest()
        }) {
            Ok(list) => {
                self.interest_ids = list.iter().map(|r| r.release_id).collect();
                self.interest = list;
                self.status = format!("Set aside in the crate: {label}");
            }
            Err(e) => self.status = format!("Couldn't crate the record: {e}"),
        }
    }

    /// Take a record out of the crate — decided, either way.
    pub(crate) fn uncrate_record(&mut self, release_id: u64) {
        match Catalog::open(&self.db_path).and_then(|c| {
            c.remove_interest(release_id)?;
            c.list_interest()
        }) {
            Ok(list) => {
                self.interest_ids = list.iter().map(|r| r.release_id).collect();
                self.interest = list;
            }
            Err(e) => self.status = format!("Couldn't update the crate: {e}"),
        }
    }

    /// The Crate tab body: the dig strip, then the set-aside records as a
    /// cover grid, newest find first. `query` is the shared vinyl search,
    /// already trimmed and lowercased.
    pub(crate) fn draw_interest(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, query: &str) {
        // The dig strip works from the crate too — triage often means pulling
        // one more thread before deciding. Same placement contract as the
        // other tabs: above the scrolling grid.
        if let Some(o) = self.draw_dig(ui) {
            self.open_release_sheet(o.release_id, o.artist, o.title, o.sub, o.cover_url, ctx);
        }
        if self.dig.is_some() {
            ui.add_space(8.0);
        }

        if self.interest.is_empty() {
            ui.add_space(40.0);
            ui.vertical_centered(|ui| {
                ui.heading("Nothing set aside yet");
                ui.add_space(6.0);
                ui.label("The crate holds records you dug up but haven't decided on.");
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "While digging, hit ☆ Crate on a record's sheet (or a card's \
                         right-click menu) to park a find here — the wantlist stays \
                         for records you've committed to.",
                    )
                    .weak(),
                );
            });
            return;
        }

        let filtered: Vec<usize> = self
            .interest
            .iter()
            .enumerate()
            .filter(|(_, r)| query.is_empty() || interest_matches(r, query))
            .map(|(i, _)| i)
            .collect();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            let meta = if query.is_empty() {
                match self.interest.len() {
                    1 => "1 record set aside, newest find first".to_string(),
                    n => format!("{n} records set aside, newest find first"),
                }
            } else {
                format!(
                    "{} of {} set-aside records match",
                    filtered.len(),
                    self.interest.len()
                )
            };
            ui.label(egui::RichText::new(meta).weak());
        });
        ui.add_space(4.0);
        if filtered.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("Nothing in the crate matches that search.").weak());
            });
            return;
        }

        // Same virtualized cover grid the seller crates use, with the third
        // caption line naming the thread that found the record.
        const GAP: f32 = 14.0;
        const MIN_COVER: f32 = 132.0;
        const MAX_COVER: f32 = 170.0;
        const CAPTION_H: f32 = 58.0;
        let avail = ui.available_width();
        let cols = (((avail + GAP) / (MIN_COVER + GAP)).floor().max(1.0)) as usize;
        let cover_side = ((avail - GAP * (cols as f32 - 1.0)) / cols as f32)
            .floor()
            .clamp(MIN_COVER.min(avail.max(1.0)), MAX_COVER);
        let row_h = cover_side + CAPTION_H + GAP;
        let n_rows = filtered.len().div_ceil(cols);

        let mut act: Option<InterestAct> = None;
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
                            if let Some(a) = self.interest_card(ui, idx, cover_side) {
                                act = Some(a);
                            }
                        }
                    });
                }
                ui.add_space(8.0);
            });

        match act {
            Some(InterestAct::Open(idx)) => {
                if let Some(r) = self.interest.get(idx).cloned() {
                    let sub = interest_sub(&r);
                    self.open_release_sheet(r.release_id, r.artist, r.title, sub, r.thumb_url, ctx);
                }
            }
            Some(InterestAct::Dig(idx)) => {
                if let Some(r) = self.interest.get(idx).cloned() {
                    let sub = interest_sub(&r);
                    self.start_dig_release(
                        r.release_id,
                        r.artist,
                        r.title,
                        r.label,
                        sub,
                        r.thumb_url,
                    );
                }
            }
            Some(InterestAct::Want(idx)) => {
                if let Some(r) = self.interest.get(idx).cloned() {
                    // Promotion is the decision — the crate's copy goes with
                    // it. The wantlist add itself runs through the ordinary
                    // confirm-gated edit path.
                    self.uncrate_record(r.release_id);
                    let edit = VinylEdit::Want {
                        release_ids: vec![r.release_id],
                        label: format!("{} — {}", r.artist, r.title),
                    };
                    self.request_vinyl_edit(ctx.clone(), edit);
                }
            }
            Some(InterestAct::Remove(idx)) => {
                if let Some(r) = self.interest.get(idx).cloned() {
                    self.uncrate_record(r.release_id);
                    self.status = format!("Out of the crate: {} — {}", r.artist, r.title);
                }
            }
            None => {}
        }
    }

    /// One card of the crate: cover, credit, and the thread that found it.
    fn interest_card(
        &mut self,
        ui: &mut egui::Ui,
        idx: usize,
        cover_side: f32,
    ) -> Option<InterestAct> {
        use crate::ui::tokens::color;

        // Snapshot before `dig_cover` needs `self` mutably.
        let (release_id, artist, title, sub, via, thumb, owned, wanted, viewed) = {
            let r = self.interest.get(idx)?;
            (
                r.release_id,
                r.artist.clone(),
                r.title.clone(),
                interest_sub(r),
                r.via.clone(),
                r.thumb_url.clone(),
                self.vinyl_owned.contains(&r.release_id),
                self.vinyl_wanted.contains(&r.release_id),
                self.viewed_releases.contains(&r.release_id),
            )
        };
        let tex = thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());

        let mut act: Option<InterestAct> = None;
        ui.allocate_ui_with_layout(
            egui::vec2(cover_side, cover_side + 58.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                let (rect, resp) = ui
                    .allocate_exact_size(egui::vec2(cover_side, cover_side), egui::Sense::click());
                let resp = resp
                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                    .on_hover_note("Open the record — listen, then decide");
                // Dig disc hit area, claimed before painting (same reasoning
                // as the shelf and seller grids).
                const D: f32 = 30.0;
                let dig_rect = egui::Rect::from_min_size(
                    egui::pos2(rect.right() - D - 6.0, rect.bottom() - D - 6.0),
                    egui::vec2(D, D),
                );
                let dig_hit = ui.interact(
                    dig_rect,
                    ui.id().with(("interest-dig", release_id)),
                    egui::Sense::click(),
                );
                let dig_hovered = dig_hit.hovered();
                let card_hovered = resp.hovered() || dig_hovered;
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
                            "☆",
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
                // Membership chips: a crated record you've since shelved or
                // wantlisted is a decision already made elsewhere — flag it so
                // the crate can be tidied.
                let mut chip_x = rect.left() + 5.0;
                for (show, text, fill) in [
                    (owned, "OWNED", egui::Color32::from_rgb(40, 120, 70)),
                    (
                        !owned && wanted,
                        "WANT",
                        egui::Color32::from_rgb(120, 90, 30),
                    ),
                ] {
                    if !show {
                        continue;
                    }
                    let font = egui::FontId::proportional(10.0);
                    let galley =
                        ui.painter()
                            .layout_no_wrap(text.into(), font, egui::Color32::WHITE);
                    let pad = egui::vec2(5.0, 3.0);
                    let chip = egui::Rect::from_min_size(
                        egui::pos2(chip_x, rect.top() + 5.0),
                        galley.size() + pad * 2.0,
                    );
                    ui.painter()
                        .rect_filled(chip, egui::Rounding::same(4.0), fill);
                    ui.painter()
                        .galley(chip.min + pad, galley, egui::Color32::WHITE);
                    chip_x = chip.right() + 4.0;
                }
                // Viewed eye, top-right — already auditioned.
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
                // Dig disc, hover-revealed.
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
                        .on_hover_note("Dig onward from this record")
                        .clicked()
                    {
                        dig_clicked = true;
                        act = Some(InterestAct::Dig(idx));
                    }
                }
                if resp.clicked() && !dig_clicked {
                    act = Some(InterestAct::Open(idx));
                }
                resp.context_menu(|ui| {
                    if ui
                        .button("＋ Move to wantlist")
                        .on_hover_note("Commit to it: add to your Discogs wantlist and clear it from the crate")
                        .clicked()
                    {
                        act = Some(InterestAct::Want(idx));
                        ui.close_menu();
                    }
                    if ui
                        .button("✖ Remove from crate")
                        .on_hover_note("Decided against it")
                        .clicked()
                    {
                        act = Some(InterestAct::Remove(idx));
                        ui.close_menu();
                    }
                    ui.separator();
                    if ui.button("↗ Open on Discogs").clicked() {
                        open_url(&format!("https://www.discogs.com/release/{release_id}"));
                        ui.close_menu();
                    }
                });
                ui.add_space(4.0);
                ui.set_max_width(cover_side);
                ui.spacing_mut().item_spacing.y = 1.0;
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&title).font(crate::ui::tokens::font::footnote()),
                    )
                    .truncate(),
                );
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(&artist)
                            .font(crate::ui::tokens::font::footnote())
                            .weak(),
                    )
                    .truncate(),
                );
                // Third line: the thread that found it, or the year/format
                // when the trail wasn't recorded.
                let trail = via.unwrap_or(sub);
                if !trail.is_empty() {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(trail)
                                .font(crate::ui::tokens::font::caption())
                                .color(egui::Color32::from_gray(125)),
                        )
                        .truncate(),
                    );
                }
            },
        );
        act
    }
}
