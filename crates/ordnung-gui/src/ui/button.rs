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

/// A square push button holding one glyph from the app's icon face (see
/// `phosphor_icons::named`): the "+" at the end of a list's caption, and
/// any other action that is one mark rather than a word. The standard
/// interact height on both sides, the theme's frame, the glyph at the body
/// size, so a row of them and the buttons beside them come out one family. Without the explicit square a one-character label lands in the
/// theme's 10 × 6 padding and reads as a stretched pill.
pub fn square(ui: &mut egui::Ui, glyph: &str) -> egui::Response {
    let side = ui.spacing().interact_size.y;
    let prev = ui.spacing().button_padding;
    ui.spacing_mut().button_padding = egui::Vec2::ZERO;
    let resp = ui.add(
        egui::Button::new(egui::RichText::new(glyph).font(font::icon(font::body().size + 2.0)))
            .min_size(egui::vec2(side, side))
            .rounding(egui::Rounding::same(radius::SM)),
    );
    ui.spacing_mut().button_padding = prev;
    resp
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

/// Horizontal inset of a [`deck`] key's label.
const DECK_PAD_X: f32 = 10.0;

/// A deck key: a control of the player's performance surface — loop, cue,
/// quantize — as distinct from the app's push buttons. The library's chrome
/// is an outlined, rounded grey button with a sentence-case label, the way a
/// macOS toolbar button sits on its bar; that family is for *managing* the
/// collection. A deck key is the other family, for *playing*: flat and
/// sunken like a key on a player, squarer, its label small caps, and when
/// engaged it lights — a colour fill with dark ink — rather than taking the
/// selection blue. Keeping the two apart is what lets the eye tell the
/// transport from the toolbar at a glance.
///
/// Takes the row's control height, so it lines up with a field or a readout
/// beside it. `lit` is the colour the key shows while engaged (`None` at
/// rest). Disabled, it fades and takes no click.
pub fn deck(
    ui: &mut egui::Ui,
    label: &str,
    lit: Option<egui::Color32>,
    enabled: bool,
) -> egui::Response {
    let h = ui.spacing().interact_size.y;
    // A single glyph (‹, ›, Q) sits at body size so it reads as a key cap;
    // a word is small caps at footnote size.
    let font_id = if label.chars().count() == 1 {
        font::strong(font::body().size)
    } else {
        font::strong(font::footnote().size)
    };
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_uppercase(), font_id, color::LABEL);
    let w = (galley.size().x + 2.0 * DECK_PAD_X).max(h * 1.25).round();
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(w, h),
        if enabled { egui::Sense::click() } else { egui::Sense::hover() },
    );
    if ui.is_rect_visible(rect) {
        let rounding = egui::Rounding::same(radius::XS);
        let hovered = enabled && resp.hovered();
        let down = enabled && resp.is_pointer_button_down_on();
        let (fill, ink, edge) = match lit {
            Some(c) if enabled => (c, egui::Color32::from_gray(16), None),
            _ if !enabled => (color::FIELD, color::LABEL_4, Some(color::SURFACE_HI)),
            _ if down => (color::SURFACE_ACTIVE, color::LABEL, None),
            _ if hovered => (color::SURFACE_HOVER, color::LABEL, None),
            _ => (color::FIELD, color::LABEL_2, Some(color::SURFACE_HI)),
        };
        ui.painter().rect_filled(rect, rounding, fill);
        if let Some(edge) = edge {
            ui.painter()
                .rect_stroke(rect, rounding, egui::Stroke::new(1.0, edge));
        }
        ui.painter()
            .galley(rect.center() - galley.size() / 2.0, galley, ink);
        if hovered {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
    }
    resp
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

/// A name on a line of text that opens a page: the artist on a record
/// sheet's header, the performer or remixer on one of its tracks. Set in
/// the line's own type and ink, so the line still reads as a line, and
/// marked as a link only under the pointer: the ink brightens, a rule runs
/// under the name, the hand shows. For the page a name stands for; an
/// action on the record itself goes on a button.
pub fn name_link(
    ui: &mut egui::Ui,
    name: &str,
    font: egui::FontId,
    ink: egui::Color32,
) -> egui::Response {
    let galley = ui
        .painter()
        .layout_no_wrap(name.to_owned(), font, egui::Color32::PLACEHOLDER);
    let (rect, resp) = ui.allocate_exact_size(galley.size(), egui::Sense::click());
    paint_name_link(ui, rect, &resp, galley, ink);
    resp
}

/// [`name_link`] painted into a rect the caller laid out earlier, in a
/// galley the caller made, interacting through `id`. For a name inside a
/// row that is itself a hit target: register it after the row's own
/// interact, so the name sits on top and takes the click, the way
/// [`like_mark_at`] does.
pub fn name_link_at(
    ui: &mut egui::Ui,
    rect: egui::Rect,
    id: egui::Id,
    galley: std::sync::Arc<egui::Galley>,
    ink: egui::Color32,
) -> egui::Response {
    let resp = ui.interact(rect, id, egui::Sense::click());
    paint_name_link(ui, rect, &resp, galley, ink);
    resp
}

fn paint_name_link(
    ui: &egui::Ui,
    rect: egui::Rect,
    resp: &egui::Response,
    galley: std::sync::Arc<egui::Galley>,
    ink: egui::Color32,
) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    let hot = resp.hovered() || resp.is_pointer_button_down_on();
    let ink = if hot { ui.visuals().strong_text_color() } else { ink };
    if hot {
        ui.painter().hline(
            rect.x_range(),
            rect.bottom() - 1.0,
            egui::Stroke::new(1.0, ink),
        );
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    ui.painter().galley(rect.min, galley, ink);
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
