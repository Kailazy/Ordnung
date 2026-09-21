//! Tempo (BPM) and beat phase from a spectral-flux onset envelope.
//!
//! High-resolution, octave-robust pipeline:
//!
//! 1. **Onset envelope** — log-compressed spectral flux (positive bin-to-bin
//!    increases of `ln(1+magnitude)`), then local-mean subtraction so loud bass
//!    and slow drift don't drown the transients.
//! 2. **Coarse period** — mean-removed autocorrelation scored by a harmonic comb
//!    (a true beat period peaks at its multiples) and weighted by a log-Gaussian
//!    tempo prior centred on club tempi. The comb + prior resolve the ½×/2×
//!    octave ambiguity that fools a bare autocorrelation peak. Parabolic
//!    interpolation lifts the integer-lag peak to a fractional period.
//! 3. **Fine period + phase** — a fractional-period comb is phase-locked against
//!    the whole onset envelope (linearly interpolated) and the period is fine-
//!    searched around the coarse estimate. Aligning hundreds of beats over the
//!    track pins the tempo to well under 0.5 BPM and yields the first downbeat.
//! 4. **Sample-level grid lock** ([`lock_grid`]) — a kick-weighted onset flux at
//!    ~1.5 ms resolution over the *full* track refines the period to sub-0.001
//!    BPM (a static grid extrapolates any residual into audible creep), then the
//!    snap folds sub/high/full-band RMS envelopes into one average beat and
//!    anchors at the *foot* of the most kick-like rising edge — see the
//!    `EDGE_*`/`VOTE_*` constants for why flux-max alone grids off the beat.

use super::dsp::Spectrogram;

/// Plausible BPM search range.
const BPM_MIN: f32 = 70.0;
const BPM_MAX: f32 = 185.0;

/// Log-Gaussian tempo prior: centre and width (in octaves). Disambiguates octave
/// errors by pulling toward the band where most DJ material actually sits, without
/// hard-clamping genuinely fast/slow tracks.
const TEMPO_BIAS_BPM: f32 = 125.0;
const TEMPO_BIAS_OCTAVES: f32 = 0.65;

/// Number of harmonics summed by the comb when scoring a candidate period.
const COMB_HARMONICS: usize = 4;

/// Fine search half-width around the coarse BPM, and its step.
const REFINE_FRAC: f32 = 0.035;
const REFINE_STEPS: usize = 320;

/// Compression applied before the flux: `ln(1 + GAMMA * magnitude)`.
const GAMMA: f32 = 1.0;

pub struct TempoResult {
    pub bpm: f32,
    /// First detected beat position, in milliseconds.
    pub beat_offset_ms: u64,
    /// Salience of the chosen period (peak comb score / median), clamped to [0, 1].
    /// ~0.5+ is a confident lock; near 0 means weak/ambiguous tempo.
    pub confidence: f32,
}

/// Spectral-flux onset strength per frame: positive changes of the log-compressed
/// magnitude, then local-mean subtracted and half-wave rectified.
pub fn onset_envelope(spec: &Spectrogram) -> Vec<f32> {
    if spec.is_empty() {
        return Vec::new();
    }
    let mut env = Vec::with_capacity(spec.len());
    env.push(0.0);
    // Reuse last frame's log magnitudes instead of recomputing the ln twice.
    let mut prev_log: Vec<f32> = spec.frame(0)
        .iter()
        .map(|&m| (1.0 + GAMMA * m).ln())
        .collect();
    for frame in spec.frames().skip(1) {
        let mut flux = 0.0;
        for (b, &m) in frame.iter().enumerate() {
            let c = (1.0 + GAMMA * m).ln();
            let d = c - prev_log[b];
            if d > 0.0 {
                flux += d;
            }
            prev_log[b] = c;
        }
        env.push(flux);
    }
    // Remove DC / slow drift with a simple moving-average subtraction.
    subtract_moving_average(&mut env, 16);
    env
}

pub fn detect(spec: &Spectrogram) -> TempoResult {
    let env = onset_envelope(spec);
    let frame_rate = spec.frame_rate();

    let lag_min = (60.0 / BPM_MAX * frame_rate).floor().max(1.0) as usize;
    let lag_max = (60.0 / BPM_MIN * frame_rate).ceil() as usize;
    if env.len() <= lag_max + 2 {
        // Not enough material to estimate a tempo confidently.
        return TempoResult {
            bpm: 0.0,
            beat_offset_ms: 0,
            confidence: 0.0,
        };
    }

    // Mean-removed autocorrelation (removes the DC pedestal that flattens the ACF
    // of a non-negative envelope), normalized so acf[0] == 1.
    let mean = env.iter().sum::<f32>() / env.len() as f32;
    let zero_mean: Vec<f32> = env.iter().map(|&x| x - mean).collect();
    let max_acf_lag = (lag_max * COMB_HARMONICS).min(zero_mean.len() - 1);
    let acf = autocorrelation(&zero_mean, max_acf_lag);

    // Harmonic-comb score, weighted by the tempo prior, over the candidate periods.
    let hi = lag_max.min(env.len() - 1);
    let mut scores = vec![0.0f32; hi + 1];
    let mut best_lag = lag_min;
    let mut best_score = f32::MIN;
    for lag in lag_min..=hi {
        let bpm = 60.0 * frame_rate / lag as f32;
        let s = comb_score(&acf, lag) * tempo_prior(bpm);
        scores[lag] = s;
        if s > best_score {
            best_score = s;
            best_lag = lag;
        }
    }

    // Sub-lag refinement of the peak by parabolic interpolation on the comb score.
    let coarse_lag = parabolic_peak(&scores, best_lag, lag_min, hi);
    let coarse_bpm = (60.0 * frame_rate / coarse_lag).clamp(BPM_MIN, BPM_MAX);

    // Fine period + phase: phase-lock a fractional comb against the whole envelope.
    let (bpm0, phase0) = refine(&env, frame_rate, coarse_bpm);
    // Metrical correction: the autocorrelation comb can settle on a *slower* fold of
    // the true tempo (2/3× → a "3:2/triplet" read, 4/5× → "5:4"), because that fold's
    // comb teeth still hit every 2nd/3rd true beat. The true, faster tempo phase-locks
    // with more energy per tap (all taps on beats, not alternating on/off), so promote
    // a faster metrical relative when its aligned energy clearly beats the current lock.
    let (bpm, phase_frames) = correct_metrical(&env, frame_rate, bpm0, phase0);
    // Frame → time at the window *centre* (see `dsp::frame_to_ms`); the envelope
    // index is not a timestamp on its own.
    let beat_offset_ms = super::dsp::frame_to_ms(phase_frames, spec.sample_rate())
        .round()
        .max(0.0) as u64;

    TempoResult {
        bpm: (bpm * 100.0).round() / 100.0,
        beat_offset_ms,
        confidence: confidence(&scores, lag_min, hi, best_score),
    }
}

/// Log-Gaussian weight: 1.0 at `TEMPO_BIAS_BPM`, falling off over octaves.
fn tempo_prior(bpm: f32) -> f32 {
    if bpm <= 0.0 {
        return 0.0;
    }
    let z = (bpm / TEMPO_BIAS_BPM).log2() / TEMPO_BIAS_OCTAVES;
    (-0.5 * z * z).exp()
}

/// Sum of the normalized autocorrelation at a period and its first harmonics; the
/// true beat period scores higher than its ½× (which misses the fundamental) or
/// 2× (which only sees a subset of the comb teeth).
fn comb_score(acf: &[f32], period: usize) -> f32 {
    if period == 0 {
        return 0.0;
    }
    let mut s = 0.0;
    for k in 1..=COMB_HARMONICS {
        let lag = period * k;
        if lag >= acf.len() {
            break;
        }
        s += acf[lag] / k as f32;
    }
    s
}

/// Normalized autocorrelation `acf[0..=max_lag]`, with `acf[0] == 1`.
fn autocorrelation(x: &[f32], max_lag: usize) -> Vec<f32> {
    let n = x.len();
    let mut acf = vec![0.0f32; max_lag + 1];
    for (lag, slot) in acf.iter_mut().enumerate() {
        let mut sum = 0.0;
        for i in lag..n {
            sum += x[i] * x[i - lag];
        }
        *slot = sum;
    }
    let z = acf[0];
    if z > 0.0 {
        for v in acf.iter_mut() {
            *v /= z;
        }
    }
    acf
}

/// Parabolic interpolation of the peak at `peak` against its neighbours, returning
/// a fractional lag. Falls back to the integer peak at the search boundary.
fn parabolic_peak(scores: &[f32], peak: usize, lo: usize, hi: usize) -> f32 {
    if peak <= lo || peak >= hi {
        return peak as f32;
    }
    let (sm1, s0, sp1) = (scores[peak - 1], scores[peak], scores[peak + 1]);
    let denom = sm1 - 2.0 * s0 + sp1;
    if denom.abs() < f32::EPSILON {
        return peak as f32;
    }
    let delta = (0.5 * (sm1 - sp1) / denom).clamp(-1.0, 1.0);
    peak as f32 + delta
}

/// Fine-search the period around `coarse_bpm` by phase-locking a fractional-period
/// comb to the envelope; returns `(bpm, phase_in_frames)` of the best alignment.
fn refine(env: &[f32], frame_rate: f32, coarse_bpm: f32) -> (f32, f32) {
    let lo = (coarse_bpm * (1.0 - REFINE_FRAC)).max(BPM_MIN);
    let hi = (coarse_bpm * (1.0 + REFINE_FRAC)).min(BPM_MAX);

    let mut best_bpm = coarse_bpm;
    let mut best_energy = f32::MIN;
    for k in 0..=REFINE_STEPS {
        let bpm = lo + (hi - lo) * k as f32 / REFINE_STEPS as f32;
        let period = 60.0 / bpm * frame_rate;
        // Coarse 1-frame phase grid is enough to rank periods.
        let (_, energy) = comb_align(env, period, 1.0);
        if energy > best_energy {
            best_energy = energy;
            best_bpm = bpm;
        }
    }

    // Pin the phase at the winning period with a finer (¼-frame) grid.
    let period = 60.0 / best_bpm * frame_rate;
    let (phase, _) = comb_align(env, period, 0.25);
    (best_bpm, phase)
}

/// Faster metrical relatives to test against the detected tempo. `2/3` and `4/5`
/// folds are what the comb settles on, so their inverses (`3/2`, `5/4`) plus the
/// neighbouring `4/3` and the octave `2` recover the true tempo.
const METRICAL_MULTIPLES: [f32; 4] = [5.0 / 4.0, 4.0 / 3.0, 3.0 / 2.0, 2.0];

/// A faster relative must beat the current lock's aligned-energy×prior score by this
/// margin to win — so only a *clearly* better tempo overrides, leaving correct locks
/// (and genuinely slow tracks) untouched.
const OVERRIDE_MARGIN: f32 = 1.05;

/// Promote a faster metrical relative of `(bpm, phase)` when it phase-locks with more
/// energy per tap (weighted by the tempo prior). Returns the chosen `(bpm, phase)`.
fn correct_metrical(env: &[f32], frame_rate: f32, bpm: f32, phase: f32) -> (f32, f32) {
    let aligned_energy = |b: f32| comb_align(env, 60.0 / b * frame_rate, 0.25).1;
    let base_score = aligned_energy(bpm) * tempo_prior(bpm);

    let mut best = (bpm, phase);
    let mut best_score = base_score;
    for m in METRICAL_MULTIPLES {
        let cand = bpm * m;
        if cand > BPM_MAX || cand < BPM_MIN {
            continue;
        }
        // Fine-lock period + phase around the relative, then score its alignment.
        let (cbpm, cphase) = refine(env, frame_rate, cand);
        let score = aligned_energy(cbpm) * tempo_prior(cbpm);
        // Compare every relative against the *original* lock, not the running best, so
        // one clear winner is required rather than a chain of marginal step-ups.
        if score > base_score * OVERRIDE_MARGIN && score > best_score {
            best = (cbpm, cphase);
            best_score = score;
        }
    }
    best
}

/// Best phase of a comb with `period` frames over the envelope, scanning phases in
/// `phase_step`-frame increments. Returns `(phase_frames, mean_energy_per_tap)`.
fn comb_align(env: &[f32], period: f32, phase_step: f32) -> (f32, f32) {
    let n = env.len();
    if period < 1.0 || n == 0 {
        return (0.0, 0.0);
    }
    let mut best_phase = 0.0f32;
    let mut best_energy = f32::MIN;
    let mut phase = 0.0;
    while phase < period {
        let mut sum = 0.0;
        let mut count = 0u32;
        let mut x = phase;
        while x < (n - 1) as f32 {
            sum += lerp(env, x);
            count += 1;
            x += period;
        }
        let energy = if count > 0 { sum / count as f32 } else { 0.0 };
        if energy > best_energy {
            best_energy = energy;
            best_phase = phase;
        }
        phase += phase_step;
    }
    (best_phase, best_energy)
}

/// Linear interpolation of the envelope at fractional index `x`.
fn lerp(env: &[f32], x: f32) -> f32 {
    if x <= 0.0 {
        return env[0];
    }
    let i = x as usize;
    if i + 1 >= env.len() {
        return *env.last().unwrap_or(&0.0);
    }
    let f = x - i as f32;
    env[i] * (1.0 - f) + env[i + 1] * f
}

/// Peak-to-median comb-score ratio, squashed into [0, 1] as a confidence value.
fn confidence(scores: &[f32], lo: usize, hi: usize, peak: f32) -> f32 {
    if hi <= lo || peak <= 0.0 {
        return 0.0;
    }
    let mut vals: Vec<f32> = scores[lo..=hi].iter().copied().filter(|v| *v > 0.0).collect();
    if vals.is_empty() {
        return 0.0;
    }
    vals.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = vals[vals.len() / 2];
    if median <= 0.0 {
        return 1.0;
    }
    // ratio 1 → 0 confidence, large ratio → ~1. ratio of 3 maps to ~0.5.
    let ratio = peak / median;
    (1.0 - (1.0 / ratio)).clamp(0.0, 1.0)
}

fn subtract_moving_average(env: &mut [f32], radius: usize) {
    if env.is_empty() {
        return;
    }
    let n = env.len();
    let smoothed: Vec<f32> = (0..n)
        .map(|i| {
            let lo = i.saturating_sub(radius);
            let hi = (i + radius + 1).min(n);
            let s: f32 = env[lo..hi].iter().sum();
            s / (hi - lo) as f32
        })
        .collect();
    for i in 0..n {
        env[i] = (env[i] - smoothed[i]).max(0.0);
    }
}

// --- Grid locking against the samples --------------------------------------
//
// The comb lock above is derived from a 4096-sample STFT over the analysis
// window: it pins the *period* to a fraction of a BPM, but its phase can only
// ever be as sharp as a hop, and spectral flux peaks while a transient's energy
// is still climbing into the window — so the anchor lands tens of milliseconds
// early even after the window-centre correction. And the fine period search is
// a quantized grid (~0.03 BPM steps at club tempo) whose residual error, plus
// the 0.01 BPM display rounding, extrapolates into visible creep on a static
// grid: 0.02 BPM off at 124 puts the lines ~50 ms late by the end of a
// six-minute track. Invisible on the overview, glaring on the zoom lane.
//
// So the last step leaves the spectrogram behind: re-derive an onset envelope
// from the samples at ~1.5 ms resolution over the *whole* track, fine-search
// the period against every beat of it (pivoting at the anchor, so a phase
// error doesn't bias the period), then slide the anchor (period fixed) to
// wherever the beats collect the most transient energy.

/// Block hop / length of the fine onset envelope, in samples at the track's own
/// rate. 64/256 at 44.1 kHz is ~1.5 ms resolution over a ~5.8 ms window — short
/// enough to localize an attack, long enough not to chase a 60 Hz kick's own
/// waveform.
const SNAP_HOP: usize = 64;
const SNAP_WIN: usize = 256;

/// Kick band for the snap. Weighted over the broadband flux because a DJ reads
/// the grid against the kick, and because hi-hats sit on the off-beats often
/// enough to drag a broadband-only snap half a beat sideways.
const SNAP_LOW_HZ: f32 = 200.0;
const SNAP_BROADBAND_WEIGHT: f32 = 0.35;

/// Phase search step, in milliseconds.
const SNAP_STEP_MS: f32 = 0.5;

/// Full-track period refine (see [`refine_period_env`]): the flux is folded
/// into one beat per [`DRIFT_CHUNK_MS`] chunk at [`DRIFT_BINS`] bins, each
/// chunk's fold is aligned to the previous one by circular cross-correlation,
/// and a line through the accumulated shifts gives the period error. Needs
/// [`DRIFT_MIN_CHUNKS`] chunks; a fit whose residual exceeds
/// [`DRIFT_MAX_RESIDUAL_BT`] of a beat, or whose correction exceeds
/// [`DRIFT_MAX_FRAC`] of the period (a whole-beat slip over the track is
/// ~0.13%), is distrusted and the comb's BPM kept.
const DRIFT_CHUNK_MS: f64 = 20_000.0;
const DRIFT_BINS: usize = 256;
const DRIFT_MIN_CHUNKS: usize = 4;
const DRIFT_MAX_RESIDUAL_BT: f64 = 0.04;
const DRIFT_MAX_FRAC: f64 = 0.001;

/// Beat-phase edge detection: rekordbox-style grids sit where the kick's
/// energy bump *begins*, not where onset flux is loudest. A bare flux comb
/// drags the anchor into the bump on soft/sidechained kicks (late by 50–200 ms)
/// or onto louder offbeat percussion entirely — measured against rekordbox's
/// own grids, 93 of 121 club tracks gridded off the beat that way. The snap
/// instead folds the sub- and full-band RMS envelopes into one beat and
/// anchors at a *rising edge* of that folded profile (mean energy in the
/// [`EDGE_FRAC`]-beat window after a phase minus the window before) — the
/// kick's leading foot, which sidechain pumping sharpens further.
///
/// Offbeat chord/bass stabs put a second rising edge half a beat away and can
/// out-sharpen a soft kick, so every strong edge (≥ [`EDGE_KEEP`] of the best)
/// is a candidate and the one whose bump carries the most *sub-band* energy
/// wins — kicks own the sub band; stabs and hats are mid/high-heavy.
///
/// [`EDGE_FULL_WEIGHT`] blends the full-band profile into the sub-band one for
/// edge *timing* (sub alone rises mushily); [`EDGE_MIN_STEP`] is the trust
/// gate: the winning edge must step by at least this fraction of the profile's
/// mean or the coarse anchor is kept (flat material has no edge worth
/// inventing).
const EDGE_FRAC: f64 = 0.10;
const EDGE_FULL_WEIGHT: f32 = 0.5;
const EDGE_MIN_STEP: f32 = 0.10;
const EDGE_KEEP: f32 = 0.15;
/// Sub-band low-pass for the kick-vs-stab vote: cutoff (Hz) and pole count.
const SUB_HZ: f32 = 60.0;
const SUB_POLES: u32 = 2;
/// High band (one-pole high-pass) for the beat-vs-offbeat vote: hats and
/// claps live here, and so does the kick's click.
const CLAP_HZ: f32 = 2_000.0;
/// The vote reads each candidate's *bump height* in the sub and full bands:
/// the peak within [`RISE_BT`] of a beat after the foot minus the floor just
/// before it. The window is as long as a kick's sub energy takes to develop
/// (~70 ms at club tempo) and no longer — a wider one let a weak edge
/// sitting just before the kick claim the kick's own bump. Offbeat hats
/// raise only the high band, which deliberately gets NO vote: any weight on
/// it lets an open hat outvote a soft kick (measured on the rekordbox
/// reference set). The sub band decides for anything with a kick in it; the
/// full band carries clicky, sub-light kicks and breaks the tie against an
/// offbeat bass note that matches the kick's sub.
const RISE_BT: f64 = 0.15;
const VOTE_SUB_WEIGHT: f32 = 1.0;
const VOTE_FULL_WEIGHT: f32 = 1.0;
/// How far before the winning edge the foot refine may walk back, in beats:
/// a soft kick's sub swell (which wins the edge) can trail its own click by
/// ~50 ms, and the line belongs on the click.
const FOOT_BACK_BT: f64 = 0.20;
/// The sample-level envelopes shared by the anchor snap and period refine:
/// kick-weighted onset flux plus the low- and full-band RMS it derives from,
/// all at [`SNAP_HOP`] resolution, addressed in milliseconds.
struct FluxEnv {
    flux: Vec<f32>,
    /// Mean-normalized sub-, high- and full-band RMS envelopes (same index grid).
    sub_rms: Vec<f32>,
    hi_rms: Vec<f32>,
    full_rms: Vec<f32>,
    /// Milliseconds per envelope index, and the offset of index 0. A flux
    /// value is the energy *change* across the step, so it belongs midway
    /// between the two windows' centres; an RMS block belongs at its centre.
    ms_per: f64,
    ms_0: f64,
    ms_0_energy: f64,
}

impl FluxEnv {
    fn new(samples: &[f32], sample_rate: u32) -> Option<FluxEnv> {
        if sample_rate == 0 || samples.len() < SNAP_WIN * 4 {
            return None;
        }
        let sr = sample_rate as f32;
        let (flux, sub_rms, hi_rms, full_rms) = onset_flux(samples, sr);
        if flux.len() < 8 {
            return None;
        }
        Some(FluxEnv {
            flux,
            sub_rms,
            hi_rms,
            full_rms,
            ms_per: SNAP_HOP as f64 / sr as f64 * 1000.0,
            ms_0: (SNAP_WIN as f64 / 2.0 - SNAP_HOP as f64 / 2.0) / sr as f64 * 1000.0,
            ms_0_energy: SNAP_WIN as f64 / 2.0 / sr as f64 * 1000.0,
        })
    }

    fn span_ms(&self) -> f64 {
        self.flux.len() as f64 * self.ms_per + self.ms_0
    }

    /// Fold an RMS envelope into one beat of `period_ms` over `n_bins` phase
    /// bins: the track's average beat as an energy profile. Bins are per-bin
    /// means so sparse coverage can't tilt the profile, and any bin no block
    /// ever landed in is filled by linear interpolation from its nearest
    /// filled neighbours (circularly) — callers should also size `n_bins` with
    /// [`FluxEnv::beat_bins`], because a beat that is a whole number of hops
    /// long (48 kHz at 125 BPM is exactly 360) only ever visits that many
    /// phases, and the walk-back foot refine reads an empty bin as a floor.
    fn folded(&self, env: &[f32], period_ms: f64, n_bins: usize) -> Vec<f32> {
        let mut sum = vec![0.0f64; n_bins];
        let mut cnt = vec![0u32; n_bins];
        for (i, &v) in env.iter().enumerate() {
            let ms = self.ms_0_energy + i as f64 * self.ms_per;
            let ph = (ms / period_ms).fract();
            let b = ((ph * n_bins as f64) as usize).min(n_bins - 1);
            sum[b] += v as f64;
            cnt[b] += 1;
        }
        let mut out: Vec<f32> = sum
            .iter()
            .zip(&cnt)
            .map(|(&s, &c)| if c == 0 { f32::NAN } else { (s / c as f64) as f32 })
            .collect();
        fill_gaps(&mut out);
        out
    }

    /// Phase bins for a folded beat: [`SNAP_STEP_MS`] resolution, but never
    /// finer than one hop of the envelope (see [`FluxEnv::folded`]).
    fn beat_bins(&self, period_ms: f64) -> usize {
        let step = (SNAP_STEP_MS as f64).max(self.ms_per);
        (period_ms / step).round().max(8.0) as usize
    }
}

/// Replace every NaN in the circular profile `p` by the linear interpolation
/// between its nearest non-NaN neighbours. Leaves `p` untouched when nothing
/// is finite.
fn fill_gaps(p: &mut [f32]) {
    let n = p.len();
    if n == 0 || !p.iter().any(|v| v.is_finite()) {
        return;
    }
    let src = p.to_vec();
    for b in 0..n {
        if src[b].is_finite() {
            continue;
        }
        let mut lo = b;
        let mut dl = 0;
        while !src[lo].is_finite() {
            lo = (lo + n - 1) % n;
            dl += 1;
        }
        let mut hi = b;
        let mut dh = 0;
        while !src[hi].is_finite() {
            hi = (hi + 1) % n;
            dh += 1;
        }
        let t = dl as f32 / (dl + dh) as f32;
        p[b] = src[lo] * (1.0 - t) + src[hi] * t;
    }
}

/// Snap the anchor onto the beat phase (see `snap_anchor_env`), keeping `bpm`
/// fixed. The result is always the phase's FIRST instance in the track (within
/// one period of 0), so the static grid covers every beat from the start.
/// Keeps `coarse_ms`'s phase when the audio gives the search nothing to lock
/// onto.
pub fn snap_anchor(samples: &[f32], sample_rate: u32, bpm: f32, coarse_ms: u64) -> u64 {
    if bpm <= 0.0 {
        return coarse_ms;
    }
    match FluxEnv::new(samples, sample_rate) {
        Some(env) => snap_anchor_env(&env, bpm, coarse_ms),
        None => coarse_ms,
    }
}

/// One candidate foot from the snap's vote, with the numbers that ranked it
/// (read by the diagnostic test only).
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(test), allow(dead_code))]
struct SnapCandidate {
    /// Phase bin of the rising edge (of `SnapProfile::n` per beat).
    bin: usize,
    edge: f32,
    sub_rise: f32,
    full_rise: f32,
    hi_rise: f32,
    score: f32,
}

/// The snap's folded beat profiles and its decision, kept together so the
/// diagnostic test (`debug_snap_on_real_track`) can print exactly what the
/// production snap saw.
#[cfg_attr(not(test), allow(dead_code))]
struct SnapProfile {
    /// Phase bins per beat; bin `b` covers phase `b / n` of a beat, counted
    /// from track time 0 modulo the period.
    n: usize,
    p_sub: Vec<f32>,
    p_hi: Vec<f32>,
    p_full: Vec<f32>,
    p_edge: Vec<f32>,
    cands: Vec<SnapCandidate>,
    /// `None` when no edge was worth trusting (flat material).
    best_bin: Option<usize>,
    foot: usize,
    /// Foot refine internals (diagnostics): floor, peak, threshold, peak bin.
    foot_dbg: (f32, f32, f32, usize),
}

fn snap_anchor_env(env: &FluxEnv, bpm: f32, coarse_ms: u64) -> u64 {
    let period_ms = 60_000.0 / bpm as f64;
    let prof = snap_profile(env, bpm);
    if prof.best_bin.is_none() {
        // No edge worth trusting (flat / transient-free): keep the coarse
        // phase, but still pull it into the first period so the grid spans
        // the whole track.
        let ms = coarse_ms as f64;
        return (ms - (ms / period_ms).floor() * period_ms).round() as u64;
    }
    // Anchor at the winning phase's FIRST instance in the track: the grid
    // extrapolates forward only, so an anchor even one beat in leaves the
    // track's real first beat with no grid line (and shifts every bar
    // number). rekordbox does the same — its grids start within the first
    // period (a line may land in intro silence; that's what a static grid
    // over the whole track means).
    let phase_ms = prof.foot as f64 / prof.n as f64 * period_ms;
    let ms = phase_ms - (phase_ms / period_ms).floor() * period_ms;
    ms.round().max(0.0) as u64
}

/// Fold the envelopes into one beat at `bpm`, find the candidate feet and vote
/// (see the `EDGE_*` / `VOTE_*` constants).
fn snap_profile(env: &FluxEnv, bpm: f32) -> SnapProfile {
    let period_ms = 60_000.0 / bpm as f64;
    let n = env.beat_bins(period_ms);
    let p_sub = env.folded(&env.sub_rms, period_ms, n);
    let p_hi = env.folded(&env.hi_rms, period_ms, n);
    let p_full = env.folded(&env.full_rms, period_ms, n);
    // Candidate generation sees every band: a clicky kick that barely dents
    // the full-band RMS still raises a sharp high-band edge.
    let p_edge: Vec<f32> = (0..n)
        .map(|b| p_sub[b] + EDGE_FULL_WEIGHT * (p_full[b] + p_hi[b]))
        .collect();
    let half = ((n as f64 * EDGE_FRAC) as usize).max(1);

    // Circular windowed means via doubled prefix sums.
    let cum = |p: &[f32]| {
        let mut c = Vec::with_capacity(2 * n + 1);
        c.push(0.0f64);
        for i in 0..2 * n {
            c.push(c[i] + p[i % n] as f64);
        }
        c
    };
    let cum_edge = cum(&p_edge);
    let win_mean = |c: &[f64], a: usize, len: usize| ((c[a + len] - c[a]) / len as f64) as f32;

    // Rising-edge strength at every phase of the average beat.
    let edge_at =
        |b: usize| win_mean(&cum_edge, b, half) - win_mean(&cum_edge, b + n - half, half);
    let edges: Vec<f32> = (0..n).map(edge_at).collect();
    let best_edge = edges.iter().cloned().fold(f32::MIN, f32::max);
    let mean_level = win_mean(&cum_edge, 0, n).max(f32::EPSILON);
    let mut prof = SnapProfile {
        n,
        p_sub,
        p_hi,
        p_full,
        p_edge,
        cands: Vec::new(),
        best_bin: None,
        foot: 0,
        foot_dbg: (0.0, 0.0, 0.0, 0),
    };
    if !(best_edge > mean_level * EDGE_MIN_STEP) {
        return prof;
    }

    // Candidate feet: strong edges, greedily kept with ≥ 0.15-beat separation.
    let min_sep = n * 3 / 20;
    let mut order: Vec<usize> = (0..n).collect();
    order.sort_by(|&a, &b| edges[b].partial_cmp(&edges[a]).unwrap());
    let mut cands: Vec<usize> = Vec::new();
    for b in order {
        if edges[b] < best_edge * EDGE_KEEP || cands.len() >= 5 {
            break;
        }
        let sep_ok = cands.iter().all(|&c| {
            let d = (b as i64 - c as i64).rem_euclid(n as i64) as usize;
            d.min(n - d) >= min_sep
        });
        if sep_ok {
            cands.push(b);
        }
    }

    // Beat-vs-offbeat vote: the beat is where the kick's energy arrives, so
    // each candidate is scored by the bump it starts in every band — the
    // peak inside a window after the foot minus the floor just before it.
    // Offbeat hats have no sub bump and barely dent the full band; bass
    // swells and chord stabs raise the sub or mid bands slowly and without a
    // broadband attack. See the `VOTE_*` constants for the weights.
    let rise = |p: &[f32], c: usize, ahead: usize| {
        let peak = (c..=c + ahead).map(|j| p[j % n]).fold(f32::MIN, f32::max);
        let base = (c + n - half..=c + n).map(|j| p[j % n]).fold(f32::MAX, f32::min);
        (peak - base).max(0.0)
    };
    let ahead = ((n as f64 * RISE_BT) as usize).max(1);
    prof.cands = cands
        .iter()
        .map(|&c| {
            let sub_rise = rise(&prof.p_sub, c, ahead);
            let full_rise = rise(&prof.p_full, c, ahead);
            let hi_rise = rise(&prof.p_hi, c, ahead);
            SnapCandidate {
                bin: c,
                edge: edges[c],
                sub_rise,
                full_rise,
                hi_rise,
                score: VOTE_SUB_WEIGHT * sub_rise + VOTE_FULL_WEIGHT * full_rise,
            }
        })
        .collect();
    let best_bin = prof
        .cands
        .iter()
        .max_by(|a, b| a.score.partial_cmp(&b.score).unwrap())
        .map(|c| c.bin)
        .unwrap_or(0);

    // The big edge window plateaus over any placement that fully covers the
    // bump, so refine to the *foot*: find the bump's peak within a window
    // after the winning edge, then walk back down its slope to the last
    // point still above base + 15% of the rise. Walking back from the peak
    // (rather than forward from the window's start) is what keeps a noisy
    // pre-bump floor from producing a spurious early crossing. The crossing
    // is read off the FULL-band profile — that is the amplitude envelope the
    // waveform view draws, so the line lands where the visible slope begins.
    // The sub-weighted profile would read the kick's sub swell, which
    // develops tens of ms after the attack and parks the line mid-bump on
    // soft kicks.
    let p_full = &prof.p_full;
    let back = ((n as f64 * FOOT_BACK_BT) as usize).max(half);
    let floor = (best_bin + n - back..=best_bin + n)
        .map(|j| p_full[j % n])
        .fold(f32::MAX, f32::min);
    // Indices run in the doubled range `[n, 2n)` so the walk back can cross
    // the profile's wrap-around.
    let (peak_at, peak) = (best_bin + n..=best_bin + n + half)
        .map(|j| (j, p_full[j % n]))
        .fold((best_bin + n, f32::MIN), |acc, (j, v)| if v > acc.1 { (j, v) } else { acc });
    let thr = floor + 0.15 * (peak - floor);
    let mut foot = peak_at;
    while foot > best_bin + n - back && p_full[(foot - 1) % n] >= thr {
        foot -= 1;
    }
    let foot = foot % n;
    prof.best_bin = Some(best_bin);
    prof.foot = foot;
    prof.foot_dbg = (floor, peak, thr, peak_at % n);
    prof
}

/// Refine the comb's `bpm` against the whole track by measuring the grid's
/// *drift*: fold the flux into one beat per chunk, align consecutive chunks
/// by cross-correlation, and fit a line through the accumulated phase shift.
/// A period that is slightly long makes every chunk's beat pattern arrive a
/// little later than the last; the slope of that walk is the period error.
/// Aligning whole patterns (not peaks) makes this indifferent to which bump
/// is loudest in a given chunk, and to where the anchor sits — the comb
/// score it replaces pivoted on the anchor and, with the anchor off the
/// transient, sloped to its search boundary instead of peaking. Two passes:
/// the second folds at the corrected period, so its chunks are unsmeared.
/// Returns `bpm` unchanged when the fit is distrusted (see the `DRIFT_*`
/// constants).
fn refine_period_env(env: &FluxEnv, bpm: f32) -> f32 {
    let mut period = 60_000.0 / bpm as f64;
    for _ in 0..2 {
        match drift_fit(env, period) {
            Some(p) => period = p,
            None => return bpm,
        }
    }
    (60_000.0 / period) as f32
}

/// One drift-fit pass at `period` (ms): the corrected period, or `None` when
/// the track is too short or the fit too noisy to trust.
fn drift_fit(env: &FluxEnv, period: f64) -> Option<f64> {
    let n = DRIFT_BINS;
    let span = env.span_ms();
    let n_chunks = (span / DRIFT_CHUNK_MS).floor() as usize;
    if n_chunks < DRIFT_MIN_CHUNKS {
        return None;
    }
    let mut profiles: Vec<Vec<f32>> = Vec::with_capacity(n_chunks);
    let mut centres: Vec<f64> = Vec::with_capacity(n_chunks);
    for c in 0..n_chunks {
        let t0 = c as f64 * DRIFT_CHUNK_MS;
        let t1 = t0 + DRIFT_CHUNK_MS;
        let i0 = ((t0 - env.ms_0) / env.ms_per).max(0.0) as usize;
        let i1 = (((t1 - env.ms_0) / env.ms_per).max(0.0) as usize).min(env.flux.len());
        let mut acc = vec![0.0f32; n];
        for i in i0..i1 {
            let ms = env.ms_0 + i as f64 * env.ms_per;
            let b = ((ms / period).rem_euclid(1.0) * n as f64) as usize % n;
            acc[b] += env.flux[i];
        }
        // Shape only: a level difference between chunks must not drive the fit.
        let mean = acc.iter().sum::<f32>() / n as f32;
        acc.iter_mut().for_each(|v| *v -= mean);
        profiles.push(acc);
        centres.push((t0 + t1) / 2.0);
    }

    // Accumulated shift of each chunk's pattern relative to the first, in
    // bins (positive = later), from a circular cross-correlation with the
    // previous chunk and a parabolic sub-bin peak.
    let mut shifts = vec![0.0f64; n_chunks];
    for c in 1..n_chunks {
        let (a, b) = (&profiles[c - 1], &profiles[c]);
        let xc: Vec<f32> = (0..n)
            .map(|s| (0..n).map(|i| a[i] * b[(i + s) % n]).sum())
            .collect();
        let best = (0..n)
            .max_by(|&x, &y| xc[x].partial_cmp(&xc[y]).unwrap())
            .unwrap_or(0);
        let (sm1, s0, sp1) = (xc[(best + n - 1) % n], xc[best], xc[(best + 1) % n]);
        let denom = sm1 - 2.0 * s0 + sp1;
        let delta = if denom.abs() < f32::EPSILON {
            0.0
        } else {
            (0.5 * (sm1 - sp1) / denom).clamp(-1.0, 1.0)
        };
        let mut shift = best as f64 + delta as f64;
        if shift > n as f64 / 2.0 {
            shift -= n as f64;
        }
        shifts[c] = shifts[c - 1] + shift;
    }

    // Least-squares line through (centre, shift).
    let m = n_chunks as f64;
    let mx = centres.iter().sum::<f64>() / m;
    let my = shifts.iter().sum::<f64>() / m;
    let sxx: f64 = centres.iter().map(|x| (x - mx) * (x - mx)).sum();
    let sxy: f64 = centres.iter().zip(&shifts).map(|(x, y)| (x - mx) * (y - my)).sum();
    if sxx <= 0.0 {
        return None;
    }
    let slope = sxy / sxx; // bins per ms
    let intercept = my - slope * mx;
    let residual = (centres
        .iter()
        .zip(&shifts)
        .map(|(x, y)| {
            let e = y - (intercept + slope * x);
            e * e
        })
        .sum::<f64>()
        / m)
        .sqrt()
        / n as f64;
    if residual > DRIFT_MAX_RESIDUAL_BT {
        return None;
    }
    // Phase walks by `d` beats per ms when the true period is `period·(1 + d·period)`.
    let d = slope / n as f64;
    let frac = d * period;
    if frac.abs() > DRIFT_MAX_FRAC {
        return None;
    }
    Some(period * (1.0 + frac))
}

/// Lock the grid against the full track's samples: refine the comb's `bpm` to
/// sub-0.001 BPM (see [`refine_period_env`]) and slide `coarse_ms` onto the
/// nearest transient at the refined period. One flux envelope serves both.
/// Falls back to the inputs when the audio gives the search nothing.
pub fn lock_grid(samples: &[f32], sample_rate: u32, bpm: f32, coarse_ms: u64) -> (f32, u64) {
    if bpm <= 0.0 {
        return (bpm, coarse_ms);
    }
    let Some(env) = FluxEnv::new(samples, sample_rate) else {
        return (bpm, coarse_ms);
    };
    let bpm = refine_period_env(&env, bpm);
    (bpm, snap_anchor_env(&env, bpm, coarse_ms))
}

/// Fine onset envelope for [`snap_anchor`]: positive change in short-block RMS,
/// computed for a kick-band copy of the signal and for the full band, each
/// normalized to its own mean before they're mixed. Normalizing first is what
/// lets one weight (`SNAP_BROADBAND_WEIGHT`) hold across a bass-heavy techno
/// track and a thin, kickless intro alike. Also returns the mean-normalized
/// sub-band ([`SUB_HZ`], [`SUB_POLES`]) and full-band RMS envelopes, which the
/// snap's folded profiles read.
fn onset_flux(samples: &[f32], sr: f32) -> (Vec<f32>, Vec<f32>, Vec<f32>, Vec<f32>) {
    let n_blocks = samples.len().saturating_sub(SNAP_WIN) / SNAP_HOP + 1;
    let mut rms_lo = Vec::with_capacity(n_blocks);
    let mut rms_sub = Vec::with_capacity(n_blocks);
    let mut rms_hi = Vec::with_capacity(n_blocks);
    let mut rms_full = Vec::with_capacity(n_blocks);

    // One-pole low-pass (a = 1 - e^{-2π fc/sr}) run once over the signal; the
    // block loop below reads the filtered copy.
    let a = 1.0 - (-std::f32::consts::TAU * SNAP_LOW_HZ / sr).exp();
    let mut lp = 0.0f32;
    let low: Vec<f32> = samples
        .iter()
        .map(|&s| {
            lp += a * (s - lp);
            lp
        })
        .collect();

    // Steeper sub-band copy for the kick-vs-stab vote: a leaky one-pole lets
    // loud mid stabs bleed into the "low" band; two poles at SUB_HZ keep the
    // sub band the kick's own.
    let a_sub = 1.0 - (-std::f32::consts::TAU * SUB_HZ / sr).exp();
    let mut sub = samples.to_vec();
    for _ in 0..SUB_POLES {
        let mut lp = 0.0f32;
        for x in &mut sub {
            lp += a_sub * (*x - lp);
            *x = lp;
        }
    }

    // High band (clap/snare) for the backbeat vote: one-pole high-pass.
    let a_hi = 1.0 - (-std::f32::consts::TAU * CLAP_HZ / sr).exp();
    let mut lp_hi = 0.0f32;
    let hi: Vec<f32> = samples
        .iter()
        .map(|&s| {
            lp_hi += a_hi * (s - lp_hi);
            s - lp_hi
        })
        .collect();

    let mut pos = 0;
    while pos + SNAP_WIN <= samples.len() {
        let mut sl = 0.0f64;
        let mut ss = 0.0f64;
        let mut sh = 0.0f64;
        let mut sf = 0.0f64;
        for i in pos..pos + SNAP_WIN {
            sl += (low[i] * low[i]) as f64;
            ss += (sub[i] * sub[i]) as f64;
            sh += (hi[i] * hi[i]) as f64;
            sf += (samples[i] * samples[i]) as f64;
        }
        let inv = 1.0 / SNAP_WIN as f64;
        rms_lo.push((sl * inv).sqrt() as f32);
        rms_sub.push((ss * inv).sqrt() as f32);
        rms_hi.push((sh * inv).sqrt() as f32);
        rms_full.push((sf * inv).sqrt() as f32);
        pos += SNAP_HOP;
    }

    let rise = |xs: &[f32]| -> Vec<f32> {
        let mut v = vec![0.0f32; xs.len()];
        for i in 1..xs.len() {
            v[i] = (xs[i] - xs[i - 1]).max(0.0);
        }
        let mean = v.iter().sum::<f32>() / v.len().max(1) as f32;
        if mean > 0.0 {
            for x in &mut v {
                *x /= mean;
            }
        }
        v
    };
    let lo = rise(&rms_lo);
    let full = rise(&rms_full);
    let flux = lo
        .iter()
        .zip(&full)
        .map(|(l, f)| l + SNAP_BROADBAND_WEIGHT * f)
        .collect();

    // Mean-normalize the RMS envelopes themselves for the snap's folded profiles.
    let normalize = |xs: &mut Vec<f32>| {
        let mean = xs.iter().sum::<f32>() / xs.len().max(1) as f32;
        if mean > 0.0 {
            xs.iter_mut().for_each(|x| *x /= mean);
        }
    };
    normalize(&mut rms_sub);
    normalize(&mut rms_hi);
    normalize(&mut rms_full);
    (flux, rms_sub, rms_hi, rms_full)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis::dsp::spectrogram;

    /// A click train at `bpm`: 20 ms decaying 1 kHz bursts on each beat.
    fn click_train(sr: u32, bpm: f32, secs: u32) -> Vec<f32> {
        let period = 60.0 / bpm * sr as f32;
        let n = sr as usize * secs as usize;
        let click_len = (sr as f32 * 0.02) as usize;
        let mut s = vec![0.0f32; n];
        let mut beat = 0.0;
        while (beat as usize) < n {
            let start = beat as usize;
            for j in 0..click_len {
                if start + j < n {
                    let t = j as f32 / sr as f32;
                    let env = 1.0 - j as f32 / click_len as f32;
                    s[start + j] = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * env;
                }
            }
            beat += period;
        }
        s
    }

    fn detect_bpm(sr: u32, bpm: f32, secs: u32) -> f32 {
        let s = click_train(sr, bpm, secs);
        let spec = spectrogram(&s, sr);
        detect(&spec).bpm
    }

    /// Diagnostic: what the snap saw on a real track. `SNAP_DEBUG_FILE=<audio>`
    /// picks the track, `SNAP_DEBUG_RB_MS=<ms>` (optional) marks rekordbox's
    /// first beat so the rows can be read against the reference grid. Each
    /// row is the folded average beat (64 bins, phase 0 = track time 0 mod the
    /// period), levels 0–9 per band; the marker row shows rekordbox's line
    /// (`R`), the coarse comb phase (`C`), every candidate (`c`), the winning
    /// bump (`B`) and the final foot (`O`).
    #[test]
    #[ignore = "diagnostic; SNAP_DEBUG_FILE=<audio> [SNAP_DEBUG_RB_MS=<ms>]"]
    fn debug_snap_on_real_track() {
        let path = std::env::var("SNAP_DEBUG_FILE").expect("SNAP_DEBUG_FILE");
        let rb_ms: Option<f64> = std::env::var("SNAP_DEBUG_RB_MS").ok().and_then(|s| s.parse().ok());
        // Mirror production (`analyze_file`): the spectrogram covers the key
        // window (first 150 s); the flux envelope covers the whole track.
        let audio = crate::analysis::decode_mono_capped(&path, Some(48_000 * 20 * 60)).unwrap();
        let key_cap = 150 * audio.sample_rate as usize;
        let spec = spectrogram(&audio.samples[..audio.samples.len().min(key_cap)], audio.sample_rate);
        let t = detect(&spec);
        let env = FluxEnv::new(&audio.samples, audio.sample_rate).unwrap();
        let bpm = refine_period_env(&env, t.bpm);
        let period_ms = 60_000.0 / bpm as f64;
        let prof = snap_profile(&env, bpm);
        let snap = snap_anchor_env(&env, bpm, t.beat_offset_ms);
        const COLS: usize = 64;
        let n = prof.n;
        let col = |bin: usize| bin * COLS / n;
        let col_ms = |ms: f64| col(((ms / period_ms).rem_euclid(1.0) * n as f64) as usize % n);
        let sparow = |p: &[f32], label: &str| {
            let bins: Vec<f32> = (0..COLS)
                .map(|i| {
                    let (a, b) = (i * n / COLS, ((i + 1) * n / COLS).max(i * n / COLS + 1));
                    p[a..b.min(n)].iter().sum::<f32>() / (b.min(n) - a) as f32
                })
                .collect();
            let (lo, hi) = bins.iter().fold((f32::MAX, f32::MIN), |(a, b), &x| (a.min(x), b.max(x)));
            let row: String = bins
                .iter()
                .map(|&v| char::from_digit((((v - lo) / (hi - lo).max(1e-9)) * 9.0) as u32, 10).unwrap())
                .collect();
            eprintln!("  {label:<6} |{row}|");
        };
        eprintln!(
            "{}\n  bpm {bpm:.3} coarse {}ms snap {snap}ms rb {:?}ms  ({n} bins/beat, {COLS} cols, {:.1} ms/col)",
            path.rsplit('/').next().unwrap_or(&path),
            t.beat_offset_ms,
            rb_ms,
            period_ms / COLS as f64
        );
        // Where the track audibly starts (first ~1.5 ms block at 30% of the
        // opening 10 s peak), the downbeat phase, and the grid's first bars.
        let sr = audio.sample_rate as f64;
        let peak10 = env.full_rms.iter().take((10_000.0 / env.ms_per) as usize).cloned().fold(0.0f32, f32::max);
        let first_on = env.full_rms.iter().position(|&v| v >= 0.3 * peak10).map(|i| env.ms_0_energy + i as f64 * env.ms_per);
        let dphase = crate::analysis::downbeat::detect_phase(&spec, bpm, snap);
        let first_beat_number = ((4 - dphase % 4) % 4) + 1;
        let _ = sr;
        eprintln!(
            "  first audible onset {:?} ms; downbeat phase {dphase} (first beat is number {first_beat_number}); grid: {}",
            first_on.map(|v| v.round()),
            (0..8).map(|k| format!("{:.0}{}", snap as f64 + k as f64 * period_ms, if (k as u32 + first_beat_number - 1) % 4 == 0 { "*" } else { "" })).collect::<Vec<_>>().join(" ")
        );
        eprintln!(
            "  foot refine: best_bin {:?} floor {:.3} peak {:.3} thr {:.3} peak_at {} foot {}; p_full[0..12 step 8] {:?}",
            prof.best_bin, prof.foot_dbg.0, prof.foot_dbg.1, prof.foot_dbg.2, prof.foot_dbg.3, prof.foot,
            (0..96).step_by(8).map(|j| format!("{:.2}", prof.p_full[j])).collect::<Vec<_>>()
        );
        eprintln!(
            "  sr {} p_full[70..100] {:?}",
            audio.sample_rate,
            (70..100).map(|j| format!("{:.2}", prof.p_full[j])).collect::<Vec<_>>()
        );
        sparow(&prof.p_sub, "sub");
        sparow(&prof.p_full, "full");
        sparow(&prof.p_hi, "hi");
        sparow(&prof.p_edge, "edge");
        let mut marks = vec![' '; COLS];
        for c in &prof.cands {
            marks[col(c.bin)] = 'c';
        }
        if let Some(b) = prof.best_bin {
            marks[col(b)] = 'B';
        }
        marks[col(prof.foot)] = 'O';
        marks[col_ms(t.beat_offset_ms as f64)] = 'C';
        if let Some(rb) = rb_ms {
            marks[col_ms(rb)] = 'R';
        }
        eprintln!("  marks  |{}|", marks.iter().collect::<String>());
        // Period check: fold the kick flux per 20 s chunk at a given BPM and
        // print the chunk's peak phase (ms into the beat). A steady column
        // means the period is right; a walk means it's off.
        let drift = |bpm: f32, label: &str| {
            let period = 60_000.0 / bpm as f64;
            let span = env.span_ms();
            let chunk = 20_000.0;
            let mut cols = Vec::new();
            let mut t0 = 0.0;
            while t0 + chunk <= span {
                let nb = 64usize;
                let mut acc = vec![0.0f64; nb];
                let i0 = ((t0 - env.ms_0) / env.ms_per).max(0.0) as usize;
                let i1 = (((t0 + chunk) - env.ms_0) / env.ms_per) as usize;
                for i in i0..i1.min(env.flux.len()) {
                    let ms = env.ms_0 + i as f64 * env.ms_per;
                    let b = (((ms / period).fract()) * nb as f64) as usize % nb;
                    acc[b] += env.flux[i] as f64;
                }
                let best = (0..nb).max_by(|&a, &b| acc[a].partial_cmp(&acc[b]).unwrap()).unwrap();
                cols.push(format!("{:>4.0}", best as f64 / nb as f64 * period));
                t0 += chunk;
            }
            eprintln!("  drift @{label} {bpm:.3}: peak phase per 20s [{}] ms", cols.join(" "));
        };
        drift(bpm, "ours");
        drift(t.bpm, "comb");
        if let Ok(rb_bpm) = std::env::var("SNAP_DEBUG_RB_BPM") {
            if let Ok(v) = rb_bpm.parse::<f32>() {
                drift(v, "rb  ");
            }
        }
        for c in &prof.cands {
            eprintln!(
                "  cand bin {:>4} ({:>+6.1}ms from rb) edge {:.3} sub_rise {:.3} full_rise {:.3} hi_rise {:.3} score {:.3}{}",
                c.bin,
                rb_ms.map(|rb| {
                    let d = c.bin as f64 / n as f64 * period_ms - rb.rem_euclid(period_ms);
                    d - (d / period_ms).round() * period_ms
                }).unwrap_or(f64::NAN),
                c.edge, c.sub_rise, c.full_rise, c.hi_rise, c.score,
                if Some(c.bin) == prof.best_bin { "  <-- winner" } else { "" }
            );
        }
    }

    #[test]
    fn detects_120_bpm_click_train() {
        let got = detect_bpm(44_100, 120.0, 30);
        assert!((got - 120.0).abs() < 1.0, "expected ~120, got {got}");
    }

    #[test]
    fn high_resolution_off_grid_tempo() {
        // 127.3 BPM falls between integer autocorrelation lags (≈126.0 / 129.2);
        // the fine comb search must recover it to well under a BPM.
        let got = detect_bpm(44_100, 127.3, 40);
        assert!((got - 127.3).abs() < 0.6, "expected ~127.3, got {got}");
    }

    #[test]
    fn accurate_across_dj_range() {
        for &target in &[90.0f32, 128.0, 140.0, 174.0] {
            let got = detect_bpm(44_100, target, 40);
            assert!(
                (got - target).abs() < 0.8,
                "expected ~{target}, got {got}"
            );
        }
    }

    #[test]
    fn lock_grid_removes_bpm_quantization_creep() {
        // A 124.83 BPM click train with the coarse lock off by the frame-grid
        // refine's quantization (~0.03 BPM). Over four minutes that alone puts
        // the last beats ~35 ms off; the full-track period refine must recover
        // the true tempo to a few thousandths of a BPM.
        let sr = 44_100;
        let s = click_train(sr, 124.83, 240);
        let (bpm, anchor) = lock_grid(&s, sr, 124.86, 0);
        assert!((bpm - 124.83).abs() < 0.005, "expected ~124.83, got {bpm}");
        // The clicks start at t=0, so the snapped anchor stays at/near zero
        // (or a whole beat in, if the snap walked past the track start).
        let period = 60_000.0 / 124.83;
        let frac = (anchor as f64 % period).min(period - anchor as f64 % period);
        assert!(frac < 5.0, "anchor {anchor} ms is off-beat by {frac:.1} ms");
    }

    #[test]
    fn recovers_true_tempo_from_three_two_fold() {
        // A 138 BPM track with a strong accent every 3rd beat (46 BPM bar pulse)
        // tempts the comb toward 92 BPM (= 2/3 × 138), a "3:2" fold. The metrical
        // correction must climb back to 138.
        let sr = 44_100;
        let mut s = click_train(sr, 138.0, 40);
        let accent = click_train(sr, 46.0, 40); // every 3rd beat, louder
        for (i, &v) in accent.iter().enumerate() {
            if i < s.len() {
                s[i] += 0.9 * v;
            }
        }
        let spec = spectrogram(&s, sr);
        let got = detect(&spec).bpm;
        assert!((got - 138.0).abs() < 1.5, "expected ~138, got {got}");
    }

    #[test]
    fn resists_double_tempo_from_offbeats() {
        // Kicks at 130 BPM with weaker offbeat hats halfway between (which a bare
        // autocorrelation could read as 260). The comb + prior must hold 130.
        let sr = 44_100;
        let mut s = click_train(sr, 130.0, 40);
        let off = click_train(sr, 130.0, 40);
        let shift = (60.0 / 130.0 / 2.0 * sr as f32) as usize;
        for (i, &v) in off.iter().enumerate() {
            if i + shift < s.len() {
                s[i + shift] += 0.5 * v;
            }
        }
        let spec = spectrogram(&s, sr);
        let got = detect(&spec).bpm;
        assert!((got - 130.0).abs() < 1.0, "expected ~130, got {got}");
    }
}
