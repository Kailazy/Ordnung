# Ordnung

A DJ music library for macOS that does its own analysis and writes USB sticks
your CDJs can read. No rekordbox required.

Point it at your music folder and it builds a catalog, works out the BPM,
beatgrid, musical key (in Camelot), waveform and loudness of every track, and
keeps it all in one place. When it's time to play, pick your crates and export
a stick.

## Download

**[Download Ordnung for macOS](../../releases/latest/download/Ordnung-macos-universal.dmg)**
— one `.dmg` for both Apple Silicon and Intel Macs. Requires macOS 11 or
newer. All versions are on the [Releases](../../releases) page.

1. Open the `.dmg` and drag **Ordnung** into **Applications**.
2. Double-click Ordnung. macOS will say it can't verify the app — click **Done**.
3. Open **System Settings → Privacy & Security**, scroll down, and click
   **Open Anyway** next to the Ordnung message. Confirm once more.

That's a one-time step. From then on Ordnung opens like any other app, and it
tells you in-app when a newer version is out.

<details>
<summary>Why the extra step, and the Terminal shortcut</summary>

Ordnung isn't yet notarized with Apple, so Gatekeeper treats it as an app from
an unidentified developer the first time it runs. If you'd rather skip the
Settings trip, run this once after copying the app to Applications:

```bash
xattr -dr com.apple.quarantine /Applications/Ordnung.app
```
</details>

Nothing else to install. Ordnung is a single self-contained app: no Homebrew,
no runtimes, no rekordbox.

## What it does

- **Library.** Scan a folder of music into a catalog and rescan whenever you
  add files. Search, sort and filter by anything, and see every track's
  waveform in the player.
- **Analysis.** BPM, beatgrid, Camelot key, waveform and loudness are computed
  by Ordnung itself, offline, and cached so they only run once.
- **Cues and loops.** Eight hot cues, memory cues and a beat loop on the player
  bar. Everything you set rides along on the next USB export.
- **Liked songs and crates.** Heart a song anywhere it appears, and organise
  your sets in crates.
- **Tracklists.** Paste a mix tracklist and every line gets matched to a record.
- **Vinyl.** Sync your Discogs collection and wantlist, browse each record's
  sheet, and listen to what you only own on wax through the built-in
  mini-player. Dig outward from a record through its artists and labels, let
  the radio dig for you, and watch saved sellers for wantlist records in stock.
- **USB sticks.** Plug in a rekordbox stick and browse it, play from it, pull
  tracks into your library, and edit its playlists, cues and beatgrids in
  place. The originals are backed up on the stick before the first edit.
- **USB export — BETA, in the works.** Writing a complete native rekordbox
  stick from your library (`export.pdb`, analysis files, cues, playlists) is
  built and being tested on real CDJs, but it is not in the downloadable
  builds yet. It will switch on once it has proven itself on more hardware.

## Converting audio

The **Convert** action (turning a file into another format) runs on
[ffmpeg](https://ffmpeg.org). You don't install it: the first time Ordnung
opens it quietly downloads its own copy, about 30 MB, into its data folder
and keeps it there across app updates. It only downloads again when a new
Ordnung version ships with a newer ffmpeg. If the download fails, Settings →
Conversion shows why and has a Download button.

## Discogs

The vinyl features talk to Discogs and need a free personal access token.
Create one at [discogs.com/settings/developers](https://www.discogs.com/settings/developers)
and paste it into **Settings → Discogs** inside the app.

## Where your data lives

Everything Ordnung knows is kept in `~/.ordnung/`: the catalog database, the
analysis cache and your settings. Your music files are never modified unless
you explicitly ask for a tag write or an in-place conversion, and anything
deleted goes to the Trash.

## Contributing

Want to build from source, hack on the engine or cut a release? See
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

MIT.
