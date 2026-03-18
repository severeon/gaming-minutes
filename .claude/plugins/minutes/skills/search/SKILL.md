---
name: minutes-search
description: Search past meeting transcripts and voice memos. Finds text across all recordings with date and type filtering.
user_invocable: true
---

# /minutes search

Search meeting transcripts and voice memos.

## Usage

```bash
minutes search "pricing strategy"
minutes search "onboarding" -t memo
minutes search "API design" --since 2026-03-01 --limit 5
```

## Flags

- `-t, --content-type <meeting|memo>` — Filter by type
- `--since <date>` — Only show results after this date (ISO format)
- `-l, --limit <n>` — Maximum results (default: 10)

## Output

Returns JSON with matching meetings/memos including:
- Title, date, content type
- Snippet showing the matching text
- File path to the full transcript

The search is case-insensitive and matches against both the transcript body and the title.
