//! Tracklist match: turn a pasted mix tracklist into lines, and rank the
//! Discogs releases a search turned up for each line.
//!
//! Pure functions, no network. The paste is whatever a DJ copies off
//! 1001tracklists, a SoundCloud description, MixesDB or a YouTube comment,
//! so the parser is forgiving about ordinals, timestamps, dash variants and
//! trailing `[Label]` / `(Label)` hints, and honest about what it can't read
//! (`ID - ID` lines stay IDs, headings stay noise). The scorer works only on
//! what a search hit already carries, so ranking spends no requests; the
//! verification step that confirms a track is really on the record lives
//! with the caller, which holds the release cache. See
//! `docs/design/tracklist-match.md`.

use crate::discogs::{strip_original_mix, ReleaseCandidate};

/// What one pasted line turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// A song: artist and/or title read off the line.
    Track,
    /// An unidentified song the tracklist author marked as such (`ID - ID`,
    /// `Unknown - ?`). Kept in position, never searched.
    Id,
    /// Not a song: a heading, a rule, a bare timestamp, a blank line.
    Noise,
}

impl LineKind {
    /// Stable key for storage.
    pub fn key(self) -> &'static str {
        match self {
            LineKind::Track => "track",
            LineKind::Id => "id",
            LineKind::Noise => "noise",
        }
    }

    /// Inverse of [`LineKind::key`]; unknown keys read as noise.
    pub fn from_key(key: &str) -> Self {
        match key {
            "track" => LineKind::Track,
            "id" => LineKind::Id,
            _ => LineKind::Noise,
        }
    }
}

/// One line of a pasted tracklist, parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TracklistLine {
    /// 1-based, counting track and ID lines only (noise takes no number).
    pub position: usize,
    /// The line as pasted, trimmed, for display and re-parse.
    pub raw: String,
    /// Seconds into the mix, from a leading `[12:34]` / `1:02:03` / `12:34`.
    pub timestamp: Option<u32>,
    pub artist: Option<String>,
    pub title: Option<String>,
    /// A trailing `[Label]` / `(Label)` / ` - Label` on the line.
    pub label_hint: Option<String>,
    /// A trailing `[ENV 006]`-style token: a catalog number, which names one
    /// pressing outright.
    pub catno_hint: Option<String>,
    pub kind: LineKind,
}

/// How sure the matcher is that a line's chosen release carries the track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Confidence {
    /// No candidate at all.
    None,
    /// Something came back, but nothing on the hit ties it to the line.
    Unsure,
    /// Artist and title agree with the hit.
    Likely,
    /// A catalog number matched, artist + title + label agree, or the record's
    /// own tracklist was checked and carries the track.
    Sure,
}

impl Confidence {
    pub fn key(self) -> &'static str {
        match self {
            Confidence::None => "none",
            Confidence::Unsure => "unsure",
            Confidence::Likely => "likely",
            Confidence::Sure => "sure",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "sure" => Confidence::Sure,
            "likely" => Confidence::Likely,
            "unsure" => Confidence::Unsure,
            _ => Confidence::None,
        }
    }
}

/// One candidate's local score for a line — see [`rank_candidates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RankedCandidate {
    /// Index into the candidate slice that was ranked.
    pub index: usize,
    pub score: i32,
    pub confidence: Confidence,
}

// --- Parsing ---------------------------------------------------------------

/// Parse a whole paste into lines, in order. Noise lines are kept (so the
/// caller can show what was skipped) but take no position number.
pub fn parse_tracklist(text: &str) -> Vec<TracklistLine> {
    let mut out = Vec::new();
    let mut position = 0usize;
    let text = unfold(text);
    for raw in text.lines() {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let mut line = parse_line(raw);
        if line.kind != LineKind::Noise {
            position += 1;
            line.position = position;
        }
        out.push(line);
    }
    // A paste with no `Artist - Title` shape anywhere is a title-only list
    // (some SoundCloud descriptions): read every text line as a title rather
    // than throwing the whole paste away as noise.
    let any_track = out.iter().any(|l| l.kind == LineKind::Track);
    if !any_track {
        let mut position = 0usize;
        for l in out.iter_mut() {
            if l.kind == LineKind::Noise && looks_like_text(&l.raw) {
                let (ts, body) = take_timestamp(&normalise(&l.raw));
                let body = strip_ordinal(&body);
                if body.chars().any(|c| c.is_alphabetic()) {
                    position += 1;
                    l.position = position;
                    l.timestamp = ts;
                    l.title = Some(body.to_string());
                    l.kind = LineKind::Track;
                }
            }
        }
    }
    out
}

/// A paste whose line breaks were lost (a SoundCloud comment, a description
/// copied as one paragraph) runs its songs together on one line, each
/// introduced by a marker: `[04] Konduku - YTK [07] Dorisburg - …`, `12:30
/// Artist - Title 16:45 Next - …`, or `1. A - B 2. C - D`. Put the breaks
/// back: a line carrying two or more markers that follow song text is split
/// before each of them, when what stands before it is a whole `Artist -
/// Title`. A line with one marker (the usual one song per line) is left
/// alone, so a clock inside a title never splits it. Bare
/// bracketed minutes (`[04]`) on a split line become `[04:00]`, so the
/// clock pass reads them; elsewhere `[3]` stays an ordinal.
fn unfold(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 64);
    for (n, line) in text.lines().enumerate() {
        if n > 0 {
            out.push('\n');
        }
        out.push_str(&unfold_line(line));
    }
    out
}

/// A marker found on a line: where it starts and ends, and the bare minute
/// count when it's a `[mm]` that wants a `:00` appended.
struct Marker {
    start: usize,
    end: usize,
    bare_minutes: bool,
}

fn unfold_line(line: &str) -> String {
    let b = line.as_bytes();
    let mut markers: Vec<Marker> = Vec::new();
    let mut last_ordinal: Option<u32> = None;
    let mut i = 0;
    while i < b.len() {
        let at_word_start = i == 0 || b[i - 1].is_ascii_whitespace();
        let c = b[i];
        if c == b'[' || c == b'(' {
            let close = if c == b'[' { b']' } else { b')' };
            if let Some(len) = b[i + 1..].iter().position(|&x| x == close) {
                let inner = &line[i + 1..i + 1 + len];
                let bare = !inner.is_empty()
                    && inner.len() <= 3
                    && inner.bytes().all(|x| x.is_ascii_digit());
                if bare || parse_clock(inner).is_some() {
                    markers.push(Marker { start: i, end: i + 2 + len, bare_minutes: bare });
                    i += 2 + len;
                    continue;
                }
            }
        } else if c.is_ascii_digit() && at_word_start {
            let len = b[i..]
                .iter()
                .position(|&x| !(x.is_ascii_digit() || x == b':'))
                .unwrap_or(b.len() - i);
            let tok = &line[i..i + len];
            let end = i + len;
            let followed = end == b.len() || b[end].is_ascii_whitespace();
            if parse_clock(tok).is_some() && followed {
                markers.push(Marker { start: i, end, bare_minutes: false });
                i = end;
                continue;
            }
            // `1. ` / `1) ` ordinals, accepted only in sequence: the first
            // one seen, then each number one above the last accepted.
            let digits = tok.bytes().take_while(|x| x.is_ascii_digit()).count();
            if digits == len && digits <= 3 && end + 1 < b.len() && matches!(b[end], b'.' | b')') && b[end + 1].is_ascii_whitespace() {
                if let Ok(n) = tok.parse::<u32>() {
                    let in_sequence = match last_ordinal {
                        None => true,
                        Some(last) => n == last + 1,
                    };
                    if in_sequence {
                        last_ordinal = Some(n);
                        markers.push(Marker { start: i, end: end + 1, bare_minutes: false });
                    }
                }
            }
            i = end;
            continue;
        }
        i += 1;
    }
    // A marker breaks the line when a whole song sits before it: an
    // `Artist - Title` with words on both sides. Text that is only half a
    // song (`A - 4:20 Anthem`) keeps its clock. Once the line has broken,
    // any words since the last break count (`[30] (not in history) [35]`),
    // unless a bracket is still open, which puts the marker inside a title
    // (`C - D (Extended 4:20 Mix)`).
    let mut breaks = Vec::new();
    let mut seg_start = 0;
    for m in &markers {
        let seg = &line[seg_start..m.start];
        let words = seg.chars().any(|c| c.is_alphabetic());
        let open = seg.matches(['(', '[']).count() > seg.matches([')', ']']).count();
        if !open && (looks_like_song(seg) || (!breaks.is_empty() && words)) {
            breaks.push(m.start);
            seg_start = m.start;
        }
    }
    if breaks.len() < 2 {
        return line.to_string();
    }
    let mut out = String::with_capacity(line.len() + breaks.len() + markers.len() * 3);
    let mut pos = 0;
    for m in &markers {
        out.push_str(&line[pos..m.start]);
        if breaks.contains(&m.start) {
            out.push('\n');
        }
        if m.bare_minutes {
            // `[04]` → `[04:00]`
            out.push_str(&line[m.start..m.end - 1]);
            out.push_str(":00");
            out.push_str(&line[m.end - 1..m.end]);
        } else {
            out.push_str(&line[m.start..m.end]);
        }
        pos = m.end;
    }
    out.push_str(&line[pos..]);
    out
}

/// `Artist - Title` with letters on both sides of the dash.
fn looks_like_song(seg: &str) -> bool {
    let seg = normalise(seg);
    match seg.split_once(" - ") {
        Some((a, t)) => {
            a.chars().any(|c| c.is_alphabetic()) && t.chars().any(|c| c.is_alphabetic())
        }
        None => false,
    }
}

/// Parse one trimmed, non-empty line.
fn parse_line(raw: &str) -> TracklistLine {
    let mut line = TracklistLine {
        position: 0,
        raw: raw.to_string(),
        timestamp: None,
        artist: None,
        title: None,
        label_hint: None,
        catno_hint: None,
        kind: LineKind::Noise,
    };
    let text = normalise(raw);
    // A "Tracklist:" / "Tracklist - …" heading names the list, not a song.
    if is_heading(&text) {
        return line;
    }
    let (ts, rest) = take_timestamp(&text);
    line.timestamp = ts;
    let rest = strip_ordinal(&rest);
    // The timestamp may also come after the ordinal: `01. [12:34] Artist - …`.
    let (ts2, rest) = take_timestamp(&rest);
    if line.timestamp.is_none() {
        line.timestamp = ts2;
    }
    let rest = strip_ordinal(&rest).trim().to_string();
    if rest.is_empty() {
        return line;
    }
    let (body, label_hint, catno_hint) = split_hints(&rest);
    line.label_hint = label_hint;
    line.catno_hint = catno_hint;

    let (artist, title, extra_label) = split_artist_title(&body);
    if line.label_hint.is_none() {
        line.label_hint = extra_label;
    }
    match (artist, title) {
        (None, None) => {
            line.kind = LineKind::Noise;
        }
        (artist, title) => {
            let is_id = artist.as_deref().map_or(true, is_id_word)
                && title.as_deref().map_or(true, is_id_word);
            // A line that is nothing but an ID marker is an ID; a line with
            // one readable half is still worth a search.
            if is_id {
                line.kind = LineKind::Id;
            } else {
                line.kind = LineKind::Track;
                line.artist = artist.filter(|a| !is_id_word(a));
                line.title = title.filter(|t| !is_id_word(t));
            }
        }
    }
    line
}

/// Fold the typographic variants a paste carries into the plain forms the
/// rest of the parser expects: every dash to `-`, smart quotes to straight,
/// non-breaking spaces to spaces, leading bullets dropped.
fn normalise(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        out.push(match c {
            '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
            | '\u{2212}' => '-',
            '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}' | '\u{00b4}' | '\u{02bc}'
            | '\u{2032}' | '`' => '\'',
            '\u{201c}' | '\u{201d}' | '\u{201e}' | '\u{201f}' => '"',
            '\u{00a0}' | '\u{2007}' | '\u{202f}' | '\t' => ' ',
            c => c,
        });
    }
    let trimmed = out
        .trim_start_matches(|c: char| {
            matches!(c, '•' | '▪' | '◦' | '·' | '*' | '>' | '–' | '—') || c.is_whitespace()
        })
        .trim();
    // "w/ Artist - Title" is a mashup credit; the song after it is the one to
    // look up.
    let trimmed = trimmed
        .strip_prefix("w/ ")
        .or_else(|| trimmed.strip_prefix("W/ "))
        .or_else(|| trimmed.strip_prefix("with "))
        .unwrap_or(trimmed);
    trimmed.trim().to_string()
}

/// `Tracklist`, `Tracklist:`, `Tracklist - Deep Space`, `TRACKLIST (part 1)`.
fn is_heading(text: &str) -> bool {
    let l = text.trim().to_lowercase();
    match l.strip_prefix("tracklist") {
        Some(rest) => rest.is_empty() || rest.starts_with([':', ' ', '-', '(', '/']),
        None => false,
    }
}

/// Does a line carry enough letters to be a title in a title-only paste?
fn looks_like_text(raw: &str) -> bool {
    let letters = raw.chars().filter(|c| c.is_alphabetic()).count();
    letters >= 3 && !raw.trim_end_matches(':').eq_ignore_ascii_case("tracklist")
}

/// A leading `[12:34]`, `(1:02:03)`, `12:34` or `12:34 -`, as seconds plus
/// the remainder.
fn take_timestamp(s: &str) -> (Option<u32>, String) {
    let s = s.trim_start();
    let (inner, after) = if let Some(rest) = s.strip_prefix('[') {
        match rest.split_once(']') {
            Some((i, a)) => (i.trim(), a),
            None => return (None, s.to_string()),
        }
    } else if let Some(rest) = s.strip_prefix('(') {
        match rest.split_once(')') {
            Some((i, a)) => (i.trim(), a),
            None => return (None, s.to_string()),
        }
    } else {
        let end = s
            .find(|c: char| !(c.is_ascii_digit() || c == ':'))
            .unwrap_or(s.len());
        (&s[..end], &s[end..])
    };
    match parse_clock(inner) {
        Some(secs) => {
            let after = after.trim_start();
            let after = after
                .strip_prefix("- ")
                .or_else(|| after.strip_prefix("-"))
                .unwrap_or(after);
            (Some(secs), after.trim_start().to_string())
        }
        None => (None, s.to_string()),
    }
}

/// `12:34` → 754, `1:02:03` → 3723. Anything else is not a clock.
fn parse_clock(s: &str) -> Option<u32> {
    let parts: Vec<&str> = s.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    let mut secs = 0u32;
    for p in &parts {
        if p.is_empty() || p.len() > 2 && parts.len() == 3 || !p.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        secs = secs * 60 + p.parse::<u32>().ok()?;
    }
    Some(secs)
}

/// Drop a leading ordinal: `01.`, `1)`, `#1`, `[1]`, `01 -`, `1:` (a bare
/// number followed by a colon is an ordinal, not a clock, once the clock
/// pass has run).
fn strip_ordinal(s: &str) -> String {
    let s = s.trim_start();
    let hashed = s.starts_with('#');
    let s = s.strip_prefix('#').unwrap_or(s);
    let bracket = s.starts_with('[');
    let body = if bracket { &s[1..] } else { s };
    let digits = body.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits == 0 || digits > 3 {
        return s.to_string();
    }
    let mut rest = &body[digits..];
    if bracket {
        match rest.strip_prefix(']') {
            Some(r) => rest = r,
            None => return s.to_string(),
        }
    } else {
        // A number needs a separator after it: "01." / "1)" / "1:" / "01 -" /
        // "01 Artist". "2 Bad Mice - …" would lose its 2, so a bare number
        // is only an ordinal when followed by a dot, paren, colon or dash.
        if let Some(r) = rest.strip_prefix(['.', ')', ':']) {
            rest = r;
        } else {
            let after = rest.trim_start();
            if let Some(r) = after.strip_prefix("- ") {
                rest = r;
            } else if rest.starts_with(' ') && (hashed || s.split(" - ").count() >= 3) {
                // "01 Artist - Title - Label": leading number before a full
                // artist/title split, the way filenames carry it.
                rest = after;
            } else {
                return s.to_string();
            }
        }
    }
    rest.trim_start().to_string()
}

/// Peel trailing `[…]` / `(…)` tokens off the line into hints, leaving the
/// artist/title body. Parentheses that read as a mix descriptor stay on the
/// title: `(Original Mix)`, `(DJ Koze Remix)`, `(Pt. II)`.
fn split_hints(s: &str) -> (String, Option<String>, Option<String>) {
    let mut body = s.trim().to_string();
    let mut label = None;
    let mut catno = None;
    loop {
        let Some(close) = body.chars().last() else { break };
        let open = match close {
            ']' => '[',
            ')' => '(',
            _ => break,
        };
        let Some(start) = body.rfind(open) else { break };
        let inner = body[start + 1..body.len() - 1].trim().to_string();
        // A parenthesised label is a word or three (`(Kif)`, `(Running
        // Back)`); a long parenthesised tail is a subtitle and stays.
        let long = inner.split_whitespace().count() > 3;
        if inner.is_empty() || (open == '(' && (is_mix_descriptor(&inner) || long)) {
            break;
        }
        // Nothing before the bracket means the bracket is the whole line
        // (a bare "[12:34]" leftover): leave it.
        if body[..start].trim().is_empty() {
            break;
        }
        // `[Label - CAT 01]` carries both; `[CAT 01 - A1]` a number and the
        // position on it, which the search doesn't need.
        let (first, second) = match inner.split_once(" - ") {
            Some((a, b)) => (a.trim().to_string(), Some(b.trim().to_string())),
            None => (inner, None),
        };
        if looks_like_catno(&first) {
            if catno.is_none() {
                catno = Some(first);
            }
        } else {
            if label.is_none() {
                label = Some(first);
            }
            if let Some(s) = second.filter(|s| looks_like_catno(s)) {
                if catno.is_none() {
                    catno = Some(s);
                }
            }
        }
        body = body[..start].trim_end().to_string();
    }
    (body, label, catno)
}

/// Is a parenthesised tail part of the song's title rather than a hint?
fn is_mix_descriptor(inner: &str) -> bool {
    const WORDS: [&str; 24] = [
        "mix", "remix", "rmx", "edit", "version", "dub", "instrumental", "rework",
        "bootleg", "vip", "extended", "radio", "club", "acoustic", "live", "feat",
        "ft.", "ft ", "pt.", "pt ", "part", "vocal", "reprise", "cut",
    ];
    let l = inner.to_lowercase();
    WORDS.iter().any(|w| l.contains(w)) || l.chars().all(|c| c.is_ascii_digit())
}

/// A catalog number: letters and digits, ending in a digit, at most three
/// space-separated words, no lowercase run that reads as a word (`ALLV 001`,
/// `ENV006`, `KDJ-12`, `SS 003`). A label name (`Sound Signature`, `Kif`)
/// has no digits.
fn looks_like_catno(s: &str) -> bool {
    let s = s.trim();
    if s.len() > 20 || s.split_whitespace().count() > 3 {
        return false;
    }
    let has_digit = s.chars().any(|c| c.is_ascii_digit());
    let has_alpha = s.chars().any(|c| c.is_ascii_alphabetic());
    let ends_digit = s
        .chars()
        .rev()
        .find(|c| c.is_ascii_alphanumeric())
        .is_some_and(|c| c.is_ascii_digit());
    let plain = s
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '.' | '/' | '_'));
    // A year alone ("1994") is a year, not a catno.
    let bare_year = s.len() == 4 && s.bytes().all(|b| b.is_ascii_digit());
    has_digit && has_alpha && ends_digit && plain && !bare_year
}

/// Split the body into artist and title on the first ` - `; a third segment
/// is a label. `Artist "Title"` quoted shapes split on the quotes.
fn split_artist_title(body: &str) -> (Option<String>, Option<String>, Option<String>) {
    let body = body.trim();
    if body.is_empty() {
        return (None, None, None);
    }
    let parts: Vec<&str> = body.split(" - ").map(str::trim).collect();
    if parts.len() >= 2 {
        let artist = parts[0];
        // The last segment is a label when there are three or more and it is
        // short and plain; otherwise the title itself contained " - ".
        let (title, label) = if parts.len() >= 3 {
            let last = parts[parts.len() - 1];
            if last.split_whitespace().count() <= 3 && !is_mix_descriptor(last) {
                (parts[1..parts.len() - 1].join(" - "), Some(last.to_string()))
            } else {
                (parts[1..].join(" - "), None)
            }
        } else {
            (parts[1].to_string(), None)
        };
        return (non_empty(artist), non_empty(&title), label.filter(|l| !l.is_empty()));
    }
    // A dash glued to one side (`Leafar Legov- When The Morning Comes`,
    // `Artist -Title`) is still the separator when it's the only one.
    if let Some((a, tt)) = split_glued_dash(body) {
        return (non_empty(a), non_empty(tt), None);
    }
    // Artist "Title"
    if let Some(open) = body.find('"') {
        if let Some(close) = body[open + 1..].find('"') {
            let artist = body[..open].trim();
            let title = body[open + 1..open + 1 + close].trim();
            if !artist.is_empty() && !title.is_empty() {
                return (non_empty(artist), non_empty(title), None);
            }
        }
    }
    // "Artist: Title" is rare but real (radio show notes).
    if let Some((a, t)) = body.split_once(": ") {
        if !a.contains(' ') || a.split_whitespace().count() <= 4 {
            let t = t.trim();
            if !t.is_empty() && !a.trim().is_empty() {
                return (non_empty(a.trim()), non_empty(t), None);
            }
        }
    }
    (None, None, None)
}

/// The one `-` with a space on exactly one side and a word on the other;
/// `None` when there is none or more than one. A hyphen inside a word
/// (`Pin-1`, `e-mail`) has words on both sides and never counts.
fn split_glued_dash(body: &str) -> Option<(&str, &str)> {
    let chars: Vec<(usize, char)> = body.char_indices().collect();
    let mut found: Option<usize> = None;
    for k in 1..chars.len().saturating_sub(1) {
        if chars[k].1 != '-' {
            continue;
        }
        let (prev, next) = (chars[k - 1].1, chars[k + 1].1);
        let glued = (prev.is_alphanumeric() && next == ' ') || (prev == ' ' && next.is_alphanumeric());
        if glued {
            if found.is_some() {
                return None;
            }
            found = Some(chars[k].0);
        }
    }
    let at = found?;
    Some((&body[..at], &body[at + 1..]))
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim().trim_matches(|c| c == '"' || c == '\'');
    (!s.is_empty()).then(|| s.to_string())
}

/// Words a tracklist author uses for a song they couldn't identify.
fn is_id_word(s: &str) -> bool {
    let l = s.trim().to_lowercase();
    let l = l.trim_matches(|c: char| !c.is_alphanumeric());
    matches!(
        l,
        "id" | "" | "unknown" | "unknown artist" | "untitled" | "unreleased" | "n/a" | "na"
            | "tba" | "tbc" | "unknown title" | "id id"
    )
}

// --- Scoring ---------------------------------------------------------------

/// Fold a name for comparison: lowercase, accents stripped, a Discogs
/// `(2)` disambiguator dropped, a leading "the" dropped, punctuation
/// collapsed to single spaces.
pub fn fold_name(s: &str) -> String {
    let mut s = crate::catalog::fold_search(s.trim());
    // "artist (2)"
    if let Some(open) = s.rfind(" (") {
        let tail = &s[open + 2..];
        if tail.ends_with(')') && tail[..tail.len() - 1].bytes().all(|b| b.is_ascii_digit()) {
            s.truncate(open);
        }
    }
    let mut out = String::with_capacity(s.len());
    let mut space = true;
    for c in s.chars() {
        if c.is_alphanumeric() {
            out.push(c);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    let out = out.trim().to_string();
    out.strip_prefix("the ").map(str::to_string).unwrap_or(out)
}

/// Fold a song title for comparison: [`fold_name`] after the `(Original
/// Mix)`-style marker is dropped.
pub fn fold_title(s: &str) -> String {
    fold_name(strip_original_mix(s))
}

/// Is `artist` the credited artist on a hit? Exact fold, or the hit credits
/// several and one of them is ours ("A & B", "A / B", "A Feat. B").
fn artist_agrees(line_artist: &str, hit_artist: &str) -> bool {
    let want = fold_name(line_artist);
    if want.is_empty() {
        return false;
    }
    let have = fold_name(hit_artist);
    if have == want {
        return true;
    }
    let have_parts = credit_parts(hit_artist);
    if have_parts.iter().any(|p| *p == want) {
        return true;
    }
    // The line credits several too (`Cajmere ft Dajae`): the lead name
    // agreeing is enough, however the hit writes the rest.
    let want_parts = credit_parts(line_artist);
    want_parts.len() > 1
        && !want_parts[0].is_empty()
        && have_parts.first().is_some_and(|h| *h == want_parts[0])
}

/// The names in a credit, folded: `Kerri Chandler & Jerome Sydenham`,
/// `Dennis Ferrer Feat. Kerri Chandler`, `Cajmere ft Dajae`. Split on the
/// raw punctuation first (folding erases it), then on the joining words.
fn credit_parts(credit: &str) -> Vec<String> {
    credit
        .split([',', '&', '/', '+'])
        .flat_map(|part| {
            let folded = fold_name(part);
            [" feat ", " featuring ", " ft ", " and ", " vs ", " x "]
                .iter()
                .fold(vec![folded], |acc, sep| {
                    acc.into_iter()
                        .flat_map(|p| p.split(sep).map(|q| q.trim().to_string()).collect::<Vec<_>>())
                        .collect()
                })
        })
        .filter(|p| !p.is_empty())
        .collect()
}

/// Score every candidate for a line, best first. Uses only what the search
/// hit carries, so a call costs nothing. The caller decides which of the
/// survivors to commit (its own pressing rule) and whether to verify.
pub fn rank_candidates(line: &TracklistLine, cands: &[ReleaseCandidate]) -> Vec<RankedCandidate> {
    let mut out: Vec<RankedCandidate> = cands
        .iter()
        .enumerate()
        .map(|(index, c)| {
            let score = score_candidate(line, c);
            RankedCandidate {
                index,
                score,
                confidence: confidence_for(score),
            }
        })
        .collect();
    out.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.index.cmp(&b.index)));
    out
}

/// A catalog-number match is decisive; artist + title + label is sure;
/// artist + title is likely; anything else is unsure.
pub fn confidence_for(score: i32) -> Confidence {
    if score >= 110 {
        Confidence::Sure
    } else if score >= 80 {
        Confidence::Likely
    } else {
        Confidence::Unsure
    }
}

/// One candidate's score against one line. Weights: catno 100 (decisive),
/// artist 40, title equal 50 / contained 40, label 30, vinyl 5; an artist
/// that plainly disagrees (and isn't "Various") costs 25.
pub fn score_candidate(line: &TracklistLine, c: &ReleaseCandidate) -> i32 {
    let mut score = 0i32;
    let hit_artist = fold_name(&c.artist);
    let various = hit_artist == "various" || hit_artist == "various artists";
    if let Some(a) = line.artist.as_deref() {
        if artist_agrees(a, &c.artist) {
            score += 40;
        } else if !various && !hit_artist.is_empty() {
            score -= 25;
        }
    }
    if let Some(t) = line.title.as_deref() {
        let want = fold_title(t);
        let have = fold_title(&c.title);
        if !want.is_empty() {
            if have == want {
                score += 50;
            } else if have.contains(&want) || (want.len() >= 8 && want.contains(&have) && !have.is_empty()) {
                score += 40;
            }
        }
    }
    if let Some(l) = line.label_hint.as_deref() {
        let want = fold_name(l);
        let have = fold_name(&c.label);
        if !want.is_empty() && (have == want || have.starts_with(&want) || want.starts_with(&have) && !have.is_empty()) {
            score += 30;
        }
    }
    if let Some(cn) = line.catno_hint.as_deref() {
        if catno_key(cn) == catno_key(&c.catno) && !c.catno.trim().is_empty() {
            score += 100;
        }
    }
    if c.format.to_lowercase().contains("vinyl") {
        score += 5;
    }
    score
}

/// Catalog numbers compare without case, spaces or punctuation: `ENV 006`,
/// `env-006` and `ENV006` are one number.
pub fn catno_key(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

/// The query text a line sends to Discogs: artist and title, or whichever
/// half it has.
pub fn search_terms(line: &TracklistLine) -> (String, String) {
    (
        line.artist.clone().unwrap_or_default(),
        line.title
            .as_deref()
            .map(|t| strip_original_mix(t).to_string())
            .unwrap_or_default(),
    )
}

/// A name for a paste: its `Tracklist:` / title heading when it has one,
/// else `None` (the caller dates it).
pub fn suggested_name(text: &str) -> Option<String> {
    for raw in text.lines().take(3) {
        let l = normalise(raw);
        if l.is_empty() {
            continue;
        }
        let lower = l.to_lowercase();
        if let Some(rest) = lower
            .strip_prefix("tracklist:")
            .or_else(|| lower.strip_prefix("tracklist -"))
            .or_else(|| lower.strip_prefix("tracklist "))
        {
            let name = l[l.len() - rest.len()..].trim();
            return (!name.is_empty()).then(|| name.to_string());
        }
        // A first line with no separator and no clock is a heading.
        let parsed = parse_line(&l);
        if parsed.kind == LineKind::Noise && looks_like_text(&l) && l.len() <= 80 {
            return Some(l.trim_end_matches(':').to_string());
        }
        break;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(raw: &str) -> TracklistLine {
        parse_line(&normalise(raw))
    }

    fn at(raw: &str) -> (Option<String>, Option<String>) {
        let l = line(raw);
        (l.artist, l.title)
    }

    #[test]
    fn plain_ordinal_line() {
        let l = line("01. Metro Area - Miura");
        assert_eq!(l.kind, LineKind::Track);
        assert_eq!(l.artist.as_deref(), Some("Metro Area"));
        assert_eq!(l.title.as_deref(), Some("Miura"));
        assert_eq!(l.timestamp, None);
    }

    #[test]
    fn bracket_timestamp_en_dash_and_label_hint() {
        let l = line("[12:34] Theo Parrish – Solitary Flight [Sound Signature]");
        assert_eq!(l.timestamp, Some(754));
        assert_eq!(l.artist.as_deref(), Some("Theo Parrish"));
        assert_eq!(l.title.as_deref(), Some("Solitary Flight"));
        assert_eq!(l.label_hint.as_deref(), Some("Sound Signature"));
        assert_eq!(l.catno_hint, None);
    }

    #[test]
    fn long_clock_quoted_title_paren_label() {
        let l = line("1:02:03 Pépé Bradock \"Deep Burnt\" (Kif)");
        assert_eq!(l.timestamp, Some(3723));
        assert_eq!(l.artist.as_deref(), Some("Pépé Bradock"));
        assert_eq!(l.title.as_deref(), Some("Deep Burnt"));
        assert_eq!(l.label_hint.as_deref(), Some("Kif"));
    }

    #[test]
    fn parenthesised_subtitle_stays_on_title() {
        let (a, t) = at("DJ Sprinkles - Grand Central, Pt. I (Deep Into The Bowel Of House)");
        assert_eq!(a.as_deref(), Some("DJ Sprinkles"));
        assert_eq!(
            t.as_deref(),
            Some("Grand Central, Pt. I (Deep Into The Bowel Of House)")
        );
    }

    #[test]
    fn remix_credit_stays_on_title() {
        let l = line("Isolée - Beau Mot Plage (Freeform Reform)");
        // Not a listed mix word, but the label hint would be nonsense: keep
        // it a title when it has no digits and the line already has a label.
        assert_eq!(l.artist.as_deref(), Some("Isolée"));
        let l2 = line("Isolée - Beau Mot Plage (DJ Koze Remix) [Playhouse]");
        assert_eq!(l2.title.as_deref(), Some("Beau Mot Plage (DJ Koze Remix)"));
        assert_eq!(l2.label_hint.as_deref(), Some("Playhouse"));
        let _ = l;
    }

    #[test]
    fn third_segment_is_a_label() {
        let l = line("Moodymann - Shades Of Jae - KDJ");
        assert_eq!(l.artist.as_deref(), Some("Moodymann"));
        assert_eq!(l.title.as_deref(), Some("Shades Of Jae"));
        assert_eq!(l.label_hint.as_deref(), Some("KDJ"));
    }

    #[test]
    fn catno_in_brackets() {
        let l = line("Larry Heard - The Sun Can't Compare (Long Version) [ALLV 001]");
        assert_eq!(l.artist.as_deref(), Some("Larry Heard"));
        assert_eq!(l.title.as_deref(), Some("The Sun Can't Compare (Long Version)"));
        assert_eq!(l.catno_hint.as_deref(), Some("ALLV 001"));
        assert_eq!(l.label_hint, None);
    }

    #[test]
    fn mashup_prefix_keeps_its_own_artist() {
        let (a, t) = at("w/ Kerri Chandler - Rain");
        assert_eq!(a.as_deref(), Some("Kerri Chandler"));
        assert_eq!(t.as_deref(), Some("Rain"));
    }

    #[test]
    fn id_lines_are_ids() {
        for raw in ["ID - ID", "Unknown - ?", "??? - Untitled", "05. ID - ID", "[1:10:00] ID - ID"] {
            assert_eq!(line(raw).kind, LineKind::Id, "{raw}");
        }
        // One readable half is still a track.
        let l = line("ID - Rain");
        assert_eq!(l.kind, LineKind::Track);
        assert_eq!(l.artist, None);
        assert_eq!(l.title.as_deref(), Some("Rain"));
    }

    #[test]
    fn noise_lines_are_noise() {
        for raw in ["Tracklist:", "-----", "12:34", "", "Tracklist", "•"] {
            assert_eq!(parse_tracklist(raw).iter().filter(|l| l.kind != LineKind::Noise).count(), 0, "{raw}");
        }
    }

    #[test]
    fn tracklist_headings_are_noise_not_songs() {
        for raw in ["Tracklist: Deep Space Test", "Tracklist - Ben UFO", "TRACKLIST", "Tracklist (part 1)"] {
            assert_eq!(line(raw).kind, LineKind::Noise, "{raw}");
        }
        // But a real "Artist: Title" line still splits.
        assert_eq!(at("Moodymann: Shades Of Jae").0.as_deref(), Some("Moodymann"));
    }

    #[test]
    fn whole_paste_positions_skip_noise() {
        let text = "Tracklist:\n\n01. A - B\n02. ID - ID\n-----\n03. C - D [Label]\n";
        let lines = parse_tracklist(text);
        let tracks: Vec<(usize, LineKind)> = lines
            .iter()
            .filter(|l| l.kind != LineKind::Noise)
            .map(|l| (l.position, l.kind))
            .collect();
        assert_eq!(
            tracks,
            vec![(1, LineKind::Track), (2, LineKind::Id), (3, LineKind::Track)]
        );
        assert_eq!(lines.len(), 5);
    }

    #[test]
    fn one_line_paste_with_minute_stamps_is_unfolded() {
        let text = "[00] Mathew Jonson - Freedom Engine [04] Konduku - YTK [07] Dorisburg - Internet Tension [10] Prime Minister of Doom - Grand Finale [14] Dawn Razor - Locked [17] Atomic Moog - Static Flow [19] Azu Tiwaline - Nyctophilia [21] Wata Igarashi - Mood of the Machines, Pt. 1 [27] Atomic Moog - Planet [30] (not in history) [35] DJ Mastra - Nachtbullet [38] Planet Love - Scander [42] Corrie - Stars [45] Steve Barnes - Cosmic Sandwich [50] Neel - Deep Quarantine [54] D. Tiffany & Roza Terenzi - Lil Drummer Boy [57]";
        let lines = parse_tracklist(text);
        let tracks: Vec<&TracklistLine> = lines.iter().filter(|l| l.kind == LineKind::Track).collect();
        assert_eq!(tracks.len(), 15, "{lines:#?}");
        assert_eq!(tracks[0].artist.as_deref(), Some("Mathew Jonson"));
        assert_eq!(tracks[0].timestamp, Some(0));
        assert_eq!(tracks[1].artist.as_deref(), Some("Konduku"));
        assert_eq!(tracks[1].timestamp, Some(240));
        assert_eq!(tracks[7].title.as_deref(), Some("Mood of the Machines, Pt. 1"));
        assert_eq!(tracks[14].artist.as_deref(), Some("D. Tiffany & Roza Terenzi"));
        assert_eq!(tracks[14].timestamp, Some(54 * 60));
        // "(not in history)" and the closing "[57]" are skipped, not songs.
        assert_eq!(lines.iter().filter(|l| l.kind == LineKind::Noise).count(), 2);
    }

    #[test]
    fn one_line_paste_with_clocks_or_ordinals_is_unfolded() {
        let lines = parse_tracklist("0:00 A - B 12:30 C - D (Extended 4:20 Mix) 1:02:03 E - F");
        let artists: Vec<&str> = lines.iter().filter_map(|l| l.artist.as_deref()).collect();
        assert_eq!(artists, vec!["A", "C", "E"]);
        assert_eq!(lines[1].title.as_deref(), Some("D (Extended 4:20 Mix)"));
        assert_eq!(lines[2].timestamp, Some(3723));
        let lines = parse_tracklist("1. A - B 2. C - D 3. E - F (Vol. 2 Mix)");
        let titles: Vec<&str> = lines.iter().filter_map(|l| l.title.as_deref()).collect();
        assert_eq!(titles, vec!["B", "D", "F (Vol. 2 Mix)"]);
    }

    #[test]
    fn one_marker_per_line_never_splits() {
        // A clock inside a title, a duration at the end, a bracketed ordinal.
        let text = "01. A - 4:20 Anthem 5:32\n[3] C - D\n12:34 E - F [Label]\n";
        let lines = parse_tracklist(text);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].title.as_deref(), Some("4:20 Anthem 5:32"));
        assert_eq!(lines[1].artist.as_deref(), Some("C"));
        assert_eq!(lines[1].timestamp, None);
        assert_eq!(lines[2].timestamp, Some(754));
    }

    #[test]
    fn title_only_paste_reads_every_line_as_a_title() {
        let text = "Miura\nSolitary Flight\nDeep Burnt\n";
        let lines = parse_tracklist(text);
        assert!(lines.iter().all(|l| l.kind == LineKind::Track && l.artist.is_none()));
        assert_eq!(lines[2].title.as_deref(), Some("Deep Burnt"));
    }

    #[test]
    fn ordinal_variants() {
        assert_eq!(at("1) A - B").0.as_deref(), Some("A"));
        assert_eq!(at("#7 A - B").0.as_deref(), Some("A"));
        assert_eq!(at("[3] A - B").0.as_deref(), Some("A"));
        assert_eq!(at("12 - A - B").0.as_deref(), Some("A"));
        assert_eq!(at("01 A - B - Label").0.as_deref(), Some("A"));
        // A number that is part of the artist name survives.
        assert_eq!(at("2 Bad Mice - Bombscare").0.as_deref(), Some("2 Bad Mice"));
        assert_eq!(at("808 State - Pacific").0.as_deref(), Some("808 State"));
    }

    #[test]
    fn timestamp_then_ordinal_and_dash_after_clock() {
        let l = line("12:34 - Metro Area - Miura");
        assert_eq!(l.timestamp, Some(754));
        assert_eq!(l.artist.as_deref(), Some("Metro Area"));
        let l = line("01. [00:00] Metro Area - Miura");
        assert_eq!(l.timestamp, Some(0));
        assert_eq!(l.artist.as_deref(), Some("Metro Area"));
    }

    #[test]
    fn bullets_and_nbsp_are_stripped() {
        let l = line("• Metro\u{a0}Area — Miura");
        assert_eq!(l.artist.as_deref(), Some("Metro Area"));
        assert_eq!(l.title.as_deref(), Some("Miura"));
    }

    #[test]
    fn catno_detection() {
        for yes in ["ALLV 001", "ENV006", "KDJ-12", "SS 003", "PLAY 001"] {
            assert!(looks_like_catno(yes), "{yes}");
        }
        for no in ["Sound Signature", "Kif", "1994", "Original Mix", "Running Back Records"] {
            assert!(!looks_like_catno(no), "{no}");
        }
    }

    #[test]
    fn suggested_name_from_heading() {
        assert_eq!(
            suggested_name("Tracklist: Deep Space 2019\n01. A - B").as_deref(),
            Some("Deep Space 2019")
        );
        assert_eq!(
            suggested_name("Ben UFO @ Dekmantel 2016\n01. A - B").as_deref(),
            Some("Ben UFO @ Dekmantel 2016")
        );
        assert_eq!(suggested_name("01. A - B\n02. C - D"), None);
    }

    // --- scoring

    fn cand(artist: &str, title: &str, label: &str, catno: &str, format: &str) -> ReleaseCandidate {
        ReleaseCandidate {
            release_id: "1".into(),
            artist: artist.into(),
            title: title.into(),
            year: String::new(),
            label: label.into(),
            catno: catno.into(),
            country: String::new(),
            format: format.into(),
            thumb_url: String::new(),
            cover_image_url: String::new(),
            in_collection: 0,
            in_wantlist: 0,
        }
    }

    #[test]
    fn fold_name_drops_disambiguator_article_and_punctuation() {
        assert_eq!(fold_name("Lawrence (2)"), "lawrence");
        assert_eq!(fold_name("The Sun Can't Compare"), "sun can t compare");
        assert_eq!(fold_name("Pépé Bradock"), "pepe bradock");
        assert_eq!(fold_title("Miura (Original Mix)"), "miura");
    }

    #[test]
    fn artist_title_label_is_sure_artist_title_is_likely() {
        let l = line("Theo Parrish - Solitary Flight [Sound Signature]");
        let sure = cand("Theo Parrish", "Solitary Flight", "Sound Signature", "SS 011", "Vinyl, 12\"");
        let likely = cand("Theo Parrish", "Solitary Flight", "Peacefrog", "PF 1", "Vinyl");
        let album = cand("Theo Parrish", "Parallel Dimensions", "Ubiquity", "U 1", "CD");
        let wrong = cand("Moodymann", "Solitary Flight", "KDJ", "KDJ 1", "Vinyl");
        let ranked = rank_candidates(&l, &[album.clone(), wrong.clone(), likely.clone(), sure.clone()]);
        assert_eq!(ranked[0].index, 3);
        assert_eq!(ranked[0].confidence, Confidence::Sure);
        assert_eq!(ranked[1].index, 2);
        assert_eq!(ranked[1].confidence, Confidence::Likely);
        assert!(ranked[2].confidence <= Confidence::Unsure);
        assert!(score_candidate(&l, &wrong) < score_candidate(&l, &album));
    }

    #[test]
    fn catno_wins_outright_and_various_is_neutral() {
        let l = line("Larry Heard - The Sun Can't Compare [ALLV 001]");
        let comp = cand("Various", "Deep House Vol. 3", "Some Label", "SL 3", "Vinyl");
        let by_catno = cand("Larry Heard", "The Sun Can't Compare", "Alleviated", "allv-001", "Vinyl");
        assert_eq!(confidence_for(score_candidate(&l, &by_catno)), Confidence::Sure);
        assert!(score_candidate(&l, &comp) >= 0);
        assert_eq!(confidence_for(score_candidate(&l, &comp)), Confidence::Unsure);
    }

    #[test]
    fn multi_artist_credit_agrees() {
        assert!(artist_agrees("Kerri Chandler", "Kerri Chandler & Jerome Sydenham"));
        assert!(artist_agrees("Kerri Chandler", "Dennis Ferrer Feat. Kerri Chandler"));
        assert!(!artist_agrees("Kerri Chandler", "Kerri"));
        assert!(artist_agrees("Cajmere ft Dajae", "Cajmere Featuring Dajae"));
        assert!(artist_agrees("Cajmere ft Dajae", "Cajmere"));
        assert!(!artist_agrees("Cajmere ft Dajae", "Dajae"));
    }

    #[test]
    fn glued_dash_accent_and_bracketed_position() {
        let l = line("38:48 Leafar Legov- When The Morning Comes");
        assert_eq!(l.artist.as_deref(), Some("Leafar Legov"));
        assert_eq!(l.title.as_deref(), Some("When The Morning Comes"));
        let l = line("Artist -Title");
        assert_eq!(l.artist.as_deref(), Some("Artist"));
        // Two glued dashes are ambiguous; a hyphenated word is not a dash.
        assert_eq!(line("A- B- C").kind, LineKind::Noise);
        assert_eq!(at("Pin-1 - Untitled").0.as_deref(), Some("Pin-1"));
        let l = line("54:40 Moodymann - It´s 2 Late 4 U And Me (youANDme EDIT)");
        assert_eq!(l.title.as_deref(), Some("It's 2 Late 4 U And Me (youANDme EDIT)"));
        let l = line("25:20 SnPLO - Unknown [Pin-1 - A1]");
        assert_eq!(l.catno_hint.as_deref(), Some("Pin-1"));
        assert_eq!(l.kind, LineKind::Track);
        let l = line("A - B [Sound Signature - SS 011]");
        assert_eq!(l.label_hint.as_deref(), Some("Sound Signature"));
        assert_eq!(l.catno_hint.as_deref(), Some("SS 011"));
    }
}
