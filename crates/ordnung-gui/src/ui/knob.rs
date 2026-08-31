//! The toolbar's master volume knob.
//!
//! A rotary dial rather than a slider, the way rekordbox's master level reads:
//! a small round control is legible at a glance in a dense toolbar, and its
//! footprint doesn't change with the value the way a labelled slider's does.
//!
//! The dial sweeps 270° with the dead zone at the bottom (the convention on
//! physical gear — a pointer straight down is unambiguously "off", and the gap
//! makes min and max distinguishable instead of meeting at the same angle).

use super::hover::HoverNoteExt;
use super::tokens::color;

/// Diameter of the knob's dial, and the size it allocates.
const SIZE: f32 = 26.0;
/// Where the sweep starts and ends, in radians clockwise from straight up.
/// ±135° leaves a 90° dead zone centred on the bottom.
const SWEEP: f32 = 2.356_194_5; // 135° in radians
/// Radius of the arc drawn around the dial, and its thickness.
const ARC_R: f32 = 11.0;
const ARC_W: f32 = 2.5;
/// Vertical drag distance, in points, that covers the full 0→1 range. Chosen so
/// a comfortable ~100px gesture spans everything, with fine control inside it.
const DRAG_SPAN: f32 = 120.0;
/// Step applied per scroll-wheel notch.
const SCROLL_STEP: f32 = 0.05;

/// Angle (radians, clockwise from straight up) for a `0.0`–`1.0` value.
fn angle_for(v: f32) -> f32 {
    -SWEEP + v.clamp(0.0, 1.0) * (2.0 * SWEEP)
}

/// Point on a circle of radius `r` around `c` at `angle` clockwise from up.
fn on_circle(c: egui::Pos2, r: f32, angle: f32) -> egui::Pos2 {
    egui::pos2(c.x + r * angle.sin(), c.y - r * angle.cos())
}

/// Paint an arc from `from` to `to` (both clockwise-from-up radians) as a
/// polyline. egui 0.29 has no arc primitive, so the curve is sampled; the step
/// is fine enough that the segments read as smooth at this radius.
fn arc(p: &egui::Painter, c: egui::Pos2, r: f32, from: f32, to: f32, stroke: egui::Stroke) {
    const STEP: f32 = 0.09; // ~5° per segment
    let n = (((to - from).abs() / STEP).ceil() as usize).max(1);
    let pts: Vec<egui::Pos2> = (0..=n)
        .map(|i| on_circle(c, r, from + (to - from) * (i as f32 / n as f32)))
        .collect();
    p.add(egui::Shape::line(pts, stroke));
}

/// Draw the master volume knob and return the new value when the user changed
/// it, or `None` when they didn't.
///
/// Drag vertically to set the level, scroll over it to nudge, or double-click
/// to return to unity. The percentage rides in the hover note rather than a
/// permanent label, so the control keeps a fixed width in the toolbar.
pub fn volume(ui: &mut egui::Ui, value: f32) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(
        egui::vec2(SIZE, SIZE),
        egui::Sense::click_and_drag(),
    );
    let value = value.clamp(0.0, 1.0);
    let mut changed = None;

    // Vertical drag: up raises, down lowers. `drag_delta` is used rather than an
    // absolute pointer position so the knob picks up from wherever it currently
    // sits, instead of jumping to match where the pointer grabbed it.
    if resp.dragged() {
        let dy = resp.drag_delta().y;
        if dy != 0.0 {
            changed = Some((value - dy / DRAG_SPAN).clamp(0.0, 1.0));
        }
    }
    // Scroll only while the pointer is over the knob, so a wheel gesture meant
    // for the list underneath never lands here.
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let base = changed.unwrap_or(value);
            changed = Some((base + scroll.signum() * SCROLL_STEP).clamp(0.0, 1.0));
        }
    }
    // Double-click resets to unity — the standard escape from a knob nudged off
    // full without having to drag it back by eye.
    if resp.double_clicked() {
        changed = Some(1.0);
    }

    let shown = changed.unwrap_or(value);
    let c = rect.center();
    let painter = ui.painter();
    let active = resp.hovered() || resp.dragged();

    // Dial body, then the full sweep as an unfilled track, then the filled arc
    // up to the current value.
    painter.circle_filled(c, SIZE / 2.0 - 3.0, color::SURFACE_HI);
    arc(
        painter,
        c,
        ARC_R,
        -SWEEP,
        SWEEP,
        egui::Stroke::new(ARC_W, color::SURFACE_ACTIVE),
    );
    // Muted reads as a warning rather than an accent: a knob at zero is a
    // reason the app is silent, and should say so at a glance.
    let fill = if shown <= 0.0 {
        color::LABEL_4
    } else if active {
        color::ACCENT_HOVER
    } else {
        color::ACCENT
    };
    if shown > 0.0 {
        arc(
            painter,
            c,
            ARC_R,
            -SWEEP,
            angle_for(shown),
            egui::Stroke::new(ARC_W, fill),
        );
    }
    // Pointer line from the dial's centre out to its edge.
    let a = angle_for(shown);
    painter.line_segment(
        [on_circle(c, 2.5, a), on_circle(c, SIZE / 2.0 - 4.5, a)],
        egui::Stroke::new(2.0, if active { color::LABEL } else { color::LABEL_2 }),
    );

    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeVertical);
    }
    resp.on_hover_note(format!(
        "Master volume {}%. Drag or scroll to set, double-click for 100%",
        (shown * 100.0).round() as i32
    ));

    changed
}
