# Liked songs — a crate for songs met outside the library

**Status:** built (2026-09-18, v0.129.0). `liked_songs` table (schema v19)
with `Catalog::like_song` / `unlike_song` / `list_liked_songs` /
`library_song_keys` and the key rule `catalog::song_key`; the GUI's
`liked.rs` (the `LikeSpec`, `App::toggle_like`, the Liked view) and the
`ui::button::like_mark` component on every song row that isn't a track.
**Target surface:** Ordnung GUI, both sides; engine in `ordnung-core`.
**One line:** a + on every song you meet on a record's sheet, a mix
tracklist or the radio puts it in one crate, so each record says which of
its songs you like and the crate says which of them you still have to get.

## 1. Why

The vinyl side shows songs the library doesn't hold: a record's tracklist
in the sheet, the lines of a pasted mix, the songs on the radio. Liking one
of those had nowhere to go: playlists hold library tracks only. The crate
holds the song itself (artist and title), with the record it was met on as
a pin, and answers two questions: which songs on this record do I like, and
which liked songs are not in my library yet (the ones to download).

## 2. The key

A song is one row however many records carry it: the key is the artist and
title normalized the way tracks are linked to records (`norm_match`). A
title that names nothing ("Untitled", "Track 2", "B1") can't tell one song
on a record from the next, so for those the key also carries the release
and position. A white label's four Untitled sides are four songs.

## 3. Surfaces

- Record sheet rows, tracklist rows, radio rows: the like mark at the
  row's edge, "+" until liked, a pink heart after. The mark is registered
  after the row's own hit target so it takes the click.
- The Liked view (sidebar, under Library, shown while the crate holds
  something): cover, song, record and position, FILE or TO GET, the heart
  to unlike. Click opens the record's sheet with the song's row lit; the
  context menu plays the library file, searches Discogs, or unlikes.
  "Still to get" narrows the crate to the songs no library track is.
- In the library: a liked song counts as had when it was liked from a row
  that already knew its file, or when a track's artist and title key the
  same way.

## 4. Open

- A "download" hand-off from TO GET rows (Soulseek, see
  soulseek-acquisition.md).
- Liking from the search popup's vinyl hits.
