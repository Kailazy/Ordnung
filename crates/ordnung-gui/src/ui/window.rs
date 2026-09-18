//! The app's floating window: the record sheet, every dialog, every panel
//! that opens over the library. One component, so they share one surface
//! (`glass`), one chrome, and one set of ways to be sized and placed.
//!
//! Sizing, placement and chrome are the configuration. The default is what
//! most dialogs want: sized to their content, not resizable, opening in
//! the centre of the screen every time (a window dragged aside comes back
//! to the centre on its next open), with a title bar and a close button. A
//! panel that grows with its list is `resizable_height`; one the user may
//! drag to size is `resizable`; a popover opens `at` a point, or `anchored`
//! to a screen edge. A window whose content carries its own heading turns
//! the `title_bar` off and keeps the close button, in the corner. None
//! collapse.

use super::glass;
use super::tokens::color;
use eframe::egui;

/// How the window is placed when it first opens (the user may drag it after).
enum Place {
    /// Centred on the screen, on every open.
    Center,
    /// Its `pivot` corner at this point.
    At(egui::Align2, egui::Pos2),
    /// Its `pivot` corner held at this point every frame; not draggable.
    Fixed(egui::Align2, egui::Pos2),
    /// Pinned to a screen edge, offset from it; not draggable.
    Anchor(egui::Align2, egui::Vec2),
}

pub struct Window<'o> {
    title: egui::WidgetText,
    id: Option<egui::Id>,
    open: Option<&'o mut bool>,
    title_bar: bool,
    resizable: [bool; 2],
    default_size: [Option<f32>; 2],
    min_size: [Option<f32>; 2],
    max_size: [Option<f32>; 2],
    auto_sized: bool,
    place: Place,
    inner_margin: Option<egui::Margin>,
    glass: Option<egui::Id>,
}

impl<'o> Window<'o> {
    pub fn new(title: impl Into<egui::WidgetText>) -> Self {
        Self {
            title: title.into(),
            id: None,
            open: None,
            title_bar: true,
            resizable: [false, false],
            default_size: [None, None],
            min_size: [None, None],
            max_size: [None, None],
            auto_sized: false,
            place: Place::Center,
            inner_margin: None,
            glass: None,
        }
    }

    /// An id apart from the title, for a window whose title changes (or
    /// that may be open more than once).
    pub fn id(mut self, id: egui::Id) -> Self {
        self.id = Some(id);
        self
    }

    /// Key the backdrop apart from the window id: for a window whose id
    /// changes with its content but whose opening can be seen coming (see
    /// `glass::prime`) before the content is known.
    pub fn glass_id(mut self, id: egui::Id) -> Self {
        self.glass = Some(id);
        self
    }

    /// Show a close button in the title bar; it clears the flag.
    pub fn open(mut self, open: &'o mut bool) -> Self {
        self.open = Some(open);
        self
    }

    /// No title bar. A popover, or a window whose content leads with its
    /// own heading; with [`Self::open`] set it still gets a close button,
    /// in the top-right corner of the content, which the content should
    /// leave clear ([`CLOSE_W`] wide).
    pub fn title_bar(mut self, title_bar: bool) -> Self {
        self.title_bar = title_bar;
        self
    }

    /// The user may drag it to size, both ways.
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = [resizable, resizable];
        self
    }

    /// Fixed width, height following the content up to a drag.
    pub fn resizable_height(mut self) -> Self {
        self.resizable = [false, true];
        self
    }

    pub fn default_width(mut self, w: f32) -> Self {
        self.default_size[0] = Some(w);
        self
    }

    pub fn default_size(mut self, size: impl Into<egui::Vec2>) -> Self {
        let size = size.into();
        self.default_size = [Some(size.x), Some(size.y)];
        self
    }

    pub fn min_width(mut self, w: f32) -> Self {
        self.min_size[0] = Some(w);
        self
    }

    pub fn min_height(mut self, h: f32) -> Self {
        self.min_size[1] = Some(h);
        self
    }

    pub fn max_width(mut self, w: f32) -> Self {
        self.max_size[0] = Some(w);
        self
    }

    pub fn max_height(mut self, h: f32) -> Self {
        self.max_size[1] = Some(h);
        self
    }

    pub fn min_size(mut self, size: impl Into<egui::Vec2>) -> Self {
        let size = size.into();
        self.min_size = [Some(size.x), Some(size.y)];
        self
    }

    /// Sized to the content every frame, never remembering a size.
    pub fn auto_sized(mut self) -> Self {
        self.auto_sized = true;
        self
    }

    /// Open with the `pivot` corner at `pos` (a popover under the control
    /// that opened it).
    pub fn at(mut self, pivot: egui::Align2, pos: egui::Pos2) -> Self {
        self.place = Place::At(pivot, pos);
        self
    }

    /// Held with the `pivot` corner at `pos` every frame (a panel that
    /// follows the control it edits); the user can't drag it.
    pub fn fixed_at(mut self, pivot: egui::Align2, pos: egui::Pos2) -> Self {
        self.place = Place::Fixed(pivot, pos);
        self
    }

    /// Pinned to a screen edge; the user can't drag it.
    pub fn anchored(mut self, align: egui::Align2, offset: impl Into<egui::Vec2>) -> Self {
        self.place = Place::Anchor(align, offset.into());
        self
    }

    /// A tighter or looser content inset than the standard window margin.
    pub fn inner_margin(mut self, margin: egui::Margin) -> Self {
        self.inner_margin = Some(margin);
        self
    }

    /// Draw the window. `None` when it isn't drawn this pass: closed, or
    /// holding for its backdrop (see `glass`).
    pub fn show<R>(
        self,
        ctx: &egui::Context,
        contents: impl FnOnce(&mut egui::Ui) -> R,
    ) -> Option<egui::InnerResponse<Option<R>>> {
        if matches!(self.open, Some(false)) {
            return None;
        }
        let id = self
            .id
            .unwrap_or_else(|| egui::Id::new(self.title.text()));
        let glass_id = self.glass.unwrap_or(id);
        if !glass::ready(ctx, glass_id) {
            ctx.request_repaint();
            return None;
        }
        let opening = glass::opening(ctx, glass_id);
        let style = ctx.style();
        let rounding = style.visuals.window_rounding;
        let stroke = style.visuals.window_stroke;
        // The frame paints its shadow and keeps its shape; the glass is
        // its fill, painted under the content in the window's layer.
        let mut frame = egui::Frame::window(&style)
            .fill(egui::Color32::TRANSPARENT)
            .stroke(egui::Stroke::NONE);
        if let Some(m) = self.inner_margin {
            frame = frame.inner_margin(m);
        }
        let mut w = egui::Window::new(self.title)
            .id(id)
            .collapsible(false)
            .title_bar(self.title_bar)
            .frame(frame)
            .resizable(self.resizable);
        let mut open = self.open;
        let corner_close = !self.title_bar && open.is_some();
        if let Some(open) = open.as_deref_mut() {
            w = w.open(open);
        }
        match (self.default_size[0], self.default_size[1]) {
            (Some(x), Some(y)) => w = w.default_size([x, y]),
            (Some(x), None) => w = w.default_width(x),
            (None, Some(y)) => w = w.default_height(y),
            (None, None) => {}
        }
        if let Some(x) = self.min_size[0] {
            w = w.min_width(x);
        }
        if let Some(y) = self.min_size[1] {
            w = w.min_height(y);
        }
        if let Some(x) = self.max_size[0] {
            w = w.max_width(x);
        }
        if let Some(y) = self.max_size[1] {
            w = w.max_height(y);
        }
        if self.auto_sized {
            w = w.auto_sized();
        }
        w = match self.place {
            // Centred on the pass it opens (`current_pos` overrides the
            // place egui remembers for it), free to drag after.
            Place::Center if opening => w
                .pivot(egui::Align2::CENTER_CENTER)
                .current_pos(ctx.screen_rect().center()),
            Place::Center => w
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center()),
            Place::At(pivot, pos) => w.pivot(pivot).default_pos(pos),
            Place::Fixed(pivot, pos) => w.pivot(pivot).fixed_pos(pos),
            Place::Anchor(align, offset) => w.anchor(align, offset),
        };
        let mut slot = None;
        let mut closed = false;
        let shown = w.show(ctx, |ui| {
            slot = Some(glass::begin(ui));
            let r = contents(ui);
            // After the content, not before: the chrome is placed from
            // what the content took (`min_rect`), which is what the frame
            // wraps. The rect egui offers before layout (`max_rect`) is its
            // remembered desired width, which never shrinks, so it can run
            // past the frame and put the button on the edge. Last also
            // puts it above anything the content drew in the corner.
            if corner_close {
                closed = close_button(ui, id);
            }
            r
        });
        if let (Some(shown), Some(slot)) = (&shown, slot) {
            glass::end(ctx, slot, glass_id, shown.response.rect, rounding, stroke);
            glass::drawn(ctx, glass_id);
        }
        if closed {
            if let Some(open) = open {
                *open = false;
            }
        }
        shown
    }
}

/// Width the close button takes at the top-right of a window without a
/// title bar; content on that row stops short of it.
pub const CLOSE_W: f32 = 28.0;

/// The close button of a window without a title bar: a cross in the
/// top-right corner of the content, taking no space from the layout. Call
/// after the content: it sits on the content's own extent.
fn close_button(ui: &mut egui::Ui, id: egui::Id) -> bool {
    let r = ui.min_rect();
    let side = ui.spacing().interact_size.y;
    let rect = egui::Rect::from_min_size(
        egui::pos2(r.right() - side, r.top() - 2.0),
        egui::vec2(side, side),
    );
    let resp = ui.interact(rect, id.with("corner-close"), egui::Sense::click());
    let visuals = ui.style().interact(&resp);
    let cross = rect.shrink(rect.width() * 0.32);
    let stroke = visuals.fg_stroke;
    ui.painter()
        .line_segment([cross.left_top(), cross.right_bottom()], stroke);
    ui.painter()
        .line_segment([cross.right_top(), cross.left_bottom()], stroke);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// The glass's edge, for a surface that isn't a window (a menu, a popup)
/// and wants the same hairline.
pub fn edge() -> egui::Stroke {
    egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE)
}
