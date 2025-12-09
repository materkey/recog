"""
Recog - модуль для транскрибирования аудио/видео с диаризацией спикеров.
"""

from .config import (
    DEFAULT_MODEL,
    DEFAULT_CHUNK_DURATION,
    DEFAULT_OVERLAP,
    SPEAKER_SIMILARITY_THRESHOLD,
    PYANNOTE4_VENV,
    DIARIZE_WORKER,
    load_dotenv,
)
from .models import Segment, DiarizationSegment, EmbeddingSample, SpeakerInfo
from .audio import extract_audio_to_wav, get_audio_path
from .asr import ASRModule
from .diarization import DiarizationModule, run_diarization_pyannote4
from .speaker_db import SpeakerDatabase
from .alignment import align_segments_with_speakers, resolve_speaker_labels
from .output import format_time, generate_markdown
from .pipeline import transcribe_pipeline, enroll_speaker_pipeline, build_speaker_db_pipeline
from .live import SingleDeviceCapture, LiveAudioCapture, LiveTranscriber, live_pipeline

__all__ = [
    # Config
    "DEFAULT_MODEL",
    "DEFAULT_CHUNK_DURATION",
    "DEFAULT_OVERLAP",
    "SPEAKER_SIMILARITY_THRESHOLD",
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
