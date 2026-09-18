//! The app's floating window: the record sheet, every dialog, every panel
//! that opens over the library. One component, so they share one surface
//! (`glass`), one chrome, and one set of ways to be sized and placed.
//!
//! Sizing, placement and chrome are the configuration. The default is what
//! most dialogs want: sized to their content, not resizable, opening in
//! the centre of the screen every time (a window dragged aside comes back
//! to the centre on its next open), with a title bar and a close button. A
//! panel that grows with its list is `resizable_height`; one the user may
//! drag to size is `resizable`, by its bottom-right corner unless its
//! `grips` say otherwise; a popover opens `at` a point, or `anchored`
//! to a screen edge. A window whose content carries its own heading turns
//! the `title_bar` off and keeps the close button, in the corner. None
//! collapse.
//!
//! The title bar and the close button are the component's, not egui's.
//! The window is as wide as its content: the title never widens it past
//! the width the content or its `default_width` asked for, and a title
//! longer than that is cut with an ellipsis, centred and clear of the
//! close button on both sides. (egui's own bar widened the frame to the
//! title and left the content, and the close on its edge, short of the
//! corner.) The close is one mark (the same cross every close in the app
//! is drawn with, see `icon`), one size, and one place, the top-right
//! corner of the frame. With a title bar it sits on the title's line;
//! without one, on the content's first row of controls, so a button placed
//! up there (short of [`CLOSE_W`]) and the cross share a centre line.

use super::glass;
use super::icon;
use super::tokens::{color, space};
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

/// Where the user can grab a resizable window to size it: any set of its
/// edges and corners. A corner sizes both ways, an edge one. The default
/// is the bottom-right corner alone, which is where a window is expected
/// to be sized from; a window sized only in height offers its bottom edge.
/// The top edge is never a default: egui sizes from the top by moving the
/// top and keeping the height when the content can't shrink, which drags
/// the whole window rather than sizing it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Grips(u8);

#[allow(dead_code)] // the set is the API; callers pick from it
impl Grips {
    pub const NONE: Self = Self(0);
    pub const LEFT: Self = Self(1);
    pub const RIGHT: Self = Self(2);
    pub const TOP: Self = Self(4);
    pub const BOTTOM: Self = Self(8);
    pub const TOP_LEFT: Self = Self(16);
    pub const TOP_RIGHT: Self = Self(32);
    pub const BOTTOM_LEFT: Self = Self(64);
    pub const BOTTOM_RIGHT: Self = Self(128);
    pub const EDGES: Self = Self(15);
    pub const CORNERS: Self = Self(240);
    pub const ALL: Self = Self(255);

    pub const fn with(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub const fn has(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl std::ops::BitOr for Grips {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        self.with(rhs)
    }
}

/// Half the side of the square a corner grip answers to: a little more
/// than egui's own, so the corner is caught without aiming for the pixel.
/// Set into the style by `theme`, read back from it here so the blockers
/// below cover exactly what egui offers.
pub const CORNER_GRIP: f32 = 14.0;

pub struct Window<'o> {
    title: egui::WidgetText,
    id: Option<egui::Id>,
    open: Option<&'o mut bool>,
    title_bar: bool,
    resizable: [bool; 2],
    grips: Option<Grips>,
    default_size: [Option<f32>; 2],
    min_size: [Option<f32>; 2],
    max_size: [Option<f32>; 2],
    auto_sized: bool,
    place: Place,
    inner_margin: Option<egui::Margin>,
    glass: Option<egui::Id>,
    spawn: Option<egui::Id>,
}

impl<'o> Window<'o> {
    pub fn new(title: impl Into<egui::WidgetText>) -> Self {
        Self {
            title: title.into(),
            id: None,
            open: None,
            title_bar: true,
            resizable: [false, false],
            grips: None,
            default_size: [None, None],
            min_size: [None, None],
            max_size: [None, None],
            auto_sized: false,
            place: Place::Center,
            inner_margin: None,
            glass: None,
            spawn: None,
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

    /// Show a close button (in the title bar, or the corner without one);
    /// it clears the flag.
    pub fn open(mut self, open: &'o mut bool) -> Self {
        self.open = Some(open);
        self
    }

    /// No title bar. A popover, or a window whose content leads with its
    /// own heading; with [`Self::open`] set it still gets a close button,
    /// in the top-right corner of the content on its first row, which the
    /// content should leave clear ([`CLOSE_W`] wide).
    pub fn title_bar(mut self, title_bar: bool) -> Self {
        self.title_bar = title_bar;
        self
    }

    /// The user may drag it to size, both ways, by the bottom-right corner
    /// (or the [`Self::grips`] given). Such a window is always the size it
    /// was given, whatever its content claims: the frame covers the whole
    /// of it, so content laid out in the offered rect (panels, a table)
    /// never draws past the glass.
    pub fn resizable(mut self, resizable: bool) -> Self {
        self.resizable = [resizable, resizable];
        self
    }

    /// Fixed width, height following the content up to a drag on the
    /// bottom edge (or the [`Self::grips`] given).
    pub fn resizable_height(mut self) -> Self {
        self.resizable = [false, true];
        self
    }

    /// Which edges and corners size the window, for one that is resizable.
    #[allow(dead_code)]
    pub fn grips(mut self, grips: Grips) -> Self {
        self.grips = Some(grips);
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

    /// One of a family of centred windows (the record sheets, one per
    /// record) that open where the family was last left: centred, until
    /// the user drags one somewhere, then every one opens with its top-left
    /// corner there for the rest of the session.
    pub fn spawn_with(mut self, family: egui::Id) -> Self {
        self.spawn = Some(family);
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
        // The title bar is a row of the content (see `title_bar` below):
        // the title's line, centred in its strip by the frame's top margin
        // repeated under it, the hairline, and the content's clearance
        // from the hairline.
        let margin = frame.inner_margin;
        let title_gap = if self.title_bar {
            margin.top + TITLE_CLEARANCE
        } else {
            0.0
        };
        let title = self.title;
        // egui grabs every edge and corner of a resizable window. The
        // grips this window doesn't offer are covered by blockers (see
        // `block_grips`), and a drag on a blocker moves the window, as a
        // drag on the frame there would without them.
        let grips = self.grips.unwrap_or(if self.resizable[0] {
            Grips::BOTTOM_RIGHT
        } else {
            Grips::BOTTOM
        });
        let last_rect = egui::AreaState::load(ctx, id).map(|s| s.rect());
        let moved = BLOCKERS
            .iter()
            .flat_map(|key| HALVES.map(|half| blocker_id(id, key, half)))
            .filter_map(|bid| ctx.read_response(bid))
            .filter(|r| r.dragged())
            .fold(egui::Vec2::ZERO, |acc, r| acc + r.drag_delta());
        // egui sees neither the title nor `open`: both are drawn here, in
        // the content, so the title can't widen the frame past the content
        // and the close is in the frame's corner whether there's a title
        // bar or not. (egui's bar also registers a double-click widget
        // across the title's line that would take the close's click.) The
        // flag is cleared below from the click.
        let mut w = egui::Window::new(egui::WidgetText::default())
            .id(id)
            .collapsible(false)
            .title_bar(false)
            .frame(frame)
            .resizable(self.resizable);
        let open = self.open;
        let close = open.is_some();
        if self.auto_sized {
            w = w.auto_sized();
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
        // egui keeps a window's pivot where it is as the size changes.
        // That is the placement's pivot while the content sets the size
        // (a sheet whose tracklist arrives stays centred), and the top-left
        // corner while the user drags a grip: any other pivot then mirrors
        // the resize, the top-left corner moving out as the bottom-right is
        // dragged out. Either way the pivot is put back each pass from
        // where the window was last pass, so switching between them doesn't
        // move it; a drag on a grip blocker is added.
        // Opening: the pass it first draws, whether the backdrop's (the
        // family of sheets shares one, so a sheet swapped in over another
        // is opening too) or its own.
        let pass = ctx.cumulative_pass_nr();
        let drawn_id = id.with("drawn-pass");
        let opening = opening
            || ctx
                .data(|d| d.get_temp::<u64>(drawn_id))
                .map_or(true, |p| p + 1 < pass);
        // The family's remembered corner, once one of them has been moved.
        let spawn = self.spawn;
        let remembered = spawn.and_then(|f| ctx.data(|d| d.get_temp::<egui::Pos2>(f)));
        let held = if opening { None } else { last_rect };
        let resizing = held.is_some() && grip_dragged(ctx, id);
        w = match (self.place, held) {
            (Place::Center | Place::At(..), Some(r)) if resizing => {
                w.pivot(egui::Align2::LEFT_TOP).current_pos(r.left_top())
            }
            // Held by the corner it opened at, once the family has one.
            (Place::Center, Some(r)) if remembered.is_some() => w
                .pivot(egui::Align2::LEFT_TOP)
                .current_pos(r.left_top() + moved),
            (Place::Center, Some(r)) => w
                .pivot(egui::Align2::CENTER_CENTER)
                .current_pos(r.center() + moved),
            (Place::At(pivot, _), Some(r)) => {
                w.pivot(pivot).current_pos(pivot.pos_in_rect(&r) + moved)
            }
            // Where the family was left, on the pass it opens.
            (Place::Center, None) if opening && remembered.is_some() => w
                .pivot(egui::Align2::LEFT_TOP)
                .current_pos(remembered.unwrap()),
            // Centred on the pass it opens (`current_pos` overrides the
            // place egui remembers for it), free to drag after.
            (Place::Center, None) if opening => w
                .pivot(egui::Align2::CENTER_CENTER)
                .current_pos(ctx.screen_rect().center()),
            (Place::Center, None) => w
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center()),
            (Place::At(pivot, pos), None) => w.pivot(pivot).default_pos(pos),
            (Place::Fixed(pivot, pos), _) => w.pivot(pivot).fixed_pos(pos),
            (Place::Anchor(align, offset), _) => w.anchor(align, offset),
        };
        let mut slot = None;
        let mut closed = false;
        let resizable = self.resizable;
        let with_title_bar = self.title_bar;
        let shown = w.show(ctx, |ui| {
            slot = Some(glass::begin(ui));
            // Before the content, so the content's own controls stay on
            // top of the blockers where they overlap the frame's edge.
            if let Some(rect) = last_rect.filter(|_| resizable[0] || resizable[1]) {
                block_grips(ui, id, rect, resizable, grips);
            }
            let bar = with_title_bar.then(|| title_row(ui, &title, close, title_gap));
            let from = first_shape(ui);
            let r = contents(ui);
            // The frame covers everything the content painted. egui wraps
            // the frame around the content's `min_rect`, which only what
            // the content *claimed* widens; a panel or a table laid out in
            // the offered rect paints there without claiming it, and the
            // frame, with the glass under it, would then stop where the
            // last claiming widget did, the rest on bare screen (the
            // Tracklists window, once its paste box left the top: only its
            // left bar was framed, the rows and the title's strip sat on
            // the app). So the content's painting is measured, not trusted:
            // whatever it put in the window's layer, the frame takes in.
            ui.expand_to_include_rect(painted_since(ui, from));
            // And a window the user sizes is as large as they made it,
            // painted or not: egui's own idiom for a resizable window,
            // which also holds it at the size it was given rather than
            // letting it shrink to a shorter content. One resizable in
            // height alone follows its content by contract
            // (`resizable_height`) and is left to it.
            if resizable == [true, true] {
                ui.expand_to_include_rect(ui.max_rect());
            }
            // After the content, not before: the chrome is placed from
            // what the content took (`min_rect`), which is what the frame
            // wraps. The rect egui offers before layout (`max_rect`) is its
            // remembered desired width, which never shrinks, so it can run
            // past the frame and put the button on the edge; only where
            // the user sizes the width is the frame that wide. Last also
            // puts it above anything the content drew in the corner.
            let left = ui.min_rect().left();
            let right = if resizable[0] {
                ui.max_rect().right().max(ui.min_rect().right())
            } else {
                ui.min_rect().right()
            };
            if let Some(bar) = bar {
                title_bar(ui, &title, bar, close, left..=right, margin);
            }
            if close {
                let centre_y = match bar {
                    Some(bar) => bar.center().y,
                    None => ui.max_rect().top() + control_row_h(ui) / 2.0,
                };
                closed = close_button(ui, id, right, centre_y);
            }
            r
        });
        if let (Some(shown), Some(slot)) = (&shown, slot) {
            glass::end(ctx, slot, glass_id, shown.response.rect, rounding, stroke);
            glass::drawn(ctx, glass_id);
            ctx.data_mut(|d| d.insert_temp(drawn_id, pass));
            // A drag by the user, on the frame or a grip blocker, is where
            // the family opens from now on. The window's own size changes
            // and a resize leave the corner alone.
            let dragged = moved != egui::Vec2::ZERO
                || ctx
                    .read_response(id.with("move"))
                    .is_some_and(|r| r.dragged());
            if let (Some(family), true) = (spawn, dragged) {
                let corner = shown.response.rect.left_top();
                ctx.data_mut(|d| d.insert_temp(family, corner));
            }
        }
        if closed {
            if let Some(open) = open {
                *open = false;
            }
        }
        shown
    }
}

/// Where the window's layer's paint list stands now: the index the next
/// shape will take. With [`painted_since`], brackets what a stretch of
/// content painted.
fn first_shape(ui: &egui::Ui) -> usize {
    let layer = ui.layer_id();
    ui.ctx()
        .graphics(|g| g.get(layer).map_or(0, |l| l.next_idx().0))
}

/// The rect the shapes painted into the window's layer since `from` show
/// in, within what the window offered its content (`max_rect`). Each
/// shape counts for its visible part only, what its clip lets through;
/// what lies past the offered rect is clipped by egui anyway, and taking
/// it in would widen the window by that sliver every pass. `Rect::NOTHING`
/// when nothing was painted, which expands a rect by nothing.
fn painted_since(ui: &egui::Ui, from: usize) -> egui::Rect {
    let layer = ui.layer_id();
    let offered = ui.max_rect();
    ui.ctx().graphics(|g| {
        let Some(list) = g.get(layer) else {
            return egui::Rect::NOTHING;
        };
        list.all_entries()
            .skip(from)
            .map(|s| s.shape.visual_bounding_rect().intersect(s.clip_rect))
            .filter(|r| r.is_positive())
            .fold(egui::Rect::NOTHING, |acc, r| acc.union(r))
            .intersect(offered)
    })
}

/// The close button's square: the standard interact height, so its target
/// is the size of a control.
pub const CLOSE_SIDE: f32 = 24.0;

/// Width the close button takes at the top-right of a window without a
/// title bar, a gap included; content on that row stops short of it.
pub const CLOSE_W: f32 = CLOSE_SIDE + space::S3;

/// Clearance the content keeps from the hairline under the title: the
/// first line of a dialog is never flush against the rule. The title's
/// own clearance from the rule is the frame's top margin, so the title
/// sits centred in its strip, the same space above it and below.
pub const TITLE_CLEARANCE: f32 = space::S4;

/// The height of a push button, which is what the first row of a window's
/// content is taken to hold: the close button is centred on it.
fn control_row_h(ui: &egui::Ui) -> f32 {
    (ui.text_style_height(&egui::TextStyle::Button).round()
        + 2.0 * ui.spacing().button_padding.y)
        .max(ui.spacing().interact_size.y)
}

/// Clearance the title keeps from each side of its row: the close
/// button's width when there is one, a gap otherwise, on both sides so it
/// stays centred.
fn title_clear(close: bool) -> f32 {
    if close {
        CLOSE_W
    } else {
        space::S3
    }
}

/// Take the title's row from the top of the content, before the content:
/// as tall as the title (never shorter than a control), the gap under it
/// (its clearance from the hairline and the content's from it) included,
/// and as wide as the title with its clearance, up to the width
/// on offer. The row can widen a window whose content is narrower than
/// its title, up to the width egui was asked for (`default_width`, or
/// egui's own), never past it: that's where the title gets cut instead.
/// Returns the title's line, without the gap.
fn title_row(ui: &mut egui::Ui, title: &egui::WidgetText, close: bool, gap: f32) -> egui::Rect {
    let full = title.clone().into_galley(
        ui,
        Some(egui::TextWrapMode::Extend),
        f32::INFINITY,
        egui::TextStyle::Heading,
    );
    let h = full
        .size()
        .y
        .max(ui.spacing().interact_size.y)
        .max(CLOSE_SIDE);
    let w = (full.size().x + 2.0 * title_clear(close))
        .min(ui.available_width())
        .max(0.0);
    let spacing = ui.spacing().item_spacing.y;
    ui.spacing_mut().item_spacing.y = 0.0;
    let (_, rect) = ui.allocate_space(egui::vec2(w, h + gap));
    ui.spacing_mut().item_spacing.y = spacing;
    egui::Rect::from_min_size(rect.min, egui::vec2(w, h))
}

/// Paint the title bar on its row, after the content: the title centred
/// across the frame's width, cut with an ellipsis where it would reach the
/// clearance at either side, and the hairline egui drew under its own bar,
/// the frame's top margin under the row (so the title is centred between
/// the frame's edge and the rule), across the frame from margin to margin.
/// The content starts [`TITLE_CLEARANCE`] under the rule.
fn title_bar(
    ui: &mut egui::Ui,
    title: &egui::WidgetText,
    row: egui::Rect,
    close: bool,
    x: std::ops::RangeInclusive<f32>,
    margin: egui::Margin,
) {
    let row = egui::Rect::from_x_y_ranges(x, row.y_range());
    let avail = (row.width() - 2.0 * title_clear(close)).max(0.0);
    let galley = title.clone().into_galley(
        ui,
        Some(egui::TextWrapMode::Truncate),
        avail,
        egui::TextStyle::Heading,
    );
    let pos = egui::Align2::CENTER_CENTER
        .align_size_within_rect(galley.size(), row)
        .min;
    let outer = egui::Rangef::new(row.left() - margin.left, row.right() + margin.right);
    // The hairline reaches into the margins, past the content's clip.
    let clip = ui.clip_rect();
    ui.set_clip_rect(clip.union(egui::Rect::from_x_y_ranges(outer, row.y_range())));
    ui.painter()
        .galley(pos, galley, ui.visuals().text_color());
    ui.painter().hline(
        outer.shrink(0.1),
        row.bottom() + margin.top,
        ui.visuals().widgets.noninteractive.bg_stroke,
    );
    ui.set_clip_rect(clip);
}

/// The window's close button: the app's close cross, in a square flush
/// with the frame's right edge (`right`) and centred on `centre_y`, taking
/// no space from the layout. Call after the content: it sits on the
/// content's own extent, and paints over whatever is there.
fn close_button(ui: &mut egui::Ui, id: egui::Id, right: f32, centre_y: f32) -> bool {
    let rect = egui::Rect::from_center_size(
        egui::pos2(right - CLOSE_SIDE / 2.0, centre_y),
        egui::Vec2::splat(CLOSE_SIDE),
    );
    // On the title's line the square is above the content, outside the
    // clip egui gives the content; let it through for this one control.
    let clip = ui.clip_rect();
    ui.set_clip_rect(clip.union(rect));
    let resp = ui.interact(rect, id.with("corner-close"), egui::Sense::click());
    icon::close(ui.painter(), rect.center(), icon::col(&resp), icon::CLOSE_ARM);
    ui.set_clip_rect(clip);
    resp.on_hover_cursor(egui::CursorIcon::PointingHand).clicked()
}

/// Whether one of egui's own grips on the window is being dragged: egui
/// registers them under its area's layer, by these names.
fn grip_dragged(ctx: &egui::Context, id: egui::Id) -> bool {
    let layer = egui::LayerId::new(egui::Order::Middle, id);
    let base = egui::Id::new(layer).with("edge_drag");
    [
        "left",
        "right",
        "top",
        "bottom",
        "right_bottom",
        "right_top",
        "left_bottom",
        "left_top",
    ]
    .iter()
    .any(|k| ctx.read_response(base.with(k)).is_some_and(|r| r.dragged()))
}

/// The blockers a window may register, by key (see [`block_grips`]).
const BLOCKERS: [&str; 8] = ["left", "right", "top", "bottom", "tl", "tr", "bl", "br"];

/// Each blocker is two widgets, each a pixel short of the grip at one end
/// (see [`block_grips`]).
const HALVES: [&str; 2] = ["a", "b"];

fn blocker_id(id: egui::Id, key: &str, half: &str) -> egui::Id {
    id.with("grip-block").with(key).with(half)
}

/// Cover the grips egui offers on `rect` (the window's outer rect last
/// frame) that `grips` doesn't, with drag-sensing widgets registered after
/// egui's own so they take the pointer first. egui's side grips are bands
/// `resize_grab_radius_side` either side of an edge, its corner grips
/// squares `resize_grab_radius_corner` around a corner, corners on top
/// (only where both axes size). A side blocker stops short of an allowed
/// corner; a corner blocker covers the ends of allowed sides, as egui's
/// corner would have taken those anyway. Sizing in one axis only has no
/// corner grips, so a corner named there is that end of the band.
///
/// Each blocker goes in as two widgets, one a pixel short at each end:
/// egui's hit test gives a drag hit to a smaller drag widget the hit one
/// contains, taking it for a handle on a background, and a blocker the
/// same size as the grip counts as containing it. Neither half does.
fn block_grips(
    ui: &mut egui::Ui,
    id: egui::Id,
    rect: egui::Rect,
    axes: [bool; 2],
    grips: Grips,
) {
    let (sr, cr) = {
        let i = &ui.style().interaction;
        (i.resize_grab_radius_side, i.resize_grab_radius_corner)
    };
    let mut blocks: Vec<(&str, egui::Rect)> = Vec::new();
    if axes[0] && axes[1] {
        for (g, key, p) in [
            (Grips::TOP_LEFT, "tl", rect.left_top()),
            (Grips::TOP_RIGHT, "tr", rect.right_top()),
            (Grips::BOTTOM_LEFT, "bl", rect.left_bottom()),
            (Grips::BOTTOM_RIGHT, "br", rect.right_bottom()),
        ] {
            if !grips.has(g) {
                blocks.push((
                    key,
                    egui::Rect::from_center_size(p, egui::Vec2::splat(2.0 * cr)),
                ));
            }
        }
    }
    // A side band, its ends pulled in where the corner is offered.
    let band = |a: egui::Pos2, b: egui::Pos2| egui::Rect::from_min_max(a, b).expand(sr);
    if axes[1] && !grips.has(Grips::TOP) {
        let mut r = band(rect.left_top(), rect.right_top());
        if grips.has(Grips::TOP_LEFT) {
            r.min.x = rect.min.x + cr;
        }
        if grips.has(Grips::TOP_RIGHT) {
            r.max.x = rect.max.x - cr;
        }
        blocks.push(("top", r));
    }
    if axes[1] && !grips.has(Grips::BOTTOM) {
        let mut r = band(rect.left_bottom(), rect.right_bottom());
        if grips.has(Grips::BOTTOM_LEFT) {
            r.min.x = rect.min.x + cr;
        }
        if grips.has(Grips::BOTTOM_RIGHT) {
            r.max.x = rect.max.x - cr;
        }
        blocks.push(("bottom", r));
    }
    if axes[0] && !grips.has(Grips::LEFT) {
        let mut r = band(rect.left_top(), rect.left_bottom());
        if grips.has(Grips::TOP_LEFT) {
            r.min.y = rect.min.y + cr;
        }
        if grips.has(Grips::BOTTOM_LEFT) {
            r.max.y = rect.max.y - cr;
        }
        blocks.push(("left", r));
    }
    if axes[0] && !grips.has(Grips::RIGHT) {
        let mut r = band(rect.right_top(), rect.right_bottom());
        if grips.has(Grips::TOP_RIGHT) {
            r.min.y = rect.min.y + cr;
        }
        if grips.has(Grips::BOTTOM_RIGHT) {
            r.max.y = rect.max.y - cr;
        }
        blocks.push(("right", r));
    }
    if blocks.is_empty() {
        return;
    }
    // The bands reach outside the frame, past the content's clip.
    let clip = ui.clip_rect();
    ui.set_clip_rect(clip.union(rect.expand(cr.max(sr))));
    for (key, r) in blocks {
        if !r.is_positive() {
            continue;
        }
        let (mut a, mut b) = (r, r);
        if r.width() >= r.height() {
            a.max.x -= 1.0;
            b.min.x += 1.0;
        } else {
            a.max.y -= 1.0;
            b.min.y += 1.0;
        }
        ui.interact(a, blocker_id(id, key, HALVES[0]), egui::Sense::drag());
        ui.interact(b, blocker_id(id, key, HALVES[1]), egui::Sense::drag());
    }
    ui.set_clip_rect(clip);
}

/// The glass's edge, for a surface that isn't a window (a menu, a popup)
/// and wants the same hairline.
pub fn edge() -> egui::Stroke {
    egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rect every shape in `layer` shows in, clipped, on the pass that
    /// just ran.
    fn painted(ctx: &egui::Context, layer: egui::LayerId) -> egui::Rect {
        ctx.graphics(|g| {
            g.get(layer).map_or(egui::Rect::NOTHING, |l| {
                l.all_entries()
                    .map(|s| s.shape.visual_bounding_rect().intersect(s.clip_rect))
                    .filter(|r| r.is_positive())
                    .fold(egui::Rect::NOTHING, |acc, r| acc.union(r))
            })
        })
    }

    /// The frame's edge is a hairline centred on it, so the layer paints
    /// half a pixel past the frame by design; the glitch under test is
    /// whole panels past it.
    fn covers(frame: egui::Rect, paint: egui::Rect) -> bool {
        frame.expand(1.0).contains_rect(paint)
    }

    /// Run `content` in a window for a few passes and hand back the frame
    /// the window ended up with and the rect its layer painted.
    fn frame_and_paint(
        resizable: bool,
        content: impl Fn(&mut egui::Ui) + Copy,
    ) -> (egui::Rect, egui::Rect) {
        let ctx = egui::Context::default();
        // The shadow is the one shape meant to lie outside the frame.
        ctx.style_mut(|s| s.visuals.window_shadow = egui::epaint::Shadow::NONE);
        let id = egui::Id::new("under-test");
        let mut out = None;
        for _ in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                ..Default::default()
            };
            ctx.run(input, |ctx| {
                let mut open = true;
                let shown = Window::new("Under test")
                    .id(id)
                    .open(&mut open)
                    .resizable(resizable)
                    .default_size(egui::vec2(600.0, 400.0))
                    .show(ctx, content)
                    .expect("drawn");
                let layer = egui::LayerId::new(egui::Order::Middle, id);
                out = Some((shown.response.rect, painted(ctx, layer)));
            });
        }
        out.unwrap()
    }

    /// Panels lay out in the offered rect without claiming it; the frame
    /// must still take in what they painted.
    #[test]
    fn frame_covers_panels_that_claim_nothing() {
        for resizable in [true, false] {
            let (frame, paint) = frame_and_paint(resizable, |ui| {
                egui::SidePanel::left("side")
                    .default_width(150.0)
                    .show_inside(ui, |ui| {
                        ui.label("side");
                    });
                egui::CentralPanel::default().show_inside(ui, |ui| {
                    let r = ui.max_rect();
                    ui.painter().rect_filled(r, 0.0, egui::Color32::RED);
                });
            });
            assert!(
                covers(frame, paint),
                "resizable={resizable}: painted {paint:?} outside frame {frame:?}"
            );
            assert!(frame.width() >= 590.0, "resizable={resizable}: frame {frame:?} narrower than the panels");
        }
    }

    /// A user-sized window is the size it was given, whatever the content.
    #[test]
    fn resizable_window_holds_its_size() {
        let (frame, _) = frame_and_paint(true, |ui| {
            ui.label("tiny");
        });
        assert!(frame.width() >= 600.0 && frame.height() >= 400.0, "frame {frame:?}");
    }

    /// The title sits centred between the frame's top edge and the rule,
    /// and the content starts a clearance under the rule, never flush.
    #[test]
    fn title_bar_keeps_its_margins() {
        let ctx = egui::Context::default();
        ctx.style_mut(|s| s.visuals.window_shadow = egui::epaint::Shadow::NONE);
        let id = egui::Id::new("title-under-test");
        let mut out = None;
        for _ in 0..4 {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1280.0, 800.0))),
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                let mut open = true;
                let mut content_top = 0.0;
                let shown = Window::new("Under test")
                    .id(id)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        content_top = ui.cursor().top();
                        ui.label("first line");
                    })
                    .expect("drawn");
                let layer = egui::LayerId::new(egui::Order::Middle, id);
                // The rule is the one horizontal line the window paints
                // across its width: the widest 1-tall shape in the layer.
                let rule_y = ctx.graphics(|g| {
                    g.get(layer).and_then(|l| {
                        l.all_entries()
                            .map(|s| s.shape.visual_bounding_rect())
                            .filter(|r| r.height() <= 2.0 && r.width() > 100.0)
                            .map(|r| r.center().y)
                            .next()
                    })
                });
                out = Some((shown.response.rect, content_top, rule_y));
            });
        }
        let (frame, content_top, rule_y) = out.unwrap();
        let rule_y = rule_y.expect("a hairline under the title");
        let margin = ctx.style().spacing.window_margin;
        let strip = rule_y - frame.top();
        let title_h = strip - 2.0 * margin.top;
        assert!(title_h >= CLOSE_SIDE - 0.5, "title strip {strip} too short for a centred title");
        let clearance = content_top - rule_y;
        assert!(
            (clearance - TITLE_CLEARANCE).abs() < 0.5,
            "content starts {clearance} under the rule, wanted {TITLE_CLEARANCE}"
        );
    }

    /// A fixed window still wraps its content rather than the offered rect.
    #[test]
    fn fixed_window_wraps_its_content() {
        let (frame, paint) = frame_and_paint(false, |ui| {
            ui.label("tiny");
        });
        assert!(covers(frame, paint), "painted {paint:?} outside frame {frame:?}");
        assert!(frame.width() < 400.0, "frame {frame:?} took the offered width");
    }
}
