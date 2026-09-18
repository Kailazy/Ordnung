//! Ordnung's visual component library.
//!
//! [`tokens`] holds the design tokens (colours, radii, spacing, type ramp) and
//! [`theme`] pushes them into egui's global style. The components sit beside
//! them: [`button`] is the one push button, [`field`] the one text box,
//! [`chip`] the avatar pill, [`menu`] the dropdown a button anchors,
//! [`window`] the one floating window, [`sidebar`] the window docked to an
//! edge, [`glass`] the surface they all sit on, [`control_row`] the rule
//! that a row of controls shares one height.

pub mod button;
pub mod chip;
pub mod field;
pub mod frost;
pub mod glass;
pub mod hover;
pub mod icon;
pub mod knob;
pub mod menu;
pub mod phosphor_icons;
pub mod sheet;
pub mod sidebar;
pub mod theme;
// Tokens are an intentionally ahead-of-use palette: Pass 1 wires only a subset
// into the global style; the rest are consumed as call sites migrate off inline
// literals. Allow the interim dead-code until that pass lands.
#[allow(dead_code)]
pub mod tokens;
pub mod window;

use eframe::egui;

/// The one height every control on a row shares: a single-line text field at
/// the body size, [`field::MARGIN`] included, which is 32 pt. The field is
/// the reference because it is the control whose height is least free to
/// move (its text must not clip); the buttons and pickers beside it are
/// brought to this through [`control_row`]. The text height is rounded the
/// way egui rounds a laid-out line, so the number is a whole point.
pub fn control_h(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Body).round() + field::MARGIN.sum().y
}

/// Lay out a row of controls at one height. Every `Button`, `ComboBox`,
/// glyph and single-line `TextEdit` added inside comes out [`control_h`]
/// tall, so a row never shows three neighbouring controls at three heights.
/// A push button on its own is shorter than a field; here its padding grows
/// so it meets the field. Use the plain `ui.button` inside, not
/// `small_button`: the small variant sets its own text size and would
/// break the line it sits on.
pub fn control_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let h = control_h(ui);
    // Rounded like the line egui lays out, or the padding would carry the
    // fraction and the button land a hair past `h`.
    let text_h = ui.text_style_height(&egui::TextStyle::Button).round();
    ui.scope(|ui| {
        let s = ui.spacing_mut();
        s.interact_size.y = h;
        s.button_padding.y = ((h - text_h) / 2.0).max(0.0);
        add(ui)
    })
    .inner
}
