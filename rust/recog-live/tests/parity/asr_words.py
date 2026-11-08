"""The Python side of tests/asr_parity.rs: words_from_tokens of recog/wavo_backend.py.

Request: {"cases": [[[piece, start_ms], ...], ...]}; answer: one list of
[text, start, end] per case.
"""

import json
import sys

sys.path.insert(0, sys.argv[1])

from recog.wavo_backend import words_from_tokens  # noqa: E402

request = json.load(sys.stdin)
print(
    json.dumps(
        [
            [[w.text, w.start, w.end] for w in words_from_tokens([tuple(t) for t in case])]
            for case in request["cases"]
        ]
    )
)
