//! Painted icons for controls that shouldn't read as text in a button frame.
//!
//! egui renders "✕" and friends through the font stack, so a close made that
//! way carries the weight, baseline and hinting of whatever glyph the font
//! happens to supply — and sits in default button chrome next to controls that
//! are drawn. These are drawn on the same terms as the transport's play
//! triangle, so a row of controls reads as one set.

use super::hover::HoverNoteExt;
use super::tokens::{color, font, radius, space};
use ordnung_core::model::VinylList;

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

/// A stop square centred on `c`, `r` to each edge.
pub fn stop(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    p.rect_filled(egui::Rect::from_center_size(c, egui::Vec2::splat(r * 2.0)), 1.5, col);
}

/// Skip: a play triangle running into a bar, the next-track mark.
pub fn skip_next(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let h = r * 0.9;
    p.add(egui::Shape::convex_polygon(
        vec![
            egui::pos2(c.x - r, c.y - h),
            egui::pos2(c.x + r * 0.45, c.y),
            egui::pos2(c.x - r, c.y + h),
        ],
        col,
        egui::Stroke::NONE,
    ));
    p.rect_filled(
        egui::Rect::from_min_max(egui::pos2(c.x + r * 0.6, c.y - h), egui::pos2(c.x + r, c.y + h)),
        0.5,
        col,
    );
}

/// A frameless square button carrying one painted mark: `side` on each edge,
/// the mark drawn by `paint` at the centre in the ink the pointer earns it.
/// Rests grey, goes white under the pointer on a soft rounded ground, and dims
/// with the pointer ignored when `enabled` is false. `tip` is the hover note.
///
/// The transport rows use this so a pause, a skip and a stop read as one set of
/// marks rather than four words of differing width in button chrome.
pub fn mark_button(
    ui: &mut egui::Ui,
    side: f32,
    enabled: bool,
    tip: &str,
    paint: impl FnOnce(&egui::Painter, egui::Pos2, egui::Color32),
) -> egui::Response {
    let sense = if enabled {
        egui::Sense::click()
    } else {
        egui::Sense::hover()
    };
    let (rect, resp) = ui.allocate_exact_size(egui::Vec2::splat(side), sense);
    let resp = if enabled {
        resp.on_hover_note(tip)
    } else {
        resp.on_disabled_hover_text(super::hover::note(tip))
    };
    let ink = if !enabled {
        REST.gamma_multiply(0.45)
    } else {
        if resp.hovered() {
            ui.painter().rect_filled(rect, radius::SM, color::SURFACE_HOVER);
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        col(&resp)
    };
    paint(ui.painter(), rect.center(), ink);
    resp
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

/// Artist: a head over shoulders. The map's artist thread node.
pub fn artist(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.circle_stroke(egui::pos2(c.x, c.y - r * 0.38), r * 0.36, s);
    // Shoulders: a bowl opening downward under the head.
    let centre = egui::pos2(c.x, c.y + r * 0.95);
    let pts: Vec<egui::Pos2> = (0..=10)
        .map(|i| {
            let a = std::f32::consts::PI * (1.0 + i as f32 / 10.0);
            centre + egui::vec2(a.cos(), a.sin()) * r * 0.82
        })
        .collect();
    p.add(egui::Shape::line(pts, s));
}

/// Label: a house, roof over walls. The map's label thread node.
pub fn house(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x - r * 0.78, c.y + r * 0.82),
            egui::pos2(c.x - r * 0.78, c.y - r * 0.05),
            egui::pos2(c.x, c.y - r * 0.88),
            egui::pos2(c.x + r * 0.78, c.y - r * 0.05),
            egui::pos2(c.x + r * 0.78, c.y + r * 0.82),
        ],
        s,
    ));
}

/// Style: a diamond with a dot at its heart. The map's style thread node.
pub fn style(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(c.x, c.y - r * 0.95),
            egui::pos2(c.x + r * 0.95, c.y),
            egui::pos2(c.x, c.y + r * 0.95),
            egui::pos2(c.x - r * 0.95, c.y),
        ],
        s,
    ));
    p.circle_filled(c, MARK * 0.8, col);
}

/// Radio: a dot sending out two arcs. The map's radio node.
pub fn broadcast(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    let o = egui::pos2(c.x - r * 0.45, c.y + r * 0.45);
    p.circle_filled(o, MARK * 0.9, col);
    for k in [0.55f32, 1.0] {
        let pts: Vec<egui::Pos2> = (0..=8)
            .map(|i| {
                // From due north round to due east: the quarter facing away
                // from the corner the dot sits in.
                let a = -std::f32::consts::FRAC_PI_2 + i as f32 / 8.0 * std::f32::consts::FRAC_PI_2;
                o + egui::vec2(a.cos(), a.sin()) * r * 1.25 * k
            })
            .collect();
        p.add(egui::Shape::line(pts, s));
    }
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

/// Folder: the reveal-in-Finder mark, drawn as a tab-and-body outline.
///
/// `r` is the half-width of the body, and the whole mark is centred on `c`, so
/// it optically matches a line of text the way the marks above do rather than
/// sitting on its own baseline.
pub fn folder(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32) {
    let s = egui::Stroke::new(MARK, col);
    let (l, rt) = (c.x - r, c.x + r);
    let (top, bot) = (c.y - r * 0.62, c.y + r * 0.62);
    // Tab along the top-left, then the body — one closed outline so the two
    // never drift apart at a corner.
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(l, bot),
            egui::pos2(l, top - r * 0.2),
            egui::pos2(l + r * 0.5, top - r * 0.2),
            egui::pos2(l + r * 0.72, top),
            egui::pos2(rt, top),
            egui::pos2(rt, bot),
        ],
        s,
    ));
}

/// A frameless folder button sized to sit inline with a line of text: allocates
/// its own square, paints the mark, and answers the pointer. Returns true when
/// clicked.
///
/// Same shape as [`close_button`] — the control is the whole thing, not just its
/// mark — so every "show me this file" affordance stays one control.
pub fn folder_button(ui: &mut egui::Ui, tip: &str) -> bool {
    const SIZE: f32 = 14.0;
    const R: f32 = 4.5;
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(SIZE, SIZE), egui::Sense::click());
    let resp = resp.on_hover_note(tip);
    folder(ui.painter(), rect.center(), col(&resp), R);
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked()
}

/// A bare check, centred on `c` with half-extent `r`.
///
/// Takes its own `stroke` width rather than the shared [`MARK`]: this one is
/// drawn at badge sizes as well as at text size, and a 1.6px stroke on a 4px
/// tick is a blob. The proportions are the usual short-arm/long-arm tick, sized
/// so it still reads as a check when it's only a few pixels across.
pub fn check(p: &egui::Painter, c: egui::Pos2, col: egui::Color32, r: f32, stroke: f32) {
    let s = egui::Stroke::new(stroke, col);
    p.add(egui::Shape::line(
        vec![
            egui::pos2(c.x - r, c.y + r * 0.06),
            egui::pos2(c.x - r * 0.24, c.y + r * 0.72),
            egui::pos2(c.x + r, c.y - r * 0.7),
        ],
        s,
    ));
}

// --- Shelf marks -----------------------------------------------------------
//
// The two Discogs shelves have one mark each, drawn here and nowhere else, so
// a record's standing reads the same on a tab, a button, a chip, a legend
// and a row: the collection is a record half out of its sleeve, the wantlist
// is a heart, the mark Discogs itself puts on wants. Solid when the record is
// on that shelf, outlined when it isn't.

/// Radius the marks are drawn at beside body text.
pub const SHELF_R: f32 = 6.5;

/// The mark for `list`, centred on `c`.
pub fn shelf(p: &egui::Painter, c: egui::Pos2, r: f32, ink: egui::Color32, filled: bool, list: VinylList) {
    match list {
        VinylList::Collection => collection(p, c, r, ink, filled),
        VinylList::Wantlist => wantlist(p, c, r, ink, filled),
    }
}

/// A record half out of its sleeve: a rounded square with a disc emerging
/// from its right edge. One record is the right unit: the question the mark
/// answers is whether *this* record is on the shelf.
pub fn collection(p: &egui::Painter, c: egui::Pos2, r: f32, ink: egui::Color32, filled: bool) {
    let w = r * 0.92;
    // The sleeve sits left of centre so the disc has somewhere to emerge to,
    // keeping the pair balanced on `c` rather than hanging off it.
    let sleeve = egui::Rect::from_min_max(
        egui::pos2(c.x - w * 1.02, c.y - w),
        egui::pos2(c.x + w * 0.30, c.y + w),
    );
    let rounding = egui::Rounding::same((r * 0.15).max(1.0));
    let disc_c = egui::pos2(c.x + w * 0.34, c.y);
    let disc_r = w * 0.86;
    let stroke = egui::Stroke::new((r * 0.15).clamp(1.0, 1.6), ink);
    if filled {
        p.rect_filled(sleeve, rounding, ink);
        p.circle_filled(disc_c, disc_r, ink);
        // The spindle hole is punched in the ground, which is what keeps a
        // solid disc reading as a record rather than as a dot.
        p.circle_filled(disc_c, disc_r * 0.24, color::SURFACE);
    } else {
        p.rect_stroke(sleeve, rounding, stroke);
        p.circle_stroke(disc_c, disc_r, stroke);
        p.circle_filled(disc_c, disc_r * 0.22, ink);
    }
}

/// A heart, the wantlist's mark on discogs.com too: two round lobes over a
/// point, the plainest heart there is.
///
/// Built from geometry rather than the parametric curve, which bulged at the
/// lobes and pinched at the notch and read as a blob at 13px. Here the lobes
/// are two circles meeting at the notch and the sides run straight and
/// tangent from them down to the tip, so the outline and the fill are the
/// same clean shape at every size it's drawn.
pub fn wantlist(p: &egui::Painter, c: egui::Pos2, r: f32, ink: egui::Color32, filled: bool) {
    let lobe = r * 0.52; // lobe radius; the heart is 2 lobes wide
    let drop = r * 1.32; // notch to tip
    // Centre the whole shape on `c`: the top of the lobes and the tip sit the
    // same distance from it.
    let notch_y = c.y - (drop - lobe) * 0.5;
    let tip = egui::pos2(c.x, notch_y + drop);
    let left = egui::pos2(c.x - lobe, notch_y);
    let right = egui::pos2(c.x + lobe, notch_y);
    // Where the straight side leaves the left lobe: the tangent from the tip.
    let v = tip - left;
    let len = v.length();
    let phi = v.y.atan2(v.x);
    let alpha = (lobe / len).acos();
    // Of the two tangents, the outer one (larger angle, further left).
    let t0 = phi + alpha;
    const STEPS: usize = 18;
    let arc = |centre: egui::Pos2, from: f32, to: f32, out: &mut Vec<egui::Pos2>| {
        for i in 0..=STEPS {
            let a = from + (to - from) * i as f32 / STEPS as f32;
            out.push(egui::pos2(centre.x + lobe * a.cos(), centre.y + lobe * a.sin()));
        }
    };
    let mut pts = Vec::with_capacity(STEPS * 2 + 4);
    pts.push(tip);
    // Up the left side, over the left lobe to the notch (angle 0 on this
    // circle), over the right lobe (from angle π) and down to the tip.
    arc(left, t0, std::f32::consts::TAU, &mut pts);
    arc(right, std::f32::consts::PI, 3.0 * std::f32::consts::PI - t0, &mut pts);
    if filled {
        // A heart isn't convex, so it's filled as the union of pieces that
        // are: the two lobes and the kite between them and the tip.
        p.circle_filled(left, lobe, ink);
        p.circle_filled(right, lobe, ink);
        let t_left = pts[1];
        let t_right = pts[pts.len() - 1];
        p.add(egui::Shape::convex_polygon(
            vec![tip, t_left, left, right, t_right],
            ink,
            egui::Stroke::NONE,
        ));
    } else {
        p.add(egui::Shape::closed_line(
            pts,
            egui::Stroke::new((r * 0.15).clamp(1.0, 1.6), ink),
        ));
    }
}

/// The chip ground a shelf badge sits on: the collection's green and the
/// wantlist's amber, kept from the seller cards where they started.
pub fn shelf_fill(list: VinylList) -> egui::Color32 {
    match list {
        VinylList::Collection => egui::Color32::from_rgb(40, 120, 70),
        VinylList::Wantlist => egui::Color32::from_rgb(120, 90, 30),
    }
}

/// A small filled chip carrying the mark, for a card or a row that wants a
/// record's standing badged without words. Returns the chip's rect.
pub fn shelf_chip(p: &egui::Painter, min: egui::Pos2, list: VinylList) -> egui::Rect {
    let chip = egui::Rect::from_min_size(min, egui::vec2(20.0, 16.0));
    p.rect_filled(chip, egui::Rounding::same(4.0), shelf_fill(list));
    shelf(p, chip.center(), 5.0, egui::Color32::WHITE, true, list);
    chip
}

/// A button carrying the shelf mark before its label: filled when the record
/// is on the shelf, outlined when it's an offer to put it there. An empty
/// label makes it a square mark-only button. Disabled while `enabled` is
/// false, drawn the way a disabled stock button is.
pub fn shelf_button(
    ui: &mut egui::Ui,
    list: VinylList,
    present: bool,
    label: &str,
    enabled: bool,
) -> egui::Response {
    let text_font = font::body();
    let galley = (!label.is_empty()).then(|| {
        ui.painter()
            .layout_no_wrap(label.to_string(), text_font, color::LABEL)
    });
    let pad = ui.spacing().button_padding;
    let mark = SHELF_R * 2.0 + 2.0;
    let text_w = galley.as_ref().map_or(0.0, |g| g.size().x + space::S2);
    let h = galley
        .as_ref()
        .map_or(mark, |g| g.size().y.max(mark))
        + pad.y * 2.0;
    let size = egui::vec2(mark + text_w + pad.x * 2.0, h);
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let visuals = if enabled {
        ui.style().interact(&resp)
    } else {
        &ui.style().visuals.widgets.inactive
    };
    ui.painter().rect(
        rect,
        radius::SM,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
    );
    let ink = if enabled {
        visuals.text_color()
    } else {
        visuals.text_color().gamma_multiply(0.5)
    };
    let c = egui::pos2(rect.left() + pad.x + mark * 0.5, rect.center().y);
    shelf(ui.painter(), c, SHELF_R, ink, present, list);
    if let Some(g) = galley {
        let pos = egui::pos2(c.x + mark * 0.5 + space::S2, rect.center().y - g.size().y * 0.5);
        ui.painter().galley(pos, g, ink);
    }
    resp
}

/// A square button carrying one text glyph, the exact size of a mark-only
/// [`shelf_button`], so the two sit side by side in a row at one height and
/// one width. Use it for the dig glyph beside a wantlist heart; a stock
/// `small_button` comes out a different box and the row reads as mismatched.
pub fn glyph_button(ui: &mut egui::Ui, glyph: &str, enabled: bool) -> egui::Response {
    let pad = ui.spacing().button_padding;
    let mark = SHELF_R * 2.0 + 2.0;
    let size = egui::vec2(mark + pad.x * 2.0, mark + pad.y * 2.0);
    let (rect, resp) = ui.allocate_exact_size(
        size,
        if enabled {
            egui::Sense::click()
        } else {
            egui::Sense::hover()
        },
    );
    let visuals = if enabled {
        ui.style().interact(&resp)
    } else {
        &ui.style().visuals.widgets.inactive
    };
    ui.painter().rect(
        rect,
        radius::SM,
        visuals.weak_bg_fill,
        visuals.bg_stroke,
    );
    let ink = if enabled {
        visuals.text_color()
    } else {
        visuals.text_color().gamma_multiply(0.5)
    };
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        glyph,
        font::body(),
        ink,
    );
    resp
}

/// A count beside its shelf mark, for a status line: the mark, then the number.
pub fn shelf_count(ui: &mut egui::Ui, list: VinylList, n: usize) {
    let text = n.to_string();
    let galley = ui
        .painter()
        .layout_no_wrap(text, font::body(), color::LABEL);
    let mark = SHELF_R * 2.0 + 2.0;
    let size = egui::vec2(mark + space::S2 + galley.size().x, galley.size().y.max(mark));
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let c = egui::pos2(rect.left() + mark * 0.5, rect.center().y);
    shelf(ui.painter(), c, SHELF_R, color::LABEL_2, true, list);
    ui.painter().galley(
        egui::pos2(c.x + mark * 0.5 + space::S2, rect.center().y - galley.size().y * 0.5),
        galley,
        color::LABEL,
    );
}
