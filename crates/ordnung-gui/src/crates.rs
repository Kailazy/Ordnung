//! Crates: named sets of songs on records, the vinyl side's playlists.
//!
//! A playlist holds library tracks; a crate holds songs as they sit on
//! records ([`SongPin`]s, the same rows the crate of liked songs is made
//! of), so a set put together for a gig says which records to bring as
//! well as which songs are on them. Crates sit under the Vinyl tile in the
//! sidebar, drawn like the playlist tree (`sidebar::draw_crate_rows`), and
//! a song gets in by being dragged there from a record's sheet, the liked
//! songs or a tracklist (a [`DraggedSongs`] drag), or through "Add to
//! crate" in a song's menu ([`add_to_crate_menu`]).
//!
//! The crate view shows the set two ways, switched by a segmented control:
//! **Records**, one row per record with the songs the crate takes from it,
//! which is the list to pull from the shelf; and **Songs**, the songs
//! themselves through the one song table (`song_rows`), like the Liked
//! view. Storage is the `crates` and `crate_songs` catalog tables; see
//! `docs/design/crates.md`.

use super::*;
use crate::liked::LikeSpec;
use crate::song_rows::{center_two, song_matches, thumb, SongSet};
use crate::ui::hover::HoverNoteExt;
use crate::ui::tokens::{color, font, space};
use ordnung_core::model::{CrateSet, SongPin};

/// Which layout the crate view shows.
#[derive(Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum CrateLayout {
    /// One row per record: what to bring.
    #[default]
    Records,
    /// One row per song, like the Liked view.
    Songs,
}

/// A record the crate takes songs from, with those songs.
struct RecordGroup {
    release_id: Option<u64>,
    name: String,
    sub: String,
    thumb: Option<String>,
    /// Indices into the crate's songs.
    songs: Vec<usize>,
}

/// What a record row asked for.
enum RecordAct {
    Open(usize),
    /// Take every song of this record out of the crate.
    Remove(usize),
}

/// The "Add to crate" submenu of a song's context menu: one entry per
/// crate but `here` (the crate the row is already in). Returns the crate
/// picked. Shown even with no crates, saying so, so the way in is found.
pub(crate) fn add_to_crate_menu(
    ui: &mut egui::Ui,
    crates: &[CrateSet],
    here: Option<Id>,
) -> Option<Id> {
    let mut pick = None;
    ui.menu_button("Add to crate", |ui| {
        let mut any = false;
        for c in crates.iter().filter(|c| Some(c.id) != here) {
            any = true;
            if ui.button(&c.name).clicked() {
                pick = Some(c.id);
                ui.close_menu();
            }
        }
        if !any {
            ui.add_enabled(false, egui::Button::new("No other crate yet"))
                .on_disabled_hover_note("Make one with the + beside CRATES in the sidebar");
        }
    });
    pick
}

/// Group a crate's songs by the record they sit on, in the order the
/// records first appear; songs pinned to no record gather at the end.
fn group_by_record(songs: &[SongPin]) -> Vec<RecordGroup> {
    let mut groups: Vec<RecordGroup> = Vec::new();
    let mut loose: Vec<usize> = Vec::new();
    for (i, s) in songs.iter().enumerate() {
        let Some(r) = s.release_id else {
            loose.push(i);
            continue;
        };
        match groups.iter_mut().find(|g| g.release_id == Some(r)) {
            Some(g) => g.songs.push(i),
            None => {
                let mut sub = Vec::new();
                if let Some(y) = s.rel_year {
                    sub.push(y.to_string());
                }
                match (s.rel_label.as_deref(), s.rel_catno.as_deref()) {
                    (Some(l), Some(c)) => sub.push(format!("{l} {c}")),
                    (Some(l), None) => sub.push(l.to_string()),
                    (None, Some(c)) => sub.push(c.to_string()),
                    (None, None) => {}
                }
                groups.push(RecordGroup {
                    release_id: Some(r),
                    name: s.record_name(),
                    sub: sub.join(" · "),
                    thumb: s.rel_thumb.clone(),
                    songs: vec![i],
                });
            }
        }
    }
    if !loose.is_empty() {
        groups.push(RecordGroup {
            release_id: None,
            name: "No record".to_string(),
            sub: "Songs met without a record".to_string(),
            thumb: None,
            songs: loose,
        });
    }
    groups
}

impl App {
    /// Read the crates back from the catalog (part of `reload`). The open
    /// crate's songs are read again the next time they are drawn.
    pub(crate) fn load_crate_sets(&mut self) {
        self.crate_sets = Catalog::open(&self.db_path)
            .and_then(|c| c.list_crate_sets())
            .unwrap_or_default();
        self.crate_songs_for = None;
        if let LibraryView::CrateSet(id) = self.view {
            if !self.crate_sets.iter().any(|c| c.id == id) {
                self.view = LibraryView::Vinyl;
            }
        }
    }

    /// Have the crate's songs loaded.
    fn ensure_crate_songs(&mut self, id: Id) {
        if self.crate_songs_for == Some(id) {
            return;
        }
        self.crate_songs = Catalog::open(&self.db_path)
            .and_then(|c| c.list_crate_songs(id))
            .unwrap_or_default();
        self.crate_songs_for = Some(id);
    }

    /// The crate's name, for status lines.
    fn crate_name(&self, id: Id) -> String {
        self.crate_sets
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| "the crate".to_string())
    }

    /// Put songs in a crate (a drop on its row, or Add to crate in a
    /// song's menu). Songs already in it stay as they were.
    pub(crate) fn add_songs_to_crate(&mut self, id: Id, songs: Vec<LikeSpec>) {
        let name = self.crate_name(id);
        let pins: Vec<SongPin> = songs.into_iter().map(LikeSpec::into_pin).collect();
        match Catalog::open(&self.db_path).and_then(|c| c.add_crate_songs(id, &pins)) {
            Ok(n) => {
                let words = match (n, pins.len()) {
                    (0, 1) => format!("{} is already in {name}", pins[0].song_label()),
                    (0, _) => format!("Every one of those is already in {name}"),
                    (1, 1) => format!("Put {} in {name}", pins[0].song_label()),
                    (n, _) => format!("Put {n} songs in {name}"),
                };
                self.status = words;
                self.load_crate_sets();
            }
            Err(e) => self.fail(format!("Couldn't add to {name}: {e}")),
        }
    }

    /// Take one song out of a crate.
    pub(crate) fn remove_crate_song(&mut self, id: Id, s: &SongPin) {
        let name = self.crate_name(id);
        match Catalog::open(&self.db_path).and_then(|c| {
            c.remove_crate_song(id, &s.artist, &s.title, s.release_id, s.position.as_deref())
        }) {
            Ok(_) => {
                self.status = format!("Took {} out of {name}", s.song_label());
                self.load_crate_sets();
            }
            Err(e) => self.fail(format!("Couldn't take the song out of {name}: {e}")),
        }
    }

    /// The top bar's count for the open crate: records or songs, whichever
    /// the view shows, through the same filter it applies.
    pub(crate) fn crate_count_words(&self) -> String {
        let query = self.filter.trim().to_lowercase();
        let shown = self.crate_songs.iter().filter(|s| song_matches(s, &query));
        match self.crate_layout {
            CrateLayout::Songs => {
                let n = shown.count();
                if n == 1 { "1 song".to_string() } else { format!("{n} songs") }
            }
            CrateLayout::Records => {
                let mut ids: Vec<Option<u64>> = shown.map(|s| s.release_id).collect();
                ids.sort_unstable();
                ids.dedup();
                let n = ids.len();
                if n == 1 { "1 record".to_string() } else { format!("{n} records") }
            }
        }
    }

    /// The crate view: its name and counts over the records to bring, or
    /// the songs.
    pub(crate) fn draw_crate_set(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, id: Id) {
        self.ensure_crate_songs(id);
        self.ensure_library_index();
        let Some(set) = self.crate_sets.iter().find(|c| c.id == id).cloned() else {
            return;
        };
        let songs = self.crate_songs.clone();
        let in_library: Vec<Option<Id>> = songs
            .iter()
            .map(|s| self.local_track(&s.song(), s.local_track_id))
            .collect();
        let query = self.filter.trim().to_lowercase();
        let shown: Vec<usize> = songs
            .iter()
            .enumerate()
            .filter(|(_, s)| song_matches(s, &query))
            .map(|(i, _)| i)
            .collect();
        let to_get = in_library.iter().filter(|l| l.is_none()).count();

        ui.add_space(space::S3);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new(&set.name).font(font::headline())).truncate());
            if !songs.is_empty() {
                let records = match set.records {
                    1 => "1 record".to_string(),
                    n => format!("{n} records"),
                };
                let mut words = match set.songs {
                    1 => format!("1 song on {records}"),
                    n => format!("{n} songs on {records}"),
                };
                if to_get > 0 {
                    words.push_str(&format!(" · {to_get} not in your library"));
                }
                ui.add_space(space::S2);
                ui.label(egui::RichText::new(words).font(font::caption()).color(color::LABEL_3));
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                use crate::ui::button::{segmented, Segment};
                let picked = segmented(
                    ui,
                    Some(match self.crate_layout {
                        CrateLayout::Records => 0,
                        CrateLayout::Songs => 1,
                    }),
                    &[
                        Segment { label: "Records", tip: "The records to bring, with the songs the crate takes from each" },
                        Segment { label: "Songs", tip: "Every song in the crate" },
                    ],
                );
                match picked {
                    Some(0) => self.crate_layout = CrateLayout::Records,
                    Some(1) => self.crate_layout = CrateLayout::Songs,
                    _ => {}
                }
            });
        });
        ui.add_space(space::S2);

        if songs.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("Nothing in this crate yet");
                ui.label("Drag songs here from a record's sheet, the liked songs or a tracklist, or pick Add to crate in a song's menu.");
            });
        } else if shown.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.label(egui::RichText::new("No song in the crate matches the filter.").weak());
            });
        } else {
            match self.crate_layout {
                CrateLayout::Songs => {
                    let set = SongSet::CrateSet(id);
                    if let Some(act) = self.song_rows(ui, ctx, set, &songs, &shown, &in_library) {
                        self.apply_song_act(ctx, set, act, &songs, &in_library);
                    }
                }
                CrateLayout::Records => {
                    let shown_songs: Vec<SongPin> = shown.iter().map(|&i| songs[i].clone()).collect();
                    let shown_files: Vec<bool> = shown.iter().map(|&i| in_library[i].is_some()).collect();
                    let groups = group_by_record(&shown_songs);
                    if let Some(act) = self.crate_records(ui, ctx, id, &groups, &shown_songs, &shown_files) {
                        match act {
                            RecordAct::Open(g) => {
                                let g = &groups[g];
                                if let (Some(r), Some(&first)) = (g.release_id, g.songs.first()) {
                                    self.open_release_sheet(
                                        r,
                                        shown_songs[first].rel_artist.clone().unwrap_or_default(),
                                        shown_songs[first].rel_title.clone().unwrap_or_default(),
                                        g.sub.clone(),
                                        g.thumb.clone(),
                                        ctx,
                                    );
                                }
                            }
                            RecordAct::Remove(g) => {
                                let name = self.crate_name(id);
                                let songs: Vec<SongPin> = groups[g].songs.iter().map(|&i| shown_songs[i].clone()).collect();
                                let res = Catalog::open(&self.db_path).and_then(|c| {
                                    for s in &songs {
                                        c.remove_crate_song(id, &s.artist, &s.title, s.release_id, s.position.as_deref())?;
                                    }
                                    Ok(())
                                });
                                match res {
                                    Ok(()) => {
                                        self.status = format!("Took {} out of {name}", groups[g].name);
                                        self.load_crate_sets();
                                    }
                                    Err(e) => self.fail(format!("Couldn't take the record out of {name}: {e}")),
                                }
                            }
                        }
                    }
                }
            }
        }

        // The view is a drop target too, for a song carried here with the
        // crate already open: an outline while it hovers, in on release.
        if let Some(payload) = egui::DragAndDrop::payload::<DraggedSongs>(ctx) {
            let rect = ui.max_rect();
            let over = ctx.pointer_interact_pos().is_some_and(|p| rect.contains(p));
            if over {
                ui.painter().rect_stroke(
                    rect.shrink(2.0),
                    egui::Rounding::same(6.0),
                    egui::Stroke::new(1.5, color::ACCENT),
                );
                if ctx.input(|i| i.pointer.any_released()) {
                    egui::DragAndDrop::clear_payload(ctx);
                    self.add_songs_to_crate(id, payload.0.clone());
                }
            }
        }
    }

    /// The Records layout: one row per record, the songs the crate takes
    /// from it under its name, and OWNED / WANT / FILES at the edge.
    fn crate_records(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        crate_id: Id,
        groups: &[RecordGroup],
        songs: &[SongPin],
        files: &[bool],
    ) -> Option<RecordAct> {
        use egui_extras::{Column, TableBuilder};
        for g in groups {
            if let Some(u) = g.thumb.clone() {
                let _ = self.dig_cover(&u);
            }
        }
        let mut act: Option<RecordAct> = None;
        let row_h = 58.0;
        const THUMB: f32 = 48.0;
        TableBuilder::new(ui)
            .id_salt(("crate_record_rows", crate_id))
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(THUMB + 6.0))
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::remainder().at_least(180.0).clip(true))
            .column(Column::exact(150.0).clip(true))
            .body(|body| {
                body.rows(row_h, groups.len(), |mut row| {
                    let gi = row.index();
                    let g = &groups[gi];
                    let tex = g.thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());
                    row.col(|ui| thumb(ui, THUMB, tex.as_ref()));
                    // The record: name over its imprint.
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            center_two(ui, !g.sub.is_empty());
                            ui.add(egui::Label::new(egui::RichText::new(&g.name).color(color::LABEL)).truncate());
                            if !g.sub.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(&g.sub).font(font::caption()).color(color::LABEL_3)).truncate());
                            }
                        });
                    });
                    // The songs the crate takes from it: `A1 Miura · B2 Caught Up`.
                    row.col(|ui| {
                        let words: Vec<String> = g
                            .songs
                            .iter()
                            .map(|&i| {
                                let s = &songs[i];
                                match s.position.as_deref().filter(|p| !p.is_empty()) {
                                    Some(p) => format!("{p} {}", s.title),
                                    None if g.release_id.is_none() => s.song_label(),
                                    None => s.title.clone(),
                                }
                            })
                            .collect();
                        let n = g.songs.len();
                        let count = if n == 1 { "1 song".to_string() } else { format!("{n} songs") };
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            center_two(ui, true);
                            ui.add(egui::Label::new(egui::RichText::new(words.join(" · ")).color(color::LABEL_2)).truncate());
                            ui.add(egui::Label::new(egui::RichText::new(count).font(font::caption()).color(color::LABEL_3)).truncate());
                        });
                    });
                    // OWNED / WANT for the shelf, FILES when every song of
                    // the record is a track in the library.
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let badge = |ui: &mut egui::Ui, text: &str, col: egui::Color32, tip: &str| {
                                ui.add(egui::Label::new(egui::RichText::new(text).font(font::caption()).color(col).strong()))
                                    .on_hover_note(tip);
                            };
                            if g.songs.iter().all(|&i| files[i]) {
                                badge(ui, "FILES", color::GREEN, "Every song the crate takes from this record is a track in your library");
                            }
                            if let Some(r) = g.release_id {
                                if self.vinyl_owned.contains(&r) {
                                    badge(ui, "OWNED", color::GREEN, "In your collection");
                                } else if self.vinyl_wanted.contains(&r) {
                                    badge(ui, "WANT", color::ACCENT_HOVER, "On your wantlist");
                                }
                            }
                        });
                    });
                    let resp = row.response();
                    if resp.hovered() && g.release_id.is_some() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() && g.release_id.is_some() {
                        act = Some(RecordAct::Open(gi));
                    }
                    resp.context_menu(|ui| {
                        if g.release_id.is_some() && ui.button("Open record").clicked() {
                            act = Some(RecordAct::Open(gi));
                            ui.close_menu();
                        }
                        if ui.button("Take out of this crate").on_hover_note("Every song the crate takes from this record").clicked() {
                            act = Some(RecordAct::Remove(gi));
                            ui.close_menu();
                        }
                    });
                });
            });
        act
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pin(title: &str, release: Option<u64>, pos: &str) -> SongPin {
        SongPin {
            id: 0,
            artist: "A".into(),
            title: title.into(),
            release_id: release,
            position: Some(pos.to_string()),
            rel_artist: Some("Metro Area".into()),
            rel_title: release.map(|r| format!("Record {r}")),
            rel_label: Some("Environ".into()),
            rel_catno: None,
            rel_year: Some(2001),
            rel_thumb: None,
            local_track_id: None,
            added_at: 0,
        }
    }

    #[test]
    fn records_group_in_first_seen_order_with_loose_songs_last() {
        let songs = [
            pin("Loose", None, ""),
            pin("Miura", Some(42), "A1"),
            pin("Deep Burnt", Some(9), "A"),
            pin("Caught Up", Some(42), "B1"),
        ];
        let groups = group_by_record(&songs);
        assert_eq!(groups.len(), 3);
        assert_eq!(groups[0].release_id, Some(42));
        assert_eq!(groups[0].songs, vec![1, 3]);
        assert_eq!(groups[0].name, "Metro Area – Record 42");
        assert_eq!(groups[0].sub, "2001 · Environ");
        assert_eq!(groups[1].release_id, Some(9));
        assert_eq!(groups[2].release_id, None);
        assert_eq!(groups[2].songs, vec![0]);
    }
}
