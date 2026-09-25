//! The player's pitch fader.
//!
//! A Technics SL-1200's pitch control laid on its side: a slot with a
//! tick every 2% and a taller one at zero, a cap with an index line, ±8%
//! end to end. Like the MK5's fader it has a quartz-lock zone at the centre:
//! within a hair of zero the value snaps to exactly zero and a green lamp
//! lights at the centre tick, so "back to normal speed" is a place the hand
//! can find, not a number to aim at. The percent and the resulting BPM read
//! out beneath the slot, the way a CDJ's tempo display sits by its slider.
//!
//! The fader only reports; how the rate is applied (turntable or key lock)
//! is the engine's business (see `audio::PitchState`).

use super::tokens::{color, font, radius};

/// Footprint: as tall as the transport's hand-drawn controls beside it.
pub const W: f32 = 150.0;
pub const H: f32 = 36.0;
/// The slot's inset from the footprint's sides, so the cap at either end
/// stays inside the rect.
const SLOT_INSET: f32 = 8.0;
/// Where the slot sits vertically, and its thickness.
const SLOT_Y: f32 = 11.0;
const SLOT_W: f32 = 3.0;
/// The cap.
const CAP: egui::Vec2 = egui::vec2(9.0, 16.0);
/// Below this many percent of zero the fader is quartz-locked at zero.
const LOCK_ZONE_PCT: f32 = 0.25;
/// Step applied per scroll-wheel notch, in percent.
const SCROLL_STEP_PCT: f32 = 0.1;

/// Draw the pitch fader at `value` percent over `±range` and return the new
/// value when the user moved it, or `None`. `bpm` is the track's tempo at
/// zero, for the readout; `None` leaves that half of the readout blank.
///
/// Drag or click along the slot to set, scroll over it to nudge, or
/// double-click to return to zero.
pub fn pitch(ui: &mut egui::Ui, value: f32, range: f32, bpm: Option<f32>) -> Option<f32> {
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(W, H), egui::Sense::click_and_drag());
    let range = range.max(0.1);
    let value = value.clamp(-range, range);
    let (x0, x1) = (rect.left() + SLOT_INSET, rect.right() - SLOT_INSET);
    let x_for = |v: f32| x0 + (v / range * 0.5 + 0.5) * (x1 - x0);
    let v_for = |x: f32| (((x - x0) / (x1 - x0)).clamp(0.0, 1.0) * 2.0 - 1.0) * range;
    let quartz = |v: f32| if v.abs() < LOCK_ZONE_PCT { 0.0 } else { v };

    let mut changed = None;
    // The cap goes where the hand is: grabbing anywhere along the slot moves
    // it there, and a drag then rides along.
    if resp.dragged() || resp.drag_started() || resp.clicked() {
        if let Some(p) = resp.interact_pointer_pos() {
            let v = quartz(v_for(p.x));
            if v != value {
                changed = Some(v);
            }
        }
    }
    if resp.hovered() {
        let scroll = ui.input(|i| i.smooth_scroll_delta.y);
        if scroll != 0.0 {
            let base = changed.unwrap_or(value);
            changed = Some((base + scroll.signum() * SCROLL_STEP_PCT).clamp(-range, range));
        }
    }
    if resp.double_clicked() {
        changed = Some(0.0);
    }

    let shown = changed.unwrap_or(value);
    let active = resp.hovered() || resp.dragged();
    let p = ui.painter();
    let slot_y = rect.top() + SLOT_Y;

    // The slot, its ticks, and the quartz-lock lamp at the centre tick.
    p.line_segment(
        [egui::pos2(x0, slot_y), egui::pos2(x1, slot_y)],
        egui::Stroke::new(SLOT_W, color::FIELD),
    );
    let ticks = (range / 2.0).round().max(1.0) as i32;
    for i in -ticks..=ticks {
        let v = i as f32 * range / ticks as f32;
        let x = x_for(v);
        let (len, ink) = if i == 0 {
            (7.0, color::LABEL_3)
        } else {
            (4.0, color::LABEL_4)
        };
        p.line_segment(
            [egui::pos2(x, slot_y + 4.0), egui::pos2(x, slot_y + 4.0 + len)],
            egui::Stroke::new(1.0, ink),
        );
    }
    let lamp = if shown == 0.0 { color::GREEN } else { color::SURFACE_ACTIVE };
    p.circle_filled(egui::pos2(x_for(0.0), slot_y - 6.0), 1.8, lamp);

    // The cap with its index line.
    let cap = egui::Rect::from_center_size(egui::pos2(x_for(shown), slot_y), CAP);
    p.rect_filled(
        cap,
        egui::Rounding::same(radius::XS / 2.0),
        if active { color::SURFACE_HOVER } else { color::SURFACE_HI },
    );
    p.rect_stroke(
        cap,
        egui::Rounding::same(radius::XS / 2.0),
        egui::Stroke::new(1.0, if active { color::OUTLINE_HOVER } else { color::OUTLINE }),
    );
    p.line_segment(
        [
            egui::pos2(cap.center().x, cap.top() + 3.0),
            egui::pos2(cap.center().x, cap.bottom() - 3.0),
        ],
        egui::Stroke::new(1.5, if active { color::LABEL } else { color::LABEL_2 }),
    );

    // Readout: percent on the left, the tempo it makes on the right.
    let text_y = rect.bottom() - 4.0;
    p.text(
        egui::pos2(x0, text_y),
        egui::Align2::LEFT_BOTTOM,
        format!("{shown:+.1}%"),
        font::mono_small(),
        if shown == 0.0 { color::LABEL_3 } else { color::LABEL },
    );
    if let Some(bpm) = bpm.filter(|b| *b > 0.0) {
        p.text(
            egui::pos2(x1, text_y),
            egui::Align2::RIGHT_BOTTOM,
            format!("{:.1} BPM", bpm * (1.0 + shown / 100.0)),
            font::mono_small(),
            color::LABEL_2,
        );
    }

    if active {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fader paints and reports nothing when untouched, at any value.
    #[test]
    fn idle_fader_reports_no_change() {
        let ctx = egui::Context::default();
        for v in [-8.0, -0.1, 0.0, 3.3, 8.0] {
            let _ = ctx.run(Default::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    assert_eq!(pitch(ui, v, 8.0, Some(120.0)), None);
                });
            });
        }
    }
}
