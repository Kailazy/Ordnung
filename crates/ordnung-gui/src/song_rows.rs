//! One table for a set of songs on records.
//!
//! The crate of liked songs (`liked`) and every crate (`crates`) hold
//! [`SongPin`] rows, and both show them through this table: cover, the
//! song, the record it sits on, and the marks at the row's edge. The
//! table reports what a row asked for as a [`SongAct`], and
//! [`App::apply_song_act`] does it, so opening a record, playing the file,
//! liking or putting a song in a crate works the same from either set.
//! A row can be dragged: the drag carries a [`DraggedSongs`] payload that a
//! crate in the sidebar (or the open crate view) takes.

use super::*;
use crate::liked::LikeSpec;
use crate::ui::hover::HoverNoteExt;
use crate::ui::tokens::{color, font};
use ordnung_core::model::SongPin;

/// Which set the rows are from: decides the marks at the row's edge and
/// what the context menu offers.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SongSet {
    Liked,
    CrateSet(Id),
}

/// What a row asked for, applied once the table has let go of its
/// borrows. Indices are into the rows the table was given.
pub(crate) enum SongAct {
    Open(usize),
    PlayLocal(usize),
    Search(usize),
    /// Like the song, or unlike it if it's liked.
    ToggleLike(usize),
    /// Put the song in that crate.
    AddToCrate(usize, Id),
    /// Take the song out of the crate the rows are from.
    RemoveFromCrate(usize),
    /// Mark the song with that tag, or take the tag off it.
    SetTag(usize, Id, bool),
}

impl SongAct {
    fn index(&self) -> usize {
        match *self {
            SongAct::Open(i)
            | SongAct::PlayLocal(i)
            | SongAct::Search(i)
            | SongAct::ToggleLike(i)
            | SongAct::AddToCrate(i, _)
            | SongAct::RemoveFromCrate(i)
            | SongAct::SetTag(i, _, _) => i,
        }
    }
}

/// Whether the song has `query` (already lowercased) in any of its words.
/// Blank matches everything.
pub(crate) fn song_matches(s: &SongPin, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [
        Some(s.artist.as_str()),
        Some(s.title.as_str()),
        s.rel_artist.as_deref(),
        s.rel_title.as_deref(),
        s.rel_label.as_deref(),
        s.rel_catno.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|f| f.to_lowercase().contains(query))
}

/// The caption under the record: `A1 · 2001 · Environ ENV 006`.
pub(crate) fn record_sub(s: &SongPin) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(p) = s.position.as_deref() {
        parts.push(p.to_string());
    }
    if let Some(y) = s.rel_year {
        parts.push(y.to_string());
    }
    match (s.rel_label.as_deref(), s.rel_catno.as_deref()) {
        (Some(l), Some(c)) => parts.push(format!("{l} {c}")),
        (Some(l), None) => parts.push(l.to_string()),
        (None, Some(c)) => parts.push(c.to_string()),
        (None, None) => {}
    }
    parts.join(" · ")
}

/// Pad a cell so its one- or two-line text block sits in the middle of the
/// row rather than against its top.
pub(crate) fn center_two(ui: &mut egui::Ui, two: bool) {
    let body = ui.text_style_height(&egui::TextStyle::Body);
    let block = if two {
        body + 1.0 + ui.fonts(|f| f.row_height(&font::caption()))
    } else {
        body
    };
    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
}

/// A cover thumb, or the blank square with a note where there is none.
pub(crate) fn thumb(ui: &mut egui::Ui, side: f32, tex: Option<&Tex>) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
    match tex {
        Some(h) => {
            egui::Image::new(h)
                .fit_to_exact_size(egui::vec2(side, side))
                .rounding(egui::Rounding::same(4.0))
                .paint_at(ui, rect);
        }
        None => {
            ui.painter().rect_filled(rect, egui::Rounding::same(4.0), egui::Color32::from_gray(34));
            ui.painter().text(
                rect.center(),
                egui::Align2::CENTER_CENTER,
                "♪",
                egui::FontId::proportional(side * 0.45),
                egui::Color32::from_gray(70),
            );
        }
    }
}

impl App {
    /// The table: `rows` are the set's songs, `shown` the indices to draw
    /// (the rest are filtered out), `in_library` the library track that is
    /// each song, if any. Returns what a row asked for.
    pub(crate) fn song_rows(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        set: SongSet,
        rows: &[SongPin],
        shown: &[usize],
        in_library: &[Option<Id>],
    ) -> Option<SongAct> {
        use egui_extras::{Column, TableBuilder};
        for &i in shown {
            if let Some(u) = rows[i].rel_thumb.clone() {
                let _ = self.dig_cover(&u);
            }
        }
        let mut act: Option<SongAct> = None;
        let row_h = 46.0;
        const THUMB: f32 = 36.0;
        // In a crate the ✕ takes the song out; in a tag it takes the tag
        // off the song. Same act, said for what the set is.
        let (remove_note, remove_item) = match set {
            SongSet::CrateSet(id) if self.crate_kind(id) == ordnung_core::model::CrateKind::Tag => {
                ("Take the tag off this song", "Take the tag off")
            }
            _ => ("Take the song out of this crate", "Take out of this crate"),
        };
        let salt = match set {
            SongSet::Liked => egui::Id::new("liked_rows"),
            SongSet::CrateSet(id) => egui::Id::new(("crate_song_rows", id)),
        };
        TableBuilder::new(ui)
            .id_salt(salt)
            .striped(true)
            // Click opens the record; a drag carries the song to a crate.
            .sense(egui::Sense::click_and_drag())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(THUMB + 6.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(150.0).clip(true))
            .body(|body| {
                body.rows(row_h, shown.len(), |mut row| {
                    let i = shown[row.index()];
                    let s = &rows[i];
                    let local = in_library[i];
                    let tex = s.rel_thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());
                    row.col(|ui| thumb(ui, THUMB, tex.as_ref()));
                    // The song: title over artist, and the tags it
                    // carries after the artist (`Artist · dub, dark`).
                    let tags = self.tag_words(&s.song().key());
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            let caption = match (s.artist.is_empty(), tags.is_empty()) {
                                (true, true) => String::new(),
                                (false, true) => s.artist.clone(),
                                (true, false) => tags.clone(),
                                (false, false) => format!("{} · {tags}", s.artist),
                            };
                            center_two(ui, !caption.is_empty());
                            ui.add(egui::Label::new(egui::RichText::new(&s.title).color(color::LABEL)).truncate());
                            if !caption.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(caption).font(font::caption()).color(color::LABEL_3)).truncate());
                            }
                        });
                    });
                    // The record it sits on.
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            let name = s.record_name();
                            let sub = record_sub(s);
                            if name.is_empty() && sub.is_empty() {
                                center_two(ui, false);
                                ui.label(egui::RichText::new("No record").color(color::LABEL_3));
                            } else {
                                center_two(ui, !sub.is_empty());
                                ui.add(egui::Label::new(egui::RichText::new(name).color(color::LABEL)).truncate());
                                if !sub.is_empty() {
                                    ui.add(egui::Label::new(egui::RichText::new(sub).font(font::caption()).color(color::LABEL_3)).truncate());
                                }
                            }
                        });
                    });
                    // The marks at the row's edge: the heart, and in a
                    // crate the ✕ that takes the song out; a green FILE on
                    // the songs a track in the library already is. A song
                    // still to be got carries no mark: the absence says it.
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let side = ui.spacing().interact_size.y;
                            let liked = self.is_liked(&s.artist, &s.title, s.release_id, s.position.as_deref());
                            if crate::ui::button::like_mark(ui, liked, side).clicked() {
                                act = Some(SongAct::ToggleLike(i));
                            }
                            if matches!(set, SongSet::CrateSet(_))
                                && crate::ui::button::glyph(ui, "✕", true)
                                    .on_hover_note(remove_note)
                                    .clicked()
                            {
                                act = Some(SongAct::RemoveFromCrate(i));
                            }
                            if local.is_some() {
                                ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                    ui.add(egui::Label::new(egui::RichText::new("FILE").font(font::caption()).color(color::GREEN).strong()))
                                        .on_hover_note("A track in your library is this song");
                                });
                            }
                        });
                    });
                    let resp = row.response();
                    if resp.hovered() && s.release_id.is_some() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() && s.release_id.is_some() {
                        act = Some(SongAct::Open(i));
                    }
                    if resp.drag_started() {
                        egui::DragAndDrop::set_payload(ctx, DraggedSongs(vec![LikeSpec::from_pin(s)]));
                    }
                    let crates = self.crate_sets.clone();
                    resp.context_menu(|ui| {
                        if s.release_id.is_some() && ui.button("Open record").clicked() {
                            act = Some(SongAct::Open(i));
                            ui.close_menu();
                        }
                        if local.is_some() && ui.button("▶ Play my file").clicked() {
                            act = Some(SongAct::PlayLocal(i));
                            ui.close_menu();
                        }
                        if ui.button("Search Discogs for this song").on_hover_note("Put the song in the search box, Discogs mode").clicked() {
                            act = Some(SongAct::Search(i));
                            ui.close_menu();
                        }
                        ui.separator();
                        let here = match set {
                            SongSet::CrateSet(id) => Some(id),
                            SongSet::Liked => None,
                        };
                        if let Some(cid) = crate::crates::add_to_crate_menu(ui, &crates, here) {
                            act = Some(SongAct::AddToCrate(i, cid));
                        }
                        let tagged = self.song_tag_ids(&s.song().key()).to_vec();
                        if let Some((tag, on)) = crate::crates::tag_menu(ui, &crates, &tagged) {
                            act = Some(SongAct::SetTag(i, tag, on));
                        }
                        let liked = self.is_liked(&s.artist, &s.title, s.release_id, s.position.as_deref());
                        if ui.button(if liked { "Unlike" } else { "♥ Like" }).clicked() {
                            act = Some(SongAct::ToggleLike(i));
                            ui.close_menu();
                        }
                        if here.is_some() && ui.button(remove_item).clicked() {
                            act = Some(SongAct::RemoveFromCrate(i));
                            ui.close_menu();
                        }
                    });
                });
            });
        act
    }

    /// Do what a row asked for. `rows` and `in_library` are what the table
    /// was drawn from.
    pub(crate) fn apply_song_act(
        &mut self,
        ctx: &egui::Context,
        set: SongSet,
        act: SongAct,
        rows: &[SongPin],
        in_library: &[Option<Id>],
    ) {
        let i = act.index();
        let Some(s) = rows.get(i).cloned() else { return };
        match act {
            SongAct::Open(_) => {
                if let Some(r) = s.release_id {
                    self.open_release_sheet(
                        r,
                        s.rel_artist.clone().unwrap_or_default(),
                        s.rel_title.clone().unwrap_or_default(),
                        record_sub(&s),
                        s.rel_thumb.clone(),
                        ctx,
                    );
                    // The sheet lights the song's row the way the pointer
                    // would, so it's found at a glance.
                    if let Some(sheet) = self.vinyl_sheet.as_mut().filter(|sh| sh.release_id == r) {
                        sheet.mark = Some(s.title.clone());
                    }
                }
            }
            SongAct::PlayLocal(_) => {
                if let Some(tid) = in_library.get(i).copied().flatten() {
                    match Catalog::open(&self.db_path).and_then(|c| c.get_track(tid)) {
                        Ok(t) => self.play_track(tid, PathBuf::from(t.source_path)),
                        Err(e) => self.fail(format!("Couldn't find that track: {e}")),
                    }
                }
            }
            SongAct::Search(_) => {
                self.search_query = s.song_label();
                self.set_search_scope(crate::records::SearchScope::Discogs);
                self.search_popup_open = true;
                self.start_record_search();
            }
            SongAct::ToggleLike(_) => self.toggle_like(LikeSpec::from_pin(&s)),
            SongAct::AddToCrate(_, cid) => self.add_songs_to_crate(cid, vec![LikeSpec::from_pin(&s)]),
            SongAct::RemoveFromCrate(_) => {
                if let SongSet::CrateSet(cid) = set {
                    self.remove_crate_song(cid, &s);
                }
            }
            SongAct::SetTag(_, tag, on) => self.set_tag(tag, on, vec![LikeSpec::from_pin(&s)]),
        }
    }
}
