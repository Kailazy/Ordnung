//! The first-run welcome tour.
//!
//! An established DJ downloading Ordnung is being asked to point a new,
//! unproven app at a library they have spent years building. The tour exists to
//! answer, before anything is touched: what does this do, what does it read,
//! and — the question that actually decides trust — *when does it write to my
//! files?*
//!
//! So the tour is not decorative. Its penultimate step is a real fork: the user
//! picks automatic or manual metadata writeback, and that choice is written to
//! [`Config::auto_write_tags`] on Finish. Until they answer, no Discogs
//! enrichment has run and nothing has been written. This is the one place the
//! app's file-writing policy is stated in plain language rather than inferred
//! from a settings checkbox.
//!
//! GUI-only policy, so it lives in the GUI boundary per `ordnung-architecture`;
//! `ordnung-core` knows nothing about it.

use super::*;

/// Bumped when the tour changes enough that returning users should see it again.
/// [`Config::onboarding_completed_version`] stores the last version a user
/// finished, so a stale completion re-opens the tour exactly once rather than
/// nagging on every launch.
///
/// v2 added the library-root step. Existing users replay once deliberately:
/// they don't have a root either, and the tour is the right place to ask.
///
/// v3 added the digital/vinyl question and the inline Discogs token field.
pub(crate) const TOUR_VERSION: u32 = 3;

/// One step of the tour. Ordered as the questions actually arrive: what is this,
/// how does my music get in, what does Discogs add, and only then — now that
/// there is something to write — how should writing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TourStep {
    /// What Ordnung is, and the promise about source files.
    Welcome,
    /// Digital-first or vinyl-first — shapes the sidebar and the Discogs pitch.
    Medium,
    /// Adding music and what analysis produces.
    Library,
    /// Picking the library root — the on-ramp. Importing starts on Finish.
    LibraryRoot,
    /// Playing, digging, and the vinyl shelf.
    Crate,
    /// Connecting Discogs and what it fills in.
    Discogs,
    /// The writeback fork: automatic or manual.
    Writeback,
}

impl TourStep {
    /// Every step, in tour order.
    pub(crate) const ALL: [TourStep; 7] = [
        TourStep::Welcome,
        TourStep::Medium,
        TourStep::Library,
        TourStep::LibraryRoot,
        TourStep::Crate,
        TourStep::Discogs,
        TourStep::Writeback,
    ];

    /// This step's 0-based position, for the progress dots and the step counter.
    fn index(self) -> usize {
        TourStep::ALL.iter().position(|s| *s == self).unwrap_or(0)
    }

    /// The next step, or `None` on the last one (where the button says Finish).
    fn next(self) -> Option<TourStep> {
        TourStep::ALL.get(self.index() + 1).copied()
    }

    /// The previous step, or `None` on the first.
    fn prev(self) -> Option<TourStep> {
        if self.index() == 0 {
            None
        } else {
            TourStep::ALL.get(self.index() - 1).copied()
        }
    }
}

/// Live state of the tour while its window is open.
#[derive(Debug, Clone)]
pub(crate) struct Tour {
    /// Which step is showing.
    pub(crate) step: TourStep,
    /// The writeback choice made on [`TourStep::Writeback`], seeded from the
    /// current config so reopening the tour shows what's actually in force.
    /// Only committed to config when the user finishes.
    pub(crate) auto_write: bool,
    /// The folder picked on [`TourStep::LibraryRoot`], seeded from the current
    /// config so a replay shows the root already in force. Committed on Finish;
    /// a root that actually changed also kicks off the first import.
    pub(crate) library_root: Option<PathBuf>,
    /// The digital/vinyl answer from [`TourStep::Medium`], seeded from
    /// [`Config::nav_primary`]. `true` means vinyl-first. Committed on Finish
    /// as the sidebar's primary library, and it steers the Discogs step's pitch.
    pub(crate) vinyl_first: bool,
    /// Discogs token typed on [`TourStep::Discogs`], seeded from the saved
    /// token. Committed on Finish; a token that actually changed also kicks off
    /// the identity check so the user sees it turn into a signed-in account.
    pub(crate) token_input: String,
}

impl Tour {
    /// Open the tour at the first step, seeded from the live config.
    pub(crate) fn new(
        auto_write: bool,
        library_root: Option<PathBuf>,
        vinyl_first: bool,
        token_input: String,
    ) -> Self {
        Self {
            step: TourStep::Welcome,
            auto_write,
            library_root,
            vinyl_first,
            token_input,
        }
    }
}

/// A drawn mark for a feature row, as a function pointer into [`crate::ui::icon`].
/// Painted rather than set as text: a font glyph brings its own weight and
/// baseline, so a column of them reads as unrelated characters instead of a set.
type Mark = fn(&egui::Painter, egui::Pos2, egui::Color32, f32);

/// One capability line: a tinted icon tile, a title, and a short gloss.
///
/// The tile is the point — at 38pt with an accent wash behind it, the row reads
/// as an icon with a label rather than a paragraph with a bullet. Sizes come
/// from the type ramp; this is a first-run sheet read at arm's length, so it
/// sits a step up from the dense table chrome elsewhere in the app.
fn feature_row(ui: &mut egui::Ui, mark: Mark, title: &str, body: &str) {
    const TILE: f32 = 38.0;
    let accent = crate::sidebar::NAV_ACCENT;
    ui.horizontal_top(|ui| {
        let (rect, _) = ui.allocate_exact_size(egui::vec2(TILE, TILE), egui::Sense::hover());
        // Soft accent wash so the mark sits on a surface rather than floating.
        ui.painter().rect_filled(
            rect,
            egui::Rounding::same(crate::ui::tokens::radius::SM),
            accent.gamma_multiply(0.16),
        );
        mark(ui.painter(), rect.center(), accent, TILE * 0.28);
        ui.add_space(crate::ui::tokens::space::S4);
        ui.vertical(|ui| {
            // Nudge the text block down so its cap-height aligns with the tile's
            // optical centre rather than its top edge.
            ui.add_space(crate::ui::tokens::space::S1);
            ui.label(
                egui::RichText::new(title)
                    .font(crate::ui::tokens::font::strong(
                        crate::ui::tokens::font::headline().size,
                    ))
                    .color(crate::ui::tokens::color::LABEL),
            );
            ui.add_space(crate::ui::tokens::space::S1);
            ui.label(
                egui::RichText::new(body)
                    .font(crate::ui::tokens::font::body())
                    .color(crate::ui::tokens::color::LABEL_2),
            );
        });
    });
    ui.add_space(crate::ui::tokens::space::S5);
}

/// A framed, clickable choice card: an icon tile that doubles as the selection
/// indicator (accent wash when chosen, flat when not), a title, and a gloss.
/// The whole card is the click target. Used for the tour's two forks — the
/// digital/vinyl question and the writeback choice — so both read as the same
/// kind of decision. Returns `true` when clicked.
fn choice_card(
    ui: &mut egui::Ui,
    accent: egui::Color32,
    selected: bool,
    mark: Mark,
    title: &str,
    body: &str,
) -> bool {
    let stroke = if selected {
        egui::Stroke::new(1.5, accent)
    } else {
        egui::Stroke::new(1.0, crate::ui::tokens::color::SEPARATOR_OPAQUE)
    };
    let resp = egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .rounding(egui::Rounding::same(crate::ui::tokens::radius::SM))
        .stroke(stroke)
        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                const TILE: f32 = 34.0;
                let (r, _) = ui.allocate_exact_size(egui::vec2(TILE, TILE), egui::Sense::hover());
                let tint = if selected {
                    accent
                } else {
                    crate::ui::tokens::color::LABEL_3
                };
                ui.painter().rect_filled(
                    r,
                    egui::Rounding::same(crate::ui::tokens::radius::SM),
                    tint.gamma_multiply(if selected { 0.20 } else { 0.10 }),
                );
                mark(ui.painter(), r.center(), tint, TILE * 0.28);
                ui.add_space(crate::ui::tokens::space::S4);
                ui.vertical(|ui| {
                    ui.label(
                        egui::RichText::new(title)
                            .font(crate::ui::tokens::font::strong(
                                crate::ui::tokens::font::headline().size,
                            ))
                            .color(crate::ui::tokens::color::LABEL),
                    );
                    ui.add_space(crate::ui::tokens::space::S2);
                    ui.label(
                        egui::RichText::new(body)
                            .font(crate::ui::tokens::font::body())
                            .color(crate::ui::tokens::color::LABEL_2),
                    );
                });
            });
        })
        .response;
    resp.interact(egui::Sense::click()).clicked()
}

/// A step's heading and one-line standfirst — the same shape on every page, so
/// the eye lands in the same place each time Next is pressed.
fn step_heading(ui: &mut egui::Ui, title: &str, standfirst: &str) {
    ui.label(
        egui::RichText::new(title)
            .font(crate::ui::tokens::font::title())
            .color(crate::ui::tokens::color::LABEL),
    );
    ui.add_space(crate::ui::tokens::space::S2);
    ui.label(
        egui::RichText::new(standfirst)
            .font(crate::ui::tokens::font::headline())
            .color(crate::ui::tokens::color::LABEL_2),
    );
    ui.add_space(crate::ui::tokens::space::S6);
}

impl App {
    /// Open the welcome tour if this install has never finished it. Called once
    /// at startup; a user who finished the current tour never sees it again.
    pub(crate) fn maybe_open_tour(&mut self) {
        if self.config.onboarding_completed_version < TOUR_VERSION {
            self.open_tour();
        }
    }

    /// Open the tour on demand, from Settings. Reopening never re-runs on the
    /// next launch by itself: finishing stamps the version again, and closing
    /// leaves whatever stamp was already there.
    pub(crate) fn open_tour(&mut self) {
        self.tour = Some(Tour::new(
            self.config.auto_write_tags,
            self.config.library_root.clone(),
            crate::config::NavPrimary::from_key(&self.config.nav_primary)
                == crate::config::NavPrimary::Vinyl,
            self.config.discogs_token.clone(),
        ));
    }

    /// The welcome tour window. Modal in spirit — it's the first thing a new
    /// user sees — but closable, because trapping someone in a tour is its own
    /// kind of untrustworthy.
    pub(crate) fn draw_tour(&mut self, ctx: &egui::Context) {
        let Some(tour) = self.tour.clone() else {
            return;
        };
        let accent = crate::sidebar::NAV_ACCENT;
        let mut open = true;
        // Deferred so the step's body can borrow `self` immutably while the
        // footer decides what to do.
        let mut goto: Option<TourStep> = None;
        let mut finish = false;
        let mut auto_write = tour.auto_write;
        let mut library_root = tour.library_root.clone();
        let mut vinyl_first = tour.vinyl_first;
        let mut token_input = tour.token_input.clone();

        egui::Window::new("Welcome to Ordnung")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.screen_rect().center())
            .show(ctx, |ui| {
                // Every page is laid out inside one fixed box, so the window
                // never resizes between steps and Next stays exactly where the
                // pointer left it. See `ui::sheet`.
                crate::ui::sheet::stepped(
                    ui,
                    crate::ui::sheet::SheetSize::TOUR,
                    |ui| {
                        match tour.step {
                            TourStep::Welcome => {
                                step_heading(
                                    ui,
                                    "Your record collection, in order",
                                    "BPM, key, artwork and release data \u{2014} digital files and \
                                     records in one place.",
                                );

                                // The trust statement. The reason this step
                                // exists, so it gets a framed block of its own.
                                egui::Frame::none()
                                    .fill(ui.visuals().extreme_bg_color)
                                    .rounding(egui::Rounding::same(
                                        crate::ui::tokens::radius::SM,
                                    ))
                                    .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.5)))
                                    .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let (r, _) = ui.allocate_exact_size(
                                                egui::vec2(22.0, 22.0),
                                                egui::Sense::hover(),
                                            );
                                            crate::ui::icon::shield(
                                                ui.painter(),
                                                r.center(),
                                                accent,
                                                9.0,
                                            );
                                            ui.add_space(crate::ui::tokens::space::S2);
                                            ui.label(
                                                egui::RichText::new(
                                                    "Your files stay where they are",
                                                )
                                                .font(crate::ui::tokens::font::strong(
                                                    crate::ui::tokens::font::headline().size,
                                                ))
                                                .color(crate::ui::tokens::color::LABEL),
                                            );
                                        });
                                        ui.add_space(crate::ui::tokens::space::S3);
                                        ui.label(
                                            egui::RichText::new(
                                                "Ordnung reads your music into its own catalog. \
                                                 Nothing is moved or renamed. The only thing \
                                                 that writes to your files is tag writeback, \
                                                 and you choose how that works in a moment.",
                                            )
                                            .font(crate::ui::tokens::font::body())
                                            .color(crate::ui::tokens::color::LABEL_2),
                                        );
                                    });
                            }
                            TourStep::Medium => {
                                step_heading(
                                    ui,
                                    "How do you play?",
                                    "Ordnung holds both. This just decides which library \
                                     leads.",
                                );
                                if choice_card(
                                    ui,
                                    accent,
                                    !vinyl_first,
                                    crate::ui::icon::waveform,
                                    "Mostly digital",
                                    "Files first. Your digital library tops the sidebar; \
                                     the vinyl shelf sits below.",
                                ) {
                                    vinyl_first = false;
                                }
                                ui.add_space(crate::ui::tokens::space::S3);
                                if choice_card(
                                    ui,
                                    accent,
                                    vinyl_first,
                                    crate::ui::icon::record,
                                    "Mostly vinyl",
                                    "Records first. Your Discogs shelf tops the sidebar; \
                                     the digital library sits below.",
                                ) {
                                    vinyl_first = true;
                                }
                                ui.add_space(crate::ui::tokens::space::S3);
                                ui.label(
                                    egui::RichText::new("Change this any time in Settings.")
                                        .font(crate::ui::tokens::font::body())
                                        .color(crate::ui::tokens::color::LABEL_3),
                                );
                            }
                            TourStep::Library => {
                                step_heading(
                                    ui,
                                    "Add your music",
                                    "Drop files on the window, or pick a folder.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::import,
                                    "Import",
                                    "Reads the tags already in your files.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::waveform,
                                    "Analysis",
                                    "BPM, Camelot key, beatgrid and waveform, from the audio.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::list,
                                    "Organize",
                                    "Search, sort, playlists. Health finds dupes and \
                                     missing files.",
                                );
                            }
                            TourStep::LibraryRoot => {
                                step_heading(
                                    ui,
                                    "Where does your music live?",
                                    "Pick your music folder and importing starts when you \
                                     finish the tour.",
                                );

                                // The chosen root, in a framed block like the
                                // Welcome step's trust statement — this is the
                                // tour's one real input, so it gets the same
                                // visual weight.
                                egui::Frame::none()
                                    .fill(ui.visuals().extreme_bg_color)
                                    .rounding(egui::Rounding::same(
                                        crate::ui::tokens::radius::SM,
                                    ))
                                    .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.5)))
                                    .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let (r, _) = ui.allocate_exact_size(
                                                egui::vec2(22.0, 22.0),
                                                egui::Sense::hover(),
                                            );
                                            crate::ui::icon::library(
                                                ui.painter(),
                                                r.center(),
                                                accent,
                                                9.0,
                                            );
                                            ui.add_space(crate::ui::tokens::space::S2);
                                            match &library_root {
                                                Some(root) => {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            root.display().to_string(),
                                                        )
                                                        .font(crate::ui::tokens::font::strong(
                                                            crate::ui::tokens::font::body().size,
                                                        ))
                                                        .color(crate::ui::tokens::color::LABEL),
                                                    );
                                                }
                                                None => {
                                                    ui.label(
                                                        egui::RichText::new(
                                                            "No folder chosen yet",
                                                        )
                                                        .font(crate::ui::tokens::font::body())
                                                        .color(crate::ui::tokens::color::LABEL_3),
                                                    );
                                                }
                                            }
                                        });
                                        ui.add_space(crate::ui::tokens::space::S3);
                                        let label = if library_root.is_some() {
                                            "Change folder…"
                                        } else {
                                            "Choose folder…"
                                        };
                                        let btn = egui::Button::new(
                                            egui::RichText::new(label)
                                                .color(egui::Color32::WHITE),
                                        )
                                        .fill(accent);
                                        if ui.add(btn).clicked() {
                                            if let Some(dir) =
                                                rfd::FileDialog::new().pick_folder()
                                            {
                                                library_root = Some(dir);
                                            }
                                        }
                                    });
                                ui.add_space(crate::ui::tokens::space::S5);
                                feature_row(
                                    ui,
                                    crate::ui::icon::import,
                                    "Runs in the background",
                                    "Import and analysis keep working while you use the app, \
                                     and pick up where they left off.",
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Optional. You can always add music with \
                                         Add songs\u{2026} or by dropping files on the window.",
                                    )
                                    .font(crate::ui::tokens::font::body())
                                    .color(crate::ui::tokens::color::LABEL_3),
                                );
                            }
                            TourStep::Crate => {
                                step_heading(
                                    ui,
                                    "Play and dig",
                                    "Built for choosing records, not just filing them.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::deck,
                                    "Preview deck",
                                    "Scrubbable waveform, keyboard transport, drag-out.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::dig,
                                    "Dig",
                                    "Follow a track through Discogs by label and artist.",
                                );
                                feature_row(
                                    ui,
                                    crate::ui::icon::record,
                                    "Vinyl shelf",
                                    "Your collection and wantlist, with covers and prices.",
                                );
                            }
                            TourStep::Discogs => {
                                // The pitch follows the digital/vinyl answer:
                                // a vinyl-first user's shelf *is* their Discogs
                                // collection, so linking is the point; a
                                // digital-first user gets it as enrichment.
                                if vinyl_first {
                                    step_heading(
                                        ui,
                                        "Link your Discogs collection",
                                        "Your shelf lives on Discogs. Link it and your \
                                         records show up here.",
                                    );
                                    feature_row(
                                        ui,
                                        crate::ui::icon::record,
                                        "Your collection and wantlist",
                                        "Records you own and want, with covers and prices.",
                                    );
                                    feature_row(
                                        ui,
                                        crate::ui::icon::tag,
                                        "Release matching",
                                        "Label, catalog number, year, country, genre for \
                                         your files too.",
                                    );
                                } else {
                                    step_heading(
                                        ui,
                                        "Connect Discogs",
                                        "Optional, but it fills in what your files are \
                                         missing.",
                                    );
                                    feature_row(
                                        ui,
                                        crate::ui::icon::tag,
                                        "Release matching",
                                        "Label, catalog number, year, country, genre.",
                                    );
                                    feature_row(
                                        ui,
                                        crate::ui::icon::art,
                                        "Cover art",
                                        "Full-size artwork. You review every cover.",
                                    );
                                }

                                // The token, right here: a vinyl-first user in
                                // particular shouldn't have to finish the tour
                                // and go hunting through Settings to get the
                                // thing the previous step promised them.
                                egui::Frame::none()
                                    .fill(ui.visuals().extreme_bg_color)
                                    .rounding(egui::Rounding::same(
                                        crate::ui::tokens::radius::SM,
                                    ))
                                    .stroke(egui::Stroke::new(1.0, accent.gamma_multiply(0.5)))
                                    .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                                    .show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.label(
                                            egui::RichText::new("Personal access token")
                                                .font(crate::ui::tokens::font::strong(
                                                    crate::ui::tokens::font::body().size,
                                                ))
                                                .color(crate::ui::tokens::color::LABEL),
                                        );
                                        ui.add_space(crate::ui::tokens::space::S2);
                                        ui.add(
                                            egui::TextEdit::singleline(&mut token_input)
                                                .password(true)
                                                .hint_text("Paste your Discogs token")
                                                .desired_width(ui.available_width()),
                                        );
                                        ui.add_space(crate::ui::tokens::space::S2);
                                        ui.horizontal(|ui| {
                                            ui.label(
                                                egui::RichText::new("Free from")
                                                    .font(crate::ui::tokens::font::body())
                                                    .color(crate::ui::tokens::color::LABEL_3),
                                            );
                                            ui.hyperlink_to(
                                                "discogs.com/settings/developers",
                                                "https://www.discogs.com/settings/developers",
                                            );
                                        });
                                    });
                                ui.add_space(crate::ui::tokens::space::S3);
                                ui.label(
                                    egui::RichText::new(
                                        "Optional \u{2014} you can add it later in Settings \
                                         \u{2192} Discogs. Read-only: Ordnung never edits \
                                         your Discogs account.",
                                    )
                                    .font(crate::ui::tokens::font::body())
                                    .color(crate::ui::tokens::color::LABEL_3),
                                );
                            }
                            TourStep::Writeback => {
                                step_heading(
                                    ui,
                                    "Updating your files",
                                    "Edits and Discogs data land in the catalog. Writing them \
                                     into the files is your call.",
                                );

                                // The fork itself: two framed, clickable cards.
                                // Radio rows would read as a settings detail;
                                // this is the decision the tour exists for.
                                if choice_card(
                                    ui,
                                    accent,
                                    auto_write,
                                    crate::ui::icon::sync,
                                    "Automatic",
                                    "Keeps files in sync in the background. Only tracks you \
                                     changed, only their tags. Recommended.",
                                ) {
                                    auto_write = true;
                                }
                                ui.add_space(crate::ui::tokens::space::S3);
                                if choice_card(
                                    ui,
                                    accent,
                                    !auto_write,
                                    crate::ui::icon::hold,
                                    "Manual",
                                    "Nothing is written until you press Write. Files stay \
                                     byte-identical.",
                                ) {
                                    auto_write = false;
                                }
                                ui.add_space(crate::ui::tokens::space::S3);
                                ui.label(
                                    egui::RichText::new("Change this any time in Settings.")
                                        .font(crate::ui::tokens::font::body())
                                        .color(crate::ui::tokens::color::LABEL_3),
                                );
                            }
                        }
                    },
                    |ui| {
                        // Progress dots: cheap orientation, and they make the
                        // tour read as finite.
                        crate::ui::sheet::progress_dots(
                            ui,
                            TourStep::ALL.len(),
                            tour.step.index(),
                            accent,
                        );

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let last = tour.step.next().is_none();
                            let label = if last { "Finish" } else { "Next" };
                            let btn = egui::Button::new(
                                egui::RichText::new(label).color(egui::Color32::WHITE),
                            )
                            .min_size(egui::vec2(88.0, 26.0))
                            .fill(accent);
                            if ui.add(btn).clicked() {
                                match tour.step.next() {
                                    Some(next) => goto = Some(next),
                                    None => finish = true,
                                }
                            }
                            // Back is drawn but disabled on the first step, so
                            // the footer's button row keeps a constant shape and
                            // Next never shifts sideways between pages.
                            let prev = tour.step.prev();
                            if ui
                                .add_enabled(prev.is_some(), egui::Button::new("Back"))
                                .clicked()
                            {
                                goto = prev;
                            }
                        });
                    },
                );
            });

        // Apply what the frame decided. The live selections are kept even when
        // the user steps back and forth, so Finish commits what they see.
        if let Some(t) = self.tour.as_mut() {
            t.auto_write = auto_write;
            t.library_root = library_root.clone();
            t.vinyl_first = vinyl_first;
            t.token_input = token_input.clone();
            if let Some(next) = goto {
                t.step = next;
            }
        }

        if finish {
            self.finish_tour(ctx, auto_write, library_root, vinyl_first, token_input);
        } else if !open {
            // Closing with the X is a deliberate "not now": don't write a
            // writeback choice the user skipped past, but do stop reopening the
            // tour on every launch. The config default (automatic) stands.
            self.config.onboarding_completed_version = TOUR_VERSION;
            if let Err(e) = self.config.save() {
                self.status = format!("Couldn't save settings: {e}");
            }
            self.tour = None;
        }
    }

    /// Commit the tour's choices (writeback policy and library root), mark it
    /// done, and — when a root was actually picked or changed — kick off the
    /// first import. The import only starts here, on Finish: closing the tour
    /// with the X must never start reading a folder the user didn't confirm.
    fn finish_tour(
        &mut self,
        ctx: &egui::Context,
        auto_write: bool,
        library_root: Option<PathBuf>,
        vinyl_first: bool,
        token_input: String,
    ) {
        let changed = self.config.auto_write_tags != auto_write;
        self.config.auto_write_tags = auto_write;
        if changed {
            // Same re-arm the Settings checkbox does: an explicit choice is a
            // fresh request to try the files again.
            self.auto_write_stalled_at = None;
        }
        // A replay that keeps the same root doesn't re-scan; the root has to
        // have actually changed for Finish to mean "go read that folder".
        let root_changed = library_root.is_some() && library_root != self.config.library_root;
        self.config.library_root = library_root;
        // The digital/vinyl answer lands as the sidebar's primary library. When
        // it actually changed, the startup view follows too — a vinyl-first
        // user opens on their shelf — but a replay that kept the same answer
        // leaves a hand-picked startup view alone.
        let medium = if vinyl_first {
            crate::config::NavPrimary::Vinyl
        } else {
            crate::config::NavPrimary::Digital
        };
        if crate::config::NavPrimary::from_key(&self.config.nav_primary) != medium {
            self.config.nav_primary = medium.key().to_string();
            self.config.startup_view = if vinyl_first { "vinyl" } else { "library" }.to_string();
        }
        // A token typed in the tour is committed like the Settings tab does it,
        // including the mirror field the Settings text box edits; a token that
        // actually changed also gets the identity check so the account shows as
        // signed in. An emptied field clears the token deliberately.
        let token = token_input.trim().to_string();
        let token_changed = token != self.config.discogs_token;
        if token_changed {
            self.config.discogs_token = token.clone();
            self.token_input = token.clone();
            if token.is_empty() {
                self.config.discogs_username.clear();
                self.discogs_auth = DiscogsAuth::SignedOut;
            }
        }
        self.config.onboarding_completed_version = TOUR_VERSION;
        if let Err(e) = self.config.save() {
            self.status = format!("Couldn't save settings: {e}");
        } else {
            self.status = if auto_write {
                "Ordnung will keep your files in sync as you edit.".to_string()
            } else {
                "Edits stay in the catalog until you write them to files.".to_string()
            };
        }
        self.tour = None;
        if token_changed && !self.config.discogs_token.is_empty() {
            self.spawn_discogs_identity_check(ctx.clone());
        }
        if root_changed && !self.is_busy() {
            if let Some(root) = self.config.library_root.clone() {
                // Overwrites the status above with "Scanning …", which is the
                // more useful message: the on-ramp's whole point is that the
                // user sees the import start.
                self.spawn_scan(ctx.clone(), root);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tour must be finite and traversable in both directions: every step
    /// reachable from the first by Next, and back again by Back.
    #[test]
    fn steps_walk_forward_and_back_over_every_step() {
        let mut step = TourStep::Welcome;
        let mut seen = vec![step];
        while let Some(next) = step.next() {
            step = next;
            seen.push(step);
        }
        assert_eq!(seen, TourStep::ALL.to_vec());
        // The last step is where Finish lives, not another Next.
        assert_eq!(step, TourStep::Writeback);

        while let Some(prev) = step.prev() {
            step = prev;
        }
        assert_eq!(step, TourStep::Welcome);
        assert!(TourStep::Welcome.prev().is_none());
    }

    /// The writeback fork is the point of the tour, so it must be the step the
    /// user lands on last — after they've been told what writes to files and
    /// why. A reorder that buries it should fail here.
    #[test]
    fn the_writeback_choice_is_the_final_step() {
        assert_eq!(TourStep::ALL.last(), Some(&TourStep::Writeback));
    }

    /// A fresh install has never completed the tour, so the gate opens it; a
    /// config stamped with the current version does not.
    #[test]
    fn a_fresh_config_is_due_the_tour_and_a_completed_one_is_not() {
        let fresh = Config::default();
        assert!(fresh.onboarding_completed_version < TOUR_VERSION);

        let done = Config {
            onboarding_completed_version: TOUR_VERSION,
            ..Config::default()
        };
        assert!(!(done.onboarding_completed_version < TOUR_VERSION));
    }

    /// The tour opens showing whatever writeback policy and library root are
    /// actually in force, so a user replaying it isn't told they chose
    /// something they didn't.
    #[test]
    fn the_tour_seeds_its_choices_from_the_live_settings() {
        let fresh = |auto: bool| Tour::new(auto, None, false, String::new());
        assert!(fresh(true).auto_write);
        assert!(!fresh(false).auto_write);
        assert_eq!(fresh(true).step, TourStep::Welcome);
        assert_eq!(fresh(true).library_root, None);
        assert!(!fresh(true).vinyl_first);
        assert!(fresh(true).token_input.is_empty());

        let root = PathBuf::from("/music");
        let seeded = Tour::new(true, Some(root.clone()), true, "tok".into());
        assert_eq!(seeded.library_root, Some(root));
        assert!(seeded.vinyl_first);
        assert_eq!(seeded.token_input, "tok");
    }

    /// v2 added the library-root step; a user who finished v1 has no root and
    /// must see the tour once more. That replay is the migration mechanism, so
    /// this pins both the bump and the step's presence.
    #[test]
    fn a_v1_completion_is_due_the_root_asking_replay() {
        assert!(TOUR_VERSION >= 2);
        let v1 = Config {
            onboarding_completed_version: 1,
            ..Config::default()
        };
        assert!(v1.onboarding_completed_version < TOUR_VERSION);
        assert!(TourStep::ALL.contains(&TourStep::LibraryRoot));
    }

    /// v3 added the digital/vinyl question and the inline token field. The
    /// medium question must come before the Discogs step, because the Discogs
    /// pitch is written in terms of the answer.
    #[test]
    fn the_medium_question_precedes_the_discogs_step() {
        assert!(TOUR_VERSION >= 3);
        let medium = TourStep::ALL
            .iter()
            .position(|s| *s == TourStep::Medium)
            .unwrap();
        let discogs = TourStep::ALL
            .iter()
            .position(|s| *s == TourStep::Discogs)
            .unwrap();
        assert!(medium < discogs);
    }
}
