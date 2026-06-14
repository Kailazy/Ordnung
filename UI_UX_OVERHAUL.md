# Ordnung — UI/UX Overhaul

Tracking doc for a staged UI/UX pass on the Ordnung egui GUI. Each task is done **one at a time**, confirmed before starting. Paste the **Master prompt** below into a fresh chat to resume from wherever the status table left off.

---

## Master prompt (paste this into a new chat to continue)

> I'm working through a staged UI/UX overhaul of **Ordnung**, my Rust/egui DJ catalog app. The full plan + live status lives in `UI_UX_OVERHAUL.md` at the repo root — read it first. The GUI crate is `crates/ordnung-gui` (toolbar + sidebar in `src/app.rs`, sidebar widgets in `src/sidebar.rs`, the Duplicates view in `src/views.rs`, import/scan jobs in `src/jobs.rs`, the transcode/quality badge system in `src/table.rs` + `crates/ordnung-core/src/model/mod.rs`).
>
> Rules: respect the architecture skill (`ordnung-architecture`) — GUI wraps `ordnung-core`, keep reusable logic in core, never let core depend upward, and source files are sacred (never moved/modified on import). Work **one task at a time**, and **ask me before starting each one**. After finishing a task, update its row in the status table in `UI_UX_OVERHAUL.md` to ✅ and note what changed. Pick up the next ⬜ task in order.

---

## Status

| # | Task | Status | Notes |
|---|------|--------|-------|
| 1 | **Add songs: pick files OR folder** — give a choice (individual songs vs. whole folder) when adding, and avoid a full re-scan of unchanged files every time. | ✅ Done | `Add songs…` is now a menu (Choose files… / Choose folder…); files route through the import path, folders through scan. Rescan skips files already in the catalog with an unchanged size+mtime signature (new `tracks.src_size`/`src_mtime` cols + `Catalog::track_unchanged`), reported as "N unchanged". Self-healing for pre-existing rows. Core test added; full workspace builds + 82 core tests pass. |
| 2 | **Transcode insight (`ltd` + "upsampled from 320")** — explain what `ltd` means and surface tracks likely transcoded up from 320 kbps. | ✅ Done | `ltd` = `Inconclusive` (gradual roll-off, benign). The detection/Quality column/severity-sort already existed; added (1) a `quality_legend_ui` legend on **hover of the Quality header** explaining clean/`~320?`/`lossy`/`ltd` + the 320 insight, (2) one-click **"⚠ Likely transcoded"** preset (+ per-verdict buttons) in the Quality filter popup, (3) comma-OR support in `apply_col_filters` so `~320?, lossy` shows both. Likely-from-320 = the `~320?` verdict. Builds clean, no new clippy warnings. Note: Quality column is in the default set but may be scrolled off-screen right of Format. |
| 3 | **Optimize the top toolbar** — hierarchy, grouping, primary action, utility actions to the right. | ✅ Done | `Add songs…` is now the accent-filled primary (`menu_custom_button`). Analysis trio consolidated: `⚡ Analyze` stays one-click; Re-analyze (force) + Fetch song data fold into an adjacent `▾` menu. `Refresh`/`Settings` + live counts moved into a right-aligned utility group (`counts · ↻ Refresh · ⚙ Settings · Info`); Settings stays enabled while busy, Refresh gated. Filter stays middle-left. All actions + busy-gating preserved. Builds clean, no new clippy warnings. |
| 4 | **Sidebar redesign** — big rectangular buttons; Library largest, playlists smaller & consistent, a distinct section for unique views (Duplicates / Missing). | ✅ Done | Thin `selectable_label` rows replaced by full-width rectangular tiles via a new `nav_button` helper (sidebar.rs). Three pinned sections using nested `show_inside` panels: **Library** (`♪ All songs`, tallest tile, 46px/17pt, accent fill when active) on top; a scrolling **PLAYLISTS** tree in the middle (consistent 30px/13.5pt tiles, `+` moved into the section caption); a distinct bottom-pinned **COLLECTIONS** group (Duplicates / Missing / Vinyl, 34px/14pt) set off by a separator + caption. All behavior preserved: selection highlight (accent), drag-tracks-to-playlist drop targets + hover stroke, inline rename, context menus, missing/vinyl counts. Folder headers bumped to 13.5pt. Builds clean, no new clippy warnings. |
| 5 | **Duplicates as node tiles** — each dup fragment of a song as a tile, ergonomic keep/reject, pre-pick the best copy (already does), batch-trash the rejects. | ⬜ Not started | |

Legend: ⬜ not started · 🔶 in progress · ✅ done

---

## Task detail

### 1 — Add songs: pick files OR folder
- **Now:** `Add songs…` calls `rfd::FileDialog::new().pick_folder()` (app.rs:350) → `spawn_scan` → `run_scan` walks `scan::discover(dir)` and `scan_file`+`upsert_scanned` **every** file each time (jobs.rs:399, 457). Finder only lets you select whole folders, and re-adding re-reads everything.
- **Goal:**
  - On `Add songs…`, let the user choose **specific files** (`rfd …pick_files()`, multi-select audio) **or** a **whole folder** (current behavior). Drop-import already handles mixed files+folders via `run_import`, so route file-picks through that same path.
  - Avoid the full re-scan: skip files already in the catalog and unchanged (by mtime+size, or `content_hash`) so re-adding a folder is near-instant. Decide in-task whether the skip lives in `import_files` (GUI) or as a core helper (preferred if reusable). Honor "source files are sacred."
- **Open question to resolve at start:** present the choice as (a) a small two-button popover under `Add songs…` ("Files…" / "Folder…"), or (b) a split button. Default to (a) unless told otherwise.

### 2 — Transcode insight (`ltd` + likely-upsampled-from-320)
- **What `ltd` is (confirmed from code):** it's `TranscodeVerdict::Inconclusive` (table.rs:1534). Meaning: a low-pass cutoff exists but the edge is **gradual** (< 25 dB/kHz, `STEEP_DB_PER_KHZ`, model/mod.rs:219) — genuine band-limited mastering or an old recording, **not** an encoder brick wall. Reported, never flagged. Tooltip: "Band-limited with a gentle roll-off — not a transcode signature."
- **The four verdicts** (`transcode_verdict`, model/mod.rs:223):
  - `clean` (green) — full-band, looks lossless.
  - `~320?` (yellow, `Suspect`) — sharp cliff near 20 kHz; **consistent with a 320 kbps transcode**, but lossless masters with a 20 kHz shelf also land here → hint, not proof.
  - `lossy` (red, `LikelyLossy`) — sharp wall well below Nyquist; almost certainly upsampled from a lossy source.
  - `ltd` (gray, `Inconclusive`) — gradual roll-off; benign.
- **The "upsampled from 320" signal the user wants** = the `~320?` verdict. `estimated_source_kbps()` (model/mod.rs:243) already maps cutoff → "~320 kbps (or lossless w/ 20 kHz shelf)", "~256", "~192/AAC", "~128", "≤96".
- **Goal:** make this insight findable, not just a hover. Ideas to weigh in-task: a short on-screen legend for the four chips; a filter/sort to isolate `~320?` (and `lossy`) tracks; surface the estimated source bitrate inline. This is primarily an exposure/clarity task — the detection already exists.

### 3 — Optimize the top toolbar
- **Now:** flat row of equal-weight buttons (app.rs:337–602): `Add songs…`, `Analyze`, `Re-analyze`, `Fetch song data`, `Refresh`, contextual (`Convert N…`, `Remove from playlist`, `Write edited`, `Relocate`), then `Settings`, filter, counts, `Info` toggle.
- **Goal:** establish hierarchy — make `Add songs…` the visible primary; group the analysis trio (`Analyze`/`Re-analyze`/`Fetch song data`) as one cluster (possibly a split/▾ button); push utility (`Refresh`, `Settings`) to the right near the counts; keep contextual buttons appearing only when relevant. Preserve every existing action and its busy-state gating.

### 4 — Sidebar redesign
- **Now:** one-line `selectable_label` rows (app.rs:682–752): `♪ All songs`, `⧉ Duplicates`, `⚠ Missing`, then the playlist tree (`draw_playlist_nodes`, sidebar.rs:110), then a pinned `💿 My Vinyl Collection`.
- **Goal (per user):**
  - **Big rectangular buttons** instead of thin one-line rows.
  - **Library** ("All songs") visually **largest**.
  - **Playlists** slightly **smaller than Library** and **consistent** with each other.
  - A **distinct section** for the unique views (**Duplicates**, **Missing** — and likely Vinyl) that reads as its own group, separate from playlists.
  - Keep all behavior: selection highlight, drag-tracks-to-playlist drop targets, inline rename, context menus, counts (missing/vinyl).

### 5 — Duplicates as node tiles
- **Now:** each group renders as a `ui.group` with stacked `render_copy` rows, each row a `✓ Keep | 🗑 Delete` segmented pair (views.rs:304–448). Best copy pre-marked keep via `dup_decisions`; "Delete N marked" commits all marks in one background batch (views.rs:546).
- **Goal (per user):**
  - Render **each duplicate fragment as a tile** (node), the dupes of one song grouped together.
  - **Ergonomic keep/reject** per tile.
  - **Pre-pick the best copy** automatically (already done via `best_copy_index` / `dup_decisions`).
  - **Batch delete**: select the bad dupes, then one trash action commits them (the `🗑 Delete N marked` flow already exists — restyle around tiles, keep the batch-commit + Trash + recoverable behavior).
- Keep the existing safety rails: "deletes the whole track" warning when all copies are marked, "Not a dup" dismissal, keeper inherits playlist slots.

---

## Remaining design suggestions (not yet scheduled — captured for later)

These are from the original design review, **excluding** the items already covered by Tasks 1–5 above (toolbar hierarchy, sidebar grouping, duplicate keep/delete ergonomics, transcode/`ltd` insight, add-songs picker).

### A — Badge legend / glossary (the non-transcode badges)
Beyond the four transcode chips (Task 2), the format/bitrate badges (`AIFF 1411k`, `MP3 320k`) and the ★ best-copy mark have no on-screen key. Consider a small `ⓘ` next to headings that expands a one-line glossary. Note: `AIFF 1411k` tagged `lossy` reads as a contradiction (1411k AIFF is lossless PCM) — the `lossy` there means the *audio originated from a lossy source*; the wording could be clearer (e.g. `from-lossy` / `transcode?`).

### B — Transport / now-playing bar is under-built for a DJ tool
- Show **total duration** (`2:22 / 6:47`), not just elapsed.
- Add **Prev / Next** flanking Play.
- Give the progress bar an **obvious scrub handle** on hover.
- The `Copied for Soulseek — paste into the search box` status sits in the most ignorable corner — make transient feedback a **toast** near the triggering action instead.

### C — Table view: two content gaps + polish
- **Add a BPM/tempo column** next to Key — harmonic mixing needs both; tempo is core to the analysis engine but isn't shown.
- **Album-art placeholders are inconsistent** — ~half are blank gray squares that read as "broken." Use a single tasteful placeholder (faint ♪ / genre-tinted square) so the column looks intentional when art is absent.
- Minor: bump the **title** (primary scan target) toward near-white while keeping album/genre dim, to sharpen row hierarchy.

### D — Duplicates view polish
- Consistent **card treatment** per group (subtle bg, rounded corner) so each group is a scannable unit. (Folds into Task 5's tile design.)
- **Tooltip collision**: the "Move this file to the Trash on commit" tip overlaps the row beneath — ensure tooltips render above content.
- Promote the long gray intro paragraph to a **compact stat strip** (number+label tiles) with the prose behind a "How this works ⓘ" disclosure.

### E — Empty states
When a filter yields 0 tracks, show a deliberate empty state ("No tracks match 'xyz'") rather than a blank panel.

### F — General hierarchy through-line
The information design is strong; the visual hierarchy is flat (everything same weight/color). Across the app, let weight/color do the prioritizing the user currently does themselves — primary actions, primary text, and primary data should stand out from utility/secondary.
