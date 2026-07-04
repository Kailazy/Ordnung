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
/// available space — used when two tiles share a row (e.g. the big "All songs"
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
    ui.spacing_mut().button_padding.x = 12.0;

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

/// Render the children of `parent` in the sidebar tree, recursing into folders.
/// Folders are collapsible; playlists are selectable rows that double as
/// drag-and-drop targets for table rows. Plain-field state (`view`, `renaming`)
/// is mutated in place; catalog edits are funneled through `action`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_playlist_nodes(
    ui: &mut egui::Ui,
    all: &[Playlist],
    parent: Option<Id>,
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
            egui::CollapsingHeader::new(egui::RichText::new(p.name.as_str()).size(13.5))
                .id_salt(("pl-folder", p.id))
                .default_open(true)
                .show(ui, |ui| {
                    draw_playlist_nodes(ui, all, Some(p.id), view, renaming, action);
                })
                .header_response
                .context_menu(|ui| folder_context_menu(ui, p, renaming, action));
            ui.add_space(3.0);
        } else {
            draw_playlist_leaf(ui, p, view, renaming, action);
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
    let Some(state) = renaming.as_mut().filter(|s| s.id == p.id) else {
        return false;
    };
    let hint = if p.is_folder {
        "New folder"
    } else {
        "New playlist"
    };
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
    if resp.lost_focus() {
        let escaped = ui.input(|i| i.key_pressed(egui::Key::Escape));
        let name = state.buf.trim().to_string();
        if escaped {
            // Cancel: discard a just-created row, keep an existing one untouched.
            if state.is_new {
                *action = Some(SidebarAction::Delete(p.id));
            }
        } else if !name.is_empty() {
            *action = Some(SidebarAction::Rename(p.id, name));
        } else if state.is_new {
            *action = Some(SidebarAction::Delete(p.id));
        }
        *renaming = None;
    }
    true
}

/// One playlist row: inline-rename when active, otherwise a selectable label
/// that highlights on drag-hover and adds the dragged tracks when dropped on.
pub(crate) fn draw_playlist_leaf(
    ui: &mut egui::Ui,
    p: &Playlist,
    view: &mut LibraryView,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    let selected = *view == LibraryView::Playlist(p.id);
    let resp = nav_button(ui, &format!("♪  {}", p.name), selected, 30.0, 13.5)
        .on_hover_note("Click to view. Drag tracks here to add them.");
    // Small right-aligned track count inside the tile. Muted so the name stays
    // the focus; brighter on the accent fill so it's still readable when selected.
    let count_color = if selected {
        egui::Color32::from_white_alpha(170)
    } else {
        egui::Color32::from_gray(130)
    };
    ui.painter().text(
        egui::pos2(resp.rect.right() - 12.0, resp.rect.center().y),
        egui::Align2::RIGHT_CENTER,
        p.track_ids.len().to_string(),
        egui::FontId::proportional(11.0),
        count_color,
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
    resp.context_menu(|ui| {
        if ui.button("Rename").clicked() {
            *renaming = Some(Renaming {
                id: p.id,
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
    ui.add_space(3.0);
}

pub(crate) fn folder_context_menu(
    ui: &mut egui::Ui,
    p: &Playlist,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    if ui.button("New playlist here").clicked() {
        *action = Some(SidebarAction::NewPlaylist(Some(p.id)));
        ui.close_menu();
    }
    ui.separator();
    if ui.button("Rename").clicked() {
        *renaming = Some(Renaming {
            id: p.id,
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
