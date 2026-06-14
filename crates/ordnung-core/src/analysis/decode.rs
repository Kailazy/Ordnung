//! Decode any supported container/codec to mono f32 PCM via symphonia.
//!
//! Analysis works on a single mono channel; we downmix by averaging. The native
//! sample rate is preserved and returned so downstream DSP can convert time/lag
//! to seconds correctly.

use crate::error::{Error, Result};
use std::path::Path;
use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::formats::{FormatOptions, SeekMode, SeekTo};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::Time;

pub struct DecodedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Decode `path` fully to mono f32 samples in [-1, 1].
pub fn decode_mono(path: impl AsRef<Path>) -> Result<DecodedAudio> {
    decode_mono_inner(path, None, None)
}

/// Decode to mono f32, stopping early once `max_samples` are collected.
///
/// Analysis only needs a representative window: for steady-tempo material a slice
/// is as accurate as the whole track and far faster to decode. `None` decodes all.
pub fn decode_mono_capped(path: impl AsRef<Path>, max_samples: Option<usize>) -> Result<DecodedAudio> {
    decode_mono_inner(path, max_samples, None)
}

/// Decode roughly `window_secs` of mono audio centered on the middle of the
/// track, seeking past the head instead of decoding the whole file.
///
/// Preview playback only needs a short slice from the middle; decoding the entire
/// track (often minutes) just to keep ~12 s dominated click-to-play latency. We
/// read the duration from the container, seek near the midpoint, and decode only
/// the window. Falls back to a capped decode from the start when the duration is
/// unknown or the track is shorter than the window.
pub fn decode_mono_middle(path: impl AsRef<Path>, window_secs: f32) -> Result<DecodedAudio> {
    decode_mono_inner(path, None, Some(window_secs.max(0.0)))
}

/// Probe a file's audio properties via the decoder, confirming real sample data is
/// present — the fallback `scan` uses when the tag reader (lofty) rejects a file.
///
/// Reads container/codec metadata for properties, then decodes a few packets to
/// prove the audio data actually exists. A truncated download (header intact, no
/// sample data) probes fine but yields no decodable frames, so it's rejected here
/// rather than imported as a phantom track. Bitrate is estimated from file size
/// when the codec doesn't report a frame count.
pub fn probe_for_scan(path: impl AsRef<Path>) -> Result<crate::model::AudioProperties> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let file_len = file.metadata().map(|m| m.len()).unwrap_or(0);
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| decode_err(path, e))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| Error::Decode {
            path: path.to_path_buf(),
            msg: "no decodable audio track".into(),
        })?;
    let track_id = track.id;
    let cp = &track.codec_params;
    let sample_rate = cp.sample_rate.unwrap_or(0);
    let channels = cp.channels.map(|c| c.count() as u8).unwrap_or(0);
    let bit_depth = cp.bits_per_sample.map(|b| b as u8);
    let duration_ms = match (cp.n_frames, cp.sample_rate) {
        (Some(n), Some(sr)) if sr > 0 => (u128::from(n) * 1000 / u128::from(sr)) as u64,
        _ => 0,
    };
    // Truncation guard for uncompressed/lossless audio (PCM reports a bit depth):
    // the file must be roughly the size its declared duration implies. A header
    // says 8 minutes but the file holds 1% of that many bytes → an incomplete
    // download whose audio chunk was cut off. (Skipped for lossy codecs, which
    // report no bit depth and whose size doesn't map to duration this way.)
    if let (Some(bd), true) = (bit_depth, sample_rate > 0 && channels > 0 && duration_ms > 0) {
        let expected = u128::from(duration_ms)
            * u128::from(sample_rate)
            * u128::from(channels)
            * u128::from(bd / 8)
            / 1000;
        if u128::from(file_len) * 2 < expected {
            return Err(Error::Decode {
                path: path.to_path_buf(),
                msg: format!(
                    "truncated: {} of ~{} expected bytes ({}%) — incomplete download",
                    file_len,
                    expected,
                    (u128::from(file_len) * 100 / expected.max(1)).min(100),
                ),
            });
        }
    }

    // Building the decoder copies what it needs from `cp`, releasing the borrow of
    // `format` so we can pull packets below.
    let mut decoder = symphonia::default::get_codecs()
        .make(cp, &DecoderOptions::default())
        .map_err(|e| decode_err(path, e))?;

    // Confirm actual audio — a handful of packets is plenty. A header-only truncated
    // file returns EOF before any frame decodes.
    let mut got_audio = false;
    for _ in 0..64 {
        match format.next_packet() {
            Ok(p) => {
                if p.track_id() != track_id {
                    continue;
                }
                match decoder.decode(&p) {
                    Ok(buf) => {
                        if buf.frames() > 0 {
                            got_audio = true;
                            break;
                        }
                    }
                    Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
                    Err(e) => return Err(decode_err(path, e)),
                }
            }
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(decode_err(path, e)),
        }
    }
    if !got_audio {
        return Err(Error::Decode {
            path: path.to_path_buf(),
            msg: "no audio data — file appears truncated or empty".into(),
        });
    }

    // bytes * 8 / duration_ms == kilobits per second. Includes tag/cover overhead,
    // so it's an estimate, but right for CBR and a useful figure when the codec
    // didn't report its own bitrate.
    let bitrate_kbps = if duration_ms > 0 && file_len > 0 {
        Some((file_len.saturating_mul(8) / duration_ms) as u32)
    } else {
        None
    };

    Ok(crate::model::AudioProperties {
        sample_rate_hz: sample_rate,
        bit_depth,
        channels,
        duration_ms,
        bitrate_kbps,
    })
}

/// Shared decode core. `max_samples` caps the output length. `middle_window_secs`,
/// when set, seeks to the middle of the track and decodes only that window (it
/// also acts as the cap); it takes precedence over `max_samples`.
fn decode_mono_inner(
    path: impl AsRef<Path>,
    max_samples: Option<usize>,
    middle_window_secs: Option<f32>,
) -> Result<DecodedAudio> {
    let path = path.as_ref();
    let file = std::fs::File::open(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            mss,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|e| decode_err(path, e))?;
    let mut format = probed.format;

    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| Error::Decode {
            path: path.to_path_buf(),
            msg: "no decodable audio track".into(),
        })?;
    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100);
    let n_frames = track.codec_params.n_frames;

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| decode_err(path, e))?;

    // Resolve the output cap, seeking to the middle first when a preview window
    // was requested and the track is long enough to bother.
    let max_samples = match middle_window_secs {
        Some(window_secs) => {
            let window = (window_secs * sample_rate as f32).ceil() as usize;
            if let Some(total) = n_frames {
                if (total as usize) > window {
                    let mid_secs = total as f64 / sample_rate as f64 / 2.0;
                    let start_secs = (mid_secs - window_secs as f64 / 2.0).max(0.0);
                    if format
                        .seek(
                            SeekMode::Coarse,
                            SeekTo::Time {
                                time: Time::from(start_secs),
                                track_id: Some(track_id),
                            },
                        )
                        .is_ok()
                    {
                        decoder.reset();
                    }
                }
            }
            Some(window)
        }
        None => max_samples,
    };

    // Pre-size the output to the known cap so a ~150 s window (≈7 M f32, ~29 MB)
    // fills without the repeated grow-and-copy reallocations of an empty Vec.
    let mut samples: Vec<f32> = match max_samples {
        Some(cap) => Vec::with_capacity(cap),
        None => Vec::new(),
    };
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            // Clean end of stream.
            Err(symphonia::core::errors::Error::IoError(ref e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(e) => return Err(decode_err(path, e)),
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => append_mono(&buf, &mut samples),
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip bad frame
            Err(e) => return Err(decode_err(path, e)),
        }
        if let Some(cap) = max_samples {
            if samples.len() >= cap {
                samples.truncate(cap);
                break;
            }
        }
    }

    if samples.is_empty() {
        return Err(Error::Decode {
            path: path.to_path_buf(),
            msg: "decoded zero samples".into(),
        });
    }
    Ok(DecodedAudio {
        samples,
        sample_rate,
    })
}

/// Average all channels of a decoded buffer into the mono output.
fn append_mono(buf: &AudioBufferRef, out: &mut Vec<f32>) {
    match buf {
        AudioBufferRef::F32(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f]
        }),
        AudioBufferRef::S16(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f] as f32 / i16::MAX as f32
        }),
        AudioBufferRef::S32(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f] as f32 / i32::MAX as f32
        }),
        AudioBufferRef::U8(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            (b.chan(ch)[f] as f32 - 128.0) / 128.0
        }),
        AudioBufferRef::F64(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f] as f32
        }),
        AudioBufferRef::S24(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f].inner() as f32 / 8_388_607.0
        }),
        AudioBufferRef::U16(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            (b.chan(ch)[f] as f32 - 32_768.0) / 32_768.0
        }),
        AudioBufferRef::U24(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            (b.chan(ch)[f].inner() as f32 - 8_388_608.0) / 8_388_608.0
        }),
        AudioBufferRef::U32(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            (b.chan(ch)[f] as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
        }),
        AudioBufferRef::S8(b) => downmix(b.spec().channels.count(), b.frames(), out, |ch, f| {
            b.chan(ch)[f] as f32 / i8::MAX as f32
        }),
    }
}

fn downmix<F: Fn(usize, usize) -> f32>(channels: usize, frames: usize, out: &mut Vec<f32>, get: F) {
    out.reserve(frames);
    let inv = 1.0 / channels.max(1) as f32;
    for f in 0..frames {
        let mut acc = 0.0;
        for ch in 0..channels {
            acc += get(ch, f);
        }
        out.push(acc * inv);
    }
}

fn decode_err(path: &Path, e: symphonia::core::errors::Error) -> Error {
    Error::Decode {
        path: path.to_path_buf(),
        msg: e.to_string(),
    }
}
