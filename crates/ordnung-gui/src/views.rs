//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

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

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Duplicates");
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
    /// The "My Vinyl Collection" view: a grid of large cover icons backed by the
    /// local Discogs cache, with a Refresh button that re-syncs from Discogs.
    pub(crate) fn draw_vinyl(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        /// Side length of each cover icon in points — deliberately large so the
        /// grid reads as a record wall rather than a list.
        const COVER: f32 = 150.0;
        /// Gap between cells (and the width budget for the caption under each).
        const GAP: f32 = 14.0;

        let busy = self.is_busy();
        let mut refresh = false;
        // The user's Discogs collection page, known once a sync has resolved the
        // username. `None` until the first sync.
        let collection_url = {
            let u = self.config.discogs_username.trim();
            (!u.is_empty()).then(|| format!("https://www.discogs.com/user/{u}/collection"))
        };

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("My Vinyl Collection");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_enabled_ui(!busy, |ui| {
                    if ui
                        .button("↻ Refresh")
                        .on_hover_note("Sync with Discogs and download missing covers")
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
            });
        });
        ui.separator();

        if self.vinyl.is_empty() {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No vinyl synced yet");
                    ui.add_space(6.0);
                    ui.label("Pull your record collection straight from Discogs.");
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

        // Snapshot what we render so the scroll closure doesn't borrow `self.vinyl`
        // while we read the cover cache. Kick off cover decodes up front (the
        // request is deduplicated, so doing it every frame is cheap).
        struct Cell {
            instance_id: u64,
            release_id: u64,
            title: String,
            artist: String,
            sub: String,
            has_cover: bool,
            /// Catalog track ids linked to this release — empty if you don't own
            /// a digital copy. Drives the "in catalog" badge and the jump-to.
            linked: Vec<Id>,
        }
        let cells: Vec<Cell> = self
            .vinyl
            .iter()
            .map(|v| {
                let sub = match (v.year, v.format.as_deref()) {
                    (Some(y), Some(f)) => format!("{y} · {f}"),
                    (Some(y), None) => y.to_string(),
                    (None, Some(f)) => f.to_string(),
                    (None, None) => String::new(),
                };
                Cell {
                    instance_id: v.instance_id,
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
                }
            })
            .collect();
        for c in &cells {
            if c.has_cover {
                self.request_vinyl_cover(c.instance_id);
            }
        }

        ui.add_space(4.0);
        // Set when a cell's "in catalog" badge is clicked: the release title (to
        // filter the catalog by that album) and the linked track ids (to select).
        // Applied after the grid so we don't mutate `self` mid-render.
        let mut goto: Option<(String, Vec<Id>)> = None;
        egui::ScrollArea::vertical().show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);
                for c in &cells {
                    let tex = match self.vinyl_covers.get(&c.instance_id) {
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
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(COVER, COVER),
                                egui::Sense::click(),
                            );
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
                                    ui.id().with(("vinyl-cat", c.instance_id)),
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
                                    goto = Some((c.title.clone(), c.linked.clone()));
                                }
                            }
                            let tip = if c.sub.is_empty() {
                                format!("{}\n{}\n\nOpen on Discogs ↗", c.artist, c.title)
                            } else {
                                format!("{}\n{}\n{}\n\nOpen on Discogs ↗", c.artist, c.title, c.sub)
                            };
                            // The cover opens Discogs — but not when the click landed
                            // on the catalog badge layered above it.
                            if resp.on_hover_note(tip).clicked() && !badge_clicked {
                                open_url(&release_url);
                            }
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
                            ui.add(
                                egui::Label::new(egui::RichText::new(&c.artist).weak()).truncate(),
                            );
                        },
                    );
                }
            });
            ui.add_space(8.0);
        });

        if refresh {
            self.spawn_refresh_vinyl(ctx.clone());
        }
        if let Some((album, tracks)) = goto {
            self.jump_to_catalog_tracks(album, tracks);
        }
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

        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading("Missing files");
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
    pub(crate) fn draw_usb(&mut self, ui: &mut egui::Ui, vol: &Path) {
        let name = vol
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| vol.display().to_string());
        let is_rekordbox = self
            .usb_volumes
            .iter()
            .any(|v| v.path == vol && v.is_rekordbox_export);

        let mut rescan = false;
        let mut eject = false;
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.heading(format!("⏏  {name}"));
            if !self.usb_loading {
                ui.label(
                    egui::RichText::new(format!("{} track(s)", self.usb_tracks.len())).weak(),
                );
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
                        format!("Couldn't eject {n}: {err}")
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
            return;
        }

        if self.usb_loading {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.spinner();
                ui.label("Scanning device…");
            });
            return;
        }
        if self.usb_tracks.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("No audio files");
                ui.label("Nothing playable was found on this volume.");
            });
            return;
        }

        // ── Bottom edit panel (pinned, so the list scroll can fill the rest) ──
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
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                if ui.button("✕").on_hover_note("Close the editor").clicked() {
                                    self.usb_selected = None;
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
                            },
                        );
                    });
                    ui.add_space(4.0);
                    egui::Grid::new("usb_tag_grid")
                        .num_columns(4)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            let field = |ui: &mut egui::Ui, label: &str, buf: &mut String| {
                                ui.label(egui::RichText::new(label).weak());
                                ui.add(
                                    egui::TextEdit::singleline(buf).desired_width(220.0),
                                );
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
                    }
                    Err(e) => self.status = format!("Couldn't write {fname}: {e}"),
                }
            }
        }

        // ── Track list ─────────────────────────────────────────────────────
        // Snapshot the row strings so the scroll closure doesn't borrow `self`.
        struct UsbRow {
            title: String,
            meta: String,
            rel_path: String,
        }
        let rows: Vec<UsbRow> = self
            .usb_tracks
            .iter()
            .map(|t| {
                let file = Path::new(&t.source_path);
                let fname = file
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let title = match (t.tags.artist.as_deref(), t.tags.title.as_deref()) {
                    (Some(a), Some(ti)) => format!("{a} — {ti}"),
                    (None, Some(ti)) => ti.to_string(),
                    _ => fname.clone(),
                };
                let mut meta: Vec<String> = vec![format_label(t.format).to_string()];
                let secs = t.properties.duration_ms as f32 / 1000.0;
                if secs > 0.0 {
                    meta.push(fmt_time(secs));
                }
                if let Some(bpm) = t.tags.bpm_tag {
                    meta.push(format!("{bpm:.0} BPM"));
                }
                if let Some(k) = t.tags.initial_key_tag.as_deref() {
                    meta.push(k.to_string());
                }
                if let Some(size) = t.src_size {
                    meta.push(format!("{:.1} MB", size as f64 / 1_000_000.0));
                }
                let rel_path = file
                    .strip_prefix(vol)
                    .unwrap_or(file)
                    .display()
                    .to_string();
                UsbRow {
                    title,
                    meta: meta.join("  ·  "),
                    rel_path,
                }
            })
            .collect();

        let selected = self.usb_selected;
        let mut clicked: Option<usize> = None;
        egui::CentralPanel::default()
            .frame(egui::Frame::none())
            .show_inside(ui, |ui| {
                let row_h = 40.0;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show_rows(ui, row_h, rows.len(), |ui, range| {
                        for i in range {
                            let r = &rows[i];
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(ui.available_width(), row_h),
                                egui::Sense::click(),
                            );
                            if resp.clicked() {
                                clicked = Some(i);
                            }
                            if selected == Some(i) {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::Rounding::same(6.0),
                                    crate::sidebar::NAV_ACCENT.gamma_multiply(0.35),
                                );
                            } else if resp.hovered() {
                                ui.painter().rect_filled(
                                    rect,
                                    egui::Rounding::same(6.0),
                                    egui::Color32::from_gray(50),
                                );
                            }
                            let p = ui.painter();
                            p.text(
                                egui::pos2(rect.left() + 10.0, rect.top() + 6.0),
                                egui::Align2::LEFT_TOP,
                                &r.title,
                                egui::FontId::proportional(14.0),
                                egui::Color32::from_gray(230),
                            );
                            p.text(
                                egui::pos2(rect.left() + 10.0, rect.bottom() - 6.0),
                                egui::Align2::LEFT_BOTTOM,
                                &r.rel_path,
                                egui::FontId::monospace(10.5),
                                egui::Color32::from_gray(130),
                            );
                            p.text(
                                egui::pos2(rect.right() - 10.0, rect.center().y),
                                egui::Align2::RIGHT_CENTER,
                                &r.meta,
                                egui::FontId::proportional(11.5),
                                egui::Color32::from_gray(160),
                            );
                        }
                    });
            });
        if let Some(i) = clicked {
            // Re-clicking the selected row closes the editor; a new row loads
            // its tags into fresh edit buffers.
            if self.usb_selected == Some(i) {
                self.usb_selected = None;
            } else {
                self.usb_selected = Some(i);
                self.usb_edit = UsbEdit::from_tags(&self.usb_tracks[i].tags);
                self.usb_edit_saved = self.usb_edit.clone();
            }
        }
    }
}
