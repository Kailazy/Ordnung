//! Ordnung's visual component library.
//!
//! [`tokens`] holds the design tokens (colours, radii, spacing, type ramp) and
//! [`theme`] pushes them into egui's global style. Bespoke component helpers will
//! live alongside these in a later pass.

pub mod frost;
pub mod hover;
pub mod icon;
pub mod knob;
pub mod menu;
pub mod phosphor_icons;
pub mod sheet;
pub mod theme;
// Tokens are an intentionally ahead-of-use palette: Pass 1 wires only a subset
// into the global style; the rest are consumed as call sites migrate off inline
// literals. Allow the interim dead-code until that pass lands.
#[allow(dead_code)]
pub mod tokens;

use eframe::egui;

/// The one height every control on a row shares: a single-line text field at
/// the body size, egui's default 2 pt vertical text margin included. The field
/// is the reference because it is the control whose height is least free to
/// move (its text must not clip); buttons and pickers beside it are brought to
/// this through [`control_row`].
pub fn control_h(ui: &egui::Ui) -> f32 {
    ui.text_style_height(&egui::TextStyle::Body) + 2.0 * 2.0
}

/// Lay out a row of controls at one height. Every `Button`, `ComboBox` and
/// single-line `TextEdit` added inside comes out [`control_h`] tall, so a row
/// never shows three neighbouring controls at three heights. Use the plain
/// `ui.button` inside, not `small_button`: the small variant sets its own
/// text size and would break the line it sits on.
pub fn control_row<R>(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> R) -> R {
    let h = control_h(ui);
    let text_h = ui.text_style_height(&egui::TextStyle::Button);
    ui.scope(|ui| {
        let s = ui.spacing_mut();
        s.interact_size.y = h;
        s.button_padding.y = ((h - text_h) / 2.0).max(0.0);
        add(ui)
    })
    .inner
}
