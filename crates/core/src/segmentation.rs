//! Session audio segmentation: single-speaker regions > N seconds,
//! per-speaker embeddings, voice matching against one or more
//! voices.db files. Output consumed by downstream enrollment and
//! transcript-editor tooling.

use std::fmt;

/// Parse a strict `HH:MM:SS` or `HH:MM:SS.sss` timestamp string
/// into fractional seconds. Rejects bare numbers ("90") and
/// shorter forms ("MM:SS") — callers pass ffmpeg-style timestamps.
pub fn parse_timestamp(s: &str) -> Result<f64, TimestampError> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return Err(TimestampError::Format(s.to_string()));
    }
    let hours: u64 = parts[0]
        .parse()
        .map_err(|_| TimestampError::Format(s.to_string()))?;
    let minutes: u64 = parts[1]
        .parse()
        .map_err(|_| TimestampError::Format(s.to_string()))?;
    let seconds: f64 = parts[2]
        .parse()
        .map_err(|_| TimestampError::Format(s.to_string()))?;
    if minutes >= 60 || seconds < 0.0 || seconds >= 60.0 {
        return Err(TimestampError::OutOfRange(s.to_string()));
    }
    Ok((hours as f64) * 3600.0 + (minutes as f64) * 60.0 + seconds)
}

/// Render an `f64` seconds value back into `HH:MM:SS.sss`.
pub fn format_timestamp(seconds: f64) -> String {
    let total_ms = (seconds * 1000.0).round() as i64;
    let ms = total_ms.rem_euclid(1000);
    let total_s = total_ms.div_euclid(1000);
    let s = total_s % 60;
    let total_min = total_s / 60;
    let m = total_min % 60;
    let h = total_min / 60;
    format!("{:02}:{:02}:{:02}.{:03}", h, m, s, ms)
}

/// Errors returned by [`parse_timestamp`].
#[derive(Debug, Clone, PartialEq)]
pub enum TimestampError {
    /// The input is not `HH:MM:SS` or `HH:MM:SS.sss`.
    Format(String),
    /// The input parses but a component is out of its canonical range.
    OutOfRange(String),
}

impl fmt::Display for TimestampError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Format(s) => write!(
                f,
                "invalid timestamp '{}': expected HH:MM:SS or HH:MM:SS.sss",
                s
            ),
            Self::OutOfRange(s) => write!(
                f,
                "timestamp '{}' has minutes or seconds out of [0, 60)",
                s
            ),
        }
    }
}

impl std::error::Error for TimestampError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_hh_mm_ss() {
        assert_eq!(parse_timestamp("01:22:07").unwrap(), 4927.0);
    }

    #[test]
    fn parses_with_milliseconds() {
        assert_eq!(parse_timestamp("00:00:03.120").unwrap(), 3.120);
    }

    #[test]
    fn rejects_bare_seconds() {
        assert!(matches!(
            parse_timestamp("90"),
            Err(TimestampError::Format(_))
        ));
    }

    #[test]
    fn rejects_mm_ss_only() {
        assert!(matches!(
            parse_timestamp("01:30"),
            Err(TimestampError::Format(_))
        ));
    }

    #[test]
    fn rejects_60_minutes() {
        assert!(matches!(
            parse_timestamp("00:60:00"),
            Err(TimestampError::OutOfRange(_))
        ));
    }

    #[test]
    fn format_roundtrips_known_values() {
        assert_eq!(format_timestamp(0.0), "00:00:00.000");
        assert_eq!(format_timestamp(3.120), "00:00:03.120");
        assert_eq!(format_timestamp(4927.5), "01:22:07.500");
    }

    #[test]
    fn format_handles_fractional_rounding() {
        assert_eq!(format_timestamp(59.9999), "00:01:00.000");
    }
}
