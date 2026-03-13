use std::path::Path;

use rayon::prelude::*;
use rustfft::{num_complex::Complex, FftPlanner};
use symphonia::core::{
    audio::SampleBuffer,
    codecs::{DecoderOptions, CODEC_TYPE_NULL},
    errors::Error as SymphoniaError,
    formats::FormatOptions,
    io::MediaSourceStream,
    meta::MetadataOptions,
    probe::Hint,
};

use crate::subtitle;

/// The sample rate at which we compute the audio energy envelope (100 Hz = 10 ms windows).
pub const ENERGY_RATE_HZ: usize = 100;

// ---------------------------------------------------------------------------
// Audio extraction
// ---------------------------------------------------------------------------

/// Decode the first audio track from `path` and compute its 100 Hz RMS energy
/// envelope in a single streaming pass, without buffering the full-rate PCM.
///
/// Returns `(energy_envelope, sample_rate)`.  The energy array has one element
/// per 10 ms of audio at 100 Hz.
pub fn decode_audio(path: &Path) -> Result<(Vec<f32>, u32), String> {
    let file = std::fs::File::open(path)
        .map_err(|e| format!("Cannot open video file: {}", e))?;

    let mss = MediaSourceStream::new(Box::new(file), Default::default());

    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .map_err(|e| format!("Unsupported format: {}", e))?;

    let mut format = probed.format;

    // Find the first audio track: require sample_rate and channels to be set
    // so we never accidentally select a video or data track.
    let track = format
        .tracks()
        .iter()
        .find(|t| {
            t.codec_params.codec != CODEC_TYPE_NULL
                && t.codec_params.sample_rate.is_some()
                && t.codec_params.channels.is_some()
        })
        .ok_or_else(|| "No audio track found in file".to_string())?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap();

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Cannot create decoder: {}", e))?;

    // Streaming energy accumulator — avoids storing the full-rate PCM track.
    let window_size = (sample_rate as usize / ENERGY_RATE_HZ).max(1);
    let mut energy: Vec<f32> = Vec::new();
    let mut win_sum_sq: f32 = 0.0;
    let mut win_count: usize = 0;

    let mut sample_buf: Option<SampleBuffer<f32>> = None;
    // Track the spec the sample buffer was created with so we can detect changes.
    let mut buf_spec: Option<symphonia::core::audio::SignalSpec> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(SymphoniaError::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                decoder.reset();
                continue;
            }
            Err(e) => return Err(format!("Packet error: {}", e)),
        };

        if packet.track_id() != track_id {
            continue;
        }

        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(SymphoniaError::IoError(_)) | Err(SymphoniaError::DecodeError(_)) => continue,
            Err(e) => return Err(format!("Decode error: {}", e)),
        };

        let spec = *decoded.spec();
        let n_channels = spec.channels.count();
        let needed_capacity = decoded.frames() * n_channels;

        // Reinitialise the sample buffer when the spec or capacity changes.
        // This handles packets with a larger frame count or a different channel
        // layout, which would otherwise cause `copy_interleaved_ref` to panic.
        // Compare against `buf_spec` (the spec the buffer was built with), not
        // against `spec` itself, which would always be equal.
        let needs_new_buf = sample_buf
            .as_ref()
            .map_or(true, |b| needed_capacity > b.capacity() || buf_spec != Some(spec));
        if needs_new_buf {
            sample_buf = Some(SampleBuffer::<f32>::new(decoded.capacity() as u64, spec));
            buf_spec = Some(spec);
        }

        if let Some(ref mut buf) = sample_buf {
            buf.copy_interleaved_ref(decoded);
            // Mix down to mono and accumulate into the current energy window
            for chunk in buf.samples().chunks(n_channels) {
                let mono: f32 = chunk.iter().sum::<f32>() / n_channels as f32;
                win_sum_sq += mono * mono;
                win_count += 1;
                if win_count >= window_size {
                    energy.push((win_sum_sq / win_count as f32).sqrt());
                    win_sum_sq = 0.0;
                    win_count = 0;
                }
            }
        }
    }

    // Flush any partial window at the end of the stream
    if win_count > 0 {
        energy.push((win_sum_sq / win_count as f32).sqrt());
    }

    if energy.is_empty() {
        return Err("No audio samples decoded from file".to_string());
    }

    Ok((energy, sample_rate))
}

// ---------------------------------------------------------------------------
// Energy envelope (standalone, used in tests and as a building block)
// ---------------------------------------------------------------------------

/// Compute a short-time RMS energy envelope at [`ENERGY_RATE_HZ`] Hz from
/// raw PCM samples.
///
/// Each output sample covers `sample_rate / ENERGY_RATE_HZ` PCM samples
/// (≈ 10 ms window).  Parallelism is applied across windows only; the inner
/// per-window sum uses a plain iterator to avoid nested-parallelism overhead.
pub fn compute_energy_envelope(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let window = (sample_rate as usize / ENERGY_RATE_HZ).max(1);
    samples
        .par_chunks(window)
        .map(|chunk| {
            let mean_sq: f32 =
                chunk.iter().map(|&s| s * s).sum::<f32>() / chunk.len() as f32;
            mean_sq.sqrt()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// FFT cross-correlation
// ---------------------------------------------------------------------------

/// Compute the optimal global time offset (in **seconds**) between the audio
/// energy envelope and the subtitle expected-timing signal using FFT
/// cross-correlation.
///
/// A positive result means the subtitles are *ahead* of the audio (should be
/// shifted back); a negative result means they are *behind* (should be
/// advanced).
///
/// This produces a single **constant** offset.  Variable drift (e.g. frame-
/// rate difference, commercial-break cuts) is not handled here.
pub fn compute_global_offset(
    audio_energy: &[f32],
    sub_expected: &[f32],
    rate_hz: usize,
) -> f32 {
    let n = audio_energy
        .len()
        .max(sub_expected.len())
        .next_power_of_two()
        * 2;

    // Convert to complex with zero-padding
    let mut a: Vec<Complex<f32>> = audio_energy
        .par_iter()
        .map(|&v| Complex { re: v, im: 0.0 })
        .collect();
    a.resize(n, Complex::default());

    let mut b: Vec<Complex<f32>> = sub_expected
        .par_iter()
        .map(|&v| Complex { re: v, im: 0.0 })
        .collect();
    b.resize(n, Complex::default());

    // Forward FFT both signals
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(n);
    let ifft = planner.plan_fft_inverse(n);

    fft.process(&mut a);
    fft.process(&mut b);

    // Cross-correlation: A * conj(B)
    a.par_iter_mut().zip(b.par_iter()).for_each(|(ca, cb)| {
        *ca = *ca * cb.conj();
    });

    // Inverse FFT → time-domain correlation array
    ifft.process(&mut a);

    // Find the peak (argmax of real part)
    let (max_idx, _) = a
        .iter()
        .enumerate()
        .max_by(|(_, c1), (_, c2)| {
            c1.re.partial_cmp(&c2.re).unwrap_or(std::cmp::Ordering::Equal)
        })
        .unwrap();

    // Map index to a signed offset (wrap around the midpoint)
    let offset_idx = if max_idx > n / 2 {
        max_idx as isize - n as isize
    } else {
        max_idx as isize
    };

    offset_idx as f32 / rate_hz as f32
}

// ---------------------------------------------------------------------------
// High-level alignment pipeline
// ---------------------------------------------------------------------------

/// Result bundle returned by [`run_alignment`].
pub struct AlignmentResult {
    /// Detected global constant offset in seconds.  Positive = subs are ahead
    /// of the audio and will be shifted backwards; negative = subs lag.
    pub offset_secs: f32,
    /// Normalised audio energy envelope (for UI display).
    pub audio_energy: Vec<f32>,
    /// Binary subtitle timing signal (for UI display).
    pub sub_signal: Vec<f32>,
}

/// Run the full alignment pipeline:
/// 1. Stream-decode audio from `video_path` into a 100 Hz energy envelope.
/// 2. Parse subtitles from `sub_path`.
/// 3. Build an expected-timing signal from the cue timestamps.
/// 4. FFT cross-correlate energy vs timing signal → constant global offset.
///
/// The returned offset is a single constant shift.  Variable drift (e.g. due
/// to frame-rate differences or cut commercial breaks) is not modelled.
pub fn run_alignment(video_path: &Path, sub_path: &Path) -> Result<AlignmentResult, String> {
    // --- Audio (energy envelope, streamed) ---
    let (energy, _sample_rate) = decode_audio(video_path)?;

    // Normalise for display
    let max_e = energy.iter().cloned().fold(f32::NEG_INFINITY, f32::max);
    let norm_energy: Vec<f32> = if max_e > 0.0 {
        energy.iter().map(|&e| e / max_e).collect()
    } else {
        energy.clone()
    };

    // --- Subtitles ---
    let sub_content = std::fs::read_to_string(sub_path)
        .map_err(|e| format!("Cannot read subtitle file: {}", e))?;
    let entries = subtitle::parse_srt(&sub_content)?;
    if entries.is_empty() {
        return Err("No subtitle entries found in file".to_string());
    }
    let sub_signal = subtitle::entries_to_expected_signal(&entries, energy.len(), ENERGY_RATE_HZ);

    // --- Alignment ---
    let offset_secs = compute_global_offset(&energy, &sub_signal, ENERGY_RATE_HZ);

    Ok(AlignmentResult {
        offset_secs,
        audio_energy: norm_energy,
        sub_signal,
    })
}

/// Apply `offset_secs` to all entries in `sub_path` and write the result to
/// `output_path` in SRT format.
pub fn apply_offset_and_save(
    sub_path: &Path,
    offset_secs: f32,
    output_path: &Path,
) -> Result<(), String> {
    let content = std::fs::read_to_string(sub_path)
        .map_err(|e| format!("Cannot read subtitle file: {}", e))?;
    let entries = subtitle::parse_srt(&content)?;
    let offset_ms = (offset_secs * 1_000.0).round() as i64;
    let shifted = subtitle::apply_offset(&entries, -offset_ms);
    let out = subtitle::write_srt(&shifted);
    std::fs::write(output_path, out)
        .map_err(|e| format!("Cannot write output file: {}", e))?;
    Ok(())
}

/// Derive a sensible output path: same directory as the input, with `_synced`
/// appended before the extension.
pub fn default_output_path(sub_path: &Path) -> std::path::PathBuf {
    let stem = sub_path
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy();
    let ext = sub_path
        .extension()
        .unwrap_or_default()
        .to_string_lossy();
    let new_name = format!("{}_synced.{}", stem, ext);
    sub_path.with_file_name(new_name)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_energy_envelope_silence() {
        let samples = vec![0.0f32; 4800];
        let energy = compute_energy_envelope(&samples, 48_000);
        // 4800 samples / (48000/100) = 10 windows
        assert_eq!(energy.len(), 10);
        assert!(energy.iter().all(|&e| e == 0.0));
    }

    #[test]
    fn test_energy_envelope_full_scale() {
        // Constant 1.0 signal → RMS = 1.0 per window
        let samples = vec![1.0f32; 4800];
        let energy = compute_energy_envelope(&samples, 48_000);
        for e in &energy {
            assert!((e - 1.0).abs() < 1e-5, "Expected 1.0, got {}", e);
        }
    }

    #[test]
    fn test_fft_offset_zero() {
        // Identical signals → offset should be 0
        let signal: Vec<f32> = (0..200)
            .map(|i| if i % 20 < 5 { 1.0 } else { 0.0 })
            .collect();
        let offset = compute_global_offset(&signal, &signal, ENERGY_RATE_HZ);
        assert!(
            offset.abs() < 0.15,
            "Expected ~0 offset, got {}",
            offset
        );
    }

    #[test]
    fn test_fft_offset_known_shift() {
        // Use a longer signal with impulses placed only in the central region
        // to avoid wrap-around edge effects from the circular FFT.
        let n = 1024;
        let shift = 5usize;
        let mut audio = vec![0.0f32; n];
        let mut subs = vec![0.0f32; n];
        // Place impulses in [n/4, 3*n/4) so every shifted copy stays within bounds
        for i in (n / 4..3 * n / 4).step_by(50) {
            audio[i] = 1.0;
            if i + shift < n {
                subs[i + shift] = 1.0;
            }
        }
        let offset = compute_global_offset(&audio, &subs, ENERGY_RATE_HZ);
        // subs are shift samples behind audio → expected offset ≈ -shift/rate
        let expected = -(shift as f32) / ENERGY_RATE_HZ as f32;
        assert!(
            (offset - expected).abs() < 0.02,
            "Expected offset ≈ {}, got {}",
            expected,
            offset
        );
    }

    #[test]
    fn test_default_output_path() {
        let p = std::path::Path::new("/home/user/movie.srt");
        let out = default_output_path(p);
        assert_eq!(out, std::path::Path::new("/home/user/movie_synced.srt"));
    }
}
