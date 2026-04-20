//! Session audio segmentation: single-speaker regions > N seconds,
//! per-speaker embeddings, voice matching against one or more
//! voices.db files. Output consumed by downstream enrollment and
//! transcript-editor tooling.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;

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

/// Top-level output of `minutes segment`. Serializes to the JSON schema
/// documented in `docs/plans/2026-04-19-minutes-segment-cli-design.md`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentationReport {
    /// Information about the input audio and models used.
    pub source: Source,
    /// Effective parameters for this segmentation run.
    pub params: Params,
    /// Per-speaker embeddings and voice matches. Absent under `--no-diarize`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<BTreeMap<String, SpeakerEntry>>,
    /// Ordered list of segments (single-speaker or voice-active regions).
    pub segments: Vec<Segment>,
    /// Aggregate stats for the run.
    pub stats: Stats,
}

/// Source audio + model metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    /// Absolute path to the input audio file.
    pub audio_path: PathBuf,
    /// Path to the transcript markdown used for previews, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    /// Duration of the audio in seconds.
    pub duration_seconds: f64,
    /// Versions of the models used to produce this report.
    pub model_versions: ModelVersions,
}

/// Model identifiers for the diarization, embedding, and whisper stages.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelVersions {
    /// Diarization engine identifier (e.g. "pyannote-3.1").
    pub diarization: String,
    /// Speaker-embedding model tag (e.g. "cam++-lm").
    pub embedding: String,
    /// Whisper / transcription model tag (e.g. "tdt-600m").
    pub whisper: String,
}

/// Effective parameters used for this segmentation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    /// Whether diarization was enabled.
    pub diarize: bool,
    /// Minimum segment duration in seconds.
    pub min_segment_seconds: f64,
    /// Clip start timestamp (HH:MM:SS).
    pub start: String,
    /// Clip end timestamp (HH:MM:SS).
    pub end: String,
    /// voices.db paths consulted for voice matching.
    pub voices_paths: Vec<PathBuf>,
}

/// Per-speaker metadata: averaged embedding and best voice match.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerEntry {
    /// Base64-encoded little-endian f32 averaged embedding. Absent when
    /// `emit_embeddings = false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
    /// Highest-confidence voice match across all voices.db paths.
    /// Absent when no profile crossed `config.voice.match_threshold`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_match: Option<VoiceMatch>,
}

/// A matched voice profile (above confidence threshold).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceMatch {
    /// Profile slug (primary key in voices.db).
    pub slug: String,
    /// Human-readable profile name.
    pub name: String,
    /// Cosine similarity of the speaker embedding to the profile.
    pub confidence: f32,
}

/// A single segment: either a single-speaker region (with diarization) or
/// a voice-active region (without).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    /// Zero-based segment id within this run.
    pub id: u32,
    /// Start timestamp (HH:MM:SS.sss).
    pub start: String,
    /// End timestamp (HH:MM:SS.sss).
    pub end: String,
    /// Segment duration in seconds.
    pub duration_seconds: f64,
    /// Speaker label from diarization (e.g. "SPEAKER_0"). Absent under
    /// `--no-diarize`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_label: Option<String>,
    /// First transcript line whose `[timestamp]` prefix falls in this
    /// segment's range, truncated to `preview_char_limit` chars at a word
    /// boundary. Absent when `emit_preview_text = false` or no matching
    /// line found.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_preview: Option<String>,
}

/// Aggregate stats surfaced alongside the segments.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    /// Number of segments that survived the duration filter.
    pub segment_count: u32,
    /// Total seconds of audio covered by the surviving segments.
    pub total_voiced_seconds: f64,
    /// Count of segments whose speaker had no voice match above threshold.
    pub unmatched_segments: u32,
}

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

    #[test]
    fn report_roundtrips_through_serde_json() {
        let report = SegmentationReport {
            source: Source {
                audio_path: "/tmp/x.wav".into(),
                transcript_path: None,
                duration_seconds: 721.8,
                model_versions: ModelVersions {
                    diarization: "pyannote-3.1".into(),
                    embedding: "cam++-lm".into(),
                    whisper: "tdt-600m".into(),
                },
            },
            params: Params {
                diarize: true,
                min_segment_seconds: 10.0,
                start: "00:00:00".into(),
                end: "00:12:01".into(),
                voices_paths: vec!["/tmp/voices.db".into()],
            },
            speakers: Some({
                let mut m = std::collections::BTreeMap::new();
                m.insert(
                    "SPEAKER_0".to_string(),
                    SpeakerEntry {
                        embedding: Some("AAAA".into()),
                        voice_match: Some(VoiceMatch {
                            slug: "thomas".into(),
                            name: "Thomas".into(),
                            confidence: 0.89,
                        }),
                    },
                );
                m
            }),
            segments: vec![Segment {
                id: 0,
                start: "00:00:03.120".into(),
                end: "00:00:18.450".into(),
                duration_seconds: 15.33,
                speaker_label: Some("SPEAKER_0".into()),
                transcript_preview: Some("I mean here's the thing…".into()),
            }],
            stats: Stats {
                segment_count: 1,
                total_voiced_seconds: 15.33,
                unmatched_segments: 0,
            },
        };
        let json = serde_json::to_string(&report).unwrap();
        let back: SegmentationReport = serde_json::from_str(&json).unwrap();
        assert_eq!(back.segments.len(), 1);
        assert_eq!(back.segments[0].speaker_label.as_deref(), Some("SPEAKER_0"));
        assert_eq!(
            back.speakers
                .as_ref()
                .and_then(|m| m.get("SPEAKER_0"))
                .and_then(|s| s.voice_match.as_ref())
                .map(|v| v.confidence),
            Some(0.89)
        );
    }

    #[test]
    fn no_diarize_report_omits_speakers_and_labels() {
        let report = SegmentationReport {
            source: Source {
                audio_path: "/tmp/x.wav".into(),
                transcript_path: None,
                duration_seconds: 60.0,
                model_versions: ModelVersions::default(),
            },
            params: Params {
                diarize: false,
                min_segment_seconds: 10.0,
                start: "00:00:00".into(),
                end: "00:01:00".into(),
                voices_paths: vec![],
            },
            speakers: None,
            segments: vec![Segment {
                id: 0,
                start: "00:00:05.000".into(),
                end: "00:00:25.000".into(),
                duration_seconds: 20.0,
                speaker_label: None,
                transcript_preview: None,
            }],
            stats: Stats {
                segment_count: 1,
                total_voiced_seconds: 20.0,
                unmatched_segments: 0,
            },
        };
        let json = serde_json::to_string(&report).unwrap();
        assert!(!json.contains("\"speakers\""), "speakers should be omitted, got: {}", json);
        assert!(
            !json.contains("\"speaker_label\""),
            "speaker_label should be omitted, got: {}",
            json
        );
    }
}
