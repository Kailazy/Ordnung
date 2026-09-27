# rekordbox reference suite (`rb_suite`)

Does Ordnung write a stick a CDJ reads exactly the way rekordbox writes it?
The suite answers that by comparing the two USB images byte by byte.

A **reference** is a verbatim copy of a stick exported by rekordbox. The
suite rebuilds the same songs in Ordnung, performs every operation the
reference shows through the same catalog calls the GUI makes, exports, and
diffs Ordnung's stick against rekordbox's: `export.pdb`, every ANLZ section
(`.DAT` / `.EXT` / `.2EX`), both playlist databases, the Device Library Plus
columns, My Tags, artwork and the files under `/PIONEER`.

```
make rb-suite                      # every reference
make rb-suite NAME=stop126         # one reference
make rb-suite NAME=stop126 LEG=mirror
make rb-accept NAME=stop126        # pin the numbers after reviewing a run
make rb-capture STICK=/Volumes/EYEBAGS NAME=ops
```

`STICK` is any folder that holds `PIONEER/`: a mounted USB, or a copy of
one on disk. For example, the stop126 reference comes from
`make rb-capture STICK=~/Desktop/EYEBAGS-rekordbox-stop126-2026-09-21 NAME=stop126`.

## Layout

| Path | Tracked | What |
|---|---|---|
| `testdata/rekordbox-reference/<name>/` | no (holds audio) | the stick image: `PIONEER/` + `Contents/`, or `before/` + `after/` |
| `testdata/rekordbox-suite/<name>/MANIFEST*.sha256` | yes | SHA-256 of every reference file under `PIONEER`. A run refuses a reference whose bytes changed |
| `testdata/rekordbox-suite/<name>/accepted.tsv` | yes | the accepted baseline |
| `target/rb-suite/<name>/` | no | scratch catalogs, the exported sticks, `report.txt`, `tally.tsv` |

The suite never writes to a reference or a stick. It clones the audio into
`target/rb-suite/<name>/audio` and does all its work there.

## Legs

- **mirror**: the main byte-for-byte check. Ordnung scans and analyzes the
  reference's own audio. Then it applies what rekordbox shows through the
  GUI's catalog calls: title, artist, album, genre, label, year, comment,
  rating, track and disc number, remixer, composer, original artist, key,
  BPM, beatgrid, and hot and memory cues and loops with their colours and
  comments. It also rebuilds the playlist tree. Then it exports the stick
  and diffs it. The only data Ordnung computes itself here are the
  waveforms and the artwork JPEGs. Waveforms are scored by shape
  (correlation ≥ 0.85) and height (ours ÷ rekordbox's between 0.8 and
  1.25). Artwork is compared by size.
- **analysis**: Ordnung's own detection, left untouched. It grades BPM,
  key, grid phase (median offset ≤ 10 ms) and downbeat against rekordbox.
- **edit**: in-place edits on a copy of the reference: a full cue set
  (pads A–H, a hot loop, memory cues and a memory loop) on every third
  track, a 20 ms grid shift on the next third, and five playlist
  operations. Each edit must read back, and every byte outside what the
  edit owns must stay identical, down to the `export.pdb` page.
- **edit-mirror**: for a `before/` + `after/` pair. It replays the changes
  made in rekordbox with Ordnung's in-place editor and diffs the result
  against `after`. Changes Ordnung has no in-place operation for (tags,
  colours, playlist reordering) are listed as unsupported.

## Reading a run

Every difference is either **explained** (per-export ids and dates,
Ordnung's flat `/Contents`, key notation, rekordbox's stale file sizes) or
flagged `!!`. The summary counts the flagged differences per `area field`;
`report.txt` lists examples of each. The coverage table shows what the
reference exercises next to what came out of Ordnung. A `<` marks a feature
Ordnung dropped.

A run fails when a flagged count rises, a new one appears, or an analysis
score drops below `accepted.tsv`. When a run improves, it says so; lock the
gain in with `make rb-accept`.

## Making a reference that exercises every operation

`stop126` (the 94-track "stop1 sep 26" export, 2026-09-21) is a plain
analyzed export: grids, keys, tags and artwork, but no cues, ratings,
colours or My Tags. To cover everything a DJ sees on the player, build an
**ops** reference from the same playlist in rekordbox 7. It already sits in
the rekordbox collection. Track numbers below are positions in the
playlist.

1. Duplicate "stop1 sep 26" as **RB Suite**, so the original stays as it is.
2. **Hot cues**: tracks 1–10 get all eight pads A–H. On tracks 1–5, give
   every pad its own colour and a comment. On tracks 11–15, make pads C and
   F loops (4 and 16 beats).
3. **Memory cues**: tracks 16–25 get three memory cues each. On 21–25, make
   one a loop. On 16–20, give them colours and comments.
4. **Beatgrid**: shift the grid on tracks 26–30. Change the tempo on 31–33.
   Set a variable-tempo (dynamic) grid on 34.
5. **Key and BPM by hand**: edit the key on 35–36 and the BPM on 37.
6. **Tags edited in rekordbox**: on tracks 38–47, change the title, artist,
   album, genre, label, comment, year and track number, and set a remixer
   and a composer. Clear the genre on 46 and the comment on 47.
7. **Rating**: one to five stars across tracks 48–57.
8. **Track colour**: all eight colours across tracks 58–65.
9. **My Tags**: tag tracks 66–72, some with several tags.
10. **Playlists**: a folder **RB Suite folder** holding two playlists and a
    sub-folder with one more playlist, a playlist in a hand-sorted order,
    and an empty playlist.
11. Quit anything else using the stick. Export the RB Suite playlist and
    the folder to a freshly formatted FAT32 stick (MBR).
12. Eject the stick and plug it back in. **Quit rekordbox**, because its
    device agent can rewrite or delete a mounted stick's databases. Then:

    ```
    make rb-capture STICK=/Volumes/EYEBAGS NAME=ops
    make rb-suite NAME=ops
    ```

    Review `target/rb-suite/ops/report.txt`, then `make rb-accept NAME=ops`
    and commit the new `testdata/rekordbox-suite/ops/`.

**Changes made to an existing stick** (the edit-mirror leg): capture the
stick right after step 11 with `STAGE=before`. Then change cues, a grid,
tags and playlists for tracks already on the stick, re-export to the same
stick, and capture again with `STAGE=after`. Use the same `NAME` for both.

## Refreshing a reference

A reference is pinned by its manifest. `make rb-capture` refuses to
overwrite an existing reference unless `--replace` is passed (run the
example directly). Re-capturing rewrites the manifest, so commit the new
manifest together with the reason for it.
