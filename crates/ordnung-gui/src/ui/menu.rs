//! Animated dropdown menu — Ordnung's popover for pickers and small action
//! menus, replacing egui's stock `menu_button` where the chrome deserves the
//! app's own motion language.
//!
//! The motion mirrors the search suggestion popup so every transient surface in
//! the app moves the same way: the panel rises its last few points into place
//! while fading in (cubic-out, reversed on close), and rows cascade top-down on
//! a short stagger instead of appearing all at once. Hover highlights ease in
//! and out rather than snapping. All of it is driven by egui's animation clock,
//! so a dismissed menu fades away from wherever its open animation had reached.
//!
//! Usage: draw any anchor widget, then attach the menu to its response.
//!
//! ```ignore
//! let btn = ui.button("Sort");
//! menu::dropdown(&btn, 170.0, |m| {
//!     if m.selectable(current == Sort::Artist, "Artist") {
//!         current = Sort::Artist;
//!         m.close();
//!     }
//!     m.separator();
//!     if m.item("Reset") { /* … */ }
//! });
//! ```
//!
//! Clicking the anchor toggles the menu; Esc or a click anywhere else dismisses
//! it. Rows that should dismiss on selection call [`MenuUi::close`] — rows that
//! toggle (multi-select filters) simply don't, and the menu stays up.

use eframe::egui;

use super::tokens::{color, font, radius, space};

/// Open/close transition length, matched to the search popup's `POPUP_ANIM`.
const ANIM: f32 = 0.14;
/// How far the panel rises into place while fading in.
const RISE: f32 = 6.0;
/// Gap between the anchor's bottom edge and the panel.
const GAP: f32 = 4.0;
/// One row's entrance fade/slide length.
const ROW_ANIM: f32 = 0.16;
/// Delay between successive rows' entrances — the cascade.
const ROW_STAGGER: f32 = 0.02;
/// Hover highlight ease, matched to the app's row highlights.
const HOVER_ANIM: f32 = 0.11;
/// Height of one menu row.
const ROW_H: f32 = 26.0;
/// Width reserved for the ✓ column in selectable rows, so labels align whether
/// checked or not (the Apple menu convention).
const CHECK_W: f32 = 18.0;
/// Horizontal text inset inside a row.
const ROW_PAD: f32 = space::S3;

/// Attach an animated dropdown to `anchor`. Clicking the anchor toggles it;
/// `min_width` keeps a sparse menu from collapsing to its longest label.
pub fn dropdown(anchor: &egui::Response, min_width: f32, add: impl FnOnce(&mut MenuUi)) {
    let ctx = anchor.ctx.clone();
    let id = anchor.id.with("ord_dropdown");
    let mut open: bool = ctx.data(|d| d.get_temp(id).unwrap_or(false));
    let now = ctx.input(|i| i.time);
    if anchor.clicked() {
        open = !open;
        if open {
            // Stamp the open moment: every row times its entrance off this.
            ctx.data_mut(|d| d.insert_temp(id.with("at"), now));
        }
    }

    // How far up the panel is, 0 → 1. Drawing continues past a `false` in
    // `open` until the fade-out settles, so dismissing is as smooth as opening.
    let open_t = ctx.animate_bool_with_time_and_easing(
        id.with("t"),
        open,
        ANIM,
        egui::emath::easing::cubic_out,
    );
    if open_t <= 0.0 {
        ctx.data_mut(|d| d.insert_temp(id, open));
        return;
    }

    let opened_at: f64 = ctx.data(|d| d.get_temp(id.with("at")).unwrap_or(now));
    let since_open = (now - opened_at) as f32;
    // Dismissal context, sampled before the content draws so this frame's own
    // interactions can't muddy it: a focused text field means Esc is aimed at
    // the field (unfocus), not the menu; an open popup (a combo box inside the
    // menu) means a click on one of its options — which lands outside this
    // Area — must not read as click-away.
    let field_focused = ctx.memory(|m| m.focused().is_some());
    let nested_popup_open = ctx.memory(|m| m.any_popup_open());
    // Rise and fade share the one `open_t` so the two halves of the motion can
    // never drift apart.
    let rise = RISE * (1.0 - open_t);

    let mut rows = 0usize;
    let mut want_close = false;
    let area = egui::Area::new(id.with("area"))
        .order(egui::Order::Foreground)
        // Non-interactive until fully materialised, so a click during the fade
        // lands on whatever the user aimed at rather than on a ghost row.
        .interactable(open && open_t > 0.99)
        .fixed_pos(anchor.rect.left_bottom() + egui::vec2(0.0, GAP - rise))
        .constrain(true)
        .show(&ctx, |ui| {
            ui.multiply_opacity(open_t);
            egui::Frame::none()
                .fill(color::SURFACE_HI)
                .stroke(egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE))
                .rounding(egui::Rounding::same(radius::MD))
                .inner_margin(egui::Margin::symmetric(space::S2, space::S2))
                .shadow(egui::epaint::Shadow {
                    offset: egui::vec2(0.0, 6.0),
                    blur: 22.0,
                    spread: 0.0,
                    color: egui::Color32::from_black_alpha(90),
                })
                .show(ui, |ui| {
                    ui.set_min_width(min_width);
                    let mut m = MenuUi {
                        ui,
                        since_open,
                        row: 0,
                        close: false,
                    };
                    add(&mut m);
                    rows = m.row;
                    want_close = m.close;
                });
        });

    if want_close {
        open = false;
    }
    if open {
        if !field_focused && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            open = false;
        } else if !anchor.clicked() && !nested_popup_open && area.response.clicked_elsewhere() {
            // The anchor guard matters on the frame the menu opens: that click
            // is outside the area too, and without it the menu would dismiss
            // itself the moment it was summoned.
            open = false;
        }
    }
    // The box's own open animation drives repaints while it runs; the row
    // cascade outlives it slightly, so keep frames coming until the last row
    // has fully entered.
    if open && since_open < ROW_ANIM + ROW_STAGGER * rows as f32 {
        ctx.request_repaint();
    }
    ctx.data_mut(|d| d.insert_temp(id, open));
}

/// The builder handed to a [`dropdown`]'s content closure. Rows are drawn in
/// call order and each times its entrance off its position in that order.
pub struct MenuUi<'u> {
    ui: &'u mut egui::Ui,
    since_open: f32,
    row: usize,
    close: bool,
}

// Like `tokens`, this is an intentionally ahead-of-use API: the first adopter
// (the vinyl sort menu) exercises `selectable`/`separator`/`close`; the rest of
// the surface exists for the call sites that migrate next. Allow the interim
// dead-code until they do.
#[allow(dead_code)]
impl MenuUi<'_> {
    /// A plain action row. Returns true on click.
    pub fn item(&mut self, label: impl Into<String>) -> bool {
        self.row(None, label.into(), color::LABEL)
    }

    /// An action row in the destructive red, for deletes and their kin.
    pub fn item_danger(&mut self, label: impl Into<String>) -> bool {
        self.row(None, label.into(), color::RED)
    }

    /// A row with a ✓ column — for single- or multi-select lists. All
    /// selectable rows share the column so their labels align. Returns true on
    /// click; the caller decides whether that selects, toggles, or closes.
    pub fn selectable(&mut self, selected: bool, label: impl Into<String>) -> bool {
        self.row(Some(selected), label.into(), color::LABEL)
    }

    /// A section caption — small, semibold, quiet — for a menu that groups
    /// rows under headings (facet pickers). Aligned to the row text inset.
    pub fn header(&mut self, text: impl Into<String>) {
        self.ui.add_space(space::S2);
        self.ui.horizontal(|ui| {
            ui.add_space(ROW_PAD);
            ui.label(
                egui::RichText::new(text.into())
                    .font(font::strong(font::footnote().size))
                    .color(color::LABEL_3),
            );
        });
    }

    /// A capped-height scrollable run of rows for long lists (style tags).
    /// Rows inside keep their place in the entrance cascade, and a `close()`
    /// from within still dismisses the menu.
    pub fn scroll(&mut self, max_height: f32, add: impl FnOnce(&mut MenuUi)) {
        let since_open = self.since_open;
        let mut row = self.row;
        let mut close = false;
        egui::ScrollArea::vertical()
            .max_height(max_height)
            .show(self.ui, |ui| {
                let mut m = MenuUi {
                    ui,
                    since_open,
                    row,
                    close: false,
                };
                add(&mut m);
                row = m.row;
                close = m.close;
            });
        self.row = row;
        self.close |= close;
    }

    /// A hairline between row groups, inset from the panel edges.
    pub fn separator(&mut self) {
        let w = self.ui.available_width();
        let (rect, _) = self
            .ui
            .allocate_exact_size(egui::vec2(w, space::S3), egui::Sense::hover());
        let y = rect.center().y;
        self.ui.painter().hline(
            (rect.left() + space::S2)..=(rect.right() - space::S2),
            y,
            egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
        );
    }

    /// A quiet caption row — coverage notes, hints. Not interactive.
    pub fn note(&mut self, text: impl Into<String>) {
        self.ui.add_space(space::S1);
        self.ui.horizontal(|ui| {
            ui.add_space(ROW_PAD);
            ui.label(
                egui::RichText::new(text.into())
                    .font(font::footnote())
                    .color(color::LABEL_3),
            );
        });
        self.ui.add_space(space::S1);
    }

    /// Dismiss the menu after this frame — call on the click that should end
    /// the interaction. Toggling rows simply don't, and the menu stays up.
    pub fn close(&mut self) {
        self.close = true;
    }

    /// Escape hatch to the raw [`egui::Ui`] for bespoke content (scroll areas,
    /// buttons). Such content takes no part in the row cascade.
    pub fn ui(&mut self) -> &mut egui::Ui {
        self.ui
    }

    /// One row: animated hover fill, staggered fade/slide entrance, optional
    /// ✓ column, label in `text_color`.
    fn row(&mut self, selected: Option<bool>, label: String, text_color: egui::Color32) -> bool {
        let i = self.row;
        self.row += 1;
        // This row's entrance, 0 → 1: rows trail each other by `ROW_STAGGER`
        // so the list unrolls top-down instead of appearing at once.
        let t = ((self.since_open - ROW_STAGGER * i as f32) / ROW_ANIM).clamp(0.0, 1.0);
        let enter = egui::emath::easing::cubic_out(t);

        let w = self.ui.available_width();
        let (rect, resp) = self
            .ui
            .allocate_exact_size(egui::vec2(w, ROW_H), egui::Sense::click());
        if !self.ui.is_rect_visible(rect) {
            return resp.clicked();
        }

        let hot = self
            .ui
            .ctx()
            .animate_bool_with_time(resp.id.with("hot"), resp.hovered(), HOVER_ANIM);
        let painter = self.ui.painter();
        if hot > 0.0 {
            let fill = if resp.is_pointer_button_down_on() {
                color::SURFACE_ACTIVE
            } else {
                color::SURFACE_HOVER
            };
            painter.rect_filled(
                rect,
                egui::Rounding::same(radius::SM),
                fill.gamma_multiply(hot),
            );
        }

        // Entrance: fade the row in while it slides its last few points left
        // into place. Subtle by design — the slide is 5pt over 160 ms.
        let slide = (1.0 - enter) * 5.0;
        let mut x = rect.left() + ROW_PAD + slide;
        if selected == Some(true) {
            painter.text(
                egui::pos2(x, rect.center().y),
                egui::Align2::LEFT_CENTER,
                "✓",
                font::body(),
                color::ACCENT.gamma_multiply(enter),
            );
        }
        if selected.is_some() {
            x += CHECK_W;
        }
        painter.text(
            egui::pos2(x, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font::body(),
            text_color.gamma_multiply(enter),
        );
        resp.clicked()
    }
}
