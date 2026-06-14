# Discogs Track Inspector — Design Brief

**Status:** Draft for design exploration
**Target surface:** Ordnung GUI (desktop app, `ordnung-gui` crate)
**Trigger:** Double-click on a track row in the library view
**Audience for this brief:** UI/UX designer

---

## 1. Context

Ordnung is a DJ catalog and CDJ/rekordbox USB exporter. Users import their local
music library, Ordnung analyzes each file (BPM, key, beatgrid, waveform), and they
prepare playlists for performance. Files on disk are the source of truth; Ordnung
never mutates them silently.

Most DJ libraries are messy: inconsistent artist spellings, missing years, generic
"Electronic" genres, no label or catalog number. This makes filtering, smart
playlists, and crate-digging context all weaker than they should be.

Discogs is the most comprehensive crowd-sourced music release database. Its
**Style** taxonomy (e.g. "Deep House", "Detroit Techno", "UK Garage") is far more
useful to DJs than ID3 genres, and it carries release-level context — label,
catalog number, year, country, format, other versions — that helps users
understand what they actually own.

This feature surfaces that context **on demand**, per track, without modifying
the user's files or their catalog metadata unless they explicitly ask.

---

## 2. User & jobs-to-be-done

**Primary user:** Working DJ with 500–20,000 tracks. Comfortable with desktop apps,
not necessarily technical. Cares about provenance, accuracy, and discovery.

**Jobs the inspector serves:**

1. *"Tell me what this record actually is."* — Confirm artist, title, year, label,
   format, especially for files with bad or missing tags.
2. *"Show me the release context."* — What album/EP is this from? What other tracks
   are on it? Who's the label?
3. *"Help me find similar or related material."* — Other releases by this artist,
   other remixes of this track, other releases on this label.
4. *"Fix my metadata."* — Pull clean data from Discogs into Ordnung's catalog
   (and optionally write back to the file's tags).
5. *"Take me to the source."* — Open the release on Discogs in a browser for
   deeper digging, marketplace browsing, or wantlist add.

The inspector is **read-first**. Editing is secondary and always explicit.

---

## 3. Invocation model

- **Primary trigger:** double-click a track row.
- **Secondary triggers** (designer to validate placement): right-click → "Show Discogs info"; keyboard shortcut from a focused row; a Discogs glyph in the row when a confirmed match exists.
- **Dismissal:** Esc, click-out, or close affordance. State (selected candidate, scroll position) should persist for the session.
- **Surface treatment:** designer's call between *side panel / drawer*, *modal sheet*, or *inline expansion*. Recommend evaluating against: (a) does the user still need to see the library list while inspecting? (b) will they want to inspect several tracks in sequence? Side panel typically wins both.

---

## 4. Information architecture

The panel needs to gracefully express several distinct match states. Treat these
as **first-class layouts**, not error states bolted onto a happy path.

### 4.1 Match states

| State                 | What's shown                                                          | Primary CTA              |
| --------------------- | --------------------------------------------------------------------- | ------------------------ |
| **Confirmed match**   | Full release detail (see §4.2)                                        | "Open on Discogs"        |
| **Auto-suggested**    | Best guess + "Is this right?" affirm/reject                           | "Confirm" / "Show others"|
| **Multiple candidates** | Ranked list of N releases with disambiguating fields                | Pick one                 |
| **No match found**    | Empty state with manual search box pre-filled with artist + title     | "Search Discogs"         |
| **Not yet enriched**  | CTA to fetch (some users disable network enrichment by default)       | "Fetch from Discogs"     |
| **Offline / error**   | Clear cause + retry; if cached data exists, show it with a stale badge| "Retry"                  |

### 4.2 Confirmed-match content (priority order)

1. **Release artwork** — large, prominent. Falls back to a tasteful placeholder.
2. **Title + primary artist** — the canonical Discogs version, with a subtle
   indicator if it differs from the local file's tags (designer: this is a key
   moment — the user is learning their file is wrong).
3. **Release identity strip** — label, catalog number, year, country, format
   (e.g. "Vinyl, 12\", 33⅓ RPM"). Dense but scannable.
4. **Genre / Style chips** — Discogs styles are the gem here. Multiple chips,
   tappable as a future filter hook (mark as `future` in handoff).
5. **This track on the release** — track position (e.g. "A2"), track duration as
   listed on Discogs, vs. analyzed file duration. Small but useful for verification.
6. **Tracklist** — collapsed by default; expandable. Highlights the row matching
   the local file.
7. **Other versions of this track** — remixes, edits, compilation appearances.
   Each row is itself a Discogs release link.
8. **Marketplace snippet** *(optional, designer to evaluate)* — lowest price, copies
   for sale, median price. DJs and collectors care; minimalists may want to hide it.
   Recommend a user preference, off by default.
9. **Actions footer** — see §4.3.

### 4.3 Actions

Group, in order of expected frequency:

- **Open on Discogs** (external link, release page)
- **Open Master Release** (when distinct from this pressing)
- **Open Artist page**
- **Apply to catalog** — pulls selected fields into Ordnung's catalog. Designer
  should show a *field-level preview* of what will change (old → new), with
  checkboxes per field. Never an all-or-nothing button.
- **Write tags to file** — opt-in, secondary visual weight, with a confirmation
  step that names the file. This is the only action that touches the user's
  source files; treat it accordingly.
- **Choose a different match** — returns to the candidate picker.
- **Refresh from Discogs** — re-fetches; useful for stale cached entries.
- **Unlink** — break the match, return to the unenriched state.

---

## 5. Candidate picker (multi-match state)

When confidence is low or ambiguous, surface a ranked list. Each candidate row
needs *just enough* to disambiguate at a glance:

- Thumbnail artwork
- Title + artist (highlight where they differ from the local file)
- Year, label, catalog #, country, format
- Track count, release type (Single / EP / Album / Compilation)
- Confidence signal (designer's call: numeric %, badge, or sort order alone)

Selecting a candidate should preview it in the main panel without committing.
A confirm step persists the link to the catalog.

---

## 6. Visual & content principles

- **Density that respects context.** This is a power-user tool inspected often.
  Avoid generous-spacing dashboard aesthetics; aim for "Things 3 inspector" or
  "Linear issue side panel" calm-but-dense.
- **Typographic hierarchy carries the IA.** Designer should be able to read the
  panel top-to-bottom and immediately understand: *what is this, where does it
  come from, what can I do with it.*
- **Diffs are content.** When Discogs disagrees with the local file (different
  artist spelling, different year), that disagreement *is* the value. Surface it
  visually, don't hide it.
- **Discogs as a guest in our product.** Attribute clearly ("Data from Discogs"),
  but the panel is Ordnung's UI, not a Discogs embed.
- **No empty politeness.** "No match found" should immediately offer the manual
  search box, not an apology.

---

## 7. States & edge cases the design must address

- Very long titles, very long artist lists ("Various Artists" compilations,
  collaborations with 4+ artists).
- Non-Latin scripts in artist/title/label.
- Missing artwork.
- Track present on dozens of releases (compilations, repressings) — picker must
  handle 50+ candidates gracefully.
- File analyzed at, e.g., 7:23 but Discogs lists the track as 5:48 — surface the
  mismatch, don't hide it. Could indicate wrong match, an extended mix, or DJ edit.
- Rate-limited (Discogs allows 60 requests/minute authenticated). Need a calm
  "Slow down, more in 12s" state — not a scary error.
- User has not configured a Discogs token. First-run education within the panel,
  not blocking everything else.
- Stale cache: panel works offline from cache, with a visible "Last fetched 3
  weeks ago — refresh" affordance.

---

## 8. Out of scope (for this iteration)

- Editing data *on Discogs* (submissions, corrections) — link out only.
- Discogs wantlist / collection sync — note as a future hook.
- Bulk enrichment UI (a separate batch flow lives outside the inspector).
- MusicBrainz, Beatport, Bandcamp parity — the inspector's data shape should
  *allow* future sources, but this brief covers Discogs only.

---

## 9. Inputs the designer can rely on

For every track Ordnung will provide:

- Local catalog fields: artist, title, album, year, genre, duration, BPM,
  Camelot key, file format, bitrate, path.
- Match status: `unenriched | suggested | confirmed | unmatched | error`.
- Discogs payload (when matched): release ID, master ID, artist(s), title, year,
  label(s), catalog #, country, format, genres, styles, full tracklist with
  positions and durations, artwork URLs (multiple sizes), marketplace summary,
  list of other versions.
- Confidence score 0.0–1.0 and the signals behind it (string similarity,
  duration delta, catalog # exact match, etc.) — surface as much or as little
  as you think helpful.

---

## 10. Deliverables we'd love back

1. Wireframes covering all match states in §4.1.
2. High-fidelity mocks for the confirmed-match state and the multi-candidate picker.
3. Interaction notes for: opening, dismissing, switching tracks while open, the
   "apply to catalog" field-level confirmation, and the "write tags to file"
   confirmation.
4. Empty / loading / error / rate-limit / offline-with-cache state designs.
5. A short rationale doc — 1 page — explaining the chosen surface (panel vs.
   modal vs. inline) and the IA decisions, so we can review the *thinking* and
   not just the pixels.

---

## 11. Open questions for the designer to weigh in on

- Side panel vs. modal vs. inline — your recommendation, with reasoning.
- Should the inspector be *resizable* / *pinnable*, given users will likely
  inspect many tracks in a session?
- How much marketplace data is "useful context" vs. "off-brand for a DJ tool"?
- For the multi-candidate picker, is a list view or a card grid more scannable
  given the disambiguating fields involved?
- How do we represent confidence honestly without making the user feel like
  they're babysitting a bad algorithm?
