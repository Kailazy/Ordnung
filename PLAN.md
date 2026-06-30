# Ordnung — Plan

A fast, self-sufficient DJ music catalog and rekordbox/CDJ USB exporter.
Replaces rekordbox for cataloging, analysis, and USB export — with full control
over conversions and tagging.

> **Ordnung** (German: *order, tidiness*) — the tool that keeps your crate in order.

---

## 1. Goals (from requirements)

- **Native rekordbox/CDJ export.** Write a real `export.pdb` + ANLZ analysis files
  to a FAT32 USB so CDJs read the library natively: tracks, playlists, hot/memory
  cues, beatgrids, and waveforms.
- **Self-sufficient.** Ordnung is the source of truth. It does *not* depend on
  rekordbox; it generates all metadata, beatgrids, and analysis itself.
- **Auto analysis.** Detect BPM and musical key (DSP), display key in **Camelot**.
- **Explicit conversions.** Convert between CDJ-compatible formats (MP3, AAC, WAV,
  AIFF, FLAC) only when the user asks. **Nothing destructive or transformative
  happens automatically.** This is a hard product rule.
- **Flat master pool + playlists.** One master folder holds everything; organization
  is expressed through playlists, not folder nesting.
- **Performant.** < 2k tracks, local drive. Parallel analysis, persistent cache so
  nothing is ever re-analyzed without reason.
- **CLI core now, GUI later.** The engine is a library; the CLI is one front end.
  A GUI can wrap the same core later.

## 2. Non-negotiable principles

1. **No surprises.** Scan, analyze, convert, organize, and export are distinct,
   explicit commands. The library never mutates source files or writes a USB as a
   side effect of another action.
2. **Source files are sacred by default.** Tag writes and conversions are opt-in and,
   for conversions, write to new files unless `--in-place` is explicitly passed.
3. **Camelot is the display contract.** Keys are stored canonically (pitch class +
   mode) and rendered as Camelot by default; other notations are pure presentation.
4. **The catalog is the truth.** A local SQLite catalog + analysis cache is
   authoritative; the USB export is a derived artifact.

## 3. Architecture

Rust workspace. Pure-Rust DSP and parsing where practical (portability, no fragile
FFI); shell out to `ffmpeg` only for conversion.

```
ordnung/
├── Cargo.toml                  # workspace
├── crates/
│   ├── ordnung-core/           # domain model + engines (no UI, no policy)
│   │   ├── model/              # Track, Analysis, Key, Playlist, ExportProfile (dir)
│   │   ├── analysis/           # key, waveform, loudness (rustfft) — BPM/beatgrid off (dir)
│   │   ├── catalog.rs          # SQLite persistence + Catalog type (rusqlite)
│   │   ├── scan.rs             # discovery + tag/property read (lofty, symphonia)
│   │   ├── tag.rs              # metadata read/write (lofty)
│   │   ├── convert.rs          # ffmpeg orchestration (explicit only)
│   │   ├── discogs.rs          # Discogs metadata/artwork client
│   │   └── error.rs            # crate error type
│   ├── ordnung-rbdb/           # rekordbox export: export.pdb + ANLZ writers (Phase 5, skeleton)
│   │   ├── pdb.rs              # DeviceSQL tables (export.pdb)
│   │   └── anlz.rs             # .DAT/.EXT (beatgrid, cues, waveforms)
│   ├── ordnung-cli/            # clap command surface; a "policy" layer → core + rbdb
│   └── ordnung-gui/            # egui/eframe desktop app (primary front-end) → core
```

### Dependencies (chosen, not yet pinned)

| Concern        | Crate / tool        | Why |
|----------------|---------------------|-----|
| Audio decode   | `symphonia`         | Pure-Rust decode of MP3/AAC/WAV/AIFF/FLAC/OGG → samples for analysis |
| Tag read/write | `lofty`             | Unified ID3/MP4/Vorbis/RIFF/AIFF tagging |
| DSP / FFT      | `rustfft`           | Chromagram (key) + spectral-flux onset (BPM) |
| Catalog DB     | `rusqlite`          | Embedded, fast, zero-server catalog + cache |
| rekordbox DB   | `rekordcrate`       | Read/write DeviceSQL `export.pdb` via `binrw` |
| Conversion     | `ffmpeg` (subprocess)| Universal, only invoked on explicit `convert`/`export` |
| Concurrency    | `rayon`             | Parallel analysis across cores |
| CLI            | `clap`              | Command surface |
| Progress       | `indicatif`         | Long-running scan/analyze/export feedback |
| Errors         | `thiserror`/`anyhow`| Typed errors in core, ergonomic at CLI edge |

### Pipeline (each stage = one explicit command)

1. **scan** — discover audio files, read tags + audio properties, upsert into catalog.
2. **analyze** — compute BPM, beatgrid, Camelot key, waveform, loudness; cache by
   content hash; parallel; only (re)analyzes when missing or `--force`.
3. **tag** — read/edit/write metadata; opt-in writeback to files.
4. **playlist** — create/edit playlists and playlist folders (the organization layer).
5. **convert** — explicit transcode between CDJ-compatible formats via ffmpeg.
6. **export** — assemble FAT32 USB: `/CONTENTS`, `/PIONEER/rekordbox/export.pdb`,
   per-track ANLZ `.DAT`/`.EXT` carrying beatgrid, cues, and waveforms.

## 4. Data model (core types)

- `Track` — id, source path, format, audio properties (sample rate, bit depth,
  channels, duration), tag snapshot, link to `Analysis`.
- `Analysis` — bpm, `Beatgrid`, `Key`, waveform preview/detail, peak/loudness,
  `Vec<Cue>` (hot + memory), content hash, analyzer version.
- `Key` — canonical `(PitchClass, Mode)`; renders Camelot / Open Key / classical.
- `Cue` — kind (hot/memory/loop), position (ms + sample), optional color/label.
- `Beatgrid` — anchored beat positions / tempo segments.
- `Playlist` — ordered track refs; nestable under playlist folders.
- `Catalog` — the SQLite-backed master pool; single source of truth.
- `ExportProfile` — target device, optional conversion rules, selected playlists.

## 5. Roadmap (phased; see the `ordnung-roadmap` skill for live status)

- **Phase 0 — Scaffold.** Workspace, domain model, catalog schema, CLI skeleton,
  Camelot key module (done first; small, central, fully tested).
- **Phase 1 — Catalog & tags.** `scan`, `ls`, `tag`. Read properties + tags; persist.
- **Phase 2 — Analysis.** `analyze`: BPM + Camelot key + beatgrid + waveform, cached.
- **Phase 3 — Playlists.** `playlist` create/edit/nest; the organization layer.
- **Phase 4 — Conversion.** `convert`: explicit, CDJ-safe ffmpeg presets.
- **Phase 5 — rekordbox export.** `export`: `export.pdb` + ANLZ to FAT32 USB.
- **Phase 6 — Validation.** Round-trip against rekordbox/real CDJ; hot-cue editing.
- **GUI (built early, now the primary front-end).** `ordnung-gui` is a native
  egui/eframe app wrapping `ordnung-core`: catalog table, inspector + tag editing,
  conversion, Discogs artwork/metadata, inline + full-track waveforms, drag-to-
  rekordbox. See the `ordnung-roadmap` skill for live GUI status.

## 6. Risks & mitigations

- **export.pdb/ANLZ correctness** is the hardest part. Mitigate by leaning on
  rekordcrate's `binrw` write path, validating bytes against rekordbox-produced
  exports, and round-tripping (parse our output, diff against reference). Keep all
  format knowledge in the `rekordbox-format` skill.
- **Key/BPM accuracy** vs rekordbox/Mixed In Key. Mitigate with a versioned analyzer,
  a labeled test set, and Camelot as a stable contract. Algorithms live in the
  `audio-analysis` skill.
- **CDJ firmware variance** (which formats/DB versions a model accepts). Track a
  compatibility matrix; default to the broadest-supported choices.

## 7. Consistency: Claude skills

Four project skills keep work aligned with this plan:

- **ordnung-architecture** — module boundaries, data model, conventions, the
  explicit-only rule. Read before adding any feature.
- **rekordbox-format** — the export.pdb/ANLZ/USB-layout reference and invariants.
- **audio-analysis** — BPM/key/beatgrid algorithms, the Camelot mapping, cache contract.
- **ordnung-roadmap** — the phased plan, current status, and per-phase definition of done.
