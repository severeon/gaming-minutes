use crate::config::Config;
use serde::{Deserialize, Serialize};
use std::path::Path;

// ──────────────────────────────────────────────────────────────
// Speaker diarization via subprocess.
//
// Engines:
//   "pyannote"   → Python pyannote.audio (best quality, needs Python)
//   "none"       → Skip diarization (default)
//
// Protocol:
//   Rust spawns a Python script that runs pyannote, outputs JSON to stdout.
//   No library linking — subprocess call is AGPL-safe.
//
// Output format:
//   [{"speaker": "SPEAKER_0", "start": 0.0, "end": 5.2},
//    {"speaker": "SPEAKER_1", "start": 5.2, "end": 12.8}, ...]
// ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerSegment {
    pub speaker: String,
    pub start: f64,
    pub end: f64,
}

#[derive(Debug, Clone)]
pub struct DiarizationResult {
    pub segments: Vec<SpeakerSegment>,
    pub num_speakers: usize,
}

/// Run speaker diarization on an audio file.
/// Returns None if diarization is disabled.
pub fn diarize(audio_path: &Path, config: &Config) -> Option<DiarizationResult> {
    let engine = &config.diarization.engine;

    if engine == "none" {
        return None;
    }

    tracing::info!(engine = %engine, file = %audio_path.display(), "running diarization");

    let result = match engine.as_str() {
        "pyannote" => diarize_with_pyannote(audio_path),
        other => {
            tracing::warn!(engine = %other, "unknown diarization engine, skipping");
            return None;
        }
    };

    match result {
        Ok(result) => {
            tracing::info!(
                speakers = result.num_speakers,
                segments = result.segments.len(),
                "diarization complete"
            );
            Some(result)
        }
        Err(e) => {
            tracing::error!(error = %e, "diarization failed, continuing without speaker labels");
            None
        }
    }
}

/// Apply diarization results to a transcript.
/// Replaces timestamp-only lines with speaker-labeled lines.
pub fn apply_speakers(transcript: &str, result: &DiarizationResult) -> String {
    let mut output = String::new();

    for line in transcript.lines() {
        // Parse timestamp from lines like "[0:00] text"
        if let Some(rest) = line.strip_prefix('[') {
            if let Some(bracket_end) = rest.find(']') {
                let ts_str = &rest[..bracket_end];
                let text = rest[bracket_end + 1..].trim();

                if let Some(secs) = parse_timestamp(ts_str) {
                    let speaker = find_speaker(secs, &result.segments);
                    output.push_str(&format!("[{} {}] {}\n", speaker, ts_str, text));
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push('\n');
    }

    output
}

/// Find which speaker is talking at a given timestamp.
fn find_speaker(time_secs: f64, segments: &[SpeakerSegment]) -> &str {
    for seg in segments {
        if time_secs >= seg.start && time_secs < seg.end {
            return &seg.speaker;
        }
    }
    "UNKNOWN"
}

/// Parse a timestamp like "0:00" or "1:30" into seconds.
fn parse_timestamp(ts: &str) -> Option<f64> {
    let parts: Vec<&str> = ts.split(':').collect();
    match parts.len() {
        2 => {
            let mins: f64 = parts[0].parse().ok()?;
            let secs: f64 = parts[1].parse().ok()?;
            Some(mins * 60.0 + secs)
        }
        3 => {
            let hours: f64 = parts[0].parse().ok()?;
            let mins: f64 = parts[1].parse().ok()?;
            let secs: f64 = parts[2].parse().ok()?;
            Some(hours * 3600.0 + mins * 60.0 + secs)
        }
        _ => None,
    }
}

/// Run pyannote diarization via Python subprocess.
fn diarize_with_pyannote(
    audio_path: &Path,
) -> Result<DiarizationResult, Box<dyn std::error::Error>> {
    // Check if Python is available
    let python = find_python()?;

    // Security: pass audio path as sys.argv[1], never interpolate into source code.
    // A crafted filename with quotes/backticks could inject arbitrary Python otherwise.
    let script = r#"
import json, sys
try:
    from pyannote.audio import Pipeline
    pipeline = Pipeline.from_pretrained("pyannote/speaker-diarization-3.1",
                                         use_auth_token=False)
    diarization = pipeline(sys.argv[1])
    segments = []
    for turn, _, speaker in diarization.itertracks(yield_label=True):
        segments.append({"speaker": speaker, "start": turn.start, "end": turn.end})
    print(json.dumps(segments))
except ImportError:
    print("ERROR: pyannote.audio not installed. Run: pip install pyannote.audio", file=sys.stderr)
    sys.exit(1)
except Exception as e:
    print(f"ERROR: {e}", file=sys.stderr)
    sys.exit(1)
"#;

    let output = std::process::Command::new(&python)
        .args(["-c", script, audio_path.to_str().unwrap_or("")])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("pyannote failed: {}", stderr).into());
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let segments: Vec<SpeakerSegment> = serde_json::from_str(&stdout)?;

    let num_speakers = segments
        .iter()
        .map(|s| s.speaker.as_str())
        .collect::<std::collections::HashSet<_>>()
        .len();

    Ok(DiarizationResult {
        segments,
        num_speakers,
    })
}

/// Find the Python interpreter.
fn find_python() -> Result<String, Box<dyn std::error::Error>> {
    for candidate in &["python3", "python"] {
        let result = std::process::Command::new(candidate)
            .args(["--version"])
            .output();
        if let Ok(output) = result {
            if output.status.success() {
                return Ok(candidate.to_string());
            }
        }
    }
    Err("Python not found. Install Python 3 for speaker diarization.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_timestamp_minutes_seconds() {
        assert_eq!(parse_timestamp("0:00"), Some(0.0));
        assert_eq!(parse_timestamp("1:30"), Some(90.0));
        assert_eq!(parse_timestamp("10:05"), Some(605.0));
    }

    #[test]
    fn parse_timestamp_hours() {
        assert_eq!(parse_timestamp("1:00:00"), Some(3600.0));
    }

    #[test]
    fn parse_timestamp_invalid() {
        assert_eq!(parse_timestamp("abc"), None);
        assert_eq!(parse_timestamp(""), None);
    }

    #[test]
    fn find_speaker_returns_correct_label() {
        let segments = vec![
            SpeakerSegment {
                speaker: "SPEAKER_0".into(),
                start: 0.0,
                end: 5.0,
            },
            SpeakerSegment {
                speaker: "SPEAKER_1".into(),
                start: 5.0,
                end: 10.0,
            },
        ];

        assert_eq!(find_speaker(2.5, &segments), "SPEAKER_0");
        assert_eq!(find_speaker(7.0, &segments), "SPEAKER_1");
        assert_eq!(find_speaker(15.0, &segments), "UNKNOWN");
    }

    #[test]
    fn apply_speakers_labels_transcript() {
        let transcript = "[0:00] Hello everyone\n[0:05] Thanks for joining\n";
        let result = DiarizationResult {
            segments: vec![
                SpeakerSegment {
                    speaker: "SPEAKER_0".into(),
                    start: 0.0,
                    end: 3.0,
                },
                SpeakerSegment {
                    speaker: "SPEAKER_1".into(),
                    start: 3.0,
                    end: 10.0,
                },
            ],
            num_speakers: 2,
        };

        let labeled = apply_speakers(transcript, &result);
        assert!(labeled.contains("[SPEAKER_0 0:00]"));
        assert!(labeled.contains("[SPEAKER_1 0:05]"));
    }

    #[test]
    fn diarize_returns_none_when_disabled() {
        let config = Config::default(); // engine = "none"
        let result = diarize(Path::new("/fake.wav"), &config);
        assert!(result.is_none());
    }
}
