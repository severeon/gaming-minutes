# Design: `minutes segment` CLI

**Status:** Draft (awaiting review)
**Branch:** targets `gaming-main`
**Author:** severeon (Thomas)
**Date:** 2026-04-19

## Problem

Reviewing and enrolling player voices after a tabletop session is manual.
A 4-hour game recording contains ~100+ stretches where a single player is
talking long enough (>10s) to make a clean voice-enrollment sample, but
there is no tooling to surface them. The user ends up scrubbing blindly.

Two downstream consumers need this primitive:

1. **Post-session enrollment** — identify candidate clips, review, name,
   persist to `voices.db`.
2. **Transcript editor (future)** — a scrubbable timeline with speaker
   labels editable per-segment, collapsible sections, reprocess-on-save.

This design defines a CLI primitive `minutes segment` that produces the
segmentation data both consumers need. The GUI is a separate milestone.

## Non-goals

- GUI / desktop app (separate milestone).
- Per-speaker enrollment action from within this command (future:
  `minutes enroll --from-segment`).
- Streaming / live segmentation. File-only.
- Coupling to games / tabletop content-type. Primitive is
  content-type-agnostic.
- `--campaign <slug>` sugar. Deferred until Arcanum content-packs define
  the `voices.db` path convention.

## CLI signature

```text
minutes segment <audio.wav>
  [--diarize | --no-diarize]   # default: --diarize
  [--min-secs <float>]         # default from config; fallback 10.0
  [--start HH:MM:SS[.sss]]     # ffmpeg -ss compatible
  [--end HH:MM:SS[.sss]]       # ffmpeg -to compatible
  [--voices <path>]...         # repeatable; default [~/.minutes/voices.db]
  [--use-transcript <path.md>] # auto-detected if <basename>.md sits next to wav
  [--output <path>]            # default: stdout
```

Exit codes:

| Code | Meaning |
|------|---------|
| 0    | Success (including valid JSON with zero segments) |
| 2    | Missing or unreadable audio file |
| 3    | Diarization model not downloaded; stderr advises `minutes setup --diarization` |
| 4    | `--diarize` requested on a platform where pyannote-rs stubs out |

## Output schema

```json
{
  "source": {
    "audio_path": "/…/2026-04-17-gulbar-s-outcasts.wav",
    "transcript_path": "/…/2026-04-19-i-mean-….md",
    "duration_seconds": 721.8,
    "model_versions": {
      "diarization": "pyannote-3.1",
      "embedding":   "wespeaker-v2",
      "whisper":     "tdt-600m"
    }
  },
  "params": {
    "diarize": true,
    "min_segment_seconds": 10.0,
    "start": "00:00:00",
    "end":   "00:12:01",
    "voices_paths": ["/Users/tquick/.minutes/voices.db"]
  },
  "speakers": {
    "SPEAKER_0": {
      "embedding": "base64-encoded-768-bytes",
      "voice_match": { "slug": "thomas", "name": "Thomas", "confidence": 0.89 }
    },
    "SPEAKER_1": {
      "embedding": "base64-encoded-768-bytes",
      "voice_match": null
    }
  },
  "segments": [
    {
      "id": 0,
      "start": "00:00:03.120",
      "end":   "00:00:18.450",
      "duration_seconds": 15.33,
      "speaker_label": "SPEAKER_0",
      "transcript_preview": "I mean here's the thing anything that you would need…"
    }
  ],
  "stats": {
    "segment_count": 23,
    "total_voiced_seconds": 612.4,
    "unmatched_segments": 8
  }
}
```

### Schema decisions

- Timestamps are `HH:MM:SS.sss` strings (ffmpeg-friendly, human-readable,
  lexicographically sortable). `duration_seconds` is a decimal convenience
  — subtracting two timestamp strings in GUI code is annoying.
- **Embeddings live on the `speakers` block, not per-segment.** Pyannote
  already clusters segments into SPEAKER_X groups and exposes one
  averaged embedding per speaker (`DiarizationResult.speaker_embeddings`
  in `diarize.rs`). A 4-hour session with 5 speakers carries 5 × 768B
  = 3.8KB of embedding data total, not 150KB. Per-segment
  variation within a speaker is not useful to the GUI — clustering is
  already done, label-propagation is trivial (`speaker_label ==
  speaker_label`).
- Embeddings are base64-encoded 192×4=768-byte little-endian f32 blobs,
  inline.
- `voice_match` is also per-speaker — it's the highest-confidence profile
  for that speaker's averaged embedding. Nullable when no profile crosses
  `config.voice.match_threshold` (default 0.65 per existing `voice.rs`).
- `speakers` block is omitted entirely under `--no-diarize`.
  `speaker_label` on each segment is also absent in that mode.
- `transcript_preview` is nullable — absent when no transcript is
  available. Char limit from config, default 120, truncated at word
  boundary.
- **Config toggles control field presence.** Setting
  `emit_embeddings = false` omits the `embedding` field under each
  speaker entirely (not `null`), same for `emit_preview_text = false`
  and `transcript_preview`. Callers that need smaller JSON can turn
  these off; the GUI consumer requires both on.
- **Timestamp input format is strict `HH:MM:SS[.sss]`.** Bare
  integer-seconds (`--start 90`) is rejected with a parse error. Matches
  the user's explicit ask for ffmpeg-compatible format and keeps CLI
  parsing deterministic.
- `stats` is a convenience block; the GUI could compute it, but surfacing
  it saves a loop and documents ground truth.

## Config stanza

`~/.config/minutes/config.toml`:

```toml
[segmentation]
default_diarize       = true
emit_embeddings       = true
emit_preview_text     = true
min_segment_seconds   = 10.0
merge_gap_seconds     = 0.5
preview_char_limit    = 120
```

All values overridable by CLI flags. Omitted stanza = compiled-in
defaults. Matches the pattern used by every other `[section]` in the
existing `config.toml`.

## Architecture

One new module `crates/core/src/segmentation.rs`, one new CLI handler
`Commands::Segment` in `crates/cli/src/main.rs`. Depends on existing
`diarize.rs`, `voice.rs`, `transcribe.rs`, `vad.rs`. No Tauri or desktop
work in this milestone.

```text
CLI (minutes segment <wav>)
  ↓
segmentation::run(args, config) -> SegmentationReport
  ├─ if --no-diarize: vad::detect_voiced_regions(wav)
  └─ else:            diarize::diarize(wav, config)
                      → merge_contiguous_same_speaker()
                      → filter_min_duration(args.min_secs)
                      → extract_embeddings_per_segment()
                      → match_against_voices(args.voices_paths)
                      → attach_transcript_preview(args.use_transcript)
  ↓
serde_json::to_writer_pretty(output, &report)
```

Public surface:

```rust
pub struct SegmentationReport {
    pub source: Source,
    pub params: Params,
    pub speakers: Option<std::collections::BTreeMap<String, SpeakerEntry>>, // None under --no-diarize
    pub segments: Vec<Segment>,
    pub stats: Stats,
}

pub fn run(args: &SegmentArgs, config: &Config) -> Result<SegmentationReport, Error>;
```

## Algorithm — `--diarize` (default)

1. Load WAV → 16kHz mono PCM via existing `transcribe::load_wav_16k`.
2. Clip to `[args.start, args.end]` when provided. Both are optional;
   half-open intervals supported.
3. Call `diarize::diarize(audio_path, config)` (returns
   `DiarizationResult` with `segments: Vec<SpeakerSegment>` plus
   `speaker_embeddings: HashMap<speaker_label, averaged_embedding>`).
4. Merge adjacent `SpeakerSegment`s where `speaker` matches and gap <
   `config.segmentation.merge_gap_seconds` (default 500ms).
5. Drop merged segments where `duration < args.min_secs`.
6. Retain only the speakers that still appear in the surviving segments
   — drop entries from `speaker_embeddings` for speakers whose segments
   all fell below the duration threshold.
7. For each remaining speaker, call `voice::match_embedding(averaged,
   profiles, threshold)` across every `--voices` path. Keep the single
   highest-confidence match if `confidence ≥
   config.voice.match_threshold`. Otherwise `voice_match = null`.
8. If transcript path available, parse `[HH:MM:SS]` lines and attach
   the first ~120 chars whose timestamp falls in `[seg.start, seg.end]`
   as `transcript_preview`. Truncate at word boundary.

## Algorithm — `--no-diarize`

1. Steps 1–2 as above.
2. Run Silero VAD via existing `vad::detect_voiced_regions`.
3. Filter `< min_secs`.
4. Emit with `speaker_label: null`, `voice_match: null`, `embedding:
   null`. `transcript_preview` still attached if available.

## Multi-`voices.db` semantics

- `--voices <path>` is repeatable. Default `[~/.minutes/voices.db]`.
- The matcher iterates every path, computes best per-profile confidence,
  returns the single highest across all DBs (or null).
- If a profile's `model_version` mismatches the pyannote embedding model,
  skip that profile. Emit a stderr warning once per DB.
- `voices_paths` in the output JSON records exactly which DBs were
  consulted — important for reproducibility.

This is the point where future Arcanum campaign content-packs plug in:
`--voices ~/arcanum/campaigns/turtles/voices.db --voices ~/.minutes/voices.db`
stacks campaign-scoped enrollments on top of the user's global store,
with no new primitives required.

## Error handling

| Condition | Behavior |
|-----------|----------|
| Missing audio file | Exit 2, stderr `segment: audio file not found: <path>` |
| Unreadable audio (decode failure) | Exit 2, stderr includes decoder error |
| `voices.db` at a given path is missing | Warn on stderr, continue with that DB skipped |
| All `voices.db` paths missing | Warn once, proceed with empty voice set (no `voice_match` anywhere) |
| Pyannote model not downloaded | Exit 3, stderr `run: minutes setup --diarization` |
| `--diarize` on non-unix (pyannote-rs stub) | Exit 4, stderr `diarization unavailable on this platform; try --no-diarize` |
| `--use-transcript` file unparseable | Warn, continue without `transcript_preview` |
| Zero segments ≥ `min_secs` | Exit 0, valid JSON with `segments: []`, stderr note |
| `--start` ≥ `--end` or outside audio duration | Exit 2, stderr explaining the bound |

## Risks

- **Embedding model parity.** The design assumes pyannote-rs segment
  embeddings and `voices.db` enrollment embeddings use the same model.
  If they diverge (e.g. different `wespeaker` variant), confidences are
  garbage. Mitigation: the CLI records
  `source.model_versions.embedding` in the output, and the matcher
  already compares `voice_profiles.model_version` per profile — a
  mismatch becomes a skip with a warning, not a silently wrong match.
  Verified during implementation on a known-enrolled voice.
- **Pyannote stub on Windows / non-unix.** Existing feature-gated stub
  in `diarize.rs` returns `Err` on non-unix. Exit 4 is the
  cross-platform contract. Tests gated accordingly.
- **Long-session memory.** A 4-hour WAV at 16kHz mono is ~460MB of
  PCM — fits in RAM on any dev machine. If this becomes a concern for
  longer recordings (8h+), iterate on streaming load in v2.
- **`--start/--end` precision.** `00:00:03` is three seconds, not three
  samples. Rounding to the nearest 16kHz sample is fine; document that
  segment boundaries are rounded to 10ms ticks (pyannote's native
  resolution).

## Testing

- **Unit** (`segmentation.rs`):
  - merge logic: contiguous same-speaker, gap threshold boundaries,
    cross-speaker boundary preserved
  - duration filter edge cases (exactly `min_secs`, 0.01 under)
  - timestamp clipping: both bounds, one bound, neither bound, inverted
  - transcript preview: timestamp-in-range matching, word-boundary
    truncation, missing transcript
- **Integration** (`tests/integration/segmentation.rs`):
  - fixture WAV from `crates/assets/demo.wav` → full pipeline → assert
    non-empty JSON, schema shape, at least one segment, no panics
- **Schema** (serde round-trip): `SegmentationReport` serializes +
  deserializes without drift
- **CLI smoke** (`tests/integration/cli.rs`): `minutes segment demo.wav
  --no-diarize` exits 0 with parseable JSON
- **No pyannote model in CI.** Integration tests use `--no-diarize`
  path. Diarize path exercised in `#[ignore]`-gated local test. Same
  pattern as existing whisper tests.

## Open questions

None at spec-write time. Specific call-outs the user confirmed:

- Merge gap threshold: 500ms. Acceptable.
- Exit code scheme: 0/2/3/4 distinct per failure class.
- `--no-diarize` path: kept in v1 as a "just find voice-active regions"
  escape hatch.

## Milestones after this

1. **GUI-side consumer (Tauri panel).** Loads the JSON, renders
   waveform + segments list, click-to-play, click-to-label,
   label-propagation via in-browser cosine matching on embeddings.
2. **`minutes enroll --from-segment`.** Writes the selected segment's
   embedding into a specified `voices.db` as a named profile.
3. **Scrubbable transcript editor.** Consumes both the markdown and the
   segment JSON, allows editing transcript text + speaker assignments,
   round-trips to markdown with edits preserved. Reprocess button
   re-runs `minutes segment` on the edited audio bounds.
