//! Hot cues, memory cues and the loop on the now-playing bar: markers over
//! both waveform lanes, and the cue bar, a strip across the top of the zoom
//! lane laid out the way a player's pad section is. Eight pads (A–H, each
//! its own colour), the beat loop with its length, the loop key and quantize,
//! and the memory cue list. Every edit is written straight to the catalog
//! (`Catalog::set_cues`), so it survives a reload and rides the next USB
//! export as PCOB/PCO2 entries. The active loop itself lives in the audio
//! engine (`AudioEngine::set_loop`) so it really loops, sample-exact. Part
//! of the GUI `App`; split out of `player`.
use super::*;
use ordnung_core::model::Cue;

/// The eight pad colours, rekordbox's hot cue palette, one per slot A–H:
/// a pad with no colour of its own takes its slot's, so the eight always
/// read apart on the pads and on the lanes.
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

/// The colour a slot's pad takes when its cue has none of its own.
fn slot_rgb(slot: u8) -> [u8; 3] {
    CUE_PALETTE[(slot as usize) % CUE_PALETTE.len()].0
}

/// Pad colour for a cue: its own, else its slot's.
fn cue_rgb(c: &Cue) -> egui::Color32 {
    let [r, g, b] = c.color.unwrap_or_else(|| slot_rgb(c.hot_slot.unwrap_or(3)));
    egui::Color32::from_rgb(r, g, b)
}

/// Memory cues draw in rekordbox's orange.
const MEMORY_COLOR: egui::Color32 = egui::Color32::from_rgb(255, 159, 10);

/// The active loop draws in white over the lanes and on the pad that holds it.
const ACTIVE_LOOP_COLOR: egui::Color32 = egui::Color32::from_rgb(245, 245, 250);
/// The loop key's lit colour. A CDJ's loop keys blink amber while a loop is
/// engaged, and the key's label stays put: the blink is the signal, not the
/// word on the cap.
const LOOP_KEY_ON: egui::Color32 = crate::ui::tokens::color::YELLOW;
/// The blink's off phase. Dim amber rather than the sunken field so the key
/// still reads as engaged between flashes.
const LOOP_KEY_OFF: egui::Color32 = egui::Color32::from_rgb(118, 98, 8);
/// One full on/off cycle of the loop key blink, in seconds.
const LOOP_BLINK_PERIOD: f64 = 0.5;

/// The loop key's fill for this frame while a loop is engaged, and a repaint
/// booked for the next edge so it keeps blinking on a paused deck.
fn loop_key_blink(ctx: &egui::Context) -> egui::Color32 {
    let phase = (ctx.input(|i| i.time) / LOOP_BLINK_PERIOD).fract();
    let to_edge = if phase < 0.5 { 0.5 - phase } else { 1.0 - phase };
    ctx.request_repaint_after(std::time::Duration::from_secs_f64(
        to_edge * LOOP_BLINK_PERIOD + 0.005,
    ));
    if phase < 0.5 { LOOP_KEY_ON } else { LOOP_KEY_OFF }
}

/// rekordbox keeps at most ten memory cues on a track; the export follows.
pub(crate) const MAX_MEMORY_CUES: usize = 10;

/// Loop lengths the beat loop steps through, in beats.
pub(crate) const LOOP_BEATS: [u32; 6] = [1, 2, 4, 8, 16, 32];
/// Index into [`LOOP_BEATS`] a fresh session starts on: four beats, one bar.
pub(crate) const DEFAULT_LOOP_BEATS: usize = 2;

/// Length of one beat when the track has no beatgrid to count on.
const FALLBACK_BEAT_MS: f64 = 500.0;

/// Height of the cue bar, headers included.
pub(crate) const CUE_BAR_H: f32 = 58.0;

/// Paint cue markers over a waveform lane. `window` is the visible span in
/// track fractions (the zoom lane scrolls; the overview strip is `(0, 1)`).
/// `compact` draws the overview strip's small ticks instead of lettered
/// flags. `active` is the loop playback is circling right now, drawn in
/// white over whichever cue (if any) it came from.
pub(crate) fn draw_cue_markers(
    painter: &egui::Painter,
    rect: egui::Rect,
    cues: &[Cue],
    active: Option<(u64, u64)>,
    dur_secs: f32,
    window: (f32, f32),
    compact: bool,
) {
    if dur_secs <= 0.0 || (cues.is_empty() && active.is_none()) {
        return;
    }
    let (w0, w1) = window;
    let span = (w1 - w0).max(f32::EPSILON);
    let x_of = |ms: u64| {
        let frac = ms as f32 / 1000.0 / dur_secs;
        rect.left() + ((frac - w0) / span) * rect.width()
    };
    let wash = |painter: &egui::Painter, start: u64, end: u64, color: egui::Color32, alpha: u8| {
        let (x0, x1) = (x_of(start), x_of(end));
        let body = egui::Rect::from_min_max(
            egui::pos2(x0.max(rect.left()), rect.top()),
            egui::pos2(x1.min(rect.right()), rect.bottom()),
        );
        if body.width() > 0.0 {
            painter.rect_filled(
                body,
                egui::Rounding::ZERO,
                egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), alpha),
            );
        }
        if x1 >= rect.left() && x1 <= rect.right() {
            painter.line_segment(
                [egui::pos2(x1, rect.top()), egui::pos2(x1, rect.bottom())],
                egui::Stroke::new(1.0, color),
            );
        }
    };
    // The loop playback is circling gets the one white wash below; the cue
    // it came from skips its own, so the two never stack into a glare.
    let is_active = |c: &Cue| {
        matches!((active, c.loop_end_ms), (Some((a, b)), Some(end))
            if a.abs_diff(c.position_ms) <= 2 && b.abs_diff(end) <= 2)
    };
    for c in cues {
        let color = if c.is_hot() { cue_rgb(c) } else { MEMORY_COLOR };
        let x = x_of(c.position_ms);
        // Loop body first, so the markers sit on top of the wash.
        if let Some(end) = c.loop_end_ms.filter(|_| !is_active(c)) {
            wash(painter, c.position_ms, end, color, 30);
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
    // The live loop on top of everything: a white wash with both edges
    // drawn, so it reads as the thing playing rather than another cue.
    if let Some((start, end)) = active.filter(|(a, b)| b > a) {
        wash(painter, start, end, ACTIVE_LOOP_COLOR, if compact { 36 } else { 22 });
        let x0 = x_of(start);
        if x0 >= rect.left() && x0 <= rect.right() {
            painter.line_segment(
                [egui::pos2(x0, rect.top()), egui::pos2(x0, rect.bottom())],
                egui::Stroke::new(1.0, ACTIVE_LOOP_COLOR),
            );
        }
    }
}

/// What the loop section of the bar asked for this frame.
#[derive(Clone, Copy)]
enum LoopCmd {
    /// Start a beat loop of the set length at the playhead.
    BeatLoop,
    /// Stop looping; playback runs on from where it is.
    Exit,
    /// Change the beat length (and resize the live loop with it).
    Beats(usize),
    /// Flip quantize.
    ToggleQuantize,
}

impl App {
    /// The loaded track's cues: the catalog's for a library track, the
    /// stick's ANLZ lists for a device track.
    pub(crate) fn load_cues(&self, id: Id) -> Vec<Cue> {
        if id >= USB_ID_BASE {
            return self
                .usb_anlz_dat(id)
                .map(|dat| ordnung_rbdb::anlz::read_cues(&dat))
                .unwrap_or_default();
        }
        Catalog::open(&self.db_path)
            .and_then(|c| c.cues_for(id))
            .unwrap_or_default()
    }

    /// Whether cues and the grid of `id` can be edited: always for a library
    /// track; for a device track only when the stick carries its rekordbox
    /// analysis files, which is where the edit is written.
    pub(crate) fn cues_editable(&self, id: Id) -> bool {
        id < USB_ID_BASE || self.usb_anlz_dat(id).is_some()
    }

    /// Write the loaded track's cue set: to the catalog for a library track,
    /// straight into the stick's ANLZ files for a device track — the way
    /// rekordbox's device view saves a cue the moment it's set. Hot cues that
    /// never got a colour are given their slot's on the way, so the export
    /// shows the same eight colours the pads do.
    fn commit_cues(&mut self) {
        let Some(np) = self.now_playing.as_mut() else {
            return;
        };
        for c in np.cues.iter_mut() {
            if let (Some(slot), None) = (c.hot_slot, c.color) {
                c.color = Some(slot_rgb(slot));
            }
        }
        let (id, cues) = (np.id, np.cues.clone());
        if id >= USB_ID_BASE {
            let (Some(dat), Some(vol), Some(pdb_id)) = (
                self.usb_anlz_dat(id),
                self.usb_loaded_for.clone(),
                usb_track_index(id).and_then(|i| self.usb_pdb_info.get(&i)?.pdb_id),
            ) else {
                return;
            };
            if let Err(e) = ordnung_rbdb::edit::write_stick_cues(&vol, pdb_id, &dat, &cues) {
                self.status = format!("Couldn't write cues to the stick: {e}");
            }
            return;
        }
        match Catalog::open(&self.db_path).and_then(|c| c.set_cues(id, &cues)) {
            Ok(()) => {}
            Err(e) => self.status = format!("Couldn't save cues: {e}"),
        }
    }

    /// Where the playhead is right now, in ms, the position every new cue
    /// lands on (before quantize).
    fn playhead_ms(&self) -> u64 {
        let Some(a) = self.audio.as_ref() else {
            return 0;
        };
        let dur = a.duration();
        let frac = self.scrub.unwrap_or(if dur > 0.0 { a.position() / dur } else { 0.0 });
        ((frac * dur).max(0.0) * 1000.0).round() as u64
    }

    /// The lane's beat length in ms, when the track has a tempo.
    fn beat_ms(&self) -> Option<f64> {
        self.now_playing
            .as_ref()
            .and_then(|n| n.grid)
            .filter(|g| g.bpm > 0.0)
            .map(|g| 60_000.0 / g.bpm as f64)
    }

    /// `ms` snapped to the nearest beat of the grid when quantize is on;
    /// untouched when it's off or the track has no grid.
    fn snap_ms(&self, ms: u64) -> u64 {
        if !self.config.cue_quantize {
            return ms;
        }
        let Some(g) = self.now_playing.as_ref().and_then(|n| n.grid) else {
            return ms;
        };
        let period = 60_000.0 / g.bpm.max(1.0) as f64;
        let i = ((ms as f64 - g.first_beat_ms) / period).round();
        (g.first_beat_ms + i * period).max(0.0).round() as u64
    }

    /// The length the beat loop control is set to, in ms: that many beats
    /// of the grid, or of a 120 bpm stand-in when the track has none.
    fn loop_len_ms(&self) -> u64 {
        let beats = LOOP_BEATS[self.loop_beats.min(LOOP_BEATS.len() - 1)] as f64;
        (beats * self.beat_ms().unwrap_or(FALLBACK_BEAT_MS)).round() as u64
    }

    /// The loop playback is circling, in ms.
    pub(crate) fn active_loop_ms(&self) -> Option<(u64, u64)> {
        self.audio
            .as_ref()
            .and_then(|a| a.active_loop())
            .map(|(a, b)| ((a * 1000.0).round() as u64, (b * 1000.0).round() as u64))
    }

    fn seek_ms(&mut self, ms: u64) {
        if let Some(a) = self.audio.as_mut() {
            a.seek(ms as f32 / 1000.0);
        }
        self.scrub = None;
    }

    /// Jump to `start` and loop up to `end`, the way a loop pad plays.
    fn engage_loop(&mut self, start: u64, end: u64) {
        self.seek_ms(start);
        if let Some(a) = self.audio.as_mut() {
            a.set_loop(Some((start as f32 / 1000.0, end as f32 / 1000.0)));
        }
    }

    fn exit_loop(&mut self) {
        if let Some(a) = self.audio.as_mut() {
            a.set_loop(None);
        }
    }

    /// The beat loop button: loop the set length from the playhead (snapped
    /// to the beat with quantize on), without a jump.
    fn beat_loop_here(&mut self) {
        let start = self.snap_ms(self.playhead_ms());
        let end = start + self.loop_len_ms();
        if let Some(a) = self.audio.as_mut() {
            a.set_loop(Some((start as f32 / 1000.0, end as f32 / 1000.0)));
        }
    }

    /// Step the beat length; a live loop keeps its start and takes the new
    /// length, like halving and doubling on a player.
    fn set_loop_beats(&mut self, idx: usize) {
        self.loop_beats = idx.min(LOOP_BEATS.len() - 1);
        if let Some((start, _)) = self.active_loop_ms() {
            let end = start + self.loop_len_ms();
            if let Some(a) = self.audio.as_mut() {
                a.set_loop(Some((start as f32 / 1000.0, end as f32 / 1000.0)));
            }
        }
    }

    /// Press a hot cue pad: jump to it when set (a loop pad starts its
    /// loop), plant it at the playhead when empty. With a loop running, an
    /// empty pad stores that loop instead, as a player does. Also the
    /// keyboard's 1–8. A device track's pad lands on the stick itself.
    pub(crate) fn trigger_hot_cue(&mut self, slot: u8) {
        let Some(np) = self.now_playing.as_ref() else {
            return;
        };
        let editable = self.cues_editable(np.id);
        let found = np
            .cues
            .iter()
            .find(|c| c.hot_slot == Some(slot))
            .map(|c| (c.position_ms, c.loop_end_ms));
        if let Some((start, end)) = found {
            match end {
                Some(end) => self.engage_loop(start, end),
                None => {
                    self.exit_loop();
                    self.seek_ms(start);
                }
            }
            return;
        }
        if !editable {
            return;
        }
        let (position_ms, loop_end_ms) = match self.active_loop_ms() {
            Some((a, b)) => (a, Some(b)),
            None => (self.snap_ms(self.playhead_ms()), None),
        };
        if let Some(np) = self.now_playing.as_mut() {
            np.cues.push(Cue {
                hot_slot: Some(slot),
                position_ms,
                loop_end_ms,
                label: None,
                color: Some(slot_rgb(slot)),
            });
        }
        self.commit_cues();
    }

    /// Add a memory cue (or a memory loop) at the playhead. A running loop
    /// is what "+ Loop" stores; otherwise the loop takes the set beat length.
    fn add_memory_cue(&mut self, as_loop: bool) {
        let (position_ms, loop_end_ms) = match (as_loop, self.active_loop_ms()) {
            (true, Some((a, b))) => (a, Some(b)),
            _ => {
                let p = self.snap_ms(self.playhead_ms());
                (p, as_loop.then(|| p + self.loop_len_ms()))
            }
        };
        let editable = self
            .now_playing
            .as_ref()
            .is_some_and(|np| self.cues_editable(np.id));
        if !editable {
            return;
        }
        if let Some(np) = self.now_playing.as_mut() {
            // One memory cue per instant; a second press is a no-op.
            if np
                .cues
                .iter()
                .any(|c| !c.is_hot() && c.position_ms == position_ms)
            {
                return;
            }
            if np.cues.iter().filter(|c| !c.is_hot()).count() >= MAX_MEMORY_CUES {
                self.status = format!("rekordbox keeps at most {MAX_MEMORY_CUES} memory cues on a track");
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

    /// Apply one action to the cue at `idx` and persist.
    fn edit_cue(&mut self, idx: usize, action: CueAction) {
        let playhead = self.snap_ms(self.playhead_ms());
        let loop_len = self.loop_len_ms();
        let mut jump = None;
        let mut rename = None;
        if let Some(np) = self.now_playing.as_mut() {
            if idx >= np.cues.len() {
                return;
            }
            match action {
                CueAction::Jump => jump = Some((np.cues[idx].position_ms, np.cues[idx].loop_end_ms)),
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
        if let Some((ms, end)) = jump {
            match end {
                Some(end) => self.engage_loop(ms, end),
                None => {
                    self.exit_loop();
                    self.seek_ms(ms);
                }
            }
            return;
        }
        if let Some(r) = rename {
            self.cue_rename = Some(r);
            return;
        }
        self.commit_cues();
    }

    /// The CUES tab on the zoom lane: shows and hides the cue bar above it.
    pub(crate) fn draw_cue_tab(&mut self, ui: &mut egui::Ui, lane: egui::Rect) {
        if self.now_playing.is_none() {
            return;
        }
        // The tab: a pill beside the GRID tab, lit while the bar is showing.
        let tab_rect = egui::Rect::from_min_size(
            egui::pos2(lane.right() - 94.0, lane.top() + 3.0),
            egui::vec2(42.0, 15.0),
        );
        let tab = ui
            .interact(tab_rect, ui.id().with("cue_edit_tab"), egui::Sense::click())
            .on_hover_note("Hot cues, loop and memory cues");
        if tab.clicked() {
            self.config.cue_bar_open = !self.config.cue_bar_open;
            let _ = self.config.save();
        }
        if tab.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        let (fill, text) = match (self.config.cue_bar_open, tab.hovered()) {
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
    }

    /// The cue bar: a strip the lane's width, above the zoom lane. Left to
    /// right: the eight hot cue pads, the loop section, the memory cues.
    /// Seeks and loops go straight to the engine; edits to the catalog (or,
    /// for a device track, onto the stick).
    pub(crate) fn draw_cue_bar(&mut self, ui: &mut egui::Ui) {
        use crate::ui::tokens::{color, font, radius, space};
        let Some((editable, cues)) = self
            .now_playing
            .as_ref()
            .map(|n| (self.cues_editable(n.id), n.cues.clone()))
        else {
            return;
        };
        const MARGIN: f32 = 10.0;
        const GAP: f32 = space::S2;
        const SECT_GAP: f32 = space::S5;
        const HEADER_H: f32 = 14.0;
        const PAD_H: f32 = 40.0;
        const LOOP_W: f32 = 206.0;
        const MEM_MIN_W: f32 = 170.0;
        const ADD_W: f32 = 124.0;
        /// The add buttons as bare glyphs, for a bar too narrow for words.
        const ADD_W_COMPACT: f32 = 72.0;
        const CHIP_H: f32 = 28.0;

        let playhead = self.playhead_ms();
        let active = self.active_loop_ms();
        let quantize = self.config.cue_quantize;
        let beat = self.beat_ms();
        let beats = LOOP_BEATS[self.loop_beats.min(LOOP_BEATS.len() - 1)];
        let same_loop = |c: &Cue| -> bool {
            matches!((active, c.loop_end_ms), (Some((a, b)), Some(end))
                if a.abs_diff(c.position_ms) <= 2 && b.abs_diff(end) <= 2)
        };
        let loop_beats_of = |start: u64, end: u64| -> Option<String> {
            beat.map(|p| {
                let n = (end - start) as f64 / p;
                if (n - n.round()).abs() < 0.05 {
                    format!("{}", n.round() as u32)
                } else {
                    format!("{n:.1}")
                }
            })
        };

        let mut pad_hit: Option<u8> = None;
        let mut action: Option<(usize, CueAction)> = None;
        let mut add_memory: Option<bool> = None;
        let mut loop_cmd: Option<LoopCmd> = None;
        let mut rename_done: Option<Option<String>> = None; // Some(None) = cancel
        let mut rename_buf = self.cue_rename.clone();

        let width = (ui.available_width() - 2.0 * MARGIN).max(300.0);
        ui.horizontal(|ui| {
            ui.add_space(MARGIN);
            let (bar, _) = ui.allocate_exact_size(egui::vec2(width, CUE_BAR_H), egui::Sense::hover());
            let painter = ui.painter().clone();

            let pad_w = ((bar.width() - LOOP_W - MEM_MIN_W - 2.0 * SECT_GAP - 7.0 * GAP) / 8.0)
                .clamp(40.0, 96.0);
            let pads_w = 8.0 * pad_w + 7.0 * GAP;
            let row_top = bar.top() + HEADER_H;
            let row_h = bar.height() - HEADER_H;
            let pads_rect = egui::Rect::from_min_size(bar.left_top(), egui::vec2(pads_w, bar.height()));
            let loop_rect = egui::Rect::from_min_size(
                egui::pos2(pads_rect.right() + SECT_GAP, bar.top()),
                egui::vec2(LOOP_W, bar.height()),
            );
            let mem_rect = egui::Rect::from_min_max(
                egui::pos2(loop_rect.right() + SECT_GAP, bar.top()),
                bar.right_bottom(),
            );
            for x in [pads_rect.right() + SECT_GAP / 2.0, loop_rect.right() + SECT_GAP / 2.0] {
                painter.line_segment(
                    [egui::pos2(x, row_top + 4.0), egui::pos2(x, bar.bottom() - 4.0)],
                    egui::Stroke::new(1.0, color::SEPARATOR_OPAQUE),
                );
            }

            // Section headers, with a quiet note at each one's right edge.
            let header = |painter: &egui::Painter, r: egui::Rect, title: &str, note: &str, note_col: egui::Color32| {
                painter.text(
                    egui::pos2(r.left() + 1.0, r.top()),
                    egui::Align2::LEFT_TOP,
                    title,
                    font::caption(),
                    color::LABEL_3,
                );
                if !note.is_empty() {
                    painter.text(
                        egui::pos2(r.right() - 1.0, r.top()),
                        egui::Align2::RIGHT_TOP,
                        note,
                        font::caption(),
                        note_col,
                    );
                }
            };
            header(
                &painter,
                pads_rect,
                "HOT CUES",
                if editable { "Keys 1–8" } else { "From the stick" },
                color::LABEL_4,
            );
            let loop_note = match active {
                Some((a, b)) => match loop_beats_of(a, b) {
                    Some(n) => format!("{n} beats"),
                    None => format!("{:.1} s", (b - a) as f64 / 1000.0),
                },
                None => String::new(),
            };
            header(&painter, loop_rect, "LOOP", &loop_note, ACTIVE_LOOP_COLOR);
            let memory: Vec<(usize, &Cue)> = cues
                .iter()
                .enumerate()
                .filter(|(_, c)| !c.is_hot())
                .collect();
            header(
                &painter,
                mem_rect,
                "MEMORY CUES",
                &if editable { format!("{}/{MAX_MEMORY_CUES}", memory.len()) } else { String::new() },
                color::LABEL_4,
            );

            // --- Pads ---------------------------------------------------
            let pad_top = row_top + (row_h - PAD_H) / 2.0;
            for slot in 0..8u8 {
                let rect = egui::Rect::from_min_size(
                    egui::pos2(pads_rect.left() + slot as f32 * (pad_w + GAP), pad_top),
                    egui::vec2(pad_w, PAD_H),
                );
                let id = ui.id().with(("cue_pad", slot));
                let resp = ui.interact(rect, id, egui::Sense::click());
                let found = cues
                    .iter()
                    .enumerate()
                    .find(|(_, c)| c.hot_slot == Some(slot));
                let letter = (b'A' + slot) as char;
                let r = radius::SM;
                match found {
                    Some((idx, c)) => {
                        let col = cue_rgb(c);
                        let lit = same_loop(c);
                        // The ✕ is a widget every frame, not only while the
                        // pad is hovered: the pointer moving onto it leaves
                        // the pad, and a ✕ that vanished then would flicker
                        // under the pointer and never take the click.
                        let x_rect = egui::Rect::from_min_size(
                            rect.right_top() + egui::vec2(-18.0, 2.0),
                            egui::vec2(16.0, 16.0),
                        );
                        let x_resp = editable.then(|| {
                            ui.interact(x_rect, id.with("x"), egui::Sense::click())
                                .on_hover_note(format!("Remove hot cue {letter}"))
                        });
                        let hot = resp.hovered() || x_resp.as_ref().map_or(false, |x| x.hovered());
                        // A rubber pad: the colour, with a darker lip at the
                        // bottom so it stands off the bar; full brightness
                        // under the pointer.
                        let body_col = if hot || lit {
                            col
                        } else {
                            egui::Color32::from_rgba_unmultiplied(col.r(), col.g(), col.b(), 205)
                        };
                        painter.rect_filled(rect, egui::Rounding::same(r), col.gamma_multiply(0.45));
                        let body = egui::Rect::from_min_max(rect.min, rect.max - egui::vec2(0.0, 3.0));
                        painter.rect_filled(body, egui::Rounding::same(r), body_col);
                        if lit {
                            painter.rect_stroke(
                                rect,
                                egui::Rounding::same(r),
                                egui::Stroke::new(2.0, ACTIVE_LOOP_COLOR),
                            );
                        }
                        let ink = egui::Color32::from_gray(16);
                        painter.text(
                            rect.left_top() + egui::vec2(6.0, 3.0),
                            egui::Align2::LEFT_TOP,
                            letter,
                            font::strong(14.0),
                            ink,
                        );
                        let sub = match c.label.as_deref().filter(|l| !l.is_empty()) {
                            Some(l) => l.to_string(),
                            None => fmt_time(c.position_ms as f32 / 1000.0),
                        };
                        painter.text(
                            rect.left_bottom() + egui::vec2(6.0, -5.0),
                            egui::Align2::LEFT_BOTTOM,
                            sub,
                            font::mono_small(),
                            ink,
                        );
                        // Top right: the loop badge, or the ✕ while hovered.
                        let show_x = hot && x_resp.is_some();
                        if let (true, Some(x)) = (show_x, x_resp.as_ref()) {
                            crate::ui::icon::close(
                                &painter,
                                x_rect.center(),
                                if x.hovered() { egui::Color32::WHITE } else { ink },
                                3.5,
                            );
                        }
                        if !show_x {
                            if let Some(end) = c.loop_end_ms {
                                let badge = match loop_beats_of(c.position_ms, end) {
                                    Some(n) if pad_w >= 60.0 => format!("⟲{n}"),
                                    _ => "⟲".to_string(),
                                };
                                painter.text(
                                    rect.right_top() + egui::vec2(-6.0, 3.0),
                                    egui::Align2::RIGHT_TOP,
                                    badge,
                                    font::strong(11.0),
                                    ink,
                                );
                            }
                        }
                        let note = format!(
                            "Hot cue {letter} at {}{}",
                            fmt_time(c.position_ms as f32 / 1000.0),
                            if c.is_loop() { ", a loop" } else { "" }
                        );
                        if x_resp.as_ref().map_or(false, |x| x.clicked()) {
                            action = Some((idx, CueAction::Delete));
                        } else if resp.clicked() {
                            pad_hit = Some(slot);
                        }
                        if hot {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        let is_loop = c.is_loop();
                        resp.clone().on_hover_note(note).context_menu(|ui| {
                            cue_context_menu(ui, idx, is_loop, editable, playhead > c.position_ms, &mut action);
                        });
                    }
                    None => {
                        let hot = resp.hovered() && editable;
                        painter.rect_filled(
                            rect,
                            egui::Rounding::same(r),
                            if hot { color::SURFACE_HOVER } else { color::FIELD },
                        );
                        painter.rect_stroke(
                            rect,
                            egui::Rounding::same(r),
                            egui::Stroke::new(1.0, if hot { color::OUTLINE_HOVER } else { color::SURFACE_HI }),
                        );
                        painter.text(
                            rect.left_top() + egui::vec2(6.0, 3.0),
                            egui::Align2::LEFT_TOP,
                            letter,
                            font::strong(14.0),
                            if hot { color::LABEL_2 } else { color::LABEL_4 },
                        );
                        if hot {
                            let sub = match active {
                                Some(_) => "store loop".to_string(),
                                None => fmt_time(self.snap_ms(playhead) as f32 / 1000.0),
                            };
                            painter.text(
                                rect.left_bottom() + egui::vec2(6.0, -5.0),
                                egui::Align2::LEFT_BOTTOM,
                                sub,
                                font::mono_small(),
                                color::LABEL_2,
                            );
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() && editable {
                            pad_hit = Some(slot);
                        }
                        if editable {
                            resp.on_hover_note(if active.is_some() {
                                "Store the running loop on this pad"
                            } else {
                                "Set a hot cue at the playhead"
                            });
                        }
                    }
                }
            }

            // --- Loop -----------------------------------------------------
            let loop_row = egui::Rect::from_min_max(
                egui::pos2(loop_rect.left(), row_top),
                loop_rect.right_bottom(),
            );
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(loop_row), |ui| {
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    crate::ui::control_row(ui, |ui| {
                        let idx = self.loop_beats.min(LOOP_BEATS.len() - 1);
                        // The loop, quantize and memory controls are deck keys
                        // (see `ui::button::deck`), not the toolbar's push
                        // buttons: this row plays the track, it doesn't manage
                        // the library, and it should look like a different
                        // instrument.
                        if crate::ui::button::deck(ui, "‹", None, idx > 0)
                            .on_hover_note("Halve the loop")
                            .clicked()
                        {
                            loop_cmd = Some(LoopCmd::Beats(idx - 1));
                        }
                        let (lr, _) = ui.allocate_exact_size(
                            egui::vec2(30.0, crate::ui::control_h(ui)),
                            egui::Sense::hover(),
                        );
                        // The beat count is a readout between its two keys.
                        ui.painter().rect_filled(lr, egui::Rounding::same(radius::XS), color::FIELD);
                        ui.painter().text(
                            lr.center(),
                            egui::Align2::CENTER_CENTER,
                            beats.to_string(),
                            font::strong(13.0),
                            if active.is_some() { LOOP_KEY_ON } else { color::LABEL },
                        );
                        if crate::ui::button::deck(ui, "›", None, idx + 1 < LOOP_BEATS.len())
                            .on_hover_note("Double the loop")
                            .clicked()
                        {
                            loop_cmd = Some(LoopCmd::Beats(idx + 1));
                        }
                        // Engaged, the key blinks like a CDJ's instead of
                        // switching its label to a white "Exit".
                        let loop_btn = crate::ui::button::deck(
                            ui,
                            "Loop",
                            active.is_some().then(|| loop_key_blink(ui.ctx())),
                            true,
                        );
                        if loop_btn
                            .on_hover_note(if active.is_some() {
                                "Stop looping"
                            } else {
                                "Loop this many beats from the playhead"
                            })
                            .clicked()
                        {
                            loop_cmd = Some(if active.is_some() { LoopCmd::Exit } else { LoopCmd::BeatLoop });
                        }
                        let q = crate::ui::button::deck(
                            ui,
                            "Q",
                            quantize.then_some(color::ACCENT_HOVER),
                            true,
                        );
                        if q
                            .on_hover_note(if quantize {
                                "Quantize on: cues and loops snap to the beat"
                            } else {
                                "Quantize off: cues land exactly at the playhead"
                            })
                            .clicked()
                        {
                            loop_cmd = Some(LoopCmd::ToggleQuantize);
                        }
                    });
                });
            });

            // --- Memory cues ---------------------------------------------
            let compact_add = mem_rect.width() < ADD_W + SECT_GAP + 120.0;
            let add_w = match (editable, compact_add) {
                (false, _) => 0.0,
                (true, false) => ADD_W,
                (true, true) => ADD_W_COMPACT,
            };
            let chips_row = egui::Rect::from_min_max(
                egui::pos2(mem_rect.left(), row_top),
                egui::pos2(mem_rect.right() - add_w - if editable { SECT_GAP } else { 0.0 }, mem_rect.bottom()),
            );
            let add_row = egui::Rect::from_min_max(
                egui::pos2(mem_rect.right() - add_w, row_top),
                mem_rect.right_bottom(),
            );
            if editable {
                ui.allocate_new_ui(egui::UiBuilder::new().max_rect(add_row), |ui| {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.spacing_mut().item_spacing.x = GAP;
                        crate::ui::control_row(ui, |ui| {
                            let full = memory.len() >= MAX_MEMORY_CUES;
                            if crate::ui::button::deck(ui, if compact_add { "⟲" } else { "+ Loop" }, None, !full)
                                .on_hover_note(if active.is_some() {
                                    "Save the running loop as a memory loop"
                                } else {
                                    "Memory loop of the set length at the playhead"
                                })
                                .clicked()
                            {
                                add_memory = Some(true);
                            }
                            if crate::ui::button::deck(ui, if compact_add { "+" } else { "+ Cue" }, None, !full)
                                .on_hover_note("Memory cue at the playhead")
                                .clicked()
                            {
                                add_memory = Some(false);
                            }
                        });
                    });
                });
            }
            ui.allocate_new_ui(egui::UiBuilder::new().max_rect(chips_row), |ui| {
                // The strip scrolls; nothing of it may run under the buttons.
                ui.set_clip_rect(chips_row.intersect(ui.clip_rect()));
                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                    ui.spacing_mut().item_spacing.x = GAP;
                    // Renaming: the strip becomes the name field until Save.
                    if let Some((idx, buf)) = rename_buf.as_mut() {
                        crate::ui::control_row(ui, |ui| {
                            let edit = ui.add(
                                crate::ui::field::Field::singleline(buf)
                                    .hint("Cue name")
                                    .width((chips_row.width() - 130.0).max(80.0)),
                            );
                            if !edit.has_focus() && !edit.lost_focus() {
                                edit.request_focus();
                            }
                            let enter = edit.lost_focus()
                                && ui.input(|i| i.key_pressed(egui::Key::Enter));
                            if crate::ui::button::button(ui, "Save").clicked() || enter {
                                rename_done = Some(Some(buf.clone()));
                            }
                            if crate::ui::button::button(ui, "Cancel").clicked()
                                || ui.input(|i| i.key_pressed(egui::Key::Escape))
                            {
                                rename_done = Some(None);
                            }
                            let _ = idx;
                        });
                        return;
                    }
                    if memory.is_empty() {
                        ui.label(
                            egui::RichText::new(if editable {
                                "None yet. + Cue marks the playhead."
                            } else {
                                "None on the stick."
                            })
                            .font(font::footnote())
                            .color(color::LABEL_4),
                        );
                        return;
                    }
                    egui::ScrollArea::horizontal()
                        .id_salt("memory_cue_strip")
                        .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
                        .auto_shrink([false, true])
                        .show(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = GAP;
                            // Off the clip's edge, so the first chip's outline
                            // and corner aren't shaved by it.
                            ui.add_space(2.0);
                            for (idx, c) in &memory {
                                let (idx, c) = (*idx, *c);
                                let glyph = if c.is_loop() { "⟲" } else { "▸" };
                                let mut text = format!("{glyph} {}", fmt_time(c.position_ms as f32 / 1000.0));
                                if let (Some(end), true) = (c.loop_end_ms, c.is_loop()) {
                                    if let Some(n) = loop_beats_of(c.position_ms, end) {
                                        text.push_str(&format!(" ·{n}"));
                                    }
                                }
                                let label = c.label.as_deref().filter(|l| !l.is_empty());
                                let g_time = ui.fonts(|f| {
                                    f.layout_no_wrap(text.clone(), font::mono_small(), MEMORY_COLOR)
                                });
                                let g_label = label.map(|l| {
                                    ui.fonts(|f| {
                                        f.layout_no_wrap(l.to_string(), font::footnote(), color::LABEL_2)
                                    })
                                });
                                let pad_x = 8.0;
                                let x_w = if editable { 16.0 } else { 0.0 };
                                let w = pad_x
                                    + g_time.size().x
                                    + g_label.as_ref().map_or(0.0, |g| 6.0 + g.size().x)
                                    + x_w
                                    + pad_x;
                                let (rect, resp) = ui.allocate_exact_size(
                                    egui::vec2(w, CHIP_H),
                                    egui::Sense::click(),
                                );
                                let x_rect = egui::Rect::from_center_size(
                                    egui::pos2(rect.right() - pad_x - 6.0, rect.center().y),
                                    egui::vec2(16.0, 16.0),
                                );
                                let x_resp = editable.then(|| {
                                    ui.interact(x_rect, resp.id.with("x"), egui::Sense::click())
                                        .on_hover_note("Remove this memory cue")
                                });
                                let hot = resp.hovered() || x_resp.as_ref().map_or(false, |x| x.hovered());
                                let lit = same_loop(c);
                                ui.painter().rect_filled(
                                    rect,
                                    egui::Rounding::same(radius::SM),
                                    if hot { color::SURFACE_HOVER } else { color::SURFACE_HI },
                                );
                                ui.painter().rect_stroke(
                                    rect,
                                    egui::Rounding::same(radius::SM),
                                    egui::Stroke::new(
                                        1.0,
                                        if lit { MEMORY_COLOR } else { color::OUTLINE },
                                    ),
                                );
                                let mut x = rect.left() + pad_x;
                                ui.painter().galley(
                                    egui::pos2(x, rect.center().y - g_time.size().y / 2.0),
                                    g_time.clone(),
                                    MEMORY_COLOR,
                                );
                                x += g_time.size().x;
                                if let Some(g) = &g_label {
                                    x += 6.0;
                                    ui.painter().galley(
                                        egui::pos2(x, rect.center().y - g.size().y / 2.0),
                                        g.clone(),
                                        color::LABEL_2,
                                    );
                                }
                                let mut x_clicked = false;
                                if let Some(x) = &x_resp {
                                    crate::ui::icon::close(
                                        ui.painter(),
                                        x_rect.center(),
                                        if x.hovered() { egui::Color32::WHITE } else { color::LABEL_3 },
                                        3.5,
                                    );
                                    x_clicked = x.clicked();
                                }
                                if hot {
                                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                }
                                if x_clicked {
                                    action = Some((idx, CueAction::Delete));
                                } else if resp.clicked() {
                                    action = Some((idx, CueAction::Jump));
                                }
                                let is_loop = c.is_loop();
                                let after = playhead > c.position_ms;
                                resp.on_hover_note(if is_loop { "Play this loop" } else { "Jump to this cue" })
                                    .context_menu(|ui| {
                                        cue_context_menu(ui, idx, is_loop, editable, after, &mut action);
                                    });
                            }
                        });
                });
            });
        });

        if let Some(slot) = pad_hit {
            self.trigger_hot_cue(slot);
        }
        match loop_cmd {
            Some(LoopCmd::BeatLoop) => self.beat_loop_here(),
            Some(LoopCmd::Exit) => self.exit_loop(),
            Some(LoopCmd::Beats(i)) => self.set_loop_beats(i),
            Some(LoopCmd::ToggleQuantize) => {
                self.config.cue_quantize = !self.config.cue_quantize;
                let _ = self.config.save();
            }
            None => {}
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
    if ui.button(if is_loop { "Play loop" } else { "Jump to cue" }).clicked() {
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
        if ui.button("Make it a point").clicked() {
            *action = Some((idx, CueAction::ClearLoop));
            ui.close_menu();
        }
    } else if ui.button("Make it a loop").clicked() {
        *action = Some((idx, CueAction::LoopDefault));
        ui.close_menu();
    }
    if ui
        .add_enabled(playhead_after, egui::Button::new("Loop out at playhead"))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_slot_gets_its_own_colour() {
        let mut seen = std::collections::HashSet::new();
        for slot in 0..Cue::HOT_SLOTS {
            assert!(seen.insert(slot_rgb(slot)), "slot {slot} repeats a colour");
        }
    }

    #[test]
    fn uncoloured_cue_takes_its_slot_colour() {
        let c = Cue {
            hot_slot: Some(5),
            position_ms: 0,
            loop_end_ms: None,
            label: None,
            color: None,
        };
        let [r, g, b] = slot_rgb(5);
        assert_eq!(cue_rgb(&c), egui::Color32::from_rgb(r, g, b));
    }
}
