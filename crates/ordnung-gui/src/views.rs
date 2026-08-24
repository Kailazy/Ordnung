//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

// `Ordering` from the glob import is the atomic one; the vinyl sort wants the
// comparison enum, so it gets its own name.
use std::cmp::Ordering as CmpOrdering;

/// Flip a comparison when the sort runs descending. Keeps the "missing values
/// last" arms of the vinyl comparators out of the direction logic — only the
/// both-present case is ever reversed.
fn cmp_direction(ord: CmpOrdering, ascending: bool) -> CmpOrdering {
    if ascending {
        ord
    } else {
        ord.reverse()
    }
}

/// The Library Health header: one window, two tabs. Duplicate copies and missing
/// source files are two readings of the same question, so they share a view (and
/// a single sidebar entry) instead of competing for two. Drawn in place of each
/// view's own heading, left of that view's action buttons. Returns the tab the
/// user clicked, if any — the caller owns the switch, since `self` is borrowed
/// by the surrounding layout closure.
fn health_tabs(
    ui: &mut egui::Ui,
    current: &LibraryView,
    dup_count: Option<usize>,
    missing_count: u64,
) -> Option<LibraryView> {
    let mut switch = None;
    let tab = |ui: &mut egui::Ui, label: String, active: bool| {
        ui.selectable_label(active, egui::RichText::new(label).size(15.0).strong())
            .clicked()
    };
    let dup_label = match dup_count {
        Some(n) if n > 0 => format!("⧉  Duplicates ({n})"),
        _ => "⧉  Duplicates".to_string(),
    };
    if tab(ui, dup_label, *current == LibraryView::Duplicates) {
        switch = Some(LibraryView::Duplicates);
    }
    let missing_label = if missing_count > 0 {
        format!("⚠  Missing ({missing_count})")
    } else {
        "⚠  Missing".to_string()
    };
    if tab(ui, missing_label, *current == LibraryView::Missing) {
        switch = Some(LibraryView::Missing);
    }
    switch
}

/// Show a small confirmation dialog. When `pos` is set (the screen point where
/// the user clicked the action), the dialog opens right there so the confirm
/// button lands under the cursor — no swipe across the window. Without a
/// position it falls back to centered.
fn confirm_window(
    title: &str,
    pos: Option<egui::Pos2>,
    ctx: &egui::Context,
    add_contents: impl FnOnce(&mut egui::Ui),
) {
    let win = egui::Window::new(title).collapsible(false).resizable(false);
    let win = match pos {
        // Nudge up-left so the cursor sits inside the dialog body, a short hop
        // from the confirm button row.
        Some(p) => win.default_pos(p - egui::vec2(28.0, 16.0)),
        None => win
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center()),
    };
    win.show(ctx, add_contents);
}

/// One cell of a vinyl grid: everything the cover tile draws, snapshotted from a
/// `VinylRecord` so the render closure never borrows the record lists. Shared by
/// the collection and wantlist sections of the vinyl view.
struct VinylCell {
    /// Cover cache key: which list this belongs to, plus that list's row id.
    key: VinylCoverKey,
    release_id: u64,
    title: String,
    artist: String,
    /// Second caption line, e.g. `1993 · Vinyl, 12"`.
    sub: String,
    has_cover: bool,
    /// Discogs `date_added`, ISO 8601 — which sorts correctly as plain text.
    /// `None` on records cached before Discogs reported one.
    added: Option<String>,
    /// Lowest current marketplace listing, and the currency it's quoted in.
    /// `None` until a sync has priced this record (or when nothing is for sale).
    price: Option<f64>,
    price_currency: Option<String>,
    /// Catalog track ids linked to this release — empty if you don't own a
    /// digital copy. Drives the "in catalog" badge and the jump-to.
    linked: Vec<Id>,
    /// True when this release is *also* in the other Discogs list. Moving it
    /// there would ask Discogs for a second copy (collection adds aren't
    /// idempotent), so the move is shown as already done instead.
    also_in_other: bool,
}

/// How the vinyl grids are ordered. Persisted as a stable key in
/// [`crate::config::Config::vinyl_sort`], so an unknown key from another build
/// falls back to `Artist` (the fixed order the view shipped with) rather than
/// failing to load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VinylSort {
    /// Discogs `date_added` — when the record joined the collection/wantlist.
    Added,
    /// Lowest current marketplace listing (see `Client::marketplace_price`).
    Price,
    /// Artist, then title. The catalog's own ordering.
    Artist,
}

impl VinylSort {
    fn from_key(key: &str) -> Self {
        match key {
            "added" => VinylSort::Added,
            "price" => VinylSort::Price,
            _ => VinylSort::Artist,
        }
    }

    fn key(self) -> &'static str {
        match self {
            VinylSort::Added => "added",
            VinylSort::Price => "price",
            VinylSort::Artist => "artist",
        }
    }

    fn label(self) -> &'static str {
        match self {
            VinylSort::Added => "Date added",
            VinylSort::Price => "Price",
            VinylSort::Artist => "Artist",
        }
    }

    /// Direction labels for this field, `(ascending, descending)` — "oldest
    /// first" says more than "ascending" when the field is a date.
    fn direction_labels(self) -> (&'static str, &'static str) {
        match self {
            VinylSort::Added => ("Oldest first", "Newest first"),
            VinylSort::Price => ("Cheapest first", "Most expensive first"),
            VinylSort::Artist => ("A → Z", "Z → A"),
        }
    }
}

/// Order one grid's cells in place. `cells` arrives in catalog order (artist,
/// then title), which is what `Artist` sorts by — so that case only has to
/// decide whether to reverse. Ties fall back to artist so the layout is stable
/// between frames (dates repeat; prices repeat more).
fn sort_vinyl_cells(cells: &mut [VinylCell], sort: VinylSort, ascending: bool) {
    match sort {
        VinylSort::Artist => {
            if !ascending {
                cells.reverse();
            }
        }
        VinylSort::Added => cells.sort_by(|a, b| {
            let ord = match (&a.added, &b.added) {
                (Some(x), Some(y)) => cmp_direction(x.cmp(y), ascending),
                (Some(_), None) => CmpOrdering::Less,
                (None, Some(_)) => CmpOrdering::Greater,
                (None, None) => CmpOrdering::Equal,
            };
            ord.then_with(|| a.artist.cmp(&b.artist))
        }),
        VinylSort::Price => cells.sort_by(|a, b| {
            let ord = match (a.price, b.price) {
                // Prices are finite (`marketplace_price` rejects anything
                // non-positive), so a total order is safe here.
                (Some(x), Some(y)) => {
                    cmp_direction(x.partial_cmp(&y).unwrap_or(CmpOrdering::Equal), ascending)
                }
                (Some(_), None) => CmpOrdering::Less,
                (None, Some(_)) => CmpOrdering::Greater,
                (None, None) => CmpOrdering::Equal,
            };
            ord.then_with(|| a.artist.cmp(&b.artist))
        }),
    }
}

/// Render a marketplace price the way the grid shows it: symbol for the
/// currencies a record collection actually turns up, else the bare code.
/// `decimals` is for the tooltip, where there's room for the exact figure.
fn format_price(value: f64, currency: Option<&str>, decimals: bool) -> String {
    let code = currency.unwrap_or("").trim().to_uppercase();
    let symbol = match code.as_str() {
        "USD" | "CAD" | "AUD" | "NZD" => "$",
        "EUR" => "€",
        "GBP" => "£",
        "JPY" => "¥",
        _ => "",
    };
    let amount = if decimals {
        format!("{value:.2}")
    } else {
        format!("{}", value.round() as i64)
    };
    if symbol.is_empty() {
        if code.is_empty() {
            amount
        } else {
            format!("{amount} {code}")
        }
    } else {
        format!("{symbol}{amount}")
    }
}

/// What a click or right-click in the vinyl grid asked for. Returned from the
/// render closure and applied by the caller, once the borrows the grid holds on
/// the record lists are released. Edits name their record by cache key
/// (`list` + `instance_id`) rather than carrying it, so the caller resolves it
/// against the live lists.
enum VinylGridAction {
    /// Show the catalog tracks linked to this release: the release title (to
    /// narrow the library by album) and the track ids to select.
    Goto(String, Vec<Id>),
    /// Move this record to the other Discogs list.
    Move(VinylCoverKey),
    /// Drop this record from the list it's in.
    Remove(VinylCoverKey),
    /// Open this record's sheet — its tracklist and everything that can play it.
    Open(VinylCoverKey),
    /// Open the sheet *and* start the record from its first playable track.
    Play(VinylCoverKey),
    /// Start a dig from this record — see [`crate::dig`].
    Dig(VinylCoverKey),
}

impl App {
    /// Recount tracks with a missing source file (drives the toolbar's relocate
    /// button). Kept out of `reload` so filter keystrokes don't stat the whole
    /// catalog; called after jobs and on Refresh, when file existence can change.
    pub(crate) fn recount_missing(&mut self) {
        self.missing_labels = Catalog::open(&self.db_path)
            .and_then(|c| c.missing_track_labels())
            .unwrap_or_default();
        self.missing_count = self.missing_labels.len() as u64;
    }

    /// Switch the Library Health window to one of its two tabs and remember the
    /// choice, so reopening the section from the sidebar lands back here. The
    /// reload is explicit: the view-change hook in `update` runs before the content
    /// panel is drawn, so a switch made *inside* the panel would otherwise leave
    /// the new tab's data (the duplicate scan, the missing-file stat) unloaded.
    pub(crate) fn open_health_tab(&mut self, tab: LibraryView, ctx: &egui::Context) {
        self.view = tab.clone();
        self.health_tab = tab;
        self.reload();
        // `poll_duplicates` also runs before the content panel, so a fresh scan
        // needs one more frame to start.
        ctx.request_repaint();
    }

    /// Render the Duplicates view: grouped blocks (identical audio first, then
    /// same-song variants). Each group proposes keeping the ★ best copy and
    /// deleting the rest; every copy carries an instant keep/delete toggle (pure
    /// state — no disk IO, so marking never blocks). When the user is happy, the
    /// toolbar's "Delete N marked" commits every marked copy at once in a
    /// background job: it moves the source files to the Trash (recoverable) and
    /// hands each dropped copy's playlist slots to its kept counterpart.
    pub(crate) fn draw_duplicates(&mut self, ui: &mut egui::Ui) {
        let audio_enabled = self.audio.is_some();

        // Seed a keep/delete proposal for any group we haven't seen yet, and
        // forget decisions for copies that no longer exist. Default proposal:
        // keep the best copy, mark the rest for deletion — the user revises the
        // marks before committing.
        let live: HashSet<Id> = self
            .dup_groups
            .iter()
            .flat_map(|g| g.tracks.iter().map(|t| t.id))
            .collect();
        self.dup_decisions.retain(|id, _| live.contains(id));
        for g in &self.dup_groups {
            let best = best_copy_index(&g.tracks).unwrap_or(0);
            if !self.dup_decisions.contains_key(&g.tracks[best].id) {
                for (i, t) in g.tracks.iter().enumerate() {
                    self.dup_decisions.insert(t.id, i != best);
                }
            }
        }

        // Snapshot everything we draw so the egui closures don't borrow `self`
        // (we need `self` mutably afterwards to apply the collected actions).
        struct CopyView {
            id: Id,
            fmt: String,
            br: String,
            path: PathBuf,
            playing: bool,
            /// The best copy in its group (lossless, else highest bitrate) — the
            /// default keeper, flagged with a ★ badge.
            is_best: bool,
            /// Whether the user currently has this copy marked for deletion.
            marked_delete: bool,
            /// Transcode-quality verdict from the analysis cache. `None` means the
            /// copy hasn't been analyzed for it yet (offer an Analyze button).
            quality: Option<TranscodeVerdict>,
            quality_cut_hz: Option<f32>,
            quality_src: Option<&'static str>,
        }
        struct GroupView {
            kind: DuplicateKind,
            title: String,
            /// Stable group identity, for the "not a duplicate" dismissal.
            key: String,
            /// All copies, best first; each carries its own keep/delete mark.
            copies: Vec<CopyView>,
        }
        // Per-copy transcode-quality verdict, looked up once from the analysis
        // cache. Mirrors the Library "Quality" column: only meaningful at analyzer
        // v6+, where the low-pass cutoff was measured. Copies missing it read as
        // "not analyzed" and get an inline Analyze button below.
        type QualityInfo = (Option<TranscodeVerdict>, Option<f32>, Option<&'static str>);
        let quality: HashMap<Id, QualityInfo> = Catalog::open(&self.db_path)
            .map(|c| {
                self.dup_groups
                    .iter()
                    .flat_map(|g| &g.tracks)
                    .map(|t| {
                        let a = c.get_analysis(t.id).ok().flatten();
                        let v = a
                            .as_ref()
                            .filter(|a| a.analyzer_version >= 6)
                            .map(|a| a.transcode_verdict());
                        let cut = a.as_ref().and_then(|a| a.lowpass_hz);
                        let src = a.as_ref().and_then(|a| a.estimated_source_kbps());
                        (t.id, (v, cut, src))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let make_copy =
            |t: &Track, audio: &Option<AudioEngine>, is_best: bool, marked_delete: bool| {
                let (qv, qcut, qsrc) = quality.get(&t.id).copied().unwrap_or((None, None, None));
                CopyView {
                    id: t.id,
                    fmt: format_label(t.format).to_string(),
                    br: t
                        .properties
                        .as_ref()
                        .and_then(|p| p.bitrate_kbps)
                        .map(|b| format!("{b}k"))
                        .unwrap_or_else(|| "—".into()),
                    path: PathBuf::from(&t.source_path),
                    playing: audio
                        .as_ref()
                        .is_some_and(|a| matches!(a.state_for(t.id), PlayState::Playing)),
                    is_best,
                    marked_delete,
                    quality: qv,
                    quality_cut_hz: qcut,
                    quality_src: qsrc,
                }
            };
        let groups: Vec<GroupView> = self
            .dup_groups
            .iter()
            .map(|g| {
                let best = best_copy_index(&g.tracks).unwrap_or(0);
                let head = &g.tracks[0];
                let title = format!(
                    "{} — {}",
                    head.tags.artist.as_deref().unwrap_or("—"),
                    head.tags.title.as_deref().unwrap_or("—"),
                );
                // Best copy first so it's the default keeper, then the rest in
                // catalog order. Each copy's mark comes from `dup_decisions`.
                let marked = |id: Id| self.dup_decisions.get(&id).copied().unwrap_or(false);
                let mut copies = Vec::with_capacity(g.tracks.len());
                copies.push(make_copy(
                    &g.tracks[best],
                    &self.audio,
                    true,
                    marked(g.tracks[best].id),
                ));
                for (i, t) in g.tracks.iter().enumerate() {
                    if i != best {
                        copies.push(make_copy(t, &self.audio, false, marked(t.id)));
                    }
                }
                GroupView {
                    kind: g.kind,
                    title,
                    key: g.key.clone(),
                    copies,
                }
            })
            .collect();

        let identical_n = groups
            .iter()
            .filter(|g| g.kind == DuplicateKind::Identical)
            .count();
        let variant_n = groups
            .iter()
            .filter(|g| g.kind == DuplicateKind::SameTrack)
            .count();
        let acoustic_n = groups
            .iter()
            .filter(|g| g.kind == DuplicateKind::Acoustic)
            .count();
        // Total copies marked for deletion across every group — drives the
        // "Delete N marked" commit button.
        let marked_total = groups
            .iter()
            .flat_map(|g| &g.copies)
            .filter(|c| c.marked_delete)
            .count();
        // Every copy that still has no transcode-quality verdict — what the
        // top-level "Analyze" button scans in one pass.
        let unanalyzed: Vec<Id> = groups
            .iter()
            .flat_map(|g| &g.copies)
            .filter(|c| c.quality.is_none())
            .map(|c| c.id)
            .collect();

        enum Act {
            Preview(Id, PathBuf),
            Reveal(PathBuf),
            // Set one copy's decision explicitly: `true` marks it for deletion,
            // `false` keeps it. Each copy decides independently — a group may keep
            // several copies or, if every copy is marked, delete the track outright.
            SetDelete(Id, bool),
            // Apply the default proposal to a group: keep `best`, mark the rest.
            Suggest { best: Id, ids: Vec<Id> },
            // Clear every delete mark in a group (keep all of its copies).
            KeepAll(Vec<Id>),
            // Mark a group "not a duplicate" by its stable key — persists, so it
            // never reappears. Deletes nothing.
            NotDuplicate(String),
            // Analyze these copies for the transcode-quality tag (the ids missing
            // a verdict). On completion the duplicates view refreshes and the
            // chips fill in.
            Analyze(Vec<Id>),
        }
        let mut acts: Vec<Act> = Vec::new();
        let mut recompute = false;
        // Set when the user clicks "Delete N marked" — handled after the action
        // loop so this frame's toggles are already applied.
        let mut request_commit = false;

        // The tab the user clicked in the shared Library Health header, applied
        // after the layout closure releases its borrow of `self`.
        let mut switch_tab = None;
        // A count is only honest once a scan has settled; while one is pending the
        // tab shows no number rather than a stale one.
        let dup_count = (!self.dup_dirty && !self.dup_loading).then(|| self.dup_groups.len());

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            switch_tab = health_tabs(ui, &self.view, dup_count, self.missing_count);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("↻ Recompute")
                    .on_hover_note("Re-scan the catalog for duplicates")
                    .clicked()
                {
                    recompute = true;
                }
                if !unanalyzed.is_empty() {
                    let n = unanalyzed.len();
                    if ui
                        .button(format!("⚡ Analyze {n} for quality"))
                        .on_hover_note("Scan unchecked copies for lossy transcodes")
                        .clicked()
                    {
                        acts.push(Act::Analyze(unanalyzed.clone()));
                    }
                }
                // The one mutating action: commit every marked copy at once. It
                // runs in the background, so it never blocks reviewing the rest.
                let enabled = marked_total > 0 && !self.is_busy();
                let resp = ui.add_enabled(
                    enabled,
                    egui::Button::new(
                        egui::RichText::new(format!("🗑 Delete {marked_total} marked"))
                            .strong()
                            .color(egui::Color32::WHITE),
                    )
                    .fill(egui::Color32::from_rgb(0xB0, 0x30, 0x30)),
                );
                if resp
                    .on_hover_note(
                        "Move marked copies to the Trash. Kept copies inherit \
                         their playlist slots.",
                    )
                    .clicked()
                {
                    request_commit = true;
                }
            });
        });

        // Switching tabs swaps the whole body, so stop drawing this one's.
        if let Some(tab) = switch_tab {
            self.open_health_tab(tab, ui.ctx());
            return;
        }

        if groups.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                // A scan in flight (or queued) means we can't yet claim "none found".
                if self.dup_loading || self.dup_dirty {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("Scanning the catalog for duplicates…");
                    });
                } else {
                    ui.heading("No duplicates found ✓");
                    ui.label("Every track is unique by audio content and by artist + title.");
                }
            });
            if recompute {
                self.reload();
            }
            return;
        }

        ui.label(
            egui::RichText::new(format!(
                "{identical_n} identical-audio · {variant_n} same-track variant · \
                 {acoustic_n} sounds-identical group(s).  \
                 Each group keeps the ★ best copy (lossless, else highest bitrate) and marks \
                 the rest for deletion — click any copy's tile to keep or reject it. When \
                 you're happy, hit \"Delete N marked\" to trash every marked copy at once \
                 (recoverable, runs in the background)."
            ))
            .weak(),
        );
        ui.separator();

        // Render one copy as a node tile: a click-to-toggle card showing its
        // keep/reject state, the ★ best badge, format + bitrate, the transcode-
        // quality chip (or an inline Analyze), and Preview / Reveal. Clicking
        // anywhere on the card flips keep⇄reject — the whole tile is the target, so
        // triaging a group is one click per copy. The inner buttons (Analyze /
        // Preview / Reveal) keep their own clicks and never toggle. Rejected tiles
        // read red and dim with a struck-out filename so decisions scan at a glance.
        // Toggling is pure in-memory state — it never touches disk, so it never blocks.
        fn render_tile(ui: &mut egui::Ui, c: &CopyView, audio_enabled: bool, acts: &mut Vec<Act>) {
            const TILE_W: f32 = 252.0;
            const TILE_H: f32 = 104.0;
            let kept = !c.marked_delete;
            let green = egui::Color32::from_rgb(0x3A, 0x8A, 0x4E);
            let red = egui::Color32::from_rgb(0xB0, 0x40, 0x40);

            // Reserve the tile rect first so the inner buttons, added afterwards,
            // sit on top and win their own clicks; the surrounding card click then
            // only fires when it lands on bare tile, not on a button.
            let (rect, tile) =
                ui.allocate_exact_size(egui::vec2(TILE_W, TILE_H), egui::Sense::click());
            let tile = tile.on_hover_cursor(egui::CursorIcon::PointingHand);
            let hovered = tile.hovered();
            // Keep = neutral card with a green edge; reject = red-tinted and darker.
            let (fill, edge) = if kept {
                (
                    egui::Color32::from_gray(if hovered { 0x30 } else { 0x29 }),
                    if hovered {
                        green.gamma_multiply(1.4)
                    } else {
                        green
                    },
                )
            } else {
                (
                    egui::Color32::from_rgb(0x33, 0x24, 0x24),
                    if hovered {
                        red.gamma_multiply(1.4)
                    } else {
                        red
                    },
                )
            };
            ui.painter().rect(
                rect,
                egui::Rounding::same(8.0),
                fill,
                egui::Stroke::new(if kept { 1.5 } else { 2.0 }, edge),
            );

            let mut inner_clicked = false;
            let mut content = ui.new_child(
                egui::UiBuilder::new()
                    .max_rect(rect.shrink(9.0))
                    .layout(egui::Layout::top_down(egui::Align::Min))
                    .id_salt(("dup-tile", c.id)),
            );
            let ui = &mut content;
            ui.spacing_mut().item_spacing.y = 5.0;

            // Row 1: keep/reject state pill · ★ best · format + bitrate (right).
            ui.horizontal(|ui| {
                let (pill, pill_bg) = if kept {
                    ("✓ KEEP", green)
                } else {
                    ("🗑 REJECT", red)
                };
                ui.label(
                    egui::RichText::new(pill)
                        .small()
                        .strong()
                        .color(egui::Color32::WHITE)
                        .background_color(pill_bg),
                );
                if c.is_best {
                    ui.label(
                        egui::RichText::new("★").color(egui::Color32::from_rgb(0xD8, 0xB0, 0x4A)),
                    )
                    .on_hover_note("Best copy: lossless, else highest bitrate");
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    ui.monospace(egui::RichText::new(format!("{} {}", c.fmt, c.br)).strong());
                });
            });

            // Row 2: transcode-quality chip (or inline Analyze) · Preview · Reveal.
            ui.horizontal(|ui| {
                match c.quality {
                    Some(v) => {
                        let (label, bg) = quality_chip(v);
                        ui.label(
                            egui::RichText::new(label)
                                .small()
                                .color(chip_text_color(bg))
                                .background_color(bg),
                        )
                        .on_hover_note(quality_blurb(
                            v,
                            c.quality_cut_hz,
                            c.quality_src,
                        ));
                    }
                    None => {
                        if ui
                            .small_button("Analyze")
                            .on_hover_note("Scan this copy for a lossy transcode")
                            .clicked()
                        {
                            inner_clicked = true;
                            acts.push(Act::Analyze(vec![c.id]));
                        }
                    }
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .small_button("Reveal")
                        .on_hover_note("Show in Finder")
                        .clicked()
                    {
                        inner_clicked = true;
                        acts.push(Act::Reveal(c.path.clone()));
                    }
                    if audio_enabled {
                        let label = if c.playing { "⏸" } else { "▶" };
                        if ui.small_button(label).on_hover_note("Preview").clicked() {
                            inner_clicked = true;
                            acts.push(Act::Preview(c.id, c.path.clone()));
                        }
                    }
                });
            });

            // Row 3: filename, struck out when rejected.
            let name = c.path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let mut name_text = egui::RichText::new(name).monospace().small().weak();
            if !kept {
                name_text = name_text.strikethrough();
            }
            ui.add(egui::Label::new(name_text).truncate())
                .on_hover_note(c.path.display().to_string());

            // Whole-tile click toggles keep⇄reject, unless an inner button took it.
            if tile.clicked() && !inner_clicked {
                acts.push(Act::SetDelete(c.id, kept));
            }
            tile.on_hover_note(if kept {
                "Click to mark for deletion"
            } else {
                "Click to keep this copy"
            });
        }

        let render_group = |ui: &mut egui::Ui, g: &GroupView, acts: &mut Vec<Act>| {
            ui.group(|ui| {
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new(&g.title).strong());
                    let marked = g.copies.iter().filter(|c| c.marked_delete).count();
                    if marked > 0 {
                        // When every copy is marked, the track leaves the catalog
                        // entirely — call that out in a louder colour so it's never
                        // an accident.
                        if marked == g.copies.len() {
                            ui.label(
                                egui::RichText::new("· deletes the whole track")
                                    .small()
                                    .strong()
                                    .color(egui::Color32::from_rgb(0xE0, 0x6C, 0x6C)),
                            )
                            .on_hover_note("No copy is kept. All copies move to the Trash.");
                        } else {
                            ui.label(
                                egui::RichText::new(format!("· {marked} to delete"))
                                    .small()
                                    .color(egui::Color32::from_rgb(0xC0, 0x6C, 0x6C)),
                            );
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui
                            .button("Not a dup")
                            .on_hover_note(
                                "Not the same song. Dismisses this group for good; \
                                 nothing is deleted.",
                            )
                            .clicked()
                        {
                            acts.push(Act::NotDuplicate(g.key.clone()));
                        }
                        let ids: Vec<Id> = g.copies.iter().map(|c| c.id).collect();
                        if ui
                            .button("Keep all")
                            .on_hover_note("Clear every delete mark in this group")
                            .clicked()
                        {
                            acts.push(Act::KeepAll(ids.clone()));
                        }
                        if let Some(best) = g.copies.iter().find(|c| c.is_best).map(|c| c.id) {
                            if ui
                                .button("★ Suggest")
                                .on_hover_note("Keep the best copy, mark the rest for deletion")
                                .clicked()
                            {
                                acts.push(Act::Suggest { best, ids });
                            }
                        }
                    });
                });
                ui.add_space(6.0);
                // Lay the copies out as a wrapping row of node tiles so the dupes
                // of one song read as a cluster of cards rather than a stack.
                ui.horizontal_wrapped(|ui| {
                    ui.spacing_mut().item_spacing = egui::vec2(10.0, 10.0);
                    for c in &g.copies {
                        render_tile(ui, c, audio_enabled, acts);
                    }
                });
            });
        };

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if identical_n > 0 {
                    ui.add_space(4.0);
                    ui.heading("Identical audio");
                    ui.label(
                        egui::RichText::new("The same recording imported more than once — safe to keep one.")
                            .weak(),
                    );
                    for g in groups.iter().filter(|g| g.kind == DuplicateKind::Identical) {
                        render_group(ui, g, &mut acts);
                    }
                }
                if variant_n > 0 {
                    ui.add_space(10.0);
                    ui.heading("Same track, different files");
                    ui.label(
                        egui::RichText::new(
                            "Same artist + title, different files — likely re-encodes or formats. Review by hand.",
                        )
                        .weak(),
                    );
                    for g in groups.iter().filter(|g| g.kind == DuplicateKind::SameTrack) {
                        render_group(ui, g, &mut acts);
                    }
                }
                if acoustic_n > 0 {
                    ui.add_space(10.0);
                    ui.heading("Sounds identical");
                    ui.label(
                        egui::RichText::new(
                            "Matched by audio fingerprint despite differing files and tags — \
                             the duplicates you'd only catch on playback. Review by hand.",
                        )
                        .weak(),
                    );
                    for g in groups.iter().filter(|g| g.kind == DuplicateKind::Acoustic) {
                        render_group(ui, g, &mut acts);
                    }
                }
            });

        for act in acts {
            match act {
                Act::Preview(id, path) => self.play_track(id, path),
                Act::Reveal(path) => reveal_in_finder(&path),
                // Setting a decision is pure state — no disk IO, so it never
                // blocks. Each copy is independent: marking every copy in a group is
                // allowed and deletes the track outright (the group header and the
                // commit dialog both flag that), so there's no last-keeper guard.
                Act::SetDelete(id, delete) => {
                    self.dup_decisions.insert(id, delete);
                }
                // Re-apply the default proposal to a group: keep best, mark rest.
                Act::Suggest { best, ids } => {
                    for id in ids {
                        self.dup_decisions.insert(id, id != best);
                    }
                }
                // Clear a group's delete marks — keep everything in it.
                Act::KeepAll(ids) => {
                    for id in ids {
                        self.dup_decisions.insert(id, false);
                    }
                }
                // Run analysis for the copies missing a quality verdict. `force` is
                // false: these ids are exactly the ones not yet analyzed at the
                // current version. On completion `poll_worker` → `reload`
                // recomputes the duplicate groups (we're in the Duplicates view),
                // so the chips appear without any extra plumbing.
                Act::Analyze(ids) if !ids.is_empty() && !self.is_busy() => {
                    self.spawn_analyze_ids(ui.ctx().clone(), ids, false);
                }
                Act::Analyze(_) => {}
                // Persist the dismissal and recompute so the group drops out now.
                Act::NotDuplicate(key) => {
                    if let Ok(c) = Catalog::open(&self.db_path) {
                        match c.ignore_duplicate_group(&key) {
                            Ok(()) => {
                                self.status = "Marked as not a duplicate.".into();
                                recompute = true;
                            }
                            Err(e) => self.status = format!("Couldn't dismiss group: {e}"),
                        }
                    }
                }
            }
        }

        // Stage the commit confirmation from what the user sees: in each group the
        // keeper is the first copy still kept (the ★ best when it survives), and
        // every marked copy hands its playlist slots to it. When a group has no
        // keeper (every copy marked), each copy is staged self-referencing
        // (keeper == drop) — the trash worker reads that as "delete outright", so
        // the track and its playlist slots go away with nothing to inherit them.
        if request_commit && !self.is_busy() {
            let mut batch: Vec<(Id, Id, PathBuf)> = Vec::new();
            for g in &groups {
                let keeper = g.copies.iter().find(|c| !c.marked_delete).map(|c| c.id);
                for c in g.copies.iter().filter(|c| c.marked_delete) {
                    batch.push((keeper.unwrap_or(c.id), c.id, c.path.clone()));
                }
            }
            if batch.is_empty() {
                self.status = "Nothing marked for deletion.".into();
            } else {
                self.dup_confirm_pos = ui.ctx().pointer_interact_pos();
                self.dup_pending_bulk = Some(batch);
            }
        }

        // Confirmation for the staged "delete marked" batch. Confirming hands the
        // batch to a background job (non-blocking); `poll_worker` reloads and
        // recomputes the groups when it finishes. Built once per frame so the
        // closure can stage the spawn without re-borrowing `self`.
        let mut spawn_batch: Option<Vec<(Id, Id, PathBuf)>> = None;
        if let Some(batch) = self.dup_pending_bulk.clone() {
            let n = batch.len();
            let mut close = false;
            // Esc anywhere cancels — no need to aim at the Cancel button.
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                close = true;
            }
            // Copies staged self-referencing (keeper == drop) belong to groups where
            // every copy was marked — those remove the track entirely, so warn.
            let whole_track = batch.iter().filter(|(k, d, _)| k == d).count();
            confirm_window(
                "Delete marked duplicates",
                self.dup_confirm_pos,
                ui.ctx(),
                |ui| {
                    ui.label(format!(
                        "Move {n} marked cop{} to the Trash and remove {} from the catalog? \
                     The kept copy in each group stays.",
                        if n == 1 { "y" } else { "ies" },
                        if n == 1 { "it" } else { "them" },
                    ));
                    if whole_track > 0 {
                        ui.label(
                            egui::RichText::new(if whole_track == 1 {
                                "⚠ One of these is in a group with no copy kept — \
                             that track leaves the catalog entirely."
                                    .to_string()
                            } else {
                                format!(
                                    "⚠ {whole_track} of these are in groups with no copy kept — \
                                 those tracks leave the catalog entirely."
                                )
                            })
                            .color(egui::Color32::from_rgb(0xE0, 0x6C, 0x6C)),
                        );
                    }
                    egui::ScrollArea::vertical()
                        .max_height(200.0)
                        .show(ui, |ui| {
                            for (_, _, path) in &batch {
                                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                                ui.label(egui::RichText::new(name).weak().monospace())
                                    .on_hover_note(path.display().to_string());
                            }
                        });
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        let confirm = ui.add(
                            egui::Button::new(
                                egui::RichText::new(format!("Move {n} to Trash"))
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(0xB0, 0x30, 0x30)),
                        );
                        // Focus the confirm button on open so Enter/Space commits
                        // without moving the mouse.
                        if ui.memory(|m| m.focused().is_none()) {
                            confirm.request_focus();
                        }
                        if confirm.clicked() {
                            spawn_batch = Some(batch.clone());
                            close = true;
                        }
                    });
                    ui.add_space(2.0);
                    ui.label(
                        egui::RichText::new("Enter to confirm · Esc to cancel")
                            .weak()
                            .small(),
                    );
                },
            );
            if close {
                self.dup_pending_bulk = None;
                self.dup_confirm_pos = None;
            }
        }
        if let Some(batch) = spawn_batch {
            // Hand off to the background worker; the view stays interactive and
            // reloads itself when the job reports Done.
            self.spawn_trash_marked(ui.ctx().clone(), batch);
        }

        if recompute {
            self.reload();
        }
    }

    /// Render the Missing files view: every track whose source file is gone from
    /// disk, as a review list. Each can be relocated (pick a folder; files found by
    /// name + content fingerprint are repointed) or removed — removal drops only the
    /// stale catalog row (and its playlist/analysis links), never a real file, since
    /// the file is already gone. Mirrors the Duplicates view's staged-action +
    /// confirmation pattern.
    /// The "Vinyl Collection" view: a grid of large cover icons backed by the
    /// local Discogs cache, with a Refresh button that re-syncs from Discogs.
    /// Records the user *wants* follow in their own Wantlist section below,
    /// rendered by the same grid from the same sync.
    pub(crate) fn draw_vinyl(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.is_busy();
        let mut refresh = false;
        // The user's Discogs collection page, known once a sync has resolved the
        // username. `None` until the first sync.
        let collection_url = {
            let u = self.config.discogs_username.trim();
            (!u.is_empty()).then(|| format!("https://www.discogs.com/user/{u}/collection"))
        };

        // Sort choice, persisted across launches. Read once per frame so the
        // menu below can write it back without borrowing the config twice.
        let mut sort = VinylSort::from_key(&self.config.vinyl_sort);
        let mut ascending = self.config.vinyl_sort_ascending;

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Vinyl Collection");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    if ui
                        .button("↻ Refresh")
                        .on_hover_note(
                            "Sync with Discogs, download missing covers and check prices",
                        )
                        .clicked()
                    {
                        refresh = true;
                    }
                });
                if let Some(url) = &collection_url {
                    if ui
                        .button("↗ Open in Discogs")
                        .on_hover_note("Open your collection on discogs.com")
                        .clicked()
                    {
                        open_url(url);
                    }
                }
                // Sort: field first, then a direction pair whose wording follows
                // the field ("Newest first" reads better than "Descending").
                let arrow = if ascending { "↑" } else { "↓" };
                ui.menu_button(format!("⇅ {} {arrow}", sort.label()), |ui| {
                    ui.set_min_width(170.0);
                    for option in [VinylSort::Added, VinylSort::Price, VinylSort::Artist] {
                        if ui
                            .selectable_label(sort == option, option.label())
                            .clicked()
                        {
                            sort = option;
                            ui.close_menu();
                        }
                    }
                    ui.separator();
                    let (asc_label, desc_label) = sort.direction_labels();
                    if ui.selectable_label(!ascending, desc_label).clicked() {
                        ascending = false;
                        ui.close_menu();
                    }
                    if ui.selectable_label(ascending, asc_label).clicked() {
                        ascending = true;
                        ui.close_menu();
                    }
                })
                .response
                .on_hover_note("Order both shelves by date added, price or artist");
            });
        });
        if sort.key() != self.config.vinyl_sort || ascending != self.config.vinyl_sort_ascending {
            self.config.vinyl_sort = sort.key().to_string();
            self.config.vinyl_sort_ascending = ascending;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save settings: {e}");
            }
        }
        ui.separator();

        if self.vinyl.is_empty() && self.wantlist.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No vinyl synced yet");
                    ui.add_space(6.0);
                    ui.label("Pull your record collection and wantlist straight from Discogs.");
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(
                            "Uses your Discogs token (set it in Settings). Records only — \
                             CDs and files in your collection are skipped.",
                        )
                        .weak(),
                    );
                    ui.add_space(14.0);
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui
                            .add(egui::Button::new(
                                egui::RichText::new("  ↻ Sync from Discogs  ").size(15.0),
                            ))
                            .clicked()
                        {
                            refresh = true;
                        }
                    });
                });
            });
            if refresh {
                self.spawn_refresh_vinyl(ctx.clone());
            }
            return;
        }

        // Snapshot what we render so the scroll closure doesn't borrow the record
        // lists while we read the cover cache. Kick off cover decodes up front
        // (the request is deduplicated, so doing it every frame is cheap).
        let owned = self.vinyl_cells(VinylList::Collection, &self.vinyl, sort, ascending);
        let wanted = self.vinyl_cells(VinylList::Wantlist, &self.wantlist, sort, ascending);
        for c in owned.iter().chain(wanted.iter()) {
            if c.has_cover {
                self.request_vinyl_cover(c.key);
            }
        }
        // The user's Discogs wantlist page, for the section's own link out.
        let wantlist_url = {
            let u = self.config.discogs_username.trim();
            (!u.is_empty()).then(|| format!("https://www.discogs.com/user/{u}/wants"))
        };

        ui.add_space(4.0);
        // What the user asked of a cell (jump to the catalog, or a list edit).
        // Applied after the grid so we don't mutate `self` mid-render.
        let mut action: Option<VinylGridAction> = None;
        // A record the dig strip asked to open, applied with `action` below.
        let mut open_sheet: Option<dig::DigOpen> = None;
        // The dig strip sits above both shelves rather than inside the scroll
        // area: it's the thing you're steering, so it shouldn't scroll away
        // under the wall of covers you're steering past.
        if let Some(key) = self.draw_dig(ui) {
            open_sheet = Some(key);
        }
        if self.dig.is_some() {
            ui.add_space(8.0);
        }
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);
            if owned.is_empty() {
                ui.label(egui::RichText::new("Nothing in your Discogs collection yet.").weak());
            } else if let Some(a) = self.vinyl_grid(ui, &owned) {
                action = Some(a);
            }
            // Wantlist: the same grid under its own header, so records you want
            // read as a distinct shelf rather than blending into what you own.
            if !wanted.is_empty() {
                ui.add_space(18.0);
                ui.horizontal(|ui| {
                    ui.heading(format!("Wantlist ({})", wanted.len()));
                    if let Some(url) = &wantlist_url {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .button("↗ Open in Discogs")
                                .on_hover_note("Open your wantlist on discogs.com")
                                .clicked()
                            {
                                open_url(url);
                            }
                        });
                    }
                });
                ui.label(egui::RichText::new("Records you want but don't own yet.").weak());
                ui.separator();
                ui.add_space(4.0);
                if let Some(a) = self.vinyl_grid(ui, &wanted) {
                    action = Some(a);
                }
            }
            ui.add_space(8.0);
        });

        if refresh {
            self.spawn_refresh_vinyl(ctx.clone());
        }
        match action {
            Some(VinylGridAction::Goto(album, tracks)) => {
                self.jump_to_catalog_tracks(album, tracks)
            }
            Some(VinylGridAction::Move(key)) => {
                if let Some(record) = self.vinyl_record(key) {
                    self.request_vinyl_edit(
                        ctx.clone(),
                        VinylEdit::Move {
                            from: key.0,
                            record: Box::new(record),
                        },
                    );
                }
            }
            Some(VinylGridAction::Remove(key)) => {
                if let Some(record) = self.vinyl_record(key) {
                    self.request_vinyl_edit(
                        ctx.clone(),
                        VinylEdit::Remove {
                            list: key.0,
                            record: Box::new(record),
                        },
                    );
                }
            }
            Some(VinylGridAction::Open(key)) => self.open_vinyl_sheet(key, ctx),
            Some(VinylGridAction::Dig(key)) => self.start_dig(key),
            Some(VinylGridAction::Play(key)) => {
                // The tracklist may still be loading; the sheet starts playback
                // itself once it has one (see `pending_play`).
                self.open_vinyl_sheet(key, ctx);
                if let Some(sheet) = self.vinyl_sheet.as_mut() {
                    sheet.pending_play = true;
                }
            }
            None => {}
        }
        if let Some(o) = open_sheet {
            self.open_release_sheet(o.release_id, o.artist, o.title, o.sub, o.cover_url, ctx);
        }
    }

    /// Look up the cached record a grid cell stands for. `None` if the lists
    /// changed under the click (a sync landing mid-frame), in which case the
    /// action is simply dropped rather than applied to the wrong record.
    /// The cached row for one release in one list, found by *release* id. The
    /// record sheet can be open on a record it has no cache key for (one reached
    /// by a dig), so a list edit from there resolves the row this way instead.
    pub(crate) fn vinyl_record_in(&self, list: VinylList, release_id: u64) -> Option<VinylRecord> {
        let records = match list {
            VinylList::Collection => &self.vinyl,
            VinylList::Wantlist => &self.wantlist,
        };
        records.iter().find(|v| v.release_id == release_id).cloned()
    }

    pub(crate) fn vinyl_record(&self, (list, instance_id): VinylCoverKey) -> Option<VinylRecord> {
        let records = match list {
            VinylList::Collection => &self.vinyl,
            VinylList::Wantlist => &self.wantlist,
        };
        records
            .iter()
            .find(|r| r.instance_id == instance_id)
            .cloned()
    }

    /// Run a vinyl list edit, or park it for confirmation first when it would
    /// destroy a collection copy on Discogs — that copy's date added, rating and
    /// notes go with it, and Ordnung can't put them back.
    pub(crate) fn request_vinyl_edit(&mut self, ctx: egui::Context, edit: VinylEdit) {
        if edit.destroys_collection_copy() {
            self.confirm_vinyl_edit = Some(edit);
        } else {
            self.spawn_vinyl_edit(ctx, edit);
        }
    }

    /// Build the render-ready cells for one vinyl list: display strings resolved
    /// and catalog links looked up, so the grid closure never borrows the record
    /// lists themselves. Ordered by the view's current sort — the records arrive
    /// from the catalog in artist order, so that's what `Artist` falls back to.
    ///
    /// Records missing the sort field (never priced, or no date added) always
    /// sink to the end, in either direction: flipping to "cheapest first" should
    /// surface your cheapest *known* price, not a wall of unpriced sleeves.
    fn vinyl_cells(
        &self,
        list: VinylList,
        records: &[VinylRecord],
        sort: VinylSort,
        ascending: bool,
    ) -> Vec<VinylCell> {
        let mut cells = self.vinyl_cells_unsorted(list, records);
        sort_vinyl_cells(&mut cells, sort, ascending);
        cells
    }

    /// The cells for one list in catalog order (artist, then title), before the
    /// view's sort is applied.
    fn vinyl_cells_unsorted(&self, list: VinylList, records: &[VinylRecord]) -> Vec<VinylCell> {
        records
            .iter()
            .map(|v| {
                let sub = match (v.year, v.format.as_deref()) {
                    (Some(y), Some(f)) => format!("{y} · {f}"),
                    (Some(y), None) => y.to_string(),
                    (None, Some(f)) => f.to_string(),
                    (None, None) => String::new(),
                };
                VinylCell {
                    key: (list, v.instance_id),
                    release_id: v.release_id,
                    title: if v.title.trim().is_empty() {
                        "Untitled".to_string()
                    } else {
                        v.title.clone()
                    },
                    artist: v.artist.clone(),
                    sub,
                    has_cover: v.has_cover,
                    linked: self
                        .vinyl_links
                        .get(&v.release_id)
                        .cloned()
                        .unwrap_or_default(),
                    also_in_other: match list {
                        VinylList::Collection => self.vinyl_wanted.contains(&v.release_id),
                        VinylList::Wantlist => self.vinyl_owned.contains(&v.release_id),
                    },
                    added: v.added.clone(),
                    price: v.price,
                    price_currency: v.price_currency.clone(),
                }
            })
            .collect()
    }

    /// Paint one wrapping grid of vinyl covers. Returns whatever the user asked
    /// for by clicking a cell's badge or picking from its right-click menu, for
    /// the caller to apply after the frame's borrows are released.
    fn vinyl_grid(&self, ui: &mut egui::Ui, cells: &[VinylCell]) -> Option<VinylGridAction> {
        /// Side length of each cover icon in points — deliberately large so the
        /// grid reads as a record wall rather than a list.
        const COVER: f32 = 150.0;
        /// Gap between cells (and the width budget for the caption under each).
        const GAP: f32 = 14.0;

        // The record whose video is playing in the mini-player, so its cover
        // keeps a visible pause disc while the wall scrolls.
        let playing_key = self
            .vinyl_sheet
            .as_ref()
            .filter(|s| s.playing_video.is_some())
            .and_then(|s| s.key);

        let mut action: Option<VinylGridAction> = None;
        // Top-aligned wrapping rather than `horizontal_wrapped`: that helper
        // centres cells vertically in the row, and egui snaps only the first
        // cell of a row to the row's top — so any row taller than a cell left
        // the leading cover sitting higher than its neighbours.
        let wrap = egui::Layout::left_to_right(egui::Align::TOP).with_main_wrap(true);
        ui.with_layout(wrap, |ui| {
            ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
            for c in cells {
                let tex = match self.vinyl_covers.get(&c.key) {
                    Some(ThumbState::Ready(Some(t))) => Some(t.clone()),
                    _ => None,
                };
                // One cell: cover icon + two caption lines, all clipped to the
                // cover width so long titles don't break the grid alignment.
                let release_url = format!("https://www.discogs.com/release/{}", c.release_id);
                ui.allocate_ui_with_layout(
                    egui::vec2(COVER, COVER + 42.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        // The cover is a link to the release page on Discogs —
                        // click-sensing, with a hand cursor on hover.
                        let (rect, resp) =
                            ui.allocate_exact_size(egui::vec2(COVER, COVER), egui::Sense::click());
                        let resp = resp.on_hover_cursor(egui::CursorIcon::PointingHand);
                        match &tex {
                            Some(h) => {
                                egui::Image::new(h)
                                    .fit_to_exact_size(egui::vec2(COVER, COVER))
                                    .rounding(egui::Rounding::same(6.0))
                                    .paint_at(ui, rect);
                            }
                            None => {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::Rounding::same(6.0),
                                    egui::Color32::from_gray(34),
                                );
                                ui.painter().text(
                                    rect.center(),
                                    egui::Align2::CENTER_CENTER,
                                    "💿",
                                    egui::FontId::proportional(40.0),
                                    egui::Color32::from_gray(90),
                                );
                            }
                        }
                        // Subtle hover frame to signal the cover is clickable.
                        if resp.hovered() {
                            ui.painter().rect_stroke(
                                rect,
                                egui::Rounding::same(6.0),
                                egui::Stroke::new(2.0, egui::Color32::from_rgb(90, 200, 120)),
                            );
                        }
                        // "In your catalog" badge: a small chip pinned to the
                        // top-right corner of records you already own a digital
                        // copy of. Sits on top of the cover and takes click
                        // priority so tapping it jumps to the catalog instead of
                        // opening Discogs.
                        let mut badge_clicked = false;
                        if !c.linked.is_empty() {
                            const B: f32 = 22.0;
                            let badge_rect = egui::Rect::from_min_size(
                                egui::pos2(rect.right() - B - 4.0, rect.top() + 4.0),
                                egui::vec2(B, B),
                            );
                            let badge = ui.interact(
                                badge_rect,
                                ui.id().with(("vinyl-cat", c.key)),
                                egui::Sense::click(),
                            );
                            let bg = if badge.hovered() {
                                egui::Color32::from_rgb(120, 220, 150)
                            } else {
                                egui::Color32::from_rgb(90, 200, 120)
                            };
                            ui.painter()
                                .rect_filled(badge_rect, egui::Rounding::same(5.0), bg);
                            ui.painter().text(
                                badge_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "♪",
                                egui::FontId::proportional(14.0),
                                egui::Color32::from_gray(20),
                            );
                            let n = c.linked.len();
                            let tip = if n > 1 {
                                format!("In your catalog ({n} tracks). Click to show.")
                            } else {
                                "In your catalog. Click to show.".to_string()
                            };
                            let badge = badge.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if badge.on_hover_note(tip).clicked() {
                                badge_clicked = true;
                                action =
                                    Some(VinylGridAction::Goto(c.title.clone(), c.linked.clone()));
                            }
                        }
                        // Play disc, bottom-right: start the record. Shown on
                        // hover, so the wall stays a wall until you reach for it.
                        //
                        // The hit area is registered every frame, not only while
                        // the disc is visible: it sits on top of the cover, so
                        // pointing at it takes the hover *away* from the cover.
                        // Gating the whole widget on the cover's hover therefore
                        // made the disc vanish from under the cursor and dropped
                        // the click onto the cover behind it. Visibility follows
                        // either hover instead.
                        let mut play_clicked = false;
                        const D: f32 = 30.0;
                        let disc = egui::Rect::from_min_size(
                            egui::pos2(rect.right() - D - 6.0, rect.bottom() - D - 6.0),
                            egui::vec2(D, D),
                        );
                        let hit = ui.interact(
                            disc,
                            ui.id().with(("vinyl-play", c.key)),
                            egui::Sense::click(),
                        );
                        let playing_this = playing_key == Some(c.key);
                        // Read before the block below consumes `hit`; the dig
                        // disc reveals on the play disc's hover too, so the pair
                        // appears and disappears together.
                        let play_hovered = hit.hovered();
                        if resp.hovered() || hit.hovered() || playing_this {
                            let bg = if hit.hovered() {
                                egui::Color32::from_rgb(120, 220, 150)
                            } else {
                                egui::Color32::from_black_alpha(190)
                            };
                            let fg = if hit.hovered() {
                                egui::Color32::from_gray(20)
                            } else {
                                egui::Color32::from_gray(240)
                            };
                            ui.painter().circle_filled(disc.center(), D / 2.0, bg);
                            let glyph = if playing_this { "❚❚" } else { "▶" };
                            ui.painter().text(
                                disc.center()
                                    + egui::vec2(if glyph == "▶" { 1.5 } else { 0.0 }, 0.0),
                                egui::Align2::CENTER_CENTER,
                                glyph,
                                egui::FontId::proportional(13.0),
                                fg,
                            );
                            let hit = hit.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if hit.on_hover_note("Play this record").clicked() {
                                play_clicked = true;
                                action = Some(VinylGridAction::Play(c.key));
                            }
                        }
                        // Dig disc, left of the play disc: start a crate dig
                        // from this record. Same hover-reveal and same
                        // always-registered hit area as the play disc above —
                        // see that comment for why the interact() is
                        // unconditional.
                        let mut dig_clicked = false;
                        let dig_rect = egui::Rect::from_min_size(
                            egui::pos2(disc.left() - D - 5.0, rect.bottom() - D - 6.0),
                            egui::vec2(D, D),
                        );
                        let dig_hit = ui.interact(
                            dig_rect,
                            ui.id().with(("vinyl-dig", c.key)),
                            egui::Sense::click(),
                        );
                        if resp.hovered() || dig_hit.hovered() || play_hovered {
                            let bg = if dig_hit.hovered() {
                                egui::Color32::from_rgb(120, 220, 150)
                            } else {
                                egui::Color32::from_black_alpha(190)
                            };
                            let fg = if dig_hit.hovered() {
                                egui::Color32::from_gray(20)
                            } else {
                                egui::Color32::from_gray(240)
                            };
                            ui.painter().circle_filled(dig_rect.center(), D / 2.0, bg);
                            ui.painter().text(
                                dig_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "⛏",
                                egui::FontId::proportional(14.0),
                                fg,
                            );
                            let dig_hit = dig_hit.on_hover_cursor(egui::CursorIcon::PointingHand);
                            if dig_hit
                                .on_hover_note(
                                    "Dig from here: records like this on Discogs that \
                                     aren't in your collection",
                                )
                                .clicked()
                            {
                                dig_clicked = true;
                                action = Some(VinylGridAction::Dig(c.key));
                            }
                        }
                        // Price chip, bottom-left of the cover: what the sort is
                        // ordering by, shown where it can't push the caption
                        // around. Absent until a sync has priced this record.
                        if let Some(p) = c.price {
                            let text = format_price(p, c.price_currency.as_deref(), false);
                            let galley = ui.painter().layout_no_wrap(
                                text,
                                egui::FontId::proportional(12.0),
                                egui::Color32::from_gray(240),
                            );
                            let pad = egui::vec2(6.0, 3.0);
                            let size = galley.size() + pad * 2.0;
                            let chip = egui::Rect::from_min_size(
                                egui::pos2(rect.left() + 4.0, rect.bottom() - size.y - 4.0),
                                size,
                            );
                            ui.painter().rect_filled(
                                chip,
                                egui::Rounding::same(5.0),
                                egui::Color32::from_black_alpha(190),
                            );
                            ui.painter()
                                .galley(chip.min + pad, galley, egui::Color32::WHITE);
                        }
                        let price_line = match c.price {
                            Some(p) => format!(
                                "\nFrom {} on Discogs",
                                format_price(p, c.price_currency.as_deref(), true)
                            ),
                            None => String::new(),
                        };
                        let tip = if c.sub.is_empty() {
                            format!(
                                "{}\n{}{price_line}\n\nShow the tracklist",
                                c.artist, c.title
                            )
                        } else {
                            format!(
                                "{}\n{}\n{}{price_line}\n\nShow the tracklist",
                                c.artist, c.title, c.sub
                            )
                        };
                        // The cover opens the record sheet — but not when the
                        // click landed on the catalog badge or the play disc
                        // layered above it. Discogs itself is one click further
                        // in, from the sheet or the menu below.
                        let resp = resp.on_hover_note(tip);
                        if resp.clicked() && !badge_clicked && !play_clicked && !dig_clicked {
                            action = Some(VinylGridAction::Open(c.key));
                        }
                        // Right-click: move this record between the two lists, or
                        // drop it. Both write straight to the user's Discogs
                        // account, so the wording says which list is which rather
                        // than a bare "Move".
                        let (list, _) = c.key;
                        resp.context_menu(|ui| {
                            ui.label(egui::RichText::new(&c.title).strong());
                            ui.label(egui::RichText::new(&c.artist).weak());
                            ui.separator();
                            if ui
                                .button("Open on Discogs ↗")
                                .on_hover_note("Open this release on discogs.com")
                                .clicked()
                            {
                                open_url(&release_url);
                                ui.close_menu();
                            }
                            if ui
                                .button("⛏  Dig from here")
                                .on_hover_note(
                                    "Walk the collection from this record, by artist or label",
                                )
                                .clicked()
                            {
                                action = Some(VinylGridAction::Dig(c.key));
                                ui.close_menu();
                            }
                            ui.separator();
                            let (move_label, move_tip, remove_label) = match list {
                                VinylList::Collection => (
                                    "Move to wantlist",
                                    "Give up this copy on Discogs and want it instead",
                                    "Remove from collection",
                                ),
                                VinylList::Wantlist => (
                                    "Move to collection",
                                    "Mark this record as owned on Discogs",
                                    "Remove from wantlist",
                                ),
                            };
                            // Already in both lists: the move has nothing to do,
                            // and running it would ask Discogs for a duplicate
                            // copy. Say where it already is instead.
                            let (move_label, move_tip) = if c.also_in_other {
                                match list {
                                    VinylList::Collection => (
                                        "✓  Already in your wantlist",
                                        "This record is in both lists on Discogs",
                                    ),
                                    VinylList::Wantlist => (
                                        "✓  Already in your collection",
                                        "You already own this on Discogs",
                                    ),
                                }
                            } else {
                                (move_label, move_tip)
                            };
                            if ui
                                .add_enabled(!c.also_in_other, egui::Button::new(move_label))
                                .on_hover_note(move_tip)
                                .on_disabled_hover_text(move_tip)
                                .clicked()
                            {
                                action = Some(VinylGridAction::Move(c.key));
                                ui.close_menu();
                            }
                            if ui
                                .button(remove_label)
                                .on_hover_note("Delete it from this Discogs list")
                                .clicked()
                            {
                                action = Some(VinylGridAction::Remove(c.key));
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui.button("Open on Discogs ↗").clicked() {
                                open_url(&release_url);
                                ui.close_menu();
                            }
                        });
                        ui.set_max_width(COVER);
                        ui.add_space(4.0);
                        // Title doubles as the textual link to the release page.
                        let title = ui.add(
                            egui::Label::new(egui::RichText::new(&c.title).strong())
                                .truncate()
                                .sense(egui::Sense::click()),
                        );
                        if title
                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                            .clicked()
                        {
                            open_url(&release_url);
                        }
                        ui.add(egui::Label::new(egui::RichText::new(&c.artist).weak()).truncate());
                    },
                );
            }
        });
        action
    }

    /// Jump from the vinyl grid into the catalog: show the full library narrowed
    /// to this release's album (so only its songs are listed), then select and
    /// reveal the linked `tracks`. `release_title` is the Discogs title, used as
    /// a fallback. The table scrolls to and highlights the first track next frame.
    pub(crate) fn jump_to_catalog_tracks(&mut self, release_title: String, tracks: Vec<Id>) {
        if tracks.is_empty() {
            return;
        }
        // Filter by the linked track's *own* album text, not the Discogs release
        // title — they can differ (the track may keep its original album tag),
        // and filtering by a title the track doesn't carry would hide it. Fall
        // back to the release title only when the track has no album.
        let album = Catalog::open(&self.db_path)
            .ok()
            .and_then(|c| c.get_track(tracks[0]).ok())
            .and_then(|t| t.tags.album)
            .filter(|a| !a.trim().is_empty())
            .unwrap_or(release_title);

        self.view = LibraryView::Library;
        self.filter = album;
        self.col_filters.clear();
        // Rebuild the (now filtered) Library rows first; `reload` prunes the
        // selection to live rows, so seed the selection *after* it.
        self.reload();
        self.selection = tracks.iter().copied().collect();
        self.selected = tracks.first().copied();
        self.select_anchor = self.selected;
        self.scroll_to_track = self.selected;
        self.refresh_selected();
    }

    pub(crate) fn draw_missing(&mut self, ui: &mut egui::Ui) {
        // Snapshot what we draw so the egui closures don't borrow `self` (needed
        // mutably afterwards to apply actions).
        struct MissingView {
            id: Id,
            title: String,
            path: PathBuf,
        }
        let items: Vec<MissingView> = self
            .missing_list
            .iter()
            .map(|t| MissingView {
                id: t.id,
                title: format!(
                    "{} — {}",
                    t.tags.artist.as_deref().unwrap_or("—"),
                    t.tags.title.as_deref().unwrap_or("—"),
                ),
                path: PathBuf::from(&t.source_path),
            })
            .collect();
        let all_ids: Vec<Id> = items.iter().map(|m| m.id).collect();

        let mut relocate = false;
        let mut recompute = false;
        let mut remove_one: Option<Id> = None;
        let mut remove_all = false;
        // The tab the user clicked in the shared Library Health header, applied
        // after the layout closure releases its borrow of `self`.
        let mut switch_tab = None;
        // The duplicate cache is dropped while this view is up, so this tab has no
        // trustworthy group count to show.
        let dup_count = None;

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            switch_tab = health_tabs(ui, &self.view, dup_count, self.missing_count);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("↻ Recheck")
                    .on_hover_note("Re-scan disk for tracks whose source file is gone")
                    .clicked()
                {
                    recompute = true;
                }
                if !items.is_empty()
                    && ui
                        .add(
                            egui::Button::new(
                                egui::RichText::new("🔗 Find moved files…")
                                    .color(egui::Color32::WHITE),
                            )
                            .fill(egui::Color32::from_rgb(150, 90, 40)),
                        )
                        .on_hover_note(
                            "Search a folder and repoint matches in the catalog. \
                             Files are never modified.",
                        )
                        .clicked()
                {
                    relocate = true;
                }
            });
        });

        // Switching tabs swaps the whole body, so stop drawing this one's.
        if let Some(tab) = switch_tab {
            self.open_health_tab(tab, ui.ctx());
            return;
        }

        if items.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("Nothing missing ✓");
                ui.label("Every track's source file is present on disk.");
            });
            if recompute {
                self.reload();
            }
            return;
        }

        ui.label(
            egui::RichText::new(format!(
                "{} track(s) point at a file that's no longer on disk. Relocate the ones you've \
                 moved, or remove the rest — removal only drops the catalog entry (and any \
                 playlist/analysis links); your files are never touched.",
                items.len()
            ))
            .weak(),
        );
        ui.separator();

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for m in &items {
                    ui.horizontal(|ui| {
                        if ui
                            .button("Remove")
                            .on_hover_note("Remove this entry from the catalog")
                            .clicked()
                        {
                            remove_one = Some(m.id);
                        }
                        ui.label(egui::RichText::new(&m.title).strong());
                    });
                    ui.label(
                        egui::RichText::new(m.path.display().to_string())
                            .monospace()
                            .weak(),
                    );
                    ui.add_space(4.0);
                }
            });

        ui.separator();
        if ui
            .button(format!("Remove all {}", all_ids.len()))
            .on_hover_note("Remove all missing entries from the catalog")
            .clicked()
        {
            remove_all = true;
        }

        // Apply the non-modal actions.
        if relocate {
            if let Some(dir) = rfd::FileDialog::new().pick_folder() {
                self.spawn_relocate(ui.ctx().clone(), dir);
            }
        }
        if let Some(id) = remove_one {
            self.missing_pending_remove = Some(vec![id]);
        }
        if remove_all {
            self.missing_pending_remove = Some(all_ids);
        }

        // Confirmation modal for a staged removal. Drops the catalog rows (files are
        // already gone), then recomputes so the list updates.
        if let Some(ids) = self.missing_pending_remove.clone() {
            let n = ids.len();
            let mut close = false;
            egui::Window::new("Remove missing tracks")
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ui.ctx().screen_rect().center())
                .show(ui.ctx(), |ui| {
                    ui.label(format!(
                        "Remove {n} missing track{} from the catalog? Their source file{} \
                         already gone — this only deletes the catalog entr{} (and any \
                         playlist/analysis links). No files on disk are touched.",
                        if n == 1 { "" } else { "s" },
                        if n == 1 { " is" } else { "s are" },
                        if n == 1 { "y" } else { "ies" },
                    ));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                        if ui
                            .add(
                                egui::Button::new(
                                    egui::RichText::new(format!("Remove {n}"))
                                        .color(egui::Color32::WHITE),
                                )
                                .fill(egui::Color32::from_rgb(0xB0, 0x30, 0x30)),
                            )
                            .clicked()
                        {
                            match Catalog::open(&self.db_path) {
                                Ok(c) => match c.delete_tracks(&ids) {
                                    Ok(removed) => {
                                        self.status = format!(
                                            "Removed {removed} missing track(s) from the catalog."
                                        );
                                        recompute = true;
                                    }
                                    Err(e) => self.status = format!("Couldn't remove: {e}"),
                                },
                                Err(e) => self.status = format!("Couldn't open catalog: {e}"),
                            }
                            close = true;
                        }
                    });
                });
            if close {
                self.missing_pending_remove = None;
            }
        }

        if recompute {
            self.reload();
        }
    }

    /// The USB device view: every audio file on the mounted volume, scanned
    /// straight off the device (nothing here touches the catalog). A row click
    /// opens direct tag editing — Save writes to the file on the stick. When
    /// the volume is a rekordbox export, a banner warns that players read
    /// export.pdb/ANLZ, not file tags, so direct edits desync until re-export.
    pub(crate) fn draw_usb(&mut self, ui: &mut egui::Ui, vol: &Path) -> Option<Vec<PathBuf>> {
        let name = vol
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| vol.display().to_string());
        let is_rekordbox = self
            .usb_volumes
            .iter()
            .any(|v| v.path == vol && v.is_rekordbox_export);
        // When a playlist from the export is selected, name it in the header.
        let playlist_name = match &self.view {
            LibraryView::Usb(_, Some(pid)) => self
                .usb_playlists
                .iter()
                .find(|p| p.id == *pid)
                .map(|p| p.name.clone()),
            _ => None,
        };

        let mut rescan = false;
        let mut eject = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            match &playlist_name {
                Some(pl) => ui.heading(format!("⏏  {name}  ▸  ♪ {pl}")),
                None => ui.heading(format!("⏏  {name}")),
            };
            if !self.usb_loading {
                ui.label(egui::RichText::new(format!("{} track(s)", self.rows.len())).weak());
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .button("⏏ Eject")
                    .on_hover_note("Unmount the volume so it's safe to unplug")
                    .clicked()
                {
                    eject = true;
                }
                if ui
                    .button("Show in Finder")
                    .on_hover_note("Open the volume in Finder")
                    .clicked()
                {
                    let _ = std::process::Command::new("open").arg(vol).spawn();
                }
                if ui
                    .button("↻ Rescan")
                    .on_hover_note("Re-read the device's files")
                    .clicked()
                {
                    rescan = true;
                }
            });
        });
        if is_rekordbox {
            // The desync warning. Player-facing metadata on a rekordbox stick
            // lives in derived files, so direct edits are invisible to CDJs.
            egui::Frame::none()
                .fill(egui::Color32::from_rgb(60, 50, 25))
                .rounding(egui::Rounding::same(6.0))
                .inner_margin(egui::Margin::symmetric(10.0, 8.0))
                .show(ui, |ui| {
                    ui.label(
                        egui::RichText::new(
                            "rekordbox export detected. CDJs read track titles, BPM, \
                             beatgrids and waveforms from PIONEER/rekordbox/export.pdb \
                             and the ANLZ analysis files, not from the audio files' own \
                             tags. Tags edited here won't show on players, and replacing \
                             audio desyncs waveforms and file sizes, until the USB is \
                             re-exported.",
                        )
                        .color(egui::Color32::from_rgb(230, 200, 120)),
                    );
                });
            ui.add_space(4.0);
        }

        if eject {
            // Hand the unmount to diskutil on a worker (it can take seconds)
            // and report its actual outcome — an eject refused because a file
            // is open would otherwise look like a dead button. On success the
            // sidebar poll notices the volume disappear and the view drops
            // back to the Library.
            let vol = vol.to_path_buf();
            let n = name.clone();
            let (tx, rx) = std::sync::mpsc::channel();
            self.usb_eject_rx = Some(rx);
            let ctx = ui.ctx().clone();
            std::thread::spawn(move || {
                let msg = match std::process::Command::new("diskutil")
                    .arg("eject")
                    .arg(&vol)
                    .output()
                {
                    Ok(o) if o.status.success() => format!("Ejected {n}. Safe to unplug."),
                    Ok(o) => {
                        // diskutil writes some refusals to stdout, not stderr.
                        let err = String::from_utf8_lossy(&o.stderr).trim().to_string();
                        let err = if err.is_empty() {
                            String::from_utf8_lossy(&o.stdout).trim().to_string()
                        } else {
                            err
                        };
                        // Translate diskutil's jargon ("dissented by PID 501")
                        // into something actionable, asking lsof which apps
                        // actually hold files open on the drive.
                        eject_failure_message(&n, &err, &volume_users(&vol))
                    }
                    Err(e) => format!("Couldn't eject {n}: {e}"),
                };
                let _ = tx.send(msg);
                ctx.request_repaint();
            });
            self.status = format!("Ejecting {name}…");
        }
        if rescan {
            self.usb_loaded_for = None; // poll_usb respawns the scan
            return None;
        }

        if self.usb_loading {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label("Scanning device…");
            });
            return None;
        }
        if self.usb_tracks.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("No audio files");
                ui.label("Nothing playable was found on this volume.");
            });
            return None;
        }
        if self.rows.is_empty() {
            // The playlist resolved to nothing (files missing from /Contents)
            // or the search filter hid every row.
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("No tracks to show");
                ui.label(if self.filter.trim().is_empty() {
                    "This playlist's files weren't found on the device."
                } else {
                    "Nothing on this device matches the search."
                });
            });
            return None;
        }

        // The direct tag editor follows the table's (single) selection: the
        // synthetic row id decodes back to an index into `usb_tracks`.
        let table_sel = self
            .selected
            .and_then(usb_track_index)
            .filter(|i| *i < self.usb_tracks.len());
        if table_sel != self.usb_selected {
            self.usb_selected = table_sel;
            if let Some(i) = table_sel {
                self.usb_edit = UsbEdit::from_tags(&self.usb_tracks[i].tags);
                self.usb_edit_saved = self.usb_edit.clone();
            }
        }

        // ── Bottom edit panel (pinned, so the table scroll can fill the rest) ──
        if let Some(i) = self.usb_selected.filter(|i| *i < self.usb_tracks.len()) {
            let file = PathBuf::from(&self.usb_tracks[i].source_path);
            let fname = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let dirty = self.usb_edit != self.usb_edit_saved;
            let mut save = false;
            egui::TopBottomPanel::bottom("usb_edit_panel")
                .frame(egui::Frame::none())
                .show_separator_line(false)
                .show_inside(ui, |ui| {
                    ui.add_space(6.0);
                    ui.separator();
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.label(egui::RichText::new(&fname).strong());
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if crate::ui::icon::close_button(ui, "Close the editor") {
                                // Also drop the table selection, or the
                                // panel would re-open from it next frame.
                                self.usb_selected = None;
                                self.selected = None;
                                self.selection.clear();
                            }
                            if ui
                                .add_enabled(
                                    dirty,
                                    egui::Button::new(
                                        egui::RichText::new("Save to file")
                                            .color(egui::Color32::WHITE),
                                    )
                                    .fill(crate::sidebar::NAV_ACCENT),
                                )
                                .on_hover_note("Write these tags into the file on the device")
                                .clicked()
                            {
                                save = true;
                            }
                            if ui
                                .button("Reveal")
                                .on_hover_note("Show this file in Finder")
                                .clicked()
                            {
                                reveal_in_finder(&file);
                            }
                        });
                    });
                    ui.add_space(4.0);
                    egui::Grid::new("usb_tag_grid")
                        .num_columns(4)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            let field = |ui: &mut egui::Ui, label: &str, buf: &mut String| {
                                ui.label(egui::RichText::new(label).weak());
                                ui.add(egui::TextEdit::singleline(buf).desired_width(220.0));
                            };
                            field(ui, "Title", &mut self.usb_edit.title);
                            field(ui, "Artist", &mut self.usb_edit.artist);
                            ui.end_row();
                            field(ui, "Album", &mut self.usb_edit.album);
                            field(ui, "Genre", &mut self.usb_edit.genre);
                            ui.end_row();
                            field(ui, "Comment", &mut self.usb_edit.comment);
                            ui.end_row();
                        });
                    ui.add_space(6.0);
                });
            if save {
                let mut tags = self.usb_tracks[i].tags.clone();
                self.usb_edit.apply_to(&mut tags);
                match tag::write_to_file(&file, &tags, None) {
                    Ok(()) => {
                        // Re-read the file so the row reflects exactly what
                        // landed on the device (and pick up any tag rewrite).
                        if let Ok(fresh) = scan::scan_file(&file) {
                            self.usb_tracks[i] = fresh;
                        } else {
                            self.usb_tracks[i].tags = tags;
                        }
                        self.usb_edit_saved = self.usb_edit.clone();
                        self.status = format!("Saved tags to {fname}.");
                        // Rebuild the table rows so they show the saved tags.
                        self.reload();
                    }
                    Err(e) => self.status = format!("Couldn't write {fname}: {e}"),
                }
            }
        }

        // ── Track table ──────────────────────────────────────────────────
        // The same table as the library — sorting, filters, per-row play,
        // ⌥-drag file drag-out — fed by `usb_rows` (built in `reload`).
        let mut native_drag = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                native_drag = self.draw_table(ui);
            });
        native_drag
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A cell carrying only what the sort reads.
    fn cell(artist: &str, added: Option<&str>, price: Option<f64>) -> VinylCell {
        VinylCell {
            key: (VinylList::Collection, 1),
            release_id: 1,
            title: String::new(),
            artist: artist.into(),
            sub: String::new(),
            has_cover: false,
            linked: Vec::new(),
            also_in_other: false,
            added: added.map(str::to_string),
            price,
            price_currency: price.map(|_| "USD".to_string()),
        }
    }

    fn artists(cells: &[VinylCell]) -> Vec<&str> {
        cells.iter().map(|c| c.artist.as_str()).collect()
    }

    /// The grid arrives in artist order, so `Artist` only flips it.
    #[test]
    fn artist_sort_keeps_catalog_order_and_reverses_it() {
        let mut cells = vec![cell("A", None, None), cell("B", None, None)];
        sort_vinyl_cells(&mut cells, VinylSort::Artist, true);
        assert_eq!(artists(&cells), ["A", "B"]);
        sort_vinyl_cells(&mut cells, VinylSort::Artist, false);
        assert_eq!(artists(&cells), ["B", "A"]);
    }

    #[test]
    fn added_sorts_by_discogs_date_in_both_directions() {
        let build = || {
            vec![
                cell("mid", Some("2022-06-01T00:00:00-07:00"), None),
                cell("newest", Some("2024-01-02T00:00:00-08:00"), None),
                cell("oldest", Some("2019-03-04T00:00:00-08:00"), None),
            ]
        };
        let mut cells = build();
        sort_vinyl_cells(&mut cells, VinylSort::Added, false);
        assert_eq!(artists(&cells), ["newest", "mid", "oldest"]);
        let mut cells = build();
        sort_vinyl_cells(&mut cells, VinylSort::Added, true);
        assert_eq!(artists(&cells), ["oldest", "mid", "newest"]);
    }

    #[test]
    fn price_sorts_by_value_in_both_directions() {
        let build = || {
            vec![
                cell("mid", None, Some(16.09)),
                cell("dear", None, Some(120.0)),
                cell("cheap", None, Some(5.03)),
            ]
        };
        let mut cells = build();
        sort_vinyl_cells(&mut cells, VinylSort::Price, false);
        assert_eq!(artists(&cells), ["dear", "mid", "cheap"]);
        let mut cells = build();
        sort_vinyl_cells(&mut cells, VinylSort::Price, true);
        assert_eq!(artists(&cells), ["cheap", "mid", "dear"]);
    }

    /// Records with nothing to sort on sink to the bottom either way — otherwise
    /// "cheapest first" would open on a page of never-priced sleeves.
    #[test]
    fn records_missing_the_sort_field_sink_to_the_end() {
        for ascending in [true, false] {
            let mut cells = vec![
                cell("unpriced", None, None),
                cell("priced", None, Some(9.0)),
            ];
            sort_vinyl_cells(&mut cells, VinylSort::Price, ascending);
            assert_eq!(artists(&cells), ["priced", "unpriced"]);

            let mut cells = vec![
                cell("undated", None, None),
                cell("dated", Some("2020"), None),
            ];
            sort_vinyl_cells(&mut cells, VinylSort::Added, ascending);
            assert_eq!(artists(&cells), ["dated", "undated"]);
        }
    }

    #[test]
    fn sort_choice_survives_a_round_trip_through_the_config_key() {
        for sort in [VinylSort::Added, VinylSort::Price, VinylSort::Artist] {
            assert_eq!(VinylSort::from_key(sort.key()), sort);
        }
        // A key from a build that knows sorts this one doesn't.
        assert_eq!(VinylSort::from_key("condition"), VinylSort::Artist);
    }

    #[test]
    fn prices_render_with_a_symbol_when_the_currency_has_one() {
        assert_eq!(format_price(24.4, Some("USD"), false), "$24");
        assert_eq!(format_price(24.4, Some("USD"), true), "$24.40");
        assert_eq!(format_price(9.5, Some("EUR"), true), "€9.50");
        assert_eq!(format_price(9.5, Some("SEK"), true), "9.50 SEK");
        assert_eq!(format_price(9.5, None, true), "9.50");
    }
}
