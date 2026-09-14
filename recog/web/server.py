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
import shutil
import sys
from datetime import datetime, timedelta
from pathlib import Path
from typing import Awaitable, Callable, Literal, Optional
from contextlib import asynccontextmanager

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from pydantic import BaseModel, Field
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler

from recog.web.ai_provider import get_provider, get_all_providers, build_prompt, get_task_settings
from dotenv import load_dotenv
from recog.desktop.service import DesktopIntegration, status as desktop_status

# Load .env from project root
load_dotenv(Path(__file__).resolve().parents[2] / ".env")

from recog.meeting_storage import MeetingStorage, get_external_paths
from recog.log_utils import log_rate_limited
from recog.config import get_data_dir, MIC_SPEAKER_NAME
from recog.user_profile import UserProfileStore
from recog.web.user_profile import ProfileCoordinator
from recog.ai_settings import AiSettingsStore, SettingsConflictError, SettingsReadError
from recog.web.ai_settings import AiSettingsCoordinator
from recog.ws_origin import origin_allowed
from recog.web.bridge_subscriber import BridgeSubscriber
from recog.web.live_watcher import LiveWatcher
from recog.web.transcript_watcher import TranscriptWatcher
from recog.web.meeting_catalog import MeetingCatalog

# Configuration
HOST = "127.0.0.1"
PORT = 9080
DATA_DIR = get_data_dir()
MEETINGS_DIR = DATA_DIR / "meetings"
BRIDGE_WS_URL = os.environ.get("RECOG_BRIDGE_WS_URL", "ws://localhost:8766")
FFMPEG = shutil.which("ffmpeg") or "ffmpeg"


def get_storage() -> MeetingStorage:
    """Get MeetingStorage instance with external paths."""
    return MeetingStorage(MEETINGS_DIR, external_paths=get_external_paths())


# --- Transcript Helpers ---

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


def _select_recent_segments(meeting_id: str, window_seconds: int, max_segments: int) -> list[dict]:
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


def _build_ai_transcript(segments: list[dict], max_chars: int) -> str:
    transcript = _segments_to_transcript(segments)
    if len(transcript) > max_chars:
        transcript = transcript[-max_chars:]
        transcript = "...(truncated)...\n" + transcript
    return transcript


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


def _analysis_prompt_metadata(storage, meeting_id):
    meeting = storage.get_meeting(meeting_id) if meeting_id else None
    meta = (meeting or {}).get("meta", {})
    participants = meta.get("participants", {})
    return {"meeting_title": meta.get("title") or "",
            "participants": ", ".join(info.get("name") or key for key, info in participants.items())}


# Global watchers
transcript_watcher = TranscriptWatcher()
live_watcher = LiveWatcher()
observer: Optional[Observer] = None
bridge_subscriber: Optional[BridgeSubscriber] = None
meeting_catalog: Optional[MeetingCatalog] = None


async def _forward_bridge_message(raw: str) -> None:
    """Relay a bridge frame to all connected webapp live-stream clients."""
    await live_watcher._broadcast(raw)


profile_coordinator = ProfileCoordinator(UserProfileStore(DATA_DIR), MIC_SPEAKER_NAME,
                                         _forward_bridge_message)
ai_settings_coordinator = AiSettingsCoordinator(AiSettingsStore(DATA_DIR), _forward_bridge_message)


async def _bridge_settings_connected(send):
    for coordinator in (profile_coordinator, ai_settings_coordinator):
        try:
            await coordinator.connected(send)
        except (OSError, ValueError) as error:
            print(f"[settings] reconnect reconciliation failed: {error}", file=sys.stderr)


async def _bridge_settings_disconnected():
    for coordinator in (profile_coordinator, ai_settings_coordinator):
        try:
            await coordinator.disconnected()
        except (OSError, ValueError) as error:
            print(f"[settings] disconnect state unavailable: {error}", file=sys.stderr)


# --- Copilot proxy (CAP-5) ---
# The server is a pure relay (D5): no manual-session state, no spawn/stop
# bookkeeping. The only thing kept is the last agent-state chip frame from
# the bridge, replayed to each new live-stream client right after `init` so
# a browser opening #/live mid-session does not show a stale chip.

INBOUND_COPILOT_TYPES = frozenset({
    "copilot_chat_message",
    "copilot_spawn_request",
    "copilot_stop_request",
})

# Last `copilot_status {status: "agent_state"}` frame seen from the bridge.
_agent_state_primer: Optional[str] = None


def _cache_agent_state_frame(raw: str) -> None:
    """Remember the raw frame iff it is the agent-state chip frame."""
    global _agent_state_primer
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        return
    if not isinstance(data, dict):
        return
    if data.get("type") == "copilot_status" and data.get("status") == "agent_state":
        _agent_state_primer = raw


async def _on_bridge_frame(raw: str) -> None:
    """Bridge-frame wrapper: cache the agent-state primer, then delegate."""
    try:
        frame = json.loads(raw)
    except (json.JSONDecodeError, TypeError):
        frame = None
    if isinstance(frame, dict) and frame.get("type") in {"user_profile_applied", "user_profile_error"}:
        await profile_coordinator.handle_frame(frame)
        return
    if isinstance(frame, dict) and frame.get("type") in {"ai_settings_applied", "ai_settings_error"}:
        await ai_settings_coordinator.handle_frame(frame)
        return
    _cache_agent_state_frame(raw)
    await _forward_bridge_message(raw)


async def _send_agent_state_primer(websocket: WebSocket) -> None:
    """Deliver the cached agent-state frame to a freshly connected client.

    A send failure (client already gone) is swallowed with a log — it must
    not crash the endpoint loop.
    """
    if _agent_state_primer is None:
        return
    try:
        await websocket.send_text(_agent_state_primer)
    except Exception as exc:
        print(
            f"[live-stream] agent_state primer send failed: {exc!r}",
            file=sys.stderr,
        )


async def handle_client_frame(
    raw: str,
    subscriber: Optional[BridgeSubscriber],
    reply_cb: Callable[[str], Awaitable[None]],
) -> None:
    """Route one webapp-originated frame from /api/live/stream.

    Whitelisted copilot frames are relayed verbatim (the original raw
    string) via `subscriber.send(raw)`; validation beyond the type
    whitelist is bridge-side. On failure (`send` returned False, or no
    subscriber at all) the originating client gets
    `copilot_status {status: "bridge_disconnected", request_id}` —
    best-effort, no queueing, no retry.
    """
    try:
        data = json.loads(raw)
    except (json.JSONDecodeError, TypeError, RecursionError):
        print("[live-stream] dropped malformed client frame", file=sys.stderr)
        return
    if not isinstance(data, dict):
        print("[live-stream] dropped non-object client frame", file=sys.stderr)
        return
    msg_type = data.get("type")
    # isinstance guard keeps this total: an unhashable `type` (e.g. a list)
    # would raise TypeError on frozenset membership.
    if not isinstance(msg_type, str) or msg_type not in INBOUND_COPILOT_TYPES:
        print(
            f"[live-stream] dropped non-copilot client frame type={msg_type!r}",
            file=sys.stderr,
        )
        return

    sent = False
    if subscriber is not None:
        sent = await subscriber.send(raw)
    if sent:
        return

    reply = json.dumps({
        "type": "copilot_status",
        "status": "bridge_disconnected",
        "request_id": data.get("request_id"),
    })
    try:
        await reply_cb(reply)
    except Exception as exc:
        print(
            f"[live-stream] bridge_disconnected reply failed: {exc!r}",
            file=sys.stderr,
        )


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Startup and shutdown events."""
    global observer, bridge_subscriber, meeting_catalog

    # Start file watcher
    loop = asyncio.get_running_loop()
    transcript_watcher.set_loop(loop)
    live_watcher.set_loop(loop)

    MEETINGS_DIR.mkdir(parents=True, exist_ok=True)
    observer = Observer()
    observer.schedule(transcript_watcher, str(MEETINGS_DIR), recursive=True)
    observer.schedule(live_watcher, str(MEETINGS_DIR), recursive=True)
    meeting_catalog = MeetingCatalog(MEETINGS_DIR, get_external_paths())
    observer.schedule(meeting_catalog, str(MEETINGS_DIR), recursive=True)
    observer_started = False
    desktop = DesktopIntegration(DATA_DIR / "desktop")

    try:
        observer.start()
        observer_started = True
        for root in meeting_catalog.roots:
            if root == meeting_catalog.base_path:
                continue
            try:
                if root.is_dir():
                    observer.schedule(meeting_catalog, str(root), recursive=True)
            except OSError as exc:
                print(f"[Catalog] Cannot watch {root}; using periodic reconciliation: {exc}", file=sys.stderr)
        await meeting_catalog.start()

        # Subscribe to KTalk bridge for downstream live-stream frames and
        # upstream Copilot requests. D10 refine frames use this channel because
        # the JSONL tail-reader cannot see post-rewrite refines.
        bridge_subscriber = BridgeSubscriber(
            bridge_url=BRIDGE_WS_URL,
            forward_callback=_on_bridge_frame,
            connected_callback=_bridge_settings_connected,
            disconnected_callback=_bridge_settings_disconnected,
        )
        await bridge_subscriber.start()

        await desktop.start()

        print(f"Server started at http://{HOST}:{PORT}")

        yield
    finally:
        await desktop.stop()
        if bridge_subscriber:
            await bridge_subscriber.stop()
        observer.stop()
        if observer_started:
            await asyncio.to_thread(observer.join)
        await meeting_catalog.stop()
        meeting_catalog = None


app = FastAPI(title="Recog Transcripts", lifespan=lifespan)


@app.get("/api/desktop/status")
async def get_desktop_status():
    return await asyncio.to_thread(desktop_status, DATA_DIR / "desktop")


class AiModelsRequest(BaseModel):
    models: dict[str, str]
    revision: str

    model_config = {"extra": "forbid"}


class AiTaskRequest(BaseModel):
    enabled: Optional[bool] = Field(default=None, strict=True)
    prompt: str
    providers: list[str]
    models: dict[str, str] = Field(default_factory=dict)
    reasoning_effort: Optional[str] = None
    claude_effort: Optional[str] = None
    revision: str

    model_config = {"extra": "forbid"}


async def _write_ai_settings(operation, *args, revision):
    try:
        return await ai_settings_coordinator.write(operation, *args, expected_revision=revision)
    except SettingsConflictError as error:
        raise HTTPException(status_code=409, detail=str(error)) from error
    except SettingsReadError as error:
        raise HTTPException(status_code=500, detail="Не удалось прочитать настройки AI.") from error
    except ValueError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
    except OSError as error:
        raise HTTPException(status_code=500, detail="Не удалось сохранить настройки AI.") from error


@app.get("/api/ai-settings")
async def get_ai_settings():
    try:
        return await ai_settings_coordinator.view()
    except (OSError, ValueError) as error:
        raise HTTPException(status_code=500, detail="Не удалось прочитать настройки AI.") from error


@app.put("/api/ai-settings/models")
async def save_ai_models(request: AiModelsRequest):
    return await _write_ai_settings(ai_settings_coordinator.store.write_models,
                                    request.models, revision=request.revision)


@app.put("/api/ai-settings/tasks/{task_id}")
async def save_ai_task(task_id: str, request: AiTaskRequest):
    values = request.model_dump(exclude={"revision"}, exclude_none=True)
    return await _write_ai_settings(ai_settings_coordinator.store.write_task,
                                    task_id, values, revision=request.revision)


@app.delete("/api/ai-settings/tasks/{task_id}")
async def reset_ai_task(task_id: str, revision: str):
    return await _write_ai_settings(ai_settings_coordinator.store.reset_task,
                                    task_id, revision=revision)


class UserProfileRequest(BaseModel):
    name: str
    aliases: list[str] = Field(default_factory=list)


@app.get("/api/user-profile")
async def get_user_profile():
    try:
        return await profile_coordinator.snapshot()
    except (OSError, ValueError) as error:
        raise HTTPException(status_code=500, detail="Не удалось прочитать профиль пользователя.") from error


@app.put("/api/user-profile")
async def save_user_profile(request: UserProfileRequest):
    try:
        return await profile_coordinator.save(request.name, request.aliases)
    except ValueError as error:
        raise HTTPException(status_code=422, detail=str(error)) from error
    except OSError as error:
        raise HTTPException(status_code=500, detail="Не удалось сохранить профиль пользователя.") from error


@app.delete("/api/user-profile")
async def clear_user_profile():
    try:
        return await profile_coordinator.clear()
    except (OSError, ValueError) as error:
        raise HTTPException(status_code=500, detail="Не удалось очистить профиль пользователя.") from error


# --- REST API ---

@app.get("/api/meetings")
async def list_meetings():
    """Get list of all meetings."""
    try:
        if meeting_catalog is not None:
            meetings = meeting_catalog.list_meetings()
        else:
            meetings = await asyncio.to_thread(get_storage().list_meetings)
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


@app.delete("/api/meetings/{meeting_id}")
async def delete_meeting(meeting_id: str):
    """Permanently delete a meeting (directory + index entry)."""
    try:
        if '..' in meeting_id or '/' in meeting_id or '\\' in meeting_id:
            raise HTTPException(status_code=400, detail="Invalid meeting ID")
        storage = get_storage()
        try:
            deleted = storage.delete_meeting(meeting_id)
        except ValueError as e:
            raise HTTPException(status_code=409, detail=str(e))
        if not deleted:
            raise HTTPException(status_code=404, detail="Meeting not found")
        # Defensive: if the live watcher still points at the just-deleted id
        # (meeting ended but watcher not yet refreshed), release it.
        if live_watcher.current_meeting_id == meeting_id:
            live_watcher.set_current_meeting(None)
        if meeting_catalog is not None:
            await meeting_catalog.refresh()
        return JSONResponse(content={"success": True})
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
async def generate_prompt_increment(
    meeting_id: str,
    prompt_id: str,
    force: bool = False,
    participant_id: Optional[str] = None,
):
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
        if (
            participant_id is not None
            and participant_id not in meeting.get("meta", {}).get("participants", {})
        ):
            raise HTTPException(status_code=400, detail="Participant not found")

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
        version = await gen.generate(
            meeting_id,
            meeting_path,
            prompt,
            force=force,
            participant_id=participant_id,
        )

        if version is None:
            # Check if a failed meta was written (generate returns None for both skip and failure)
            latest_metas = [
                meta for meta in gen.list_increments(meeting_id, prompt_id)
                if meta.get("participant_id") == participant_id
            ]
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
        settings = get_task_settings("title")
        title = await generate_title_async(transcript, settings=settings, variables={
            **_analysis_prompt_metadata(storage, meeting_id),
            "user_name": ", ".join(_local_participant_names(meeting.get("meta", {}))) or "User",
        })
        title_provider = settings["providers"][0]
        if not title:
            raise HTTPException(status_code=500, detail="Failed to generate title")

        # Update meta.json
        meta_path = MEETINGS_DIR / meeting_id / "meta.json"
        if meta_path.exists():
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["title"] = title
            meta["title_source"] = title_provider
            meta["title_model"] = settings["models"][title_provider]
            meta["title_settings_revision"] = settings["revision"]
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

        if meeting_catalog is not None:
            await meeting_catalog.refresh()
        return JSONResponse(content={
            "meeting_id": meeting_id,
            "title": title,
            "title_source": title_provider
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
        active_meetings = await asyncio.to_thread(storage.list_active_meetings)

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
        active_meetings = await asyncio.to_thread(storage.list_active_meetings)
        all_meetings = (meeting_catalog.list_meetings() if meeting_catalog is not None
                        else await asyncio.to_thread(storage.list_meetings))
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
            segments = _select_recent_segments(meeting_id, window_seconds, max_segments)

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

class AnalyzeRequest(BaseModel):
    question: Literal["increment", "summary"] = "increment"
    model_config = {"extra": "forbid"}
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
        settings = get_task_settings(request.question)

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            settings=settings,
            **_analysis_prompt_metadata(storage, meeting_id)
        )

        # 5. Call AI provider
        provider = get_provider(task=request.question, settings=settings)
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
        settings = get_task_settings(request.question)

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            settings=settings,
            **_analysis_prompt_metadata(storage, meeting_id)
        )

        # 5. Call all providers in parallel
        providers = get_all_providers(task=request.question, settings=settings)

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
    question: Literal["increment", "summary"] = "increment",
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
            settings = get_task_settings(question)

            # 4. Build prompt
            prompt = build_prompt(
                question=question,
                transcript=transcript,
                user_name=local_participant,
                settings=settings,
                **_analysis_prompt_metadata(storage, meeting_id),
            )

            # 5. Stream from all providers in parallel
            providers = get_all_providers(task=question, settings=settings)

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
    # Origin allowlist (recog/ws_origin.py): WebSockets bypass CORS, so a
    # malicious page could otherwise relay copilot frames through this
    # endpoint. Deny the handshake before accept() — live_watcher must
    # never see a rejected socket. No-Origin clients (tests, tools) pass.
    origins = websocket.headers.getlist("origin")
    origin = origins[0] if len(origins) == 1 else None
    rejected_reason = "duplicate Origin headers" if len(origins) > 1 else None
    if rejected_reason is None and not origin_allowed(origin):
        rejected_reason = f"disallowed origin {origin!r}"
    if rejected_reason is not None:
        # Rate-limited: this path is attacker-reachable (a hostile page can
        # loop connection attempts) — do not let it flood stderr.
        log_rate_limited(
            "frontend-ws-origin-reject",
            f"[Frontend] Rejected /api/live/stream connection from "
            f"{rejected_reason}",
        )
        await websocket.close(code=1008)
        return
    await websocket.accept()
    live_watcher.register(websocket)

    # Send current meeting info
    try:
        storage = get_storage()
        active_meetings = await live_watcher.read_active_meetings(storage)
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
            await _send_agent_state_primer(websocket)
        else:
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": None,
                "segments": [],
                "active_meetings": [],
                "refine_enabled": False,
            }))
            await _send_agent_state_primer(websocket)
    except Exception as e:
        await websocket.send_text(json.dumps({
            "type": "error",
            "message": str(e)
        }))

    try:
        while True:
            raw = await websocket.receive_text()
            await handle_client_frame(raw, bridge_subscriber, websocket.send_text)
    except WebSocketDisconnect:
        pass
    finally:
        live_watcher.unregister(websocket)


def compute_segment_duration_ms(segments: list, segment_index: int) -> int:
    """
    Slice duration for a transcript segment inside its track's WAV.

    Mic and system WAVs are written independently (silence is skipped per
    track), so their timelines differ: only segments with the same is_local
    flag can bound each other's duration. Event rows carry no meaningful
    offset and are skipped.
    """
    default_chunk_duration = 3000
    segment = segments[segment_index]
    audio_offset_ms = segment.get("audio_offset_ms") or 0
    track_local = segment.get("is_local", False)

    # Walk forward: extend over same-speaker continuations, then bound the
    # slice by the first same-track segment after the group.
    last_valid_offset = audio_offset_ms
    next_offset = None
    idx = segment_index + 1
    while idx < len(segments):
        next_seg = segments[idx]
        if next_seg.get("event_type") or next_seg.get("is_local", False) != track_local:
            idx += 1
            continue
        if next_seg.get("cont") and next_seg.get("speaker_id") == segment.get("speaker_id"):
            seg_offset = next_seg.get("audio_offset_ms")
            if seg_offset is not None:
                last_valid_offset = seg_offset
            idx += 1
        else:
            next_offset = next_seg.get("audio_offset_ms")
            break

    if next_offset is not None:
        segment_duration_ms = next_offset - audio_offset_ms
    else:
        segment_duration_ms = (last_valid_offset - audio_offset_ms) + default_chunk_duration

    return max(1000, min(segment_duration_ms, 30000))


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
                    detail = (
                        "Audio expired (older than 24h retention)"
                        if meta.get("audio_deleted_at")
                        else "No audio file available for this meeting"
                    )
                    raise HTTPException(status_code=400, detail=detail)

    audio_path = Path(audio_path)
    if not audio_path.exists():
        raise HTTPException(
            status_code=400,
            detail="Audio file not found (may have been cleaned up)"
        )

    # Slice length: bounded by the next same-track segment (continuations
    # extend the group) — see compute_segment_duration_ms.
    segment_duration_ms = compute_segment_duration_ms(segments, segment_index)

    # Extract audio slice using ffmpeg
    with tempfile.NamedTemporaryFile(suffix=".wav", delete=False) as tmp:
        tmp_path = tmp.name

    try:
        result = subprocess.run([
            FFMPEG, "-y",
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


# --- Static Files ---

@app.get("/")
async def root():
    return FileResponse(Path(__file__).parent / "static" / "index.html")


# Mount static files
app.mount("/static", StaticFiles(directory=Path(__file__).parent / "static"), name="static")


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT)
