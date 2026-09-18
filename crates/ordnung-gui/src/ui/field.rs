//! The app's text field.
//!
//! One box for every place the user types: the toolbar search, a filter, a
//! name, a token, a paste. It is egui's `TextEdit` under one set of
//! defaults, so every field reads as the same control. The text sits 8 pt
//! in from the sides rather than egui's 4, which left the caret and hint
//! pressed against the frame; the vertical inset stays at egui's 2, which is
//! the height `super::control_row` measures a row against. A focused field
//! shows the accent outline; a field that is the whole surface it sits on (a
//! paste box, an inline rename) can turn that `outline` off.
//!
//! The builder forwards the `TextEdit` settings a call site needs; anything
//! rarer goes through [`Field::edit`].

use super::tokens::space;
use eframe::egui::{self, Margin, Stroke, TextBuffer, Widget};

/// The standard inset of the text from the frame.
pub const MARGIN: Margin = Margin::symmetric(space::S3, space::S1);

// The builder is complete ahead of use: a setting no call site needs yet
// stays, so a new field never reaches past the component.
#[allow(dead_code)]
pub struct Field<'t> {
    edit: egui::TextEdit<'t>,
    outline: bool,
}

#[allow(dead_code)]
impl<'t> Field<'t> {
    /// A one-line field.
    pub fn singleline(text: &'t mut dyn TextBuffer) -> Self {
        Self {
            edit: egui::TextEdit::singleline(text).margin(MARGIN),
            outline: true,
        }
    }

    /// A wrapping, multi-line box.
    pub fn multiline(text: &'t mut dyn TextBuffer) -> Self {
        Self {
            edit: egui::TextEdit::multiline(text).margin(MARGIN),
            outline: true,
        }
    }

    /// Grey text shown while the field is empty.
    pub fn hint(mut self, hint: impl Into<egui::WidgetText>) -> Self {
        self.edit = self.edit.hint_text(hint);
        self
    }

    /// The width of the text area; the margin is added on top.
    /// `f32::INFINITY` fills the row.
    pub fn width(mut self, w: f32) -> Self {
        self.edit = self.edit.desired_width(w);
        self
    }

    /// How many lines a multi-line box asks for before it has text.
    pub fn rows(mut self, rows: usize) -> Self {
        self.edit = self.edit.desired_rows(rows);
        self
    }

    pub fn font(mut self, font: impl Into<egui::FontSelection>) -> Self {
        self.edit = self.edit.font(font);
        self
    }

    pub fn id(mut self, id: egui::Id) -> Self {
        self.edit = self.edit.id(id);
        self
    }

    pub fn id_salt(mut self, salt: impl std::hash::Hash) -> Self {
        self.edit = self.edit.id_salt(salt);
        self
    }

    /// Show dots instead of the text.
    pub fn password(mut self, password: bool) -> Self {
        self.edit = self.edit.password(password);
        self
    }

    /// A looser or tighter inset than [`MARGIN`].
    pub fn margin(mut self, margin: impl Into<Margin>) -> Self {
        self.edit = self.edit.margin(margin);
        self
    }

    /// The smallest the frame may be, margin included.
    pub fn min_size(mut self, size: egui::Vec2) -> Self {
        self.edit = self.edit.min_size(size);
        self
    }

    /// Whether a focused field draws the accent outline (the default).
    pub fn outline(mut self, outline: bool) -> Self {
        self.outline = outline;
        self
    }

    /// Anything the builder doesn't forward.
    pub fn edit(mut self, f: impl FnOnce(egui::TextEdit<'t>) -> egui::TextEdit<'t>) -> Self {
        self.edit = f(self.edit);
        self
    }

    /// Draw it; the same as `ui.add(field)`.
    pub fn show(self, ui: &mut egui::Ui) -> egui::Response {
        ui.add(self)
    }
}

impl Widget for Field<'_> {
    fn ui(self, ui: &mut egui::Ui) -> egui::Response {
        if self.outline {
            self.edit.ui(ui)
        } else {
            // egui frames a focused field with the selection stroke; drop
            // it for this widget alone.
            ui.scope(|ui| {
                ui.visuals_mut().selection.stroke = Stroke::NONE;
                self.edit.ui(ui)
            })
            .inner
        }
    }
}
