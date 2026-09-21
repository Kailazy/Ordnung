---
name: audio-analysis
description: Algorithms and contracts for Ordnung's audio analysis — BPM/tempo detection, beatgrid generation, musical key detection, the canonical Camelot key mapping, waveform/loudness, and the analysis cache. Use when working on the ordnung-core analysis module, tuning detection accuracy, or anything touching keys/BPM/beatgrids/waveforms.
---

# Ordnung audio analysis (ordnung-core/analysis)

Pure-Rust DSP (`symphonia` to decode → samples; `rustfft` for spectra). Engines are
stateless and parallelizable with `rayon`. Every result is cached.

## Pipeline per track

The live pipeline (analyzer **v26**) emits **BPM, a static beatgrid with downbeats,
key, waveform, and loudness**.

1. **Decode** to mono f32 PCM at a known rate (e.g. downmix; 44.1 kHz) via symphonia.
2. **BPM / tempo** (`tempo::detect`, re-enabled v16) — spectral-flux onset envelope →
   autocorrelation + harmonic comb with a log-Gaussian club-tempo prior (70–185 BPM,
   octave and 3:2/5:4 metrical correction). `tempo::lock_grid` then refines the
   period against the *full* track with a **drift fit** (v26): the ~1.5 ms kick
   flux is folded into one beat per 20 s chunk, consecutive chunks are aligned
   by circular cross-correlation, and a line through the accumulated shift is
   the period error (two passes; distrusted when the residual exceeds 0.04
   beat or the correction 0.1%). On the reference set every BPM matches
   rekordbox to 0.01. Do NOT go back to a comb score pivoted on the anchor:
   with the anchor off the transient it slopes to its search boundary and
   extrapolates up to 70 ms of creep over a track (that was v21–v25).
3. **Beatgrid** — a constant-tempo (static) grid anchored by the beat-aware snap
   (v22, reworked v26): sub/high/full-band RMS envelopes folded into one average
   beat; strong rising edges become candidate feet; each candidate is scored
   by its **bump height in the sub and full bands over 0.15 beat** (peak after
   the foot minus the floor before it). The high band gets no vote and there
   is no sustain penalty — both let offbeat hats beat a soft kick. The line
   lands at the winning bump's foot: walk back down the full-band slope from
   the peak (up to 0.2 beat) to the 15% crossing, i.e. the kick's attack/click.
   `downbeat::detect_phase` (v17/v24) then picks the bar's "1" (kick entrance).
   **Ground truth is local:** `testdata/rekordbox-grids/*.tsv` are rekordbox
   7's own PQTZ grids for 17 `testdata/seeker-sample` tracks; run
   `cargo test -p ordnung-core --test grid_eval --release -- --ignored --nocapture`
   (v26: dev set BPM 17/17, in phase 12/17 — v25 had 4/17; the 50-track
   random holdout in `testdata/rekordbox-grids-holdout`, untouched by tuning,
   scores BPM 50/50, in phase 44/50). The audio for both sets lives in the
   gitignored `testdata/seeker-sample`; the holdout tracks were copied out of
   the user's library on their explicit request. For a miss, run
   `SNAP_DEBUG_FILE=<audio> SNAP_DEBUG_RB_MS=<rb first beat> SNAP_DEBUG_RB_BPM=<rb bpm>
   cargo test -p ordnung-core --release --lib debug_snap_on_real_track -- --ignored --nocapture`:
   it prints the folded profiles the snap voted on, every candidate's scores,
   and a per-chunk phase-drift row for ours vs rekordbox's BPM. The larger
   USB eval (`ordnung-rbdb`'s `downbeat_eval`, 121 tracks) needs the EYEBAGS
   stick mounted. rekordbox's line sits AT the kick's attack (the old "45 ms
   early" reading was an artifact of the v22 foot landing mid-bump). Known
   miss class: dub/click-kick tracks where rekordbox grids a click while the
   sub sits on an offbeat bass note or chord stab (domina, Monolense,
   metapattern, RB208 in the local set) — the folded average beat cannot tell
   those apart; the GUI grid lane's "Put the downbeat on the playhead" is the
   fallback.
4. **Key** — HPCP-style chromagram correlated against EDM-tuned profiles → best
   `(PitchClass, Mode)`. See "Key detection" below; the naive version is a trap.
5. **Waveform** — preview (low-res, for CDJ overview) + detailed/color bins. Spans the
   full track; color bins carry `[low, mid, high, loudness]` (120 Hz / 2 kHz splits).
6. **Loudness/peak** — peak and K-weighted (BS.1770) integrated loudness for gain hints.

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
