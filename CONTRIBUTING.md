# Contributing to Ordnung

This is the developer side of the project: building from source, the shape
of the codebase, and how releases are cut. If you just want to use the app,
the [README](README.md) has the download.

## Requirements

- **Rust** stable, pinned by [rust-toolchain.toml](rust-toolchain.toml);
  `rustup` picks it up automatically.
- **Xcode command-line tools** (`xcode-select --install`).
- **librsvg** for icon rendering when building the `.app` bundle:
  `brew install librsvg`.
- **ffmpeg** only if you work on conversion or USB export for older players:
  `brew install ffmpeg`. Decoding, analysis and SQLite are pure Rust or bundled.
- **Linux:** `libdbus-1-dev` for media-key integration. Linux builds compile
  but the app is only packaged and tested on macOS.

## Build and run

```bash
make run                                # run the GUI from source (debug)
cargo build --release -p ordnung-gui    # release binary -> target/release/Ordnung
cargo test --workspace                  # run the tests
```

`make` with no target lists every convenience target.

### macOS app bundle

```bash
make app        # build, sign, install to /Applications, pin to Dock, relaunch
make app-only   # build + sign the local Ordnung.app, leave /Applications alone
```

The USB export is a beta: it's a default cargo feature (`usb-export`), so
`make run` and `make app` include it for testing on real sticks, while the
release build passes `--dist`, which compiles it out. To see exactly what
users get, run `cargo run -p ordnung-gui --no-default-features`.

`make app` rasterizes [tools/icon.svg](tools/icon.svg), assembles
`Ordnung.app`, and codesigns it. Run `bash tools/make-signing-cert.sh` once to
create a local signing certificate; without it the bundle is ad-hoc signed and
macOS forgets its file-access and media-key permissions on every rebuild.

[tools/build-app.sh](tools/build-app.sh) also takes `--universal` (fat
arm64 + x86_64 binary), `--dmg` (package a drag-to-Applications
`Ordnung.dmg`) and `--version=X.Y.Z`. The release CI runs it as
`--no-install --no-launch --universal --dmg --dist --version=<tag>`.

### Configuration for dev launches

Ordnung reads an optional `.env` from the repo root on startup. The only
variable is the Discogs token used by the vinyl features:

```bash
cp .env.example .env
# set DISCOGS_TOKEN — https://www.discogs.com/settings/developers
```

A token saved in the app's Settings window (`~/.ordnung/config.toml`) takes
priority over the env var. Keep a scratch `HOME` for testing anything that
scans or analyses, so you never point a dev build at your real library.

## Workspace layout

A Cargo workspace with three crates:

| Crate          | Kind | Responsibility |
|----------------|------|----------------|
| `ordnung-core` | lib  | Domain model, catalog (SQLite), scan, tag (lofty), analysis (BPM / key / beatgrid / waveform / loudness), conversion (ffmpeg), Discogs enrichment, update check |
| `ordnung-rbdb` | lib  | rekordbox/CDJ export: `export.pdb` (DeviceSQL) + ANLZ writers |
| `ordnung-gui`  | bin  | `Ordnung`, the desktop app (egui); the only policy/UI layer over the engine |

Engines live in core, policy lives in the GUI. Core has no UI and no
`println!`; the GUI decides when to run things and how to show them.

See [PLAN.md](PLAN.md) for the architecture and phased roadmap,
[HANDOFF.md](HANDOFF.md) for the current state of each phase, and
[docs/rekordbox-export-structure.md](docs/rekordbox-export-structure.md) for
the USB export format.

## Product rules

These are enforced design constraints, not preferences:

- **Explicit-only.** Scan, analyze, tag, convert and export are separate
  actions; none silently does another's work.
- **Source files are sacred.** Tag writes and conversions are opt-in.
  Conversions create new files unless asked to work in place. Deletes go to
  the OS Trash, never a hard `rm`.
- **The catalog is the truth.** The SQLite catalog plus analysis cache is
  authoritative; a USB export is a derived artifact.
- **Camelot is the contract.** Keys are stored canonically (pitch class +
  mode) and rendered as Camelot by default.

## Conventions

- Reuse before you build: one song model, one row layout, one catalog query
  per shape. Grep for the behaviour first and say in the commit what was
  reused.
- GUI controls come from the `ui/` component library (`ui::button`,
  `ui::chip`, `ui::window`, `ui::menu`), never raw `egui::Window` or
  `small_button`. Controls on one row share one height via `ui::control_row`.
- Bump the catalog `SCHEMA_VERSION` with every schema change, or existing
  catalogs never migrate.
- The tree is not `rustfmt`-clean; don't run a repo-wide `cargo fmt`.
- Verify GUI changes in the running app (`make run`), not just `cargo check`.

## Releases

The version lives in `[workspace.package] version` in the root
[Cargo.toml](Cargo.toml). Every verified change ships as a release:

1. Bump the version. Features bump MINOR, fixes and polish bump PATCH, MAJOR
   only when explicitly decided.
2. `cargo check -p ordnung-gui` to refresh `Cargo.lock`.
3. Commit `Cargo.toml` and `Cargo.lock` as `release: bump workspace version
   to X.Y.Z` and push.
4. `git tag vX.Y.Z && git push origin vX.Y.Z`.

Pushing the tag runs [.github/workflows/release.yml](.github/workflows/release.yml),
which builds the universal DMG and publishes a GitHub Release. Never move a
pushed tag; a botched release gets a new patch version.

The genre database the app downloads is a separate rolling release asset,
refreshed with `make genredb-publish` (see the Makefile for why it must run
from a residential connection).
