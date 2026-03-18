---
name: minutes-record
description: Start or stop recording a meeting or voice memo. Captures audio from the default input device, transcribes with whisper.cpp, and saves as searchable markdown.
user_invocable: true
---

# /minutes record

Start or stop recording audio.

## Usage

To **start** recording:
```bash
minutes record
```
This captures audio from the default input device (built-in mic or BlackHole for system audio).
The recording runs until you press Ctrl-C or run `minutes stop` in another terminal.

To **stop** recording from Claude Code:
```bash
minutes stop
```
This stops the capture, transcribes the audio with whisper.cpp, and saves the result as markdown.

To **check status**:
```bash
minutes status
```

## Output

After stopping, the transcript is saved to `~/meetings/` as a markdown file with YAML frontmatter containing the title, date, duration, and full transcript.

## Prerequisites

- Whisper model downloaded: `minutes setup --model small`
- For system audio (Zoom/Meet): BlackHole virtual audio device installed
- For mic audio: works out of the box
