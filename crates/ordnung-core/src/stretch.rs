//! Playback-rate engines for the player's pitch fader.
//!
//! A turntable's pitch fader changes the platter speed, so tempo and pitch
//! move together: +8% is 8% faster and a little under a semitone and a half
//! sharper. That is *varispeed*, and it is nothing more than reading the
//! samples at a fractional rate — [`hermite`] is the interpolator the player
//! reads through, here so the same curve is testable on its own.
//!
//! Key lock (a CDJ's "master tempo") changes the tempo and leaves the pitch
//! where it was. That needs a time stretcher. [`Stretcher`] is a WSOLA
//! (waveform-similarity overlap-add) stretcher, the algorithm DJ software has
//! used for key lock since SoundTouch: the output is built from short
//! segments of the input, each taken from near where the tempo says it should
//! be but nudged (within a small seek window) to the offset whose waveform
//! best continues the previous segment, then crossfaded onto it. Within the
//! ±8% a pitch fader covers it is clean on rhythmic material and costs a
//! fraction of a core, which is what a player needs; a phase vocoder would
//! smear transients for no gain at these ratios.
//!
//! Pure DSP: the stretcher never touches the audio buffer itself. It asks for
//! the input frames it needs through a callback, so the caller decides how a
//! read that crosses a loop point wraps, and what happens when the decoder
//! hasn't produced those frames yet.

/// Four-point cubic Hermite interpolation: the value at `t` (`0..1`) between
/// `y1` and `y2`, with `y0` and `y3` shaping the slope at each end. Smooth
/// through the sample points with a flatter passband and less aliasing than
/// linear interpolation, at four multiplies per sample.
#[inline]
pub fn hermite(y0: f32, y1: f32, y2: f32, y3: f32, t: f32) -> f32 {
    let c0 = y1;
    let c1 = 0.5 * (y2 - y0);
    let c2 = y0 - 2.5 * y1 + 2.0 * y2 - 0.5 * y3;
    let c3 = 0.5 * (y3 - y0) + 1.5 * (y1 - y2);
    ((c3 * t + c2) * t + c1) * t + c0
}

/// Length of one synthesis segment, in milliseconds. Long enough that a
/// segment carries a few pitch periods of bass, short enough that a kick
/// isn't doubled when segments overlap.
const SEGMENT_MS: f32 = 40.0;
/// Crossfade between consecutive segments, in milliseconds.
const OVERLAP_MS: f32 = 8.0;
/// How far either side of the nominal position the stretcher may look for a
/// better-matching segment start, in milliseconds. Covers a full period of
/// anything above ~65 Hz, so bass notes line up phase-coherently.
const SEEK_MS: f32 = 15.0;

/// What [`Stretcher::next_block`] found when it went to read input.
#[derive(Debug, PartialEq, Eq)]
pub enum Block {
    /// A block of output is ready to [`Stretcher::pop`].
    Ready,
    /// The reader had no audio for the frames the block needs yet. Nothing
    /// advanced; the caller emits silence and tries again later.
    Starved,
}

/// A WSOLA time stretcher over interleaved `f32` frames.
///
/// Feed it a `rate` per block (`1.08` plays 8% faster) and a reader that fills
/// interleaved frames from any absolute frame index; take output one sample
/// at a time with [`pop`](Self::pop). The reader is asked for a window of
/// `segment + 2 × seek` frames per block, once, so it may lock a shared buffer
/// per call without contention.
pub struct Stretcher {
    channels: usize,
    /// Segment, overlap and seek radius, in frames.
    seg: usize,
    overlap: usize,
    seek: usize,
    /// Nominal input frame where the next block's segment starts.
    in_pos: f64,
    /// The last `overlap` frames of the previous segment, interleaved, waiting
    /// to be crossfaded into the next one. Empty before the first block.
    tail: Vec<f32>,
    /// Output samples of the current block, and how many have been popped.
    out: Vec<f32>,
    out_read: usize,
    /// Input frame that `out[0]` corresponds to, for [`input_position`].
    out_origin: f64,
    /// Rate the current block was built at, for [`input_position`].
    out_rate: f64,
    /// Scratch: the read window, its mono mix, and the tail's mono mix.
    window: Vec<f32>,
    window_mono: Vec<f32>,
    tail_mono: Vec<f32>,
}

impl Stretcher {
    /// A stretcher sized for `sample_rate`, starting its input at frame
    /// `start_frame`.
    pub fn new(sample_rate: u32, channels: usize, start_frame: f64) -> Self {
        let frames = |ms: f32| ((sample_rate.max(1) as f32 * ms / 1000.0).round() as usize).max(2);
        let overlap = frames(OVERLAP_MS);
        Self {
            channels: channels.max(1),
            seg: frames(SEGMENT_MS).max(3 * overlap),
            overlap,
            seek: frames(SEEK_MS),
            in_pos: start_frame.max(0.0),
            tail: Vec::new(),
            out: Vec::new(),
            out_read: 0,
            out_origin: start_frame.max(0.0),
            out_rate: 1.0,
            window: Vec::new(),
            window_mono: Vec::new(),
            tail_mono: Vec::new(),
        }
    }

    /// Frames of output each block yields.
    pub fn hop(&self) -> usize {
        self.seg - self.overlap
    }

    /// The input frame the next sample to pop corresponds to, so playback can
    /// carry on from the right place when the stretcher is switched off.
    pub fn input_position(&self) -> f64 {
        if self.out.is_empty() {
            return self.in_pos;
        }
        self.out_origin + (self.out_read / self.channels) as f64 * self.out_rate
    }

    /// Forget the pending output and tail and continue from `frame`. The next
    /// block starts clean, with no crossfade, so it joins whatever the caller
    /// was playing up to `frame` without a seam.
    pub fn reset_at(&mut self, frame: f64) {
        self.in_pos = frame.max(0.0);
        self.out_origin = self.in_pos;
        self.tail.clear();
        self.out.clear();
        self.out_read = 0;
    }

    /// Fold the input cursor back by `len` frames once it has reached `end`,
    /// the way a loop wraps. The pending output is kept: the caller's reader
    /// already served those frames from the loop's start.
    pub fn wrap(&mut self, end: f64, len: f64) {
        if len > 0.0 && self.in_pos >= end {
            self.in_pos -= len;
            self.out_origin -= len;
        }
    }

    /// Next output sample, or `None` when the current block is used up and
    /// [`next_block`](Self::next_block) must run.
    #[inline]
    pub fn pop(&mut self) -> Option<f32> {
        let s = *self.out.get(self.out_read)?;
        self.out_read += 1;
        Some(s)
    }

    /// Build the next block of output at `rate`. `read(first_frame, frames)`
    /// fills `frames` with interleaved input starting at the (possibly
    /// negative, then silent) absolute frame `first_frame`, returning `false`
    /// when that audio isn't available yet.
    pub fn next_block(
        &mut self,
        rate: f64,
        mut read: impl FnMut(i64, &mut [f32]) -> bool,
    ) -> Block {
        let ch = self.channels;
        let (seg, ov, seek) = (self.seg, self.overlap, self.seek);
        let hop = seg - ov;
        let nominal = self.in_pos.round() as i64;
        // First block after a reset: no tail to match against, so take the
        // segment exactly at the nominal position and emit it as is.
        let (first, n_frames) = if self.tail.is_empty() {
            (nominal, seg)
        } else {
            (nominal - seek as i64, seg + 2 * seek)
        };
        self.window.clear();
        self.window.resize(n_frames * ch, 0.0);
        if !read(first, &mut self.window) {
            return Block::Starved;
        }
        // Best-matching segment start within the window.
        let offset = if self.tail.is_empty() {
            0
        } else {
            self.mix_mono();
            self.best_offset()
        };
        let start = offset * ch;
        let segment = &self.window[start..start + seg * ch];

        self.out.clear();
        self.out.reserve(hop * ch);
        if self.tail.is_empty() {
            self.out.extend_from_slice(&segment[..hop * ch]);
        } else {
            // Crossfade the pending tail into the segment's head, then the
            // segment's middle straight through. Its last `ov` frames become
            // the next tail.
            for i in 0..ov {
                let w = (i as f32 + 0.5) / ov as f32;
                for c in 0..ch {
                    let a = self.tail[i * ch + c];
                    let b = segment[i * ch + c];
                    self.out.push(a + (b - a) * w);
                }
            }
            self.out.extend_from_slice(&segment[ov * ch..hop * ch]);
        }
        self.tail.clear();
        self.tail.extend_from_slice(&segment[hop * ch..seg * ch]);
        self.out_read = 0;
        self.out_origin = self.in_pos;
        self.out_rate = rate;
        self.in_pos += hop as f64 * rate;
        Block::Ready
    }

    /// Mono mixes of the window and the tail, for the correlation search.
    fn mix_mono(&mut self) {
        let ch = self.channels;
        let mono = |src: &[f32], dst: &mut Vec<f32>| {
            dst.clear();
            dst.extend(src.chunks_exact(ch).map(|f| f.iter().sum::<f32>()));
        };
        mono(&self.window, &mut self.window_mono);
        mono(&self.tail, &mut self.tail_mono);
    }

    /// The window offset (in frames) whose first `overlap` frames best
    /// continue the tail: the maximum of the normalised cross-correlation,
    /// so a louder candidate doesn't win on level alone.
    fn best_offset(&self) -> usize {
        let ov = self.overlap;
        let tail = &self.tail_mono[..ov];
        let mut best = (f32::NEG_INFINITY, self.seek);
        for d in 0..=2 * self.seek {
            let cand = &self.window_mono[d..d + ov];
            let mut corr = 0.0f32;
            let mut energy = 0.0f32;
            for (a, b) in tail.iter().zip(cand) {
                corr += a * b;
                energy += b * b;
            }
            let score = if energy > 0.0 { corr / energy.sqrt() } else { 0.0 };
            if score > best.0 {
                best = (score, d);
            }
        }
        best.1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hermite_passes_through_the_samples() {
        assert_eq!(hermite(0.0, 1.0, 2.0, 3.0, 0.0), 1.0);
        assert!((hermite(0.0, 1.0, 2.0, 3.0, 0.5) - 1.5).abs() < 1e-6, "a line stays a line");
        assert!((hermite(1.0, 1.0, 1.0, 1.0, 0.3) - 1.0).abs() < 1e-6, "a constant stays constant");
    }

    /// Stretch a ramp at 1.0 and it comes out as the ramp: the seam search
    /// finds the exact continuation and the crossfade is between equals.
    #[test]
    fn unity_rate_is_transparent() {
        let input: Vec<f32> = (0..20_000).map(|i| (i % 97) as f32 * 0.01).collect();
        let mut st = Stretcher::new(8_000, 1, 0.0);
        let mut out = Vec::new();
        while out.len() < 10_000 {
            match st.next_block(1.0, |first, buf| fill(&input, first, buf)) {
                Block::Ready => while let Some(s) = st.pop() {
                    out.push(s)
                },
                Block::Starved => panic!("input is all there"),
            }
        }
        for (i, (a, b)) in out.iter().zip(&input).enumerate() {
            assert!((a - b).abs() < 1e-5, "sample {i}: {a} vs {b}");
        }
    }

    /// The tempo changes and the pitch doesn't: a 200 Hz sine stretched at
    /// 1.08 still has 200 zero-crossing pairs per output second, while the
    /// input it consumed per output second grew by 8%.
    #[test]
    fn faster_rate_keeps_the_frequency_and_eats_input_faster() {
        let sr = 8_000u32;
        let input: Vec<f32> = (0..sr * 4)
            .map(|i| (i as f32 * 200.0 * std::f32::consts::TAU / sr as f32).sin())
            .collect();
        let mut st = Stretcher::new(sr, 1, 0.0);
        let mut out = Vec::new();
        while out.len() < sr as usize {
            assert_eq!(st.next_block(1.08, |first, buf| fill(&input, first, buf)), Block::Ready);
            while let Some(s) = st.pop() {
                out.push(s);
            }
        }
        let out = &out[..sr as usize];
        let rising = out.windows(2).filter(|w| w[0] < 0.0 && w[1] >= 0.0).count();
        assert!((195..=205).contains(&rising), "{rising} cycles/s, want ~200");
        let consumed = st.input_position() / out.len() as f64;
        assert!((consumed - 1.08).abs() < 0.03, "consumed {consumed} input per output");
        let peak = out.iter().fold(0.0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.95 && peak <= 1.01, "amplitude survives the crossfades: {peak}");
    }

    #[test]
    fn stereo_frames_stay_interleaved_and_aligned() {
        let sr = 8_000u32;
        let mut input = Vec::new();
        for i in 0..sr * 2 {
            let x = (i as f32 * 110.0 * std::f32::consts::TAU / sr as f32).sin();
            input.push(x);
            input.push(x * 0.5);
        }
        let mut st = Stretcher::new(sr, 2, 0.0);
        let mut out = Vec::new();
        for _ in 0..8 {
            assert_eq!(st.next_block(0.94, |first, buf| fill_ch(&input, 2, first, buf)), Block::Ready);
            while let Some(s) = st.pop() {
                out.push(s);
            }
        }
        assert_eq!(out.len() % 2, 0);
        for f in out.chunks(2) {
            assert!((f[1] - f[0] * 0.5).abs() < 1e-4, "frame {f:?} lost its channel relation");
        }
    }

    #[test]
    fn starved_read_leaves_the_position_alone() {
        let mut st = Stretcher::new(8_000, 1, 100.0);
        assert_eq!(st.next_block(1.0, |_, _| false), Block::Starved);
        assert_eq!(st.input_position(), 100.0);
        assert_eq!(st.pop(), None);
    }

    /// Test reader: copy mono frames out of `input`, silence before frame 0
    /// and after its end.
    fn fill(input: &[f32], first: i64, buf: &mut [f32]) -> bool {
        fill_ch(input, 1, first, buf)
    }

    fn fill_ch(input: &[f32], ch: usize, first: i64, buf: &mut [f32]) -> bool {
        for (i, s) in buf.iter_mut().enumerate() {
            let idx = first * ch as i64 + i as i64;
            *s = if idx >= 0 { input.get(idx as usize).copied().unwrap_or(0.0) } else { 0.0 };
        }
        true
    }
}
