---
name: audio-analysis
description: Algorithms and contracts for Ordnung's audio analysis — BPM/tempo detection, beatgrid generation, musical key detection, the canonical Camelot key mapping, waveform/loudness, and the analysis cache. Use when working on the ordnung-core analysis module, tuning detection accuracy, or anything touching keys/BPM/beatgrids/waveforms.
---

# Ordnung audio analysis (ordnung-core/analysis)

Pure-Rust DSP (`symphonia` to decode → samples; `rustfft` for spectra). Engines are
stateless and parallelizable with `rayon`. Every result is cached.

## Pipeline per track

1. **Decode** to mono f32 PCM at a known rate (e.g. downmix; 44.1 kHz) via symphonia.
2. **BPM / tempo** — spectral-flux onset envelope → tempo via autocorrelation /
   comb-filter over a plausible DJ range (~70–185 BPM, with octave-error correction).
3. **Beatgrid** — phase-align beats to onset peaks; emit anchored beat positions
   (ms + sample). Assume near-constant tempo for electronic music; support tempo
   segments for variable material.
4. **Key** — HPCP-style chromagram correlated against EDM-tuned profiles → best
   `(PitchClass, Mode)`. See "Key detection" below; the naive version is a trap.
5. **Waveform** — preview (low-res, for CDJ overview) + detailed/color bins.
6. **Loudness/peak** — peak and integrated loudness for gain hints.

## Canonical Key model & Camelot mapping

Store keys as canonical `(PitchClass 0..11 = C..B, Mode = Major|Minor)`. Render on
demand. Camelot is the default display. Mapping (also used to fill rekordbox's
Open Key labels on export):

| Camelot | Key (classical) | Open Key |
|---------|-----------------|----------|
| 1A | A♭ minor / G♯m | 6m |
| 2A | E♭ minor       | 7m |
| 3A | B♭ minor       | 8m |
| 4A | F minor        | 9m |
| 5A | C minor        | 10m |
| 6A | G minor        | 11m |
| 7A | D minor        | 12m |
| 8A | A minor        | 1m |
| 9A | E minor        | 2m |
| 10A| B minor        | 3m |
| 11A| F♯ minor       | 4m |
| 12A| D♭ minor / C♯m | 5m |
| 1B | B major        | 6d |
| 2B | F♯ major / G♭  | 7d |
| 3B | D♭ major / C♯  | 8d |
| 4B | A♭ major / G♯  | 9d |
| 5B | E♭ major       | 10d |
| 6B | B♭ major       | 11d |
| 7B | F major        | 12d |
| 8B | C major        | 1d |
| 9B | G major        | 2d |
| 10B| D major        | 3d |
| 11B| A major        | 4d |
| 12B| E major        | 5d |

`A` = minor, `B` = major. Camelot wheel: ±1 number = adjacent (compatible);
same number A↔B = relative major/minor. This drives harmonic-mixing features.

## Key detection (the hard part — lessons learned)

Naive chroma + Krumhansl-Schmuckler profiles FAILS on this material in two stages,
both observed here:

1. **Major skew + relative-minor confusion.** Krumhansl/Sha'ath profiles come from
   classical probe-tone studies and barely separate a key from its relative minor.
   Fix: use **`edma` profiles** (Faraldo et al., corpus-derived from EDM — they beat
   Krumhansl/Sha'ath on this repertoire), plus a small **minor mode bias** since EDM
   skews minor (their tunable "mode bias"). A genuinely ambiguous track can use a
   "majmin" tiebreak profile (Essentia `useMajMin`) — not yet implemented.
2. **Flat, unresolvable chroma.** Summing *every* FFT bin (and log-compressing) lets
   broadband/percussive energy smear across pitch classes; the chroma goes flat
   (peakedness ~1.3) and the profile's own shape, not the audio, picks the tonic —
   producing a single "attractor" key across the whole library. Fix: **HPCP-style
   spectral peak-picking** — only local maxima above ~0.1×frame-max contribute, with
   parabolic interpolation for sub-bin frequency, a 4096 FFT for low-end resolution,
   and per-frame L1 normalization. This raised peakedness to ~2–4 and spread keys
   correctly across the wheel.

3. **Chroma band floor was cutting the actual roots.** The pitched band started at
   110 Hz (A2) to dodge the kick — but much techno's *tonic fundamental* lives in the
   F2–A2 octave (87–110 Hz). Excluding it left only the fifth and upper harmonics, so
   the detector locked onto the dominant (a perfect fifth away → a wrong Camelot
   *number*). Fix: **drop `F_MIN` to 90 Hz.** This single change was the biggest gain
   on the labelled set (21%→34% exact). Going lower (≤70 Hz) re-admits kick/sub smear.
4. **Off-A440 masters smear the tonic.** A track tuned a few cents off (or pitched for
   a remix) puts every peak between two semitone bins, splitting the chroma and
   shifting the detected tonic. Fix: **per-track tuning correction** — the
   magnitude-weighted *circular* mean of each peak's distance from equal temperament
   gives a global semitone offset, subtracted before binning (what rekordbox/Essentia
   do). And **minor mode bias 1.20** (was 1.05): EDM skews minor and this recovers the
   *parallel*-major flips (e.g. F minor read as F major = +3 Camelot number, A→B side)
   without over-calling minor (74/79 minor vs rekordbox's 71/79).

What did NOT help on this set: harmonic/sub-harmonic folding of peaks (each peak
contributing to f/2, f/3… as candidate fundamentals) *lowered* accuracy — it adds
spurious energy to the fourth/twelfth-below. Don't reintroduce without measuring.

Diagnostics:
- `cargo test -p ordnung-core --test chroma_debug --release -- --ignored --nocapture`
  prints each track's chroma + peakedness. Peakedness near 1.0 = broken; 2+ = healthy.
- `cargo test -p ordnung-core --test key_eval --release -- --ignored --nocapture` is
  the **accuracy regression test**: runs production `key::detect` over the 79-track
  labelled set (rekordbox ground truth in KEY_CHECK.md) and asserts exact/compatible
  rates hold. It prints a per-track E/r/a/X breakdown — use it to calibrate any
  chroma/profile/band change. Current floor (analyzer v9): **27/79 exact (34%), 40
  compatible (50%)**.

Still open (Phase 2.1): the majmin tiebreak (Essentia `useMajMin`) for the major-key
tracks the minor bias now costs us (~8/79); harmonic-weighted HPCP done *right* to
break the residual fifth/dominant confusion (the dominant remaining miss class). The
Camelot *number* is still the harder thing to get right; relative major/minor are
harmonically compatible anyway.

## Cache contract

- Key the cache by **content hash** of the decoded source (so re-tagging or moving a
  file doesn't trigger re-analysis) plus `analyzer_version`.
- Bump `analyzer_version` whenever an algorithm changes; that invalidates stale
  results automatically. `analyze` only recomputes when missing or `--force`.
- Cache + results persist in the SQLite catalog, not loose files.

## Accuracy discipline

- Keep a small **labeled test set** (tracks with known BPM/key) and assert detection
  stays within tolerance in tests; treat regressions as bugs.
- Common failure modes to guard: BPM octave errors (½×/2×), key relative-major/minor
  confusion. Prefer correcting these explicitly over silently guessing.
- Never silently "fix" user-provided values; detection fills MISSING data unless the
  user explicitly forces re-analysis (see the explicit-only rule in `ordnung-architecture`).
