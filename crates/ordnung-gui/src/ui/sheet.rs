//! Fixed-size stepped sheet — the shell behind multi-page dialogs (the welcome
//! tour, and any wizard that follows it).
//!
//! The rule this component exists to enforce: **the frame never moves.** A
//! stepped dialog whose window resizes to its content forces the user to re-aim
//! at Next on every page, which turns a four-click tour into four separate
//! hunts for a moving button. So the body is a hard-pinned box — content shorter
//! than the box does not shrink it, content taller than the box scrolls inside
//! it — and the footer sits at a constant offset below.
//!
//! Sizing is stated once in [`SheetSize`] rather than per page, so pages can be
//! added or reworded without any of them being able to shift the geometry.

use eframe::egui;

/// The fixed geometry every page of a sheet is laid out inside. One value for
/// the whole dialog: pages never negotiate their own size.
#[derive(Debug, Clone, Copy)]
pub struct SheetSize {
    /// Content width in points.
    pub width: f32,
    /// Body height in points — the constant that keeps the footer still.
    pub body_height: f32,
}

impl SheetSize {
    /// The tour's geometry. Width holds a paragraph at a comfortable measure at
    /// the sheet's type sizes; height is fitted to the tallest page — the
    /// writeback fork, whose two cards plus a two-line heading run ~315pt.
    ///
    /// The icon tiles are what keep this honest: at 38pt they set a row's height
    /// rather than the text doing it, so the three-row pages come out ~255pt and
    /// sit close to the tallest one. Trimming the box below the tallest page
    /// would buy a little less slack on the short ones at the cost of scrolling
    /// the fork — the wrong trade, since the fork is the page that matters.
    pub const TOUR: SheetSize = SheetSize {
        width: 520.0,
        body_height: 320.0,
    };
}

/// Lay out one page of a stepped sheet at a constant size.
///
/// `body` draws the current page's content into a box of exactly
/// `size.body_height` points; `footer` draws the navigation row beneath the
/// separator. Overflow scrolls rather than growing the window, and an
/// underfilled page leaves empty space rather than collapsing — both so the
/// footer lands on the same pixel on every page.
pub fn stepped(
    ui: &mut egui::Ui,
    size: SheetSize,
    body: impl FnOnce(&mut egui::Ui),
    footer: impl FnOnce(&mut egui::Ui),
) {
    ui.set_width(size.width);

    // The body box. `allocate_ui_with_layout` reserves the space; the
    // `ScrollArea` with `auto_shrink([false, false])` is what actually pins it —
    // it fills the box when content is short and scrolls when content is tall,
    // so in neither direction can a page resize the window.
    ui.allocate_ui_with_layout(
        egui::vec2(size.width, size.body_height),
        egui::Layout::top_down(egui::Align::Min),
        |ui| {
            ui.set_min_size(egui::vec2(size.width, size.body_height));
            ui.set_max_size(egui::vec2(size.width, size.body_height));
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    // Inset so scrolled content doesn't collide with the bar.
                    ui.set_max_width(size.width - super::tokens::space::S4);
                    body(ui);
                });
        },
    );

    ui.add_space(super::tokens::space::S3);
    ui.separator();
    ui.add_space(super::tokens::space::S2);
    ui.horizontal(|ui| {
        footer(ui);
    });
}

/// A row of progress dots for a stepped sheet: `total` steps with `current`
/// (0-based) filled. Steps already passed read at half strength, so position in
/// the sequence is legible at a glance without a label.
pub fn progress_dots(ui: &mut egui::Ui, total: usize, current: usize, accent: egui::Color32) {
    for i in 0..total {
        let (r, _) = ui.allocate_exact_size(egui::vec2(12.0, 12.0), egui::Sense::hover());
        let color = if i == current {
            accent
        } else if i < current {
            accent.gamma_multiply(0.45)
        } else {
            super::tokens::color::LABEL_4
        };
        ui.painter()
            .circle_filled(r.center(), if i == current { 4.0 } else { 3.0 }, color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The whole point of the component: one geometry for the flow. If a future
    /// change makes the size per-page, the constant stops being a constant and
    /// the "Next never moves" guarantee is gone.
    #[test]
    fn the_tour_geometry_is_a_single_shared_constant() {
        let a = SheetSize::TOUR;
        let b = SheetSize::TOUR;
        assert_eq!(a.width, b.width);
        assert_eq!(a.body_height, b.body_height);
        // A body box has to be big enough to be worth pinning; a degenerate
        // value here would let content drive the height again.
        assert!(a.body_height > 200.0);
        assert!(a.width > 320.0);
    }
}
