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
    // Inter and DejaVu only reach Latin / Cyrillic / Greek, so titles in CJK,
    // Arabic, Hebrew, Thai, the Indic scripts, etc. — all common in DJ metadata
    // — render as tofu boxes. Pull broad-coverage system fonts off disk at
    // runtime and sit them at the back of the chain: loaded this way they cost
    // nothing in the binary, and each is only consulted for glyphs the faces
    // ahead of it lack.
    let fallbacks = load_system_fallback_fonts(&mut fonts);

    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        let chain = fonts.families.entry(family).or_default();
        chain.insert(0, "DejaVuSans".to_owned());
        chain.insert(0, "Inter".to_owned());
        // After DejaVu, before egui's own fallbacks. Broadest faces first, so a
        // glyph is served by the widest font that has it; script-specific faces
        // behind them fill in anything the broad ones miss.
        for (i, name) in fallbacks.iter().enumerate() {
            chain.insert(2 + i, name.clone());
        }
    }
    fonts.families.insert(
        FontFamily::Name(super::hover::SERIF_FAMILY.into()),
        vec!["SourceSerif".to_owned(), "DejaVuSans".to_owned()],
    );
    ctx.set_fonts(fonts);
}

/// Register broad-coverage system fonts for the world's major scripts and return
/// their keys, in chain order. Each group lists candidate paths per platform
/// (broadest coverage first); the first path that exists is loaded and the rest
/// of that group skipped, so we register at most one face per script. Groups
/// with no available font are silently dropped.
///
/// We read these off disk rather than bundling them: a single full-coverage CJK
/// face is 15–25 MB, and the complete set would dwarf the binary — for metadata
/// that's usually Latin, that's not a trade worth making. The broad Unicode
/// font first covers most non-Latin titles outright; the per-script faces behind
/// it fill in anything it misses and cover platforms where it's absent.
fn load_system_fallback_fonts(fonts: &mut FontDefinitions) -> Vec<String> {
    // (registration key, candidate paths in priority order).
    const GROUPS: &[(&str, &[&str])] = &[
        // Broadest single faces — CJK + most of the BMP in one file.
        (
            "fallback-unicode",
            &[
                // macOS: single-face .ttf covering CJK, Cyrillic, Greek, Arabic,
                // Hebrew, Thai, Devanagari, and more.
                "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
                // Linux: Noto Sans CJK (Debian/Ubuntu, Fedora).
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "/usr/share/fonts/google-noto-cjk/NotoSansCJK-Regular.ttc",
                // Windows.
                "C:/Windows/Fonts/msyh.ttc", // Microsoft YaHei (CJK)
                "C:/Windows/Fonts/arialuni.ttf",
            ],
        ),
        // Korean Hangul (Arial Unicode covers it, but Noto-less Linux may not).
        (
            "fallback-korean",
            &[
                "/System/Library/Fonts/AppleSDGothicNeo.ttc",
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
                "C:/Windows/Fonts/malgun.ttf",
            ],
        ),
        // Arabic.
        (
            "fallback-arabic",
            &[
                "/System/Library/Fonts/SFArabic.ttf",
                "/System/Library/Fonts/GeezaPro.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansArabic-Regular.ttf",
                "C:/Windows/Fonts/arial.ttf",
            ],
        ),
        // Hebrew.
        (
            "fallback-hebrew",
            &[
                "/System/Library/Fonts/SFHebrew.ttf",
                "/usr/share/fonts/truetype/noto/NotoSansHebrew-Regular.ttf",
                "C:/Windows/Fonts/david.ttf",
            ],
        ),
        // Thai.
        (
            "fallback-thai",
            &[
                "/System/Library/Fonts/Supplemental/Thonburi.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansThai-Regular.ttf",
                "C:/Windows/Fonts/tahoma.ttf",
            ],
        ),
        // Devanagari (Hindi, Marathi, Nepali).
        (
            "fallback-devanagari",
            &[
                "/System/Library/Fonts/Kohinoor.ttc",
                "/System/Library/Fonts/Supplemental/DevanagariMT.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansDevanagari-Regular.ttf",
                "C:/Windows/Fonts/Nirmala.ttf",
            ],
        ),
        // Bengali.
        (
            "fallback-bengali",
            &[
                "/System/Library/Fonts/KohinoorBangla.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansBengali-Regular.ttf",
                "C:/Windows/Fonts/Nirmala.ttf",
            ],
        ),
        // Tamil.
        (
            "fallback-tamil",
            &[
                "/System/Library/Fonts/Supplemental/Tamil Sangam MN.ttc",
                "/usr/share/fonts/truetype/noto/NotoSansTamil-Regular.ttf",
                "C:/Windows/Fonts/Nirmala.ttf",
            ],
        ),
    ];

    let mut keys = Vec::new();
    for (key, candidates) in GROUPS {
        for path in *candidates {
            if let Ok(bytes) = std::fs::read(path) {
                fonts
                    .font_data
                    .insert((*key).to_owned(), FontData::from_owned(bytes));
                keys.push((*key).to_owned());
                break;
            }
        }
    }
    keys
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
