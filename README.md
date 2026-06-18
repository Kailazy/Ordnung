# Ordnung

A fast, self-sufficient DJ music catalog, analyzer, and rekordbox/CDJ USB
exporter — written in Rust.

Ordnung scans your music, builds a SQLite catalog, analyzes tempo, beatgrids,
musical key (Camelot), waveforms and loudness, optionally converts and enriches
tracks, and (in progress) exports a native rekordbox USB the CDJs can read — all
without rekordbox itself. It is **not** a rekordbox plugin: it generates every
piece of metadata and analysis on its own.

The primary front-end is a native desktop app (`Ordnung`, built on egui). A CLI
(`ordnung`) exposes the same engine for scripting.

## Product rules

These are enforced design constraints, not preferences:

- **Explicit-only.** `scan`, `analyze`, `tag`, `convert`, `export` are separate
  actions; none silently does another's work.
- **Source files are sacred.** Tag writes and conversions are opt-in.
  Conversions create new files unless you pass `--in-place`. Deletes go to the
  OS Trash, never a hard `rm`.
- **The catalog is the truth.** The SQLite catalog plus analysis cache is
  authoritative; a USB export is a derived artifact.
- **Camelot is the contract.** Keys are stored canonically (pitch class + mode)
  and rendered as Camelot by default.

## Workspace layout

A Cargo workspace with four crates:

| Crate          | Kind   | Responsibility |
|----------------|--------|----------------|
| `ordnung-core` | lib    | Domain model, catalog (SQLite), scan, tag (lofty), analysis (BPM / key / beatgrid / waveform / loudness), conversion (ffmpeg), Discogs enrichment |
| `ordnung-rbdb` | lib    | rekordbox/CDJ export — `export.pdb` (DeviceSQL) + ANLZ writers (in progress) |
| `ordnung-cli`  | bin    | `ordnung` — command-line front-end (the only print/policy layer) |
| `ordnung-gui`  | bin    | `Ordnung` — native desktop app wrapping the core engine |

See [PLAN.md](PLAN.md) and [HANDOFF.md](HANDOFF.md) for the full architecture and
phased roadmap.

## Requirements

- **Rust** (stable; pinned via [rust-toolchain.toml](rust-toolchain.toml) — `rustup`
  installs it automatically).
- **ffmpeg** — only needed for audio conversion. Decoding/analysis use pure-Rust
  crates; SQLite is bundled. `brew install ffmpeg` / `apt install ffmpeg`.
- **macOS app build only:** `librsvg` (provides `rsvg-convert`) for icon
  rendering, plus Xcode command-line tools. `brew install librsvg`.
- **Linux:** `libdbus` for media-key integration (`apt install libdbus-1-dev`).

## Build & run

```bash
# Run the GUI from source (debug)
make run                 # == cargo run -p ordnung-gui

# Build release binaries
cargo build --release -p ordnung-gui    # -> target/release/Ordnung
cargo build --release -p ordnung-cli    # -> target/release/ordnung
```

### macOS app bundle

```bash
make app        # build → sign → install to /Applications → pin to Dock → relaunch
make app-only   # build + ad-hoc sign the local Ordnung.app, don't touch /Applications
```

`make app` rasterizes the icon, assembles `Ordnung.app`, and ad-hoc codesigns it
with a stable identity so file-access/media-key permissions and the custom icon
persist across rebuilds. See [tools/build-app.sh](tools/build-app.sh).

## CLI usage

```bash
ordnung scan <dir>      # index files into the catalog
ordnung ls              # list catalog entries
ordnung analyze         # BPM, beatgrid, key, waveform, loudness
ordnung key             # show detected keys (Camelot)
ordnung tag             # read/write tags (opt-in writes)
ordnung missing         # tracks with missing files
ordnung dupes           # find duplicates
ordnung repoint         # fix moved-file paths
```

## Configuration

Ordnung reads an optional `.env` from the repo root on startup (for dev launches
and the CLI). The only variable today is a Discogs token used for metadata
enrichment:

```bash
cp .env.example .env
# then set DISCOGS_TOKEN — https://www.discogs.com/settings/developers
```

A token saved in the GUI Settings window (`~/.ordnung/config.toml`) takes
priority over the env var.

## License

MIT.
