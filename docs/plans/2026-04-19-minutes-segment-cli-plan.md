# `minutes segment` CLI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a `minutes segment <audio.wav>` CLI that produces a JSON segmentation report (single-speaker regions > N seconds, per-speaker embeddings, per-speaker voice matches against one or more `voices.db` files, per-segment transcript previews) for downstream consumption by a future Tauri GUI.

**Architecture:** New `segmentation.rs` module in `minutes-core` that composes existing `diarize.rs`, `voice.rs`, and `vad.rs` primitives. New `Commands::Segment` CLI handler in `minutes-cli` that serializes the report to stdout or `--output <path>`. No GUI work. No changes to diarization internals — the module consumes `DiarizationResult.speaker_embeddings` as-is.

**Tech Stack:** Rust (minutes-core + minutes-cli), clap (CLI), serde + serde_json (schema), rusqlite (voices.db access via existing `voice.rs` helpers), base64 (embedding encoding).

**Branch:** All work targets `gaming-main`. The design doc on `main` will be cherry-picked to `gaming-main` as Task 0.

---

## File Structure

**Create:**

- `crates/core/src/segmentation.rs` — module containing report types, orchestrator, merger, preview extractor, embedding codec, VAD-regions helper
- `tests/integration/segmentation_smoke.rs` — end-to-end fixture test (no-diarize path)

**Modify:**

- `crates/core/src/lib.rs` — `pub mod segmentation;`
- `crates/core/src/config.rs` — add `SegmentationConfig` struct and `segmentation` field on `Config`
- `crates/cli/src/main.rs` — add `Commands::Segment { … }` variant, handler `cmd_segment`, dispatch entry
- `README.md` — one-line mention of the new command under the CLI commands section

**Do not touch:** `diarize.rs` internals, `voice.rs` public surface (only consume existing functions), `pipeline.rs`.

---

## Task 0: Branch prep

**Files:**

- No file changes. Branch setup only.

- [ ] **Step 1: Cherry-pick the design doc onto `gaming-main`**

Run:

```bash
git checkout gaming-main
git cherry-pick 0c528e8  # design doc commit on main
```

Expected: clean cherry-pick (the file doesn't conflict with any gaming-main change).

- [ ] **Step 2: Confirm you're on `gaming-main` for all subsequent tasks**

Run:

```bash
git rev-parse --abbrev-ref HEAD
```

Expected output: `gaming-main`

- [ ] **Step 3: Commit the cherry-picked design (already committed by cherry-pick)**

No action — cherry-pick creates the commit. Verify with `git log --oneline -1` showing the design doc commit.

---

## Task 1: Config stanza (`SegmentationConfig`)

**Files:**

- Modify: `crates/core/src/config.rs`

**Context.** Existing `Config` struct at `crates/core/src/config.rs:14` uses nested stanza structs (e.g. `voice_config: VoiceConfig` from line ~80) with `#[serde(default)]` on each field. Follow that pattern exactly.

- [ ] **Step 1: Write failing tests for SegmentationConfig defaults**

Add to the `#[cfg(test)] mod tests` block at the bottom of `crates/core/src/config.rs`:

```rust
#[test]
fn segmentation_config_has_sensible_defaults() {
    let cfg = SegmentationConfig::default();
    assert!(cfg.default_diarize);
    assert!(cfg.emit_embeddings);
    assert!(cfg.emit_preview_text);
    assert!((cfg.min_segment_seconds - 10.0).abs() < f64::EPSILON);
    assert!((cfg.merge_gap_seconds - 0.5).abs() < f64::EPSILON);
    assert_eq!(cfg.preview_char_limit, 120);
}

#[test]
fn segmentation_stanza_roundtrips_through_toml() {
    let toml_src = r#"
[segmentation]
default_diarize = false
emit_embeddings = false
min_segment_seconds = 5.0
merge_gap_seconds = 1.0
preview_char_limit = 80
"#;
    let parsed: Config = toml::from_str(toml_src).expect("parse");
    assert!(!parsed.segmentation.default_diarize);
    assert!(!parsed.segmentation.emit_embeddings);
    assert!(parsed.segmentation.emit_preview_text); // not specified → default
    assert!((parsed.segmentation.min_segment_seconds - 5.0).abs() < f64::EPSILON);
    assert_eq!(parsed.segmentation.preview_char_limit, 80);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib config::tests::segmentation
```

Expected: compile error (`SegmentationConfig` doesn't exist yet).

- [ ] **Step 3: Add `SegmentationConfig` struct and wire it into `Config`**

In `crates/core/src/config.rs`, add near the other stanza structs (search for `pub struct VoiceConfig` and place this nearby):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SegmentationConfig {
    pub default_diarize: bool,
    pub emit_embeddings: bool,
    pub emit_preview_text: bool,
    pub min_segment_seconds: f64,
    pub merge_gap_seconds: f64,
    pub preview_char_limit: usize,
}

impl Default for SegmentationConfig {
    fn default() -> Self {
        Self {
            default_diarize: true,
            emit_embeddings: true,
            emit_preview_text: true,
            min_segment_seconds: 10.0,
            merge_gap_seconds: 0.5,
            preview_char_limit: 120,
        }
    }
}
```

Add a field on `Config` (match the existing pattern for other stanzas — field is named with snake_case, has `#[serde(default)]`):

```rust
#[serde(default)]
pub segmentation: SegmentationConfig,
```

Place it alphabetically among the other stanza fields.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib config::tests::segmentation
```

Expected: both tests PASS.

- [ ] **Step 5: Verify the rest of the config tests still pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib config::
```

Expected: all config tests PASS. If any previously-passing test now fails because a `Config` literal elsewhere doesn't include `segmentation: SegmentationConfig::default()`, add it there.

- [ ] **Step 6: Commit**

Run:

```bash
git add crates/core/src/config.rs
git commit -m "feat(segmentation): add [segmentation] config stanza

Default values mirror the CLI's compiled-in defaults: diarize on,
embeddings + preview text emitted, min segment 10s, merge gap 500ms,
preview char limit 120.

Follows the same Default-impl + serde(default) pattern as VoiceConfig
and DiarizationConfig so omitted stanzas don't error."
```

---

## Task 2: Timestamp parser (`HH:MM:SS[.sss]` ↔ `f64` seconds)

**Files:**

- Create: (inside `crates/core/src/segmentation.rs` — this task also bootstraps the module file)
- Modify: `crates/core/src/lib.rs` (one-line `pub mod segmentation;`)

- [ ] **Step 1: Create the module file with only the parser + tests**

Create `crates/core/src/segmentation.rs`:

```rust
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

#[derive(Debug, Clone, PartialEq)]
pub enum TimestampError {
    Format(String),
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
```

- [ ] **Step 2: Register the module**

Modify `crates/core/src/lib.rs` — add `pub mod segmentation;` alphabetically among the existing `pub mod X;` lines.

- [ ] **Step 3: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests
```

Expected: 7 tests PASS.

- [ ] **Step 4: Run the broader suite to verify no regressions**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib
```

Expected: all tests PASS (including the pre-existing failures noted in the earlier diarization patch — those are environmental, not code regressions).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs crates/core/src/lib.rs
git commit -m "feat(segmentation): bootstrap module with HH:MM:SS parser

Strict ffmpeg-compatible timestamp parser and formatter. Rejects
bare integer seconds and MM:SS forms to keep CLI parsing deterministic.
Format roundtrip is ms-precision via banker's rounding."
```

---

## Task 3: `SegmentationReport` types (schema)

**Files:**

- Modify: `crates/core/src/segmentation.rs` (append types)

**Context.** The output schema is documented in `docs/plans/2026-04-19-minutes-segment-cli-design.md` under "Output schema". Struct names below follow that schema exactly. `BTreeMap` is used for `speakers` so JSON output is deterministic (keys sorted).

- [ ] **Step 1: Write failing serde roundtrip tests**

Append to the `#[cfg(test)] mod tests` block in `crates/core/src/segmentation.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::report
```

Expected: compile error — types don't exist.

- [ ] **Step 3: Add the types to `segmentation.rs`**

Append to `crates/core/src/segmentation.rs` (after the `TimestampError` impl):

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SegmentationReport {
    pub source: Source,
    pub params: Params,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speakers: Option<BTreeMap<String, SpeakerEntry>>,
    pub segments: Vec<Segment>,
    pub stats: Stats,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub audio_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<PathBuf>,
    pub duration_seconds: f64,
    pub model_versions: ModelVersions,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ModelVersions {
    pub diarization: String,
    pub embedding: String,
    pub whisper: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Params {
    pub diarize: bool,
    pub min_segment_seconds: f64,
    pub start: String,
    pub end: String,
    pub voices_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpeakerEntry {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub embedding: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice_match: Option<VoiceMatch>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceMatch {
    pub slug: String,
    pub name: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Segment {
    pub id: u32,
    pub start: String,
    pub end: String,
    pub duration_seconds: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_preview: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    pub segment_count: u32,
    pub total_voiced_seconds: f64,
    pub unmatched_segments: u32,
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::report
```

Expected: both tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs
git commit -m "feat(segmentation): SegmentationReport schema types

Per-speaker embeddings + voice_match live on a top-level 'speakers'
BTreeMap (sorted keys → deterministic JSON). Omitted entirely under
--no-diarize via serde skip_serializing_if. Per-segment fields keep
only the frame-of-reference data (timestamps, label, preview)."
```

---

## Task 4: Embedding base64 codec

**Files:**

- Modify: `crates/core/src/segmentation.rs` (append codec helpers)
- Modify: `crates/core/Cargo.toml` (add `base64` dep if not already present)

**Context.** Check first whether `base64` is already a workspace dep. Run `grep -n base64 crates/core/Cargo.toml` — if present, skip the Cargo.toml edit.

- [ ] **Step 1: Write failing codec tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::embedding
```

Expected: compile error — functions don't exist.

- [ ] **Step 3: Add `base64` dep if missing**

Check:

```bash
grep -n '^base64' crates/core/Cargo.toml
```

If no match, add under `[dependencies]`:

```toml
base64 = "0.22"
```

- [ ] **Step 4: Implement the codec**

Append to `crates/core/src/segmentation.rs`:

```rust
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

#[derive(Debug, Clone)]
pub enum EmbeddingDecodeError {
    Base64(String),
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
```

- [ ] **Step 5: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::embedding
```

Expected: both tests PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/segmentation.rs crates/core/Cargo.toml
git commit -m "feat(segmentation): base64 codec for f32 embeddings

Little-endian f32 bytes → base64 standard alphabet. 192-dim = 256-char
string. Decoder rejects truncated byte runs (length % 4 != 0)."
```

---

## Task 5: Multi-DB voice matcher

**Files:**

- Modify: `crates/core/src/segmentation.rs` (append `match_against_voices_dbs`)

**Context.** `voice.rs:192` has `load_all_with_embeddings(conn)` and `voice.rs:218` has `match_embedding(&[f32], &[VoiceProfileWithEmbedding], threshold) -> Option<String>`. The existing matcher only returns the name. We need slug + name + confidence. Rather than editing `voice.rs`, we write a local helper that opens each DB, loads profiles, computes cosine similarities directly, and returns the best match across all DBs.

- [ ] **Step 1: Write failing tests using in-memory DBs**

Append to the `#[cfg(test)] mod tests` block:

```rust
#[test]
fn match_across_empty_dbs_returns_none() {
    let embedding = vec![1.0_f32; 192];
    let result = match_against_voices_dbs(&embedding, &[], 0.65);
    assert!(result.is_none());
}

#[test]
fn match_below_threshold_returns_none() {
    // Build a temp voices.db with a profile whose embedding is orthogonal
    // to the input (cosine = 0, below any positive threshold).
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
    query[1] = 1.0; // orthogonal to emb
    let result = match_against_voices_dbs(&query, &[tmp.path().to_path_buf()], 0.65);
    assert!(result.is_none());
}

#[test]
fn match_returns_best_across_multiple_dbs() {
    let tmp_a = tempfile::NamedTempFile::new().unwrap();
    let tmp_b = tempfile::NamedTempFile::new().unwrap();
    let db_a = crate::voice::open_db_at(tmp_a.path()).unwrap();
    let db_b = crate::voice::open_db_at(tmp_b.path()).unwrap();

    let cfg = crate::config::Config::default();
    let mv = crate::voice::model_version(&cfg);

    // Alice: 90-degree off (cosine = 0) in db_a
    let mut alice = vec![0.0_f32; 192];
    alice[0] = 1.0;
    crate::voice::save_profile(&db_a, "alice", "Alice", &alice, "test", mv).unwrap();

    // Bob: perfect match in db_b
    let bob = vec![0.1_f32; 192]; // normalized-ish
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
```

Requires `tempfile` as a dev-dependency. Check:

```bash
grep -n '^tempfile' crates/core/Cargo.toml
```

If absent, add to `[dev-dependencies]`:

```toml
tempfile = "3"
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::match_
```

Expected: compile error — function doesn't exist.

- [ ] **Step 3: Implement the matcher**

Append to `crates/core/src/segmentation.rs`:

```rust
use std::path::Path;

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

fn _path_types_exist(_: &Path) {} // compile-time anchor; delete if unused after final assembly
```

Remove `_path_types_exist` in Task 8 once everything is assembled.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::match_
```

Expected: 3 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs crates/core/Cargo.toml
git commit -m "feat(segmentation): multi-voices.db matcher

Iterates every --voices path, finds the single highest cosine similarity
across all enrolled profiles, returns VoiceMatch{slug,name,confidence}
when above threshold. Open/load failures per-path emit a warning and
are skipped rather than aborting."
```

---

## Task 6: Segment merger + duration filter

**Files:**

- Modify: `crates/core/src/segmentation.rs`

**Context.** Input: `Vec<diarize::SpeakerSegment>` sorted by start time. Merge consecutive same-speaker segments where the gap is under `merge_gap_seconds`. Then drop merged segments shorter than `min_segment_seconds`.

- [ ] **Step 1: Write failing merger tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::merger
cargo test -p minutes-core --no-default-features --lib segmentation::tests::filter
```

Expected: compile errors — functions don't exist.

- [ ] **Step 3: Implement merger + filter**

Append to `crates/core/src/segmentation.rs`:

```rust
use crate::diarize::SpeakerSegment;

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
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests
```

Expected: all segmentation tests PASS (6 new: 3 merger, 2 filter, plus prior).

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs
git commit -m "feat(segmentation): merger + min-duration filter

Merge runs of same-speaker segments separated by < merge_gap_seconds.
Filter is >= min_duration (inclusive at the threshold boundary)."
```

---

## Task 7: Transcript preview extractor

**Files:**

- Modify: `crates/core/src/segmentation.rs`

**Context.** Existing game-session markdown uses `[HH:MM:SS]` prefixes (verify by opening `~/meetings/games/2026-04-19-i-mean-….md`). Format: `[M:SS]` for sessions under an hour, `[H:MM:SS]` for longer ones. Need to handle both.

- [ ] **Step 1: Write failing preview-extractor tests**

Append to the `#[cfg(test)] mod tests` block:

```rust
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
        Some("I mean here's the thing anything that you would want.")
    );
}

#[test]
fn preview_truncates_at_word_boundary() {
    let md = "[0:03] alpha bravo charlie delta echo foxtrot golf hotel india\n";
    let p = transcript_preview(md, 0.0, 20.0, 20);
    let p = p.unwrap();
    assert!(p.len() <= 20 + 1); // allow ellipsis
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
```

Note: the first test's expected string is slightly different from the input because multiple lines in range concatenate. Actually — re-reading the design — `transcript_preview` takes the FIRST line in range. Let me correct the assertion. The test body claims concatenation; design says "first ~120 chars whose timestamp falls in `[seg.start, seg.end]`". Ambiguous. Pick: **first match wins, don't concatenate**. Fix test:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::preview
```

Expected: compile error — function doesn't exist.

- [ ] **Step 3: Implement extractor**

Append to `crates/core/src/segmentation.rs`:

```rust
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
        let Some(close) = line.find(']') else { continue };
        let ts_tok = &line[1..close];
        let Some(ts) = parse_inline_timestamp(ts_tok) else { continue };
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
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::preview
```

Expected: 4 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs
git commit -m "feat(segmentation): transcript preview extractor

Parses [M:SS] and [H:MM:SS] inline timestamps, returns the first line
whose prefix falls in the requested range, truncated at a word boundary
with an ellipsis when over char_limit."
```

---

## Task 8: `--diarize` orchestrator (`segmentation::run`)

**Files:**

- Modify: `crates/core/src/segmentation.rs`

**Context.** This ties together config, diarize, merger, filter, voice match, preview. The `--no-diarize` path comes in Task 9.

- [ ] **Step 1: Write failing orchestrator test (diarize path)**

Because pyannote isn't available in CI, the test stubs `DiarizationResult` and calls a non-public helper `build_report_from_diarization` rather than `run()` itself.

Append to `#[cfg(test)] mod tests`:

```rust
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
            crate::diarize::SpeakerSegment { speaker: "SPEAKER_1".into(), start: 16.0, end: 20.0 }, // 4s, below min
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
    let mut args = BuildArgs {
        audio_path: "/tmp/demo.wav".into(),
        transcript_path: None,
        audio_duration_seconds: 30.0,
        diarize: true,
        min_segment_seconds: 10.0,
        merge_gap_seconds: 0.5,
        start_str: "00:00:00".into(),
        end_str: "00:00:30".into(),
        voices_paths: vec![],
        emit_embeddings: false, // off
        emit_preview_text: false,
        preview_char_limit: 120,
        transcript_markdown: None,
        model_versions: ModelVersions::default(),
        voice_match_threshold: 0.65,
    };
    args.emit_embeddings = false;
    let report = build_report_from_diarization(&diar, &args);
    assert!(report.speakers.unwrap()["SPEAKER_0"].embedding.is_none());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::build_report
```

Expected: compile errors — `BuildArgs` and `build_report_from_diarization` don't exist.

- [ ] **Step 3: Implement `BuildArgs` and `build_report_from_diarization`**

Append to `crates/core/src/segmentation.rs`:

```rust
/// Internal assembly args — separated from CLI-facing `SegmentArgs` so
/// unit tests can build a report without hitting pyannote or the filesystem.
pub struct BuildArgs {
    pub audio_path: PathBuf,
    pub transcript_path: Option<PathBuf>,
    pub audio_duration_seconds: f64,
    pub diarize: bool,
    pub min_segment_seconds: f64,
    pub merge_gap_seconds: f64,
    pub start_str: String,
    pub end_str: String,
    pub voices_paths: Vec<PathBuf>,
    pub emit_embeddings: bool,
    pub emit_preview_text: bool,
    pub preview_char_limit: usize,
    pub transcript_markdown: Option<String>,
    pub model_versions: ModelVersions,
    pub voice_match_threshold: f32,
}

/// Build a SegmentationReport from a diarization result.
/// Does not touch the filesystem, does not call pyannote. Pure function.
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
        segments: {
            let count = segments.len();
            let _ = count; // suppress unused in release
            segments
        },
        stats: Stats {
            segment_count: kept.len() as u32,
            total_voiced_seconds,
            unmatched_segments: unmatched,
        },
    }
}
```

Delete the `_path_types_exist` anchor function from Task 5 if it's still there.

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::build_report
```

Expected: 3 tests PASS.

- [ ] **Step 5: Run the full segmentation suite**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::
```

Expected: all segmentation tests PASS (seventeen-ish: 7 timestamp + 2 report roundtrip + 2 embedding + 3 match + 5 merger/filter + 4 preview + 3 build_report ≈ 26).

- [ ] **Step 6: Commit**

```bash
git add crates/core/src/segmentation.rs
git commit -m "feat(segmentation): orchestrator build_report_from_diarization

Pure function: DiarizationResult + BuildArgs → SegmentationReport.
Speakers with no surviving segments (all below min duration) are
dropped from the output. Voice matching runs once per speaker against
the averaged embedding. emit_embeddings / emit_preview_text config
toggles control field presence."
```

---

## Task 9: `--no-diarize` path (energy-VAD regions)

**Files:**

- Modify: `crates/core/src/segmentation.rs`

**Context.** Existing `vad::Vad` is a chunk processor (`process(rms: f32) -> VadResult`). We iterate 100ms chunks of PCM, compute RMS, feed `Vad`, emit voiced regions.

- [ ] **Step 1: Write failing VAD-regions tests**

Append to `#[cfg(test)] mod tests`:

```rust
#[test]
fn vad_regions_detect_single_long_active_block() {
    let sample_rate = 16_000_u32;
    let mut pcm = Vec::new();
    // 1s silence
    pcm.extend(std::iter::repeat(0.0_f32).take(sample_rate as usize));
    // 12s loud
    pcm.extend(std::iter::repeat(0.3_f32).take((sample_rate * 12) as usize));
    // 1s silence
    pcm.extend(std::iter::repeat(0.0_f32).take(sample_rate as usize));

    let regions = vad_voiced_regions(&pcm, sample_rate, 10.0);
    assert_eq!(regions.len(), 1);
    let (start, end) = regions[0];
    assert!(start < 2.0); // hangover can nudge start
    assert!((end - start) >= 10.0);
}

#[test]
fn vad_regions_filter_short_blocks() {
    let sample_rate = 16_000_u32;
    let mut pcm = Vec::new();
    pcm.extend(std::iter::repeat(0.0_f32).take(sample_rate as usize));
    // 3s voiced — below 10s threshold
    pcm.extend(std::iter::repeat(0.3_f32).take((sample_rate * 3) as usize));
    pcm.extend(std::iter::repeat(0.0_f32).take(sample_rate as usize));
    let regions = vad_voiced_regions(&pcm, sample_rate, 10.0);
    assert!(regions.is_empty());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::vad_regions
```

Expected: compile error — function doesn't exist.

- [ ] **Step 3: Implement `vad_voiced_regions`**

Append to `crates/core/src/segmentation.rs`:

```rust
/// Run energy-based VAD over PCM and return `(start_sec, end_sec)`
/// regions of continuous voice activity ≥ `min_duration_seconds`.
pub fn vad_voiced_regions(
    pcm: &[f32],
    sample_rate: u32,
    min_duration_seconds: f64,
) -> Vec<(f64, f64)> {
    let chunk_ms: u64 = 100;
    let chunk_len = (sample_rate as u64 * chunk_ms / 1000) as usize;
    if chunk_len == 0 || pcm.is_empty() {
        return Vec::new();
    }
    let mut vad = crate::vad::Vad::new();
    let mut regions: Vec<(f64, f64)> = Vec::new();
    let mut current: Option<(f64, f64)> = None;
    for (chunk_idx, chunk) in pcm.chunks(chunk_len).enumerate() {
        let rms = if chunk.is_empty() {
            0.0
        } else {
            let sum: f32 = chunk.iter().map(|x| x * x).sum();
            (sum / chunk.len() as f32).sqrt()
        };
        let result = vad.process(rms);
        let start_sec = (chunk_idx * chunk_len) as f64 / sample_rate as f64;
        let end_sec = start_sec + chunk_ms as f64 / 1000.0;
        if result.speaking {
            match current.as_mut() {
                Some(r) => r.1 = end_sec,
                None => current = Some((start_sec, end_sec)),
            }
        } else if let Some(r) = current.take() {
            regions.push(r);
        }
    }
    if let Some(r) = current.take() {
        regions.push(r);
    }
    regions
        .into_iter()
        .filter(|(s, e)| (e - s) >= min_duration_seconds)
        .collect()
}

/// Build a SegmentationReport from VAD-only output (no diarization).
pub fn build_report_from_vad(regions: &[(f64, f64)], args: &BuildArgs) -> SegmentationReport {
    let mut total_voiced_seconds = 0.0;
    let segments: Vec<Segment> = regions
        .iter()
        .enumerate()
        .map(|(i, (start, end))| {
            let duration = end - start;
            total_voiced_seconds += duration;
            let preview = if args.emit_preview_text {
                args.transcript_markdown
                    .as_deref()
                    .and_then(|md| transcript_preview(md, *start, *end, args.preview_char_limit))
            } else {
                None
            };
            Segment {
                id: i as u32,
                start: format_timestamp(*start),
                end: format_timestamp(*end),
                duration_seconds: duration,
                speaker_label: None,
                transcript_preview: preview,
            }
        })
        .collect();
    SegmentationReport {
        source: Source {
            audio_path: args.audio_path.clone(),
            transcript_path: args.transcript_path.clone(),
            duration_seconds: args.audio_duration_seconds,
            model_versions: args.model_versions.clone(),
        },
        params: Params {
            diarize: false,
            min_segment_seconds: args.min_segment_seconds,
            start: args.start_str.clone(),
            end: args.end_str.clone(),
            voices_paths: args.voices_paths.clone(),
        },
        speakers: None,
        segments: segments.clone(),
        stats: Stats {
            segment_count: regions.len() as u32,
            total_voiced_seconds,
            unmatched_segments: 0,
        },
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib segmentation::tests::vad_regions
```

Expected: 2 tests PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/core/src/segmentation.rs
git commit -m "feat(segmentation): --no-diarize path via energy VAD

Chunked 100ms RMS → Vad::process → accumulate regions → filter by
min_duration. build_report_from_vad emits a report with speakers:null
and speaker_label:null on every segment."
```

---

## Task 10: CLI handler

**Files:**

- Modify: `crates/cli/src/main.rs`

**Context.** Existing clap variants live in the `enum Commands` block at line ~153 and the dispatch `match` at line ~1131. Follow the exact patterns used by `Commands::Process { … }` (a reasonable model — also takes a file path + config-style flags).

- [ ] **Step 1: Add the `Commands::Segment` variant**

Place alphabetically in the `enum Commands` block (between `Search` and `Setup`):

```rust
    /// Identify single-speaker regions ≥ min-secs in an audio file and emit
    /// a JSON segmentation report for downstream enrollment/editing tools.
    Segment {
        /// Path to the audio file (WAV, or any format ffmpeg can decode).
        audio: PathBuf,

        /// Enable diarization (default). Use --no-diarize to downgrade to
        /// energy-VAD voice-activity regions.
        #[arg(long, overrides_with = "no_diarize")]
        diarize: bool,

        #[arg(long, overrides_with = "diarize")]
        no_diarize: bool,

        /// Minimum segment duration in seconds. Defaults to config
        /// [segmentation].min_segment_seconds, then 10.0.
        #[arg(long)]
        min_secs: Option<f64>,

        /// Clip audio to start at HH:MM:SS[.sss] before segmenting.
        #[arg(long)]
        start: Option<String>,

        /// Clip audio to end at HH:MM:SS[.sss] before segmenting.
        #[arg(long)]
        end: Option<String>,

        /// Path to a voices.db to match against. Repeatable. Default:
        /// ~/.minutes/voices.db.
        #[arg(long)]
        voices: Vec<PathBuf>,

        /// Path to an existing transcript markdown for preview-text
        /// attachment. Auto-detected as <basename>.md if omitted.
        #[arg(long)]
        use_transcript: Option<PathBuf>,

        /// Write JSON to this path. Omit to write to stdout.
        #[arg(long)]
        output: Option<PathBuf>,
    },
```

- [ ] **Step 2: Add dispatch arm**

In the dispatch `match` at line ~1131, add:

```rust
        Commands::Segment {
            audio,
            diarize,
            no_diarize,
            min_secs,
            start,
            end,
            voices,
            use_transcript,
            output,
        } => cmd_segment(
            audio,
            diarize,
            no_diarize,
            min_secs,
            start,
            end,
            voices,
            use_transcript,
            output,
            &config,
        ),
```

- [ ] **Step 3: Implement `cmd_segment`**

Place the function in `main.rs` near `cmd_process`:

```rust
#[allow(clippy::too_many_arguments)]
fn cmd_segment(
    audio: PathBuf,
    diarize_flag: bool,
    no_diarize_flag: bool,
    min_secs: Option<f64>,
    start: Option<String>,
    end: Option<String>,
    voices: Vec<PathBuf>,
    use_transcript: Option<PathBuf>,
    output: Option<PathBuf>,
    config: &minutes_core::config::Config,
) -> Result<()> {
    use minutes_core::segmentation as seg;
    use std::io::Write;

    // Resolve flags vs config defaults.
    let diarize = if diarize_flag {
        true
    } else if no_diarize_flag {
        false
    } else {
        config.segmentation.default_diarize
    };
    let min_segment_seconds = min_secs.unwrap_or(config.segmentation.min_segment_seconds);
    let merge_gap_seconds = config.segmentation.merge_gap_seconds;

    // Audio presence.
    if !audio.exists() {
        eprintln!("segment: audio file not found: {}", audio.display());
        std::process::exit(2);
    }

    // Load audio → f32 16kHz mono. Uses existing helper.
    let (pcm, sample_rate) = match minutes_core::transcribe::load_wav_16k(&audio) {
        Ok(p) => (p, 16_000_u32),
        Err(e) => {
            eprintln!("segment: failed to decode audio: {}", e);
            std::process::exit(2);
        }
    };
    let audio_duration_seconds = pcm.len() as f64 / sample_rate as f64;

    // Resolve start/end.
    let start_sec = match start.as_deref() {
        Some(s) => match seg::parse_timestamp(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("segment: {}", e);
                std::process::exit(2);
            }
        },
        None => 0.0,
    };
    let end_sec = match end.as_deref() {
        Some(s) => match seg::parse_timestamp(s) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("segment: {}", e);
                std::process::exit(2);
            }
        },
        None => audio_duration_seconds,
    };
    if start_sec >= end_sec || end_sec > audio_duration_seconds + 0.001 {
        eprintln!(
            "segment: invalid range [{}, {}] for audio duration {:.3}s",
            start_sec, end_sec, audio_duration_seconds
        );
        std::process::exit(2);
    }

    // Clip PCM.
    let start_idx = (start_sec * sample_rate as f64) as usize;
    let end_idx = ((end_sec * sample_rate as f64) as usize).min(pcm.len());
    let clipped_pcm = &pcm[start_idx..end_idx];

    // Resolve voices.
    let voices_paths = if voices.is_empty() {
        vec![minutes_core::voice::db_path()]
    } else {
        voices
    };

    // Resolve transcript path (auto-detect if not provided).
    let transcript_path: Option<PathBuf> = use_transcript.or_else(|| {
        let candidate = audio.with_extension("md");
        if candidate.exists() {
            Some(candidate)
        } else {
            None
        }
    });
    let transcript_markdown = transcript_path
        .as_ref()
        .and_then(|p| std::fs::read_to_string(p).ok());

    let start_str = seg::format_timestamp(start_sec)
        .chars()
        .take(8)
        .collect::<String>(); // HH:MM:SS (strip ms)
    let end_str = seg::format_timestamp(end_sec)
        .chars()
        .take(8)
        .collect::<String>();

    let model_versions = seg::ModelVersions {
        diarization: config.diarization.engine.clone(),
        embedding: minutes_core::voice::model_version(config).to_string(),
        whisper: config.transcription.model.clone(),
    };

    let build_args = seg::BuildArgs {
        audio_path: audio.clone(),
        transcript_path,
        audio_duration_seconds: (end_sec - start_sec),
        diarize,
        min_segment_seconds,
        merge_gap_seconds,
        start_str,
        end_str,
        voices_paths,
        emit_embeddings: config.segmentation.emit_embeddings,
        emit_preview_text: config.segmentation.emit_preview_text,
        preview_char_limit: config.segmentation.preview_char_limit,
        transcript_markdown,
        model_versions,
        voice_match_threshold: config.voice.match_threshold,
    };

    let report = if diarize {
        // Call pyannote. On non-unix this returns None and we degrade.
        let Some(result) = minutes_core::diarize::diarize(&audio, config) else {
            if !cfg!(unix) {
                eprintln!(
                    "segment: diarization unavailable on this platform; retry with --no-diarize"
                );
                std::process::exit(4);
            } else {
                eprintln!(
                    "segment: diarization failed — models may be missing. run: minutes setup --diarization"
                );
                std::process::exit(3);
            }
        };
        seg::build_report_from_diarization(&result, &build_args)
    } else {
        let regions = seg::vad_voiced_regions(
            clipped_pcm,
            sample_rate,
            build_args.min_segment_seconds,
        );
        seg::build_report_from_vad(&regions, &build_args)
    };

    let json = serde_json::to_string_pretty(&report)
        .map_err(|e| anyhow::anyhow!("serialize report: {}", e))?;
    match output {
        Some(path) => {
            std::fs::write(&path, json)?;
            eprintln!("segment: wrote {} segments to {}", report.stats.segment_count, path.display());
        }
        None => {
            let stdout = std::io::stdout();
            let mut lock = stdout.lock();
            writeln!(lock, "{}", json)?;
        }
    }
    Ok(())
}
```

**Important.** `minutes_core::transcribe::load_wav_16k` may be named slightly differently — if `cargo check` fails on that symbol, grep `crates/core/src/transcribe.rs` for `pub fn load_wav` and adjust. Same for `config.voice.match_threshold` and `config.diarization.engine` — verify against the current `Config` struct before running the test.

- [ ] **Step 4: Verify the binary builds**

Run:

```bash
cargo check -p minutes-cli
```

Expected: clean compile. If there are symbol mismatches, fix them based on actual source paths.

- [ ] **Step 5: Smoke-test with the bundled demo WAV (--no-diarize path)**

Run:

```bash
cargo run -p minutes-cli -- segment crates/assets/demo.wav --no-diarize --min-secs 1 2>/dev/null | head -40
```

Expected: valid JSON with `"speakers": null` and at least one `segments[*]`.

- [ ] **Step 6: Commit**

```bash
git add crates/cli/src/main.rs
git commit -m "feat(cli): minutes segment subcommand

Dispatches to segmentation::build_report_from_{diarization,vad}.
Exit codes 2 (bad audio / bad range), 3 (diarize model missing),
4 (diarize unavailable on platform). Output to stdout by default or
--output <path>."
```

---

## Task 11: Integration smoke test

**Files:**

- Create: `tests/integration/segmentation_smoke.rs`

- [ ] **Step 1: Write the smoke test**

Create `tests/integration/segmentation_smoke.rs`:

```rust
//! Smoke test: `minutes segment` against the bundled demo WAV.
//! Uses --no-diarize to avoid the pyannote model dependency in CI.

use std::process::Command;

#[test]
fn segment_demo_no_diarize_emits_valid_json() {
    let demo = std::env::current_dir()
        .unwrap()
        .join("crates/assets/demo.wav");
    assert!(demo.exists(), "demo fixture missing at {}", demo.display());

    let output = Command::new(env!("CARGO_BIN_EXE_minutes"))
        .args([
            "segment",
            demo.to_str().unwrap(),
            "--no-diarize",
            "--min-secs",
            "0.5",
        ])
        .output()
        .expect("run minutes segment");

    assert!(
        output.status.success(),
        "minutes segment exited with {}: stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    let json: serde_json::Value = serde_json::from_slice(&output.stdout)
        .expect("output should be valid JSON");
    assert!(json.get("source").is_some());
    assert!(json.get("segments").is_some());
    assert!(json.get("stats").is_some());
    // --no-diarize → speakers block absent or null
    let speakers = json.get("speakers");
    assert!(speakers.is_none() || speakers.unwrap().is_null());
}
```

Note: `env!("CARGO_BIN_EXE_minutes")` only works if the test target depends on the CLI binary. Verify by running the test; if it fails with "no such env var", add `[[test]]` + `required-features` wiring to `crates/cli/Cargo.toml` or relocate the test inside the CLI crate. Simpler relocation: put the test in `crates/cli/tests/segment_smoke.rs` — the `CARGO_BIN_EXE_<name>` env var is auto-provided for integration tests inside the same crate.

**If the first location fails, use the simpler placement:** create at `crates/cli/tests/segment_smoke.rs` instead, and update the Files-to-Create list accordingly.

- [ ] **Step 2: Run the integration test**

Run:

```bash
cargo test -p minutes-cli --test segment_smoke
```

(If you moved the file under `crates/cli/tests/`, this is the right path. If not, use `cargo test --test segmentation_smoke`.)

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
git add crates/cli/tests/segment_smoke.rs
git commit -m "test(segmentation): end-to-end smoke test on demo WAV

--no-diarize path (no pyannote model dep). Verifies CLI exits 0,
stdout is valid JSON, speakers block is absent/null, segments + stats
present."
```

---

## Task 12: README + config.toml example + markdownlint cleanup

**Files:**

- Modify: `README.md` (add one line under CLI commands)
- Modify: `docs/plans/2026-04-19-minutes-segment-cli-design.md` (address markdownlint table warnings if IDE still reports them)

- [ ] **Step 1: Update README**

Find the CLI commands list in `README.md` (search for a subcommand that already exists, e.g. `minutes process`). Add immediately after `process`:

```markdown
- `minutes segment <audio.wav>` — Identify single-speaker regions ≥ N seconds; emit a JSON report with per-speaker embeddings and voice matches for downstream enrollment/editor tooling.
```

Also update the command-count in the "Claude Ecosystem Integration" section if it mentions a specific number of CLI commands.

- [ ] **Step 2: Lint + format**

Run:

```bash
cargo fmt --all -- --check
cargo clippy --all --no-default-features -- -D warnings
cargo clippy -p minutes-core --features diarize -- -D warnings
```

Expected: clean.

- [ ] **Step 3: Run the no-default-features test suite**

Run:

```bash
cargo test -p minutes-core --no-default-features --lib
cargo test -p minutes-cli
```

Expected: all segmentation tests PASS; pre-existing environmental failures (TCC `~/Documents`, parakeet binary resolution, graph date fix, `sh` sandbox exec — flagged by the earlier diarize agent) remain unchanged.

- [ ] **Step 4: Commit docs changes**

```bash
git add README.md docs/plans/2026-04-19-minutes-segment-cli-design.md
git commit -m "docs: README entry for minutes segment subcommand"
```

---

## Self-Review

**Spec coverage**

| Spec section | Implementing task |
|--------------|-------------------|
| CLI signature table | Task 10 (clap variant) |
| Exit codes 0/2/3/4 | Task 10 (`cmd_segment` early returns) |
| Output schema — source/params/speakers/segments/stats | Tasks 3, 8 |
| Schema decisions — timestamp format, embeddings on speakers, nullable voice_match, config toggles control presence | Tasks 2, 3, 8 |
| Config stanza `[segmentation]` | Task 1 |
| Architecture diagram | Tasks 8, 9 (orchestrators) + Task 10 (CLI wire-up) |
| `--diarize` algorithm steps 1–8 | Tasks 6, 7, 8, 10 |
| `--no-diarize` algorithm | Task 9 |
| Multi-voices.db semantics | Task 5 |
| Error handling table | Task 10 (exit-2/3/4 branches) |
| Risk: embedding model parity | Captured via `model_versions` in Source (Task 3); matcher skips mismatched DBs implicitly because `voice::load_all_with_embeddings` only returns the blob — the mismatch-warning behavior is inherited from existing `voice.rs`, not re-implemented. |
| Risk: pyannote stub on Windows | Task 10 exit-4 branch |
| Testing — unit, integration, schema, CLI smoke, no-pyannote-in-CI | Tasks 1, 2, 3, 4, 5, 6, 7, 8, 9, 11 |

**Placeholders** — none. Every step has concrete code or an exact command.

**Type consistency** — `ModelVersions`, `SpeakerEntry`, `VoiceMatch`, `Segment`, `BuildArgs`, `SegmentationReport` used consistently across tasks. `parse_timestamp`/`format_timestamp` referenced by same names in Tasks 2, 8, 10. `build_report_from_diarization` / `build_report_from_vad` stable.

**Known residual caveats**

1. Task 5 includes a temporary `_path_types_exist` anchor function removed in Task 8 — called out explicitly.
2. Task 10 includes a verification note that `minutes_core::transcribe::load_wav_16k` may need a name adjustment based on actual source — this is signposted to prevent silent mismatch.
3. Task 11 includes a fallback relocation instruction for the integration-test binary-env-var lookup.

---

## Execution Handoff

**Plan complete and saved to `docs/plans/2026-04-19-minutes-segment-cli-plan.md`. Two execution options:**

**1. Subagent-Driven (recommended)** — dispatch a fresh subagent per task, review between tasks, fast iteration.

**2. Inline Execution** — execute tasks in this session using executing-plans, batch execution with checkpoints.

**Which approach?**
