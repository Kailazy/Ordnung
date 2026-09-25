//! The nav: a panel docked to a screen edge that snaps between designed
//! widths. The library nav on the left is one, the track inspector on the
//! right another. A nav does not resize freely: it has a few [`Tier`]s,
//! each a layout the panel was designed for, and dragging its edge picks
//! one of them.
//!
//! The layout in force is *frozen* for the whole of a drag. Committing a
//! tier live meant the panel's labels rewrapped under the pointer on the way
//! past every boundary: the panel flickering through layouts you were only
//! travelling over, not choosing. So the edge drag does not move the panel
//! at all. A ghost line follows the pointer, a wider marker sits at the tier
//! the drop would land on, and that tier is applied once, on release. One
//! layout change per drag, at the moment you commit to it. The change then
//! eases over a few frames so it reads as a lock into place, not a cut.
//!
//! [`NavState`] is the state a caller keeps between frames (the tier, and
//! the drag if one is under way); [`Nav`] shows the panel for one frame.

use eframe::egui;

/// One designed width of a nav. Implemented by the caller's own tier enum.
pub trait Tier: Copy + PartialEq + std::fmt::Debug + 'static {
    /// Every tier, narrowest first.
    const ALL: &'static [Self];
    /// The panel's width at this tier, in points.
    fn width(self) -> f32;
}

/// The tier a drag to width `w` selects, given the tier `current` in force.
///
/// The panel is pinned to a tier width at every instant, so the pointer sits
/// *away* from the panel edge for most of a drag and a plain nearest-match
/// would strobe between two layouts whenever it hovered near a boundary.
/// `current` therefore holds until the pointer is decisively into a
/// neighbour.
///
/// How far "decisively" is depends on which way you're going, because the
/// two directions aren't equally costly to get wrong. Widening is the cheap,
/// common intent, and an accidental widen is obvious and instantly undone.
/// Narrowing throws layout away (the library nav loses every caption), so it
/// stays deliberate. An equal split also *reads* unequal from a narrow tier:
/// a symmetric 33% meant pulling 130pt off a 56pt rail, more than twice the
/// panel's own width, before anything happened. So widening commits just
/// past the panel's own edge, while narrowing keeps the full hold.
pub fn dragged_to<T: Tier>(current: T, w: f32) -> T {
    /// Fraction of the gap past the midpoint needed to narrow, the
    /// destructive direction, held deliberately far.
    const STICK_SHRINK: f32 = 0.33;
    /// Widening instead commits *before* the midpoint: a quarter of the way
    /// out from the current edge, so a short confident pull is enough.
    const REACH_GROW: f32 = 0.25;
    // Deliberately *not* keyed off a nearest tier: the whole point of an
    // asymmetric widen is to fire while the pointer is still nearer the
    // narrow tier than the wide one, which an early `nearest == current`
    // return would swallow. Each direction is tested against its own
    // threshold instead.
    let cur = current.width();
    let wider = T::ALL.iter().copied().find(|t| t.width() > cur);
    let narrower = T::ALL.iter().copied().rev().find(|t| t.width() < cur);
    if let Some(t) = wider {
        let gap = t.width() - cur;
        if w > cur + gap * REACH_GROW {
            return t;
        }
    }
    if let Some(t) = narrower {
        let gap = cur - t.width();
        let midpoint = (cur + t.width()) / 2.0;
        if w < midpoint - gap * STICK_SHRINK {
            return t;
        }
    }
    current
}

/// A resize in progress. The panel stays locked to its tier while the edge
/// is held; this is only what the ghost line draws and what the drop will
/// commit.
#[derive(Clone, Copy, Debug)]
struct Drag<T: Tier> {
    /// Live pointer x, in screen space: where the ghost line is painted.
    x: f32,
    /// The tier this drag would land on, run through the same hysteresis
    /// the commit uses, so the ghost previews the real outcome rather than
    /// a nearest-match the drop would then disagree with.
    target: T,
}

/// What a caller keeps between frames: the tier in force, and the drag if
/// the edge is held. `Copy`, so a caller whose panel body borrows the rest
/// of its state can take a copy out and put it back after the frame.
#[derive(Clone, Copy, Debug)]
pub struct NavState<T: Tier> {
    /// The tier in force. Only a drop changes it.
    pub tier: T,
    drag: Option<Drag<T>>,
}

impl<T: Tier> NavState<T> {
    pub fn new(tier: T) -> Self {
        Self { tier, drag: None }
    }
}

/// Which screen edge the panel is docked to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
}

/// A nav for one frame: the panel at its tier, its edge drag, and the ghost.
pub struct Nav<'a, T: Tier> {
    id: egui::Id,
    side: Side,
    state: &'a mut NavState<T>,
    shown: Option<T>,
    slide: f32,
}

/// What [`Nav::show`] hands back.
pub struct NavResponse<R> {
    /// The panel's body, or `None` when the panel was slid fully away.
    pub inner: Option<egui::InnerResponse<R>>,
    /// The width the panel was shown at this frame, after the slide.
    pub width: f32,
    /// Whether a drop just committed a different tier: the moment to persist
    /// [`NavState::tier`].
    pub committed: bool,
}

impl<'a, T: Tier> Nav<'a, T> {
    pub fn left(id: impl Into<egui::Id>, state: &'a mut NavState<T>) -> Self {
        Self::new(id.into(), Side::Left, state)
    }

    pub fn right(id: impl Into<egui::Id>, state: &'a mut NavState<T>) -> Self {
        Self::new(id.into(), Side::Right, state)
    }

    fn new(id: egui::Id, side: Side, state: &'a mut NavState<T>) -> Self {
        Self {
            id,
            side,
            state,
            shown: None,
            slide: 1.0,
        }
    }

    /// Lay the panel out at `tier` this frame without changing the tier in
    /// force: a width borrowed for a moment (the rail promoted to fit a
    /// rename field), not the user's choice.
    pub fn shown(mut self, tier: T) -> Self {
        self.shown = Some(tier);
        self
    }

    /// A 0..=1 factor on the width, for a panel that slides in and out on
    /// top of snapping; at 0 the panel is not shown at all.
    pub fn slide(mut self, t: f32) -> Self {
        self.slide = t.clamp(0.0, 1.0);
        self
    }

    /// egui's id for the panel's own resize handle.
    fn drag_id(&self) -> egui::Id {
        self.id.with("__resize")
    }

    /// Advance the edge drag: while the handle is held, only the ghost's
    /// target moves; on release, the tier changes, and this returns `true`
    /// when it changed to something else.
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        let drag_id = self.drag_id();
        if ctx.is_being_dragged(drag_id) {
            if let Some(pos) = ctx.pointer_interact_pos() {
                let screen = ctx.screen_rect();
                let w = match self.side {
                    Side::Left => pos.x - screen.left(),
                    Side::Right => screen.right() - pos.x,
                };
                // Hysteresis is applied against the tier in force, which is
                // the tier the panel is still showing, so the ghost snaps to
                // the same tier the drop will pick and never previews a
                // landing the release refuses.
                self.state.drag = Some(Drag {
                    x: pos.x,
                    target: dragged_to(self.state.tier, w),
                });
                // The panel is frozen for the duration, so nothing else is
                // asking for frames. Without this the ghost would only
                // advance when some other part of the UI happened to
                // repaint, and the line would visibly lag the cursor.
                ctx.request_repaint();
            }
            false
        } else if let Some(drag) = self.state.drag.take() {
            // Released: the only place the tier changes.
            let changed = drag.target != self.state.tier;
            self.state.tier = drag.target;
            changed
        } else {
            false
        }
    }

    /// Show the panel. `configure` receives the panel already set to snap
    /// (resizable, pinned to this frame's width) and may add its frame; the
    /// separator line is egui's and is left to the caller's chrome.
    pub fn show<R>(
        mut self,
        ctx: &egui::Context,
        configure: impl FnOnce(egui::SidePanel) -> egui::SidePanel,
        add: impl FnOnce(&mut egui::Ui) -> R,
    ) -> NavResponse<R> {
        let committed = self.poll(ctx);
        let target = self.shown.unwrap_or(self.state.tier).width();
        // Ease between tiers so the change of layout reads as a deliberate
        // lock into place rather than a hard cut. `animate_value` repaints
        // until it settles; the width it produces is only ever *travelling
        // between* two tiers, never a width the user can hold it at.
        let settled = ctx.animate_value_with_time(self.id.with("snap"), target, 0.13);
        let width = settled * self.slide;
        if width < 0.5 {
            return NavResponse {
                inner: None,
                width: 0.0,
                committed,
            };
        }
        let panel = match self.side {
            Side::Left => egui::SidePanel::left(self.id),
            Side::Right => egui::SidePanel::right(self.id),
        }
        .resizable(true)
        .default_width(width)
        // Pinned to the snapped width at all times: a collapsed range is
        // what stops egui's own resize from writing an arbitrary width back
        // into the panel. The drop above is the only thing that changes it.
        .width_range(width..=width);
        let inner = configure(panel).show(ctx, add);

        // The panel is pinned to a single width, so egui reads it as already
        // at its minimum and offers a one-way resize cursor, implying the
        // panel can only be widened. It snaps both ways, so say so, on hover
        // as well as mid-drag.
        let hovered = ctx.is_pointer_over_area()
            && ctx
                .read_response(self.drag_id())
                .is_some_and(|r| r.hovered());
        if self.state.drag.is_some() || hovered {
            ctx.set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        if let Some(drag) = self.state.drag {
            self.paint_ghost(ctx, drag);
        }
        NavResponse {
            inner: Some(inner),
            width,
            committed,
        }
    }

    /// The resize ghost. While the edge is held the panel itself does not
    /// move, so this is the entire feedback for the drag: a hairline tracks
    /// the pointer freely, and a wider marker sits at the tier the drop
    /// would land on. Painted in a foreground layer after the panel so it
    /// reads on top of the panel's own content rather than being clipped
    /// by it.
    fn paint_ghost(&self, ctx: &egui::Context, drag: Drag<T>) {
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(egui::LayerId::new(
            egui::Order::Foreground,
            self.id.with("resize_ghost"),
        ));
        // Where the panel would settle if released now. Drawn solid, in the
        // accent, so the eye reads the landing rather than the pointer.
        let snap_x = match self.side {
            Side::Left => screen.left() + drag.target.width(),
            Side::Right => screen.right() - drag.target.width(),
        };
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(snap_x - 1.5, screen.top()),
                egui::pos2(snap_x + 1.5, screen.bottom()),
            ),
            egui::Rounding::ZERO,
            ACCENT,
        );
        // The pointer's own position, dimmer and hairline: it explains why
        // the snap marker sits where it does while the two are apart, and
        // is redundant (so unobtrusive) once the drag settles onto a tier.
        if (drag.x - snap_x).abs() > 2.0 {
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(drag.x - 0.5, screen.top()),
                    egui::pos2(drag.x + 0.5, screen.bottom()),
                ),
                egui::Rounding::ZERO,
                egui::Color32::from_white_alpha(60),
            );
        }
    }
}

/// The ghost's landing marker: the app's accent.
const ACCENT: egui::Color32 = super::tokens::color::ACCENT;

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    enum Two {
        Narrow,
        Wide,
    }

    impl Tier for Two {
        const ALL: &'static [Self] = &[Two::Narrow, Two::Wide];
        fn width(self) -> f32 {
            match self {
                Two::Narrow => 56.0,
                Two::Wide => 212.0,
            }
        }
    }

    /// Widening commits a quarter of the gap out; narrowing only well past
    /// the midpoint. In between, the tier in force holds.
    #[test]
    fn hysteresis_is_asymmetric() {
        let gap = Two::Wide.width() - Two::Narrow.width();
        assert_eq!(dragged_to(Two::Narrow, 56.0 + gap * 0.2), Two::Narrow);
        assert_eq!(dragged_to(Two::Narrow, 56.0 + gap * 0.3), Two::Wide);
        let mid = (56.0 + 212.0) / 2.0;
        assert_eq!(dragged_to(Two::Wide, mid - gap * 0.2), Two::Wide);
        assert_eq!(dragged_to(Two::Wide, mid - gap * 0.4), Two::Narrow);
        // Past either end there is nowhere further to go.
        assert_eq!(dragged_to(Two::Wide, 900.0), Two::Wide);
        assert_eq!(dragged_to(Two::Narrow, -50.0), Two::Narrow);
    }

    /// A right-hand nav lays out at its tier's width from the right edge,
    /// and the slide factor takes it away entirely at zero.
    #[test]
    fn right_nav_sits_at_its_tier_and_slides_away() {
        let ctx = egui::Context::default();
        let mut state = NavState::new(Two::Wide);
        let mut rect = None;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let r = Nav::right("side", &mut state).show(ctx, |p| p, |ui| ui.label("x"));
            rect = r.inner.map(|i| i.response.rect);
        });
        let rect = rect.expect("shown");
        let screen = ctx.screen_rect();
        assert!((rect.right() - screen.right()).abs() < 0.5, "{rect:?}");
        assert!((rect.width() - 212.0).abs() < 0.5, "{rect:?}");

        let mut shown = true;
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            let r = Nav::right("side", &mut state)
                .slide(0.0)
                .show(ctx, |p| p, |ui| ui.label("x"));
            shown = r.inner.is_some();
        });
        assert!(!shown);
    }
}
