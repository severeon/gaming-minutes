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
