//! Painted icons for controls that shouldn't read as text in a button frame.
//!
//! egui renders "✕" and friends through the font stack, so a close made that
//! way carries the weight, baseline and hinting of whatever glyph the font
//! happens to supply — and sits in default button chrome next to controls that
//! are drawn. These are drawn on the same terms as the transport's play
//! triangle, so a row of controls reads as one set.

use super::hover::HoverNoteExt;

/// Resting and hover colours shared by the icons here, so a close in the player
/// bar and a close on a card answer the pointer the same way.
pub const REST: egui::Color32 = egui::Color32::from_gray(150);
pub const HOVER: egui::Color32 = egui::Color32::WHITE;

/// Colour for a painted icon, given how the pointer is treating it.
pub fn col(resp: &egui::Response) -> egui::Color32 {
    if resp.hovered() {
        HOVER
    } else {
        REST
    }
}

/// Draw a close cross centred on `c`, with arms `r` long.
pub fn close(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let stroke = egui::Stroke::new(1.5, col);
    p.line_segment(
        [egui::pos2(c.x - r, c.y - r), egui::pos2(c.x + r, c.y + r)],
        stroke,
    );
    p.line_segment(
        [egui::pos2(c.x + r, c.y - r), egui::pos2(c.x - r, c.y + r)],
        stroke,
    );
}

/// A frameless close button: allocates its own square, paints the cross, and
/// answers the pointer. `tip` is the hover note. Returns true when clicked.
///
/// This is the whole control, not just its mark — every close that used to be a
/// `✕` in a button goes through here, so they stay one control rather than
/// drifting apart at each call site.
pub fn close_button(ui: &mut egui::Ui, tip: &str) -> bool {
    const SIZE: f32 = 24.0;
    const ARM: f32 = 4.5;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
    let resp = resp.on_hover_note(tip);
    close(ui.painter(), rect.center(), col(&resp), ARM);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// Draw a play triangle, or the two pause bars, centred on `c`.
///
/// One mark for both states, so the control that toggles between them keeps its
/// footprint: the glyph swaps inside a fixed square instead of a label growing
/// from "Play" to "Pause" and shifting everything to its right.
pub fn play_pause(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, playing: bool) {
    if playing {
        for dx in [-4.0f32, 3.0] {
            p.rect_filled(
                egui::Rect::from_min_size(egui::pos2(c.x + dx, c.y - 6.0), egui::vec2(3.0, 12.0)),
                0.5,
                col,
            );
        }
    } else {
        p.add(egui::Shape::convex_polygon(
            vec![
                egui::pos2(c.x - 4.0, c.y - 6.5),
                egui::pos2(c.x + 6.0, c.y),
                egui::pos2(c.x - 4.0, c.y + 6.5),
            ],
            col,
            egui::Stroke::NONE,
        ));
    }
}
