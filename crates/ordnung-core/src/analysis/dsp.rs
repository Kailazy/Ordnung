//! Shared DSP primitives: a windowed STFT magnitude spectrogram that both the
//! tempo (onset) and key (chroma) analyzers consume, so we pay for the FFT once.

use realfft::RealFftPlanner;

// 4096 gives ~10.8 Hz bins at 44.1 kHz — enough to separate semitones across the
// pitched band used for key detection, while the 512 hop keeps onset timing sharp.
pub const WINDOW: usize = 4096;
pub const HOP: usize = 512;

/// Bins per STFT frame: the non-redundant half of the spectrum, plus DC and
/// Nyquist. Exactly what a real-input FFT produces.
pub const BINS: usize = WINDOW / 2 + 1;

/// Magnitude spectrogram, frame-major.
///
/// Held as one flat `n_frames * BINS` buffer rather than a `Vec<Vec<f32>>`. The
/// key window alone is ~12,900 frames, so the nested form meant ~12,900 separate
/// heap allocations totalling ~106 MB, and every downstream pass (tempo, key,
/// quality, downbeat, fingerprint) reached each frame through a pointer
/// indirection. Flat is one allocation and a contiguous scan.
pub struct Spectrogram {
    data: Vec<f32>,
    sample_rate: u32,
}

impl Spectrogram {
    /// Assemble a spectrogram from per-frame magnitudes. Every frame must be
    /// [`BINS`] long — the shape the STFT produces and every consumer assumes.
    /// Mainly for tests, which build synthetic spectra by hand.
    pub fn from_frames(frames: &[Vec<f32>], sample_rate: u32) -> Self {
        assert!(
            frames.iter().all(|f| f.len() == BINS),
            "every frame must have {BINS} bins"
        );
        let mut data = Vec::with_capacity(frames.len() * BINS);
        for f in frames {
            data.extend_from_slice(f);
        }
        Spectrogram { data, sample_rate }
    }

    /// Frames per second of the spectrogram (used to convert lag↔seconds).
    pub fn frame_rate(&self) -> f32 {
        self.sample_rate as f32 / HOP as f32
    }

    /// Frequency in Hz of spectrogram bin `i`.
    pub fn bin_hz(&self, i: usize) -> f32 {
        i as f32 * self.sample_rate as f32 / WINDOW as f32
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of STFT frames.
    pub fn len(&self) -> usize {
        self.data.len() / BINS
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Bins per frame — always [`BINS`], exposed so callers read intent rather
    /// than reaching for a magic constant.
    pub fn n_bins(&self) -> usize {
        BINS
    }

    /// Magnitudes of frame `i`. Panics out of range, like slice indexing.
    pub fn frame(&self, i: usize) -> &[f32] {
        &self.data[i * BINS..(i + 1) * BINS]
    }

    /// Every frame in order.
    pub fn frames(&self) -> impl Iterator<Item = &[f32]> + '_ {
        self.data.chunks_exact(BINS)
    }

    /// Frames `start..end`, clamped to the available range.
    pub fn frames_range(&self, start: usize, end: usize) -> impl Iterator<Item = &[f32]> + '_ {
        let n = self.len();
        let (start, end) = (start.min(n), end.min(n));
        self.data[start * BINS..end * BINS].chunks_exact(BINS)
    }
}

/// When does frame `t` *happen*? Its Hann window spans samples `[t·HOP, t·HOP +
/// WINDOW)`, and the window weights its own centre most, so the audio a frame
/// describes is centred half a window in — `t·HOP + WINDOW/2`. Timestamping a
/// frame by its start instead (the tempting `t / frame_rate`) reports every
/// event `WINDOW/2` = ~46 ms at 44.1 kHz *early*, which is enough to slide a
/// whole beatgrid off its kicks. Convert with these two, never by hand.
pub fn frame_to_ms(frame: f32, sample_rate: u32) -> f32 {
    (frame * HOP as f32 + WINDOW as f32 / 2.0) / sample_rate as f32 * 1000.0
}

/// Inverse of [`frame_to_ms`] — the (fractional, possibly negative) frame whose
/// window is centred on `ms`. Negative means the moment sits in the track's
/// first half-window, before any frame is centred on it.
pub fn ms_to_frame(ms: f32, sample_rate: u32) -> f32 {
    (ms / 1000.0 * sample_rate as f32 - WINDOW as f32 / 2.0) / HOP as f32
}

/// Number of STFT frames [`for_each_frame`] / [`spectrogram`] emit for a signal
/// of `n_samples` (one Hann window every `HOP`).
pub fn frame_count(n_samples: usize) -> usize {
    if n_samples >= WINDOW {
        (n_samples - WINDOW) / HOP + 1
    } else {
        0
    }
}

/// Stream the magnitude STFT frame-by-frame without materializing the whole
/// spectrogram — for full-track passes (e.g. the colored waveform) where keeping
/// every frame would cost hundreds of MB. `f` receives each frame's magnitudes
/// (bins `0..BINS`) in order; the slice is reused between calls.
///
/// The transform is real-input: the windowed audio has no imaginary part, and a
/// complex FFT spends half its work proving that stays true. `realfft` wraps the
/// same rustfft primitives, so this is the identical algorithm with the redundant
/// half removed — the magnitudes come out bit-for-bit unchanged.
pub fn for_each_frame<F: FnMut(&[f32])>(samples: &[f32], mut f: F) {
    let window = hann(WINDOW);
    let mut planner = RealFftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);

    let mut buf = fft.make_input_vec();
    let mut spectrum = fft.make_output_vec();
    // Preallocated so the transform doesn't allocate per frame.
    let mut scratch = fft.make_scratch_vec();
    let mut mags = vec![0.0f32; BINS];

    let mut pos = 0;
    while pos + WINDOW <= samples.len() {
        for (b, (s, w)) in buf
            .iter_mut()
            .zip(samples[pos..pos + WINDOW].iter().zip(&window))
        {
            *b = s * w;
        }
        // Only fails on a length mismatch, and all three buffers came from `fft`.
        fft.process_with_scratch(&mut buf, &mut spectrum, &mut scratch)
            .expect("realfft buffers are sized by the plan");
        for (m, c) in mags.iter_mut().zip(spectrum.iter()) {
            *m = c.norm();
        }
        f(&mags);
        pos += HOP;
    }
}

/// Compute the magnitude STFT of mono samples with a Hann window. Materializes
/// every frame; for long full-track passes prefer [`for_each_frame`].
pub fn spectrogram(samples: &[f32], sample_rate: u32) -> Spectrogram {
    let mut data = Vec::with_capacity(frame_count(samples.len()) * BINS);
    for_each_frame(samples, |mags| data.extend_from_slice(mags));
    Spectrogram { data, sample_rate }
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (n as f32 - 1.0);
            x.sin().powi(2)
        })
        .collect()
}
