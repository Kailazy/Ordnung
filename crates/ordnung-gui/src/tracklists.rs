//! The Tracklists window: paste a mix tracklist, and every line gets looked
//! up and matched to its Discogs record. Opened from the tiny ≡ glyph in
//! the top bar, left of the counts; a tool, not a tab.
//!
//! Inside, the saved tracklists sit in a left bar under a search field and
//! a + that opens the paste window: one square box for the text and a
//! Match button, nothing else (⌘V with the window open and nothing focused
//! lands there too). Match saves the paste as a tracklist and
//! runs the background job in `jobs::run_match_tracklist`, whose rows fill
//! in one at a time. A saved tracklist can be reopened in that window (Edit
//! the paste, in the ⋯ menu or the tab's own menu) to add or fix lines;
//! Save writes it back through `Catalog::update_tracklist`, which keeps
//! the matches of lines that didn't change, and only the new lines get
//! looked up. Each row shows the song as pasted, the record it
//! matched (cover, title, year, label and catalog number, format), a
//! confidence pip and OWNED / WANT / IN LIBRARY badges; a click opens the
//! ordinary record sheet, the ↻ at the row's edge looks that one line up
//! again (a line Discogs rate-limited, say), and the context menu wants,
//! digs, re-picks or rejects. Matched records join the record map. Parsing and scoring live
//! in `ordnung_core::tracklist`; see `docs/design/tracklist-match.md`.

use super::*;
use crate::jobs::MatchScope;
use crate::records::SearchScope;
use crate::ui::tokens::{color, font, space};
use egui_extras::{Column, TableBuilder};
use ordnung_core::model::{ChosenBy, DugRelease};
use ordnung_core::tracklist::{self, Confidence, LineKind};

/// What a row click or context-menu pick asked for, applied after the table
/// releases its borrows. Indices are into `tracklist_entries`.
enum LineAct {
    Open(usize),
    Want(usize),
    Dig(usize),
    Buy(usize),
    Pick(usize),
    SearchDiscogs(usize),
    NotThis(usize),
    PlayLocal(usize),
    /// Look this one line up again, whatever it holds.
    Retry(usize),
}

/// Whole-list actions, from the header's ⋯ or a tab's menu in the left bar.
#[derive(Clone, Copy)]
enum ListAct {
    Match,
    Rematch,
    Edit,
    CopyText,
    Delete,
}

/// The whole-list menu, shared by the header's ⋯ and the tab's own menu.
/// `with_match` adds Match, which the header keeps as its own button.
fn list_menu(ui: &mut egui::Ui, busy: bool, with_match: bool) -> Option<ListAct> {
    let mut act = None;
    if with_match
        && ui.add_enabled(!busy, egui::Button::new("Match")).on_hover_note("Look up the lines that aren't settled yet").clicked()
    {
        act = Some(ListAct::Match);
    }
    if ui.add_enabled(!busy, egui::Button::new("Re-match every line")).on_hover_note("Search again for every line you haven't chosen by hand").clicked() {
        act = Some(ListAct::Rematch);
    }
    if ui.button("Edit the paste").on_hover_note("Open the text in the paste window to add, fix or remove lines").clicked() {
        act = Some(ListAct::Edit);
    }
    if ui.button("Copy as text").on_hover_note("Copy the lines with their matched records").clicked() {
        act = Some(ListAct::CopyText);
    }
    ui.separator();
    if ui.button(egui::RichText::new("Delete tracklist").color(color::RED)).on_hover_note("Remove this tracklist. Matched records stay on the map").clicked() {
        act = Some(ListAct::Delete);
    }
    if act.is_some() {
        ui.close_menu();
    }
    act
}

/// One line of text cut to `max_w` with an ellipsis, for painting where a
/// label can't go.
fn truncated(ui: &egui::Ui, text: &str, font: egui::FontId, ink: egui::Color32, max_w: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::text::LayoutJob::simple_singleline(text.to_owned(), font, ink);
    job.wrap = egui::text::TextWrapping::truncate_at_width(max_w.max(0.0));
    ui.fonts(|f| f.layout_job(job))
}

/// `17 Sep 2026` from unix seconds, without a date crate: the civil-date
/// algorithm from Howard Hinnant's `days_from_civil` inverse.
pub(crate) fn fmt_day(unix: i64) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let days = unix.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{d} {} {y}", MONTHS[(m - 1) as usize])
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// `12:34` / `1:02:03` for a line's timestamp.
fn fmt_clock(secs: u32) -> String {
    let (h, m, s) = (secs / 3600, (secs / 60) % 60, secs % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Pad a cell so its one- or two-line text block (body line, 1 pt, caption
/// line) sits in the middle of the row rather than against its top.
fn center_lines(ui: &mut egui::Ui, two: bool) {
    let body = ui.text_style_height(&egui::TextStyle::Body);
    let block = if two {
        body + 1.0 + ui.fonts(|f| f.row_height(&font::caption()))
    } else {
        body
    };
    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
}

/// The caption under a matched record: `2001 · Environ ENV 006 · Vinyl, 12"`.
fn record_sub(e: &TracklistEntry) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(y) = e.rel_year {
        parts.push(y.to_string());
    }
    match (e.rel_label.as_deref(), e.rel_catno.as_deref()) {
        (Some(l), Some(c)) => parts.push(format!("{l} {c}")),
        (Some(l), None) => parts.push(l.to_string()),
        (None, Some(c)) => parts.push(c.to_string()),
        (None, None) => {}
    }
    if let Some(f) = e.rel_format.as_deref() {
        parts.push(f.to_string());
    }
    parts.join(" · ")
}

/// Colour and words for a line's confidence pip.
fn confidence_look(e: &TracklistEntry) -> (egui::Color32, &'static str) {
    if e.kind == LineKind::Id {
        return (color::LABEL_4, "An ID: the mix's author couldn't name it");
    }
    match (e.release_id.is_some(), e.confidence, e.chosen_by) {
        (true, _, ChosenBy::User) => (color::GREEN, "Chosen by you"),
        (true, Confidence::Sure, _) => (color::GREEN, "Sure: the record's tracklist carries this song"),
        (true, Confidence::Likely, _) => (color::TEAL, "Likely: artist and title agree"),
        (true, _, _) => (color::ORANGE, "Unsure: the best guess, unconfirmed"),
        (false, _, ChosenBy::User) => (color::LABEL_4, "Rejected by you"),
        (false, _, ChosenBy::Auto) => (color::RED, "Not found on Discogs"),
        (false, _, ChosenBy::Nobody) => (color::LABEL_4, "Not matched yet"),
    }
}

/// Does a line match the tab's search? Every term must appear in the song,
/// the raw line or the matched record.
fn entry_matches(e: &TracklistEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let hay = format!(
        "{} {} {} {} {} {} {}",
        e.raw,
        e.artist.as_deref().unwrap_or(""),
        e.title.as_deref().unwrap_or(""),
        e.rel_track.as_deref().unwrap_or(""),
        e.rel_artist.as_deref().unwrap_or(""),
        e.rel_title.as_deref().unwrap_or(""),
        e.rel_label.as_deref().unwrap_or(""),
    )
    .to_lowercase();
    query.split_whitespace().all(|t| hay.contains(t))
}

impl App {
    /// Load the current tracklist's lines if they aren't the ones loaded.
    fn ensure_tracklist_entries(&mut self) {
        let Some(id) = self.tracklist_current else {
            self.tracklist_entries.clear();
            self.tracklist_entries_for = None;
            return;
        };
        if self.tracklist_entries_for == Some(id) {
            return;
        }
        self.tracklist_entries = Catalog::open(&self.db_path)
            .and_then(|c| c.tracklist_entries(id))
            .unwrap_or_default();
        self.tracklist_entries_for = Some(id);
    }

    pub(crate) fn reload_tracklists(&mut self) {
        self.tracklists = Catalog::open(&self.db_path)
            .and_then(|c| c.list_tracklists())
            .unwrap_or_default();
        self.tracklist_entries_for = None;
    }

    /// The window itself; everything else here draws inside it.
    pub(crate) fn draw_tracklist_window(&mut self, ctx: &egui::Context) {
        if !self.tracklist_open {
            return;
        }
        let mut open = true;
        crate::ui::window::Window::new("Tracklists")
            .id(egui::Id::new("tracklists_window"))
            .open(&mut open)
            .resizable(true)
            .default_size(egui::vec2(780.0, 540.0))
            .min_size(egui::vec2(560.0, 320.0))
            .show(ctx, |ui| {
                let query = self.tracklist_filter.trim().to_lowercase();
                self.draw_tracklists(ui, ctx, &query);
            });
        if !open {
            self.tracklist_open = false;
        }
    }

    fn draw_tracklists(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, query: &str) {
        // ⌘V with nothing focused lands in the paste window, opening it if
        // it isn't. While a saved tracklist is being edited there, the
        // paste adds to it rather than replacing what is being edited.
        let pasted = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Paste(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            })
        });
        if let Some(t) = pasted {
            if ctx.memory(|m| m.focused().is_none()) {
                if self.tracklist_paste_open
                    && self.tracklist_editing.is_some()
                    && !self.tracklist_paste.trim().is_empty()
                {
                    if !self.tracklist_paste.ends_with('\n') {
                        self.tracklist_paste.push('\n');
                    }
                    self.tracklist_paste.push_str(&t);
                } else {
                    self.tracklist_paste = t;
                    self.tracklist_editing = None;
                }
                self.tracklist_paste_open = true;
                self.tracklist_focus_paste = true;
            }
        }
        // Opening the window with nothing saved yet goes straight to the
        // paste; with tracklists to show, it just shows them.
        if self.tracklist_focus_paste && !self.tracklist_paste_open {
            if self.tracklists.is_empty() {
                self.tracklist_paste_open = true;
            } else {
                self.tracklist_focus_paste = false;
            }
        }
        if self.tracklist_current.is_none() {
            self.tracklist_current = self.tracklists.first().map(|t| t.id);
        }
        self.ensure_tracklist_entries();

        if self.tracklists.is_empty() {
            ui.add_space(space::S6);
            ui.vertical_centered(|ui| {
                ui.label(
                    egui::RichText::new("No tracklists yet")
                        .font(font::headline())
                        .color(color::LABEL_2),
                );
                ui.add_space(space::S3);
                if crate::ui::button::button(ui, "Paste a tracklist")
                    .on_hover_note("Paste a mix's tracklist and match every line to a record")
                    .clicked()
                {
                    self.open_tracklist_paste(None);
                }
            });
            self.draw_tracklist_paste_window(ctx);
            return;
        }

        egui::SidePanel::left("tracklists_side")
            .resizable(true)
            .default_width(200.0)
            .min_width(150.0)
            .show_inside(ui, |ui| self.draw_tracklist_side(ui, ctx));
        egui::CentralPanel::default().show_inside(ui, |ui| {
            self.draw_tracklist_rows(ui, ctx, query);
        });
        self.draw_tracklist_pick(ctx);
        self.draw_tracklist_paste_window(ctx);
    }

    /// Open the paste window: empty for a new tracklist, or holding a
    /// saved one's text to add, fix or remove lines.
    fn open_tracklist_paste(&mut self, edit: Option<Id>) {
        self.tracklist_paste.clear();
        self.tracklist_editing = None;
        if let Some(t) = edit.and_then(|id| self.tracklists.iter().find(|t| t.id == id)) {
            self.tracklist_paste = t.pasted_text.clone();
            if !self.tracklist_paste.ends_with('\n') {
                self.tracklist_paste.push('\n');
            }
            self.tracklist_editing = Some(t.id);
        }
        self.tracklist_paste_open = true;
        self.tracklist_focus_paste = true;
    }

    /// The paste window: one square box for the text and a Match button in
    /// its corner, nothing else. The name comes from a `Tracklist:` line in
    /// the paste, else the day; a saved tracklist reopened here keeps its
    /// name, and Match reads Save.
    fn draw_tracklist_paste_window(&mut self, ctx: &egui::Context) {
        if !self.tracklist_paste_open {
            return;
        }
        const SIDE: f32 = 440.0;
        let box_id = egui::Id::new("tracklist_paste_box");
        let editing = self
            .tracklist_editing
            .and_then(|id| self.tracklists.iter().find(|t| t.id == id))
            .cloned();
        let focus = std::mem::take(&mut self.tracklist_focus_paste);
        let busy = self.is_busy();
        let mut open = true;
        let mut save = false;
        crate::ui::window::Window::new(if editing.is_some() { "Edit tracklist" } else { "New tracklist" })
            .id(egui::Id::new("tracklist_paste_window"))
            .open(&mut open)
            .show(ctx, |ui| {
                ui.set_width(SIDE);
                egui::ScrollArea::vertical()
                    .id_salt("tracklist_paste_scroll")
                    .max_height(SIDE)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let resp = crate::ui::field::Field::multiline(&mut self.tracklist_paste)
                            .id(box_id)
                            .width(SIDE)
                            .min_size(egui::vec2(SIDE, SIDE))
                            .hint("Paste the tracklist here")
                            .font(egui::TextStyle::Body)
                            .show(ui);
                        if focus {
                            resp.request_focus();
                        }
                    });
                ui.add_space(space::S3);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let lines = tracklist::parse_tracklist(&self.tracklist_paste);
                    let can = lines.iter().any(|l| l.kind != LineKind::Noise) && !busy;
                    let (verb, note) = match (&editing, busy) {
                        (_, true) => ("Match", "Wait for the current job to finish"),
                        (Some(_), false) => ("Save", "Save the edit. Lines that didn't change keep their records; new ones get looked up"),
                        (None, false) => ("Match", "Save the tracklist and look every line up on Discogs"),
                    };
                    if crate::ui::button::button_enabled(ui, can, verb).on_hover_note(note).clicked() {
                        save = true;
                    }
                });
            });
        if !open {
            self.close_tracklist_paste(ctx, box_id);
            return;
        }
        if !save {
            return;
        }
        let text = self.tracklist_paste.clone();
        let lines = tracklist::parse_tracklist(&text);
        let name = match &editing {
            Some(t) => t.name.clone(),
            None => tracklist::suggested_name(&text)
                .unwrap_or_else(|| format!("Pasted {}", fmt_day(now_unix()))),
        };
        let saved = match editing {
            Some(t) => Catalog::open(&self.db_path)
                .and_then(|c| c.update_tracklist(t.id, &name, &text, &lines))
                .map(|()| t.id),
            None => Catalog::open(&self.db_path).and_then(|c| c.create_tracklist(&name, &text, &lines)),
        };
        match saved {
            Ok(id) => {
                self.reload_tracklists();
                self.tracklist_current = Some(id);
                self.close_tracklist_paste(ctx, box_id);
                // Only what isn't settled gets looked up, so an edit that
                // adds three lines costs three lines' requests.
                self.ensure_tracklist_entries();
                let unsettled = self.tracklist_entries.iter().any(|e| {
                    e.kind == LineKind::Track
                        && e.chosen_by != ChosenBy::User
                        && (e.release_id.is_none() || e.confidence < Confidence::Likely)
                });
                if unsettled {
                    self.spawn_match_tracklist(ctx.clone(), id, MatchScope::Unsettled);
                } else {
                    self.status = format!("Saved the tracklist {name}.");
                }
            }
            Err(e) => self.fail(format!("Couldn't save the tracklist: {e}")),
        }
    }

    fn close_tracklist_paste(&mut self, ctx: &egui::Context, box_id: egui::Id) {
        self.tracklist_paste.clear();
        self.tracklist_editing = None;
        self.tracklist_paste_open = false;
        ctx.memory_mut(|m| m.surrender_focus(box_id));
    }

    /// The left bar: the search field with the + for a new paste above the
    /// saved tracklists, newest first. A tab's secondary click opens the
    /// same menu the header's ⋯ has.
    fn draw_tracklist_side(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let busy = self.is_busy();
        let mut pick: Option<Id> = None;
        let mut act: Option<(Id, ListAct)> = None;
        let mut new_paste = false;
        ui.add_space(space::S2);
        // The row takes its own height only; a bare `with_layout` here
        // would claim the whole bar and leave the tabs nowhere to go.
        let row = egui::vec2(ui.available_width(), crate::ui::control_h(ui));
        ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            crate::ui::control_row(ui, |ui| {
                if crate::ui::button::button(ui, "+").on_hover_note("Paste a new tracklist").clicked() {
                    new_paste = true;
                }
                crate::ui::field::Field::singleline(&mut self.tracklist_filter)
                    .width(ui.available_width())
                    .hint("Search")
                    .show(ui)
                    .on_hover_note("Filter the lines by song, artist, label or matched record");
            });
        });
        ui.add_space(space::S2);
        let lists = self.tracklists.clone();
        egui::ScrollArea::vertical()
            .id_salt("tracklists_side_scroll")
            .show(ui, |ui| {
                for t in &lists {
                    let active = self.tracklist_current == Some(t.id);
                    let w = ui.available_width();
                    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 40.0), egui::Sense::click());
                    if active {
                        ui.painter().rect_filled(rect, egui::Rounding::same(6.0), color::SURFACE_HI);
                    } else if resp.hovered() {
                        ui.painter().rect_filled(rect, egui::Rounding::same(6.0), color::SURFACE);
                    }
                    let ink = if active || resp.hovered() { color::LABEL } else { color::LABEL_2 };
                    // Cut to the bar, not spilling past it.
                    let inner_w = w - 2.0 * space::S3;
                    let name = truncated(ui, &t.name, font::strong(font::body().size), ink, inner_w);
                    ui.painter().galley(egui::pos2(rect.left() + space::S3, rect.top() + 6.0), name, ink);
                    let sub = truncated(
                        ui,
                        &format!("{} of {} matched · {}", t.matched, t.lines, fmt_day(t.created_at)),
                        font::caption(),
                        color::LABEL_3,
                        inner_w,
                    );
                    let sub_h = sub.size().y;
                    ui.painter().galley(egui::pos2(rect.left() + space::S3, rect.bottom() - 6.0 - sub_h), sub, color::LABEL_3);
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() {
                        pick = Some(t.id);
                    }
                    resp.context_menu(|ui| {
                        if let Some(a) = list_menu(ui, busy, true) {
                            act = Some((t.id, a));
                        }
                    });
                }
            });
        if new_paste {
            self.open_tracklist_paste(None);
        }
        if let Some(id) = pick {
            self.tracklist_current = Some(id);
            self.tracklist_entries_for = None;
        }
        if let Some((id, a)) = act {
            // The action reads the current tracklist's lines, so the tab it
            // came from becomes current first.
            self.tracklist_current = Some(id);
            self.tracklist_entries_for = None;
            self.ensure_tracklist_entries();
            self.apply_list_act(ctx, id, a);
        }
    }

    /// One whole-list action on tracklist `id`, whose lines are loaded.
    fn apply_list_act(&mut self, ctx: &egui::Context, id: Id, act: ListAct) {
        let Some(t) = self.tracklists.iter().find(|t| t.id == id).cloned() else { return };
        match act {
            ListAct::Match => self.spawn_match_tracklist(ctx.clone(), id, MatchScope::Unsettled),
            ListAct::Rematch => self.spawn_match_tracklist(ctx.clone(), id, MatchScope::All),
            ListAct::Edit => self.open_tracklist_paste(Some(id)),
            ListAct::CopyText => {
                let mut out = String::new();
                for e in &self.tracklist_entries {
                    let song = if e.kind == LineKind::Id { "ID".to_string() } else { e.song_label() };
                    out.push_str(&format!("{:02}. {song}", e.position));
                    if e.release_id.is_some() {
                        let name = match (e.rel_artist.as_deref(), e.rel_title.as_deref()) {
                            (Some(a), Some(t)) if !a.is_empty() => format!("{a} - {t}"),
                            (_, Some(t)) => t.to_string(),
                            _ => String::new(),
                        };
                        out.push_str(&format!("  →  {name}"));
                        let sub = record_sub(e);
                        if !sub.is_empty() {
                            out.push_str(&format!(" ({sub})"));
                        }
                    }
                    out.push('\n');
                }
                ctx.output_mut(|o| o.copied_text = out);
                self.status = format!("Copied {} lines.", self.tracklist_entries.len());
            }
            ListAct::Delete => {
                match Catalog::open(&self.db_path).and_then(|c| c.delete_tracklist(id)) {
                    Ok(()) => {
                        self.tracklist_current = None;
                        self.reload_tracklists();
                        self.status = format!("Deleted the tracklist {}.", t.name);
                    }
                    Err(e) => self.fail(format!("Couldn't delete the tracklist: {e}")),
                }
            }
        }
    }

    /// The current tracklist: its header actions and one row per line.
    fn draw_tracklist_rows(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, query: &str) {
        let Some(id) = self.tracklist_current else { return };
        let Some(t) = self.tracklists.iter().find(|t| t.id == id).cloned() else { return };
        let busy = self.is_busy();

        let mut act: Option<ListAct> = None;
        // One control row: the buttons take their width first; the name and
        // status get what's left and truncate, so a narrow window never runs
        // them together.
        let row = egui::vec2(ui.available_width(), crate::ui::control_h(ui));
        ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
            {
                crate::ui::control_row(ui, |ui| {
                    ui.menu_button("⋯", |ui| {
                        act = list_menu(ui, busy, false);
                    });
                    let unsettled = self
                        .tracklist_entries
                        .iter()
                        .filter(|e| e.kind == LineKind::Track && e.chosen_by != ChosenBy::User && (e.release_id.is_none() || e.confidence < Confidence::Likely))
                        .count();
                    if crate::ui::button::button_enabled(ui, unsettled > 0 && !busy, "Match")
                        .on_hover_note("Look up the lines that aren't settled yet")
                        .clicked()
                    {
                        act = Some(ListAct::Match);
                    }
                });
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&t.name).font(font::headline()))
                        .truncate(),
                );
                let when = match t.matched_at {
                    Some(m) => format!("{} of {} matched · matched {}", t.matched, t.lines, fmt_day(m)),
                    None => format!("{} lines · not matched yet", t.lines),
                };
                ui.add(egui::Label::new(egui::RichText::new(when).weak()).truncate());
            });
        });
        ui.add_space(space::S2);

        // The rows.
        let shown: Vec<usize> = self
            .tracklist_entries
            .iter()
            .enumerate()
            .filter(|(_, e)| entry_matches(e, query))
            .map(|(i, _)| i)
            .collect();
        // Kick off cover decodes for what's on screen (deduplicated).
        for &i in &shown {
            if let Some(u) = self.tracklist_entries[i].rel_thumb.clone() {
                let _ = self.dig_cover(&u);
            }
        }
        let mut line_act: Option<LineAct> = None;
        let row_h = 46.0;
        const THUMB: f32 = 36.0;
        TableBuilder::new(ui)
            .id_salt(("tracklist_rows", id))
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(30.0))
            .column(Column::exact(THUMB + 6.0))
            .column(Column::remainder().at_least(160.0))
            .column(Column::remainder().at_least(160.0))
            .column(Column::exact(140.0))
            .body(|body| {
                body.rows(row_h, shown.len(), |mut row| {
                    let i = shown[row.index()];
                    let e = self.tracklist_entries[i].clone();
                    let tex = e.rel_thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());
                    // #
                    row.col(|ui| {
                        ui.label(egui::RichText::new(format!("{}", e.position)).font(font::mono_small()).color(color::LABEL_3));
                    });
                    // cover
                    row.col(|ui| {
                        let (rect, _) = ui.allocate_exact_size(egui::vec2(THUMB, THUMB), egui::Sense::hover());
                        match &tex {
                            Some(h) => {
                                egui::Image::new(h)
                                    .fit_to_exact_size(egui::vec2(THUMB, THUMB))
                                    .rounding(egui::Rounding::same(4.0))
                                    .paint_at(ui, rect);
                            }
                            None => {
                                if e.release_id.is_some() {
                                    ui.painter().rect_filled(rect, egui::Rounding::same(4.0), egui::Color32::from_gray(34));
                                    ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "♪", egui::FontId::proportional(16.0), egui::Color32::from_gray(70));
                                }
                            }
                        }
                    });
                    // the song as pasted: its name, with the clock and any
                    // hints under it; the artist is what the record column
                    // names
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            // The record's spelling wins over the paste's
                            // once the record's tracklist settled the line.
                            let label = match (e.kind, e.rel_track.as_deref().or(e.title.as_deref())) {
                                (LineKind::Id, _) => "ID".to_string(),
                                (_, Some(t)) => t.to_string(),
                                _ => e.song_label(),
                            };
                            let mut sub = Vec::new();
                            if let Some(ts) = e.timestamp_s {
                                sub.push(fmt_clock(ts));
                            }
                            if let Some(l) = e.label_hint.as_deref() {
                                sub.push(l.to_string());
                            }
                            if let Some(c) = e.catno_hint.as_deref() {
                                sub.push(c.to_string());
                            }
                            center_lines(ui, !sub.is_empty());
                            let name = ui.add(egui::Label::new(egui::RichText::new(label).color(color::LABEL)).truncate());
                            if let (Some(_), Some(pasted)) = (e.rel_track.as_deref(), e.title.as_deref()) {
                                name.on_hover_note(format!("Pasted as {pasted}"));
                            }
                            if !sub.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(sub.join(" · ")).font(font::caption()).color(color::LABEL_3)).truncate());
                            }
                        });
                    });
                    // the matched record
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            match e.release_id {
                                Some(_) => {
                                    let name = match (e.rel_artist.as_deref(), e.rel_title.as_deref()) {
                                        (Some(a), Some(t)) if !a.is_empty() => format!("{a} – {t}"),
                                        (_, Some(t)) => t.to_string(),
                                        _ => String::new(),
                                    };
                                    center_lines(ui, true);
                                    ui.add(egui::Label::new(egui::RichText::new(name).color(color::LABEL)).truncate());
                                    ui.add(egui::Label::new(egui::RichText::new(record_sub(&e)).font(font::caption()).color(color::LABEL_3)).truncate());
                                }
                                None => {
                                    // No record: the pasted artist keeps its
                                    // place here, the status under it.
                                    let (_, words) = confidence_look(&e);
                                    let text = match (e.kind, e.chosen_by) {
                                        (LineKind::Id, _) => "",
                                        (_, ChosenBy::Nobody) => "Not matched yet",
                                        (_, ChosenBy::User) => "Rejected",
                                        (_, ChosenBy::Auto) => "Not found on Discogs",
                                    };
                                    match e.artist.as_deref().filter(|a| !a.is_empty() && e.kind != LineKind::Id) {
                                        Some(a) => {
                                            center_lines(ui, true);
                                            ui.add(egui::Label::new(egui::RichText::new(a).color(color::LABEL_2)).truncate());
                                            ui.add(egui::Label::new(egui::RichText::new(text).font(font::caption()).color(color::LABEL_3)).truncate())
                                                .on_hover_note(words);
                                        }
                                        None => {
                                            center_lines(ui, false);
                                            ui.label(egui::RichText::new(text).color(color::LABEL_3)).on_hover_note(words);
                                        }
                                    }
                                }
                            }
                        });
                    });
                    // pip + badges, and the ↻ at the row's edge
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if e.kind == LineKind::Track {
                                let note = match (busy, e.release_id) {
                                    (true, _) => "Wait for the current job to finish",
                                    (false, Some(_)) => "Look this line up on Discogs again",
                                    (false, None) => "Look this line up on Discogs again. Use it once Discogs has stopped asking us to slow down",
                                };
                                if crate::ui::button::glyph(ui, "↻", !busy).on_hover_note(note).clicked() {
                                    line_act = Some(LineAct::Retry(i));
                                }
                            }
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                let (c, words) = confidence_look(&e);
                                let (pr, presp) = ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                                ui.painter().circle_filled(pr.center(), 4.0, c);
                                presp.on_hover_note(words);
                                let badge = |ui: &mut egui::Ui, text: &str, col: egui::Color32, tip: &str| {
                                    ui.add(egui::Label::new(
                                        egui::RichText::new(text).font(font::caption()).color(col).strong(),
                                    ))
                                    .on_hover_note(tip);
                                };
                                if let Some(r) = e.release_id {
                                    if self.vinyl_owned.contains(&r) {
                                        badge(ui, "OWNED", color::GREEN, "In your collection");
                                    } else if self.vinyl_wanted.contains(&r) {
                                        badge(ui, "WANT", color::ACCENT_HOVER, "On your wantlist");
                                    }
                                }
                                if e.local_track_id.is_some() {
                                    badge(ui, "FILE", color::LABEL_2, "A track in your library is this song");
                                }
                            });
                        });
                    });
                    let resp = row.response();
                    if resp.hovered() && e.release_id.is_some() {
                        ui_cursor_hand(ctx);
                    }
                    if resp.clicked() {
                        if e.release_id.is_some() {
                            line_act = Some(LineAct::Open(i));
                        } else if e.kind == LineKind::Track {
                            line_act = Some(LineAct::SearchDiscogs(i));
                        }
                    }
                    resp.context_menu(|ui| {
                        if let Some(r) = e.release_id {
                            if ui.button("Open record").clicked() {
                                line_act = Some(LineAct::Open(i));
                                ui.close_menu();
                            }
                            if !self.vinyl_owned.contains(&r) && !self.vinyl_wanted.contains(&r) {
                                if ui.add_enabled(!busy, egui::Button::new("♥ Want")).clicked() {
                                    line_act = Some(LineAct::Want(i));
                                    ui.close_menu();
                                }
                            }
                            if ui.button("◈ Dig from it").clicked() {
                                line_act = Some(LineAct::Dig(i));
                                ui.close_menu();
                            }
                            if ui.button("↗ Open on Discogs").clicked() {
                                line_act = Some(LineAct::Buy(i));
                                ui.close_menu();
                            }
                            ui.separator();
                        }
                        if e.local_track_id.is_some() && ui.button("▶ Play my file").clicked() {
                            line_act = Some(LineAct::PlayLocal(i));
                            ui.close_menu();
                        }
                        if e.kind == LineKind::Track {
                            if !e.candidates.is_empty() && ui.button("Pick another…").on_hover_note("Choose among the records the search turned up").clicked() {
                                line_act = Some(LineAct::Pick(i));
                                ui.close_menu();
                            }
                            if ui.add_enabled(!busy, egui::Button::new("↻ Look up again")).on_hover_note("Search Discogs for this line once more").clicked() {
                                line_act = Some(LineAct::Retry(i));
                                ui.close_menu();
                            }
                            if ui.button("Search Discogs for this line").on_hover_note("Put the line in the search box, Discogs mode").clicked() {
                                line_act = Some(LineAct::SearchDiscogs(i));
                                ui.close_menu();
                            }
                            if e.release_id.is_some() && ui.button("✖ Not this record").on_hover_note("Clear the match. A re-match won't touch this line").clicked() {
                                line_act = Some(LineAct::NotThis(i));
                                ui.close_menu();
                            }
                        }
                    });
                });
            });

        if let Some(a) = act {
            self.apply_list_act(ctx, id, a);
        }

        let Some(act) = line_act else { return };
        let Some(e) = self.tracklist_entries.get(match act {
            LineAct::Open(i) | LineAct::Want(i) | LineAct::Dig(i) | LineAct::Buy(i) | LineAct::Pick(i)
            | LineAct::SearchDiscogs(i) | LineAct::NotThis(i) | LineAct::PlayLocal(i) | LineAct::Retry(i) => i,
        }).cloned() else { return };
        let rel_artist = e.rel_artist.clone().unwrap_or_default();
        let rel_title = e.rel_title.clone().unwrap_or_default();
        match act {
            LineAct::Retry(_) => self.spawn_match_tracklist(ctx.clone(), id, MatchScope::Line(e.position)),
            LineAct::Open(_) => {
                if let Some(r) = e.release_id {
                    self.open_release_sheet(r, rel_artist, rel_title, record_sub(&e), e.rel_thumb.clone(), ctx);
                    // The sheet lights the song's row the way the pointer
                    // would, so it's found at a glance.
                    if let Some(sheet) = self.vinyl_sheet.as_mut().filter(|s| s.release_id == r) {
                        sheet.mark = e.rel_track.clone().or_else(|| e.title.clone());
                    }
                }
            }
            LineAct::Want(_) => {
                if let Some(r) = e.release_id {
                    self.request_vinyl_edit(
                        ctx.clone(),
                        VinylEdit::Want {
                            release_ids: vec![r],
                            label: format!("{rel_artist} — {rel_title}"),
                        },
                    );
                }
            }
            LineAct::Dig(_) => {
                if let Some(r) = e.release_id {
                    let sub = match (e.rel_year, e.rel_format.as_deref()) {
                        (Some(y), Some(f)) => format!("{y} · {f}"),
                        (Some(y), None) => y.to_string(),
                        (None, Some(f)) => f.to_string(),
                        (None, None) => String::new(),
                    };
                    self.start_dig_release(r, rel_artist, rel_title, e.rel_label.clone(), sub, e.rel_thumb.clone());
                }
            }
            LineAct::Buy(_) => {
                if let Some(r) = e.release_id {
                    open_url(&format!("https://www.discogs.com/release/{r}"));
                }
            }
            LineAct::Pick(_) => self.tracklist_pick = Some((e.tracklist_id, e.position)),
            LineAct::SearchDiscogs(_) => {
                self.search_query = e.song_label();
                self.set_search_scope(SearchScope::Discogs);
                self.search_popup_open = true;
                self.start_record_search();
            }
            LineAct::NotThis(_) => {
                self.settle_tracklist_line(e.tracklist_id, e.position, None, &e.candidates);
            }
            LineAct::PlayLocal(_) => {
                if let Some(tid) = e.local_track_id {
                    match Catalog::open(&self.db_path).and_then(|c| c.get_track(tid)) {
                        Ok(t) => self.play_track(tid, PathBuf::from(t.source_path)),
                        Err(e) => self.fail(format!("Couldn't find that track: {e}")),
                    }
                }
            }
        }
    }

    /// Write a user's choice for one line and refresh what's shown.
    fn settle_tracklist_line(
        &mut self,
        tracklist_id: Id,
        position: u32,
        release: Option<&discogs::ReleaseCandidate>,
        candidates: &[discogs::ReleaseCandidate],
    ) {
        let conf = if release.is_some() { Confidence::Sure } else { Confidence::None };
        let res = Catalog::open(&self.db_path).and_then(|c| {
            c.set_tracklist_match(tracklist_id, position, release, conf, ChosenBy::User, candidates)
        });
        if let Err(e) = res {
            self.fail(format!("Couldn't save the pick: {e}"));
            return;
        }
        if let Some(c) = release {
            let sub = match (c.year.trim(), c.format.trim()) {
                ("", "") => String::new(),
                (y, "") => y.to_string(),
                ("", f) => f.to_string(),
                (y, f) => format!("{y} · {f}"),
            };
            self.note_dug(DugRelease {
                release_id: c.release_id.parse().unwrap_or(0),
                artist: c.artist.clone(),
                title: c.release_title().to_string(),
                label: Some(c.label.clone()).filter(|l| !l.is_empty()),
                sub,
                thumb_url: Some(c.thumb_url.clone()).filter(|u| !u.is_empty()),
                wanted: false,
                dug_at: now_unix(),
            });
        }
        self.reload_tracklists();
    }

    /// The "pick another" window: the search's candidates for one line.
    fn draw_tracklist_pick(&mut self, ctx: &egui::Context) {
        let Some((tl, pos)) = self.tracklist_pick else { return };
        let Some(e) = self
            .tracklist_entries
            .iter()
            .find(|e| e.tracklist_id == tl && e.position == pos)
            .cloned()
        else {
            self.tracklist_pick = None;
            return;
        };
        for c in &e.candidates {
            if !c.thumb_url.is_empty() {
                let _ = self.dig_cover(&c.thumb_url);
            }
        }
        let mut open = true;
        let mut choice: Option<Option<discogs::ReleaseCandidate>> = None;
        crate::ui::window::Window::new(format!("Which record is {}?", e.song_label()))
            .id(egui::Id::new(("tracklist_pick", tl, pos)))
            .open(&mut open)
            .anchored(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.set_min_width(560.0);
                egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                    for c in &e.candidates {
                        let current = e.release_id.map(|r| r.to_string()).as_deref() == Some(c.release_id.as_str());
                        let w = ui.available_width();
                        let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, 52.0), egui::Sense::click());
                        if current {
                            ui.painter().rect_filled(rect, egui::Rounding::same(6.0), color::ACCENT_SOFT.gamma_multiply(0.45));
                        } else if resp.hovered() {
                            ui.painter().rect_filled(rect, egui::Rounding::same(6.0), color::SURFACE);
                        }
                        let thumb = egui::Rect::from_min_size(rect.min + egui::vec2(4.0, 4.0), egui::vec2(44.0, 44.0));
                        let tex = (!c.thumb_url.is_empty())
                            .then(|| self.dig_cover(&c.thumb_url).cloned())
                            .flatten();
                        match tex {
                            Some(h) => {
                                egui::Image::new(&h).fit_to_exact_size(thumb.size()).rounding(egui::Rounding::same(4.0)).paint_at(ui, thumb);
                            }
                            _ => {
                                ui.painter().rect_filled(thumb, egui::Rounding::same(4.0), egui::Color32::from_gray(34));
                            }
                        }
                        let x = thumb.right() + space::S3;
                        ui.painter().text(
                            egui::pos2(x, rect.top() + 9.0),
                            egui::Align2::LEFT_TOP,
                            &c.title,
                            font::strong(font::body().size),
                            color::LABEL,
                        );
                        let mut meta: Vec<&str> = Vec::new();
                        for s in [c.year.as_str(), c.label.as_str(), c.catno.as_str(), c.format.as_str()] {
                            if !s.is_empty() {
                                meta.push(s);
                            }
                        }
                        ui.painter().text(
                            egui::pos2(x, rect.bottom() - 8.0),
                            egui::Align2::LEFT_BOTTOM,
                            format!("{} · {} have, {} want", meta.join(" · "), c.in_collection, c.in_wantlist),
                            font::caption(),
                            color::LABEL_3,
                        );
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            choice = Some(Some(c.clone()));
                        }
                    }
                });
                ui.add_space(space::S2);
                crate::ui::control_row(ui, |ui| {
                    ui.horizontal(|ui| {
                        if ui.button("None of these").on_hover_note("Clear the match; a re-match won't touch this line").clicked() {
                            choice = Some(None);
                        }
                        if ui.button("Search Discogs instead").clicked() {
                            self.search_query = e.song_label();
                            self.set_search_scope(SearchScope::Discogs);
                            self.search_popup_open = true;
                            self.start_record_search();
                            choice = Some(Some(discogs::ReleaseCandidate { release_id: String::new(), ..e.candidates[0].clone() }));
                        }
                    });
                });
            });
        if let Some(pick) = choice {
            match pick {
                Some(c) if c.release_id.is_empty() => {}
                Some(c) => self.settle_tracklist_line(tl, pos, Some(&c), &e.candidates),
                None => self.settle_tracklist_line(tl, pos, None, &e.candidates),
            }
            open = false;
        }
        if !open {
            self.tracklist_pick = None;
        }
    }
}

fn ui_cursor_hand(ctx: &egui::Context) {
    ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
}
