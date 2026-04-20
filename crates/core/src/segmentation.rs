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

/// Parse a `[M:SS]`, `[MM:SS]`, or `[H:MM:SS]` / `[HH:MM:SS]` timestamp
/// prefix into seconds. Returns `None` when the prefix isn't a timestamp.
fn parse_inline_timestamp(tok: &str) -> Option<f64> {
    let parts: Vec<&str> = tok.split(':').collect();
    let seconds: f64 = match parts.len() {
        2 => {
            let m: u64 = parts[0].parse().ok()?;
            let s: f64 = parts[1].parse().ok()?;
            (m as f64) * 60.0 + s
        }
        3 => {
            let h: u64 = parts[0].parse().ok()?;
            let m: u64 = parts[1].parse().ok()?;
            let s: f64 = parts[2].parse().ok()?;
            (h as f64) * 3600.0 + (m as f64) * 60.0 + s
        }
        _ => return None,
    };
    Some(seconds)
}

/// Find the first transcript line whose `[timestamp]` prefix falls in
/// `[start_sec, end_sec)`. Truncate its body to `char_limit` chars at
/// a word boundary, appending `…` when truncated.
pub fn transcript_preview(
    markdown: &str,
    start_sec: f64,
    end_sec: f64,
    char_limit: usize,
) -> Option<String> {
    for line in markdown.lines() {
        let line = line.trim();
        if !line.starts_with('[') {
            continue;
        }
        let close = match line.find(']') {
            Some(c) => c,
            None => continue,
        };
        let ts_tok = &line[1..close];
        let ts = match parse_inline_timestamp(ts_tok) {
            Some(t) => t,
            None => continue,
        };
        if ts < start_sec || ts >= end_sec {
            continue;
        }
        let body = line[close + 1..].trim();
        return Some(truncate_on_word_boundary(body, char_limit));
    }
    None
}

fn truncate_on_word_boundary(s: &str, limit: usize) -> String {
    if s.chars().count() <= limit {
        return s.to_string();
    }
    let mut acc = String::new();
    let mut last_space = None;
    for (idx, ch) in s.chars().enumerate() {
        if idx >= limit {
            break;
        }
        if ch.is_whitespace() {
            last_space = Some(acc.len());
        }
        acc.push(ch);
    }
    if let Some(pos) = last_space {
        acc.truncate(pos);
    }
    acc.push('…');
    acc
}

/// Internal assembly args — separated from CLI-facing `SegmentArgs` so
/// unit tests can build a report without hitting pyannote or the filesystem.
pub struct BuildArgs {
    /// Absolute path to the input audio file.
    pub audio_path: PathBuf,
    /// Transcript markdown path if known.
    pub transcript_path: Option<PathBuf>,
    /// Duration of the clipped audio window in seconds.
    pub audio_duration_seconds: f64,
    /// Whether diarization was enabled.
    pub diarize: bool,
    /// Minimum segment duration in seconds.
    pub min_segment_seconds: f64,
    /// Merge gap threshold in seconds.
    pub merge_gap_seconds: f64,
    /// Clip start as HH:MM:SS string (for the output `params` block).
    pub start_str: String,
    /// Clip end as HH:MM:SS string (for the output `params` block).
    pub end_str: String,
    /// voices.db paths to consult for voice matching.
    pub voices_paths: Vec<PathBuf>,
    /// Whether to emit base64 embeddings in the `speakers` block.
    pub emit_embeddings: bool,
    /// Whether to populate `transcript_preview` on segments.
    pub emit_preview_text: bool,
    /// Max chars for each preview (truncated at word boundary).
    pub preview_char_limit: usize,
    /// Loaded transcript markdown content (for preview extraction).
    pub transcript_markdown: Option<String>,
    /// Model version strings to surface in the `source.model_versions` block.
    pub model_versions: ModelVersions,
    /// Minimum cosine similarity for a `voice_match` to stick.
    pub voice_match_threshold: f32,
}

/// Build a `SegmentationReport` from a diarization result.
/// Pure function: does not touch the filesystem or call pyannote.
pub fn build_report_from_diarization(
    diar: &crate::diarize::DiarizationResult,
    args: &BuildArgs,
) -> SegmentationReport {
    let merged = merge_same_speaker(&diar.segments, args.merge_gap_seconds);
    let kept = filter_min_duration(&merged, args.min_segment_seconds);

    let mut surviving_speakers: std::collections::BTreeSet<String> =
        std::collections::BTreeSet::new();
    for seg in &kept {
        surviving_speakers.insert(seg.speaker.clone());
    }

    let mut speakers: BTreeMap<String, SpeakerEntry> = BTreeMap::new();
    for label in &surviving_speakers {
        let averaged = diar.speaker_embeddings.get(label);
        let embedding = if args.emit_embeddings {
            averaged.map(|e| encode_embedding(e))
        } else {
            None
        };
        let voice_match = averaged.and_then(|e| {
            match_against_voices_dbs(e, &args.voices_paths, args.voice_match_threshold)
        });
        speakers.insert(
            label.clone(),
            SpeakerEntry {
                embedding,
                voice_match,
            },
        );
    }

    let mut total_voiced_seconds = 0.0;
    let mut unmatched = 0_u32;
    let segments: Vec<Segment> = kept
        .iter()
        .enumerate()
        .map(|(i, seg)| {
            let duration = seg.end - seg.start;
            total_voiced_seconds += duration;
            let label = Some(seg.speaker.clone());
            if let Some(ref l) = label {
                if speakers
                    .get(l)
                    .and_then(|s| s.voice_match.as_ref())
                    .is_none()
                {
                    unmatched += 1;
                }
            }
            let preview = if args.emit_preview_text {
                args.transcript_markdown.as_deref().and_then(|md| {
                    transcript_preview(md, seg.start, seg.end, args.preview_char_limit)
                })
            } else {
                None
            };
            Segment {
                id: i as u32,
                start: format_timestamp(seg.start),
                end: format_timestamp(seg.end),
                duration_seconds: duration,
                speaker_label: label,
                transcript_preview: preview,
            }
        })
        .collect();

    let segment_count = kept.len() as u32;

    SegmentationReport {
        source: Source {
            audio_path: args.audio_path.clone(),
            transcript_path: args.transcript_path.clone(),
            duration_seconds: args.audio_duration_seconds,
            model_versions: args.model_versions.clone(),
        },
        params: Params {
            diarize: args.diarize,
            min_segment_seconds: args.min_segment_seconds,
            start: args.start_str.clone(),
            end: args.end_str.clone(),
            voices_paths: args.voices_paths.clone(),
        },
        speakers: Some(speakers),
        segments,
        stats: Stats {
            segment_count,
            total_voiced_seconds,
            unmatched_segments: unmatched,
        },
    }
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
    fn preview_extracts_first_line_in_range() {
        let md = "\
[0:03] I mean here's the thing anything that you would need or want.
[0:10] I mean uh uh Gul Gulbar
[0:14] will literally provide it for you.
";
        let p = transcript_preview(md, 0.0, 20.0, 120);
        assert_eq!(
            p.as_deref(),
            Some("I mean here's the thing anything that you would need or want.")
        );
    }

    #[test]
    fn preview_truncates_at_word_boundary() {
        let md = "[0:03] alpha bravo charlie delta echo foxtrot golf hotel india\n";
        let p = transcript_preview(md, 0.0, 20.0, 20);
        let p = p.unwrap();
        assert!(p.len() <= 20 + 3); // allow multibyte ellipsis
        assert!(p.ends_with('…') || p.ends_with("echo") || p.ends_with("delta"));
        assert!(!p.contains("foxtr")); // shouldn't split a word mid-way
    }

    #[test]
    fn preview_handles_hms_prefix() {
        let md = "[1:22:07] about three thousand words in\n";
        let p = transcript_preview(md, 4927.0, 5000.0, 120);
        assert!(p.as_deref().unwrap().contains("three thousand"));
    }

    #[test]
    fn preview_none_when_no_line_in_range() {
        let md = "[0:03] too early\n[1:00] too late\n";
        let p = transcript_preview(md, 10.0, 50.0, 120);
        assert!(p.is_none());
    }

    #[test]
    fn build_report_populates_speakers_and_segments() {
        let diar = crate::diarize::DiarizationResult {
            segments: vec![
                crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 12.0 },
                crate::diarize::SpeakerSegment { speaker: "SPEAKER_1".into(), start: 13.0, end: 30.0 },
            ],
            num_speakers: 2,
            from_stems: false,
            speaker_embeddings: {
                let mut m = std::collections::HashMap::new();
                m.insert("SPEAKER_0".to_string(), vec![0.1_f32; 192]);
                m.insert("SPEAKER_1".to_string(), vec![0.2_f32; 192]);
                m
            },
        };
        let args = BuildArgs {
            audio_path: "/tmp/demo.wav".into(),
            transcript_path: None,
            audio_duration_seconds: 60.0,
            diarize: true,
            min_segment_seconds: 10.0,
            merge_gap_seconds: 0.5,
            start_str: "00:00:00".into(),
            end_str: "00:01:00".into(),
            voices_paths: vec![],
            emit_embeddings: true,
            emit_preview_text: false,
            preview_char_limit: 120,
            transcript_markdown: None,
            model_versions: ModelVersions::default(),
            voice_match_threshold: 0.65,
        };
        let report = build_report_from_diarization(&diar, &args);
        let speakers = report.speakers.expect("speakers block");
        assert_eq!(speakers.len(), 2);
        assert!(speakers.contains_key("SPEAKER_0"));
        assert!(speakers["SPEAKER_0"].embedding.is_some());
        assert_eq!(report.segments.len(), 2);
        assert_eq!(report.stats.segment_count, 2);
    }

    #[test]
    fn build_report_drops_speaker_when_all_segments_below_min() {
        let diar = crate::diarize::DiarizationResult {
            segments: vec![
                crate::diarize::SpeakerSegment { speaker: "SPEAKER_0".into(), start: 0.0, end: 15.0 },
                crate::diarize::SpeakerSegment { speaker: "SPEAKER_1".into(), start: 16.0, end: 20.0 },
            ],
            num_speakers: 2,
            from_stems: false,
            speaker_embeddings: {
                let mut m = std::collections::HashMap::new();
                m.insert("SPEAKER_0".to_string(), vec![0.1; 192]);
                m.insert("SPEAKER_1".to_string(), vec![0.2; 192]);
                m
            },
        };
        let args = BuildArgs {
            audio_path: "/tmp/demo.wav".into(),
            transcript_path: None,
            audio_duration_seconds: 30.0,
            diarize: true,
            min_segment_seconds: 10.0,
            merge_gap_seconds: 0.5,
            start_str: "00:00:00".into(),
            end_str: "00:00:30".into(),
            voices_paths: vec![],
            emit_embeddings: true,
            emit_preview_text: false,
            preview_char_limit: 120,
            transcript_markdown: None,
            model_versions: ModelVersions::default(),
            voice_match_threshold: 0.65,
        };
        let report = build_report_from_diarization(&diar, &args);
        let speakers = report.speakers.unwrap();
        assert!(speakers.contains_key("SPEAKER_0"));
        assert!(!speakers.contains_key("SPEAKER_1"));
        assert_eq!(report.segments.len(), 1);
    }

    #[test]
    fn build_report_omits_embeddings_when_config_off() {
        let diar = crate::diarize::DiarizationResult {
            segments: vec![crate::diarize::SpeakerSegment {
                speaker: "SPEAKER_0".into(), start: 0.0, end: 15.0,
            }],
            num_speakers: 1,
            from_stems: false,
            speaker_embeddings: {
                let mut m = std::collections::HashMap::new();
                m.insert("SPEAKER_0".to_string(), vec![0.1; 192]);
                m
            },
        };
        let args = BuildArgs {
            audio_path: "/tmp/demo.wav".into(),
            transcript_path: None,
            audio_duration_seconds: 30.0,
            diarize: true,
            min_segment_seconds: 10.0,
            merge_gap_seconds: 0.5,
            start_str: "00:00:00".into(),
            end_str: "00:00:30".into(),
            voices_paths: vec![],
            emit_embeddings: false,
            emit_preview_text: false,
            preview_char_limit: 120,
            transcript_markdown: None,
            model_versions: ModelVersions::default(),
            voice_match_threshold: 0.65,
        };
        let report = build_report_from_diarization(&diar, &args);
        assert!(report.speakers.unwrap()["SPEAKER_0"].embedding.is_none());
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
