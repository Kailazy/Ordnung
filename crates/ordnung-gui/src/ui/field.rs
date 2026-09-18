//! The app's text field.
//!
//! One box for every place the user types: the toolbar search, a filter, a
//! name, a token, a paste. It is egui's `TextEdit` under one set of
//! defaults, so every field reads as the same control. The text sits 12 pt
//! in from the sides and 8 pt from the top and bottom rather than egui's
//! 4 × 2, which made a field a squat 20 pt bar with the caret and hint
//! pressed against the frame, so that the toolbar search, the table filter
//! and the sidebar rename each padded it back out by hand. At the body
//! size the box comes out 32 pt tall, twice its text: the proportion of a
//! standard desktop input, and the one height every row of controls is
//! measured against ([`super::control_h`]). A push button on its own is
//! shorter; on a row with a field it goes through [`super::control_row`],
//! which brings it up to the field. A focused field shows the accent
//! outline; a field that is the whole surface it sits on (a paste box, an
//! inline rename) can turn that `outline` off.
//!
//! The builder forwards the `TextEdit` settings a call site needs; anything
//! rarer goes through [`Field::edit`].

use super::tokens::space;
use eframe::egui::{self, Margin, Stroke, TextBuffer, Widget};

/// The standard inset of the text from the frame: 12 × 8.
pub const MARGIN: Margin = Margin::symmetric(space::S4, space::S3);

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


#[cfg(test)]
mod tests {
    use super::*;

    /// Lay `add` out in a row on a themed context and hand back the height
    /// the row took, which is the height the control was allocated (a
    /// `TextEdit`'s own response rect is the text, not the frame).
    fn row_height(add: impl FnOnce(&mut egui::Ui)) -> f32 {
        let ctx = egui::Context::default();
        super::super::theme::install(&ctx);
        let mut h = 0.0;
        let mut add = Some(add);
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            egui::CentralPanel::default().show(ctx, |ui| {
                h = ui.horizontal(|ui| add.take().unwrap()(ui)).response.rect.height();
            });
        });
        h
    }

    /// The field is the row height: 32 pt at the body size.
    #[test]
    fn field_is_the_control_height() {
        let mut text = String::new();
        let field = row_height(|ui| {
            ui.add(Field::singleline(&mut text).hint("Search"));
        });
        let mut row = 0.0;
        row_height(|ui| row = super::super::control_h(ui));
        assert_eq!(field, row, "field {field} vs control_h {row}");
        assert_eq!(field, 32.0);
    }

    /// A button beside a field, laid out through the row rule, comes out
    /// exactly the field's height rather than its own shorter one.
    #[test]
    fn control_row_brings_the_button_to_the_field() {
        let mut text = String::new();
        let mut field = 0.0;
        let mut button = 0.0;
        row_height(|ui| {
            super::super::control_row(ui, |ui| {
                field = ui.add(Field::singleline(&mut text)).rect.height() + MARGIN.sum().y;
                button = super::super::button::button(ui, "Add").rect.height();
            });
        });
        assert_eq!(field, button, "field {field} vs button {button}");
        assert_eq!(button, 32.0);
    }
}
