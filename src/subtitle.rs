/// Milliseconds per second — used throughout for timestamp conversions.
const MS_PER_SECOND: f64 = 1_000.0;

/// A single subtitle entry parsed from an SRT file.
#[derive(Debug, Clone)]
pub struct SubEntry {
    pub index: usize,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

/// Parse an SRT file content into a list of [`SubEntry`] values.
///
/// The SRT format consists of blocks separated by blank lines. Each block
/// contains:
/// 1. A sequence number
/// 2. A timestamp line: `HH:MM:SS,mmm --> HH:MM:SS,mmm`
/// 3. One or more lines of subtitle text
pub fn parse_srt(content: &str) -> Result<Vec<SubEntry>, String> {
    let mut entries = Vec::new();

    // Split into blocks on blank lines (handles both \r\n and \n)
    let normalised = content.replace("\r\n", "\n");
    let blocks: Vec<&str> = normalised
        .split("\n\n")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .collect();

    for block in &blocks {
        let lines: Vec<&str> = block.lines().collect();
        if lines.len() < 3 {
            continue;
        }

        let index: usize = lines[0]
            .trim()
            .parse()
            .map_err(|_| format!("Cannot parse sequence number: {:?}", lines[0]))?;

        let (start_ms, end_ms) = parse_timestamp_line(lines[1])?;

        let text = lines[2..].join("\n");

        entries.push(SubEntry {
            index,
            start_ms,
            end_ms,
            text,
        });
    }

    Ok(entries)
}

/// Parse a timestamp line of the form `HH:MM:SS,mmm --> HH:MM:SS,mmm`.
/// Handles varied spacing around `-->` (e.g. no spaces, extra spaces, tabs).
fn parse_timestamp_line(line: &str) -> Result<(i64, i64), String> {
    let arrow = line
        .find("-->")
        .ok_or_else(|| format!("Invalid timestamp line: {:?}", line))?;
    let start = parse_srt_time(line[..arrow].trim())?;
    let end = parse_srt_time(line[arrow + "-->".len()..].trim())?;
    Ok((start, end))
}

/// Parse a single SRT timestamp `HH:MM:SS,mmm` into milliseconds.
fn parse_srt_time(s: &str) -> Result<i64, String> {
    // Accept both comma and dot as the millisecond separator
    let s = s.replace(',', ".");
    let (hms, ms_str) = s
        .rsplit_once('.')
        .ok_or_else(|| format!("No millisecond separator in: {:?}", s))?;

    let parts: Vec<&str> = hms.split(':').collect();
    if parts.len() != 3 {
        return Err(format!("Expected HH:MM:SS, got: {:?}", hms));
    }

    let h: i64 = parts[0].parse().map_err(|_| format!("Bad hours: {:?}", parts[0]))?;
    let m: i64 = parts[1].parse().map_err(|_| format!("Bad minutes: {:?}", parts[1]))?;
    let s: i64 = parts[2].parse().map_err(|_| format!("Bad seconds: {:?}", parts[2]))?;
    // Pad or truncate millisecond string to exactly 3 digits
    let ms_padded = format!("{:0<3}", &ms_str[..ms_str.len().min(3)]);
    let ms: i64 = ms_padded.parse().map_err(|_| format!("Bad ms: {:?}", ms_str))?;

    Ok(h * 3_600_000 + m * 60_000 + s * 1_000 + ms)
}

/// Format milliseconds back to `HH:MM:SS,mmm`.
fn ms_to_srt_time(mut ms: i64) -> String {
    // Clamp to zero: negative timestamps are invalid in SRT
    if ms < 0 {
        ms = 0;
    }
    let h = ms / 3_600_000;
    ms %= 3_600_000;
    let m = ms / 60_000;
    ms %= 60_000;
    let s = ms / 1_000;
    let millis = ms % 1_000;
    format!("{:02}:{:02}:{:02},{:03}", h, m, s, millis)
}

/// Return a binary signal at `rate` Hz (one sample per `1000/rate` ms) where
/// a sample is `1.0` if a subtitle is active at that time and `0.0` otherwise.
/// `length` is the number of samples to generate (typically the same length as
/// the audio energy array).
pub fn entries_to_expected_signal(entries: &[SubEntry], length: usize, rate_hz: usize) -> Vec<f32> {
    let mut signal = vec![0.0f32; length];
    let ms_per_sample = MS_PER_SECOND / rate_hz as f64;
    for entry in entries {
        let start_idx = (entry.start_ms as f64 / ms_per_sample) as usize;
        let end_idx = (entry.end_ms as f64 / ms_per_sample) as usize;
        for i in start_idx..end_idx.min(length) {
            signal[i] = 1.0;
        }
    }
    signal
}

/// Apply a constant offset (in milliseconds) to all entries, clamping to 0.
pub fn apply_offset(entries: &[SubEntry], offset_ms: i64) -> Vec<SubEntry> {
    entries
        .iter()
        .map(|e| SubEntry {
            index: e.index,
            start_ms: (e.start_ms + offset_ms).max(0),
            end_ms: (e.end_ms + offset_ms).max(0),
            text: e.text.clone(),
        })
        .collect()
}

/// Serialise a list of entries to SRT format.
pub fn write_srt(entries: &[SubEntry]) -> String {
    entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            format!(
                "{}\n{} --> {}\n{}\n",
                i + 1,
                ms_to_srt_time(e.start_ms),
                ms_to_srt_time(e.end_ms),
                e.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE_SRT: &str = "\
1
00:00:01,500 --> 00:00:04,000
Hello, world!

2
00:00:05,000 --> 00:00:07,500
How are you?
";

    #[test]
    fn test_parse_srt() {
        let entries = parse_srt(SAMPLE_SRT).unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].start_ms, 1_500);
        assert_eq!(entries[0].end_ms, 4_000);
        assert_eq!(entries[0].text, "Hello, world!");
        assert_eq!(entries[1].start_ms, 5_000);
        assert_eq!(entries[1].end_ms, 7_500);
    }

    #[test]
    fn test_apply_offset_positive() {
        let entries = parse_srt(SAMPLE_SRT).unwrap();
        let shifted = apply_offset(&entries, 2_000);
        assert_eq!(shifted[0].start_ms, 3_500);
        assert_eq!(shifted[0].end_ms, 6_000);
    }

    #[test]
    fn test_apply_offset_negative_clamp() {
        let entries = parse_srt(SAMPLE_SRT).unwrap();
        // Shifting by -10s should clamp first entry to 0
        let shifted = apply_offset(&entries, -10_000);
        assert_eq!(shifted[0].start_ms, 0);
        assert_eq!(shifted[0].end_ms, 0);
    }

    #[test]
    fn test_roundtrip() {
        let entries = parse_srt(SAMPLE_SRT).unwrap();
        let out = write_srt(&entries);
        let re_parsed = parse_srt(&out).unwrap();
        assert_eq!(re_parsed.len(), entries.len());
        for (a, b) in entries.iter().zip(re_parsed.iter()) {
            assert_eq!(a.start_ms, b.start_ms);
            assert_eq!(a.end_ms, b.end_ms);
        }
    }

    #[test]
    fn test_parse_timestamp_line_flexible_arrow() {
        // SRTs in the wild use varying spacing around -->
        let variants = [
            "00:00:01,500 --> 00:00:04,000",  // standard
            "00:00:01,500-->00:00:04,000",    // no spaces
            "00:00:01,500  -->  00:00:04,000", // extra spaces
        ];
        for v in &variants {
            let (start, end) = parse_timestamp_line(v)
                .unwrap_or_else(|e| panic!("Failed on {:?}: {}", v, e));
            assert_eq!(start, 1_500, "start mismatch for {:?}", v);
            assert_eq!(end, 4_000, "end mismatch for {:?}", v);
        }
    }

    #[test]
    fn test_ms_to_srt_time() {
        assert_eq!(ms_to_srt_time(0), "00:00:00,000");
        assert_eq!(ms_to_srt_time(3_661_001), "01:01:01,001");
        assert_eq!(ms_to_srt_time(-500), "00:00:00,000"); // clamp
    }

    #[test]
    fn test_expected_signal() {
        let entries = parse_srt(SAMPLE_SRT).unwrap();
        // 100 Hz → 10 ms per sample; 10 seconds = 1000 samples
        let signal = entries_to_expected_signal(&entries, 1000, 100);
        // entry 1: 1500ms -> 4000ms  =  indices 150..400
        assert_eq!(signal[149], 0.0);
        assert_eq!(signal[150], 1.0);
        assert_eq!(signal[399], 1.0);
        assert_eq!(signal[400], 0.0);
    }
}
