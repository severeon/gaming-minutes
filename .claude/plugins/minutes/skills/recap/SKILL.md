---
name: minutes-recap
description: Summarize today's meetings and voice memos into a daily digest.
user_invocable: true
---

# /minutes recap

Generate a daily digest of today's meetings and voice memos.

## Usage

```bash
minutes list --limit 20 -t meeting
minutes search "$(date +%Y-%m-%d)"
```

Then synthesize: read each meeting file, extract key points, decisions, and action items, and present a consolidated daily brief.

## What to include

1. Number of meetings/memos today
2. Key decisions across all meetings
3. Open action items (with assignees if available)
4. Topics discussed
5. Any follow-ups needed
