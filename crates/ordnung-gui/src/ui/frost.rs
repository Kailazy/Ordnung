//! Frosted backdrop for a floating window.
//!
//! egui paints every layer straight to one surface, so a see-through window
//! can only tint what's under it: at any alpha that lets the app show, it shows
//! in outline. What a frosted surface wants is the app *blurred*, and there is
//! no backdrop blur to ask the GPU for. So the frost is made the long way round:
//! when the window opens, the frame is snapshotted before the window is drawn,
//! shrunk and box-blurred on the CPU, and painted under the window from then on,
//! mapped so the window sees the part of the snapshot it sits over. The window
//! is held back for the one frame the snapshot takes.
//!
//! The snapshot is taken once, at open. Whatever moves under the window after
//! that is stale in it, but at this blur nothing under the window has a shape
//! to be stale in, and taking it again would only capture the window itself.

use eframe::egui;
use std::time::{Duration, Instant};

/// How long to hold the window back for a snapshot that isn't coming (a
/// backend without screenshots) before drawing it plain.
const GIVE_UP: Duration = Duration::from_millis(300);

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
    /// The snapshot is in flight since then.
    Asked(Instant),
    /// The blurred snapshot, ready to paint.
    Have(egui::TextureHandle),
    /// No snapshot came; the window goes on plain.
    Bare,
}

/// A window's frosted backdrop. Keep one per window across its open spell and
/// [`Frost::clear`] it when the window closes, so the next open takes a fresh
/// snapshot.
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

    /// Call once a frame before drawing the window. `false` means the window
    /// must sit this frame out: the screen under it is being snapshotted, and
    /// drawing it now would put it in its own backdrop.
    pub fn ready(&mut self, ctx: &egui::Context) -> bool {
        match &self.state {
            State::Have(_) | State::Bare => true,
            State::Fresh => {
                self.screen = ctx.screen_rect();
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot);
                self.state = State::Asked(Instant::now());
                ctx.request_repaint();
                false
            }
            State::Asked(since) => {
                let shot = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(image.clone()),
                        _ => None,
                    })
                });
                if let Some(image) = shot {
                    let tex =
                        ctx.load_texture("frost", blur(&image), egui::TextureOptions::LINEAR);
                    self.state = State::Have(tex);
                    return true;
                }
                if since.elapsed() > GIVE_UP {
                    self.state = State::Bare;
                    return true;
                }
                ctx.request_repaint();
                false
            }
        }
    }

    /// Paint the frost under a window occupying `rect`. It goes on the
    /// background order, above every panel and below every window, so it
    /// needn't be painted before the window itself.
    pub fn paint(&self, ctx: &egui::Context, id: egui::Id, rect: egui::Rect, rounding: egui::Rounding) {
        let State::Have(tex) = &self.state else {
            return;
        };
        let s = self.screen;
        if s.width() <= 0.0 || s.height() <= 0.0 {
            return;
        }
        let uv = |p: egui::Pos2| {
            egui::pos2((p.x - s.min.x) / s.width(), (p.y - s.min.y) / s.height())
        };
        ctx.layer_painter(egui::LayerId::new(egui::Order::Background, id))
            .add(egui::epaint::RectShape {
                rect,
                rounding,
                fill: egui::Color32::WHITE,
                stroke: egui::Stroke::NONE,
                blur_width: 0.0,
                fill_texture_id: tex.id(),
                uv: egui::Rect::from_min_max(uv(rect.min), uv(rect.max)),
            });
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
