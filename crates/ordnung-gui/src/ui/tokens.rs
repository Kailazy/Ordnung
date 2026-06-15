//! Design tokens — the single source of truth for Ordnung's visual language.
//!
//! Apple-inspired and deliberately small: layered dark-gray surfaces (never pure
//! black), hairline separators, an 8-pt spacing grid, a concentric corner-radius
//! scale, an SF-like type ramp, and a semantic colour palette. Everything visual
//! should reference a token here rather than an inline literal, so the whole app
//! re-skins from one place.
//!
//! Pass 1 wires these into egui's global [`Style`](eframe::egui::Style) (see
//! [`super::theme`]); bespoke components consume them directly in a later pass.

use eframe::egui::{Color32, FontId};

/// Corner radii. Apple keeps nested corners *concentric*: an inset child's radius
/// equals its parent's radius minus the padding between them, so the two curves
/// stay parallel. Derive those child radii with [`inner`] rather than guessing.
pub mod radius {
    pub const XS: f32 = 4.0; // chips, tiny pills
    pub const SM: f32 = 6.0; // buttons, inputs
    pub const MD: f32 = 10.0; // menus, popovers
    pub const LG: f32 = 14.0; // cards, windows
    pub const XL: f32 = 20.0; // large sheets
}

/// Radius for an element inset by `pad` inside a container of radius `outer`, so
/// their corners stay concentric. Clamped at 0 (a flat inner corner).
pub fn inner(outer: f32, pad: f32) -> f32 {
    (outer - pad).max(0.0)
}

/// 8-pt spacing grid. Reach for these instead of ad-hoc pixel gaps; consistent
/// rhythm is most of what makes a layout read as "designed".
pub mod space {
    pub const S1: f32 = 2.0;
    pub const S2: f32 = 4.0;
    pub const S3: f32 = 8.0;
    pub const S4: f32 = 12.0;
    pub const S5: f32 = 16.0;
    pub const S6: f32 = 24.0;
    pub const S7: f32 = 32.0;
}

/// Semantic colours. Surfaces are layered grays that read as elevation; label
/// colours descend in contrast for hierarchy; separators are hairline; the system
/// hues follow Apple's dark-mode HIG values for status accents.
pub mod color {
    use super::Color32;

    // --- Surfaces, low → high elevation ---
    /// App background behind panels. Matches egui's prior default dark fill
    /// (`from_gray(27)`) so the window brightness is unchanged from before.
    pub const BG: Color32 = Color32::from_rgb(27, 27, 27);
    /// Content background for the main songs/table area — a touch lighter than
    /// `BG` so the central list reads as raised above the nav sidebar and the
    /// top/bottom bars (which stay at `BG`).
    pub const CONTENT_BG: Color32 = Color32::from_rgb(35, 35, 37);
    /// Default panel / resting surface.
    pub const SURFACE: Color32 = Color32::from_rgb(32, 32, 35);
    /// Raised surface — cards, hovered rows, menus.
    pub const SURFACE_HI: Color32 = Color32::from_rgb(44, 44, 48);
    /// Sunken surface — text fields and other inputs.
    pub const FIELD: Color32 = Color32::from_rgb(20, 20, 22);

    // --- Hairlines ---
    /// Translucent separator that adapts to whatever sits behind it (white α20,
    /// premultiplied — equivalent to `from_white_alpha(20)` but const-constructible).
    pub const SEPARATOR: Color32 = Color32::from_rgba_premultiplied(20, 20, 20, 20);
    /// Opaque separator for use over a known dark surface.
    pub const SEPARATOR_OPAQUE: Color32 = Color32::from_rgb(56, 56, 60);

    // --- Labels, primary → faint ---
    pub const LABEL: Color32 = Color32::from_rgb(235, 235, 240);
    pub const LABEL_2: Color32 = Color32::from_rgb(168, 168, 176);
    pub const LABEL_3: Color32 = Color32::from_rgb(120, 120, 128);
    pub const LABEL_4: Color32 = Color32::from_rgb(88, 88, 96);

    // --- Accent ---
    pub const ACCENT: Color32 = Color32::from_rgb(74, 134, 232);
    pub const ACCENT_HOVER: Color32 = Color32::from_rgb(98, 156, 245);
    /// Pre-blended translucent accent for selection fills over a dark surface.
    pub const ACCENT_SOFT: Color32 = Color32::from_rgb(44, 70, 120);

    // --- System status hues (Apple HIG, dark) ---
    pub const RED: Color32 = Color32::from_rgb(255, 69, 58);
    pub const ORANGE: Color32 = Color32::from_rgb(255, 159, 10);
    pub const YELLOW: Color32 = Color32::from_rgb(255, 214, 10);
    pub const GREEN: Color32 = Color32::from_rgb(48, 209, 88);
    pub const BLUE: Color32 = Color32::from_rgb(10, 132, 255);
    pub const GRAY: Color32 = Color32::from_rgb(142, 142, 147);
}

/// A size ramp for consistent text hierarchy, named after the role each size
/// plays. Uses the default proportional/monospace faces — typography is left to
/// egui's defaults for now; only the sizes are standardised here for reuse.
pub mod font {
    use super::FontId;

    pub fn large_title() -> FontId {
        FontId::proportional(26.0)
    }
    pub fn title() -> FontId {
        FontId::proportional(20.0)
    }
    pub fn headline() -> FontId {
        FontId::proportional(15.0)
    }
    pub fn body() -> FontId {
        FontId::proportional(13.0)
    }
    pub fn callout() -> FontId {
        FontId::proportional(12.0)
    }
    pub fn footnote() -> FontId {
        FontId::proportional(11.0)
    }
    pub fn caption() -> FontId {
        FontId::proportional(10.0)
    }
    pub fn mono() -> FontId {
        FontId::monospace(12.5)
    }
}
