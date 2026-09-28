//! Escape closes the top surface, one per press, in the order they're
//! stacked on screen.
//!
//! Every surface that Escape can close (a [`super::window::Window`] with a
//! close flag, a [`super::menu`] dropdown) asks [`take`] each pass whether
//! this pass's Escape is its own. The answer is yes for one surface only:
//! the top one by egui's layer order among the surfaces that asked last
//! pass, so a dialog over the Settings window closes first and the Settings
//! window on the next press, whatever order the code draws them in. The
//! surface that takes it consumes the key, so nothing under it (the table
//! clearing its selection) acts on the same press.
//!
//! A text field outside every surface (the sidebar's inline rename, the
//! search box, a cue's name) keeps its Escape: that press cancels the edit
//! and closes nothing. A field inside a surface doesn't hold it back, the
//! way Escape in a macOS dialog's field cancels the dialog.

use eframe::egui;

#[derive(Clone, Default)]
struct Stack {
    /// The pass `this` is collecting for.
    pass: u64,
    /// Surfaces that asked this pass.
    this: Vec<egui::LayerId>,
    /// Surfaces that asked last pass: the candidates for this pass's Escape.
    last: Vec<egui::LayerId>,
    /// The layer of the widget that had keyboard focus as the last pass
    /// ended. egui drops focus at the start of the pass an Escape arrives
    /// in, before anything draws, so this pass can't be asked.
    focus: Option<egui::LayerId>,
    /// An egui context menu or popup was up as the last pass ended: it
    /// closes on this Escape itself (without consuming it), and takes the
    /// press.
    menu: bool,
}

fn stack_id() -> egui::Id {
    egui::Id::new("ord_escape_stack")
}

fn stack(ctx: &egui::Context) -> Stack {
    let pass = ctx.cumulative_pass_nr();
    let mut s: Stack = ctx.data(|d| d.get_temp(stack_id())).unwrap_or_default();
    if s.pass != pass {
        s.last = std::mem::take(&mut s.this);
        s.pass = pass;
    }
    s
}

/// Note where keyboard focus sits and whether an egui menu is up, as the
/// pass ends: what the next pass's Escape was pressed over. Called once,
/// after everything drew.
pub fn end_pass(ctx: &egui::Context) {
    let mut s = stack(ctx);
    let focused = ctx.memory(|m| m.focused());
    s.focus = focused.and_then(|id| ctx.read_response(id)).map(|r| r.layer_id);
    s.menu = ctx.is_context_menu_open() || ctx.memory(|m| m.any_popup_open());
    ctx.data_mut(|d| d.insert_temp(stack_id(), s));
}

/// Whether some surface is up to take an Escape: for code under the
/// surfaces that would otherwise act on the same press.
pub fn pending(ctx: &egui::Context) -> bool {
    !stack(ctx).last.is_empty()
}

/// Register `layer` as a surface Escape closes, and answer whether this
/// pass's Escape is its to act on; when it is, the key is consumed.
/// `held` keeps the press from it (a menu whose own search field is
/// typing), without handing it to the surface underneath.
pub fn take(ctx: &egui::Context, layer: egui::LayerId, held: bool) -> bool {
    let mut s = stack(ctx);
    if !s.this.contains(&layer) {
        s.this.push(layer);
    }
    let candidates: Vec<egui::LayerId> = s.last.iter().chain([&layer]).copied().collect();
    let (focus, menu) = (s.focus, s.menu);
    ctx.data_mut(|d| d.insert_temp(stack_id(), s));

    if held || !ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
        return false;
    }
    // A field outside every surface is being edited: its Escape.
    if focus.is_some_and(|f| !candidates.contains(&f)) {
        return false;
    }
    // An egui context menu or popup (a combo box's list) is on top and
    // closes itself.
    if menu || ctx.memory(|m| m.any_popup_open()) {
        return false;
    }
    let top = ctx.memory(|m| m.layer_ids().filter(|l| candidates.contains(l)).last());
    if top.unwrap_or(layer) != layer {
        return false;
    }
    ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
}

#[cfg(test)]
mod tests {
    use super::super::window::Window;
    use super::*;

    /// One pass with windows "A" and "B" drawn in that order (each while
    /// its flag is up) and, optionally, Escape pressed.
    fn pass(ctx: &egui::Context, esc: bool, a: &mut bool, b: &mut bool, raise_a: bool) {
        let events = if esc {
            vec![egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            }]
        } else {
            vec![]
        };
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
            events,
            ..Default::default()
        };
        let _ = ctx.run(input, |ctx| {
            Window::new("A").open(a).show(ctx, |ui| ui.label("a"));
            Window::new("B")
                .open(b)
                .at(egui::Align2::LEFT_TOP, egui::pos2(40.0, 40.0))
                .show(ctx, |ui| ui.label("b"));
            if raise_a {
                ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, egui::Id::new("A")));
            }
            end_pass(ctx);
        });
    }

    #[test]
    fn escape_closes_the_top_window_first_whatever_the_draw_order() {
        let ctx = egui::Context::default();
        let (mut a, mut b) = (true, true);
        // A draws first but is raised above B.
        for _ in 0..3 {
            pass(&ctx, false, &mut a, &mut b, true);
        }
        pass(&ctx, true, &mut a, &mut b, false);
        assert!(!a && b, "the top window (A) closes, B stays: a={a} b={b}");
        pass(&ctx, false, &mut a, &mut b, false);
        pass(&ctx, true, &mut a, &mut b, false);
        assert!(!b, "the next press closes B");
    }

    #[test]
    fn a_field_outside_every_window_keeps_its_escape() {
        let ctx = egui::Context::default();
        let mut open = true;
        let mut text = String::new();
        let field = egui::Id::new("rename");
        let run = |ctx: &egui::Context, esc: bool, open: &mut bool, text: &mut String, focus: bool| {
            let events = if esc {
                vec![egui::Event::Key {
                    key: egui::Key::Escape,
                    physical_key: None,
                    pressed: true,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                }]
            } else {
                vec![]
            };
            let input = egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))),
                events,
                // A text field lets go of the keyboard when the window loses focus.
                focused: true,
                ..Default::default()
            };
            let _ = ctx.run(input, |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    let r = ui.add(egui::TextEdit::singleline(text).id(field));
                    if focus {
                        r.request_focus();
                    }
                });
                Window::new("W").open(open).show(ctx, |ui| ui.label("w"));
                end_pass(ctx);
            });
        };
        run(&ctx, false, &mut open, &mut text, true);
        run(&ctx, false, &mut open, &mut text, false);
        run(&ctx, true, &mut open, &mut text, false);
        assert!(open, "Escape in the sidebar's field must not close the window");
        run(&ctx, false, &mut open, &mut text, false);
        run(&ctx, true, &mut open, &mut text, false);
        assert!(!open, "with the field let go, Escape closes the window");
    }
}
