"""The Python side of tests/constants.rs: the constants of recog/config.py, and those
of the main loop in recog/live.py and recog/system_audio.py.

Run as `python constants.py <repo root>` with a JSON request on stdin (ignored).
"""

import importlib
import json
import sys

sys.path.insert(0, sys.argv[1])

from recog import config  # noqa: E402

NAMES = [
    "VAD_RMS_THRESHOLD",
    "SPEAKER_TURN_GAP",
    "PCM_CHUNK_DURATION_S",
    "PCM_CHUNK_PAYLOAD_S",
    "PCM_BATCH_INTERVAL_MS",
    "PCM_MAX_BATCH_SIZE",
    "PCM_CHUNK_OVERLAP_MS",
    "PCM_SILENCE_WATCHDOG_S",
    "PCM_REFINE_WINDOW_S",
    "PCM_REFINE_MONOLOG_CAP_S",
    "CAPTURE_RESYNC_MAX_S",
    "LAST_SPEAKER_TTL",
    "SILENCE_SPLIT_TIMEOUT",
    "RETROACTIVE_MAX_SEGMENTS",
    "RETROACTIVE_MAX_SECONDS",
    "MAX_SEGMENT_DURATION",
    "PCM_WAV_ANCHOR_MAX_AGE_MS",
]

MODULE_NAMES = {
    "live": ["MAX_WAV_DATA_BYTES", "AUDIO_CLEANUP_INTERVAL_S", "SILENCE_CHECK_INTERVAL_S", "GOLDEN_KEEP_DAYS"],
    "system_audio": ["DIGITAL_SILENCE_RESTART_S"],
}

json.load(sys.stdin)
answer = {name: float(getattr(config, name)) for name in NAMES}
for module_name, names in MODULE_NAMES.items():
    module = importlib.import_module(f"recog.{module_name}")
    answer.update({f"{module_name}.{name}": float(getattr(module, name)) for name in names})
print(json.dumps(answer))
