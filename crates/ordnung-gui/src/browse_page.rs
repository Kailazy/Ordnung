//! The browse page: one label's or one artist's discography, read front to back.
//!
//! The dig's artist and label threads sample — one random find per pull. This
//! is the deliberate version: click a label anywhere (the record sheet's
//! header, a shelf or seller card's menu), or an artist in the search box's
//! Discogs results, and read the whole catalog as a list, with your own
//! shelves marked on every row. For the labels that define a sound — Chain
//! Reaction, Basic Channel, Underground Resistance — and for the artists whose
//! every 12" is worth knowing, reading the run in order *is* the dig.
//!
//! Rides `GET /labels/{id}/releases` and `GET /artists/{id}/releases` (the
//! same browses the dig's threads use), 100 rows a page, in the order Discogs
//! returns them — catalog order for a label, by year for an artist. Non-record
//! rows (CDs, files) are dropped; master rows with no format of their own are
//! kept rather than mislabeled. One paced request per page.

use super::*;
use ordnung_core::discogs::{BrowsePage, BrowseRelease, BrowseThread};

/// Window width — versions-panel wide: rows carry year, format and catalog
/// number beside the credit, and wrapping those makes the run unreadable.
const PANEL_W: f32 = 560.0;

/// Side of a row's sleeve thumbnail.
const THUMB: f32 = 48.0;

/// The open browse page: whose run (a label's or an artist's), which page of
/// it, and the rows.
pub(crate) struct BrowsePanel {
    /// Which association the page follows — the imprint's catalog or the
    /// artist's discography. Decides the endpoint, the title glyph and what a
    /// row's caption has room for.
    pub thread: BrowseThread,
    /// Discogs label or artist id — what the browse actually pages. 0 while
    /// the id is still being resolved from the release the page was opened
    /// from (label pages only; an artist page is opened by id).
    pub id: u64,
    /// Name for the title bar, best known so far (a release's own label string
    /// until the detail resolves the canonical one).
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

/// One finished browse-page fetch, handed back to the UI thread.
pub(crate) struct BrowseFetched {
    /// Whose run the fetch was for — a result for a panel since re-pointed at
    /// another label or artist is dropped.
    pub thread: BrowseThread,
    pub id: u64,
    /// Canonical name, when the fetch resolved it (a label page's first fetch
    /// does; page turns and artist pages don't need to).
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
    /// Start a dig from it.
    Dig(usize),
    /// Fetch another page of the run.
    Page(u32),
}

/// Keep the rows a record digger wants: pressings (or format-unknown master
/// rows, which are usually records too), one per *work* — a label page lists
/// the original, the repress and every regional edition separately.
/// The page's records, one row per work. Discogs lists every pressing of a
/// record in the run (test pressing, standard, repress) and puts them in no
/// useful order; the row that survives the collapse is the pressing on one of
/// your shelves when there is one, else the first listed. That keeps the row's
/// id pointing at *your* copy, so the shelf marks and the sheet it opens
/// agree with your collection instead of with whichever pressing came first.
fn crate_rows(
    page: &BrowsePage,
    owned: &HashSet<u64>,
    wanted: &HashSet<u64>,
) -> Vec<BrowseRelease> {
    let mut slot: HashMap<String, usize> = HashMap::new();
    let mut rows: Vec<BrowseRelease> = Vec::new();
    for r in page
        .releases
        .iter()
        .filter(|r| !r.format_known || crate::dig::is_vinyl(&r.format))
    {
        let (a, t) = crate::dig::row_artist_title(r);
        let key = crate::dig::work_key(&a, &t);
        let rank = |id: u64| -> u8 {
            if owned.contains(&id) {
                2
            } else if wanted.contains(&id) {
                1
            } else {
                0
            }
        };
        match slot.get(&key) {
            None => {
                slot.insert(key, rows.len());
                rows.push(r.clone());
            }
            Some(&i) if rank(r.release_id) > rank(rows[i].release_id) => {
                rows[i] = r.clone();
            }
            Some(_) => {}
        }
    }
    rows
}

impl App {
    /// Open the label page for the label of `release_id`. The label id rides
    /// the release detail, which is cached for any record whose sheet was
    /// opened — so this usually resolves without a request and spends its one
    /// paced call on the first page of the run. `hint` names the label until
    /// the detail corrects it.
    pub(crate) fn open_label_page(&mut self, release_id: u64, hint: Option<String>) {
        let name = hint.unwrap_or_else(|| "…".to_string());
        self.browse_panel = Some(BrowsePanel {
            thread: BrowseThread::Label,
            id: 0,
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
        self.browse_rx = Some(rx);
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
                let _ = tx.send(BrowseFetched {
                    thread: BrowseThread::Label,
                    id: 0,
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
            let _ = tx.send(BrowseFetched {
                thread: BrowseThread::Label,
                id: label_id,
                name: label_name,
                page: 1,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Open the browse page on an artist's discography. The id is known up
    /// front (it comes from a Discogs artist search hit), so this spends its
    /// one paced call straight on the first page of the run.
    pub(crate) fn open_artist_page(&mut self, artist_id: u64, name: String) {
        self.browse_panel = Some(BrowsePanel {
            thread: BrowseThread::Artist,
            id: artist_id,
            name,
            page: 1,
            pages: 1,
            items: 0,
            releases: Vec::new(),
            loading: false,
            error: None,
        });
        self.fetch_browse_page(1);
    }

    /// Fetch a page of the open run (the first, for an artist page; another,
    /// on a page turn).
    fn fetch_browse_page(&mut self, page: u32) {
        let Some(panel) = self.browse_panel.as_mut() else {
            return;
        };
        if panel.id == 0 || panel.loading {
            return;
        }
        let (thread, id) = (panel.thread, panel.id);
        panel.loading = true;
        panel.page = page;
        let token = self.discogs_token();
        let (tx, rx) = mpsc::channel();
        self.browse_rx = Some(rx);
        let ctx = self.egui_ctx.clone();
        thread::spawn(move || {
            let client =
                discogs::Client::new(token, "Ordnung/0.1 +https://kailazy.github.io/Ordnung/");
            let result = client
                .browse_by_id(thread, id, page)
                .map_err(|e| e.to_string());
            let _ = tx.send(BrowseFetched {
                thread,
                id,
                name: None,
                page,
                result,
            });
            ctx.request_repaint();
        });
    }

    /// Adopt a finished browse-page fetch onto the open panel.
    pub(crate) fn poll_browse_page(&mut self) {
        let Some(rx) = &self.browse_rx else { return };
        let Ok(msg) = rx.try_recv() else { return };
        self.browse_rx = None;
        let Some(panel) = self.browse_panel.as_mut() else {
            return;
        };
        // A label page's first fetch is the only one that arrives while the
        // panel still has no id; everything else must match the run on screen.
        if msg.thread != panel.thread || (panel.id != 0 && msg.id != panel.id) {
            return;
        }
        panel.loading = false;
        if msg.id != 0 {
            panel.id = msg.id;
        }
        if let Some(name) = msg.name.filter(|n| !n.trim().is_empty()) {
            panel.name = name;
        }
        match msg.result {
            Ok(page) => {
                panel.page = msg.page;
                panel.pages = page.pages.max(1);
                panel.items = page.items;
                panel.releases = crate_rows(&page, &self.vinyl_owned, &self.vinyl_wanted);
            }
            Err(e) => panel.error = Some(e),
        }
    }

    /// Draw the open browse page, if any.
    pub(crate) fn draw_browse_page(&mut self, ctx: &egui::Context) {
        let Some(panel) = self.browse_panel.as_ref() else {
            return;
        };
        let thread = panel.thread;
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
            cover: Option<Tex>,
            artist: String,
            title: String,
            /// `1994 · 12" · CR-03`, whichever parts exist. An artist's rows
            /// name the label too, since it varies down their run.
            sub: String,
            /// Credited as remixer rather than as the artist (artist runs only).
            remix: bool,
            owned: bool,
            wanted: bool,
            /// A wantlist edit on this record is in flight or queued.
            pending: bool,
        }
        let specs: Vec<(String, u64, String, String, String, bool)> = panel
            .releases
            .iter()
            .map(|r| {
                let (artist, title) = crate::dig::row_artist_title(r);
                let imprint = match thread {
                    // A label page is the imprint; naming it on every row
                    // says nothing. An artist's run hops labels, so there it
                    // is the row's most telling fact after the year.
                    BrowseThread::Label => r.catno.trim().to_string(),
                    BrowseThread::Artist => [r.label.trim(), r.catno.trim()]
                        .into_iter()
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join(" "),
                };
                let sub = [
                    r.year.filter(|y| *y > 0).map(|y| y.to_string()),
                    (!r.format.trim().is_empty()).then(|| r.format.clone()),
                    (!imprint.is_empty()).then_some(imprint),
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
                    thread == BrowseThread::Artist && !r.main,
                )
            })
            .collect();
        let mut rows: Vec<Row> = Vec::with_capacity(specs.len());
        for (thumb, release_id, artist, title, sub, remix) in specs {
            // By record, not pressing: the run may list another pressing of a
            // record you own.
            let owned = self.owns_record(release_id, &artist, &title);
            let wanted = self.wants_record(release_id, &artist, &title);
            let pending = self.vinyl_pending(VinylList::Wantlist, release_id).is_some();
            rows.push(Row {
                cover: (!thumb.trim().is_empty())
                    .then(|| self.dig_cover(&thumb).cloned())
                    .flatten(),
                artist,
                title,
                sub,
                remix,
                owned,
                wanted,
                pending,
            });
        }

        let mut act: Option<Act> = None;
        let mut open = true;
        let glyph = match thread {
            BrowseThread::Label => "⌂",
            BrowseThread::Artist => "♪",
        };
        egui::Window::new(format!("{glyph} {name}"))
            .id(egui::Id::new("browse-page"))
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
                let whose = match thread {
                    BrowseThread::Label => "The label's whole run",
                    BrowseThread::Artist => "Everything the artist put out",
                };
                let blurb = if items > 0 {
                    format!(
                        "{whose}, as Discogs lists it — {items} releases, \
                         records only shown. Your shelves are marked on every row."
                    )
                } else {
                    format!(
                        "{whose}, as Discogs lists it. Your shelves are marked on \
                         every row."
                    )
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
                        let reading = match thread {
                            BrowseThread::Label => "Reading the label's crate…",
                            BrowseThread::Artist => "Reading the artist's crate…",
                        };
                        ui.label(egui::RichText::new(reading).weak());
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
                                            (r.remix, "REMIX"),
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
                                        let parked = r.owned || r.wanted;
                                        if ui
                                            .add_enabled(
                                                !parked && !r.pending,
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
            self.browse_panel = None;
            return;
        }

        let row_of = |i: usize| -> Option<&BrowseRelease> {
            self.browse_panel.as_ref().and_then(|p| p.releases.get(i))
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
                    // A label page knows the imprint; an artist's row carries
                    // its own, when Discogs listed one.
                    let label_name = match thread {
                        BrowseThread::Label => self
                            .browse_panel
                            .as_ref()
                            .map(|p| p.name.clone())
                            .filter(|n| n != "…"),
                        BrowseThread::Artist => {
                            Some(r.label.trim().to_string()).filter(|l| !l.is_empty())
                        }
                    };
                    let thumb = (!r.thumb_url.trim().is_empty()).then(|| r.thumb_url.clone());
                    self.start_dig_release(r.release_id, artist, title, label_name, sub, thumb);
                }
            }
            Some(Act::Page(p)) => self.fetch_browse_page(p),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(id: u64, artist: &str, title: &str, format: &str) -> BrowseRelease {
        BrowseRelease {
            release_id: id,
            title: title.into(),
            artist: artist.into(),
            year: Some(2019),
            format: format.into(),
            format_known: true,
            label: "Nightime Drama".into(),
            catno: "NTD010".into(),
            thumb_url: String::new(),
            main: true,
        }
    }

    /// The screenshot case: Discogs lists the test pressing of Night Drive EP
    /// ahead of the standard 12" the collection holds. The collapsed row has
    /// to be the owned pressing, not whichever came first.
    #[test]
    fn crate_rows_keep_the_shelved_pressing_of_a_record() {
        let page = BrowsePage {
            pages: 1,
            items: 3,
            releases: vec![
                release(1, "Various", "Night Drive EP", "12\", EP, TP"),
                release(2, "Various", "Night Drive EP", "12\", EP"),
                release(3, "Trinity", "Cascade Drive", "12\""),
            ],
        };
        let owned: HashSet<u64> = [2].into_iter().collect();
        let rows = crate_rows(&page, &owned, &HashSet::new());
        let ids: Vec<u64> = rows.iter().map(|r| r.release_id).collect();
        assert_eq!(ids, vec![2, 3], "owned pressing wins, order of first sight kept");

        // A wanted pressing wins over an unshelved one, but not over an owned one.
        let wanted: HashSet<u64> = [1].into_iter().collect();
        let rows = crate_rows(&page, &HashSet::new(), &wanted);
        assert_eq!(rows[0].release_id, 1);
        let rows = crate_rows(&page, &owned, &wanted);
        assert_eq!(rows[0].release_id, 2);

        // Nothing shelved: first listed stands, as before.
        let rows = crate_rows(&page, &HashSet::new(), &HashSet::new());
        assert_eq!(rows[0].release_id, 1);
    }
}
