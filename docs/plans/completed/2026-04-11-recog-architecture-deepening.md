# Recog Architecture Deepening

## Overview
- Углубление модулей проекта Recog: удаление мёртвого кода диаризации (~1800 строк), устранение инвертированных зависимостей `recog/` → `frontend/`, декомпозиция `LiveTranscriber.run()` (570 → ~60 строк оркестрации)
- Проблема: `recog/` не может быть standalone-библиотекой из-за циклических зависимостей; `run()` — нетестируемый god method; ~1800 строк неиспользуемого кода (pyannote-диаризация, batch pipeline)
- Результат: чистая однонаправленная зависимость `frontend/ → recog/`, тестируемые методы вместо closures, удаление мёртвого кода
- ~~Дедупликация speaker assignment~~ — **отменена**: агент-исследователь установил, что 3 реализации принципиально разные (batch overlap vs real-time KTalk vs per-chunk embedding), а после удаления `alignment.py` и `diarize_session.py` остаётся только одна реализация в `live.py`

## Context (from deep agent research)

### Инверсия слоёв — точная карта
Ровно 2 файла в `recog/` импортируют из `frontend/`:
- `recog/increment.py:18` — `from frontend.ai_provider import CodexCliProvider`
- `recog/title_generator.py:12` — `from frontend.ai_provider import _resolve_cli`

Транзитивная цепочка: `meeting_storage.py` → `recog.title_generator` → `frontend.ai_provider`

Что переносить из `frontend/ai_provider.py` в `recog/ai_provider.py`:
- `_resolve_cli()` (строки 34–57) — поиск CLI binary через PATH/nvm
- `_nvm_version_key()` (строки 27–31) — helper для `_resolve_cli`
- `AIProvider` ABC (строки 132–157) — базовый класс
- `CodexCliProvider` (строки 241–353) — единственный провайдер, нужный `recog/`

Что остаётся в `frontend/ai_provider.py`:
- `ClaudeCliProvider` (строки 159–238)
- `OpenRouterProvider` (строки 356–393)
- `get_provider()`, `get_all_providers()`, `build_prompt()` (строки 396–431)
- Prompt constants: `INCREMENT_PROMPT`, `SUMMARY_PROMPT`, `ANCHOR_PROMPT`

### Диаризация — точная карта удаления

| Файл | Действие | Импортируется в |
|------|----------|-----------------|
| `diarize_worker.py` | DELETE | `recog/diarization.py:46`, `build_speaker_db.py:61` (оба удаляются) |
| `recog/diarization.py` | DELETE | `recog/pipeline.py:16`, `recog/diarize_session.py:13` (оба удаляются) |
| `recog/diarize_session.py` | DELETE | `frontend/server.py:986` (endpoint удаляется) |
| `recog/alignment.py` | DELETE | `recog/pipeline.py:83,87` (удаляется). **НЕ импортируется live.py** |
| `recog/audio.py` | DELETE | `recog/pipeline.py:49,131,179` (удаляется) |
| `recog/pipeline.py` | DELETE | `mp4_notes.py:213,336,346` (команды удаляются) |
| `build_speaker_db.py` | DELETE | нигде (standalone скрипт) |
| `embedding_worker.py` | KEEP | `frontend/server.py:1877` (`/api/speakers/enroll-from-segments`) |
| `recog/models.py` | MODIFY | удалить `DiarizationSegment` (строки 20–24), остальное живое |

CLI команды mp4_notes.py:
- `transcribe` → **DEAD** (зависит от pipeline)
- `enroll-speaker` → **DEAD** (зависит от pipeline)
- `build-speaker-db` → **DEAD** (зависит от pipeline)
- `live` → **SURVIVES**
- `meetings` → **SURVIVES**

Frontend endpoints:
- `POST /api/meetings/{id}/diarize` (строки 945–1011) → **DEAD** (импортирует diarize_session)
- `POST /api/meetings/{id}/undo-diarize` (строки 1014–1036) → **DEAD**
- `DiarizeRequest` Pydantic model (строка 941) → удалить
- Все остальные endpoints → **SURVIVE**

### LiveTranscriber.run() — полная карта closures

| Closure | Строки | Captures | Extractable |
|---------|--------|----------|-------------|
| `normalize_word` | 791–792 | ничего (pure) | YES → module-level |
| `deduplicate` | 794–807 | `normalize_word` | YES → module-level |
| `process_pending` | 809–868 | `self`, `speaker_turn_gap`, `max_segment_duration` | YES → метод (после вынесения констант) |
| `process_chunk` | 870–1155 | `self`, `executor`, `mic_speaker_name`, `speaker_turn_gap`, `max_segment_duration`, `deduplicate` | PARTIAL → dispatcher + 3 handler-метода |
| `get_speaker` | 1149–1151 | `self` | YES → `_resolve_speaker_embedding()` |

Source-text тесты которые СЛОМАЮТСЯ при декомпозиции:
- `test_live_speaker_attribution.py:test_sole_unmuted_fallback_in_speaker_assignment_chain` — проверяет порядок по `.index()` в исходнике
- `test_acceptance_ktalk_unknown.py:test_orphan_meeting_no_sole_unmuted_check` — ищет строки по номерам строк
- `test_acceptance_ktalk_unknown.py:test_orphan_meeting_no_retroactive_correction` — аналогично
- `test_acceptance_ktalk_unknown.py:test_mic_speaker_path_unchanged` — ищет комментарий и проверяет indentation-блок
- `test_acceptance_ktalk_unknown.py:test_fallback_chain_order_in_source` — `.index()` на 5 символах после `elif self.use_ktalk:`

### Дубликаты в ktalk_bridge.py
- `_find_cli_binary()` (строки 685–707) — функциональный дубликат `_resolve_cli` из `frontend/ai_provider.py`
- `_load_dotenv()` (строки 38–50) — функциональный дубликат `recog.config.load_dotenv()`

## Development Approach
- **Testing approach**: Regular (код → тесты)
- **Constraint**: промежуточные состояния могут быть сломаны, финальный результат должен работать
- Complete each task fully before moving to the next
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
- **CRITICAL: all tests must pass before starting next task** — no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**

## Testing Strategy
- **Unit tests**: `tests/test_increment.py` (обновить импорты), новый `tests/test_ai_provider.py`
- **Existing tests**: все должны проходить после каждого task (кроме явно удалённых)
- **Smoke test**: `python mp4_notes.py --help`, `python -m compileall recog frontend`
- **Regression**: `python -m pytest tests/ -v` после каждого task
- **Source-text тесты**: переписать на поведенческие assertions в Task 5

## Progress Tracking
- Mark completed items with `[x]` immediately when done
- Add newly discovered tasks with ➕ prefix
- Document issues/blockers with ⚠️ prefix

## Implementation Steps

---
### WAVE 1: Удаление мёртвого кода + исправление слоёв
---

### Task 1: Удалить неиспользуемый код диаризации и batch pipeline

Пользователь подтвердил: pyannote-диаризация не используется. Спикеры определяются только через KTalk extension.

**Удалить файлы целиком:**
- [x] `diarize_worker.py` (236 строк — subprocess worker для pyannote)
- [x] `build_speaker_db.py` (standalone утилита, зависит от diarize_worker)
- [x] `recog/diarization.py` (469 строк — `DiarizationModule` + `run_diarization_pyannote4`)
- [x] `recog/diarize_session.py` (244 строки — post-hoc re-diarize)
- [x] `recog/alignment.py` (104 строки — ASR+diarization merge. **Проверено: live.py НЕ импортирует**)
- [x] `recog/audio.py` (76 строк — ffmpeg extract, используется только pipeline.py)
- [x] `recog/pipeline.py` (200 строк — `transcribe_pipeline`, `enroll_speaker_pipeline`, `build_speaker_db_pipeline`)
- [x] `tests/test_alignment.py` (тестирует удалённый alignment.py)
- [x] `tests/e2e/test_diarization_e2e.py` (e2e тесты pyannote diarization)

**Модифицировать файлы:**
- [x] `mp4_notes.py`: удалить subparsers и обработку команд `transcribe`, `enroll-speaker`, `build-speaker-db` (строки 35–207, 213, 336, 346). Оставить `live` и `meetings`
- [x] `frontend/server.py`: удалить `DiarizeRequest` model (строка 941), endpoint `diarize_meeting` (строки 945–1011), endpoint `undo_diarization` (строки 1014–1036), импорт `diarize_session`
- [x] `recog/config.py`: удалить `PYANNOTE4_VENV` и `DIARIZE_WORKER` (строки 65–66)
- [x] `recog/models.py`: удалить `DiarizationSegment` dataclass (строки 20–24)
- [x] `recog/__init__.py`: удалить lazy-load entries для всех удалённых модулей (`DiarizationModule`, `run_diarization_pyannote4`, `align_segments_with_speakers`, `resolve_speaker_labels`, `transcribe_pipeline`, `enroll_speaker_pipeline`, `build_speaker_db_pipeline`, `extract_audio_to_wav`, `get_audio_path`, `DiarizationSegment`, `PYANNOTE4_VENV`, `DIARIZE_WORKER`)
- [x] `tests/conftest.py`: удалить импорт `DiarizationSegment` (строка 19) и fixture `diarization_segments` (строки 82–87)
- [x] `setup_venvs.sh`: удалить секцию создания `venv-pyannote` (строки 51–66)

**Verification:**
- [x] `python -m compileall recog frontend` — без ошибок
- [x] `python -m pytest tests/ -v` — все оставшиеся тесты проходят (73 passed)
- [x] `python mp4_notes.py --help` — CLI работает, показывает только `live` и `meetings`

### Task 2: Исправить инверсию слоёв — вынести AI provider в recog/

**Создать `recog/ai_provider.py`:**
- [x] перенести `AIProvider` ABC из `frontend/ai_provider.py:132–157`
- [x] перенести `CodexCliProvider` из `frontend/ai_provider.py:241–353`
- [x] перенести `_resolve_cli()` из `frontend/ai_provider.py:34–57` → сделать публичной `resolve_cli()`
- [x] перенести `_nvm_version_key()` из `frontend/ai_provider.py:27–31` → заменить на вызов `from recog.node_env import version_key`
- [x] импорты внутри нового файла: `from recog.node_env import get_node_env, version_key`

**Обновить `frontend/ai_provider.py`:**
- [x] удалить перенесённый код (`AIProvider`, `CodexCliProvider`, `_resolve_cli`, `_nvm_version_key`)
- [x] добавить: `from recog.ai_provider import AIProvider, CodexCliProvider, resolve_cli` (для обратной совместимости и использования в `ClaudeCliProvider`)
- [x] `ClaudeCliProvider` остаётся в `frontend/` — теперь наследует `AIProvider` через реэкспорт

**Обновить импорты в recog/:**
- [x] `recog/increment.py:18`: `from frontend.ai_provider import CodexCliProvider` → `from recog.ai_provider import CodexCliProvider`
- [x] `recog/title_generator.py:12`: `from frontend.ai_provider import _resolve_cli` → `from recog.ai_provider import resolve_cli`; обновить вызов на строке 53

**Сделать приватные символы публичными:**
- [x] `recog/node_env.py`: `_version_key()` → `version_key()`, обновить внутренний вызов (строка 41)
- [x] `frontend/ai_provider.py`: обновить импорт `_version_key` → `version_key` (строка 24, использование строка 31)
- [x] `ktalk_bridge.py`: обновить импорт `_version_key` → `version_key` (строка 32, использование строка 703)
- [x] `recog/speaker_db.py`: `_cosine_similarity` → `cosine_similarity` (staticmethod, строка 220)
- [x] `recog/live.py`: `SpeakerDatabase._cosine_similarity` → `SpeakerDatabase.cosine_similarity` (строка 446)

**Устранить дубликаты в ktalk_bridge.py:**
- [x] заменить `_find_cli_binary()` (строки 685–707) на вызов `from recog.ai_provider import resolve_cli`
- [x] заменить `_load_dotenv()` (строки 38–50) на `from recog.config import load_dotenv; load_dotenv()`

**Verification:**
- [x] `python -c "from recog.ai_provider import CodexCliProvider, resolve_cli"` — без ошибки
- [x] `grep -r "from frontend" recog/` — пусто (нет импортов frontend из recog)
- [x] `grep -rn "_version_key\|_resolve_cli\|_cosine_similarity" recog/ frontend/ ktalk_bridge.py` — нет приватных cross-module imports
- [x] `python -m compileall recog frontend` — без ошибок
- [x] `python -m pytest tests/ -v` — все тесты проходят

---
### WAVE 2: Декомпозиция LiveTranscriber.run()
---

### Task 3: Phase 1 — безопасные извлечения (не ломают тесты)

8 извлечений из `run()`, которые не затрагивают source-text assertions в тестах:

- [x] `SourceState` (строка 773 в run()) → module-level `@dataclass` рядом с `RecentSegment` (~строка 271). Добавить type alias для pending-update tuple
- [x] `normalize_word` (строки 791–792) → module-level функция `def normalize_word(w: str) -> str`
- [x] `deduplicate` (строки 794–807) → module-level функция `def deduplicate(text: str, prev_words: list) -> str | None`
- [x] `get_speaker` closure (строки 1149–1151) → метод `_resolve_speaker_embedding(self, chunk: np.ndarray) -> str`
- [x] setup-блок (строки 686–717) → метод `_setup_meeting_storage(self) -> None`
- [x] KTalk wiring (строки 719–753) → метод `_setup_ktalk_bridge(self) -> None`
- [x] audio start (строки 757–763) → метод `_start_audio_captures(self) -> None`
- [x] finally-блок (строки 1180–1226) → метод `_teardown_session(self, executor, force_exit: bool) -> None`
- [x] Вынести `SPEAKER_TURN_GAP = 3.0` и `MAX_SEGMENT_DURATION = 20.0` в `recog/config.py` как константы

**Verification:**
- [x] `python -m pytest tests/ -v` — ВСЕ тесты проходят (включая source-text assertions)
- [x] `python -m compileall recog` — без ошибок

### Task 4: Phase 2 — извлечение process_chunk и process_pending (ломает source-text тесты)

**Сначала переписать source-text тесты на поведенческие:**
- [x] `test_live_speaker_attribution.py:test_sole_unmuted_fallback_in_speaker_assignment_chain` — заменить `.index()` проверку на прямой вызов KTalkBridge mock + проверку результата
- [x] `test_acceptance_ktalk_unknown.py:test_orphan_meeting_no_sole_unmuted_check` — заменить line-index арифметику на behavior assertion через mock
- [x] `test_acceptance_ktalk_unknown.py:test_orphan_meeting_no_retroactive_correction` — аналогично
- [x] `test_acceptance_ktalk_unknown.py:test_mic_speaker_path_unchanged` — заменить comment-scanning на behavior test (mock calls)
- [x] `test_acceptance_ktalk_unknown.py:test_fallback_chain_order_in_source` — удалить или заменить на behavioral test (порядок уже валидируется `test_speaker_switch_*` тестами)

**Затем извлечь:**
- [x] `process_pending` → `_process_pending(self, state: SourceState, is_mic: bool)` (используя `SPEAKER_TURN_GAP`, `MAX_SEGMENT_DURATION` из config)
- [x] silence-split check → `_check_silence_split(self, state: SourceState) -> bool` (7-строчный блок из process_chunk)
- [x] mic path → `_handle_mic_chunk(self, text, ts, chunk_time, chunk_gen, state: SourceState)` (строки 944–975)
- [x] KTalk path → `_handle_ktalk_chunk(self, text, ts, chunk_start_time, chunk_end_time, chunk_gen, state: SourceState)` (строки 978–1121)
- [x] async-diarization path → `_handle_embedding_chunk(self, text, ts, chunk_time, chunk_gen, state: SourceState, chunk, executor)` (строки 1140–1155)
- [x] Оставшийся `process_chunk` → dispatcher-метод `_process_chunk(self, capture, state, executor, is_mic)` (~40 строк): read audio → WAV write → VAD → ASR → dedup → silence split → branch to handler
- [x] `run()` → ~60 строк: setup calls, `while self.running: sleep(0.1)` + 4 вызова `_process_pending` + `_process_chunk`, try/finally

**Verification:**
- [x] `python -m pytest tests/ -v` — все тесты проходят (73 passed)
- [x] `python -m compileall recog` — без ошибок

---
### WAVE 3: Тесты новых компонентов + финализация
---

### Task 5: Тесты извлечённых методов

- [x] `tests/test_live_decomposed.py`:
  - [x] test: `normalize_word` — strips punctuation, lowercases
  - [x] test: `deduplicate` — removes leading overlap, returns None for full duplicate
  - [x] test: `_handle_mic_chunk` — always uses `MIC_SPEAKER_NAME`, `is_local=True`
  - [x] test: `_check_silence_split` — silence > `SILENCE_SPLIT_TIMEOUT` creates new session
  - [x] test: `_resolve_speaker_embedding` — delegates to `_get_embedding` + `_identify_speaker`
- [x] написать `tests/test_ai_provider.py`:
  - [x] test: `CodexCliProvider` importable from `recog.ai_provider`
  - [x] test: `resolve_cli("codex")` returns path or raises FileNotFoundError
  - [x] test: `resolve_cli("nonexistent_binary_xyz")` raises or returns fallback
- [x] проверить: `python -m pytest tests/ -v` — 100% pass

### Task 6: Финальная верификация

- [x] `python -m compileall recog frontend` — без ошибок
- [x] `python -m pytest tests/ -v` — все тесты проходят (109 passed)
- [x] `python mp4_notes.py --help` — CLI работает (live, meetings)
- [x] `python mp4_notes.py live --list-devices` — live mode стартует (4 устройства)
- [x] `grep -r "from frontend" recog/` — пусто
- [x] `grep -rn "_version_key\|_resolve_cli\|_cosine_similarity" recog/ frontend/ ktalk_bridge.py` — пусто (только внутренний _nvm_version_key в ai_provider.py)
- [x] Подсчитать строки ДО и ПОСЛЕ: recog/ 3194 строк (было ~4822, net -1628)

### Task 7: Обновить документацию

- [x] CLAUDE.md: удалить упоминания `transcribe` pipeline, `diarize_worker`, `venv-pyannote`, `alignment.py`, `diarize_session.py`, `pipeline.py`, `audio.py`
- [x] CLAUDE.md: добавить `recog/ai_provider.py` (CodexCliProvider, resolve_cli, AIProvider ABC) в Module Organization
- [x] CLAUDE.md: обновить описание `LiveTranscriber` — декомпозированные методы вместо monolithic run()
- [x] CLAUDE.md: убрать `enroll-speaker` и `build-speaker-db` из Running секции
- [x] ADR-0006: пометить pyannote-venv часть как "Obsolete" — осталась только одна venv (gigaam)

## Technical Details

### Граф зависимостей ПОСЛЕ рефакторинга

```
frontend/
  server.py ──→ recog.* (однонаправленно)
  ai_provider.py ──→ recog.ai_provider (импорт AIProvider, CodexCliProvider)

recog/
  ai_provider.py    ← NEW: AIProvider ABC, CodexCliProvider, resolve_cli()
  live.py           ← CHANGED: run() ~60 строк; 10+ извлечённых методов
  increment.py      ← CHANGED: import from recog.ai_provider
  title_generator.py ← CHANGED: import from recog.ai_provider
  speaker_db.py     ← CHANGED: cosine_similarity() public
  node_env.py       ← CHANGED: version_key() public
  config.py         ← CHANGED: -PYANNOTE4_VENV, -DIARIZE_WORKER, +SPEAKER_TURN_GAP, +MAX_SEGMENT_DURATION
  models.py         ← CHANGED: -DiarizationSegment
  asr.py            (без изменений)
  output.py         (без изменений)
  prompt_registry.py (без изменений)

ktalk_bridge.py     ← CHANGED: -_find_cli_binary (→ resolve_cli), -_load_dotenv (→ config.load_dotenv)

УДАЛЕНО (9 файлов, ~1800 строк):
  diarize_worker.py, build_speaker_db.py
  recog/diarization.py, recog/diarize_session.py, recog/alignment.py
  recog/audio.py, recog/pipeline.py
  tests/test_alignment.py, tests/e2e/test_diarization_e2e.py
```

### Метрики
- Удалено: ~1800 строк мёртвого кода + 9 файлов
- Добавлено: ~150 строк (`recog/ai_provider.py`)
- NET: -1650 строк
- Циклические зависимости: 2 → 0
- Приватные cross-module imports: 5 → 0
- Source-text test assertions: 6 → 0
- `LiveTranscriber.run()`: 570 строк → ~60 строк dispatcher

## Post-Completion

**Manual verification:**
- Запустить live-транскрипцию с KTalk extension — speaker attribution работает
- Открыть frontend (http://127.0.0.1:9080) — meetings, increments, speaker management
- Перезапустить сервисы: `./recog-service.sh restart`
- Проверить что speaker enrollment через UI (enroll-from-segments) работает (использует `embedding_worker.py`, не удалён)

**Cleanup:**
- Удалить `venv-pyannote/` директорию (больше не нужна, если `embedding_worker.py` переведён на `venv-gigaam`)
- Решить судьбу `add_speaker_live.py` (использует speechbrain напрямую, не через diarize_worker)
