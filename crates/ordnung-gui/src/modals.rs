//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// The cover-drop confirmation popup: shown after an image is dropped onto a
    /// track row. Previews the image, asks whether to set it as that track's
    /// cover, and — when the track has album-mates — offers a per-song selector to
    /// apply the same cover across the album. Nothing is written until "Set cover".
    pub(crate) fn draw_cover_drop(&mut self, ctx: &egui::Context) {
        if self.cover_drop.is_none() {
            return;
        }
        let mut open = true;
        let mut apply = false;
        let mut cancel = false;
        if let Some(d) = self.cover_drop.as_mut() {
            egui::Window::new("Set album art")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center())
                .show(ctx, |ui| {
                    ui.set_min_width(420.0);
                    // Image preview, centred.
                    if let Some(tex) = &d.preview {
                        ui.vertical_centered(|ui| {
                            ui.add(
                                egui::Image::new(tex)
                                    .maintain_aspect_ratio(true)
                                    .fit_to_exact_size(egui::vec2(140.0, 140.0)),
                            );
                        });
                        ui.add_space(4.0);
                    }
                    ui.label(egui::RichText::new("Set this image as the cover for").strong());
                    ui.label(egui::RichText::new(&d.track_label).strong());
                    if let Some(name) = d.image_path.file_name() {
                        ui.label(
                            egui::RichText::new(name.to_string_lossy())
                                .small()
                                .weak(),
                        );
                    }

                    // Album-mate selector — only when this track shares its album
                    // with others. Lets the user dress the whole album in one go.
                    if !d.siblings.is_empty() {
                        ui.add_space(4.0);
                        ui.separator();
                        ui.label(
                            egui::RichText::new("Also apply to other tracks on this album")
                                .strong(),
                        );
                        if !d.album.trim().is_empty() {
                            ui.label(egui::RichText::new(&d.album).small().weak());
                        }
                        ui.horizontal(|ui| {
                            if ui.small_button("Select all").clicked() {
                                for s in &mut d.siblings {
                                    s.selected = true;
                                }
                            }
                            if ui.small_button("None").clicked() {
                                for s in &mut d.siblings {
                                    s.selected = false;
                                }
                            }
                            ui.label(
                                egui::RichText::new(
                                    "Tracks that already have a cover start unchecked.",
                                )
                                .small()
                                .weak(),
                            );
                        });
                        ui.add_space(2.0);
                        // Cap the list height so a big album can't push the buttons
                        // off-screen; it scrolls past that.
                        egui::ScrollArea::vertical()
                            .max_height(200.0)
                            .auto_shrink([false, false])
                            .show(ui, |ui| {
                                for s in &mut d.siblings {
                                    ui.horizontal(|ui| {
                                        ui.checkbox(&mut s.selected, &s.label);
                                        if s.has_art {
                                            ui.label(
                                                egui::RichText::new("· has cover")
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                    });
                                }
                            });
                        let n = d.siblings.iter().filter(|s| s.selected).count();
                        ui.label(
                            egui::RichText::new(format!(
                                "{n} of {} album track(s) selected",
                                d.siblings.len()
                            ))
                            .small()
                            .weak(),
                        );
                    }

                    ui.add_space(8.0);
                    ui.separator();
                    ui.horizontal(|ui| {
                        let n = d.siblings.iter().filter(|s| s.selected).count();
                        let label = if n > 0 {
                            format!("Set cover ({} track(s))", n + 1)
                        } else {
                            "Set cover".to_string()
                        };
                        let btn = egui::Button::new(
                            egui::RichText::new(label).color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(64, 110, 180));
                        if ui.add(btn).clicked() {
                            apply = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                        ui.with_layout(
                            egui::Layout::right_to_left(egui::Align::Center),
                            |ui| {
                                ui.label(
                                    egui::RichText::new("Catalog only — your files aren't touched.")
                                        .small()
                                        .weak(),
                                );
                            },
                        );
                    });
                });
        }
        if apply {
            if let Some(d) = self.cover_drop.take() {
                self.apply_cover_drop(d);
            }
        } else if cancel || !open {
            self.cover_drop = None;
        }
    }

    /// The batch-convert dialog: one set of options applied to every selected
    /// track. Mirrors the single convert modal's options (minus name editing).
    pub(crate) fn draw_batch_convert(&mut self, ctx: &egui::Context) {
        let mut open = self.batch_convert.is_some();
        let mut start = false;
        let mut close = false;
        if let Some(m) = self.batch_convert.as_mut() {
            egui::Window::new("Convert selected tracks")
                .open(&mut open)
                .collapsible(false)
                .resizable(false)
                .pivot(egui::Align2::CENTER_CENTER)
                .default_pos(ctx.screen_rect().center())
                .show(ctx, |ui| {
                    ui.set_min_width(460.0);
                    ui.label(
                        egui::RichText::new(format!("{} track(s) selected", m.ids.len())).strong(),
                    );
                    ui.label(
                        egui::RichText::new(
                            "New files keep the full catalog metadata + cover art.",
                        )
                        .weak(),
                    );
                    ui.separator();
                    egui::Grid::new("batch_convert_grid")
                        .num_columns(2)
                        .spacing([12.0, 8.0])
                        .show(ui, |ui| {
                            ui.label("Target format:");
                            egui::ComboBox::from_id_salt("batch_target_format")
                                .selected_text(format_label(m.target))
                                .show_ui(ui, |ui| {
                                    for &f in &[
                                        Format::Mp3,
                                        Format::Aac,
                                        Format::Flac,
                                        Format::Wav,
                                        Format::Aiff,
                                    ] {
                                        ui.selectable_value(&mut m.target, f, format_label(f));
                                    }
                                });
                            ui.end_row();

                            ui.label("Bitrate (kbps):");
                            let lossy = matches!(m.target, Format::Mp3 | Format::Aac);
                            ui.add_enabled(
                                lossy,
                                egui::TextEdit::singleline(&mut m.bitrate_kbps)
                                    .hint_text(default_bitrate_hint(m.target))
                                    .desired_width(80.0),
                            );
                            ui.end_row();

                            ui.label("Output folder:");
                            ui.horizontal(|ui| {
                                let text = match &m.out_dir {
                                    Some(p) => p.display().to_string(),
                                    None => "(alongside each source)".into(),
                                };
                                ui.label(egui::RichText::new(text).monospace());
                                if ui.small_button("Pick…").clicked() {
                                    if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                        m.out_dir = Some(d);
                                    }
                                }
                                if m.out_dir.is_some() && ui.small_button("Clear").clicked() {
                                    m.out_dir = None;
                                }
                            });
                            ui.end_row();

                            ui.label("In-place:");
                            ui.checkbox(&mut m.in_place, "Replace each source file");
                            ui.end_row();
                        });

                    if m.in_place {
                        ui.colored_label(
                            egui::Color32::LIGHT_YELLOW,
                            "Warning: the original files will be removed and the catalog repointed.",
                        );
                    }
                    if let Some(err) = &m.error {
                        ui.add_space(4.0);
                        ui.colored_label(egui::Color32::LIGHT_RED, err);
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        let busy = self.job_rx.is_some();
                        if ui
                            .add_enabled(
                                !busy,
                                egui::Button::new(format!("Convert {}", m.ids.len())),
                            )
                            .clicked()
                        {
                            start = true;
                        }
                        if ui.button("Cancel").clicked() {
                            close = true;
                        }
                    });
                });
        }
        if start {
            if let Some(m) = self.batch_convert.as_ref() {
                let ids = m.ids.clone();
                let target = m.target;
                let bitrate = m.bitrate_kbps.clone();
                let out_dir = m.out_dir.clone();
                let in_place = m.in_place;
                match self.spawn_batch_convert(
                    ctx.clone(),
                    ids,
                    target,
                    &bitrate,
                    out_dir,
                    in_place,
                ) {
                    Ok(()) => close = true,
                    Err(e) => {
                        if let Some(cur) = self.batch_convert.as_mut() {
                            cur.error = Some(e);
                        }
                    }
                }
            }
        }
        if close || !open {
            self.batch_convert = None;
        }
    }

    /// Settings window: enter and persist the Discogs token. Saved to
    /// `~/.ordnung/config.toml` so it survives restarts and Finder launches
    /// (which inherit no shell environment).
    pub(crate) fn draw_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut window_open = true;
        let mut save = false;
        egui::Window::new("Settings")
            .open(&mut window_open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(440.0);
                ui.label(egui::RichText::new("Discogs token").strong());
                ui.label(
                    egui::RichText::new(
                        "Used to fetch cover art. Create a personal access token at \
                         discogs.com → Settings → Developers, then paste it here.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.token_input)
                        .password(true)
                        .hint_text("paste your Discogs token")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        self.token_input = self.config.discogs_token.clone();
                        self.settings_open = false;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(p) = config::config_path() {
                            ui.label(egui::RichText::new(p.display().to_string()).small().weak());
                        }
                    });
                });

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Analysis").strong());
                if ui
                    .checkbox(
                        &mut self.config.auto_analyze,
                        "Analyze tracks automatically when added",
                    )
                    .on_hover_text(
                        "Run BPM, key, and waveform analysis on each track as it's imported. \
                         Turn this off to analyze on demand with the Analyze button instead.",
                    )
                    .changed()
                {
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save settings: {e}");
                    }
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Waveform").strong());
                ui.label(
                    egui::RichText::new(
                        "How the player's waveform is colored. Energy shades each \
                         section by its loudness (cool → hot); Spectrum colors by \
                         frequency content (low = red, mid = green, high = blue).",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                let current = config::WaveformColorMode::from_key(&self.config.waveform_color_mode);
                let mut picked = current;
                egui::ComboBox::from_id_salt("settings_waveform_color")
                    .selected_text(match current {
                        config::WaveformColorMode::Energy => "Energy (loudness)",
                        config::WaveformColorMode::Spectrum => "Spectrum (frequency)",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut picked,
                            config::WaveformColorMode::Energy,
                            "Energy (loudness)",
                        );
                        ui.selectable_value(
                            &mut picked,
                            config::WaveformColorMode::Spectrum,
                            "Spectrum (frequency)",
                        );
                    });
                if picked != current {
                    self.config.waveform_color_mode = picked.key().to_string();
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save settings: {e}");
                    }
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Sorting").strong());
                ui.label(
                    egui::RichText::new(
                        "How the track table is sorted when the app launches. You can \
                         still click any column header to re-sort during a session.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                let mut sort_dirty = false;
                // Sortable columns, in display order, plus a "Natural order"
                // sentinel (empty key) that keeps catalog/playlist order.
                let selected_label = if self.config.default_sort.trim().is_empty() {
                    "Natural order".to_string()
                } else {
                    TableColumn::from_key(&self.config.default_sort)
                        .map(|c| c.label().to_string())
                        .unwrap_or_else(|| "Natural order".to_string())
                };
                egui::Grid::new("settings_sort_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Default sort:");
                        egui::ComboBox::from_id_salt("settings_default_sort")
                            .selected_text(selected_label)
                            .show_ui(ui, |ui| {
                                if ui
                                    .selectable_label(
                                        self.config.default_sort.trim().is_empty(),
                                        "Natural order",
                                    )
                                    .clicked()
                                    && !self.config.default_sort.is_empty()
                                {
                                    self.config.default_sort.clear();
                                    sort_dirty = true;
                                }
                                for col in TableColumn::DEFAULT_ORDER {
                                    if col.sort_column().is_none() {
                                        continue;
                                    }
                                    let key = col.key();
                                    if ui
                                        .selectable_label(
                                            self.config.default_sort == key,
                                            col.label(),
                                        )
                                        .clicked()
                                        && self.config.default_sort != key
                                    {
                                        self.config.default_sort = key.to_string();
                                        sort_dirty = true;
                                    }
                                }
                            });
                        ui.end_row();

                        ui.label("Direction:");
                        let has_sort = !self.config.default_sort.trim().is_empty();
                        let dir_text = if self.config.default_sort_ascending {
                            "Ascending (A→Z, oldest first)"
                        } else {
                            "Descending (Z→A, newest first)"
                        };
                        ui.add_enabled_ui(has_sort, |ui| {
                            egui::ComboBox::from_id_salt("settings_default_sort_dir")
                                .selected_text(dir_text)
                                .show_ui(ui, |ui| {
                                    if ui
                                        .selectable_label(
                                            self.config.default_sort_ascending,
                                            "Ascending (A→Z, oldest first)",
                                        )
                                        .clicked()
                                        && !self.config.default_sort_ascending
                                    {
                                        self.config.default_sort_ascending = true;
                                        sort_dirty = true;
                                    }
                                    if ui
                                        .selectable_label(
                                            !self.config.default_sort_ascending,
                                            "Descending (Z→A, newest first)",
                                        )
                                        .clicked()
                                        && self.config.default_sort_ascending
                                    {
                                        self.config.default_sort_ascending = false;
                                        sort_dirty = true;
                                    }
                                });
                        });
                        ui.end_row();
                    });
                if sort_dirty {
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save settings: {e}");
                    }
                    // Apply the new default to the live view immediately so the
                    // change is visible without relaunching.
                    self.sort = self.default_sort();
                    self.reload();
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Conversion").strong());
                ui.label(
                    egui::RichText::new(
                        "Defaults used when you open a Convert dialog. You can still \
                         change them per conversion.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(4.0);
                let mut convert_dirty = false;
                let mut target =
                    format_from_key(&self.config.convert_format).unwrap_or(Format::Aiff);
                egui::Grid::new("settings_convert_grid")
                    .num_columns(2)
                    .spacing([12.0, 8.0])
                    .show(ui, |ui| {
                        ui.label("Default format:");
                        let before = target;
                        egui::ComboBox::from_id_salt("settings_convert_format")
                            .selected_text(format_label(target))
                            .show_ui(ui, |ui| {
                                for &f in &[
                                    Format::Mp3,
                                    Format::Aac,
                                    Format::Flac,
                                    Format::Wav,
                                    Format::Aiff,
                                ] {
                                    ui.selectable_value(&mut target, f, format_label(f));
                                }
                            });
                        if target != before {
                            self.config.convert_format = format_key(target).to_string();
                            convert_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Default bitrate (kbps):");
                        let lossy = matches!(target, Format::Mp3 | Format::Aac);
                        let resp = ui.add_enabled(
                            lossy,
                            egui::TextEdit::singleline(&mut self.config.convert_bitrate_kbps)
                                .hint_text(default_bitrate_hint(target))
                                .desired_width(80.0),
                        );
                        if resp.lost_focus() {
                            convert_dirty = true;
                        }
                        ui.end_row();

                        ui.label("Default output folder:");
                        ui.horizontal(|ui| {
                            let text = match &self.config.convert_out_dir {
                                Some(p) => p.display().to_string(),
                                None => "(alongside each source)".into(),
                            };
                            ui.label(egui::RichText::new(text).monospace());
                            if ui.small_button("Pick…").clicked() {
                                if let Some(d) = rfd::FileDialog::new().pick_folder() {
                                    self.config.convert_out_dir = Some(d);
                                    convert_dirty = true;
                                }
                            }
                            if self.config.convert_out_dir.is_some()
                                && ui.small_button("Clear").clicked()
                            {
                                self.config.convert_out_dir = None;
                                convert_dirty = true;
                            }
                        });
                        ui.end_row();

                        ui.label("In-place by default:");
                        if ui
                            .checkbox(
                                &mut self.config.convert_in_place,
                                "Replace each source file",
                            )
                            .on_hover_text(
                                "When on, conversions replace the original file instead of \
                                 writing a new one. The catalog is repointed automatically.",
                            )
                            .changed()
                        {
                            convert_dirty = true;
                        }
                        ui.end_row();
                    });
                if self.config.convert_in_place {
                    ui.colored_label(
                        egui::Color32::LIGHT_YELLOW,
                        "In-place removes the original file on each conversion.",
                    );
                }
                if convert_dirty {
                    if let Err(e) = self.config.save() {
                        self.status = format!("Couldn't save settings: {e}");
                    }
                }

                ui.add_space(14.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(egui::RichText::new("Danger zone").strong());
                ui.label(
                    egui::RichText::new(
                        "Remove every scanned track, its analysis, and fetched artwork \
                         from the catalog. Playlists are kept but emptied. Your source \
                         audio files are not touched.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(6.0);
                let clear_btn = egui::Button::new(
                    egui::RichText::new("Clear catalog…").color(egui::Color32::WHITE),
                )
                .fill(egui::Color32::from_rgb(150, 40, 40));
                if ui.add(clear_btn).clicked() {
                    self.confirm_clear_db = true;
                }
            });

        if save {
            self.config.discogs_token = self.token_input.trim().to_string();
            match self.config.save() {
                Ok(()) => {
                    self.status = if self.config.discogs_token.is_empty() {
                        "Cleared Discogs token.".into()
                    } else {
                        "Saved Discogs token to ~/.ordnung/config.toml.".into()
                    };
                    self.settings_open = false;
                }
                Err(e) => self.status = format!("Couldn't save settings: {e}"),
            }
        }
        // The window's [x] toggled `window_open`; mirror it back to our flag.
        if !window_open {
            self.settings_open = false;
        }
    }

    /// "Are you sure?" popup for clearing the whole catalog. Drawn on top of
    /// Settings. Confirming wipes every track (analysis + fetched artwork
    /// cascade) and reloads the now-empty table.
    pub(crate) fn draw_clear_db_confirm(&mut self, ctx: &egui::Context) {
        if !self.confirm_clear_db {
            return;
        }
        let mut open = true;
        let mut confirm = false;
        egui::Window::new("Clear catalog?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                let n = self.rows.len();
                ui.label(
                    egui::RichText::new(format!(
                        "This permanently removes {n} track{} and all of their analysis \
                         and fetched artwork from the catalog. Playlists are kept but \
                         emptied.",
                        if n == 1 { "" } else { "s" }
                    ))
                    .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new("Your source audio files are not deleted or modified.")
                        .small()
                        .weak(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_clear_db = false;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new("Clear catalog").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(150, 40, 40));
                        if ui.add(btn).clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        if confirm {
            self.confirm_clear_db = false;
            match Catalog::open(&self.db_path).and_then(|c| c.clear_tracks()) {
                Ok(removed) => {
                    self.selected = None;
                    self.selected_track = None;
                    self.cover_cache.clear();
                    self.cover_full_cache.clear();
                    self.cover_inflight.clear();
                    self.reload();
                    self.status = format!(
                        "Cleared catalog — removed {removed} track{}.",
                        if removed == 1 { "" } else { "s" }
                    );
                }
                Err(e) => self.status = format!("Couldn't clear catalog: {e}"),
            }
        }
        if !open {
            self.confirm_clear_db = false;
        }
    }

    /// Report popup listing every item a background job skipped or errored on,
    /// with the reason. Auto-opens after a job that had any failures (scan, write
    /// edits, analyze, relocate) so the user knows exactly what didn't go through.
    pub(crate) fn draw_failure_report(&mut self, ctx: &egui::Context) {
        if !self.show_failure_report {
            return;
        }
        let mut open = true;
        let n = self.failure_report.len();
        let title = self.failure_report_title.clone();
        // Scale the window to the screen so it never overflows: cap width and
        // height to a fraction of the viewport (and a sane absolute max).
        let screen = ctx.screen_rect().size();
        let win_w = (screen.x * 0.6).clamp(280.0, 560.0);
        let list_h = (screen.y * 0.5).clamp(120.0, 420.0);
        egui::Window::new(format!("{title}: {n} item(s) failed"))
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .max_width(win_w)
            .default_width(win_w)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_max_width(win_w);
                ui.label(
                    egui::RichText::new(format!(
                        "{n} file{} couldn't be processed. The rest went through fine.",
                        if n == 1 { "" } else { "s" }
                    ))
                    .strong(),
                );
                ui.add_space(8.0);
                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.set_max_width(ui.available_width());
                        for (i, (name, reason)) in self.failure_report.iter().enumerate() {
                            if i > 0 {
                                ui.separator();
                            }
                            // Wrap both lines to the window width so long file
                            // names and reasons never force the popup wider.
                            ui.add(egui::Label::new(egui::RichText::new(name).strong()).wrap());
                            ui.add(egui::Label::new(egui::RichText::new(reason).weak()).wrap());
                        }
                    });
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Copy details").clicked() {
                        let text = self
                            .failure_report
                            .iter()
                            .map(|(name, reason)| format!("{name}\t{reason}"))
                            .collect::<Vec<_>>()
                            .join("\n");
                        ui.ctx().copy_text(text);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Close").clicked() {
                            self.show_failure_report = false;
                        }
                    });
                });
            });
        if !open {
            self.show_failure_report = false;
        }
    }

    /// "Write all edited tracks to their source files?" confirmation. Source
    /// files are mutated and the analysis cache for those tracks is invalidated
    /// (their mtimes change), so this is an explicit, gated action. Confirming
    /// kicks off the background bulk-write job.
    pub(crate) fn draw_bulk_write_confirm(&mut self, ctx: &egui::Context) {
        if !self.confirm_bulk_write {
            return;
        }
        let mut open = true;
        let mut confirm = false;
        let n = self.edited_count;
        egui::Window::new("Write edits to source files?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                ui.label(
                    egui::RichText::new(format!(
                        "This writes the edited tags of {n} track{} into their original \
                         files on disk.",
                        if n == 1 { "" } else { "s" }
                    ))
                    .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "Only tracks you've edited are written. Any fetched cover art is \
                         embedded into the file; tracks without fetched art keep their \
                         existing cover. Each file's modification time changes, so those \
                         tracks will be re-analyzed on the next Analyze.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_bulk_write = false;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new("Write to files").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(70, 110, 70));
                        if ui.add(btn).clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        if confirm {
            self.confirm_bulk_write = false;
            self.spawn_write_edits(ctx.clone());
        }
        if !open {
            self.confirm_bulk_write = false;
        }
    }

    /// "Delete N tracks from the catalog?" confirmation. Catalog rows (and their
    /// playlist links + analysis cache) are removed; source files are left on
    /// disk untouched. Destructive and not undoable, so it's an explicit gate.
    pub(crate) fn draw_delete_confirm(&mut self, ctx: &egui::Context) {
        let Some(ids) = self.confirm_delete.clone() else {
            return;
        };
        let n = ids.len();
        let mut open = true;
        let mut confirm = false;
        egui::Window::new("Delete from catalog?")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(400.0);
                ui.label(
                    egui::RichText::new(format!(
                        "Remove {n} track{} from the catalog?",
                        if n == 1 { "" } else { "s" }
                    ))
                    .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    egui::RichText::new(
                        "They are dropped from every playlist and the analysis cache. \
                         The original files on disk are NOT deleted. This can't be undone.",
                    )
                    .small()
                    .weak(),
                );
                ui.add_space(12.0);
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_delete = None;
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let btn = egui::Button::new(
                            egui::RichText::new("Delete").color(egui::Color32::WHITE),
                        )
                        .fill(egui::Color32::from_rgb(150, 60, 60));
                        if ui.add(btn).clicked() {
                            confirm = true;
                        }
                    });
                });
            });

        if confirm {
            self.confirm_delete = None;
            match Catalog::open(&self.db_path).and_then(|c| c.delete_tracks(&ids)) {
                Ok(removed) => {
                    self.status = format!(
                        "Deleted {removed} track{} from the catalog.",
                        if removed == 1 { "" } else { "s" }
                    );
                    self.selection.retain(|id| !ids.contains(id));
                }
                Err(e) => self.status = format!("Delete failed: {e}"),
            }
            self.reload();
        }
        if !open {
            self.confirm_delete = None;
        }
    }

    /// Modal picker: shows every Discogs release candidate found for the front
    /// track and lets the user choose one (or skip). Nothing is written to the
    /// catalog until Save; the full-resolution image for the chosen release is
    /// downloaded at that point. Skip drops the track (re-fetchable later).
    pub(crate) fn draw_artwork_review(&mut self, ctx: &egui::Context) {
        // Decode (or refresh) preview thumbnails for the front track's candidates.
        match self.artwork_queue.front() {
            Some(choices) => {
                let stale = self.artwork_previews.as_ref().map(|(id, _)| *id) != Some(choices.id);
                if stale {
                    let texs = choices
                        .candidates
                        .iter()
                        .enumerate()
                        .map(|(i, c)| decode_thumb(ctx, choices.id, i, &c.thumb_png))
                        .collect();
                    self.artwork_previews = Some((choices.id, texs));
                    self.artwork_selected = 0;
                }
            }
            None => {
                self.artwork_previews = None;
                self.artwork_selected = 0;
                return;
            }
        }

        // Refresh the album-mate counts whenever the front track changes (one DB
        // query per track, then cached). Drives the "apply to album" controls below.
        if let Some(choices) = self.artwork_queue.front() {
            let front = choices.id;
            if self.artwork_album_count.map(|(id, _, _)| id) != Some(front) {
                // One detailed query gives both the counts and the per-song labels
                // so the picker can show *which* tracks share the chosen cover.
                let details = Catalog::open(&self.db_path)
                    .and_then(|c| c.album_siblings_detailed(front))
                    .unwrap_or_default();
                let total = details.len();
                let missing = details.iter().filter(|s| !s.has_art).count();
                self.artwork_album_count = Some((front, missing, total));
                self.artwork_album_siblings = Some((
                    front,
                    details
                        .iter()
                        .map(|s| {
                            (
                                s.id,
                                track_display_label(s.artist.as_deref(), s.title.as_deref()),
                                s.has_art,
                            )
                        })
                        .collect(),
                ));
                // Default the "set cover" toggle per track: leave an existing
                // cover alone (off), offer to add one when the track has none (on).
                // Only relevant to the song-data run — the artwork run ignores it.
                if self.artwork_enrich {
                    let has_art = Catalog::open(&self.db_path)
                        .and_then(|c| c.track_has_art(front))
                        .unwrap_or(false);
                    self.artwork_set_cover = !has_art;
                }
            }
        }

        let mut save = false;
        let mut skip = false;
        let mut skip_all = false;
        // "None of these" — the user reviewed the candidates and none is the
        // right release. Marks the track fetched so it drops out of the Recently
        // Added inbox and won't be re-offered, without writing any cover/tags.
        let mut no_match = false;
        // Esc closes the picker — cancels the whole review queue (but never
        // interrupts an in-flight save, so the worker isn't left dangling).
        if !self.artwork_saving && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            skip_all = true;
        }
        let saving = self.artwork_saving;
        let enrich = self.artwork_enrich;
        let mut overwrite = self.artwork_overwrite;
        // Song-data run only: whether picking a release also writes its cover.
        let mut set_cover = self.artwork_set_cover;
        // Album-mate counts and the "also apply to them" toggles.
        let (album_missing, album_total) = self
            .artwork_album_count
            .map(|(_, m, t)| (m, t))
            .unwrap_or((0, 0));
        let mut apply_album = self.artwork_apply_album;
        let mut album_overwrite = self.artwork_album_overwrite;
        // The album-mates' song names (with whether each already has a cover),
        // cloned out so the window closure can list which tracks share the cover
        // without borrowing `self`. Empty when this track has no album-mates.
        let album_mate_labels: Vec<(String, bool)> = self
            .artwork_album_siblings
            .as_ref()
            .map(|(_, v)| v.iter().map(|(_, l, h)| (l.clone(), *h)).collect())
            .unwrap_or_default();
        let remaining = self.artwork_queue.len();
        let choices = self.artwork_queue.front().expect("front checked above");
        let previews = self.artwork_previews.as_ref().map(|(_, t)| t);
        let mut selected = self
            .artwork_selected
            .min(choices.candidates.len().saturating_sub(1));
        // For the song-data picker, look up the field preview for the currently
        // highlighted release (computed last frame; refreshed below). Keyed by
        // the overwrite flag too, so toggling it shows the right field set. Owned
        // clones so the window closure doesn't borrow `self`.
        let cur_release = choices
            .candidates
            .get(selected)
            .map(|c| c.release_id.clone());
        let preview_rows: Option<Vec<(String, String)>> = cur_release.as_ref().and_then(|r| {
            self.preview_cache
                .get(&(choices.id, r.clone(), overwrite))
                .cloned()
        });
        let preview_loading = cur_release.as_ref().is_some_and(|r| {
            self.preview_inflight
                .contains(&(choices.id, r.clone(), overwrite))
        });
        let picker_title = if enrich {
            "Pick release"
        } else {
            "Pick artwork"
        };
        // Keep the window inside the screen: cap its height to the available
        // area and let the candidate list (the part that grows with the number
        // of releases) take whatever vertical space is left, scrolling the rest.
        let screen = ctx.screen_rect();
        let max_h = (screen.height() - 80.0).max(240.0);
        let max_w = (screen.width() - 80.0).clamp(320.0, 560.0);
        // Space reserved for the fixed chrome (header, preview/album toggles,
        // buttons) so they stay visible; the list scrolls within the remainder.
        let reserve = if enrich { 320.0 } else { 200.0 };
        let list_h = (max_h - reserve).clamp(100.0, 360.0);
        egui::Window::new(picker_title)
            .collapsible(false)
            .resizable(true)
            .default_width(460.0)
            .max_width(max_w)
            .max_height(max_h)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                // Hard-cap the content width so a long, non-wrapping field value
                // (e.g. a release with dozens of styles) can't stretch the window
                // past the screen and shove the action buttons out of reach.
                ui.set_max_width(max_w);
                ui.label(egui::RichText::new(&choices.label).strong());
                ui.label(
                    egui::RichText::new(format!(
                        "{} release(s) on Discogs — pick the right one",
                        choices.candidates.len()
                    ))
                    .small()
                    .weak(),
                );
                if enrich {
                    ui.label(
                        egui::RichText::new(
                            "Pick the release whose details to add — only this track's \
                             empty fields get filled (catalog only, never your file).",
                        )
                        .small()
                        .weak(),
                    );
                }
                ui.separator();

                egui::ScrollArea::vertical()
                    .max_height(list_h)
                    .show(ui, |ui| {
                        for (i, c) in choices.candidates.iter().enumerate() {
                            let is_sel = i == selected;
                            let resp = ui
                                .horizontal(|ui| {
                                    match previews.and_then(|t| t.get(i)).and_then(|t| t.as_ref()) {
                                        Some(tex) => {
                                            ui.add(
                                                egui::Image::new(tex)
                                                    .fit_to_exact_size(egui::vec2(64.0, 64.0)),
                                            );
                                        }
                                        None => {
                                            let (rect, _) = ui.allocate_exact_size(
                                                egui::vec2(64.0, 64.0),
                                                egui::Sense::hover(),
                                            );
                                            ui.painter().rect_filled(
                                                rect,
                                                egui::Rounding::same(3.0),
                                                egui::Color32::from_gray(40),
                                            );
                                        }
                                    }
                                    ui.vertical(|ui| {
                                        ui.label(
                                            egui::RichText::new(short(
                                                &c.title,
                                                "Untitled release",
                                            ))
                                            .strong(),
                                        );
                                        let meta = [
                                            c.year.as_str(),
                                            c.label.as_str(),
                                            c.country.as_str(),
                                            c.format.as_str(),
                                        ]
                                        .iter()
                                        .filter(|s| !s.is_empty())
                                        .cloned()
                                        .collect::<Vec<&str>>()
                                        .join(" · ");
                                        if !meta.is_empty() {
                                            ui.label(egui::RichText::new(meta).small().weak());
                                        }
                                        ui.label(
                                            egui::RichText::new(format!(
                                                "Discogs release {}",
                                                c.release_id
                                            ))
                                            .small()
                                            .weak(),
                                        );
                                    });
                                })
                                .response;
                            let row = ui.interact(
                                resp.rect,
                                egui::Id::new(("art-cand", choices.id, i)),
                                egui::Sense::click(),
                            );
                            if is_sel {
                                ui.painter().rect_stroke(
                                    resp.rect,
                                    egui::Rounding::same(4.0),
                                    egui::Stroke::new(2.0, egui::Color32::LIGHT_BLUE),
                                );
                            }
                            if row.clicked() {
                                selected = i;
                            }
                            ui.separator();
                        }
                    });

                // Song-data mode: show exactly which fields the highlighted
                // release would write (and their values) before the user commits.
                if enrich {
                    ui.add_space(6.0);
                    ui.checkbox(&mut overwrite, "Overwrite existing fields")
                        .on_hover_note(
                            "Off: fill only empty fields (non-destructive). On: replace \
                         the track's existing values with this release's too. \
                         Catalog only — your source files are never touched.",
                        );
                    ui.label(
                        egui::RichText::new(if overwrite {
                            "Will write to this track (replacing existing values)"
                        } else {
                            "Will add to this track (empty fields only)"
                        })
                        .strong(),
                    );
                    egui::Frame::none()
                        .fill(egui::Color32::from_gray(28))
                        .inner_margin(egui::Margin::same(8.0))
                        .rounding(egui::Rounding::same(4.0))
                        .show(ui, |ui| match (&preview_rows, preview_loading) {
                            (Some(rows), _) if !rows.is_empty() => {
                                // Scrolls when a release fills many fields, so the
                                // grid can't push the action buttons off-screen.
                                egui::ScrollArea::vertical()
                                    .max_height(160.0)
                                    .show(ui, |ui| {
                                        egui::Grid::new(("preview-grid", choices.id))
                                            .num_columns(2)
                                            .spacing(egui::vec2(12.0, 4.0))
                                            .show(ui, |ui| {
                                                for (field, value) in rows {
                                                    ui.label(
                                                        egui::RichText::new(field.as_str()).weak(),
                                                    );
                                                    // Wrap long values (genre/style
                                                    // lists run very long) so they
                                                    // flow downward instead of
                                                    // widening the modal.
                                                    ui.add(egui::Label::new(value.as_str()).wrap());
                                                    ui.end_row();
                                                }
                                            });
                                    });
                            }
                            (Some(_), _) => {
                                ui.label(
                                    egui::RichText::new(
                                        "No new fields — this release adds nothing the \
                                         track is missing.",
                                    )
                                    .small()
                                    .weak(),
                                );
                            }
                            (None, true) => {
                                ui.horizontal(|ui| {
                                    ui.add(egui::Spinner::new());
                                    ui.label(
                                        egui::RichText::new("Loading release details…").weak(),
                                    );
                                });
                            }
                            (None, false) => {
                                ui.label(
                                    egui::RichText::new("Select a release to preview its fields.")
                                        .small()
                                        .weak(),
                                );
                            }
                        });
                }

                // Song-data run: setting the cover is optional and separate from
                // the tag fill, so enriching a track that already has artwork
                // doesn't silently replace it. Defaulted off when the track has a
                // cover, on when it doesn't (see the front-track refresh above).
                if enrich {
                    ui.add_space(6.0);
                    ui.checkbox(&mut set_cover, "Set the album cover from this release")
                        .on_hover_note(
                            "Off: keep the track's current cover and only fill its \
                             tag fields. On: also replace the cover with this \
                             release's art. Catalog only — your source files are \
                             never touched.",
                        );
                }

                // Offer to dress the rest of the album in one go: copy the chosen
                // cover to its album-mates. By default only the cover-less ones;
                // an opt-in sub-toggle also overwrites mates that already have art
                // so the whole album matches exactly. Shown only when album-mates
                // exist, a cover is actually being written, and no save is in flight.
                if album_total > 0 && (set_cover || !enrich) && !saving {
                    ui.add_space(6.0);
                    ui.checkbox(
                        &mut apply_album,
                        "Also apply this cover to other tracks on this album",
                    )
                    .on_hover_note(
                        "Copies the selected artwork to the other tracks on this \
                             album in the catalog.",
                    );
                    // Spell out exactly which tracks share this album, so the user
                    // knows what "apply to album" (and the overwrite toggle) touches.
                    if !album_mate_labels.is_empty() {
                        egui::CollapsingHeader::new(format!(
                            "{} other track(s) on this album",
                            album_mate_labels.len()
                        ))
                        .id_salt(("album-mate-list", choices.id))
                        .default_open(false)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical().max_height(120.0).show(ui, |ui| {
                                for (label, has) in &album_mate_labels {
                                    ui.horizontal(|ui| {
                                        ui.label(egui::RichText::new(label).small());
                                        if *has {
                                            ui.label(
                                                egui::RichText::new("· has cover")
                                                    .small()
                                                    .weak(),
                                            );
                                        }
                                    });
                                }
                            });
                        });
                    }
                    if apply_album {
                        ui.indent("apply_album_opts", |ui| {
                            ui.add_enabled_ui(album_total > album_missing, |ui| {
                                ui.checkbox(
                                    &mut album_overwrite,
                                    "Replace covers they already have (match the whole album)",
                                )
                                .on_hover_note(
                                    "On: every track on the album gets this cover, \
                                     replacing any it already has. Off: only tracks \
                                     with no cover are filled in.",
                                );
                            });
                            // Spell out exactly how many tracks will be touched.
                            let n = if album_overwrite {
                                album_total
                            } else {
                                album_missing
                            };
                            let detail = if album_overwrite {
                                format!(
                                    "Applies to all {album_total} other track(s) on this album."
                                )
                            } else if album_missing == 0 {
                                "No cover-less tracks on this album — enable replace to \
                                 cover them all."
                                    .to_string()
                            } else {
                                format!(
                                    "Applies to {n} cover-less track(s); {} already have art.",
                                    album_total - album_missing
                                )
                            };
                            ui.label(egui::RichText::new(detail).small().weak());
                        });
                    }
                }

                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if saving {
                        // Full-res download + write is running on a worker; show
                        // a spinner in place of Save and lock the other controls
                        // so the front track can't change mid-save.
                        ui.add(egui::Spinner::new());
                        ui.label(
                            egui::RichText::new(if enrich { "Filling…" } else { "Saving…" }).weak(),
                        );
                    } else {
                        // Picking a release commits both its tags and its cover, so
                        // it's available whenever a candidate exists — even if the
                        // release adds no new fields, its artwork is still applied.
                        let (btn_label, can_commit) = if enrich {
                            ("Use this release", !choices.candidates.is_empty())
                        } else {
                            ("Save selected", !choices.candidates.is_empty())
                        };
                        if ui
                            .add_enabled(can_commit, egui::Button::new(btn_label))
                            .clicked()
                        {
                            save = true;
                        }
                        if ui
                            .button("Skip track")
                            .on_hover_note(
                                "Decide later — leaves the track in Recently Added so the next \
                                 fetch offers it again.",
                            )
                            .clicked()
                        {
                            skip = true;
                        }
                        // Song-data run only: a definitive "none of these is right"
                        // that retires the track from the inbox (vs. Skip, which
                        // keeps it re-fetchable). Wrong matches are a song-details
                        // problem; the artwork-only run has no inbox to clear.
                        if enrich
                            && ui
                                .button("None of these")
                                .on_hover_note(
                                    "No candidate matches this song. Marks it done so it leaves \
                                     Recently Added and isn't offered again. Re-run later with \
                                     \"Edit release…\" if you change your mind.",
                                )
                                .clicked()
                        {
                            no_match = true;
                        }
                        if remaining > 1 && ui.button(format!("Skip all ({remaining})")).clicked() {
                            skip_all = true;
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format!("{remaining} track(s) to review"))
                                .small()
                                .weak(),
                        );
                    });
                });
            });

        self.artwork_selected = selected;
        self.artwork_overwrite = overwrite;
        self.artwork_set_cover = set_cover;
        self.artwork_apply_album = apply_album;
        self.artwork_album_overwrite = album_overwrite;

        // Song-data picker: make sure the highlighted release's field preview is
        // loading/loaded (no-op once cached). Done here so it picks up a row the
        // user just clicked (or a toggle of `overwrite`), without borrowing
        // `self` inside the window closure.
        if enrich && !save && !skip && !skip_all && !no_match {
            if let Some((tid, rid)) = self.artwork_queue.front().and_then(|ch| {
                ch.candidates
                    .get(selected)
                    .map(|c| (ch.id, c.release_id.clone()))
            }) {
                self.ensure_metadata_preview(tid, &rid, overwrite, ctx);
            }
        }

        if skip_all {
            self.artwork_queue.clear();
            self.artwork_previews = None;
            self.artwork_selected = 0;
        } else if save {
            // The commit runs on a background thread; the queue advances in
            // `poll_artwork_save` once it lands, so the spinner stays visible.
            // One path for both modes: save the chosen release's cover, and (when
            // this is a song-data run) also fill the track's empty tag fields from
            // that same release. `enrich` is read inside `save_selected_artwork`.
            self.save_selected_artwork(ctx, selected);
        } else if no_match {
            // Retire the track from the inbox: mark it fetched (no cover/tags
            // written) so it leaves Recently Added and isn't re-offered, then
            // advance. Reload so the badge/inbox update right away.
            if let Some(choices) = self.artwork_queue.front() {
                let id = choices.id;
                // Note whether it was in the inbox *before* marking, so the
                // message only claims removal when the track actually left it.
                let was_recent = Catalog::open(&self.db_path)
                    .and_then(|c| c.is_recently_added(id, ANALYZER_VERSION))
                    .unwrap_or(false);
                match Catalog::open(&self.db_path).and_then(|c| c.mark_metadata_fetched(id)) {
                    Ok(()) => {
                        let still_recent = Catalog::open(&self.db_path)
                            .and_then(|c| c.is_recently_added(id, ANALYZER_VERSION))
                            .unwrap_or(false);
                        self.status = if was_recent && !still_recent {
                            "Marked as no match — removed from Recently Added.".into()
                        } else {
                            "Marked as no match.".into()
                        };
                    }
                    Err(e) => self.status = format!("Couldn't mark as no match: {e}"),
                }
            }
            self.artwork_queue.pop_front();
            self.artwork_previews = None;
            self.artwork_selected = 0;
            self.reload();
        } else if skip {
            self.artwork_queue.pop_front();
            self.artwork_previews = None;
            self.artwork_selected = 0;
        }
    }

    /// Ensure a field preview for `(track_id, release_id)` is loading or loaded.
    /// Spawns a background lookup (fetch the release detail, diff it against the
    /// track's current tags) the first time a candidate is highlighted in the
    /// song-data picker; cached afterwards so re-highlighting is instant.
    pub(crate) fn ensure_metadata_preview(
        &mut self,
        track_id: Id,
        release_id: &str,
        overwrite: bool,
        ctx: &egui::Context,
    ) {
        let key = (track_id, release_id.to_string(), overwrite);
        if self.preview_cache.contains_key(&key) || self.preview_inflight.contains(&key) {
            return;
        }
        let token = self.discogs_token();
        if token.trim().is_empty() {
            return;
        }
        self.preview_inflight.insert(key);
        let release_id = release_id.to_string();
        let db_path = self.db_path.clone();
        let tx = self.preview_tx.clone();
        let ctx = ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung");
            // Open the catalog first so the fetch can go cache-first (reusing a
            // release already pulled, sparing a rate-limited round trip).
            let resolved = Catalog::open(&db_path).and_then(|catalog| {
                let detail =
                    catalog.release_cached_or(&release_id, || client.fetch_release(&release_id))?;
                let tags = catalog
                    .get_track(track_id)
                    .map(|t| t.tags)
                    .unwrap_or_default();
                Ok((detail, tags))
            });
            let (fills, detail) = match resolved {
                Ok((detail, tags)) => {
                    let fills = detail
                        .proposed_fills(&tags, overwrite)
                        .into_iter()
                        .map(|f| (f.field.label().to_string(), f.value))
                        .collect();
                    (fills, Some(detail))
                }
                Err(_) => (Vec::new(), None),
            };
            let _ = tx.send(PreviewMsg {
                track_id,
                release_id,
                overwrite,
                fills,
                detail,
            });
            ctx.request_repaint();
        });
    }

    /// Drain finished field previews into the caches.
    pub(crate) fn poll_metadata_preview(&mut self) {
        while let Ok(msg) = self.preview_rx.try_recv() {
            let key = (msg.track_id, msg.release_id.clone(), msg.overwrite);
            self.preview_inflight.remove(&key);
            if let Some(detail) = msg.detail {
                self.release_detail_cache.insert(msg.release_id, detail);
            }
            self.preview_cache.insert(key, msg.fills);
        }
    }

    /// Kick off the save for the chosen candidate on a background thread: it
    /// downloads the full-resolution image from the Discogs CDN (the slow part,
    /// ~1–5 s) and writes it plus the already-fetched thumbnail to the catalog.
    /// Falls back to the thumbnail bytes if the full image can't be fetched, so
    /// we always store something. The UI stays responsive and shows a spinner
    /// on the Save button; `poll_artwork_save` advances the queue once the
    /// thread reports the id back. Does nothing if a save is already in flight.
    pub(crate) fn save_selected_artwork(&mut self, ctx: &egui::Context, selected: usize) {
        if self.artwork_saving {
            return;
        }
        let Some(choices) = self.artwork_queue.front() else {
            return;
        };
        let Some(c) = choices.candidates.get(selected) else {
            return;
        };
        let track_id = choices.id;

        // Clone everything the worker needs so it owns its data (no borrow of
        // self / the queue across the thread boundary).
        let token = self.discogs_token();
        let release_id = c.release_id.clone();
        let thumb_url = c.thumb_url.clone();
        let cover_image_url = c.cover_image_url.clone();
        let thumb_png = c.thumb_png.clone();
        let db_path = self.db_path.clone();
        let tx = self.art_save_tx.clone();
        let ctx = ctx.clone();
        // "Fetch song data" run → also fill missing tag fields; "Fetch artwork"
        // run → cover only, leave tags untouched.
        let enrich = self.artwork_enrich;
        // Whether to copy this cover onto the track's album-mates, and whether that
        // includes overwriting mates that already have their own art.
        let apply_album = self.artwork_apply_album;
        let album_overwrite = self.artwork_album_overwrite;
        // Whether the song-data write replaces existing values or only fills empties.
        let overwrite = self.artwork_overwrite;
        // Song-data run: whether to also write the cover. The artwork run always
        // writes it (that's its whole purpose); only enrich makes it optional.
        let set_cover = !enrich || self.artwork_set_cover;

        self.artwork_saving = true;
        thread::spawn(move || {
            // Album-mates this cover gets propagated onto (filled below); reported
            // back so the UI can drop their stale thumbnails.
            let mut also: Vec<Id> = Vec::new();
            // One client serves both the full-res image and the release-detail
            // metadata fetch below; absent only when no token is configured.
            let client = if token.trim().is_empty() {
                None
            } else {
                Some(discogs::Client::new(
                    token,
                    "Ordnung/0.1 +https://github.com/ordnung-dj/ordnung",
                ))
            };

            // Resolve the full-resolution image — skipped entirely when the cover
            // isn't being written (a song-data run that's leaving the existing art
            // in place), so we don't spend a rate-limited request on bytes we drop.
            let full_bytes = if set_cover {
                let full = client.as_ref().and_then(|c| {
                    let url = if cover_image_url.is_empty() {
                        &thumb_url
                    } else {
                        &cover_image_url
                    };
                    c.fetch_full(url)
                });
                Some(full.unwrap_or_else(|| thumb_png.clone()))
            } else {
                None
            };

            if let Ok(catalog) = Catalog::open(&db_path) {
                // Write the cover only when asked. In a song-data run the user may
                // be enriching tags on a track that already has art they want kept.
                if let Some(full_bytes) = &full_bytes {
                    let _ = catalog.set_external_artwork(
                        track_id,
                        "discogs",
                        Some(&release_id),
                        Some(&thumb_url),
                        Some(&thumb_png),
                        Some(full_bytes),
                    );

                    // Dress the rest of the album: give the same cover to its
                    // album-mates. In "overwrite" mode every mate is targeted and the
                    // fetched art is flagged to supersede any embedded cover (so it
                    // shows and exports); otherwise only cover-less mates are filled.
                    // Either way they point at the same release, so a later re-fetch
                    // still works. The touched ids ride back so the UI can refresh them.
                    if apply_album {
                        let siblings = if album_overwrite {
                            catalog.album_siblings(track_id)
                        } else {
                            catalog.album_siblings_missing_art(track_id)
                        };
                        if let Ok(siblings) = siblings {
                            for &sib in &siblings {
                                let _ = catalog.set_external_artwork(
                                    sib,
                                    "discogs",
                                    Some(&release_id),
                                    Some(&thumb_url),
                                    Some(&thumb_png),
                                    Some(full_bytes),
                                );
                                // Make the fetched art win over any embedded cover only
                                // when the user asked to replace existing ones.
                                let _ = catalog.set_prefer_external_artwork(sib, album_overwrite);
                            }
                            also = siblings;
                        }
                    }
                }

                // Now that the user has committed to this specific release, fill
                // in any album-level tag fields the track is still missing
                // (genre/style, label, catalog #, year, country, album, date).
                // Catalog only — source files are untouched; non-destructive, so
                // existing values are kept. Failures here don't block the cover.
                // Only on a "Fetch song data" run — the artwork button is harmless.
                if let (true, Some(client)) = (enrich, &client) {
                    // Cache-first so committing several tracks from the same release
                    // (or a re-edit) doesn't re-fetch its immutable detail each time.
                    let rel = catalog
                        .release_cached_or(&release_id, || client.fetch_release(&release_id));
                    if let (Ok(rel), Ok(track)) = (rel, catalog.get_track(track_id)) {
                        let mut tags = track.tags;
                        if rel.apply_to_tags(&mut tags, overwrite) > 0 {
                            let _ = catalog.update_tags(track_id, &tags);
                        }
                    }
                    // The user committed to a release for this track — mark it
                    // fetched so a later run won't re-present it (even if Discogs
                    // couldn't fill every field). Re-runnable via "Edit release…".
                    let _ = catalog.mark_metadata_fetched(track_id);
                }
            }
            let _ = tx.send(ArtSaveDone { id: track_id, also });
            ctx.request_repaint();
        });
    }

    /// Drain finished background artwork saves. For each, advance past the saved
    /// track (it's the front of the queue — the picker is locked while saving),
    /// drop its cached textures so the new art re-decodes, and refresh the table.
    pub(crate) fn poll_artwork_save(&mut self) {
        while let Ok(done) = self.art_save_rx.try_recv() {
            self.artwork_saving = false;
            if self.artwork_queue.front().map(|c| c.id) == Some(done.id) {
                self.artwork_queue.pop_front();
                self.artwork_previews = None;
                self.artwork_selected = 0;
                // The front track's mate counts are stale once the queue advances.
                self.artwork_album_count = None;
            }
            // Drop cached textures for the picked track and every album-mate the
            // cover was copied onto, so each re-decodes to the new art next render.
            for id in std::iter::once(done.id).chain(done.also) {
                self.cover_cache.remove(&id);
                self.cover_full_cache.remove(&id);
                self.cover_inflight.remove(&id);
            }
            self.reload();
        }
    }
}
