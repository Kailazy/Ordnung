//! ANLZ half of the golden differ: compare one `.DAT`/`.EXT`/`.2EX` file
//! rekordbox wrote against Ordnung's for the same track, section by section.
//!
//! Every section is compared byte for byte first. Only when the bytes differ
//! does the section's own meaning decide the verdict: a path that differs only
//! by folder (Ordnung keeps `/Contents` flat), cues and beats decoded so the
//! delta names *which* cue or beat moved, and waveforms scored by shape and
//! level, since they come from Ordnung's own decode and never match
//! rekordbox's bytes exactly. Layouts: `docs/rekordbox-export-structure.md` §5.

use ordnung_core::model::{Beat, Cue};

use super::{delta, Delta};

/// Pearson correlation a waveform must reach against rekordbox's to count
/// as the same picture.
pub const SHAPE_MIN_R: f32 = 0.85;
/// Mean-height ratio (ours / rekordbox's) a waveform must stay within: the
/// v0.141.1 bug drew heights at twice rekordbox's.
pub const LEVEL_RANGE: std::ops::RangeInclusive<f32> = 0.8..=1.25;

/// One tagged section: fourcc, declared header span, and the whole section
/// bytes (12-byte prelude included).
struct Section<'a> {
    tag: String,
    len_header: usize,
    bytes: &'a [u8],
}

impl Section<'_> {
    fn header(&self) -> &[u8] {
        &self.bytes[12.min(self.bytes.len())..self.len_header.min(self.bytes.len())]
    }
    fn body(&self) -> &[u8] {
        &self.bytes[self.len_header.min(self.bytes.len())..]
    }
    /// Past the 12-byte prelude: what the reader's section parsers take.
    fn after_prelude(&self) -> &[u8] {
        &self.bytes[12.min(self.bytes.len())..]
    }
}

use crate::anlz::u32_at as be32;

/// Walk a PMAI file's sections. `None` when the file isn't PMAI; a malformed
/// length ends the walk early.
fn sections(data: &[u8]) -> Option<Vec<Section<'_>>> {
    let (spans, _) = crate::anlz::section_spans(data)?;
    let section = |(off, len): (usize, usize)| Section {
        tag: String::from_utf8_lossy(&data[off..off + 4]).into_owned(),
        len_header: be32(data, off + 4).unwrap_or(0) as usize,
        bytes: &data[off..off + len],
    };
    Some(spans.into_iter().map(section).collect())
}

/// How closely one waveform section of ours follows rekordbox's.
#[derive(Debug, Clone, PartialEq)]
pub struct Shape {
    /// `DAT.PWAV`, `EXT.PWV5`, …
    pub section: String,
    /// Pearson correlation of the decoded heights.
    pub r: f32,
    /// Mean height, ours / rekordbox's.
    pub level: f32,
}

impl Shape {
    pub fn ok(&self) -> bool {
        self.r >= SHAPE_MIN_R && LEVEL_RANGE.contains(&self.level)
    }
}

/// The result of diffing one ANLZ file.
#[derive(Debug, Clone, Default)]
pub struct AnlzDiff {
    pub deltas: Vec<Delta>,
    /// Every waveform section's score, including ones that passed, so a
    /// report can show the spread.
    pub shapes: Vec<Shape>,
}

/// Diff one ANLZ file. `kind` ("DAT", "EXT", "2EX") prefixes every field.
pub fn diff_anlz(kind: &str, golden: &[u8], ours: &[u8]) -> AnlzDiff {
    let mut out = AnlzDiff::default();
    let (Some(gs), Some(os)) = (sections(golden), sections(ours)) else {
        out.deltas.push(delta(
            &format!("{kind}.file"),
            if golden.starts_with(b"PMAI") { "PMAI" } else { "not PMAI" },
            if ours.starts_with(b"PMAI") { "PMAI" } else { "not PMAI" },
            None,
        ));
        return out;
    };

    // PMAI header, minus its total-length field (which follows the sections).
    let hdr = |d: &[u8]| -> Vec<u8> {
        let n = be32(d, 4).unwrap_or(0) as usize;
        let mut h = d.get(..n.min(d.len())).unwrap_or(&[]).to_vec();
        if h.len() >= 12 {
            h[8..12].fill(0);
        }
        h
    };
    if hdr(golden) != hdr(ours) {
        out.deltas.push(delta(&format!("{kind}.header"), hex(&hdr(golden)), hex(&hdr(ours)), None));
    }

    let order = |s: &[Section]| s.iter().map(|x| x.tag.as_str()).collect::<Vec<_>>().join(" ");
    if order(&gs) != order(&os) {
        out.deltas.push(delta(&format!("{kind}.sections"), order(&gs), order(&os), None));
    }

    // Pair sections by (tag, occurrence): a file carries two cue lists.
    let keys = |s: &[Section]| {
        let mut k: Vec<(String, usize)> = Vec::new();
        for x in s {
            let n = k.iter().filter(|(t, _)| *t == x.tag).count();
            k.push((x.tag.clone(), n));
        }
        k
    };
    let mut all = keys(&gs);
    for k in keys(&os) {
        if !all.contains(&k) {
            all.push(k);
        }
    }
    fn nth<'v, 'a>(s: &'v [Section<'a>], tag: &str, n: usize) -> Option<&'v Section<'a>> {
        s.iter().filter(|x| x.tag == tag).nth(n)
    }
    for (tag, occ) in all {
        let field = format!("{kind}.{tag}");
        match (nth(&gs, &tag, occ), nth(&os, &tag, occ)) {
            (Some(g), Some(o)) => diff_section(&field, g, o, &mut out),
            (Some(_), None) => out.deltas.push(delta(&field, "present", "absent", None)),
            (None, Some(_)) => out.deltas.push(delta(&field, "absent", "present", None)),
            (None, None) => {}
        }
    }
    out
}

fn diff_section(field: &str, g: &Section, o: &Section, out: &mut AnlzDiff) {
    if g.bytes == o.bytes {
        return;
    }
    match g.tag.as_str() {
        "PPTH" => {
            let (a, b) = (ppth(g), ppth(o));
            let base = |p: &str| p.rsplit('/').next().unwrap_or("").to_lowercase();
            let why = (base(&a) == base(&b))
                .then_some("Ordnung keeps /Contents flat; rekordbox mirrors the source tree");
            out.deltas.push(delta(field, a, b, why));
        }
        "PQTZ" => {
            if g.header() != o.header() {
                out.deltas.push(delta(&format!("{field}.header"), hex(g.header()), hex(o.header()), None));
            }
            let (a, b) = (beats(g), beats(o));
            if a == b {
                out.deltas.push(first_byte_diff(field, g, o));
            } else {
                out.deltas.push(beat_delta(field, &a, &b));
            }
        }
        "PCOB" | "PCO2" => {
            let parse = |s: &Section| {
                if s.tag == "PCOB" {
                    crate::anlz::parse_pcob_body(s.after_prelude())
                } else {
                    crate::anlz::parse_pco2_body(s.after_prelude())
                }
            };
            let (a, b) = (parse(g), parse(o));
            if a == b {
                // Same cues as the reader sees them, different bytes: a field
                // the decoder ignores (colour index, loop beats, order links).
                out.deltas.push(first_byte_diff(field, g, o));
            } else {
                out.deltas.push(cue_delta(field, &a, &b));
            }
        }
        "PWAV" | "PWV2" | "PWV3" | "PWV4" | "PWV5" | "PWV6" | "PWV7" => {
            if g.header() != o.header() {
                out.deltas.push(delta(&format!("{field}.header"), hex(g.header()), hex(o.header()), None));
            }
            let (a, b) = (heights(&g.tag, g.body()), heights(&o.tag, o.body()));
            let shape = Shape {
                section: field.to_string(),
                r: pearson(&a, &b),
                level: mean(&b) / mean(&a).max(1e-6),
            };
            let why = shape.ok().then_some("waveform is Ordnung's own decode; shape and level match");
            out.deltas.push(delta(
                &format!("{field}.shape"),
                format!("mean {:.2}", mean(&a)),
                format!("r {:.3} level {:.2}", shape.r, shape.level),
                why,
            ));
            out.shapes.push(shape);
        }
        "PVBR" => {
            let nonzero = |s: &Section| s.body().chunks(4).filter(|c| c.iter().any(|&b| b != 0)).count();
            let differ = g
                .body()
                .chunks(4)
                .zip(o.body().chunks(4))
                .filter(|(x, y)| x != y)
                .count();
            out.deltas.push(delta(
                field,
                format!("{} nonzero entries", nonzero(g)),
                format!("{} nonzero, {differ} differ", nonzero(o)),
                None,
            ));
        }
        _ => out.deltas.push(first_byte_diff(field, g, o)),
    }
}

fn ppth(s: &Section) -> String {
    let body = s.body();
    let n = be32(s.header(), 0).unwrap_or(0) as usize;
    let raw = body.get(..n.min(body.len())).unwrap_or(&[]);
    let units: Vec<u16> = raw
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

fn beats(s: &Section) -> Vec<Beat> {
    // Header: u32 0, u32 0x00080000, u32 count; body: 8-byte beats.
    let n = be32(s.header(), 8).unwrap_or(0) as usize;
    s.body()
        .chunks_exact(8)
        .take(n)
        .map(|e| Beat {
            number: u32::from(u16::from_be_bytes([e[0], e[1]])),
            bpm: f32::from(u16::from_be_bytes([e[2], e[3]])) / 100.0,
            position_ms: u64::from(u32::from_be_bytes([e[4], e[5], e[6], e[7]])),
        })
        .collect()
}

fn beat_str(b: &Beat) -> String {
    format!("{}/{:.2}/{}ms", b.number, b.bpm, b.position_ms)
}

fn beat_delta(field: &str, a: &[Beat], b: &[Beat]) -> Delta {
    let differ = a.iter().zip(b).filter(|(x, y)| x != y).count() + a.len().abs_diff(b.len());
    let max_ms = a
        .iter()
        .zip(b)
        .map(|(x, y)| x.position_ms.abs_diff(y.position_ms))
        .max()
        .unwrap_or(0);
    let first = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let at = |v: &[Beat]| v.get(first).map(beat_str).unwrap_or_else(|| "-".into());
    delta(
        &format!("{field}.beats"),
        format!("{} beats, #{first} {}", a.len(), at(a)),
        format!("{} beats, #{first} {} ({differ} differ, max {max_ms} ms)", b.len(), at(b)),
        None,
    )
}

fn cue_str(c: &Cue) -> String {
    let mut s = match c.slot_letter() {
        Some(l) => format!("{l}@{}", c.position_ms),
        None => format!("M@{}", c.position_ms),
    };
    if let Some(end) = c.loop_end_ms {
        s.push_str(&format!("-{end}"));
    }
    if let Some(rgb) = c.color {
        s.push_str(&format!(" #{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
    }
    if let Some(l) = &c.label {
        s.push_str(&format!(" \"{l}\""));
    }
    s
}

fn cue_delta(field: &str, a: &[Cue], b: &[Cue]) -> Delta {
    let first = a.iter().zip(b).position(|(x, y)| x != y).unwrap_or(a.len().min(b.len()));
    let at = |v: &[Cue]| v.get(first).map(cue_str).unwrap_or_else(|| "-".into());
    delta(
        &format!("{field}.cues"),
        format!("{} cues, #{first} {}", a.len(), at(a)),
        format!("{} cues, #{first} {}", b.len(), at(b)),
        None,
    )
}

/// The first differing byte, as an offset into the section.
fn first_byte_diff(field: &str, g: &Section, o: &Section) -> Delta {
    let i = g
        .bytes
        .iter()
        .zip(o.bytes)
        .position(|(x, y)| x != y)
        .unwrap_or(g.bytes.len().min(o.bytes.len()));
    let around = |b: &[u8]| hex(b.get(i..(i + 8).min(b.len())).unwrap_or(&[]));
    delta(
        &format!("{field}.bytes"),
        format!("len {} @{i:#x} {}", g.bytes.len(), around(g.bytes)),
        format!("len {} @{i:#x} {}", o.bytes.len(), around(o.bytes)),
        None,
    )
}

/// Decode a waveform body to the heights a player draws, band-major for
/// the multi-band forms so the correlation covers every band.
fn heights(tag: &str, body: &[u8]) -> Vec<f32> {
    match tag {
        // 1 byte per column: 5-bit height under 3-bit whiteness.
        "PWAV" | "PWV3" => body.iter().map(|b| (b & 0x1F) as f32).collect(),
        // 1 byte per column, 4-bit height.
        "PWV2" => body.iter().map(|b| (b & 0x0F) as f32).collect(),
        // u16 per column: rgb 3+3+3 bits, 5-bit height, 2 spare.
        "PWV5" => body
            .chunks_exact(2)
            .map(|c| ((u16::from_be_bytes([c[0], c[1]]) >> 2) & 0x1F) as f32)
            .collect(),
        // 6 bytes per column (colour preview).
        "PWV4" => bands(body, 6),
        // 3 bytes per column: low, mid, high.
        "PWV6" | "PWV7" => bands(body, 3),
        _ => body.iter().map(|&b| b as f32).collect(),
    }
}

fn bands(body: &[u8], stride: usize) -> Vec<f32> {
    let cols: Vec<&[u8]> = body.chunks_exact(stride).collect();
    (0..stride)
        .flat_map(|k| cols.iter().map(move |c| c[k] as f32))
        .collect()
}

fn mean(v: &[f32]) -> f32 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f32>() / v.len() as f32
    }
}

/// Pearson correlation over the common length; 1.0 for two flat series that
/// agree, 0.0 when only one is flat.
fn pearson(a: &[f32], b: &[f32]) -> f32 {
    let n = a.len().min(b.len());
    if n == 0 {
        return 0.0;
    }
    let (a, b) = (&a[..n], &b[..n]);
    let (ma, mb) = (mean(a), mean(b));
    let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
    for (x, y) in a.iter().zip(b) {
        let (dx, dy) = ((x - ma) as f64, (y - mb) as f64);
        sab += dx * dy;
        saa += dx * dx;
        sbb += dy * dy;
    }
    if saa == 0.0 || sbb == 0.0 {
        return if saa == sbb && ma == mb { 1.0 } else { 0.0 };
    }
    (sab / (saa.sqrt() * sbb.sqrt())) as f32
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect::<Vec<_>>().join("")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(sections: &[(&[u8; 4], u32, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (tag, len_header, b) in sections {
            body.extend_from_slice(*tag);
            body.extend_from_slice(&len_header.to_be_bytes());
            body.extend_from_slice(&(12 + b.len() as u32).to_be_bytes());
            body.extend_from_slice(b);
        }
        let mut v = b"PMAI".to_vec();
        v.extend_from_slice(&0x1Cu32.to_be_bytes());
        v.extend_from_slice(&(0x1C + body.len() as u32).to_be_bytes());
        v.extend_from_slice(&[0u8; 16]);
        v.extend_from_slice(&body);
        v
    }

    fn pqtz(times: &[u32]) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 0, 0, 8, 0, 0];
        b.extend_from_slice(&(times.len() as u32).to_be_bytes());
        for (i, t) in times.iter().enumerate() {
            b.extend_from_slice(&((i % 4 + 1) as u16).to_be_bytes());
            b.extend_from_slice(&12800u16.to_be_bytes());
            b.extend_from_slice(&t.to_be_bytes());
        }
        b
    }

    #[test]
    fn identical_files_have_no_deltas() {
        let f = file(&[(b"PQTZ", 0x18, pqtz(&[100, 569]))]);
        let d = diff_anlz("DAT", &f, &f);
        assert!(d.deltas.is_empty(), "{:?}", d.deltas);
    }

    #[test]
    fn a_moved_beat_is_named() {
        let g = file(&[(b"PQTZ", 0x18, pqtz(&[100, 569, 1038]))]);
        let o = file(&[(b"PQTZ", 0x18, pqtz(&[100, 570, 1038]))]);
        let d = diff_anlz("DAT", &g, &o);
        assert_eq!(d.deltas.len(), 1);
        assert_eq!(d.deltas[0].field, "DAT.PQTZ.beats");
        assert!(d.deltas[0].ours.contains("1 differ, max 1 ms"), "{:?}", d.deltas[0]);
        assert!(d.deltas[0].explained.is_none());
    }

    #[test]
    fn a_missing_section_is_unexplained() {
        let g = file(&[(b"PQTZ", 0x18, pqtz(&[100])), (b"PWVC", 0x0E, vec![0, 3, 0, 0, 0, 0])]);
        let o = file(&[(b"PQTZ", 0x18, pqtz(&[100]))]);
        let d = diff_anlz("EXT", &g, &o);
        assert!(d.deltas.iter().any(|x| x.field == "EXT.PWVC" && x.ours == "absent"));
        assert!(d.deltas.iter().any(|x| x.field == "EXT.sections"));
    }

    #[test]
    fn waveforms_score_by_shape_and_level() {
        let wave: Vec<u8> = (0..400u32).map(|i| ((i * 7) % 31) as u8).collect();
        let hdr = |n: u32| {
            let mut h = n.to_be_bytes().to_vec();
            h.extend_from_slice(&0x0001_0000u32.to_be_bytes());
            h
        };
        let with = |w: &[u8]| {
            let mut b = hdr(400);
            b.extend_from_slice(w);
            file(&[(b"PWAV", 0x14, b)])
        };
        // Same shape, one step taller everywhere: explained.
        let near: Vec<u8> = wave.iter().map(|b| (b + 1).min(31)).collect();
        let d = diff_anlz("DAT", &with(&wave), &with(&near));
        assert_eq!(d.shapes.len(), 1);
        assert!(d.shapes[0].ok(), "{:?}", d.shapes);
        assert!(d.deltas.iter().all(|x| x.explained.is_some()));
        // Twice as tall: the level check catches it.
        let tall: Vec<u8> = wave.iter().map(|b| (b * 2).min(31)).collect();
        let d = diff_anlz("DAT", &with(&wave), &with(&tall));
        assert!(!d.shapes[0].ok());
    }
}
