# Ordnung — Handoff

A fast, self-sufficient DJ music catalog and rekordbox/CDJ USB exporter, written in
Rust. Replaces rekordbox for cataloging, analysis, tagging, and (eventually) USB
export — with full user control over conversions and tagging.

> "Ordnung" = German for *order / tidiness*. CLI core now, GUI later.

This file is the single catch-up document for the project. For deeper detail see
[PLAN.md](PLAN.md), the skills in [.claude/skills/](.claude/skills/), and
[KEY_CHECK.md](KEY_CHECK.md).

---

## 1. What the user wants (requirements, from Q&A)

The user is a DJ cataloging their library. Decisions captured up front:

- **CDJ compatibility:** full **native rekordbox export** (export.pdb + ANLZ) so CDJs
  read the library natively — playlists, hot/memory cues, beatgrids, waveforms.
- **Self-sufficient:** Ordnung is the source of truth; it does NOT import from or
  depend on rekordbox. It generates all metadata/analysis itself.
- **Analysis:** auto-detect **BPM** and **musical key**, key shown in **Camelot**.
- **Conversions:** convert between CDJ-compatible formats (MP3/AAC/WAV/AIFF/FLAC)
  **only when explicitly asked** — "nothing should be done without me wanting it."
- **Organization:** one flat **master pool** of everything + **playlists** (no
  genre/BPM folder nesting).
- **Scale:** < 2k tracks, local drive.
- **Interface:** CLI core now; design so a GUI can wrap the core later.

### Hard product rules (never violate)
1. **Explicit-only.** `scan`, `analyze`, `tag`, `playlist`, `convert`, `export` are
   separate commands; none does another's work as a side effect.
2. **Source files are sacred.** Tag writeback and conversion are opt-in; conversions
   write new files unless `--in-place`.
3. **Catalog is the source of truth;** the USB export is a derived artifact.
4. **Camelot is the display contract.** Keys stored canonically `(PitchClass, Mode)`,
   rendered Camelot by default.

### ⚠️ Standing rule
**Never touch the user's master library** at
`~/Library/Mobile Documents/com~apple~CloudDocs/seeker/` (~720 audio files). Test only
on copies in `testdata/seeker-sample/` — now **80 files** (was 16): the original 16 +
a 64-track random subset copied from the master pool for a larger test set. This is
also saved in project memory.

---

## 2. Architecture

Rust workspace (edition 2021), pinned to **stable** via `rust-toolchain.toml`
(lofty needs rustc ≥1.89; machine was bumped nightly 1.86 → stable 1.95).

```
ordnung/
├── Cargo.toml                       # workspace
├── rust-toolchain.toml              # channel = stable
├── PLAN.md, HANDOFF.md, KEY_CHECK.md
├── testdata/seeker-sample/          # 16 sample tracks (gitignored)
├── .claude/skills/                  # consistency anchors (see §6)
└── crates/
    ├── ordnung-core/                # domain model + engines (no UI/policy)
    │   └── src/
    │       ├── model/{mod.rs,key.rs}    # Track/Analysis/Key/Playlist/... + Camelot
    │       ├── catalog.rs               # SQLite persistence + cache
    │       ├── scan.rs                  # discovery + tag/property read + filename parse
    │       ├── tag.rs                   # opt-in tag writeback to files
    │       ├── convert.rs               # explicit ffmpeg transcode (Phase 4)
    │       ├── error.rs                 # thiserror types
    │       └── analysis/
    │           ├── mod.rs               # analyze_file orchestrator, ANALYZER_VERSION
    │           ├── decode.rs            # symphonia → mono f32 (capped)
    │           ├── dsp.rs               # STFT spectrogram (shared)
    │           ├── tempo.rs             # spectral-flux onset → BPM + beat anchor
    │           ├── key.rs               # HPCP chroma + edma profiles → Camelot
    │           └── waveform.rs          # preview + peak/RMS
    ├── ordnung-rbdb/                # rekordbox export (pdb/anlz) — STUBS, Phase 5
    └── ordnung-cli/                 # clap commands; the only policy/print layer
        └── src/{main.rs,commands.rs}
```

Dependency direction: `cli` → `rbdb` → `core`. Core never depends upward.

### Tech stack (chosen + why)
| Concern | Crate/tool | Notes |
|---|---|---|
| Audio decode | **symphonia 0.5** | Pinned to 0.5 — 0.6.0 is a fresh API rewrite with sparse docs. Features: mp3, aac, isomp4, alac, aiff (+ default flac/wav/ogg/pcm). |
| Tags | **lofty 0.24** | read/write ID3/MP4/Vorbis/RIFF/AIFF |
| DSP/FFT | **rustfft 6** | STFT for onset + chroma |
| Catalog DB | **rusqlite 0.39** (bundled) | embedded SQLite |
| Concurrency | **rayon** | parallel analysis |
| CLI | **clap 4** (derive) | command surface |
| Progress | **indicatif** | scan/analyze bars |
| Errors | **thiserror** (core) / **anyhow** (cli) | |
| rekordbox export | **rekordcrate** (planned, Phase 5) | binrw read/write of DeviceSQL; format docs at djl-analysis.deepsymmetry.org |
| Conversion | **ffmpeg** subprocess (Phase 4 ✅) | only on explicit convert/export; ffprobe-verified |

---

## 3. Current state — what's built & working

### Phase 0 — Scaffold ✅
Workspace compiles; domain model; CLI skeleton; Camelot/key module fully unit-tested.

### Phase 1 — Catalog & tags ✅
- `ordnung scan <DIR>` — discover audio, read properties+tags (lofty), upsert into
  SQLite. **Filename fallback parser** fills missing artist/title from DJ-style names
  (`01 - Artist - Title`, handles track-number prefixes and the Unicode hyphen).
- `ordnung ls [QUERY] [--limit N]` — list/filter the catalog (now shows BPM + key).
- `ordnung tag <ID> [--set field=val]... [--write]` — view/edit; `--write` opts into
  writing the source file's tags (verified via ffprobe).

### Phase 1.1 — Rescan precedence ✅
`user_edited` flag (+ forward migration). On rescan, audio properties always refresh
from the file, but tag fields are only overwritten if the user hasn't edited them.
Verified: a catalog edit survives a rescan and the source file stays untouched.

### Phase 2 — Analysis ✅ (key accuracy is the soft spot — see §5)
`ordnung analyze [QUERY] [--force]` — pure-Rust DSP, parallel (rayon), cached in an
`analysis` table; skip gated on `ANALYZER_VERSION` + source size/mtime.
- **BPM**: spectral-flux onset envelope → autocorrelation, octave-folded to a club band.
- **Beat anchor**: comb-filter phase of the onset envelope.
- **Key → Camelot**: HPCP-style chromagram (spectral **peak-picking**, parabolic
  interpolation, 4096 FFT, per-frame norm) correlated against Faraldo et al.'s
  **`edma`** EDM profiles + a small minor mode bias.
- **Waveform** preview + **peak** + **RMS dBFS** loudness (not true LUFS yet).
- Analyzer is at **v4**. Decode is capped (~150s) for speed; results cached forever.

### Tests
17 core unit tests pass (Camelot mapping, filename parser, rescan precedence,
synthetic 120-BPM click train, 440 Hz→A chroma, 3 playlist tests, + 2 convert tests:
extension/bitrate presets, output-path mapping). Two slow file-based tests
(`decode_samples`, `chroma_debug`) are `#[ignore]`d; run with `--ignored --nocapture`.

### Phase 3 — Playlists ✅
`ordnung playlist <ACTION>` over the flat master pool: `new [--folder] [--parent ID]`,
`ls` (tree), `show`, `add`, `rm`, `reorder`, `rename`, `mv [--parent ID]`, `delete`.
Backed by two cascading catalog tables (`playlists` with self-referential `parent_id`
for folders; `playlist_tracks` unique-per-playlist with `position`). Core enforces:
tracks only in playlists, nesting only under folders, no cycles, reorder = permutation.
Source files untouched. 3 new catalog tests (incl. file-based survive-reload) pass.

### Phase 4 — Conversion ✅
`ordnung convert <ID>... --to <FMT> [--bitrate K] [--out DIR] [--in-place] [--yes]`.
Engine `ordnung-core/convert.rs` shells out to **ffmpeg** (the only subprocess) with
per-format presets (mp3 libmp3lame 320k default, aac in `.m4a` 256k default, flac/wav/
aiff lossless). Verifies each output with **ffprobe** (codec must match target). Writes
NEW files by default (refuses to clobber an existing dest or the source); `--in-place`
encodes to a temp sibling then replaces the original and repoints the catalog via
`Catalog::relink_source`, gated behind a confirmation prompt unless `--yes`. Never
auto-runs. Source files are only removed under explicit `--in-place`.

### Not started
- **Phase 5** rekordbox export (the hard one; `ordnung-rbdb` is stubs) ·
  **Phase 6** validation · GUI.

---

## 4. Validation results (vs rekordbox, 79-track ground truth)

Ground truth = rekordbox's own analysis (user screenshots), now transcribed for **all
79 tracks** (80 copied into `testdata/seeker-sample/`, 1 skipped on a corrupt tag
timestamp). Full per-track table in [KEY_CHECK.md](KEY_CHECK.md).

- **BPM: 64/79 (81%) within 2 BPM, 66/79 (83%) correct modulo octave.** The original
  16-track read (75%) was unluckily weighted with the hardest genres — at scale the
  spectral-flux tempo path is the analyzer's strong suit. 13 genuine misses, mostly
  octave-doubled sparse/half-step tracks.
- **Key: 17/79 (21%) exact Camelot, 34/79 (43%) harmonically compatible.** Still the
  weak spot. Misses are dominated by wrong *tonic number*, not just A/B side — confirms
  the handoff: needs harmonic-weighted HPCP + full-track + tuning correction.
- **Side balance:** ours 55/79 minor vs rekordbox 71/79 — minor bias still too weak.
- **`1A` cluster (confirmed real):** rekordbox labels 18/79 tracks `1A` (A♭ minor); the
  user confirmed these were genuinely analyzed. We get only 3 exact, but 5 land adjacent
  (`2A`/`12A`) and 2 are the right tonic flipped to major (`4B`) — i.e. the misses
  cluster near `1A`, confirming stronger minor bias + tighter chroma/tuning as the fix.

- **BPM: 11/16 within 2 BPM**, +1 clean half-time octave error (Ifeksa 73↔146) =
  12/16 correct modulo octave. The 3 misses (The Knowledge dubstep, Barker "Cascade
  Effect" near-beatless, Flaty "Elevation" footwork) are the hardest genres for
  onset-based tempo — expected, not a bug.
- **Key: 4/16 exact Camelot** (Lime In Da Coconut 1A, 303 Views 12A, Elevation 9A,
  Space Jelly 3A); **7/16 harmonically compatible** (incl. relative/adjacent).
- Two signals from the data: (1) rekordbox called **15/16 minor (A-side)**; our side
  leans major → minor bias is too weak. (2) Most misses are wrong **tonic number**,
  which bias can't fix — needs better chroma.

---

## 5. Known issues / honest caveats

1. **Key accuracy is mediocre (4/16 exact vs rekordbox).** Two fixable causes:
   - Minor mode bias too timid (rekordbox: this genre ≈ all minor). Easy to bump,
     but only fixes the A/B side, not the number.
   - Tonic (number) errors → needs a better chromagram: **harmonic weighting** (true
     HPCP), **full-track analysis** (currently capped at 150s — an intro can mislead),
     and **tuning correction**. These are the documented next steps.
   - No labeled key set beyond these 16 tracks → don't overfit; trust the Camelot
     **number** over the A/B side (relative maj/min mix harmonically anyway).
2. **BPM octave reads wobble** on tonally/rhythmically sparse tracks.
3. **Loudness is RMS dBFS**, not BS.1770 LUFS.
4. **Key is not yet an editable field** — add to the model + `tag` command to allow
   manual correction (high-value, easy win).
5. **Phase 5 (rekordbox export) is the real risk** — reverse-engineered binary
   format; plan is to lean on rekordcrate's binrw write path and validate by
   round-tripping against real rekordbox exports (see the `rekordbox-format` skill).
6. **symphonia pinned to 0.5** deliberately; revisit 0.6 only when its docs mature.

---

## 6. Consistency assets (skills + memory)

Project skills in `.claude/skills/` (auto-surface by description; read before related
work):
- **ordnung-architecture** — crate boundaries, data model, the explicit-only rule,
  "where does my change go".
- **rekordbox-format** — export.pdb/ANLZ/USB layout reference + invariants +
  round-trip validation workflow (for Phase 5).
- **audio-analysis** — BPM/key/beatgrid algorithms, the Camelot table, the full
  key-detection lessons (peak-picking + edma profiles), cache contract.
- **ordnung-roadmap** — live phased status + per-phase definition of done.

Project memory (`~/.claude/projects/-Users-kailazarov-Desktop-Ordnung/memory/`):
- **never-touch-master-library** — the §1 standing rule.
- **key-detection-research** — papers (Sha'ath KeyFinder thesis; Faraldo et al. EDM
  key estimation; Essentia profiles) + the working approach.

---

## 7. How to build & run

```bash
# from the repo root (toolchain auto-selects stable)
cargo build --release
cargo test                                   # 12 unit tests; ignores slow file tests

# work against the SAFE sample copy + a local DB (never the master library)
DB=testdata/catalog.db
./target/release/ordnung --db "$DB" scan testdata/seeker-sample
./target/release/ordnung --db "$DB" analyze            # cached; --force to redo
./target/release/ordnung --db "$DB" ls                 # shows BPM + Camelot key
./target/release/ordnung --db "$DB" tag 4              # view one track
./target/release/ordnung --db "$DB" tag 4 --set genre=Techno [--write]
./target/release/ordnung key Am                        # Camelot/OpenKey/classical

# playlists (Phase 3) — flat master pool + ordered playlists, nestable in folders
./target/release/ordnung --db "$DB" playlist new "Sets" --folder
./target/release/ordnung --db "$DB" playlist new "Warmup" --parent 1
./target/release/ordnung --db "$DB" playlist add 2 23 3 15   # track ids from `ls`
./target/release/ordnung --db "$DB" playlist reorder 2 15 23 3
./target/release/ordnung --db "$DB" playlist ls              # tree view
./target/release/ordnung --db "$DB" playlist show 2

# conversion (Phase 4) — explicit; new files unless --in-place; ffprobe-verified
./target/release/ordnung --db "$DB" convert 4 --to flac --out /tmp/conv
./target/release/ordnung --db "$DB" convert 4 --to aac --bitrate 256 --out /tmp/conv
./target/release/ordnung --db "$DB" convert 4 --to flac --in-place   # asks to confirm

# diagnostics (slow; decode all samples)
cargo test -p ordnung-core --test chroma_debug --release -- --ignored --nocapture

# regenerate KEY_CHECK.md after re-analysis: see the python snippet in git history
```

Default DB (no `--db`) is `~/.ordnung/catalog.db`.

---

## 8. Recommended next steps (in order)

1. **TODO — Key accuracy pass** (now measured against **79** rekordbox labels in
   [KEY_CHECK.md](KEY_CHECK.md): 21% exact, 43% compatible). Diagnosis is concrete, do
   in this order:
   - **(a) Stronger minor prior** — recovers the `4B→1A` parallel-mode flips and the
     55-vs-71 minor shortfall; cheapest win, measure first in isolation.
   - **(b) Full-track chroma** — lift the ~150s decode cap for key (intros mislead).
   - **(c) Harmonic-weighted HPCP + tuning correction** — to collapse the `2A`/`12A`
     adjacents onto the true tonic.
   Re-run `analyze --force` + regenerate KEY_CHECK after each step; don't overfit to 79.
2. **Manual key correction** — make key an editable field (model + `tag`).
3. **Phase 3 — Playlists** (flat master pool + playlists; unblocks export data model).
4. **Phase 4 — Conversion** (explicit ffmpeg presets).
5. **Phase 5 — rekordbox export** (the end goal; hardest part).
