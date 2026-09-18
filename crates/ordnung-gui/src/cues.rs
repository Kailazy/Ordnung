//! Hot and memory cues on the now-playing bar: markers over both waveform
//! lanes, and the CUES panel that sets, jumps to, loops, names, colours and
//! deletes them. Every edit is written straight to the catalog
//! (`Catalog::set_cues`), so it survives a reload and rides the next USB
//! export as PCOB/PCO2 entries. Part of the GUI `App`; split out of `player`.
use super::*;
use ordnung_core::model::Cue;

/// The eight pad colours offered in the panel — rekordbox's hot cue palette.
pub(crate) const CUE_PALETTE: [([u8; 3], &str); 8] = [
    ([255, 0, 23], "Red"),
    ([255, 140, 0], "Orange"),
    ([255, 214, 10], "Yellow"),
    ([40, 226, 20], "Green"),
    ([64, 200, 224], "Aqua"),
    ([10, 132, 255], "Blue"),
    ([191, 90, 242], "Purple"),
    ([255, 55, 95], "Pink"),
];

/// Pad colour for a cue: its own, else the default green.
fn cue_rgb(c: &Cue) -> egui::Color32 {
    let [r, g, b] = c.color.unwrap_or(CUE_PALETTE[3].0);
    egui::Color32::from_rgb(r, g, b)
}

/// Memory cues draw in rekordbox's orange.
const MEMORY_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 159, 10);

/// Default loop length when the track has no beatgrid to count bars on.
const FALLBACK_LOOP_MS: u64 = 2_000;

/// Paint cue markers over a waveform lane. `window` is the visible span in
/// track fractions (the zoom lane scrolls; the overview strip is `(0, 1)`).
/// `compact` draws the overview strip's small ticks instead of lettered flags.
pub(crate) fn draw_cue_markers(
    painter: &egui::Painter,
    rect: egui::Rect,
    cues: &[Cue],
    dur_secs: f32,
    window: (f32, f32),
    compact: bool,
) {
    if dur_secs <= 0.0 || cues.is_empty() {
        return;
    }
    let (w0, w1) = window;
    let span = (w1 - w0).max(f32::EPSILON);
    let x_of = |ms: u64| {
        let frac = ms as f32 / 1000.0 / dur_secs;
        rect.left() + ((frac - w0) / span) * rect.width()
    };
    for c in cues {
        let color = if c.is_hot() { cue_rgb(c) } else { MEMORY_COLOR };
        let x = x_of(c.position_ms);
        // Loop body first, so the markers sit on top of the wash.
        if let Some(end) = c.loop_end_ms {
            let x1 = x_of(end);
            let body = egui::Rect::from_min_max(
                egui::pos2(x.max(rect.left()), rect.top()),
                egui::pos2(x1.min(rect.right()), rect.bottom()),
            );
            if body.width() > 0.0 {
                painter.rect_filled(
                    body,
                    egui::Rounding::ZERO,
                    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), 38),
                );
            }
            if x1 >= rect.left() && x1 <= rect.right() {
                painter.line_segment(
                    [egui::pos2(x1, rect.top()), egui::pos2(x1, rect.bottom())],
                    egui::Stroke::new(1.0, color),
                );
            }
        }
        if x < rect.left() || x > rect.right() {
            continue;
        }
        painter.line_segment(
            [egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())],
            egui::Stroke::new(if compact { 1.0 } else { 1.5 }, color),
        );
        if compact {
            // A small tick at the top edge: a filled triangle pointing down.
            let s = 4.0;
            painter.add(egui::Shape::convex_polygon(
                vec![
                    egui::pos2(x - s, rect.top()),
                    egui::pos2(x + s, rect.top()),
                    egui::pos2(x, rect.top() + s * 1.4),
                ],
                color,
                egui::Stroke::NONE,
            ));
            continue;
        }
        match c.slot_letter() {
            Some(letter) => {
                // Lettered flag hanging off the top of the marker.
                let flag = egui::Rect::from_min_size(
                    egui::pos2(x, rect.top()),
                    egui::vec2(16.0, 14.0),
                );
                painter.rect_filled(
                    flag,
                    egui::Rounding {
                        nw: 0.0,
                        sw: 0.0,
                        ne: 3.0,
                        se: 3.0,
                    },
                    color,
                );
                painter.text(
                    flag.center(),
                    egui::Align2::CENTER_CENTER,
                    letter,
                    crate::ui::tokens::font::strong(11.0),
                    egui::Color32::from_gray(18),
                );
            }
            None => {
                let s = 5.0;
                painter.add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(x - s, rect.top()),
                        egui::pos2(x + s, rect.top()),
                        egui::pos2(x, rect.top() + s * 1.5),
                    ],
                    color,
                    egui::Stroke::NONE,
                ));
            }
        }
        if let Some(label) = c.label.as_deref().filter(|l| !l.is_empty()) {
            painter.text(
                egui::pos2(x + 19.0, rect.top() + 2.0),
                egui::Align2::LEFT_TOP,
                label,
                crate::ui::tokens::font::caption(),
                color,
            );
        }
    }
}

impl App {
    /// The loaded track's cues: the catalog's for a library track, the
    /// stick's ANLZ lists for a device track.
    pub(crate) fn load_cues(&self, id: Id) -> Vec<Cue> {
        if id >= USB_ID_BASE {
            let i = (id - USB_ID_BASE) as usize;
            return self
                .usb_pdb_info
                .get(&i)
                .and_then(|info| info.anlz_path.as_ref())
                .map(|dat| ordnung_rbdb::anlz::read_cues(dat))
                .unwrap_or_default();
        }
        Catalog::open(&self.db_path)
            .and_then(|c| c.cues_for(id))
            .unwrap_or_default()
    }

    /// Write the loaded track's cue set to the catalog. Device tracks are
    /// read-only here (their cues live on the stick), so this is a no-op for
    /// them.
    fn commit_cues(&mut self) {
        let Some(np) = self.now_playing.as_ref() else {
            return;
        };
        if np.id >= USB_ID_BASE {
            return;
        }
        let (id, cues) = (np.id, np.cues.clone());
        match Catalog::open(&self.db_path).and_then(|c| c.set_cues(id, &cues)) {
            Ok(()) => {}
            Err(e) => self.status = format!("Couldn't save cues: {e}"),
        }
    }

    /// Where the playhead is right now, in ms — the position every new cue
    /// lands on.
    fn playhead_ms(&self) -> u64 {
        let Some(a) = self.audio.as_ref() else {
            return 0;
        };
        let dur = a.duration();
        let frac = self.scrub.unwrap_or(if dur > 0.0 { a.position() / dur } else { 0.0 });
        ((frac * dur).max(0.0) * 1000.0).round() as u64
    }

    /// The length a fresh loop gets: four beats of the lane's grid, or two
    /// seconds when the track has no tempo.
    fn default_loop_ms(&self) -> u64 {
        self.now_playing
            .as_ref()
            .and_then(|n| n.grid)
            .filter(|g| g.bpm > 0.0)
            .map(|g| (4.0 * 60_000.0 / g.bpm as f64).round() as u64)
            .unwrap_or(FALLBACK_LOOP_MS)
    }

    /// Press a hot cue pad: jump to it when set, plant it at the playhead when
    /// empty. Also the keyboard's 1–8. Device tracks only jump.
    pub(crate) fn trigger_hot_cue(&mut self, slot: u8) {
        let Some(np) = self.now_playing.as_ref() else {
            return;
        };
        if let Some(c) = np.cues.iter().find(|c| c.hot_slot == Some(slot)) {
            let secs = c.position_ms as f32 / 1000.0;
            if let Some(a) = self.audio.as_mut() {
                a.seek(secs);
            }
            self.scrub = None;
            return;
        }
        if np.id >= USB_ID_BASE {
            return;
        }
        let position_ms = self.playhead_ms();
        if let Some(np) = self.now_playing.as_mut() {
            np.cues.push(Cue {
                hot_slot: Some(slot),
                position_ms,
                loop_end_ms: None,
                label: None,
                color: None,
            });
        }
        self.commit_cues();
    }

    /// Add a memory cue (or a memory loop) at the playhead.
    fn add_memory_cue(&mut self, as_loop: bool) {
        let position_ms = self.playhead_ms();
        let loop_end_ms = as_loop.then(|| position_ms + self.default_loop_ms());
        if let Some(np) = self.now_playing.as_mut() {
            if np.id >= USB_ID_BASE {
                return;
            }
            // One memory cue per instant; a second press is a no-op.
            if np
                .cues
                .iter()
                .any(|c| !c.is_hot() && c.position_ms == position_ms)
            {
                return;
            }
            np.cues.push(Cue {
                hot_slot: None,
                position_ms,
                loop_end_ms,
                label: None,
                color: None,
            });
            np.cues.sort_by_key(|c| (c.hot_slot.is_none(), c.hot_slot.unwrap_or(0), c.position_ms));
        }
        self.commit_cues();
    }

    /// Apply one context-menu action to the cue at `idx` and persist.
    fn edit_cue(&mut self, idx: usize, action: CueAction) {
        let playhead = self.playhead_ms();
        let loop_len = self.default_loop_ms();
        let mut jump = None;
        let mut rename = None;
        if let Some(np) = self.now_playing.as_mut() {
            if idx >= np.cues.len() {
                return;
            }
            match action {
                CueAction::Jump => jump = Some(np.cues[idx].position_ms),
                CueAction::Rename => {
                    rename = Some((idx, np.cues[idx].label.clone().unwrap_or_default()))
                }
                CueAction::MoveHere => np.cues[idx].position_ms = playhead,
                CueAction::LoopToPlayhead => {
                    if playhead > np.cues[idx].position_ms {
                        np.cues[idx].loop_end_ms = Some(playhead);
                    }
                }
                CueAction::LoopDefault => {
                    np.cues[idx].loop_end_ms = Some(np.cues[idx].position_ms + loop_len)
                }
                CueAction::ClearLoop => np.cues[idx].loop_end_ms = None,
                CueAction::Color(rgb) => np.cues[idx].color = rgb,
                CueAction::Delete => {
                    np.cues.remove(idx);
                }
            }
        }
        if let Some(ms) = jump {
            if let Some(a) = self.audio.as_mut() {
                a.seek(ms as f32 / 1000.0);
            }
            self.scrub = None;
            return;
        }
        if let Some(r) = rename {
            self.cue_rename = Some(r);
            return;
        }
        self.commit_cues();
    }

    /// The CUES tab on the zoom lane and, while open, the panel of pads and
    /// memory cues floating above the lane's left edge (the beatgrid editor
    /// takes the right). Returns nothing; seeks go straight to the engine.
    pub(crate) fn draw_cue_editor(&mut self, ui: &mut egui::Ui, lane: egui::Rect) {
        let Some((editable, cues)) = self
            .now_playing
            .as_ref()
            .map(|n| (n.id < USB_ID_BASE, n.cues.clone()))
        else {
            return;
        };

        // The tab: a pill beside the GRID tab, lit while the panel is open.
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(lane.right() - 94.0, lane.top() + 3.0),
            egui::vec2(42.0, 15.0),
        );
        let tab = ui
            .interact(tab_rect, ui.id().with("cue_edit_tab"), egui::Sense::click())
            .on_hover_note("Hot cues and memory cues");
        if tab.clicked() {
            self.cue_edit_open = !self.cue_edit_open;
        }
        if tab.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let (fill, text) = match (self.cue_edit_open, tab.hovered()) {
            (true, _) => (
                crate::ui::tokens::color::ACCENT,
                crate::ui::tokens::color::LABEL,
            ),
            (false, true) => (
                egui::Color32::from_rgba_unmultiplied(150, 150, 150, 120),
                crate::ui::tokens::color::LABEL,
            ),
            (false, false) => (
                egui::Color32::from_rgba_unmultiplied(150, 150, 150, 70),
                crate::ui::tokens::color::LABEL_2,
            ),
        };
        ui.painter().rect_filled(
            tab_rect,
            egui::Rounding::same(crate::ui::tokens::radius::XS),
            fill,
        );
        ui.painter().text(
            tab_rect.center(),
            egui::Align2::CENTER_CENTER,
            "CUES",
            crate::ui::tokens::font::caption(),
            text,
        );
        if !self.cue_edit_open {
            return;
        }

        let mut pad_hit: Option<u8> = None;
        let mut action: Option<(usize, CueAction)> = None;
        let mut add_memory: Option<bool> = None;
        let mut rename_done: Option<Option<String>> = None; // Some(None) = cancel
        let mut rename_buf = self.cue_rename.clone();
        let playhead = self.playhead_ms();

        crate::ui::window::Window::new("Cues")
            .id(egui::Id::new("cue_edit_panel"))
            .title_bar(false)
            .fixed_at(egui::Align2::LEFT_BOTTOM, egui::pos2(lane.left(), lane.top() - 6.0))
            .show(ui.ctx(), |ui| {
                const W: f32 = 296.0;
                const GAP: f32 = crate::ui::tokens::space::S2;
                ui.set_width(W);
                ui.spacing_mut().item_spacing = egui::vec2(GAP, GAP);

                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("HOT CUES")
                            .font(crate::ui::tokens::font::caption())
                            .color(crate::ui::tokens::color::LABEL_3),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(if editable {
                                "Empty pad sets, set pad jumps. Keys 1–8."
                            } else {
                                "From the stick. Pads jump."
                            })
                            .font(crate::ui::tokens::font::caption())
                            .color(crate::ui::tokens::color::LABEL_3),
                        );
                    });
                });

                // Two rows of four pads.
                let pad_w = (W - 3.0 * GAP) / 4.0;
                let pad_h = 36.0;
                for row in 0..2u8 {
                    ui.horizontal(|ui| {
                        for col in 0..4u8 {
                            let slot = row * 4 + col;
                            let (rect, resp) = ui.allocate_exact_size(
                                egui::vec2(pad_w, pad_h),
                                egui::Sense::click(),
                            );
                            let found = cues
                                .iter()
                                .enumerate()
                                .find(|(_, c)| c.hot_slot == Some(slot));
                            let letter = (b'A' + slot) as char;
                            let r = crate::ui::tokens::radius::SM;
                            match found {
                                Some((_, c)) => {
                                    let col = cue_rgb(c);
                                    let fill = if resp.hovered() {
                                        col
                                    } else {
                                        egui::Color32::from_rgba_unmultiplied(
                                            col.r(),
                                            col.g(),
                                            col.b(),
                                            200,
                                        )
                                    };
                                    ui.painter().rect_filled(rect, egui::Rounding::same(r), fill);
                                    let ink = egui::Color32::from_gray(18);
                                    ui.painter().text(
                                        rect.left_top() + egui::vec2(6.0, 4.0),
                                        egui::Align2::LEFT_TOP,
                                        letter,
                                        crate::ui::tokens::font::strong(13.0),
                                        ink,
                                    );
                                    if c.is_loop() {
                                        ui.painter().text(
                                            rect.right_top() + egui::vec2(-6.0, 4.0),
                                            egui::Align2::RIGHT_TOP,
                                            "⟲",
                                            crate::ui::tokens::font::caption(),
                                            ink,
                                        );
                                    }
                                    let sub = match c.label.as_deref().filter(|l| !l.is_empty()) {
                                        Some(l) => l.to_string(),
                                        None => fmt_time(c.position_ms as f32 / 1000.0),
                                    };
                                    ui.painter().text(
                                        rect.left_bottom() + egui::vec2(6.0, -4.0),
                                        egui::Align2::LEFT_BOTTOM,
                                        sub,
                                        crate::ui::tokens::font::caption(),
                                        ink,
                                    );
                                }
                                None => {
                                    ui.painter().rect_filled(
                                        rect,
                                        egui::Rounding::same(r),
                                        if resp.hovered() && editable {
                                            crate::ui::tokens::color::SURFACE_HOVER
                                        } else {
                                            crate::ui::tokens::color::SURFACE_HI
                                        },
                                    );
                                    ui.painter().text(
                                        rect.left_top() + egui::vec2(6.0, 4.0),
                                        egui::Align2::LEFT_TOP,
                                        letter,
                                        crate::ui::tokens::font::strong(13.0),
                                        crate::ui::tokens::color::LABEL_4,
                                    );
                                    if resp.hovered() && editable {
                                        ui.painter().text(
                                            rect.left_bottom() + egui::vec2(6.0, -4.0),
                                            egui::Align2::LEFT_BOTTOM,
                                            fmt_time(playhead as f32 / 1000.0),
                                            crate::ui::tokens::font::caption(),
                                            crate::ui::tokens::color::LABEL_2,
                                        );
                                    }
                                }
                            }
                            if resp.hovered() && (found.is_some() || editable) {
                                ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                            }
                            if resp.clicked() {
                                pad_hit = Some(slot);
                            }
                            if let Some((idx, c)) = found {
                                let note = format!(
                                    "Hot cue {letter} at {}",
                                    fmt_time(c.position_ms as f32 / 1000.0)
                                );
                                resp.clone().on_hover_note(note);
                                let is_loop = c.is_loop();
                                resp.context_menu(|ui| {
                                    cue_context_menu(ui, idx, is_loop, editable, playhead > c.position_ms, &mut action);
                                });
                            }
                        }
                    });
                }

                ui.add_space(crate::ui::tokens::space::S1);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("MEMORY CUES")
                            .font(crate::ui::tokens::font::caption())
                            .color(crate::ui::tokens::color::LABEL_3),
                    );
                    if editable {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            crate::ui::control_row(ui, |ui| {
                                if ui
                                    .button("⟲ Loop")
                                    .on_hover_note("Memory loop at the playhead")
                                    .clicked()
                                {
                                    add_memory = Some(true);
                                }
                                if ui
                                    .button("+ Cue")
                                    .on_hover_note("Memory cue at the playhead")
                                    .clicked()
                                {
                                    add_memory = Some(false);
                                }
                            });
                        });
                    }
                });

                let memory: Vec<(usize, &Cue)> = cues
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| !c.is_hot())
                    .collect();
                if memory.is_empty() {
                    ui.label(
                        egui::RichText::new("None yet")
                            .font(crate::ui::tokens::font::footnote())
                            .color(crate::ui::tokens::color::LABEL_4),
                    );
                } else {
                    ui.horizontal_wrapped(|ui| {
                        for (idx, c) in memory {
                            let mut text = fmt_time(c.position_ms as f32 / 1000.0);
                            if c.is_loop() {
                                text.push_str(" ⟲");
                            }
                            if let Some(l) = c.label.as_deref().filter(|l| !l.is_empty()) {
                                text.push_str("  ");
                                text.push_str(l);
                            }
                            let chip = egui::Button::new(
                                egui::RichText::new(text)
                                    .font(crate::ui::tokens::font::footnote())
                                    .color(MEMORY_COLOR),
                            )
                            .fill(crate::ui::tokens::color::SURFACE_HI)
                            .rounding(crate::ui::tokens::radius::XS);
                            let resp = ui.add(chip).on_hover_note("Jump to this cue");
                            if resp.clicked() {
                                action = Some((idx, CueAction::Jump));
                            }
                            let is_loop = c.is_loop();
                            let after = playhead > c.position_ms;
                            resp.context_menu(|ui| {
                                cue_context_menu(ui, idx, is_loop, editable, after, &mut action);
                            });
                        }
                    });
                }

                // Inline rename for the cue picked from a context menu.
                if let Some((idx, buf)) = rename_buf.as_mut() {
                    ui.add_space(crate::ui::tokens::space::S1);
                    crate::ui::control_row(ui, |ui| {
                        ui.horizontal(|ui| {
                            let edit = ui.add(
                                egui::TextEdit::singleline(buf)
                                    .hint_text("Cue name")
                                    .desired_width(W - 120.0),
                            );
                            if !edit.has_focus() && !edit.lost_focus() {
                                edit.request_focus();
                            }
                            let enter = edit.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if ui.button("Save").clicked() || enter {
                                rename_done = Some(Some(buf.clone()));
                            }
                            if ui.button("Cancel").clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                rename_done = Some(None);
                            }
                            let _ = idx;
                        });
                    });
                }
            });

        if let Some(slot) = pad_hit {
            self.trigger_hot_cue(slot);
        }
        if let Some(as_loop) = add_memory {
            self.add_memory_cue(as_loop);
        }
        if let Some((idx, act)) = action {
            self.edit_cue(idx, act);
        }
        match rename_done {
            Some(Some(name)) => {
                if let Some((idx, _)) = self.cue_rename.take() {
                    if let Some(np) = self.now_playing.as_mut() {
                        if let Some(c) = np.cues.get_mut(idx) {
                            let t = name.trim();
                            c.label = (!t.is_empty()).then(|| t.to_string());
                        }
                    }
                    self.commit_cues();
                }
            }
            Some(None) => self.cue_rename = None,
            None => {
                // Keep the buffer the user is typing into.
                if let Some(r) = rename_buf {
                    if self.cue_rename.is_some() {
                        self.cue_rename = Some(r);
                    }
                }
            }
        }
    }
}

/// What a cue's context menu can do to it.
#[derive(Clone, Copy)]
enum CueAction {
    Jump,
    Rename,
    MoveHere,
    LoopToPlayhead,
    LoopDefault,
    ClearLoop,
    Color(Option<[u8; 3]>),
    Delete,
}

fn cue_context_menu(
    ui: &mut egui::Ui,
    idx: usize,
    is_loop: bool,
    editable: bool,
    playhead_after: bool,
    action: &mut Option<(usize, CueAction)>,
) {
    if ui.button("Jump to cue").clicked() {
        *action = Some((idx, CueAction::Jump));
        ui.close_menu();
    }
    if !editable {
        return;
    }
    if ui.button("Rename…").clicked() {
        *action = Some((idx, CueAction::Rename));
        ui.close_menu();
    }
    if ui.button("Move to playhead").clicked() {
        *action = Some((idx, CueAction::MoveHere));
        ui.close_menu();
    }
    ui.separator();
    if is_loop {
        if ui.button("Remove loop").clicked() {
            *action = Some((idx, CueAction::ClearLoop));
            ui.close_menu();
        }
    } else if ui.button("Loop 4 beats").clicked() {
        *action = Some((idx, CueAction::LoopDefault));
        ui.close_menu();
    }
    if ui
        .add_enabled(playhead_after, egui::Button::new("Loop to playhead"))
        .clicked()
    {
        *action = Some((idx, CueAction::LoopToPlayhead));
        ui.close_menu();
    }
    ui.separator();
    ui.horizontal(|ui| {
        for (rgb, name) in CUE_PALETTE {
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(16.0, 16.0), egui::Sense::click());
            ui.painter().rect_filled(
                rect,
                egui::Rounding::same(3.0),
                egui::Color32::from_rgb(rgb[0], rgb[1], rgb[2]),
            );
            if resp.hovered() {
                ui.painter().rect_stroke(
                    rect,
                    egui::Rounding::same(3.0),
                    egui::Stroke::new(1.5, crate::ui::tokens::color::LABEL),
                );
            }
            if resp.on_hover_note(name).clicked() {
                *action = Some((idx, CueAction::Color(Some(rgb))));
                ui.close_menu();
            }
        }
    });
    ui.separator();
    if ui.button("Delete cue").clicked() {
        *action = Some((idx, CueAction::Delete));
        ui.close_menu();
    }
}
