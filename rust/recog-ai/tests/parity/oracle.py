"""The Python side of rust/recog-ai/tests/parity.rs: runs recog's own modules on one
request (JSON on stdin) and prints the outcome as JSON.

Started with -I by the Rust tests, with a temporary HOME, the stub CLIs first on PATH
and the repository root as argv[1]. `recog.install_config` is replaced before any recog
import: the real one loads the checkout's .env and the user's config.toml at import
time, and the parity runs must read neither.
"""

import asyncio
import json
import sys
import types
import unicodedata
from pathlib import Path

sys.path.insert(0, sys.argv[1])

_install = types.ModuleType("recog.install_config")
_install.load_dotenv = lambda env_path=None: None
_install.INSTALL_CONFIG = types.SimpleNamespace(
    web=types.SimpleNamespace(external_meetings_paths=[])
)
_install.canonical_source = lambda source: source
sys.modules["recog.install_config"] = _install

from recog import (  # noqa: E402
    ai_provider,
    ai_settings,
    increment,
    meeting_chat,
    prompt_registry,
    title_generator,
)
from recog.ai_settings import AiSettingsStore, render_prompt  # noqa: E402


def outcome(call):
    try:
        return {"ok": call()}
    except Exception as exc:  # the class and message are what parity compares
        return {"error": type(exc).__name__, "message": str(exc)}


def store_for(request):
    prompts = request.get("prompts_dir")
    return AiSettingsStore(Path(request["data_dir"]), Path(prompts) if prompts else None)


def settings_op(store, op):
    kind = op["op"]
    revision = op.get("revision")
    if revision == "$current":
        revision = store.read()["revision"]
    if kind == "read":
        return store.read()
    if kind == "view":
        return store.view()
    if kind == "resolve":
        return store.resolve_task(
            op["task_id"],
            fallback_prompt=op.get("fallback_prompt"),
            fallback_label=op.get("fallback_label"),
        )
    if kind == "write_models":
        return store.write_models(op["models"], revision)
    if kind == "write_task":
        return store.write_task(op["task_id"], op["values"], revision)
    if kind == "reset_task":
        return store.reset_task(op["task_id"], revision)
    raise ValueError(f"unknown op {kind}")


async def run_provider(request):
    provider = ai_provider.create_provider(
        request["provider"],
        task=request["task"],
        model=request.get("model"),
        reasoning_effort=request.get("effort"),
    )
    described = {
        "key": provider.key,
        "name": provider.name,
        "model": provider.model,
        "reasoning_effort": provider.reasoning_effort,
    }
    if request["mode"] == "complete":
        return {**described, "answer": await provider.complete(request["prompt"])}
    chunks = []
    try:
        async for text, done in provider.stream(request["prompt"]):
            chunks.append([text, done])
    except Exception as exc:
        return {**described, "chunks": chunks, "error": str(exc)}
    return {**described, "chunks": chunks}


def prompt_config(raw):
    return prompt_registry.PromptConfig(raw["id"], raw["name"], raw.get("filter"), raw["template"])


async def run_increment(request):
    generator = increment.IncrementGenerator(
        Path(request["meetings_base"]), Path(request["prompts_dir"])
    )
    meeting_path = Path(request["meeting_path"])
    if request.get("all"):
        await generator.generate_all(request["meeting_id"], meeting_path)
        return None
    return await generator.generate(
        request["meeting_id"],
        meeting_path,
        prompt_config(request["prompt"]),
        force=request.get("force", False),
        participant_id=request.get("participant_id"),
    )


def run_meeting_title(request):
    from recog.meeting_storage import MeetingStorage

    owner = types.SimpleNamespace(base_path=Path(request["data_dir"]) / "meetings")
    owner._get_transcript_as_text = lambda path: MeetingStorage._get_transcript_as_text(owner, path)
    return MeetingStorage._generate_title_for_meeting(owner, Path(request["meeting_path"]))


def ranges(predicate):
    found, start = [], None
    for code in range(sys.maxunicode + 2):
        inside = code <= sys.maxunicode and not 0xD800 <= code <= 0xDFFF and predicate(chr(code))
        if inside and start is None:
            start = code
        elif not inside and start is not None:
            found.append([start, code - 1])
            start = None
    return found


def chat_op(path, op):
    kind = op["op"]
    if kind == "append":
        meeting_chat.append_turn(path, meeting_chat.ChatTurn(**op["turn"]))
        return None
    if kind == "read":
        return [vars(turn) for turn in meeting_chat.read_turns(path)]
    if kind == "clear":
        meeting_chat.clear_turns(path)
        return None
    if kind == "build":
        turns = meeting_chat.read_turns(path)
        return meeting_chat.build_chat_prompt(op["instructions"], turns, op["question"])
    raise ValueError(f"unknown op {kind}")


def main():
    request = json.load(sys.stdin)
    # The modules print progress to stdout; only the result goes there.
    result_stream, sys.stdout = sys.stdout, sys.stderr
    kind = request["kind"]
    if kind == "render":
        result = [
            outcome(lambda case=case: render_prompt(case["template"], case["variables"]))
            for case in request["cases"]
        ]
    elif kind == "defaults":
        from recog import ai_defaults

        result = {"tasks": ai_defaults.DEFAULT_TASKS, "models": ai_defaults.DEFAULT_MODELS}
    elif kind == "frontmatter":
        result = [list(prompt_registry._parse_frontmatter(text)) for text in request["texts"]]
    elif kind == "registry":
        registry = prompt_registry.PromptRegistry(Path(request["dir"]))
        result = {
            "load_all": [vars(config) for config in registry.load_all()],
            "get": {
                name: (vars(c) if (c := registry.get(name)) else None) for name in request["get"]
            },
            "matching": {
                title: [c.id for c in registry.matching(title)] for title in request["titles"]
            },
        }
    elif kind == "settings":
        store = store_for(request)
        result = [outcome(lambda op=op: settings_op(store, op)) for op in request["ops"]]
    elif kind == "validate":
        result = [
            outcome(lambda case=case: ai_settings._task_values(case["task_id"], case["values"]))
            for case in request["cases"]
        ]
    elif kind == "models":
        result = [outcome(lambda case=case: ai_settings._models(case)) for case in request["cases"]]
    elif kind == "provider":
        result = outcome(lambda: asyncio.run(run_provider(request)))
    elif kind == "provider_for_settings":
        store = store_for(request)
        settings = store.resolve_task(request["task_id"])
        provider = ai_provider.provider_for_settings(
            settings, task=request["task"], provider=request.get("provider")
        )
        result = outcome(
            lambda: {
                "name": provider.name,
                "answer": asyncio.run(provider.complete(request["prompt"])),
            }
        )
    elif kind == "increment":
        result = outcome(lambda: asyncio.run(run_increment(request)))
    elif kind == "title":
        settings = store_for(request).resolve_task("title")
        result = title_generator.generate_title(
            request["transcript"],
            request.get("model"),
            settings=settings,
            variables=request.get("variables"),
        )
    elif kind == "meeting_title":
        result = run_meeting_title(request)
    elif kind == "sample":
        result = title_generator._sample_transcript(request["transcript"])
    elif kind == "chat":
        path = Path(request["meeting_path"])
        result = [outcome(lambda op=op: chat_op(path, op)) for op in request["ops"]]
    elif kind == "mark":
        result = [
            ai_provider.mark_ccs_automation(case["prompt"], case["task"])
            for case in request["cases"]
        ]
    elif kind == "codex_command":
        result = [
            ai_provider.build_codex_exec_command(
                case["cli"],
                case["model"],
                case["effort"],
                ephemeral=case["ephemeral"],
                isolated=case["isolated"],
            )
            for case in request["cases"]
        ]
    elif kind == "classes":
        result = {
            "space": ranges(str.isspace),
            "alnum": ranges(str.isalnum),
            "printable": ranges(str.isprintable),
            "other": ranges(lambda ch: unicodedata.category(ch).startswith("C")),
            "word": ranges(lambda ch: ai_settings._SAVED_ID.fullmatch("saved:" + ch) is not None),
            "unidata": unicodedata.unidata_version,
        }
    elif kind == "dumps":
        result = [
            json.dumps(json.loads(text), ensure_ascii=False, indent=indent)
            for text, indent in request["cases"]
        ]
    else:
        raise ValueError(f"unknown request {kind}")
    result_stream.write(json.dumps(result, ensure_ascii=False))


main()
