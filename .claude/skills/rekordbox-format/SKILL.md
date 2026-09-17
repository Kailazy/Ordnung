---
name: rekordbox-format
description: Reference and invariants for writing a native rekordbox/CDJ USB export — the export.pdb DeviceSQL database, ANLZ analysis files (.DAT/.EXT), and the FAT32 USB folder layout. Use whenever working on the ordnung-rbdb crate, the export command, beatgrid/cue/waveform serialization, or debugging why a CDJ won't read an exported USB.
---

# rekordbox export format (ordnung-rbdb)

This is the hardest, highest-risk part of Ordnung. Treat the reverse-engineered spec
as authoritative and validate every byte against a real rekordbox-produced export.

## Canonical references

- **`docs/rekordbox-export-structure.md`** — our own byte-level dissection of a
  real rekordbox 7.2.2 export (EYEBAGS stick, 2026-09-04): full field maps,
  identifications DS leaves unknown (masterContentId/fileType/update-counter
  fields in track rows, page bookkeeping semantics, free_size formula, PQT2/
  PWVC/PVB2 sections, exportExt = My Tag, full DLP schema), and a Phase 5
  writer checklist. Read it FIRST when writing export bytes.
- **Deep Symmetry djl-analysis** — the authoritative reverse-engineered docs for
  `export.pdb` and ANLZ: https://djl-analysis.deepsymmetry.org/
- **rekordcrate** (Rust, `binrw` read/write of DeviceSQL/PDB):
  https://github.com/Holzhaus/rekordcrate — reference implementation for
  cross-checking. Ordnung's writer is now **self-contained** (`pdbw`/`anlz`/
  `export`/`dlp::write_library`, landed 2026-09-04): rekordcrate's toolchain
  problems and private fields made writing our own, validated against the
  golden dissection, the safer path. Consult it when bytes disagree.
- Original RE credit: Henry Betts, Fabian Lesniak, James Elliott.

## USB layout (FAT32, MBR)

```
/CONTENTS/...                     # the audio files (our flat master pool)
/PIONEER/rekordbox/export.pdb     # DeviceSQL DB: tracks, artists, albums, genres,
                                  #   playlists, playlist tree, keys, colors, ...
/PIONEER/USBANLZ/<...>/ANLZ0000.DAT   # beatgrid, cue list, waveform preview, path
                       /ANLZ0000.EXT   # extended: color/detailed waveforms, nxs2 cues
```

- USB must be **FAT32 with Master Boot Record** for broad CDJ compatibility.
- Track rows in `export.pdb` reference their ANLZ file path; keep them consistent.

## export.pdb (DeviceSQL) essentials

- Page-based DeviceSQL database; tables are linked lists of pages of rows.
- Strings use **DeviceSQLString** (short/long forms) — use rekordcrate's type,
  never hand-encode.
- Core tables to populate: Tracks, Artists, Albums, Genres, Keys, Colors, Labels,
  Artwork, PlaylistTree, PlaylistEntries.
- Track rows hold: title, artist/album/genre/key/label ids, bpm (×100 integer),
  duration, sample rate, bitrate, file path, file size, date added, analyze path.
- IDs are interned: dedupe artists/albums/genres/keys into their tables and reference
  by id. BPM is stored as an integer = round(bpm × 100).

## ANLZ files (.DAT / .EXT)

Tagged section format ("PMAI" header, then `PXXX` tagged sections). Key sections:

- `PQTZ` — beat grid (beat number, tempo, time in ms per beat).
- `PCOB`/`PCO2` — cue list (memory + hot cues); `PCO2` carries nxs2 color/label.
  Written from `Track::cues` since 2026-09-17: hot list before memory list in
  both files; `PCPT` 0x38 fixed (hot_cue = pad+1, type 1 point / 2 loop,
  status 4 for loops, loop_time 0xFFFFFFFF unless loop); `PCP2` 0x58 + comment
  bytes (UTF-16BE, no terminator), colour code 0 + RGB, 40 zero tail bytes.
  `anlz::read_cues` reads them back for tests and the GUI's device tracks.
- `PPTH` — file path; `PVBR` — VBR seek index for MP3.
- `PWAV`/`PWV2` — waveform preview; `PWV3`/`PWV4`/`PWV5` — detailed/color waveforms.
- `.DAT` = the classic set CDJs require; `.EXT` = extended (color waveforms, nxs2).

Camelot/Open Key mapping for the Keys table: store canonical pitch/mode in the
catalog; map to rekordbox's key id/name on export (rekordbox uses Open Key labels
internally — see `audio-analysis` for the Camelot↔OpenKey table).

## Invariants (check these before claiming export works)

1. `export.pdb` parses cleanly when re-read by rekordcrate (round-trip).
2. Every track row points to existing ANLZ files; every ANLZ `PPTH` matches `/CONTENTS` path.
3. BPM = round(bpm×100); durations in seconds; positions in ms (and sample where required).
4. Beatgrid first downbeat aligns with the analysis beatgrid anchor.
5. Playlists in `PlaylistTree` reference valid track ids in entry order.
6. USB is FAT32/MBR and paths use the exact casing CDJs expect.
7. **Table-first pages are INDEX pages with a mandatory body.** Every table's
   first page (flags 0x64) must carry the full empty-index form — magics
   0x03ec and 0x03ffffff, num_entries/first_empty, and the entry array filled
   with 0x1FFFFFF8 (last 20 bytes zero) — plus zeroed empty-candidate pages
   terminating each chain. A zero-body sentinel reads as "Device library is
   corrupted" on players (and fails rekordcrate). `pdbw::sentinel_page`
   writes the canonical form; validate any pdb-writer change with
   `rekordcrate dump-pdb` (build with `rustup run stable`, cli feature)
   before it touches a stick.
8. **Never let SQLite write in place on the mounted stick.** macOS's msdos
   (FAT32) driver breaks SQLite mid-write ("attempt to write a readonly
   database" after the first commit), leaving a truncated exportLibrary.db
   that players reject as "Device library is corrupted". Build/edit the DLP
   database on local disk and copy the finished file over — `dlp::write_library`
   and `edit::sync_dlp_playlists` both do this; keep any new SQLite-on-stick
   path doing the same.

## Player compatibility (what the export converts)

| Player | FLAC | Max rate | Export policy (`PlayerTarget`) |
|---|---|---|---|
| CDJ-2000, 2000NXS, 900, 850, XDJ-1000 | no | 48 kHz | `Classic`: FLAC and >48 kHz → 16-bit AIFF on the stick |
| CDJ-2000NXS2, XDJ-1000MK2, XDJ-RX2/XZ, CDJ-3000, OPUS-QUAD | yes | 96 kHz | `Modern` (default): copy as-is; >96 kHz → AIFF at 96 kHz |

MP3, AAC, WAV and AIFF (16/24-bit) play on every generation. Conversions
write only under `/Contents`; the library file is never touched.

## Validation workflow

1. Export a tiny library (2–3 tracks, 1 playlist, a few cues) from Ordnung.
2. Re-parse our `export.pdb`/ANLZ with rekordcrate; diff structure vs a reference
   export produced by rekordbox for the same files.
3. Load on real CDJ/XDJ (or rekordbox in export mode) and confirm tracks, waveforms,
   beatgrids, cues, and playlists appear.

Keep a `fixtures/` set of small rekordbox-produced exports as golden references.
