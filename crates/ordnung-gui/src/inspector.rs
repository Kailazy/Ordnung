//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// Commit the inspector's edited Core fields to the catalog, and — when
    /// `write_file` is given — also write them into the source file via the core
    /// `tag` engine (the GUI counterpart to the CLI's `tag` vs `tag --write`).
    /// Other tag fields are preserved: we fetch the full row, mutate only the
    /// editable fields, and write the whole thing back. `update_tags` marks
    /// the row `user_edited` so a later rescan won't clobber these edits.
    pub(crate) fn save_tags(&mut self, id: Id, write_file: Option<PathBuf>) {
        // Year: empty clears it; otherwise it must parse as an integer.
        let year_str = self.tag_edit.year.trim();
        let year = if year_str.is_empty() {
            None
        } else {
            match year_str.parse::<u16>() {
                Ok(y) => Some(y),
                Err(_) => {
                    self.status = "Year must be a whole number like 2024 (or left blank).".into();
                    return;
                }
            }
        };

        let catalog = match Catalog::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Save failed: {e}");
                return;
            }
        };
        let mut track = match catalog.get_track(id) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Save failed: {e}");
                return;
            }
        };
        track.tags.title = non_empty(&self.tag_edit.title);
        track.tags.artist = non_empty(&self.tag_edit.artist);
        track.tags.album_artist = non_empty(&self.tag_edit.album_artist);
        track.tags.album = non_empty(&self.tag_edit.album);
        track.tags.genre = non_empty(&self.tag_edit.genre);
        track.tags.label = non_empty(&self.tag_edit.label);
        track.tags.year = year;
        track.tags.comment = non_empty(&self.tag_edit.comment);

        if let Err(e) = catalog.update_tags(id, &track.tags) {
            self.status = format!("Save failed: {e}");
            return;
        }

        // With auto-write on, a plain "Save" is also a write: the setting is the
        // user's standing instruction to keep source files in sync, so the edit
        // never parks behind the bulk-write button. Explicit "Write to source
        // file" clicks still win when the setting is off.
        let write_file = write_file.or_else(|| {
            self.config
                .auto_write_tags
                .then(|| PathBuf::from(&track.source_path))
        });

        match write_file {
            Some(path) => match tag::write_to_file(&path, &track.tags, None) {
                Ok(()) => {
                    // Catalog and file now agree — drop the "needs writing" flag so
                    // this track no longer shows up as edited / pending a write.
                    let _ = catalog.clear_user_edited(id);
                    self.status = format!("Saved to catalog and wrote tags into {}", path.display())
                }
                Err(e) => {
                    self.status =
                        format!("Saved to catalog, but writing the source file failed: {e}")
                }
            },
            None => self.status = "Saved to catalog (rescan-safe).".into(),
        }

        // Reload the table (artist/title/etc. columns may have changed) and
        // re-read the selection, which resets the edit buffers + dirty state.
        self.reload();
        self.refresh_selected();
    }

    /// The inspector's pull tab: a handle at the top-right of the content
    /// area, sitting at the drawer's inner boundary. `inset` is how far in from
    /// the right edge to sit — the drawer's animated width — so the tab rides
    /// out with the panel and stays the one control that toggles it.
    ///
    /// The tab also doubles as a nameplate for the last track the player
    /// loaded: the title runs down it in rotated text under the chevron, so the
    /// handle carries information rather than only a toggle.
    pub(crate) fn draw_inspector_tab(&mut self, ctx: &egui::Context, inset: f32) {
        // Slim: the tab is a grip, not a button. The chevron spans its full
        // width so the narrower it gets the more it reads as an arrow.
        const W: f32 = 18.0;
        /// Height of the chevron block at the top of the tab.
        const HEAD: f32 = 40.0;
        /// Extra height given over to the rotated last-played title.
        const LABEL_H: f32 = 130.0;

        // The title that rides down the tab: whatever the player last loaded.
        // `now_playing` outlives playback (only an explicit close clears it), so
        // this stays the *last* played track rather than blanking on pause.
        let label = self
            .now_playing
            .as_ref()
            .map(|np| {
                if np.artist.is_empty() {
                    np.title.clone()
                } else {
                    format!("{} — {}", np.title, np.artist)
                }
            })
            .unwrap_or_default();
        let h = if label.is_empty() {
            HEAD
        } else {
            HEAD + LABEL_H
        };

        // Horizontal: measured from the window's own right edge, because that's
        // what `inset` (the drawer's width) is measured against. Using
        // `available_rect` here would double-count — by this point it has
        // already had the open drawer subtracted from it, which left the tab
        // stranded mid-table.
        //
        // Vertical: flush with `available_rect`'s top (what the toolbar, player
        // and status bars left over) with no gap, so the tab meets the top
        // border cleanly instead of leaving a notch of table above it.
        let pos = egui::pos2(
            ctx.screen_rect().right() - inset - W,
            ctx.available_rect().top(),
        );
        // Middle order, and clipped to the content area: a Foreground area
        // paints over the side and top panels, which put the tab on top of the
        // toolbar buttons whenever the toolbar wrapped to a taller row. Both
        // together mean the tab can only ever draw inside the region the
        // toolbar, player and status bars left behind.
        egui::Area::new(egui::Id::new("inspector_tab"))
            .order(egui::Order::Middle)
            .constrain_to(ctx.available_rect())
            .fixed_pos(pos)
            .show(ctx, |ui| {
                ui.set_clip_rect(ctx.available_rect());
                let (rect, resp) = ui.allocate_exact_size(egui::vec2(W, h), egui::Sense::click());
                let open = self.inspector_open;
                let hovered = resp.hovered();
                if hovered {
                    ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                }
                // Rounded only on the bottom-left: the top edge butts against
                // the content area's top border and the right edge against the
                // drawer, so the one free corner is the only one to soften.
                let rounding = egui::Rounding {
                    nw: 0.0,
                    sw: W / 2.0,
                    ne: 0.0,
                    se: 0.0,
                };
                let bg = if hovered {
                    egui::Color32::from_gray(82)
                } else {
                    crate::ui::tokens::color::DRAWER
                };
                ui.painter().rect_filled(rect, rounding, bg);

                let fg = if hovered {
                    egui::Color32::from_gray(235)
                } else {
                    egui::Color32::from_gray(170)
                };
                // The chevron points where the panel is headed: outward (▶) to
                // push it away, inward (◀) to pull it back open. Painted as a
                // triangle rather than a glyph so it can be sized to the tab —
                // it spans the full width and most of the head's height, which
                // a font-rendered arrow at this size can't do.
                let head = egui::Rect::from_min_size(rect.min, egui::vec2(W, HEAD));
                let pad = 4.0;
                let (x_tip, x_base) = if open {
                    (head.right() - pad, head.left() + pad)
                } else {
                    (head.left() + pad, head.right() - pad)
                };
                let (top, bottom) = (head.top() + pad, head.bottom() - pad);
                ui.painter().add(egui::Shape::convex_polygon(
                    vec![
                        egui::pos2(x_base, top),
                        egui::pos2(x_tip, head.center().y),
                        egui::pos2(x_base, bottom),
                    ],
                    fg,
                    egui::Stroke::NONE,
                ));

                // Last-played title, rotated a quarter turn so it reads top-to-
                // bottom down the tab. Laid out truncated to the label run's
                // height (its length once rotated) so a long title ends in an
                // ellipsis instead of running off the bottom.
                if !label.is_empty() {
                    let galley = ui.painter().layout_job({
                        let mut job = egui::text::LayoutJob::simple_singleline(
                            label,
                            egui::FontId::proportional(11.0),
                            fg,
                        );
                        // One row, capped at the label run's length: anything
                        // longer ends in egui's default `…` rather than running
                        // off the bottom of the tab.
                        job.wrap.max_width = LABEL_H - 12.0;
                        job.wrap.max_rows = 1;
                        job.wrap.break_anywhere = true;
                        job
                    });
                    // Rotation is about the galley's own min corner, so place
                    // that corner at the top-right of the label run: +90° then
                    // sweeps the text down the tab, centred across its width.
                    let x = rect.center().x + galley.size().y / 2.0;
                    let y = head.bottom() + 6.0;
                    ui.painter().add(
                        egui::epaint::TextShape::new(egui::pos2(x, y), galley, fg)
                            .with_angle(std::f32::consts::FRAC_PI_2),
                    );
                }

                let resp = resp.on_hover_note(if open {
                    "Hide the track inspector"
                } else {
                    "Show the track inspector"
                });
                if resp.clicked() {
                    self.inspector_open = !self.inspector_open;
                }
            });
    }

    /// The right-hand inspector — every standardized metadata field we have
    /// for the selected track. Empty fields are hidden so visible content is
    /// only what the file actually carried.
    /// Returns `Some((id, source_path))` when the user clicked "embed cover into
    /// file" this frame, so the caller can run the writeback after this `&mut
    /// self` borrow ends.
    pub(crate) fn draw_inspector(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
    ) -> Option<InspectorAction> {
        // Small all-caps caption rather than a big heading: the track's own
        // name below is what should read as the panel's title.
        ui.add_space(6.0);
        ui.add(egui::Label::new(egui::RichText::new("T R A C K").small().weak()).truncate());
        ui.add_space(2.0);

        // Copied out before borrowing `selected_track` so the button below can
        // act on them without holding an immutable borrow of `self`.
        let busy = self.is_busy();
        let has_ext_art = self.selected_has_external_art;

        // Resolve the full-resolution cover up front, while we still hold
        // `&mut self` (this may kick off a background decode). Pulling the
        // id/path out without binding `selected_track` keeps the later immutable
        // borrow conflict-free. `cover_loading` drives a spinner while the worker
        // decodes (the texture isn't ready yet but a load is in flight).
        let (cover_tex, cover_loading) = match self
            .selected_track
            .as_ref()
            .map(|t| (t.id, t.source_path.clone()))
        {
            Some((id, path)) => {
                let tex = self.cover_full_texture(ctx, id, &path);
                let loading = tex.is_none() && self.cover_inflight.contains(&id);
                (tex, loading)
            }
            None => (None, false),
        };

        let Some(t) = &self.selected_track else {
            ui.add(
                egui::Label::new(
                    egui::RichText::new("Click a track in the table to inspect.").weak(),
                )
                .wrap(),
            );
            return None;
        };
        let id = t.id;
        let source_path = PathBuf::from(t.source_path.clone());

        // Title on its own line at heading weight, artist beneath it — the
        // "A — B" run-together line read as one undifferentiated string.
        ui.add(
            egui::Label::new(
                egui::RichText::new(t.tags.title.as_deref().unwrap_or("Untitled"))
                    .heading()
                    .size(17.0),
            )
            .truncate(),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(t.tags.artist.as_deref().unwrap_or("Unknown"))
                    .size(13.0)
                    .weak(),
            )
            .truncate(),
        );
        // The full path is long and rarely needed at a glance: show the file
        // name, with the whole path on hover.
        let path_str = t.source_path.clone();
        let file_name = std::path::Path::new(&path_str)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path_str.clone());
        ui.add_space(1.0);
        ui.add(egui::Label::new(egui::RichText::new(file_name).small().weak()).truncate())
            .on_hover_text(&path_str);

        // Cover art preview. Decoded off-thread (see `cover_full_texture`): once
        // ready we show the high-quality image (embedded art wins, fetched
        // Discogs art is the fallback), scaled to a square that fits the panel
        // width. While the worker decodes, show a spinner so a large source
        // image never makes the panel look empty or frozen.
        if let Some(tex) = &cover_tex {
            ui.add_space(10.0);
            let side = ui.available_width().min(240.0);
            ui.vertical_centered(|ui| {
                ui.add(
                    egui::Image::new(tex)
                        .maintain_aspect_ratio(true)
                        .fit_to_exact_size(egui::vec2(side, side))
                        .rounding(6.0),
                );
            });
            ui.add_space(4.0);
        } else if cover_loading {
            ui.add_space(10.0);
            let side = ui.available_width().min(240.0);
            ui.vertical_centered(|ui| {
                // Reserve the same square the artwork will occupy so the panel
                // below doesn't jump once the decode lands.
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(side, side), egui::Sense::hover());
                ui.painter()
                    .rect_filled(rect, 6.0, ui.visuals().extreme_bg_color);
                ui.put(rect, egui::Spinner::new());
            });
            ui.add_space(4.0);
        }

        // The user's requested action this frame, if any. Acted on by the caller
        // after this method's borrow of `self` ends.
        let mut action: Option<InspectorAction> = None;

        // Writeback action: imprint the fetched cover into the source file.
        // Mirrors the CLI's `tag --write --art` — explicit and source-mutating,
        // so it only appears when fetched artwork actually exists.
        if has_ext_art {
            ui.add_space(6.0);
            ui.add_enabled_ui(!busy, |ui| {
                if ui
                    .add_sized(
                        [ui.available_width(), 24.0],
                        egui::Button::new("⬇ Embed fetched cover into file"),
                    )
                    .on_hover_note("Write the fetched cover into the source file's tags")
                    .clicked()
                {
                    action = Some(InspectorAction::EmbedCover(id, source_path.clone()));
                }
            });
        }
        ui.add_space(8.0);

        // --- Editable Core tags ------------------------------------------
        // The fields a user most often fixes or fills (e.g. from Discogs), plus
        // free-form Notes (the comment tag).
        // Edits live in `self.tag_edit`; "Save" commits them to the catalog,
        // "Write to source file" also writes them into the original file. Both
        // are disabled until something actually changes.
        let dirty = self.tag_edit != self.tag_edit_saved;
        // One scroll area over the edit form *and* the read-only sections: with
        // the form outside it, a tall inspector clipped the form instead of
        // scrolling the column as a whole.
        egui::ScrollArea::vertical().show(ui, |ui| {
            // Edit mode is off by default: the same fields render as plain
            // rows, so the panel reads as a summary rather than eight text
            // boxes. `editing` is copied out because the closures below borrow
            // `self.tag_edit` mutably.
            let editing = self.tags_editing;
            let mut toggle_editing = false;
            inspector_section_with_action(
                ui,
                "Tags",
                |ui| {
                    // Leaving edit mode with unsaved changes would silently drop
                    // them, so the toggle says so rather than looking like a
                    // plain view switch.
                    let label = if editing { "Done" } else { "✏ Edit" };
                    let btn = ui.small_button(label);
                    let btn = if editing && dirty {
                        btn.on_hover_note("Unsaved edits stay in the form until you save")
                    } else {
                        btn.on_hover_note("Edit the catalog tags for this track")
                    };
                    if btn.clicked() {
                        toggle_editing = true;
                    }
                },
                |ui| {
                    if editing {
                        egui::Grid::new("tag-edit-grid")
                            .num_columns(2)
                            .spacing(egui::vec2(8.0, 4.0))
                            .show(ui, |ui| {
                                edit_row(ui, "Title", &mut self.tag_edit.title);
                                edit_row(ui, "Artist", &mut self.tag_edit.artist);
                                edit_row(ui, "Album artist", &mut self.tag_edit.album_artist);
                                edit_row(ui, "Album", &mut self.tag_edit.album);
                                edit_row(ui, "Genre", &mut self.tag_edit.genre);
                                edit_row(ui, "Label", &mut self.tag_edit.label);
                                edit_row(ui, "Year", &mut self.tag_edit.year);
                                edit_row_multiline(ui, "Notes", &mut self.tag_edit.comment);
                            });

                        ui.add_space(8.0);
                        ui.horizontal(|ui| {
                            // With auto-write on, Save already writes the source
                            // file, so the second button would be the same action
                            // under a different name — drop it and let Save's
                            // tooltip say what it now does.
                            let auto = self.config.auto_write_tags;
                            ui.add_enabled_ui(dirty && !busy, |ui| {
                                if ui
                                    .add(egui::Button::new("Save").min_size(egui::vec2(72.0, 24.0)))
                                    .on_hover_note(if auto {
                                        "Save to the catalog and write the tags into the source file"
                                    } else {
                                        "Save to the catalog only. The source file is untouched."
                                    })
                                    .clicked()
                                {
                                    action = Some(InspectorAction::SaveToCatalog(id));
                                }
                                if !auto
                                    && ui
                                        .add(
                                            egui::Button::new("⬇ Write to source file")
                                                .min_size(egui::vec2(0.0, 24.0)),
                                        )
                                        .on_hover_note(
                                            "Save to the catalog and write the tags into the source file",
                                        )
                                        .clicked()
                                {
                                    action =
                                        Some(InspectorAction::WriteToFile(id, source_path.clone()));
                                }
                            });
                            if dirty {
                                ui.label(
                                    egui::RichText::new("● unsaved")
                                        .small()
                                        .color(egui::Color32::from_rgb(220, 190, 90)),
                                );
                            }
                        });
                    } else {
                        // Read-only view of the same buffers, so unsaved edits
                        // stay visible after toggling out of edit mode. Empty
                        // fields are dimmed placeholders rather than hidden:
                        // this block is also the "what's missing" checklist.
                        tag_view_row(ui, "Title", &self.tag_edit.title);
                        tag_view_row(ui, "Artist", &self.tag_edit.artist);
                        tag_view_row(ui, "Album artist", &self.tag_edit.album_artist);
                        tag_view_row(ui, "Album", &self.tag_edit.album);
                        tag_view_row(ui, "Genre", &self.tag_edit.genre);
                        tag_view_row(ui, "Label", &self.tag_edit.label);
                        tag_view_row(ui, "Year", &self.tag_edit.year);
                        tag_view_row(ui, "Notes", &self.tag_edit.comment);
                        if dirty {
                            ui.add_space(4.0);
                            ui.label(
                                egui::RichText::new("● unsaved edits — reopen Edit to save")
                                    .small()
                                    .color(egui::Color32::from_rgb(220, 190, 90)),
                            );
                        }
                    }
                },
            );
            if toggle_editing {
                self.tags_editing = !self.tags_editing;
            }

            // --- Audio (technical) ---------------------------------------
            inspector_section(ui, "Audio", |ui| {
                if let Some(p) = &t.properties {
                    inspector_row(ui, "Format", format_label(t.format));
                    inspector_row(ui, "Sample rate", &format!("{} Hz", p.sample_rate_hz));
                    inspector_row(ui, "Channels", &p.channels.to_string());
                    inspector_row(ui, "Duration", &fmt_duration(p.duration_ms));
                    if let Some(b) = p.bitrate_kbps {
                        inspector_row(ui, "Bitrate", &format!("{b} kbps"));
                    }
                    if let Some(d) = p.bit_depth {
                        inspector_row(ui, "Bit depth", &format!("{d}-bit"));
                    }
                }
            });

            // --- Spectral quality (transcode check) ----------------------
            // Container bitrate can't catch a lossy source upsampled into a
            // lossless file (AIFF/WAV always read 1411). The low-pass cutoff can:
            // lossy encoders leave a brick wall in the spectrum. Shown only once a
            // track has been analyzed by v6+ (older cached analyses have no cutoff).
            if let Some(a) = &t.analysis {
                if a.analyzer_version >= 6 {
                    inspector_section(ui, "Spectral quality", |ui| {
                        let verdict = a.transcode_verdict();
                        let (label, color) = match verdict {
                            // Clean and Inconclusive both mean "no transcode
                            // signature" — band-limiting is a mastering choice, not
                            // a flag — so they read identically here.
                            TranscodeVerdict::Clean | TranscodeVerdict::Inconclusive => (
                                "No transcode signature",
                                egui::Color32::from_rgb(120, 200, 130),
                            ),
                            TranscodeVerdict::Suspect => (
                                "Cutoff ~20 kHz — possible 320k transcode",
                                egui::Color32::from_rgb(220, 190, 90),
                            ),
                            TranscodeVerdict::LikelyLossy => (
                                "Brick wall — likely lossy transcode",
                                egui::Color32::from_rgb(225, 110, 100),
                            ),
                        };
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new("Verdict:").weak().small());
                            ui.label(egui::RichText::new(label).color(color).strong());
                        });
                        if let Some(hz) = a.lowpass_hz {
                            inspector_row(
                                ui,
                                "Low-pass cutoff",
                                &format!("{:.1} kHz", hz / 1000.0),
                            );
                        }
                        if matches!(
                            verdict,
                            TranscodeVerdict::Suspect | TranscodeVerdict::LikelyLossy
                        ) {
                            if let Some(est) = a.estimated_source_kbps() {
                                inspector_row(ui, "Estimated source", est);
                            }
                        }
                    });
                }
            }

            // --- Dates & numbering ---------------------------------------
            // Title/Artist/Album/Genre/Label/Year live in the editable "Edit
            // tags" section above; the rest of the descriptive core stays here
            // read-only.
            let g = &t.tags;
            inspector_section(ui, "Dates & numbering", |ui| {
                opt_row(ui, "Recording date", &g.recording_date);
                opt_row(ui, "Release date", &g.release_date);
                opt_row(ui, "Original release", &g.original_release_date);
                if let (Some(n), Some(t)) = (g.track_number, g.track_total) {
                    inspector_row(ui, "Track #", &format!("{n} of {t}"));
                } else if let Some(n) = g.track_number {
                    inspector_row(ui, "Track #", &n.to_string());
                }
                if let (Some(n), Some(t)) = (g.disc_number, g.disc_total) {
                    inspector_row(ui, "Disc #", &format!("{n} of {t}"));
                } else if let Some(n) = g.disc_number {
                    inspector_row(ui, "Disc #", &n.to_string());
                }
                if let Some(c) = g.compilation {
                    inspector_row(ui, "Compilation", if c { "yes" } else { "no" });
                }
                if g.has_cover {
                    inspector_row(ui, "Cover art", "embedded");
                }
            });

            // --- Credits -------------------------------------------------
            if has_any_credit(g) {
                inspector_section(ui, "Credits", |ui| {
                    opt_row(ui, "Composer", &g.composer);
                    opt_row(ui, "Remixer", &g.remixer);
                    opt_row(ui, "Producer", &g.producer);
                    opt_row(ui, "Conductor", &g.conductor);
                    opt_row(ui, "Lyricist", &g.lyricist);
                    opt_row(ui, "Arranger", &g.arranger);
                    opt_row(ui, "Performer", &g.performer);
                    opt_row(ui, "Mix DJ", &g.mix_dj);
                    opt_row(ui, "Writer", &g.writer);
                });
            }

            // --- DJ / mixing --------------------------------------------
            if has_any_dj(g) {
                inspector_section(ui, "DJ / mixing", |ui| {
                    if let Some(b) = g.bpm_tag {
                        inspector_row(ui, "BPM (file tag)", &format!("{b:.1}"));
                    }
                    opt_row(ui, "Initial key (file tag)", &g.initial_key_tag);
                    opt_row(ui, "Mood", &g.mood);
                    opt_row(ui, "Grouping", &g.grouping);
                });
            }

            // --- Release identifiers ------------------------------------
            if has_any_release(g) {
                inspector_section(ui, "Release identifiers", |ui| {
                    opt_row(ui, "ISRC", &g.isrc);
                    opt_row(ui, "Catalog #", &g.catalog_number);
                    opt_row(ui, "Barcode", &g.barcode);
                    opt_row(ui, "Publisher", &g.publisher);
                    opt_row(ui, "Copyright", &g.copyright);
                    opt_row(ui, "Release country", &g.release_country);
                });
            }

            // --- MusicBrainz / AcoustID ---------------------------------
            if has_any_mb(g) {
                inspector_section(ui, "MusicBrainz / AcoustID", |ui| {
                    opt_row(ui, "Recording ID", &g.musicbrainz_recording_id);
                    opt_row(ui, "Track ID", &g.musicbrainz_track_id);
                    opt_row(ui, "Release ID", &g.musicbrainz_release_id);
                    opt_row(ui, "Release group", &g.musicbrainz_release_group_id);
                    opt_row(ui, "Artist ID", &g.musicbrainz_artist_id);
                    opt_row(ui, "Release artist", &g.musicbrainz_release_artist_id);
                    opt_row(ui, "Work ID", &g.musicbrainz_work_id);
                    opt_row(ui, "Release type", &g.musicbrainz_release_type);
                    opt_row(ui, "AcoustID", &g.acoust_id);
                });
            }

            // --- ReplayGain ---------------------------------------------
            if g.replay_gain_track_gain.is_some()
                || g.replay_gain_album_gain.is_some()
                || g.replay_gain_track_peak.is_some()
                || g.replay_gain_album_peak.is_some()
            {
                inspector_section(ui, "ReplayGain", |ui| {
                    if let Some(v) = g.replay_gain_track_gain {
                        inspector_row(ui, "Track gain", &format!("{v:+.2} dB"));
                    }
                    if let Some(v) = g.replay_gain_track_peak {
                        inspector_row(ui, "Track peak", &format!("{v:.6}"));
                    }
                    if let Some(v) = g.replay_gain_album_gain {
                        inspector_row(ui, "Album gain", &format!("{v:+.2} dB"));
                    }
                    if let Some(v) = g.replay_gain_album_peak {
                        inspector_row(ui, "Album peak", &format!("{v:.6}"));
                    }
                });
            }

            // --- Encoder / origin ---------------------------------------
            if has_any_encoder(g) {
                inspector_section(ui, "Encoder / origin", |ui| {
                    opt_row(ui, "Encoded by", &g.encoded_by);
                    opt_row(ui, "Encoder software", &g.encoder_software);
                    opt_row(ui, "Encoder settings", &g.encoder_settings);
                    opt_row(ui, "Original artist", &g.original_artist);
                    opt_row(ui, "Original album", &g.original_album);
                });
            }

            // --- Content / descriptive ---------------------------------
            if has_any_content(g) {
                inspector_section(ui, "Content", |ui| {
                    opt_row(ui, "Subtitle", &g.subtitle);
                    opt_row(ui, "Description", &g.description);
                    opt_row(ui, "Language", &g.language);
                    opt_row(ui, "Script", &g.script);
                    opt_row(ui, "Work", &g.work);
                    opt_row(ui, "Movement", &g.movement);
                    if let (Some(n), Some(t)) = (g.movement_number, g.movement_total) {
                        inspector_row(ui, "Movement #", &format!("{n} of {t}"));
                    }
                    // Comment is shown/edited as "Notes" in the Edit tags section
                    // above, so it's intentionally not repeated here.
                    if let Some(lyr) = &g.lyrics {
                        if !lyr.is_empty() {
                            ui.label(egui::RichText::new("Lyrics").weak());
                            ui.label(lyr);
                        }
                    }
                });
            }
        });

        action
    }

    /// Imprint the fetched (external) full-resolution cover into the track's
    /// source file via the core `tag` engine — the GUI counterpart to the CLI's
    /// `tag --write --art`. Runs inline (single quick file write) and reports
    /// through the status line.
    pub(crate) fn embed_cover_into_file(&mut self, id: Id, path: PathBuf) {
        let catalog = match Catalog::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Embed failed: {e}");
                return;
            }
        };
        let art = match catalog.get_external_artwork_full(id) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                self.status = "No fetched artwork to embed for this track.".into();
                return;
            }
            Err(e) => {
                self.status = format!("Embed failed: {e}");
                return;
            }
        };
        let track = match catalog.get_track(id) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Embed failed: {e}");
                return;
            }
        };
        match tag::write_to_file(&path, &track.tags, Some(&art)) {
            Ok(()) => {
                self.status = format!("Embedded cover art into {}", path.display());
                // The file now carries the cover, but the catalog still holds the
                // scan-time cover_thumb/has_cover — so without a refresh the table
                // row "looks unchanged" even though the write succeeded. Re-read
                // the file (cheap: tags + properties, no DSP) and upsert so
                // cover_thumb reflects the embedded art, mirroring a rescan.
                //
                // The write above succeeded, so the cover IS now in the file —
                // that's the authoritative signal. The fetched external artwork
                // is therefore redundant: drop it so the "Embed fetched cover"
                // button (inspector) — derived from the external full-res row —
                // disappears for this track.
                //
                // The rescan is best-effort *thumbnail refresh* only: it updates
                // cover_thumb/has_cover from the file. We deliberately do NOT gate
                // the clear on it — an earlier version did, which left the button
                // stuck forever whenever the read-back hiccupped (scan error, or
                // a reader that couldn't see the just-written picture) even though
                // the write had succeeded. If we can't confirm the read-back, say
                // so rather than silently keeping the affordance around.
                let verified = match scan::scan_file(&path) {
                    Ok(scanned) => {
                        let ok = scanned.tags.has_cover;
                        let _ = catalog.upsert_scanned(&scanned);
                        ok
                    }
                    Err(_) => false,
                };
                if let Err(e) = catalog.clear_external_artwork(id) {
                    self.status = format!("Embedded cover, but failed to clear fetched art: {e}");
                } else if !verified {
                    self.status = format!(
                        "Embedded cover into {} (read-back unverified)",
                        path.display()
                    );
                }
                // The file now carries the catalog's tags + cover, so this track
                // is fully synced — drop the user_edited flag (set when the art
                // was fetched) so it leaves the bulk write-edits count.
                let _ = catalog.clear_user_edited(id);
                // Drop cached textures BEFORE reload (reload retains caches for
                // still-live ids) so the next render re-decodes the new cover.
                self.cover_cache.remove(&id);
                self.cover_full_cache.remove(&id);
                self.cover_inflight.remove(&id);
                self.reload();
                // reload() rebuilds the table rows but not the inspector's cached
                // external-art flag — refresh it so the "Embed fetched cover"
                // button also clears for the open track.
                self.refresh_selected();
            }
            Err(e) => self.status = format!("Embed failed: {e}"),
        }
    }
}

/// Returns true if any "credit"-type field is populated.
pub(crate) fn has_any_credit(g: &ordnung_core::Tags) -> bool {
    [
        &g.composer,
        &g.remixer,
        &g.producer,
        &g.conductor,
        &g.lyricist,
        &g.arranger,
        &g.performer,
        &g.mix_dj,
        &g.writer,
    ]
    .iter()
    .any(|v| has_value(v))
}

pub(crate) fn has_any_dj(g: &ordnung_core::Tags) -> bool {
    g.bpm_tag.is_some()
        || has_value(&g.initial_key_tag)
        || has_value(&g.mood)
        || has_value(&g.grouping)
}

pub(crate) fn has_any_release(g: &ordnung_core::Tags) -> bool {
    [
        &g.isrc,
        &g.catalog_number,
        &g.barcode,
        &g.publisher,
        &g.copyright,
        &g.release_country,
    ]
    .iter()
    .any(|v| has_value(v))
}

pub(crate) fn has_any_mb(g: &ordnung_core::Tags) -> bool {
    [
        &g.musicbrainz_recording_id,
        &g.musicbrainz_track_id,
        &g.musicbrainz_release_id,
        &g.musicbrainz_release_group_id,
        &g.musicbrainz_artist_id,
        &g.musicbrainz_release_artist_id,
        &g.musicbrainz_work_id,
        &g.musicbrainz_release_type,
        &g.acoust_id,
    ]
    .iter()
    .any(|v| has_value(v))
}

pub(crate) fn has_any_encoder(g: &ordnung_core::Tags) -> bool {
    [
        &g.encoded_by,
        &g.encoder_software,
        &g.encoder_settings,
        &g.original_artist,
        &g.original_album,
    ]
    .iter()
    .any(|v| has_value(v))
}

pub(crate) fn has_any_content(g: &ordnung_core::Tags) -> bool {
    [
        &g.subtitle,
        &g.description,
        &g.language,
        &g.script,
        &g.work,
        &g.movement,
        &g.comment,
        &g.lyrics,
    ]
    .iter()
    .any(|v| has_value(v))
        || g.movement_number.is_some()
}

pub(crate) fn has_value(v: &Option<String>) -> bool {
    v.as_deref().is_some_and(|s| !s.trim().is_empty())
}

/// One titled block in the inspector: a dimmed caption with a hairline rule
/// under it, then the body. The rule is what makes the sections read as
/// distinct groups instead of one long list of label/value pairs.
pub(crate) fn inspector_section(
    ui: &mut egui::Ui,
    title: &str,
    add_body: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(12.0);
    ui.label(egui::RichText::new(title.to_uppercase()).strong().small());
    ui.add_space(3.0);
    section_rule(ui);
    ui.add_space(5.0);
    add_body(ui);
}

/// Like [`inspector_section`] but with a trailing control on the caption row
/// (e.g. the tag block's Edit toggle), right-aligned against the rule below.
pub(crate) fn inspector_section_with_action(
    ui: &mut egui::Ui,
    title: &str,
    add_action: impl FnOnce(&mut egui::Ui),
    add_body: impl FnOnce(&mut egui::Ui),
) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.label(egui::RichText::new(title.to_uppercase()).strong().small());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), add_action);
    });
    ui.add_space(3.0);
    section_rule(ui);
    ui.add_space(5.0);
    add_body(ui);
}

/// The hairline under a section caption.
fn section_rule(ui: &mut egui::Ui) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(w, 1.0), egui::Sense::hover());
    ui.painter().rect_filled(
        rect,
        0.0,
        ui.visuals().weak_text_color().gamma_multiply(0.3),
    );
}

/// A tag field shown read-only (edit mode off). Unlike [`opt_row`] an empty
/// value still renders, as a dimmed em dash: the tag block doubles as the
/// "what is still missing on this track" list, so a blank must stay visible.
pub(crate) fn tag_view_row(ui: &mut egui::Ui, label: &str, value: &str) {
    let v = value.trim();
    if v.is_empty() {
        inspector_row_dim(ui, label, "—");
    } else {
        inspector_row(ui, label, v);
    }
}

/// One read-only label/value line. The label sits in a fixed-width column so
/// values align vertically down a section rather than starting at a different
/// x for every row.
pub(crate) fn inspector_row(ui: &mut egui::Ui, label: &str, value: &str) {
    row_impl(ui, label, value, None);
}

/// [`inspector_row`] with the value dimmed — used for "not set" placeholders.
pub(crate) fn inspector_row_dim(ui: &mut egui::Ui, label: &str, value: &str) {
    let c = ui.visuals().weak_text_color();
    row_impl(ui, label, value, Some(c));
}

fn row_impl(ui: &mut egui::Ui, label: &str, value: &str, value_color: Option<egui::Color32>) {
    const LABEL_W: f32 = 108.0;
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(
            egui::vec2(LABEL_W.min(ui.available_width() * 0.5), 16.0),
            egui::Sense::hover(),
        );
        ui.painter().text(
            rect.left_center(),
            egui::Align2::LEFT_CENTER,
            label,
            egui::FontId::proportional(11.0),
            ui.visuals().weak_text_color(),
        );
        let mut txt = egui::RichText::new(value).size(12.5);
        if let Some(c) = value_color {
            txt = txt.color(c);
        }
        ui.label(txt);
    });
}

pub(crate) fn opt_row(ui: &mut egui::Ui, label: &str, value: &Option<String>) {
    if let Some(s) = value.as_deref().filter(|s| !s.trim().is_empty()) {
        inspector_row(ui, label, s);
    }
}

/// One editable label + single-line field row inside the inspector's edit grid.
/// `ui.end_row()` advances the surrounding `egui::Grid`.
pub(crate) fn edit_row(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(egui::RichText::new(label).weak().small());
    ui.add(
        egui::TextEdit::singleline(value)
            .hint_text("—")
            .desired_width(f32::INFINITY),
    );
    ui.end_row();
}

/// Like [`edit_row`] but a wrapping, multi-line box — for free-form text such as
/// the comment/notes field, which is often longer than one line.
pub(crate) fn edit_row_multiline(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(egui::RichText::new(label).weak().small());
    ui.add(
        egui::TextEdit::multiline(value)
            .hint_text("—")
            .desired_rows(2)
            .desired_width(f32::INFINITY),
    );
    ui.end_row();
}
