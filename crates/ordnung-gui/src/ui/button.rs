//! The app's button.
//!
//! One shape for every push button, so a row of them reads as one family and
//! a button on its own reads as a button: the body type, the theme's 10 × 6
//! padding (see `theme`), and the standard interact height. egui's
//! `small_button` is not used anywhere in the app: it zeroes the vertical
//! padding, which leaves the label pressed against the frame's top and
//! bottom, and it sets its own type size, which breaks the line it sits on.
//! Where a smaller control seems called for, the answer is this button with
//! a shorter label, not a smaller button.
//!
//! The one control that is smaller is [`inline`], and it isn't a smaller
//! push button: it's a word that sits on a line of text and acts. It takes
//! the line's height, so the line stays a line, and it never goes on a row
//! of controls.
//!
//! Buttons that share a row with a text field or a picker go inside
//! [`super::control_row`], which brings all of them to the field's height.

use eframe::egui;

use super::tokens::{color, font};

/// A push button. Same as `ui.button`, named so call sites reach for the
/// one component rather than choosing between egui's variants.
pub fn button(ui: &mut egui::Ui, label: impl Into<egui::WidgetText>) -> egui::Response {
    ui.add(egui::Button::new(label))
}

/// A push button that can be greyed out. Disabled, it still takes its space
/// and its label, so the row doesn't reflow when it comes back.
pub fn button_enabled(
    ui: &mut egui::Ui,
    enabled: bool,
    label: impl Into<egui::WidgetText>,
) -> egui::Response {
    ui.add_enabled(enabled, egui::Button::new(label))
}

/// The anchor of a [`super::menu::dropdown`]: a [`button`] whose label ends
/// in a chevron, so it says before the click that it opens a list rather
/// than doing something.
pub fn menu_button(ui: &mut egui::Ui, label: impl Into<String>) -> egui::Response {
    button(ui, format!("{} ▾", label.into()))
}

/// A control on a line of text: a footnote-sized word in the secondary label
/// colour, with a faint pill behind it only while the pointer is on it. For
/// an action that belongs to the fact beside it (the versions of the pressing
/// the line names) rather than to the record as a whole, which would go on the
/// row of buttons. It comes out the height of the line, not of a button, so
/// only ever put it on a line of text, never in a row of controls.
pub fn inline(ui: &mut egui::Ui, label: &str) -> egui::Response {
    let pad = egui::vec2(6.0, 1.0);
    let galley = ui.painter().layout_no_wrap(
        label.to_owned(),
        font::footnote(),
        color::LABEL_2,
    );
    let (rect, resp) = ui.allocate_exact_size(galley.size() + 2.0 * pad, egui::Sense::click());
    if ui.is_rect_visible(rect) {
        let visuals = ui.style().interact(&resp);
        if resp.hovered() || resp.is_pointer_button_down_on() {
            ui.painter().rect(
                rect,
                egui::Rounding::same(rect.height() / 2.0),
                visuals.weak_bg_fill,
                visuals.bg_stroke,
            );
        }
        let col = if resp.hovered() { visuals.fg_stroke.color } else { color::LABEL_2 };
        ui.painter()
            .galley(rect.min + pad, galley, col);
    }
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
