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

// --- Tour / feature marks -------------------------------------------------
//
// Drawn rather than set as text, for the reason in the module doc: a font glyph
// carries whatever weight and baseline the font happens to supply, so a column
// of "✚ ~ ≡" reads as three unrelated characters. These share one stroke weight
// and one optical box, so a list of them reads as a set.

/// Stroke weight shared by the marks below, so they read as one family.
const MARK: f32 = 1.6;

/// Import: a tray with an arrow coming down into it.
pub fn import(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    // Shaft + head.
    p.line_segment(
        [egui::pos2(c.x, c.y - r), egui::pos2(c.x, c.y + r * 0.15)],
        s,
    );
    p.line_segment(
        [
            egui::pos2(c.x - r * 0.42, c.y - r * 0.28),
            egui::pos2(c.x, c.y + r * 0.15),
        ],
        s,
    );
    p.line_segment(
        [
            egui::pos2(c.x + r * 0.42, c.y - r * 0.28),
            egui::pos2(c.x, c.y + r * 0.15),
        ],
        s,
    );
    // Tray.
    p.line_segment(
        [
            egui::pos2(c.x - r, c.y + r * 0.45),
            egui::pos2(c.x - r, c.y + r),
        ],
        s,
    );
    p.line_segment(
        [egui::pos2(c.x - r, c.y + r), egui::pos2(c.x + r, c.y + r)],
        s,
    );
    p.line_segment(
        [
            egui::pos2(c.x + r, c.y + r * 0.45),
            egui::pos2(c.x + r, c.y + r),
        ],
        s,
    );
}

/// Analysis: a waveform — four bars of differing height, the app's own idiom.
pub fn waveform(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let heights = [0.45f32, 1.0, 0.62, 0.85, 0.35];
    let step = (r * 2.0) / (heights.len() as f32 - 0.35);
    for (i, h) in heights.iter().enumerate() {
        let x = c.x - r + step * i as f32;
        let half = r * h;
        p.line_segment(
            [egui::pos2(x, c.y - half), egui::pos2(x, c.y + half)],
            egui::Stroke::new(MARK, col),
        );
    }
}

/// Organize: three stacked list rows, the shortest last.
pub fn list(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    for (i, w) in [1.0f32, 0.72, 0.86].iter().enumerate() {
        let y = c.y - r * 0.62 + r * 0.62 * i as f32;
        p.line_segment(
            [egui::pos2(c.x - r, y), egui::pos2(c.x - r + r * 2.0 * w, y)],
            s,
        );
    }
}

/// Library: a card box — three filing cards stepping back behind the box's
/// front panel. The catalog as a physical thing you file records into, which is
/// what "the library" means here; a plain list glyph reads as one playlist
/// among many rather than as the container that holds them all.
///
/// Drawn entirely in strokes of `col`, with no fill: the icon sits on a dark
/// tile and on the accent fill of the selected one, so it cannot assume a
/// background colour to paint over.
pub fn library(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    // The cards recede upward and inward: each is drawn narrower and higher than
    // the one in front, so the stack reads as depth rather than as three
    // separate rectangles.
    //
    // Each card's *bottom* is inside the box and must not be drawn, so the
    // cards are clipped to the strip above the box front rather than being
    // covered over afterwards. Painting an occluding panel on top instead only
    // works against a known background, and this icon sits on two (the dark
    // tile and the accent fill of the selected one).
    let lip = c.y - r * 0.12;
    let cards = p.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(c.x - r, c.y - r * 1.1),
        egui::pos2(c.x + r, lip),
    ));
    for (i, w) in [0.86f32, 0.72, 0.58].iter().enumerate() {
        let top = c.y - r * (0.34 + 0.20 * i as f32);
        cards.rect_stroke(
            egui::Rect::from_min_max(
                egui::pos2(c.x - r * w, top),
                // Runs past the clip edge; the clip is what ends it.
                egui::pos2(c.x + r * w, c.y + r * 0.2),
            ),
            egui::Rounding::same(1.0),
            s,
        );
    }
    // The box front, drawn over the clipped card bottoms.
    p.rect_stroke(
        egui::Rect::from_min_max(
            egui::pos2(c.x - r, lip),
            egui::pos2(c.x + r, c.y + r * 0.86),
        ),
        egui::Rounding::same(1.5),
        s,
    );
}

/// Deck: a play triangle inside a circle.
pub fn deck(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    p.circle_stroke(c, r, egui::Stroke::new(MARK, col));
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - r * 0.28, c.y - r * 0.42),
            egui::pos2(c.x + r * 0.45, c.y),
            egui::pos2(c.x - r * 0.28, c.y + r * 0.42),
        ],
        col,
        egui::Stroke::NONE,
    ));
}

/// Dig: a magnifier — searching outward from what you already have.
pub fn dig(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    let lens = egui::pos2(c.x - r * 0.15, c.y - r * 0.15);
    p.circle_stroke(lens, r * 0.62, s);
    p.line_segment(
        [
            egui::pos2(lens.x + r * 0.45, lens.y + r * 0.45),
            egui::pos2(c.x + r * 0.85, c.y + r * 0.85),
        ],
        s,
    );
}

/// Record: a disc — outer edge, label, spindle.
pub fn record(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.circle_stroke(c, r, s);
    p.circle_stroke(c, r * 0.38, s);
    p.circle_filled(c, MARK * 0.7, col);
}

/// Release match: a tag with its punch hole.
pub fn tag(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    // A pentagon-ish tag body: square left, point right.
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - r, c.y - r * 0.68),
            egui::pos2(c.x + r * 0.3, c.y - r * 0.68),
            egui::pos2(c.x + r, c.y),
            egui::pos2(c.x + r * 0.3, c.y + r * 0.68),
            egui::pos2(c.x - r, c.y + r * 0.68),
        ],
        s,
    ));
    p.circle_filled(egui::pos2(c.x - r * 0.5, c.y), MARK * 0.8, col);
}

/// Cover art: a framed picture with a horizon and a sun.
pub fn art(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    let rect = egui::Rect::from_center_size(c, egui::vec2(r * 2.0, r * 1.8));
    p.rect_stroke(rect, egui::Rounding::same(2.0), s);
    p.circle_filled(
        egui::pos2(rect.left() + r * 0.55, rect.top() + r * 0.5),
        MARK * 0.9,
        col,
    );
    // A simple peak along the bottom edge.
    p.add(egui::Shape::line(
        vec![
            egui::pos2(rect.left() + MARK, rect.bottom() - MARK),
            egui::pos2(c.x - r * 0.1, c.y + r * 0.05),
            egui::pos2(rect.right() - MARK, rect.bottom() - MARK),
        ],
        s,
    ));
}

/// Sync: two arrows chasing each other round a loop — automatic writeback.
pub fn sync(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    // Two opposing arcs, each with a head, drawn as short polylines.
    for flip in [1.0f32, -1.0] {
        let pts: Vec<egui::Pos2> = (0..=14)
            .map(|i| {
                let t = std::f32::consts::PI * (i as f32 / 14.0) * 0.92 + 0.18;
                egui::pos2(c.x + flip * r * t.cos(), c.y + flip * r * t.sin())
            })
            .collect();
        let end = *pts.last().unwrap();
        p.add(egui::Shape::line(pts, s));
        // Arrow head at the arc's end, pointing along the travel direction.
        let a = r * 0.38;
        p.line_segment(
            [end, egui::pos2(end.x + flip * a * 0.1, end.y - flip * a)],
            s,
        );
        p.line_segment(
            [
                end,
                egui::pos2(end.x - flip * a * 0.85, end.y - flip * a * 0.5),
            ],
            s,
        );
    }
}

/// Hold: a pause-like pair of bars in a circle — writes parked until you say so.
pub fn hold(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.circle_stroke(c, r, s);
    for dx in [-r * 0.28, r * 0.28] {
        p.line_segment(
            [
                egui::pos2(c.x + dx, c.y - r * 0.42),
                egui::pos2(c.x + dx, c.y + r * 0.42),
            ],
            s,
        );
    }
}

/// Shield: the trust mark on the welcome step.
pub fn shield(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x, c.y - r),
            egui::pos2(c.x + r * 0.82, c.y - r * 0.62),
            egui::pos2(c.x + r * 0.72, c.y + r * 0.35),
            egui::pos2(c.x, c.y + r),
            egui::pos2(c.x - r * 0.72, c.y + r * 0.35),
            egui::pos2(c.x - r * 0.82, c.y - r * 0.62),
        ],
        s,
    ));
    // Check inside.
    p.add(egui::Shape::line(
        vec![
            egui::pos2(c.x - r * 0.34, c.y - r * 0.02),
            egui::pos2(c.x - r * 0.08, c.y + r * 0.28),
            egui::pos2(c.x + r * 0.4, c.y - r * 0.32),
        ],
        s,
    ));
}
