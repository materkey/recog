---
name: recog-meetings
description: Search and analyze recog meeting transcripts - find quotes by speaker, filter by date range, search by keywords
---

# Recog Meetings Search Skill

Search and analyze meeting transcripts from the recog speech recognition system.

## Quick Start

```bash
# Find all quotes from a speaker
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane Doe"

# With date range
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --from 2026-01-15 --to 2026-01-20

# Search by keyword
uv run .claude/skills/recog-meetings/search_meetings.py --text "gradle"

# Combined: speaker + keyword
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --text "релиз"
```

## Data Location

Meetings are stored under the recog data directory: `~/.recog/meetings/` by default, or `$RECOG_DATA_DIR/meetings/` when `RECOG_DATA_DIR` is set (see `recog.config.get_data_dir()`).

Override with:
- `--path /custom/path` argument
- `RECOG_MEETINGS_PATH` environment variable

## Data Format

Each meeting is stored in a folder `YYYY-MM-DD_HHMMSS_<uuid>/` containing:
- `meta.json` - meeting metadata (title, participants, stats)
- `transcript.jsonl` - transcript segments (speaker, text, timestamps)

Segment structure:
```json
{
  "id": 1,
  "ts": "2026-01-15T05:22:31+00:00",
  "ts_display": "08:22:31",
  "speaker": "Me",
  "speaker_id": "local",
  "text": "Текст реплики",
  "is_local": true,
  "cont": false
}
```

## Commands

### 1. Search by Speaker

Find all segments from a specific speaker (supports regex):

```bash
# Exact name
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane Doe"

# Partial match
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane"

# With date filter
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --from 2026-01-15

# With context (N lines before/after)
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --context 2

# JSON output
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --json

# Limit results
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --limit 10
```

### 2. Search by Text/Keyword

Find segments containing specific text (supports regex):

```bash
# Simple keyword
uv run .claude/skills/recog-meetings/search_meetings.py --text "gradle"

# Regex pattern
uv run .claude/skills/recog-meetings/search_meetings.py --text "релиз.*android"

# Combined with speaker
uv run .claude/skills/recog-meetings/search_meetings.py --speaker "Jane" --text "тест"
```

### 3. List Speakers

Get all speakers with statistics:

```bash
# All speakers
uv run .claude/skills/recog-meetings/search_meetings.py --list-speakers

# Speakers in date range
uv run .claude/skills/recog-meetings/search_meetings.py --list-speakers --from 2026-01-15

# JSON output
uv run .claude/skills/recog-meetings/search_meetings.py --list-speakers --json
```

Output shows: segments count, words count, meetings count per speaker.

### 4. Get Statistics

Overall database statistics:

```bash
uv run .claude/skills/recog-meetings/search_meetings.py --stats

# With date filter
uv run .claude/skills/recog-meetings/search_meetings.py --stats --from 2026-01-01 --to 2026-01-31
```

### 5. List Meetings

List all meetings:

```bash
# All meetings
uv run .claude/skills/recog-meetings/search_meetings.py --list-meetings

# Recent meetings
uv run .claude/skills/recog-meetings/search_meetings.py --list-meetings --from 2026-01-20

# JSON output
uv run .claude/skills/recog-meetings/search_meetings.py --list-meetings --json
```

### 6. Get Meeting Transcript

Full transcript of a specific meeting:

```bash
# By meeting ID
uv run .claude/skills/recog-meetings/search_meetings.py --meeting 2026-01-15_052231_310b068e

# JSON output
uv run .claude/skills/recog-meetings/search_meetings.py --meeting 2026-01-15_052231_310b068e --json
```

## Date Formats

Supported formats:
- `YYYY-MM-DD` (recommended): `2026-01-15`
- `DD.MM.YYYY`: `15.01.2026`
- `DD/MM/YYYY`: `15/01/2026`

## Output Formats

### Text (default)

Human-readable format:
```
[2026-01-15 Название встречи]
  [08:22:31] Jane Doe: Текст реплики
```

### JSON

Machine-readable with `--json`:
```json
[
  {
    "meeting_id": "2026-01-15_052231_310b068e",
    "meeting_title": "Название встречи",
    "meeting_date": "2026-01-15",
    "ts": "08:22:31",
    "speaker": "Jane Doe",
    "text": "Текст реплики"
  }
]
```

## Examples

### Find what Jane said about releases this month

```bash
uv run .claude/skills/recog-meetings/search_meetings.py \
  --speaker "Jane" \
  --text "релиз|release" \
  --from 2026-01-01 \
  --json
```

### Get context around specific quotes

```bash
uv run .claude/skills/recog-meetings/search_meetings.py \
  --speaker "Jane" \
  --text "проблем" \
  --context 3
```

### Find all mentions of a project

```bash
uv run .claude/skills/recog-meetings/search_meetings.py \
  --text "PROJ-\\d+" \
  --limit 50
```

### Who speaks the most?

```bash
uv run .claude/skills/recog-meetings/search_meetings.py --list-speakers --json | head -20
```

## Trigger Patterns

Use this skill when user asks to:
- Search meeting transcripts / "найди в митингах"
- Find quotes by speaker / "что говорил X" / "реплики Y"
- Search by keyword / "где упоминали" / "найди обсуждение"
- Meeting statistics / "статистика митингов"
- List speakers / "кто участвовал"
- List meetings / "какие были встречи"

## Meeting Name Aliases

Users often refer to meetings colloquially (дейлик, груминг, ретро, 1x1) while the
stored title is the formal conference name. When filtering by meeting type, look
up the actual title pattern in `--list-meetings` output first and search by that.

## Tips

1. **Use `--json` for programmatic processing** - pipe to `jq` for filtering
2. **Speaker names are case-insensitive** - "artur" matches "Jane Doe"
3. **Regex supported** - use patterns like `релиз.*android` or `PROJ-\d+`
4. **Context helps understand** - add `--context 2` to see surrounding dialogue
5. **Combine filters** - speaker + text + date range for precise search
