"""The Python side of the pure-function cases in rust/recog-copilot/tests/channel_differential.rs.

Reads a JSON list of cases on stdin (written with NaN and Infinity where a case holds
them, which json.loads accepts), runs each through recog/copilot_channel.py — the
channel push formatting, the literal context block, FastMCP's argument validation of
the four tools, pydantic's JSON of a tool result, and the privacy masking a delivery
goes through — and writes the JSON list of outputs on stdout.

Everything personal is pointed at a temporary directory before recog is imported, so
the user's settings and data never reach the comparison.
"""

import json
import os
import sys
import tempfile
from pathlib import Path

REPO = Path(sys.argv[1]).resolve()
SCRATCH = Path(tempfile.mkdtemp(prefix="recog-channel-oracle-"))
os.environ["RECOG_CONFIG_FILE"] = "/nonexistent/recog/config.toml"
os.environ["RECOG_CONFERENCE_HOSTS_FILE"] = "/nonexistent/recog/conference-hosts.json"
os.environ["RECOG_DATA_DIR"] = str(SCRATCH / "data")
sys.path.insert(0, str(REPO))

import pydantic_core  # noqa: E402
from pydantic import ValidationError  # noqa: E402

import recog.copilot_channel as cc  # noqa: E402
from recog.privacy import CopilotPrivacy, PrivacyStore  # noqa: E402

MCP = cc._build_mcp(cc.BridgeLink(), "127.0.0.1", 0)


def run_format(case):
    try:
        content, meta = cc._format_channel_event(case["event"])
    except Exception as exc:  # the push logs it and moves on
        return {"error": f"{type(exc).__name__}: {exc}"}
    return {"content": content, "meta": meta}


def run_context(case):
    return cc.build_copilot_context(case["snapshot"])


def run_validate(case):
    tool = MCP._tool_manager.get_tool(case["tool"])
    metadata = tool.fn_metadata
    try:
        model = metadata.arg_model.model_validate(metadata.pre_parse_json(case["arguments"]))
    except ValidationError as exc:
        return {"error": str(exc)}
    return {"value": model.model_dump_one_level()}


def run_to_json(case):
    return pydantic_core.to_json(case["value"], fallback=str, indent=2).decode()


_privacy_count = 0


def run_privacy(case):
    """A fresh store with privacy on, the payloads delivered in order."""
    global _privacy_count
    _privacy_count += 1
    store = PrivacyStore(SCRATCH / f"privacy-{_privacy_count}")
    store.write(True)
    privacy = CopilotPrivacy(store)
    out = []
    for payload in case["payloads"]:
        masked = privacy.apply(payload)
        out.append({"masked": masked, "push": run_format({"event": masked})})
    return out


HANDLERS = {
    "format": run_format,
    "context": run_context,
    "validate": run_validate,
    "to_json": run_to_json,
    "privacy": run_privacy,
}


def main():
    cases = json.load(sys.stdin)
    json.dump([HANDLERS[case["kind"]](case) for case in cases], sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
