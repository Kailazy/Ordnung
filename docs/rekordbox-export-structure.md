# rekordbox USB export — byte-level structure reference

Reverse-engineering notes for the Phase 5 writer (`ordnung-rbdb`), produced by
dissecting the EYEBAGS golden-reference stick byte by byte on 2026-09-04.

Provenance: real rekordbox **7.2.2** export (DEVSETTING.DAT carries the
version), 609 tracks, 3 playlists, exported 2026-04-16 and updated since; the
stick has also been in an **XDJ-AZ** (fw 1.23, per RBFLTR.DAT). Everything
below was verified against actual bytes — "all rows" means all 609 track rows.
Where a claim comes from Deep Symmetry / rekordcrate instead of our own bytes
it is marked (DS). Fields DS leaves unknown that we identified are marked
**[new]**.

Companion references: <https://djl-analysis.deepsymmetry.org/> (site root; the
export docs live under `rekordbox-export-analysis/`), and crate-digger's
`rekordbox_anlz.ksy` Kaitai spec.

---

## 1. Volume layout

Physical: **MBR (FDisk) partition scheme, one Windows_FAT_32 partition**
(34.4 GB partition on a 124.6 GB stick — undersized partitions are fine).

```
/Contents/<Artist>/<Album>/<file>       audio (rekordbox's layout; any layout works,
                                        pdb rows store the literal path)
/PIONEER/rekordbox/export.pdb           DeviceSQL DB (§2)
/PIONEER/rekordbox/exportExt.pdb        DeviceSQL DB, My Tag data (§3)
/PIONEER/rekordbox/exportLibrary.db     Device Library Plus, SQLCipher-4 (§4)
/PIONEER/rekordbox/exportLibrary.db-shm/-wal   WAL sidecars (may exist, may be empty)
/PIONEER/rekordbox/RBFLTR.DAT           written by the player (§7)
/PIONEER/USBANLZ/P###/<8-hex>/ANLZ0000.{DAT,EXT,2EX}   analysis files (§5)
/PIONEER/Artwork/<5-digit>/{aN,aN_m,bN,bN_m}.jpg       artwork (§6)
/PIONEER/{DEVSETTING,MYSETTING,MYSETTING2,DJMMYSETTING}.DAT   settings (§7)
/PIONEER/{CDP,log,.CacheData,TrashBox}  player/rekordbox litter, not needed
```

Case as shown ("Contents", "Artwork" — not CONTENTS/ARTWORK; FAT is
case-insensitive but rekordbox writes exactly these).

---

## 2. export.pdb (DeviceSQL)

All integers little-endian. Page size 4096.

### 2.1 File header (page 0)

| off  | type | value here | meaning |
|------|------|-----------|---------|
| 0x00 | u32  | 0         | magic |
| 0x04 | u32  | 4096      | len_page |
| 0x08 | u32  | 20        | num_tables |
| 0x0c | u32  | 136       | next_unused_page (== file length / 4096 + 1 slack) |
| 0x10 | u32  | 5         | unknown, constant 5 |
| 0x14 | u32  | 2381      | sequence (global transaction counter; every data page's own tx id is ≤ this) |
| 0x18 | u32  | 0         | gap |
| 0x1c | 20 × 16B | —     | table directory |

Table directory entry: `u32 type, u32 empty_candidate, u32 first_page, u32 last_page`.
**All 20 types 0..19 present, in order, even when empty.** Tables are
pre-allocated a sentinel page at odd indices: type *n* gets first_page
2n+1 (1,3,5,…,39). `empty_candidate` = a page number the allocator may use
next (points at allocated-but-unlinked pages; safe to set = last_page+1 when
writing fresh).

Table types: 0 tracks, 1 genres, 2 artists, 3 albums, 4 labels, 5 keys,
6 colors, 7 playlist_tree, 8 playlist_entries, 9/10 unknown (empty), 11
history_playlists (empty), 12 history_entries (empty), 13 artwork, 14/15
unknown (empty), 16 menu columns, 17 browse categories, 18 sort menu,
19 export summary (DS calls it "history"; see §2.7).

### 2.2 Page header (0x28 bytes)

| off  | type | meaning |
|------|------|---------|
| 0x00 | u32  | 0 |
| 0x04 | u32  | page_index (self) |
| 0x08 | u32  | table type |
| 0x0c | u32  | next_page (last page in a chain points at empty_candidate / an unlinked page — stop at last_page, don't chase) |
| 0x10 | u32  | **[new]** transaction id of the last write to this page (≤ header sequence). Sentinel pages: 1 |
| 0x14 | u32  | 0 always |
| 0x18 | u8   | num_row_slots — counts slots **including deleted rows** |
| 0x19 | u16 (unaligned) | **[new]** 32 × number of *present* rows (the next index_shift to hand out) |
| 0x1b | u8   | page_flags: 0x24 data, 0x34 data that has seen deletes/rewrites (bit 0x10), 0x64 sentinel (bit 0x40 = no rows — skip page) |
| 0x1c | u16  | free_size. **Exact rule:** `4096 − 0x28 − used_size − (4·num_row_groups + 2·num_row_slots)` (verified to the byte on many pages) |
| 0x1e | u16  | used_size — heap bytes consumed incl. per-row padding |
| 0x20 | u16  | **[new]** rows written in the page's most recent write batch (1 while appending; full row count for a page written in one shot; 0x1fff on sentinels and occasionally on rewritten pages) |
| 0x22 | u16  | **[new]** slot index of the most recently written row (0-based). NOT "num_rows_large" in rb7 exports; the >255-row case never occurs (max ~124 rows/page). 0x1fff on sentinels/rewritten pages |
| 0x24 | u16  | 0 on data pages; 1004 on sentinel pages (constant) |
| 0x26 | u16  | 0 on data pages; sentinel: 18 for tracks, 1 for type 19, else 0 |

Row heap starts at 0x28 and grows up; the row index sits at the page end and
grows down.

### 2.3 Row index (page tail)

Rows are addressed in groups of 16. Group *g* occupies the 36-byte block
`[4096 − 36(g+1), 4096 − 36g)`, laid out from the block's END:

```
block_end-2:  u16  [new] "last-batch bitmask" — bits of the rows most recently
                   written in this group. One-shot pages: equals the presence
                   flags. Append-mode pages: just the newest row's bit.
                   Matches page hdr 0x22's slot. Writer: mirror the presence flags.
block_end-4:  u16  row presence flags (bit r = slot 16g+r live; deleted rows
                   keep their offset but drop their bit)
block_end-6:  u16  heap offset of slot 16g+0   (relative to page+0x28)
block_end-8:  u16  slot 16g+1 … and so on downward (slot 15 lowest)
```

Only `ceil(slots/16)` groups exist; a group stores only the offset words it
needs (accounting per §2.2 free_size formula). Deleted slots are real: pages
exist with e.g. 10 slots, 8 present.

### 2.4 DeviceSQLString

First byte = kind:
- **odd** → short ASCII: length = (kind >> 1) − 1, bytes follow immediately.
  Max 126 chars (kind 0xFF). Empty string = single byte 0x03.
- **0x40** → long ASCII: u16 total_len (includes the 4-byte header), u8 pad,
  then total_len−4 bytes. Used only when >126 chars.
- **0x90** → long UTF-16LE, same u16 total_len header. Also used for the ISRC
  quirk: when the body starts 0x03, the rest is NUL-terminated ASCII
  (rekordbox writes ISRC values this way).

Verified across all 609 rows: title/filename/file_path use short form until
length forces 0x40, and 0x90 whenever non-ASCII appears anywhere.

### 2.5 Track row (type 0)

Fixed 0x88-byte header, then packed strings. All 21 string-offset u16s at
0x5e; each offset is relative to row start.

| off  | type | field | observed |
|------|------|-------|----------|
| 0x00 | u16  | magic | 0x0024 always |
| 0x02 | u16  | index_shift | 32 × slot index within page (verified all rows) |
| 0x04 | u32  | **[new]** = DLP `content.contentLink` | 0xC0700 in every row and every DLP row (DS "bitmask") |
| 0x08 | u32  | sample_rate Hz | 44100/48000/96000 |
| 0x0c | u32  | composer artist id | 0 when none |
| 0x10 | u32  | file_size bytes | |
| 0x14 | u32  | **[new]** rekordbox master-db content id (`content.masterContentId`, random 28-bit) | matched 609/609 (DS "unknown2") |
| 0x18 | u16  | unknown | 12251 (0x2FDB) in all rows |
| 0x1a | u16  | unknown | 36677 (0x8F45) in all rows |
| 0x1c | u32  | artwork id | 0 = none |
| 0x20 | u32  | key id | into keys table |
| 0x24 | u32  | original-artist id | |
| 0x28 | u32  | label id | |
| 0x2c | u32  | remixer artist id | |
| 0x30 | u32  | bitrate kbps | 1411 AIFF, 1536 = 48k AIFF, 320 MP3 … |
| 0x34 | u32  | track number | 0 when unset |
| 0x38 | u32  | tempo = BPM × 100 | |
| 0x3c | u32  | genre id | |
| 0x40 | u32  | album id | |
| 0x44 | u32  | artist id | |
| 0x48 | u32  | id (this table's key, 1-based dense) | 1..609 contiguous |
| 0x4c | u16  | disc number | |
| 0x4e | u16  | play count (`djPlayCount`) | |
| 0x50 | u16  | year | |
| 0x52 | u16  | sample depth bits | 16/24/32 |
| 0x54 | u16  | duration whole seconds | |
| 0x56 | u16  | unknown | 41 (0x29) in all rows (DLP `analysedBits` is 41 or 105, so not the same field; write 41) |
| 0x58 | u8   | color id | |
| 0x59 | u8   | rating 0–5 | |
| 0x5a | u16  | **[new]** file type enum = DLP `content.fileType` | 1 MP3, 5 FLAC, 11 WAV, 12 AIFF (609/609; DS "unknown6"; m4a presumably 4) |
| 0x5c | u16  | unknown | 3 in all rows |
| 0x5e | u16×21 | string offsets, in this order: |

0 isrc (0x90-ISRC form) · 1 texter · 2 **[new]** `informationUpdateCount` as
decimal string · 3 **[new]** `analysisDataUpdateCount` · 4 **[new]**
`cueUpdateCount` · 5 message · 6 kuvo_public ("ON" everywhere) · 7
autoload_hotcues ("ON" everywhere) · 8, 9 empty · 10 date_added "YYYY-MM-DD" ·
11 release_date · 12 mix_name · 13 empty · 14 analyze_path
(`/PIONEER/USBANLZ/P###/<8-hex>/ANLZ0000.DAT` — the .DAT, never .EXT) · 15
analyze_date "YYYY-MM-DD" · 16 comment · 17 title · 18 empty · 19 filename ·
20 file_path (`/Contents/...`, volume-absolute, forward slashes).

The update-counter strings (2/3/4) matched the Device Library Plus columns
78/78 wherever DLP still has values; empty string when the counter is 0. A
writer can put "1"/"1"/"" for freshly analyzed tracks, or just mirror pdb ↔
DLP.

Rows are 2-aligned (index_shift/heap offsets are even); rekordbox pads row
ends generously (e.g. a 285-byte row padded to 336). Padding is
non-normative — readers use the offset index.

### 2.6 Small-table rows

All verified against live bytes; ids are 1-based and interned in first-seen
order.

- **genre / label (1, 4):** `u32 id, string name`. 4-aligned.
- **key (5):** `u32 id, u32 id2 (== id), string name`. Names are **literal
  Camelot strings** ("8A", "12B"…); 24 keys interned in first-seen order.
- **artist (2):** `u16 subtype (0x60, or 0x64 when the name offset exceeds
  0xFF), u16 index_shift, u32 id, u8 0x03, u8 ofs_name (0x0a)`; subtype 0x64
  inserts `u16 ofs_name_far` at 0x0a and the name follows. Name offset is
  relative to row start.
- **album (3):** `u16 0x80, u16 index_shift, u32 0, u32 artist_id, u32 id,
  u32 0, u8 0x03, u8 ofs_name (0x16), string name`.
- **color (6):** `u32 0, u8 id2 (== id), u16 id, u8 0, string name`. Exactly 8
  rows always: Pink Red Orange Yellow Green Aqua Blue Purple (ids 1–8).
- **artwork (13):** `u32 id, string path` — path is the small
  `/PIONEER/Artwork/<dir>/a<id>.jpg` (§6).
- **playlist_tree (7):** `u32 parent_id, u32 unknown, u32 sort_order, u32 id,
  u32 raw_is_folder, string name` (DS + our reader; **empty in rb7 exports**).
- **playlist_entries (8):** `u32 entry_index, u32 track_id, u32 playlist_id`
  (DS; empty in rb7 exports).

### 2.7 Browse-menu tables 16/17/18 and summary table 19 [new]

These mirror three Device Library Plus tables row-for-row (verified by count
and content):

- **16 = menuItem** (27 rows): `u16 id, u16 kind, string name` where kind is
  128+n (0x80 GENRE, 0x81 ARTIST, 0x82 ALBUM, 0x83 TRACK, 0x85 BPM, 0x86
  RATING, 0x87 YEAR, 0x88 REMIXER, …; 0x84 unused). Name is a 0x90 UTF-16LE
  string whose text is wrapped in U+FFFA … U+FFFB (interlinear annotation
  anchors): `￺GENRE￻`. The player's browse-menu labels.
- **17 = category** (22 rows, 8-byte): `u16 menuItem_id, u16 sequenceNo, u32
  flags` (observed 0x163 / 0x105; low bit ≈ isVisible). Browse-tree order.
- **18 = sort** (17 rows, 8-byte): `u16 menuItem_id, u16 sequenceNo, u32
  isVisible(=1)`. Sort-menu order.
- **19 = export summary** (page written with 96 × 40-byte slots, exactly one
  present): `u16 0x0280, u16 index_shift, u32 numberOfContents (609), u32 0,
  string createdDate ("2026-04-16"), 2 unknown bytes (0x19 0x1e), string
  dbVersion ("1000"), empty string, pad to 40`. Matches the DLP `property`
  row. (DS calls this table "history"; the actual DLP history playlist is
  *not* here.)

A writer that wants maximum fidelity replicates 16/17/18 verbatim (they're
static menus) and writes one type-19 row; CDJ-2000-era players are not known
to require any of them.

### 2.8 What a writer must get right vs. bookkeeping

CDJs resolve rows through: table directory → page chain → presence flags →
row offsets → row fields → DeviceSQLStrings. The bookkeeping fields
(tx ids @0x10, batch fields @0x20/@0x22, last-batch bitmask, 0x19-counter,
free/used sizes) are rekordbox's own write-side state; for a one-shot export
write them in the "one-shot signature" observed on colors/columns pages:
`0x20 = row count, 0x22 = 0, trail bitmask = presence flags, tx id = same
constant everywhere, header sequence = that constant + a few`.

---

## 3. exportExt.pdb — My Tag database

Same DeviceSQL container: 4096-byte pages, **9 tables** (types 0–8 by
position), each with the same sentinel-page scheme. Only two hold data:

- **slot 3 — tag definitions** (rows in the same heap/index format):
  `u16 0x0680, u16 index_shift, u32 0, u32 0, u32 kind (0 = category row, 1 =
  tag row), u32 sort_order, u32 random_id, u32 0, u8 0x03, u8 ofs_name, u8
  ofs_empty, string name, empty string`. Categories ("Genre", "Components",
  "Situation"…) and their child tags interleave; parentage is positional
  (tags follow their category row).
- **slot 7 — track↔tag map:** `u16 0x0700, u16 index_shift, u32s…, u32
  random_id, u8 0x03, u8 ofs ×5, five strings` (all empty on this stick — no
  tracks are tagged).

Safe for Phase 5: write the 9-table skeleton with empty tables (or copy
these 28 definition rows); nothing on a CDJ requires it.

---

## 4. exportLibrary.db — Device Library Plus

SQLite + **SQLCipher 4 defaults** (PBKDF2-HMAC-SHA512 ×256000, AES-256-CBC
per page, HMAC-SHA512, salt = first 16 file bytes, 80 reserved bytes/page,
page 4096, **WAL journal**, UTF-8). Static documented key in
`ordnung-rbdb::dlp::DLP_KEY`. Decrypted and HMAC-verified page-by-page.

22 tables. The ones that matter, with join keys:

- `content` (609): one row per track. Carries *everything* the pdb track row
  has: `bpmx100, length, path, fileName, fileSize, fileType, bitrate,
  bitDepth, samplingRate, rating, releaseYear/-Date, dateAdded, djComment,
  isrc, djPlayCount, isHotCueAutoLoadOn, isKuvoDeliverStatusOn`, all the
  `artist_id_*` / `album_id` / `genre_id` / `label_id` / `key_id` /
  `color_id` / `image_id` interned refs, plus `masterContentId` (= pdb
  0x14), `masterDbId` (one constant per library), `contentLink` (= pdb 0x04),
  `analysisDataFilePath` (same ANLZ path, **without** the `ANLZ0000.DAT`
  filename… it stores the full .DAT path here too), `analysedBits` (41 or
  105), `hasModified`, and the three UpdateCount columns mirrored into pdb
  string slots 2/3/4. **content_id is its own 1..609 sequence and happens to
  equal the pdb track id row-for-row here, but the documented join key is the
  path.**
- `playlist` (tree: `playlist_id, sequenceNo, name, image_id, attribute
  (0 list / 1 folder), playlist_id_parent`) and `playlist_content`
  (`playlist_id, content_id, sequenceNo` 1-based). **rekordbox 7 writes
  playlists ONLY here** — export.pdb's tables 7/8 stay empty.
- `artist/album/genre/label/key/color/image`: `*_id, name/path` interned
  tables; `key` holds the same 24 Camelot strings; `image.path` points at the
  **b**-named artwork (`/PIONEER/Artwork/00001/b1.jpg`) — same JPEG bytes as
  the a-file (§6).
- `menuItem` (27) / `category` (22) / `sort` (17): the source of pdb tables
  16/17/18, including the U+FFFA/U+FFFB-wrapped names.
- `history` + `history_content`: session history ("HISTORY 001").
- `myTag` (28) + `myTag_content`: the exportExt.pdb data in SQL form.
- `cue`, `hotCueBankList(_cue)`, `recommendedLike`: present, empty here (hot
  cues live in ANLZ; this `cue` table is for exported cue banks).
- `property` (1 row): `deviceName, dbVersion ('1000'), numberOfContents,
  createdDate, backGroundColorType, myTagMasterDBID`.

Target decision recorded in the skill: older CDJs read export.pdb playlists;
OPUS-QUAD/OMNIS-DUO/XDJ-AZ-class read this DB. Writing playlists into *both*
is the compatible choice.

---

## 5. ANLZ files

Per track dir `/PIONEER/USBANLZ/P###/<8-hex>/`. P-dirs 000–125 and the 8-hex
names are **random allocations from rekordbox's master library** (no
derivable mapping from track id or masterContentId; ~5 tracks/P-dir here).
The pdb row stores the literal path, so a writer may use any scheme. When
two identical audio files exist, rekordbox reuses the dir and names the
second set `ANLZ0001.*` (observed twice in 609).

All values **big-endian**. File = `PMAI` header + tagged sections:
`u32 fourcc, u32 len_header, u32 len_tag` (len_tag = total section size;
next section at +len_tag).

PMAI header (0x1c bytes) — identical in all 1821 files:
`"PMAI", 0x1c, len_file, 0x00000001, 0x00010000, 0x00010000, 0x00000000`.
(Correction 2026-09-04: the third u32 is 0x00010000, not 0x00000001 — the
original transcription was wrong; re-verified against all 1218 DAT/EXT files.
Writing 1 there makes rekordbox silently ignore the analysis: tracks browse
fine but show no waveform previews.)

Section order is rigid (all 607 sets):

```
.DAT  PPTH PVBR PQTZ PWAV PWV2 PCOB(hot) PCOB(memory)
.EXT  PPTH PWV3 PCOB(hot) PCOB(mem) PCO2(hot) PCO2(mem) PQT2 PWV5 PWV4
      (7 old files lack PQT2; 1 VBR MP3 appends PVB2 at the end)
.2EX  PPTH PWV7 PWV6 PWVC
```

### 5.1 Sections

**PPTH** (len_header 0x10): `u32 len_path`, then UTF-16**BE** path with
trailing NUL (len_path counts the NUL's 2 bytes). Same `/Contents/...` string
as the pdb row.

**PVBR** (0x10): `u32 0`, then 400 × u32 seek index, then implicit; all
zeros for CBR files (AIFF/CBR-MP3 — i.e. usually all zero). One VBR MP3 on
the stick instead got **PVB2 [new]** in its .EXT: header 0x20 = `u32 0, u32 0,
u32 file_size, u32 0x190 (=400), u32 0x14 (=20)`, then 400 × 20-byte entries.

**PQTZ** (0x18): `u32 0, u32 0x00080000, u32 num_beats`, then per beat:
`u16 beat_number (1..4 position in bar; 1 = downbeat), u16 tempo (BPM×100 —
per-beat, so dynamic grids just vary it), u32 time_ms`. First beat ≈ first
downbeat anchor; beats cover the whole file.

**PWAV / PWV2** (0x14): `u32 len (400 / 100), u32 0x00010000`, then len
bytes: `bits 0–4 height (0–31), bits 5–7 "whiteness"`. PWAV is the 400-column
preview, PWV2 the 100-column tiny one (CDJ-900 screen).

**PCOB** (0x18) — cue list, classic: `u32 type (1 = hot cues, 0 = memory —
hot list comes FIRST in both files), u16 0, u16 count, u32 0xFFFFFFFF
("memory_count")`, then count × **PCPT** entries (0x38 bytes, len_header
0x1c): `u32 hot_cue (0 = memory, 1.. = hot cue A..), u32 status (0 observed;
DS: 4 = active loop), u32 0x00010000, u16 order_first (0xFFFF), u16
order_last (0xFFFF), u8 type (1 point, 2 loop), u8 0, u16 1000, u32 time_ms,
u32 loop_time_ms (0xFFFFFFFF unless loop), 16 zero bytes`.

**PCO2** (0x14) — nxs2 cue list: `u32 type (1 hot / 0 memory), u16 count,
u16 0`, then **PCP2** entries (len_header 0x10, len_entry 0x58 observed):
`u32 hot_cue, u8 type, u8 0, u16 1000, u32 time_ms, u32 loop_time, u8
color_id (memory cues), u8 = 1 [new, constant], u8[6] 0, u16 loop_numerator,
u16 loop_denominator, u32 len_comment, UTF-16BE comment, u8 color_code, u8
red, u8 green, u8 blue, pad to len_entry`. rekordbox 7's default hot-cue
color is written as code 0, RGB (255, 0, 23). Empty lists are legal and are
what rekordbox writes for un-cued tracks (24-byte PCOB, 20-byte PCO2).

**PQT2 [new]** (0x38 — not in DS/kaitai): extended beat grid.
`u32 0, u32 0x01000002, u32 0, two 8-byte PQTZ-style beat entries = the
track's FIRST and LAST beat, u32 num_payload_entries, u32 unknown (0 or
random-looking), u32 0, u32 0`, then num × u16 payload — one per PQTZ beat.
Payload semantics still unresolved (per-track modular ramp ≈ mod 1000, step
uncorrelated with BPM). **rekordbox itself writes header-only PQT2 with
count 0 and no payload** (observed), so a writer can emit the 0x38-byte empty
form: first/last beat filled, count 0.

**PWV3** (0x18): `u32 1 (entry bytes), u32 num_entries, u32 0x00960000`,
then 1 byte/column, **150 columns/second** (0x96; num = ceil(duration×150)),
same 5-bit height + 3-bit whiteness packing. The big scrolling waveform.

**PWV5** (0x18): `u32 2, u32 num (same 150/s), u32 0x00960305`, then u16be
per column: bits 15–13 red, 12–10 green, 9–7 blue, 6–2 height, 1–0 zero
(DS). Color scrolling waveform.

**PWV4** (0x18): `u32 6, u32 1200, u32 0`, then 1200 × 6 bytes (color
preview; internal byte semantics undocumented — copy or approximate;
players only need plausible imagery).

**PWV6** (0x14): `u32 3, u32 1200`, then 1200 × 3 bytes — 3-band preview
(CDJ-3000): per column low/mid/high band levels (observed 0..~0x7f).

**PWV7** (0x18): `u32 3, u32 num (150/s), u32 0x00960000`, then 3
bytes/column — 3-band scrolling waveform.

**PWVC [new]** (0x0e, not in DS/kaitai): 20-byte section: `u16 0, u16 0x63,
u16 0x64, u16 0x120` observed constant-ish — appears to be 3-band
color/scale metadata. Copy verbatim.

**PSSI** (song structure/phrases): absent on this stick (no phrase
analysis); documented in DS with XOR masking. Optional — players cope
without it.

### 5.2 What players need

CDJ-2000NXS-era: .DAT only (PQTZ beatgrid, PWAV/PWV2, PCOB). nxs2/CDJ-3000:
.EXT for color waveforms + PCO2 (hot cue colors, comments). CDJ-3000 also
reads .2EX for 3-band display. `analyze_path` in the pdb must point at the
.DAT; players derive .EXT/.2EX by extension swap.

---

## 6. Artwork

19 dirs of 20 ids each: id N lives in dir `%05d` = (N−1)/20 + 1. Four files
per id: `aN.jpg` 80×80, `aN_m.jpg` 240×240, `bN.jpg`, `bN_m.jpg` —
**a and b are byte-identical**; export.pdb artwork rows reference `aN.jpg`,
exportLibrary.db `image.path` references `bN.jpg`. (377 ids referenced here.)

---

## 7. Settings & misc files

- `MYSETTING.DAT` (148 B), `MYSETTING2.DAT` (148 B), `DJMMYSETTING.DAT`
  (160 B), `DEVSETTING.DAT` (140 B): DS-documented fixed layout — u32 0x60,
  32-byte vendor ("PIONEER"/"PIONEER DJ"/"PioneerDJ"), 32-byte software
  ("rekordbox"), 32-byte version ("0.001"/"1.000"; DEVSETTING carries the
  rekordbox version "7.2.2"), u32 payload len, payload, u16 CRC16. Players
  read DJ preferences (waveform color, sync mode…) from these; optional for
  library browsing.
- `RBFLTR.DAT` [new]: written by the *player* (not rekordbox): `"FMAI"` header
  (len_header 0x74, len_file), 32-byte vendor "AlphaTheta Co.", 32-byte model
  "XDJ-AZ", 32-byte firmware "1.23", u32 count, then `FCND` sections
  (filter conditions). Ignore on write.
- `djprofile.nxs`: DJ profile (name "Kai Lazarov" + ids). Player-written.
- `PIONEER/CDP/`, `log/`, `.CacheData/`, `TrashBox/`: player/rekordbox litter.

---

## 8. Open questions

1. PQT2 per-beat u16 payload semantics (empty form is a valid write).
2. PWV4 6-byte column internals; PWVC field meanings.
3. Track-row constants 0x2FDB @0x18 / 0x8F45 @0x1a (same in every row —
   write verbatim; possibly a format/version cookie).
4. Table-19 row bytes 0x19 0x1e between the date and dbVersion strings.
5. PCPT `status` on rb7 hot cues is 0 (DS documents 4 for active loops).
6. Whether modern players verify exportExt.pdb / the DLP `cue` table at all.

## 9. Phase 5 writer checklist (deltas vs the skill)

- 20 tables, fixed order, sentinel-page scheme (§2.1); playlists into
  export.pdb tables 7/8 **and** exportLibrary.db for full device coverage.
- Track row: constants 0x24 magic / 0xC0700 @0x04 / 0x2FDB / 0x8F45 / 41
  @0x56 / 3 @0x5c, "ON"/"ON" kuvo+autoload strings, real fileType @0x5a,
  fresh random 28-bit ids @0x14, counters "1"/"1"/"" in slots 2/3/4.
- Keys: write Camelot strings straight from the catalog (id2 = id).
- Colors: always the fixed 8. menuItem/category/sort: copy §2.7 verbatim.
- ANLZ: emit .DAT + .EXT (+ .2EX for CDJ-3000 polish) with the exact section
  orders of §5; empty PCOB/PCO2/PQT2 forms are valid; PPTH/paths UTF-16BE.
- Artwork: write a/b file pairs + _m thumbnails, 20 ids per dir.
- FAT32 + MBR; `/Contents` and `/PIONEER` casing as in §1.
