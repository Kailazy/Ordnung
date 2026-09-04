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
pub(crate) const TOUR_VERSION: u32 = 1;

/// One step of the tour. Ordered as the questions actually arrive: what is this,
/// how does my music get in, what does Discogs add, and only then — now that
/// there is something to write — how should writing work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TourStep {
    /// What Ordnung is, and the promise about source files.
    Welcome,
    /// Adding music and what analysis produces.
    Library,
    /// Playing, digging, and the vinyl shelf.
    Crate,
    /// Connecting Discogs and what it fills in.
    Discogs,
    /// The writeback fork: automatic or manual.
    Writeback,
}

impl TourStep {
    /// Every step, in tour order.
    pub(crate) const ALL: [TourStep; 5] = [
        TourStep::Welcome,
        TourStep::Library,
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
}

impl Tour {
    /// Open the tour at the first step, seeded from the live config.
    pub(crate) fn new(auto_write: bool) -> Self {
        Self {
            step: TourStep::Welcome,
            auto_write,
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
            self.tour = Some(Tour::new(self.config.auto_write_tags));
        }
    }

    /// Open the tour on demand, from Settings. Reopening never re-runs on the
    /// next launch by itself: finishing stamps the version again, and closing
    /// leaves whatever stamp was already there.
    pub(crate) fn open_tour(&mut self) {
        self.tour = Some(Tour::new(self.config.auto_write_tags));
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
                                step_heading(
                                    ui,
                                    "Connect Discogs",
                                    "Optional, but it fills in what your files are missing.",
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
                                feature_row(
                                    ui,
                                    crate::ui::icon::record,
                                    "Your collection",
                                    "Records you own, shown next to your files.",
                                );
                                ui.label(
                                    egui::RichText::new(
                                        "Settings \u{2192} Discogs, with a free token. Read-only \u{2014} \
                                         Ordnung never edits your Discogs account.",
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
                                let card = |ui: &mut egui::Ui,
                                            selected: bool,
                                            mark: Mark,
                                            title: &str,
                                            body: &str|
                                 -> bool {
                                    let stroke = if selected {
                                        egui::Stroke::new(1.5, accent)
                                    } else {
                                        egui::Stroke::new(
                                            1.0,
                                            crate::ui::tokens::color::SEPARATOR_OPAQUE,
                                        )
                                    };
                                    let resp = egui::Frame::none()
                                        .fill(ui.visuals().extreme_bg_color)
                                        .rounding(egui::Rounding::same(
                                            crate::ui::tokens::radius::SM,
                                        ))
                                        .stroke(stroke)
                                        .inner_margin(egui::Margin::symmetric(16.0, 14.0))
                                        .show(ui, |ui| {
                                            ui.set_width(ui.available_width());
                                            ui.horizontal_top(|ui| {
                                                // The card's mark, in a tile
                                                // matching the feature rows —
                                                // and it doubles as the
                                                // selection indicator: a filled
                                                // wash when chosen, flat when
                                                // not. The whole card is the
                                                // click target, so there's no
                                                // separate radio to hit.
                                                const TILE: f32 = 34.0;
                                                let (r, _) = ui.allocate_exact_size(
                                                    egui::vec2(TILE, TILE),
                                                    egui::Sense::hover(),
                                                );
                                                let tint = if selected {
                                                    accent
                                                } else {
                                                    crate::ui::tokens::color::LABEL_3
                                                };
                                                ui.painter().rect_filled(
                                                    r,
                                                    egui::Rounding::same(
                                                        crate::ui::tokens::radius::SM,
                                                    ),
                                                    tint.gamma_multiply(if selected {
                                                        0.20
                                                    } else {
                                                        0.10
                                                    }),
                                                );
                                                mark(
                                                    ui.painter(),
                                                    r.center(),
                                                    tint,
                                                    TILE * 0.28,
                                                );
                                                ui.add_space(crate::ui::tokens::space::S4);
                                                ui.vertical(|ui| {
                                                    ui.label(
                                                        egui::RichText::new(title)
                                                            .font(crate::ui::tokens::font::strong(
                                                                crate::ui::tokens::font::headline()
                                                                    .size,
                                                            ))
                                                            .color(
                                                                crate::ui::tokens::color::LABEL,
                                                            ),
                                                    );
                                                    ui.add_space(crate::ui::tokens::space::S2);
                                                    ui.label(
                                                        egui::RichText::new(body)
                                                            .font(crate::ui::tokens::font::body())
                                                            .color(
                                                                crate::ui::tokens::color::LABEL_2,
                                                            ),
                                                    );
                                                });
                                            });
                                        })
                                        .response;
                                    resp.interact(egui::Sense::click()).clicked()
                                };

                                if card(
                                    ui,
                                    auto_write,
                                    crate::ui::icon::sync,
                                    "Automatic",
                                    "Keeps files in sync in the background. Only tracks you \
                                     changed, only their tags. Recommended.",
                                ) {
                                    auto_write = true;
                                }
                                ui.add_space(crate::ui::tokens::space::S3);
                                if card(
                                    ui,
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

        // Apply what the frame decided. The live card selection is kept even
        // when the user steps back and forth, so Finish commits what they see.
        if let Some(t) = self.tour.as_mut() {
            t.auto_write = auto_write;
            if let Some(next) = goto {
                t.step = next;
            }
        }

        if finish {
            self.finish_tour(auto_write);
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

    /// Commit the tour's writeback choice and mark it done.
    fn finish_tour(&mut self, auto_write: bool) {
        let changed = self.config.auto_write_tags != auto_write;
        self.config.auto_write_tags = auto_write;
        if changed {
            // Same re-arm the Settings checkbox does: an explicit choice is a
            // fresh request to try the files again.
            self.auto_write_stalled_at = None;
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

    /// The tour opens showing whatever writeback policy is actually in force,
    /// so a user replaying it isn't told they chose something they didn't.
    #[test]
    fn the_tour_seeds_its_choice_from_the_live_setting() {
        assert!(Tour::new(true).auto_write);
        assert!(!Tour::new(false).auto_write);
        assert_eq!(Tour::new(true).step, TourStep::Welcome);
    }
}
