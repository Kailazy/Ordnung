//! The record map: every record you've crossed paths with, drawn as one living
//! web. Artists are the hubs; their releases hang off them, and releases an
//! artist put out on the same label gather under a small label node between
//! the two. The collection and the wantlist are on it as they stand, and so
//! is every record a dig has landed on — a dug record joins the map the moment
//! it's dug to, and reads as wanted the moment it's asked for on a list,
//! before Discogs has answered.
//!
//! The layout is live rather than a fixed drawing, but tidy: each hub lays
//! its records on concentric rings around itself, and only the hubs take
//! part in a simulation, each as one body the size of its whole cloud, so
//! clouds keep clear of each other and settle into an even field. Records
//! glide to their ring slots on a critically damped spring, so a landing
//! arrives and stops rather than wobbling. The simulation sleeps once it's
//! still, so an idle map costs nothing.
//!
//! Zoom is bounded on the wide end at the scale that shows the whole map, so
//! however far out you go, nothing is ever off the edge. Split out of
//! `views.rs`; part of the GUI `App`.
//!
//! The map is also where you dig. A record under the pointer (or the one the
//! dig stands on) puts out three small thread nodes, artist, label and style,
//! and clicking one takes that thread exactly as the strip's buttons do: the
//! find lands on the map, joined to the record it was dug from by a trail in
//! the thread's colour. The radio (see `radio.rs`) walks the map the same way
//! on its own, playing each record it lands on.
use super::*;
use crate::dig::DigThread;
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

impl Status {
    pub(crate) const ALL: [Status; 3] = [Status::Owned, Status::Wanted, Status::Dug];

    /// The word the config stores for a kind hidden from the map.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Owned => "owned",
            Self::Wanted => "wanted",
            Self::Dug => "dug",
        }
    }

    /// The kind's name in the map's filter menu and legend.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Owned => "Collection",
            Self::Wanted => "Wantlist",
            Self::Dug => "Discovered",
        }
    }

    /// The kinds `hide` (the config's list of keys) leaves off the map.
    pub(crate) fn hidden(hide: &[String]) -> Vec<Status> {
        Self::ALL
            .into_iter()
            .filter(|s| hide.iter().any(|k| k == s.key()))
            .collect()
    }
}

/// What the map gathers records around. Switching re-homes every record
/// under new hubs in place, and the springs carry them across.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Arrange {
    /// Artist hubs, with a label sub-group where an artist has two or more
    /// records on one label.
    Artists,
    /// Style hubs ("Deep House"), each a cloud of the records tagged with
    /// it first; a record's further styles tie it loosely to those clouds
    /// too, so neighbouring styles drift together.
    Genres,
}

impl Arrange {
    pub(crate) fn from_key(k: &str) -> Self {
        if k == "genres" {
            Self::Genres
        } else {
            Self::Artists
        }
    }
    pub(crate) fn key(self) -> &'static str {
        match self {
            Self::Artists => "artists",
            Self::Genres => "genres",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A cluster's centre: an artist, or a style in genre clouds.
    Hub,
    /// The sub-group between a hub and some of its records: a label an
    /// artist has two or more records on.
    Sub,
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
    /// Discogs genres then styles, as the shelf row or the tag caches know
    /// them. Empty for a record nothing has tagged yet.
    pub genres: Vec<String>,
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
    /// How far the node pushes others away. A record's is its size; a hub's
    /// takes in the room its records need, so two big clouds keep their
    /// distance instead of interleaving.
    reach: f32,
    /// A record's (or sub-group's) place on its cloud's rings, relative to
    /// the top-level hub. The node eases toward `hub_pos + slot` every tick;
    /// only hubs are moved by the simulation itself.
    slot: egui::Vec2,
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
    /// How far this record's thread nodes are out: 0 tucked away, 1 at
    /// rest. Its own spring, so they bloom and fold rather than blink.
    threads: f32,
    threads_v: f32,
}

/// A thread taken: from the record it was taken out of to the record it
/// found, and what was matched on the way (the artist, label or style name).
struct Trail {
    from: u64,
    to: u64,
    thread: DigThread,
    via: String,
}

struct Edge {
    a: usize,
    b: usize,
    /// A same-label tie between two artists' label groups, or a record's
    /// tie to a further style: drawn as a faint dash when zoomed in, never
    /// a force.
    soft: bool,
}

/// What the user did to the map this frame, applied by the caller once the
/// state is back on `self`.
pub(crate) enum GraphAct {
    Open(Release),
    /// A thread node was clicked: take that thread out of this record.
    Thread(Release, DigThread),
    /// The radio node was clicked: start the radio from this record.
    Radio(Release),
}

/// A thread taken from the map, from the click to the landing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Await {
    /// Standing on the record, waiting for its Discogs ids before the
    /// thread can be asked for.
    Resolve { from: u64, thread: DigThread },
    /// The step is out; waiting for Discogs to answer with a record.
    Land { from: u64, thread: DigThread },
}

impl Await {
    pub(crate) fn is_for(self, release_id: u64, thread: DigThread) -> bool {
        match self {
            Await::Resolve { from, thread: t } | Await::Land { from, thread: t } => {
                from == release_id && t == thread
            }
        }
    }
}

/// What the map knows about a record's thread before it's taken.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Avail {
    /// Not looked up yet: a click will stand on the record and find out.
    Unknown,
    /// The Discogs id or style tags are known; a click takes it.
    Ready,
    /// The release detail answered and this record has nothing down that
    /// thread.
    Missing,
}

/// The colour a thread wears on the map: its trails and its lit node.
pub(crate) fn thread_tint(t: DigThread) -> egui::Color32 {
    use crate::ui::tokens::color;
    match t {
        DigThread::Artist => color::TEAL,
        DigThread::Label => color::YELLOW,
        DigThread::Style => color::PURPLE,
        // The sideways threads take the hue of the name thread they browse
        // by — an alias is still an artist, a company still a label, an
        // era still a style — a shade off, so a trail reads as kin.
        DigThread::Alias => color::MINT,
        DigThread::Credit => color::ORANGE,
        DigThread::Company => color::BROWN,
        DigThread::Era => color::INDIGO,
    }
}

/// One of the small nodes a record puts out: a thread to dig, or the radio.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Knob {
    Thread(DigThread),
    /// Start the radio from this record.
    Radio,
}

impl Knob {
    fn tint(self) -> egui::Color32 {
        match self {
            Knob::Thread(t) => thread_tint(t),
            Knob::Radio => crate::ui::tokens::color::ACCENT,
        }
    }
}

/// The knob's mark, painted at `r` half-size.
fn knob_glyph(p: &egui::Painter, k: Knob, c: egui::Pos2, ink: egui::Color32, r: f32) {
    match k {
        Knob::Thread(DigThread::Artist) => crate::ui::icon::artist(p, c, ink, r),
        Knob::Thread(DigThread::Label) => crate::ui::icon::house(p, c, ink, r),
        Knob::Thread(DigThread::Style) => crate::ui::icon::style(p, c, ink, r),
        // Never bloomed on the map (see `knobs_of`); drawn as the style
        // glyph should one ever be.
        Knob::Thread(_) => crate::ui::icon::style(p, c, ink, r),
        Knob::Radio => crate::ui::icon::broadcast(p, c, ink, r),
    }
}

/// Screen radius of a thread node. Screen space, not world: the nodes are a
/// control, and a control stays the same size however far out the map is.
const THREAD_R: f32 = 10.0;
/// Clear space between a cover's edge and its thread nodes.
const THREAD_GAP: f32 = 12.0;
/// Angle between neighbouring thread nodes.
const THREAD_SPREAD: f32 = 0.6;
/// Smallest a cover may be on screen for a hover to put its nodes out: any
/// smaller and four nodes the size of a control would swamp it. The dig's
/// own record keeps its nodes at any scale.
const HOVER_MIN_R: f32 = 7.0;

/// The simulation, its camera and the interaction state. Lives on `App` and
/// is rebuilt from the record lists whenever they change.
pub(crate) struct GraphState {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    index: HashMap<String, usize>,
    /// Signature of the sources the nodes were built from; a sync is skipped
    /// while it matches.
    sig: u64,
    /// How the map was last built.
    arrange: Arrange,
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
    /// The record whose thread nodes are out for the pointer: the one under
    /// it, held while the pointer crosses the gap to the nodes themselves.
    shown: Option<String>,
    /// The record the dig stands on. Its threads stay out without a hover,
    /// and the radio lights it while it plays.
    pub(crate) focus: Option<String>,
    /// The knob under the pointer: its record's index and which knob.
    hover_knob: Option<(usize, Knob)>,
    /// A thread taken from the map, from click to landing.
    pub(crate) awaiting: Option<Await>,
    /// A record the camera should move to once it's on the map, and whether
    /// to lean in on it or only bring it into view.
    pub(crate) lean: Option<(String, bool)>,
    /// Every thread ever taken this session, by release id, drawn as a trail
    /// wherever both ends are on the map. Outlives the dig web itself, so a
    /// fresh dig doesn't wipe the walk that came before.
    trails: Vec<Trail>,
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
            arrange: Arrange::Artists,
            cam: egui::Vec2::ZERO,
            zoom: 1.0,
            cam_goal: egui::Vec2::ZERO,
            zoom_goal: 1.0,
            fitted: false,
            follow_fit: true,
            hover: None,
            drag: None,
            shown: None,
            focus: None,
            hover_knob: None,
            awaiting: None,
            lean: None,
            trails: Vec::new(),
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
/// Velocity kept per 60 Hz frame by a moving hub. Higher is bouncier; 1.0
/// would never settle. Set so a cloud glides into place with at most a
/// slight overshoot: the map should move like something heavy, not rubber.
const DAMPING: f32 = 0.86;
/// The spring that carries a record to its ring slot: stiffness and, at
/// critical damping, the matching drag, so it arrives without a wobble.
const SLOT_K: f32 = 70.0;
/// Clear space between two covers on a ring, and between rings.
const RING_GAP: f32 = 10.0;
/// Range and strength of the repulsion between any two nodes.
const REPEL_RANGE: f32 = 320.0;
const REPEL_K: f32 = 7000.0;
/// Pull toward the middle, keeping the map one body. A constant tug rather
/// than a spring, so the map settles as an even disc instead of a dense
/// core with a thin rim.
const GRAVITY_HUB: f32 = 70.0;
/// A small spring-like share on top of the constant tug, so the rim of a
/// large map meets a firm edge rather than creeping outward for minutes.
const GRAVITY_SLOPE: f32 = 0.05;
const MAX_SPEED: f32 = 900.0;
/// Below this top speed the simulation goes to sleep.
const SLEEP_SPEED: f32 = 6.0;
/// Top speed under which the map is "settling": damping firms up so the last
/// low-amplitude jitter dies instead of ringing on. Big motion (a landing,
/// a fling) stays bouncy.
const SETTLE_SPEED: f32 = 90.0;
const SETTLE_DAMPING: f32 = 0.80;
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

/// A hub is an anchor, not a body: the cloud's first record sits on it and
/// the area is painted around it, so it only needs a small radius of its
/// own for hit-testing.
fn hub_radius(_weight: usize) -> f32 {
    4.0
}

/// A cloud's tint, from its name: one hue per cloud, kept muted and dark
/// so it reads as a wash under the covers rather than a colour of its own.
fn cloud_tint(name: &str) -> egui::Color32 {
    let h = name
        .bytes()
        .fold(0x811c_9dc5u32, |a, b| (a ^ b as u32).wrapping_mul(0x0100_0193));
    let hue = (h % 360) as f32 / 360.0;
    let (s, l) = (0.55, 0.58);
    // HSL to RGB.
    let q = l + s - l * s;
    let p = 2.0 * l - q;
    let chan = |t: f32| -> f32 {
        let t = t.rem_euclid(1.0);
        if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        }
    };
    egui::Color32::from_rgb(
        (chan(hue + 1.0 / 3.0) * 255.0) as u8,
        (chan(hue) * 255.0) as u8,
        (chan(hue - 1.0 / 3.0) * 255.0) as u8,
    )
}

/// Lay `n` bodies of half-size `body` on concentric rings outside a hub of
/// radius `inner`: each ring holds as many as fit at cover pitch, spread
/// evenly around it. Returns each body's offset from the hub, in order, and
/// the outer radius of the cloud. Consecutive bodies sit side by side, so a
/// sub-group's records form one arc.
fn ring_slots(n: usize, inner: f32, body: f32, phase: f32) -> (Vec<egui::Vec2>, f32) {
    let pitch = body * 2.0 + RING_GAP;
    let mut out = Vec::with_capacity(n);
    if n == 0 {
        return (out, inner);
    }
    // The first body sits on the anchor; the rings go around it.
    out.push(egui::Vec2::ZERO);
    let mut radius = inner.max(body) + body + RING_GAP;
    let mut left = n - 1;
    let mut ring = 0usize;
    while left > 0 {
        let cap = ((std::f32::consts::TAU * radius) / pitch).floor().max(1.0) as usize;
        let here = left.min(cap);
        // Alternate the start angle ring to ring so spokes don't line up.
        let start = phase + ring as f32 * 0.5;
        for j in 0..here {
            let a = start + std::f32::consts::TAU * j as f32 / here as f32;
            out.push(egui::vec2(a.cos(), a.sin()) * radius);
        }
        left -= here;
        ring += 1;
        if left > 0 {
            radius += pitch;
        }
    }
    let outer = if n == 1 { body } else { radius + body };
    (out, outer)
}

/// The cloud for records with no genre or style at all. It holds them like
/// any other cloud but wears no caption: a word for the absence of a tag
/// is not a place on the map.
const UNTAGGED: &str = "Untagged";

/// Is this tag one of Discogs's coarse genres (as opposed to a style)?
fn is_coarse_genre(tag: &str) -> bool {
    crate::DISCOGS_GENRES.iter().any(|g| g.eq_ignore_ascii_case(tag))
}

/// The style a record files under in genre clouds, and the further styles
/// that tie it to other clouds. The first *style* wins over the coarse genre
/// — for a DJ's shelves "Electronic" would hold nearly everything, and the
/// styles are where the shape is — with the genre as the fallback for a
/// record tagged only coarsely.
fn cloud_of(genres: &[String]) -> (String, Vec<String>) {
    let mut styles = genres.iter().filter(|t| !is_coarse_genre(t)).map(|t| t.trim().to_string());
    match styles.next() {
        Some(first) => (first, styles.take(2).collect()),
        None => match genres.first() {
            Some(g) => (g.trim().to_string(), Vec::new()),
            None => (UNTAGGED.to_string(), Vec::new()),
        },
    }
}

impl GraphState {
    /// Rebuild the nodes from the three sources when any of them changed.
    /// Existing nodes keep their position and motion; new ones are born at
    /// their hub and spring out from it; nodes no source names any more go.
    fn sync(
        &mut self,
        arrange: Arrange,
        owned: &[VinylRecord],
        wanted: &[VinylRecord],
        dug: &[DugRelease],
        dug_genres: &HashMap<u64, Vec<String>>,
        hidden: &[Status],
    ) {
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
                        genres: v.genres.clone(),
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
                    genres: dug_genres.get(&d.release_id).cloned().unwrap_or_default(),
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
        // The filter applies after the merge, so a shelf record a dig also
        // landed on stays with its shelf when discovered records are hidden.
        if !hidden.is_empty() {
            releases.retain(|r| !hidden.contains(&r.status));
        }

        let mut sig = 0xcbf2_9ce4_8422_2325u64;
        mix(&mut sig, arrange as u64);
        for r in &releases {
            mix(&mut sig, r.release_id);
            mix(&mut sig, r.status as u64);
            mix_str(&mut sig, r.label.as_deref().unwrap_or(""));
            for g in &r.genres {
                mix_str(&mut sig, g);
            }
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
        self.arrange = arrange;

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

        // Loose ties from a record to the further clouds it belongs to, by
        // key; resolved to indices once the nodes are compacted.
        let mut ties: Vec<(String, String)> = Vec::new();
        let n_before = self.nodes.len();
        for (r, ak) in releases.into_iter().zip(artist_keys) {
            let hub_key = match arrange {
                Arrange::Artists => {
                    let artist_name =
                        crate::dig::strip_disambiguator(&r.artist).trim().to_string();
                    let artist_name = if artist_name.is_empty() {
                        "Unknown artist".to_string()
                    } else {
                        artist_name
                    };
                    let ai = self.ensure(Kind::Hub, &ak, &artist_name, None);
                    self.nodes[ai].weight += 1;
                    match &r.label {
                        Some(l)
                            if per_label
                                .get(&(ak.clone(), fold(l)))
                                .copied()
                                .unwrap_or(0)
                                >= 2 =>
                        {
                            let lk = format!("l:{ak}|{}", fold(l));
                            let li = self.ensure(Kind::Sub, &lk, l.trim(), Some(ak.clone()));
                            self.nodes[li].weight += 1;
                            lk
                        }
                        _ => ak.clone(),
                    }
                }
                Arrange::Genres => {
                    let (first, more) = cloud_of(&r.genres);
                    let gk = format!("g:{}", fold(&first));
                    let gi = self.ensure(Kind::Hub, &gk, &first, None);
                    self.nodes[gi].weight += 1;
                    for style in more {
                        let sk = format!("g:{}", fold(&style));
                        // A tie's far end exists only if something files
                        // under it first; a style nobody leads with isn't
                        // a cloud, and the tie is dropped at resolution.
                        ties.push((format!("r:{}", r.release_id), sk));
                    }
                    gk
                }
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
            let w = self.nodes[i].weight as f32;
            self.nodes[i].r = match self.nodes[i].kind {
                Kind::Hub => hub_radius(self.nodes[i].weight),
                Kind::Sub => LABEL_R + 1.5 * w.sqrt(),
                Kind::Release => REL_R,
            };
            let _ = w;
            self.nodes[i].hay = match self.nodes[i].kind {
                Kind::Release => std::mem::take(&mut self.nodes[i].hay),
                _ => self.nodes[i].name.to_lowercase(),
            };
        }

        // Newborns: a hub whose records are already on the map appears among
        // them (a rearrangement grows its new hubs out of the old clusters,
        // then the springs sort the records); a hub with no home yet takes a
        // sunflower spiral slot so a whole fresh map opens evenly; a record
        // starts on its hub and springs out.
        let placed: Vec<bool> = self.nodes.iter().map(|n| n.scale > 0.0).collect();
        let mut spiral = 0usize;
        for i in 0..self.nodes.len() {
            if placed[i] || self.nodes[i].kind == Kind::Release {
                continue;
            }
            let (mut sum, mut count) = (egui::Vec2::ZERO, 0usize);
            for (j, n) in self.nodes.iter().enumerate() {
                if placed[j] && n.hub == Some(i) {
                    sum += n.pos;
                    count += 1;
                }
            }
            let a = next_rand(&mut self.seed) * std::f32::consts::TAU;
            let jitter = egui::vec2(a.cos(), a.sin());
            self.nodes[i].pos = if count > 0 {
                // Rearranging spreads every cluster over the whole map, so
                // the centroids all land near the middle; a growing offset
                // keeps the new hubs from starting on top of each other.
                let k = spiral as f32;
                spiral += 1;
                sum / count as f32 + jitter * (40.0 * k.sqrt())
            } else if let Some(p) = self.nodes[i].hub.filter(|&h| placed[h]).map(|h| self.nodes[h].pos) {
                p + jitter * (self.nodes[i].r + 6.0)
            } else {
                let k = (self.nodes.len() + spiral) as f32;
                spiral += 1;
                let r = 30.0 * k.sqrt();
                let t = k * 2.399_963;
                egui::vec2(r * t.cos(), r * t.sin()) + jitter * 8.0
            };
            self.nodes[i].vel = egui::Vec2::ZERO;
        }
        let hub_pos: Vec<Option<egui::Vec2>> = self
            .nodes
            .iter()
            .map(|n| n.hub.map(|h| self.nodes[h].pos))
            .collect();
        let mut seed = self.seed;
        for ((n, placed), hub_pos) in self.nodes.iter_mut().zip(placed).zip(hub_pos) {
            if placed || n.kind != Kind::Release {
                continue;
            }
            let a = next_rand(&mut seed) * std::f32::consts::TAU;
            let jitter = egui::vec2(a.cos(), a.sin());
            n.pos = match hub_pos {
                Some(p) => p + jitter * (n.r + 6.0),
                None => jitter * 20.0,
            };
            n.vel = egui::Vec2::ZERO;
        }
        self.seed = seed;

        // Every top-level hub lays its members on rings: its own records
        // first, then each label sub-group as one arc (the group's mark,
        // then its records). A sub-group's records are keyed to the group
        // but placed on the artist's rings, so the arc reads as a bay of
        // that artist's cloud rather than a second cloud hanging off it.
        let hubs: Vec<usize> = (0..self.nodes.len())
            .filter(|&i| self.nodes[i].kind == Kind::Hub)
            .collect();
        for h in hubs {
            let mut members: Vec<usize> = Vec::new();
            let mut direct: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| self.nodes[i].kind == Kind::Release && self.nodes[i].hub == Some(h))
                .collect();
            direct.sort_by(|a, b| self.nodes[*a].key.cmp(&self.nodes[*b].key));
            members.extend(direct);
            let mut subs: Vec<usize> = (0..self.nodes.len())
                .filter(|&i| self.nodes[i].kind == Kind::Sub && self.nodes[i].hub == Some(h))
                .collect();
            subs.sort_by(|a, b| self.nodes[*a].name.cmp(&self.nodes[*b].name));
            for sidx in subs {
                members.push(sidx);
                let mut under: Vec<usize> = (0..self.nodes.len())
                    .filter(|&i| {
                        self.nodes[i].kind == Kind::Release && self.nodes[i].hub == Some(sidx)
                    })
                    .collect();
                under.sort_by(|a, b| self.nodes[*a].key.cmp(&self.nodes[*b].key));
                members.extend(under);
            }
            // A fixed phase per hub, so a lone record doesn't always hang
            // due east of its artist.
            let phase = (self.nodes[h].key.bytes().fold(7u32, |a, b| a.wrapping_mul(31).wrapping_add(b as u32)) % 628) as f32 / 100.0;
            let (slots, outer) = ring_slots(members.len(), self.nodes[h].r, REL_R, phase);
            for (m, slot) in members.iter().zip(slots) {
                self.nodes[*m].slot = slot;
            }
            self.nodes[h].reach = outer + RING_GAP;
        }

        // Ties are drawn, never pulled: a label two artists share, and a
        // record's further styles.
        self.edges.clear();
        for i in 0..self.nodes.len() {
            if let Some(h) = self.nodes[i].hub {
                self.edges.push(Edge {
                    a: i,
                    b: h,
                    soft: false,
                });
            }
        }
        let mut by_label: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, n) in self.nodes.iter().enumerate() {
            if n.kind == Kind::Sub {
                by_label.entry(fold(&n.name)).or_default().push(i);
            }
        }
        for (_, group) in by_label {
            for w in group.windows(2) {
                self.edges.push(Edge {
                    a: w[0],
                    b: w[1],
                    soft: true,
                });
            }
        }
        for (rk, gk) in ties {
            if let (Some(&a), Some(&b)) = (self.index.get(&rk), self.index.get(&gk)) {
                self.edges.push(Edge { a, b, soft: true });
            }
        }
        // A map built from nothing is settled off screen first, so it opens
        // in its shape with every record blooming into place, rather than
        // spending its first half minute drifting apart. Records that land
        // later spring out from their hub in full view.
        if n_before == 0 && !self.nodes.is_empty() {
            for _ in 0..240 {
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
                Kind::Hub => hub_radius(1),
                Kind::Sub => LABEL_R,
                Kind::Release => REL_R,
            },
            reach: REL_R,
            slot: egui::Vec2::ZERO,
            scale: 0.0,
            scale_v: 0.0,
            hub_key,
            hub: None,
            release: None,
            weight: 0,
            alive: true,
            hay: String::new(),
            held: false,
            threads: 0.0,
            threads_v: 0.0,
        });
        self.index.insert(key.to_string(), i);
        i
    }

    /// A dug record traded for another pressing: the node, its trails and
    /// the camera's interest in it carry over to the new id, so the swap
    /// changes the sleeve and nothing about the shape of the map. The next
    /// sync refreshes the node's words from the dug list.
    pub(crate) fn remap_release(&mut self, from: u64, to: u64) {
        let (old, new) = (format!("r:{from}"), format!("r:{to}"));
        // If the new pressing already has a node of its own, the old one is
        // left for the sync to retire: the sources no longer name it.
        if !self.index.contains_key(&new) {
            if let Some(i) = self.index.remove(&old) {
                self.nodes[i].key = new.clone();
                if let Some(r) = self.nodes[i].release.as_mut() {
                    r.release_id = to;
                }
                self.index.insert(new.clone(), i);
            }
        }
        for t in self.trails.iter_mut() {
            if t.from == from {
                t.from = to;
            }
            if t.to == from {
                t.to = to;
            }
        }
        let swap = |k: &mut Option<String>| {
            if k.as_deref() == Some(old.as_str()) {
                *k = Some(new.clone());
            }
        };
        swap(&mut self.focus);
        swap(&mut self.shown);
        if let Some((k, _)) = self.lean.as_mut() {
            if *k == old {
                *k = new.clone();
            }
        }
        self.sig = 0;
        self.wake();
    }

    pub(crate) fn wake(&mut self) {
        if self.last_tick.is_none() {
            self.last_tick = Some(Instant::now());
        }
        self.energy = f32::MAX;
    }

    /// One simulation step. Returns whether anything is still moving.
    ///
    /// Only hubs are simulated: each is one body whose reach is the whole
    /// cloud, so clouds keep clear of each other and the map settles as an
    /// even field. Everything else eases to its ring slot around its hub.
    fn tick(&mut self, dt: f32) -> bool {
        let n = self.nodes.len();
        if n == 0 {
            return false;
        }
        let dt = dt.clamp(1.0 / 240.0, 1.0 / 30.0);
        let hubs: Vec<usize> = (0..n).filter(|&i| self.nodes[i].kind == Kind::Hub).collect();
        let substeps = 2;
        let h = dt / substeps as f32;
        let mut forces = vec![egui::Vec2::ZERO; n];
        let mut top_speed = 0.0f32;
        for _ in 0..substeps {
            forces.fill(egui::Vec2::ZERO);
            // Repulsion between clouds over a uniform grid: only pairs
            // within range meet.
            let cell = REPEL_RANGE;
            let mut grid: HashMap<(i32, i32), Vec<usize>> = HashMap::new();
            for &i in &hubs {
                let p = self.nodes[i].pos;
                let c = ((p.x / cell).floor() as i32, (p.y / cell).floor() as i32);
                grid.entry(c).or_default().push(i);
            }
            for (&(cx, cy), bucket) in &grid {
                for dx in -1..=1 {
                    for dy in -1..=1 {
                        let other = match grid.get(&(cx + dx, cy + dy)) {
                            Some(o) => o,
                            None => continue,
                        };
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
                                let dist = d.length().max(0.5);
                                let (ri, rj) = (self.nodes[i].reach, self.nodes[j].reach);
                                // Clouds must not overlap, and their washes
                                // want clear ground between them: a firm push
                                // while they're closer than that margin, and
                                // a soft one for a way beyond.
                                let touch = ri + rj + 36.0;
                                let range = touch + REPEL_RANGE * 0.5;
                                if dist >= range {
                                    continue;
                                }
                                let dir = d / dist;
                                let mut f = REPEL_K * 0.02 * (1.0 - (dist - touch).max(0.0) / (range - touch));
                                if dist < touch {
                                    f += (touch - dist) * 140.0;
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
            for &i in &hubs {
                let node = &mut self.nodes[i];
                if node.held {
                    continue;
                }
                let len = node.pos.length();
                let pull = if len > 1e-3 {
                    node.pos / len * (GRAVITY_HUB * (len / 40.0).min(1.0) + GRAVITY_SLOPE * len)
                } else {
                    egui::Vec2::ZERO
                };
                let f = forces[i] - pull;
                let mass = (node.reach / 40.0).max(1.0);
                node.vel = (node.vel + f / mass * h) * damp;
                let speed = node.vel.length();
                if speed > MAX_SPEED {
                    node.vel *= MAX_SPEED / speed;
                }
                node.pos += node.vel * h;
                top_speed = top_speed.max(speed);
            }
        }
        // Members ease to their slots around whichever hub they belong to
        // (a sub-group's records ride the artist's rings): a critically
        // damped spring, so they arrive and stop.
        let crit = 2.0 * SLOT_K.sqrt();
        for i in 0..n {
            if self.nodes[i].kind == Kind::Hub || self.nodes[i].held {
                continue;
            }
            let Some(mut top) = self.nodes[i].hub else { continue };
            if self.nodes[top].kind == Kind::Sub {
                let Some(up) = self.nodes[top].hub else { continue };
                top = up;
            }
            let target = self.nodes[top].pos + self.nodes[i].slot;
            let node = &mut self.nodes[i];
            let a = (target - node.pos) * SLOT_K - node.vel * crit;
            node.vel += a * dt;
            node.pos += node.vel * dt;
            let off = (target - node.pos).length();
            top_speed = top_speed.max(node.vel.length().max(off * 2.0));
        }
        self.energy = top_speed;
        if top_speed <= SLEEP_SPEED {
            // Still enough: stop outright, so nothing creeps while asleep.
            for node in &mut self.nodes {
                node.vel = egui::Vec2::ZERO;
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
            // A hub's extent is its wash plus the caption over it.
            let ext = if n.kind == Kind::Hub { n.reach * 1.3 + 26.0 } else { n.r + 12.0 };
            let b = egui::Rect::from_center_size(c, egui::Vec2::splat(ext * 2.0));
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

    /// Where record `i`'s knobs sit on screen: the three threads and the
    /// radio, fanned out on the far side of the record from its cloud's
    /// centre and scaled by the record's bloom, so they grow out of the
    /// cover rather than appear.
    fn knob_slots(&self, i: usize, canvas_center: egui::Pos2) -> [(egui::Pos2, Knob); 4] {
        let n = &self.nodes[i];
        let p = canvas_center + (n.pos - self.cam) * self.zoom;
        let r = n.r * n.scale * self.zoom;
        let mut top = n.hub;
        if let Some(h) = top {
            if self.nodes[h].kind == Kind::Sub {
                top = self.nodes[h].hub;
            }
        }
        let out = match top {
            Some(h) => {
                let d = n.pos - self.nodes[h].pos;
                if d.length() > 1e-3 {
                    d.normalized()
                } else {
                    egui::vec2(0.0, -1.0)
                }
            }
            None => egui::vec2(0.0, -1.0),
        };
        let base = out.angle();
        let dist = (r + THREAD_GAP + THREAD_R) * n.threads.min(1.0);
        let at = |k: f32, knob: Knob| {
            let a = base + k * THREAD_SPREAD;
            (p + egui::vec2(a.cos(), a.sin()) * dist, knob)
        };
        [
            at(-1.5, Knob::Thread(DigThread::Artist)),
            at(-0.5, Knob::Thread(DigThread::Label)),
            at(0.5, Knob::Thread(DigThread::Style)),
            at(1.5, Knob::Radio),
        ]
    }

    /// Whether `p` is inside record `i`'s halo: the cover plus the ring its
    /// knobs sit on, with a little slack. While the pointer is in there the
    /// record keeps its knobs out and its neighbours can't steal the hover,
    /// so crossing the gap to a knob in a packed cloud is a steady move.
    fn in_halo(&self, i: usize, p: egui::Pos2, canvas_center: egui::Pos2) -> bool {
        let n = &self.nodes[i];
        let s = canvas_center + (n.pos - self.cam) * self.zoom;
        let r = n.r * n.scale.max(1.0) * self.zoom;
        (p - s).length() <= r + THREAD_GAP + THREAD_R * 2.0 + 8.0
    }

    /// The record filed under `key`, if it's on the map.
    pub(crate) fn release_at_key(&self, key: &str) -> Option<Release> {
        self.index
            .get(key)
            .and_then(|&i| self.nodes[i].release.clone())
    }

    /// The record the map is standing on: the dig's focus, or failing that
    /// the one whose threads are out under the pointer.
    pub(crate) fn standing_on(&self) -> Option<Release> {
        self.focus
            .as_deref()
            .or(self.shown.as_deref())
            .and_then(|k| self.release_at_key(k))
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
        let arrange = Arrange::from_key(&self.config.graph_arrange);
        let hidden = Status::hidden(&self.config.graph_hide);
        g.sync(
            arrange,
            &self.vinyl,
            &self.wantlist,
            &self.dug,
            &self.dug_genres,
            &hidden,
        );
        self.map_dig_tick(&mut g);

        let resp = ui.allocate_rect(rect, egui::Sense::click_and_drag());
        let painter = ui.painter().with_clip_rect(rect);
        let now = Instant::now();
        let ctx = ui.ctx().clone();
        let mut act = None;

        if g.nodes.is_empty() {
            // Empty because there's nothing, or because the filters hide
            // all of what there is: the second wants a different nudge.
            let have_any = !self.vinyl.is_empty() || !self.wantlist.is_empty() || !self.dug.is_empty();
            let (head, hint) = if !hidden.is_empty() && have_any {
                (
                    "Everything is hidden",
                    "Show more kinds of record from the Show menu above",
                )
            } else {
                (
                    "Nothing on the map yet",
                    "Sync your Discogs shelves, or start a dig from any record",
                )
            };
            painter.text(
                rect.center() - egui::vec2(0.0, 10.0),
                egui::Align2::CENTER_CENTER,
                head,
                font::headline(),
                color::LABEL_2,
            );
            painter.text(
                rect.center() + egui::vec2(0.0, 12.0),
                egui::Align2::CENTER_CENTER,
                hint,
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
        // A landing the map was asked to go to: lean in on it once it's
        // here, or just bring it into view.
        if let Some((key, lean_in)) = g.lean.take() {
            match g.index.get(&key).copied() {
                Some(i) => {
                    g.cam_goal = g.nodes[i].pos;
                    if lean_in {
                        g.zoom_goal = 1.1f32.clamp(min_zoom, MAX_ZOOM);
                    }
                    g.follow_fit = false;
                }
                None => g.lean = Some((key, lean_in)),
            }
        }
        let center = rect.center();
        let to_screen = |cam: egui::Vec2, zoom: f32, p: egui::Vec2| -> egui::Pos2 {
            center + (p - cam) * zoom
        };
        let to_world = |cam: egui::Vec2, zoom: f32, s: egui::Pos2| -> egui::Vec2 {
            cam + (s - center) / zoom
        };

        let pointer = resp.hover_pos().or_else(|| resp.interact_pointer_pos());
        if resp.hovered() || resp.dragged() {
            // A pinch zooms about the pointer; a two-finger scroll pans the
            // canvas in both directions, the way a map does under a trackpad,
            // and so does a drag on empty canvas.
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
        // Records and sub-group marks first; failing those, the cloud whose
        // area the pointer is over.
        let hit = |g: &GraphState, p: egui::Pos2| -> Option<usize> {
            let mut best: Option<(usize, f32)> = None;
            for (i, n) in g.nodes.iter().enumerate() {
                if n.kind == Kind::Hub {
                    continue;
                }
                let s = to_screen(g.cam, g.zoom, n.pos);
                let r = (n.r * n.scale.max(0.2) * g.zoom).max(5.0) + 2.0;
                let d = (p - s).length();
                if d <= r && best.is_none_or(|(_, bd)| d < bd) {
                    best = Some((i, d));
                }
            }
            if best.is_none() {
                for (i, n) in g.nodes.iter().enumerate() {
                    if n.kind != Kind::Hub {
                        continue;
                    }
                    let s = to_screen(g.cam, g.zoom, n.pos);
                    let d = (p - s).length();
                    let r = n.reach * g.zoom;
                    if d <= r && best.is_none_or(|(_, bd)| d < bd) {
                        best = Some((i, d));
                    }
                }
            }
            best.map(|(i, _)| i)
        };
        // Knobs first: they sit outside their record, over whatever the
        // cloud has there, so a pointer on one must not read as the
        // neighbour under it.
        let knob_hit = |g: &GraphState, p: egui::Pos2| -> Option<(usize, Knob)> {
            let mut best: Option<((usize, Knob), f32)> = None;
            for (i, n) in g.nodes.iter().enumerate() {
                if n.kind != Kind::Release || n.threads < 0.6 {
                    continue;
                }
                for (c, k) in g.knob_slots(i, center) {
                    let d = (p - c).length();
                    if d <= THREAD_R + 3.0 && best.is_none_or(|(_, bd)| d < bd) {
                        best = Some(((i, k), d));
                    }
                }
            }
            best.map(|(h, _)| h)
        };
        let pointer_free = resp.hovered() && g.drag.is_none() && !resp.dragged();
        let knob_now = if pointer_free {
            pointer.and_then(|p| knob_hit(&g, p))
        } else {
            None
        };
        // The record whose knobs are out holds the hover for as long as the
        // pointer stays inside its halo: in a packed cloud the gap between a
        // cover and its knobs lies over other covers, and without this the
        // knobs would jump to whichever neighbour the pointer crossed.
        let held = if pointer_free {
            match (&g.shown, pointer) {
                (Some(k), Some(p)) => g
                    .index
                    .get(k)
                    .copied()
                    .filter(|&i| g.nodes[i].threads > 0.6 && g.in_halo(i, p, center)),
                _ => None,
            }
        } else {
            None
        };
        let hovered_now = if pointer_free {
            match knob_now {
                Some((i, _)) => Some(i),
                None => held.or_else(|| pointer.and_then(|p| hit(&g, p))),
            }
        } else {
            None
        };
        if hovered_now != g.hover {
            g.hover = hovered_now;
            g.wake();
        }
        if knob_now != g.hover_knob {
            g.hover_knob = knob_now;
            g.wake();
        }
        // Which record has its knobs out for the pointer: the one under it,
        // let go once the pointer has left its halo.
        match hovered_now.filter(|&i| g.nodes[i].kind == Kind::Release) {
            Some(i) => {
                if g.shown.as_deref() != Some(g.nodes[i].key.as_str()) {
                    g.shown = Some(g.nodes[i].key.clone());
                    g.wake();
                }
            }
            None => {
                let keep = match (&g.shown, pointer) {
                    (Some(k), Some(p)) if pointer_free => {
                        g.index.get(k).is_some_and(|&i| g.in_halo(i, p, center))
                    }
                    _ => false,
                };
                if !keep && g.shown.is_some() {
                    g.shown = None;
                    g.wake();
                }
            }
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
                // A released hub keeps a little of the pointer's motion and
                // drifts to rest, rather than flying off like a slingshot; a
                // released record just returns to its slot.
                let keep = if g.nodes[i].kind == Kind::Hub { 0.3 } else { 0.0 };
                g.nodes[i].vel *= keep;
                let speed = g.nodes[i].vel.length();
                if speed > MAX_SPEED * 0.5 {
                    g.nodes[i].vel *= MAX_SPEED * 0.5 / speed;
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
            if let Some((i, knob)) = g.hover_knob {
                if let Some(r) = &g.nodes[i].release {
                    match knob {
                        Knob::Thread(thread) => {
                            if self.thread_avail(r.release_id, thread) != Avail::Missing {
                                act = Some(GraphAct::Thread(r.clone(), thread));
                            }
                        }
                        Knob::Radio => act = Some(GraphAct::Radio(r.clone())),
                    }
                }
            } else if let Some(i) = pointer.and_then(|p| hit(&g, p)) {
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
            } else if !self.radio.on {
                // Empty canvas: the dig's record lets go of its threads. The
                // radio keeps its record lit; it's what's playing.
                g.focus = None;
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
            // Near critical damping: a pop-in or a hover grows to size with
            // one soft overshoot, not a wobble.
            let a = 260.0 * (target - n.scale) - 27.0 * n.scale_v;
            n.scale_v += a * sdt;
            n.scale = (n.scale + n.scale_v * sdt).clamp(0.0, 2.0);
            if (n.scale - target).abs() > 0.002 || n.scale_v.abs() > 0.02 {
                moving = true;
            } else {
                n.scale = target;
                n.scale_v = 0.0;
            }
        }
        // The thread nodes' bloom: out for the record under the pointer and
        // the one the dig stands on, folded away everywhere else.
        let shown_idx = g.shown.as_ref().and_then(|k| g.index.get(k).copied());
        let focus_idx = g.focus.as_ref().and_then(|k| g.index.get(k).copied());
        let zoom_now = g.zoom;
        for (i, n) in g.nodes.iter_mut().enumerate() {
            let big_enough = n.r * zoom_now >= HOVER_MIN_R;
            let target = if n.kind == Kind::Release
                && (Some(i) == focus_idx || (Some(i) == shown_idx && big_enough))
            {
                1.0
            } else {
                0.0
            };
            let a = 320.0 * (target - n.threads) - 30.0 * n.threads_v;
            n.threads_v += a * sdt;
            n.threads = (n.threads + n.threads_v * sdt).clamp(0.0, 1.2);
            if (n.threads - target).abs() > 0.003 || n.threads_v.abs() > 0.02 {
                moving = true;
            } else {
                n.threads = target;
                n.threads_v = 0.0;
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

        // Cloud areas go under everything: a soft, feathered wash in the
        // cloud's own tint, wide enough that its records rest inside it.
        // The feathering is a stack of faint discs, largest first, so the
        // edge fades out rather than stopping.
        for (i, n) in g.nodes.iter().enumerate() {
            if n.kind != Kind::Hub || n.scale <= 0.01 {
                continue;
            }
            let p = to_screen(cam, zoom, n.pos);
            let reach = n.reach * zoom;
            if !visible(p, reach) {
                continue;
            }
            let on = matches[i];
            let lit = Some(i) == g.hover || Some(i) == g.drag;
            let tint = cloud_tint(&n.name);
            let layers = 7;
            let base = if lit { 9.0 } else { 6.5 } * n.scale.clamp(0.0, 1.0);
            for k in 0..layers {
                let t = k as f32 / (layers - 1) as f32;
                let r = reach * (1.28 - 0.5 * t);
                let a = (base * (0.6 + 0.8 * t)) as u8;
                painter.circle_filled(p, r, dim(tint.gamma_multiply(a as f32 / 255.0), on));
            }
        }

        // Links between clouds.
        let link_w = (1.0 * zoom.sqrt()).clamp(0.5, 1.6);
        for e in &g.edges {
            if !e.soft {
                continue;
            }
            let (a, b) = (&g.nodes[e.a], &g.nodes[e.b]);
            let pa = to_screen(cam, zoom, a.pos);
            let pb = to_screen(cam, zoom, b.pos);
            if !visible(pa, 0.0) && !visible(pb, 0.0) {
                continue;
            }
            let on = matches[e.a] || matches[e.b];
            if e.soft {
                // Cross-ties are detail for a search or a hover: drawn
                // always, they scribble over the clouds they join.
                let hovered = Some(e.a) == g.hover || Some(e.b) == g.hover;
                if !(hovered || (searching && on)) {
                    continue;
                }
                painter.add(egui::Shape::dashed_line(
                    &[pa, pb],
                    egui::Stroke::new(link_w * 0.8, dim(color::LABEL_4.gamma_multiply(0.7), on)),
                    4.0 * zoom.max(0.5),
                    6.0 * zoom.max(0.5),
                ));
            }
            // Spokes aren't drawn: the area under a cloud is what says which
            // records belong to it.
        }

        // Dig trails: every thread taken, from the record it was taken out
        // of to the record it found, in the thread's colour. The dig web
        // feeds them in as it grows; the map keeps them once it's gone.
        if let Some(dig) = &self.dig {
            for s in &dig.steps {
                if let (Some(p), Some((t, m))) = (s.parent, s.via.as_ref()) {
                    let from = dig.steps[p].release_id;
                    let to = s.release_id;
                    if !g.trails.iter().any(|x| x.from == from && x.to == to) {
                        g.trails.push(Trail {
                            from,
                            to,
                            thread: *t,
                            via: m.clone(),
                        });
                    }
                }
            }
        }
        // A trail runs from the edge of one cover to the edge of the next
        // and ends in an arrowhead, so it reads as "this led to that" and
        // not just "these two are joined". Where there's room it carries the
        // name that was matched, so the map says how one record led to the
        // next without a trip to the sheet. The trail into the record on air
        // is lit: that's the step you're hearing.
        let on_air = self
            .radio
            .on
            .then(|| self.radio.now.as_ref().map(|n| n.release_id))
            .flatten();
        let trail_w = (1.8 * zoom.sqrt()).clamp(0.9, 2.4);
        let head_l = (4.0 + 3.0 * zoom.sqrt()).clamp(5.0, 9.0);
        let word_size = (10.5 * zoom.sqrt()).clamp(9.0, 12.0);
        for tr in &g.trails {
            let (Some(&a), Some(&b)) = (
                g.index.get(&format!("r:{}", tr.from)),
                g.index.get(&format!("r:{}", tr.to)),
            ) else {
                continue;
            };
            let ca = to_screen(cam, zoom, g.nodes[a].pos);
            let cb = to_screen(cam, zoom, g.nodes[b].pos);
            if !visible(ca, 0.0) && !visible(cb, 0.0) {
                continue;
            }
            let d = cb - ca;
            let len = d.length();
            if len < 1.0 {
                continue;
            }
            let dir = d / len;
            // Where the line leaves a square cover, along this direction.
            let exit = |i: usize| -> f32 {
                let r = g.nodes[i].r * g.nodes[i].scale * zoom;
                r / dir.x.abs().max(dir.y.abs()).max(0.01)
            };
            let pa = ca + dir * (exit(a) + 1.0);
            let pb = cb - dir * (exit(b) + 1.0);
            let run = (pb - pa).dot(dir);
            if run < head_l {
                // Covers touching: nothing to draw between them.
                continue;
            }
            let on = matches[a] || matches[b];
            let lit = Some(a) == g.hover || Some(b) == g.hover || on_air == Some(tr.to);
            let tint = dim(
                thread_tint(tr.thread).gamma_multiply(if lit { 0.95 } else { 0.55 }),
                on,
            );
            let w = if lit { trail_w + 0.6 } else { trail_w };
            painter.line_segment([pa, pb], egui::Stroke::new(w, tint));
            let perp = egui::vec2(-dir.y, dir.x);
            let hl = if lit { head_l + 1.5 } else { head_l };
            painter.add(egui::Shape::convex_polygon(
                vec![pb, pb - dir * hl + perp * hl * 0.55, pb - dir * hl - perp * hl * 0.55],
                tint,
                egui::Stroke::NONE,
            ));
            // The matched name, on the trail, when the trail is long enough
            // to carry it without the words swallowing the line.
            if run < 44.0 || tr.via.trim().is_empty() {
                continue;
            }
            let mut word = tr.via.trim().to_string();
            if word.chars().count() > 20 {
                word = word.chars().take(19).collect::<String>() + "…";
            }
            let ink = dim(if lit { color::LABEL } else { color::LABEL_2 }, on);
            let galley = painter.layout_no_wrap(word, egui::FontId::proportional(word_size), ink);
            let glyph_r = word_size * 0.38;
            let tw = galley.size().x + glyph_r * 2.0 + 4.0;
            if run - hl < tw + 18.0 {
                continue;
            }
            let mid = pa + dir * ((run - hl) * 0.5);
            let box_w = tw + 10.0;
            let box_h = galley.size().y + 4.0;
            let bg = egui::Rect::from_center_size(mid, egui::vec2(box_w, box_h));
            painter.rect_filled(
                bg,
                box_h * 0.5,
                dim(color::SURFACE.gamma_multiply(if lit { 0.96 } else { 0.86 }), on),
            );
            let gc = egui::pos2(bg.left() + 5.0 + glyph_r, mid.y);
            knob_glyph(
                &painter,
                Knob::Thread(tr.thread),
                gc,
                dim(thread_tint(tr.thread).gamma_multiply(if lit { 1.0 } else { 0.8 }), on),
                glyph_r,
            );
            painter.galley(
                egui::pos2(gc.x + glyph_r + 4.0, mid.y - galley.size().y * 0.5),
                galley,
                ink,
            );
        }

        // Hubs under, records over, the hovered node last so it sits on top.
        let mut order: Vec<usize> = (0..g.nodes.len()).collect();
        order.sort_by_key(|&i| {
            (
                match g.nodes[i].kind {
                    Kind::Sub => 0,
                    Kind::Hub => 1,
                    Kind::Release => 2,
                },
                (Some(i) == g.hover || Some(i) == g.drag) as u8,
            )
        });
        let mut cover_asks: Vec<usize> = Vec::new();
        // Hub names go on last, over the covers: a big cloud's records
        // would otherwise bury the one word that says what the cloud is.
        let mut hub_names: Vec<(egui::Pos2, f32, String, egui::Color32)> = Vec::new();
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
                Kind::Hub => {
                    // Nothing at the anchor itself; the area is the body.
                    // Names earn their place by size: a cloud that's a
                    // speck on screen stays unlabelled until you lean in,
                    // so the whole-map view isn't a carpet of type.
                    let area = n.reach * zoom;
                    if (area >= 26.0 || lit) && n.name != UNTAGGED {
                        hub_names.push((p, area, n.name.clone(), dim(color::LABEL, on)));
                    }
                }
                Kind::Sub => {
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
            // A title only when the cover is big enough on screen for the
            // words to belong to it alone, or on hover: a caption under
            // every cover of a packed cloud is a second cloud of type.
            if r >= 30.0 || lit {
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

        // Thread nodes, and the radio's light on the record it's playing.
        let radio_on = self.radio.on;
        let t_now = ctx.input(|i| i.time) as f32;
        for i in 0..g.nodes.len() {
            let n = &g.nodes[i];
            if n.kind != Kind::Release || n.threads <= 0.01 {
                continue;
            }
            let Some(rel) = &n.release else { continue };
            let p = to_screen(cam, zoom, n.pos);
            let r = n.r * n.scale * zoom;
            if !visible(p, r + 60.0) {
                continue;
            }
            let is_focus = g.focus.as_deref() == Some(n.key.as_str());
            if is_focus && radio_on {
                // A breathing ring: this is the record you're hearing.
                let breath = 0.5 + 0.5 * (t_now * 2.2).sin();
                let ring = egui::Rect::from_center_size(
                    p,
                    egui::Vec2::splat(r * 2.0 + 10.0 + 6.0 * breath),
                );
                painter.rect_stroke(
                    ring,
                    (r * 0.18).clamp(2.0, 6.0) + 5.0,
                    egui::Stroke::new(2.0, color::ACCENT.gamma_multiply(0.35 + 0.45 * breath)),
                );
                ctx.request_repaint();
            }
            let bloom = n.threads.min(1.0);
            for (c, knob) in g.knob_slots(i, center) {
                let (avail, busy) = match knob {
                    Knob::Thread(t) => (
                        self.thread_avail(rel.release_id, t),
                        g.awaiting.is_some_and(|a| a.is_for(rel.release_id, t)),
                    ),
                    Knob::Radio => (Avail::Ready, false),
                };
                // The radio's knob reads as switched on while it's playing
                // this very record.
                let on_air = knob == Knob::Radio && is_focus && radio_on;
                let lit = g.hover_knob == Some((i, knob)) || on_air;
                let tint = knob.tint();
                let tr = THREAD_R * bloom;
                if tr < 1.0 {
                    continue;
                }
                let dir = (c - p).normalized();
                let a = p + dir * (r + 1.0);
                let b = c - dir * tr;
                painter.line_segment(
                    [a, b],
                    egui::Stroke::new(
                        1.0,
                        if lit { tint } else { color::LABEL_4 }.gamma_multiply(bloom),
                    ),
                );
                painter.circle_filled(
                    c,
                    tr,
                    if on_air {
                        color::ACCENT_SOFT
                    } else if lit {
                        color::SURFACE_HOVER
                    } else {
                        color::SURFACE_HI
                    },
                );
                let edge = match (lit, avail) {
                    (true, _) => tint,
                    (_, Avail::Missing) => color::LABEL_4.gamma_multiply(0.6),
                    _ => color::LABEL_3,
                };
                painter.circle_stroke(c, tr, egui::Stroke::new(1.2, edge));
                let ink = match (lit, avail) {
                    (true, _) => color::LABEL,
                    (_, Avail::Missing) => color::LABEL_4,
                    _ => color::LABEL_2,
                };
                if tr >= 4.0 {
                    knob_glyph(&painter, knob, c, ink, tr * 0.5);
                }
                if busy {
                    // An arc chasing round the node while Discogs answers.
                    let a0 = t_now * 5.0;
                    let pts: Vec<egui::Pos2> = (0..=10)
                        .map(|k| {
                            let a = a0 + k as f32 / 10.0 * 1.7;
                            c + egui::vec2(a.cos(), a.sin()) * (tr + 3.0)
                        })
                        .collect();
                    painter.add(egui::Shape::line(pts, egui::Stroke::new(1.6, tint)));
                    ctx.request_repaint();
                }
            }
        }

        // The radio's walk: every record it has stood on wears its number
        // at the corner, so the order the records came in reads off the map
        // however the trails cross. The same numbers run along the row
        // under the radio bar.
        if !self.radio.walk.is_empty() {
            let mut stops: HashMap<u64, Vec<usize>> = HashMap::new();
            for (k, stop) in self.radio.walk.iter().enumerate() {
                stops.entry(stop.release_id).or_default().push(k + 1);
            }
            for (id, nums) in stops {
                let Some(&i) = g.index.get(&format!("r:{id}")) else {
                    continue;
                };
                let n = &g.nodes[i];
                let p = to_screen(cam, zoom, n.pos);
                let r = n.r * n.scale * zoom;
                if r < 4.0 || !visible(p, r + 20.0) {
                    continue;
                }
                let on = matches[i];
                let text = nums
                    .iter()
                    .map(|k| k.to_string())
                    .collect::<Vec<_>>()
                    .join("·");
                let live = self.radio.on && on_air == Some(id);
                let size = if r >= 14.0 { 10.5 } else { 9.0 };
                let galley = painter.layout_no_wrap(
                    text,
                    font::strong(size),
                    egui::Color32::WHITE,
                );
                let br = (size * 0.85).max(galley.size().x * 0.5 + 3.0);
                let c = egui::pos2(p.x - r, p.y - r);
                let pill = egui::Rect::from_center_size(
                    c,
                    egui::vec2(br * 2.0, size * 1.7),
                );
                painter.rect_filled(
                    pill,
                    pill.height() * 0.5,
                    dim(
                        if live { color::ACCENT_HOVER } else { color::ACCENT },
                        on,
                    ),
                );
                painter.rect_stroke(
                    pill,
                    pill.height() * 0.5,
                    egui::Stroke::new(1.0, dim(color::BG, on)),
                );
                painter.galley(
                    c - galley.size() * 0.5,
                    galley,
                    dim(egui::Color32::WHITE, on),
                );
            }
        }

        for (p, area, name, ink) in hub_names {
            let size = (13.0 * zoom.sqrt()).clamp(9.0, 16.0);
            let galley = painter.layout_no_wrap(name, font::strong(size), ink);
            // Over the top of the area, like a caption on a region of a map.
            let pos = egui::pos2(
                p.x - galley.size().x * 0.5,
                p.y - area - galley.size().y - 2.0,
            );
            // A soft shadow keeps the word legible over a busy cloud.
            painter.galley(
                pos + egui::vec2(0.0, 1.0),
                galley.clone(),
                egui::Color32::from_black_alpha(140),
            );
            painter.galley(pos, galley, ink);
        }

        // The hovered knob, or the hovered record, in words.
        if let Some((i, knob)) = g.hover_knob {
            if let Some(rel) = g.nodes[i].release.clone() {
                let words = match knob {
                    Knob::Thread(thread) => {
                        let avail = self.thread_avail(rel.release_id, thread);
                        let busy = g.awaiting.is_some_and(|a| a.is_for(rel.release_id, thread));
                        if avail != Avail::Missing {
                            ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        self.thread_words(&rel, thread, avail, busy)
                    }
                    Knob::Radio => {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                        let on_air = self.radio.on
                            && g.focus.as_deref() == Some(g.nodes[i].key.as_str());
                        if on_air {
                            "The radio is playing this record".to_string()
                        } else {
                            "Start the radio here: play this record, then dig on from it"
                                .to_string()
                        }
                    }
                };
                resp.clone().on_hover_note_at_pointer(|ui| {
                    ui.set_max_width(260.0);
                    ui.label(crate::ui::hover::note(words));
                });
            }
        } else if let Some(i) = g.hover {
            let small = g.nodes[i].r * g.zoom < HOVER_MIN_R;
            if let Some(rel) = g.nodes[i].release.clone() {
                ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                resp.clone().on_hover_note_at_pointer(|ui| {
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
                        Status::Dug => ("Discovered", color::ORANGE),
                    };
                    ui.label(egui::RichText::new(word).color(c).small());
                    if small {
                        ui.label(
                            egui::RichText::new("Zoom in to dig or start the radio from here")
                                .color(color::LABEL_3)
                                .small(),
                        );
                    }
                });
            } else if g.nodes[i].kind == Kind::Hub {
                let n = &g.nodes[i];
                let words = format!(
                    "{} · {} record{}",
                    n.name,
                    n.weight,
                    if n.weight == 1 { "" } else { "s" }
                );
                resp.clone().on_hover_note_at_pointer(|ui| {
                    ui.label(words);
                });
            }
        }

        // Legend and count, pinned to the canvas corner.
        {
            let (mut owned, mut wanted, mut dug, mut artists) = (0, 0, 0, 0);
            for n in &g.nodes {
                match (n.kind, n.release.as_ref().map(|r| r.status)) {
                    (Kind::Hub, _) => artists += 1,
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
            // The shelf marks, then the dug ring, each with its count.
            let caption = |x: &mut f32, text: String| {
                let galley = painter.layout_no_wrap(text, font::caption(), color::LABEL_3);
                let size = galley.size();
                painter.galley(egui::pos2(*x, y - size.y * 0.5), galley, color::LABEL_3);
                *x += size.x + 14.0;
            };
            for (list, n, c) in [
                (VinylList::Collection, owned, color::LABEL_3),
                (VinylList::Wantlist, wanted, color::ACCENT),
            ] {
                crate::ui::icon::shelf(&painter, egui::pos2(x + 5.5, y), 5.5, c, true, list);
                x += 16.0;
                caption(&mut x, n.to_string());
            }
            painter.rect_stroke(
                egui::Rect::from_center_size(egui::pos2(x + 5.0, y), egui::Vec2::splat(9.0)),
                2.0,
                egui::Stroke::new(2.0, color::ORANGE),
            );
            x += 15.0;
            caption(&mut x, format!("Dug {dug}"));
            // The trails' key, once there are trails to read.
            if !g.trails.is_empty() {
                for (t, word) in [
                    (DigThread::Artist, "artist"),
                    (DigThread::Label, "label"),
                    (DigThread::Style, "style"),
                ] {
                    painter.line_segment(
                        [egui::pos2(x, y), egui::pos2(x + 10.0, y)],
                        egui::Stroke::new(2.0, thread_tint(t)),
                    );
                    painter.add(egui::Shape::convex_polygon(
                        vec![
                            egui::pos2(x + 14.0, y),
                            egui::pos2(x + 9.0, y - 3.0),
                            egui::pos2(x + 9.0, y + 3.0),
                        ],
                        thread_tint(t),
                        egui::Stroke::NONE,
                    ));
                    x += 18.0;
                    caption(&mut x, word.to_string());
                }
            }
            let count = match g.arrange {
                Arrange::Artists => format!("{artists} artists"),
                Arrange::Genres => format!("{artists} styles"),
            };
            painter.text(
                egui::pos2(rect.right() - 14.0, y),
                egui::Align2::RIGHT_CENTER,
                count,
                font::caption(),
                color::LABEL_4,
            );
        }

        self.graph = g;
        self.draw_radio_bar(ui, rect, &ctx);
        act
    }

    /// Take `thread` out of `rel` from the map: stand the dig on the record
    /// and ask for the thread once its ids are in. The find lands on the map
    /// through the same path a strip step takes.
    pub(crate) fn map_take_thread(&mut self, rel: &Release, thread: DigThread) {
        self.map_stand_on(rel);
        self.graph.focus = Some(format!("r:{}", rel.release_id));
        self.graph.awaiting = Some(Await::Resolve {
            from: rel.release_id,
            thread,
        });
        self.graph.wake();
    }

    /// Make `rel` the dig's head: a shelf record digs as that record, a bare
    /// one as itself. A record already in the web is refocused, not re-dug.
    pub(crate) fn map_stand_on(&mut self, rel: &Release) {
        match rel.key {
            Some(key) => self.start_dig(key),
            None => self.start_dig_release(
                rel.release_id,
                rel.artist.clone(),
                rel.title.clone(),
                rel.label.clone(),
                rel.sub.clone(),
                rel.cover_url(),
            ),
        }
    }

    /// Carry a thread taken from the map through to its landing. Runs once
    /// a frame, after the map has synced, so a find is already a node when
    /// the camera is sent to it.
    fn map_dig_tick(&mut self, g: &mut GraphState) {
        let Some(aw) = g.awaiting else { return };
        let Some(dig) = self.dig.as_ref() else {
            g.awaiting = None;
            return;
        };
        match aw {
            Await::Resolve { from, thread } => {
                if dig.head().release_id != from {
                    g.awaiting = None;
                    return;
                }
                if dig.pending.is_some() {
                    return;
                }
                if dig.head().query(thread).is_some() {
                    self.dig_step(thread);
                    g.awaiting = Some(Await::Land { from, thread });
                } else if dig.head().detail_resolved {
                    self.status = match thread {
                        DigThread::Artist => "Discogs lists no artist for this record",
                        DigThread::Label => "Discogs lists no label for this record",
                        DigThread::Style => "Discogs lists no style for this record",
                        _ => "Nothing to follow out of this record",
                    }
                    .to_string();
                    g.awaiting = None;
                }
            }
            Await::Land { from, .. } => {
                if dig.pending.is_some() {
                    return;
                }
                if dig.head().release_id != from {
                    let key = format!("r:{}", dig.head().release_id);
                    g.focus = Some(key.clone());
                    // A map fitting itself grows to take the find in; one
                    // the user has framed pans to it and keeps the scale.
                    if !g.follow_fit {
                        g.lean = Some((key, false));
                    }
                    g.awaiting = None;
                    self.dig_evict();
                    self.dig_prime();
                } else {
                    if let Some(e) = &dig.error {
                        self.status = e.clone();
                    }
                    g.awaiting = None;
                }
            }
        }
    }

    /// What the map knows about `thread` out of `release_id`: only a dig
    /// that has stood on the record knows for sure.
    pub(crate) fn thread_avail(&self, release_id: u64, thread: DigThread) -> Avail {
        let step = self
            .dig
            .as_ref()
            .and_then(|d| d.steps.iter().find(|s| s.release_id == release_id));
        match step {
            Some(s) if s.query(thread).is_some() => Avail::Ready,
            Some(s) if s.detail_resolved => Avail::Missing,
            _ => Avail::Unknown,
        }
    }

    /// The hover line for a thread node.
    fn thread_words(&self, rel: &Release, thread: DigThread, avail: Avail, busy: bool) -> String {
        if busy {
            return "Searching Discogs…".to_string();
        }
        let artist = crate::dig::strip_disambiguator(&rel.artist).trim().to_string();
        match (thread, avail) {
            (DigThread::Artist, Avail::Missing) => {
                "Discogs lists no artist for this record".to_string()
            }
            (DigThread::Artist, _) if artist.is_empty() => {
                "Dig the artist: another record by this artist you don't own".to_string()
            }
            (DigThread::Artist, _) => {
                format!("Dig the artist: another {artist} record you don't own")
            }
            (DigThread::Label, Avail::Missing) => {
                "Discogs lists no label for this record".to_string()
            }
            (DigThread::Label, _) => match &rel.label {
                Some(l) => format!("Dig the label: another record on {l} you don't own"),
                None => "Dig the label: another record on this label you don't own".to_string(),
            },
            (DigThread::Style, Avail::Missing) => {
                "Discogs lists no style for this record".to_string()
            }
            (DigThread::Style, _) => {
                let step = self
                    .dig
                    .as_ref()
                    .and_then(|d| d.steps.iter().find(|s| s.release_id == rel.release_id));
                match step {
                    Some(s) if !s.styles.is_empty() => {
                        format!("Dig the style: {}", crate::dig::style_tip(&s.styles, true))
                    }
                    _ => "Dig the style: a record that sounds like this one, that you don't own"
                        .to_string(),
                }
            }
            // The map blooms only the three name threads; the sideways
            // ones are taken from the strip and the sheet.
            (_, _) => "Follow this thread to a record you don't own".to_string(),
        }
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
        // A landing is meant to be seen: if discovered records are hidden
        // when a dig lands on a new one, they come back into view rather
        // than the find vanishing into a hidden kind.
        let is_new = !self.dug.iter().any(|x| x.release_id == d.release_id);
        if is_new && !d.wanted && self.config.graph_hide.iter().any(|k| k == Status::Dug.key()) {
            self.config.graph_hide.retain(|k| k != Status::Dug.key());
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save settings: {e}");
            }
        }
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

    /// How many records the map would show of each kind with no filter,
    /// merged the way `sync` merges them: a shelf record a dig also landed
    /// on is its shelf's, and a dug record asked for on a list is wanted.
    pub(crate) fn map_status_counts(&self) -> [(Status, usize); 3] {
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut owned = 0;
        for v in &self.vinyl {
            if seen.insert(v.release_id) {
                owned += 1;
            }
        }
        let mut wanted = 0;
        for v in &self.wantlist {
            if seen.insert(v.release_id) {
                wanted += 1;
            }
        }
        let mut dug = 0;
        for d in &self.dug {
            if seen.insert(d.release_id) {
                if d.wanted {
                    wanted += 1;
                } else {
                    dug += 1;
                }
            }
        }
        [
            (Status::Owned, owned),
            (Status::Wanted, wanted),
            (Status::Dug, dug),
        ]
    }

    /// Forget every record that was only ever dug to, on the map and in the
    /// catalog. Wanted ones stay: they're on a list, not merely discovered.
    pub(crate) fn clear_dug(&mut self) {
        match Catalog::open(&self.db_path).and_then(|c| c.clear_dug_releases()) {
            Ok(n) => {
                self.dug.retain(|d| d.wanted);
                let keep: std::collections::HashSet<u64> =
                    self.dug.iter().map(|d| d.release_id).collect();
                self.dug_genres.retain(|id, _| keep.contains(id));
                self.graph.wake();
                self.status = format!(
                    "Forgot {n} discovered record{}",
                    if n == 1 { "" } else { "s" }
                );
            }
            Err(e) => self.status = format!("Couldn't clear discovered records: {e}"),
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
