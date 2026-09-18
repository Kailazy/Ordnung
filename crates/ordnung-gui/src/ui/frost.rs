//! Frosted backdrop for a floating window.
//!
//! egui paints every layer straight to one surface, so a see-through window
//! can only tint what's under it: at any alpha that lets the app show, it shows
//! in outline. What a frosted surface wants is the app *blurred*, and there is
//! no backdrop blur to ask the GPU for. So the frost is made the long way round:
//! the frame is snapshotted before the window is drawn, shrunk and box-blurred,
//! and painted under the window from then on, mapped so the window sees the
//! part of the snapshot it sits over.
//!
//! The snapshot is the expensive part, and it can't be made cheap: reading the
//! frame back stalls the main thread on the GPU and a full-resolution pixel
//! copy (about 40 ms on a Retina display in a release build, ten times that in
//! debug). Taken when the window opens, that stall lands on the click and the
//! whole app hitches. So it is taken on the mouse *press* instead, while the
//! window is still closed ([`Frost::prime`]): a press is a moment when nothing
//! on screen is moving, the click that opens the window comes a frame or more
//! later, and by then the snapshot is in hand. The blur runs on a worker
//! thread so it never costs the frame anything. A window opened without a
//! press (keyboard, a menu item) falls back to snapshotting on open, holding
//! the window back for the one frame that takes.
//!
//! Whatever moves under the window after the snapshot is stale in it, but at
//! this blur nothing under the window has a shape to be stale in, and taking
//! it again would only capture the window itself.

use crate::tex::{Tex, TexGraveyard};
use eframe::egui;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long to hold the window back for a snapshot that isn't coming (a
/// backend without screenshots) before drawing it plain.
const GIVE_UP: Duration = Duration::from_millis(300);

/// How long a primed snapshot stays good for while the window is closed. The
/// click a press is for follows it within this; a window opened later isn't
/// the one the press was for, and what's on screen may have changed since.
const PRIME_TTL: Duration = Duration::from_secs(1);

/// Long side of the blurred snapshot, in texels. Small enough that the blur
/// is free; magnifying it back up with bilinear filtering is itself most of
/// the softness.
const TEXELS: usize = 320;

/// Box-blur radius in texels per pass; three passes each way approximates a
/// Gaussian of about that many texels' sigma.
const RADIUS: usize = 3;
const PASSES: usize = 3;

enum State {
    /// Nothing asked for yet.
    Fresh,
    /// The snapshot is in flight since then. The window must not draw: it
    /// would end up in its own backdrop.
    Asked(Instant),
    /// The snapshot, taken then, is being blurred on a worker thread.
    Blurring(Receiver<egui::ColorImage>, Instant),
    /// The blurred snapshot, taken then, ready to paint.
    Have(Tex, Instant),
    /// No snapshot came; the window goes on plain.
    Bare,
}

/// A window's frosted backdrop. Keep one per window across its open spell and
/// [`Frost::clear`] it when the window closes, so the next open takes a fresh
/// snapshot. `glass` keeps one per window and does this bookkeeping.
pub struct Frost {
    state: State,
    /// The screen the snapshot covers, in points, to map a window rect onto it.
    screen: egui::Rect,
}

impl Frost {
    pub fn new() -> Self {
        Self {
            state: State::Fresh,
            screen: egui::Rect::NOTHING,
        }
    }

    /// Take the snapshot now, ahead of the window opening. Call on a mouse
    /// press while the window is closed (and nothing that shouldn't be in the
    /// backdrop, like a menu, is up): the stall lands under the press, and
    /// the click that follows finds the snapshot ready. A newer press
    /// replaces an older snapshot; one already in flight is left to land.
    pub fn prime(&mut self, ctx: &egui::Context) {
        if matches!(self.state, State::Asked(_) | State::Blurring(..)) {
            return;
        }
        self.ask(ctx);
    }

    /// Drop a primed snapshot nobody opened a window on within
    /// [`PRIME_TTL`]. Call once a frame while the window is closed, never
    /// while it's open: the snapshot under an open window stays as long as
    /// the window does.
    pub fn expire(&mut self) {
        if let State::Have(_, taken) = &self.state {
            if taken.elapsed() > PRIME_TTL {
                self.state = State::Fresh;
            }
        }
    }

    /// Move a snapshot along: pick up the frame when it lands, the blur when
    /// it's done. Call once a frame whether or not the window is open; the
    /// frame arrives as an input event that is only there for one frame.
    pub fn poll(&mut self, ctx: &egui::Context, graveyard: &TexGraveyard) {
        match &self.state {
            State::Asked(since) => {
                let shot = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
                if let Some(image) = shot {
                    let (tx, rx) = mpsc::channel();
                    let ctx = ctx.clone();
                    std::thread::spawn(move || {
                        if tx.send(blur(&image)).is_ok() {
                            ctx.request_repaint();
                        }
                    });
                    self.state = State::Blurring(rx, *since);
                } else if since.elapsed() > GIVE_UP {
                    self.state = State::Bare;
                } else {
                    ctx.request_repaint();
                }
            }
            State::Blurring(rx, taken) => {
                if let Ok(blurred) = rx.try_recv() {
                    let tex = ctx.load_texture("frost", blurred, egui::TextureOptions::LINEAR);
                    self.state = State::Have(graveyard.wrap(tex), *taken);
                }
            }
            State::Fresh | State::Have(..) | State::Bare => {}
        }
    }

    /// Call once a frame before drawing the window. `false` means the window
    /// must sit this frame out: the screen under it is being snapshotted, and
    /// drawing it now would put it in its own backdrop (or the blur is a
    /// frame from done, and the window shouldn't flash plain first).
    pub fn ready(&mut self, ctx: &egui::Context, graveyard: &TexGraveyard) -> bool {
        if matches!(self.state, State::Fresh) {
            self.ask(ctx);
            return false;
        }
        self.poll(ctx, graveyard);
        match self.state {
            State::Have(..) | State::Bare => true,
            State::Blurring(..) => {
                ctx.request_repaint();
                false
            }
            State::Asked(_) | State::Fresh => false,
        }
    }

    fn ask(&mut self, ctx: &egui::Context) {
        self.screen = ctx.screen_rect();
        ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
        self.state = State::Asked(Instant::now());
        ctx.request_repaint();
    }

    /// The frost under a window occupying `rect`, as a shape for the caller
    /// to place: under the window's tint, in the window's own layer (see
    /// `glass`), so a window over another window blurs that one too. `None`
    /// until a snapshot is in hand, or when none came.
    pub fn shape(&self, rect: egui::Rect, rounding: egui::Rounding) -> Option<egui::Shape> {
        let State::Have(tex, _) = &self.state else {
            return None;
        };
        let s = self.screen;
        if s.width() <= 0.0 || s.height() <= 0.0 {
            return None;
        }
        let uv = |p: egui::Pos2| {
            egui::pos2((p.x - s.min.x) / s.width(), (p.y - s.min.y) / s.height())
        };
        Some(egui::Shape::Rect(egui::epaint::RectShape {
            rect,
            rounding,
            fill: egui::Color32::WHITE,
            stroke: egui::Stroke::NONE,
            blur_width: 0.0,
            fill_texture_id: tex.id(),
            uv: egui::Rect::from_min_max(uv(rect.min), uv(rect.max)),
        }))
    }

    /// Forget the snapshot: the window closed, and its next open is over a
    /// different screen.
    pub fn clear(&mut self) {
        self.state = State::Fresh;
    }
}

/// Shrink the frame to [`TEXELS`] on its long side, then box-blur it.
fn blur(src: &egui::ColorImage) -> egui::ColorImage {
    let [w, h] = src.size;
    if w == 0 || h == 0 {
        return egui::ColorImage::new([1, 1], egui::Color32::BLACK);
    }
    let f = (w.max(h) / TEXELS).max(1);
    let (sw, sh) = ((w / f).max(1), (h / f).max(1));
    let n = (f * f) as f32;
    let mut px = vec![[0f32; 3]; sw * sh];
    for y in 0..sh {
        for x in 0..sw {
            let mut acc = [0f32; 3];
            for dy in 0..f {
                let row = (y * f + dy) * w + x * f;
                for c in &src.pixels[row..row + f] {
                    acc[0] += c.r() as f32;
                    acc[1] += c.g() as f32;
                    acc[2] += c.b() as f32;
                }
            }
            px[y * sw + x] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
    let mut tmp = px.clone();
    for _ in 0..PASSES {
        box_pass(&px, &mut tmp, sw, sh, true);
        box_pass(&tmp, &mut px, sw, sh, false);
    }
    egui::ColorImage {
        size: [sw, sh],
        pixels: px
            .iter()
            .map(|c| egui::Color32::from_rgb(c[0] as u8, c[1] as u8, c[2] as u8))
            .collect(),
    }
}

/// One box-blur pass of `src` into `dst`, along rows when `horizontal`, else
/// columns, with edges clamped.
fn box_pass(src: &[[f32; 3]], dst: &mut [[f32; 3]], w: usize, h: usize, horizontal: bool) {
    let (len, lines) = if horizontal { (w, h) } else { (h, w) };
    let at = |line: usize, i: usize| if horizontal { line * w + i } else { i * w + line };
    for line in 0..lines {
        for i in 0..len {
            let lo = i.saturating_sub(RADIUS);
            let hi = (i + RADIUS).min(len - 1);
            let mut acc = [0f32; 3];
            for j in lo..=hi {
                let c = src[at(line, j)];
                acc[0] += c[0];
                acc[1] += c[1];
                acc[2] += c[2];
            }
            let n = (hi - lo + 1) as f32;
            dst[at(line, i)] = [acc[0] / n, acc[1] / n, acc[2] / n];
        }
    }
}
