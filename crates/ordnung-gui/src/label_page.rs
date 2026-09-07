//! The label page: one imprint's discography, read front to back.
//!
//! The dig's label thread samples a label — one random find per pull. This is
//! the deliberate version: click a label anywhere (the record sheet's header,
//! a shelf or seller card's menu) and read the whole catalog as a list, with
//! your own shelves marked on every row. For the labels that define a sound —
//! Chain Reaction, Basic Channel, Underground Resistance — reading the run in
//! order *is* the dig.
//!
//! Rides `GET /labels/{id}/releases` (the same browse the dig's label thread
//! uses), 100 rows a page, most releases in catalog order as Discogs returns
//! them. Non-record rows (CDs, files) are dropped; master rows with no format
//! of their own are kept rather than mislabeled. One paced request per page.

use super::*;
use ordnung_core::discogs::{BrowsePage, BrowseRelease, BrowseThread};

/// Window width — versions-panel wide: rows carry year, format and catalog
/// number beside the credit, and wrapping those makes the run unreadable.
const PANEL_W: f32 = 560.0;

/// Side of a row's sleeve thumbnail.
const THUMB: f32 = 48.0;

/// The open label page: which imprint, which page of its run, and the rows.
pub(crate) struct LabelPanel {
    /// Discogs label id — what the browse actually pages. 0 while the id is
    /// still being resolved from the release the page was opened from.
    pub label_id: u64,
    /// Imprint name for the title bar, best known so far (the release's own
    /// label string until the detail resolves the canonical one).
    pub name: String,
    /// 1-based page currently shown (or being fetched).
    pub page: u32,
    /// Total pages Discogs reports, at least 1 once loaded.
    pub pages: u32,
    /// Total releases across the run, as Discogs counts them (all formats).
    pub items: u32,
    /// This page's rows, vinyl-filtered and pressing-deduplicated.
    pub releases: Vec<BrowseRelease>,
    pub loading: bool,
    /// Why there's nothing to show, when there isn't.
    pub error: Option<String>,
}

/// One finished label-page fetch, handed back to the UI thread.
pub(crate) struct LabelFetched {
    /// The label the fetch was for — a result for a panel since re-pointed at
    /// another imprint is dropped.
    pub label_id: u64,
    /// Canonical label name, when the fetch resolved it (the first fetch
    /// does; page turns don't need to).
    pub name: Option<String>,
    pub page: u32,
    pub result: std::result::Result<BrowsePage, String>,
}

/// What a row asked for, applied after the window releases its borrows.
enum Act {
    /// Open this release's sheet — listen before judging the sleeve.
    Open(usize),
    /// Add it to the Discogs wantlist.
    Want(usize),
    /// Set it aside in the crate of interest.
    Crate(usize),
    /// Start a dig from it.
    Dig(usize),
    /// Fetch another page of the run.
    Page(u32),
}

/// Keep the rows a record digger wants: pressings (or format-unknown master
/// rows, which are usually records too), one per *work* — a label page lists
/// the original, the repress and every regional edition separately.
fn crate_rows(page: &BrowsePage) -> Vec<BrowseRelease> {
    let mut seen = HashSet::new();
    page.releases
        .iter()
        .filter(|r| !r.format_known || crate::dig::is_vinyl(&r.format))
        .filter(|r| {
            let (a, t) = crate::dig::row_artist_title(r);
            seen.insert(crate::dig::work_key(&a, &t))
        })
        .cloned()
        .collect()
}

impl App {
    /// Open the label page for the label of `release_id`. The label id rides
    /// the release detail, which is cached for any record whose sheet was
    /// opened — so this usually resolves without a request and spends its one
    /// paced call on the first page of the run. `hint` names the label until
    /// the detail corrects it.
    pub(crate) fn open_label_page(&mut self, release_id: u64, hint: Option<String>) {
        let name = hint.unwrap_or_else(|| "…".to_string());
        self.label_panel = Some(LabelPanel {
            label_id: 0,
            name: name.clone(),
            page: 1,
            pages: 1,
            items: 0,
            releases: Vec::new(),
            loading: true,
            error: None,
        });
        let token = self.discogs_token();
        let db = self.db_path.clone();
        let (tx, rx) = mpsc::channel();
        self.label_rx = Some(rx);
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let id = release_id.to_string();
            let detail = Catalog::open(&db).ok().and_then(|cat| {
                if let Ok(Some(d)) = cat.cached_release(&id) {
                    return Some(d);
                }
                cat.release_cached_or(&id, || client.fetch_release(&id))
                    .ok()
            });
            let (label_id, label_name) = match &detail {
                Some(d) => (d.label_ids.first().copied(), d.label.clone()),
                None => (None, None),
            };
            let Some(label_id) = label_id else {
                let _ = tx.send(LabelFetched {
                    label_id: 0,
                    name: label_name,
                    page: 1,
                    result: Err("Discogs lists no label for this record.".to_string()),
                });
                ctx.request_repaint();
                return;
            };
            let result = client
                .browse_by_id(BrowseThread::Label, label_id, 1)
                .map_err(|e| e.to_string());
            let _ = tx.send(LabelFetched {
                label_id,
                name: label_name,
                page: 1,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Fetch another page of the open label's run.
    fn fetch_label_page(&mut self, page: u32) {
        let Some(panel) = self.label_panel.as_mut() else {
            return;
        };
        if panel.label_id == 0 || panel.loading {
            return;
        }
        let label_id = panel.label_id;
        panel.loading = true;
        panel.page = page;
        let token = self.discogs_token();
        let (tx, rx) = mpsc::channel();
        self.label_rx = Some(rx);
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let result = client
                .browse_by_id(BrowseThread::Label, label_id, page)
                .map_err(|e| e.to_string());
            let _ = tx.send(LabelFetched {
                label_id,
                name: None,
                page,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished label-page fetch onto the open panel.
    pub(crate) fn poll_label_page(&mut self) {
        let Some(rx) = &self.label_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.label_rx = None;
        let Some(panel) = self.label_panel.as_mut() else {
            return;
        };
        // The first fetch is the only one that arrives while the panel still
        // has no id; page turns must match the imprint on screen.
        if panel.label_id != 0 && msg.label_id != panel.label_id {
            return;
        }
        panel.loading = false;
        if msg.label_id != 0 {
            panel.label_id = msg.label_id;
        }
        if let Some(name) = msg.name.filter(|n| !n.trim().is_empty()) {
            panel.name = name;
        }
        match msg.result {
            Ok(page) => {
                panel.page = msg.page;
                panel.pages = page.pages.max(1);
                panel.items = page.items;
                panel.releases = crate_rows(&page);
            }
            Err(e) => panel.error = Some(e),
        }
    }

    /// Draw the open label page, if any.
    pub(crate) fn draw_label_page(&mut self, ctx: &egui::Context) {
        let Some(panel) = self.label_panel.as_ref() else {
            return;
        };
        let (name, page, pages, items, loading, error) = (
            panel.name.clone(),
            panel.page,
            panel.pages,
            panel.items,
            panel.loading,
            panel.error.clone(),
        );
        // Snapshot the rows, then fill sleeves through the shared URL cache
        // (`dig_cover` needs `&mut self`, so two passes like the versions
        // panel).
        struct Row {
            release_id: u64,
            cover: Option<Tex>,
            artist: String,
            title: String,
            /// `1994 · 12" · CR-03`, whichever parts exist.
            sub: String,
            owned: bool,
            wanted: bool,
            crated: bool,
        }
        let specs: Vec<(String, u64, String, String, String)> = panel
            .releases
            .iter()
            .map(|r| {
                let (artist, title) = crate::dig::row_artist_title(r);
                let sub = [
                    r.year.filter(|y| *y > 0).map(|y| y.to_string()),
                    (!r.format.trim().is_empty()).then(|| r.format.clone()),
                    (!r.catno.trim().is_empty()).then(|| r.catno.clone()),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(" · ");
                (
                    r.thumb_url.clone(),
                    r.release_id,
                    crate::dig::strip_disambiguator(&artist).to_string(),
                    title,
                    sub,
                )
            })
            .collect();
        let mut rows: Vec<Row> = Vec::with_capacity(specs.len());
        for (thumb, release_id, artist, title, sub) in specs {
            rows.push(Row {
                release_id,
                cover: (!thumb.trim().is_empty())
                    .then(|| self.dig_cover(&thumb).cloned())
                    .flatten(),
                artist,
                title,
                sub,
                owned: self.vinyl_owned.contains(&release_id),
                wanted: self.vinyl_wanted.contains(&release_id),
                crated: self.interest_ids.contains(&release_id),
            });
        }
        let editing = self.is_busy();

        let mut act: Option<Act> = None;
        let mut open = true;
        egui::Window::new(format!("⌂ {name}"))
            .id(egui::Id::new("label-page"))
            .open(&mut open)
            .collapsible(false)
            .resizable([false, true])
            .default_width(PANEL_W)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                ui.set_min_width(PANEL_W);
                ui.set_max_width(PANEL_W);
                ui.add_space(2.0);
                let blurb = if items > 0 {
                    format!(
                        "The label's whole run, as Discogs lists it — {items} releases, \
                         records only shown. Your shelves are marked on every row."
                    )
                } else {
                    "The label's whole run, as Discogs lists it. Your shelves are \
                     marked on every row."
                        .to_string()
                };
                ui.label(egui::RichText::new(blurb).weak().small());
                ui.add_space(6.0);
                if let Some(e) = &error {
                    ui.label(egui::RichText::new(e).weak());
                    ui.add_space(4.0);
                    return;
                }
                if loading && rows.is_empty() {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(egui::RichText::new("Reading the label's crate…").weak());
                    });
                    ui.add_space(4.0);
                    return;
                }
                if rows.is_empty() {
                    ui.label(egui::RichText::new("No records on this page of the run.").weak());
                }
                egui::ScrollArea::vertical()
                    .max_height(440.0)
                    .show(ui, |ui| {
                        for (i, r) in rows.iter().enumerate() {
                            ui.horizontal(|ui| {
                                let (trect, tresp) = ui.allocate_exact_size(
                                    egui::vec2(THUMB, THUMB),
                                    egui::Sense::click(),
                                );
                                match &r.cover {
                                    Some(t) => {
                                        egui::Image::new(t)
                                            .fit_to_exact_size(egui::vec2(THUMB, THUMB))
                                            .rounding(egui::Rounding::same(4.0))
                                            .paint_at(ui, trect);
                                    }
                                    None => {
                                        ui.painter().rect_filled(
                                            trect,
                                            egui::Rounding::same(4.0),
                                            egui::Color32::from_gray(34),
                                        );
                                    }
                                }
                                if tresp
                                    .on_hover_cursor(egui::CursorIcon::PointingHand)
                                    .on_hover_note("Open the record")
                                    .clicked()
                                {
                                    act = Some(Act::Open(i));
                                }
                                ui.add_space(8.0);
                                ui.vertical(|ui| {
                                    ui.spacing_mut().item_spacing.y = 1.0;
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        let t = ui
                                            .add(
                                                egui::Label::new(
                                                    egui::RichText::new(format!(
                                                        "{} — {}",
                                                        r.artist, r.title
                                                    ))
                                                    .strong(),
                                                )
                                                .truncate()
                                                .sense(egui::Sense::click()),
                                            )
                                            .on_hover_cursor(egui::CursorIcon::PointingHand)
                                            .on_hover_note("Open the record");
                                        if t.clicked() {
                                            act = Some(Act::Open(i));
                                        }
                                        for (show, text) in [
                                            (r.owned, "OWNED"),
                                            (!r.owned && r.wanted, "WANT"),
                                            (r.crated, "CRATE"),
                                        ] {
                                            if show {
                                                ui.label(
                                                    egui::RichText::new(text)
                                                        .small()
                                                        .color(egui::Color32::from_gray(140)),
                                                );
                                            }
                                        }
                                    });
                                    if !r.sub.is_empty() {
                                        ui.label(egui::RichText::new(&r.sub).small().weak());
                                    }
                                });
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("🔍")
                                            .on_hover_note("Dig from this record")
                                            .clicked()
                                        {
                                            act = Some(Act::Dig(i));
                                        }
                                        let parked = r.owned || r.wanted || r.crated;
                                        if ui
                                            .add_enabled(!parked, egui::Button::new("☆").small())
                                            .on_hover_note("Set aside in your crate of interest")
                                            .clicked()
                                        {
                                            act = Some(Act::Crate(i));
                                        }
                                        if ui
                                            .add_enabled(
                                                !parked && !editing,
                                                egui::Button::new("＋").small(),
                                            )
                                            .on_hover_note("Add to your Discogs wantlist")
                                            .clicked()
                                        {
                                            act = Some(Act::Want(i));
                                        }
                                    },
                                );
                            });
                            ui.add_space(3.0);
                            ui.separator();
                            ui.add_space(3.0);
                        }
                    });
                // Pager, only when the run outgrows one page.
                if pages > 1 {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        if ui
                            .add_enabled(page > 1 && !loading, egui::Button::new("←"))
                            .on_hover_note("Earlier in the run")
                            .clicked()
                        {
                            act = Some(Act::Page(page - 1));
                        }
                        if ui
                            .add_enabled(page < pages && !loading, egui::Button::new("→"))
                            .on_hover_note("Later in the run")
                            .clicked()
                        {
                            act = Some(Act::Page(page + 1));
                        }
                        if loading {
                            ui.spinner();
                        }
                        ui.label(egui::RichText::new(format!("Page {page} of {pages}")).weak());
                    });
                }
            });
        if !open {
            self.label_panel = None;
            return;
        }

        let row_of = |i: usize| -> Option<&BrowseRelease> {
            self.label_panel.as_ref().and_then(|p| p.releases.get(i))
        };
        match act {
            Some(Act::Open(i)) => {
                if let Some(r) = row_of(i).cloned() {
                    let (artist, title) = crate::dig::row_artist_title(&r);
                    let sub = [
                        r.year.filter(|y| *y > 0).map(|y| y.to_string()),
                        (!r.format.trim().is_empty()).then(|| r.format.clone()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let thumb = (!r.thumb_url.trim().is_empty()).then(|| r.thumb_url.clone());
                    self.open_release_sheet(r.release_id, artist, title, sub, thumb, ctx);
                }
            }
            Some(Act::Want(i)) => {
                if let Some(r) = row_of(i).cloned() {
                    let (artist, title) = crate::dig::row_artist_title(&r);
                    let edit = VinylEdit::Want {
                        release_ids: vec![r.release_id],
                        label: format!("{artist} — {title}"),
                    };
                    self.request_vinyl_edit(ctx.clone(), edit);
                }
            }
            Some(Act::Crate(i)) => {
                if let Some(r) = row_of(i).cloned() {
                    let (artist, title) = crate::dig::row_artist_title(&r);
                    let label_name = self.label_panel.as_ref().map(|p| p.name.clone());
                    self.crate_record(InterestRecord {
                        release_id: r.release_id,
                        title,
                        artist,
                        year: r.year.filter(|y| *y > 0),
                        label: label_name.clone().filter(|n| n != "…"),
                        catalog_number: (!r.catno.trim().is_empty()).then(|| r.catno.clone()),
                        format: (!r.format.trim().is_empty()).then(|| r.format.clone()),
                        thumb_url: (!r.thumb_url.trim().is_empty()).then(|| r.thumb_url.clone()),
                        via: label_name.map(|n| format!("label page: {n}")),
                        added_at: 0,
                    });
                }
            }
            Some(Act::Dig(i)) => {
                if let Some(r) = row_of(i).cloned() {
                    let (artist, title) = crate::dig::row_artist_title(&r);
                    let sub = [
                        r.year.filter(|y| *y > 0).map(|y| y.to_string()),
                        (!r.format.trim().is_empty()).then(|| r.format.clone()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ");
                    let label_name = self
                        .label_panel
                        .as_ref()
                        .map(|p| p.name.clone())
                        .filter(|n| n != "…");
                    let thumb = (!r.thumb_url.trim().is_empty()).then(|| r.thumb_url.clone());
                    self.start_dig_release(r.release_id, artist, title, label_name, sub, thumb);
                }
            }
            Some(Act::Page(p)) => self.fetch_label_page(p),
            None => {}
        }
    }
}
