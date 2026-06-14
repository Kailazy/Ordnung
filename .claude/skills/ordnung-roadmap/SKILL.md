---
name: ordnung-roadmap
description: Live phased roadmap and per-phase definition-of-done for Ordnung. Use to check what to build next, whether a phase is complete, how features should be sequenced, or to update status after finishing work. Keeps implementation order consistent with PLAN.md.
---

# Ordnung roadmap (live status)

Build in order. Do not start a later phase before the earlier one meets its
definition of done (DoD), unless the user explicitly reprioritizes. Update the
status markers in this file as work completes.

Legend: `[ ]` not started · `[~]` in progress · `[x]` done

## Phase 0 — Scaffold  `[x]`
Workspace + crates compile; domain model types exist; catalog schema drafted;
CLI skeleton runs; Camelot/key module implemented and unit-tested.
**DoD:** `cargo build` + `cargo test` pass; `ordnung --help` works; key conversions tested.
Pinned to stable toolchain via `rust-toolchain.toml` (lofty needs rustc ≥1.89).

## Phase 1 — Catalog & tags  `[x]`
`scan` (discover audio, read properties + tags via lofty, upsert catalog with a
filename fallback parser for missing artist/title), `ls` (list/filter catalog),
`tag` (view/edit; opt-in `--write` writeback to source files via lofty).
**DoD met:** scanning a folder populates SQLite; `ls` shows tracks; tag edits persist
to catalog and optionally to file; no source file touched without `tag --write`.
Verified against `testdata/seeker-sample` (16 files, mp3/flac/aiff).

## Phase 1.1 — Rescan precedence  `[x]`
Added a `user_edited` flag (with a forward migration). On rescan, audio properties
always refresh from the file, but tag fields are only overwritten when the track
hasn't been user-edited; `tag --set` marks the track edited. Covered by catalog unit
tests and verified end-to-end (catalog edit survives rescan; source file untouched).

## Phase 2 — Analysis  `[x]`
`analyze [QUERY] [--force]`: pure-Rust DSP — BPM (spectral-flux onset +
autocorrelation, octave-folded), beat anchor, Camelot key (per-frame-normalized,
log-compressed, band-limited chromagram + Krumhansl-Schmuckler), waveform preview,
peak + RMS-dBFS loudness. Parallel via rayon; cached in the `analysis` table; skip
gated on analyzer_version + source size/mtime. `ls` shows BPM + Camelot key.
**DoD met:** all 16 sample tracks analyze; re-run is a no-op; synthetic regression
tests (120 BPM click train, 440 Hz→A) pass. BPM validated against ground truth
(tagged "Lava" BPM=130 → detected 129; "Midtown 120" → 120).
**Key detection upgraded (v4):** moved off naive Krumhansl chroma (which clustered
all tracks on one key) to HPCP-style spectral peak-picking + 4096 FFT + EDM-tuned
`edma` profiles + minor mode bias. Keys now spread correctly across the wheel; the
two same-release DJ Sprinkles tracks agree on Camelot number (4A/4B). See the
`audio-analysis` skill for the full method and citations.
**Still open (Phase 2.1):** no labeled key set yet, so no hard accuracy number —
trust the Camelot *number* over the A/B side. Add manual key correction (key is not
yet an editable field — extend the model + `tag`), bin-adaptive tuning, and a majmin
tiebreak. BPM octave reads can wobble on tonally-sparse tracks. Loudness is RMS dBFS,
not BS.1770 LUFS.

## Phase 3 — Playlists  `[x]`
`playlist` subcommands over the flat master pool: `new [--folder] [--parent ID]`,
`ls` (tree), `show`, `add`, `rm`, `reorder`, `rename`, `mv [--parent ID]`, `delete`.
Two catalog tables: `playlists` (self-referential `parent_id` for folder nesting,
`position` for sibling order) and `playlist_tracks` (unique per playlist, `position`
for track order); both cascade on delete. Invariants enforced in core: tracks only in
playlists not folders, nesting only under folders, no parent cycles, reorder must be a
permutation. Source files never touched — playlists are pure catalog views.
**DoD met:** playlists + nested folders persist and survive reload (file-based reload
unit test); 3 new catalog tests + an end-to-end CLI smoke test pass. `Playlist` model
and `Catalog` CRUD live in `ordnung-core`; the CLI is presentation-only.

## Phase 4 — Conversion  `[x]`
`convert <ID>... --to <FMT> [--bitrate K] [--out DIR] [--in-place] [--yes]`. Engine in
`ordnung-core/convert.rs` shells out to `ffmpeg` (the only subprocess) with per-format
presets — mp3 `libmp3lame` (default 320k), aac `aac` in an `.m4a` container (default
256k), flac/wav(`pcm_s16le`)/aiff(`pcm_s16be`) lossless; bitrate ignored for lossless.
Drops cover-art video streams (`-vn`), carries text metadata (`-map_metadata 0`), and
**verifies every output with ffprobe** (codec must match the target). New files by
default (alongside source or `--out DIR`); refuses to overwrite an existing dest or the
source. `--in-place` encodes to a temp sibling, then removes the original and repoints
the catalog (`Catalog::relink_source` updates path/format/properties; analysis self-
invalidates on the size/mtime change) — gated behind a confirmation unless `--yes`.
Never auto-runs (its own command; `scan` is still separate for cataloging new files).
**DoD met:** user picks format + bitrate; presets verified via ffprobe (flac→flac,
aac→aac@~192–256k); 2 unit tests (extensions/bitrate, path mapping) + end-to-end smoke
tests (new-file, in-place repoint, overwrite-guard, confirm-abort) pass.

## Phase 5 — rekordbox export  `[ ]`
`export`: assemble FAT32/MBR USB — `/CONTENTS`, `export.pdb`, ANLZ `.DAT`/`.EXT`
with beatgrids, cues, waveforms, playlists. (See `rekordbox-format`.)
**DoD:** exported USB re-parses cleanly (round-trip) and loads on rekordbox/CDJ with
tracks, waveforms, beatgrids, cues, and playlists intact.

## Phase 6 — Validation & cues  `[ ]`
Golden-fixture diffing vs rekordbox exports; hot/memory cue editing; compatibility
matrix across CDJ/XDJ models.
**DoD:** round-trip diffs are explained; cue edits export correctly.

## Later — GUI  `[~]`
`ordnung-gui` wraps `ordnung-core` (library grid, harmonic mixing view).
**Started early at user request** (out of phase order): the `ordnung-gui` crate is
in the workspace, built on `egui`/`eframe` with `rfd` for native folder pickers.
First-cut features:
- Catalog table (Artist / Title / Album / Genre / Dur / BPM / Camelot / Format /
  kbps / Path), filterable; reloads from SQLite on every refresh and after a job.
- Toolbar: Scan folder…, Analyze, Re-analyze, **Fetch artwork** (Discogs), Refresh;
  plus an **Abort** button in the status bar for the running scan / artwork fetch.
- **Double-click a track → conversion modal** (target format dropdown, bitrate for
  lossy targets, optional output folder, in-place toggle with warning). Routes to
  the existing `ordnung-core::convert` engine and reuses the CLI's in-place
  relink-source behavior (`Catalog::relink_source` + `scan_file`). No engine logic
  duplicated; GUI is pure presentation/policy, mirroring `ordnung-cli`.
- All scan/analyze/convert work runs on a background `std::thread`; messages flow
  back via `mpsc` and the worker calls `ctx.request_repaint()` to drive the UI.
- **Fetch artwork (Discogs)**: for tracks with no embedded cover, query Discogs
  (needs `DISCOGS_TOKEN`), paced ~1 req/1.1s. Each match is reviewed one-at-a-time
  in a Save/Skip modal *before* anything is written to the catalog; Skip writes
  nothing (track is re-queried on a later run). Scan + fetch are cancellable via an
  Abort button (`Arc<AtomicBool>` polled by the worker between items).

- **Multi-select + unified drag (playlist *and* rekordbox), one gesture.** The
  table supports Cmd-click (toggle) / Shift-click (range) multi-selection on top of
  the single primary row that drives the inspector (`App::selection: HashSet<Id>` +
  `select_anchor`; resolved in `apply_click_selection`). A plain row-drag always
  sets an egui DnD payload (`DraggedTracks`), so it can be dropped onto a playlist
  leaf in the sidebar (`draw_playlist_leaf` → `dnd_release_payload`). The instant
  the cursor leaves the window mid-drag (`update` watches `pointer.hover_pos` vs
  `screen_rect`), it hands the same tracks off to a real macOS `NSDraggingSession`
  (`macos_drag::begin_file_drag`) carrying the source files as file URLs, so they
  drop straight into rekordbox / Finder and import — no Finder round-trip, no
  modifier key. The two can't run at once because the native session blocks egui's
  event loop until the drop completes, so the window-edge crossing is what arbitrates
  in-app vs out-of-app (latched by `App::native_drag_active`, gated by
  `drag_seen_inside` so a transient `None` cursor pos at drag-start can't fire it).
  Crossing the edge *commits* to the external drag and immediately drops the egui
  payload, so dragging back in and releasing — over a playlist or anywhere —
  is a no-op; only an actual drop outside the window acts. Native code is
  isolated in `crates/ordnung-gui/src/macos_drag.rs` (`#[cfg(target_os = "macos")]`
  via objc2/objc2-app-kit, no-op stub elsewhere); it references files by path and
  never touches the catalog or source bytes. Note: the drag-out carries the audio
  only — Ordnung's beatgrids/cues/keys still require the Phase 5 native USB export.

Still to do for full GUI parity: tag editing, intra-playlist track reordering by
drag (adding tracks to a playlist by drag now works; reordering within one does
not), multi-select batch conversion (the selection model now exists — wire the
convert modal to it), waveform preview, harmonic-mixing key view.

GUI artwork TODOs:
- **Multi-candidate release picker.** The Discogs search already returns up to 10
  releases (`per_page=10`), but `discogs::find_artwork` auto-takes the first hit.
  Return the full candidate list (release id / title / year / label / thumbnail)
  and turn the Save/Skip modal into a picker so the user can *choose one of many*
  releases per track. See the multi-candidate picker in
  `docs/design/discogs-track-inspector.md` §5. (Code pointer: `find_artwork` TODO.)

---
When finishing work, flip the marker and note anything that changed the design back
into `PLAN.md` and the relevant skill (`ordnung-architecture`, `rekordbox-format`,
`audio-analysis`).
