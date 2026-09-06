"""
Recog - модуль для транскрибирования аудио/видео с диаризацией спикеров.
"""

import importlib
from typing import Any

from .config import (
    DEFAULT_MODEL,
    DEFAULT_CHUNK_DURATION,
    DEFAULT_OVERLAP,
    MIC_SPEAKER_NAME,
    load_dotenv,
)

__all__ = [
    # Config
    "DEFAULT_MODEL",
    "DEFAULT_CHUNK_DURATION",
    "DEFAULT_OVERLAP",
    "MIC_SPEAKER_NAME",
    "load_dotenv",
    # Models
    "Segment",
    # ASR
    "ASRModule",
    "build_asr",
    "RemoteASRClient",
    "FallbackASR",
    # Output
    "format_time",
    "generate_markdown",
    # Live
    "SingleDeviceCapture",
    "LiveTranscriber",
    "live_pipeline",
]

_LAZY_ATTRS: dict[str, tuple[str, str]] = {
    # Models
    "Segment": ("recog.models", "Segment"),
    # ASR
    "ASRModule": ("recog.asr", "ASRModule"),
    "build_asr": ("recog.remote_asr", "build_asr"),
    "RemoteASRClient": ("recog.remote_asr", "RemoteASRClient"),
    "FallbackASR": ("recog.remote_asr", "FallbackASR"),
    # Output
    "format_time": ("recog.output", "format_time"),
    "generate_markdown": ("recog.output", "generate_markdown"),
    # Live
    "SingleDeviceCapture": ("recog.live", "SingleDeviceCapture"),
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
