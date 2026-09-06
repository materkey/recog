"""Private, revisioned AI settings shared by CLI, web and live processes."""

from contextlib import contextmanager
import copy
import fcntl
import json
import os
from pathlib import Path
import re
import tempfile
import unicodedata
import uuid

from recog.ai_defaults import (
    DEFAULT_MODELS, DEFAULT_TASKS, PLACEHOLDERS, PROVIDERS, REASONING_EFFORTS, CLAUDE_EFFORTS,
)
from recog.prompt_registry import PromptRegistry

MAX_SETTINGS_BYTES = 256 * 1024
MAX_PROMPT_CHARS = 32000
MAX_TASKS = 256
_PLACEHOLDER_TOKEN = re.compile(r"{([A-Za-z_][A-Za-z0-9_]*(?:[.!:\[][^{\n}]*)?)}")
_SAVED_ID = re.compile(r"saved:[\w][\w.-]{0,159}\Z", re.UNICODE)
_INCREMENT_TASK = "analysis.increment"
_SAVED_INCREMENT_TASK = "saved:my-increment"


class SettingsConflictError(ValueError):
    """The caller's draft was based on an older settings revision."""


class SettingsReadError(ValueError):
    """Existing persisted settings are malformed; this is not invalid input."""


def _template_parts(template: str):
    """Track literal JSON braces so its adjacent closing braces stay intact."""
    opens = []
    index = 0
    while index < len(template):
        if template.startswith("{{", index):
            opens.append(True)
            yield "{", None
            index += 2
        elif template[index] == "{":
            match = _PLACEHOLDER_TOKEN.match(template, index)
            if match:
                yield "", match.group(1)
                index = match.end()
            else:
                opens.append(False)
                yield "{", None
                index += 1
        elif template[index] == "}":
            escaped = opens.pop() if opens else True
            index += 2 if escaped and template.startswith("}}", index) else 1
            yield "}", None
        else:
            yield template[index], None
            index += 1


def _template_variables(template: str) -> set[str]:
    found = {name for _, name in _template_parts(template) if name}
    unknown = found - set(PLACEHOLDERS)
    if unknown:
        raise ValueError("Unknown prompt placeholders: " + ", ".join(sorted(unknown)))
    return found


def render_prompt(template: str, variables: dict) -> str:
    """Render known placeholders once, preserving JSON and substituted braces."""
    if not isinstance(template, str) or not isinstance(variables, dict):
        raise ValueError("Prompt must be text and variables must be an object")
    found = _template_variables(template)
    if "transcript" in found and "transcript" not in variables:
        raise ValueError("Missing prompt variable: transcript")

    def replace(literal, name):
        if name is None:
            return literal
        value = variables.get(name, "")
        if not isinstance(value, str):
            raise ValueError(f"Prompt variable {name} must be text")
        return value

    return "".join(replace(literal, name) for literal, name in _template_parts(template))


def _known_fields(value, allowed, label):
    if not isinstance(value, dict):
        raise ValueError(f"{label} must be an object")
    if set(value) - set(allowed):
        raise ValueError(f"Unknown {label} fields: {', '.join(sorted(set(value) - set(allowed)))}")


def _models(value):
    _known_fields(value, PROVIDERS, "models")
    normalized = {}
    for provider, model in value.items():
        if not isinstance(model, str):
            raise ValueError("Model must be text")
        if not model.strip():
            continue
        if len(model) > 160 or any(c.isspace() or unicodedata.category(c).startswith("C") for c in model):
            raise ValueError("Model must be at most 160 characters without whitespace or control characters")
        if model.startswith("-"):
            raise ValueError("Model must not start with a dash")
        normalized[provider] = model
    return normalized


def _valid_task_id(task_id):
    if not isinstance(task_id, str) or not (task_id in DEFAULT_TASKS or _SAVED_ID.fullmatch(task_id)):
        raise ValueError("Unknown AI task")


def _task_values(task_id, values):
    _valid_task_id(task_id)
    _known_fields(values, ("prompt", "providers", "models", "reasoning_effort", "claude_effort"), "task")
    normalized = {}
    if "prompt" in values:
        prompt = values["prompt"]
        if not isinstance(prompt, str) or not prompt.strip() or len(prompt) > MAX_PROMPT_CHARS or "\x00" in prompt:
            raise ValueError(f"Prompt must be nonempty text of at most {MAX_PROMPT_CHARS} characters")
        if task_id != "copilot":
            if "transcript" not in _template_variables(prompt):
                raise ValueError("Prompt must include {transcript}")
        normalized["prompt"] = prompt
    if "providers" in values:
        providers = values["providers"]
        allowed = ("claude",) if task_id == "copilot" else PROVIDERS
        limit = len(allowed) if task_id.startswith("analysis.") else 1
        if (not isinstance(providers, list) or not 1 <= len(providers) <= limit
                or any(not isinstance(p, str) or p not in allowed for p in providers)
                or len(set(providers)) != len(providers)):
            raise ValueError("Invalid provider selection")
        normalized["providers"] = list(providers)
    if "models" in values:
        models = _models(values["models"])
        if task_id == "copilot" and set(models) - {"claude"}:
            raise ValueError("Copilot supports only Claude")
        if models:
            normalized["models"] = models
    if "reasoning_effort" in values:
        effort = values["reasoning_effort"]
        if not isinstance(effort, str) or effort not in REASONING_EFFORTS:
            raise ValueError("Invalid Codex reasoning effort")
        if task_id == "copilot":
            raise ValueError("Copilot does not support Codex reasoning effort")
        normalized["reasoning_effort"] = effort
    if "claude_effort" in values:
        effort = values["claude_effort"]
        if not isinstance(effort, str) or effort not in CLAUDE_EFFORTS:
            raise ValueError("Invalid Claude effort")
        normalized["claude_effort"] = effort
    return normalized


class AiSettingsStore:
    def __init__(self, data_dir: Path, prompts_dir: Path | None = None):
        self.path = Path(data_dir) / "ai-settings.json"
        self.prompts_dir = Path(prompts_dir) if prompts_dir is not None else Path(__file__).parent / "prompts"

    def read(self) -> dict:
        try:
            with self.path.open("rb") as stream:
                data = stream.read(MAX_SETTINGS_BYTES + 1)
        except FileNotFoundError:
            return {"version": 1, "revision": "defaults", "models": {}, "tasks": {}}
        if len(data) > MAX_SETTINGS_BYTES:
            raise SettingsReadError("AI settings file exceeds size limit")
        try:
            raw = json.loads(data)
        except (ValueError, UnicodeError) as exc:
            raise SettingsReadError("Invalid AI settings JSON") from exc
        try:
            # Retired features may still be present in settings saved by an
            # earlier version. Ignore them without modifying the file on read.
            if isinstance(raw, dict):
                if isinstance(raw.get("models"), dict):
                    raw["models"].pop("openrouter", None)
                if isinstance(raw.get("tasks"), dict):
                    raw["tasks"].pop("anchor", None)
                    raw["tasks"].pop("analysis.custom", None)
                    for values in raw["tasks"].values():
                        if not isinstance(values, dict):
                            continue
                        if isinstance(values.get("models"), dict) and "openrouter" in values["models"]:
                            values["models"].pop("openrouter", None)
                            if not values["models"]:
                                values.pop("models")
                        if isinstance(values.get("providers"), list) and "openrouter" in values["providers"]:
                            values["providers"] = [p for p in values["providers"] if p != "openrouter"]
                            if not values["providers"]:
                                values.pop("providers")
            self._validate_raw(raw)
        except ValueError as exc:
            raise SettingsReadError(str(exc)) from exc
        return raw

    @staticmethod
    def _validate_raw(raw):
        _known_fields(raw, ("version", "revision", "models", "tasks"), "settings")
        if set(raw) != {"version", "revision", "models", "tasks"}:
            raise ValueError("Incomplete AI settings")
        if type(raw["version"]) is not int or raw["version"] != 1:
            raise ValueError("Unsupported AI settings version")
        try:
            if not isinstance(raw["revision"], str) or str(uuid.UUID(raw["revision"])) != raw["revision"]:
                raise ValueError("Invalid settings revision")
        except (ValueError, TypeError, AttributeError) as exc:
            raise ValueError("Invalid settings revision") from exc
        if _models(raw["models"]) != raw["models"]:
            raise ValueError("Noncanonical model overrides")
        tasks = raw["tasks"]
        if not isinstance(tasks, dict) or len(tasks) > MAX_TASKS:
            raise ValueError("Invalid task overrides")
        for task_id, values in tasks.items():
            if _task_values(task_id, values) != values:
                raise ValueError("Noncanonical task override")

    @staticmethod
    def _effective_models(snapshot):
        models = dict(DEFAULT_MODELS)
        for provider in PROVIDERS:
            if provider in snapshot["models"]:
                continue
            legacy = os.getenv(f"{provider.upper()}_MODEL", "")
            if legacy.strip():
                models.update(_models({provider: legacy}))
        models.update(snapshot["models"])
        return models

    def _definition(self, task_id, fallback_prompt=None, fallback_label=None):
        _valid_task_id(task_id)
        if task_id in (_INCREMENT_TASK, _SAVED_INCREMENT_TASK):
            definition = dict(DEFAULT_TASKS[_INCREMENT_TASK])
            config = PromptRegistry(self.prompts_dir).get("my-increment")
            if config is not None:
                definition["prompt"] = config.template
            elif fallback_prompt is not None:
                definition["prompt"] = fallback_prompt
            return definition
        if task_id in DEFAULT_TASKS:
            return DEFAULT_TASKS[task_id]
        config = PromptRegistry(self.prompts_dir).get(task_id[len("saved:"):])
        if config is not None:
            return {"label": config.name, "prompt": config.template, "multiple": False}
        if fallback_prompt is not None:
            _task_values(task_id, {"prompt": fallback_prompt})
            return {"label": fallback_label or task_id, "prompt": fallback_prompt, "multiple": False}
        raise ValueError("Unknown saved prompt")

    def resolve_task(self, task_id, *, snapshot=None, fallback_prompt=None, fallback_label=None):
        snapshot = self.read() if snapshot is None else snapshot
        definition = self._definition(task_id, fallback_prompt, fallback_label)
        override = snapshot["tasks"].get(task_id, {})
        if task_id in (_INCREMENT_TASK, _SAVED_INCREMENT_TASK):
            # Read both historical IDs without rewriting existing settings.
            # The visible scenario takes precedence on fields customized twice.
            override = {**snapshot["tasks"].get(_SAVED_INCREMENT_TASK, {}),
                        **snapshot["tasks"].get(_INCREMENT_TASK, {})}
        if task_id.startswith("analysis."):
            providers = ["claude", "codex"]
        elif task_id == "copilot":
            providers = ["claude"]
        else:
            providers = ["codex"]
        effort = override.get("reasoning_effort", os.getenv("CODEX_REASONING_EFFORT", "xhigh") or "xhigh")
        if effort not in REASONING_EFFORTS:
            raise ValueError("Invalid legacy CODEX_REASONING_EFFORT")
        models = self._effective_models(snapshot)
        models.update(override.get("models", {}))
        providers = list(override.get("providers", providers))
        if task_id == _SAVED_INCREMENT_TASK:
            # Saved versions contain one result; keep their existing Codex
            # preference when comparison has multiple selected providers.
            providers = ["codex" if "codex" in providers else providers[0]]
        return {
            "id": task_id, "label": definition["label"],
            "prompt": override.get("prompt", definition["prompt"]),
            "default_prompt": definition["prompt"],
            "providers": providers,
            "allowed_providers": ["claude"] if task_id == "copilot" else list(PROVIDERS),
            "multiple": definition["multiple"], "models": models,
            "model_overrides": dict(override.get("models", {})),
            "reasoning_effort": override.get("reasoning_effort", effort),
            "claude_effort": override.get("claude_effort", ""),
            "placeholders": [] if task_id == "copilot" else list(PLACEHOLDERS),
            "required_placeholders": [] if task_id == "copilot" else ["transcript"],
            "apply_mode": "next_event_model_next_session" if task_id == "copilot" else "next_request",
            "customized": bool(override), "revision": snapshot["revision"],
        }

    def _view(self, snapshot):
        task_ids = list(DEFAULT_TASKS)
        task_ids.extend(f"saved:{config.id}" for config in PromptRegistry(self.prompts_dir).load_all()
                        if config.id != "my-increment")
        return {
            "revision": snapshot["revision"], "models": self._effective_models(snapshot),
            "model_overrides": dict(snapshot["models"]),
            "tasks": [self.resolve_task(task_id, snapshot=snapshot) for task_id in task_ids],
        }

    def view(self):
        return self._view(self.read())

    @contextmanager
    def _write_lock(self):
        self.path.parent.mkdir(parents=True, exist_ok=True)
        # A stable inode is necessary: locking the replaced settings file itself
        # would allow another process to enter through the new inode.
        fd = os.open(self.path.with_suffix(".lock"), os.O_CREAT | os.O_RDWR, 0o600)
        try:
            os.fchmod(fd, 0o600)
            fcntl.flock(fd, fcntl.LOCK_EX)
            yield
        finally:
            os.close(fd)

    def _mutate(self, update, expected_revision):
        if expected_revision is not None and not isinstance(expected_revision, str):
            raise ValueError("Revision must be text")
        with self._write_lock():
            snapshot = self.read()
            if expected_revision is not None and expected_revision != snapshot["revision"]:
                raise SettingsConflictError("AI settings changed; reload before saving")
            snapshot = copy.deepcopy(snapshot)
            update(snapshot)
            snapshot["revision"] = str(uuid.uuid4())
            self._validate_raw(snapshot)
            # Resolve before committing so a failing view cannot report an error
            # after successfully changing the user's settings.
            view = self._view(snapshot)
            data = (json.dumps(snapshot, ensure_ascii=False, indent=2) + "\n").encode("utf-8")
            if len(data) > MAX_SETTINGS_BYTES:
                raise ValueError("AI settings exceed size limit")
            fd, name = tempfile.mkstemp(prefix=".ai-settings-", suffix=".tmp", dir=self.path.parent)
            try:
                with os.fdopen(fd, "wb") as stream:
                    os.fchmod(stream.fileno(), 0o600)
                    stream.write(data)
                    stream.flush()
                    os.fsync(stream.fileno())
                os.replace(name, self.path)
            finally:
                if os.path.exists(name):
                    os.unlink(name)
            return view

    def write_models(self, models: dict, expected_revision=None):
        normalized = _models(models)

        def update(snapshot):
            for provider in models:
                snapshot["models"].pop(provider, None)
            snapshot["models"].update(normalized)

        return self._mutate(update, expected_revision)

    def write_task(self, task_id, values: dict, expected_revision=None):
        if task_id == _SAVED_INCREMENT_TASK:
            task_id = _INCREMENT_TASK
        self._definition(task_id)
        normalized = _task_values(task_id, values)

        def update(snapshot):
            if task_id == _INCREMENT_TASK:
                snapshot["tasks"].pop(_SAVED_INCREMENT_TASK, None)
            if normalized:
                snapshot["tasks"][task_id] = normalized
            else:
                snapshot["tasks"].pop(task_id, None)

        return self._mutate(update, expected_revision)

    def reset_task(self, task_id, expected_revision=None):
        if task_id == _SAVED_INCREMENT_TASK:
            task_id = _INCREMENT_TASK
        self._definition(task_id)
        def update(snapshot):
            snapshot["tasks"].pop(task_id, None)
            if task_id == _INCREMENT_TASK:
                snapshot["tasks"].pop(_SAVED_INCREMENT_TASK, None)
        return self._mutate(update, expected_revision)
