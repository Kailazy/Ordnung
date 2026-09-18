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
//! Buttons that share a row with a text field or a picker go inside
//! [`super::control_row`], which brings all of them to the field's height.

use eframe::egui;

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
