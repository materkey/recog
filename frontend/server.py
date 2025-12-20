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
import sys
from pathlib import Path
from typing import Optional
from contextlib import asynccontextmanager

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException, Request
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse, StreamingResponse
from pydantic import BaseModel
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler

from ai_provider import get_provider, get_all_providers, build_prompt

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))
from meeting_storage import MeetingStorage

# Configuration
HOST = "127.0.0.1"
PORT = 9080
MEETINGS_DIR = Path(__file__).parent.parent / "meetings"


def get_storage() -> MeetingStorage:
    """Get MeetingStorage instance."""
    return MeetingStorage(MEETINGS_DIR)


class TranscriptWatcher(FileSystemEventHandler):
    """Watch transcript files for changes and notify WebSocket clients."""

    def __init__(self):
        self.clients: dict[str, set[WebSocket]] = {}  # meeting_id -> set of websockets
        self.file_positions: dict[str, int] = {}  # file_path -> last read position
        self._loop: Optional[asyncio.AbstractEventLoop] = None

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self._loop = loop

    def register(self, meeting_id: str, websocket: WebSocket):
        if meeting_id not in self.clients:
            self.clients[meeting_id] = set()
        self.clients[meeting_id].add(websocket)

    def unregister(self, meeting_id: str, websocket: WebSocket):
        if meeting_id in self.clients:
            self.clients[meeting_id].discard(websocket)
            if not self.clients[meeting_id]:
                del self.clients[meeting_id]

    def on_modified(self, event):
        if event.is_directory:
            return

        path = Path(event.src_path)
        if path.name == "transcript.jsonl":
            meeting_id = path.parent.name
            if meeting_id in self.clients and self._loop:
                asyncio.run_coroutine_threadsafe(
                    self._notify_clients(meeting_id, path),
                    self._loop
                )

    async def _notify_clients(self, meeting_id: str, transcript_path: Path):
        """Read new lines and send to connected clients."""
        if not transcript_path.exists():
            return

        file_key = str(transcript_path)
        last_pos = self.file_positions.get(file_key, 0)

        try:
            with open(transcript_path, 'r', encoding='utf-8') as f:
                f.seek(last_pos)
                new_lines = f.readlines()
                self.file_positions[file_key] = f.tell()

            if new_lines and meeting_id in self.clients:
                segments = []
                for line in new_lines:
                    line = line.strip()
                    if line:
                        try:
                            segments.append(json.loads(line))
                        except json.JSONDecodeError:
                            pass

                if segments:
                    message = json.dumps({"type": "segments", "data": segments})
                    dead_clients = []
                    for ws in self.clients[meeting_id]:
                        try:
                            await ws.send_text(message)
                        except Exception:
                            dead_clients.append(ws)

                    for ws in dead_clients:
                        self.clients[meeting_id].discard(ws)
        except Exception as e:
            print(f"Error reading transcript: {e}")


class LiveWatcher(FileSystemEventHandler):
    """Watch for live transcription updates (current meeting)."""

    def __init__(self):
        self.clients: set[WebSocket] = set()
        self.current_meeting_id: Optional[str] = None
        self.file_position: int = 0
        self._loop: Optional[asyncio.AbstractEventLoop] = None

    def set_loop(self, loop: asyncio.AbstractEventLoop):
        self._loop = loop

    def register(self, websocket: WebSocket):
        self.clients.add(websocket)

    def unregister(self, websocket: WebSocket):
        self.clients.discard(websocket)

    def on_modified(self, event):
        if event.is_directory:
            return

        path = Path(event.src_path)

        # Watch index.json for new meetings
        if path.name == "index.json" and self._loop:
            asyncio.run_coroutine_threadsafe(
                self._check_current_meeting(),
                self._loop
            )

        # Watch transcript of current meeting
        if path.name == "transcript.jsonl" and self._loop:
            meeting_id = path.parent.name
            if meeting_id == self.current_meeting_id:
                asyncio.run_coroutine_threadsafe(
                    self._notify_new_segments(path),
                    self._loop
                )

    async def _check_current_meeting(self):
        """Check for new/changed current meeting."""
        try:
            storage = get_storage()
            active_meetings = storage.list_active_meetings()
            current = active_meetings[0] if active_meetings else None

            if current and current["id"] != self.current_meeting_id:
                self.current_meeting_id = current["id"]
                self.file_position = 0

                # Notify clients about meeting change
                message = json.dumps({
                    "type": "meeting_change",
                    "meeting": current
                })
                await self._broadcast(message)
        except Exception as e:
            print(f"Error checking current meeting: {e}")

    async def _notify_new_segments(self, transcript_path: Path):
        """Read new lines and send to connected clients."""
        if not transcript_path.exists():
            return

        try:
            with open(transcript_path, 'r', encoding='utf-8') as f:
                f.seek(self.file_position)
                new_lines = f.readlines()
                self.file_position = f.tell()

            if new_lines:
                segments = []
                for line in new_lines:
                    line = line.strip()
                    if line:
                        try:
                            segments.append(json.loads(line))
                        except json.JSONDecodeError:
                            pass

                if segments:
                    message = json.dumps({"type": "segments", "data": segments})
                    await self._broadcast(message)
        except Exception as e:
            print(f"Error reading transcript: {e}")

    async def _broadcast(self, message: str):
        dead_clients = []
        for ws in self.clients:
            try:
                await ws.send_text(message)
            except Exception:
                dead_clients.append(ws)

        for ws in dead_clients:
            self.clients.discard(ws)


# Global watchers
transcript_watcher = TranscriptWatcher()
live_watcher = LiveWatcher()
observer: Optional[Observer] = None


@asynccontextmanager
async def lifespan(app: FastAPI):
    """Startup and shutdown events."""
    global observer

    # Start file watcher
    loop = asyncio.get_running_loop()
    transcript_watcher.set_loop(loop)
    live_watcher.set_loop(loop)

    MEETINGS_DIR.mkdir(parents=True, exist_ok=True)
    observer = Observer()
    observer.schedule(transcript_watcher, str(MEETINGS_DIR), recursive=True)
    observer.schedule(live_watcher, str(MEETINGS_DIR), recursive=True)
    observer.start()

    print(f"Server started at http://{HOST}:{PORT}")

    yield

    # Shutdown
    if observer:
        observer.stop()
        observer.join()


app = FastAPI(title="Recog Transcripts", lifespan=lifespan)


# --- REST API ---

@app.get("/api/meetings")
async def list_meetings():
    """Get list of all meetings."""
    try:
        storage = MeetingStorage(MEETINGS_DIR)
        meetings = storage.list_meetings()
        return JSONResponse(content=meetings)
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


@app.get("/api/meetings/{meeting_id}")
async def get_meeting(meeting_id: str):
    """Get meeting metadata."""
    try:
        storage = MeetingStorage(MEETINGS_DIR)
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
        storage = MeetingStorage(MEETINGS_DIR)
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")
        return JSONResponse(content=meeting.get("segments", []))
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


# --- AI Analysis ---

class AnalyzeRequest(BaseModel):
    question: str = "increment"  # "increment" | "summary" | "custom"
    custom_prompt: Optional[str] = None


class AnalyzeResponse(BaseModel):
    answer: str
    provider: str


class MultiAnalyzeResponse(BaseModel):
    responses: dict[str, dict]  # provider -> {answer, error}


@app.post("/api/meetings/{meeting_id}/analyze")
async def analyze_meeting(meeting_id: str, request: AnalyzeRequest):
    """Analyze meeting transcript with AI."""
    try:
        # 1. Load meeting
        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        # 2. Get local participant names (may have multiple: mic name + KTalk name)
        participants = meeting["meta"].get("participants", {})
        local_names = [p["name"] for p in participants.values() if p.get("is_local")]
        local_participant = ", ".join(local_names) if local_names else "User"

        # 3. Build transcript text
        segments = meeting.get("segments", [])
        if not segments:
            raise HTTPException(status_code=400, detail="Meeting has no transcript")

        transcript = "\n".join(
            f"[{s['ts_display']}] {s['speaker']}: {s['text']}"
            for s in segments
        )

        # Limit transcript length (rough estimate: 4 chars per token)
        max_chars = 100000  # ~25k tokens
        if len(transcript) > max_chars:
            transcript = transcript[-max_chars:]
            transcript = "...(начало обрезано)...\n" + transcript

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            custom_prompt=request.custom_prompt
        )

        # 5. Call AI provider
        provider = get_provider()
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
        # 1. Load meeting
        storage = get_storage()
        meeting = storage.get_meeting(meeting_id)
        if not meeting:
            raise HTTPException(status_code=404, detail="Meeting not found")

        # 2. Get local participant names
        participants = meeting["meta"].get("participants", {})
        local_names = [p["name"] for p in participants.values() if p.get("is_local")]
        local_participant = ", ".join(local_names) if local_names else "User"

        # 3. Build transcript text
        segments = meeting.get("segments", [])
        if not segments:
            raise HTTPException(status_code=400, detail="Meeting has no transcript")

        transcript = "\n".join(
            f"[{s['ts_display']}] {s['speaker']}: {s['text']}"
            for s in segments
        )

        # Limit transcript length
        max_chars = 100000
        if len(transcript) > max_chars:
            transcript = transcript[-max_chars:]
            transcript = "...(начало обрезано)...\n" + transcript

        # 4. Build prompt
        prompt = build_prompt(
            question=request.question,
            transcript=transcript,
            user_name=local_participant,
            custom_prompt=request.custom_prompt
        )

        # 5. Call all providers in parallel
        providers = get_all_providers()

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
async def analyze_meeting_stream(meeting_id: str, question: str = "increment"):
    """Stream AI analysis from multiple providers using SSE."""

    async def generate():
        try:
            # 1. Load meeting
            storage = get_storage()
            meeting = storage.get_meeting(meeting_id)
            if not meeting:
                yield f"data: {json.dumps({'type': 'error', 'message': 'Meeting not found'})}\n\n"
                return

            # 2. Get local participant names
            participants = meeting["meta"].get("participants", {})
            local_names = [p["name"] for p in participants.values() if p.get("is_local")]
            local_participant = ", ".join(local_names) if local_names else "User"

            # 3. Build transcript text
            segments = meeting.get("segments", [])
            if not segments:
                yield f"data: {json.dumps({'type': 'error', 'message': 'Meeting has no transcript'})}\n\n"
                return

            transcript = "\n".join(
                f"[{s['ts_display']}] {s['speaker']}: {s['text']}"
                for s in segments
            )

            # Limit transcript length
            max_chars = 100000
            if len(transcript) > max_chars:
                transcript = transcript[-max_chars:]
                transcript = "...(начало обрезано)...\n" + transcript

            # 4. Build prompt
            prompt = build_prompt(
                question=question,
                transcript=transcript,
                user_name=local_participant,
                custom_prompt=None
            )

            # 5. Stream from all providers in parallel
            providers = get_all_providers()

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
            live_watcher.current_meeting_id = current["id"]
            meeting_data = storage.get_meeting(current["id"])
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": current,
                "segments": meeting_data.get("segments", []) if meeting_data else [],
                "active_meetings": active_meetings
            }))
        else:
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": None,
                "segments": [],
                "active_meetings": []
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


# --- Static Files ---

@app.get("/")
async def root():
    return FileResponse(Path(__file__).parent / "static" / "index.html")


# Mount static files
app.mount("/static", StaticFiles(directory=Path(__file__).parent / "static"), name="static")


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host=HOST, port=PORT)
