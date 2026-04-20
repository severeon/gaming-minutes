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

use crate::diarize::SpeakerSegment;
use base64::Engine as _;

/// Encode an f32 embedding vector as base64-encoded little-endian
/// f32 bytes. Output is `len * 4` bytes before base64 wrapping.
pub fn encode_embedding(embedding: &[f32]) -> String {
    let bytes: Vec<u8> = embedding.iter().flat_map(|f| f.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(&bytes)
}

/// Decode a base64 f32 embedding string back into a Vec<f32>.
/// Errors if the decoded byte length isn't a multiple of 4.
pub fn decode_embedding(encoded: &str) -> Result<Vec<f32>, EmbeddingDecodeError> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| EmbeddingDecodeError::Base64(e.to_string()))?;
    if bytes.len() % 4 != 0 {
        return Err(EmbeddingDecodeError::TruncatedFloat(bytes.len()));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// Errors returned by [`decode_embedding`].
#[derive(Debug, Clone)]
pub enum EmbeddingDecodeError {
    /// The input was not valid base64.
    Base64(String),
    /// The decoded byte length is not a multiple of 4 (needed for f32).
    TruncatedFloat(usize),
}

impl fmt::Display for EmbeddingDecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Base64(s) => write!(f, "base64 decode failed: {}", s),
            Self::TruncatedFloat(n) => write!(
                f,
                "embedding byte length {} is not a multiple of 4",
                n
            ),
        }
    }
}

impl std::error::Error for EmbeddingDecodeError {}

/// Best-match a query embedding against profiles loaded from zero or more
/// voices.db paths. Returns `None` when no profile crosses `threshold` or
/// when no DB could be opened.
pub fn match_against_voices_dbs(
    query: &[f32],
    db_paths: &[PathBuf],
    threshold: f32,
) -> Option<VoiceMatch> {
    let mut best: Option<(String, String, f32)> = None;
    for path in db_paths {
        let conn = match crate::voice::open_db_at(path) {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(db = %path.display(), error = %e, "skipping voices.db (open failed)");
                continue;
            }
        };
        let profiles = match crate::voice::load_all_with_embeddings(&conn) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(db = %path.display(), error = %e, "skipping voices.db (load failed)");
                continue;
            }
        };
        for p in profiles {
            let sim = crate::voice::cosine_similarity(query, &p.embedding);
            let is_better = best.as_ref().map_or(true, |(_, _, s)| sim > *s);
            if is_better {
                best = Some((p.person_slug.clone(), p.name.clone(), sim));
            }
        }
    }
    match best {
        Some((slug, name, confidence)) if confidence >= threshold => Some(VoiceMatch {
            slug,
            name,
            confidence,
        }),
        _ => None,
    }
}

/// Merge consecutive same-speaker segments where the gap between them
/// is below `max_gap_seconds`. Input must be sorted by start time.
pub fn merge_same_speaker(input: &[SpeakerSegment], max_gap_seconds: f64) -> Vec<SpeakerSegment> {
    let mut out: Vec<SpeakerSegment> = Vec::with_capacity(input.len());
    for seg in input {
        match out.last_mut() {
            Some(last)
                if last.speaker == seg.speaker
                    && (seg.start - last.end) < max_gap_seconds =>
            {
                last.end = seg.end;
            }
            _ => out.push(seg.clone()),
        }
    }
    out
}

/// Drop segments whose duration is strictly less than `min_duration_seconds`.
pub fn filter_min_duration(
    input: &[SpeakerSegment],
    min_duration_seconds: f64,
) -> Vec<SpeakerSegment> {
    input
        .iter()
        .filter(|s| (s.end - s.start) >= min_duration_seconds)
        .cloned()
        .collect()
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

    #[test]
    fn embedding_base64_roundtrips() {
        let input: Vec<f32> = vec![0.1, -0.25, 1.0, 0.0, 3.14159];
        let encoded = encode_embedding(&input);
        let decoded = decode_embedding(&encoded).unwrap();
        assert_eq!(decoded.len(), input.len());
        for (a, b) in input.iter().zip(decoded.iter()) {
            assert!((a - b).abs() < 1e-6);
        }
    }

    #[test]
    fn embedding_base64_rejects_truncated_input() {
        // 3 bytes = can't form a complete f32 (needs 4)
        let bad = base64::engine::general_purpose::STANDARD.encode([0u8, 0, 0]);
        assert!(decode_embedding(&bad).is_err());
    }

    #[test]
    fn match_across_empty_dbs_returns_none() {
        let embedding = vec![1.0_f32; 192];
        let result = match_against_voices_dbs(&embedding, &[], 0.65);
        assert!(result.is_none());
    }

    #[test]
    fn match_below_threshold_returns_none() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let db = crate::voice::open_db_at(tmp.path()).unwrap();
        let mut emb = vec![0.0_f32; 192];
        emb[0] = 1.0;
        let cfg = crate::config::Config::default();
        crate::voice::save_profile(
            &db,
            "alice",
            "Alice",
            &emb,
            "test",
            crate::voice::model_version(&cfg),
        )
        .unwrap();

        let mut query = vec![0.0_f32; 192];
        query[1] = 1.0; // orthogonal
        let result = match_against_voices_dbs(&query, &[tmp.path().to_path_buf()], 0.65);
        assert!(result.is_none());
    }

    #[test]
    fn merger_combines_adjacent_same_speaker() {
        let input = vec![
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 5.0 },
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 5.2, end: 10.0 },
        ];
        let merged = merge_same_speaker(&input, 0.5);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].start, 0.0);
        assert_eq!(merged[0].end, 10.0);
    }

    #[test]
    fn merger_preserves_gap_above_threshold() {
        let input = vec![
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 5.0 },
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 6.0, end: 10.0 },
        ];
        let merged = merge_same_speaker(&input, 0.5);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merger_preserves_speaker_change() {
        let input = vec![
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 5.0 },
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_1".into(), start: 5.1, end: 10.0 },
        ];
        let merged = merge_same_speaker(&input, 0.5);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged[0].speaker, "SPEAKER_0");
        assert_eq!(merged[1].speaker, "SPEAKER_1");
    }

    #[test]
    fn filter_drops_below_min_duration() {
        let input = vec![
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 5.0 },
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_1".into(), start: 6.0, end: 20.0 },
        ];
        let kept = filter_min_duration(&input, 10.0);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].speaker, "SPEAKER_1");
    }

    #[test]
    fn filter_keeps_exact_min_duration() {
        let input = vec![crate::diarize::SpeakerSegment {
            speaker: "SPEAKER_0".into(),
            start: 0.0,
            end: 10.0,
        }];
        assert_eq!(filter_min_duration(&input, 10.0).len(), 1);
    }

    #[test]
    fn match_returns_best_across_multiple_dbs() {
        let tmp_a = tempfile::NamedTempFile::new().unwrap();
        let tmp_b = tempfile::NamedTempFile::new().unwrap();
        let db_a = crate::voice::open_db_at(tmp_a.path()).unwrap();
        let db_b = crate::voice::open_db_at(tmp_b.path()).unwrap();

        let cfg = crate::config::Config::default();
        let mv = crate::voice::model_version(&cfg);

        let mut alice = vec![0.0_f32; 192];
        alice[0] = 1.0;
        crate::voice::save_profile(&db_a, "alice", "Alice", &alice, "test", mv).unwrap();

        let bob = vec![0.1_f32; 192];
        crate::voice::save_profile(&db_b, "bob", "Bob", &bob, "test", mv).unwrap();

        let query = bob.clone();
        let result = match_against_voices_dbs(
            &query,
            &[tmp_a.path().to_path_buf(), tmp_b.path().to_path_buf()],
            0.5,
        )
        .expect("match");
        assert_eq!(result.slug, "bob");
        assert_eq!(result.name, "Bob");
        assert!(result.confidence > 0.99);
    }
}
