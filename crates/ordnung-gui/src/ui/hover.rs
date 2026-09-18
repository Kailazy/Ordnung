//! Tooltip / hover-text styling. egui has no per-tooltip text style, so we route
//! hover copy through a small helper so every note reads consistently. Tooltips
//! use the same proportional UI font as the body, just one shade darker than the
//! primary label so they read as quiet, supplementary chrome. Call sites use
//! [`HoverNoteExt::on_hover_note`] in place of egui's `Response::on_hover_text`;
//! [`note`] styles ad-hoc labels inside `on_hover_note_ui` closures the same
//! way. Every note is shown through [`super::tooltip`], on glass; nothing
//! calls egui's `on_hover_*` directly.

use super::tooltip;
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

/// Extension giving every widget a tooltip in the note style, on glass.
/// Drop-ins for egui's `on_hover_text`, `on_disabled_hover_text`,
/// `on_hover_ui` and `on_hover_ui_at_pointer`.
pub trait HoverNoteExt {
    /// `text`, styled as a note, beside the widget while it is enabled.
    fn on_hover_note(self, text: impl Into<String>) -> Self;
    /// `text`, styled as a note, beside the widget while it is disabled.
    fn on_disabled_hover_note(self, text: impl Into<String>) -> Self;
    /// A note with its own layout; style its labels with [`note`].
    fn on_hover_note_ui(self, add: impl FnOnce(&mut egui::Ui)) -> Self;
    /// [`Self::on_hover_note_ui`] at the pointer, for a widget too large to
    /// sit a note beside.
    fn on_hover_note_at_pointer(self, add: impl FnOnce(&mut egui::Ui)) -> Self;
}

impl HoverNoteExt for egui::Response {
    fn on_hover_note(self, text: impl Into<String>) -> Self {
        let text = note(text);
        tooltip::on_hover_ui(self, |ui| {
            ui.label(text);
        })
    }

    fn on_disabled_hover_note(self, text: impl Into<String>) -> Self {
        let text = note(text);
        tooltip::on_disabled_hover_ui(self, |ui| {
            ui.label(text);
        })
    }

    fn on_hover_note_ui(self, add: impl FnOnce(&mut egui::Ui)) -> Self {
        tooltip::on_hover_ui(self, add)
    }

    fn on_hover_note_at_pointer(self, add: impl FnOnce(&mut egui::Ui)) -> Self {
        tooltip::on_hover_ui_at_pointer(self, add)
    }
}
