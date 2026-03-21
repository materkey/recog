# Prompt Registry — Design

## Summary

Замена захардкоженного INCREMENT_PROMPT на конфигурируемый реестр промптов. Каждый промпт — отдельный .md файл в `prompts/` с YAML frontmatter. При ktalk_leave запускаются все подходящие промпты параллельно. Результаты хранятся per-prompt: `increments/<prompt-id>/v1.md`.

## Requirements

1. Промпты хранятся как .md файлы в `prompts/` директории
2. YAML frontmatter: `name` (отображаемое), `filter` (regex для title встречи, null = все)
3. Доступные переменные в промптах: `{user_name}`, `{transcript}`, `{participants}`, `{meeting_title}`
4. При ktalk_leave — загрузить промпты, отфильтровать по title, запустить параллельно
5. Результаты в `increments/<prompt-id>/v1.md` (поддиректория на prompt)
6. Delta-генерация per-prompt (у каждого свой version counter)
7. AI провайдер: один для всех (Codex из .env)
8. Миграция: переместить старые `increments/v*.md` → `increments/my-increment/v*.md`

## Architecture

### Prompt file format

```markdown
---
name: My increment
filter: null
---

Проанализируй транскрипт рабочей встречи.

Участник: {user_name}
...

Транскрипт:
{transcript}
```

- ID промпта = имя файла без `.md` (e.g. `my-increment`)
- `name` — отображаемое имя в UI
- `filter` — regex для title встречи. `null` = все встречи. Пример: `"Sync|синк|sync"`
- Переменные: `{user_name}`, `{transcript}`, `{participants}`, `{meeting_title}`

### Storage layout

```
prompts/                        ← NEW: prompt definitions
  my-increment.md
  team-facts.md
  my-plan.md

meetings/2026-03-21_143025_abc/
  increments/
    my-increment/               ← subdirectory per prompt_id
      v1.md
      v1_meta.json
      v2.md
      v2_meta.json
    team-facts/
      v1.md
      v1_meta.json
```

### New module: recog/prompt_registry.py

```python
@dataclass
class PromptConfig:
    id: str                    # filename without .md
    name: str                  # display name from frontmatter
    filter: Optional[str]      # regex for title, None = all meetings
    template: str              # prompt body with {variable} placeholders

class PromptRegistry:
    def __init__(self, prompts_dir: Path):
        self._dir = prompts_dir

    def load_all(self) -> list[PromptConfig]:
        """Load all .md files from prompts/, parse frontmatter."""

    def get(self, prompt_id: str) -> Optional[PromptConfig]:
        """Load a single prompt by ID."""

    def matching(self, meeting_title: str) -> list[PromptConfig]:
        """Return prompts whose filter matches the title (or filter is null)."""
```

Frontmatter parsing: split on `---` markers, parse YAML header, rest is template body.
No PyYAML dependency needed — frontmatter is simple enough for manual parsing (2 fields).

### Changes to IncrementGenerator

```python
# New method — entry point from ktalk_bridge:
async def generate_all(self, meeting_id: str, meeting_path: Path):
    """Load matching prompts, run generate() for each in parallel."""
    registry = PromptRegistry(self._prompts_dir)
    meta = load meeting meta for title
    prompts = registry.matching(meta.get("title", ""))
    tasks = [self.generate(meeting_id, meeting_path, p) for p in prompts]
    await asyncio.gather(*tasks, return_exceptions=True)

# Modified — now takes prompt config:
async def generate(self, meeting_id, meeting_path, prompt: PromptConfig) -> Optional[int]:
    # Delta version per prompt_id subdirectory
    # Storage: increments/<prompt.id>/vN.md
    # Template: prompt.template.format(user_name=..., transcript=..., participants=..., meeting_title=...)
```

`_next_version()` and `_increments_dir()` take `prompt_id` parameter to scope to subdirectory.

### ktalk_bridge.py changes

Replace `generate()` call with `generate_all()`:
```python
# Was:
asyncio.create_task(self._increment_generator.generate(prev_id, prev_path))

# Now:
asyncio.create_task(self._increment_generator.generate_all(prev_id, prev_path))
```

### REST API

```
GET /api/prompts                                     → [{id, name, filter}]
GET /api/meetings/{id}/increments                    → {prompt_id: [metas]}
GET /api/meetings/{id}/increments/{prompt_id}        → [metas]
GET /api/meetings/{id}/increments/{prompt_id}/{ver}  → {meta, content}
```

### UI changes (app.js)

"My increment" button → "AI Analysis" showing all generated prompts:

- Level 1 tabs: prompt names (My increment, Team facts, My plan)
- Level 2 tabs: versions within selected prompt (v1, v2)
- Content: markdown rendering of selected version
- Regenerate button per prompt

### Migration

One-time migration of existing `increments/v*.md` files:

```
increments/v1.md          → increments/my-increment/v1.md
increments/v1_meta.json   → increments/my-increment/v1_meta.json
increments/v2.md          → increments/my-increment/v2.md
increments/v2_meta.json   → increments/my-increment/v2_meta.json
```

Run at startup in IncrementGenerator.__init__() or as a standalone migration script.

### Default prompts

Ship with `prompts/my-increment.md` containing the current INCREMENT_PROMPT.
Remove the hardcoded INCREMENT_PROMPT from ai_provider.py.

## Edge Cases

1. **No prompts dir / empty**: skip generation, log warning
2. **Prompt parse error**: skip that prompt, log error, continue with others
3. **Filter doesn't match any meetings**: normal — prompt just doesn't run
4. **New prompt added**: next ktalk_leave will pick it up automatically
5. **Prompt deleted**: existing results in increments/ stay, just no new generations
6. **Multiple prompts fail**: gather(return_exceptions=True) — each failure independent
