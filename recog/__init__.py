"""
Recog - модуль для транскрибирования аудио/видео с диаризацией спикеров.
"""

import importlib
from typing import Any

from .config import (
    DEFAULT_MODEL,
    DEFAULT_CHUNK_DURATION,
    DEFAULT_OVERLAP,
    SPEAKER_SIMILARITY_THRESHOLD,
    MIC_SPEAKER_NAME,
    PYANNOTE4_VENV,
    DIARIZE_WORKER,
    load_dotenv,
)

__all__ = [
    # Config
    "DEFAULT_MODEL",
    "DEFAULT_CHUNK_DURATION",
    "DEFAULT_OVERLAP",
    "SPEAKER_SIMILARITY_THRESHOLD",
    "MIC_SPEAKER_NAME",
    "PYANNOTE4_VENV",
    "DIARIZE_WORKER",
    "load_dotenv",
    # Models
    "Segment",
    "DiarizationSegment",
    "EmbeddingSample",
    "SpeakerInfo",
    # Audio
    "extract_audio_to_wav",
    "get_audio_path",
    # ASR
    "ASRModule",
    # Diarization
    "DiarizationModule",
    "run_diarization_pyannote4",
    # Speaker DB
    "SpeakerDatabase",
    # Alignment
    "align_segments_with_speakers",
    "resolve_speaker_labels",
    # Output
    "format_time",
    "generate_markdown",
    # Pipeline
    "transcribe_pipeline",
    "enroll_speaker_pipeline",
    "build_speaker_db_pipeline",
    # Live
    "SingleDeviceCapture",
    "LiveAudioCapture",
    "LiveTranscriber",
    "live_pipeline",
]

_LAZY_ATTRS: dict[str, tuple[str, str]] = {
    # Models
    "Segment": ("recog.models", "Segment"),
    "DiarizationSegment": ("recog.models", "DiarizationSegment"),
    "EmbeddingSample": ("recog.models", "EmbeddingSample"),
    "SpeakerInfo": ("recog.models", "SpeakerInfo"),
    # Audio
    "extract_audio_to_wav": ("recog.audio", "extract_audio_to_wav"),
    "get_audio_path": ("recog.audio", "get_audio_path"),
    # ASR
    "ASRModule": ("recog.asr", "ASRModule"),
    # Diarization
    "DiarizationModule": ("recog.diarization", "DiarizationModule"),
    "run_diarization_pyannote4": ("recog.diarization", "run_diarization_pyannote4"),
    # Speaker DB
    "SpeakerDatabase": ("recog.speaker_db", "SpeakerDatabase"),
    # Alignment
    "align_segments_with_speakers": ("recog.alignment", "align_segments_with_speakers"),
    "resolve_speaker_labels": ("recog.alignment", "resolve_speaker_labels"),
    # Output
    "format_time": ("recog.output", "format_time"),
    "generate_markdown": ("recog.output", "generate_markdown"),
    # Pipeline
    "transcribe_pipeline": ("recog.pipeline", "transcribe_pipeline"),
    "enroll_speaker_pipeline": ("recog.pipeline", "enroll_speaker_pipeline"),
    "build_speaker_db_pipeline": ("recog.pipeline", "build_speaker_db_pipeline"),
    # Live
    "SingleDeviceCapture": ("recog.live", "SingleDeviceCapture"),
    "LiveAudioCapture": ("recog.live", "LiveAudioCapture"),
    "LiveTranscriber": ("recog.live", "LiveTranscriber"),
    "live_pipeline": ("recog.live", "live_pipeline"),
}


def __getattr__(name: str) -> Any:
    if name in _LAZY_ATTRS:
        module_name, attr_name = _LAZY_ATTRS[name]
        module = importlib.import_module(module_name)
        value = getattr(module, attr_name)
        globals()[name] = value
        return value
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


def __dir__() -> list[str]:
    return sorted(set(globals().keys()) | set(_LAZY_ATTRS.keys()))
