//! The crate of liked songs.
//!
//! A song met outside the library (a line of a mix tracklist, a row of a
//! record's sheet, a song on the radio) carries a "+" at its row's edge.
//! Clicking it puts the song in the crate; the mark turns into a heart
//! wherever that song shows, so a record's sheet says at a glance which of
//! its songs are liked. The crate is the Liked view in the sidebar: every
//! liked song with the record it was liked on, marked where a track in the
//! library already is that song, drawn through the one song table
//! (`song_rows`) the crates share. Storage is the `liked_songs` catalog
//! table, keyed by [`song_key`] so a song liked twice, on two records, is
//! one row.

use super::*;
use crate::song_rows::{song_matches, SongSet};
use crate::ui::tokens::{color, font, space};
use ordnung_core::catalog::song_key;
use ordnung_core::model::{SongPin, SongRef};

/// What a song carries in from the row it was met on: the song, and the
/// record it sits on when there was one. What a like, a drag to a crate
/// and Add to crate all hand over.
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
    /// The row to store, stamped now.
    pub(crate) fn into_pin(self) -> SongPin {
        SongPin {
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
            added_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0),
        }
    }

    /// What a library track carries to a like or a tag: the song as its
    /// tags say it, on the record it is matched to (`release_id`, from
    /// the app's track-to-release map), with this very file as the track
    /// that is it. The inspector's heart and the library's Tags menu both
    /// hand this over.
    pub(crate) fn from_track(t: &Track, release_id: Option<u64>) -> LikeSpec {
        let song = t.song();
        LikeSpec {
            artist: song.artist,
            title: song.title,
            release_id,
            position: None,
            rel_artist: t.tags.album_artist.clone().or_else(|| t.tags.artist.clone()),
            rel_title: t.tags.album.clone(),
            rel_label: t.tags.label.clone(),
            rel_catno: t.tags.catalog_number.clone(),
            rel_year: t.tags.year,
            rel_thumb: None,
            local_track_id: Some(t.id),
        }
    }

    /// The spec of a stored row, for carrying it on to another set.
    pub(crate) fn from_pin(s: &SongPin) -> LikeSpec {
        LikeSpec {
            artist: s.artist.clone(),
            title: s.title.clone(),
            release_id: s.release_id,
            position: s.position.clone(),
            rel_artist: s.rel_artist.clone(),
            rel_title: s.rel_title.clone(),
            rel_label: s.rel_label.clone(),
            rel_catno: s.rel_catno.clone(),
            rel_year: s.rel_year,
            rel_thumb: s.rel_thumb.clone(),
            local_track_id: s.local_track_id,
        }
    }
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
            let mut song = spec.into_pin();
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
        self.ensure_library_index();
        let rows = self.liked.clone();
        let in_library: Vec<Option<Id>> = rows
            .iter()
            .map(|s| self.local_track(&s.song(), s.local_track_id))
            .collect();
        let query = self.filter.trim().to_lowercase();
        let to_get_only = self.liked_to_get_only;
        let shown: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(i, s)| song_matches(s, &query) && !(to_get_only && in_library[*i].is_some()))
            .map(|(i, _)| i)
            .collect();
        let have = in_library.iter().filter(|l| l.is_some()).count();
        let to_get = rows.len() - have;

        // The count is the top bar's (see the toolbar in `app`), where every
        // view's count sits; the heading carries the one number the crate
        // is for — how many of its songs are still to be got — and the
        // switch that shows only those.
        ui.add_space(space::S3);
        ui.horizontal(|ui| {
            ui.add(egui::Label::new(egui::RichText::new("Liked songs").font(font::headline())).truncate());
            if !rows.is_empty() {
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

        if rows.is_empty() {
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
        if let Some(act) = self.song_rows(ui, ctx, SongSet::Liked, &rows, &shown, &in_library) {
            self.apply_song_act(ctx, SongSet::Liked, act, &rows, &in_library);
        }
    }
}

/// The key a liked song is held under.
fn liked_key(s: &SongPin) -> String {
    song_key(&s.artist, &s.title, s.release_id, s.position.as_deref())
}

fn spec_label(s: &LikeSpec) -> String {
    if s.artist.is_empty() {
        s.title.clone()
    } else {
        format!("{} - {}", s.artist, s.title)
    }
}
