---
name: ordnung-architecture
description: Canonical architecture, module boundaries, data model, and coding conventions for the Ordnung DJ catalog/exporter. Read BEFORE adding or changing any feature in this repo, before deciding where code belongs, or when reviewing whether a change fits the design. Enforces the explicit-only product rule and crate boundaries.
---

# Ordnung architecture (consistency anchor)

Ordnung is a Rust workspace: a self-sufficient DJ catalog that analyzes music and
exports a native rekordbox/CDJ USB. See `PLAN.md` for the full plan. This skill is
the rulebook that keeps changes consistent with it.

## Hard product rules (never violate)

1. **Explicit-only.** Each pipeline stage is a separate, user-invoked command:
   `scan`, `analyze`, `tag`, `playlist`, `convert`, `export`. No command performs
   another's work as a side effect. The library NEVER converts files, writes tags to
   source files, or writes a USB unless that exact operation was requested.
2. **Source files are sacred.** Conversions write NEW files; `--in-place` is the only
   way to overwrite, and tag writeback to source files is opt-in per command.
3. **Catalog is the source of truth**, the USB export is a derived artifact, and
   Ordnung does NOT read from or depend on rekordbox.
4. **Camelot is the display contract.** Store keys canonically `(PitchClass, Mode)`;
   render Camelot by default. Notation is presentation, never storage.

If a requested change would break one of these, stop and flag it rather than coding it.

## Crate boundaries

- `ordnung-core` — domain model + engines. **No policy, no UI, no `println!`, no
  process exits.** Pure library returning typed results/errors. Submodules:
  `model`, `catalog`, `scan`, `analysis`, `tag`, `convert`.
- `ordnung-rbdb` — rekordbox export only: `pdb` (DeviceSQL via `rekordcrate`) and
  `anlz` (.DAT/.EXT). Depends on `ordnung-core` model, not the reverse. Format
  details belong in the `rekordbox-format` skill.
- `ordnung-cli` — the ONLY place that decides policy, prints, prompts, and maps
  user intent to core calls. Owns `clap` definitions and progress UI.
- Future `ordnung-gui` wraps `ordnung-core` exactly like the CLI does — so keep all
  reusable logic in core, never in the CLI.

Dependency direction: `cli`/`gui` → `rbdb` → `core`. Never let `core` depend upward.

## Data model (authoritative shapes)

`Track`, `Analysis`, `Key`, `Cue`, `Beatgrid`, `Playlist`, `Catalog`, `ExportProfile`
— defined in `ordnung-core/model`. Extend these in place; do not invent parallel
structs in `cli` or `rbdb`. `Analysis` carries a `content_hash` and `analyzer_version`
so the cache can invalidate correctly.

## Conventions

- Errors: `thiserror` enums in `core`/`rbdb`; `anyhow` only at the `cli` boundary.
- Concurrency: `rayon` for batch analysis; keep engines `Send + Sync` and stateless
  where possible (state lives in the catalog).
- DSP and format parsing are pure Rust (`rustfft`, `rekordcrate`). `ffmpeg` is the
  only subprocess, invoked solely by the `convert`/`export` paths.
- Every new engine function takes inputs + config and returns data; it does not read
  CLI flags, prompt, or print.
- Tests live next to code; the Camelot/key mapping and any format writer get unit
  tests with known-good fixtures.

## Where does my change go? (quick guide)

- New metadata field → `model` + `catalog` schema + `tag` read/write.
- New analysis output → `analysis` + `Analysis` struct + cache + (if exported) `rbdb/anlz`.
- New user command → `cli` only, delegating to existing core engines.
- New target format → `convert` presets + `rekordbox-format` compatibility matrix.

When in doubt, consult `ordnung-roadmap` for sequencing and `PLAN.md` for intent.
