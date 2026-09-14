//! The record map: every record you've crossed paths with, drawn as one living
//! web. Artists are the hubs; their releases hang off them, and releases an
//! artist put out on the same label gather under a small label node between
//! the two. The collection and the wantlist are on it as they stand, and so
//! is every record a dig has landed on — a dug record joins the map the moment
//! it's dug to, and reads as wanted the moment it's asked for on a list,
//! before Discogs has answered.
//!
//! The layout is a force simulation rather than a fixed drawing: springs hold
//! each record to its hub, everything repels everything nearby, and hubs are
//! drawn loosely to the middle. The springs are deliberately underdamped, so
//! a record that lands, or a hub that gets flung, overshoots and settles the
//! way something on a string does rather than sliding into a slot. The
//! simulation sleeps once it's still, so an idle map costs nothing.
//!
//! Zoom is bounded on the wide end at the scale that shows the whole map, so
//! however far out you go, nothing is ever off the edge. Split out of
//! `views.rs`; part of the GUI `App`.
use super::*;
use ordnung_core::model::DugRelease;
use std::collections::HashMap;
use std::time::Instant;

/// How a record stands with you, which decides its ring.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    /// In the collection.
    Owned,
    /// On the wantlist, or asked for on it from a dig.
    Wanted,
    /// Only ever dug to.
    Dug,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Artist,
    /// A label an artist has two or more records on: the sub-group between
    /// that artist and those records.
    Label,
    Release,
}

/// Where a release node's cover comes from: the local cache for shelf
/// records, the Discogs CDN for dug ones.
#[derive(Clone, PartialEq)]
enum Cover {
    None,
    Shelf(VinylCoverKey),
    Url(String),
}

/// What a release node knows, snapshotted so the draw never borrows the lists.
#[derive(Clone)]
pub(crate) struct Release {
    pub release_id: u64,
    pub artist: String,
    pub title: String,
    pub sub: String,
    pub label: Option<String>,
    pub status: Status,
    cover: Cover,
    /// The shelf key when the record is on a shelf, so a click opens the
    /// same sheet the grid would.
    pub key: Option<VinylCoverKey>,
}

impl Release {
    /// The cover's URL, for a record with no local cache entry to key.
    pub fn cover_url(&self) -> Option<String> {
        match &self.cover {
            Cover::Url(u) => Some(u.clone()),
            _ => None,
        }
    }
}

struct Node {
    kind: Kind,
    key: String,
    /// Artist or label name; a release's title.
    name: String,
    pos: egui::Vec2,
    vel: egui::Vec2,
    /// World-space radius (half side for a release cover).
    r: f32,
    /// Display scale, driven by its own little spring: 0 at birth, 1 at rest,
    /// a touch over while hovered. Separate from the layout so a pop-in or a
    /// hover never disturbs the neighbours.
    scale: f32,
    scale_v: f32,
    /// Which hub this hangs off (an artist or label node), by key so the
    /// index survives compaction.
    hub_key: Option<String>,
    hub: Option<usize>,
    release: Option<Release>,
    /// Releases under a hub, counted through its label sub-groups.
    weight: usize,
    /// Set during a sync for every node the sources still name.
    alive: bool,
    /// Lowercased search text.
    hay: String,
    /// Pinned to the pointer while dragged.
    held: bool,
}

struct Edge {
    a: usize,
    b: usize,
    rest: f32,
    k: f32,
    /// A same-label tie between two artists' label groups: weak, and drawn
    /// as a faint dash rather than a line.
    soft: bool,
}

/// What the user did to the map this frame, applied by the caller once the
/// state is back on `self`.
pub(crate) enum GraphAct {
    Open(Release),
}

/// The simulation, its camera and the interaction state. Lives on `App` and
/// is rebuilt from the record lists whenever they change.
pub(crate) struct GraphState {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
    /// Signature of the sources the nodes were built from; a sync is skipped
    /// while it matches.
    sig: u64,
    /// World point at the middle of the canvas, and the scale, as drawn now
    /// and as they're heading. Input moves the goal; the drawn camera eases
    /// after it.
    cam: egui::Vec2,
    zoom: f32,
    cam_goal: egui::Vec2,
    zoom_goal: f32,
    /// False until the camera has been fitted to the map once.
    fitted: bool,
    /// While set, the camera keeps re-fitting to the whole map every frame,
    /// so a map still settling (or growing under a dig) stays in view. Any
    /// zoom or pan by hand takes over; Fit hands it back.
    follow_fit: bool,
    /// The node under the pointer, if any.
    hover: Option<usize>,
    /// The node being dragged, or `None` while a drag pans the canvas.
    drag: Option<usize>,
    /// When the last simulation tick ran; `None` while asleep.
    last_tick: Option<Instant>,
    seed: u64,
    /// Largest speed after the last tick — the sleep test.
    energy: f32,
}

impl Default for GraphState {
    fn default() -> Self {
        Self {
            nodes: Vec::new(),
            edges: Vec::new(),
            index: HashMap::new(),
            sig: 0,
            cam: egui::Vec2::ZERO,
            zoom: 1.0,
            cam_goal: egui::Vec2::ZERO,
            zoom_goal: 1.0,
            fitted: false,
            follow_fit: true,
            hover: None,
            drag: None,
            last_tick: None,
            seed: 0x9E37_79B9_7F4A_7C15,
            energy: 0.0,
        }
    }
}

// --- Physics constants (world units) ----------------------------------------

/// Half side of a release cover.
const REL_R: f32 = 20.0;
/// A label sub-group's ring.
const LABEL_R: f32 = 9.0;
/// Spring stiffness of a hub link. Stiff, with the light damping below, is
/// what makes a landing overshoot and settle rather than slide.
const SPRING_K: f32 = 34.0;
/// Velocity kept per 60 Hz frame. Higher is bouncier; 1.0 would never settle.
const DAMPING: f32 = 0.955;
/// Range and strength of the repulsion between any two nodes.
const REPEL_RANGE: f32 = 260.0;
const REPEL_K: f32 = 7000.0;
/// Pull toward the middle, keeping the map one body. A constant tug rather
/// than a spring, so the map settles as an even disc instead of a dense
/// core with a thin rim.
const GRAVITY_HUB: f32 = 70.0;
const GRAVITY_LEAF: f32 = 4.0;
/// A small spring-like share on top of the constant tug, so the rim of a
/// large map meets a firm edge rather than creeping outward for minutes.
const GRAVITY_SLOPE: f32 = 0.05;
const MAX_SPEED: f32 = 1400.0;
/// Below this top speed the simulation goes to sleep.
const SLEEP_SPEED: f32 = 6.0;
/// Top speed under which the map is "settling": damping firms up so the last
/// low-amplitude jitter dies instead of ringing on. Big motion (a landing,
/// a fling) stays bouncy.
const SETTLE_SPEED: f32 = 90.0;
const SETTLE_DAMPING: f32 = 0.90;
/// Closest the camera can get.
const MAX_ZOOM: f32 = 3.0;
/// Canvas padding around the map when fitted.
const FIT_PAD: f32 = 36.0;

fn fold(s: &str) -> String {
    s.trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// A cheap stable hash for the source signature.
fn mix(h: &mut u64, v: u64) {
    *h ^= v;
    *h = h.wrapping_mul(0x100_0000_01b3);
}
fn mix_str(h: &mut u64, s: &str) {
    for b in s.bytes() {
        mix(h, b as u64);
    }
    mix(h, 0xff);
}

/// xorshift, for spawn offsets. Deterministic per state so a rebuilt map
/// lands the same way twice.
fn next_rand(seed: &mut u64) -> f32 {
    let mut x = *seed;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *seed = x;
    (x >> 11) as f32 / (1u64 << 53) as f32
}

fn artist_radius(weight: usize) -> f32 {
    (13.0 + 4.2 * (weight as f32).sqrt()).min(44.0)
}

impl GraphState {
    /// Rebuild the nodes from the three sources when any of them changed.
    /// Existing nodes keep their position and motion; new ones are born at
    /// their hub and spring out from it; nodes no source names any more go.
    fn sync(&mut self, owned: &[VinylRecord], wanted: &[VinylRecord], dug: &[DugRelease]) {
        // Merge by release: a record on a shelf is that shelf's, whatever a
        // dig also knows about it; a dug record asked for on a list reads as
        // wanted straight away.
        let mut releases: Vec<Release> = Vec::with_capacity(owned.len() + wanted.len() + dug.len());
        let mut seen: HashMap<u64, usize> = HashMap::new();
        let mut push = |rel: Release, seen: &mut HashMap<u64, usize>| {
            if seen.contains_key(&rel.release_id) {
                return;
            }
            seen.insert(rel.release_id, releases.len());
            releases.push(rel);
        };
        for (list, recs, status) in [
            (VinylList::Collection, owned, Status::Owned),
            (VinylList::Wantlist, wanted, Status::Wanted),
        ] {
            for v in recs {
                let key = (list, v.instance_id);
                let cover = if v.has_cover {
                    Cover::Shelf(key)
                } else if let Some(u) = v.thumb_url.as_deref().filter(|u| !u.trim().is_empty()) {
                    Cover::Url(u.to_string())
                } else {
                    Cover::None
                };
                push(
                    Release {
                        release_id: v.release_id,
                        artist: v.artist.clone(),
                        title: v.title.clone(),
                        sub: crate::views::vinyl_sub(v),
                        label: v.label.clone().filter(|l| !l.trim().is_empty()),
                        status,
                        cover,
                        key: Some(key),
                    },
                    &mut seen,
                );
            }
        }
        for d in dug {
            push(
                Release {
                    release_id: d.release_id,
                    artist: d.artist.clone(),
                    title: d.title.clone(),
                    sub: d.sub.clone(),
                    label: d.label.clone().filter(|l| !l.trim().is_empty()),
                    status: if d.wanted { Status::Wanted } else { Status::Dug },
                    cover: match d.thumb_url.as_deref().filter(|u| !u.trim().is_empty()) {
                        Some(u) => Cover::Url(u.to_string()),
                        None => Cover::None,
                    },
                    key: None,
                },
                &mut seen,
            );
        }

        let mut sig = 0xcbf2_9ce4_8422_2325u64;
        for r in &releases {
            mix(&mut sig, r.release_id);
            mix(&mut sig, r.status as u64);
            mix_str(&mut sig, r.label.as_deref().unwrap_or(""));
            match &r.cover {
                Cover::None => mix(&mut sig, 0),
                Cover::Shelf((l, id)) => {
                    mix(&mut sig, 1 + *l as u64);
                    mix(&mut sig, *id);
                }
                Cover::Url(u) => mix_str(&mut sig, u),
            }
        }
        if sig == self.sig {
            return;
        }
        self.sig = sig;

        for n in &mut self.nodes {
            n.alive = false;
            n.weight = 0;
        }

        // Which (artist, label) pairs earn a sub-group: two or more records.
        let mut per_label: HashMap<(String, String), usize> = HashMap::new();
        let mut artist_keys: Vec<String> = Vec::with_capacity(releases.len());
        for r in &releases {
            let ak = format!("a:{}", fold(crate::dig::strip_disambiguator(&r.artist)));
            if let Some(l) = &r.label {
                *per_label.entry((ak.clone(), fold(l))).or_default() += 1;
            }
            artist_keys.push(ak);
        }

        let n_before = self.nodes.len();
        for (r, ak) in releases.into_iter().zip(artist_keys) {
            let artist_name = crate::dig::strip_disambiguator(&r.artist).trim().to_string();
            let artist_name = if artist_name.is_empty() {
                "Unknown artist".to_string()
            } else {
                artist_name
            };
            let ai = self.ensure(Kind::Artist, &ak, &artist_name, None);
            self.nodes[ai].weight += 1;
            let hub_key = match &r.label {
                Some(l) if per_label.get(&(ak.clone(), fold(l))).copied().unwrap_or(0) >= 2 => {
                    let lk = format!("l:{ak}|{}", fold(l));
                    let li = self.ensure(Kind::Label, &lk, l.trim(), Some(ak.clone()));
                    self.nodes[li].weight += 1;
                    lk
                }
                _ => ak.clone(),
            };
            let rk = format!("r:{}", r.release_id);
            let ri = self.ensure(Kind::Release, &rk, &r.title, Some(hub_key));
            let node = &mut self.nodes[ri];
            node.hay = format!(
                "{} {} {} {}",
                r.artist,
                r.title,
                r.sub,
                r.label.as_deref().unwrap_or("")
            )
            .to_lowercase();
            node.name = if r.title.trim().is_empty() {
                "Untitled".to_string()
            } else {
                r.title.trim().to_string()
            };
            node.release = Some(r);
        }
        let born = self.nodes.len() - n_before;

        // Drop what no source names any more, then re-point hubs by key.
        if self.nodes.iter().any(|n| !n.alive) {
            self.nodes.retain(|n| n.alive);
        }
        self.index = self
            .nodes
            .iter()
            .enumerate()
            .map(|(i, n)| (n.key.clone(), i))
            .collect();
        for i in 0..self.nodes.len() {
            let hub = self.nodes[i]
                .hub_key
                .as_ref()
                .and_then(|k| self.index.get(k).copied());
            self.nodes[i].hub = hub;
            self.nodes[i].r = match self.nodes[i].kind {
                Kind::Artist => artist_radius(self.nodes[i].weight),
                Kind::Label => LABEL_R + 1.5 * (self.nodes[i].weight as f32).sqrt(),
                Kind::Release => REL_R,
            };
            self.nodes[i].hay = match self.nodes[i].kind {
                Kind::Release => std::mem::take(&mut self.nodes[i].hay),
                _ => self.nodes[i].name.to_lowercase(),
            };
        }

        // Newborns start on their hub (or, for a hub with no home yet, on a
        // sunflower spiral so a whole fresh map opens evenly) and let the
        // springs carry them out.
        let mut spiral = 0usize;
        for i in 0..self.nodes.len() {
            if self.nodes[i].alive && self.nodes[i].scale > 0.0 {
                continue;
            }
            let hub_pos = self.nodes[i].hub.map(|h| self.nodes[h].pos);
            let a = next_rand(&mut self.seed) * std::f32::consts::TAU;
            let jitter = egui::vec2(a.cos(), a.sin());
            self.nodes[i].pos = match hub_pos {
                Some(p) => p + jitter * (self.nodes[i].r + 6.0),
                None => {
                    // Spread new hubs among the ones already there rather
                    // than piling them on the origin.
                    let k = (self.nodes.len() + spiral) as f32;
                    spiral += 1;
                    let r = 30.0 * k.sqrt();
                    let t = k * 2.399_963;
                    egui::vec2(r * t.cos(), r * t.sin()) + jitter * 8.0
                }
            };
            self.nodes[i].vel = egui::Vec2::ZERO;
        }

        // Links: every node to its hub, and a faint tie between two artists'
        // groups on the same label so the label reads across the map.
        self.edges.clear();
        for i in 0..self.nodes.len() {
            let Some(h) = self.nodes[i].hub else { continue };
            let (rest, k) = match (self.nodes[i].kind, self.nodes[h].kind) {
                (Kind::Release, Kind::Artist) => {
                    let fan = (self.nodes[h].weight as f32).sqrt() * 9.0;
                    (self.nodes[h].r + REL_R + 12.0 + fan.min(60.0), SPRING_K)
                }
                (Kind::Release, Kind::Label) => {
                    let fan = (self.nodes[h].weight as f32).sqrt() * 8.0;
                    (self.nodes[h].r + REL_R + 18.0 + fan.min(40.0), SPRING_K)
                }
                (Kind::Label, _) => (self.nodes[h].r + 64.0, SPRING_K * 0.8),
                _ => (80.0, SPRING_K),
            };
            self.edges.push(Edge {
                a: i,
                b: h,
                rest,
                k,
                soft: false,
            });
        }
        let mut by_label: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.kind == Kind::Label {
                by_label.entry(fold(&n.name)).or_default().push(i);
            }
        }
        for (_, group) in by_label {
            for w in group.windows(2) {
                self.edges.push(Edge {
                    a: w[0],
                    b: w[1],
                    rest: 260.0,
                    k: 1.2,
                    soft: true,
                });
            }
        }
        // A map built from nothing is settled off screen first, so it opens
        // in its shape with every record blooming into place, rather than
        // spending its first half minute drifting apart. Records that land
        // later spring out from their hub in full view.
        if n_before == 0 && !self.nodes.is_empty() {
            for _ in 0..160 {
                self.tick(1.0 / 30.0);
            }
        }
        if born > 0 || self.nodes.len() != n_before {
            self.wake();
        }
    }

    /// Find or create a node by key, marking it alive.
    fn ensure(&mut self, kind: Kind, key: &str, name: &str, hub_key: Option<String>) -> usize {
        if let Some(&i) = self.index.get(key) {
            let n = &mut self.nodes[i];
            n.alive = true;
            n.name = name.to_string();
            n.hub_key = hub_key;
            return i;
        }
        let i = self.nodes.len();
        self.nodes.push(Node {
            kind,
            key: key.to_string(),
            name: name.to_string(),
            pos: egui::Vec2::ZERO,
            vel: egui::Vec2::ZERO,
            r: match kind {
                Kind::Artist => artist_radius(1),
                Kind::Label => LABEL_R,
                Kind::Release => REL_R,
            },
            scale: 0.0,
            scale_v: 0.0,
            hub_key,
            hub: None,
            release: None,
            weight: 0,
            alive: true,
            hay: String::new(),
            held: false,
        });
        self.index.insert(key.to_string(), i);
        i
    }

    fn wake(&mut self) {
        if self.last_tick.is_none() {
            self.last_tick = Some(Instant::now());
        }
        self.energy = f32::MAX;
    }

    /// One simulation step. Returns whether anything is still moving.
    fn tick(&mut self, dt: f32) -> bool {
        let n = self.nodes.len();
        if n == 0 {
            return false;
        }
        let dt = dt.clamp(1.0 / 240.0, 1.0 / 30.0);
        let substeps = 2;
        let h = dt / substeps as f32;
        let mut forces = vec![egui::Vec2::ZERO; n];
        let mut top_speed = 0.0f32;
        for _ in 0..substeps {
            forces.fill(egui::Vec2::ZERO);
            // Springs.
            for e in &self.edges {
                let d = self.nodes[e.b].pos - self.nodes[e.a].pos;
                let len = d.length().max(0.01);
                let f = d / len * (e.k * (len - e.rest));
                forces[e.a] += f;
                forces[e.b] -= f;
            }
            // Repulsion over a uniform grid: only pairs within range meet.
            let cell = REPEL_RANGE;
            let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
            for (i, node) in self.nodes.iter().enumerate() {
                let c = ((node.pos.x / cell).floor() as i32, (node.pos.y / cell).floor() as i32);
                grid.entry(c).or_default().push(i);
            }
            let range2 = REPEL_RANGE * REPEL_RANGE;
            for (&(cx, cy), bucket) in &grid {
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        let other = match grid.get(&(cx + dx, cy + dy)) {
                            Some(o) => o,
                            None => continue,
                        };
                        // Each unordered pair once: same cell by index order,
                        // different cells by cell order.
                        let same = dx == 0 && dy == 0;
                        if !same && (dx, dy) < (0, 0) {
                            continue;
                        }
                        for &i in bucket {
                            for &j in other {
                                if same && j <= i {
                                    continue;
                                }
                                let d = self.nodes[j].pos - self.nodes[i].pos;
                                let d2 = d.length_sq();
                                if d2 >= range2 {
                                    continue;
                                }
                                let dist = d2.sqrt().max(0.5);
                                let dir = d / dist;
                                let (ri, rj) = (self.nodes[i].r, self.nodes[j].r);
                                let falloff = 1.0 - dist / REPEL_RANGE;
                                let mut f = REPEL_K * (ri * rj) / (d2 + 400.0) * falloff;
                                // Bodies never sit on top of each other.
                                let touch = ri + rj + 8.0;
                                if dist < touch {
                                    f += (touch - dist) * 28.0;
                                }
                                forces[i] -= dir * f;
                                forces[j] += dir * f;
                            }
                        }
                    }
                }
            }
            // Gravity to the middle, and the integration.
            let settling = self.energy < SETTLE_SPEED;
            top_speed = 0.0;
            let damp = if settling { SETTLE_DAMPING } else { DAMPING }.powf(h * 60.0);
            for (i, node) in self.nodes.iter_mut().enumerate() {
                if node.held {
                    continue;
                }
                let g = match node.kind {
                    Kind::Artist => GRAVITY_HUB,
                    Kind::Label => GRAVITY_HUB * 0.35,
                    Kind::Release => GRAVITY_LEAF,
                };
                let len = node.pos.length();
                let pull = if len > 1e-3 {
                    node.pos / len * (g * (len / 40.0).min(1.0) + GRAVITY_SLOPE * len)
                } else {
                    egui::Vec2::ZERO
                };
                let f = forces[i] - pull;
                let mass = (node.r / 14.0).max(1.0);
                node.vel = (node.vel + f / mass * h) * damp;
                let speed = node.vel.length();
                if speed > MAX_SPEED {
                    node.vel *= MAX_SPEED / speed;
                }
                node.pos += node.vel * h;
                top_speed = top_speed.max(speed);
            }
        }
        self.energy = top_speed;
        if top_speed <= SLEEP_SPEED {
            // Still enough: stop outright, so nothing creeps while asleep.
            for n in &mut self.nodes {
                n.vel = egui::Vec2::ZERO;
            }
            return false;
        }
        true
    }

    /// World-space bounds of every node, padded by its radius.
    fn bounds(&self) -> Option<egui::Rect> {
        let mut r: Option<egui::Rect> = None;
        for n in &self.nodes {
            let c = egui::pos2(n.pos.x, n.pos.y);
            let b = egui::Rect::from_center_size(c, egui::Vec2::splat(n.r * 2.0 + 24.0));
            r = Some(match r {
                Some(x) => x.union(b),
                None => b,
            });
        }
        r
    }

    fn fit_zoom(&self, canvas: egui::Rect) -> Option<(egui::Vec2, f32)> {
        let b = self.bounds()?;
        let w = (canvas.width() - FIT_PAD * 2.0).max(40.0);
        let h = (canvas.height() - FIT_PAD * 2.0).max(40.0);
        let z = (w / b.width().max(1.0)).min(h / b.height().max(1.0));
        Some((b.center().to_vec2(), z.clamp(0.02, MAX_ZOOM)))
    }

    /// Aim the camera at the whole map.
    pub(crate) fn fit(&mut self, canvas: egui::Rect) {
        self.follow_fit = true;
        if let Some((c, z)) = self.fit_zoom(canvas) {
            self.cam_goal = c;
            self.zoom_goal = z;
        }
    }

}

impl App {
    /// Draw the map into `rect`. Returns what the user asked of it.
    pub(crate) fn draw_graph(
        &mut self,
        ui: &mut egui::Ui,
        rect: egui::Rect,
        query: &str,
    ) -> Option<GraphAct> {
        use crate::ui::tokens::{color, font};
        let mut g = std::mem::take(&mut self.graph);
        g.sync(&self.vinyl, &self.wantlist, &self.dug);

        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        let mut act = None;

        if g.nodes.is_empty() {
            painter.text(
                rect.center() - egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                "Nothing on the map yet",
                font::headline(),
                color::LABEL_2,
            );
            painter.text(
                rect.center() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_CENTER,
                "Sync your Discogs shelves, or start a dig from any record",
                font::footnote(),
                color::LABEL_3,
            );
            self.graph = g;
            return None;
        }

        // Frame time for the simulation and the camera.
        let dt = g
            .last_tick
            .map(|t| now.duration_since(t).as_secs_f32())
            .unwrap_or(1.0 / 60.0);

        // --- Camera ---------------------------------------------------------
        let fit = g.fit_zoom(rect);
        if !g.fitted {
            if let Some((c, z)) = fit {
                g.cam = c;
                g.zoom = z;
                g.cam_goal = c;
                g.zoom_goal = z;
                g.fitted = true;
            }
        }
        if g.follow_fit {
            if let Some((c, z)) = fit {
                g.cam_goal = c;
                g.zoom_goal = z;
            }
        }
        let min_zoom = fit.map(|(_, z)| z).unwrap_or(0.05);
        let center = rect.center();
        let to_screen = |cam: egui::Vec2, zoom: f32, p: egui::Vec2| -> egui::Pos2 {
            center + (p - cam) * zoom
        };
        let to_world = |cam: egui::Vec2, zoom: f32, s: egui::Pos2| -> egui::Vec2 {
            cam + (s - center) / zoom
        };

        let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
        if resp.hovered() || resp.dragged() {
            // Pinch (or ctrl+scroll) zooms about the pointer; a plain scroll
            // pans, the way a trackpad moves a map.
            let (zd, scroll) = ui.input(|i| (i.zoom_delta(), i.smooth_scroll_delta));
            if (zd - 1.0).abs() > 1e-4 {
                if let Some(p) = pointer {
                    let anchor = to_world(g.cam_goal, g.zoom_goal, p);
                    let z = (g.zoom_goal * zd).clamp(min_zoom, MAX_ZOOM.max(min_zoom));
                    g.zoom_goal = z;
                    g.cam_goal = anchor - (p - center) / z;
                    g.follow_fit = false;
                }
            }
            if scroll != egui::Vec2::ZERO {
                g.cam_goal -= scroll / g.zoom_goal;
                g.follow_fit = false;
            }
        }

        // --- Hit test -------------------------------------------------------
        let hit = |g: &GraphState, p: egui::Pos2| -> Option<usize> {
            let mut best: Option<(usize, f32)> = None;
            for (i, n) in g.nodes.iter().enumerate() {
                let s = to_screen(g.cam, g.zoom, n.pos);
                let r = (n.r * n.scale.max(0.2) * g.zoom).max(5.0) + 2.0;
                let d = (p - s).length();
                if d <= r && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            best.map(|(i, _)| i)
        };
        let hovered_now = if resp.hovered() && g.drag.is_none() && !resp.dragged() {
            pointer.and_then(|p| hit(&g, p))
        } else {
            None
        };
        if hovered_now != g.hover {
            g.hover = hovered_now;
            g.wake();
        }

        // --- Drag: a node follows the pointer and flies on release; empty
        // canvas pans. -----------------------------------------------------------
        if resp.drag_started() {
            g.drag = pointer.and_then(|p| hit(&g, p));
            if let Some(i) = g.drag {
                g.nodes[i].held = true;
            }
            g.wake();
        }
        if resp.dragged() {
            let delta = resp.drag_delta();
            match g.drag {
                Some(i) => {
                    if let Some(p) = pointer {
                        let w = to_world(g.cam, g.zoom, p);
                        // Keep the fling velocity from the pointer's motion.
                        let v = delta / g.zoom / dt.max(1e-3);
                        let n = &mut g.nodes[i];
                        n.pos = w;
                        n.vel = n.vel * 0.5 + v * 0.5;
                    }
                    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                None => {
                    g.cam_goal -= delta / g.zoom;
                    g.cam -= delta / g.zoom;
                    g.follow_fit = false;
                    ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
                }
            }
            g.wake();
        }
        if resp.drag_stopped() {
            if let Some(i) = g.drag.take() {
                g.nodes[i].held = false;
                let speed = g.nodes[i].vel.length();
                if speed > MAX_SPEED {
                    g.nodes[i].vel *= MAX_SPEED / speed;
                }
            }
            g.wake();
        }

        // --- Clicks ---------------------------------------------------------
        if resp.double_clicked() {
            match pointer.and_then(|p| hit(&g, p)) {
                Some(i) => {
                    // Lean in on a hub; on a record, open it (the sheet is
                    // what a single click does too, so this just re-sends).
                    let target = g.nodes[i].hub.filter(|_| g.nodes[i].kind == Kind::Release).unwrap_or(i);
                    g.cam_goal = g.nodes[target].pos;
                    g.zoom_goal = 1.25f32.clamp(min_zoom, MAX_ZOOM);
                    g.follow_fit = false;
                }
                None => g.fit(rect),
            }
        } else if resp.clicked() {
            if let Some(i) = pointer.and_then(|p| hit(&g, p)) {
                match g.nodes[i].kind {
                    Kind::Release => {
                        if let Some(r) = &g.nodes[i].release {
                            act = Some(GraphAct::Open(r.clone()));
                        }
                    }
                    _ => {
                        g.cam_goal = g.nodes[i].pos;
                        g.zoom_goal = g.zoom_goal.max(1.0).clamp(min_zoom, MAX_ZOOM);
                        g.follow_fit = false;
                    }
                }
            }
        }

        // Camera easing, then keep the map on screen: the goal can't zoom
        // wider than the fit, or wander past the map's edge.
        g.zoom_goal = g.zoom_goal.clamp(min_zoom, MAX_ZOOM.max(min_zoom));
        if let Some(b) = g.bounds() {
            let half = rect.size() / g.zoom_goal * 0.5;
            let slack = |lo: f32, hi: f32, h: f32| -> (f32, f32) {
                // The whole map fits on screen: pin it to the middle instead
                // of letting it slide off.
                if hi - lo <= h * 2.0 {
                    let c = (lo + hi) * 0.5;
                    (c, c)
                } else {
                    (lo + h, hi - h)
                }
            };
            let (x0, x1) = slack(b.left(), b.right(), half.x);
            let (y0, y1) = slack(b.top(), b.bottom(), half.y);
            g.cam_goal.x = g.cam_goal.x.clamp(x0, x1);
            g.cam_goal.y = g.cam_goal.y.clamp(y0, y1);
        }
        let ease = 1.0 - (-dt * 11.0).exp();
        let cam_moving = (g.cam_goal - g.cam).length() * g.zoom > 0.15
            || (g.zoom_goal - g.zoom).abs() > 1e-3;
        if cam_moving {
            g.cam += (g.cam_goal - g.cam) * ease;
            g.zoom += (g.zoom_goal - g.zoom) * ease;
        } else {
            g.cam = g.cam_goal;
            g.zoom = g.zoom_goal;
        }

        // --- Simulation, and the little display springs -------------------------
        let mut moving = g.tick(dt);
        let hover = g.hover;
        let drag = g.drag;
        // The display springs integrate explicitly, so a long debug frame
        // must not become a long step or they'd blow up.
        let sdt = dt.min(1.0 / 30.0);
        for (i, n) in g.nodes.iter_mut().enumerate() {
            let target = if Some(i) == hover || Some(i) == drag {
                1.18
            } else {
                1.0
            };
            let a = 260.0 * (target - n.scale) - 13.0 * n.scale_v;
            n.scale_v += a * sdt;
            n.scale = (n.scale + n.scale_v * sdt).clamp(0.0, 2.0);
            if (n.scale - target).abs() > 0.002 || n.scale_v.abs() > 0.02 {
                moving = true;
            } else {
                n.scale = target;
                n.scale_v = 0.0;
            }
        }
        g.last_tick = if moving || cam_moving { Some(now) } else { None };
        if moving || cam_moving {
            ctx.request_repaint();
        }

        // --- Paint ----------------------------------------------------------
        let zoom = g.zoom;
        let cam = g.cam;
        let query = query.trim().to_lowercase();
        let searching = !query.is_empty();
        let dim = |c: egui::Color32, on: bool| -> egui::Color32 {
            if searching && !on {
                c.gamma_multiply(0.22)
            } else {
                c
            }
        };
        let matches: Vec<bool> = g
            .nodes
            .iter()
            .map(|n| !searching || n.hay.contains(&query))
            .collect();
        let visible = |p: egui::Pos2, r: f32| -> bool { rect.expand(r + 40.0).contains(p) };

        // Links. A match lights the link to its hub too, so a hit's family
        // is legible under the dimming.
        let link_w = (1.0 * zoom.sqrt()).clamp(0.5, 1.6);
        for e in &g.edges {
            let (a, b) = (&g.nodes[e.a], &g.nodes[e.b]);
            let pa = to_screen(cam, zoom, a.pos);
            let pb = to_screen(cam, zoom, b.pos);
            if !visible(pa, 0.0) && !visible(pb, 0.0) {
                continue;
            }
            let on = matches[e.a] || matches[e.b];
            if e.soft {
                painter.add(egui::Shape::dashed_line(
                    &[pa, pb],
                    egui::Stroke::new(link_w * 0.8, dim(color::LABEL_4.gamma_multiply(0.7), on)),
                    4.0 * zoom.max(0.5),
                    6.0 * zoom.max(0.5),
                ));
            } else {
                let c = match a.kind {
                    Kind::Label => color::LABEL_4,
                    _ => color::SEPARATOR_OPAQUE,
                };
                painter.line_segment([pa, pb], egui::Stroke::new(link_w, dim(c, on)));
            }
        }

        // Hubs under, records over, the hovered node last so it sits on top.
        let mut order: Vec<usize> = (0..g.nodes.len()).collect();
        order.sort_by_key(|&i| {
            (
                match g.nodes[i].kind {
                    Kind::Label => 0,
                    Kind::Artist => 1,
                    Kind::Release => 2,
                },
                (Some(i) == g.hover || Some(i) == g.drag) as u8,
            )
        });
        let mut cover_asks: Vec<usize> = Vec::new();
        for &i in &order {
            let n = &g.nodes[i];
            let p = to_screen(cam, zoom, n.pos);
            let r = n.r * n.scale * zoom;
            if !visible(p, r) || n.scale <= 0.01 {
                continue;
            }
            let on = matches[i];
            let lit = Some(i) == g.hover || Some(i) == g.drag;
            match n.kind {
                Kind::Artist => {
                    painter.circle(
                        p,
                        r,
                        dim(if lit { color::SURFACE_ACTIVE } else { color::SURFACE_HI }, on),
                        egui::Stroke::new(
                            1.5f32.min(r * 0.2),
                            dim(if lit { color::LABEL } else { color::LABEL_3 }, on),
                        ),
                    );
                    // Names earn their place by size: a hub that's a dot on
                    // screen stays unlabelled until you lean in, so the
                    // whole-map view isn't a carpet of type.
                    if r >= 9.5 || lit {
                        let size = (13.0 * zoom.sqrt()).clamp(9.0, 16.0);
                        painter.text(
                            p + egui::vec2(0.0, r + 3.0),
                            egui::Align2::CENTER_TOP,
                            &n.name,
                            font::strong(size),
                            dim(color::LABEL, on),
                        );
                    }
                }
                Kind::Label => {
                    painter.circle_stroke(
                        p,
                        r,
                        egui::Stroke::new(1.2, dim(if lit { color::LABEL_2 } else { color::LABEL_4 }, on)),
                    );
                    painter.circle_filled(p, (r * 0.28).max(1.5), dim(color::LABEL_3, on));
                    if r >= 6.0 && zoom >= 0.45 {
                        let size = (11.0 * zoom.sqrt()).clamp(9.0, 13.0);
                        painter.text(
                            p + egui::vec2(0.0, r + 2.0),
                            egui::Align2::CENTER_TOP,
                            &n.name,
                            egui::FontId::proportional(size),
                            dim(color::LABEL_3, on),
                        );
                    }
                }
                Kind::Release => {
                    if r >= 4.0 {
                        cover_asks.push(i);
                    }
                }
            }
        }

        // Covers need `self` (the caches), so they're a second pass. URL
        // covers are network fetches into a capped cache, so a frame asks for
        // a bounded number of them, and only at a size worth fetching for:
        // more than the cap would evict and refetch every frame.
        let mut url_asks = 0usize;
        for i in cover_asks {
            let n = &g.nodes[i];
            let Some(rel) = &n.release else { continue };
            let p = to_screen(cam, zoom, n.pos);
            let r = n.r * n.scale * zoom;
            let on = matches[i];
            let lit = Some(i) == g.hover || Some(i) == g.drag;
            let sq = egui::Rect::from_center_size(p, egui::Vec2::splat(r * 2.0));
            let rounding = (r * 0.18).clamp(2.0, 6.0);
            let tex = match &rel.cover {
                Cover::Shelf(key) => {
                    self.request_vinyl_cover(*key);
                    match self.vinyl_covers.get(key) {
                        Some(ThumbState::Ready(Some(t))) => Some(t.id()),
                        _ => None,
                    }
                }
                Cover::Url(u) => {
                    let known = matches!(self.dig_covers.get(u), Some(ThumbState::Ready(_)));
                    if known || (r >= 6.0 && url_asks < 400) {
                        if !known {
                            url_asks += 1;
                        }
                        self.dig_cover(u).map(|t| t.id())
                    } else {
                        None
                    }
                }
                Cover::None => None,
            };
            let tint = dim(egui::Color32::WHITE, on);
            match tex {
                Some(id) => {
                    painter.add(egui::Shape::Rect(egui::epaint::RectShape {
                        rect: sq,
                        rounding: rounding.into(),
                        fill: tint,
                        stroke: egui::Stroke::NONE,
                        blur_width: 0.0,
                        fill_texture_id: id,
                        uv: egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    }));
                }
                None => {
                    painter.rect_filled(sq, rounding, dim(color::SURFACE_HI, on));
                    // A blank sleeve: the disc peeking out.
                    if r >= 8.0 {
                        painter.circle_stroke(
                            p,
                            r * 0.42,
                            egui::Stroke::new(1.0, dim(color::LABEL_4, on)),
                        );
                        painter.circle_filled(p, r * 0.08, dim(color::LABEL_4, on));
                    }
                }
            }
            let ring = match rel.status {
                Status::Owned => (1.0, color::SEPARATOR_OPAQUE),
                Status::Wanted => (2.0, color::ACCENT),
                Status::Dug => (2.0, color::ORANGE),
            };
            let (w, c) = ring;
            let w = if lit { w + 0.8 } else { w };
            let c = if lit && rel.status == Status::Owned {
                color::LABEL_2
            } else {
                c
            };
            painter.rect_stroke(sq, rounding, egui::Stroke::new(w, dim(c, on)));
            if searching && on {
                painter.rect_stroke(
                    sq.expand(3.0),
                    rounding + 2.0,
                    egui::Stroke::new(1.0, color::ACCENT.gamma_multiply(0.6)),
                );
            }
            if r >= 17.0 || lit {
                let size = (11.0 * zoom.sqrt()).clamp(9.0, 13.0);
                let mut title = n.name.clone();
                if title.chars().count() > 22 {
                    title = title.chars().take(21).collect::<String>() + "…";
                }
                painter.text(
                    p + egui::vec2(0.0, r + 3.0),
                    egui::Align2::CENTER_TOP,
                    title,
                    egui::FontId::proportional(size),
                    dim(if lit { color::LABEL } else { color::LABEL_2 }, on),
                );
            }
        }

        // The hovered record, in words.
        if let Some(i) = g.hover {
            if let Some(rel) = g.nodes[i].release.clone() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.set_max_width(260.0);
                    ui.label(egui::RichText::new(&rel.title).font(font::strong(font::body().size)));
                    ui.label(&rel.artist);
                    let mut line = rel.sub.clone();
                    if let Some(l) = &rel.label {
                        if !line.is_empty() {
                            line.push_str(" · ");
                        }
                        line.push_str(l);
                    }
                    if !line.is_empty() {
                        ui.label(egui::RichText::new(line).weak());
                    }
                    let (word, c) = match rel.status {
                        Status::Owned => ("In your collection", color::LABEL_2),
                        Status::Wanted => ("On your wantlist", color::ACCENT),
                        Status::Dug => ("Dug to, not on a list yet", color::ORANGE),
                    };
                    ui.label(egui::RichText::new(word).color(c).small());
                });
            } else if g.nodes[i].kind == Kind::Artist {
                let n = &g.nodes[i];
                let words = format!(
                    "{} · {} record{}",
                    n.name,
                    n.weight,
                    if n.weight == 1 { "" } else { "s" }
                );
                resp.clone().on_hover_ui_at_pointer(|ui| {
                    ui.label(words);
                });
            }
        }

        // Legend and count, pinned to the canvas corner.
        {
            let (mut owned, mut wanted, mut dug, mut artists) = (0, 0, 0, 0);
            for n in &g.nodes {
                match (n.kind, n.release.as_ref().map(|r| r.status)) {
                    (Kind::Artist, _) => artists += 1,
                    (_, Some(Status::Owned)) => owned += 1,
                    (_, Some(Status::Wanted)) => wanted += 1,
                    (_, Some(Status::Dug)) => dug += 1,
                    _ => {}
                }
            }
            let mut x = rect.left() + 14.0;
            let y = rect.bottom() - 16.0;
            // A soft backing so the legend reads over whatever drifts under it.
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.bottom() - 32.0),
                    rect.max,
                ),
                0.0,
                color::CONTENT_BG.gamma_multiply(0.85),
            );
            let mut swatch = |c: egui::Color32, w: f32, text: String| {
                painter.rect_stroke(
                    egui::Rect::from_center_size(egui::pos2(x + 5.0, y), egui::Vec2::splat(9.0)),
                    2.0,
                    egui::Stroke::new(w, c),
                );
                let galley = painter.layout_no_wrap(text, font::caption(), color::LABEL_3);
                let size = galley.size();
                painter.galley(egui::pos2(x + 15.0, y - size.y * 0.5), galley, color::LABEL_3);
                x += 15.0 + size.x + 14.0;
            };
            swatch(color::LABEL_3, 1.0, format!("Own {owned}"));
            swatch(color::ACCENT, 2.0, format!("Want {wanted}"));
            swatch(color::ORANGE, 2.0, format!("Dug {dug}"));
            let count = format!("{artists} artists");
            painter.text(
                egui::pos2(rect.right() - 14.0, y),
                egui::Align2::RIGHT_CENTER,
                count,
                font::caption(),
                color::LABEL_4,
            );
        }

        self.graph = g;
        act
    }

    /// Aim the map's camera at the whole map. `rect` is the canvas as last
    /// drawn; the map keeps no size of its own.
    pub(crate) fn graph_fit(&mut self, rect: egui::Rect) {
        self.graph.fit(rect);
        self.graph.wake();
    }

    /// A record landed on the map from a dig (or from a list add away from
    /// the shelves). Kept in memory for this frame and written through to the
    /// catalog, so the map remembers it next launch.
    pub(crate) fn note_dug(&mut self, d: DugRelease) {
        match self.dug.iter_mut().find(|x| x.release_id == d.release_id) {
            Some(x) => {
                if d.label.is_some() {
                    x.label = d.label.clone();
                }
                if !d.sub.is_empty() {
                    x.sub = d.sub.clone();
                }
                if d.thumb_url.is_some() {
                    x.thumb_url = d.thumb_url.clone();
                }
                x.wanted |= d.wanted;
            }
            None => self.dug.push(d.clone()),
        }
        if let Ok(cat) = Catalog::open(&self.db_path) {
            let _ = cat.record_dug_release(&d);
        }
    }

    /// The user asked Discogs to put `release_id` on a list. If the map only
    /// knows it as dug, it reads as wanted from now; if the map has never
    /// seen it (a want from the library table, say), it lands on the map
    /// with whatever this window knows about it. `fallback` is the edit's
    /// status label, an `Artist – Title` string, for when nothing else does.
    pub(crate) fn note_listed(&mut self, release_id: u64, fallback: &str) {
        if self.vinyl_owned.contains(&release_id) || self.vinyl_wanted.contains(&release_id) {
            return;
        }
        if let Some(d) = self.dug.iter_mut().find(|x| x.release_id == release_id) {
            d.wanted = true;
            if let Ok(cat) = Catalog::open(&self.db_path) {
                let _ = cat.mark_dug_wanted(release_id);
            }
            return;
        }
        let d = self
            .release_meta(release_id)
            .unwrap_or_else(|| {
                let (artist, title) = fallback
                    .split_once(" – ")
                    .or_else(|| fallback.split_once(" - "))
                    .map(|(a, t)| (a.trim().to_string(), t.trim().to_string()))
                    .unwrap_or_else(|| (String::new(), fallback.trim().to_string()));
                DugRelease {
                    release_id,
                    artist,
                    title,
                    label: None,
                    sub: String::new(),
                    thumb_url: None,
                    wanted: true,
                    dug_at: unix_now(),
                }
            });
        self.note_dug(DugRelease { wanted: true, ..d });
    }

    /// What this window knows about a bare release: the dig it may be on,
    /// the sheet it may be open in, or a seller's crate it may be listed in.
    fn release_meta(&self, release_id: u64) -> Option<DugRelease> {
        if let Some(step) = self
            .dig
            .as_ref()
            .and_then(|d| d.steps.iter().find(|s| s.release_id == release_id))
        {
            return Some(DugRelease {
                release_id,
                artist: step.artist.clone(),
                title: step.title.clone(),
                label: step.label.clone(),
                sub: step.sub.clone(),
                thumb_url: step.thumb_url.clone(),
                wanted: false,
                dug_at: unix_now(),
            });
        }
        if let Some(s) = self
            .vinyl_sheet
            .as_ref()
            .filter(|s| s.release_id == release_id)
        {
            return Some(DugRelease {
                release_id,
                artist: s.artist.clone(),
                title: s.title.clone(),
                label: None,
                sub: s.sub.clone(),
                thumb_url: s.cover_url.clone(),
                wanted: false,
                dug_at: unix_now(),
            });
        }
        if let Some(l) = self
            .seller_listings
            .iter()
            .find(|l| l.release_id == release_id)
        {
            return Some(DugRelease {
                release_id,
                artist: l.artist.clone(),
                title: l.title.clone(),
                label: l.label.clone(),
                sub: match (l.year, l.format.as_deref()) {
                    (Some(y), Some(f)) => format!("{y} · {f}"),
                    (Some(y), None) => y.to_string(),
                    (None, Some(f)) => f.to_string(),
                    (None, None) => String::new(),
                },
                thumb_url: l.thumb_url.clone(),
                wanted: false,
                dug_at: unix_now(),
            });
        }
        None
    }
}

pub(crate) fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// The tab strip's map button: a small solar system, painted rather than
/// set in type so it sits with the drawn controls. `active` fills it the
/// way the active tab is filled; the sun and orbits carry the accent then.
pub(crate) fn solar_icon(p: &egui::Painter, c: egui::Pos2, ink: egui::Color32, r: f32) {
    let thin = egui::Stroke::new(1.1, ink.gamma_multiply(0.55));
    p.circle_stroke(c, r * 0.52, thin);
    p.circle_stroke(c, r * 0.92, thin);
    p.circle_filled(c, r * 0.2, ink);
    // Two planets on the orbits, off-axis so it reads as motion, not a dial.
    let a1 = -0.9f32;
    let a2 = 2.35f32;
    p.circle_filled(
        c + egui::vec2(a1.cos(), a1.sin()) * r * 0.52,
        r * 0.11,
        ink,
    );
    p.circle_filled(
        c + egui::vec2(a2.cos(), a2.sin()) * r * 0.92,
        r * 0.14,
        ink,
    );
}
