//! The avatar chip: a round face and a name in a pill. A person (an artist,
//! a seller) where a square cover would read as a record.
//!
//! One shape wherever a person is offered: the pill is [`H`] tall, the face
//! sits at its left edge with the pill's own inset around it, and the name
//! gets room on both sides, a gap from the face and a wider inset at the
//! end, so it sits in the pill rather than against its rim. Sizes are here
//! and nowhere else.
//!
//! Chips are measured before they are placed ([`measure`]), because the row
//! that offers them fits as many as its width takes and draws the rest not
//! at all; [`show`] then paints one at a point.

use super::tokens::{color, font, space};
use crate::tex::Tex;
use eframe::egui;
use std::sync::Arc;

/// Height of the pill.
pub const H: f32 = 36.0;
/// Side of the round face.
const FACE: f32 = 26.0;
/// Inset from the pill's edge to the face.
const PAD_FACE: f32 = (H - FACE) / 2.0;
/// Gap from the face to the name.
const GAP: f32 = space::S3;
/// Inset from the name to the pill's end.
const PAD_NAME: f32 = space::S4;

/// How fast the hover fill comes and goes.
const HOT_ANIM: f32 = 0.11;

/// A chip's name laid out, and the width the pill needs for it.
pub struct Chip {
    name: Arc<egui::Galley>,
    pub width: f32,
}

/// Lay out the name, truncated past `max_name_w`, and size the pill.
pub fn measure(ui: &egui::Ui, name: &str, max_name_w: f32) -> Chip {
    let name = ui.fonts(|f| {
        let mut job =
            egui::text::LayoutJob::simple_singleline(name.to_string(), font::body(), color::LABEL);
        job.wrap.max_width = max_name_w;
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = false;
        f.layout_job(job)
    });
    let width = PAD_FACE + FACE + GAP + name.size().x + PAD_NAME;
    Chip { name, width }
}

/// Paint the chip with its top-left corner at `min`. `face` is the portrait,
/// or a placeholder note when there is none; `selected` lights it as the
/// hover does (a keyboard cursor).
pub fn show(
    ui: &mut egui::Ui,
    chip: &Chip,
    min: egui::Pos2,
    id: egui::Id,
    face: Option<&Tex>,
    selected: bool,
) -> egui::Response {
    let rect = egui::Rect::from_min_size(min, egui::vec2(chip.width, H));
    let resp = ui.interact(rect, id, egui::Sense::click());
    let hot = ui
        .ctx()
        .animate_bool_with_time(resp.id.with("hot"), selected || resp.hovered(), HOT_ANIM);
    let rounding = egui::Rounding::same(H / 2.0);
    ui.painter().rect_filled(rect, rounding, color::SURFACE_HI);
    if hot > 0.0 {
        ui.painter().rect_filled(
            rect,
            rounding,
            ui.visuals().widgets.hovered.weak_bg_fill.gamma_multiply(hot),
        );
    }
    let face_rect = egui::Rect::from_center_size(
        egui::pos2(rect.left() + PAD_FACE + FACE / 2.0, rect.center().y),
        egui::vec2(FACE, FACE),
    );
    let face_round = egui::Rounding::same(FACE / 2.0);
    match face {
        Some(tex) => {
            egui::Image::new(tex).rounding(face_round).paint_at(ui, face_rect);
        }
        None => {
            ui.painter().rect_filled(face_rect, face_round, color::FIELD);
            ui.painter().text(
                face_rect.center(),
                egui::Align2::CENTER_CENTER,
                "♪",
                font::footnote(),
                color::LABEL_3,
            );
        }
    }
    ui.painter().galley(
        egui::pos2(
            face_rect.right() + GAP,
            rect.center().y - chip.name.size().y / 2.0,
        ),
        chip.name.clone(),
        color::LABEL,
    );
    resp.on_hover_cursor(egui::CursorIcon::PointingHand)
}
