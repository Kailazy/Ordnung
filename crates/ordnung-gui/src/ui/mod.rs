//! Ordnung's visual component library.
//!
//! [`tokens`] holds the design tokens (colours, radii, spacing, type ramp) and
//! [`theme`] pushes them into egui's global style. The components sit beside
//! them: [`button`] is the one push button, [`field`] the one text box,
//! [`chip`] the avatar pill, [`menu`] the dropdown a button anchors,
//! [`window`] the one floating window, [`sidebar`] the window docked to an
//! edge, [`nav`] the docked panel that snaps between designed widths,
//! [`tooltip`] the hover note, [`glass`] the surface they all sit on,
//! [`control_row`] the rule that a row of controls shares one height.

pub mod button;
pub mod chip;
pub mod fader;
pub mod field;
pub mod frost;
pub mod glass;
pub mod hover;
pub mod icon;
pub mod knob;
pub mod menu;
pub mod nav;
pub mod phosphor_icons;
pub mod sheet;
pub mod sidebar;
pub mod theme;
// Tokens are an intentionally ahead-of-use palette: Pass 1 wires only a subset
// into the global style; the rest are consumed as call sites migrate off inline
// literals. Allow the interim dead-code until that pass lands.
#[allow(dead_code)]
pub mod tokens;
pub mod tooltip;
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

/// A label that leads with an icon: the glyph from the app's icon face, a
/// gap, then the text in `font`, both centred on one line. For a button or
/// a menu item whose label carries a mark ("Analyze" behind its bolt), so
/// the mark is set in the icon face rather than left to the text chain,
/// where Inter's private-use glyphs would claim some of the codepoints.
/// Colours are left to the widget, so the label still answers hover.
pub fn icon_text(glyph: &str, text: &str, font: egui::FontId) -> egui::WidgetText {
    let mut job = egui::text::LayoutJob::default();
    job.append(
        glyph,
        0.0,
        egui::TextFormat {
            font_id: tokens::font::icon(font.size + 2.0),
            color: egui::Color32::PLACEHOLDER,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.append(
        text,
        tokens::space::S3,
        egui::TextFormat {
            font_id: font,
            color: egui::Color32::PLACEHOLDER,
            valign: egui::Align::Center,
            ..Default::default()
        },
    );
    job.into()
}

/// The chip that floats at the pointer while something is dragged inside
/// the app ("3 track(s)", "1 song"), so it's clear something is being
/// carried toward a drop target. Painted in the tooltip layer, above all.
pub fn drag_chip(ctx: &egui::Context, text: String) {
    let Some(pos) = ctx.pointer_interact_pos() else {
        return;
    };
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        egui::Id::new("drag-preview"),
    ));
    let at = pos + egui::vec2(14.0, 6.0);
    let galley = painter.layout_no_wrap(text, tokens::font::callout(), egui::Color32::WHITE);
    let pad = egui::vec2(6.0, 3.0);
    let rect = egui::Rect::from_min_size(at, galley.size() + pad * 2.0);
    painter.rect_filled(
        rect,
        egui::Rounding::same(4.0),
        egui::Color32::from_rgb(60, 110, 170),
    );
    painter.galley(at + pad, galley, egui::Color32::WHITE);
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
