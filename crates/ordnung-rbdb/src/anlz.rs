//! ANLZ `.DAT`/`.EXT`/`.2EX` writer.
//!
//! Serializes the tagged-section analysis files CDJs read: beatgrid (`PQTZ`),
//! waveforms (`PWAV`/`PWV2`/`PWV3`/`PWV4`/`PWV5`/`PWV6`/`PWV7`), cue lists
//! (`PCOB`/`PCO2`), path (`PPTH`), VBR index (`PVBR`), the extended grid
//! (`PQT2`) and the 3-band scale stub (`PWVC`). Section orders, header
//! constants and field layouts follow rekordbox 7.2.2 exactly —
//! byte provenance in `docs/rekordbox-export-structure.md` §5.
//!
//! All integers big-endian. Empty cue lists and the header-only `PQT2` are
//! forms rekordbox itself writes, so they are safe minimal outputs.

use ordnung_core::model::Beat;

/// Everything the ANLZ files for one track are derived from.
pub(crate) struct AnlzInput<'a> {
    /// Path of the audio file *on the USB*, volume-absolute with forward
    /// slashes (e.g. `/Contents/track.aiff`).
    pub usb_path: &'a str,
    /// Full expanded beatgrid (every beat, bar positions 1–4). Empty = no grid.
    pub beats: &'a [Beat],
    pub duration_ms: u64,
    /// 400-bin amplitude preview, values 0–255 (may be empty).
    pub preview: &'a [u8],
    /// `[low, mid, high, loudness]` quads at 20 bins/sec (may be empty).
    pub bands: &'a [u8],
    /// Rekordbox-style detail columns `[low, mid, high, amp]` at 150 col/s
    /// (`waveform::scroll_bands`) — linear per-column band peaks, exactly
    /// what the detailed waveforms should carry. Empty when the audio wasn't
    /// decodable at export time; the coarse cached data above fills in.
    pub scroll: &'a [u8],
}

const BANDS_PER_SEC: f64 = 20.0;
/// Detailed-waveform rate rekordbox uses everywhere (0x96 = 150 columns/sec).
const SCROLL_PER_SEC: u32 = 150;

// ---------------------------------------------------------------------------
// Read side — waveforms back OUT of a stick's ANLZ pair
// ---------------------------------------------------------------------------

/// A track's waveforms read back from its ANLZ files, converted to the same
/// shapes the catalog analyzer produces (`Analysis::waveform_preview` /
/// `waveform_bands`), so device rows can render exactly like library rows
/// without decoding any audio.
#[derive(Debug, Clone, Default)]
pub struct AnlzWaveforms {
    /// 400-bin amplitude preview, 0–255 per bin (from `PWAV`).
    pub preview: Vec<u8>,
    /// `[low, mid, high, loudness]` quads at 20 bins/sec (from the `.EXT`'s
    /// `PWV5` color waveform). Empty when the stick has no `.EXT` — the
    /// preview alone still draws a monochrome waveform.
    pub bands: Vec<u8>,
}

/// Locate one tagged section's body (past its 12-byte prelude) in an ANLZ
/// file. Defensive: any malformed length ends the walk.
fn find_section<'a>(data: &'a [u8], want: &[u8; 4]) -> Option<&'a [u8]> {
    if data.len() < 0x1C || &data[0..4] != b"PMAI" {
        return None;
    }
    let mut off = 0x1C;
    while off + 12 <= data.len() {
        let len_tag = u32::from_be_bytes(data[off + 8..off + 12].try_into().ok()?) as usize;
        if len_tag < 12 || off + len_tag > data.len() {
            return None;
        }
        if &data[off..off + 4] == want {
            return Some(&data[off + 12..off + len_tag]);
        }
        off += len_tag;
    }
    None
}

/// Read a track's beatgrid back from its `ANLZ0000.DAT`'s `PQTZ` section:
/// per beat `u16 bar position (1–4), u16 BPM×100, u32 time ms`, big-endian.
/// Empty when the file is missing or carries no grid.
pub fn read_beatgrid(dat_path: &std::path::Path) -> Vec<Beat> {
    let Ok(dat) = std::fs::read(dat_path) else {
        return Vec::new();
    };
    let Some(body) = find_section(&dat, b"PQTZ") else {
        return Vec::new();
    };
    // Body: u32 0, u32 0x00080000, u32 count, then 8-byte beat entries.
    let Some(n) = body
        .get(8..12)
        .and_then(|b| b.try_into().ok())
        .map(u32::from_be_bytes)
    else {
        return Vec::new();
    };
    let Some(entries) = body.get(12..12 + n as usize * 8) else {
        return Vec::new();
    };
    entries
        .chunks_exact(8)
        .map(|e| Beat {
            number: u32::from(u16::from_be_bytes([e[0], e[1]])),
            bpm: f32::from(u16::from_be_bytes([e[2], e[3]])) / 100.0,
            position_ms: u64::from(u32::from_be_bytes([e[4], e[5], e[6], e[7]])),
        })
        .collect()
}

/// Read a track's waveforms from its `ANLZ0000.DAT` (the `.EXT` sibling is
/// derived by extension swap, as players do). `None` when the file is
/// missing, unreadable, or carries no `PWAV`.
pub fn read_waveforms(dat_path: &std::path::Path) -> Option<AnlzWaveforms> {
    let dat = std::fs::read(dat_path).ok()?;
    // PWAV body: u32 len, u32 0x00010000, then len bytes of
    // 5-bit height | 3-bit whiteness columns.
    let body = find_section(&dat, b"PWAV")?;
    let n = u32::from_be_bytes(body.get(0..4)?.try_into().ok()?) as usize;
    let cols = body.get(8..8 + n)?;
    let preview: Vec<u8> = cols.iter().map(|b| (b & 0x1F) << 3).collect();
    if preview.iter().all(|&b| b == 0) {
        return None; // an un-analyzed export; let the caller fall back
    }

    // PWV5 body: u32 2, u32 num, u32 0x00960305, then u16be columns at
    // 150/sec — bits 15–13 low, 12–10 mid, 9–7 high, 6–2 height.
    let bands = std::fs::read(dat_path.with_extension("EXT"))
        .ok()
        .and_then(|ext| {
            let body = find_section(&ext, b"PWV5")?;
            let n = u32::from_be_bytes(body.get(4..8)?.try_into().ok()?) as usize;
            let cols = body.get(12..12 + n * 2)?;
            // Resample 150 cols/sec down to the GUI's 20 bins/sec.
            let bins = n * BANDS_PER_SEC as usize / SCROLL_PER_SEC as usize;
            let mut out = Vec::with_capacity(bins * 4);
            for j in 0..bins {
                let i = (j * SCROLL_PER_SEC as usize / BANDS_PER_SEC as usize).min(n - 1);
                let v = u16::from_be_bytes([cols[i * 2], cols[i * 2 + 1]]);
                let scale3 = |c: u16| ((c & 7) * 255 / 7) as u8;
                out.push(scale3(v >> 13));
                out.push(scale3(v >> 10));
                out.push(scale3(v >> 7));
                out.push((((v >> 2) & 0x1F) << 3) as u8);
            }
            Some(out)
        })
        .unwrap_or_default();

    Some(AnlzWaveforms { preview, bands })
}

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

/// One tagged section: fourcc + len_header + len_tag, then body. `body` holds
/// everything past the 12-byte prelude (the section's own header fields
/// included), so `len_tag` — the full section size — is `12 + body.len()`;
/// `len_header` is the declared header span within that.
fn section(tag: &[u8; 4], len_header: u32, body: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(12 + body.len());
    v.extend_from_slice(tag);
    v.extend_from_slice(&be32(len_header));
    v.extend_from_slice(&be32(12 + body.len() as u32));
    v.extend_from_slice(body);
    v
}

/// Wrap sections in the `PMAI` file header (constants byte-identical to
/// rekordbox 7 output).
fn pmai(sections: Vec<Vec<u8>>) -> Vec<u8> {
    let body_len: usize = sections.iter().map(|s| s.len()).sum();
    let mut v = Vec::with_capacity(0x1C + body_len);
    v.extend_from_slice(b"PMAI");
    v.extend_from_slice(&be32(0x1C));
    v.extend_from_slice(&be32(0x1C + body_len as u32));
    // Header words verified against all 1218 golden ANLZ files:
    // 1, 0x00010000, 0x00010000, 0 — the third word is 0x00010000 (a
    // version-like field), NOT 1; rekordbox silently ignores the analysis
    // (no waveform previews) when it doesn't match.
    v.extend_from_slice(&be32(1));
    v.extend_from_slice(&be32(0x0001_0000));
    v.extend_from_slice(&be32(0x0001_0000));
    v.extend_from_slice(&be32(0));
    for s in sections {
        v.extend_from_slice(&s);
    }
    v
}

fn ppth(usb_path: &str) -> Vec<u8> {
    let mut body = Vec::new();
    let units: Vec<u16> = usb_path.encode_utf16().chain(std::iter::once(0)).collect();
    body.extend_from_slice(&be32(units.len() as u32 * 2));
    for u in units {
        body.extend_from_slice(&be16(u));
    }
    section(b"PPTH", 0x10, &body)
}

/// Zeroed VBR seek index — what rekordbox writes for constant-bitrate audio.
/// (u32 at 0x0c, 400 index entries, one trailing u32 — 1620 bytes total,
/// matching rekordbox's fixed size.)
fn pvbr() -> Vec<u8> {
    section(b"PVBR", 0x10, &[0u8; 4 + 400 * 4 + 4])
}

fn pqtz(beats: &[Beat]) -> Vec<u8> {
    let mut body = Vec::with_capacity(12 + beats.len() * 8);
    body.extend_from_slice(&be32(0));
    body.extend_from_slice(&be32(0x0008_0000));
    body.extend_from_slice(&be32(beats.len() as u32));
    for b in beats {
        body.extend_from_slice(&be16(b.number.clamp(1, 4) as u16));
        body.extend_from_slice(&be16((b.bpm * 100.0).round() as u16));
        body.extend_from_slice(&be32(b.position_ms as u32));
    }
    section(b"PQTZ", 0x18, &body)
}

/// Header-only extended beatgrid (count 0) — a form rekordbox itself emits.
fn pqt2() -> Vec<u8> {
    let mut body = vec![0u8; 0x38 - 0x0C];
    body[4..8].copy_from_slice(&be32(0x0100_0002));
    section(b"PQT2", 0x38, &body)
}

/// Empty classic cue list (`type`: 1 = hot cues, 0 = memory cues).
fn pcob(list_type: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(12);
    body.extend_from_slice(&be32(list_type));
    body.extend_from_slice(&be16(0));
    body.extend_from_slice(&be16(0));
    body.extend_from_slice(&[0xFF; 4]);
    section(b"PCOB", 0x18, &body)
}

/// Empty nxs2 cue list.
fn pco2(list_type: u32) -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    body.extend_from_slice(&be32(list_type));
    body.extend_from_slice(&be16(0));
    body.extend_from_slice(&be16(0));
    section(b"PCO2", 0x14, &body)
}

/// Sample the band quads at a time position; returns `[low, mid, high, loud]`.
fn band_at(bands: &[u8], t_ms: f64) -> [u8; 4] {
    let bins = bands.len() / 4;
    if bins == 0 {
        return [0; 4];
    }
    let i = ((t_ms / 1000.0 * BANDS_PER_SEC) as usize).min(bins - 1);
    [
        bands[i * 4],
        bands[i * 4 + 1],
        bands[i * 4 + 2],
        bands[i * 4 + 3],
    ]
}

/// Amplitude 0–255 at a fraction of the track, from the 400-bin preview.
fn amp_at(preview: &[u8], frac: f64) -> u8 {
    if preview.is_empty() {
        return 0;
    }
    let i = ((frac * preview.len() as f64) as usize).min(preview.len() - 1);
    preview[i]
}

/// Whiteness 0–7: high-band share of the spectrum at this instant.
fn whiteness(b: &[u8; 4]) -> u8 {
    let (low, mid, high) = (b[0] as u32, b[1] as u32, b[2] as u32);
    let total = low + mid + high;
    if total == 0 {
        return 0;
    }
    (((mid / 2 + high) * 8) / total).min(7) as u8
}

/// Monochrome preview column: 3-bit whiteness | 5-bit height.
fn mono_col(inp: &AnlzInput, frac: f64) -> u8 {
    let h = amp_at(inp.preview, frac) >> 3; // 0..31
    let b = band_at(inp.bands, frac * inp.duration_ms as f64);
    (whiteness(&b) << 5) | h
}

fn pwav(inp: &AnlzInput) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 400);
    body.extend_from_slice(&be32(400));
    body.extend_from_slice(&be32(0x0001_0000));
    for i in 0..400 {
        body.push(mono_col(inp, i as f64 / 400.0));
    }
    section(b"PWAV", 0x14, &body)
}

fn pwv2(inp: &AnlzInput) -> Vec<u8> {
    let mut body = Vec::with_capacity(8 + 100);
    body.extend_from_slice(&be32(100));
    body.extend_from_slice(&be32(0x0001_0000));
    for i in 0..100 {
        body.push(mono_col(inp, i as f64 / 100.0));
    }
    section(b"PWV2", 0x14, &body)
}

fn scroll_cols(duration_ms: u64) -> u32 {
    ((duration_ms as u64 * SCROLL_PER_SEC as u64) / 1000).max(1) as u32
}

/// Big scrolling monochrome waveform: 1 byte/column at 150 col/s.
fn pwv3(inp: &AnlzInput) -> Vec<u8> {
    let n = scroll_cols(inp.duration_ms);
    let mut body = Vec::with_capacity(12 + n as usize);
    body.extend_from_slice(&be32(1));
    body.extend_from_slice(&be32(n));
    body.extend_from_slice(&be32(0x0096_0000));
    for i in 0..n {
        let frac = i as f64 / n as f64;
        body.push(match scroll_col(inp, frac) {
            Some([l, m, h, amp]) => (whiteness(&[l, m, h, 0]) << 5) | (amp >> 3),
            None => mono_col(inp, frac),
        });
    }
    section(b"PWV3", 0x18, &body)
}

/// Color scrolling waveform: u16be per column — 3-bit R/G/B + 5-bit height.
/// Channel order follows the golden reference (bits 15–13 track the low band).
fn pwv5(inp: &AnlzInput) -> Vec<u8> {
    let n = scroll_cols(inp.duration_ms);
    let mut body = Vec::with_capacity(12 + n as usize * 2);
    body.extend_from_slice(&be32(2));
    body.extend_from_slice(&be32(n));
    body.extend_from_slice(&be32(0x0096_0305));
    for i in 0..n {
        let frac = i as f64 / n as f64;
        let (low, mid, high, h) = if let Some([l, m, hb, amp]) = scroll_col(inp, frac) {
            ((l >> 4) as u16, (m >> 4) as u16, (hb >> 4) as u16, (amp >> 3) as u16)
        } else {
            let b = band_at(inp.bands, frac * inp.duration_ms as f64);
            (
                (b[0] >> 5) as u16,
                (b[1] >> 5) as u16,
                (b[2] >> 5) as u16,
                (amp_at(inp.preview, frac) >> 3) as u16,
            )
        };
        body.extend_from_slice(&be16((low << 13) | (mid << 10) | (high << 7) | (h << 2)));
    }
    section(b"PWV5", 0x18, &body)
}

/// Color preview: 1200 columns × 6 bytes. The byte semantics are our own
/// reverse-engineering (peak height, 0x80|brightness, back/front fill heights,
/// mid level, high level — correlations in the format doc); rekordbox's exact
/// derivation is unknown, but these render a faithful preview.
fn pwv4(inp: &AnlzInput) -> Vec<u8> {
    const N: usize = 1200;
    let mut body = Vec::with_capacity(12 + N * 6);
    body.extend_from_slice(&be32(6));
    body.extend_from_slice(&be32(N as u32));
    body.extend_from_slice(&be32(0));
    for i in 0..N {
        let f0 = i as f64 / N as f64;
        let f1 = (i + 1) as f64 / N as f64;
        // Aggregate the window [f0, f1).
        let (mut peak, mut sum, mut cnt) = (0u32, 0u32, 0u32);
        let (mut mid, mut high, mut wsum) = (0u32, 0u32, 0u32);
        let steps = 8;
        for s in 0..steps {
            let f = f0 + (f1 - f0) * (s as f64 / steps as f64);
            let a = amp_at(inp.preview, f) as u32;
            peak = peak.max(a);
            sum += a;
            cnt += 1;
            let b = band_at(inp.bands, f * inp.duration_ms as f64);
            mid = mid.max(b[1] as u32);
            high = high.max(b[2] as u32);
            wsum += whiteness(&b) as u32;
        }
        let mean = sum / cnt.max(1);
        let bright = (wsum * 127 / (7 * cnt.max(1))).min(127) as u8;
        body.push((peak / 2).min(127) as u8); // peak height
        body.push(0x80 | bright); // brightness / color hint
        body.push((peak * 9 / 20).min(127) as u8); // back fill
        body.push((mean / 2).min(127) as u8); // front fill
        body.push((mid / 2).min(127) as u8); // mid level
        body.push((high / 6).min(41) as u8); // high level
    }
    section(b"PWV4", 0x18, &body)
}

/// The scroll column `[low, mid, high, amp]` at a fraction of the track,
/// when export-time audio decoding produced one.
fn scroll_col(inp: &AnlzInput, frac: f64) -> Option<[u8; 4]> {
    let cols = inp.scroll.len() / 4;
    if cols == 0 {
        return None;
    }
    let i = ((frac * cols as f64) as usize).min(cols - 1);
    Some(inp.scroll[i * 4..i * 4 + 4].try_into().unwrap())
}

/// One 3-band column `[low, mid, high]`, 0–127 — the scale the golden PWV7
/// carries. Prefers the export-time scroll columns (real per-column band
/// peaks). The cached fallback's band bytes are sqrt-companded for GUI
/// drawing (`waveform::color_bands`) while rekordbox stores linear levels,
/// so square them back or every loud track renders as a solid pinned block;
/// with no band data at all, shape the amplitude preview so the waveform
/// never comes out blank.
fn band3_col(inp: &AnlzInput, frac: f64) -> [u8; 3] {
    if let Some([l, m, h, _]) = scroll_col(inp, frac) {
        return [l, m, h];
    }
    let decompand = |v: u8| {
        let r = v as f64 / 255.0;
        (r * r * 127.0).round() as u8
    };
    if inp.bands.is_empty() {
        let a = amp_at(inp.preview, frac);
        return [decompand(a), decompand(a) >> 1, decompand(a) >> 3];
    }
    let b = band_at(inp.bands, frac * inp.duration_ms as f64);
    [decompand(b[0]), decompand(b[1]), decompand(b[2])]
}

/// 3-band scrolling waveform (rekordbox 6/7's default display style and the
/// CDJ-3000's): 3 bytes/column (low/mid/high, 0–127) at 150 col/s.
fn pwv7(inp: &AnlzInput) -> Vec<u8> {
    let n = scroll_cols(inp.duration_ms);
    let mut body = Vec::with_capacity(12 + n as usize * 3);
    body.extend_from_slice(&be32(3));
    body.extend_from_slice(&be32(n));
    body.extend_from_slice(&be32(0x0096_0000));
    for i in 0..n {
        body.extend_from_slice(&band3_col(inp, i as f64 / n as f64));
    }
    section(b"PWV7", 0x18, &body)
}

/// 3-band preview: 1200 columns × 3 bytes. Golden files normalize each band
/// to the same track-wide mean brightness (~26) so all three stay visible in
/// the small preview; reproduce that from per-column window means.
fn pwv6(inp: &AnlzInput) -> Vec<u8> {
    const N: usize = 1200;
    const TARGET_MEAN: f64 = 26.0;
    // Pass 1: per-column window means of each band.
    let mut cols = vec![[0f64; 3]; N];
    let steps = 8;
    for (i, col) in cols.iter_mut().enumerate() {
        for s in 0..steps {
            let f = (i as f64 + s as f64 / steps as f64) / N as f64;
            let b = band3_col(inp, f);
            for k in 0..3 {
                col[k] += b[k] as f64 / steps as f64;
            }
        }
    }
    // Pass 2: per-band gain to the golden mean brightness.
    let mut gain = [1.0f64; 3];
    for k in 0..3 {
        let mean = cols.iter().map(|c| c[k]).sum::<f64>() / N as f64;
        if mean > 0.0 {
            gain[k] = (TARGET_MEAN / mean).min(8.0);
        }
    }
    let mut body = Vec::with_capacity(8 + N * 3);
    body.extend_from_slice(&be32(3));
    body.extend_from_slice(&be32(N as u32));
    for col in &cols {
        for k in 0..3 {
            body.push((col[k] * gain[k]).round().min(127.0) as u8);
        }
    }
    section(b"PWV6", 0x14, &body)
}

/// 3-band scale metadata stub — semantics unresolved; a mid-range observed
/// value (rekordbox writes per-track numbers in these bands' neighborhoods).
fn pwvc() -> Vec<u8> {
    let mut body = Vec::with_capacity(8);
    for w in [0u16, 0x63, 0x64, 0x120] {
        body.extend_from_slice(&be16(w));
    }
    section(b"PWVC", 0x0E, &body)
}

/// Build the `.DAT` — the classic set every CDJ generation requires.
/// Section order is rekordbox's, and rigid.
pub(crate) fn build_dat(inp: &AnlzInput) -> Vec<u8> {
    pmai(vec![
        ppth(inp.usb_path),
        pvbr(),
        pqtz(inp.beats),
        pwav(inp),
        pwv2(inp),
        pcob(1),
        pcob(0),
    ])
}

/// Build the `.EXT` — color waveforms + nxs2 cue lists for nxs2/CDJ-3000-era
/// players.
pub(crate) fn build_ext(inp: &AnlzInput) -> Vec<u8> {
    pmai(vec![
        ppth(inp.usb_path),
        pwv3(inp),
        pcob(1),
        pcob(0),
        pco2(1),
        pco2(0),
        pqt2(),
        pwv5(inp),
        pwv4(inp),
    ])
}

/// Build the `.2EX` — 3-band waveforms, the style rekordbox 6/7 and the
/// CDJ-3000 render by default. Without it an exported track draws as a flat
/// featureless bar on those displays. Section order is rekordbox's.
pub(crate) fn build_2ex(inp: &AnlzInput) -> Vec<u8> {
    pmai(vec![ppth(inp.usb_path), pwv7(inp), pwv6(inp), pwvc()])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn beats() -> Vec<Beat> {
        (0..8)
            .map(|i| Beat {
                number: (i % 4) + 1,
                position_ms: 100 + i as u64 * 500,
                bpm: 120.0,
            })
            .collect()
    }

    /// Walk a PMAI file: (tag, len_header, len_tag) triples must tile it.
    fn walk(data: &[u8]) -> Vec<(String, u32, u32)> {
        assert_eq!(&data[0..4], b"PMAI");
        let flen = u32::from_be_bytes(data[8..12].try_into().unwrap()) as usize;
        assert_eq!(flen, data.len());
        let mut off = u32::from_be_bytes(data[4..8].try_into().unwrap()) as usize;
        let mut out = Vec::new();
        while off + 12 <= data.len() {
            let tag = String::from_utf8_lossy(&data[off..off + 4]).into_owned();
            let lh = u32::from_be_bytes(data[off + 4..off + 8].try_into().unwrap());
            let lt = u32::from_be_bytes(data[off + 8..off + 12].try_into().unwrap());
            assert!(lt as usize >= 12, "section {tag} too short");
            out.push((tag, lh, lt));
            off += lt as usize;
        }
        assert_eq!(off, data.len(), "sections must tile the file exactly");
        out
    }

    #[test]
    fn dat_sections_in_rekordbox_order() {
        let b = beats();
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &b,
            duration_ms: 4_100,
            preview: &[128; 400],
            bands: &vec![64; 4 * 82],
            scroll: &[],
        };
        let dat = build_dat(&inp);
        // PMAI header words as every golden file has them; 1 in the third
        // slot makes rekordbox drop the analysis (blank waveform previews).
        assert_eq!(&dat[0x0C..0x1C], {
            let mut h = Vec::new();
            for w in [1u32, 0x0001_0000, 0x0001_0000, 0] {
                h.extend_from_slice(&w.to_be_bytes());
            }
            &h.clone()[..]
        });
        let tags: Vec<String> = walk(&dat).into_iter().map(|(t, _, _)| t).collect();
        assert_eq!(tags, ["PPTH", "PVBR", "PQTZ", "PWAV", "PWV2", "PCOB", "PCOB"]);
        // PQTZ: count and first beat round-trip.
        let sec = walk(&dat);
        let (_, _, _) = sec[2];
        let off: usize = {
            let mut o = 0x1C;
            for (t, _, lt) in &sec {
                if t == "PQTZ" {
                    break;
                }
                o += *lt as usize;
            }
            o
        };
        let count = u32::from_be_bytes(dat[off + 0x14..off + 0x18].try_into().unwrap());
        assert_eq!(count, 8);
        let n0 = u16::from_be_bytes(dat[off + 0x18..off + 0x1A].try_into().unwrap());
        let bpm0 = u16::from_be_bytes(dat[off + 0x1A..off + 0x1C].try_into().unwrap());
        let t0 = u32::from_be_bytes(dat[off + 0x1C..off + 0x20].try_into().unwrap());
        assert_eq!((n0, bpm0, t0), (1, 12000, 100));
    }

    #[test]
    fn waveforms_round_trip_through_anlz_files() {
        let b = beats();
        let preview: Vec<u8> = (0..400).map(|i| (i % 256) as u8).collect();
        let bands: Vec<u8> = (0..4 * 82).map(|i| (i % 200) as u8).collect();
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &b,
            duration_ms: 4_100,
            preview: &preview,
            bands: &bands,
            scroll: &[],
        };
        let dir = std::env::temp_dir().join(format!("ordnung-anlzr-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let dat = dir.join("ANLZ0000.DAT");
        std::fs::write(&dat, build_dat(&inp)).unwrap();
        std::fs::write(dir.join("ANLZ0000.EXT"), build_ext(&inp)).unwrap();

        let got = read_waveforms(&dat).expect("waveforms read back");
        assert_eq!(got.preview.len(), 400);
        // PWAV keeps the top 5 bits of each amplitude (quantization error ≤7),
        // and the writer's float resampling can land one source column over
        // (±1 on this ramp) — so values agree to within 8.
        for (a, b) in preview.iter().zip(&got.preview) {
            assert!(a.abs_diff(*b) <= 8, "preview {a} vs {b}");
        }
        // Bands come back at the same 20 bins/sec rate the analyzer uses.
        assert_eq!(got.bands.len() % 4, 0);
        assert!(!got.bands.is_empty());
        assert!(got.bands.iter().any(|&v| v > 0));

        // The beatgrid reads back beat-for-beat.
        let grid = read_beatgrid(&dat);
        assert_eq!(grid.len(), b.len());
        assert_eq!(grid.first().map(|x| x.number), b.first().map(|x| x.number));
        assert_eq!(
            grid.first().map(|x| x.position_ms),
            b.first().map(|x| x.position_ms)
        );
        assert!((grid[0].bpm - b[0].bpm).abs() < 0.01);

        // A DAT with no EXT still yields the preview alone.
        std::fs::remove_file(dir.join("ANLZ0000.EXT")).unwrap();
        let solo = read_waveforms(&dat).expect("preview without EXT");
        assert!(solo.bands.is_empty());
        assert_eq!(solo.preview, got.preview);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn ext_sections_in_rekordbox_order() {
        let b = beats();
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &b,
            duration_ms: 60_000,
            preview: &[200; 400],
            bands: &vec![100; 4 * 1200],
            scroll: &[],
        };
        let ext = build_ext(&inp);
        let secs = walk(&ext);
        let tags: Vec<&str> = secs.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(
            tags,
            ["PPTH", "PWV3", "PCOB", "PCOB", "PCO2", "PCO2", "PQT2", "PWV5", "PWV4"]
        );
        // PWV3 must cover duration × 150/s columns; PWV4 exactly 1200 × 6.
        let pwv3 = secs.iter().find(|(t, _, _)| t == "PWV3").unwrap();
        assert_eq!(pwv3.2, 0x18 + 60 * 150);
        let pwv4 = secs.iter().find(|(t, _, _)| t == "PWV4").unwrap();
        assert_eq!(pwv4.2, 0x18 + 1200 * 6);
    }

    #[test]
    fn ex2_sections_in_rekordbox_order() {
        let b = beats();
        let inp = AnlzInput {
            usb_path: "/Contents/x.mp3",
            beats: &b,
            duration_ms: 60_000,
            preview: &[200; 400],
            bands: &vec![100; 4 * 1200],
            scroll: &[],
        };
        let ex2 = build_2ex(&inp);
        let secs = walk(&ex2);
        let tags: Vec<&str> = secs.iter().map(|(t, _, _)| t.as_str()).collect();
        assert_eq!(tags, ["PPTH", "PWV7", "PWV6", "PWVC"]);
        // PWV7: 3 bytes × duration × 150/s columns; PWV6: exactly 1200 × 3;
        // PWVC: the fixed 20-byte stub.
        let pwv7 = secs.iter().find(|(t, _, _)| t == "PWV7").unwrap();
        assert_eq!(pwv7.2, 0x18 + 60 * 150 * 3);
        let pwv6 = secs.iter().find(|(t, _, _)| t == "PWV6").unwrap();
        assert_eq!((pwv6.1, pwv6.2), (0x14, 0x14 + 1200 * 3));
        let pwvc = secs.iter().find(|(t, _, _)| t == "PWVC").unwrap();
        assert_eq!((pwvc.1, pwvc.2), (0x0E, 20));

        // Every band byte stays in the golden 0–127 range, and the uniform
        // input renders non-blank in all three bands.
        let off = 0x1C + secs[0].2 as usize + 12 + 12; // past PPTH + PWV7 prelude/header
        let cols = &ex2[off..off + 60 * 150 * 3];
        assert!(cols.iter().all(|&v| v <= 127));
        for k in 0..3 {
            assert!(cols.iter().skip(k).step_by(3).any(|&v| v > 0));
        }
    }

    #[test]
    fn empty_analysis_still_produces_valid_files() {
        let inp = AnlzInput {
            usb_path: "/Contents/x.wav",
            beats: &[],
            duration_ms: 0,
            preview: &[],
            bands: &[],
            scroll: &[],
        };
        walk(&build_dat(&inp));
        walk(&build_ext(&inp));
        walk(&build_2ex(&inp));
    }
}
