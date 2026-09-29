//! The analysis lane: BPM, key, beatgrid and quality analysis on a worker of
//! its own, off the shared job slot. A library sweep runs for hours, and while
//! it held `job_rx` it locked out importing, converting, syncing and every
//! other job. On its own lane those run beside it, and a second Analyze (or an
//! import's new tracks) joins the queue that's already running instead of
//! being refused. Same idea as the vinyl edit lane (see
//! [`App::spawn_vinyl_edit`]), except analysis work is added to a live run
//! rather than queued behind it.
use super::*;
use std::sync::atomic::AtomicU64;

/// What to analyze: the current filtered view (`Query`) or an explicit set of
/// track ids (the right-click selection, an import's new tracks).
pub(crate) enum AnalyzeTargets {
    Query(Option<String>),
    Ids(Vec<Id>),
}

/// What the lane's coordinator receives: work to add to the run, or one
/// pool worker's finished track (`None` when a cancel skipped it).
enum LaneIn {
    Add {
        targets: AnalyzeTargets,
        force: bool,
    },
    Finished {
        id: Id,
        size: u64,
        mtime: i64,
        result: Option<Result<Analysis, String>>,
    },
}

/// Adds work to the lane. Cheap to clone: an import worker carries one so
/// its new tracks go to the lane when the files are in, and its own job ends
/// there rather than holding the slot through the analysis.
#[derive(Clone)]
pub(crate) struct AnalysisQueue(Sender<LaneIn>);

impl AnalysisQueue {
    pub(crate) fn add(&self, targets: AnalyzeTargets, force: bool) {
        let _ = self.0.send(LaneIn::Add { targets, force });
    }
}

/// The UI's end of the lane, started on first use and kept for the session.
pub(crate) struct AnalysisLane {
    queue: AnalysisQueue,
    rx: Receiver<JobMsg>,
    /// Bumped by Cancel. Every queued track carries the generation it was
    /// queued under and is skipped once that's no longer current, so a cancel
    /// drops everything queued so far and nothing added after it.
    generation: Arc<AtomicU64>,
    /// `(done, total)` of the current run; `None` when the lane is idle.
    progress: Option<(usize, usize)>,
}

impl App {
    /// The lane's queue, starting the lane if it isn't running yet.
    pub(crate) fn analysis_queue(&mut self) -> AnalysisQueue {
        if let Some(lane) = &self.analysis {
            return lane.queue.clone();
        }
        let (in_tx, in_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        let generation = Arc::new(AtomicU64::new(0));
        let db = self.db_path.clone();
        let ctx = self.egui_ctx.clone();
        let (back, gen) = (in_tx.clone(), generation.clone());
        thread::spawn(move || run_lane(db, in_rx, back, gen, tx, ctx));
        let queue = AnalysisQueue(in_tx);
        self.analysis = Some(AnalysisLane {
            queue: queue.clone(),
            rx,
            generation,
            progress: None,
        });
        queue
    }

    /// Analyze the current filtered view. Skips tracks already analyzed at
    /// the current version unless `force`.
    pub(crate) fn spawn_analyze(&mut self, force: bool) {
        let query = (!self.filter.trim().is_empty()).then(|| self.filter.clone());
        self.analysis_queue().add(AnalyzeTargets::Query(query), force);
    }

    /// Analyze a specific set of tracks (the context-menu selection) rather
    /// than the whole filtered view.
    pub(crate) fn spawn_analyze_ids(&mut self, ids: Vec<Id>, force: bool) {
        self.analysis_queue().add(AnalyzeTargets::Ids(ids), force);
    }

    /// `(done, total)` of the running analysis, `None` when none is running.
    pub(crate) fn analysis_progress(&self) -> Option<(usize, usize)> {
        self.analysis.as_ref().and_then(|l| l.progress)
    }

    /// Stop the running analysis after the tracks already decoding. What
    /// finished stays saved.
    pub(crate) fn cancel_analysis(&mut self) {
        if let Some(lane) = &self.analysis {
            lane.generation.fetch_add(1, Ordering::Relaxed);
            self.status = "Stopping analysis…".into();
        }
    }

    /// Drain the analysis lane. Returns true if rows should reload.
    pub(crate) fn poll_analysis(&mut self) -> bool {
        let Some(lane) = &mut self.analysis else {
            return false;
        };
        let mut reload = false;
        loop {
            match lane.rx.try_recv() {
                Ok(JobMsg::Status(s)) => self.status = s,
                Ok(JobMsg::Progress { done, total }) => lane.progress = Some((done, total)),
                Ok(JobMsg::Done(s)) => {
                    self.status = s;
                    lane.progress = None;
                    reload = true;
                }
                Ok(JobMsg::Failed(s)) => {
                    self.status_failed = s.clone();
                    self.status = s;
                }
                Ok(JobMsg::Failures { title, items }) => {
                    self.show_failure_report = !items.is_empty();
                    self.failure_report_title = title;
                    self.failure_report = items;
                }
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // The coordinator died (it only ends if it panics). Drop
                    // the lane so the next Analyze starts a fresh one.
                    self.analysis = None;
                    reload = true;
                    break;
                }
            }
        }
        reload
    }
}

/// The current run's tally. A run starts when work lands on an idle lane and
/// ends when every track queued into it has finished, however many batches
/// were added along the way.
#[derive(Default)]
struct Run {
    /// Every track queued into this run, with its file name for the failure
    /// report. Also what keeps a track asked for twice from running twice.
    names: HashMap<Id, String>,
    done: usize,
    ok: usize,
    failed: usize,
    skipped: usize,
    fails: Vec<(String, String)>,
}

/// The lane's coordinator: owns the catalog connection, resolves added work
/// to the tracks that actually need analysis, fans them out to the analysis
/// pool and saves each result as it lands.
///
/// Results are saved per track rather than collected: a full-library sweep
/// runs for hours, and saving as it goes makes a crash, force-quit or power
/// loss at hour three resumable at the point it stopped (`needs_analysis`
/// re-derives only what's left). The SQLite write is trivial next to the
/// decode+FFT that produced it.
fn run_lane(
    db: PathBuf,
    rx: Receiver<LaneIn>,
    back: Sender<LaneIn>,
    generation: Arc<AtomicU64>,
    tx: Sender<JobMsg>,
    ctx: egui::Context,
) {
    // A pool of the lane's own, never rayon's global one: every task here is
    // seconds of decode, and a queue of thousands of them on the global pool
    // would starve any other `par_iter` in the app until the sweep ended. Sized
    // for memory (see `analysis_pool`), else one worker per core.
    let pool = match crate::jobs::analysis_pool() {
        Some(p) => p,
        None => match rayon::ThreadPoolBuilder::new().build() {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(JobMsg::Failed(format!("Couldn't start analysis: {e}")));
                ctx.request_repaint();
                return;
            }
        },
    };
    let mut catalog: Option<Catalog> = None;
    let mut run = Run::default();
    for msg in rx {
        match msg {
            LaneIn::Add { targets, force } => {
                if catalog.is_none() {
                    match Catalog::open(&db) {
                        Ok(c) => catalog = Some(c),
                        Err(e) => {
                            let _ = tx.send(JobMsg::Failed(format!("Couldn't open the catalog: {e}")));
                            ctx.request_repaint();
                            continue;
                        }
                    }
                }
                let Some(catalog) = &catalog else { continue };
                let tracks: Vec<Track> = match targets {
                    AnalyzeTargets::Query(query) => match catalog.list_tracks(query.as_deref(), 0) {
                        Ok(t) => t,
                        Err(e) => {
                            let _ = tx.send(JobMsg::Failed(e.to_string()));
                            ctx.request_repaint();
                            continue;
                        }
                    },
                    // Silently skip any id that vanished since it was asked for.
                    AnalyzeTargets::Ids(ids) => ids
                        .iter()
                        .filter_map(|&id| catalog.get_track(id).ok())
                        .collect(),
                };
                let gen = generation.load(Ordering::Relaxed);
                let mut added = 0usize;
                for t in &tracks {
                    if run.names.contains_key(&t.id) {
                        continue;
                    }
                    let (size, mtime) = crate::jobs::file_stamp(&t.source_path);
                    match catalog.needs_analysis(t.id, size, mtime, ANALYZER_VERSION) {
                        Ok(stale) if stale || force => {}
                        _ => continue,
                    }
                    let name = Path::new(&t.source_path)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| t.source_path.clone());
                    run.names.insert(t.id, name);
                    added += 1;
                    let (id, path) = (t.id, t.source_path.clone());
                    let (back, generation) = (back.clone(), generation.clone());
                    // FIFO so tracks run in the order they were asked for. A
                    // cancel can't interrupt a decode already in flight, but
                    // every track not yet started skips at once, so a queue of
                    // thousands stops in seconds, not hours.
                    pool.spawn_fifo(move || {
                        let result = (generation.load(Ordering::Relaxed) == gen)
                            .then(|| analysis::analyze_file(&path).map_err(|e| e.to_string()));
                        let _ = back.send(LaneIn::Finished {
                            id,
                            size,
                            mtime,
                            result,
                        });
                    });
                }
                let total = run.names.len();
                if total == 0 {
                    let _ = tx.send(JobMsg::Done(if tracks.is_empty() {
                        "No matching tracks to analyze.".into()
                    } else {
                        format!("All {} track(s) already analyzed.", tracks.len())
                    }));
                } else if added > 0 {
                    let _ = tx.send(JobMsg::Status(format!("Analyzing {total} track(s)…")));
                    let _ = tx.send(JobMsg::Progress { done: run.done, total });
                }
                ctx.request_repaint();
            }
            LaneIn::Finished {
                id,
                size,
                mtime,
                result,
            } => {
                run.done += 1;
                let failure = match result {
                    None => {
                        run.skipped += 1;
                        None
                    }
                    Some(Ok(a)) => match catalog.as_ref().map(|c| c.save_analysis(id, &a, size, mtime)) {
                        Some(Err(e)) => Some(format!("couldn't save analysis: {e}")),
                        _ => {
                            run.ok += 1;
                            None
                        }
                    },
                    Some(Err(e)) => Some(format!("analysis failed: {e}")),
                };
                if let Some(why) = failure {
                    run.failed += 1;
                    let name = run.names.get(&id).cloned().unwrap_or_else(|| format!("track {id}"));
                    run.fails.push((name, why));
                }
                let total = run.names.len();
                let _ = tx.send(JobMsg::Progress { done: run.done, total });
                if run.done == total {
                    let finished = std::mem::take(&mut run);
                    if !finished.fails.is_empty() {
                        let _ = tx.send(JobMsg::Failures {
                            title: "Analyze".into(),
                            items: finished.fails,
                        });
                    }
                    // A cancelled run reports what it kept, so the user knows
                    // the finished analyses were saved and only the rest dropped.
                    let (ok, failed, skipped) = (finished.ok, finished.failed, finished.skipped);
                    let _ = tx.send(JobMsg::Done(if skipped > 0 {
                        format!(
                            "Analysis cancelled: {ok} of {total} track(s) analyzed, \
                             {failed} failed, {skipped} skipped."
                        )
                    } else {
                        format!("Analyzed {ok} track(s), {failed} failed.")
                    }));
                }
                ctx.request_repaint();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ordnung_core::catalog::ScannedTrack;
    use ordnung_core::model::{AudioProperties, Tags};

    fn catalog_with(dir: &Path, names: &[&str]) -> (PathBuf, Vec<Id>) {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let db = dir.join("catalog.db");
        let catalog = Catalog::open(&db).unwrap();
        let ids = names
            .iter()
            .map(|n| {
                let t = ScannedTrack {
                    // Never created: every analysis fails fast, which is all
                    // the lane's bookkeeping needs.
                    source_path: dir.join(n).to_string_lossy().into_owned(),
                    format: Format::Mp3,
                    properties: AudioProperties {
                        sample_rate_hz: 44100,
                        bit_depth: None,
                        channels: 2,
                        duration_ms: 1000,
                        bitrate_kbps: Some(320),
                    },
                    tags: Tags::default(),
                    cover_thumb: None,
                    fingerprint: None,
                    src_size: None,
                    src_mtime: None,
                };
                catalog.upsert_scanned(&t).unwrap().0
            })
            .collect();
        (db, ids)
    }

    /// Drive a coordinator over work queued before it starts, and return
    /// every message it sent up to its first `Done`.
    fn run_until_done(db: PathBuf, adds: Vec<AnalyzeTargets>) -> Vec<JobMsg> {
        let (in_tx, in_rx) = mpsc::channel();
        let (tx, rx) = mpsc::channel();
        for targets in adds {
            AnalysisQueue(in_tx.clone()).add(targets, false);
        }
        let back = in_tx.clone();
        let generation = Arc::new(AtomicU64::new(0));
        thread::spawn(move || run_lane(db, in_rx, back, generation, tx, egui::Context::default()));
        let mut msgs = Vec::new();
        while let Ok(m) = rx.recv_timeout(Duration::from_secs(30)) {
            let done = matches!(m, JobMsg::Done(_));
            msgs.push(m);
            if done {
                break;
            }
        }
        msgs
    }

    /// A second batch landing while the first runs joins the same run: one
    /// total, one Done, and a track asked for twice runs once.
    #[test]
    fn added_work_joins_the_running_run() {
        let dir = std::env::temp_dir().join(format!("ordnung-lane-{}", std::process::id()));
        let (db, ids) = catalog_with(&dir, &["a.mp3", "b.mp3", "c.mp3"]);
        let msgs = run_until_done(
            db,
            vec![
                AnalyzeTargets::Ids(vec![ids[0], ids[1]]),
                AnalyzeTargets::Ids(vec![ids[1], ids[2]]),
            ],
        );
        let last_progress = msgs.iter().rev().find_map(|m| match m {
            JobMsg::Progress { done, total } => Some((*done, *total)),
            _ => None,
        });
        assert_eq!(last_progress, Some((3, 3)));
        let failures = msgs.iter().find_map(|m| match m {
            JobMsg::Failures { items, .. } => Some(items.len()),
            _ => None,
        });
        assert_eq!(failures, Some(3));
        assert!(matches!(msgs.last(), Some(JobMsg::Done(s)) if s == "Analyzed 0 track(s), 3 failed."));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Work with nothing to do on an idle lane still answers, so the click
    /// isn't met with silence.
    #[test]
    fn nothing_to_do_says_so() {
        let dir = std::env::temp_dir().join(format!("ordnung-lane-empty-{}", std::process::id()));
        let (db, _) = catalog_with(&dir, &[]);
        let msgs = run_until_done(db, vec![AnalyzeTargets::Ids(vec![999])]);
        assert!(matches!(msgs.last(), Some(JobMsg::Done(s)) if s == "No matching tracks to analyze."));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
