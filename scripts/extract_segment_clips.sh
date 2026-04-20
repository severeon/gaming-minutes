#!/usr/bin/env bash
# Extract audio clips from a session WAV using a segments.json produced by
# `minutes segment`. Each clip is written to a per-speaker subdirectory with
# a filename encoding segment id, duration, and start timestamp — so the
# longest (best-for-enrollment) clips sort to the top within each speaker.
#
# Writes clips.manifest.tsv alongside the clips: id, speaker, duration,
# start, end, voice_match_name, transcript_preview, output_path.
#
# Usage:
#   scripts/extract_segment_clips.sh [segments.json] [source.wav] [output_dir]
#
# Defaults:
#   segments.json  — $HOME/meetings/segments.json
#   source.wav     — .source.audio_path from the JSON; if relative, resolved
#                    against the directory of segments.json
#   output_dir     — <segments_json_dir>/<source_basename>.clips/
#
# Deps: jq, ffmpeg.

set -euo pipefail

SEGMENTS_JSON="${1:-$HOME/meetings/segments.json}"
SOURCE_AUDIO="${2:-}"
OUTPUT_DIR="${3:-}"

if [[ ! -f "$SEGMENTS_JSON" ]]; then
  echo "error: segments.json not found at: $SEGMENTS_JSON" >&2
  exit 1
fi

command -v jq >/dev/null     || { echo "error: jq not installed" >&2; exit 1; }
command -v ffmpeg >/dev/null  || { echo "error: ffmpeg not installed" >&2; exit 1; }

# Resolve source audio path from JSON if not given.
if [[ -z "$SOURCE_AUDIO" ]]; then
  SOURCE_AUDIO=$(jq -r '.source.audio_path' "$SEGMENTS_JSON")
fi

# If audio path isn't absolute / isn't found, try resolving relative to the
# directory containing segments.json.
if [[ ! -f "$SOURCE_AUDIO" ]]; then
  JSON_DIR=$(cd "$(dirname "$SEGMENTS_JSON")" && pwd)
  CANDIDATE="$JSON_DIR/$(basename "$SOURCE_AUDIO")"
  if [[ -f "$CANDIDATE" ]]; then
    SOURCE_AUDIO="$CANDIDATE"
  else
    echo "error: source audio not found" >&2
    echo "  tried: $SOURCE_AUDIO" >&2
    echo "  tried: $CANDIDATE" >&2
    exit 1
  fi
fi

# Derive output dir.
if [[ -z "$OUTPUT_DIR" ]]; then
  BASE=$(basename "$SOURCE_AUDIO")
  BASE="${BASE%.*}"
  OUTPUT_DIR="$(dirname "$SEGMENTS_JSON")/${BASE}.clips"
fi

mkdir -p "$OUTPUT_DIR"
MANIFEST="$OUTPUT_DIR/clips.manifest.tsv"
printf 'id\tspeaker\tduration_s\tstart\tend\tvoice_match\ttranscript_preview\tpath\n' > "$MANIFEST"

TOTAL=$(jq '.segments | length' "$SEGMENTS_JSON")
echo "extracting $TOTAL clips"
echo "  source: $SOURCE_AUDIO"
echo "  output: $OUTPUT_DIR"
echo

COUNT=0
SKIPPED=0
while IFS= read -r SEG; do
  ID=$(jq -r '.id' <<<"$SEG")
  START=$(jq -r '.start' <<<"$SEG")
  END=$(jq -r '.end' <<<"$SEG")
  DUR=$(jq -r '.duration_seconds' <<<"$SEG")
  SPEAKER=$(jq -r '.speaker_label // "UNLABELED"' <<<"$SEG")
  PREVIEW=$(jq -r '.transcript_preview // ""' <<<"$SEG")
  # Look up voice_match for this speaker from the speakers block.
  MATCH=$(jq -r --arg sp "$SPEAKER" \
    '.speakers[$sp].voice_match.name // ""' "$SEGMENTS_JSON")

  DUR_F=$(printf '%05.1f' "$DUR")          # 0023.4s sorts nicely
  START_SAFE="${START//:/-}"                # 00-02-50 or 00-02-50.098

  SPEAKER_DIR="$OUTPUT_DIR/$SPEAKER"
  mkdir -p "$SPEAKER_DIR"

  OUT_FILE="$SPEAKER_DIR/$(printf '%04d' "$ID")_${DUR_F}s_${START_SAFE}.wav"

  if [[ -f "$OUT_FILE" ]]; then
    SKIPPED=$((SKIPPED + 1))
  else
    # -ss/-to before -i is fast-seek (close-enough for enrollment clips).
    # Re-encode to 16k mono PCM so the output works directly with
    # `minutes enroll` / pyannote embedding extraction.
    ffmpeg -hide_banner -loglevel error -y \
      -ss "$START" -to "$END" \
      -i "$SOURCE_AUDIO" \
      -ar 16000 -ac 1 -c:a pcm_s16le \
      "$OUT_FILE"
    COUNT=$((COUNT + 1))
  fi

  printf '%d\t%s\t%.2f\t%s\t%s\t%s\t%s\t%s\n' \
    "$ID" "$SPEAKER" "$DUR" "$START" "$END" "$MATCH" \
    "${PREVIEW//$'\t'/ }" "${OUT_FILE#$OUTPUT_DIR/}" \
    >> "$MANIFEST"

  printf '  [%3d] %-11s %6.1fs  %s  %s\n' \
    "$ID" "$SPEAKER" "$DUR" "$START" "${PREVIEW:0:60}"
done < <(jq -c '.segments[]' "$SEGMENTS_JSON")

echo
echo "done: $COUNT written, $SKIPPED skipped (already existed)"
echo "manifest: $MANIFEST"
echo
echo "by speaker:"
for d in "$OUTPUT_DIR"/*/; do
  [[ -d "$d" ]] || continue
  N=$(find "$d" -maxdepth 1 -name '*.wav' | wc -l | tr -d ' ')
  SP=$(basename "$d")
  SP_MATCH=$(jq -r --arg sp "$SP" \
    '.speakers[$sp].voice_match.name // "— unmatched —"' "$SEGMENTS_JSON")
  printf '  %-11s  %3d clips  (%s)\n' "$SP" "$N" "$SP_MATCH"
done
