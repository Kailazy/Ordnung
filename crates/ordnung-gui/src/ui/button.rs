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

use super::tokens::{color, font, radius};

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

/// One part of a [`segmented`] button: its label and its hover note.
pub struct Segment<'a> {
    pub label: &'a str,
    pub tip: &'a str,
}

/// Several buttons in one frame, parted by hairlines: a choice between a
/// few views of the same thing (a wall of covers or rows), where separate
/// buttons would read as separate actions and take a gap each. The frame
/// is the button's own (fill, outline, corner radius) and the row's height,
/// so it sits on a control row like any button; inside it, the chosen
/// segment carries the selection fill and a pointed-at one the hover fill,
/// and each segment carries its own hover note. Hands back the index of
/// the segment clicked this frame, if any; the caller owns the choice.
pub fn segmented(ui: &mut egui::Ui, selected: Option<usize>, segments: &[Segment]) -> Option<usize> {
    let pad = ui.spacing().button_padding;
    let font_id = egui::TextStyle::Button.resolve(ui.style());
    let galleys: Vec<_> = segments
        .iter()
        .map(|s| ui.painter().layout_no_wrap(s.label.to_owned(), font_id.clone(), color::LABEL))
        .collect();
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * pad.x).collect();
    let h = ui.spacing().interact_size.y;
    let size = egui::vec2(widths.iter().sum(), h);
    let (rect, whole) = ui.allocate_exact_size(size, egui::Sense::hover());
    let id = whole.id;
    // Every segment's hit area is claimed before anything is painted, so
    // the frame can be painted once, under all of them.
    let mut x = rect.left();
    let rects: Vec<egui::Rect> = widths
        .iter()
        .map(|w| {
            let r = egui::Rect::from_min_size(egui::pos2(x, rect.top()), egui::vec2(*w, h));
            x += w;
            r
        })
        .collect();
    let resps: Vec<egui::Response> = rects
        .iter()
        .enumerate()
        .map(|(i, r)| ui.interact(*r, id.with(i), egui::Sense::click()))
        .collect();
    let mut clicked = None;
    if ui.is_rect_visible(rect) {
        let visuals = ui.visuals();
        let rest = &visuals.widgets.inactive;
        let rounding = egui::Rounding::same(radius::SM);
        ui.painter().rect_filled(rect, rounding, rest.weak_bg_fill);
        // Fills under the outline: each segment's rect is inset by the
        // stroke so its fill never covers the frame's edge, and the end
        // segments keep the frame's corners.
        let inset = rest.bg_stroke.width / 2.0;
        let last = rects.len().saturating_sub(1);
        for (i, (r, resp)) in rects.iter().zip(&resps).enumerate() {
            let fill = if selected == Some(i) {
                Some(visuals.selection.bg_fill)
            } else if resp.is_pointer_button_down_on() {
                Some(visuals.widgets.active.weak_bg_fill)
            } else if resp.hovered() {
                Some(visuals.widgets.hovered.weak_bg_fill)
            } else {
                None
            };
            if let Some(fill) = fill {
                let r = r.shrink(inset);
                let rounding = egui::Rounding {
                    nw: if i == 0 { radius::SM - inset } else { 0.0 },
                    sw: if i == 0 { radius::SM - inset } else { 0.0 },
                    ne: if i == last { radius::SM - inset } else { 0.0 },
                    se: if i == last { radius::SM - inset } else { 0.0 },
                };
                ui.painter().rect_filled(r, rounding, fill);
            }
        }
        ui.painter().rect_stroke(rect, rounding, rest.bg_stroke);
        // The dividers, in the outline's own colour and weight.
        for r in rects.iter().take(last) {
            ui.painter().line_segment(
                [egui::pos2(r.right(), rect.top()), egui::pos2(r.right(), rect.bottom())],
                rest.bg_stroke,
            );
        }
        for (r, g) in rects.iter().zip(galleys) {
            ui.painter().galley(r.center() - g.size() / 2.0, g, color::LABEL);
        }
    }
    for (i, (resp, seg)) in resps.into_iter().zip(segments).enumerate() {
        use super::hover::HoverNoteExt;
        if resp.on_hover_note(seg.tip).clicked() {
            clicked = Some(i);
        }
    }
    clicked
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
        "Add to Liked songs"
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
