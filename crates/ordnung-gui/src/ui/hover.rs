//! Tooltip / hover-text styling. egui has no per-tooltip text style, so we route
//! hover copy through a small helper so every note reads consistently. Tooltips
//! use the same proportional UI font as the body, just one shade darker than the
//! primary label so they read as quiet, supplementary chrome. Call sites use
//! [`HoverNoteExt::on_hover_note`] in place of egui's `Response::on_hover_text`;
//! [`note`] styles ad-hoc labels inside `on_hover_ui` closures the same way.

use eframe::egui::{self, Color32, RichText};

/// Name of the serif font family installed in [`super::theme`]. No longer used
/// for tooltips, but the family is still registered for any future serif copy.
pub const SERIF_FAMILY: &str = "serif";

/// Hover-text point size — matches the 13pt UI body.
const SIZE: f32 = 13.0;

/// Tooltip text colour: a shade darker than the primary `LABEL` (235) so notes
/// recede slightly against the body text without dropping to a faint gray.
const COLOR: Color32 = Color32::from_rgb(205, 205, 211);

/// Wrap a hover string in the tooltip style — the main UI font, one shade darker.
/// Use inside `on_hover_ui` closures: `ui.label(hover::note("…"))`.
pub fn note(text: impl Into<String>) -> RichText {
    RichText::new(text.into()).size(SIZE).color(COLOR)
}

/// Extension giving every widget a serif tooltip. Drop-in for egui's
/// `Response::on_hover_text`, but renders the copy in the formal serif face.
pub trait HoverNoteExt {
    fn on_hover_note(self, text: impl Into<String>) -> Self;
}

impl HoverNoteExt for egui::Response {
    fn on_hover_note(self, text: impl Into<String>) -> Self {
        self.on_hover_text(note(text))
    }
}
