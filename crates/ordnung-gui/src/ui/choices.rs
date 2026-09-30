//! The row of choices a dialog ends with: Cancel on the left, the actions
//! on the right, and one way to answer from the keyboard in every dialog.
//!
//! The action is focused when the dialog opens, so Enter (or Space) takes
//! it without reaching for the mouse; Tab, Shift-Tab and the arrow keys
//! move the focus along the row, wrapping at the ends and skipping a
//! choice that's greyed out; the focused choice carries the selection
//! ring. Escape is not the row's: the [`super::window::Window`] the row
//! sits in closes on it (see [`super::escape`]), and the caller reads that
//! close as the cancel, so a dialog answers Escape the same whether or not
//! it has a row.
//!
//! The row owns the keyboard only while nothing else in the dialog holds
//! it: a checkbox or a field the user clicked keeps its focus and its
//! keys, and Tab out of it lands back on the row. Enter in a field lets
//! the field go, so it reaches the row and takes the action, the way a
//! macOS dialog's field answers Enter. On the pass the row first appears
//! it takes the focus whatever held it, the way a dialog takes it from
//! the search box behind it.
//!
//! Tab and the arrows are handled here rather than left to egui because
//! egui's focus walks every widget on screen: from the last button it
//! would step out of the dialog into the table.

use eframe::egui;

use super::button;
use super::tokens::radius;

/// How a choice is drawn: the plain button, the accent one action a dialog
/// leads with, or the red one that destroys something.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Look {
    Plain,
    Primary,
    Danger,
}

/// One choice on the row.
#[derive(Clone, Debug)]
pub struct Choice {
    pub label: String,
    pub look: Look,
    /// Drawn on the left, apart from the actions.
    pub cancel: bool,
    pub enabled: bool,
}

impl Choice {
    fn new(label: impl Into<String>, look: Look, cancel: bool) -> Self {
        Self {
            label: label.into(),
            look,
            cancel,
            enabled: true,
        }
    }

    /// The choice that leaves things as they are; on the left.
    pub fn cancel(label: impl Into<String>) -> Self {
        Self::new(label, Look::Plain, true)
    }

    /// An action drawn as a plain button.
    pub fn plain(label: impl Into<String>) -> Self {
        Self::new(label, Look::Plain, false)
    }

    /// The action the dialog is for.
    pub fn primary(label: impl Into<String>) -> Self {
        Self::new(label, Look::Primary, false)
    }

    /// An action that destroys something.
    pub fn danger(label: impl Into<String>) -> Self {
        Self::new(label, Look::Danger, false)
    }

    /// Greyed out while `enabled` is false: it keeps its place on the row
    /// and the keyboard skips it.
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// What the row remembers between passes.
#[derive(Clone, Default)]
struct State {
    /// The pass this was written in; a gap means the dialog closed and
    /// reopened, and the row starts over.
    pass: u64,
    /// The choice that carries the focus.
    focused: usize,
    /// The buttons' ids as of that pass, to tell our focus from a field's.
    ids: Vec<egui::Id>,
}

/// Draw the row; the index of the choice taken this pass, if one was, by
/// click or by key.
pub fn choices(ui: &mut egui::Ui, choices: &[Choice]) -> Option<usize> {
    if choices.is_empty() {
        return None;
    }
    let ctx = ui.ctx().clone();
    let pass = ctx.cumulative_pass_nr();
    let id = ui.make_persistent_id("ord_choices");
    let prev: Option<State> = ctx.data(|d| d.get_temp(id));
    // Not drawn last pass: the dialog is opening. (A frame may run two
    // passes, so the same pass is not a gap.)
    let fresh = prev.as_ref().map_or(true, |s| s.pass + 1 < pass);
    let default = choices
        .iter()
        .position(|c| !c.cancel && c.enabled)
        .unwrap_or(0);
    let mut focused = if fresh {
        default
    } else {
        prev.as_ref().map_or(default, |s| s.focused)
    };

    // Whose keyboard it is: the row's when it opens, when nothing has the
    // focus, or when one of its own buttons does. egui may have moved the
    // focus itself in the row's first passes (before the lock below takes),
    // so a button of ours that has it is followed rather than corrected.
    let egui_focus = ctx.memory(|m| m.focused());
    let prev_ids = prev.as_ref().map(|s| s.ids.as_slice()).unwrap_or(&[]);
    let mut ours = fresh || egui_focus.is_none();
    if let Some(i) = egui_focus.and_then(|f| prev_ids.iter().position(|id| *id == f)) {
        focused = i;
        ours = true;
    }

    let mut chosen = None;
    if ours {
        let step = |i: usize, dir: i32| -> usize {
            let n = choices.len() as i32;
            let mut j = i as i32;
            for _ in 0..n {
                j = (j + dir).rem_euclid(n);
                if choices[j as usize].enabled {
                    return j as usize;
                }
            }
            i
        };
        use egui::{Key, Modifiers};
        let moves: i32 = ctx.input_mut(|i| {
            let mut d = 0;
            if i.consume_key(Modifiers::NONE, Key::Tab) {
                d += 1;
            }
            if i.consume_key(Modifiers::SHIFT, Key::Tab) {
                d -= 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowRight)
                || i.consume_key(Modifiers::NONE, Key::ArrowDown)
            {
                d += 1;
            }
            if i.consume_key(Modifiers::NONE, Key::ArrowLeft)
                || i.consume_key(Modifiers::NONE, Key::ArrowUp)
            {
                d -= 1;
            }
            d
        });
        if moves != 0 {
            focused = step(focused, moves.signum());
        }
        let take = ctx.input_mut(|i| {
            i.consume_key(Modifiers::NONE, Key::Enter) || i.consume_key(Modifiers::NONE, Key::Space)
        });
        if take && choices[focused].enabled {
            chosen = Some(focused);
        }
    }

    // Draw: cancels on the left in order, actions on the right in order
    // (a right-to-left layout takes them last first).
    let mut ids = vec![egui::Id::NULL; choices.len()];
    let mut draw = |ui: &mut egui::Ui, i: usize| {
        let c = &choices[i];
        let resp = ui.add_enabled_ui(c.enabled, |ui| match c.look {
            Look::Plain => button::button(ui, &c.label),
            Look::Primary => button::primary(ui, &c.label),
            Look::Danger => button::danger(ui, &c.label),
        });
        let resp = resp.inner;
        ids[i] = resp.id;
        if resp.clicked() {
            chosen = Some(i);
        }
        if ours && i == focused {
            resp.request_focus();
            // Keep egui's focus from walking off the row on the keys the
            // row handles; Escape stays the window's.
            ctx.memory_mut(|m| {
                m.set_focus_lock_filter(
                    resp.id,
                    egui::EventFilter {
                        tab: true,
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        escape: false,
                    },
                )
            });
            ui.painter().rect_stroke(
                resp.rect.expand(2.0),
                egui::Rounding::same(radius::SM + 2.0),
                ui.visuals().selection.stroke,
            );
        }
    };
    ui.horizontal(|ui| {
        for i in (0..choices.len()).filter(|i| choices[*i].cancel) {
            draw(ui, i);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for i in (0..choices.len()).rev().filter(|i| !choices[*i].cancel) {
                draw(ui, i);
            }
        });
    });

    ctx.data_mut(|d| {
        d.insert_temp(
            id,
            State {
                pass,
                focused,
                ids,
            },
        )
    });
    chosen
}

#[cfg(test)]
mod tests {
    use super::super::window::Window;
    use super::*;

    fn key(key: egui::Key, shift: bool) -> egui::Event {
        egui::Event::Key {
            key,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: if shift {
                egui::Modifiers::SHIFT
            } else {
                egui::Modifiers::NONE
            },
        }
    }

    /// One pass of a dialog with Cancel and two actions, the second greyed
    /// out; what the row answered.
    fn pass(ctx: &egui::Context, events: Vec<egui::Event>) -> Option<usize> {
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let mut answer = None;
        let _ = ctx.run(input, |ctx| {
            Window::new("W").show(ctx, |ui| {
                answer = choices(
                    ui,
                    &[
                        Choice::cancel("Cancel"),
                        Choice::danger("Delete"),
                        Choice::plain("Later").enabled(false),
                    ],
                );
            });
            super::super::escape::end_pass(ctx);
        });
        answer
    }

    #[test]
    fn enter_takes_the_action_it_opened_on() {
        let ctx = egui::Context::default();
        assert_eq!(pass(&ctx, vec![]), None);
        assert_eq!(pass(&ctx, vec![key(egui::Key::Enter, false)]), Some(1));
    }

    #[test]
    fn tab_and_arrows_walk_the_row_and_skip_the_greyed_out() {
        let ctx = egui::Context::default();
        pass(&ctx, vec![]);
        // Tab from Delete wraps past the disabled Later to Cancel.
        assert_eq!(pass(&ctx, vec![key(egui::Key::Tab, false)]), None);
        assert_eq!(pass(&ctx, vec![key(egui::Key::Enter, false)]), Some(0));
        // Reopen: back on Delete; left goes to Cancel, right returns.
        pass(&ctx, vec![]);
        pass(&ctx, vec![]);
        pass(&ctx, vec![key(egui::Key::ArrowLeft, false)]);
        pass(&ctx, vec![key(egui::Key::ArrowRight, false)]);
        assert_eq!(pass(&ctx, vec![key(egui::Key::Space, false)]), Some(1));
        // Shift-Tab from Delete goes back to Cancel.
        pass(&ctx, vec![]);
        pass(&ctx, vec![]);
        pass(&ctx, vec![key(egui::Key::Tab, true)]);
        assert_eq!(pass(&ctx, vec![key(egui::Key::Enter, false)]), Some(0));
    }

    #[test]
    fn a_field_in_the_dialog_keeps_its_arrows_and_enter_in_it_takes_the_action() {
        let ctx = egui::Context::default();
        let field = egui::Id::new("f");
        let mut text = String::new();
        let mut run = |events: Vec<egui::Event>, focus: bool| {
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(800.0, 600.0),
                )),
                events,
                focused: true,
                ..Default::default()
            };
            let mut answer = None;
            let _ = ctx.run(input, |ctx| {
                Window::new("W").show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(&mut text).id(field));
                    if focus {
                        r.request_focus();
                    }
                    answer = choices(ui, &[Choice::cancel("Cancel"), Choice::primary("Save")]);
                });
            });
            answer
        };
        run(vec![], false);
        run(vec![], true);
        run(vec![], false);
        // The field's cursor moves; the row's focus stays on Save.
        assert_eq!(run(vec![key(egui::Key::ArrowLeft, false)], false), None);
        // Enter in the field lets it go and takes the action, not Cancel.
        assert_eq!(run(vec![key(egui::Key::Enter, false)], false), Some(1));
    }
}
