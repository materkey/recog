# The stub `claude` and `codex` of rust/recog-web/tests/differential.rs; the test writes
# it under a shebang naming the parity interpreter with -I. It never calls a model: it
# records the call (argv and stdin) and answers text that names the CLI and a digest
# of the prompt, so two servers get the same answer only for the same prompt.
import hashlib
import json
import os
import sys
import time

here = os.path.dirname(os.path.realpath(__file__))
name = os.path.basename(__file__)
stdin = sys.stdin.buffer.read().decode("utf-8", "surrogateescape")
calls = os.path.join(os.environ["RECOG_STUB_CALLS"])
os.makedirs(calls, exist_ok=True)
with open(os.path.join(calls, f"{name}-{time.time_ns()}-{os.getpid()}.json"), "w", encoding="utf-8") as stream:
    json.dump({"argv": sys.argv[1:], "stdin": stdin}, stream, ensure_ascii=False)
digest = hashlib.sha256(stdin.encode("utf-8", "surrogateescape")).hexdigest()[:12]
answer = f"{name} ответ {digest}"
if os.path.exists(os.path.join(here, "fail")):
    sys.stderr.write(f"{name} failed on purpose\n")
    sys.exit(3)
if name == "claude":
    if "stream-json" in sys.argv:
        for part in (answer[:6], answer):
            event = {"type": "assistant", "message": {"content": [{"type": "text", "text": part}]}}
            print(json.dumps(event, ensure_ascii=False), flush=True)
        print(json.dumps({"type": "result", "result": answer}, ensure_ascii=False), flush=True)
    else:
        sys.stdout.write(answer + "\n")
else:
    for event in (
        {"type": "thread.started"},
        {"type": "item.completed", "item": {"type": "agent_message", "text": answer}},
        {"type": "turn.completed"},
    ):
        print(json.dumps(event, ensure_ascii=False), flush=True)
