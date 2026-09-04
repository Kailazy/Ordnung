# Large libraries: onboarding, analysis, and Discogs at scale

**Status:** in progress. Quick wins landed in v0.49.2; step 1 (library root +
tour on-ramp) landed in v0.50.0. Everything else below is unbuilt.
**Goal:** a new user pointing Ordnung at 30–50k tracks gets a first run that is
legible, resumable, and mostly hands-off — instead of an empty window followed
by hours of opaque work and tens of thousands of manual picks.

---

## The premise that has to change first

`PLAN.md` §1 and `HANDOFF.md` §37 both state the design target as
**"< 2k tracks, local drive."** Everything below assumes that target moves by
more than an order of magnitude. That is a real product decision, not a
performance detail, and it should be made explicitly and written into `PLAN.md`
rather than left implied by the code.

It also has consequences **outside** the three paths this doc covers — table
virtualization, search, and cover-texture memory were not audited and may need
their own work before the claim is safe to make. Treat that as a prerequisite
investigation, not a footnote.

---

## What already landed (v0.49.2)

Three fixes that were straightforwardly missing, not design questions:

1. **Analysis saves stream to the catalog per track.** The fan-out used to
   `collect()` and write only after the whole batch, so a crash mid-sweep threw
   away the entire run. Now resumable wherever it stopped.
2. **The Discogs fetch reports determinate progress.** It was the only long job
   in `jobs.rs` with no `JobMsg::Progress`.
3. **5xx and transport failures retry.** Previously only 429 did, so a transient
   blip permanently dropped that track from a sweep.

These make long runs survivable. They do not make them *short*, or *unattended*.

---

## The four real problems

### P1 — There is no on-ramp

There is no library-root setting anywhere. Grepping
`library_path|music_dir|root_dir|watch_folder` across the GUI returns nothing.
Music enters only via `Add songs…` or drag-drop, so:

- there is no persistent notion of "my library lives here";
- there is no way to detect new arrivals since last launch;
- re-importing means re-picking the folder by hand.

The onboarding tour (`onboarding.rs`, 5 steps, wired and tested) explains what
Ordnung *does* but never asks for a folder or a token — the two things a new
user must actually provide. It ends by handing them an empty window with no
next step, and there is no first-run empty state (the only empty-library copy
lives in the USB-device view path).

### P2 — Hours of work with no sense of scale

Nothing anywhere branches on library size. The toolbar's `{} tracks` counts
*visible rows*, not the library. A user who imports 40k files gets no estimate,
no warning, and no explanation of what the next few hours will look like — which
is the single most likely reason to conclude the app has hung.

Compounding it: **one job at a time, globally.** `is_busy()` gates the toolbar,
file drops, and the inspector, so during a long first analysis the app is
largely read-only.

### P3 — Discogs demands one manual pick per track

`run_fetch_tracks` sends every track to the picker as `ArtworkChoices`; there is
no auto-accept path, and **no confidence score of any kind** —
`find_artwork_candidates` returns Discogs' own search ranking with no notion of
"this one is obviously right." 30k tracks means 30k modal decisions.

Two things make this worse than it looks:

- **The batch infrastructure exists and is unreachable.** `tracks_missing_metadata()`
  and `tracks_missing_artwork()` are fully implemented and tested with **zero
  callers** outside `catalog.rs`. `discogs_meta_fetched_at`, `mark_metadata_fetched`
  and `clear_metadata_fetched` are all written to — but nothing *reads* the
  marker to build a work list or suppress a re-query. So negative results are
  recorded and then ignored: re-running a fetch on a known no-match pays the
  full 1–4 searches again.
- **Badly-tagged tracks cost the most.** `resolve_hits` tries album×2 keys then
  title×2 keys, so a *no-match* burns four sequential requests at ~1.1s. The
  tracks that most need help are the most expensive to fail on.

### P4 — Analysis redoes work it doesn't need to

- **`ANALYZER_VERSION` is monolithic** (currently **20**). Any bump invalidates
  *everything* — a tempo-only change re-decodes and recomputes key, waveform,
  fingerprint and loudness that did not change. This has already fired 20 times.
- **Two full STFTs per track.** A materialized 150s spectrogram (~105MB) drives
  key/tempo/quality/downbeat/fingerprint; `waveform::color_bands` then runs a
  *second*, independent full-track STFT at the same window and hop. On a
  6-minute track that second pass is ~2.4× the FFT work of the first.
- **Scan is single-threaded** while analysis is fully parallel — backwards for a
  large first import. Per file it's a lofty tag parse + symphonia probe + cover
  decode/downscale + SQLite write, serially. And `scan::discover` does a full
  blocking `WalkDir` + sort before *any* progress appears.

---

## Plan

Ordered so each step makes the next one cheaper or more legible. Items marked
**[decision]** need a product call before implementation.

### 1. Library root + first-run import  *(addresses P1)* — **done, v0.50.0**

Landed as planned: `Config::library_root`, a tour step ("Where does your music
live?") that picks the folder and kicks off the first import on Finish,
`TOUR_VERSION` 2 so existing users are asked once, and a Settings → General
section to view/change the folder and scan it for new songs on demand.

- Bump `TOUR_VERSION` to replay once for existing users — they don't have a root
  either, so this is the right migration mechanism.
- Must follow **R1** from `UI_UX_OVERHAUL.md`: fixed-size stepped dialog via
  `ui::sheet::stepped`, controls that don't apply drawn *disabled, not hidden*.
  `onboarding.rs` is the named reference implementation.
- Unlocks a later "scan for new arrivals" that doesn't require re-picking a folder.

### 2. Size awareness before committing  *(addresses P2)*

Once discovery knows it found N files, say so *before* the long work starts:
rough analysis-time estimate (now computable from N × measured per-track cost),
that it runs in the background, and that it is resumable — which, since v0.49.2,
is now true.

Pair with a first-run empty state (backlog item **E** in `UI_UX_OVERHAUL.md`
covers the filter case; this is the fresh-install case).

### 3. Wire up the orphaned batch queries  *(addresses P3)*

The smallest change with the largest unlock. Give `tracks_missing_metadata()` a
caller, and make the fetch path *honour* `discogs_meta_fetched_at` instead of
ignoring it. This simultaneously:

- enables a library-wide sweep (mostly wiring, not new design), and
- makes negative caching actually suppress re-queries.

Keep the existing per-track re-pick as an explicit override that still ignores
the marker — that behaviour is deliberate and documented.

### 4. Confidence scoring + auto-accept  **[decision]**  *(addresses P3)*

Score candidates locally — artist/title/album string distance, year agreement,
format agreement. No extra API calls. High-confidence matches apply without a
modal; only genuine ambiguity reaches the picker. This is what turns 30k picks
into a few hundred.

**The product decision:** auto-applying writes tags without per-track
confirmation, which bends the explicit-only rule in `PLAN.md` §2. Recommended
shape — apply automatically but surface an *"N auto-matched — review"* affordance
with bulk undo, so consent is explicit at the **batch** level rather than per
track, and nothing is silent. The threshold should be user-visible, not a hidden
constant.

Depends on #3 to be worth much (a sweep with no auto-accept is just a longer
queue). Note the review queue is currently RAM-only (`VecDeque<ArtworkChoices>`
holding thumbnail bytes) — holding thousands is not viable, so this needs a
persistent or bounded queue regardless.

### 5. Exact-key search first  *(addresses P3)*

Search by catalogue number and ISRC before falling back to fuzzy. `catno` is
already parsed *from* Discogs but never searched *by*; the catalog already
indexes `isrc`. Either collapses the 1–4 fuzzy calls into one high-confidence
hit, and feeds directly into #4's scoring.

### 6. Parallelize scan  *(addresses P4)*

Tag/probe/cover work is I/O-and-parse bound and trivially parallelizable —
mirror the analysis fan-out. Stream `scan::discover` results instead of
collecting the whole walk first, so progress appears immediately on a big tree.

### 7. Share the first 150s of STFT between the two passes  *(addresses P4)*

Pure compute win, no UX change: have `color_bands` reuse the frames already
computed for the key window rather than recomputing them. At 40k tracks a
fraction of per-track FFT time is hours of wall clock.

### 8. Per-stage analyzer versions  *(addresses P4)*

Split `ANALYZER_VERSION` into per-stage versions (`tempo_version`,
`key_version`, …) checked against the existing per-stage columns. Turns most
future analyzer releases from a multi-hour full re-analysis into a near no-op.
Highest long-term value, lowest urgency — it pays off at v21, not today.

---

## Smaller things worth folding in

- **`content_hash` is dead weight** — computed and stored, never consulted for
  cache decisions (and can't be, since computing it requires decoding).
- **`physical_memory_bytes()` shells out to `sysctl`**, so the memory clamp on
  the analysis pool silently no-ops off macOS. Fine today; latent if Ordnung
  ever ships Linux/Windows.
- **No force-reanalyze in the GUI** — `force` is hardcoded `false` at every call
  site, so a user who wants to redo one track can't.
- **The Discogs token is stored plaintext** in `~/.ordnung/config.toml` rather
  than the Keychain.
- **The User-Agent literal is duplicated at 8 construction sites** — a version
  bump needs all eight.
- **Cancel granularity during a Discogs fetch is per-track**, so a cancel won't
  land until the current track's up-to-4 searches and 6 thumbnail downloads
  finish.

---

## Suggested sequencing

**Now:** 1 and 2 together — they are the on-ramp, and they make every long
operation below legible rather than alarming.

**Next:** 3, then 5, then 4. This order means the sweep exists before it is made
smart, and exact-key matching is feeding the scorer by the time auto-accept
turns on.

**Then:** 6 and 7, both pure throughput with no product surface.

**Eventually:** 8, before the next analyzer change rather than after.
