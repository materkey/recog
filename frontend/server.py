#!/usr/bin/env python3
"""
Web Frontend Server for Recog Transcripts

FastAPI server providing:
- REST API for meetings and transcripts
- WebSocket for live transcript streaming
- Static file serving for SPA
"""

import asyncio
import json
import os
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Optional
from contextlib import asynccontextmanager

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from pydantic import BaseModel
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler

# Add parent directory to path for imports (must be before ai_provider which imports recog.node_env)
sys.path.insert(0, str(Path(__file__).parent.parent))

from ai_provider import get_provider, get_all_providers, build_prompt
from dotenv import load_dotenv

# Load .env from project root
load_dotenv(Path(__file__).parent.parent / ".env")

from meeting_storage import MeetingStorage, get_external_paths
from bridge_subscriber import BridgeSubscriber
from live_watcher import LiveWatcher
from transcript_watcher import TranscriptWatcher

# Configuration
HOST = "127.0.0.1"
PORT = 9080
MEETINGS_DIR = Path(__file__).parent.parent / "meetings"
BRIDGE_WS_URL = os.environ.get("RECOG_BRIDGE_WS_URL", "ws://localhost:8766")


def get_storage() -> MeetingStorage:
    """Get MeetingStorage instance with external paths."""
    return MeetingStorage(MEETINGS_DIR, external_paths=get_external_paths())


# --- Helpers (WDIM / Cognitive Anchor) ---

def _parse_iso_ts(value: Optional[str]) -> Optional[datetime]:
    if not value:
        return None
    try:
        return datetime.fromisoformat(value)
    except ValueError:
        if value.endswith("Z"):
            try:
                return datetime.fromisoformat(value[:-1] + "+00:00")
            except ValueError:
                return None
    return None


def _tail_lines(path: Path, max_lines: int, chunk_size: int = 8192) -> list[str]:
    if max_lines <= 0 or not path.exists():
        return []

    lines: list[bytes] = []
    buffer = b""
    with open(path, "rb") as f:
        f.seek(0, os.SEEK_END)
        position = f.tell()

        while position > 0 and len(lines) < max_lines:
            read_size = min(chunk_size, position)
            position -= read_size
            f.seek(position)
            buffer = f.read(read_size) + buffer

            while len(lines) < max_lines:
                idx = buffer.rfind(b"\n")
                if idx == -1:
                    break
                line = buffer[idx + 1:]
                buffer = buffer[:idx]
                if line:
                    lines.append(line)

        if buffer and len(lines) < max_lines:
            lines.append(buffer)

    lines = [line.decode("utf-8", errors="ignore") for line in reversed(lines)]
    return lines


def _load_recent_segments(transcript_path: Path, max_lines: int) -> list[dict]:
    segments = []
    for line in _tail_lines(transcript_path, max_lines):
        line = line.strip()
        if not line:
            continue
        try:
            segments.append(json.loads(line))
        except json.JSONDecodeError:
            continue
    return segments


def _select_anchor_segments(meeting_id: str, window_seconds: int, max_segments: int) -> list[dict]:
    transcript_path = MEETINGS_DIR / meeting_id / "transcript.jsonl"
    if not transcript_path.exists():
        return []

    scan_limit = max(max_segments * 5, 200)
    segments = _load_recent_segments(transcript_path, scan_limit)
    if not segments:
        return []

    last_ts = _parse_iso_ts(segments[-1].get("ts"))
    if last_ts:
        cutoff = last_ts - timedelta(seconds=window_seconds)
        windowed = []
        for seg in segments:
            ts = _parse_iso_ts(seg.get("ts"))
            if ts and ts >= cutoff:
                windowed.append(seg)
        if windowed:
            segments = windowed

    if len(segments) > max_segments:
        segments = segments[-max_segments:]

    return segments


def _truncate_text(text: str, max_len: int) -> str:
    clean = " ".join(text.split())
    if len(clean) <= max_len:
        return clean
    return clean[: max_len - 3].rstrip() + "..."


def _segments_to_transcript(segments: list[dict]) -> str:
    lines = []
    for seg in segments:
        if seg.get("event_type"):
            continue
        ts_display = seg.get("ts_display") or ""
        speaker = seg.get("speaker") or ""
        text = seg.get("text") or ""
        if not text:
            continue
        lines.append(f"[{ts_display}] {speaker}: {text}")

    return "\n".join(lines)


def _build_anchor_transcript(segments: list[dict], max_chars: int = 12000) -> str:
    transcript = _segments_to_transcript(segments)
    if len(transcript) > max_chars:
        transcript = transcript[-max_chars:]
        transcript = "...(начало обрезано)...\n" + transcript
    return transcript


def _build_ai_transcript(segments: list[dict], max_chars: int) -> str:
    transcript = _segments_to_transcript(segments)
    if len(transcript) > max_chars:
        transcript = transcript[-max_chars:]
        transcript = "...(truncated)...\n" + transcript
    return transcript


def _detect_user_mention(segments: list[dict], local_names: list[str]) -> tuple[bool, str, str]:
    names = [name.strip().lower() for name in local_names if name and name.strip()]
    if not names:
        return False, "", ""

    for seg in reversed(segments):
        text = seg.get("text", "")
        text_lower = text.lower()
        for name in names:
            if name and name in text_lower:
                return True, _truncate_text(text, 160), seg.get("ts_display", "")

    return False, "", ""


def _fallback_bullets(segments: list[dict]) -> list[dict]:
    bullets = []
    for seg in segments[-3:]:
        text = seg.get("text") or ""
        if not text:
            continue
        bullets.append({
            "text": _truncate_text(text, 120),
            "ts_display": seg.get("ts_display", "")
        })
    return bullets


def _fallback_anchor_payload(segments: list[dict], local_names: list[str]) -> dict:
    current_text = segments[-1].get("text", "") if segments else ""
    current_topic = _truncate_text(current_text, 140) if current_text else "No recent transcript."
    bullets = _fallback_bullets(segments)
    mentioned, mention_context, mention_ts = _detect_user_mention(segments, local_names)

    return {
        "current_topic": current_topic,
        "topic_tone": "neutral",
        "summary_bullets": bullets if bullets else [{"text": "No recent transcript.", "ts_display": ""}],
        "user_mentioned": mentioned,
        "mention_context": mention_context if mentioned else "",
        "mention_ts_display": mention_ts if mentioned else ""
    }


def _extract_json_payload(text: str) -> Optional[dict]:
    if not text:
        return None
    cleaned = text.strip()

    if cleaned.startswith("```"):
        lines = cleaned.splitlines()
        if lines and lines[0].startswith("```"):
            lines = lines[1:]
        if lines and lines[-1].startswith("```"):
            lines = lines[:-1]
        cleaned = "\n".join(lines).strip()

    start = cleaned.find("{")
    end = cleaned.rfind("}")
    if start != -1 and end != -1 and end > start:
        cleaned = cleaned[start:end + 1]

    try:
        return json.loads(cleaned)
    except json.JSONDecodeError:
        return None


def _normalize_anchor_payload(
    payload: dict,
    segments: list[dict],
    local_names: list[str]
) -> tuple[dict, bool]:
    approximate = False
    if not isinstance(payload, dict):
        return _fallback_anchor_payload(segments, local_names), True

    current_topic = str(payload.get("current_topic") or "").strip()
    if not current_topic:
        approximate = True
        current_topic = _fallback_anchor_payload(segments, local_names)["current_topic"]

    topic_tone = str(payload.get("topic_tone") or "").strip().lower()
    if topic_tone not in {"neutral", "calm", "heated"}:
        topic_tone = "neutral"
        approximate = True

    raw_bullets = payload.get("summary_bullets") or []
    normalized_bullets = []
    for item in raw_bullets:
        if isinstance(item, dict):
            text = str(item.get("text") or "").strip()
            ts_display = str(item.get("ts_display") or "").strip()
        else:
            text = str(item).strip()
            ts_display = ""
        if text:
            normalized_bullets.append({"text": text, "ts_display": ts_display})

    if not normalized_bullets:
        approximate = True
        normalized_bullets = _fallback_bullets(segments)

    if len(normalized_bullets) > 3:
        normalized_bullets = normalized_bullets[:3]

    user_mentioned = payload.get("user_mentioned")
    mention_context = str(payload.get("mention_context") or "").strip()
    mention_ts_display = str(payload.get("mention_ts_display") or "").strip()

    if not isinstance(user_mentioned, bool):
        approximate = True
        user_mentioned, mention_context, mention_ts_display = _detect_user_mention(
            segments, local_names
        )
    elif user_mentioned and not mention_context:
        approximate = True
        _, fallback_context, fallback_ts = _detect_user_mention(segments, local_names)
        mention_context = mention_context or fallback_context
        mention_ts_display = mention_ts_display or fallback_ts

    if not user_mentioned:
        mention_context = ""
        mention_ts_display = ""

    return {
        "current_topic": current_topic,
        "topic_tone": topic_tone,
        "summary_bullets": normalized_bullets,
        "user_mentioned": user_mentioned,
        "mention_context": mention_context,
        "mention_ts_display": mention_ts_display
    }, approximate


# --- Debug AI Context ---

debug_ai_context: dict[str, object] = {
    "segments": [],
    "user_name": None,
    "meeting_id": None,
    "meeting_title": None,
    "set_at": None
}


def _local_participant_names(meta: dict) -> list[str]:
    participants = meta.get("participants", {})
    return [p.get("name", "") for p in participants.values() if p.get("is_local")]


def _find_meeting_by_title(title: str) -> Optional[str]:
    if not title:
        return None

    title_lower = title.lower()
    best_id = None
    best_created = None
    fallback_id = None
    fallback_created = None

    for meeting_dir in MEETINGS_DIR.iterdir():
        if not meeting_dir.is_dir():
            continue
        meta_path = meeting_dir / "meta.json"
        if not meta_path.exists():
            continue
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
        except json.JSONDecodeError:
            continue

        meeting_title = (meta.get("title") or "").strip()
        if not meeting_title:
            continue

        created = _parse_iso_ts(meta.get("created_at"))

        if meeting_title.lower() == title_lower:
            if best_created is None or (created and created > best_created):
                best_created = created
                best_id = meta.get("id")
        elif title_lower in meeting_title.lower():
            if fallback_created is None or (created and created > fallback_created):
                fallback_created = created
                fallback_id = meta.get("id")

    return best_id or fallback_id


def _slice_segments_by_id(
    segments: list[dict],
    from_id: Optional[int],
    to_id: Optional[int],
    max_segments: int
) -> list[dict]:
    if not segments:
        return []
    filtered = []
    for seg in segments:
        seg_id = seg.get("id")
        if seg_id is None:
            continue
        if from_id is not None and seg_id < from_id:
            continue
        if to_id is not None and seg_id > to_id:
            continue
        filtered.append(seg)

    if len(filtered) > max_segments:
        filtered = filtered[:max_segments]
    return filtered


def _slice_segments_by_ts_display(
    segments: list[dict],
    start_ts_display: str,
    max_segments: int
) -> list[dict]:
    if not segments or not start_ts_display:
        return []
    start_index = None
    for idx, seg in enumerate(segments):
        if seg.get("ts_display") == start_ts_display:
            start_index = idx
            break
    if start_index is None:
        return []
    return segments[start_index:start_index + max_segments]


def _get_debug_segments() -> list[dict]:
    segments = debug_ai_context.get("segments")
    if isinstance(segments, list) and segments:
        return segments
    return []


def _resolve_ai_context(
    meeting_id: Optional[str],
    storage: MeetingStorage,
    debug: bool
) -> tuple[list[dict], str, Optional[str]]:
    if debug:
        debug_segments = _get_debug_segments()
        if debug_segments:
            user_name = debug_ai_context.get("user_name")
            if not isinstance(user_name, str) or not user_name.strip():
                user_name = "User"
            return debug_segments, user_name, debug_ai_context.get("meeting_id") or meeting_id

    if not meeting_id:
        raise HTTPException(status_code=404, detail="Meeting not found")

    meeting = storage.get_meeting(meeting_id)
    if not meeting:
        raise HTTPException(status_code=404, detail="Meeting not found")

    local_names = _local_participant_names(meeting.get("meta", {}))
    local_participant = ", ".join(local_names) if local_names else "User"
    segments = meeting.get("segments", [])
    if not segments:
        raise HTTPException(status_code=400, detail="Meeting has no transcript")

    return segments, local_participant, meeting_id


# Global watchers
transcript_watcher = TranscriptWatcher()
live_watcher = LiveWatcher()
observer: Optional[Observer] = None
bridge_subscriber: Optional[BridgeSubscriber] = None


async def _forward_bridge_message(raw: str) -> None:
    """Relay a bridge frame to all connected webapp live-stream clients."""
    await live_watcher._broadcast(raw)


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Startup and shutdown events."""
    global observer, bridge_subscriber

    # Start file watcher
    loop = asyncio.get_running_loop()
    transcript_watcher.set_loop(loop)
    live_watcher.set_loop(loop)

    MEETINGS_DIR.mkdir(parents=True, exist_ok=True)
    observer = Observer()
    observer.schedule(transcript_watcher, str(MEETINGS_DIR), recursive=True)
    observer.schedule(live_watcher, str(MEETINGS_DIR), recursive=True)
    observer.start()

    # Subscribe to KTalk bridge to forward `transcript_refine` events
    # to webapp live-stream clients (D10: JSONL tail-reader cannot see
    # post-rewrite refines, so the bridge channel carries them).
    bridge_subscriber = BridgeSubscriber(
        bridge_url=BRIDGE_WS_URL,
        forward_callback=_forward_bridge_message,
    )
    await bridge_subscriber.start()

    print(f"Server started at http://{HOST}:{PORT}")

    yield

    # Shutdown
    if bridge_subscriber:
        await bridge_subscriber.stop()
    if observer:
        observer.stop()
        observer.join()


app = FastAPI(title="Recog Transcripts", lifespan=lifespan)


# --- REST API ---

@app.get("/api/meetings")
async def list_meetings():
    """Get list of all meetings."""
    try:
        storage = get_storage()
        meetings = storage.list_meetings()
        return JSONResponse(content=meetings)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/search")
async def search_transcripts(q: str = ""):
    """Search across all transcripts."""
    if not q or len(q) < 2:
        return JSONResponse(content=[])

    try:
        storage = get_storage()
        meetings = storage.list_meetings()
        results = []
        query_lower = q.lower()

        for meeting_info in meetings:
            meeting = storage.get_meeting(meeting_info["id"])
            if not meeting:
                continue

            segments = meeting.get("segments", [])
            matches = []

            for seg in segments:
                text = seg.get("text", "")
                if query_lower in text.lower():
                    matches.append({
                        "ts_display": seg.get("ts_display"),
                        "speaker": seg.get("speaker"),
                        "text": text
                    })

            if matches:
                results.append({
                    "meeting_id": meeting_info["id"],
                    "title": meeting_info.get("title") or meeting_info["id"],
                    "created_at": meeting_info.get("created_at"),
                    "source": meeting_info.get("source"),
                    "matches": matches[:5]  # Limit matches per meeting
                })

        return JSONResponse(content=results)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}")
async def get_meeting(meeting_id: str):
    """Get meeting metadata."""
    try:
        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")
        return JSONResponse(content=meeting.get("meta", {}))
    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/transcript")
async def get_transcript(meeting_id: str):
    """Get meeting transcript."""
    try:
        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")
        return JSONResponse(content=meeting.get("segments", []))
    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/prompts")
async def list_prompts():
    """List all available prompt definitions."""
    try:
        from recog.prompt_registry import PromptRegistry

        prompts_dir = Path(__file__).parent.parent / "prompts"
        registry = PromptRegistry(prompts_dir)
        prompts = registry.load_all()
        return JSONResponse(content=[
            {"id": p.id, "name": p.name, "filter": p.filter}
            for p in prompts
        ])
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/increments")
async def list_increments(meeting_id: str):
    """List all increments for a meeting, grouped by prompt_id."""
    if '..' in meeting_id or '/' in meeting_id or '\\' in meeting_id:
        raise HTTPException(status_code=400, detail="Invalid meeting ID")
    try:
        from recog.increment import IncrementGenerator

        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        gen = IncrementGenerator(MEETINGS_DIR)
        result = gen.list_all_increments(meeting_id)
        return JSONResponse(content=result)

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/increments/{prompt_id}")
async def list_prompt_increments(meeting_id: str, prompt_id: str):
    """List increment versions for a specific prompt in a meeting."""
    if '..' in meeting_id or '/' in meeting_id or '\\' in meeting_id:
        raise HTTPException(status_code=400, detail="Invalid meeting ID")
    if '..' in prompt_id or '/' in prompt_id or '\\' in prompt_id:
        raise HTTPException(status_code=400, detail="Invalid prompt ID")
    try:
        from recog.increment import IncrementGenerator

        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        gen = IncrementGenerator(MEETINGS_DIR)
        metas = gen.list_increments(meeting_id, prompt_id)
        return JSONResponse(content=metas)

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/increments/{prompt_id}/{version}")
async def get_increment(meeting_id: str, prompt_id: str, version: int):
    """Get a specific increment version (meta + content) for a prompt."""
    if '..' in meeting_id or '/' in meeting_id or '\\' in meeting_id:
        raise HTTPException(status_code=400, detail="Invalid meeting ID")
    if '..' in prompt_id or '/' in prompt_id or '\\' in prompt_id:
        raise HTTPException(status_code=400, detail="Invalid prompt ID")
    try:
        from recog.increment import IncrementGenerator

        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        gen = IncrementGenerator(MEETINGS_DIR)
        result = gen.get_increment(meeting_id, prompt_id, version)
        if result is None:
            raise HTTPException(status_code=404, detail=f"Increment {prompt_id}/v{version} not found")

        return JSONResponse(content=result)

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/api/meetings/{meeting_id}/increments/{prompt_id}/generate")
async def generate_prompt_increment(meeting_id: str, prompt_id: str, force: bool = False):
    """Trigger generation for a specific prompt in a meeting.

    force=true regenerates over the full transcript (manual Regenerate),
    rather than only the delta since the last version.
    """
    if '..' in meeting_id or '/' in meeting_id or '\\' in meeting_id:
        raise HTTPException(status_code=400, detail="Invalid meeting ID")
    if '..' in prompt_id or '/' in prompt_id or '\\' in prompt_id:
        raise HTTPException(status_code=400, detail="Invalid prompt ID")
    try:
        from recog.increment import IncrementGenerator
        from recog.prompt_registry import PromptRegistry

        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        meeting_path = MEETINGS_DIR / meeting_id
        if not meeting_path.exists():
            raise HTTPException(status_code=404, detail="Meeting directory not found")

        prompts_dir = Path(__file__).parent.parent / "prompts"
        registry = PromptRegistry(prompts_dir)
        prompt = registry.get(prompt_id)
        if not prompt:
            raise HTTPException(status_code=404, detail=f"Prompt '{prompt_id}' not found")

        gen = IncrementGenerator(MEETINGS_DIR, prompts_dir=prompts_dir)
        gen.migrate_flat_increments(meeting_path)
        version = await gen.generate(meeting_id, meeting_path, prompt, force=force)

        if version is None:
            # Check if a failed meta was written (generate returns None for both skip and failure)
            latest_metas = gen.list_increments(meeting_id, prompt_id)
            if latest_metas and latest_metas[-1].get("status") == "failed":
                error_msg = latest_metas[-1].get("error", "Generation failed")
                raise HTTPException(status_code=500, detail=error_msg)
            return JSONResponse(content={"status": "skipped", "message": "No new segments to process"})

        result = gen.get_increment(meeting_id, prompt_id, version)
        return JSONResponse(content={"status": "generated", "version": version, "data": result})

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/api/meetings/{meeting_id}/generate-title")
async def generate_meeting_title(meeting_id: str):
    """Generate title for a meeting using Codex."""
    try:
        # Import title generator
        sys.path.insert(0, str(Path(__file__).parent.parent))
        from recog.title_generator import generate_title_async

        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        segments = meeting.get("segments", [])
        if not segments:
            raise HTTPException(status_code=400, detail="Meeting has no transcript")

        # Build transcript text
        transcript = _segments_to_transcript(segments)
        if not transcript:
            raise HTTPException(status_code=400, detail="Meeting has no transcript text")

        # Generate title
        title = await generate_title_async(transcript)
        if not title:
            raise HTTPException(status_code=500, detail="Failed to generate title")

        # Update meta.json
        meta_path = MEETINGS_DIR / meeting_id / "meta.json"
        if meta_path.exists():
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["title"] = title
            meta["title_source"] = "codex"
            meta["updated_at"] = datetime.now().isoformat()
            meta_path.write_text(
                json.dumps(meta, ensure_ascii=False, indent=2),
                encoding="utf-8"
            )

            # Update index.json if meeting is completed
            if meta.get("ended_at"):
                index_path = MEETINGS_DIR / "index.json"
                if index_path.exists():
                    index = json.loads(index_path.read_text(encoding="utf-8"))
                    for m in index.get("meetings", []):
                        if m.get("id") == meeting_id:
                            m["title"] = title
                            break
                    index_path.write_text(
                        json.dumps(index, ensure_ascii=False, indent=2),
                        encoding="utf-8"
                    )

        return JSONResponse(content={
            "meeting_id": meeting_id,
            "title": title,
            "title_source": "codex"
        })

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/live/status")
async def get_live_status():
    """Get current live transcription status."""
    try:
        storage = get_storage()
        active_meetings = storage.list_active_meetings()

        return JSONResponse(content={
            "active": len(active_meetings) > 0,
            "current_meeting": active_meetings[0] if active_meetings else None,
            "active_meetings": active_meetings
        })
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/pcm-diagnostics")
async def get_pcm_diagnostics():
    """Live per-speaker PCM transport diagnostics.

    Reads the KTalk bridge debug file (written every ~1s) and extracts
    the `pcm` subtree: per-pid chunk counts, seq gaps, chunks_per_sec,
    degraded flag, `pcm_ever_seen`. Used by the assistant during real
    meetings to confirm PCM flow without DevTools access.
    """
    import time

    debug_file = Path("/tmp/ktalk_debug.json")
    if not debug_file.exists():
        raise HTTPException(
            status_code=503,
            detail="ktalk_debug.json missing — is com.recog.live running?",
        )
    try:
        with open(debug_file, "r") as f:
            state = json.load(f)
    except Exception as e:
        raise HTTPException(status_code=500, detail=f"read failed: {e}")
    age = time.time() - state.get("timestamp", 0.0)
    return JSONResponse(content={
        "state_age_s": round(age, 2),
        "stale": age > 5.0,
        "meeting_id": state.get("meeting", {}).get("id"),
        "participants": state.get("conference", {}).get("participants", []),
        "pcm": state.get("pcm") or {"transport_enabled": False},
        "pcm_transport": state.get("pcm_transport") or {},
    })


@app.get("/api/debug")
async def get_debug_status():
    """Get debug status from KTalk bridge and live transcription."""
    import time
    from datetime import datetime

    debug_file = Path("/tmp/ktalk_debug.json")
    ktalk_state = None
    ktalk_stale = True

    # Read KTalk bridge state from debug file
    if debug_file.exists():
        try:
            with open(debug_file, 'r') as f:
                ktalk_state = json.load(f)
            # Check if state is stale (older than 5 seconds)
            if ktalk_state and "timestamp" in ktalk_state:
                age = time.time() - ktalk_state["timestamp"]
                ktalk_stale = age > 5.0
                ktalk_state["age_seconds"] = round(age, 1)
        except Exception as e:
            ktalk_state = {"error": str(e)}

    # Get active meetings
    try:
        storage = get_storage()
        active_meetings = storage.list_active_meetings()
        all_meetings = storage.list_meetings()
    except Exception as e:
        active_meetings = []
        all_meetings = []

    return JSONResponse(content={
        "timestamp": time.time(),
        "timestamp_iso": datetime.now().isoformat(),
        "ktalk_bridge": {
            "state": ktalk_state,
            "stale": ktalk_stale,
            "debug_file": str(debug_file)
        },
        "meetings": {
            "active": active_meetings,
            "active_count": len(active_meetings),
            "total_count": len(all_meetings)
        },
        "server": {
            "host": HOST,
            "port": PORT
        }
    })


# --- Debug AI Context ---

class DebugAiContextRequest(BaseModel):
    meeting_id: Optional[str] = None
    meeting_title: Optional[str] = None
    from_id: Optional[int] = None
    to_id: Optional[int] = None
    start_ts_display: Optional[str] = None
    window_seconds: int = 240
    max_segments: int = 35
    segments: Optional[list[dict]] = None
    user_name: Optional[str] = None


class DebugAiContextResponse(BaseModel):
    active: bool
    meeting_id: Optional[str]
    meeting_title: Optional[str]
    segment_count: int
    user_name: Optional[str]
    set_at: Optional[str]
    segments: Optional[list[dict]] = None


@app.get("/api/debug/ai-context")
async def get_debug_ai_context(include_segments: bool = False, limit: int = 50):
    segments = _get_debug_segments()
    payload = {
        "active": bool(segments),
        "meeting_id": debug_ai_context.get("meeting_id"),
        "meeting_title": debug_ai_context.get("meeting_title"),
        "segment_count": len(segments),
        "user_name": debug_ai_context.get("user_name"),
        "set_at": debug_ai_context.get("set_at")
    }

    if include_segments and segments:
        capped = segments[:max(1, min(limit, 200))]
        payload["segments"] = capped

    return JSONResponse(content=payload)


@app.post("/api/debug/ai-context")
async def set_debug_ai_context(request: DebugAiContextRequest):
    storage = get_storage()

    max_segments = max(5, min(request.max_segments, 200))
    window_seconds = max(30, min(request.window_seconds, 900))

    meeting_id = request.meeting_id
    meeting_title = request.meeting_title
    segments = []
    local_names: list[str] = []

    if request.segments:
        segments = request.segments[:max_segments]
    else:
        if not meeting_id and request.meeting_title:
            meeting_id = _find_meeting_by_title(request.meeting_title)

        if not meeting_id:
            raise HTTPException(status_code=400, detail="meeting_id or segments required")

        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        meeting_title = meeting.get("meta", {}).get("title")
        local_names = _local_participant_names(meeting.get("meta", {}))
        meeting_segments = meeting.get("segments", [])

        if request.from_id is not None or request.to_id is not None:
            segments = _slice_segments_by_id(
                meeting_segments,
                request.from_id,
                request.to_id,
                max_segments
            )
        elif request.start_ts_display:
            segments = _slice_segments_by_ts_display(
                meeting_segments,
                request.start_ts_display,
                max_segments
            )
        else:
            segments = _select_anchor_segments(meeting_id, window_seconds, max_segments)

    if not segments:
        raise HTTPException(status_code=400, detail="No segments selected for debug context")

    user_name = request.user_name
    if not user_name:
        if local_names:
            user_name = ", ".join([name for name in local_names if name])
        else:
            user_name = debug_ai_context.get("user_name") or "User"

    debug_ai_context["segments"] = segments
    debug_ai_context["user_name"] = user_name
    debug_ai_context["meeting_id"] = meeting_id
    debug_ai_context["meeting_title"] = meeting_title
    debug_ai_context["set_at"] = datetime.now().isoformat()

    return JSONResponse(content={
        "active": True,
        "meeting_id": meeting_id,
        "meeting_title": meeting_title,
        "segment_count": len(segments),
        "user_name": user_name,
        "set_at": debug_ai_context.get("set_at")
    })


@app.delete("/api/debug/ai-context")
async def clear_debug_ai_context():
    debug_ai_context["segments"] = []
    debug_ai_context["user_name"] = None
    debug_ai_context["meeting_id"] = None
    debug_ai_context["meeting_title"] = None
    debug_ai_context["set_at"] = None
    return JSONResponse(content={"active": False})


# --- AI Analysis ---

class AnchorRequest(BaseModel):
    window_seconds: int = 240
    max_segments: int = 35
    meeting_id: Optional[str] = None
    debug: bool = False


class AnchorBullet(BaseModel):
    text: str
    ts_display: str = ""


class AnchorData(BaseModel):
    current_topic: str
    topic_tone: str
    summary_bullets: list[AnchorBullet]
    user_mentioned: bool
    mention_context: str = ""
    mention_ts_display: str = ""


class AnchorResponse(BaseModel):
    meeting_id: str
    provider: str
    approximate: bool
    data: AnchorData


@app.post("/api/live/anchor")
async def live_anchor(request: AnchorRequest):
    """Return a short WDIM context snapshot for the active meeting."""
    try:
        storage = get_storage()
        meeting_id = request.meeting_id
        segments: list[dict] = []
        local_names: list[str] = []
        local_participant: Optional[str] = None

        if request.debug:
            debug_segments = _get_debug_segments()
            if debug_segments:
                segments = debug_segments
                meeting_id = debug_ai_context.get("meeting_id") or meeting_id
                debug_user_name = debug_ai_context.get("user_name")
                if isinstance(debug_user_name, str) and debug_user_name.strip():
                    local_participant = debug_user_name.strip()
                    local_names = [name.strip() for name in debug_user_name.split(",") if name.strip()]

        if not segments:
            if not meeting_id:
                active = storage.list_active_meetings()
                meeting_id = active[0]["id"] if active else None

            if not meeting_id:
                raise HTTPException(status_code=404, detail="No active meeting")

            meeting = storage.get_meeting(meeting_id)
            if not meeting:
                raise HTTPException(status_code=404, detail="Meeting not found")

            local_names = _local_participant_names(meeting.get("meta", {}))
            local_participant = ", ".join(local_names) if local_names else "User"

            window_seconds = max(30, min(request.window_seconds, 600))
            max_segments = max(10, min(request.max_segments, 120))

            segments = _select_anchor_segments(meeting_id, window_seconds, max_segments)
            if not segments:
                raise HTTPException(status_code=400, detail="Meeting has no transcript")

        if not local_participant:
            local_participant = "User"

        transcript = _build_anchor_transcript(segments)
        prompt = build_prompt(
            question="anchor",
            transcript=transcript,
            user_name=local_participant,
            custom_prompt=None
        )

        provider = get_provider(task="anchor")
        approximate = False

        try:
            answer = await provider.complete(prompt)
            payload = _extract_json_payload(answer)
            if payload is None:
                approximate = True
                payload = _fallback_anchor_payload(segments, local_names)

            normalized, normalized_approx = _normalize_anchor_payload(
                payload, segments, local_names
            )
            approximate = approximate or normalized_approx
        except Exception:
            normalized = _fallback_anchor_payload(segments, local_names)
            approximate = True

        return AnchorResponse(
            meeting_id=meeting_id or "debug",
            provider=provider.name,
            approximate=approximate,
            data=normalized
        )

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


class AnalyzeRequest(BaseModel):
    question: str = "increment"  # "increment" | "summary" | "custom"
    custom_prompt: Optional[str] = None
    debug: bool = False


class AnalyzeResponse(BaseModel):
    answer: str
    provider: str


class MultiAnalyzeResponse(BaseModel):
    responses: dict[str, dict]  # provider -> {answer, error}


@app.post("/api/meetings/{meeting_id}/analyze")
async def analyze_meeting(meeting_id: str, request: AnalyzeRequest):
    """Analyze meeting transcript with AI."""
    try:
        # 1. Resolve context
        storage = get_storage()
        segments, local_participant, _ = _resolve_ai_context(
            meeting_id, storage, request.debug
        )

        # 2. Build transcript text
        transcript = _build_ai_transcript(segments, max_chars=100000)

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            custom_prompt=request.custom_prompt
        )

        # 5. Call AI provider
        provider = get_provider(task=request.question)
        answer = await provider.complete(prompt)

        return AnalyzeResponse(answer=answer, provider=provider.name)

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.post("/api/meetings/{meeting_id}/analyze-multi")
async def analyze_meeting_multi(meeting_id: str, request: AnalyzeRequest):
    """Analyze meeting transcript with multiple AI providers in parallel."""
    try:
        # 1. Resolve context
        storage = get_storage()
        segments, local_participant, _ = _resolve_ai_context(
            meeting_id, storage, request.debug
        )

        # 2. Build transcript text
        transcript = _build_ai_transcript(segments, max_chars=100000)

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            custom_prompt=request.custom_prompt
        )

        # 5. Call all providers in parallel
        providers = get_all_providers(task=request.question)

        async def call_provider(provider):
            try:
                answer = await provider.complete(prompt)
                return provider.key, {"answer": answer, "name": provider.name, "error": None}
            except Exception as e:
                return provider.key, {"answer": None, "name": provider.name, "error": str(e)}

        results = await asyncio.gather(*[call_provider(p) for p in providers])
        responses = dict(results)

        return MultiAnalyzeResponse(responses=responses)

    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/analyze-stream")
async def analyze_meeting_stream(
    meeting_id: str,
    question: str = "increment",
    debug: bool = False
):
    """Stream AI analysis from multiple providers using SSE."""

    async def generate():
        try:
            # 1. Resolve context
            storage = get_storage()
            try:
                segments, local_participant, _ = _resolve_ai_context(
                    meeting_id, storage, debug
                )
            except HTTPException as exc:
                yield f"data: {json.dumps({'type': 'error', 'message': exc.detail})}\n\n"
                return

            # 2. Build transcript text
            transcript = _build_ai_transcript(segments, max_chars=100000)

            # 4. Build prompt
            prompt = build_prompt(
                question=question,
                transcript=transcript,
                user_name=local_participant,
                custom_prompt=None
            )

            # 5. Stream from all providers in parallel
            providers = get_all_providers(task=question)

            # Send provider info
            for provider in providers:
                yield f"data: {json.dumps({'type': 'start', 'provider': provider.key, 'name': provider.name})}\n\n"

            queue: asyncio.Queue[dict] = asyncio.Queue()

            async def produce_events(provider):
                try:
                    async for text, is_done in provider.stream(prompt):
                        event = {
                            'type': 'chunk' if not is_done else 'done',
                            'provider': provider.key,
                            'text': text
                        }
                        await queue.put(event)
                except Exception as e:
                    await queue.put({
                        'type': 'error',
                        'provider': provider.key,
                        'message': str(e)
                    })
                finally:
                    await queue.put({'type': 'provider_complete', 'provider': provider.key})

            tasks = [asyncio.create_task(produce_events(p)) for p in providers]

            pending = len(tasks)
            try:
                while pending:
                    event = await queue.get()
                    if event.get('type') == 'provider_complete':
                        pending -= 1
                        continue
                    yield f"data: {json.dumps(event)}\n\n"
            finally:
                for task in tasks:
                    task.cancel()
                await asyncio.gather(*tasks, return_exceptions=True)

            yield f"data: {json.dumps({'type': 'complete'})}\n\n"

        except Exception as e:
            yield f"data: {json.dumps({'type': 'error', 'message': str(e)})}\n\n"

    return StreamingResponse(
        generate(),
        media_type="text/event-stream",
        headers={
            "Cache-Control": "no-cache",
            "Connection": "keep-alive",
        }
    )


# --- WebSocket ---

@app.websocket("/api/meetings/{meeting_id}/watch")
async def watch_meeting(websocket: WebSocket, meeting_id: str):
    """WebSocket for watching meeting updates."""
    await websocket.accept()
    transcript_watcher.register(meeting_id, websocket)

    try:
        while True:
            # Keep connection alive, handle pings
            await websocket.receive_text()
    except WebSocketDisconnect:
        pass
    finally:
        transcript_watcher.unregister(meeting_id, websocket)


@app.websocket("/api/live/stream")
async def live_stream(websocket: WebSocket):
    """WebSocket for live transcription stream."""
    await websocket.accept()
    live_watcher.register(websocket)

    # Send current meeting info
    try:
        storage = get_storage()
        active_meetings = storage.list_active_meetings()
        current = active_meetings[0] if active_meetings else None

        if current:
            live_watcher.set_current_meeting(current["id"], current.get("source"))
            meeting_data = storage.get_meeting(current["id"])
            init_segments = meeting_data.get("segments", []) if meeting_data else []
            # Populate the watermark with the segment_ids we're about to
            # deliver via `init`. Without this the first on_modified (fired
            # by the next transcript.jsonl write) reads from offset 0 with
            # an empty watermark and re-broadcasts every existing row as a
            # `segments` frame, producing duplicates on the webapp.
            live_watcher.seed_post_init_watermark(
                s.get("id") for s in init_segments
            )
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": current,
                "segments": init_segments,
                "active_meetings": active_meetings,
                # Tier-2 refine only runs for per-speaker PCM (KTalk
                # conferences). orphan/mic/mixed-path meetings never emit a
                # transcript_refine, so advertising refine here would leave
                # every segment stuck in the preliminary (tier-1) state.
                "refine_enabled": current.get("source") == "ktalk",
            }))
        else:
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": None,
                "segments": [],
                "active_meetings": [],
                "refine_enabled": False,
            }))
    except Exception as e:
        await websocket.send_text(json.dumps({
            "type": "error",
            "message": str(e)
        }))

    try:
        while True:
            await websocket.receive_text()
    except WebSocketDisconnect:
        pass
    finally:
        live_watcher.unregister(websocket)


# --- Speaker Database Management ---

SPEAKER_DB_PATH = Path(__file__).parent.parent / "speakers_ecapa.json"


def get_speaker_db():
    """Get SpeakerDatabase instance."""
    from recog.speaker_db import SpeakerDatabase
    return SpeakerDatabase(str(SPEAKER_DB_PATH))


@app.get("/api/speakers")
async def list_speakers():
    """Get list of all speakers in the database."""
    try:
        speaker_db = get_speaker_db()
        return JSONResponse(content=speaker_db.get_all_speakers())
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


class RenameSpeakerRequest(BaseModel):
    display_name: str


@app.put("/api/speakers/{speaker_id}")
async def rename_speaker(speaker_id: str, request: RenameSpeakerRequest):
    """Rename a speaker."""
    try:
        speaker_db = get_speaker_db()
        if not speaker_db.rename_speaker(speaker_id, request.display_name):
            raise HTTPException(status_code=404, detail="Speaker not found")
        speaker_db.save()
        return JSONResponse(content={"success": True})
    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.delete("/api/speakers/{speaker_id}")
async def delete_speaker(speaker_id: str):
    """Delete a speaker entirely."""
    try:
        speaker_db = get_speaker_db()
        if not speaker_db.delete_speaker(speaker_id):
            raise HTTPException(status_code=404, detail="Speaker not found")
        speaker_db.save()
        return JSONResponse(content={"success": True})
    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}/audio/{segment_index}")
async def get_segment_audio(
    meeting_id: str,
    segment_index: int,
    track: Optional[str] = None  # "mic", "system", or None for auto
):
    """
    Get audio slice for a specific transcript segment.

    Auto-selects track based on speaker (local → mic, remote → system)
    unless explicitly specified.
    """
    import subprocess
    import tempfile

    storage = get_storage()
    meeting = storage.get_meeting(meeting_id)
    if not meeting:
        raise HTTPException(status_code=404, detail="Meeting not found")

    meta = meeting.get("meta", {})
    segments = meeting.get("segments", [])
    meeting_path = MEETINGS_DIR / meeting_id

    if segment_index < 0 or segment_index >= len(segments):
        raise HTTPException(status_code=400, detail="Invalid segment index")

    segment = segments[segment_index]

    # Check if segment has audio offset
    audio_offset_ms = segment.get("audio_offset_ms")
    if audio_offset_ms is None:
        raise HTTPException(
            status_code=400,
            detail="Segment has no audio data (recorded before audio feature)"
        )

    # Auto-select track based on speaker if not specified
    if track is None:
        is_local = segment.get("is_local", False)
        track = "mic" if is_local else "system"

    # Get audio file path - check meta first, then look in folder
    audio_path = meta.get(f"audio_{track}_path")
    if not audio_path or not Path(audio_path).exists():
        # Check for audio file directly in meeting folder
        folder_audio = meeting_path / f"audio_{track}.wav"
        if folder_audio.exists():
            audio_path = str(folder_audio)
        else:
            # Fallback to other track
            fallback_track = "system" if track == "mic" else "mic"
            audio_path = meta.get(f"audio_{fallback_track}_path")
            if not audio_path or not Path(audio_path).exists():
                folder_audio = meeting_path / f"audio_{fallback_track}.wav"
                if folder_audio.exists():
                    audio_path = str(folder_audio)
                else:
                    raise HTTPException(
                        status_code=400,
                        detail="No audio file available for this meeting"
                    )

    audio_path = Path(audio_path)
    if not audio_path.exists():
        raise HTTPException(
            status_code=400,
            detail="Audio file not found (may have been cleaned up)"
        )

    # Calculate segment duration (default 3 seconds, but extend for continuations)
    segment_duration_ms = 3000
    default_chunk_duration = 3000  # Typical chunk duration

    # Look ahead for continuations to extend duration
    extended_end = segment_index + 1
    last_valid_offset = audio_offset_ms
    while extended_end < len(segments):
        next_seg = segments[extended_end]
        if next_seg.get("cont") and next_seg.get("speaker_id") == segment.get("speaker_id"):
            # Track last valid offset for fallback
            seg_offset = next_seg.get("audio_offset_ms")
            if seg_offset is not None:
                last_valid_offset = seg_offset
            extended_end += 1
        else:
            break

    # Calculate total duration based on next segment's offset
    if extended_end < len(segments):
        next_offset = segments[extended_end].get("audio_offset_ms")
        if next_offset is not None:
            segment_duration_ms = next_offset - audio_offset_ms
        else:
            # Next segment has no offset (e.g., system event) - use last valid + default
            segment_duration_ms = (last_valid_offset - audio_offset_ms) + default_chunk_duration
    else:
        # Last segment - use last valid offset + default
        segment_duration_ms = (last_valid_offset - audio_offset_ms) + default_chunk_duration

    # Ensure minimum and maximum duration
    segment_duration_ms = max(1000, min(segment_duration_ms, 30000))

    # Extract audio slice using ffmpeg
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
        tmp_path = tmp.name

    try:
        result = subprocess.run([
            "/opt/homebrew/bin/ffmpeg", "-y",
            "-i", str(audio_path),
            "-ss", str(audio_offset_ms / 1000),
            "-t", str(segment_duration_ms / 1000),
            "-ar", "16000",
            "-ac", "1",
            tmp_path
        ], capture_output=True)

        if result.returncode != 0:
            raise HTTPException(
                status_code=500,
                detail=f"Audio extraction failed: {result.stderr.decode()[:200]}"
            )

        # Read the extracted audio
        audio_data = Path(tmp_path).read_bytes()

        return StreamingResponse(
            iter([audio_data]),
            media_type="audio/wav",
            headers={
                "Content-Disposition": f'inline; filename="segment_{segment_index}.wav"',
                "Content-Length": str(len(audio_data))
            }
        )
    finally:
        Path(tmp_path).unlink(missing_ok=True)


@app.delete("/api/speakers/{speaker_id}/embeddings/{index}")
async def delete_embedding(speaker_id: str, index: int):
    """Delete a single embedding from a speaker."""
    try:
        speaker_db = get_speaker_db()
        if not speaker_db.delete_embedding(speaker_id, index):
            raise HTTPException(status_code=404, detail="Speaker or embedding not found")

        # If no embeddings left, delete the speaker entirely
        speaker = speaker_db.get_speaker(speaker_id)
        if speaker and len(speaker.embeddings) == 0:
            speaker_db.delete_speaker(speaker_id)

        speaker_db.save()
        return JSONResponse(content={"success": True})
    except HTTPException:
        raise
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


class EnrollFromSegmentsRequest(BaseModel):
    meeting_id: str
    segment_indices: list[int]  # Indices of segments in transcript
    speaker_id: Optional[str] = None  # Existing speaker to add to, or None for new
    display_name: Optional[str] = None  # Required for new speakers, optional for existing
    track: str = "system"  # "mic" or "system"


@app.post("/api/speakers/enroll-from-segments")
async def enroll_from_segments(request: EnrollFromSegmentsRequest):
    """
    Extract embedding from selected segments and add to speaker database.

    Process:
    1. Get meeting and validate audio exists
    2. Load transcript to get segment time offsets
    3. Extract audio slices for each segment using audio_offset_ms
    4. Call embedding_worker.py to extract embeddings
    5. Average embeddings if multiple segments
    6. Add to speaker database
    """
    import subprocess
    import tempfile
    import numpy as np

    try:
        storage = get_storage()
        meeting = storage.get_meeting(request.meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        meta = meeting.get("meta", {})
        segments = meeting.get("segments", [])

        # Determine audio path based on track
        if request.track == "mic":
            audio_path = meta.get("audio_mic_path")
        else:
            audio_path = meta.get("audio_system_path")

        if not audio_path:
            raise HTTPException(
                status_code=400,
                detail=f"No {request.track} audio file available for this meeting"
            )

        audio_path = Path(audio_path)
        if not audio_path.exists():
            raise HTTPException(
                status_code=400,
                detail=f"Audio file not found: {audio_path}"
            )

        # Validate segment indices
        if not request.segment_indices:
            raise HTTPException(status_code=400, detail="No segments selected")

        # Validate: either existing speaker_id or new display_name required
        if not request.speaker_id and not request.display_name:
            raise HTTPException(
                status_code=400,
                detail="Either speaker_id (for existing) or display_name (for new) is required"
            )

        selected_segments = []
        for idx in request.segment_indices:
            if idx < 0 or idx >= len(segments):
                raise HTTPException(status_code=400, detail=f"Invalid segment index: {idx}")
            seg = segments[idx]
            if "audio_offset_ms" not in seg:
                raise HTTPException(
                    status_code=400,
                    detail=f"Segment {idx} has no audio_offset_ms (recorded before audio feature)"
                )
            selected_segments.append(seg)

        # Extract embeddings for each segment
        embeddings = []
        venv_python = Path(__file__).parent.parent / "venv-gigaam" / "bin" / "python"
        worker_path = Path(__file__).parent.parent / "embedding_worker.py"

        # Assume ~3 second segments (chunk_duration from live.py)
        segment_duration_ms = 3000

        for seg in selected_segments:
            start_ms = seg["audio_offset_ms"]
            end_ms = start_ms + segment_duration_ms

            # Extract audio slice with ffmpeg
            with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
                tmp_path = tmp.name

            try:
                ffmpeg_result = subprocess.run([
                    "ffmpeg", "-y",
                    "-i", str(audio_path),
                    "-ss", str(start_ms / 1000),
                    "-t", str(segment_duration_ms / 1000),
                    "-ar", "16000",
                    "-ac", "1",
                    tmp_path
                ], capture_output=True)

                if ffmpeg_result.returncode != 0:
                    print(f"ffmpeg error: {ffmpeg_result.stderr.decode()}", file=sys.stderr)
                    continue

                # Call embedding worker
                result = subprocess.run(
                    [str(venv_python), str(worker_path)],
                    input=json.dumps({"audio_path": tmp_path}),
                    capture_output=True,
                    text=True
                )

                if result.returncode != 0:
                    print(f"embedding_worker error: {result.stderr}", file=sys.stderr)
                    continue

                data = json.loads(result.stdout)
                if "error" in data:
                    print(f"embedding_worker error: {data['error']}", file=sys.stderr)
                    continue

                embeddings.append(np.array(data["embedding"]))

            finally:
                Path(tmp_path).unlink(missing_ok=True)

        if not embeddings:
            raise HTTPException(
                status_code=500,
                detail="Failed to extract any embeddings from selected segments"
            )

        # Average embeddings
        avg_embedding = np.mean(embeddings, axis=0)

        # Add to speaker database
        speaker_db = get_speaker_db()

        # Generate speaker_id if not provided
        speaker_id = request.speaker_id
        display_name = request.display_name

        if speaker_id:
            # Adding to existing speaker - get display_name from database
            if speaker_id not in speaker_db.speakers:
                raise HTTPException(status_code=404, detail=f"Speaker not found: {speaker_id}")
            display_name = speaker_db.speakers[speaker_id].display_name
        else:
            # Create new speaker - generate ID from display_name
            speaker_id = display_name.lower().replace(" ", "_")
            # Ensure uniqueness
            base_id = speaker_id
            counter = 1
            while speaker_id in speaker_db.speakers:
                speaker_id = f"{base_id}_{counter}"
                counter += 1

        # Build transcript from selected segments
        transcript = " ".join(seg.get("text", "") for seg in selected_segments)[:200]

        speaker_db.add_speaker(
            speaker_id=speaker_id,
            display_name=display_name,
            embedding=avg_embedding,
            source_file=f"meeting_{request.meeting_id}",
            start_time=selected_segments[0]["audio_offset_ms"] / 1000,
            end_time=(selected_segments[-1]["audio_offset_ms"] + segment_duration_ms) / 1000,
            transcript=transcript
        )
        speaker_db.save()

        return JSONResponse(content={
            "success": True,
            "speaker_id": speaker_id,
            "display_name": display_name,
            "embeddings_extracted": len(embeddings),
            "segments_used": len(selected_segments)
        })

    except HTTPException:
        raise
    except Exception as e:
        import traceback
        traceback.print_exc()
        raise HTTPException(status_code=500, detail=str(e))


# --- Static Files ---

@app.get("/")
async def root():
    return FileResponse(Path(__file__).parent / "static" / "index.html")


# Mount static files
app.mount("/static", StaticFiles(directory=Path(__file__).parent / "static"), name="static")


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT)
