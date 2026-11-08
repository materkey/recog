"""A stand-in capture child for tests/capture_restart_parity.rs.

Run as `python -I -S child.py <mode> <spawn log>`: it appends its start time to the
log, then behaves as the mode says. Never touches audio devices.

- die: says why on stderr and exits at once;
- pcm_then_die: 0.2 s of float32 PCM on stdout, then exits after 0.8 s;
- silent: stays alive and writes nothing.
"""

import struct
import sys
import time

mode, log = sys.argv[1], sys.argv[2]
with open(log, "a") as f:
    f.write(f"{time.time()!r}\n")

if mode == "die":
    sys.stderr.write("stub: refused\n")
    sys.stderr.flush()
    sys.exit(1)

if mode == "pcm_then_die":
    sys.stderr.write("stub: streaming\n")
    sys.stderr.flush()
    sys.stdout.buffer.write(struct.pack("<3200f", *([0.25] * 3200)))
    sys.stdout.buffer.flush()
    time.sleep(0.8)
    sys.exit(3)

if mode == "silent":
    time.sleep(60)
    sys.exit(0)

sys.exit(f"unknown mode {mode}")
