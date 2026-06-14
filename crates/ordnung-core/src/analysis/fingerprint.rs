//! Perceptual acoustic fingerprint: a compact, time-aware summary of *what a
//! track sounds like*, used to catch "play-time" duplicates — the same recording
//! stored as two files whose bytes AND tags differ (a FLAC vs a 320 MP3, two
//! different rips/downloads, one with cleaned-up tags). The byte-level
//! `scan::file_fingerprint` and the artist+title heuristic both miss those; only
//! the decoded audio reveals them.
//!
//! Design (Chromaprint-lite): fold the magnitude spectrogram into a sequence of
//! 12-bin chroma frames, downsample to a fixed ~8 frames/sec rate so the
//! fingerprint is independent of the analyzer's FFT hop, and encode each frame as
//! a 24-bit sub-fingerprint:
//!   * bits 0..12  — chroma bins above the frame mean (which pitch classes ring)
//!   * bits 12..24 — chroma bins rising vs the previous frame (spectral motion)
//! Both are gain-invariant (chroma is per-frame normalized) and survive lossy
//! re-encoding, which only perturbs fine spectral detail. Two fingerprints are
//! compared by sliding one over the other within a small lag (to absorb encoder
//! delay / leading silence) and taking the lowest bit-error-rate over the
//! overlap. Identity, not crypto: a near-match is the signal, never an exact one.

use super::dsp::Spectrogram;

/// Target sub-fingerprints per second. Independent of the spectrogram hop: chroma
/// frames are averaged down to this rate so fingerprints from different analyzer
/// settings stay comparable. ~8/s mirrors Chromaprint and keeps fingerprints small
/// (a 5-minute track ≈ 2400 u32s ≈ 9.6 KB).
const TARGET_FPS: f32 = 8.0;

/// Bits actually used per sub-fingerprint (12 chroma-shape + 12 chroma-motion).
const USED_BITS: u32 = 24;

/// How far to slide one fingerprint against the other when matching, in
/// sub-fingerprints (~8/s). 6 s absorbs encoder delay, leading silence, and rip
/// trims without letting unrelated tracks drift into a chance alignment.
const MAX_LAG: usize = 48;

/// Minimum overlapping sub-fingerprints required to trust a score (~10 s). Short
/// overlaps make bit-error-rate noisy and invite false positives.
const MIN_OVERLAP: usize = 80;

/// Bit-error-rate at or below which two fingerprints are the same recording.
/// Unrelated tracks sit near 0.5 (random bits); the same recording across formats
/// stays well under 0.2 even through lossy re-encoding. 0.18 leaves margin both ways.
const DUP_MAX_BER: f32 = 0.18;

/// Build a perceptual fingerprint from a precomputed spectrogram. Returns an empty
/// vec for audio too short to carry `MIN_OVERLAP` frames — callers treat empty as
/// "no fingerprint" and simply don't match on it.
pub fn fingerprint(spec: &Spectrogram) -> Vec<u32> {
    let chroma = chroma_frames(spec);
    let down = downsample(&chroma, spec.frame_rate(), TARGET_FPS);
    encode(&down)
}

/// Per-frame 12-bin chromagram (index 0 = C). Each STFT frame's magnitude is
/// folded onto pitch classes over the pitched band (~A0..~C8), then L1-normalized
/// so loudness/gain differences between encodes don't move the bits. Silent frames
/// stay all-zero and are dropped downstream.
fn chroma_frames(spec: &Spectrogram) -> Vec<[f32; 12]> {
    // Pitched band: ignore sub-bass rumble and ultra-highs that carry no tonal info
    // and differ most between codecs (lossy formats roll off the top end).
    const LO_HZ: f32 = 55.0; // ~A1
    const HI_HZ: f32 = 5000.0; // ~D#8

    let mut out = Vec::with_capacity(spec.frames.len());
    for frame in &spec.frames {
        let mut c = [0.0f32; 12];
        for (i, &mag) in frame.iter().enumerate() {
            let f = spec.bin_hz(i);
            if f < LO_HZ || f > HI_HZ {
                continue;
            }
            // MIDI note → pitch class. log-magnitude compresses the dynamic range
            // so a few loud bins don't dominate the whole chroma vector.
            let midi = 69.0 + 12.0 * (f / 440.0).log2();
            let pc = (midi.round() as i32).rem_euclid(12) as usize;
            c[pc] += (mag + 1.0).ln();
        }
        let sum: f32 = c.iter().sum();
        if sum > 0.0 {
            for v in &mut c {
                *v /= sum;
            }
        }
        out.push(c);
    }
    out
}

/// Average groups of chroma frames down to `target_fps`. Averaging (rather than
/// decimating) is a low-pass that makes the fingerprint robust to the exact frame
/// phase, which differs between two encodes of the same audio.
fn downsample(frames: &[[f32; 12]], src_fps: f32, target_fps: f32) -> Vec<[f32; 12]> {
    if frames.is_empty() || src_fps <= 0.0 {
        return Vec::new();
    }
    let group = (src_fps / target_fps).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(frames.len() / group + 1);
    for chunk in frames.chunks(group) {
        let mut acc = [0.0f32; 12];
        for f in chunk {
            for p in 0..12 {
                acc[p] += f[p];
            }
        }
        let n = chunk.len() as f32;
        for v in &mut acc {
            *v /= n;
        }
        out.push(acc);
    }
    out
}

/// Encode downsampled chroma frames into 24-bit sub-fingerprints (see module doc).
/// All-zero (silent) frames are skipped so leading/trailing silence doesn't pad the
/// fingerprint with identical bits that would inflate match scores between any two
/// quiet sections.
fn encode(frames: &[[f32; 12]]) -> Vec<u32> {
    let mut out = Vec::with_capacity(frames.len());
    let mut prev: Option<[f32; 12]> = None;
    for f in frames {
        let sum: f32 = f.iter().sum();
        if sum <= 0.0 {
            continue; // silence
        }
        let mean = sum / 12.0;
        let mut word: u32 = 0;
        for p in 0..12 {
            if f[p] > mean {
                word |= 1 << p;
            }
            if let Some(prev) = prev {
                if f[p] > prev[p] {
                    word |= 1 << (p + 12);
                }
            }
        }
        out.push(word);
        prev = Some(*f);
    }
    out
}

/// Serialize a fingerprint to little-endian bytes for storage as a BLOB.
pub fn to_bytes(fp: &[u32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(fp.len() * 4);
    for &w in fp {
        out.extend_from_slice(&w.to_le_bytes());
    }
    out
}

/// Deserialize a fingerprint from `to_bytes`. Trailing bytes that don't fill a u32
/// (corruption / truncation) are ignored.
pub fn from_bytes(bytes: &[u8]) -> Vec<u32> {
    bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// Best (lowest) bit-error-rate between two fingerprints over all alignments within
/// `MAX_LAG`, considering only the used bits. Returns `None` when no alignment
/// overlaps by at least `MIN_OVERLAP` frames (too little common audio to judge).
/// 0.0 = identical over the overlap, ~0.5 = unrelated.
pub fn best_ber(a: &[u32], b: &[u32]) -> Option<f32> {
    if a.len() < MIN_OVERLAP || b.len() < MIN_OVERLAP {
        return None;
    }
    let mut best: Option<f32> = None;
    // lag = how far b is shifted right relative to a (negative = a shifted right).
    for lag in -(MAX_LAG as isize)..=(MAX_LAG as isize) {
        let (mut ia, mut ib) = if lag >= 0 {
            (0usize, lag as usize)
        } else {
            ((-lag) as usize, 0usize)
        };
        let overlap = a.len().saturating_sub(ia).min(b.len().saturating_sub(ib));
        if overlap < MIN_OVERLAP {
            continue;
        }
        let mut diff_bits: u64 = 0;
        for _ in 0..overlap {
            let x = (a[ia] ^ b[ib]) & ((1 << USED_BITS) - 1);
            diff_bits += x.count_ones() as u64;
            ia += 1;
            ib += 1;
        }
        let ber = diff_bits as f32 / (overlap as f32 * USED_BITS as f32);
        best = Some(best.map_or(ber, |cur: f32| cur.min(ber)));
    }
    best
}

/// Whether two fingerprints represent the same recording (the same audio you'd
/// hear on playback), tolerant of format, bitrate, gain, and small length/offset
/// differences. Conservative by design: ambiguous overlaps return `false`.
pub fn are_duplicates(a: &[u32], b: &[u32]) -> bool {
    best_ber(a, b).is_some_and(|ber| ber <= DUP_MAX_BER)
}

#[cfg(test)]
mod tests {
    use super::*;

    // A deterministic pseudo-random fingerprint (no Math.random in tests; seed-driven).
    fn synth(len: usize, seed: u64) -> Vec<u32> {
        let mut s = seed;
        (0..len)
            .map(|_| {
                // xorshift64
                s ^= s << 13;
                s ^= s >> 7;
                s ^= s << 17;
                (s as u32) & ((1 << USED_BITS) - 1)
            })
            .collect()
    }

    // Flip `n` random bits per word to simulate lossy-encode perturbation.
    fn perturb(fp: &[u32], bits_per_word: u32, seed: u64) -> Vec<u32> {
        let mut s = seed;
        fp.iter()
            .map(|&w| {
                let mut w = w;
                for _ in 0..bits_per_word {
                    s ^= s << 13;
                    s ^= s >> 7;
                    s ^= s << 17;
                    let bit = (s % USED_BITS as u64) as u32;
                    w ^= 1 << bit;
                }
                w
            })
            .collect()
    }

    #[test]
    fn identical_fingerprints_have_zero_ber() {
        let a = synth(200, 0xABCD);
        assert_eq!(best_ber(&a, &a), Some(0.0));
        assert!(are_duplicates(&a, &a));
    }

    #[test]
    fn lightly_perturbed_copy_is_a_duplicate() {
        // ~2 flipped bits / 24 ≈ 8% BER: a plausible lossy re-encode.
        let a = synth(300, 0x1234);
        let b = perturb(&a, 2, 0x9999);
        let ber = best_ber(&a, &b).unwrap();
        assert!(ber < DUP_MAX_BER, "expected dup, ber={ber}");
        assert!(are_duplicates(&a, &b));
    }

    #[test]
    fn unrelated_fingerprints_are_not_duplicates() {
        let a = synth(300, 0x1111);
        let b = synth(300, 0x2222);
        let ber = best_ber(&a, &b).unwrap();
        assert!(ber > 0.4, "unrelated should be near 0.5, ber={ber}");
        assert!(!are_duplicates(&a, &b));
    }

    #[test]
    fn matches_through_a_leading_offset() {
        // b is a, shifted by 20 frames (encoder delay / leading silence) + noise.
        let a = synth(300, 0x5555);
        let mut b = synth(20, 0xDEAD); // junk prefix
        b.extend(perturb(&a, 1, 0x4242));
        assert!(are_duplicates(&a, &b), "should align past the offset");
    }

    #[test]
    fn too_short_to_judge_returns_none() {
        let a = synth(MIN_OVERLAP - 1, 1);
        let b = synth(MIN_OVERLAP - 1, 1);
        assert_eq!(best_ber(&a, &b), None);
        assert!(!are_duplicates(&a, &b));
    }

    #[test]
    fn bytes_round_trip() {
        let a = synth(50, 0x7);
        assert_eq!(from_bytes(&to_bytes(&a)), a);
    }

    #[test]
    fn silent_frames_are_dropped_by_encode() {
        let frames = vec![[0.0f32; 12], {
            let mut f = [0.1f32; 12];
            f[3] = 0.9;
            f
        }];
        // One silent + one voiced frame → exactly one sub-fingerprint.
        assert_eq!(encode(&frames).len(), 1);
    }
}
