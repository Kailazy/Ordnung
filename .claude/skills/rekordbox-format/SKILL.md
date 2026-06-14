---
name: rekordbox-format
description: Reference and invariants for writing a native rekordbox/CDJ USB export — the export.pdb DeviceSQL database, ANLZ analysis files (.DAT/.EXT), and the FAT32 USB folder layout. Use whenever working on the ordnung-rbdb crate, the export command, beatgrid/cue/waveform serialization, or debugging why a CDJ won't read an exported USB.
---

# rekordbox export format (ordnung-rbdb)

This is the hardest, highest-risk part of Ordnung. Treat the reverse-engineered spec
as authoritative and validate every byte against a real rekordbox-produced export.

## Canonical references

- **Deep Symmetry djl-analysis** — the authoritative reverse-engineered docs for
  `export.pdb` and ANLZ: https://djl-analysis.deepsymmetry.org/
- **rekordcrate** (Rust, `binrw` read/write of DeviceSQL/PDB):
  https://github.com/Holzhaus/rekordcrate — build on its structs; do not re-derive.
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

## Validation workflow

1. Export a tiny library (2–3 tracks, 1 playlist, a few cues) from Ordnung.
2. Re-parse our `export.pdb`/ANLZ with rekordcrate; diff structure vs a reference
   export produced by rekordbox for the same files.
3. Load on real CDJ/XDJ (or rekordbox in export mode) and confirm tracks, waveforms,
   beatgrids, cues, and playlists appear.

Keep a `fixtures/` set of small rekordbox-produced exports as golden references.
