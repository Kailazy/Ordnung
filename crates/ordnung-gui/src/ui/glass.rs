//! Glass: the one translucent surface every floating thing in the app sits
//! on. A window, a dropdown and the search popup all share it, so they read
//! as one material rather than three shades of grey.
//!
//! Glass is two layers. Under it, the app blurred (a live [`frost`]
//! backdrop, re-made every frame); over that, one tint
//! ([`color::SURFACE_GLASS`], the standard opacity) that keeps the surface
//! dark enough to read on. The tint alone would show the app through in
//! outline, sharp text under sharp text; the blur is what makes it glass.
//! Both are painted in the surface's own layer, under its content, so a
//! window over a window frosts the one beneath it.
//!
//! Each surface keeps its own backdrop texture here, keyed by its id, for
//! as long as it is up. Use: [`begin`] at the top of the surface's content,
//! [`end`] once its rect is known. `window` and `menu` wrap this; only a
//! new kind of surface needs to call it directly. The app calls [`sweep`]
//! at the top of its frame and [`render`] at the bottom, once every surface
//! has painted.

use super::frost::{self, Backdrop, Cut, Engine};
use super::tokens::color;
use eframe::egui;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One surface: its backdrop, the last pass it was open in, the last pass
/// it actually drew, and where it was that pass.
struct Entry {
    backdrop: Option<Backdrop>,
    /// The layer the surface paints in.
    layer: egui::LayerId,
    shown: u64,
    drawn: u64,
    /// The screen the surface frosts, and where it sits, as of `shown`.
    src: egui::Rect,
    rect: egui::Rect,
}

struct Inner {
    entries: HashMap<egui::Id, Entry>,
    /// The open surfaces' layers, bottom to top within each `Order`: egui
    /// keeps its own stacking order private, so this mirrors its rules (a
    /// new area goes on top, a press on an area raises it) for the layers
    /// that carry glass.
    z: Vec<egui::LayerId>,
    /// `None` without a wgpu render state: surfaces go on tint alone.
    engine: Option<Engine>,
}

/// Every surface's state. Lives in the context's memory so any surface can
/// find its own without the app threading state to it.
#[derive(Clone)]
struct Registry(Arc<Mutex<Inner>>);

fn key() -> egui::Id {
    egui::Id::new("ordnung-glass-registry")
}

fn registry(ctx: &egui::Context) -> Option<Registry> {
    ctx.data(|d| d.get_temp::<Registry>(key()))
}

/// Put the registry in place. Once, when the app starts, with the render
/// state the backdrops are made through.
pub fn install(ctx: &egui::Context, render_state: Option<eframe::egui_wgpu::RenderState>) {
    let reg = Registry(Arc::new(Mutex::new(Inner {
        entries: HashMap::new(),
        z: Vec::new(),
        engine: render_state.map(Engine::new),
    })));
    ctx.data_mut(|d| d.insert_temp(key(), reg));
}

/// Forget every surface that has closed. Once a frame, at the top, before
/// anything paints: a closed surface's backdrop texture is freed here, where
/// nothing encoded this frame can still refer to it.
pub fn sweep(ctx: &egui::Context) {
    let Some(reg) = registry(ctx) else {
        return;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut inner = reg.0.lock().unwrap();
    let Inner { entries, z, engine } = &mut *inner;
    entries.retain(|_, e| {
        if e.shown + 1 >= pass {
            return true;
        }
        if let (Some(engine), Some(b)) = (engine.as_mut(), e.backdrop.take()) {
            engine.free(b);
        }
        false
    });
    z.retain(|l| entries.values().any(|e| e.layer == *l));
}

/// Whether this pass is the surface's first drawn since it opened: what it
/// does once per open spell (take its place on screen) it does now. Ask
/// before [`drawn`] records this pass.
pub fn opening(ctx: &egui::Context, id: egui::Id) -> bool {
    let Some(reg) = registry(ctx) else {
        return false;
    };
    let pass = ctx.cumulative_pass_nr();
    let inner = reg.0.lock().unwrap();
    inner.entries.get(&id).map_or(true, |e| e.drawn + 1 < pass)
}

/// Record that the surface drew this pass.
pub fn drawn(ctx: &egui::Context, id: egui::Id) {
    let Some(reg) = registry(ctx) else {
        return;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut inner = reg.0.lock().unwrap();
    if let Some(e) = inner.entries.get_mut(&id) {
        e.drawn = pass;
    }
}

/// The slots the glass will be painted into, reserved under the content.
pub struct Slot {
    painter: egui::Painter,
    layer: egui::LayerId,
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
        layer: ui.layer_id(),
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
    end_from(ctx, slot, id, rect, rect, rounding, stroke);
}

/// [`end`] for a surface that frosts the screen under `src` rather than
/// under itself: the player bar at the window's bottom, which shows the
/// content just above it as the continuation of what it covers. A `src`
/// clear of `rect` gets everything on screen in its frost, the surface's
/// own layer included.
pub fn end_from(
    ctx: &egui::Context,
    slot: Slot,
    id: egui::Id,
    src: egui::Rect,
    rect: egui::Rect,
    rounding: egui::Rounding,
    stroke: egui::Stroke,
) {
    let pass = ctx.cumulative_pass_nr();
    let tex = registry(ctx).and_then(|reg| {
        let mut inner = reg.0.lock().unwrap();
        let Inner { entries, z, engine } = &mut *inner;
        let e = entries.entry(id).or_insert_with(|| Entry {
            backdrop: None,
            layer: slot.layer,
            shown: 0,
            drawn: 0,
            src,
            rect,
        });
        e.shown = pass;
        e.layer = slot.layer;
        e.src = src;
        e.rect = rect;
        // A layer new to the stack goes on top, as egui puts a new area.
        if !z.contains(&slot.layer) {
            z.push(slot.layer);
        }
        let engine = engine.as_mut()?;
        Some(e.backdrop.get_or_insert_with(|| engine.backdrop(ctx)).id())
    });
    if let Some(tex) = tex {
        slot.painter.set(slot.frost, frost_shape(ctx.screen_rect(), tex, src, rect, rounding));
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

/// The backdrop texture, which covers `screen`, drawn at `rect` showing the
/// part of it under `src`.
fn frost_shape(
    screen: egui::Rect,
    tex: egui::TextureId,
    src: egui::Rect,
    rect: egui::Rect,
    rounding: egui::Rounding,
) -> egui::Shape {
    let uv = |p: egui::Pos2| {
        egui::pos2(
            (p.x - screen.min.x) / screen.width().max(1.0),
            (p.y - screen.min.y) / screen.height().max(1.0),
        )
    };
    egui::Shape::Rect(egui::epaint::RectShape {
        rect,
        rounding,
        fill: egui::Color32::WHITE,
        stroke: egui::Stroke::NONE,
        blur_width: 0.0,
        fill_texture_id: tex,
        uv: egui::Rect::from_min_max(uv(src.min), uv(src.max)),
    })
}

/// Make this frame's backdrops. Once a frame, at the bottom, after every
/// surface has painted: the frame's shapes are taken in paint order, each
/// open surface's frost is made from those painted before its own, and the
/// textures are ready before egui draws the frame.
pub fn render(ctx: &egui::Context) {
    let Some(reg) = registry(ctx) else {
        return;
    };
    let pass = ctx.cumulative_pass_nr();
    let mut inner = reg.0.lock().unwrap();
    let Inner { entries, z, engine } = &mut *inner;
    let Some(engine) = engine.as_mut() else {
        return;
    };
    // A press on a surface raises it, as egui raises the area.
    let pressed_on = ctx.input(|i| {
        i.pointer
            .any_pressed()
            .then(|| i.pointer.interact_pos())
            .flatten()
    });
    if let Some(layer) = pressed_on.and_then(|pos| ctx.layer_id_at(pos)) {
        if let Some(k) = z.iter().position(|l| *l == layer) {
            let layer = z.remove(k);
            z.push(layer);
        }
    }
    // The surfaces up this pass, with the texture each paints.
    let mut open: Vec<(&mut Entry, egui::TextureId)> = entries
        .values_mut()
        .filter(|e| e.shown == pass)
        .filter_map(|e| {
            let id = e.backdrop.as_ref()?.id();
            Some((e, id))
        })
        .collect();
    if open.is_empty() {
        return;
    }
    // The frame's shapes as egui will paint them, from a copy of its layers.
    let transforms = ctx.memory(|m| m.layer_transforms.clone());
    let mut layers = ctx.graphics(|g| g.clone());
    let shapes = layers.drain(z, &transforms);
    // Where each surface's own frost is in that list: everything before it
    // goes into its backdrop. A surface frosting somewhere it doesn't cover
    // takes the whole frame.
    let mut positions: Vec<(usize, usize)> = open
        .iter()
        .enumerate()
        .filter_map(|(k, (e, tex))| {
            if !e.src.intersects(e.rect) {
                return Some((shapes.len(), k));
            }
            shapes
                .iter()
                .position(|s| matches!(&s.shape, egui::Shape::Rect(r) if r.fill_texture_id == *tex))
                .map(|p| (p, k))
        })
        .collect();
    positions.sort_unstable();
    // Only what can show in some frost is drawn again.
    let reach: Vec<egui::Rect> = open.iter().map(|(e, _)| e.src.expand(frost::REACH)).collect();
    let mut kept = Vec::with_capacity(shapes.len());
    let mut cuts_at = Vec::with_capacity(positions.len());
    let mut next = positions.iter().peekable();
    for (i, s) in shapes.into_iter().enumerate() {
        while next.peek().is_some_and(|(p, _)| *p == i) {
            cuts_at.push((kept.len(), next.next().unwrap().1));
        }
        if matches!(s.shape, egui::Shape::Noop | egui::Shape::Callback(_)) {
            continue;
        }
        let bounds = s.clip_rect.intersect(s.shape.visual_bounding_rect());
        if reach.iter().any(|r| r.intersects(bounds)) {
            kept.push(s);
        }
    }
    for (_, k) in next {
        cuts_at.push((kept.len(), *k));
    }
    // Hand each surface's backdrop over in cut order.
    let mut backdrops: Vec<Option<&mut Backdrop>> = open
        .iter_mut()
        .map(|(e, _)| e.backdrop.as_mut())
        .collect();
    let cuts: Vec<Cut<'_>> = cuts_at
        .into_iter()
        .filter_map(|(position, k)| {
            let backdrop = backdrops[k].take()?;
            Some(Cut { position, backdrop })
        })
        .collect();
    engine.render(ctx, kept, cuts);
}
