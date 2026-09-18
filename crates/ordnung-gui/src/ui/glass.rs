//! Glass: the one translucent surface every floating thing in the app sits
//! on. A window, a dropdown and the search popup all share it, so they read
//! as one material rather than three shades of grey.
//!
//! Glass is two layers. Under it, the app blurred (a [`Frost`] snapshot);
//! over that, one tint ([`color::SURFACE_GLASS`], the standard opacity)
//! that keeps the surface dark enough to read on. The tint alone would show
//! the app through in outline, sharp text under sharp text; the blur is what
//! makes it glass. Both are painted in the window's own layer, under its
//! content, so a window over a window frosts the one beneath it.
//!
//! The snapshot is the costly part (see `frost`). Each surface keeps its own
//! [`Frost`] here, keyed by its id, across its open spell; a surface that can
//! see its opening coming (a press on the anchor, on the search field, on a
//! record that opens the sheet) calls [`prime`] on the press, so the
//! snapshot is in hand before the click. One that can't falls back to taking
//! it on open, and sits out the frame or two that takes.
//!
//! Use: [`begin`] at the top of the surface's content, [`end`] once its
//! rect is known. `window` and `menu` wrap this; only a new kind of surface
//! needs to call it directly.

use super::frost::Frost;
use super::tokens::color;
use crate::tex::TexGraveyard;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One surface's frost, and the last pass it was drawn in.
struct Entry {
    frost: Frost,
    shown: u64,
}

/// Every surface's frost, and the graveyard their textures retire to. Lives
/// in the context's memory so any surface can find its own without the app
/// threading state to it.
#[derive(Clone)]
struct Registry {
    entries: Arc<Mutex<HashMap<egui::Id, Entry>>>,
    graveyard: TexGraveyard,
}

fn key() -> egui::Id {
    egui::Id::new("ordnung-glass-registry")
}

fn registry(ctx: &egui::Context) -> Option<Registry> {
    ctx.data(|d| d.get_temp::<Registry>(key()))
}

/// Put the registry in place. Once, when the app starts, with the graveyard
/// every texture handle must retire through.
pub fn install(ctx: &egui::Context, graveyard: TexGraveyard) {
    let reg = Registry {
        entries: Arc::new(Mutex::new(HashMap::new())),
        graveyard,
    };
    ctx.data_mut(|d| d.insert_temp(key(), reg));
}

/// Move every closed surface's frost along. Once a frame, before any
/// surface is drawn: a surface that closed since the last pass forgets its
/// snapshot (its next open is over a different screen); a primed snapshot
/// nobody opened on lands, or expires.
pub fn sweep(ctx: &egui::Context) {
    let Some(reg) = registry(ctx) else {
        return;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut entries = reg.entries.lock().unwrap();
    for e in entries.values_mut() {
        if e.shown + 1 >= pass {
            // Open: the surface polls its own frost as it draws.
            continue;
        }
        if e.shown + 2 == pass {
            e.frost.clear();
            continue;
        }
        e.frost.poll(ctx, &reg.graveyard);
        e.frost.expire();
    }
}

/// Take the surface's snapshot now, ahead of it opening. Call on the press
/// that will open it (while it's closed, and nothing that shouldn't be in
/// its backdrop, like a menu, is up).
pub fn prime(ctx: &egui::Context, id: egui::Id) {
    let Some(reg) = registry(ctx) else {
        return;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut entries = reg.entries.lock().unwrap();
    let e = entries.entry(id).or_insert_with(|| Entry {
        frost: Frost::new(),
        shown: 0,
    });
    if e.shown + 1 < pass {
        e.frost.prime(ctx);
    }
}

/// Whether the surface may draw this pass. `false` means it must sit the
/// pass out: its backdrop is being snapshotted, and drawing it now would
/// put it in its own backdrop. Marks the surface as open either way.
pub fn ready(ctx: &egui::Context, id: egui::Id) -> bool {
    let Some(reg) = registry(ctx) else {
        return true;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut entries = reg.entries.lock().unwrap();
    let e = entries.entry(id).or_insert_with(|| Entry {
        frost: Frost::new(),
        shown: 0,
    });
    e.shown = pass;
    e.frost.ready(ctx, &reg.graveyard)
}

/// The slots the glass will be painted into, reserved under the content.
pub struct Slot {
    painter: egui::Painter,
    frost: egui::layers::ShapeIdx,
    tint: egui::layers::ShapeIdx,
}

/// Reserve the glass under this surface's content. Call first thing inside
/// the surface's frame, whose own fill must be transparent: the glass is the
/// fill. The reservation carries the ui's opacity, so a surface fading in
/// or out takes its glass with it.
pub fn begin(ui: &mut egui::Ui) -> Slot {
    let frost = ui.painter().add(egui::Shape::Noop);
    let tint = ui.painter().add(egui::Shape::Noop);
    // The content painter is clipped to the content; the glass covers the
    // frame. Same layer, same opacity, the screen as the clip.
    let mut painter = ui.ctx().layer_painter(ui.layer_id());
    painter.multiply_opacity(ui.painter().opacity());
    Slot {
        painter,
        frost,
        tint,
    }
}

/// Paint the glass into its slots, now that the surface's `rect` is known.
/// `stroke` is the surface's edge, drawn over the tint so it isn't dimmed
/// by it.
pub fn end(
    ctx: &egui::Context,
    slot: Slot,
    id: egui::Id,
    rect: egui::Rect,
    rounding: egui::Rounding,
    stroke: egui::Stroke,
) {
    let frost = registry(ctx).and_then(|reg| {
        let entries = reg.entries.lock().unwrap();
        entries.get(&id).and_then(|e| e.frost.shape(rect, rounding))
    });
    if let Some(shape) = frost {
        slot.painter.set(slot.frost, shape);
    }
    slot.painter.set(
        slot.tint,
        egui::Shape::Rect(egui::epaint::RectShape::new(
            rect,
            rounding,
            color::SURFACE_GLASS,
            stroke,
        )),
    );
}
