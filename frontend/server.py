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

from fastapi import FastAPI, WebSocket, WebSocketDisconnect, HTTPException
from fastapi.staticfiles import StaticFiles
from fastapi.responses import FileResponse, JSONResponse
from watchdog.observers import Observer
from watchdog.events import FileSystemEventHandler

# Add parent directory to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent))
from meeting_storage import MeetingStorage

# Configuration
HOST = "127.0.0.1"
PORT = 8080
MEETINGS_DIR = Path(__file__).parent.parent / "meetings"


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
            storage = MeetingStorage(MEETINGS_DIR)
            meetings = storage.list_meetings()

            if meetings:
                # Find most recent active meeting (no ended_at or recent)
                latest = meetings[0]  # Already sorted by date desc

                if latest["id"] != self.current_meeting_id:
                    self.current_meeting_id = latest["id"]
                    self.file_position = 0

                    # Notify clients about meeting change
                    message = json.dumps({
                        "type": "meeting_change",
                        "meeting": latest
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
    loop = asyncio.get_event_loop()
    transcript_watcher.set_loop(loop)
    live_watcher.set_loop(loop)

    observer = Observer()
    if MEETINGS_DIR.exists():
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
        storage = MeetingStorage(MEETINGS_DIR)
        meetings = storage.list_meetings()

        # Check if there's an active meeting (no ended_at)
        current = None
        for m in meetings:
            if not m.get("ended_at"):
                current = m
                break

        return JSONResponse(content={
            "active": current is not None,
            "current_meeting": current
        })
    except Exception as e:
        raise HTTPException(status_code=500, detail=str(e))


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
        storage = MeetingStorage(MEETINGS_DIR)
        meetings = storage.list_meetings()

        current = None
        for m in meetings:
            if not m.get("ended_at"):
                current = m
                break

        if current:
            live_watcher.current_meeting_id = current["id"]
            meeting_data = storage.get_meeting(current["id"])
            await websocket.send_text(json.dumps({
                "type": "init",
                "meeting": current,
                "segments": meeting_data.get("segments", []) if meeting_data else []
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
