//! The sidebar: a window docked to a screen edge. The track inspector is
//! one. It is built from the window's parts so the two read as one kind
//! of surface: the same tint (the glass's, laid over the app's ground; a
//! docked panel has nothing under it to frost), the same hairline for an
//! edge, the same content margin, and one heading style for the panel's
//! own title and for the sections inside it. A sidebar slides rather than
//! opens: the caller eases its width, and at zero it is not shown at all.
//!
//! [`header`] is the panel's title line, where a window's title bar is:
//! the caption at the left and room at the right for one action, a button
//! the edge never cuts. [`section`] opens each titled block below it with
//! the same caption over a hairline; [`rule`] is that hairline on its own.

use super::tokens::{color, font, space};
use super::window;
use eframe::egui;

/// The surface: the glass tint flattened onto the app's ground.
pub fn fill() -> egui::Color32 {
    over(color::BG, color::SURFACE_GLASS)
}

/// `over` (premultiplied) composited on `under`, opaque.
fn over(under: egui::Color32, over: egui::Color32) -> egui::Color32 {
    let keep = 1.0 - over.a() as f32 / 255.0;
    let ch = |u: u8, o: u8| (o as f32 + u as f32 * keep).round().clamp(0.0, 255.0) as u8;
    egui::Color32::from_rgb(
        ch(under.r(), over.r()),
        ch(under.g(), over.g()),
        ch(under.b(), over.b()),
    )
}

/// The content margin a sidebar shares with a window, less the top: the
/// panel's first line is its [`header`], which lays its own gap down the
/// way a window's title bar does.
fn content_margin(ctx: &egui::Context) -> egui::Margin {
    let m = egui::Frame::window(&ctx.style()).inner_margin;
    egui::Margin {
        left: m.left,
        right: m.right,
        top: 0.0,
        bottom: m.bottom,
    }
}

/// A sidebar docked to the right edge.
pub struct Sidebar {
    id: egui::Id,
    width: f32,
}

impl Sidebar {
    pub fn right(id: impl Into<egui::Id>) -> Self {
        Self {
            id: id.into(),
            width: 0.0,
        }
    }

    /// The width this frame; ease it to slide the panel. Under half a
    /// point it is not shown.
    pub fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    pub fn show<R>(
        self,
        ctx: &egui::Context,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<R>> {
        if self.width < 0.5 {
            return None;
        }
        let margin = content_margin(ctx);
        let shown = egui::SidePanel::right(self.id)
            .resizable(false)
            .exact_width(self.width)
            // The edge is the window's hairline, painted below: egui's own
            // divider is a different line.
            .show_separator_line(false)
            .frame(egui::Frame::none().fill(fill()).inner_margin(margin))
            .show(ctx, |ui| {
                let inner = ui.max_rect();
                let outer = egui::Rect::from_min_max(
                    inner.min - egui::vec2(margin.left, margin.top),
                    inner.max + egui::vec2(margin.right, margin.bottom),
                );
                ui.painter()
                    .vline(outer.left(), outer.y_range(), window::edge());
                // The content is laid out at its natural width, and a label
                // wider than the panel would otherwise widen it and paint
                // over what is beside it. The panel is the clip.
                ui.set_clip_rect(outer.intersect(ui.clip_rect()));
                ui.set_max_width(ui.available_width());
                add(ui)
            });
        Some(shown)
    }
}

/// The panel's title line, where a window's title bar is: the caption at
/// the left, and `add_action` laid out right-to-left from the panel's
/// inner edge, so a button there ends at the content's edge and the
/// caption is cut before it reaches the button.
pub fn header(ui: &mut egui::Ui, title: &str, add_action: impl FnOnce(&mut egui::Ui)) {
    ui.add_space(egui::Frame::window(ui.style()).inner_margin.top);
    caption_row(ui, title, add_action);
    ui.add_space(window::TITLE_CLEARANCE);
}

/// One titled block: the caption over a hairline, then the body. Much
/// more space above than below: a heading belongs to what follows it, and
/// equal gaps are what make a long column read as one undifferentiated
/// list.
pub fn section(ui: &mut egui::Ui, title: &str, add_body: impl FnOnce(&mut egui::Ui)) {
    section_with_action(ui, title, |_| {}, add_body);
}

/// [`section`] with a control at the right of its caption (a block's Edit
/// toggle), on the caption's own line.
pub fn section_with_action(
    ui: &mut egui::Ui,
    title: &str,
    add_action: impl FnOnce(&mut egui::Ui),
    add_body: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(space::S6);
    caption_row(ui, title, add_action);
    ui.add_space(space::S2);
    rule(ui);
    ui.add_space(space::S3);
    add_body(ui);
}

/// The caption, small capitals in the tertiary ink, and the action at the
/// row's right end. One row for the header and every section, so the
/// panel's title is set the way its sections are.
fn caption_row(ui: &mut egui::Ui, title: &str, add_action: impl FnOnce(&mut egui::Ui)) {
    let h = ui.spacing().interact_size.y.max(window::CLOSE_SIDE);
    let (row, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::hover());
    // The action first, from the right, so the caption is capped to what
    // it leaves.
    let mut action = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(row)
            .layout(egui::Layout::right_to_left(egui::Align::Center)),
    );
    add_action(&mut action);
    let taken = row.right() - action.min_rect().left();
    let cap = if taken > 0.0 && taken.is_finite() {
        row.width() - taken - space::S3
    } else {
        row.width()
    };
    let galley = ui.fonts(|f| {
        let mut job = egui::text::LayoutJob::simple_singleline(
            title.to_uppercase(),
            font::strong(font::caption().size),
            color::LABEL_3,
        );
        job.wrap.max_width = cap.max(0.0);
        job.wrap.max_rows = 1;
        job.wrap.break_anywhere = true;
        f.layout_job(job)
    });
    ui.painter().galley(
        egui::pos2(row.left(), row.center().y - galley.size().y / 2.0),
        galley,
        color::LABEL_3,
    );
}

/// The hairline under a caption, across the content.
pub fn rule(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(rect, 0.0, color::SEPARATOR_OPAQUE);
}

/// A hairline across the whole panel, margin to margin, dividing one part
/// of it from the next (a pinned head from what scrolls beneath it).
pub fn rule_full(ui: &mut egui::Ui) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 1.0), egui::Sense::hover());
    ui.painter().hline(
        ui.clip_rect().x_range(),
        rect.center().y,
        egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The header's action ends at the content's edge, inside the panel,
    /// and the caption stops short of it.
    #[test]
    fn header_action_ends_inside_the_panel() {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let mut button = None;
        let mut content_right = 0.0;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            Sidebar::right("side").width(320.0).show(ctx, |ui| {
                content_right = ui.max_rect().right();
                header(ui, "Track", |ui| {
                    button = Some(ui.button("View release").rect);
                });
            });
        });
        let button = button.expect("the action was laid out");
        assert!((button.right() - content_right).abs() < 0.5, "{button:?} vs {content_right}");
        assert!(button.left() > 0.0);
    }
}
