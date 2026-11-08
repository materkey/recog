"""The Python side of the real-model test in tests/asr_parity.rs: WavoASR of
recog/wavo_backend.py on the wavo_asr module, the GPU and the cached gigaam-v3.

Request: {"clips": [base64 of float32 LE samples, ...]}; answer, per clip:
{"chunk": transcribe_chunk, "text": ..., "words": [[text, start, end], ...]} of
transcribe_with_timestamps.
"""

import base64
import json
import sys

sys.path.insert(0, sys.argv[1])

import numpy as np  # noqa: E402

from recog.wavo_backend import WavoASR  # noqa: E402

request = json.load(sys.stdin)
asr = WavoASR()
answers = []
for clip in request["clips"]:
    audio = np.frombuffer(base64.b64decode(clip), dtype="<f4")
    result = asr.transcribe_with_timestamps(audio)
    answers.append(
        {
            "chunk": asr.transcribe_chunk(audio),
            "text": result.text,
            "words": [[w.text, w.start, w.end] for w in result.words],
        }
    )
print(json.dumps(answers))
