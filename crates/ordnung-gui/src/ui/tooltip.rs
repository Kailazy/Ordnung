//! Tooltip: the one hover note, on glass.
//!
//! egui's tooltip is a popup frame filled from the style, an opaque grey
//! card beside every glass surface in the app. This puts the note on the
//! same material as a window or a menu: the app blurred under it (a `frost`
//! snapshot) and one tint over that, with the window's hairline as its
//! edge. Everything else about the tooltip stays egui's: when it shows (the
//! pointer resting for the delay, the grace between neighbours), where it
//! goes, and that one with controls in it stays while the pointer is on it.
//!
//! The frame is egui's own, made transparent for the call: its fill, stroke
//! and shadow are switched off in the style while the tooltip is shown and
//! put back after, and the glass is painted in their place under the note.
//! The swap is only made when the note can show at all (the pointer is on
//! the widget, or the note was up last pass), so the hundreds of notes a
//! frame declares cost nothing.
//!
//! One frost serves every tooltip. Hovering from one widget to the next
//! keeps the surface up, so the snapshot is taken once per hover spell
//! rather than per widget; the first note of a spell sits a pass or two
//! out while it is taken, as a window does. Use through
//! [`super::hover::HoverNoteExt`], which styles the text; only a note with
//! its own layout calls [`on_hover_ui`] directly.

use super::glass;
use super::tokens::color;
use eframe::egui;

/// The one glass entry every tooltip shares (see the module docs).
fn glass_id() -> egui::Id {
    egui::Id::new("ordnung-tooltip")
}

/// The tooltip's edge: the window's hairline.
fn stroke() -> egui::Stroke {
    egui::Stroke::new(1.0, color::SEPARATOR)
}

/// How the note is triggered; egui's three hover rules.
#[derive(Clone, Copy)]
enum Trigger {
    /// Beside the widget while the pointer rests on it and it is enabled.
    Hover,
    /// The same, for a disabled widget.
    DisabledHover,
    /// At the pointer, for a widget too large to sit a note beside.
    AtPointer,
}

/// `add` as `resp`'s tooltip, on glass. Drop-in for egui's `on_hover_ui`.
pub fn on_hover_ui(resp: egui::Response, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    show(resp, Trigger::Hover, add)
}

/// `add` as `resp`'s tooltip while it is disabled, on glass. Drop-in for
/// egui's `on_disabled_hover_ui`.
pub fn on_disabled_hover_ui(
    resp: egui::Response,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    show(resp, Trigger::DisabledHover, add)
}

/// `add` as `resp`'s tooltip at the pointer, on glass. Drop-in for egui's
/// `on_hover_ui_at_pointer`.
pub fn on_hover_ui_at_pointer(
    resp: egui::Response,
    add: impl FnOnce(&mut egui::Ui),
) -> egui::Response {
    show(resp, Trigger::AtPointer, add)
}

fn show(resp: egui::Response, trigger: Trigger, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let ctx = resp.ctx.clone();
    // egui decides whether the note shows; this only asks whether it could,
    // so the style swap below is made for the note or two a frame that can.
    let could = resp.contains_pointer()
        || resp.is_tooltip_open()
        || ctx.memory(|m| m.everything_is_visible());
    if !could {
        return resp;
    }
    let style = ctx.style();
    ctx.style_mut(|s| {
        s.visuals.window_fill = egui::Color32::TRANSPARENT;
        s.visuals.window_stroke = egui::Stroke::NONE;
        s.visuals.popup_shadow = egui::epaint::Shadow::NONE;
    });
    let body = |ui: &mut egui::Ui| on_glass(ui, add);
    let resp = match trigger {
        Trigger::Hover => resp.on_hover_ui(body),
        Trigger::DisabledHover => resp.on_disabled_hover_ui(body),
        Trigger::AtPointer => resp.on_hover_ui_at_pointer(body),
    };
    ctx.set_style(style);
    resp
}

/// The note's content with the glass under it, inside egui's (now
/// transparent) popup frame.
fn on_glass(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let ctx = ui.ctx().clone();
    let id = glass_id();
    // While the backdrop is being snapshotted the note is laid out but not
    // painted, or it would end up in its own frost. Laying it out keeps
    // egui's placement right for the pass it appears on.
    if !glass::ready(&ctx, id) {
        ui.set_invisible();
        ctx.request_repaint();
    }
    let slot = glass::begin(ui);
    add(ui);
    // The frame wraps the content's `min_rect` in the popup margin; the
    // glass covers the same.
    let rect = ui.style().spacing.menu_margin.expand_rect(ui.min_rect());
    let rounding = ui.style().visuals.menu_rounding;
    glass::end(&ctx, slot, id, rect, rounding, stroke());
    glass::drawn(&ctx, id);
}
