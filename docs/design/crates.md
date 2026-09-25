# Crates — sets of songs on records, the vinyl side's playlists

**Status:** built (2026-09-23, v0.147.0); tags added 2026-09-25 (v0.154.0, §5). `crates` + `crate_songs` tables
(schema v22) with `Catalog::create_crate_set` / `list_crate_sets` /
`rename_crate_set` / `delete_crate_set` / `add_crate_songs` /
`remove_crate_song` / `list_crate_songs`; the GUI's `crates.rs` (the crate
view, `add_to_crate_menu`, `App::add_songs_to_crate`), the shared song
table `song_rows.rs`, the sidebar rows `sidebar::draw_crate_rows`, and the
`DraggedSongs` drag payload every song row sets.
**Target surface:** Ordnung GUI, vinyl side; engine in `ordnung-core`.
**One line:** a crate is a named set of songs as they sit on records, put
together by dragging songs in from a record's sheet, the liked songs or a
tracklist, and read back as the records to bring or as the songs.

## 1. Why

A playlist holds library tracks and answers "what do I play from the
laptop". A DJ who plays records needs the other list: which records to
pull from the shelf for a night, and which songs on each. The crate of
liked songs (`liked-songs.md`) already keeps songs that aren't files, one
set for everything liked; a crate is the same kind of row, in a named set
of the user's own making, so a gig's selection is a thing that can be
built, read and taken to the shelf.

## 2. The model

One song model for every set of songs that aren't tracks: `SongPin` (the
struct the liked songs were, renamed with the crates), the song plus the
record it was met on, keyed by `catalog::song_key`. Liked songs are one
table of pins; each crate is a set of pins keyed by `(crate_id, song_key)`,
so a song is in a crate once however many times it is dropped, and the same
song can be in several crates. `CrateSet` carries the counts the sidebar
shows (songs, distinct records). `LikeSpec` in the GUI is what a row hands
over whether it is being liked, dragged or added through a menu.

## 3. Surfaces

- Sidebar: **CRATES** under the Vinyl tile, in either layout (vinyl leading
  or the digital library leading), with the same caption-plus-"+" header
  the playlists have (`sidebar::list_header`) and the same row shape
  (`playlist_row`, inline rename through the one `rename_row` helper). A
  row is a drop target for `DraggedSongs`; its menu renames or deletes.
- Getting songs in: every song row that isn't a library track drags — a
  record sheet's row, a liked song, a tracklist line, a crate's own song
  — and carries a `DraggedSongs` payload with a "N songs" chip
  (`ui::drag_chip`, shared with the track drag). Each of those rows also
  has **Add to crate ▸** in its context menu (`crates::add_to_crate_menu`).
  The open crate view is a drop target too.
- The crate view (`LibraryView::CrateSet`): the name and counts, a
  Records / Songs segmented control. **Records** groups the songs by
  release (`group_by_record`, songs met without a record last): cover,
  the record, the songs the crate takes from it (`A1 Title · B2 Title`),
  OWNED / WANT from the shelves and FILES when every song is a library
  track; click opens the sheet, the menu takes the whole record out.
  **Songs** is the shared table (`App::song_rows`), the Liked view's rows
  with a ✕ beside the heart. The top bar's count follows the layout.
- The digital-library toolbar (Add songs, Analyze) hides on a crate as it
  does on the vinyl wall; the inspector doesn't apply.

## 4. Reuse

The Liked view was cut down to the header and `song_rows`; opening a
record, playing the file, searching Discogs, liking and adding to a crate
are one `SongAct` applied by `apply_song_act` for both sets. The sidebar's
three inline-rename twins (playlist, device, crate) resolve through one
`rename_row`. `CLAUDE.md` now states the rule this followed: look for the
component or engine that already does it before adding one.

## 5. Tags (2026-09-25, v0.154.0)

A tag is a crate of another kind: `crates.kind` (schema v23, `CrateKind`
in `model`) says whether a set is a **crate** (a selection for a night)
or a **tag** (a word marked on songs: "dub", "muddy", "sunrise",
"melodic"). Both live in the same two tables and go through the same
catalog methods; `create_crate_set` takes the kind and
`crate_memberships(kind)` reads every `(set, song key)` pair of one kind,
the one query behind "which tags does this song carry?"
(`App::song_tags`, a map from song key to tag ids, reloaded with the
sets).

- Sidebar: **TAGS** under the playlist tree, the same header, rows,
  inline rename and drop target as CRATES (`draw_crate_rows` takes the
  kind; the tag glyph instead of the package). Dropping songs on a tag
  marks them.
- Marking: every song menu (a library row, the shared song table, a
  record sheet's row, a tracklist line) has **Tags ▸**
  (`crates::tag_menu`), a check per tag; checking marks, unchecking
  unmarks (`App::set_tag`, which adds to or removes from the set). In the
  library the menu acts on the whole selection, and a tag is checked when
  every selected song carries it. The inspector shows a Tags line with
  the same menu. Library tracks are keyed the way the library index keys
  them (`crates::track_song_key`) and carry the same `LikeSpec` a like
  does (`LikeSpec::from_track`, now shared with the inspector's heart).
- Reading: the library table has a **Tags** column (`dub, dark`; sorts
  and filters like any text column, so typing a tag in the column filter
  is that tag's view of the library); the song table shows the tags after
  the artist; a tag in the sidebar opens through the crate view, on its
  Songs layout by default (the layout is remembered per kind).

## 6. Open

- Reordering songs inside a crate (they keep the order they went in).
- A crate's records as a Discogs wantlist push, or a text export like a
  playlist's track list.
- Liking from a crate's Records row (the songs, not the record).
