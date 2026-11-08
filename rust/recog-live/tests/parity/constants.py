"""The Python side of tests/constants.rs: the constants of recog/config.py.

Run as `python constants.py <repo root>` with a JSON request on stdin (ignored).
"""

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
]

json.load(sys.stdin)
print(json.dumps({name: float(getattr(config, name)) for name in NAMES}))
