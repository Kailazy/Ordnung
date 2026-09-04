//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

impl App {
    /// Reorder `ids` to the front (`to_top`) or back of playlist `pid`. The full
    /// playlist order is read from the catalog (not the possibly-filtered table)
    /// so hidden tracks are never dropped; the moved tracks keep their relative
    /// order, and the result is written back via `reorder_tracks`.
    pub(crate) fn move_in_playlist(&mut self, pid: Id, ids: &[Id], to_top: bool) {
        let cat = match Catalog::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Reorder failed: {e}");
                return;
            }
        };
        let full = match cat.list_playlist_tracks(pid, None) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Reorder failed: {e}");
                return;
            }
        };
        let moving: std::collections::HashSet<Id> = ids.iter().copied().collect();
        let mut picked: Vec<Id> = Vec::new();
        let mut rest: Vec<Id> = Vec::new();
        for t in &full {
            if moving.contains(&t.id) {
                picked.push(t.id);
            } else {
                rest.push(t.id);
            }
        }
        let ordered: Vec<Id> = if to_top {
            picked.into_iter().chain(rest).collect()
        } else {
            rest.into_iter().chain(picked).collect()
        };
        if let Err(e) = cat.reorder_tracks(pid, &ordered) {
            self.status = format!("Reorder failed: {e}");
            return;
        }
        self.reload();
    }

    /// Move `ids` so the block lands just before the track currently at position
    /// `insert_at` in playlist `pid`, preserving the moved tracks' relative order.
    /// The full order is read from the catalog (not the table) so the index maps
    /// to a real playlist position; dropping past the end appends. Used by the
    /// drag-to-reorder insertion line in the playlist table.
    pub(crate) fn insert_in_playlist(&mut self, pid: Id, ids: &[Id], insert_at: usize) {
        let cat = match Catalog::open(&self.db_path) {
            Ok(c) => c,
            Err(e) => {
                self.status = format!("Reorder failed: {e}");
                return;
            }
        };
        let full = match cat.list_playlist_tracks(pid, None) {
            Ok(t) => t,
            Err(e) => {
                self.status = format!("Reorder failed: {e}");
                return;
            }
        };
        let moving: std::collections::HashSet<Id> = ids.iter().copied().collect();
        // The first kept (non-moving) track at or after the drop point anchors the
        // insertion; the block goes immediately before it. If the drop lands among
        // the moving tracks or past the end, there's no anchor and we append.
        let anchor = full
            .iter()
            .skip(insert_at)
            .map(|t| t.id)
            .find(|id| !moving.contains(id));
        let picked: Vec<Id> = full
            .iter()
            .map(|t| t.id)
            .filter(|id| moving.contains(id))
            .collect();
        let mut ordered: Vec<Id> = Vec::with_capacity(full.len());
        let mut inserted = false;
        for t in &full {
            if moving.contains(&t.id) {
                continue;
            }
            if anchor == Some(t.id) {
                ordered.extend(picked.iter().copied());
                inserted = true;
            }
            ordered.push(t.id);
        }
        if !inserted {
            ordered.extend(picked.iter().copied());
        }
        if let Err(e) = cat.reorder_tracks(pid, &ordered) {
            self.status = format!("Reorder failed: {e}");
            return;
        }
        self.reload();
    }
}

/// The sidebar/toolbar accent — matches the "Add songs…" primary button so the
/// active navigation target reads as part of the same visual language.
pub(crate) const NAV_ACCENT: egui::Color32 = egui::Color32::from_rgb(64, 110, 180);

/// A large, full-width rectangular navigation button for the sidebar. `height`
/// sizes the tile (Library is tallest, playlists / collection views a bit
/// shorter) and `text_size` its label; `selected` paints the accent fill. The
/// `Response` is returned so callers can wire clicks, drag-and-drop drop targets
/// and context menus on top of it.
pub(crate) fn nav_button(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    height: f32,
    text_size: f32,
) -> egui::Response {
    let w = ui.available_width();
    nav_button_sized(ui, label, selected, w, height, text_size)
}

/// Like [`nav_button`] but with an explicit tile `width` instead of filling the
/// available space — used when two tiles share a row (e.g. the big "Library"
/// tile alongside the smaller "Recent" tile).
pub(crate) fn nav_button_sized(
    ui: &mut egui::Ui,
    label: &str,
    selected: bool,
    width: f32,
    height: f32,
    text_size: f32,
) -> egui::Response {
    let w = width;
    let mut text = egui::RichText::new(label).size(text_size);
    if selected {
        text = text.color(egui::Color32::WHITE).strong();
    }
    let mut btn = egui::Button::new(text)
        .min_size(egui::vec2(w, height))
        .rounding(egui::Rounding::same(6.0));
    if selected {
        btn = btn.fill(NAV_ACCENT);
    }
    // Indent the label off the left edge so it reads as a roomy nav tile rather
    // than text crammed against the border. `button_padding` is the left inset
    // for the (left-aligned) content; restore it so only this button is affected.
    let prev_padding = ui.spacing().button_padding;
    // A narrow icon-tier tile has no room for a left gutter — the glyph would
    // sit off-centre or clip — so pad it symmetrically instead and let egui
    // centre the single character.
    ui.spacing_mut().button_padding.x = if w < 90.0 { 4.0 } else { 12.0 };

    // The tile fills the sidebar's full width, so its left/right edges sit on the
    // panel clip boundary. egui's default hover/active state draws a 1px outline
    // on those edges — which gets clipped, leaving a border "cut out" on the sides.
    // Swap that edge-stroke feedback for a subtle fill so hover reads cleanly with
    // no clipped border. Saved and restored so only this button is affected.
    let prev_widgets = ui.visuals().widgets.clone();
    {
        let w = &mut ui.visuals_mut().widgets;
        w.hovered.bg_stroke = egui::Stroke::NONE;
        w.hovered.weak_bg_fill = egui::Color32::from_gray(64);
        w.active.bg_stroke = egui::Stroke::NONE;
        w.active.weak_bg_fill = egui::Color32::from_gray(74);
    }
    let resp = ui.add(btn);
    ui.visuals_mut().widgets = prev_widgets;
    ui.spacing_mut().button_padding = prev_padding;
    resp
}

/// A count badge tucked inside the right edge of a nav tile, drawn as its own
/// clickable button on top of the tile that `host` describes. Used for the
/// "New" pill inside "Library": fresh imports are a subset of the catalog,
/// not a sibling of it, so the affordance lives *in* the tile rather than
/// stealing a second one. Returns the badge's own `Response` — hit-tested
/// before the tile underneath, so a click on the pill selects the recent view
/// and never falls through to the whole catalog.
pub(crate) fn nav_tile_badge(
    ui: &mut egui::Ui,
    host: egui::Rect,
    label: &str,
    selected: bool,
) -> egui::Response {
    // Sized off the tile so the pill stays visually inset at every tier: a
    // right gutter matching the tile's left one, and a height that leaves the
    // tile's fill visible above and below.
    const GUTTER: f32 = 8.0;
    let height = (host.height() - 16.0).clamp(18.0, 24.0);
    let font = egui::FontId::proportional(12.0);
    let text_w = ui
        .fonts(|f| f.layout_no_wrap(label.to_string(), font.clone(), egui::Color32::WHITE))
        .size()
        .x;
    let width = text_w + 16.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            host.right() - GUTTER - width,
            host.center().y - height / 2.0,
        ),
        egui::vec2(width, height),
    );
    // Claimed with `Sense::click` at this exact rect so it sits above the tile
    // in the interaction stack; the tile was added first, so the badge wins.
    let resp = ui.interact(rect, ui.id().with("nav-tile-badge"), egui::Sense::click());
    // Selected: solid accent. Otherwise a muted chip that lifts on hover, so it
    // reads as a control rather than a static count.
    let fill = if selected {
        NAV_ACCENT
    } else if resp.hovered() {
        egui::Color32::from_gray(96)
    } else {
        egui::Color32::from_gray(76)
    };
    ui.painter()
        .rect_filled(rect, egui::Rounding::same(height / 2.0), fill);
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        label,
        font,
        egui::Color32::WHITE,
    );
    if resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp
}

/// Render the children of `parent` in the sidebar tree, recursing into folders.
/// Folders are collapsible; playlists are selectable rows that double as
/// drag-and-drop targets for table rows. Plain-field state (`view`, `renaming`)
/// is mutated in place; catalog edits are funneled through `action`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_playlist_nodes(
    ui: &mut egui::Ui,
    density: NavDensity,
    all: &[Playlist],
    parent: Option<Id>,
    volumes: &[ordnung_core::usb::UsbVolume],
    view: &mut LibraryView,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    for p in all.iter().filter(|p| p.parent == parent) {
        // While this entry is being (re)named, show the inline editor in place
        // of its normal row — this is what makes a just-created playlist editable.
        if draw_inline_rename(ui, p, renaming, action) {
            continue;
        }
        if p.is_folder {
            if density.icons_only() {
                // A collapsing header is a disclosure triangle plus a name, and
                // the rail has room for neither. Folders flatten to their
                // playlists here: the rail lists the things you actually
                // navigate to, and the hierarchy comes back at the wider tiers.
                draw_playlist_nodes(
                    ui,
                    density,
                    all,
                    Some(p.id),
                    volumes,
                    view,
                    renaming,
                    action,
                );
                continue;
            }
            egui::CollapsingHeader::new(
                egui::RichText::new(p.name.as_str()).font(crate::ui::tokens::font::body()),
            )
            .id_salt(("pl-folder", p.id))
            .default_open(true)
            .show(ui, |ui| {
                draw_playlist_nodes(
                    ui,
                    density,
                    all,
                    Some(p.id),
                    volumes,
                    view,
                    renaming,
                    action,
                );
            })
            .header_response
            .context_menu(|ui| folder_context_menu(ui, p, volumes, renaming, action));
            playlist_row_gap(ui, density);
        } else {
            draw_playlist_leaf(ui, density, p, volumes, view, renaming, action);
        }
    }
}

/// Shared "Export to <device>…" items: one per mounted volume, or a disabled
/// hint when nothing is plugged in. Used by both playlist and folder menus.
fn export_menu_items(
    ui: &mut egui::Ui,
    id: Id,
    volumes: &[ordnung_core::usb::UsbVolume],
    action: &mut Option<SidebarAction>,
) {
    if volumes.is_empty() {
        ui.add_enabled(false, egui::Button::new("Export to device (none mounted)"));
        return;
    }
    for v in volumes {
        if ui
            .button(format!("⇪ Export to {}…", v.name))
            .on_hover_note("Write this selection to the stick as a native rekordbox export")
            .clicked()
        {
            *action = Some(SidebarAction::ExportPlaylist(id, v.path.clone()));
            ui.close_menu();
        }
    }
}

/// If `p` is the entry currently being renamed, draw its inline text editor and
/// return `true` (the caller should skip drawing the normal row).
///
/// Edit resolves when the box loses focus — by pressing Enter, or by clicking
/// anywhere else (another row or the empty navigation area):
///   * Escape always cancels. A freshly created row is removed; an existing one
///     keeps its old name.
///   * A non-empty name commits the rename — so typing something then clicking
///     out or pressing Enter saves it, it is never discarded.
///   * A blank name on a freshly created row removes it (the accidental "+" you
///     clicked away from disappears).
pub(crate) fn draw_inline_rename(
    ui: &mut egui::Ui,
    p: &Playlist,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) -> bool {
    let Some(state) = renaming.as_mut().filter(|s| !s.usb && s.id == p.id) else {
        return false;
    };
    let hint = if p.is_folder {
        "New folder"
    } else {
        "New playlist"
    };
    let Some(outcome) = inline_rename_editor(ui, state, hint) else {
        return true;
    };
    let is_new = state.is_new;
    match outcome {
        Some(name) => *action = Some(SidebarAction::Rename(p.id, name)),
        // Cancel: discard a just-created row, keep an existing one untouched.
        None if is_new => *action = Some(SidebarAction::Delete(p.id)),
        None => {}
    }
    *renaming = None;
    true
}

/// The device-tree twin of [`draw_inline_rename`]: same editor, same resolve
/// rules, but the ids belong to the stick's rekordbox tree and the resulting
/// actions write to the device.
fn draw_usb_inline_rename(
    ui: &mut egui::Ui,
    p: &ordnung_rbdb::pdb::RbPlaylist,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) -> bool {
    let Some(state) = renaming.as_mut().filter(|s| s.usb && s.id == p.id as Id) else {
        return false;
    };
    let hint = if p.is_folder {
        "New folder"
    } else {
        "New playlist"
    };
    let Some(outcome) = inline_rename_editor(ui, state, hint) else {
        return true;
    };
    let is_new = state.is_new;
    match outcome {
        Some(name) => *action = Some(SidebarAction::RenameUsbPlaylist(p.id, name)),
        None if is_new => *action = Some(SidebarAction::DeleteUsbPlaylist(p.id)),
        None => {}
    }
    *renaming = None;
    true
}

/// The shared rename text box. Returns `None` while the edit is still open,
/// `Some(Some(name))` when a non-empty name was committed, and `Some(None)`
/// when the edit was cancelled (Escape) or left blank — the caller decides
/// what cancellation means for its tree.
fn inline_rename_editor(
    ui: &mut egui::Ui,
    state: &mut Renaming,
    hint: &str,
) -> Option<Option<String>> {
    // Inset the editor a few px on each side so its rounded focus ring sits inside
    // the panel's clip boundary — at full width the blue outline lands on the edge
    // and gets clipped, leaving the "cut off" look. The inner margin gives the text
    // tile-like padding and lifts the box to roughly the height of a nav row.
    let avail = ui.available_width();
    let resp = ui
        .horizontal(|ui| {
            ui.add_space(3.0);
            ui.add(
                egui::TextEdit::singleline(&mut state.buf)
                    .hint_text(hint)
                    .desired_width(avail - 6.0)
                    .margin(egui::Margin::symmetric(10.0, 7.0)),
            )
        })
        .inner;
    // Grab focus only on the first frame the box appears. Re-requesting it every
    // frame would pin focus to the box and make clicking away impossible.
    if state.needs_focus {
        resp.request_focus();
        state.needs_focus = false;
    }
    if !resp.lost_focus() {
        return None;
    }
    let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
    let name = state.buf.trim().to_string();
    if escaped || name.is_empty() {
        Some(None)
    } else {
        Some(Some(name))
    }
}

// ── Shared playlist-row metrics ───────────────────────────────────────────────
// One set of numbers for a playlist row wherever it appears — the catalog tree
// and a USB device's rekordbox tree — so the two sources read as the same
// pattern rather than two sidebars that happen to share a panel.

/// Tile height of a playlist row at the captioned tier.
pub(crate) const PLAYLIST_ROW_H: f32 = 34.0;
/// Label point size of a playlist row.
pub(crate) const PLAYLIST_TEXT_SIZE: f32 = 13.5;
/// Inset of the track count from the tile's right edge.
const PLAYLIST_COUNT_INSET: f32 = 12.0;
/// Gap under a playlist row. The rail's square targets need slightly more
/// separation to read as distinct than full-width bars do.
const PLAYLIST_ROW_GAP: f32 = 3.0;
const PLAYLIST_ROW_GAP_RAIL: f32 = 4.0;

/// One playlist row, shared by both trees: a "♪" tile with the name truncated
/// clear of the count lane and the track count painted inside the right end.
/// At the rail tier the row collapses to a glyph square and the name moves to
/// the tooltip; `note` is the hover note shown at the captioned tier. The
/// trailing row gap is added here so every tree spaces its rows identically.
/// Returns the tile's response so callers wire clicks, drag-and-drop and
/// context menus on top of it.
fn playlist_row(
    ui: &mut egui::Ui,
    density: NavDensity,
    name: &str,
    selected: bool,
    count: usize,
    note: &str,
) -> egui::Response {
    // The rail shows a playlist as a glyph like everything else. Keeping the
    // name here was tried and it is what broke the tier: 56pt cannot hold
    // "Traumprinz", so names wrapped to three lines and no two tiles were the
    // same height. The name goes in the tooltip instead — the rail is for
    // "which one of these did I have open", the wider tiers are for reading.
    let resp = if density.icons_only() {
        rail_tile(ui, "♪", selected).on_hover_text(name)
    } else {
        // The track count is painted over the tile's right end, so the name is
        // truncated to leave that lane clear — otherwise a long name runs
        // straight under the number. `nav_button` reserves the space; the
        // ellipsis tells the user the name is longer than shown, and the
        // tooltip carries it in full.
        nav_button_truncated(
            ui,
            "♪",
            name,
            selected,
            PLAYLIST_ROW_H,
            PLAYLIST_TEXT_SIZE,
            COUNT_LANE,
        )
        .on_hover_text(name)
        .on_hover_note(note)
    };
    if !density.icons_only() {
        // Small right-aligned track count inside the tile. Muted so the name
        // stays the focus; brighter on the accent fill so it's still readable
        // when selected.
        let count_color = if selected {
            egui::Color32::from_white_alpha(170)
        } else {
            egui::Color32::from_gray(130)
        };
        ui.painter().text(
            egui::pos2(
                resp.rect.right() - PLAYLIST_COUNT_INSET,
                resp.rect.center().y,
            ),
            egui::Align2::RIGHT_CENTER,
            count.to_string(),
            crate::ui::tokens::font::footnote(),
            count_color,
        );
    }
    resp
}

/// Trailing gap under a playlist row or folder header, tier-appropriate.
fn playlist_row_gap(ui: &mut egui::Ui, density: NavDensity) {
    ui.add_space(if density.icons_only() {
        PLAYLIST_ROW_GAP_RAIL
    } else {
        PLAYLIST_ROW_GAP
    });
}

/// One playlist row: inline-rename when active, otherwise a selectable label
/// that highlights on drag-hover and adds the dragged tracks when dropped on.
pub(crate) fn draw_playlist_leaf(
    ui: &mut egui::Ui,
    density: NavDensity,
    p: &Playlist,
    volumes: &[ordnung_core::usb::UsbVolume],
    view: &mut LibraryView,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    let selected = *view == LibraryView::Playlist(p.id);
    let resp = playlist_row(
        ui,
        density,
        &p.name,
        selected,
        p.track_ids.len(),
        "Click to view. Drag tracks here to add them",
    );
    if resp.dnd_hover_payload::<DraggedTracks>().is_some() {
        // Inset the highlight so the stroke sits inside the tile's rounded box
        // (drawn on the edge, not floating outside it) and the corners stay round.
        ui.painter().rect_stroke(
            resp.rect.shrink(1.0),
            egui::Rounding::same(6.0),
            egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 150, 220)),
        );
    }
    if let Some(payload) = resp.dnd_release_payload::<DraggedTracks>() {
        if !payload.0.is_empty() {
            *action = Some(SidebarAction::AddTracks(p.id, payload.0.clone()));
        }
    }
    if resp.clicked() {
        *view = LibraryView::Playlist(p.id);
    }
    resp.clone().context_menu(|ui| {
        export_menu_items(ui, p.id, volumes, action);
        if ui
            .button("Save track list…")
            .on_hover_note("Write this playlist's tracks to a text file")
            .clicked()
        {
            *action = Some(SidebarAction::SavePlaylistText(p.id));
            ui.close_menu();
        }
        ui.separator();
        if ui.button("Rename").clicked() {
            *renaming = Some(Renaming {
                id: p.id,
                usb: false,
                buf: p.name.clone(),
                is_new: false,
                needs_focus: true,
            });
            ui.close_menu();
        }
        if ui.button("Delete").clicked() {
            *action = Some(SidebarAction::Delete(p.id));
            ui.close_menu();
        }
    });
    playlist_row_gap(ui, density);
}

/// Render one level of a USB device's rekordbox playlist tree in the sidebar,
/// recursing into folders. Clicking a playlist filters the device view to
/// that playlist's tracks (in export order); the tree is also editable in
/// place — rename/delete from the context menu, device tracks dropped onto a
/// playlist append to it — and every edit is written straight back to the
/// stick's export databases. `parent` is the pdb node id whose children are
/// drawn (`0` = top level).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_usb_playlist_nodes(
    ui: &mut egui::Ui,
    density: NavDensity,
    all: &[ordnung_rbdb::pdb::RbPlaylist],
    tracks_by_playlist: &HashMap<u32, Vec<usize>>,
    parent: u32,
    vol: &Path,
    view: &mut LibraryView,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    for p in all.iter().filter(|p| p.parent_id == parent) {
        if draw_usb_inline_rename(ui, p, renaming, action) {
            continue;
        }
        if p.is_folder {
            if density.icons_only() {
                // Flattened in the rail, as with catalog folders above.
                draw_usb_playlist_nodes(
                    ui,
                    density,
                    all,
                    tracks_by_playlist,
                    p.id,
                    vol,
                    view,
                    renaming,
                    action,
                );
                continue;
            }
            // Same header style as the catalog's folders; only the open
            // default differs — a device tree is a big imported hierarchy, so
            // its folders start collapsed.
            egui::CollapsingHeader::new(
                egui::RichText::new(p.name.as_str()).font(crate::ui::tokens::font::body()),
            )
            .id_salt(("usb-pl-folder", vol, p.id))
            .default_open(false)
            .show(ui, |ui| {
                draw_usb_playlist_nodes(
                    ui,
                    density,
                    all,
                    tracks_by_playlist,
                    p.id,
                    vol,
                    view,
                    renaming,
                    action,
                );
            })
            .header_response
            .context_menu(|ui| {
                if ui.button("New playlist here").clicked() {
                    *action = Some(SidebarAction::NewUsbPlaylist(p.id));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Rename").clicked() {
                    *renaming = Some(Renaming {
                        id: p.id as Id,
                        usb: true,
                        buf: p.name.clone(),
                        is_new: false,
                        needs_focus: true,
                    });
                    ui.close_menu();
                }
                if ui
                    .button("Delete folder")
                    .on_hover_note("Removes this folder and everything in it from the device")
                    .clicked()
                {
                    *action = Some(SidebarAction::DeleteUsbPlaylist(p.id));
                    ui.close_menu();
                }
            });
            playlist_row_gap(ui, density);
        } else {
            let selected = *view == LibraryView::Usb(vol.to_path_buf(), Some(p.id));
            let resp = playlist_row(
                ui,
                density,
                &p.name,
                selected,
                tracks_by_playlist.get(&p.id).map(Vec::len).unwrap_or(0),
                "Click to view. Drag device tracks here to add them",
            );
            // Device tracks dragged over the row: same landing-zone outline as
            // the catalog tree, and the drop appends them on the stick.
            if resp.dnd_hover_payload::<crate::DraggedUsbTracks>().is_some() {
                ui.painter().rect_stroke(
                    resp.rect.shrink(1.0),
                    egui::Rounding::same(6.0),
                    egui::Stroke::new(1.5, egui::Color32::from_rgb(90, 150, 220)),
                );
            }
            if let Some(payload) = resp.dnd_release_payload::<crate::DraggedUsbTracks>() {
                if !payload.0.is_empty() {
                    *action = Some(SidebarAction::AddUsbTracksToPlaylist(p.id, payload.0.clone()));
                }
            }
            if resp.clicked() {
                *view = LibraryView::Usb(vol.to_path_buf(), Some(p.id));
            }
            resp.context_menu(|ui| {
                if ui
                    .button("⤵ Import to library…")
                    .on_hover_note(
                        "Copy this playlist's tracks into the library and recreate the playlist",
                    )
                    .clicked()
                {
                    *action = Some(SidebarAction::ImportUsbPlaylist(p.id));
                    ui.close_menu();
                }
                if ui
                    .button("Save track list…")
                    .on_hover_note("Write this playlist's tracks to a text file")
                    .clicked()
                {
                    *action = Some(SidebarAction::SaveUsbPlaylistText(p.id));
                    ui.close_menu();
                }
                ui.separator();
                if ui.button("Rename").clicked() {
                    *renaming = Some(Renaming {
                        id: p.id as Id,
                        usb: true,
                        buf: p.name.clone(),
                        is_new: false,
                        needs_focus: true,
                    });
                    ui.close_menu();
                }
                if ui
                    .button("Delete")
                    .on_hover_note("Removes this playlist from the device. Files stay")
                    .clicked()
                {
                    *action = Some(SidebarAction::DeleteUsbPlaylist(p.id));
                    ui.close_menu();
                }
            });
            playlist_row_gap(ui, density);
        }
    }
}

/// The "PLAYLISTS" caption row with its right-aligned "+" button, shared by
/// the catalog group and the device group so both sources carry the same
/// affordance. Returns `true` when "+" was clicked; `tip` is its hover note.
pub(crate) fn playlists_header(ui: &mut egui::Ui, tip: &str) -> bool {
    let mut clicked = false;
    ui.horizontal(|ui| {
        ui.label(
            egui::RichText::new("PLAYLISTS")
                .font(crate::ui::tokens::font::footnote())
                .color(egui::Color32::from_gray(140))
                .strong(),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            // Hold the button off the panel's right clip edge so its hover
            // outline isn't cut off.
            ui.add_space(3.0);
            // Compact square button — without an explicit min_size the "+"
            // reads as a stretched pill.
            if ui
                .add(
                    egui::Button::new("+")
                        .min_size(egui::vec2(22.0, 22.0))
                        .rounding(egui::Rounding::same(6.0)),
                )
                .on_hover_note(tip)
                .clicked()
            {
                clicked = true;
            }
        });
    });
    ui.add_space(4.0);
    clicked
}

/// The sidebar's source tabs: the local catalog ("Library") and each mounted
/// removable volume, drawn as a slim strip of abutting browser-style tabs above
/// the library group — the same shape as the vinyl view's Collection/Wantlist
/// tabs, one step smaller, because this is navigation chrome rather than a page
/// header. Only drawn while a stick is mounted, so the sidebar's default look
/// is unchanged the rest of the time. Each tab gets an equal share of the strip
/// and truncates with an ellipsis, since volume names are user-controlled and
/// unbounded.
///
/// The Library tab doubles as a drop target for device rows: dragging tracks
/// off a stick onto it copies them into the library (see
/// [`crate::DraggedUsbTracks`]) — the same gesture as dragging files into a
/// crate, pointed at the whole collection.
pub(crate) fn source_tabs(
    ui: &mut egui::Ui,
    volumes: &[ordnung_core::usb::UsbVolume],
    active_vol: Option<&Path>,
) -> SourceTabsResponse {
    use crate::ui::tokens::{color, font, radius, space};

    let n = (volumes.len() + 1) as f32;
    // Each tab's outer budget: an equal share of the strip, minus the seams.
    let budget = ((ui.available_width() - space::S1 * (n - 1.0)) / n).max(44.0);

    let tab = |ui: &mut egui::Ui, label: &str, active: bool, tip: &str| -> egui::Response {
        let text_font = if active {
            font::strong(font::callout().size)
        } else {
            font::callout()
        };
        // Truncate the label to this tab's share of the strip, the same way
        // playlist tiles do — a long volume name must not push "Library" out.
        let inner = budget - space::S3 * 2.0;
        let text_w = |s: &str| {
            ui.fonts(|f| s.chars().map(|c| f.glyph_width(&text_font, c)).sum::<f32>())
        };
        let shown = if text_w(label) <= inner {
            label.to_string()
        } else {
            let mut cut = label.to_string();
            while !cut.is_empty() && text_w(&format!("{cut}…")) > inner {
                cut.pop();
            }
            format!("{}…", cut.trim_end())
        };
        // Colour is overridden at paint time (hover lifts an inactive label).
        let galley = ui.painter().layout_no_wrap(shown, text_font, color::LABEL);
        let size = egui::vec2(
            galley.size().x + space::S3 * 2.0,
            galley.size().y + space::S2 * 2.0,
        );
        let (rect, resp) = ui.allocate_exact_size(size, egui::Sense::click());
        // Top-rounded only, seated on the hairline drawn under the strip.
        let rounding = egui::Rounding {
            nw: radius::SM,
            ne: radius::SM,
            sw: 0.0,
            se: 0.0,
        };
        let fill = if active {
            Some(color::SURFACE_HI)
        } else if resp.hovered() {
            Some(color::SURFACE)
        } else {
            None
        };
        if let Some(fill) = fill {
            ui.painter().rect_filled(rect, rounding, fill);
        }
        if active {
            let y = rect.bottom() - 1.0;
            ui.painter().line_segment(
                [
                    egui::pos2(rect.left() + space::S2, y),
                    egui::pos2(rect.right() - space::S2, y),
                ],
                egui::Stroke::new(2.0, color::ACCENT),
            );
        }
        let text_pos = rect.center() - galley.size() * 0.5;
        let ink = if active || resp.hovered() {
            color::LABEL
        } else {
            color::LABEL_2
        };
        ui.painter().galley(text_pos, galley, ink);
        resp.on_hover_note(tip)
    };

    let mut clicked = None;
    let mut dropped = None;
    let strip = ui
        .horizontal(|ui| {
            let prev_spacing = ui.spacing().item_spacing.x;
            ui.spacing_mut().item_spacing.x = space::S1;
            let lib = tab(
                ui,
                "Library",
                active_vol.is_none(),
                "Your catalog and playlists. Drop device tracks here to copy \
                 them in",
            );
            if lib.clicked() {
                clicked = Some(None);
            }
            // Device rows dragged over the tab: outline it as a landing zone,
            // and take the payload on release.
            if lib
                .dnd_hover_payload::<crate::DraggedUsbTracks>()
                .is_some()
            {
                ui.painter().rect_stroke(
                    lib.rect,
                    egui::Rounding::same(radius::SM),
                    egui::Stroke::new(1.5, color::ACCENT),
                );
            }
            if let Some(payload) = lib.dnd_release_payload::<crate::DraggedUsbTracks>() {
                dropped = Some(payload.0.clone());
            }
            for v in volumes {
                let tip = if v.is_rekordbox_export {
                    "This USB's files and rekordbox playlists"
                } else {
                    "This volume's files"
                };
                if tab(ui, &v.name, active_vol == Some(v.path.as_path()), tip).clicked() {
                    clicked = Some(Some(v.path.clone()));
                }
            }
            ui.spacing_mut().item_spacing.x = prev_spacing;
        })
        .response
        .rect;
    // The hairline the active tab seats on, spanning the panel so the strip
    // reads as one shelf edge rather than a row of floating chips.
    let y = strip.bottom();
    ui.painter().line_segment(
        [
            egui::pos2(ui.max_rect().left(), y),
            egui::pos2(ui.max_rect().right(), y),
        ],
        egui::Stroke::new(1.0, crate::ui::tokens::color::SEPARATOR_OPAQUE),
    );
    SourceTabsResponse { clicked, dropped }
}

/// What the source-tab strip reported this frame.
pub(crate) struct SourceTabsResponse {
    /// Clicked target: `None` is the catalog, `Some(path)` a volume.
    pub clicked: Option<Option<PathBuf>>,
    /// Device rows dropped onto the Library tab — a request to copy them in.
    pub dropped: Option<Vec<Id>>,
}

pub(crate) fn folder_context_menu(
    ui: &mut egui::Ui,
    p: &Playlist,
    volumes: &[ordnung_core::usb::UsbVolume],
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    if ui.button("New playlist here").clicked() {
        *action = Some(SidebarAction::NewPlaylist(Some(p.id)));
        ui.close_menu();
    }
    ui.separator();
    export_menu_items(ui, p.id, volumes, action);
    ui.separator();
    if ui.button("Rename").clicked() {
        *renaming = Some(Renaming {
            id: p.id,
            usb: false,
            buf: p.name.clone(),
            is_new: false,
            needs_focus: true,
        });
        ui.close_menu();
    }
    if ui.button("Delete folder").clicked() {
        *action = Some(SidebarAction::Delete(p.id));
        ui.close_menu();
    }
}

// ── Sidebar width tiers ───────────────────────────────────────────────────────
// The sidebar isn't a free splitter. Dragging it anywhere in between only ever
// produced half-truncated labels, so the panel locks into two designed
// layouts. Each tier is a real layout, not the same layout at a different size,
// and each is sized from the content it has to hold:
//
//   Rail (56pt) — a navigation rail, not a squeezed sidebar. Uniform centred
//     square targets, one glyph each, no text at all: names live in the
//     tooltip. Text was tried here and it is what made the tier unusable —
//     56pt cannot hold "Traumprinz", so names wrapped into two- and three-line
//     blocks and every tile became a different height. A rail's whole value is
//     a predictable column of identical targets, so the rule is absolute:
//     nothing in this tier renders a string.
//
//   Narrow (212pt) — the default, and the only captioned layout. One
//     full-width tile per row, single-line. A third, wider tier used to sit
//     above this one; it earned its width only by putting the "Library" /
//     "New" pair on a shared row, and once "New" became a badge inside the
//     "Library" tile there was nothing left for the extra 76pt to do but
//     stretch the same rows. The width here is set by the one string that
//     actually varies — a playlist name — leaving a useful prefix beside its
//     glyph and track count before the ellipsis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum NavDensity {
    Icon,
    Narrow,
}

impl NavDensity {
    /// Snapped panel width for this tier, in points.
    pub(crate) fn width(self) -> f32 {
        match self {
            // 56 = 40pt target + 8pt gutter each side. Narrower than the old
            // 64 and deliberately so: it reads as a rail rather than as a
            // sidebar that lost an argument with the drag handle.
            NavDensity::Icon => 56.0,
            NavDensity::Narrow => 212.0,
        }
    }

    /// Side length of the square target in the rail tier, and the row height
    /// every rail tile shares. One constant so the rail is a single uniform
    /// column — the property that makes a rail scannable at all.
    pub(crate) const RAIL_TILE: f32 = 40.0;

    /// The tier a drag to width `w` should select, given the tier currently in
    /// force. The panel is pinned to a tier width at every instant, so the
    /// pointer sits *away* from the panel edge for most of a drag and a plain
    /// nearest-match would strobe between two layouts whenever it hovered near
    /// a boundary. `self` therefore holds until the pointer is decisively into
    /// a neighbour.
    ///
    /// How far "decisively" is depends on which way you're going, because the
    /// two directions aren't equally costly to get wrong. Widening is the
    /// cheap, common intent — you want the labels back — and an accidental
    /// widen is obvious and instantly undone. Collapsing to the rail throws
    /// away every caption, so it stays deliberate. An equal split also *reads*
    /// unequal from the rail: a symmetric 33% meant pulling 130pt off a 56pt
    /// panel, more than twice the panel's own width, before anything happened.
    /// So widening commits just past the panel's own edge, while collapsing
    /// keeps the full hold.
    pub(crate) fn dragged_to(self, w: f32) -> Self {
        /// Fraction of the gap past the midpoint needed to collapse toward the
        /// rail — the destructive direction, held deliberately far.
        const STICK_SHRINK: f32 = 0.33;
        /// Widening instead commits *before* the midpoint: a quarter of the way
        /// out from the current edge, so a short confident pull is enough.
        const REACH_GROW: f32 = 0.25;
        // Deliberately *not* keyed off `nearest`: the whole point of an
        // asymmetric widen is to fire while the pointer is still nearer the
        // rail than the wide tier, which an early `near == self` return would
        // swallow. Each direction is tested against its own threshold instead.
        let all = [NavDensity::Icon, NavDensity::Narrow];
        // The next tier out, and the next tier in, from where we are now.
        let wider = all.iter().copied().find(|t| t.width() > self.width());
        let narrower = all.iter().copied().rev().find(|t| t.width() < self.width());
        if let Some(t) = wider {
            let gap = t.width() - self.width();
            if w > self.width() + gap * REACH_GROW {
                return t;
            }
        }
        if let Some(t) = narrower {
            let gap = self.width() - t.width();
            let midpoint = (self.width() + t.width()) / 2.0;
            if w < midpoint - gap * STICK_SHRINK {
                return t;
            }
        }
        self
    }

    /// Icon tier hides every text label except playlist names.
    pub(crate) fn icons_only(self) -> bool {
        self == NavDensity::Icon
    }

    /// Parse the persisted `Config::nav_density` key; anything unrecognised
    /// falls back to the designed default.
    pub(crate) fn from_key(key: &str) -> Self {
        // Anything else — including the retired "wide" key still sitting in
        // existing configs — lands on the default.
        match key {
            "icon" => NavDensity::Icon,
            _ => NavDensity::Narrow,
        }
    }

    /// The config key for this tier.
    pub(crate) fn key(self) -> &'static str {
        match self {
            NavDensity::Icon => "icon",
            NavDensity::Narrow => "narrow",
        }
    }
}

/// A nav tile that collapses to its glyph at the rail tier. `icon` is the
/// leading glyph, `label` the text that follows it at the wider tiers.
///
/// At the rail tier the label is dropped entirely and the glyph is centred in
/// a uniform square (see [`NavDensity::RAIL_TILE`]) — the caller's `height` and
/// `text_size` are deliberately ignored there, because a rail whose rows are
/// different heights is exactly the failure this tier exists to avoid. The name
/// moves to the tooltip, which is the only place it fits.
pub(crate) fn nav_button_dense(
    ui: &mut egui::Ui,
    density: NavDensity,
    icon: &str,
    label: &str,
    selected: bool,
    height: f32,
    text_size: f32,
) -> egui::Response {
    if density.icons_only() {
        // The wide tiers rank tiles by height and text size; the rail has only
        // one square, so carry that ranking over as glyph size instead. The
        // tall tiles (the top-level libraries) are the ones worth enlarging.
        let glyph = if height >= 40.0 {
            RAIL_GLYPH_LEAD
        } else {
            RAIL_GLYPH
        };
        rail_tile_sized(ui, icon, selected, glyph)
    } else {
        nav_button(ui, &format!("{icon}  {label}"), selected, height, text_size)
    }
}

/// Width reserved at the right end of a playlist tile for its track count, so
/// the name is laid out in what's left rather than running under the number.
/// Fits a four-digit count plus the 12pt inset the count is painted at.
pub(crate) const COUNT_LANE: f32 = 44.0;

/// A nav tile whose label is truncated with an ellipsis to fit the tile width
/// minus `reserve` — the lane kept clear for something painted over the tile's
/// right end (a track count). Without this the label is laid out at its natural
/// width and simply collides with whatever is painted there.
#[allow(clippy::too_many_arguments)]
pub(crate) fn nav_button_truncated(
    ui: &mut egui::Ui,
    icon: &str,
    label: &str,
    selected: bool,
    height: f32,
    text_size: f32,
    reserve: f32,
) -> egui::Response {
    let w = ui.available_width();
    // Budget for the text itself: the tile minus its left gutter, the glyph and
    // the reserved lane.
    let budget = (w - 12.0 - 18.0 - reserve).max(24.0);
    let font = egui::FontId::proportional(text_size);
    let text_w = |s: &str| ui.fonts(|f| s.chars().map(|c| f.glyph_width(&font, c)).sum::<f32>());
    let shown = if text_w(label) <= budget {
        label.to_string()
    } else {
        // Trim from the end until the name plus its ellipsis fits.
        let mut cut = label.to_string();
        while !cut.is_empty() && text_w(&format!("{cut}…")) > budget {
            cut.pop();
        }
        format!("{}…", cut.trim_end())
    };
    nav_button_sized(
        ui,
        &format!("{icon}  {shown}"),
        selected,
        w,
        height,
        text_size,
    )
}

/// A nav tile whose leading mark is *painted* rather than set as a font glyph
/// (see [`crate::ui::icon`]), for the entries that deserve a real icon instead
/// of whatever the font stack supplies for an emoji. The label is indented past
/// the mark so the two don't overlap; at the rail tier there is no label and the
/// mark is simply centred in the square.
///
/// `draw` is handed the mark's centre and radius, matching the signature every
/// icon in `ui::icon` already has.
pub(crate) fn nav_button_painted(
    ui: &mut egui::Ui,
    density: NavDensity,
    draw: impl Fn(&egui::Painter, egui::Pos2, egui::Color32, f32),
    label: &str,
    selected: bool,
    height: f32,
    text_size: f32,
) -> egui::Response {
    // Radius of the painted mark. The rail gets the larger one for the same
    // reason its lead glyphs are larger: it is the only ranking left once the
    // captions are gone.
    let r = if density.icons_only() { 9.0 } else { 7.5 };
    let resp = if density.icons_only() {
        rail_tile(ui, "", selected)
    } else {
        // Indent the label with spaces to clear the mark, which is painted over
        // the tile afterwards. Crude, but it keeps every nav tile going through
        // the one `nav_button` path rather than forking the layout.
        nav_button(ui, &format!("       {label}"), selected, height, text_size)
    };
    let cx = if density.icons_only() {
        resp.rect.center().x
    } else {
        resp.rect.left() + 12.0 + r
    };
    // The mark follows the label's colour: white on the accent fill when this
    // tile is the current view, dimmer otherwise.
    let col = if selected {
        egui::Color32::WHITE
    } else {
        egui::Color32::from_gray(190)
    };
    draw(ui.painter(), egui::pos2(cx, resp.rect.center().y), col, r);
    resp
}

/// One square target in the rail tier: a glyph centred in a fixed
/// [`NavDensity::RAIL_TILE`] box, itself centred in the rail's width. Every
/// rail tile is this exact size, whatever it stands for, so the rail is a
/// predictable column your eye can run down.
pub(crate) fn rail_tile(ui: &mut egui::Ui, icon: &str, selected: bool) -> egui::Response {
    rail_tile_sized(ui, icon, selected, RAIL_GLYPH)
}

/// Glyph point size for an ordinary rail tile (a playlist, a USB volume).
pub(crate) const RAIL_GLYPH: f32 = 17.0;

/// Glyph point size for the rail's primary destinations — "Library" and the
/// vinyl shelf. At the icon tier every tile is the same square with no caption
/// to rank it, so size is the only thing left to say which entries are the
/// top-level libraries and which are the list of playlists under them.
pub(crate) const RAIL_GLYPH_LEAD: f32 = 24.0;

/// [`rail_tile`] with an explicit glyph size, so the rail can rank its entries.
pub(crate) fn rail_tile_sized(
    ui: &mut egui::Ui,
    icon: &str,
    selected: bool,
    glyph: f32,
) -> egui::Response {
    let side = NavDensity::RAIL_TILE;
    // Centre the square in the rail rather than letting it sit flush left: the
    // gutter is what stops the tiles reading as a clipped-off wider sidebar.
    let indent = ((ui.available_width() - side) / 2.0).max(0.0);
    ui.horizontal(|ui| {
        ui.add_space(indent);
        nav_button_sized(ui, icon, selected, side, side, glyph)
    })
    .inner
}
