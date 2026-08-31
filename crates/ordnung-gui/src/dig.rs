//! Crate digging: walking Discogs outward from one of your records.
//!
//! A dig starts at a record you own and moves right into records you *don't*.
//! At each step the user picks the thread to follow — the same artist, or the
//! same label — and the dig pulls a release from Discogs that matches it and
//! isn't already in the collection or the wantlist. The point is discovery, so
//! anything you've already saved is filtered out at every step; a dig that only
//! walked your own shelves would just be a shuffle.
//!
//! The path is kept whole rather than collapsed to "where we are now", so
//! stepping back and taking the other branch is a real move: the discarded
//! future is only dropped once a *different* choice is made from that point,
//! which is what makes back-and-forth digging feel like flipping through a
//! crate rather than resetting a search.
//!
//! Each step costs one Discogs search, paced by the shared client throttle, so
//! the fetch runs off the UI thread and the strip shows a spinner meanwhile.

use super::*;
use ordnung_core::discogs::{BrowsePage, BrowseRelease, BrowseThread};

/// Which thread a step followed to get to its record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DigThread {
    Artist,
    Label,
}

impl DigThread {
    fn label(self) -> &'static str {
        match self {
            DigThread::Artist => "artist",
            DigThread::Label => "label",
        }
    }

    fn browse(self) -> BrowseThread {
        match self {
            DigThread::Artist => BrowseThread::Artist,
            DigThread::Label => BrowseThread::Label,
        }
    }
}

/// One record on the dig path.
pub(crate) struct DigStep {
    /// Discogs release id. The identity of a step even for the first one, which
    /// is a record you own — a dig is a walk through Discogs either way.
    pub release_id: u64,
    /// Artist and title, split out of the `Artist - Title` string Discogs
    /// returns from search (the first step gets them from the local record).
    pub artist: String,
    pub title: String,
    /// Label name, for display only.
    pub label: Option<String>,
    /// Discogs artist ids for this release — what the artist thread actually
    /// browses. Empty until the release detail resolves (see
    /// [`App::dig_resolve_ids`]); the artist button waits on it.
    pub artist_ids: Vec<u64>,
    /// Discogs label ids, primary first. Same story as `artist_ids`.
    pub label_ids: Vec<u64>,
    /// Year and format, as the strip's caption line.
    pub sub: String,
    /// Cover thumbnail URL, downloaded lazily into [`App::dig_covers`].
    pub thumb_url: Option<String>,
    /// True when this record is the one the dig started from — the only step
    /// that's already in the user's collection.
    pub owned: bool,
    /// How this step was reached, and the value that was matched. `None` on the
    /// first step, which was chosen outright rather than dug to.
    pub via: Option<(DigThread, String)>,
    /// When this record landed on the path, so the strip can play it in rather
    /// than have it blink into place. Set once, on the push, and never
    /// refreshed: walking back over a card is navigation, not a new find, and
    /// re-animating there would make the path feel like it was being rebuilt.
    pub landed_at: std::time::Instant,
}

/// An in-progress dig: the records visited, and which one is on screen.
pub(crate) struct DigPath {
    pub steps: Vec<DigStep>,
    /// Index into `steps` of the record currently being looked at. Always a
    /// valid index — `steps` is never empty while a dig exists.
    pub at: usize,
    /// Release ids already visited on this dig, so a step never lands back on
    /// something earlier in the path.
    pub seen: HashSet<u64>,
    /// The *records* already visited, folded to artist + title (see
    /// [`work_key`]). A Discogs release id identifies one pressing, not one
    /// record, so `seen` alone lets the original, the repress and the German
    /// pressing of a record all land on the same path as if they were three
    /// finds. Digging is about hearing something new, so a record counts once
    /// however many times it was pressed.
    pub works: HashSet<String>,
    /// The thread currently being fetched, if a step is in flight.
    pub pending: Option<DigThread>,
    /// Why the last step couldn't move, shown in place of the buttons.
    pub error: Option<String>,
    /// Page counts learned from Discogs, keyed by the query that produced them,
    /// so repeated digs down one thread walk deeper instead of re-rolling the
    /// same page. Keyed by `(thread, folded name)`.
    pub pages: HashMap<(bool, u64), u32>,
    /// Browses fetched speculatively for the record currently on screen, so the
    /// click that takes a thread doesn't wait on the network. Keyed by
    /// `(release_id, artist?)` — the release is part of the key so a result that
    /// lands after the user has moved on is recognisably stale and dropped
    /// rather than applied to the wrong record.
    pub ready: HashMap<(u64, bool), BrowsePage>,
    /// The head the in-flight prefetch worker was started for, and which of the
    /// two threads it has yet to deliver. Empty set means nothing is in flight.
    /// Used to avoid starting a second worker for a head already being primed.
    pub priming: Option<(u64, Vec<DigThread>)>,
    /// When the dig started, driving the strip's entrance. The strip is a panel
    /// that shoves the whole shelf down when it appears, so it eases in rather
    /// than snapping the grid out from under the pointer.
    pub opened_at: std::time::Instant,
    /// Raised to tell the prefetch worker to stop between requests.
    ///
    /// Speculation shares one process-wide request pace with everything else,
    /// so a prefetch already in flight sits *ahead* of a click that arrives
    /// mid-fetch and the user waits out work they didn't ask for. The worker
    /// checks this before each request and abandons the rest, which hands the
    /// pace back to the click within one request rather than up to thirteen.
    pub cancel_prime: Arc<AtomicBool>,
}

impl DigPath {
    pub(crate) fn head(&self) -> &DigStep {
        &self.steps[self.at]
    }
}

/// A record the strip asked to open, carrying what the sheet needs for its
/// header — a dug record isn't in any list, so there's nothing to look it up in.
pub(crate) struct DigOpen {
    pub release_id: u64,
    pub artist: String,
    pub title: String,
    pub sub: String,
    pub cover_url: Option<String>,
}

/// One finished speculative browse. Unlike [`DigFetched`] this isn't applied
/// to the path — it's parked in [`DigPath::ready`] until the user actually
/// takes that thread, at which point the pick runs against membership as it is
/// *then*, not as it was when the browse landed.
pub(crate) struct DigPrimed {
    /// The release this browse was speculated from. A result whose head has
    /// changed is dropped: the two threads out of a record are only meaningful
    /// from that record.
    pub from: u64,
    pub thread: DigThread,
    pub entity: u64,
    pub result: std::result::Result<BrowsePage, String>,
}

/// One finished browse, handed back to the UI thread.
pub(crate) struct DigFetched {
    /// The release the dig was standing on when this was requested — a result
    /// for a step the user has since navigated away from is dropped.
    pub from: u64,
    pub thread: DigThread,
    /// The Discogs artist or label id that was browsed, to record the page
    /// count against.
    pub entity: u64,
    pub result: std::result::Result<BrowsePage, String>,
}

/// Motion for the strip's entrance: long enough to read as the crate being
/// pulled out, short enough that the first click never waits on it. Matches the
/// search popup's pacing so the two panels in the app open at the same speed.
const OPEN_ANIM: f32 = 0.18;

/// How far above its resting position the strip starts, in points. It pushes
/// the shelf down as it arrives, so it slides *out* of the toolbar edge rather
/// than appearing whole.
const OPEN_RISE: f32 = 10.0;

/// Motion for one newly dug record arriving on the path, and how far along the
/// strip it starts. A find is the payoff of the whole interaction, so its card
/// gets a longer, more deliberate entrance than the panel around it.
const CARD_ANIM: f32 = 0.34;
const CARD_SLIDE: f32 = 26.0;

/// How small a landing card starts, as a fraction of its settled size. Not zero
/// — a card that grows from nothing reads as a popup, where one that grows from
/// most of its size reads as a record being pushed into the row.
const CARD_MIN_SCALE: f32 = 0.72;

/// How far along `since` the card's entrance has run, eased. Shared by the
/// card's slide, its scale and its fade so the three can't drift apart.
fn card_enter_t(since: f32) -> f32 {
    egui::emath::easing::cubic_out((since / CARD_ANIM).clamp(0.0, 1.0))
}

/// A flag that is never raised, for the call sites that must not be cancelled.
static NEVER_CANCEL: AtomicBool = AtomicBool::new(false);

/// How many unknown-format rows one browse will resolve before giving up.
/// Each is a paced API request (~1.1s), so this bounds a step's worst case:
/// most pages have only a handful of master rows among the concrete releases.
const MAX_FORMAT_LOOKUPS: usize = 12;

/// Whether a Discogs format string describes a record. The artist and label
/// browse endpoints take no format filter, so they return CDs, cassettes and
/// MP3 files alongside the pressings — and a crate dig only wants the wax.
///
/// A blank format means a *master* row, which names no format at all. Those are
/// rejected: the alternative is showing the user a "record" that turns out to be
/// a CD comp, and an artist's masters are nearly all duplicated by concrete
/// release rows on the same pages anyway.
fn is_vinyl(format: &str) -> bool {
    let f = format.to_ascii_lowercase();
    if f.trim().is_empty() {
        return false;
    }
    // Reject the non-vinyl carriers first, so a "CD, Comp" never slips through
    // on the `lp` in a word like "sampler".
    if f.contains("cd")
        || f.contains("file")
        || f.contains("cassette")
        || f.contains("dvd")
        || f.contains("shellac")
    {
        return false;
    }
    f.contains("vinyl")
        || f.contains("lp")
        || f.contains("12\"")
        || f.contains("10\"")
        || f.contains("7\"")
}

/// The body of [`App::dig_roll`], as a free function so it can be used while a
/// mutable borrow of the dig is live.
fn dig_roll_with(seed: u64, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    let mut x = seed ^ 0x9E37_79B9_7F4A_7C15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    (x % len as u64) as usize
}

/// Drop Discogs's `(2)` disambiguator for *display*. It's never dropped for
/// matching — `Lawrence (2)` really is a different artist from `Lawrence`, and
/// treating them as one is how a dig ends up in the wrong discography — but the
/// number is a database artefact nobody wants to read in a caption.
pub(crate) fn strip_disambiguator(s: &str) -> &str {
    let t = s.trim_end();
    if !t.ends_with(')') {
        return s;
    }
    match t.rfind('(') {
        Some(open) if t[open + 1..t.len() - 1].chars().all(|c| c.is_ascii_digit()) => {
            t[..open].trim_end()
        }
        _ => s,
    }
}

/// Edition words that name a *pressing* rather than a record. A repress of a
/// record is the same music, so these are dropped before two titles are
/// compared — otherwise `Jackintosh EP` and `Jackintosh EP (Repress)` read as
/// two different finds.
const EDITION_WORDS: &[&str] = &[
    "repress",
    "reissue",
    "remaster",
    "remastered",
    "reedition",
    "re-edition",
    "reprint",
    "represse",
    "limited",
    "edition",
    "promo",
    "sampler",
    "white label",
    "test pressing",
];

/// Fold an artist and title down to the *record* they name, so two pressings of
/// one release collapse onto a single key.
///
/// Discogs gives every pressing its own release id, and a dig that dedups on
/// the id alone will happily walk `Jackintosh EP` → `Jackintosh Ep` → the 2015
/// repress and call each one a new record. What varies between pressings is
/// case, punctuation, bracketed edition notes and the `EP`/`LP` suffix; what
/// stays is the artist and the words of the title. Fold away the former.
///
/// Deliberately not applied to the *artist* ids — `strip_disambiguator` handles
/// display, and two genuinely different artists with the same release title
/// (covers, split names) still fold apart because the artist is in the key.
fn work_key(artist: &str, title: &str) -> String {
    fn fold(s: &str) -> String {
        let lower = s.to_lowercase();
        // Drop bracketed asides wholesale: they're where Discogs parks the
        // catalogue number, the edition and the disambiguator.
        let mut out = String::with_capacity(lower.len());
        let mut depth = 0i32;
        for c in lower.chars() {
            match c {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => depth = (depth - 1).max(0),
                _ if depth == 0 => out.push(c),
                _ => {}
            }
        }
        // Collapse intra-word punctuation before splitting, so `E.P.` reads as
        // the one word `ep` rather than the two letters `e` and `p` — the
        // dotted spelling is a pressing's typography, not a different record.
        let out: String = out
            .chars()
            .map(|c| {
                if matches!(c, '.' | '\'' | '\u{2019}') {
                    '\0'
                } else {
                    c
                }
            })
            .filter(|c| *c != '\0')
            .collect();
        // Then keep only the alphanumeric words, minus the ones that describe a
        // pressing rather than a record.
        out.split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .filter(|w| !EDITION_WORDS.contains(w))
            // `EP`/`LP`/`12` are format noise appended inconsistently across
            // pressings of one record; the rest of the title still identifies it.
            .filter(|w| !matches!(*w, "ep" | "lp" | "12" | "10" | "7" | "vol"))
            .collect::<Vec<_>>()
            .join(" ")
    }
    let (a, t) = (fold(artist), fold(title));
    // A title that folds to nothing (a self-titled `EP`, a numbered white
    // label) would collapse every such record onto one key and wall the dig off
    // from all of them. Fall back to the unfolded title, which at least keeps
    // distinct records distinct.
    let t = if t.is_empty() {
        title.trim().to_lowercase()
    } else {
        t
    };
    format!("{a}\u{1}{t}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pressings of one record must fold together; different records must not.
    #[test]
    fn work_key_folds_pressings_not_records() {
        let same = |a: &str, t: &str, b: &str, u: &str| {
            assert_eq!(work_key(a, t), work_key(b, u), "{t:?} vs {u:?}");
        };
        let differ = |a: &str, t: &str, b: &str, u: &str| {
            assert_ne!(work_key(a, t), work_key(b, u), "{t:?} vs {u:?}");
        };
        // The case from the screenshot: one record, two Discogs releases.
        same("XDB", "Jackintosh EP", "XDB", "Jackintosh Ep");
        // Edition notes name a pressing, not a record.
        same("XDB", "Jackintosh EP", "XDB", "Jackintosh EP (Repress)");
        same("XDB", "Jackintosh EP", "XDB", "Jackintosh [2015 Reissue]");
        same("XDB", "Jackintosh EP", "XDB", "Jackintosh  LP");
        // Punctuation and spacing vary between pressings.
        same("XDB", "Cagomi E.P.", "XDB", "Cagomi EP");
        // But two actual records by one artist stay apart.
        differ("XDB", "Jackintosh EP", "XDB", "Cagomi EP");
        // And one title by two artists stays apart.
        differ("XDB", "Descap", "Losoul", "Descap");
    }

    /// A newly landed card must start hidden and finish settled, and never
    /// run backwards in between — the strip reads `enter < 1.0` to decide
    /// whether to keep requesting frames, so a value that never reaches 1.0
    /// would repaint the app forever.
    #[test]
    fn card_enter_starts_hidden_and_settles() {
        assert_eq!(card_enter_t(0.0), 0.0);
        assert_eq!(card_enter_t(CARD_ANIM), 1.0);
        assert_eq!(card_enter_t(CARD_ANIM * 10.0), 1.0);
        let mut prev = 0.0;
        for i in 0..=20 {
            let t = card_enter_t(CARD_ANIM * i as f32 / 20.0);
            assert!(t >= prev, "entrance went backwards at step {i}");
            assert!((0.0..=1.0).contains(&t), "entrance left 0..=1 at step {i}");
            prev = t;
        }
    }

    /// A title made only of folded-away words must not collapse every such
    /// record onto one key — that would wall the dig off from all of them.
    #[test]
    fn work_key_keeps_degenerate_titles_apart() {
        assert_ne!(work_key("XDB", "EP"), work_key("XDB", "LP"));
        assert_ne!(work_key("XDB", "Vol. 1"), work_key("XDB", "Vol. 2"));
    }
}

/// The artist and title of a browse row, however this endpoint chose to pack
/// them. The label endpoint puts `Artist - Title` in `title`; the artist
/// endpoint splits them already. Same rule the pick itself uses, factored out
/// so the dedup folds exactly what the step will end up displaying.
fn row_artist_title(r: &BrowseRelease) -> (String, String) {
    if r.artist.trim().is_empty() {
        split_title(&r.title)
    } else {
        (r.artist.clone(), r.title.clone())
    }
}

/// Split the `Artist - Title` string Discogs search returns. Titles legitimately
/// contain " - " (`Artist - A - B`), so only the *first* separator splits, which
/// is the one Discogs itself inserted.
fn split_title(combined: &str) -> (String, String) {
    match combined.split_once(" - ") {
        Some((a, t)) => (a.trim().to_string(), t.trim().to_string()),
        None => (String::new(), combined.trim().to_string()),
    }
}

/// One browse down `thread`, with the unknown-format rows on the page resolved.
///
/// Shared by the explicit step and the speculative prefetch so both pages are
/// judged the same way — a prefetched page has to be *pickable* the moment it's
/// wanted, and the format resolution is what makes a row pickable.
///
/// `cancel`, when raised, stops the format resolution early. The page is still
/// returned — the rows resolved so far are perfectly good, and the unresolved
/// ones stay `format_known: false`, which the pick already treats as "unknown,
/// keep as a candidate" rather than "not vinyl". Only speculation passes a flag
/// that ever rises; an explicit step passes one that never does.
fn browse_step(
    client: &discogs::Client,
    thread: DigThread,
    entity: u64,
    page: u32,
    skip: &HashSet<u64>,
    cancel: &AtomicBool,
) -> std::result::Result<BrowsePage, String> {
    client
        .browse_by_id(thread.browse(), entity, page)
        .map(|mut p| {
            // Master rows carry no format, so "is this a record?" can't be
            // answered from the listing alone. Resolve a bounded number of them
            // here, on this worker, rather than handing the UI thread rows it
            // can't judge — each costs one paced request, so only the rows that
            // could still be picked are worth resolving.
            let mut budget = MAX_FORMAT_LOOKUPS;
            for r in p.releases.iter_mut() {
                if r.format_known || budget == 0 {
                    continue;
                }
                // Each lookup is another paced request. If a click is waiting,
                // stop refining and give the pace back — a page with some rows
                // still unresolved is usable, just slightly less pre-judged.
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if skip.contains(&r.release_id) {
                    continue;
                }
                budget -= 1;
                if let Ok(f) = client.release_format(r.release_id) {
                    r.format = f;
                }
                // Judged now either way: a lookup that failed leaves an empty
                // format, which `is_vinyl` rejects.
                r.format_known = true;
            }
            p
        })
        .map_err(|e| e.to_string())
}

impl App {
    /// Begin a dig at `key`, replacing any dig already running. Silently does
    /// nothing if the record vanished from the lists under the click.
    ///
    /// Re-digging a record that's already somewhere on the current path moves
    /// the cursor there instead of starting over: the dig buttons stay visible
    /// on every cover, so hitting one for a record you dug to earlier reads as
    /// "go back to it", not "throw the path away".
    pub(crate) fn start_dig(&mut self, key: VinylCoverKey) {
        let Some(record) = self.vinyl_record(key) else {
            return;
        };
        if let Some(dig) = self.dig.as_mut() {
            if let Some(i) = dig
                .steps
                .iter()
                .position(|s| s.release_id == record.release_id)
            {
                dig.at = i;
                return;
            }
        }
        let sub = match (record.year, record.format.as_deref()) {
            (Some(y), Some(f)) => format!("{y} · {f}"),
            (Some(y), None) => y.to_string(),
            (None, Some(f)) => f.to_string(),
            (None, None) => String::new(),
        };
        let mut seen = HashSet::new();
        seen.insert(record.release_id);
        let mut works = HashSet::new();
        works.insert(work_key(&record.artist, &record.title));
        self.dig = Some(DigPath {
            steps: vec![DigStep {
                release_id: record.release_id,
                artist: record.artist.clone(),
                title: record.title.clone(),
                label: record.label.clone().filter(|l| !l.trim().is_empty()),
                artist_ids: Vec::new(),
                label_ids: Vec::new(),
                sub,
                // The starting record's cover is already cached locally, so the
                // strip reads it from `vinyl_covers` by key rather than the URL
                // cache the dug steps use.
                thumb_url: None,
                owned: true,
                via: None,
                landed_at: std::time::Instant::now(),
            }],
            at: 0,
            seen,
            works,
            pending: None,
            error: None,
            pages: HashMap::new(),
            ready: HashMap::new(),
            priming: None,
            opened_at: std::time::Instant::now(),
            cancel_prime: Arc::new(AtomicBool::new(false)),
        });
        // The local cover cache is keyed by list + instance id, so make sure the
        // starting record's cover is loaded even if the grid hasn't drawn it.
        self.request_vinyl_cover(key);
        self.dig_start_keys.insert(record.release_id, key);
        // Both branches need this record's Discogs ids before they can be taken.
        self.dig_resolve_ids(record.release_id);
    }

    /// Take `thread` out of the record on screen.
    ///
    /// If the speculative prefetch has already fetched this thread's page, the
    /// step is taken from it immediately and no request is made at all — that
    /// page was fetched for exactly this click, and the pick still runs against
    /// collection membership as it is *now*, so a stale page can't offer a
    /// record you've bought in the meantime.
    pub(crate) fn dig_step(&mut self, thread: DigThread) {
        if let Some(dig) = self.dig.as_ref() {
            let key = (dig.head().release_id, thread == DigThread::Artist);
            if dig.ready.contains_key(&key) {
                let entity = match thread {
                    DigThread::Artist => dig.head().artist_ids.first().copied(),
                    DigThread::Label => dig.head().label_ids.first().copied(),
                };
                if let Some(entity) = entity {
                    let page = self
                        .dig
                        .as_mut()
                        .expect("checked above")
                        .ready
                        .remove(&key)
                        .expect("checked above");
                    self.apply_page(thread, entity, page);
                    return;
                }
            }
        }
        self.dig_fetch_step(thread);
    }

    /// Ask Discogs for the next record down `thread`. One search request, off
    /// the UI thread; the reply is adopted by [`App::poll_dig`].
    fn dig_fetch_step(&mut self, thread: DigThread) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            // No request goes out, so nothing will land to move an open sheet
            // onto — it stays where it is.
            self.sheet_follows_dig = false;
            if let Some(dig) = self.dig.as_mut() {
                dig.error = Some(
                    "No Discogs token set. Add one in Settings to dig for new records.".to_string(),
                );
            }
            return;
        }
        let Some(dig) = self.dig.as_ref() else { return };
        let head = dig.head();
        // Browse by Discogs id, never by name: an `artist=Lawrence` *search*
        // returns four unrelated Lawrences plus Steve Lawrence, and
        // `label=Dial` returns the salsa label "Dial Record". An id names one
        // entity, so a dig down the artist thread stays with that artist.
        let Some(entity) = (match thread {
            DigThread::Artist => head.artist_ids.first().copied(),
            DigThread::Label => head.label_ids.first().copied(),
        }) else {
            self.sheet_follows_dig = false;
            return;
        };
        let from = head.release_id;
        // Walk deeper on a thread already dug: page 1 is the famous pressings,
        // and re-reading it would keep offering the same records. Once Discogs
        // has told us how many pages exist, roll inside that range.
        let known = dig.pages.get(&(thread == DigThread::Artist, entity));
        let page = match known {
            Some(&n) if n > 1 => 1 + self.dig_roll(n.min(20) as usize) as u32,
            _ => 1,
        };
        if let Some(dig) = self.dig.as_mut() {
            // The click owns the request pace from here. Any speculation still
            // running is work the user has now overtaken, so stand it down
            // rather than making them queue behind it.
            dig.cancel_prime.store(true, Ordering::Relaxed);
            dig.priming = None;
            // Drop the discarded future *now*, not when the answer lands.
            // Taking a branch is the decision; the request is only how it gets
            // filled in. Leaving the old path drawn until the fetch returns
            // makes the spinner look like a fifth step continuing the walk
            // rather than the second step of a new branch. `seen` is untouched,
            // so the abandoned records still don't come round again.
            dig.steps.truncate(dig.at + 1);
            dig.pending = Some(thread);
            dig.error = None;
        }
        // Releases the pick will reject anyway — no point spending a paced
        // request resolving the format of a record you already have.
        let mut skip = self.vinyl_owned.clone();
        skip.extend(self.vinyl_wanted.iter().copied());
        if let Some(dig) = self.dig.as_ref() {
            skip.extend(dig.seen.iter().copied());
        }
        let (tx, rx) = mpsc::channel();
        self.dig_rx = Some(rx);
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            // An explicit step is never stood down: it's the request the user
            // is waiting on, so it runs to completion.
            let result = browse_step(&client, thread, entity, page, &skip, &NEVER_CANCEL);
            let _ = tx.send(DigFetched {
                from,
                thread,
                entity,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished browse: pick a release the user doesn't already have and
    /// append it to the path.
    pub(crate) fn poll_dig(&mut self) {
        let Some(rx) = &self.dig_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.dig_rx = None;
        let mut stalled = false;
        {
            let Some(dig) = self.dig.as_mut() else { return };
            dig.pending = None;
            // The user moved somewhere else while this was in flight — the
            // answer is about a record they're no longer standing on.
            if dig.head().release_id != msg.from {
                stalled = true;
            } else if let Err(e) = &msg.result {
                dig.error = Some(e.clone());
                stalled = true;
            }
        }
        // Nothing landed, so an open sheet stops waiting for a record to move
        // to and stays on the one it's showing.
        if stalled {
            self.sheet_follows_dig = false;
            return;
        }
        let page = msg.result.expect("error returned above");
        self.apply_page(msg.thread, msg.entity, page);
    }

    /// Pick a fresh release out of `page` and append it to the path.
    ///
    /// Split out of [`App::poll_dig`] so a page that arrived speculatively can
    /// be spent through exactly the same filter as one fetched on demand — the
    /// membership snapshots are read here, at pick time, not at fetch time.
    fn apply_page(&mut self, thread: DigThread, entity: u64, page: BrowsePage) {
        // `dig_roll` reads `self`, so roll before taking the mutable borrow below
        // rather than in the middle of it. The value doesn't depend on the
        // result, only on how many fresh candidates it turns out to hold.
        let roll = self.dig_seed;
        // Membership snapshots, read before the mutable borrow: these are what
        // make a dig a discovery tool rather than a shuffle of what you have.
        let owned = self.vinyl_owned.clone();
        let wanted = self.vinyl_wanted.clone();
        // Taken here, before the borrow below: whether this page produces a
        // find or an error, the sheet's ride on the dig ends with this step. It
        // is spent at the bottom, on the branch that actually lands somewhere.
        let follow = std::mem::take(&mut self.sheet_follows_dig);
        let Some(dig) = self.dig.as_mut() else { return };
        dig.pending = None;
        dig.error = None;
        let msg_thread = thread;
        dig.pages
            .insert((thread == DigThread::Artist, entity), page.pages);

        // The whole point: drop anything already in the collection or wantlist,
        // and anything this dig has already passed through. Browsing by id has
        // already guaranteed the right artist/label, so what's left is what a
        // *record* dig wants — vinyl, and no repeats.
        let mut picked_ids = HashSet::new();
        let mut picked_works = HashSet::new();
        let candidates: Vec<&BrowseRelease> = page
            .releases
            .iter()
            .filter(|r| {
                let id = r.release_id;
                if dig.seen.contains(&id) || owned.contains(&id) || wanted.contains(&id) {
                    return false;
                }
                // Discogs lists some pressings twice on the same page.
                if !picked_ids.insert(id) {
                    return false;
                }
                // And lists the same *record* many times over as separate
                // pressings — the original, the repress, each country's
                // edition. All one find, so the first one on the page stands
                // and the rest are dropped, here and against the path so far.
                let (a, t) = row_artist_title(r);
                let work = work_key(&a, &t);
                if dig.works.contains(&work) || !picked_works.insert(work) {
                    return false;
                }
                // A row with no format of its own (every "master" entry) is
                // *unknown*, not "not vinyl" — Voyager 8's only undug record is
                // a master row, and discarding those reports "nothing new"
                // while a real 12" sits in the results. Keep it as a candidate;
                // the format is resolved once, on the one that gets picked.
                !r.format_known || is_vinyl(&r.format)
            })
            .collect();
        // Prefer the artist's own records over ones they only remixed on — but
        // fall back to the remixes rather than dead-ending, because whole pages
        // of a prolific artist's listing are nothing but remix credits.
        let mut fresh: Vec<&BrowseRelease> =
            candidates.iter().copied().filter(|r| r.main).collect();
        if fresh.is_empty() {
            fresh = candidates;
        }
        if fresh.is_empty() {
            // Either everything on this page is already yours, or the page held
            // only loose matches the exactness check rejected. Both are "try
            // again" — the next roll lands on a different page.
            dig.error = Some(format!(
                "Nothing new on that page for this {}. Try again, or take the \
                 other thread.",
                msg_thread.label()
            ));
            return;
        }
        let pick = fresh[dig_roll_with(roll, fresh.len())];
        let release_id = pick.release_id;
        let (artist, title) = row_artist_title(pick);
        let sub = [
            pick.year.filter(|y| *y > 0).map(|y| y.to_string()),
            (!pick.format.trim().is_empty()).then(|| pick.format.clone()),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ");
        // What to show on the connector. A label browse knows the label it
        // browsed even when the row's own `label` field is blank, which it often
        // is — the head's label name is the one that was followed.
        let matched = match msg_thread {
            DigThread::Artist => strip_disambiguator(&artist).to_string(),
            DigThread::Label => {
                if pick.label.trim().is_empty() {
                    dig.head().label.clone().unwrap_or_default()
                } else {
                    pick.label.clone()
                }
            }
        };
        let step = DigStep {
            release_id,
            artist,
            title,
            // Blank on most browse rows; the release detail fetched next
            // carries the real one, alongside the ids.
            label: (!pick.label.trim().is_empty()).then(|| pick.label.clone()),
            // Ids aren't in a browse row — they come from the release detail,
            // fetched next so this step's own branches are ready to take.
            artist_ids: Vec::new(),
            label_ids: Vec::new(),
            sub,
            thumb_url: (!pick.thumb_url.trim().is_empty()).then(|| pick.thumb_url.clone()),
            owned: false,
            via: Some((msg_thread, matched)),
            landed_at: std::time::Instant::now(),
        };
        // Choosing from here makes this the new future.
        dig.steps.truncate(dig.at + 1);
        dig.seen.insert(release_id);
        dig.works.insert(work_key(&step.artist, &step.title));
        // What an open sheet riding the dig needs to re-point at this record,
        // taken before the step is moved into the path.
        let landed = follow.then(|| {
            (
                step.artist.clone(),
                step.title.clone(),
                step.sub.clone(),
                step.thumb_url.clone(),
            )
        });
        dig.steps.push(step);
        dig.at = dig.steps.len() - 1;
        // The new step can't be dug from until we know its artist/label ids.
        self.dig_resolve_ids(release_id);
        // A thread taken from the open sheet's own branch buttons: the window
        // stayed up through the fetch, so it now shows the record the dig
        // walked to. Same call the strip makes when a card is clicked, so the
        // sheet arrives in exactly the state it would have opened in.
        if let Some((artist, title, sub, cover_url)) = landed {
            if self.vinyl_sheet.is_some() {
                let ctx = self.egui_ctx.clone();
                self.open_release_sheet(release_id, artist, title, sub, cover_url, &ctx);
            }
        }
    }

    /// Fetch the artist and label ids for a release the dig just landed on, so
    /// its own two branches can be taken. Cache-first, like the sheet's own
    /// tracklist fetch — a record opened before answers without a request.
    fn dig_resolve_ids(&mut self, release_id: u64) {
        let (tx, rx) = mpsc::channel();
        self.dig_ids_rx = Some(rx);
        let db = self.db_path.clone();
        let token = self.discogs_token();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let id = release_id.to_string();
            let detail = Catalog::open(&db).ok().and_then(|cat| {
                if let Ok(Some(d)) = cat.cached_release(&id) {
                    return Some(d);
                }
                if token.trim().is_empty() {
                    return None;
                }
                let client =
                    discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
                cat.release_cached_or(&id, || client.fetch_release(&id))
                    .ok()
            });
            let _ = tx.send((
                release_id,
                detail
                    .map(|d| (d.artist_ids, d.label_ids, d.label))
                    .unwrap_or_default(),
            ));
            ctx.request_repaint();
        });
    }

    /// Speculatively browse both threads out of the record on screen, so the
    /// next click lands instantly instead of waiting on Discogs.
    ///
    /// Called every frame, but does work only when the head has both its ids and
    /// a thread that is neither already cached nor already in flight — so
    /// arriving at a record primes it once, and taking one of its two branches
    /// re-primes from wherever that landed.
    ///
    /// The two browses run on one worker, sharing one [`discogs::Client`] and
    /// therefore one throttle clock. Firing them as separate threads would give
    /// each its own pacing and race the rate limit; sequential on one worker,
    /// the second simply follows the first a request-interval later, which is
    /// still well ahead of the user reading the cover that just appeared.
    fn dig_prime(&mut self) {
        let token = self.discogs_token();
        if token.trim().is_empty() {
            return;
        }
        let Some(dig) = self.dig.as_ref() else { return };
        // Never speculate over an explicit step: that request is the one the
        // user is waiting on, and a prefetch queued behind it on the throttle
        // would delay it.
        if dig.pending.is_some() {
            return;
        }
        let head = dig.head();
        let from = head.release_id;
        // Nothing to prime while this head is still resolving its own ids; the
        // next frame after they land will catch it.
        let mut want: Vec<(DigThread, u64)> = Vec::new();
        for (thread, entity) in [
            (DigThread::Artist, head.artist_ids.first().copied()),
            (DigThread::Label, head.label_ids.first().copied()),
        ] {
            let Some(entity) = entity else { continue };
            if dig.ready.contains_key(&(from, thread == DigThread::Artist)) {
                continue;
            }
            want.push((thread, entity));
        }
        if want.is_empty() {
            return;
        }
        // A worker is already priming this same head — let it finish rather than
        // starting a second one that would double the requests for one record.
        if let Some((primed_from, _)) = &dig.priming {
            if *primed_from == from {
                return;
            }
        }
        // Pick the pages the same way an explicit step would, so a prefetched
        // browse walks just as deep into a thread already dug.
        let jobs: Vec<(DigThread, u64, u32)> = want
            .iter()
            .enumerate()
            .map(|(i, &(thread, entity))| {
                let page = match dig.pages.get(&(thread == DigThread::Artist, entity)) {
                    // Offset the roll per job so the two threads of one record
                    // don't both land on the same page index.
                    Some(&n) if n > 1 => {
                        1 + dig_roll_with(
                            self.dig_seed.wrapping_add(i as u64 * 0x9E37_79B9),
                            n.min(20) as usize,
                        ) as u32
                    }
                    _ => 1,
                };
                (thread, entity, page)
            })
            .collect();
        let mut skip = self.vinyl_owned.clone();
        skip.extend(self.vinyl_wanted.iter().copied());
        skip.extend(dig.seen.iter().copied());

        // A fresh flag per worker: the previous one may already be raised by
        // the click that stood its worker down, and reusing it would cancel
        // this speculation before it made a single request.
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let dig = self.dig.as_mut().expect("checked above");
            dig.priming = Some((from, jobs.iter().map(|j| j.0).collect()));
            dig.cancel_prime = cancel.clone();
        }
        let tx = self.dig_prime_tx.clone();
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            for (thread, entity, page) in jobs {
                // Checked before each browse rather than only at the top: the
                // artist thread's page and the label thread's page are two
                // separate trips through the shared pace, and the click that
                // cancels usually arrives during the first.
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let result = browse_step(&client, thread, entity, page, &skip, &cancel);
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                if tx
                    .send(DigPrimed {
                        from,
                        thread,
                        entity,
                        result,
                    })
                    .is_err()
                {
                    return;
                }
                ctx.request_repaint();
            }
        });
    }

    /// Park finished speculative browses against the record they were fetched
    /// from, dropping any whose record is no longer the one on screen.
    pub(crate) fn poll_dig_primed(&mut self) {
        while let Ok(msg) = self.dig_prime_rx.try_recv() {
            let Some(dig) = self.dig.as_mut() else {
                continue;
            };
            if let Some((from, outstanding)) = dig.priming.as_mut() {
                if *from == msg.from {
                    outstanding.retain(|t| *t != msg.thread);
                    if outstanding.is_empty() {
                        dig.priming = None;
                    }
                }
            }
            // Stale: the user took a branch or stepped back while this was in
            // flight, so this page describes a record they've left. Dropping it
            // is the whole invalidation rule — a cached page only ever belongs
            // to the record it was speculated from.
            if dig.head().release_id != msg.from {
                continue;
            }
            let Ok(page) = msg.result else {
                // A failed speculation says nothing: leave the cache empty so
                // the explicit click retries for real and reports its own error.
                continue;
            };
            // Learn the page count even from a speculation, so the next roll
            // down this thread — prefetched or clicked — can walk deeper.
            dig.pages
                .insert((msg.thread == DigThread::Artist, msg.entity), page.pages);
            dig.ready
                .insert((msg.from, msg.thread == DigThread::Artist), page);
        }
    }

    /// Adopt resolved artist/label ids onto the step they belong to.
    pub(crate) fn poll_dig_ids(&mut self) {
        let Some(rx) = &self.dig_ids_rx else { return };
        let Ok((release_id, (artist_ids, label_ids, label))) = rx.try_recv() else {
            return;
        };
        self.dig_ids_rx = None;
        let Some(dig) = self.dig.as_mut() else { return };
        if let Some(step) = dig.steps.iter_mut().find(|s| s.release_id == release_id) {
            step.artist_ids = artist_ids;
            step.label_ids = label_ids;
            // Browse rows usually omit the label name; the detail has it.
            if step.label.is_none() {
                step.label = label.filter(|l| !l.trim().is_empty());
            }
        }
    }

    /// A cheap varying index in `0..len`. Mixes a seed advanced every frame the
    /// strip draws, so taking the same branch twice lands somewhere else.
    fn dig_roll(&self, len: usize) -> usize {
        dig_roll_with(self.dig_seed, len)
    }

    /// The decoded cover for a dug record's thumbnail URL, if it has arrived.
    /// Kicks off a download on first ask, deduplicated by URL — the strip and
    /// the sheet request the same cover and share the one fetch.
    pub(crate) fn dig_cover(&mut self, url: &str) -> Option<&Tex> {
        if !self.dig_covers.contains_key(url) {
            self.dig_covers.insert(url.to_string(), ThumbState::Loading);
            let (tx, url_owned) = (self.dig_cover_tx.clone(), url.to_string());
            let ctx = self.egui_ctx.clone();
            let token = self.discogs_token();
            thread::spawn(move || {
                let client =
                    discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
                // Image CDN downloads don't count against the API rate limit,
                // so a cover fetch never delays the next dig step.
                let img = client.fetch_thumb(&url_owned).and_then(|png| {
                    let d = image::load_from_memory(&png).ok()?;
                    let rgba = d.to_rgba8();
                    let size = [rgba.width() as usize, rgba.height() as usize];
                    Some(egui::ColorImage::from_rgba_unmultiplied(
                        size,
                        &rgba.into_raw(),
                    ))
                });
                let _ = tx.send((url_owned, img));
                ctx.request_repaint();
            });
            return None;
        }
        match self.dig_covers.get(url) {
            Some(ThumbState::Ready(t)) => t.as_ref(),
            _ => None,
        }
    }

    /// Drain finished dig-cover downloads into textures. Called each frame
    /// alongside the other cover polls.
    pub(crate) fn poll_dig_covers(&mut self, ctx: &egui::Context) {
        while let Ok((url, img)) = self.dig_cover_rx.try_recv() {
            let tex = img.map(|img| {
                self.tex_graveyard.wrap(ctx.load_texture(
                    "dig-cover",
                    img,
                    egui::TextureOptions::LINEAR,
                ))
            });
            self.dig_covers.insert(url, ThumbState::Ready(tex));
        }
    }

    /// Draw the dig strip above the grid. Returns the record whose sheet should
    /// be opened, if the user clicked one — applied by the caller so the strip
    /// doesn't mutate `self` mid-render.
    pub(crate) fn draw_dig(&mut self, ui: &mut egui::Ui) -> Option<DigOpen> {
        if self.dig.is_none() {
            return None;
        }
        /// Cover side length in the strip — deliberately smaller than the grid's
        /// 150pt tile so the path reads as a trail, not a second wall.
        const COVER: f32 = 92.0;

        // Snapshot everything the strip paints before borrowing `self` for the
        // covers, matching how `vinyl_grid` decouples from the record lists.
        struct Card {
            release_id: u64,
            title: String,
            artist: String,
            sub: String,
            thumb_url: Option<String>,
            owned: bool,
            via: Option<(DigThread, String)>,
            label: Option<String>,
            /// Seconds since this record landed on the path, driving its
            /// entrance. Settled cards report a large value and animate nothing.
            since_landed: f32,
        }
        let (
            cards,
            at,
            pending,
            error,
            head_artist,
            head_label,
            has_artist_id,
            has_label_id,
            since_opened,
        ) = {
            let dig = self.dig.as_ref().expect("checked above");
            let head = dig.head();
            (
                dig.steps
                    .iter()
                    .map(|s| Card {
                        release_id: s.release_id,
                        title: s.title.clone(),
                        artist: s.artist.clone(),
                        sub: s.sub.clone(),
                        thumb_url: s.thumb_url.clone(),
                        owned: s.owned,
                        via: s.via.clone(),
                        // A label dig knows the imprint it followed even when
                        // the row's own field came back blank.
                        label: s.label.clone().or_else(|| match &s.via {
                            Some((DigThread::Label, name)) => Some(name.clone()),
                            _ => None,
                        }),
                        since_landed: s.landed_at.elapsed().as_secs_f32(),
                    })
                    .collect::<Vec<_>>(),
                dig.at,
                dig.pending,
                dig.error.clone(),
                strip_disambiguator(&head.artist).to_string(),
                head.label.clone(),
                !head.artist_ids.is_empty(),
                !head.label_ids.is_empty(),
                dig.opened_at.elapsed().as_secs_f32(),
            )
        };

        let mut open: Option<DigOpen> = None;
        let mut step: Option<DigThread> = None;
        let mut goto: Option<usize> = None;
        let mut end = false;

        // The strip's own entrance. `open_t` runs once per dig from the moment
        // it started; a settled strip clamps to 1.0 and pays for nothing.
        let open_t = egui::emath::easing::cubic_out((since_opened / OPEN_ANIM).clamp(0.0, 1.0));
        if open_t < 1.0 {
            ui.ctx().request_repaint();
        }
        // Slide down out of the toolbar edge: the panel's top margin starts
        // squeezed and opens to its resting value, which moves the whole strip
        // *and* the shelf below it rather than letting it overlap either.
        let rise = OPEN_RISE * (1.0 - open_t);
        ui.scope(|ui| {
            ui.multiply_opacity(open_t);
            egui::Frame::none()
            .fill(egui::Color32::from_gray(26))
            .rounding(egui::Rounding::same(8.0))
            .inner_margin(egui::Margin {
                left: 12.0,
                right: 12.0,
                top: 10.0 - rise * 0.5,
                bottom: 10.0 - rise * 0.5,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("🔍  Digging").strong());
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if crate::ui::icon::close_button(ui, "Stop digging and clear this path") {
                            end = true;
                        }
                        // Forward only re-walks a path already dug — a new
                        // branch is taken with the buttons below instead.
                        if ui
                            .add_enabled(at + 1 < cards.len(), egui::Button::new("→"))
                            .on_hover_note("Forward to the next record you dug")
                            .clicked()
                        {
                            goto = Some(at + 1);
                        }
                        if ui
                            .add_enabled(at > 0, egui::Button::new("←"))
                            .on_hover_note("Back one record, to choose the other thread")
                            .clicked()
                        {
                            goto = Some(at - 1);
                        }
                    });
                });
                ui.add_space(6.0);

                // The path itself. Scrolls horizontally once a dig runs past the
                // window width; each card is clickable to jump back to that point.
                egui::ScrollArea::horizontal()
                    .max_height(COVER + 74.0)
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            for (i, card) in cards.iter().enumerate() {
                                // How far into its arrival this card is. Every
                                // card but a just-dug one is settled at 1.0, so
                                // the path as a whole stays still while the new
                                // find is the only thing moving.
                                let enter = card_enter_t(card.since_landed);
                                if enter < 1.0 {
                                    ui.ctx().request_repaint();
                                }
                                if let Some((thread, matched)) = &card.via {
                                    // The connector names what was followed, so
                                    // a finished path explains itself. It draws
                                    // itself in ahead of the record it points
                                    // at, so the arrow reads as the thread being
                                    // pulled and the card as what came up on it.
                                    ui.vertical(|ui| {
                                        ui.add_space(COVER * 0.5 - 8.0);
                                        let arrow = (enter * 1.6).min(1.0);
                                        ui.label(
                                            egui::RichText::new("→").size(16.0).color(
                                                egui::Color32::from_gray(120)
                                                    .gamma_multiply(arrow),
                                            ),
                                        )
                                        .on_hover_note(
                                            format!("Same {}: {matched}", thread.label()),
                                        );
                                    });
                                }
                                let current = i == at;
                                // A landing card slides in from the right of
                                // its slot and fades up, like a sleeve being
                                // pushed into the row. Only the contents move:
                                // the slot below is allocated at full size
                                // either way, so the rest of the path holds
                                // still while the new find settles.
                                let card_size = egui::vec2(COVER, COVER + 60.0);
                                let slot = egui::Rect::from_min_size(ui.cursor().min, card_size);
                                let shifted = slot
                                    .translate(egui::vec2(CARD_SLIDE * (1.0 - enter), 0.0));
                                ui.allocate_rect(slot, egui::Sense::hover());
                                let mut card_ui = ui.new_child(
                                    egui::UiBuilder::new()
                                        .max_rect(shifted)
                                        .layout(egui::Layout::top_down(egui::Align::Min)),
                                );
                                card_ui.multiply_opacity(enter);
                                {
                                    let ui = &mut card_ui;
                                    {
                                        let (rect, resp) = ui.allocate_exact_size(
                                            egui::vec2(COVER, COVER),
                                            egui::Sense::click(),
                                        );
                                        // The sleeve itself grows the last of
                                        // the way into its square. Shrinking
                                        // `rect` here scales the whole tile at
                                        // once — art, the dim wash, the current
                                        // ring and the "yours" chip all read off
                                        // it — so the cover can't drift out of
                                        // its own border mid-entrance.
                                        let rect = rect.shrink(
                                            COVER * (1.0 - CARD_MIN_SCALE) * 0.5 * (1.0 - enter),
                                        );
                                        // Keep the record on screen in view as
                                        // the path outgrows the window —
                                        // otherwise a dig past the right edge
                                        // animates a card the user can't see.
                                        // Only while it's still arriving, so a
                                        // deliberate scroll back down the path
                                        // isn't yanked forward again.
                                        if current && enter < 1.0 {
                                            resp.scroll_to_me(Some(egui::Align::Center));
                                        }
                                        let resp =
                                            resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                                        // The starting record's cover is in the
                                        // local cache; dug records come off the
                                        // Discogs CDN by URL.
                                        let tex: Option<Tex> = if card.owned {
                                            self.dig_start_keys
                                                .get(&card.release_id)
                                                .and_then(|k| self.vinyl_covers.get(k))
                                                .and_then(|t| match t {
                                                    ThumbState::Ready(t) => t.clone(),
                                                    _ => None,
                                                })
                                        } else {
                                            card.thumb_url
                                                .as_deref()
                                                .and_then(|u| self.dig_cover(u))
                                                .cloned()
                                        };
                                        match &tex {
                                            Some(t) => {
                                                egui::Image::new(t)
                                                    .fit_to_exact_size(egui::vec2(COVER, COVER))
                                                    .rounding(egui::Rounding::same(5.0))
                                                    .paint_at(ui, rect);
                                            }
                                            None => {
                                                ui.painter().rect_filled(
                                                    rect,
                                                    egui::Rounding::same(5.0),
                                                    egui::Color32::from_gray(38),
                                                );
                                                ui.painter().text(
                                                    rect.center(),
                                                    egui::Align2::CENTER_CENTER,
                                                    "💿",
                                                    egui::FontId::proportional(26.0),
                                                    egui::Color32::from_gray(90),
                                                );
                                            }
                                        }
                                        // Steps behind and ahead of the cursor
                                        // are dimmed, so where you are on the
                                        // path is readable at a glance.
                                        if !current {
                                            ui.painter().rect_filled(
                                                rect,
                                                egui::Rounding::same(5.0),
                                                egui::Color32::from_black_alpha(120),
                                            );
                                        } else {
                                            ui.painter().rect_stroke(
                                                rect,
                                                egui::Rounding::same(5.0),
                                                egui::Stroke::new(
                                                    2.0,
                                                    egui::Color32::from_rgb(90, 200, 120),
                                                ),
                                            );
                                        }
                                        // The record you started from is marked,
                                        // so the one record on the path you
                                        // already own doesn't look like a find.
                                        if card.owned {
                                            let chip = egui::Rect::from_min_size(
                                                rect.min + egui::vec2(4.0, 4.0),
                                                egui::vec2(44.0, 15.0),
                                            );
                                            ui.painter().rect_filled(
                                                chip,
                                                egui::Rounding::same(4.0),
                                                egui::Color32::from_black_alpha(200),
                                            );
                                            ui.painter().text(
                                                chip.center(),
                                                egui::Align2::CENTER_CENTER,
                                                "owned",
                                                egui::FontId::proportional(9.5),
                                                egui::Color32::from_gray(210),
                                            );
                                        }
                                        let tip = if current {
                                            format!(
                                                "{}\n{}\n\nOpen the tracklist and listen",
                                                card.artist, card.title
                                            )
                                        } else {
                                            format!(
                                                "{}\n{}\n\nGo back to this record",
                                                card.artist, card.title
                                            )
                                        };
                                        if resp.on_hover_note(tip).clicked() {
                                            if current {
                                                open = Some(DigOpen {
                                                    release_id: card.release_id,
                                                    artist: card.artist.clone(),
                                                    title: card.title.clone(),
                                                    sub: card.sub.clone(),
                                                    cover_url: card.thumb_url.clone(),
                                                });
                                            } else {
                                                goto = Some(i);
                                            }
                                        }
                                        ui.set_max_width(COVER);
                                        ui.add_space(3.0);
                                        let t = egui::RichText::new(&card.title).font(crate::ui::tokens::font::footnote());
                                        ui.add(
                                            egui::Label::new(if current {
                                                t.strong()
                                            } else {
                                                t.weak()
                                            })
                                            .truncate(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(&card.artist).font(crate::ui::tokens::font::footnote()).weak(),
                                            )
                                            .truncate(),
                                        );
                                        // The imprint, third line and dimmest:
                                        // a sleeve rarely says which label put
                                        // the record out, and on a dig that's
                                        // half of what you're reading for.
                                        if let Some(label) = &card.label {
                                            ui.add(
                                                egui::Label::new(
                                                    egui::RichText::new(label)
                                                        .font(crate::ui::tokens::font::caption())
                                                        .color(egui::Color32::from_gray(125)),
                                                )
                                                .truncate(),
                                            );
                                        }
                                    }
                                }
                            }
                            // The step being fetched, as a placeholder tile at
                            // the end of the path — so a dig in flight looks
                            // like it's going somewhere.
                            if pending.is_some() {
                                ui.vertical(|ui| {
                                    ui.add_space(COVER * 0.5 - 8.0);
                                    ui.label(
                                        egui::RichText::new("→")
                                            .size(16.0)
                                            .color(egui::Color32::from_gray(120)),
                                    );
                                });
                                ui.allocate_ui_with_layout(
                                    egui::vec2(COVER, COVER + 60.0),
                                    egui::Layout::top_down(egui::Align::Min),
                                    |ui| {
                                        let (rect, _) = ui.allocate_exact_size(
                                            egui::vec2(COVER, COVER),
                                            egui::Sense::hover(),
                                        );
                                        ui.painter().rect_filled(
                                            rect,
                                            egui::Rounding::same(5.0),
                                            egui::Color32::from_gray(34),
                                        );
                                        ui.put(rect, egui::Spinner::new());
                                    },
                                );
                            }
                        });
                    });

                ui.add_space(8.0);
                if let Some(e) = &error {
                    ui.label(
                        egui::RichText::new(e)
                            .small()
                            .color(egui::Color32::from_rgb(220, 160, 120)),
                    );
                    ui.add_space(6.0);
                }
                // The choice. Both threads are always shown — a disabled branch
                // with a reason teaches the shape of the record, where a hidden
                // one just looks broken.
                ui.horizontal(|ui| {
                    let busy = pending.is_some();
                    // Gated on the *id*, not the name: until the release
                    // detail resolves there's nothing to browse by.
                    let can_artist = has_artist_id;
                    let artist_tip = if can_artist {
                        format!("Find another vinyl release by {head_artist} that you don't own")
                    } else if head_artist.trim().is_empty() {
                        "Discogs lists no artist for this record".to_string()
                    } else {
                        format!("Looking up {head_artist} on Discogs…")
                    };
                    if ui
                        .add_enabled(
                            can_artist && !busy,
                            egui::Button::new("  ♪  Dig the artist  "),
                        )
                        .on_hover_note(artist_tip.clone())
                        .on_disabled_hover_text(crate::ui::hover::note(artist_tip))
                        .clicked()
                    {
                        step = Some(DigThread::Artist);
                    }
                    let can_label = has_label_id;
                    let label_tip = match &head_label {
                        Some(l) if can_label => {
                            format!("Find another vinyl release on {l} that you don't own")
                        }
                        Some(l) => format!("Looking up {l} on Discogs…"),
                        None => "Discogs lists no label for this record".to_string(),
                    };
                    if ui
                        .add_enabled(
                            can_label && !busy,
                            egui::Button::new("  ⌂  Dig the label  "),
                        )
                        .on_hover_note(label_tip.clone())
                        .on_disabled_hover_text(crate::ui::hover::note(label_tip))
                        .clicked()
                    {
                        step = Some(DigThread::Label);
                    }
                    if busy {
                        ui.label(egui::RichText::new("Searching Discogs…").weak());
                    }
                });
            });
        });

        // Vary the roll between clicks (see `dig_roll`).
        self.dig_seed = self.dig_seed.wrapping_add(0x2545_F491_4F6C_DD1D);
        if end {
            // Closing the strip abandons the dig, so any speculation for it is
            // now pure waste on a shared pace the rest of the app is using.
            if let Some(dig) = self.dig.as_ref() {
                dig.cancel_prime.store(true, Ordering::Relaxed);
            }
            self.dig = None;
        } else if let Some(i) = goto {
            if let Some(dig) = self.dig.as_mut() {
                dig.at = i;
                dig.error = None;
            }
        } else if let Some(thread) = step {
            self.dig_step(thread);
        }
        // Whichever way the head moved — a branch taken, a step back, a jump to
        // a card — everything speculated for the record we just left is now
        // dead weight, the untaken sibling included. Dropping it here is what
        // makes "clear the other one and move to the next two options" a single
        // rule rather than a case per way of moving.
        self.dig_evict();
        self.dig_prime();
        open
    }

    /// Drop speculative pages that don't belong to the record on screen, and
    /// forget an in-flight prefetch that was started for a record we've left so
    /// the next head can be primed immediately rather than waiting it out.
    fn dig_evict(&mut self) {
        let Some(dig) = self.dig.as_mut() else { return };
        let head = dig.head().release_id;
        dig.ready.retain(|(id, _), _| *id == head);
        if let Some((from, _)) = &dig.priming {
            if *from != head {
                // Its results would be dropped on arrival anyway, so stop it
                // spending requests to produce them — on a backtrack the next
                // head wants that pace for its own two threads.
                dig.cancel_prime.store(true, Ordering::Relaxed);
                dig.priming = None;
            }
        }
    }

    /// Whether digging from `key` is possible at all — it needs an artist or a
    /// label to search on. Unlike the old in-collection dig this can't promise a
    /// count up front: what's out there is a Discogs query away.
    pub(crate) fn can_dig(&self, key: VinylCoverKey) -> bool {
        self.vinyl_record(key).is_some_and(|r| {
            !r.artist.trim().is_empty() || r.label.as_deref().is_some_and(|l| !l.trim().is_empty())
        })
    }
}
