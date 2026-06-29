//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// Re-read the selected track's full Tags from the catalog. Call after any
    /// change that may have touched its row (rename / convert / rescan).
    pub(crate) fn refresh_selected(&mut self) {
        let (track, has_art) = match self.selected {
            Some(id) => match Catalog::open(&self.db_path) {
                Ok(c) => (
                    c.get_track(id).ok(),
                    c.get_external_artwork_full(id).ok().flatten().is_some(),
                ),
                Err(_) => (None, false),
            },
            None => (None, false),
        };
        self.selected_track = track;
        // Reset the inspector edit buffers to match the freshly-loaded track so
        // they never show a previous selection's values, and the dirty state
        // starts clean.
        self.tag_edit = self
            .selected_track
            .as_ref()
            .map(|t| TagEdit::from_tags(&t.tags))
            .unwrap_or_default();
        self.tag_edit_saved = self.tag_edit.clone();
        self.selected_has_external_art = has_art;
    }

    /// Set the primary (inspector) row, reloading the inspector only when it
    /// actually changes. `None` clears the inspector.
    pub(crate) fn set_primary(&mut self, id: Option<Id>) {
        if self.selected != id {
            self.selected = id;
            self.refresh_selected();
        }
    }

    /// Resolve a modifier-aware click into the selection set. Plain click selects
    /// just `id`; Cmd toggles it; Shift extends a contiguous range (in visible row
    /// order) from the anchor. Keeps `selected` (the inspector's primary) pointing
    /// at a selected row.
    pub(crate) fn apply_click_selection(&mut self, id: Id, mods: egui::Modifiers) {
        if mods.command || mods.mac_cmd {
            if !self.selection.remove(&id) {
                self.selection.insert(id);
            }
            self.select_anchor = Some(id);
        } else if mods.shift {
            let anchor = self.select_anchor.or(self.selected);
            let range = anchor.and_then(|a| {
                let ai = self.rows.iter().position(|r| r.id == a)?;
                let bi = self.rows.iter().position(|r| r.id == id)?;
                let (lo, hi) = if ai <= bi { (ai, bi) } else { (bi, ai) };
                Some(
                    self.rows[lo..=hi]
                        .iter()
                        .map(|r| r.id)
                        .collect::<HashSet<_>>(),
                )
            });
            self.selection = range.unwrap_or_else(|| std::iter::once(id).collect());
            // Anchor stays put so the user can grow/shrink the same range.
        } else {
            self.selection.clear();
            self.selection.insert(id);
            self.select_anchor = Some(id);
        }
        // Primary follows the clicked row when it stayed selected; a Cmd-click that
        // deselected the active row hands primary to any remaining member.
        if self.selection.contains(&id) {
            self.set_primary(Some(id));
        } else {
            let next = self.selection.iter().copied().next();
            self.set_primary(next);
        }
    }

    /// Drop the whole selection and the inspector's primary row. Called when the
    /// user clicks the empty space below the rows or presses Escape, so nothing
    /// stays perma-highlighted once they click away from the songs.
    pub(crate) fn clear_selection(&mut self) {
        self.selection.clear();
        self.select_anchor = None;
        self.set_primary(None);
    }

    /// True when an open popup or modal should own the Escape key this frame, so
    /// the table doesn't steal it to clear the selection out from under them.
    fn escape_consumed_elsewhere(&self) -> bool {
        self.column_menu.is_some()
            || self.col_filter_open.is_some()
            || self.convert_modal.is_some()
            || self.batch_convert.is_some()
            || self.confirm_delete.is_some()
            || self.settings_open
            || !self.artwork_queue.is_empty()
    }

    /// Keep only the rows passing every active per-column filter. Each filter is
    /// a case-insensitive substring match against that column's displayed text
    /// (`TrackRow::filter_text`); multiple active columns are AND-ed. No active
    /// filters returns `rows` untouched.
    pub(crate) fn apply_col_filters(&self, rows: Vec<TrackRow>) -> Vec<TrackRow> {
        if self.col_filters.is_empty() {
            return rows;
        }
        // Lower-case each needle once rather than per row.
        let needles: Vec<(TableColumn, String)> = self
            .col_filters
            .iter()
            .filter(|(_, v)| !v.trim().is_empty())
            .map(|(c, v)| (*c, v.trim().to_lowercase()))
            .collect();
        if needles.is_empty() {
            return rows;
        }
        rows.into_iter()
            .filter(|r| {
                // Different columns AND together; within one column, a comma lets
                // several values OR together (e.g. the Quality column's "~320?,
                // lossy" to show every likely-transcoded copy in one filter).
                needles.iter().all(|(col, n)| {
                    let hay = r.filter_text(*col).to_lowercase();
                    let mut alts = n.split(',').map(str::trim).filter(|s| !s.is_empty());
                    let mut any = false;
                    let matched = alts.any(|alt| {
                        any = true;
                        hay.contains(alt)
                    });
                    // An all-comma/blank needle has no real alternatives → don't filter.
                    !any || matched
                })
            })
            .collect()
    }

    /// Sort `self.rows` in place per `self.sort`. Text columns sort
    /// case-insensitively; the numeric/key columns sort by their raw value (so
    /// `None`/"—" clusters at one end). The sort is stable, so within equal keys
    /// rows keep their prior (catalog/playlist) order. A `None` sort is a no-op,
    /// leaving the natural order untouched.
    pub(crate) fn apply_sort(&mut self) {
        let Some((col, asc)) = self.sort else { return };
        use std::cmp::Ordering;
        let ci = |a: &str, b: &str| a.to_lowercase().cmp(&b.to_lowercase());
        let fcmp = |a: Option<f32>, b: Option<f32>| match (a, b) {
            (Some(x), Some(y)) => x.partial_cmp(&y).unwrap_or(Ordering::Equal),
            (Some(_), None) => Ordering::Less,
            (None, Some(_)) => Ordering::Greater,
            (None, None) => Ordering::Equal,
        };
        self.rows.sort_by(|a, b| {
            let ord = match col {
                SortColumn::Artist => ci(&a.artist, &b.artist),
                SortColumn::Title => ci(&a.title, &b.title),
                SortColumn::Album => ci(&a.album, &b.album),
                SortColumn::Genre => ci(&a.genre, &b.genre),
                SortColumn::Format => ci(&a.format_label, &b.format_label),
                SortColumn::Duration => a.dur_ms.cmp(&b.dur_ms),
                SortColumn::Bitrate => a.bitrate_val.cmp(&b.bitrate_val),
                SortColumn::Notes => ci(&a.notes, &b.notes),
                SortColumn::Added => a.added_at.cmp(&b.added_at),
                SortColumn::Key => a.key_sort.cmp(&b.key_sort),
                SortColumn::Bpm => fcmp(a.bpm_val, b.bpm_val),
                // Severity rank: descending floats likely-transcodes to the top.
                SortColumn::Quality => a.quality_sort.cmp(&b.quality_sort),
            };
            if asc {
                ord
            } else {
                ord.reverse()
            }
        });
    }

    /// The default sort to seed `self.sort` with on launch, resolved from the
    /// saved config. `None` (the shipped default) leaves the natural
    /// catalog/playlist order untouched; an unknown or unsortable saved key
    /// falls back to `None` too.
    pub(crate) fn default_sort(&self) -> Option<(SortColumn, bool)> {
        let key = self.config.default_sort.trim();
        if key.is_empty() {
            return None;
        }
        let col = TableColumn::from_key(key)?.sort_column()?;
        Some((col, self.config.default_sort_ascending))
    }

    /// Handle a click on a sortable header: the same column cycles
    /// ascending → descending → unsorted; a different column starts ascending.
    /// Re-sorts the current rows without hitting the catalog.
    pub(crate) fn toggle_sort(&mut self, col: SortColumn) {
        self.sort = match self.sort {
            Some((c, true)) if c == col => Some((col, false)),
            Some((c, false)) if c == col => None,
            _ => Some((col, true)),
        };
        self.reload();
    }

    /// Rebuild `column_order`/`hidden_columns` from the saved config, tolerating a
    /// stale layout: unknown keys are dropped and any column missing from the
    /// saved order is appended, so the table always shows every column this build
    /// knows about. An empty or absent config yields the default order with
    /// nothing hidden.
    pub(crate) fn load_column_layout(&mut self) {
        let mut order: Vec<TableColumn> = self
            .config
            .column_order
            .iter()
            .filter_map(|k| TableColumn::from_key(k))
            .collect();
        // Append any column not already present (config from an older build, or a
        // column added since it was written) so the layout scales with the build.
        for c in TableColumn::DEFAULT_ORDER {
            if !order.contains(&c) {
                order.push(c);
            }
        }
        self.column_order = order;
        self.hidden_columns = self
            .config
            .hidden_columns
            .iter()
            .filter_map(|k| TableColumn::from_key(k))
            .collect();
        self.column_widths = self
            .config
            .column_widths
            .iter()
            .filter_map(|(k, &w)| TableColumn::from_key(k).map(|c| (c, w)))
            .collect();
    }

    /// Persist the user's track-table column widths to config. Called after a
    /// resize settles (the drag ended), so a single drag writes the TOML once
    /// rather than every frame. Save failure is non-fatal.
    pub(crate) fn save_column_widths(&mut self) {
        self.config.column_widths = self
            .column_widths
            .iter()
            .map(|(c, &w)| (c.key().to_string(), w))
            .collect();
        let _ = self.config.save();
    }

    /// Persist the current column layout to config after a reorder or show/hide.
    /// Save failure is non-fatal — it only affects whether the layout survives the
    /// next launch.
    pub(crate) fn save_column_layout(&mut self) {
        self.config.column_order = self
            .column_order
            .iter()
            .map(|c| c.key().to_string())
            .collect();
        self.config.hidden_columns = self
            .hidden_columns
            .iter()
            .map(|c| c.key().to_string())
            .collect();
        let _ = self.config.save();
    }

    /// The column reorder popup, opened by right-clicking any table header. Lists
    /// every column (in current order); drag the ⠿ handle to reorder and toggle
    /// the checkbox to show/hide. Changes persist immediately. Closed via its X,
    /// Escape, or "Reset to default".
    pub(crate) fn show_column_menu(&mut self, ctx: &egui::Context) {
        let Some(pos) = self.column_menu else { return };
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.column_menu = None;
            return;
        }
        // Work on clones so the window closure can borrow these freely; write back
        // to `self` (and persist) only if something actually changed.
        let mut order = self.column_order.clone();
        let mut hidden = self.hidden_columns.clone();
        let mut open = true;
        let mut changed = false;
        let mut reset = false;

        egui::Window::new("Columns")
            .id(egui::Id::new("column_reorder_popup"))
            .collapsible(false)
            .open(&mut open)
            .default_pos(pos)
            .auto_sized()
            .show(ctx, |ui| {
                // Cap the content width to the narrow column list. Without this the
                // full-width `separator()` below requests all available width and
                // inflates the whole window far past the short labels.
                ui.set_max_width(150.0);
                ui.label(egui::RichText::new("Drag · toggle").weak().small());
                ui.add_space(6.0);

                let visible_count = order.iter().filter(|c| !hidden.contains(c)).count();
                let mut to: Option<usize> = None;
                let frame = egui::Frame::default().inner_margin(2.0);
                let (_, dropped) = ui.dnd_drop_zone::<usize, ()>(frame, |ui| {
                    for (idx, &col) in order.iter().enumerate() {
                        // Lay out each row by hand so that *only* the ⠿ handle is a
                        // drag source — making the whole row (checkbox included)
                        // draggable meant a press on the checkbox started a drag
                        // instead of toggling visibility.
                        let row = ui.horizontal(|ui| {
                            ui.dnd_drag_source(
                                egui::Id::new(("col_reorder_item", col)),
                                idx,
                                |ui| {
                                    let handle = ui.label(egui::RichText::new("⠿").weak());
                                    // A grab cursor on the handle signals it's the
                                    // draggable part, not the label.
                                    handle.on_hover_cursor(egui::CursorIcon::Grab);
                                },
                            );
                            let mut vis = !hidden.contains(&col);
                            // Never let the user hide the last visible column — an
                            // empty table has no header to right-click to reopen
                            // this menu.
                            let enabled = !vis || visible_count > 1;
                            if ui
                                .add_enabled(enabled, egui::Checkbox::new(&mut vis, col.label()))
                                .changed()
                            {
                                if vis {
                                    hidden.remove(&col);
                                } else {
                                    hidden.insert(col);
                                }
                                changed = true;
                            }
                        });
                        // Track which gap the pointer is over so a drop lands there.
                        // Use the whole row's rect so the entire strip is a valid
                        // drop target, not just the narrow handle.
                        if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                            if row.response.rect.contains(p) {
                                to = Some(if p.y < row.response.rect.center().y {
                                    idx
                                } else {
                                    idx + 1
                                });
                            }
                        }
                    }
                });
                if let Some(dragged) = dropped {
                    let from = *dragged;
                    if let Some(mut t) = to {
                        // Removing the source first shifts later indices down by one.
                        if t > from {
                            t -= 1;
                        }
                        let item = order.remove(from);
                        order.insert(t.min(order.len()), item);
                        changed = true;
                    }
                }

                ui.add_space(6.0);
                ui.separator();
                if ui.button("Reset").clicked() {
                    reset = true;
                }
            });

        if reset {
            order = TableColumn::DEFAULT_ORDER.to_vec();
            hidden.clear();
            changed = true;
            // Also drop any saved widths so reset restores the default sizing;
            // the table clears egui_extras' live state next frame so the change
            // shows immediately rather than after a rebuild.
            self.column_widths.clear();
            self.column_widths_dirty = false;
            self.reset_column_widths = true;
            self.save_column_widths();
        }
        if changed {
            self.column_order = order;
            self.hidden_columns = hidden;
            self.save_column_layout();
        }
        if !open {
            self.column_menu = None;
        }
    }

    /// Draws the per-column filter search bar opened by double-clicking a header.
    /// A tiny anchored bar with one text field; typing live-filters the table by
    /// substring match against that column's values. Closes on Escape, the × /
    /// close button, or a click outside the bar — the filter itself persists
    /// (the header keeps its ⌕ marker) until cleared with ×.
    pub(crate) fn show_col_filter_popup(&mut self, ctx: &egui::Context) {
        let Some(popup) = self.col_filter_open.take() else {
            return;
        };
        let ColFilterPopup { col, pos, focus } = popup;
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            return; // dropped (already `take`n) — bar closes, filter stays.
        }
        // Edit a clone of the column's current filter text; write back only if it
        // changed, mirroring `show_column_menu`'s clone-then-commit pattern.
        let mut text = self.col_filters.get(&col).cloned().unwrap_or_default();
        let mut open = true;
        let mut changed = false;
        let mut clear = false;
        // Remember the bar's rect so a click anywhere else this frame closes it.
        let mut bar_rect = egui::Rect::NOTHING;

        egui::Window::new(format!("Filter · {}", col.label()))
            .id(egui::Id::new("col_filter_popup"))
            .title_bar(false)
            .collapsible(false)
            .resizable(false)
            .default_pos(pos)
            .show(ctx, |ui| {
                // The Quality column filters by verdict, so offer one-click presets
                // (and the legend) instead of making the user remember the chip
                // labels. The free-text field below still works for anything else.
                if col == TableColumn::Quality {
                    quality_legend_ui(ui);
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .button("⚠ Likely transcoded")
                            .on_hover_note("Show only ~320? and lossy copies")
                            .clicked()
                        {
                            text = "~320?, lossy".into();
                            changed = true;
                        }
                        if ui.button("lossy").clicked() {
                            text = "lossy".into();
                            changed = true;
                        }
                        if ui.button("~320?").clicked() {
                            text = "~320?".into();
                            changed = true;
                        }
                        if ui.button("clean").clicked() {
                            text = "clean".into();
                            changed = true;
                        }
                    });
                    ui.add_space(2.0);
                }
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!("Filter {}", col.label()))
                            .small()
                            .weak(),
                    );
                    let edit = ui.add(
                        egui::TextEdit::singleline(&mut text)
                            .hint_text("contains…")
                            .desired_width(150.0),
                    );
                    // Grab the keyboard the frame the bar opens so the user can
                    // type immediately; Enter dismisses the bar (filter stays).
                    if focus {
                        edit.request_focus();
                    }
                    if edit.changed() {
                        changed = true;
                    }
                    if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        open = false;
                    }
                    // × clears this column's filter and closes the bar.
                    if ui.small_button("×").clicked() {
                        clear = true;
                    }
                });
                // Keep the rect inclusive of everything drawn this frame.
                bar_rect = ui.min_rect();
            });

        // A press outside the bar dismisses it (the reorder menu uses Escape /
        // its own close box; here we also honour click-away for a lightweight feel).
        // Skip on the opening frame (`focus`), whose own header double-click press
        // lands outside the just-created bar and would close it instantly.
        let clicked_away = !focus
            && ctx.input(|i| i.pointer.any_pressed())
            && ctx
                .input(|i| i.pointer.interact_pos())
                .is_none_or(|p| !bar_rect.contains(p));

        if clear {
            self.col_filters.remove(&col);
            self.reload();
            return; // bar closed.
        }
        if changed {
            if text.trim().is_empty() {
                self.col_filters.remove(&col);
            } else {
                self.col_filters.insert(col, text);
            }
            self.reload();
        }
        // Re-arm the open state for next frame unless something closed the bar.
        if open && !clicked_away {
            self.col_filter_open = Some(ColFilterPopup {
                col,
                pos,
                focus: false,
            });
        }
    }

    /// Draws the catalog table. Returns the file paths to hand to a native
    /// drag-out when the user started dragging a row this frame (`None` otherwise);
    /// the caller fires the drag after the panel closure so no borrows are live
    /// during AppKit's nested drag loop.
    pub(crate) fn draw_table(&mut self, ui: &mut egui::Ui) -> Option<Vec<PathBuf>> {
        let mut open_convert_for: Option<Id> = None;
        // A pending jump-to-row (e.g. from the vinyl grid's "in catalog" badge):
        // taken once so the table scrolls to it this frame and not forever after.
        // The table body is virtualized (only visible rows are built), so an
        // off-screen target's row closure never runs — we must steer the scroll
        // at the builder level via `scroll_to_row` using the row's index.
        let scroll_to_index = self
            .scroll_to_track
            .take()
            .and_then(|id| self.rows.iter().position(|r| r.id == id));
        // The row that received a plain (non-drag) click this frame. Selection is
        // resolved after the table with modifiers; a drag sets a DnD payload
        // instead (consumed by the playlist sidebar or the native drag-out).
        let mut clicked_id: Option<Id> = None;
        // Loads happen inside the row closure; collect (id, needs_load) into a
        // queue so we don't need &mut self inside the closure.
        let mut needs_cover_load: Vec<Id> = Vec::new();
        // Set when the user clicks a cover's play/stop overlay; acted on after the
        // table (playback control needs `&mut self`, unavailable in the closure).
        let mut preview_request: Option<(Id, PathBuf)> = None;
        // Snapshot of which row is loading/playing so the overlay can render the
        // right glyph without borrowing `self.audio` inside the closure.
        let audio_enabled = self.audio.is_some();
        let preview_state = |id: Id| -> PlayState {
            self.audio
                .as_ref()
                .map_or(PlayState::Idle, |a| a.state_for(id))
        };
        let ctx_clone = ui.ctx().clone();
        // Right-click context-menu state. `menu_action` is filled inside the row
        // closure and applied after the table. The snapshots let the menu build
        // its "Add to playlist" submenu and decide which playlist-only items to
        // show without borrowing `self` mutably inside the closure.
        let mut menu_action: Option<TrackMenuAction> = None;
        // The track ids of a ⌥-drag that started this frame, set in a row closure
        // below. Resolved to source-file paths and returned at the end so the
        // caller can begin the native macOS drag-out *after* these closures (and
        // their `self.rows` borrows) have dropped.
        let mut native_drag_ids: Option<Vec<Id>> = None;
        // Cmd/Ctrl+C copies the selected rows the same way the "Copy for Soulseek"
        // context-menu item does (Artist – Title, one per line). On native eframe
        // the OS copy shortcut arrives as an `Event::Copy`, not a Key::C press, so
        // we look for that. Skip when a text field has focus so editing a tag still
        // copies the text, not the rows.
        let copy_pressed = ctx_clone.input_mut(|i| {
            let hit = i.events.iter().any(|e| matches!(e, egui::Event::Copy));
            i.events.retain(|e| !matches!(e, egui::Event::Copy));
            hit
        });
        if !ctx_clone.wants_keyboard_input() && copy_pressed {
            let ids: Vec<Id> = if self.selection.is_empty() {
                self.selected.into_iter().collect()
            } else {
                self.selection.iter().copied().collect()
            };
            if !ids.is_empty() {
                menu_action = Some(TrackMenuAction::CopyForSoulseek(ids));
            }
        }
        let menu_playlists: Vec<(Id, String)> = self
            .playlists
            .iter()
            .filter(|p| !p.is_folder)
            .map(|p| (p.id, p.name.clone()))
            .collect();
        let menu_playlist_view = match self.view {
            LibraryView::Playlist(pid) => Some(pid),
            LibraryView::Library
            | LibraryView::RecentlyAdded
            | LibraryView::Duplicates
            | LibraryView::Missing
            | LibraryView::Vinyl => None,
        };
        // Reordering ("Move to top/bottom") rewrites the whole playlist order, so
        // only offer it on an unfiltered view where the visible rows are the full
        // list — never on a filtered subset that would drop the hidden tracks. A
        // per-column filter narrows the view just like the global search, so it
        // counts as "filtered" too.
        let menu_unfiltered = self.filter.trim().is_empty() && self.col_filters.is_empty();
        // Header-sort state. `cur_sort` is snapshotted so the header closure can
        // draw the active-column arrow without borrowing `self`; a click is
        // recorded into `header_clicked` and applied after the table.
        let cur_sort = self.sort;
        let mut header_clicked: Option<SortColumn> = None;
        // Set when the Waveform header's mode toggle is clicked: flip the inline
        // waveform colouring between energy and frequency after the table.
        let mut toggle_waveform_mode = false;
        // Which columns have an active per-column filter — snapshotted so the
        // header closure can mark them (a ⌕ glyph) without borrowing `self`.
        let filtered_cols: HashSet<TableColumn> = self.col_filters.keys().copied().collect();
        // Set when a header is right-clicked: the screen position to anchor the
        // column reorder popup. Applied to `self.column_menu` after the table.
        let mut open_col_menu: Option<egui::Pos2> = None;
        // Set when a header is double-clicked: the column to filter and the screen
        // position to anchor its search bar. Applied to `self.col_filter_open`.
        let mut open_col_filter: Option<(TableColumn, egui::Pos2)> = None;
        // The visible columns in order — the hidden ones are filtered out here so
        // the table builder, header, and body all iterate the same sequence. The
        // menu won't let the user hide the last column, but a hand-edited config
        // could; fall back to the full order so there's always a header to
        // right-click the menu back open from.
        let mut order: Vec<TableColumn> = self
            .column_order
            .iter()
            .copied()
            .filter(|c| !self.hidden_columns.contains(c))
            .collect();
        if order.is_empty() {
            order = self.column_order.clone();
        }
        // The whole multi-selection in visible order, computed once per frame. A
        // drag from any selected row carries this exact list; precomputing avoids
        // re-scanning every row for every visible selected row each frame.
        let selected_ordered: Vec<Id> = self
            .rows
            .iter()
            .filter(|x| self.selection.contains(&x.id))
            .map(|x| x.id)
            .collect();
        const ROW_H: f32 = 28.0;
        const COVER_PX: f32 = 22.0;
        // Color mode for the inline Waveform column, read once per frame (Copy, so
        // capturing it in the row closures doesn't borrow `self`).
        let waveform_color_mode =
            config::WaveformColorMode::from_key(&self.config.waveform_color_mode);

        // Playlist views show a leading order-index gutter and support
        // drag-to-reorder. The index column is structural in every playlist view
        // (so the persisted per-column widths under the playlist id_salt stay
        // aligned), but the live reorder affordance — the insertion line and the
        // drop — only engages when the visible rows ARE the playlist in stored
        // order: a real playlist, no sort override, and no active filter (the same
        // gate as the "Move to top/bottom" menu items).
        let playlist_pid = match self.view {
            LibraryView::Playlist(pid) => Some(pid),
            LibraryView::Library
            | LibraryView::RecentlyAdded
            | LibraryView::Duplicates
            | LibraryView::Missing
            | LibraryView::Vinyl => None,
        };
        let show_index = playlist_pid.is_some();
        let reorderable = playlist_pid.is_some() && self.sort.is_none() && menu_unfiltered;
        // Width of the index gutter, sized to the widest row number.
        let index_w = {
            let digits = self.rows.len().max(1).to_string().len() as f32;
            12.0 + digits * 7.0
        };
        // Per-row vertical bands (full-list index, top y, bottom y) for the
        // visible rows, filled inside the body closure and used after the table to
        // map the cursor onto an insertion slot for the reorder line.
        let mut row_spans: Vec<(usize, f32, f32)> = Vec::new();
        // Screen rect of each visible row, paired with its track id, so a dropped
        // cover image can be mapped onto the row under the cursor (see
        // `handle_file_drop`). Assigned to `self.row_screen_rects` after the table.
        let mut row_rects: Vec<(Id, egui::Rect)> = Vec::new();

        // Each data column's rendered width this frame, read from its header cell
        // rect and reconciled with `self.column_widths` after the table so a
        // user resize is captured and persisted (see below). Cover is fixed-size
        // and excluded.
        let mut observed_widths: Vec<(TableColumn, f32)> = Vec::new();
        // One-shot "Reset to default" signal: clears egui_extras' live widths so
        // the cleared defaults apply this frame (see the builder below).
        let reset_widths = std::mem::take(&mut self.reset_column_widths);

        // Wrap the table in a horizontal scroll area so a wide column layout
        // (or a narrow window) can be scrolled left/right. The trailing
        // `remainder` spacer fills slack when the window is wide, so the
        // horizontal bar only appears once the columns exceed the viewport.
        let scroll_out = egui::ScrollArea::horizontal()
            .id_salt("songs_table_hscroll")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                let mut builder = TableBuilder::new(ui)
                    // One stable id for EVERY view (library and all playlists) so the
                    // persisted per-column widths (saved via egui memory by eframe's
                    // `persistence` feature) are shared globally: resize a column in
                    // one playlist and the same widths apply everywhere. The leading
                    // index gutter is always present as the first column (zero-width
                    // outside playlist views, see below) so the data columns keep the
                    // same positional indices in every view and the stored widths never
                    // shift by one when switching between the library and a playlist.
                    .id_salt("songs_table")
                    .striped(false)
                    .resizable(true)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center));
                // Leading fixed-width gutter for the playlist order index. Always the
                // first column so data-column positions (and their stored widths) line
                // up across every view; collapsed to zero width when there's no index
                // to show, so the library reads as if the gutter weren't there.
                builder = builder.column(Column::exact(if show_index { index_w } else { 0.0 }));
                // Steer a pending jump (from the vinyl grid) to its row. Done at the
                // builder level because the body is virtualized — an off-screen row's
                // closure never runs, so an in-row `scroll_to_me` couldn't reach it.
                if let Some(idx) = scroll_to_index {
                    builder = builder.scroll_to_row(idx, Some(egui::Align::Center));
                }
                // One column per visible entry, in the user's chosen order, then a
                // trailing remainder spacer so the striped rows span the full width.
                for &col in &order {
                    builder = builder.column(col.spec(COVER_PX, self.column_widths.get(&col).copied()));
                }
                builder = builder.column(Column::remainder());
                // After "Reset to default", drop egui_extras' own stored widths so
                // the columns fall back to the (now-cleared) defaults this frame.
                if reset_widths {
                    builder.reset();
                }
                builder
                    .header(22.0, |mut header| {
                        // Each header is clickable to sort by that column (the cover
                        // column isn't sortable) and right-clickable to open the column
                        // reorder popup. The trailing spacer carries no label but is
                        // still a valid right-click target. The file path is
                        // intentionally not a column — it lives in the Info panel and
                        // the right-click menu.
                        // The index gutter header is always emitted to match the
                        // always-present first column; it's empty (and zero-width) when
                        // not a playlist view.
                        header.col(|ui| {
                            if show_index {
                                ui.add(
                                    egui::Label::new(egui::RichText::new("#").weak())
                                        .selectable(false),
                                );
                            }
                        });
                        for &col in &order {
                            header.col(|ui| {
                                // Record the column's actual width (egui_extras
                                // sizes each header cell to its column) so a
                                // resize can be captured and persisted globally.
                                if col != TableColumn::Cover {
                                    observed_widths.push((col, ui.max_rect().width()));
                                }
                                let resp = if col == TableColumn::Waveform {
                                    // The Waveform header carries no sort/title.
                                    // Instead it hosts a small toggle that flips the
                                    // inline waveform's colouring between energy
                                    // (loudness) and frequency (spectrum). A full-cell
                                    // interact underneath keeps right-click → reorder
                                    // working; the button is painted on top for the
                                    // click and shows the *current* mode's glyph.
                                    let resp = ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("hdr", col)),
                                        egui::Sense::click(),
                                    );
                                    let (glyph, tip) = match waveform_color_mode {
                                        config::WaveformColorMode::Energy => (
                                            "▮",
                                            "Energy waveform (loudness) · click for frequency",
                                        ),
                                        config::WaveformColorMode::Spectrum => (
                                            "≋",
                                            "Frequency waveform (spectrum) · click for energy",
                                        ),
                                    };
                                    let cell = ui.max_rect();
                                    let btn_rect = egui::Rect::from_min_size(
                                        egui::pos2(cell.left() + 2.0, cell.center().y - 9.0),
                                        egui::vec2(22.0, 18.0),
                                    );
                                    let btn = ui
                                        .put(btn_rect, egui::Button::new(glyph).small())
                                        .on_hover_text(tip);
                                    if btn.hovered() {
                                        ui.ctx()
                                            .set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if btn.clicked() {
                                        toggle_waveform_mode = true;
                                    }
                                    resp
                                } else {
                                    match col.sort_column() {
                                    // Cover: no label, but the whole cell still opens
                                    // the reorder menu on right-click.
                                    None => ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("hdr", col)),
                                        egui::Sense::click(),
                                    ),
                                    Some(sc) => {
                                        // Append ▲/▼ when this is the active sort column,
                                        // and a ⌕ when a per-column filter is narrowing it.
                                        let arrow = match cur_sort {
                                            Some((c, true)) if c == sc => " ▲",
                                            Some((c, false)) if c == sc => " ▼",
                                            _ => "",
                                        };
                                        let filt = if filtered_cols.contains(&col) {
                                            " ⌕"
                                        } else {
                                            ""
                                        };
                                        // Sense clicks across the whole header cell, not
                                        // just the label glyphs, so anywhere in the column
                                        // header sorts/filters. The label is then painted
                                        // on top (non-interactive) for the text + markers.
                                        let resp = ui.interact(
                                            ui.max_rect(),
                                            ui.id().with(("hdr", col)),
                                            egui::Sense::click(),
                                        );
                                        ui.add(
                                            egui::Label::new(
                                                egui::RichText::new(format!(
                                                    "{}{arrow}{filt}",
                                                    col.label()
                                                ))
                                                .strong()
                                                .color(crate::ui::tokens::color::LABEL_2),
                                            )
                                            .selectable(false),
                                        );
                                        if resp.hovered() {
                                            ui.ctx()
                                                .set_cursor_icon(egui::CursorIcon::PointingHand);
                                        }
                                        // Single click sorts; double click opens the
                                        // filter search bar for this column.
                                        if resp.clicked() {
                                            header_clicked = Some(sc);
                                        }
                                        if resp.double_clicked() {
                                            open_col_filter = Some((
                                                col,
                                                ui.ctx()
                                                    .pointer_interact_pos()
                                                    .unwrap_or_else(|| resp.rect.left_bottom()),
                                            ));
                                        }
                                        resp
                                    }
                                    }
                                };
                                // The Quality header is self-documenting: hovering it
                                // shows the chip legend (what clean / ~320? / lossy / ltd
                                // mean) plus how to act on it.
                                let resp = if col == TableColumn::Quality {
                                    resp.on_hover_ui(|ui| {
                                        quality_legend_ui(ui);
                                        ui.add_space(4.0);
                                        ui.label(
                                        crate::ui::hover::note(
                                            "Click to sort worst-first · double-click to filter",
                                        )
                                        .weak(),
                                    );
                                    })
                                } else {
                                    resp
                                };
                                if resp.secondary_clicked() {
                                    open_col_menu = Some(
                                        ui.ctx()
                                            .pointer_interact_pos()
                                            .unwrap_or_else(|| resp.rect.left_bottom()),
                                    );
                                }
                            });
                        }
                        // Trailing spacer header: right-click the empty area past the
                        // last column to open the reorder menu too.
                        header.col(|ui| {
                            let resp = ui.interact(
                                ui.max_rect(),
                                ui.id().with("hdr-spacer"),
                                egui::Sense::click(),
                            );
                            if resp.secondary_clicked() {
                                open_col_menu = Some(
                                    ui.ctx()
                                        .pointer_interact_pos()
                                        .unwrap_or_else(|| resp.rect.left_bottom()),
                                );
                            }
                        });
                    })
                    .body(|body| {
                        body.rows(ROW_H, self.rows.len(), |mut row| {
                            let idx = row.index();
                            let r = &self.rows[idx];
                            // Every member of the multi-selection gets the highlighted row
                            // background; the primary row (drives the inspector) is also
                            // bolded so it stands out within a multi-selection.
                            let is_sel = self.selection.contains(&r.id);
                            let is_primary = self.selected == Some(r.id);
                            row.set_selected(is_sel);

                            // Plain drag = in-app egui drag (drop onto a sidebar playlist);
                            // ⌥-drag = native macOS drag-out to rekordbox/Finder. Decided
                            // up front from the live modifier so the two never both fire.
                            // What a drag from this row carries: the whole selection (in
                            // visible order) when this row is part of it, else just this row.
                            let drag_ids: Vec<Id> = if is_sel {
                                selected_ordered.clone()
                            } else {
                                vec![r.id]
                            };
                            // Decide drag mode up front from the live modifier: a plain
                            // drag stays in-app (egui DnD → sidebar playlist / reorder),
                            // a ⌥-drag becomes the native macOS drag-out. They must never
                            // both fire, so the ⌥ branch suppresses the egui payload.
                            let alt_drag = ctx_clone.input(|i| i.modifiers.alt);

                            let mut clicked = false;
                            let mut dbl = false;
                            let mut drag = false;

                            // Playlist order gutter: a tiny right-aligned 1-based row
                            // number. Drawn first so it sits at the far left, and its
                            // cell rect is recorded as this row's vertical band for the
                            // reorder hit-test after the table.
                            // Always emit the leading gutter cell to match the
                            // always-present first column; it draws the row number only
                            // in playlist views (and is zero-width otherwise).
                            let (rect, _) = row.col(|ui| {
                                if show_index {
                                    let r = ui.max_rect();
                                    ui.painter().text(
                                        egui::pos2(r.right() - 5.0, r.center().y),
                                        egui::Align2::RIGHT_CENTER,
                                        format!("{}", idx + 1),
                                        egui::FontId::proportional(11.0),
                                        ui.visuals().weak_text_color(),
                                    );
                                }
                            });
                            if show_index && reorderable {
                                row_spans.push((idx, rect.top(), rect.bottom()));
                            }

                            // Draw each visible column in the user's chosen order. Each
                            // `row.col` closure is created and consumed within its own
                            // loop iteration, so they share the row's click/drag state
                            // without overlapping mutable borrows.
                            for &col in &order {
                                if col == TableColumn::Cover {
                                    // Cover cell (click + drag like the rest of the row).
                                    row.col(|ui| {
                                        let has_any_cover = r.has_cover || r.has_external_cover;
                                        let tex = match self.cover_cache.get(&r.id) {
                                            Some(ThumbState::Ready(Some(handle))) => {
                                                Some(handle.clone())
                                            }
                                            _ => None,
                                        };
                                        let resp = match tex {
                                            Some(handle) => ui.add(
                                                egui::Image::new(&handle)
                                                    .fit_to_exact_size(egui::vec2(
                                                        COVER_PX, COVER_PX,
                                                    ))
                                                    .sense(egui::Sense::click_and_drag()),
                                            ),
                                            None => {
                                                if !self.cover_cache.contains_key(&r.id)
                                                    && has_any_cover
                                                {
                                                    needs_cover_load.push(r.id);
                                                }
                                                let (rect, resp) = ui.allocate_exact_size(
                                                    egui::vec2(COVER_PX, COVER_PX),
                                                    egui::Sense::click_and_drag(),
                                                );
                                                ui.painter().rect_filled(
                                                    rect,
                                                    egui::Rounding::same(3.0),
                                                    egui::Color32::from_gray(40),
                                                );
                                                if has_any_cover {
                                                    ui.painter().text(
                                                        rect.center(),
                                                        egui::Align2::CENTER_CENTER,
                                                        "…",
                                                        egui::FontId::proportional(14.0),
                                                        egui::Color32::from_gray(120),
                                                    );
                                                }
                                                resp
                                            }
                                        };
                                        // Play overlay: a play/pause disc centred on the cover.
                                        // Clicking it loads the track into the bottom now-playing
                                        // bar (or pauses it if it's already current). Kept hidden
                                        // so the artwork is unobstructed, and only revealed while
                                        // the pointer is over this cover or while this track is
                                        // loading/playing (so the pause control stays reachable).
                                        let mut over_play_btn = false;
                                        let state = preview_state(r.id);
                                        let active = matches!(
                                            state,
                                            PlayState::Loading | PlayState::Playing
                                        );
                                        let hovering_cover = ui.rect_contains_pointer(resp.rect);
                                        if audio_enabled && (hovering_cover || active) {
                                            let center = resp.rect.center();
                                            let btn = egui::Rect::from_center_size(
                                                center,
                                                egui::vec2(18.0, 18.0),
                                            );
                                            let pb = ui.interact(
                                                btn,
                                                egui::Id::new(("preview-btn", r.id)),
                                                egui::Sense::click(),
                                            );
                                            over_play_btn = pb.hovered();
                                            let (alpha, glyph, glyph_col) = match state {
                                                PlayState::Idle => (
                                                    if pb.hovered() { 215 } else { 150 },
                                                    "▶",
                                                    egui::Color32::WHITE,
                                                ),
                                                PlayState::Loading => {
                                                    (220, "…", egui::Color32::WHITE)
                                                }
                                                PlayState::Playing => (
                                                    220,
                                                    "■",
                                                    egui::Color32::from_rgb(130, 210, 130),
                                                ),
                                            };
                                            ui.painter().circle_filled(
                                                center,
                                                9.0,
                                                egui::Color32::from_black_alpha(alpha),
                                            );
                                            ui.painter().text(
                                                center,
                                                egui::Align2::CENTER_CENTER,
                                                glyph,
                                                egui::FontId::proportional(10.0),
                                                glyph_col,
                                            );
                                            if pb.hovered() {
                                                ui.ctx().set_cursor_icon(
                                                    egui::CursorIcon::PointingHand,
                                                );
                                            }
                                            if pb.clicked() {
                                                preview_request =
                                                    Some((r.id, r.source_path.clone()));
                                            }
                                        }

                                        if resp.clicked() {
                                            clicked = true;
                                        }
                                        // Don't let a double-click on the play disc open the
                                        // convert modal — that's a preview start/stop, not a row
                                        // action. Likewise a drag begun on the play disc isn't a
                                        // row drag-out.
                                        if resp.double_clicked() && !over_play_btn {
                                            dbl = true;
                                        }
                                        if resp.drag_started() && !over_play_btn {
                                            drag = true;
                                            if alt_drag {
                                                native_drag_ids = Some(drag_ids.clone());
                                            }
                                        }
                                        if !over_play_btn && resp.dragged() && !alt_drag {
                                            resp.dnd_set_drag_payload(DraggedTracks(
                                                drag_ids.clone(),
                                            ));
                                        }
                                    });
                                    continue;
                                }
                                // Data columns. Key and Quality render their own coloured
                                // chips; the rest are plain labels (`text` is unused for the
                                // chip columns, which read straight from `r`).
                                let text: &str = match col {
                                    TableColumn::Artist => &r.artist,
                                    TableColumn::Title => &r.title,
                                    TableColumn::Album => &r.album,
                                    TableColumn::Genre => &r.genre,
                                    TableColumn::Duration => &r.duration,
                                    TableColumn::Bpm => &r.bpm,
                                    TableColumn::Key => &r.key,
                                    TableColumn::Format => &r.format_label,
                                    TableColumn::Bitrate => &r.bitrate,
                                    TableColumn::Notes => &r.notes,
                                    TableColumn::Added => &r.added,
                                    _ => "",
                                };
                                row.col(|ui| {
                                    // The Key column renders as a Camelot-coloured chip so
                                    // harmonically-related keys read at a glance; every
                                    // other column is a plain (non-interactive) label.
                                    if col == TableColumn::Key {
                                        if let Some(cam) = r.camelot {
                                            let bg = camelot_color(cam);
                                            let fg = chip_text_color(bg);
                                            // Fill the entire cell with the Camelot colour
                                            // so the key reads as a solid colour-block, not
                                            // a small chip floating in an empty cell, with
                                            // the key text centred inside it.
                                            let rect = ui.max_rect();
                                            ui.painter().rect_filled(rect, 0.0, bg);
                                            ui.painter().text(
                                                rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                text,
                                                egui::TextStyle::Body.resolve(ui.style()),
                                                fg,
                                            );
                                        } else {
                                            ui.add(egui::Label::new(
                                                egui::RichText::new(text).weak(),
                                            ));
                                        }
                                    } else if col == TableColumn::Quality {
                                        // Quality: fill the whole cell with the
                                        // transcode-check colour (green lossless → red
                                        // likely-lossy) and centre the verdict inside it,
                                        // the same solid colour-block treatment as the Key
                                        // column. "—" until analyzed.
                                        if let Some(v) = r.quality {
                                            let (label, bg) = quality_chip(v);
                                            let fg = chip_text_color(bg);
                                            let rect = ui.max_rect();
                                            ui.painter().rect_filled(rect, 0.0, bg);
                                            ui.painter().text(
                                                rect.center(),
                                                egui::Align2::CENTER_CENTER,
                                                label,
                                                egui::TextStyle::Body.resolve(ui.style()),
                                                fg,
                                            );
                                        } else {
                                            ui.add(egui::Label::new(
                                                egui::RichText::new("—").weak(),
                                            ));
                                        }
                                    } else if col == TableColumn::Waveform {
                                        // Inline colored waveform, painted to fill the
                                        // cell. No playhead here (that's the player bar),
                                        // so every bar is full brightness.
                                        let rect = ui.max_rect();
                                        if r.waveform.is_empty() {
                                            // Unanalyzed: a faint baseline, not an empty cell.
                                            let y = rect.center().y;
                                            ui.painter().line_segment(
                                                [
                                                    egui::pos2(rect.left() + 2.0, y),
                                                    egui::pos2(rect.right() - 2.0, y),
                                                ],
                                                egui::Stroke::new(
                                                    1.0,
                                                    egui::Color32::from_gray(55),
                                                ),
                                            );
                                        } else {
                                            let inset = rect.shrink2(egui::vec2(2.0, 3.0));
                                            crate::player::draw_waveform(
                                                ui.painter(),
                                                inset,
                                                &r.waveform,
                                                &r.waveform_bands,
                                                waveform_color_mode,
                                                None,
                                            );
                                        }
                                    } else {
                                        ui.add(egui::Label::new(if is_primary {
                                            egui::RichText::new(text).strong()
                                        } else {
                                            egui::RichText::new(text)
                                        }));
                                    }
                                    // …then sense the *entire* cell rect, not just the
                                    // glyphs. Grabbing anywhere in the cell — including the
                                    // empty space after short text — now starts a click or
                                    // drag instead of falling through to the scroll area.
                                    let mut resp = ui.interact(
                                        ui.max_rect(),
                                        ui.id().with(("cell-drag", r.id, col)),
                                        egui::Sense::click_and_drag(),
                                    );
                                    // Quality cell: explain the verdict on hover (cutoff +
                                    // estimated lossy source), or prompt to analyze.
                                    if col == TableColumn::Quality {
                                        resp = resp.on_hover_note(quality_tooltip(r));
                                    }
                                    if resp.clicked() {
                                        clicked = true;
                                    }
                                    if resp.double_clicked() {
                                        dbl = true;
                                    }
                                    if resp.drag_started() {
                                        drag = true;
                                        if alt_drag {
                                            native_drag_ids = Some(drag_ids.clone());
                                        }
                                    }
                                    if resp.dragged() && !alt_drag {
                                        resp.dnd_set_drag_payload(DraggedTracks(drag_ids.clone()));
                                    }
                                    // Right-click menu. Attached to each text cell's
                                    // interactive response (which senses secondary clicks);
                                    // the row's union response only senses hover, so it can't
                                    // carry the menu. Acts on the whole selection when this
                                    // row is part of it, else just this row (the same payload
                                    // a drag carries, already computed as `drag_ids`).
                                    resp.context_menu(|ui| {
                                        let title = if drag_ids.len() > 1 {
                                            format!("{} tracks", drag_ids.len())
                                        } else {
                                            short(&r.title, "Untitled").to_string()
                                        };
                                        ui.label(egui::RichText::new(title).strong());
                                        ui.separator();
                                        if audio_enabled && ui.button("▶  Preview").clicked() {
                                            menu_action = Some(TrackMenuAction::Preview(
                                                r.id,
                                                r.source_path.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui.button("Convert…").clicked() {
                                            menu_action = Some(TrackMenuAction::Convert(r.id));
                                            ui.close_menu();
                                        }
                                        if drag_ids.len() > 1
                                            && ui
                                                .button(format!(
                                                    "Convert {} selected…",
                                                    drag_ids.len()
                                                ))
                                                .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::ConvertSelected(
                                                drag_ids.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        let analyze_label = if drag_ids.len() > 1 {
                                            format!("Analyze {} selected", drag_ids.len())
                                        } else {
                                            "Analyze".to_string()
                                        };
                                        if ui.button(analyze_label).clicked() {
                                            menu_action =
                                                Some(TrackMenuAction::Analyze(drag_ids.clone()));
                                            ui.close_menu();
                                        }
                                        ui.menu_button("Add to playlist", |ui| {
                                            if menu_playlists.is_empty() {
                                                ui.label(
                                                    egui::RichText::new("No playlists yet")
                                                        .weak()
                                                        .italics(),
                                                );
                                            }
                                            for (pid, name) in &menu_playlists {
                                                if ui.button(name).clicked() {
                                                    menu_action =
                                                        Some(TrackMenuAction::AddToPlaylist(
                                                            *pid,
                                                            drag_ids.clone(),
                                                        ));
                                                    ui.close_menu();
                                                }
                                            }
                                        });
                                        if let Some(pid) = menu_playlist_view {
                                            ui.separator();
                                            if ui.button("Remove from playlist").clicked() {
                                                menu_action =
                                                    Some(TrackMenuAction::RemoveFromPlaylist(
                                                        pid,
                                                        drag_ids.clone(),
                                                    ));
                                                ui.close_menu();
                                            }
                                            if menu_unfiltered {
                                                if ui.button("Move to top").clicked() {
                                                    menu_action = Some(TrackMenuAction::MoveToTop(
                                                        pid,
                                                        drag_ids.clone(),
                                                    ));
                                                    ui.close_menu();
                                                }
                                                if ui.button("Move to bottom").clicked() {
                                                    menu_action =
                                                        Some(TrackMenuAction::MoveToBottom(
                                                            pid,
                                                            drag_ids.clone(),
                                                        ));
                                                    ui.close_menu();
                                                }
                                            }
                                        }
                                        ui.separator();
                                        if ui.button("Reveal in Finder").clicked() {
                                            menu_action = Some(TrackMenuAction::RevealInFinder(
                                                r.source_path.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        if ui.button("Copy file path").clicked() {
                                            menu_action = Some(TrackMenuAction::CopyPath(
                                                r.source_path.display().to_string(),
                                            ));
                                            ui.close_menu();
                                        }
                                        // Copy "Artist – Title" (one line per selected
                                        // track) for pasting straight into a Soulseek
                                        // search box. Acts on the whole selection when
                                        // this row is part of it.
                                        let slsk_label = if drag_ids.len() > 1 {
                                            format!("Copy for Soulseek ({})", drag_ids.len())
                                        } else {
                                            "Copy for Soulseek".to_string()
                                        };
                                        if ui
                                            .button(slsk_label)
                                            .on_hover_note(soulseek_query(&r.artist, &r.title))
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::CopyForSoulseek(
                                                drag_ids.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        // Open the track's release/album on Discogs in
                                        // the default browser. Deep-links to the exact
                                        // release when one was fetched; otherwise the
                                        // dispatcher falls back to a Discogs search
                                        // seeded with this artist + album/title.
                                        if ui
                                            .button("View on Discogs ↗")
                                            .on_hover_note(
                                                "Open this track's release on discogs.com",
                                            )
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::OpenDiscogs(
                                                r.id,
                                                discogs_search_query(&r.artist, &r.album, &r.title),
                                            ));
                                            ui.close_menu();
                                        }
                                        // Re-pick the Discogs release for this one track
                                        // (applies its cover + fills empty tag fields),
                                        // even if it was already fetched.
                                        if ui
                                            .button("Edit release…")
                                            .on_hover_note(
                                                "Search Discogs and choose this track's release \
                                             to set its cover and fill missing fields.",
                                            )
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::EditRelease(r.id));
                                            ui.close_menu();
                                        }
                                        // Fetch from Discogs for the whole selection (or just
                                        // this row when it isn't part of the selection), using
                                        // the same per-track release picker as the toolbar.
                                        let n = drag_ids.len();
                                        let art_label = if n > 1 {
                                            format!("Fetch artwork ({n})")
                                        } else {
                                            "Fetch artwork".to_string()
                                        };
                                        if ui
                                            .button(art_label)
                                            .on_hover_note(
                                                "Search Discogs and pick a release per track to \
                                             cache its cover. Cover art only — never touches \
                                             tags.",
                                            )
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::FetchArtwork(
                                                drag_ids.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        let data_label = if n > 1 {
                                            format!("Fetch song release details ({n})")
                                        } else {
                                            "Fetch song release details".to_string()
                                        };
                                        if ui
                                            .button(data_label)
                                            .on_hover_note(
                                                "Search Discogs and pick a release per track to \
                                             cache its cover and fill the track's empty fields \
                                             (genre/style, label, catalog #, year, country, \
                                             album, date). Only fills empty fields — edits the \
                                             catalog, not your files.",
                                            )
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::FetchSongDetails(
                                                drag_ids.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                        ui.separator();
                                        // Permanently drop the track(s) from the catalog.
                                        // Destructive (removes them from every playlist and
                                        // the analysis cache) but never touches the source
                                        // files, so it's red and gated behind a confirm.
                                        let n = drag_ids.len();
                                        let label = if n == 1 {
                                            "Delete from catalog".to_string()
                                        } else {
                                            format!("Delete {n} from catalog")
                                        };
                                        if ui
                                            .button(
                                                egui::RichText::new(label)
                                                    .color(egui::Color32::from_rgb(220, 90, 90)),
                                            )
                                            .on_hover_note(
                                                "Remove the selected track(s) from the catalog \
                                             and all playlists. The source files on disk are \
                                             not deleted.",
                                            )
                                            .clicked()
                                        {
                                            menu_action = Some(TrackMenuAction::DeleteFromCatalog(
                                                drag_ids.clone(),
                                            ));
                                            ui.close_menu();
                                        }
                                    });
                                });
                            }
                            // Trailing spacer so the striped row background fills the full
                            // table width.
                            row.col(|_| {});
                            // Record the whole row's screen rect (union of its cells)
                            // for the dropped-cover hit-test after the table.
                            row_rects.push((r.id, row.response().rect));

                            // A plain click resolves to a (modifier-aware) selection after
                            // the table. A drag takes priority and is handled via the DnD
                            // payload set above — egui suppresses `clicked()` once a drag
                            // passes the threshold, so the two are mutually exclusive in
                            // practice. The same drag drops onto a playlist (in-window) or,
                            // once it leaves the window, becomes the native drag-out.
                            if !drag && clicked {
                                clicked_id = Some(r.id);
                            }
                            if dbl {
                                clicked_id = Some(r.id);
                                open_convert_for = Some(r.id);
                            }
                        });
                    });
            });

        // Publish this frame's visible row rects for the dropped-cover hit-test.
        self.row_screen_rects = row_rects;

        // Reconcile the columns' rendered widths with the saved set. egui_extras
        // owns the live width (so a drag updates smoothly within the session under
        // a single shared id); we mirror it into `self.column_widths` and persist
        // to config so the widths are shared across every view and survive
        // rebuilds — where egui's own layout-keyed memory would reset. The flush
        // is deferred until the drag ends (pointer up) so one resize writes the
        // TOML once, not every frame.
        for (col, w) in observed_widths {
            let changed = self
                .column_widths
                .get(&col)
                .map_or(true, |prev| (prev - w).abs() > 0.5);
            if changed {
                self.column_widths.insert(col, w);
                self.column_widths_dirty = true;
            }
        }
        if self.column_widths_dirty && !ctx_clone.input(|i| i.pointer.any_down()) {
            self.save_column_widths();
            self.column_widths_dirty = false;
        }

        // Clicking the empty space below the rows clears the selection, so clicking
        // away from the songs deselects them. Row rects span the full table width,
        // so "not on any row" means the click landed in the blank area beneath the
        // last row. We skip clicks the header, a row, a drag, or a context menu
        // already owns this frame.
        if clicked_id.is_none()
            && menu_action.is_none()
            && !ctx_clone.is_context_menu_open()
            && (!self.selection.is_empty() || self.selected.is_some())
            && ctx_clone.input(|i| i.pointer.primary_clicked())
        {
            let viewport = scroll_out.inner_rect;
            if let Some(p) = ctx_clone.pointer_interact_pos() {
                let below_header = p.y > viewport.top() + 22.0;
                let on_row = self.row_screen_rects.iter().any(|(_, rect)| rect.contains(p));
                if viewport.contains(p) && below_header && !on_row {
                    self.clear_selection();
                }
            }
        }
        // Escape also clears the selection so the perma-highlight goes away.
        // Skipped while a text field has focus (Escape there cancels the edit) or
        // while a popup/modal is up (it handles its own Escape).
        if (!self.selection.is_empty() || self.selected.is_some())
            && !ctx_clone.wants_keyboard_input()
            && !self.escape_consumed_elsewhere()
            && ctx_clone.input(|i| i.key_pressed(egui::Key::Escape))
        {
            self.clear_selection();
        }

        // Drag-to-reorder within a playlist: while an in-app track drag hovers the
        // table, paint an insertion line at the nearest row gap and, on release,
        // rewrite the playlist order with the dragged tracks moved to that slot.
        // Only active when the visible rows are the playlist in stored order
        // (`reorderable`), so the slot index maps straight to a playlist position.
        let mut reorder_drop: Option<(Id, Vec<Id>, usize)> = None;
        if reorderable {
            if let (Some(pid), Some(payload)) = (
                playlist_pid,
                egui::DragAndDrop::payload::<DraggedTracks>(&ctx_clone),
            ) {
                let table_rect = scroll_out.inner_rect;
                if let Some(ptr) = ctx_clone.pointer_interact_pos() {
                    if table_rect.x_range().contains(ptr.x) && !row_spans.is_empty() {
                        // Insertion slot = the first visible row whose vertical
                        // midpoint sits below the cursor; past the last row → append.
                        let mut target = row_spans.last().map_or(0, |&(i, _, _)| i + 1);
                        let mut line_y = row_spans.last().map(|&(_, _, b)| b);
                        for &(i, top, bottom) in &row_spans {
                            if ptr.y < (top + bottom) * 0.5 {
                                target = i;
                                line_y = Some(top);
                                break;
                            }
                        }
                        if let Some(y) = line_y {
                            let accent = ui.visuals().selection.bg_fill;
                            let x0 = table_rect.left() + 2.0;
                            let x1 = table_rect.right() - 2.0;
                            let painter = ctx_clone.layer_painter(egui::LayerId::new(
                                egui::Order::Foreground,
                                egui::Id::new("reorder-line"),
                            ));
                            painter.hline(x0..=x1, y, egui::Stroke::new(2.0, accent));
                            painter.circle_filled(egui::pos2(x0 + 1.0, y), 3.5, accent);
                        }
                        if ctx_clone.input(|i| i.pointer.any_released()) {
                            reorder_drop = Some((pid, payload.0.clone(), target));
                        }
                    }
                }
            }
        }
        if let Some((pid, ids, at)) = reorder_drop {
            // Consume the payload so the sidebar's add-to-playlist drop zone can't
            // also act on the same release.
            egui::DragAndDrop::clear_payload(&ctx_clone);
            self.insert_in_playlist(pid, &ids, at);
        }

        // A clicked column header cycles its sort (asc → desc → off) and re-sorts.
        if let Some(col) = header_clicked {
            self.toggle_sort(col);
        }

        // The Waveform header toggle flips the inline waveform colouring between
        // energy and frequency; persist it so the player bar and next launch match.
        if toggle_waveform_mode {
            let next = match config::WaveformColorMode::from_key(&self.config.waveform_color_mode) {
                config::WaveformColorMode::Energy => config::WaveformColorMode::Spectrum,
                config::WaveformColorMode::Spectrum => config::WaveformColorMode::Energy,
            };
            self.config.waveform_color_mode = next.key().to_string();
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save settings: {e}");
            }
        }

        // A right-clicked header opens (or re-anchors) the column reorder popup.
        if let Some(pos) = open_col_menu {
            self.column_menu = Some(pos);
        }
        self.show_column_menu(&ctx_clone);

        // A double-clicked header opens (or re-anchors) that column's filter bar.
        if let Some((col, pos)) = open_col_filter {
            self.col_filter_open = Some(ColFilterPopup {
                col,
                pos,
                focus: true,
            });
        }
        self.show_col_filter_popup(&ctx_clone);

        // Hand the visible rows' missing covers to the worker thread. No per-frame
        // cap is needed — only on-screen rows enqueue, and the disk read + decode
        // run off the UI thread, so even flinging the scrollbar stays smooth.
        for id in needs_cover_load {
            self.request_thumb(id);
        }

        // While an in-app row-drag is live, float a "N track(s)" chip at the
        // cursor so it's clear something is being carried toward a playlist.
        if let Some(payload) = egui::DragAndDrop::payload::<DraggedTracks>(&ctx_clone) {
            if let Some(pos) = ctx_clone.pointer_interact_pos() {
                let text = format!("{} track(s)", payload.0.len());
                let painter = ctx_clone.layer_painter(egui::LayerId::new(
                    egui::Order::Tooltip,
                    egui::Id::new("drag-preview"),
                ));
                let at = pos + egui::vec2(14.0, 6.0);
                let galley = painter.layout_no_wrap(
                    text,
                    egui::FontId::proportional(12.0),
                    egui::Color32::WHITE,
                );
                let pad = egui::vec2(6.0, 3.0);
                let rect = egui::Rect::from_min_size(at, galley.size() + pad * 2.0);
                painter.rect_filled(
                    rect,
                    egui::Rounding::same(4.0),
                    egui::Color32::from_rgb(60, 110, 170),
                );
                painter.galley(at + pad, galley, egui::Color32::WHITE);
            }
        }

        if let Some((id, path)) = preview_request {
            self.play_track(id, path);
        }

        // Apply the right-click menu choice. `Convert` feeds the existing
        // `open_convert_for` path below so it opens the same modal a double-click
        // does; everything else mutates the catalog and reloads here.
        match menu_action {
            Some(TrackMenuAction::Preview(id, path)) => self.play_track(id, path),
            Some(TrackMenuAction::Convert(id)) => open_convert_for = Some(id),
            Some(TrackMenuAction::ConvertSelected(ids)) => {
                let (target, bitrate_kbps, out_dir, in_place) = convert_defaults(&self.config);
                self.batch_convert = Some(BatchConvert {
                    ids,
                    target,
                    bitrate_kbps,
                    out_dir,
                    in_place,
                    error: None,
                });
            }
            Some(TrackMenuAction::Analyze(ids)) => {
                self.spawn_analyze_ids(ctx_clone.clone(), ids, false);
            }
            Some(TrackMenuAction::AddToPlaylist(pid, ids)) => {
                match Catalog::open(&self.db_path).and_then(|c| c.add_tracks(pid, &ids)) {
                    Ok(n) => self.status = format!("Added {n} track(s) to playlist."),
                    Err(e) => self.status = format!("Add to playlist failed: {e}"),
                }
                self.reload();
            }
            Some(TrackMenuAction::RemoveFromPlaylist(pid, ids)) => {
                match Catalog::open(&self.db_path).and_then(|c| c.remove_tracks(pid, &ids)) {
                    Ok(n) => self.status = format!("Removed {n} track(s) from playlist."),
                    Err(e) => self.status = format!("Remove from playlist failed: {e}"),
                }
                self.reload();
            }
            Some(TrackMenuAction::DeleteFromCatalog(ids)) => {
                self.confirm_delete = Some(ids);
            }
            Some(TrackMenuAction::MoveToTop(pid, ids)) => self.move_in_playlist(pid, &ids, true),
            Some(TrackMenuAction::MoveToBottom(pid, ids)) => {
                self.move_in_playlist(pid, &ids, false)
            }
            Some(TrackMenuAction::RevealInFinder(path)) => reveal_in_finder(&path),
            Some(TrackMenuAction::CopyPath(path)) => {
                ctx_clone.copy_text(path);
                self.status = "Copied file path to clipboard.".into();
            }
            Some(TrackMenuAction::CopyForSoulseek(ids)) => {
                // Resolve each id back to its visible row so the copy reflects the
                // current (possibly tag-edited) artist/title. Preserve row order.
                let lines: Vec<String> = self
                    .rows
                    .iter()
                    .filter(|r| ids.contains(&r.id))
                    .map(|r| soulseek_query(&r.artist, &r.title))
                    .filter(|q| !q.is_empty())
                    .collect();
                let n = lines.len();
                ctx_clone.copy_text(lines.join("\n"));
                self.status = if n == 1 {
                    "Copied for Soulseek — paste into the search box.".into()
                } else {
                    format!("Copied {n} tracks for Soulseek (one per line).")
                };
            }
            Some(TrackMenuAction::OpenDiscogs(id, query)) => {
                // Prefer the exact release the artwork run picked; fall back to
                // a Discogs search seeded with the track's artist + album/title.
                let release_id = Catalog::open(&self.db_path)
                    .and_then(|c| c.external_release_id(id))
                    .ok()
                    .flatten();
                let url = discogs_url(release_id.as_deref(), &query);
                open_url(&url);
                self.status = format!("Opening Discogs: {url}");
            }
            Some(TrackMenuAction::EditRelease(id)) => {
                self.spawn_edit_release(ctx_clone.clone(), id);
            }
            Some(TrackMenuAction::FetchArtwork(ids)) => {
                self.spawn_fetch_tracks(ctx_clone.clone(), ids, false);
            }
            Some(TrackMenuAction::FetchSongDetails(ids)) => {
                self.spawn_fetch_tracks(ctx_clone.clone(), ids, true);
            }
            None => {}
        }

        if let Some(id) = clicked_id {
            let mods = ctx_clone.input(|i| i.modifiers);
            self.apply_click_selection(id, mods);
        }
        if let Some(id) = open_convert_for {
            if let Some(r) = self.rows.iter().find(|r| r.id == id) {
                let (target, bitrate_kbps, out_dir, in_place) = convert_defaults(&self.config);
                self.convert_modal = Some(ConvertModal {
                    track_id: r.id,
                    track_label: format!(
                        "{} — {}",
                        short(&r.artist, "Unknown"),
                        short(&r.title, "Untitled")
                    ),
                    source_path: r.source_path.clone(),
                    source_format: r.format,
                    edit_title: r.title.clone(),
                    edit_artist: r.artist.clone(),
                    edit_album: r.album.clone(),
                    name_status: None,
                    name_is_error: false,
                    target,
                    bitrate_kbps,
                    out_dir,
                    in_place,
                    error: None,
                });
            }
        }

        // Resolve a ⌥-drag started this frame into its concrete source files for
        // the native drag-out. Done here, after every table closure has dropped,
        // so no borrow of `self.rows` is live when AppKit enters its nested loop.
        native_drag_ids.map(|ids| {
            self.rows
                .iter()
                .filter(|r| ids.contains(&r.id))
                .map(|r| r.source_path.clone())
                .collect()
        })
    }
}

/// Render a track's `added_at` (unix seconds) as a short relative age against
/// `now`, for the Added column — "today", "yesterday", "5d ago", "3w ago",
/// "2mo ago", "1y ago". Coarse on purpose (the exact `added_at` drives sorting);
/// a future or zero timestamp shows "—".
pub(crate) fn fmt_added(added_at: i64, now: i64) -> String {
    const DAY: i64 = 86_400;
    let secs = now - added_at;
    if added_at <= 0 || secs < 0 {
        return "—".into();
    }
    let days = secs / DAY;
    match days {
        0 => "today".into(),
        1 => "yesterday".into(),
        2..=6 => format!("{days}d ago"),
        7..=29 => format!("{}w ago", days / 7),
        30..=364 => format!("{}mo ago", days / 30),
        _ => format!("{}y ago", days / 365),
    }
}

/// Load the rows for `view`. `keep` only matters for [`LibraryView::RecentlyAdded`]:
/// it's the set of track ids that should stay visible even after they've expired
/// out of the inbox query (i.e. tracks the user analyzed + fetched while looking
/// at the tab). They're pinned in place until the tab is left, so a row never
/// vanishes from under the cursor the instant its work finishes. Empty for every
/// other view.
pub(crate) fn load_rows(
    db: &Path,
    filter: &str,
    view: &LibraryView,
    keep: &HashSet<Id>,
) -> Result<Vec<TrackRow>, String> {
    let catalog = Catalog::open(db).map_err(|e| e.to_string())?;
    let q = if filter.trim().is_empty() {
        None
    } else {
        Some(filter.trim())
    };
    let tracks = match view {
        LibraryView::Library => catalog.list_tracks(q, 0),
        LibraryView::RecentlyAdded => {
            // The live inbox (still-incomplete tracks) plus any pinned tracks that
            // have since completed — re-fetched by id since the inbox query no
            // longer returns them. Union, de-duplicated on id.
            catalog
                .list_recently_added(q, ANALYZER_VERSION)
                .and_then(|mut t| {
                    let have: HashSet<Id> = t.iter().map(|x| x.id).collect();
                    let extra: Vec<Id> = keep
                        .iter()
                        .copied()
                        .filter(|id| !have.contains(id))
                        .collect();
                    let mut pinned = catalog.list_tracks_by_ids(&extra, q)?;
                    t.append(&mut pinned);
                    Ok(t)
                })
        }
        LibraryView::Playlist(id) => catalog.list_playlist_tracks(*id, q),
        // The Duplicates, Missing and Vinyl views render from their own caches
        // (`dup_groups` / `missing_list` / `vinyl`), not the flat track table.
        LibraryView::Duplicates | LibraryView::Missing | LibraryView::Vinyl => Ok(Vec::new()),
    }
    .map_err(|e| e.to_string())?;
    let ext_art: HashSet<Id> = catalog
        .external_artwork_ids()
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();
    // `added_at` is catalog bookkeeping (not on `Track`), pulled in one query and
    // formatted relative to now so the Added column reads "today / 3d ago".
    let added_at: HashMap<Id, i64> = catalog
        .added_at_all()
        .map_err(|e| e.to_string())?
        .into_iter()
        .collect();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let mut rows = Vec::with_capacity(tracks.len());
    for t in tracks {
        let analysis = catalog.get_analysis(t.id).map_err(|e| e.to_string())?;
        let bpm_val = analysis.as_ref().and_then(|a| a.bpm);
        let camelot = analysis.as_ref().and_then(|a| a.key).map(|k| k.camelot());
        let key_sort = camelot.map(|c| u16::from(c.number) * 2 + u16::from(c.major));
        let (bpm, key) = match analysis.as_ref() {
            Some(a) => (
                a.bpm
                    .map(|b| format!("{b:.0}"))
                    .unwrap_or_else(|| "—".into()),
                a.key
                    .map(|k| k.camelot().label())
                    .unwrap_or_else(|| "—".into()),
            ),
            None => ("—".into(), "—".into()),
        };
        // Transcode-quality verdict — only meaningful once analyzed at v6+, when
        // the low-pass cutoff was measured. Older cached analyses read "—".
        let quality = analysis
            .as_ref()
            .filter(|a| a.analyzer_version >= 6)
            .map(|a| a.transcode_verdict());
        let quality_cut_hz = analysis.as_ref().and_then(|a| a.lowpass_hz);
        let quality_src = analysis.as_ref().and_then(|a| a.estimated_source_kbps());
        let quality_sort = quality.map(|v| match v {
            TranscodeVerdict::Clean => 0u8,
            TranscodeVerdict::Inconclusive => 1,
            TranscodeVerdict::Suspect => 2,
            TranscodeVerdict::LikelyLossy => 3,
        });
        let dur_ms = t.properties.as_ref().map(|p| p.duration_ms);
        let bitrate_val = t.properties.as_ref().and_then(|p| p.bitrate_kbps);
        let (duration, bitrate) = match &t.properties {
            Some(p) => (
                fmt_duration(p.duration_ms),
                p.bitrate_kbps
                    .map(|b| b.to_string())
                    .unwrap_or_else(|| "—".into()),
            ),
            None => ("—".into(), "—".into()),
        };
        rows.push(TrackRow {
            id: t.id,
            artist: t.tags.artist.unwrap_or_default(),
            title: t.tags.title.unwrap_or_default(),
            album: t.tags.album.unwrap_or_default(),
            genre: t.tags.genre.unwrap_or_default(),
            duration,
            bpm,
            key,
            format: t.format,
            format_label: format_label(t.format).into(),
            bitrate,
            notes: t.tags.comment.unwrap_or_default(),
            added: added_at
                .get(&t.id)
                .map(|&ts| fmt_added(ts, now))
                .unwrap_or_default(),
            added_at: added_at.get(&t.id).copied().unwrap_or(0),
            waveform: analysis
                .as_ref()
                .map(|a| a.waveform_preview.clone())
                .unwrap_or_default(),
            // Only the v11+ layout is the 4-byte `[low, mid, high, loudness]`
            // stride the renderer expects; older data had a different stride and
            // would be misread, so treat it as absent (the cell falls back).
            waveform_bands: analysis
                .as_ref()
                .filter(|a| a.analyzer_version >= 11)
                .map(|a| a.waveform_bands.clone())
                .unwrap_or_default(),
            source_path: PathBuf::from(t.source_path),
            has_cover: t.tags.has_cover,
            has_external_cover: ext_art.contains(&t.id),
            dur_ms,
            bpm_val,
            bitrate_val,
            key_sort,
            camelot,
            quality,
            quality_cut_hz,
            quality_src,
            quality_sort,
        });
    }
    // The Recent view unions two queries (live inbox + pinned-complete), so its
    // rows arrive out of order — restore newest-first by `added_at`. Other views
    // keep their query order (the table's header sort applies on top either way).
    if *view == LibraryView::RecentlyAdded {
        rows.sort_by(|a, b| b.added_at.cmp(&a.added_at).then(b.id.cmp(&a.id)));
    }
    Ok(rows)
}

// --- worker thread bodies (mirror the CLI's command implementations) ---------

/// Colour for a Camelot wheel position. The 12 numbers map evenly around the hue
/// circle so harmonic neighbours (±1 on the wheel) land in adjacent hues; the
/// minor side (A) is shaded a little darker than its relative major (B), the way
/// the inner/outer rings of a Camelot wheel are drawn. Mirrors the key-tag
/// colouring DJs know from Mixed In Key / rekordbox.
pub(crate) fn camelot_color(c: Camelot) -> egui::Color32 {
    let hue = (f32::from(c.number.clamp(1, 12)) - 1.0) / 12.0 * 360.0;
    let (sat, val) = if c.major { (0.60, 0.92) } else { (0.60, 0.74) };
    hsv_to_color32(hue, sat, val)
}

/// A legend for the four transcode-quality chips, rendered as coloured swatches
/// with a one-line meaning each. Shown on hover of the Quality header and in the
/// Quality filter popup so `clean` / `~320?` / `lossy` / `ltd` are self-documenting
/// instead of something you have to learn from per-cell tooltips.
pub(crate) fn quality_legend_ui(ui: &mut egui::Ui) {
    ui.set_max_width(340.0);
    ui.strong("Transcode quality");
    ui.label(
        egui::RichText::new(
            "From the spectral roll-off — how the file's top end falls off, which \
             reveals a lossy source upsampled into a bigger container.",
        )
        .small()
        .weak(),
    );
    ui.add_space(4.0);
    let row = |ui: &mut egui::Ui, v: TranscodeVerdict, meaning: &str| {
        let (label, bg) = quality_chip(v);
        ui.horizontal(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(format!(" {label} "))
                        .small()
                        .color(chip_text_color(bg))
                        .background_color(bg),
                )
                .selectable(false),
            );
            ui.add(egui::Label::new(egui::RichText::new(meaning).small()).wrap());
        });
    };
    row(ui, TranscodeVerdict::Clean, "Full-band — looks lossless.");
    row(
        ui,
        TranscodeVerdict::Suspect,
        "Sharp cutoff near 20 kHz — likely a 320 kbps transcode (a lossless master \
         with a 20 kHz shelf also lands here).",
    );
    row(
        ui,
        TranscodeVerdict::LikelyLossy,
        "Sharp wall well below 20 kHz — almost certainly upsampled from a worse-than-320 \
         lossy source.",
    );
    row(
        ui,
        TranscodeVerdict::Inconclusive,
        "Band-limited with a gentle roll-off — old/quiet master, not a transcode. Benign.",
    );
}

/// Short label + chip colour for a transcode-quality verdict (Quality column).
pub(crate) fn quality_chip(v: TranscodeVerdict) -> (&'static str, egui::Color32) {
    match v {
        // `Clean` (full-band) and `Inconclusive` (band-limited, gentle roll-off)
        // both mean "no transcode signature" — band-limiting is a mastering choice,
        // not a quality flag — so they share one chip. Only Suspect/LikelyLossy flag.
        TranscodeVerdict::Clean | TranscodeVerdict::Inconclusive => {
            ("clean", egui::Color32::from_rgb(70, 130, 80))
        }
        TranscodeVerdict::Suspect => ("~320?", egui::Color32::from_rgb(170, 140, 60)),
        TranscodeVerdict::LikelyLossy => ("lossy", egui::Color32::from_rgb(170, 70, 65)),
    }
}

/// Tooltip explaining a row's Quality cell: the verdict in words, plus the
/// measured cutoff and estimated lossy source when there's a wall to report.
pub(crate) fn quality_tooltip(r: &TrackRow) -> String {
    let Some(v) = r.quality else {
        return "Not checked yet — run Analyze to scan for lossy transcodes.".into();
    };
    quality_blurb(v, r.quality_cut_hz, r.quality_src)
}

/// The human-readable explanation of a transcode verdict, given its measured
/// cutoff and estimated source bitrate. Shared by the Library "Quality" column
/// tooltip and the Duplicates view's per-copy quality chip.
pub(crate) fn quality_blurb(
    v: TranscodeVerdict,
    cut_hz: Option<f32>,
    src: Option<&'static str>,
) -> String {
    let mut s = match v {
        TranscodeVerdict::Clean | TranscodeVerdict::Inconclusive => {
            "No transcode signature — looks fine.".to_string()
        }
        TranscodeVerdict::Suspect => {
            "Sharp cutoff near 20 kHz — possibly a 320 kbps transcode.".to_string()
        }
        TranscodeVerdict::LikelyLossy => {
            "Brick wall well below Nyquist — almost certainly a lossy transcode.".to_string()
        }
    };
    if let Some(hz) = cut_hz {
        s += &format!("\nLow-pass cutoff: {:.1} kHz", hz / 1000.0);
    }
    if matches!(v, TranscodeVerdict::Suspect | TranscodeVerdict::LikelyLossy) {
        if let Some(src) = src {
            s += &format!("\nEstimated source: {src}");
        }
    }
    s
}

/// Black or white text for legibility on `bg`, chosen by its perceived luminance.
pub(crate) fn chip_text_color(bg: egui::Color32) -> egui::Color32 {
    let lum = 0.299 * f32::from(bg.r()) + 0.587 * f32::from(bg.g()) + 0.114 * f32::from(bg.b());
    if lum > 140.0 {
        egui::Color32::from_gray(20)
    } else {
        egui::Color32::from_gray(235)
    }
}

/// HSV (`h` in degrees, `s`/`v` in 0..=1) to an opaque colour.
pub(crate) fn hsv_to_color32(h: f32, s: f32, v: f32) -> egui::Color32 {
    let c = v * s;
    let hp = h.rem_euclid(360.0) / 60.0;
    let x = c * (1.0 - (hp % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match hp as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    egui::Color32::from_rgb(
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}
