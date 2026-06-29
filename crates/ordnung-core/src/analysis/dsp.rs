//! Shared DSP primitives: a windowed STFT magnitude spectrogram that both the
//! tempo (onset) and key (chroma) analyzers consume, so we pay for the FFT once.

use rustfft::{num_complex::Complex32, FftPlanner};

// 4096 gives ~10.8 Hz bins at 44.1 kHz — enough to separate semitones across the
// pitched band used for key detection, while the 512 hop keeps onset timing sharp.
pub const WINDOW: usize = 4096;
pub const HOP: usize = 512;

/// Magnitude spectrogram: `frames[t][bin]`, bins are `0..=WINDOW/2`.
pub struct Spectrogram {
    pub frames: Vec<Vec<f32>>,
    pub sample_rate: u32,
}

impl Spectrogram {
    /// Frames per second of the spectrogram (used to convert lag↔seconds).
    pub fn frame_rate(&self) -> f32 {
        self.sample_rate as f32 / HOP as f32
    }

    /// Frequency in Hz of spectrogram bin `i`.
    pub fn bin_hz(&self, i: usize) -> f32 {
        i as f32 * self.sample_rate as f32 / WINDOW as f32
    }
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
/// (bins `0..=WINDOW/2`) in order; the slice is reused between calls.
pub fn for_each_frame<F: FnMut(&[f32])>(samples: &[f32], mut f: F) {
    let window = hann(WINDOW);
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(WINDOW);

    let n_bins = WINDOW / 2 + 1;
    let mut buf = vec![Complex32::new(0.0, 0.0); WINDOW];
    let mut mags = vec![0.0f32; n_bins];

    let mut pos = 0;
    while pos + WINDOW <= samples.len() {
        for i in 0..WINDOW {
            buf[i] = Complex32::new(samples[pos + i] * window[i], 0.0);
        }
        fft.process(&mut buf);
        for (k, c) in buf.iter().take(n_bins).enumerate() {
            mags[k] = c.norm();
        }
        f(&mags);
        pos += HOP;
    }
}

/// Compute the magnitude STFT of mono samples with a Hann window. Materializes
/// every frame; for long full-track passes prefer [`for_each_frame`].
pub fn spectrogram(samples: &[f32], sample_rate: u32) -> Spectrogram {
    let mut frames = Vec::with_capacity(frame_count(samples.len()));
    for_each_frame(samples, |mags| frames.push(mags.to_vec()));
    Spectrogram {
        frames,
        sample_rate,
    }
}

fn hann(n: usize) -> Vec<f32> {
    (0..n)
        .map(|i| {
            let x = std::f32::consts::PI * i as f32 / (n as f32 - 1.0);
            x.sin().powi(2)
        })
        .collect()
}
