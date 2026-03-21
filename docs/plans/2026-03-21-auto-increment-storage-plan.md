# Auto Increment Storage Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Auto-generate and persist versioned "My increment" on each ktalk_leave, with delta-generation and RAG-ready frontmatter.

**Architecture:** New `recog/increment.py` module (IncrementGenerator) triggered asynchronously from ktalk_bridge on leave events. Stores `vN.md` + `vN_meta.json` in meeting's `increments/` directory. Frontend shows stored versions instead of regenerating.

**Tech Stack:** Python 3.12, asyncio, CodexCliProvider (gpt-5.4 xhigh), FastAPI, vanilla JS

**Design doc:** `docs/plans/2026-03-21-auto-increment-storage-design.md`

---

### Task 1: IncrementGenerator — core module with tests

**Files:**
- Create: `recog/increment.py`
- Create: `tests/test_increment.py`

**Step 1: Write the failing tests**

Create `tests/test_increment.py`:

```python
import json
import pytest
from pathlib import Path
from unittest.mock import AsyncMock, patch
from recog.increment import IncrementGenerator


def _create_meeting_dir(tmp_path: Path, meeting_id: str, segments: list[dict], meta_overrides: dict = None) -> Path:
    """Helper: create a meeting directory with transcript and meta."""
    meeting_path = tmp_path / meeting_id
    meeting_path.mkdir()

    # Write transcript.jsonl
    with open(meeting_path / "transcript.jsonl", "w") as f:
        for seg in segments:
            f.write(json.dumps(seg, ensure_ascii=False) + "\n")

    # Write meta.json
    meta = {
        "id": meeting_id,
        "created_at": "2026-03-21T14:00:00+03:00",
        "source": "ktalk",
        "title": "Daily sync",
        "participants": {
            "user1": {"name": "Вася", "is_local": True},
            "user2": {"name": "Петя", "is_local": False},
        },
        "stats": {"segment_count": len(segments), "words_count": 50},
    }
    if meta_overrides:
        meta.update(meta_overrides)
    (meeting_path / "meta.json").write_text(json.dumps(meta, ensure_ascii=False), encoding="utf-8")

    return meeting_path


def _make_segments(count: int, start_id: int = 1) -> list[dict]:
    """Helper: generate N fake transcript segments."""
    return [
        {
            "id": i,
            "ts": f"2026-03-21T14:{i:02d}:00+03:00",
            "ts_display": f"14:{i:02d}:00",
            "speaker": "Петя" if i % 2 == 0 else "Вася",
            "speaker_id": "user2" if i % 2 == 0 else "user1",
            "text": f"Segment text number {i}",
            "is_local": i % 2 == 1,
            "cont": False,
            "uncertain": False,
            "uncertain_reason": "",
            "audio_offset_ms": i * 3000,
        }
        for i in range(start_id, start_id + count)
    ]


@pytest.mark.asyncio
async def test_generate_v1_creates_increment_files(tmp_path):
    """
    given: meeting with 5 segments, no prior increments
    when: generate() is called
    then: v1.md and v1_meta.json created in increments/ dir
    """
    segments = _make_segments(5)
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = _create_meeting_dir(tmp_path, meeting_id, segments)

    gen = IncrementGenerator(tmp_path)

    with patch("recog.increment.CodexCliProvider") as MockProvider:
        mock_instance = MockProvider.return_value
        mock_instance.complete = AsyncMock(return_value="## Задачи\n- Сделать ревью PR")
        mock_instance.name = "Codex (gpt-5.4 xhigh)"

        await gen.generate(meeting_id, meeting_path)

    # Check files created
    inc_dir = meeting_path / "increments"
    assert inc_dir.exists()
    assert (inc_dir / "v1.md").exists()
    assert (inc_dir / "v1_meta.json").exists()

    # Check frontmatter in v1.md
    content = (inc_dir / "v1.md").read_text(encoding="utf-8")
    assert content.startswith("---\n")
    assert "meeting: Daily sync" in content
    assert "version: 1" in content
    assert "## Задачи" in content

    # Check meta
    meta = json.loads((inc_dir / "v1_meta.json").read_text())
    assert meta["version"] == 1
    assert meta["status"] == "completed"
    assert meta["segment_range"] == [1, 5]
    assert meta["segment_count"] == 5


@pytest.mark.asyncio
async def test_generate_v2_uses_delta_segments(tmp_path):
    """
    given: meeting with 10 segments, v1 already exists covering segments [1, 5]
    when: generate() is called
    then: v2.md created from segments 6-10 only
    """
    segments = _make_segments(10)
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = _create_meeting_dir(tmp_path, meeting_id, segments)

    # Create existing v1
    inc_dir = meeting_path / "increments"
    inc_dir.mkdir()
    (inc_dir / "v1.md").write_text("---\nversion: 1\n---\nOld content")
    (inc_dir / "v1_meta.json").write_text(json.dumps({
        "version": 1,
        "segment_range": [1, 5],
        "segment_count": 5,
        "status": "completed",
    }))

    gen = IncrementGenerator(tmp_path)
    captured_prompt = None

    async def capture_complete(prompt):
        nonlocal captured_prompt
        captured_prompt = prompt
        return "## Задачи\n- Новая задача после reconnect"

    with patch("recog.increment.CodexCliProvider") as MockProvider:
        mock_instance = MockProvider.return_value
        mock_instance.complete = capture_complete
        mock_instance.name = "Codex (gpt-5.4 xhigh)"

        await gen.generate(meeting_id, meeting_path)

    # v2 created
    assert (inc_dir / "v2.md").exists()
    meta = json.loads((inc_dir / "v2_meta.json").read_text())
    assert meta["version"] == 2
    assert meta["segment_range"] == [6, 10]
    assert meta["segment_count"] == 5

    # Prompt should contain only segments 6-10
    assert "Segment text number 6" in captured_prompt
    assert "Segment text number 1" not in captured_prompt


@pytest.mark.asyncio
async def test_skip_generation_when_zero_delta(tmp_path):
    """
    given: meeting with 5 segments, v1 already covers [1, 5]
    when: generate() is called
    then: no v2 created (zero delta)
    """
    segments = _make_segments(5)
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = _create_meeting_dir(tmp_path, meeting_id, segments)

    inc_dir = meeting_path / "increments"
    inc_dir.mkdir()
    (inc_dir / "v1.md").write_text("---\nversion: 1\n---\nContent")
    (inc_dir / "v1_meta.json").write_text(json.dumps({
        "version": 1,
        "segment_range": [1, 5],
        "segment_count": 5,
        "status": "completed",
    }))

    gen = IncrementGenerator(tmp_path)

    with patch("recog.increment.CodexCliProvider") as MockProvider:
        await gen.generate(meeting_id, meeting_path)

    # No v2 should be created
    assert not (inc_dir / "v2.md").exists()
    assert not (inc_dir / "v2_meta.json").exists()
    # CodexCliProvider should not have been instantiated
    MockProvider.assert_not_called()


@pytest.mark.asyncio
async def test_generate_handles_codex_failure(tmp_path):
    """
    given: meeting with 5 segments
    when: Codex raises an exception
    then: v1_meta.json written with status=failed, no v1.md
    """
    segments = _make_segments(5)
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = _create_meeting_dir(tmp_path, meeting_id, segments)

    gen = IncrementGenerator(tmp_path)

    with patch("recog.increment.CodexCliProvider") as MockProvider:
        mock_instance = MockProvider.return_value
        mock_instance.complete = AsyncMock(side_effect=Exception("Codex CLI error: timeout"))
        mock_instance.name = "Codex (gpt-5.4 xhigh)"

        await gen.generate(meeting_id, meeting_path)

    inc_dir = meeting_path / "increments"
    assert not (inc_dir / "v1.md").exists()

    meta = json.loads((inc_dir / "v1_meta.json").read_text())
    assert meta["status"] == "failed"
    assert "timeout" in meta["error"]


def test_list_increments(tmp_path):
    """
    given: meeting with 2 completed increments
    when: list_increments() called
    then: returns list of 2 meta dicts sorted by version
    """
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = tmp_path / meeting_id
    meeting_path.mkdir()
    inc_dir = meeting_path / "increments"
    inc_dir.mkdir()

    for v in [1, 2]:
        (inc_dir / f"v{v}_meta.json").write_text(json.dumps({
            "version": v,
            "status": "completed",
            "segment_range": [1, v * 5],
            "segment_count": 5,
        }))
        (inc_dir / f"v{v}.md").write_text(f"Content v{v}")

    gen = IncrementGenerator(tmp_path)
    result = gen.list_increments(meeting_id)

    assert len(result) == 2
    assert result[0]["version"] == 1
    assert result[1]["version"] == 2


def test_get_increment(tmp_path):
    """
    given: meeting with v1 increment
    when: get_increment(meeting_id, 1) called
    then: returns meta + markdown content
    """
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = tmp_path / meeting_id
    meeting_path.mkdir()
    inc_dir = meeting_path / "increments"
    inc_dir.mkdir()

    (inc_dir / "v1_meta.json").write_text(json.dumps({
        "version": 1, "status": "completed",
        "segment_range": [1, 5], "segment_count": 5,
    }))
    (inc_dir / "v1.md").write_text("---\nversion: 1\n---\n## Задачи\n- Task 1")

    gen = IncrementGenerator(tmp_path)
    result = gen.get_increment(meeting_id, 1)

    assert result is not None
    assert result["meta"]["version"] == 1
    assert "## Задачи" in result["content"]


def test_get_increment_returns_none_for_missing(tmp_path):
    """
    given: meeting with no increments
    when: get_increment(meeting_id, 1) called
    then: returns None
    """
    meeting_id = "2026-03-21_140000_testmeet"
    meeting_path = tmp_path / meeting_id
    meeting_path.mkdir()

    gen = IncrementGenerator(tmp_path)
    assert gen.get_increment(meeting_id, 1) is None
```

**Step 2: Run tests to verify they fail**

Run: `./venv-gigaam/bin/python -m pytest tests/test_increment.py -v`
Expected: FAIL — `ModuleNotFoundError: No module named 'recog.increment'`

**Step 3: Implement IncrementGenerator**

Create `recog/increment.py`:

```python
"""
Automatic increment generation on ktalk_leave.

Generates versioned "My increment" summaries using CodexCliProvider.
Each version covers a delta of transcript segments (RAG-friendly).
"""

import json
import sys
import time
from datetime import datetime, UTC
from pathlib import Path
from typing import Callable, Optional


# Import is deferred to avoid circular deps and allow mocking
def _get_codex_provider():
    from frontend.ai_provider import CodexCliProvider
    return CodexCliProvider()


def _get_increment_prompt():
    from frontend.ai_provider import INCREMENT_PROMPT
    return INCREMENT_PROMPT


class IncrementGenerator:
    def __init__(self, meetings_base: Path):
        self._meetings_base = meetings_base
        self._on_increment_ready: Optional[Callable[[str, int, str], None]] = None

    def set_callback(self, callback: Callable[[str, int, str], None]):
        """Set callback(meeting_id, version, status) called when generation finishes."""
        self._on_increment_ready = callback

    async def generate(self, meeting_id: str, meeting_path: Path):
        """
        Generate increment for a meeting. Called on ktalk_leave.

        Determines version from existing increments/.
        For v1: uses full transcript. For vN>1: delta segments only.
        Writes vN.md (with YAML frontmatter) + vN_meta.json.
        """
        # Load transcript
        transcript_path = meeting_path / "transcript.jsonl"
        if not transcript_path.exists():
            print(f"[Increment] No transcript for {meeting_id}, skipping", file=sys.stderr)
            return

        all_segments = []
        for line in transcript_path.read_text(encoding="utf-8").strip().split("\n"):
            if line:
                seg = json.loads(line)
                if not seg.get("event_type"):  # skip system events
                    all_segments.append(seg)

        if not all_segments:
            print(f"[Increment] Empty transcript for {meeting_id}, skipping", file=sys.stderr)
            return

        # Determine version and segment range
        inc_dir = meeting_path / "increments"
        version, start_seg_id = self._next_version(inc_dir)

        # Filter segments for delta
        if start_seg_id is not None:
            segments = [s for s in all_segments if s.get("id", 0) > start_seg_id]
        else:
            segments = all_segments

        if not segments:
            print(f"[Increment] Zero delta segments for {meeting_id} v{version}, skipping", file=sys.stderr)
            return

        seg_range = [segments[0]["id"], segments[-1]["id"]]

        # Create dir
        inc_dir.mkdir(exist_ok=True)

        # Write generating status
        meta = {
            "version": version,
            "generated_at": datetime.now(UTC).isoformat(),
            "provider": "codex",
            "model": "gpt-5.4",
            "reasoning_effort": "xhigh",
            "segment_range": seg_range,
            "segment_count": len(segments),
            "status": "generating",
            "duration_seconds": 0,
            "error": None,
        }
        meta_path = inc_dir / f"v{version}_meta.json"
        meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")

        # Load meeting meta for frontmatter
        meeting_meta = json.loads((meeting_path / "meta.json").read_text(encoding="utf-8"))

        # Build transcript text
        transcript_text = self._build_transcript(segments)

        # Resolve user name
        user_name = self._resolve_user_name(meeting_meta)

        # Build prompt
        prompt_template = _get_increment_prompt()
        prompt = prompt_template.format(user_name=user_name, transcript=transcript_text)

        # Call Codex
        start_time = time.monotonic()
        try:
            provider = _get_codex_provider()
            result = await provider.complete(prompt)
            duration = time.monotonic() - start_time

            # Build frontmatter
            frontmatter = self._build_frontmatter(meeting_meta, meeting_id, version, seg_range)

            # Write vN.md
            md_path = inc_dir / f"v{version}.md"
            md_path.write_text(frontmatter + result, encoding="utf-8")

            # Update meta
            meta["status"] = "completed"
            meta["duration_seconds"] = round(duration, 1)
            meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")

            # Update meeting meta.json
            self._update_meeting_meta(meeting_path, version, "completed")

            print(f"[Increment] Generated v{version} for {meeting_id} ({duration:.1f}s)", file=sys.stderr)

            if self._on_increment_ready:
                self._on_increment_ready(meeting_id, version, "completed")

        except Exception as e:
            duration = time.monotonic() - start_time
            meta["status"] = "failed"
            meta["error"] = str(e)
            meta["duration_seconds"] = round(duration, 1)
            meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")

            self._update_meeting_meta(meeting_path, version, "failed")

            print(f"[Increment] Failed v{version} for {meeting_id}: {e}", file=sys.stderr)

            if self._on_increment_ready:
                self._on_increment_ready(meeting_id, version, "failed")

    def list_increments(self, meeting_id: str) -> list[dict]:
        """Return list of increment metadata for a meeting, sorted by version."""
        inc_dir = self._find_increments_dir(meeting_id)
        if not inc_dir or not inc_dir.exists():
            return []

        result = []
        for meta_file in sorted(inc_dir.glob("v*_meta.json")):
            try:
                meta = json.loads(meta_file.read_text(encoding="utf-8"))
                result.append(meta)
            except (json.JSONDecodeError, OSError):
                continue

        return sorted(result, key=lambda m: m.get("version", 0))

    def get_increment(self, meeting_id: str, version: int) -> Optional[dict]:
        """Return {meta, content} for a specific version, or None."""
        inc_dir = self._find_increments_dir(meeting_id)
        if not inc_dir:
            return None

        meta_path = inc_dir / f"v{version}_meta.json"
        md_path = inc_dir / f"v{version}.md"

        if not meta_path.exists():
            return None

        meta = json.loads(meta_path.read_text(encoding="utf-8"))
        content = md_path.read_text(encoding="utf-8") if md_path.exists() else ""

        return {"meta": meta, "content": content}

    # -- Private helpers --

    def _next_version(self, inc_dir: Path) -> tuple[int, Optional[int]]:
        """Return (next_version, last_segment_id_of_previous_version).

        If no previous versions exist, returns (1, None).
        """
        if not inc_dir.exists():
            return 1, None

        max_version = 0
        last_seg_id = None
        for meta_file in inc_dir.glob("v*_meta.json"):
            try:
                meta = json.loads(meta_file.read_text(encoding="utf-8"))
                v = meta.get("version", 0)
                if v > max_version:
                    max_version = v
                    seg_range = meta.get("segment_range", [])
                    if seg_range and len(seg_range) == 2:
                        last_seg_id = seg_range[1]
            except (json.JSONDecodeError, OSError):
                continue

        if max_version == 0:
            return 1, None
        return max_version + 1, last_seg_id

    def _build_transcript(self, segments: list[dict]) -> str:
        """Build transcript text from segments."""
        lines = []
        for seg in segments:
            ts = seg.get("ts_display", "")
            speaker = seg.get("speaker", "")
            text = seg.get("text", "")
            if text:
                lines.append(f"[{ts}] {speaker}: {text}")
        return "\n".join(lines)

    def _resolve_user_name(self, meeting_meta: dict) -> str:
        """Find local participant name from meeting meta."""
        participants = meeting_meta.get("participants", {})
        local_names = []
        for p in participants.values():
            if p.get("is_local"):
                name = p.get("name", "")
                if name:
                    local_names.append(name)
        return ", ".join(local_names) if local_names else "User"

    def _build_frontmatter(self, meeting_meta: dict, meeting_id: str, version: int, seg_range: list[int]) -> str:
        """Build YAML frontmatter for the increment file."""
        title = meeting_meta.get("title", "Untitled")
        date = meeting_meta.get("created_at", "")[:10]
        participants = []
        for p in meeting_meta.get("participants", {}).values():
            name = p.get("name", "")
            if name:
                participants.append(name)

        lines = [
            "---",
            f"meeting: {title}",
            f"meeting_id: {meeting_id}",
            f'date: "{date}"',
            f"participants: [{', '.join(participants)}]",
            f"version: {version}",
            f"segment_range: [{seg_range[0]}, {seg_range[1]}]",
            "---",
            "",
        ]
        return "\n".join(lines)

    def _update_meeting_meta(self, meeting_path: Path, version: int, status: str):
        """Update meta.json with increments summary."""
        meta_path = meeting_path / "meta.json"
        try:
            meta = json.loads(meta_path.read_text(encoding="utf-8"))
            meta["increments"] = {
                "count": version,
                "latest_version": version,
                "latest_status": status,
            }
            meta_path.write_text(json.dumps(meta, ensure_ascii=False, indent=2), encoding="utf-8")
        except (json.JSONDecodeError, OSError) as e:
            print(f"[Increment] Failed to update meeting meta: {e}", file=sys.stderr)

    def _find_increments_dir(self, meeting_id: str) -> Optional[Path]:
        """Find the increments directory for a meeting."""
        meeting_path = self._meetings_base / meeting_id
        if not meeting_path.exists():
            return None
        inc_dir = meeting_path / "increments"
        return inc_dir if inc_dir.exists() else None
```

**Step 4: Run tests to verify they pass**

Run: `./venv-gigaam/bin/python -m pytest tests/test_increment.py -v`
Expected: all 7 tests PASS

**Step 5: Commit**

```bash
git add recog/increment.py tests/test_increment.py
git commit -m "feat: add IncrementGenerator with delta-generation and frontmatter

New module recog/increment.py generates versioned increment files
on ktalk_leave. Each version uses delta segments only (RAG-friendly).
Files include YAML frontmatter for self-contained retrieval."
```

---

### Task 2: Integrate IncrementGenerator into ktalk_bridge.py

**Files:**
- Modify: `ktalk_bridge.py:299-306` (add increment_generator field)
- Modify: `ktalk_bridge.py:1374-1395` (meeting_leave handler)
- Modify: `ktalk_bridge.py:1161-1175` (client disconnect handler)

**Step 1: Add IncrementGenerator field and setter to KTalkBridge**

In `ktalk_bridge.py`, after the existing callback fields (line ~303), add:

```python
# After: self._on_pre_meeting_switch: Optional[Callable[[], None]] = None
self._increment_generator: Optional['IncrementGenerator'] = None
```

Add setter method after `set_meeting_storage()` (line ~369):

```python
def set_increment_generator(self, generator: 'IncrementGenerator'):
    """Set IncrementGenerator for auto-generating increments on ktalk_leave."""
    self._increment_generator = generator
```

**Step 2: Modify meeting_leave handler (line ~1374)**

Capture prev meeting info BEFORE `on_ktalk_leave()` and trigger generation:

```python
elif msg_type == "meeting_leave":
    reason = data.get("reason", "user_left")
    self._clear_speaker_state()
    if self._meeting_storage:
        # Capture current meeting BEFORE leave switches to new one
        prev_meeting_id = self._meeting_storage.current_meeting_id
        prev_meeting_path = self._meeting_storage.current_meeting_path

        if self._on_pre_meeting_switch:
            self._on_pre_meeting_switch()
        new_meeting_id = self._meeting_storage.on_ktalk_leave(reason)
        print(f"[KTalk Bridge] Left meeting (reason: {reason}), new meeting: {new_meeting_id}", file=sys.stderr)

        # Trigger async increment generation for the PREVIOUS meeting
        if self._increment_generator and prev_meeting_id and prev_meeting_path:
            asyncio.create_task(
                self._increment_generator.generate(prev_meeting_id, prev_meeting_path)
            )

        if self._on_meeting_change:
            meeting_path = self._meeting_storage.current_meeting_path
            self._on_meeting_change(new_meeting_id, meeting_path)
        await self._broadcast_message({
            "type": "new_meeting",
            "meeting_id": new_meeting_id
        })
```

**Step 3: Modify client disconnect handler (line ~1161)**

Same pattern — capture before leave:

```python
if self._meeting_storage and not self._connected_clients and self._meeting_storage.current_meeting_path:
    meta = self._meeting_storage._load_meta()
    if meta and meta.get("source") == "ktalk":
        # Capture current meeting BEFORE leave
        prev_meeting_id = self._meeting_storage.current_meeting_id
        prev_meeting_path = self._meeting_storage.current_meeting_path

        if self._on_pre_meeting_switch:
            self._on_pre_meeting_switch()
        new_meeting_id = self._meeting_storage.on_ktalk_leave("disconnected")

        # Trigger async increment generation
        if self._increment_generator and prev_meeting_id and prev_meeting_path:
            asyncio.create_task(
                self._increment_generator.generate(prev_meeting_id, prev_meeting_path)
            )

        if self._on_meeting_change:
            meeting_path = self._meeting_storage.current_meeting_path
            self._on_meeting_change(new_meeting_id, meeting_path)
        print("[KTalk Bridge] Ended KTalk meeting due to client disconnect", file=sys.stderr)
```

**Step 4: Run existing tests**

Run: `./venv-gigaam/bin/python -m pytest tests/test_ktalk_bridge.py -v`
Expected: all existing tests PASS (no behavior change for mocked storage)

**Step 5: Commit**

```bash
git commit -m "feat: trigger increment generation on ktalk_leave

Capture meeting_id and path before on_ktalk_leave() switches to
new meeting. Trigger async IncrementGenerator.generate() via
asyncio.create_task in both meeting_leave and disconnect handlers." -- ktalk_bridge.py
```

---

### Task 3: Wire IncrementGenerator in live.py startup

**Files:**
- Modify: `recog/live.py` — find where KTalkBridge is created and wired, add increment_generator setup

**Step 1: Find the wiring point**

Search for `set_meeting_storage` call in `recog/live.py` — that's where the bridge gets its storage. Add increment generator setup right after.

Read `recog/live.py` and find the section where `KTalkBridge` is configured with callbacks.

**Step 2: Add IncrementGenerator wiring**

After the existing `bridge.set_meeting_storage(storage)` call, add:

```python
# Auto-generate increments on ktalk_leave
from recog.increment import IncrementGenerator
increment_gen = IncrementGenerator(storage.base_path)
bridge.set_increment_generator(increment_gen)
```

**Step 3: Run smoke test**

Run: `./venv-gigaam/bin/python mp4_notes.py --help`
Expected: no import errors

**Step 4: Commit**

```bash
git commit -m "feat: wire IncrementGenerator into live session startup

IncrementGenerator is created with the same base_path as MeetingStorage
and attached to KTalkBridge during live session initialization." -- recog/live.py
```

---

### Task 4: REST API endpoints for increments

**Files:**
- Modify: `frontend/server.py` — add 2 endpoints

**Step 1: Add list_increments endpoint**

After the existing `analyze_meeting_stream` endpoint in `frontend/server.py`, add:

```python
@app.get("/api/meetings/{meeting_id}/increments")
async def list_increments(meeting_id: str):
    """List all increment versions for a meeting."""
    from recog.increment import IncrementGenerator
    storage = get_storage()
    gen = IncrementGenerator(storage.base_path)
    return gen.list_increments(meeting_id)


@app.get("/api/meetings/{meeting_id}/increments/{version}")
async def get_increment(meeting_id: str, version: int):
    """Get specific increment version content + meta."""
    from recog.increment import IncrementGenerator
    storage = get_storage()
    gen = IncrementGenerator(storage.base_path)
    result = gen.get_increment(meeting_id, version)
    if not result:
        raise HTTPException(status_code=404, detail=f"Increment v{version} not found")
    return result
```

**Step 2: Test endpoints manually**

Run: `curl http://127.0.0.1:9080/api/meetings/SOME_MEETING_ID/increments`
Expected: `[]` (empty list for meeting with no increments)

**Step 3: Commit**

```bash
git commit -m "feat: add REST API for listing and fetching increments

GET /api/meetings/{id}/increments — list all versions
GET /api/meetings/{id}/increments/{version} — get content + meta" -- frontend/server.py
```

---

### Task 5: WebSocket notification for increment_ready

**Files:**
- Modify: `frontend/server.py` — add broadcast on increment ready
- Modify: `recog/live.py` — wire callback

**Step 1: Add broadcast function in server.py**

Near the LiveWatcher class or globally accessible, add ability to broadcast increment_ready. The simplest approach: IncrementGenerator callback sends a message through the live WebSocket watcher.

In `recog/live.py` where IncrementGenerator is wired, set the callback:

```python
def on_increment_ready(meeting_id: str, version: int, status: str):
    """Broadcast increment_ready to frontend via live watcher."""
    # This runs in the async context of ktalk_bridge
    # We need to notify any connected WebSocket clients
    import json as json_module
    message = json_module.dumps({
        "type": "increment_ready",
        "meeting_id": meeting_id,
        "version": version,
        "status": status,
    })
    # Use the bridge's broadcast mechanism if available
    print(f"[Increment] Ready: {meeting_id} v{version} ({status})", file=sys.stderr)

increment_gen.set_callback(on_increment_ready)
```

For the frontend server, the LiveWatcher already broadcasts transcript changes. The simplest approach: have the frontend poll `/api/meetings/{id}/increments` when it needs to check. The `increment_ready` log message is visible in service logs. Full WebSocket push can be added later if needed — the REST API is sufficient for the UI flow.

**Step 2: Commit**

```bash
git commit -m "feat: add increment_ready callback with logging

Log increment generation results to stderr for service monitoring.
Frontend uses REST polling for now; WebSocket push is a future enhancement." -- recog/live.py
```

---

### Task 6: Frontend UI — show stored increments

**Files:**
- Modify: `frontend/static/app.js:1362-1416` (analyzeIncrement function)
- Modify: `frontend/static/index.html:110-130` (ai-modal structure)

**Step 1: Modify analyzeIncrement() to check stored versions first**

Replace the existing `analyzeIncrement()` function in `app.js`:

```javascript
async function analyzeIncrement() {
    const meeting = views.meeting;
    if (!meeting.id) return;

    // Check for stored increments first
    try {
        const response = await fetch(`/api/meetings/${meeting.id}/increments`);
        const increments = await response.json();

        if (increments.length > 0) {
            // Show stored versions
            showStoredIncrements(meeting.id, increments);
            return;
        }
    } catch (e) {
        console.error('Failed to fetch increments:', e);
    }

    // No stored versions — generate via SSE (existing behavior)
    generateIncrementSSE(meeting.id);
}

function showStoredIncrements(meetingId, increments) {
    // Show modal
    document.getElementById('ai-modal').style.display = 'flex';

    // Build version selector
    const latestVersion = increments[increments.length - 1].version;
    const tabs = increments.map(inc => {
        const time = inc.generated_at ? new Date(inc.generated_at).toLocaleTimeString('ru-RU', {hour: '2-digit', minute: '2-digit'}) : '';
        const statusIcon = inc.status === 'completed' ? '' : inc.status === 'generating' ? ' ...' : ' (err)';
        const isLatest = inc.version === latestVersion ? ' (latest)' : '';
        return `<button class="tab-btn${inc.version === latestVersion ? ' active' : ''}"
                    data-provider="v${inc.version}"
                    onclick="switchIncrementVersion('${meetingId}', ${inc.version})">
                    v${inc.version} — ${time}${isLatest}${statusIcon}
                </button>`;
    }).join('');

    // Update modal header
    document.querySelector('.modal-header h3').textContent = 'My Increment';

    // Update tabs
    document.querySelector('.modal-tabs').innerHTML = tabs;

    // Load latest version content
    loadIncrementContent(meetingId, latestVersion);

    // Update footer with regenerate button
    document.querySelector('.modal-footer').innerHTML = `
        <button class="btn" onclick="copyAiResponse()">Copy</button>
        <button class="btn btn-ai" onclick="regenerateIncrement('${meetingId}')">Regenerate</button>
        <button class="btn" onclick="closeAiModal()">Close</button>
    `;
}

async function switchIncrementVersion(meetingId, version) {
    // Update active tab
    document.querySelectorAll('.modal-tabs .tab-btn').forEach(btn => btn.classList.remove('active'));
    document.querySelector(`.modal-tabs .tab-btn[data-provider="v${version}"]`).classList.add('active');

    loadIncrementContent(meetingId, version);
}

async function loadIncrementContent(meetingId, version) {
    const body = document.querySelector('.modal-body');
    body.innerHTML = '<div class="loading">Loading...</div>';

    try {
        const response = await fetch(`/api/meetings/${meetingId}/increments/${version}`);
        const data = await response.json();

        if (data.meta.status === 'failed') {
            body.innerHTML = `<div class="error">Generation failed: ${escapeHtml(data.meta.error || 'Unknown error')}</div>`;
            return;
        }

        if (data.meta.status === 'generating') {
            body.innerHTML = '<div class="loading">Generating...</div>';
            // Poll until ready
            setTimeout(() => loadIncrementContent(meetingId, version), 3000);
            return;
        }

        // Strip YAML frontmatter for display
        let content = data.content;
        if (content.startsWith('---\n')) {
            const endIdx = content.indexOf('\n---\n', 4);
            if (endIdx !== -1) {
                content = content.substring(endIdx + 5);
            }
        }

        body.innerHTML = `<div class="ai-tab-content active" id="ai-response-increment">
            ${renderMarkdown(content)}
        </div>`;

        // Store for copy
        aiResponses['increment'] = content;
    } catch (e) {
        body.innerHTML = `<div class="error">Failed to load: ${escapeHtml(e.message)}</div>`;
    }
}

async function regenerateIncrement(meetingId) {
    // Trigger generation via existing SSE endpoint
    generateIncrementSSE(meetingId);
}

function generateIncrementSSE(meetingId) {
    // Close any existing connection
    if (aiEventSource) {
        aiEventSource.close();
        aiEventSource = null;
    }

    // Reset state
    aiResponses = {};
    aiProviderNames = {};
    activeAiTab = 'claude';

    // Show modal with loading states
    document.getElementById('ai-modal').style.display = 'flex';
    document.querySelector('.modal-header h3').textContent = 'AI Analysis';
    document.querySelector('.modal-tabs').innerHTML = `
        <button class="tab-btn active" data-provider="claude" onclick="switchAiTab('claude')">Claude</button>
        <button class="tab-btn" data-provider="codex" onclick="switchAiTab('codex')">Codex</button>
    `;
    document.querySelector('.modal-body').innerHTML = `
        <div id="ai-response-claude" class="ai-tab-content active">
            <div class="loading">Connecting to Claude...</div>
        </div>
        <div id="ai-response-codex" class="ai-tab-content">
            <div class="loading">Connecting to Codex...</div>
        </div>
    `;
    document.querySelector('.modal-footer').innerHTML = `
        <button class="btn" onclick="copyAiResponse()">Copy</button>
        <button class="btn" onclick="closeAiModal()">Close</button>
    `;

    // Use SSE for streaming
    const url = `/api/meetings/${meetingId}/analyze-stream?question=increment`;
    aiEventSource = new EventSource(url);

    aiEventSource.onmessage = (event) => {
        try {
            const data = JSON.parse(event.data);
            handleAiStreamEvent(data);
        } catch (e) {
            console.error('Failed to parse SSE event:', e);
        }
    };

    aiEventSource.onerror = (e) => {
        console.error('SSE error:', e);
        aiEventSource.close();
        aiEventSource = null;
        if (Object.keys(aiResponses).length === 0) {
            document.getElementById('ai-response-claude').innerHTML =
                '<div class="error">Connection error</div>';
            document.getElementById('ai-response-codex').innerHTML =
                '<div class="error">Connection error</div>';
        }
    };
}
```

**Step 2: Run syntax check**

Run: `./venv-gigaam/bin/python -m py_compile frontend/static/app.js 2>&1 || echo "JS file, skip py_compile"`

Verify manually in browser: open meeting page, click "My increment" button.

**Step 3: Commit**

```bash
git add frontend/static/app.js frontend/static/index.html
git commit -m "feat: show stored increment versions in UI

My increment button now checks for stored versions first.
Shows version tabs with timestamps. Falls back to SSE generation
if no stored versions exist. Adds Regenerate button."
```

---

### Task 7: End-to-end manual test

**No code changes — verification only.**

**Step 1: Start services**

```bash
./recog-service.sh restart
```

**Step 2: Join a KTalk meeting and leave**

1. Open KTalk in Chrome with extension
2. Join a meeting
3. Talk for a few seconds
4. Leave the meeting
5. Check logs: `./recog-service.sh logs` — look for `[Increment] Generated v1`

**Step 3: Verify files**

```bash
ls meetings/*/increments/
cat meetings/<latest>/increments/v1.md
cat meetings/<latest>/increments/v1_meta.json
```

**Step 4: Verify UI**

1. Open http://127.0.0.1:9080
2. Navigate to the meeting
3. Click "My increment"
4. Should show v1 with version tab and content

**Step 5: Test reconnect**

1. Rejoin the same KTalk meeting
2. Talk for a few more seconds
3. Leave again
4. Check: v2.md should appear with delta segments only
5. UI should show both v1 and v2 tabs

---

### Task 8: Run all tests

**Step 1: Run full test suite**

```bash
./venv-gigaam/bin/python -m pytest tests/ -v
./venv-gigaam/bin/python -m compileall recog frontend
```

Expected: all tests PASS, no syntax errors

**Step 2: Final commit if any fixes needed**

Fix any issues and commit individually.
