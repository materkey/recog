# The body of the stub `claude` and `codex` of rust/recog-ai/tests/parity.rs; the test
# writes it under a shebang naming the parity interpreter with -I. It records what the
# caller passed (argv, environment, stdin, working directory) and answers what the test
# put in `<stub dir>/<name>.answer.json`.
import json
import os
import sys
import time

here = os.path.dirname(os.path.realpath(__file__))
name = os.path.basename(__file__)
stdin = sys.stdin.buffer.read().decode("utf-8", "surrogateescape")
record = {"argv": sys.argv, "env": dict(os.environ), "stdin": stdin, "cwd": os.getcwd()}
calls = os.path.join(here, "calls")
os.makedirs(calls, exist_ok=True)
with open(
    os.path.join(calls, f"{name}-{time.time_ns()}-{os.getpid()}.json"), "w", encoding="utf-8"
) as stream:
    json.dump(record, stream, ensure_ascii=False)
with open(os.path.join(here, f"{name}.answer.json"), encoding="utf-8") as stream:
    answer = json.load(stream)
sys.stdout.buffer.write(answer["stdout"].encode("utf-8"))
sys.stderr.buffer.write(answer["stderr"].encode("utf-8"))
sys.exit(answer["code"])
