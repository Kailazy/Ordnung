# Tracklist match — paste a mix, get the records

**Status:** built (2026-09-17, v0.116.0). Parser and scorer in
`ordnung-core/src/tracklist.rs` (25 unit tests on the shapes in §3 and the
weights in §4.2), `discogs::Client::find_track_releases` with the label rung,
`tracklists` / `tracklist_lines` tables (schema v17) with the candidate list
kept as JSON, the streaming job `jobs::run_match_tracklist` (verification
against the cached release tracklist for anything short of Sure, map pins
for matched records), and the Tracklists tab in `ordnung-gui/src/tracklists.rs`.
Deviations from the draft: it is not a tab (v0.116.1): a tiny ≡ glyph in
the top bar, left of the counts, opens a Tracklists window with its own
search box, so the feature is reachable from every view and the vinyl
tab strip stays shelves + market + map; the pressing rule (`best_candidate`) stayed in
the GUI jobs module since its `ReleaseAutoMatch` enum is GUI config; there
is no *Paste tracklist* button at all (v0.120.3): the one-line paste box is
simply there at the top of the window and grows with its text, with the
name field, Match and the parse preview on a control row under it once it
holds text; and the row carries no live price, the record sheet a click
away does.
Still open from §7 D: playlist-from-owned-lines, wantlist-watch tie-in,
a `source` on `DugRelease`, Soulseek hand-off.
**Target surface:** Ordnung GUI, vinyl side (`ordnung-gui`), engine in `ordnung-core`.
**One line:** paste any mix tracklist, Ordnung looks every song up and matches it
to its Discogs record, then lets you want, dig or buy the ones you don't have.

---

## 1. Why

The most common way a DJ finds records is not searching: it is hearing a mix,
opening its tracklist (1001tracklists, a SoundCloud description, a Mixcloud
page, a MixesDB entry, a YouTube comment) and IDing the lines one at a time on
discogs.com. Twenty-five copy-pastes, twenty-five search pages, twenty-five
"is this the 12" or the compilation?" decisions. Ordnung already has every
piece of that loop except the batch: free-text record lookup, the release
sheet, want/collect, the dig, the record map, seller crates and live prices.
This feature is the batch.

Today's closest thing is the toolbar search box in Discogs mode, one line at
a time by hand. The import-time auto-match (`jobs::auto_match_tracks`) already
does "text in, release out" for library files with no picker. Tracklist match
is that same engine pointed at pasted lines instead of catalog tracks, with a
review surface instead of a silent commit.

## 2. What the user does

1. In the vinyl view, open the new **Tracklists** tab. There is no big
   call-to-action: a small, quiet **Paste tracklist** text button sits at the
   top of the tab in the same weight as the shelf's secondary controls (no
   fill, no accent, hover reveals it). ⌘V with text on the clipboard while
   the tab is focused does the same thing without the button.
2. Pressing it opens a compact inline paste box, not a modal: a single-line
   text field at the reference `control_row` height, with the name field and
   a **Match** button on the same row. The box grows only as pasted text
   grows: one line stays one line, a 25-line paste stretches it to fit those
   lines (capped at about a third of the view, then it scrolls inside), and
   deleting text shrinks it back. The row directly under it is the live
   parse preview, *"24 tracks · 3 IDs · 2 lines skipped"*, which appears
   only once there is text. The name pre-fills from a `Tracklist:` / title
   line, else "Pasted 17 Sep 2026". Press **Match**.
3. The list appears at once with every line in pasted order. A background
   job resolves lines top to bottom; each row fills in as its answer lands
   (cover thumb, record title, year, label + catno, format, confidence pip).
   Status bar: *"Matching 9 of 24…"* with Abort.
4. Each row is badged **OWNED** / **WANT** (shelf membership) and
   **IN LIBRARY** (a local file matches), so what's left to buy is obvious.
   Price shows live from the marketplace stats, never cached.
5. Row actions: **♥ Want**, open the record sheet (row click), **◈ Dig** from
   it, **Pick another…** (reopens the candidate picker with the other hits, or
   the Discogs search box prefilled with the line), **✖ Not this**.
   List actions: **Want all matched**, **Show on map**, **Re-match unsure**,
   **Copy as text**.
6. The tracklist is saved. It stays in the Tracklists tab with its match state,
   and every matched record joins the record map as a dug record, so the mix
   becomes a cluster on the map you can keep digging from.
7. A saved tracklist can be reopened in the paste box (**Edit the paste** in
   the ⋯ menu) to add, fix or remove lines. **Save** writes the text back
   through `Catalog::update_tracklist`: a line that still reads the same
   (ordinal aside) keeps its match, candidates and the user's say, an edited
   or new line starts unmatched, and only the unsettled lines are looked up.

Not in scope: fetching a tracklist from a URL. 1001tracklists and the other
sites sit behind Cloudflare exactly like `discogs.com/sell` did in the sellers
spike (`bulk-sellers-spike.md`); the verdict there stands. Paste only.

## 3. Parsing the paste

New core module `ordnung-core/src/tracklist.rs`, pure functions, unit-tested
against real pasted samples checked into `testdata/tracklists/`.

```rust
pub struct TracklistLine {
    pub position: usize,          // 1-based, counting track lines only
    pub raw: String,              // the line as pasted, for display and re-parse
    pub timestamp: Option<u32>,   // seconds, from [12:34] / 1:02:03 / 12:34 prefixes
    pub artist: Option<String>,
    pub title: Option<String>,
    pub label_hint: Option<String>,   // from a trailing [Label] / (Label) / - Label
    pub catno_hint: Option<String>,   // from a trailing [LABEL 012] style token
    pub kind: LineKind,           // Track | Id | Noise
}
pub fn parse_tracklist(text: &str) -> Vec<TracklistLine>;
```

Shapes it has to handle (each becomes a test case):

| Pasted line | artist | title | hint |
|---|---|---|---|
| `01. Metro Area - Miura` | Metro Area | Miura | |
| `[12:34] Theo Parrish – Solitary Flight [Sound Signature]` | Theo Parrish | Solitary Flight | label |
| `1:02:03 Pépé Bradock "Deep Burnt" (Kif)` | Pépé Bradock | Deep Burnt | label |
| `DJ Sprinkles - Grand Central, Pt. I (Deep Into The Bowel Of House)` | DJ Sprinkles | full title | |
| `Moodymann - Shades Of Jae - KDJ` | Moodymann | Shades Of Jae | label |
| `Larry Heard - The Sun Can't Compare (Long Version) [ALLV 001]` | Larry Heard | title | catno |
| `w/ Kerri Chandler - Rain` | Kerri Chandler | Rain | (mashup, kept) |
| `ID - ID`, `Unknown - ?`, `??? - Untitled` | | | kind = Id |
| `Tracklist:`, `-----`, `12:34`, blank | | | kind = Noise |

Rules: normalise every dash variant (`-`, `–`, `—`, `‐`) and smart quotes;
strip a leading ordinal (`01.`, `1)`, `#1`, `[1]`) and a leading timestamp;
split on the first ` - `; a third segment or a trailing bracketed token is a
label or catno hint (catno when it ends in digits and has no spaces beyond
one); `w/` and `vs.` lines keep their artist. `discogs::strip_original_mix`
applies to the title before search, as it does everywhere else, and remix
credits stay part of the title. Anything without a ` - ` and without quotes
is `Noise` unless it is the only shape in the paste, in which case the whole
paste is treated as title-only lines (some SoundCloud descriptions do this).

`scan::parse_filename` is the closest existing parser; it stays where it is
(it answers a different question: filename stems, with disc/track prefixes)
but its number-stripping helper is worth sharing.

## 4. Matching a line to a record

### 4.1 The search ladder (one to three requests per line)

`discogs::Client::find_track_releases(artist, title, label_hint) ->
Vec<ReleaseCandidate>`, a sibling of `find_artwork_candidates` that

- runs the same `resolve_hits` ladder (`artist`+`track`, then `q`+`track`),
- adds a first rung when a label hint exists: `artist`+`track`+`label`,
  which is what disambiguates a track pressed on three labels,
- keeps hits without a thumbnail (the artwork picker drops them; a record
  without a cover is still a record),
- returns `ReleaseCandidate` extended with `artist` and `catno` (split with
  `split_artist_title`, as `RecordHit` already does), so the scorer and the
  row have the pressing-disambiguating fields without another request.

### 4.2 Scoring, no requests spent

Each candidate gets a local score from what the search hit already carries:

| Signal | Weight | Note |
|---|---|---|
| artist name equal after folding (case, accents, `(2)` suffix, `The`) | strong | "Various" on the hit never counts against, it means a compilation |
| release title contains the track title, or equals it | strong | true for most 12"s and EPs, false for albums |
| label hint equals hit label | strong | |
| catno hint equals hit catno | decisive | one hit with the catno wins outright |
| format contains `Vinyl` and passes `hidden_release_mediums` | filter, then tie-break | the user's existing format preference |
| `in_collection` | tie-break | the `ReleaseAutoMatch` rule from Config decides among survivors |

Score → `Confidence::{Sure, Likely, Unsure, None}`. `Sure` needs a catno
match or artist + title + label. `Likely` is artist + title. Everything else
with any hit is `Unsure` and shows its top three candidates inline; no hit is
`None`.

### 4.3 Verification (one request, only when it pays)

For `Likely` and `Unsure` rows the job fetches the top candidate's detail
through `Catalog::release_cached_or` and checks the track title against
`ReleaseDetail::tracklist` with the existing fuzzy `file_matches`. A hit
promotes to `Sure`; a miss demotes to `Unsure` and moves to the next
candidate (at most two verifications per line). This is the step that makes
"matched" mean the track is actually on the record, and it pre-warms the
detail the sheet will show. `Sure` rows skip it.

### 4.4 Pressing choice

The user wants *a* record with the track on it, usually the vinyl one they
can buy. The search ladder's survivors are filtered by the format preference
and the `ReleaseAutoMatch` criterion picks among them, exactly as the
import auto-match does (`jobs::best_candidate`, moved into core so both
callers share it). Other pressings stay one click away on the sheet
(`master_versions` already exists there).

### 4.5 Budget

The shared pace is ~1.1 s per request. A 25-line mix costs 25 searches plus
roughly 15 verifications, about 45 s. Rows resolve top to bottom and are
usable as they land, so the wait reads as progress rather than a spinner.
Foreground clicks elsewhere still take the pace ahead of the job (it runs as
a `Priority::Background` client like the auto-match).

### 4.6 Free cross-references

Before any request, each line is matched against the local catalog with
`search.rs` (artist + title, same folding) to badge **IN LIBRARY** and to
remember the local track id. After a release resolves, shelf membership
(`VinylList::Collection` / `Wantlist` sets already in memory) badges
**OWNED** / **WANT**.

## 5. Persistence

Catalog schema v16 → v17 (bump `SCHEMA_VERSION`; existing catalogs never
migrate otherwise). Two tables:

```
tracklists       (id, name, pasted_text, created_at, matched_at)
tracklist_lines  (tracklist_id, position, raw, artist, title, timestamp_s,
                  kind, release_id NULL, confidence, chosen_by  -- auto | user | none
                  local_track_id NULL, UNIQUE(tracklist_id, position))
```

`Catalog` gains `create_tracklist`, `list_tracklists`, `tracklist_lines`,
`set_line_match`, `delete_tracklist`. A user's **Pick another** or **Not
this** writes `chosen_by = user` so a re-match never overrides it (same rule
as the import auto-match's "no recorded attempt" guard).

Every matched release is also recorded with `Catalog::record_dug_release`,
so it joins the record map through the path dug records already take.
`DugRelease` needs no new field for v1; a later `source` (which tracklist)
would let the map draw the mix as one trail.

Release details land in `release_cache` as today. Prices are never stored
(see the never-cache-prices rule); the row fetches them live like the
shelves do.

## 6. Where the code goes

| Piece | Crate / file | Notes |
|---|---|---|
| Line parser + tests | `ordnung-core/src/tracklist.rs` | pure, no network |
| Candidate scoring + confidence | `ordnung-core/src/tracklist.rs` | pure, takes `&[ReleaseCandidate]` |
| `find_track_releases`, `ReleaseCandidate { artist, catno }` | `ordnung-core/src/discogs.rs` | |
| `best_candidate` | moves from `gui/jobs.rs` into `discogs.rs` | shared with the import auto-match |
| tables + CRUD, schema v17 | `ordnung-core/src/catalog.rs` | |
| `spawn_match_tracklist` / `run_match_tracklist` | `ordnung-gui/src/jobs.rs` | streams `JobMsg::LineMatched` per row; Abort via the existing cancel flag |
| Tracklists tab, inline paste box, row table | new `ordnung-gui/src/tracklists.rs` | `VinylTab::Tracklists`; the paste box is a `TextEdit::multiline` with `desired_rows(1)` and a height clamp that follows the galley, so it sizes to its text; rows through `control_row` and `ui/tokens.rs` |
| Sheet / dig / map hooks | existing `open_release_sheet`, dig, graph | no new engines |

Core stays explicit-only: matching runs only when the user presses Match or
Re-match, nothing is wanted or collected without a click, and no file is
touched. The CLI gets nothing in v1 (GUI is the primary front-end).

## 7. Build order

- **A. Parser** (½ day). `tracklist.rs` with the table in §3 as tests, plus
  four real pastes in `testdata/tracklists/` (1001tracklists, SoundCloud,
  MixesDB, a YouTube comment). Definition of done: every sample parses with
  the expected track count and no false `Noise`.
- **B. Engine + job** (1–2 days). `find_track_releases`, scoring, verification,
  tables, the background job, and `best_candidate` shared. DoD: a 25-line
  paste resolves end to end in the log with confidences; unit tests on
  scoring; a catalog test proving user picks survive re-match.
- **C. Tracklists tab** (1–2 days). Subtle paste button, text-sized paste box with live preview, streaming
  row table with badges and actions, sheet/dig/map hooks, saved lists.
  DoD: verified in `make run`, rows share one control height, a full mix
  matched and wanted from the tab without leaving the app.
- **D. Follow-ups**, each its own release: build a local playlist from the
  **IN LIBRARY** lines in mix order (recreate a set from your own files);
  wantlist-watch integration (matched records already for sale at swept
  sellers); a `source` on `DugRelease` so the map draws the mix as a trail;
  Soulseek hand-off for lines with no vinyl (`soulseek-acquisition.md`).

Feature releases: A ships as a patch (no user-visible change), B+C together
as one MINOR bump.

## 8. Open questions and risks

- **IDs and mashups.** `ID - ID` lines stay unmatched by design and are
  counted in the preview; `w/` lines match on their own artist and title.
- **Compilations.** The search hit's artist is "Various"; the scorer must
  not penalise that, and verification against the detail tracklist (which
  carries per-track `artist`) is what confirms these. Expect more
  verifications on compilation-heavy mixes.
- **Loose Discogs matching.** `track=` matches substrings, so "Rain" pulls
  hundreds of hits. Artist folding and the title-contains rule carry the
  weight; when they fail the row is honestly `Unsure` rather than wrong.
- **Same track, many records.** Original 12", reissue, compilation, album.
  Vinyl filter, then the user's `ReleaseAutoMatch` rule, then the sheet's
  other-pressings list. The row should say which pressing it chose (label +
  catno + year) so a wrong pick is visible at a glance.
- **Rate limit.** One paste of 100 lines is about three minutes. Show the
  estimate in the preview line ("about 2 min") and keep Abort live; a second paste
  queues behind the first rather than doubling the request rate.
- **Non-Latin and transliterated names.** Fold accents; do not attempt
  transliteration in v1.
- **Clipboard from PDFs and web pages.** Soft line breaks and bullet glyphs
  (`•`, `▪`) show up; the parser strips leading bullets and joins a line
  that starts with a lowercase continuation onto the previous one.
