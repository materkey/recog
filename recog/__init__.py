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
    load_dotenv,
)

__all__ = [
    # Config
    "DEFAULT_MODEL",
    "DEFAULT_CHUNK_DURATION",
    "DEFAULT_OVERLAP",
    "SPEAKER_SIMILARITY_THRESHOLD",
    "MIC_SPEAKER_NAME",
    "load_dotenv",
    # Models
    "Segment",
    "EmbeddingSample",
    "SpeakerInfo",
    # ASR
    "ASRModule",
    # Speaker DB
    "SpeakerDatabase",
    # Output
    "format_time",
    "generate_markdown",
    # Live
    "SingleDeviceCapture",
    "LiveAudioCapture",
    "LiveTranscriber",
    "live_pipeline",
]

_LAZY_ATTRS: dict[str, tuple[str, str]] = {
    # Models
    "Segment": ("recog.models", "Segment"),
    "EmbeddingSample": ("recog.models", "EmbeddingSample"),
    "SpeakerInfo": ("recog.models", "SpeakerInfo"),
    # ASR
    "ASRModule": ("recog.asr", "ASRModule"),
    # Speaker DB
    "SpeakerDatabase": ("recog.speaker_db", "SpeakerDatabase"),
    # Output
    "format_time": ("recog.output", "format_time"),
    "generate_markdown": ("recog.output", "generate_markdown"),
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
