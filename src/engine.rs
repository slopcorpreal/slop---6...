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

/// Decode the first audio track from `path` and return a mono-PCM sample vector
/// together with the original sample rate.
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

    // Find the first decodable audio track
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| "No audio track found in file".to_string())?;

    let track_id = track.id;
    let sample_rate = track.codec_params.sample_rate.unwrap_or(44_100);

    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| format!("Cannot create decoder: {}", e))?;

    let mut all_samples: Vec<f32> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<f32>> = None;

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
        let n_frames = decoded.capacity() as u64;
        let n_channels = spec.channels.count();

        if sample_buf.is_none() {
            sample_buf = Some(SampleBuffer::<f32>::new(n_frames, spec));
        }

        if let Some(ref mut buf) = sample_buf {
            buf.copy_interleaved_ref(decoded);
            // Mix down to mono
            for chunk in buf.samples().chunks(n_channels) {
                let mono: f32 = chunk.iter().sum::<f32>() / n_channels as f32;
                all_samples.push(mono);
            }
        }
    }

    if all_samples.is_empty() {
        return Err("No audio samples decoded from file".to_string());
    }

    Ok((all_samples, sample_rate))
}

// ---------------------------------------------------------------------------
// Energy envelope
// ---------------------------------------------------------------------------

/// Compute a short-time RMS energy envelope at [`ENERGY_RATE_HZ`] Hz.
///
/// Given raw PCM samples at `sample_rate` Hz, each output sample covers
/// `sample_rate / ENERGY_RATE_HZ` PCM samples (≈ 10 ms window).
pub fn compute_energy_envelope(samples: &[f32], sample_rate: u32) -> Vec<f32> {
    let window = (sample_rate as usize / ENERGY_RATE_HZ).max(1);
    samples
        .par_chunks(window)
        .map(|chunk| {
            let mean_sq: f32 = chunk.par_iter().map(|&s| s * s).sum::<f32>() / chunk.len() as f32;
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
/// delayed); a negative result means the subtitles are *behind* (should be
/// advanced).
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
        .max_by(|(_, c1), (_, c2)| c1.re.partial_cmp(&c2.re).unwrap_or(std::cmp::Ordering::Equal))
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
    /// Detected global offset in seconds.  Positive = subs are ahead of audio.
    pub offset_secs: f32,
    /// Normalised audio energy envelope (for UI display).
    pub audio_energy: Vec<f32>,
    /// Binary subtitle timing signal (for UI display).
    pub sub_signal: Vec<f32>,
}

/// Run the full alignment pipeline:
/// 1. Decode audio from `video_path`.
/// 2. Compute 100 Hz energy envelope.
/// 3. Parse subtitles from `sub_path`.
/// 4. Build expected-timing signal.
/// 5. FFT cross-correlate → global offset.
pub fn run_alignment(video_path: &Path, sub_path: &Path) -> Result<AlignmentResult, String> {
    // --- Audio ---
    let (samples, sample_rate) = decode_audio(video_path)?;
    let energy = compute_energy_envelope(&samples, sample_rate);

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
