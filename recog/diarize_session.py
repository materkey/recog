"""
On-demand diarization for saved sessions.

Allows re-diarizing audio tracks (mic or system) for existing meetings
to improve speaker identification.
"""

import json
from dataclasses import dataclass
from pathlib import Path
from typing import Dict, List, Literal, Optional, Tuple

from .diarization import run_diarization_pyannote4
from .speaker_db import SpeakerDatabase
from .config import SPEAKER_SIMILARITY_THRESHOLD


@dataclass
class DiarizeResult:
    """Result of diarization operation."""
    speakers_found: int
    segments_updated: int
    speaker_labels: Dict[str, str]  # speaker_id -> display_name


def diarize_track(
    meeting_path: Path,
    track: Literal["mic", "system"],
    speaker_db_path: Optional[Path] = None,
    chunk_duration_ms: int = 3000
) -> DiarizeResult:
    """
    Diarize a single audio track (mic or system) for an existing meeting.

    Args:
        meeting_path: Path to meeting directory (e.g., meetings/2025-01-06_xxxx)
        track: Which track to diarize - "mic" or "system"
        speaker_db_path: Optional path to speaker database for identification

    Returns:
        DiarizeResult with statistics

    Raises:
        ValueError: If audio file not found or invalid track
        RuntimeError: If diarization fails
    """
    meta_path = meeting_path / "meta.json"
    transcript_path = meeting_path / "transcript.jsonl"

    if not meta_path.exists():
        raise ValueError(f"Meeting meta not found: {meta_path}")

    # Load meta
    meta = json.loads(meta_path.read_text(encoding="utf-8"))

    # Read chunk_duration_ms from meeting meta if available
    meta_chunk = meta.get("chunk_duration_ms")
    if meta_chunk is not None:
        chunk_duration_ms = int(meta_chunk)

    # Get audio path for track
    audio_key = f"audio_{track}_path"
    audio_path_str = meta.get(audio_key)

    if not audio_path_str:
        raise ValueError(f"No {track} audio path in meeting metadata")

    audio_path = Path(audio_path_str)
    if not audio_path.exists():
        raise FileNotFoundError(f"Audio file not found: {audio_path}")

    # Run diarization via subprocess (uses venv-pyannote)
    print(f"Running diarization on {track} audio: {audio_path}")
    segments, embeddings = run_diarization_pyannote4(str(audio_path))

    print(f"Diarization found {len(set(s.speaker_id for s in segments))} speakers, {len(segments)} segments")

    # Match embeddings to speaker database (greedy 1:1 assignment)
    speaker_labels = {}
    if speaker_db_path and speaker_db_path.exists():
        speaker_db = SpeakerDatabase(str(speaker_db_path))
        # Build all candidates above threshold
        candidates = []
        for speaker_id, emb in embeddings.items():
            match = speaker_db.match_speaker(emb, SPEAKER_SIMILARITY_THRESHOLD)
            if match:
                db_id, display_name, similarity = match
                candidates.append((speaker_id, db_id, display_name, similarity))
        # Sort by similarity descending for greedy 1:1 assignment
        candidates.sort(key=lambda c: c[3], reverse=True)
        assigned_db_ids = set()
        for speaker_id, db_id, display_name, similarity in candidates:
            if speaker_id in speaker_labels:
                continue
            if db_id in assigned_db_ids:
                continue
            speaker_labels[speaker_id] = display_name
            assigned_db_ids.add(db_id)
            print(f"  {speaker_id} -> {display_name} (similarity: {similarity:.2f})")
        # Assign remaining unmatched speakers to their raw IDs
        for speaker_id in embeddings:
            if speaker_id not in speaker_labels:
                speaker_labels[speaker_id] = speaker_id
    else:
        speaker_labels = {sid: sid for sid in embeddings.keys()}

    # Create backup of transcript if not already exists
    backup_path = meeting_path / "transcript.jsonl.backup"
    if not backup_path.exists() and transcript_path.exists():
        import shutil
        shutil.copy2(transcript_path, backup_path)
        meta["transcript_backup_path"] = str(backup_path)

    # Update transcript segments
    segments_updated = _update_transcript_segments(
        transcript_path,
        segments,
        speaker_labels,
        track,
        chunk_duration_ms
    )

    # Update meta with diarization info
    diarization_key = f"diarization_{track}"
    from datetime import datetime, timezone
    meta[diarization_key] = {
        "performed_at": datetime.now(timezone.utc).isoformat(),
        "speakers_found": len(embeddings)
    }
    meta["updated_at"] = datetime.now(timezone.utc).isoformat()

    meta_path.write_text(
        json.dumps(meta, ensure_ascii=False, indent=2),
        encoding="utf-8"
    )

    return DiarizeResult(
        speakers_found=len(embeddings),
        segments_updated=segments_updated,
        speaker_labels=speaker_labels
    )


def _update_transcript_segments(
    transcript_path: Path,
    diarization_segments: List,
    speaker_labels: Dict[str, str],
    track: Literal["mic", "system"],
    chunk_duration_ms: int = 3000
) -> int:
    """
    Update transcript.jsonl with diarization results.

    Maps ASR segments to speakers by audio_offset_ms overlap.

    Args:
        transcript_path: Path to transcript.jsonl file
        diarization_segments: List of DiarizationSegment from diarization
        speaker_labels: Mapping of speaker_id -> display_name
        track: "mic" or "system" - determines which segments to update

    Returns:
        Number of segments updated
    """
    if not transcript_path.exists():
        return 0

    # Read existing segments
    content = transcript_path.read_text(encoding="utf-8").strip()
    if not content:
        return 0

    lines = content.split("\n")
    segments = []
    for line in lines:
        if line.strip():
            segments.append(json.loads(line))

    # Build diarization timeline for fast lookup
    # Convert to milliseconds for matching with audio_offset_ms
    diar_timeline = []
    for seg in diarization_segments:
        diar_timeline.append({
            "start_ms": int(seg.start * 1000),
            "end_ms": int(seg.end * 1000),
            "speaker_id": seg.speaker_id
        })

    # Sort by start time
    diar_timeline.sort(key=lambda x: x["start_ms"])

    updated_count = 0
    for seg in segments:
        # Skip system events
        if seg.get("speaker_id") == "__system__":
            continue

        # Filter by track: mic = is_local=True, system = is_local=False
        is_local = seg.get("is_local", False)
        if track == "mic" and not is_local:
            continue
        if track == "system" and is_local:
            continue

        # Get audio offset for this segment
        audio_offset = seg.get("audio_offset_ms", 0)

        # Find diarization segment with best overlap
        best_speaker = None
        best_overlap = 0

        seg_duration_ms = chunk_duration_ms
        seg_end = audio_offset + seg_duration_ms

        for diar_seg in diar_timeline:
            # Skip if no overlap possible
            if diar_seg["end_ms"] < audio_offset:
                continue
            if diar_seg["start_ms"] > seg_end:
                break  # No more overlaps possible (sorted list)

            # Calculate overlap
            overlap_start = max(audio_offset, diar_seg["start_ms"])
            overlap_end = min(seg_end, diar_seg["end_ms"])
            overlap = max(0, overlap_end - overlap_start)

            if overlap > best_overlap:
                best_overlap = overlap
                best_speaker = diar_seg["speaker_id"]

        # Update speaker if found
        if best_speaker:
            new_speaker_name = speaker_labels.get(best_speaker, best_speaker)
            if seg.get("speaker") != new_speaker_name or seg.get("speaker_id") != best_speaker:
                seg["speaker"] = new_speaker_name
                seg["speaker_id"] = best_speaker
                updated_count += 1

    # Write back
    with open(transcript_path, "w", encoding="utf-8") as f:
        for seg in segments:
            f.write(json.dumps(seg, ensure_ascii=False) + "\n")

    return updated_count
