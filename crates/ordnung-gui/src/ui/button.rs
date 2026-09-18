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

/// A glyph that acts, sitting at the edge of a row: a square the row's
/// height, the glyph in the weak label tone until the pointer is on it,
/// then strong on the hover fill. For the one action a row keeps at hand
/// (look this up again, refresh), the way the track table's Discogs release
/// line keeps its ↻. Disabled, it fades and takes no click.
pub fn glyph(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    let side = ui.spacing().interact_size.y;
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(side, side),
        if enabled { egui::Sense::click() } else { egui::Sense::hover() },
    );
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        if enabled && resp.hovered() {
            ui.painter().rect_filled(
                rect,
                visuals.widgets.hovered.rounding,
                visuals.widgets.hovered.weak_bg_fill,
            );
        }
        let ink = if !enabled {
            visuals.weak_text_color().gamma_multiply(0.5)
        } else if resp.hovered() {
            visuals.strong_text_color()
        } else {
            visuals.weak_text_color()
        };
        ui.painter().text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            glyph,
            egui::TextStyle::Button.resolve(ui.style()),
            ink,
        );
    }
    resp
}

/// The like mark at the edge of a song row: a "+" in the weak label tone
/// until the pointer is on it, a heart in the pink once the song is in the
/// crate of liked songs (see `liked`). Square, `side` to a side, so it fits
/// a row of any height: the sheet's 24 pt lines as well as the tracklist's
/// 46 pt rows. Carries its own hover note.
pub fn like_mark(ui: &mut egui::Ui, liked: bool, side: f32) -> egui::Response {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::click());
    paint_like_mark(ui, rect, &resp, liked);
    like_note(resp, liked)
}

/// The like mark painted into a rect the caller laid out by hand (a
/// painter-drawn row), interacting through `id`. Register it after the
/// row's own interact, so the mark sits on top and takes the click.
pub fn like_mark_at(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    liked: bool,
) -> egui::Response {
    let resp = ui.interact(rect, id, egui::Sense::click());
    paint_like_mark(ui, rect, &resp, liked);
    like_note(resp, liked)
}

fn like_note(resp: egui::Response, liked: bool) -> egui::Response {
    use super::hover::HoverNoteExt;
    resp.on_hover_note(if liked {
        "Liked. Click to take it out of Liked songs"
    } else {
        "Like this song: put it in Liked songs"
    })
}

fn paint_like_mark(ui: &egui::Ui, rect: egui::Rect, resp: &egui::Response, liked: bool) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let visuals = ui.visuals();
    if resp.hovered() {
        ui.painter().rect_filled(
            rect,
            visuals.widgets.hovered.rounding,
            visuals.widgets.hovered.weak_bg_fill,
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    let (glyph, ink) = match (liked, resp.hovered()) {
        (true, true) => ("♥", color::PINK.gamma_multiply(0.8)),
        (true, false) => ("♥", color::PINK),
        (false, true) => ("+", visuals.strong_text_color()),
        (false, false) => ("+", visuals.weak_text_color()),
    };
    let size = (rect.height() * 0.62).clamp(12.0, 16.0);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        egui::FontId::proportional(size),
        ink,
    );
}
