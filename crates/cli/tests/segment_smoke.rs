//! Smoke test: `minutes segment` against the bundled demo WAV.
//! Uses --no-diarize to avoid pyannote model dependency in CI.

use std::process::Command;

#[test]
fn segment_demo_no_diarize_emits_valid_json() {
    let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("demo.wav");
    assert!(demo.exists(), "demo fixture missing at {}", demo.display());

    let output = Command::new(env!("CARGO_BIN_EXE_minutes"))
        .args([
            "segment",
            demo.to_str().unwrap(),
            "--no-diarize",
            "--min-secs",
            "0.1",
        ])
        .output()
        .expect("run minutes segment");

    assert!(
        output.status.success(),
        "minutes segment exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");
    assert!(json.get("source").is_some(), "source block present");
    assert!(json.get("params").is_some(), "params block present");
    assert!(json.get("segments").is_some(), "segments block present");
    assert!(json.get("stats").is_some(), "stats block present");
    // --no-diarize → speakers block absent or null
    let speakers = json.get("speakers");
    assert!(
        speakers.is_none() || speakers.unwrap().is_null(),
        "speakers should be absent or null, got: {:?}",
        speakers
    );
    // params.diarize should be false
    assert_eq!(
        json["params"]["diarize"].as_bool(),
        Some(false),
        "params.diarize should be false"
    );
}

#[test]
fn segment_preserves_timestamp_precision_in_params() {
    let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("demo.wav");
    assert!(demo.exists(), "demo fixture missing at {}", demo.display());

    let output = Command::new(env!("CARGO_BIN_EXE_minutes"))
        .args([
            "segment",
            demo.to_str().unwrap(),
            "--no-diarize",
            "--min-secs",
            "0.1",
            "--start",
            "00:00:00.500",
            "--end",
            "00:00:02.750",
        ])
        .output()
        .expect("run minutes segment");
    assert!(
        output.status.success(),
        "minutes segment exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");
    assert_eq!(
        json["params"]["start"].as_str(),
        Some("00:00:00.500"),
        "params.start must preserve millisecond precision"
    );
    assert_eq!(
        json["params"]["end"].as_str(),
        Some("00:00:02.750"),
        "params.end must preserve millisecond precision"
    );
}

#[test]
fn segment_auto_detects_companion_markdown_for_preview() {
    use std::io::Write;

    let tmp = tempfile::tempdir().expect("tempdir");
    let wav_path = tmp.path().join("session.wav");
    let md_path = tmp.path().join("session.md");

    // Copy demo.wav into the tempdir so the companion .md is found via
    // the with_extension("md") + exists() fallback in cmd_segment.
    let demo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("assets")
        .join("demo.wav");
    assert!(demo.exists(), "demo fixture missing at {}", demo.display());
    std::fs::copy(&demo, &wav_path).expect("copy demo wav");

    // Write a companion markdown transcript covering the segment range.
    let mut md = std::fs::File::create(&md_path).expect("create md");
    writeln!(md, "[0:00] hello world this is a test transcript").unwrap();
    writeln!(md, "[0:01] something else for a later segment").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_minutes"))
        .args([
            "segment",
            wav_path.to_str().unwrap(),
            "--no-diarize",
            "--min-secs",
            "0.1",
        ])
        .output()
        .expect("run minutes segment");
    assert!(
        output.status.success(),
        "minutes segment exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("output should be valid JSON");

    // Auto-detect must have fired: source.transcript_path should point at our .md.
    let transcript_path = json["source"]["transcript_path"].as_str();
    assert_eq!(
        transcript_path,
        Some(md_path.to_str().unwrap()),
        "auto-detected transcript path should be {}, got {:?}",
        md_path.display(),
        transcript_path
    );

    // If segments were produced, transcript_preview may be populated.
    // demo.wav may yield 0 segments at low VAD signal — treat that as graceful.
    if let Some(segments) = json["segments"].as_array() {
        if !segments.is_empty() {
            // transcript_preview is either a string or absent — both are acceptable
            // since VAD determines whether a segment falls in the transcript window.
            let preview = &segments[0]["transcript_preview"];
            assert!(
                preview.is_null() || preview.is_string(),
                "transcript_preview should be null or a string, got: {:?}",
                preview
            );
        }
    }
}
