//! Split out of `main.rs`; part of the GUI `App`.
use super::*;

pub(crate) fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    if t.is_empty() {
        None
    } else {
        Some(t.to_string())
    }
}

pub(crate) fn short<'a>(s: &'a str, fallback: &'a str) -> &'a str {
    if s.is_empty() {
        fallback
    } else {
        s
    }
}

/// The convert-dialog seed values from the user's saved preferences: the target
/// format (falling back to AIFF for an empty/unknown key), the prefilled bitrate
/// text, the default output folder, and the in-place flag. One place so every
/// convert entry point (single, batch, toolbar) opens with the same defaults.
pub(crate) fn convert_defaults(cfg: &Config) -> (Format, String, Option<PathBuf>, bool) {
    (
        format_from_key(&cfg.convert_format).unwrap_or(Format::Aiff),
        cfg.convert_bitrate_kbps.clone(),
        cfg.convert_out_dir.clone(),
        cfg.convert_in_place,
    )
}

/// Stable lowercase key for a convertible target format, used to persist the
/// user's default convert format in `Config`. `Other` has no key (empty string).
pub(crate) fn format_key(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "mp3",
        Format::Aac => "aac",
        Format::Wav => "wav",
        Format::Aiff => "aiff",
        Format::Flac => "flac",
        Format::Other => "",
    }
}

/// Parse a persisted config format key back to a `Format`; unknown/empty → `None`.
pub(crate) fn format_from_key(k: &str) -> Option<Format> {
    match k {
        "mp3" => Some(Format::Mp3),
        "aac" => Some(Format::Aac),
        "wav" => Some(Format::Wav),
        "aiff" => Some(Format::Aiff),
        "flac" => Some(Format::Flac),
        _ => None,
    }
}

pub(crate) fn format_label(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "MP3",
        Format::Aac => "AAC (M4A)",
        Format::Wav => "WAV",
        Format::Aiff => "AIFF",
        Format::Flac => "FLAC",
        Format::Other => "—",
    }
}

pub(crate) fn default_bitrate_hint(f: Format) -> &'static str {
    match f {
        Format::Mp3 => "320",
        Format::Aac => "256",
        _ => "",
    }
}

/// Reveal a file in macOS Finder, selecting it in its containing folder.
/// Best-effort: a spawn failure is ignored — this is a convenience shortcut, not
/// a catalog operation, and never touches the file itself.
pub(crate) fn reveal_in_finder(path: &Path) {
    let _ = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn();
}

/// Open a URL in the user's default browser. Best-effort, like `reveal_in_finder`:
/// a spawn failure is ignored — this is a convenience shortcut, not a catalog op.
pub(crate) fn open_url(url: &str) {
    let _ = std::process::Command::new("open").arg(url).spawn();
}

/// Names of the apps that currently have files open on `vol`, via `lsof`
/// (field output, so command names aren't truncated). Deduplicated in first
/// seen order; our own process reads as "Ordnung (this app)" so the eject
/// message can point at, say, a track still loaded in the player. Empty on
/// any failure or when nothing holds the volume.
pub(crate) fn volume_users(vol: &Path) -> Vec<String> {
    let Ok(out) = std::process::Command::new("lsof")
        .args(["-Fc", "+f", "--"])
        .arg(vol)
        .output()
    else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some(name) = line.strip_prefix('c') else {
            continue;
        };
        let name = if name == "Ordnung" {
            "Ordnung (this app)".to_string()
        } else {
            name.to_string()
        };
        if !name.is_empty() && !names.contains(&name) {
            names.push(name);
        }
    }
    names
}

/// Join names for prose: "Music", "Music and Finder", "Music, Finder and X".
fn join_and(names: &[String]) -> String {
    match names {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// Turn a failed `diskutil eject` into a message a non-technical user can act
/// on. `raw` is diskutil's own output (jargon like "dissented by PID 501");
/// `users` is who `lsof` says still holds files open on the volume. The common
/// case by far is "some app is still using the drive", so that's said plainly,
/// naming the apps when known.
pub(crate) fn eject_failure_message(name: &str, raw: &str, users: &[String]) -> String {
    let raw_lc = raw.to_lowercase();
    let busy = !users.is_empty()
        || raw_lc.contains("in use")
        || raw_lc.contains("busy")
        || raw_lc.contains("dissent");
    if busy {
        if users.is_empty() {
            format!(
                "Can't eject {name} yet: another app is still using it. Close any \
                 windows or apps showing files from the drive, then click Eject again."
            )
        } else {
            let apps = join_and(users);
            let verb = if users.len() == 1 { "is" } else { "are" };
            format!(
                "Can't eject {name} yet: {apps} {verb} still using it. \
                 Quit or close {} and click Eject again.",
                if users.len() == 1 { "that app" } else { "those apps" }
            )
        }
    } else {
        format!(
            "Couldn't eject {name}. Wait a few seconds and try again; keep it \
             plugged in until you see \"Safe to unplug\"."
        )
    }
}

#[cfg(test)]
mod eject_msg_tests {
    use super::*;

    #[test]
    fn busy_messages_name_the_apps() {
        let m = eject_failure_message("EYEBAGS", "", &["Music".into()]);
        assert!(m.contains("Music is still using it"), "{m}");
        let m = eject_failure_message("EYEBAGS", "", &["Music".into(), "Finder".into()]);
        assert!(m.contains("Music and Finder are still using it"), "{m}");
        // diskutil says busy but lsof found nothing: still explained plainly.
        let m = eject_failure_message(
            "EYEBAGS",
            "Volume EYEBAGS on disk4s1 failed to unmount: dissented by PID 501",
            &[],
        );
        assert!(m.contains("another app is still using it"), "{m}");
        assert!(!m.contains("dissented"), "no jargon: {m}");
    }

    #[test]
    fn other_failures_stay_calm_and_jargon_free() {
        let m = eject_failure_message("EYEBAGS", "Failed to find disk /Volumes/EYEBAGS", &[]);
        assert!(m.contains("Couldn't eject EYEBAGS"), "{m}");
        assert!(!m.to_lowercase().contains("disk4"), "{m}");
    }
}

/// Build the free-text query for a Discogs release search from a track's tags.
/// Joins artist with album (preferred) or title so the search lands on the right
/// release even when we have no exact release id on file.
/// Format a track as a Soulseek search query: `Artist – Title`. Falls back to
/// whichever field is present when one is empty, so a query is never just a bare
/// separator. Mirrors how DJs hand-type searches into the Soulseek client.
pub(crate) fn soulseek_query(artist: &str, title: &str) -> String {
    match (artist.trim(), title.trim()) {
        ("", "") => String::new(),
        ("", t) => t.to_string(),
        (a, "") => a.to_string(),
        (a, t) => format!("{a} - {t}"),
    }
}

pub(crate) fn discogs_search_query(artist: &str, album: &str, title: &str) -> String {
    let release = if album.trim().is_empty() {
        title.trim()
    } else {
        album.trim()
    };
    [artist.trim(), release]
        .iter()
        .filter(|s| !s.is_empty())
        .cloned()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The Discogs web URL to open for a track. Deep-links to the exact release page
/// when one was fetched (`release_id` from a prior artwork run); otherwise opens
/// a Discogs release search seeded with `query`.
pub(crate) fn discogs_url(release_id: Option<&str>, query: &str) -> String {
    match release_id.map(str::trim).filter(|s| !s.is_empty()) {
        Some(id) => format!("https://www.discogs.com/release/{id}"),
        None => format!(
            "https://www.discogs.com/search/?type=release&q={}",
            percent_encode(query)
        ),
    }
}

/// Minimal RFC-3986 percent-encoding for a query value: keep the unreserved set,
/// `%XX`-encode everything else (spaces included). Enough for a Discogs search
/// `q=` parameter; we don't pull in a URL crate for one call site.
pub(crate) fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discogs_url_deep_links_when_release_known() {
        assert_eq!(
            discogs_url(Some("249504"), "ignored"),
            "https://www.discogs.com/release/249504"
        );
        // Whitespace-only id is treated as "unknown" → search fallback.
        assert!(discogs_url(Some("  "), "Daft Punk Discovery").contains("/search/"));
    }

    #[test]
    fn discogs_url_searches_when_no_release() {
        assert_eq!(
            discogs_url(None, "Daft Punk Discovery"),
            "https://www.discogs.com/search/?type=release&q=Daft%20Punk%20Discovery"
        );
    }

    #[test]
    fn search_query_prefers_album_then_title() {
        assert_eq!(
            discogs_search_query("Daft Punk", "Discovery", "One More Time"),
            "Daft Punk Discovery"
        );
        // Falls back to title when album is blank…
        assert_eq!(
            discogs_search_query("Daft Punk", "  ", "One More Time"),
            "Daft Punk One More Time"
        );
        // …and drops empty parts entirely.
        assert_eq!(discogs_search_query("", "", "Untitled"), "Untitled");
    }

    #[test]
    fn percent_encode_escapes_reserved_chars() {
        assert_eq!(percent_encode("a b&c"), "a%20b%26c");
        assert_eq!(percent_encode("A-Z_0.9~"), "A-Z_0.9~");
    }
}
