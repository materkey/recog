#!/usr/bin/env python3
# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""
Search meetings in recog transcript storage.

Examples:
    # Find all quotes from a speaker
    uv run search_meetings.py --speaker "Jane Doe"

    # Filter by date range
    uv run search_meetings.py --speaker "Jane" --from 2026-01-15 --to 2026-01-20

    # Search by keyword in text
    uv run search_meetings.py --text "gradle"

    # Combined search
    uv run search_meetings.py --speaker "Jane" --text "релиз"

    # List all speakers
    uv run search_meetings.py --list-speakers

    # Show meeting statistics
    uv run search_meetings.py --stats

    # List meetings in date range
    uv run search_meetings.py --list-meetings --from 2026-01-15

    # Full text from specific meeting
    uv run search_meetings.py --meeting 2026-01-15_052231_310b068e
"""

import argparse
import json
import os
import re
import sys
from collections import defaultdict
from datetime import datetime, date
from pathlib import Path


def get_meetings_path() -> Path:
    """Get meetings directory path from env or default."""
    env_path = os.getenv("RECOG_MEETINGS_PATH")
    if env_path:
        return Path(env_path)

    # Default: the recog data directory (see recog.config.get_data_dir)
    data_dir = os.getenv("RECOG_DATA_DIR", "").strip()
    default = (Path(data_dir).expanduser() if data_dir else Path.home() / ".recog") / "meetings"
    if default.exists():
        return default

    # Try current directory
    cwd = Path.cwd() / "meetings"
    if cwd.exists():
        return cwd

    return default


def parse_date(date_str: str) -> date:
    """Parse date string in various formats."""
    for fmt in ["%Y-%m-%d", "%d.%m.%Y", "%d/%m/%Y"]:
        try:
            return datetime.strptime(date_str, fmt).date()
        except ValueError:
            continue
    raise ValueError(f"Cannot parse date: {date_str}")


def load_meeting(meeting_dir: Path) -> dict | None:
    """Load meeting metadata and transcript."""
    meta_path = meeting_dir / "meta.json"
    transcript_path = meeting_dir / "transcript.jsonl"

    if not meta_path.exists():
        return None

    try:
        meta = json.loads(meta_path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return None

    segments = []
    if transcript_path.exists():
        content = transcript_path.read_text(encoding="utf-8").strip()
        if content:
            for line in content.split("\n"):
                if line.strip():
                    try:
                        segments.append(json.loads(line))
                    except json.JSONDecodeError:
                        continue

    return {"meta": meta, "segments": segments}


def iter_meetings(meetings_path: Path, from_date: date = None, to_date: date = None):
    """Iterate over meetings, optionally filtered by date."""
    if not meetings_path.exists():
        return

    for meeting_dir in sorted(meetings_path.iterdir(), reverse=True):
        if not meeting_dir.is_dir():
            continue

        # Quick date filter from directory name (YYYY-MM-DD_...)
        dir_name = meeting_dir.name
        if len(dir_name) >= 10 and dir_name[4] == '-' and dir_name[7] == '-':
            try:
                meeting_date = datetime.strptime(dir_name[:10], "%Y-%m-%d").date()
                if from_date and meeting_date < from_date:
                    continue
                if to_date and meeting_date > to_date:
                    continue
            except ValueError:
                pass

        meeting = load_meeting(meeting_dir)
        if meeting:
            yield meeting


def search_by_speaker(meetings_path: Path, speaker_pattern: str,
                      from_date: date = None, to_date: date = None,
                      text_pattern: str = None, context: int = 0) -> list[dict]:
    """Search for segments by speaker name."""
    results = []
    speaker_re = re.compile(speaker_pattern, re.IGNORECASE)
    text_re = re.compile(text_pattern, re.IGNORECASE) if text_pattern else None

    for meeting in iter_meetings(meetings_path, from_date, to_date):
        meta = meeting["meta"]
        segments = meeting["segments"]

        meeting_results = []
        for i, seg in enumerate(segments):
            # Skip system events
            if seg.get("speaker_id") == "__system__":
                continue

            speaker = seg.get("speaker", "")
            if not speaker_re.search(speaker):
                continue

            text = seg.get("text", "")
            if text_re and not text_re.search(text):
                continue

            result = {
                "meeting_id": meta["id"],
                "meeting_title": meta.get("title") or meta["id"],
                "meeting_date": meta.get("created_at", "")[:10],
                "ts": seg.get("ts_display", ""),
                "speaker": speaker,
                "text": text,
            }

            # Add context if requested
            if context > 0:
                context_before = []
                context_after = []
                for j in range(max(0, i - context), i):
                    if segments[j].get("speaker_id") != "__system__":
                        context_before.append({
                            "speaker": segments[j].get("speaker", ""),
                            "text": segments[j].get("text", "")
                        })
                for j in range(i + 1, min(len(segments), i + context + 1)):
                    if segments[j].get("speaker_id") != "__system__":
                        context_after.append({
                            "speaker": segments[j].get("speaker", ""),
                            "text": segments[j].get("text", "")
                        })
                if context_before:
                    result["context_before"] = context_before
                if context_after:
                    result["context_after"] = context_after

            meeting_results.append(result)

        results.extend(meeting_results)

    return results


def search_by_text(meetings_path: Path, text_pattern: str,
                   from_date: date = None, to_date: date = None) -> list[dict]:
    """Search for segments by text content."""
    results = []
    text_re = re.compile(text_pattern, re.IGNORECASE)

    for meeting in iter_meetings(meetings_path, from_date, to_date):
        meta = meeting["meta"]

        for seg in meeting["segments"]:
            if seg.get("speaker_id") == "__system__":
                continue

            text = seg.get("text", "")
            if not text_re.search(text):
                continue

            results.append({
                "meeting_id": meta["id"],
                "meeting_title": meta.get("title") or meta["id"],
                "meeting_date": meta.get("created_at", "")[:10],
                "ts": seg.get("ts_display", ""),
                "speaker": seg.get("speaker", "Unknown"),
                "text": text,
            })

    return results


def list_speakers(meetings_path: Path, from_date: date = None, to_date: date = None) -> dict:
    """Get list of all speakers with segment counts."""
    speaker_stats = defaultdict(lambda: {"count": 0, "meetings": set(), "words": 0})

    for meeting in iter_meetings(meetings_path, from_date, to_date):
        meta = meeting["meta"]

        for seg in meeting["segments"]:
            if seg.get("speaker_id") == "__system__":
                continue

            speaker = seg.get("speaker", "Unknown")
            text = seg.get("text", "")

            speaker_stats[speaker]["count"] += 1
            speaker_stats[speaker]["meetings"].add(meta["id"])
            speaker_stats[speaker]["words"] += len(text.split())

    # Convert sets to counts for JSON serialization
    result = {}
    for speaker, stats in sorted(speaker_stats.items(), key=lambda x: -x[1]["count"]):
        result[speaker] = {
            "segments": stats["count"],
            "meetings": len(stats["meetings"]),
            "words": stats["words"]
        }

    return result


def get_stats(meetings_path: Path, from_date: date = None, to_date: date = None) -> dict:
    """Get overall statistics."""
    stats = {
        "meetings": 0,
        "segments": 0,
        "speakers": set(),
        "words": 0,
        "earliest": None,
        "latest": None,
    }

    for meeting in iter_meetings(meetings_path, from_date, to_date):
        meta = meeting["meta"]
        stats["meetings"] += 1

        created = meta.get("created_at", "")[:10]
        if created:
            if stats["earliest"] is None or created < stats["earliest"]:
                stats["earliest"] = created
            if stats["latest"] is None or created > stats["latest"]:
                stats["latest"] = created

        for seg in meeting["segments"]:
            if seg.get("speaker_id") == "__system__":
                continue

            stats["segments"] += 1
            stats["speakers"].add(seg.get("speaker", "Unknown"))
            stats["words"] += len(seg.get("text", "").split())

    stats["speakers"] = len(stats["speakers"])
    return stats


def list_meetings(meetings_path: Path, from_date: date = None, to_date: date = None) -> list[dict]:
    """List all meetings."""
    results = []

    for meeting in iter_meetings(meetings_path, from_date, to_date):
        meta = meeting["meta"]
        participants = list(meta.get("participants", {}).values())

        results.append({
            "id": meta["id"],
            "title": meta.get("title"),
            "created_at": meta.get("created_at"),
            "ended_at": meta.get("ended_at"),
            "source": meta.get("source"),
            "segments": meta.get("stats", {}).get("segment_count", len(meeting["segments"])),
            "words": meta.get("stats", {}).get("words_count", 0),
            "participants": [p.get("name", "Unknown") for p in participants],
        })

    return results


def get_meeting_transcript(meetings_path: Path, meeting_id: str) -> list[dict]:
    """Get full transcript of a specific meeting."""
    meeting_dir = meetings_path / meeting_id
    meeting = load_meeting(meeting_dir)

    if not meeting:
        return []

    meta = meeting["meta"]
    result = []

    for seg in meeting["segments"]:
        if seg.get("speaker_id") == "__system__":
            continue

        result.append({
            "ts": seg.get("ts_display", ""),
            "speaker": seg.get("speaker", "Unknown"),
            "text": seg.get("text", ""),
        })

    return result


def format_output(data, output_format: str = "text"):
    """Format output for display."""
    if output_format == "json":
        print(json.dumps(data, ensure_ascii=False, indent=2))
        return

    # Text format
    if isinstance(data, list):
        for item in data:
            if "ts" in item and "speaker" in item and "text" in item:
                # Segment format
                meeting_info = f"[{item.get('meeting_date', '')} {item.get('meeting_title', '')}]" if item.get('meeting_id') else ""
                if meeting_info:
                    print(f"\n{meeting_info}")

                # Context before
                if item.get("context_before"):
                    for ctx in item["context_before"]:
                        print(f"  ... {ctx['speaker']}: {ctx['text']}")

                print(f"  [{item['ts']}] {item['speaker']}: {item['text']}")

                # Context after
                if item.get("context_after"):
                    for ctx in item["context_after"]:
                        print(f"  ... {ctx['speaker']}: {ctx['text']}")
            elif "id" in item:
                # Meeting format
                title = item.get("title") or "(без названия)"
                participants = ", ".join(item.get("participants", [])[:3])
                if len(item.get("participants", [])) > 3:
                    participants += f" +{len(item['participants']) - 3}"
                print(f"{item['id']}: {title}")
                print(f"  Сегментов: {item.get('segments', 0)}, Участники: {participants}")
    elif isinstance(data, dict):
        if "meetings" in data and "segments" in data:
            # Stats format
            print(f"Встреч: {data['meetings']}")
            print(f"Сегментов: {data['segments']}")
            print(f"Спикеров: {data['speakers']}")
            print(f"Слов: {data['words']}")
            if data.get("earliest"):
                print(f"Период: {data['earliest']} - {data['latest']}")
        else:
            # Speakers list
            for speaker, stats in data.items():
                print(f"{speaker}: {stats['segments']} реплик, {stats['words']} слов, {stats['meetings']} встреч")


def main():
    parser = argparse.ArgumentParser(
        description="Search recog meeting transcripts",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog=__doc__
    )

    # Search options
    parser.add_argument("--speaker", "-s", help="Search by speaker name (regex)")
    parser.add_argument("--text", "-t", help="Search by text content (regex)")
    parser.add_argument("--context", "-c", type=int, default=0,
                        help="Number of context lines before/after")

    # Date filters
    parser.add_argument("--from", dest="from_date", help="Start date (YYYY-MM-DD)")
    parser.add_argument("--to", dest="to_date", help="End date (YYYY-MM-DD)")

    # List/stats modes
    parser.add_argument("--list-speakers", action="store_true",
                        help="List all speakers with stats")
    parser.add_argument("--stats", action="store_true",
                        help="Show overall statistics")
    parser.add_argument("--list-meetings", action="store_true",
                        help="List all meetings")
    parser.add_argument("--meeting", "-m", help="Show transcript for specific meeting ID")

    # Output options
    parser.add_argument("--json", action="store_true", help="Output as JSON")
    parser.add_argument("--limit", "-n", type=int, help="Limit number of results")
    parser.add_argument("--path", help="Meetings directory path")

    args = parser.parse_args()

    meetings_path = Path(args.path) if args.path else get_meetings_path()

    if not meetings_path.exists():
        print(f"Error: Meetings directory not found: {meetings_path}", file=sys.stderr)
        print("Set RECOG_MEETINGS_PATH environment variable or use --path", file=sys.stderr)
        sys.exit(1)

    from_date = parse_date(args.from_date) if args.from_date else None
    to_date = parse_date(args.to_date) if args.to_date else None
    output_format = "json" if args.json else "text"

    # Execute appropriate action
    if args.list_speakers:
        data = list_speakers(meetings_path, from_date, to_date)
    elif args.stats:
        data = get_stats(meetings_path, from_date, to_date)
    elif args.list_meetings:
        data = list_meetings(meetings_path, from_date, to_date)
    elif args.meeting:
        data = get_meeting_transcript(meetings_path, args.meeting)
    elif args.speaker:
        data = search_by_speaker(meetings_path, args.speaker, from_date, to_date,
                                 args.text, args.context)
    elif args.text:
        data = search_by_text(meetings_path, args.text, from_date, to_date)
    else:
        parser.print_help()
        sys.exit(0)

    # Apply limit
    if args.limit and isinstance(data, list):
        data = data[:args.limit]

    format_output(data, output_format)


if __name__ == "__main__":
    main()
