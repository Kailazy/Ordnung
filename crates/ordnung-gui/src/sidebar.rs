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
/// "New" pill inside "All songs": fresh imports are a subset of the catalog,
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
                draw_playlist_nodes(ui, density, all, Some(p.id), view, renaming, action);
                continue;
            }
            egui::CollapsingHeader::new(
                egui::RichText::new(p.name.as_str()).font(crate::ui::tokens::font::body()),
            )
            .id_salt(("pl-folder", p.id))
            .default_open(true)
            .show(ui, |ui| {
                draw_playlist_nodes(ui, density, all, Some(p.id), view, renaming, action);
            })
            .header_response
            .context_menu(|ui| folder_context_menu(ui, p, renaming, action));
            ui.add_space(3.0);
        } else {
            draw_playlist_leaf(ui, density, p, view, renaming, action);
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
    density: NavDensity,
    p: &Playlist,
    view: &mut LibraryView,
    renaming: &mut Option<Renaming>,
    action: &mut Option<SidebarAction>,
) {
    let selected = *view == LibraryView::Playlist(p.id);
    // The rail shows a playlist as a glyph like everything else. Keeping the
    // name here was tried and it is what broke the tier: 56pt cannot hold
    // "Traumprinz", so names wrapped to three lines and no two tiles were the
    // same height. The name goes in the tooltip instead — the rail is for
    // "which one of these did I have open", the wider tiers are for reading.
    let resp = if density.icons_only() {
        rail_tile(ui, "♪", selected).on_hover_text(&p.name)
    } else {
        // The track count is painted over the tile's right end, so the name is
        // truncated to leave that lane clear — otherwise a long name runs
        // straight under the number. `nav_button` reserves the space; the
        // ellipsis tells the user the name is longer than shown, and the
        // tooltip carries it in full.
        nav_button_truncated(ui, "♪", &p.name, selected, 34.0, 13.5, COUNT_LANE)
            .on_hover_text(&p.name)
            .on_hover_note("Click to view. Drag tracks here to add them")
    };
    // Small right-aligned track count inside the tile. Muted so the name stays
    // the focus; brighter on the accent fill so it's still readable when selected.
    let count_color = if selected {
        egui::Color32::from_white_alpha(170)
    } else {
        egui::Color32::from_gray(130)
    };
    if !density.icons_only() {
        ui.painter().text(
            egui::pos2(resp.rect.right() - 12.0, resp.rect.center().y),
            egui::Align2::RIGHT_CENTER,
            p.track_ids.len().to_string(),
            crate::ui::tokens::font::footnote(),
            count_color,
        );
    }
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
    // The rail's rows sit tighter than the wide tiers': square targets in a
    // column need less separation to read as distinct than full-width bars do.
    ui.add_space(if density.icons_only() { 4.0 } else { 3.0 });
}

/// Render one level of a USB device's rekordbox playlist tree in the sidebar,
/// recursing into folders. Read-only navigation: clicking a playlist filters
/// the device view to that playlist's tracks (in export order). `parent` is
/// the pdb node id whose children are drawn (`0` = top level).
pub(crate) fn draw_usb_playlist_nodes(
    ui: &mut egui::Ui,
    density: NavDensity,
    all: &[ordnung_rbdb::pdb::RbPlaylist],
    tracks_by_playlist: &HashMap<u32, Vec<usize>>,
    parent: u32,
    vol: &Path,
    view: &mut LibraryView,
) {
    for p in all.iter().filter(|p| p.parent_id == parent) {
        if p.is_folder {
            if density.icons_only() {
                // Flattened in the rail, as with catalog folders above.
                draw_usb_playlist_nodes(ui, density, all, tracks_by_playlist, p.id, vol, view);
                continue;
            }
            egui::CollapsingHeader::new(
                egui::RichText::new(p.name.as_str()).font(crate::ui::tokens::font::callout()),
            )
            .id_salt(("usb-pl-folder", vol, p.id))
            .default_open(false)
            .show(ui, |ui| {
                draw_usb_playlist_nodes(ui, density, all, tracks_by_playlist, p.id, vol, view);
            });
            ui.add_space(2.0);
        } else {
            let selected = *view == LibraryView::Usb(vol.to_path_buf(), Some(p.id));
            let resp = if density.icons_only() {
                rail_tile(ui, "♪", selected).on_hover_text(&p.name)
            } else {
                nav_button_truncated(ui, "♪", &p.name, selected, 30.0, 12.5, COUNT_LANE)
                    .on_hover_text(&p.name)
                    .on_hover_note("Playlist from this device's rekordbox export")
            };
            // Right-aligned track count, mirroring the catalog playlist rows.
            let count_color = if selected {
                egui::Color32::from_white_alpha(170)
            } else {
                egui::Color32::from_gray(130)
            };
            if !density.icons_only() {
                ui.painter().text(
                    egui::pos2(resp.rect.right() - 10.0, resp.rect.center().y),
                    egui::Align2::RIGHT_CENTER,
                    tracks_by_playlist
                        .get(&p.id)
                        .map(Vec::len)
                        .unwrap_or(0)
                        .to_string(),
                    crate::ui::tokens::font::caption(),
                    count_color,
                );
            }
            if resp.clicked() {
                *view = LibraryView::Usb(vol.to_path_buf(), Some(p.id));
            }
            ui.add_space(2.0);
        }
    }
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
//     above this one; it earned its width only by putting the "All songs" /
//     "New" pair on a shared row, and once "New" became a badge inside the
//     "All songs" tile there was nothing left for the extra 76pt to do but
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
        let narrower = all
            .iter()
            .copied()
            .rev()
            .find(|t| t.width() < self.width());
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
    let text_w = |s: &str| {
        ui.fonts(|f| {
            s.chars()
                .map(|c| f.glyph_width(&font, c))
                .sum::<f32>()
        })
    };
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
    nav_button_sized(ui, &format!("{icon}  {shown}"), selected, w, height, text_size)
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

/// Glyph point size for the rail's primary destinations — "All songs" and the
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
