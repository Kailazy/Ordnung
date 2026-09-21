//! The crate of liked songs.
//!
//! A song met outside the library (a line of a mix tracklist, a row of a
//! record's sheet, a song on the radio) carries a "+" at its row's edge.
//! Clicking it puts the song in the crate; the mark turns into a heart
//! wherever that song shows, so a record's sheet says at a glance which of
//! its songs are liked. The crate is the Liked view in the sidebar: every
//! liked song with the record it was liked on, marked where a track in the
//! library already is that song. Storage is the `liked_songs` catalog table, keyed
//! by [`song_key`] so a song liked twice, on two records, is one row.

use super::*;
use crate::ui::hover::HoverNoteExt;
use crate::ui::tokens::{color, font, space};
use ordnung_core::catalog::song_key;
use ordnung_core::model::{LikedSong, SongRef};

/// What a like carries in from the row it was clicked on: the song, and
/// the record it was met on when there was one.
#[derive(Clone, Default)]
pub(crate) struct LikeSpec {
    pub artist: String,
    pub title: String,
    pub release_id: Option<u64>,
    pub position: Option<String>,
    pub rel_artist: Option<String>,
    pub rel_title: Option<String>,
    pub rel_label: Option<String>,
    pub rel_catno: Option<String>,
    pub rel_year: Option<u16>,
    pub rel_thumb: Option<String>,
    pub local_track_id: Option<Id>,
}

impl LikeSpec {
    fn into_song(self) -> LikedSong {
        LikedSong {
            id: 0,
            artist: self.artist,
            title: self.title,
            release_id: self.release_id,
            position: self.position.filter(|p| !p.is_empty()),
            rel_artist: self.rel_artist.filter(|s| !s.is_empty()),
            rel_title: self.rel_title.filter(|s| !s.is_empty()),
            rel_label: self.rel_label.filter(|s| !s.is_empty()),
            rel_catno: self.rel_catno.filter(|s| !s.is_empty()),
            rel_year: self.rel_year,
            rel_thumb: self.rel_thumb.filter(|s| !s.is_empty()),
            local_track_id: self.local_track_id,
            liked_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }
}

/// What a row of the crate asked for, applied once the table has let go
/// of its borrows. Indices are into `liked`.
enum LikedAct {
    Open(usize),
    PlayLocal(usize),
    Search(usize),
    Unlike(usize),
}

impl App {
    /// Whether this song is in the crate. The record and position only
    /// matter for a title that names nothing ("Untitled"), see [`song_key`].
    pub(crate) fn is_liked(
        &self,
        artist: &str,
        title: &str,
        release_id: Option<u64>,
        position: Option<&str>,
    ) -> bool {
        self.liked_keys.contains(&song_key(artist, title, release_id, position))
    }

    /// Put a song in the crate, or take it out if it's there. Writes the
    /// catalog and keeps the in-memory crate in step, so every mark for
    /// the song flips this frame.
    pub(crate) fn toggle_like(&mut self, spec: LikeSpec) {
        let key = song_key(&spec.artist, &spec.title, spec.release_id, spec.position.as_deref());
        if key.is_empty() {
            return;
        }
        let cat = match Catalog::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                self.fail(format!("Couldn't open the catalog: {e}"));
                return;
            }
        };
        if self.liked_keys.contains(&key) {
            match cat.unlike_song(&spec.artist, &spec.title, spec.release_id, spec.position.as_deref()) {
                Ok(_) => {
                    self.liked_keys.remove(&key);
                    self.liked.retain(|s| liked_key(s) != key);
                    self.status = format!("Took {} out of Liked songs", spec_label(&spec));
                }
                Err(e) => self.fail(format!("Couldn't unlike {}: {e}", spec_label(&spec))),
            }
        } else {
            let label = spec_label(&spec);
            let mut song = spec.into_song();
            match cat.like_song(&song) {
                Ok(id) => {
                    song.id = id;
                    self.liked_keys.insert(key);
                    self.liked.insert(0, song);
                    self.status = format!("Liked {label}");
                }
                Err(e) => self.fail(format!("Couldn't like {label}: {e}")),
            }
        }
    }

    /// Read the crate back from the catalog (part of `reload`).
    pub(crate) fn load_liked(&mut self) {
        self.liked = Catalog::open(&self.db_path)
            .and_then(|c| c.list_liked_songs())
            .unwrap_or_default();
        self.liked_keys = self.liked.iter().map(liked_key).collect();
        self.library_index_dirty = true;
    }

    /// Bring the library index up to date. One pass over the tracks
    /// table, taken the first time a view asks after a reload; every
    /// view that says FILE calls this before it draws.
    pub(crate) fn ensure_library_index(&mut self) {
        if !self.library_index_dirty {
            return;
        }
        self.library_index = Catalog::open(&self.db_path)
            .and_then(|c| c.library_index())
            .unwrap_or_default();
        self.library_index_dirty = false;
    }

    /// The library track that is `song`, if any: the one a row already
    /// knows (`known`) while it's still here, else one found by song.
    /// Call [`Self::ensure_library_index`] first.
    pub(crate) fn local_track(&self, song: &SongRef, known: Option<Id>) -> Option<Id> {
        self.library_index.resolve(song, known)
    }

    /// The Liked view: the crate as a table, every song with the record
    /// it was liked on and whether a file in the library is it.
    pub(crate) fn draw_liked(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        use egui_extras::{Column, TableBuilder};
        self.ensure_library_index();
        let in_library: Vec<Option<Id>> = self
            .liked
            .iter()
            .map(|s| self.local_track(&s.song(), s.local_track_id))
            .collect();
        let query = self.filter.trim().to_lowercase();
        let to_get_only = self.liked_to_get_only;
        let shown: Vec<usize> = self
            .liked
            .iter()
            .enumerate()
            .filter(|(i, s)| liked_matches(s, &query) && !(to_get_only && in_library[*i].is_some()))
            .map(|(i, _)| i)
            .collect();
        let have = in_library.iter().filter(|l| l.is_some()).count();
        let to_get = self.liked.len() - have;

        // The count is the top bar's (see the toolbar in `app`), where every
        // view's count sits; the heading carries the one number the crate
        // is for — how many of its songs are still to be got — and the
        // switch that shows only those.
        ui.add_space(space::S3);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new("Liked songs").font(font::headline())).truncate());
            if !self.liked.is_empty() {
                let words = match (have, to_get) {
                    (_, 0) => "every song is a file in your library".to_string(),
                    (0, n) => format!("{n} still to get"),
                    (h, n) => format!("{h} as files, {n} still to get"),
                };
                ui.add_space(space::S2);
                ui.label(egui::RichText::new(words).font(font::caption()).color(color::LABEL_3));
                if to_get > 0 || self.liked_to_get_only {
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        use crate::ui::button::{segmented, Segment};
                        let picked = segmented(
                            ui,
                            Some(usize::from(self.liked_to_get_only)),
                            &[
                                Segment { label: "All", tip: "Every liked song" },
                                Segment { label: "To get", tip: "Only the songs no file in your library is yet" },
                            ],
                        );
                        if let Some(p) = picked {
                            self.liked_to_get_only = p == 1;
                        }
                    });
                }
            }
        });
        ui.add_space(space::S2);

        if self.liked.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                ui.heading("Nothing liked yet");
                ui.label("Click the + on a song in a record's sheet, a tracklist or the radio to put it here.");
            });
            return;
        }
        if shown.is_empty() {
            ui.add_space(24.0);
            ui.vertical_centered(|ui| {
                let words = if to_get_only && query.is_empty() {
                    "Every liked song is a file in your library."
                } else {
                    "No liked song matches the filter."
                };
                ui.label(egui::RichText::new(words).weak());
            });
            return;
        }
        for &i in &shown {
            if let Some(u) = self.liked[i].rel_thumb.clone() {
                let _ = self.dig_cover(&u);
            }
        }

        let mut act: Option<LikedAct> = None;
        let row_h = 46.0;
        const THUMB: f32 = 36.0;
        TableBuilder::new(ui)
            .id_salt("liked_rows")
            .striped(true)
            .sense(egui::Sense::click())
            .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
            .column(Column::exact(THUMB + 6.0))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::remainder().at_least(160.0).clip(true))
            .column(Column::exact(150.0).clip(true))
            .body(|body| {
                body.rows(row_h, shown.len(), |mut row| {
                    let i = shown[row.index()];
                    let s = self.liked[i].clone();
                    let local = in_library[i];
                    let tex = s.rel_thumb.as_deref().and_then(|u| self.dig_cover(u).cloned());
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
                                ui.painter().rect_filled(rect, egui::Rounding::same(4.0), egui::Color32::from_gray(34));
                                ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, "♪", egui::FontId::proportional(16.0), egui::Color32::from_gray(70));
                            }
                        }
                    });
                    // The song: title over artist.
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            center_two(ui, !s.artist.is_empty());
                            ui.add(egui::Label::new(egui::RichText::new(&s.title).color(color::LABEL)).truncate());
                            if !s.artist.is_empty() {
                                ui.add(egui::Label::new(egui::RichText::new(&s.artist).font(font::caption()).color(color::LABEL_3)).truncate());
                            }
                        });
                    });
                    // The record it was liked on.
                    row.col(|ui| {
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            let name = match (s.rel_artist.as_deref(), s.rel_title.as_deref()) {
                                (Some(a), Some(t)) => format!("{a} – {t}"),
                                (_, Some(t)) => t.to_string(),
                                (Some(a), None) => a.to_string(),
                                (None, None) => String::new(),
                            };
                            let sub = liked_record_sub(&s);
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
                    // The heart at the row's edge, and the song's download
                    // status: FILE when a track in the library is it, NO
                    // FILE when it's still to be got.
                    row.col(|ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let side = ui.spacing().interact_size.y;
                            if crate::ui::button::like_mark(ui, true, side).clicked() {
                                act = Some(LikedAct::Unlike(i));
                            }
                            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                                let (words, ink, note) = match local {
                                    Some(_) => ("FILE", color::GREEN, "A track in your library is this song"),
                                    None => ("NO FILE", color::LABEL_3, "No track in your library is this song yet"),
                                };
                                ui.add(egui::Label::new(egui::RichText::new(words).font(font::caption()).color(ink).strong()))
                                    .on_hover_note(note);
                            });
                        });
                    });
                    let resp = row.response();
                    if resp.hovered() && s.release_id.is_some() {
                        ctx.set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if resp.clicked() && s.release_id.is_some() {
                        act = Some(LikedAct::Open(i));
                    }
                    resp.context_menu(|ui| {
                        if s.release_id.is_some() && ui.button("Open record").clicked() {
                            act = Some(LikedAct::Open(i));
                            ui.close_menu();
                        }
                        if local.is_some() && ui.button("▶ Play my file").clicked() {
                            act = Some(LikedAct::PlayLocal(i));
                            ui.close_menu();
                        }
                        if ui.button("Search Discogs for this song").on_hover_note("Put the song in the search box, Discogs mode").clicked() {
                            act = Some(LikedAct::Search(i));
                            ui.close_menu();
                        }
                        ui.separator();
                        if ui.button("Unlike").clicked() {
                            act = Some(LikedAct::Unlike(i));
                            ui.close_menu();
                        }
                    });
                });
            });

        let Some(act) = act else { return };
        let i = match act {
            LikedAct::Open(i) | LikedAct::PlayLocal(i) | LikedAct::Search(i) | LikedAct::Unlike(i) => i,
        };
        let Some(s) = self.liked.get(i).cloned() else { return };
        match act {
            LikedAct::Open(_) => {
                if let Some(r) = s.release_id {
                    let sub = liked_record_sub(&s);
                    self.open_release_sheet(
                        r,
                        s.rel_artist.clone().unwrap_or_default(),
                        s.rel_title.clone().unwrap_or_default(),
                        sub,
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
            LikedAct::PlayLocal(_) => {
                if let Some(tid) = in_library.get(i).copied().flatten() {
                    match Catalog::open(&self.db_path).and_then(|c| c.get_track(tid)) {
                        Ok(t) => self.play_track(tid, PathBuf::from(t.source_path)),
                        Err(e) => self.fail(format!("Couldn't find that track: {e}")),
                    }
                }
            }
            LikedAct::Search(_) => {
                self.search_query = s.song_label();
                self.set_search_scope(crate::records::SearchScope::Discogs);
                self.search_popup_open = true;
                self.start_record_search();
            }
            LikedAct::Unlike(_) => {
                self.toggle_like(LikeSpec {
                    artist: s.artist.clone(),
                    title: s.title.clone(),
                    release_id: s.release_id,
                    position: s.position.clone(),
                    ..Default::default()
                });
            }
        }
    }
}

/// The key a liked song is held under.
fn liked_key(s: &LikedSong) -> String {
    song_key(&s.artist, &s.title, s.release_id, s.position.as_deref())
}

fn spec_label(s: &LikeSpec) -> String {
    if s.artist.is_empty() {
        s.title.clone()
    } else {
        format!("{} - {}", s.artist, s.title)
    }
}

/// Whether the liked song has `query` (already lowercased) in any of its
/// words. Blank matches everything.
pub(crate) fn liked_matches(s: &LikedSong, query: &str) -> bool {
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
fn liked_record_sub(s: &LikedSong) -> String {
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
fn center_two(ui: &mut egui::Ui, two: bool) {
    let body = ui.text_style_height(&egui::TextStyle::Body);
    let block = if two {
        body + 1.0 + ui.fonts(|f| f.row_height(&font::caption()))
    } else {
        body
    };
    ui.add_space(((ui.available_height() - block) / 2.0).max(0.0));
}
