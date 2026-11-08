"""The Python side of the round6 test in tests/golden_parity.rs: round(x, 6).

Request: {"values": [hex floats]}; answer: [round(float.fromhex(v), 6).hex(), ...].
"""

import json
import sys

request = json.load(sys.stdin)
print(json.dumps([round(float.fromhex(v), 6).hex() for v in request["values"]]))
