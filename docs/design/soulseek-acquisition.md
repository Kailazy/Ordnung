# Soulseek Acquisition ("nicotine+ integration") — Feature Proposal

**Status:** Proposal for review
**Target surface:** Ordnung GUI + new `ordnung-slsk` crate
**Trigger:** A new explicit `fetch` stage (search + download), plus a "Copy for
Soulseek" quick action shipped ahead of it
**Audience for this brief:** Maintainer (architecture + scope decision)

---

## 1. Summary

DJs already use [Soulseek](https://www.slsknet.org/) — a peer-to-peer network — to
source tracks that aren't on streaming or stores. Today that means leaving Ordnung,
hand-typing each search into a separate client, downloading into a folder, then
coming back to scan it. This proposal closes that loop: **acquire music from
Soulseek inside Ordnung**, landing files in a staging folder that the existing
`scan` stage ingests on its own terms.

The headline is "integrate nicotine+", but the honest framing is different (see
§3): we do **not** embed nicotine+. We implement the Soulseek wire protocol
natively in Rust and use nicotine+/`slskd`/`soulseek.NET` only as protocol
references.

---

## 2. User & jobs-to-be-done

**Primary user:** Working DJ already on Soulseek, sourcing promos, edits, bootlegs,
and out-of-print records not available elsewhere.

**Jobs:**

1. *"Find this exact track on the network."* — Search by artist + title without
   leaving the app.
2. *"Get it into my library."* — Download the best candidate into a known staging
   folder, then run the normal scan → analyze → tag → export pipeline on it.
3. *"Fill a gap fast."* — While prepping a set, spot a missing track and grab it in
   a couple of clicks.

**Non-jobs (explicitly out of scope for v1):** running a sharing server to maintain
ratio, browsing arbitrary peers, chat/rooms, automated bulk wishlist crawling.

---

## 3. Why not literally embed nicotine+

Nicotine+ is a **Python/GTK desktop application**. Ordnung ships as a single Rust
binary with an egui front-end. Reusing nicotine+ itself would mean one of:

- **Subprocess + headless mode** (`nicotine --headless`) and scraping/IPC its
  state — fragile, and it drags a Python + GTK runtime into our distribution.
- **Reuse the protocol, not the code.** The Soulseek protocol is a documented TCP
  protocol with several mature reference implementations: nicotine+ (Python),
  `slskd` (C#), `soulseek.NET`, `slsk-batchdl`. That knowledge is the valuable part.

**Decision: implement the protocol natively in a new `ordnung-slsk` crate.** No
Python dependency, full control over the async/network layer, clean integration
into the egui front-end.

---

## 4. Architecture fit

This is a **new pipeline stage that sits *before* `scan`** — acquisition. It maps
onto Ordnung's hard product rules without bending them:

| Rule | How acquisition respects it |
|---|---|
| **Explicit-only** | New, separately-invoked `fetch` command (search + download). It does **not** auto-scan, auto-analyze, or auto-tag. Downloads land in a staging dir and stop. |
| **Source files are sacred** | Downloads are *new* files in a staging folder. Nothing touches the master library — consistent with the never-touch-master-library rule. |
| **Catalog is the source of truth** | Acquisition produces files on disk; they enter the catalog only when the user runs `scan`. No back-door catalog writes. |
| **Dependency direction** | `cli`/`gui` → `slsk` → `core`. `core` never depends upward and gains no network I/O. |

### Crate boundary

New sibling crate **`ordnung-slsk`**, same tier as `ordnung-rbdb`:

- Owns the TCP connection to the Soulseek server + peer connections (async, `tokio`).
- Depends on `ordnung-core`'s model only; never the reverse.
- Public surface, roughly:
  - `async fn search(query: &str) -> Vec<SearchResult>`
  - `async fn download(result: &SearchResult, staging_dir: &Path) -> DownloadHandle`
  - Progress emitted as events the GUI/CLI subscribe to.
- Keeps `ordnung-core` free of policy, UI, and network I/O — the same discipline
  that keeps ffmpeg out of core. Soulseek is the second IO/subprocess-ish concern
  after ffmpeg, which is exactly why it gets its own crate.

### Command + GUI

- **CLI:** a new `fetch` command in `ordnung-cli` — the only place that maps user
  intent to `slsk` calls, prints, and prompts.
- **GUI:** a search/download panel in `ordnung-gui` that calls `ordnung-slsk`
  exactly the way it already calls core — no protocol logic in the GUI layer.

---

## 5. Phasing

This is a **new roadmap phase**, not a tweak to an existing one. Sequenced to
de-risk the protocol before investing in UI.

### Phase 0 — "Copy for Soulseek" (shipped)

A right-click action on any track (or multi-selection) that copies
`Artist - Title`, one line per track, to the clipboard for pasting into any
Soulseek client. Zero network code, immediately useful, and it validates the query
formatting we'll reuse in `fetch`.

- Implemented in `ordnung-gui`: `TrackMenuAction::CopyForSoulseek` +
  `soulseek_query(artist, title)` helper.

### Phase 1 — Protocol proof-of-concept

Scaffold `ordnung-slsk`: server login, distributed search, and a single-file peer
download with a progress callback. **Success criterion: login + search returns real
results.** If that works, the rest is mechanical. `core`/`cli`/`gui` stay untouched
until this connects.

### Phase 2 — `fetch` command (CLI)

Wire `ordnung-slsk` into a `fetch` command: search, pick a result, download into the
staging dir. Leech-only. Surfaces results, file size, bitrate, peer queue position.

### Phase 3 — GUI panel

A search box + results list + download queue in `ordnung-gui`. "Download" drops the
file into the staging folder; a visible affordance to then run `scan` on it. Still
no automatic ingestion (explicit-only).

### Phase 4 (optional, separate decision) — sharing

Uploading/sharing to maintain ratio, peer browsing, wishlists. Materially larger
scope and a distinct product/operational decision; not committed here.

---

## 6. Risks & flags

1. **Effort is real but bounded.** A first cut (login + search + single download
   with progress) is a few focused days. Full client parity is much more — hence the
   leech-only v1.
2. **Soulseek favors sharers.** Many peers gate downloads on ratio; a pure leecher
   works but gets throttled/queued. Worth deciding early whether we ever upload
   (Phase 4).
3. **Legal / dual-use.** Soulseek is legitimate software and downloading material
   you have rights to is lawful — but the network is largely copyrighted music.
   That is the operator's responsibility; Ordnung provides the tool and does not
   police usage. Naming this plainly so the decision is conscious.
4. **Async/network in a pure-DSP codebase.** Justifies the dedicated crate and a
   `tokio` runtime confined to `ordnung-slsk`, kept out of `core`.

---

## 7. Open questions

- Staging folder location: fixed (`~/.ordnung/incoming`) vs. user-configured per
  fetch?
- Result ranking: by bitrate, by peer speed/queue, by free-slot availability?
- Do we de-dupe against the existing catalog (skip downloading what's already
  owned by content hash)?
- Should `fetch` offer a one-click "scan staging now" convenience that *still*
  routes through the explicit `scan` command, or stay fully manual?
