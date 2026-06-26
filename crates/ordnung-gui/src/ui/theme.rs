//! Global theme: installs the Inter font stack and pushes the design tokens into
//! egui's [`Style`]/[`Visuals`] once at startup. After [`install`], every *stock*
//! egui widget (buttons, inputs, scrollbars, selections, menus) already matches
//! the Ordnung visual language — no per-call-site styling required.
//!
//! This is Pass 1 of the design system: tokens + global style. Bespoke component
//! helpers (cards, chips, segmented controls) build on top in a later pass.

use eframe::egui::{
    self, Color32, FontData, FontDefinitions, FontFamily, Margin, Rounding, Stroke,
};

use super::tokens::{color, radius, space};

/// Install fonts and the global style. Call once, before any frame is laid out.
pub fn install(ctx: &egui::Context) {
    install_fonts(ctx);
    apply_style(ctx);
}

/// Install Inter as the primary UI face, with DejaVu Sans behind it as a
/// per-glyph fallback. egui's bundled default covers little beyond basic Latin,
/// so accented / Cyrillic / Greek / symbol characters common in DJ metadata would
/// render as tofu. Inter gives us a modern, tightly-spaced UI face; DejaVu's
/// wider glyph coverage sits behind it for anything Inter lacks, and egui's
/// defaults sit behind that (emoji, egui's icon glyphs).
fn install_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "Inter".to_owned(),
        FontData::from_static(include_bytes!("../../assets/fonts/Inter-Regular.ttf")),
    );
    fonts.font_data.insert(
        "DejaVuSans".to_owned(),
        FontData::from_static(include_bytes!("../../assets/fonts/DejaVuSans.ttf")),
    );
    // Source Serif sits in its own named family, used only for tooltip / hover
    // text (see `ui::hover`). A serif face there reads as more formal and
    // instructional than the sans UI body. DejaVu backs it for glyph coverage.
    fonts.font_data.insert(
        "SourceSerif".to_owned(),
        FontData::from_static(include_bytes!(
            "../../assets/fonts/SourceSerif4-Regular.ttf"
        )),
    );
    // Neither Inter nor DejaVu covers CJK, so Japanese / Chinese / Korean track
    // and release titles (common in DJ metadata) render as tofu boxes. Pull a
    // broad-coverage system font off disk at runtime and sit it at the back of
    // the chain — loaded this way it costs nothing in the binary, and it only
    // gets consulted for glyphs the nicer faces ahead of it lack.
    let cjk = load_system_cjk_font(&mut fonts);

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let chain = fonts.families.entry(family).or_default();
        chain.insert(0, "DejaVuSans".to_owned());
        chain.insert(0, "Inter".to_owned());
        if let Some(name) = &cjk {
            // After DejaVu, before egui's own fallbacks.
            chain.insert(2, name.clone());
        }
    }
    fonts.families.insert(
        FontFamily::Name(super::hover::SERIF_FAMILY.into()),
        vec!["SourceSerif".to_owned(), "DejaVuSans".to_owned()],
    );
    ctx.set_fonts(fonts);
}

/// Load the first available broad-coverage system font (CJK + the rest of the
/// BMP) and register it under the returned key. Returns `None` if none of the
/// candidate paths exist, in which case the chain is simply left as-is.
///
/// We read the font from disk rather than bundling one: a full CJK face is
/// 15–25 MB, which we'd rather not bake into every binary for metadata that's
/// usually Latin. Candidates are listed per-platform, broadest coverage first.
fn load_system_cjk_font(fonts: &mut FontDefinitions) -> Option<String> {
    const CANDIDATES: &[&str] = &[
        // macOS — single-face .ttf, covers CJK + Cyrillic + Greek + symbols.
        "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
        "/System/Library/Fonts/Hiragino Sans GB.ttc",
        // Linux — Noto Sans CJK (Debian/Ubuntu, Fedora).
        "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
        "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
        // Windows.
        "C:/Windows/Fonts/msyh.ttc",
        "C:/Windows/Fonts/YuGothR.ttc",
    ];

    for path in CANDIDATES {
        if let Ok(bytes) = std::fs::read(path) {
            const KEY: &str = "SystemCJK";
            fonts
                .font_data
                .insert(KEY.to_owned(), FontData::from_owned(bytes));
            return Some(KEY.to_owned());
        }
    }
    None
}

/// Build the global [`egui::Style`] from the design tokens.
fn apply_style(ctx: &egui::Context) {
    let mut style = (*ctx.style()).clone();
    let v = &mut style.visuals;

    v.dark_mode = true;

    // Surfaces.
    v.panel_fill = color::BG;
    v.window_fill = color::SURFACE;
    v.extreme_bg_color = color::FIELD; // text-edit / sunken input background
    v.window_stroke = Stroke::new(1.0, color::SEPARATOR);
    v.window_rounding = Rounding::same(radius::LG);
    v.menu_rounding = Rounding::same(radius::MD);

    // Selection (text + list highlight) uses the accent.
    v.selection.bg_fill = color::ACCENT_SOFT;
    v.selection.stroke = Stroke::new(1.0, color::ACCENT);
    v.hyperlink_color = color::ACCENT;

    // Widget states. Apple-ish: soft fills instead of hard outlines, consistent
    // small rounding, and no size "expansion" bulge on hover.
    let w = &mut v.widgets;
    let hairline = Stroke::new(1.0, color::SEPARATOR);

    // Non-interactive: labels, frame backgrounds, separators.
    w.noninteractive.bg_fill = color::SURFACE;
    w.noninteractive.weak_bg_fill = color::SURFACE;
    w.noninteractive.bg_stroke = hairline;
    w.noninteractive.fg_stroke = Stroke::new(1.0, color::LABEL);
    w.noninteractive.rounding = Rounding::same(radius::SM);
    w.noninteractive.expansion = 0.0;

    // Inactive: a button/input at rest.
    w.inactive.bg_fill = color::SURFACE_HI;
    w.inactive.weak_bg_fill = color::SURFACE_HI;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, color::LABEL);
    w.inactive.rounding = Rounding::same(radius::SM);
    w.inactive.expansion = 0.0;

    // Hovered.
    w.hovered.bg_fill = Color32::from_rgb(54, 54, 58);
    w.hovered.weak_bg_fill = Color32::from_rgb(54, 54, 58);
    w.hovered.bg_stroke = Stroke::NONE;
    w.hovered.fg_stroke = Stroke::new(1.0, color::LABEL);
    w.hovered.rounding = Rounding::same(radius::SM);
    w.hovered.expansion = 0.0;

    // Active (pressed).
    w.active.bg_fill = Color32::from_rgb(64, 64, 68);
    w.active.weak_bg_fill = Color32::from_rgb(64, 64, 68);
    w.active.bg_stroke = Stroke::new(1.0, color::ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, color::LABEL);
    w.active.rounding = Rounding::same(radius::SM);
    w.active.expansion = 0.0;

    // Open (e.g. an expanded combo box).
    w.open.bg_fill = color::SURFACE_HI;
    w.open.weak_bg_fill = color::SURFACE_HI;
    w.open.bg_stroke = hairline;
    w.open.fg_stroke = Stroke::new(1.0, color::LABEL);
    w.open.rounding = Rounding::same(radius::SM);
    w.open.expansion = 0.0;

    // Spacing — the 8-pt grid.
    let s = &mut style.spacing;
    s.item_spacing = egui::vec2(space::S3, space::S2 + 2.0); // 8 × 6
                                                             // Roomier inset so text sits clear of component edges: menu items and buttons
                                                             // get 14px horizontal padding, and the menu frame adds 8px around that — so
                                                             // menu text clears the edge by ~22px instead of feeling cramped.
    s.button_padding = egui::vec2(space::S4 + 2.0, space::S2 + 3.0); // 14 × 7
    s.menu_margin = Margin::same(space::S3); // 8
    s.window_margin = Margin::same(space::S4 - 2.0); // 10
    s.indent = 18.0;
    s.interact_size.y = 24.0;
    s.scroll.bar_width = 8.0;

    // Text sizes/faces are left at egui's defaults; this pass only unifies colour,
    // rounding, and spacing across components — not typography.

    ctx.set_style(style);
}
