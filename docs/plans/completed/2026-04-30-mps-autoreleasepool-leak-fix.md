# Fix: MPS Metal Command Buffer Leak via PyObjC Autoreleasepool

## Overview

Долгоживущий процесс `mp4_notes.py live --ktalk` накапливает Metal/MPS NSObject-ы и через ~3 суток uptime достигает **7.4 ГБ** physical footprint (96% уходит в swap), при том что resident < 400 МБ. Диагностика на живом процессе (`heap <pid>`):

| Класс | Кол-во | Σ байт |
|---|---|---|
| `AGXG16GFamilyCommandBuffer` | 2 427 462 | 2.18 ГБ |
| `AGXG16GFamilyCommandBuffer._impl` | 2 427 462 | 1.55 ГБ |
| `AGXG16GFamilyComputeContext` | 3 563 751 | 570 МБ |
| `MPSCommandBuffer` | 2 427 462 | 194 МБ |
| `NSArray` / `NSMutableArray` (variants) | ~33M | ~990 МБ |
| `@autoreleasepool content` | 66 804 | 273 МБ |

**Σ ≈ 5.7 ГБ** Apple Metal/MPS объектов накопились в worker-потоках. Корень — отсутствие NSAutoreleasePool в `PerSpeakerIngest` worker и `RefineWorker`, где идут forward passes на MPS, а также в callback'ах live-loop из `LiveTranscriber._transcribe_direct` (mic + fallback-mixed путь). Известная проблема: pytorch/pytorch#93397, #96975.

Цель: закрыть утечку точечной правкой `recog/asr.py` + `recog/live.py:_transcribe_direct`, без изменения семантики `_model_lock`, `torch.inference_mode()`, abort-механизма refine и любой другой inference-логики.

## Context (from discovery)

- **Файлы изменения:**
  - `recog/asr.py` — три `transcribe_*` метода + module-level helper
  - `recog/live.py` — `_transcribe_direct` (lines 441-474) — четвёртый горячий путь, обходит `transcribe_*` методы
  - `tests/test_asr.py` — unit-тесты (есть фикстуры `_make_mocked_asr`, `_make_timestamps_asr` — переиспользуем)
  - `adr/ADR-0019.md` — короткая заметка в "Operational concerns"
- **Контекст (читать, не трогать):**
  - `recog/live.py:875` — `_on_pcm_batch` → `asr.transcribe_batch` (PerSpeakerIngest worker thread)
  - `recog/live.py:820,826` — `_refine_transcribe_fn` → `asr.transcribe_with_timestamps` (RefineWorker thread)
  - `recog/refine.py` — `RefineWorker._process_task` (потокобезопасность уже обеспечена `_model_lock`)
- **Среда:**
  - venv-gigaam: PyObjC 12.1 ✅, torch 2.8.0 + MPS ✅, Python 3.12
  - На non-Apple хостах PyObjC отсутствует → нужен fallback через try/except
- **Конвенции проекта (CLAUDE.md):**
  - 4-space indent, snake_case, type hints, pathlib
  - Тесты `[given – when – then]` в названиях
  - Минимальные правки, без рефакторинга соседнего кода

### Четыре hot inference paths (после plan-review)

| # | Путь | Файл/строка | Поток |
|---|---|---|---|
| 1 | `transcribe_batch` | `asr.py:204` | PerSpeakerIngest worker (live PCM) |
| 2 | `transcribe_with_timestamps` (str) | `asr.py:240` | RefineWorker thread |
| 3 | `transcribe_with_timestamps` (ndarray) | `asr.py:266` | RefineWorker thread (через temp WAV) |
| 4 | `_transcribe_direct` | `live.py:466-470` | Main live loop (mic + fallback-mixed) |
| (5) | `transcribe_simple` chunk loop | `asr.py:122-148` | CLI offline, не в live mode |

Все четыре purely-MPS пути нуждаются в обёртке. Путь #5 — offline CLI; оборачиваем для consistency.

## Development Approach

- **Testing approach: TDD** (тесты → код)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - write unit tests for new functions/methods
  - write unit tests for modified functions/methods
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility (CPU-only хосты без PyObjC должны работать)

## Testing Strategy

- **Unit tests** в `tests/test_asr.py`:
  - `_mps_autoreleasepool(device)` контекст-менеджер: enter/exit на MPS, no-op на CPU, no-op без PyObjC, **drain pool при exception внутри `with`-блока**
  - `_maybe_empty_mps_cache` throttle: первые N-1 вызовов не вызывают `torch.mps.empty_cache`, N-й вызывает + сбрасывает счётчик; на CPU всегда no-op
  - Каждый из четырёх hot paths (`transcribe_batch`, `transcribe_with_timestamps` str и ndarray, `_transcribe_direct`) **с явным `asr.device = "mps"`**: оборачивает inference в `_mps_autoreleasepool` (assert `__enter__/__exit__` called) и вызывает `_maybe_empty_mps_cache`
  - Симметричный negative тест: на `asr.device = "cpu"` mock `objc.autorelease_pool` НЕ вызывается (защита от silently dropped wrapping)
  - `transcribe_simple`: per-chunk pool — N chunks → N enter/exit
- **E2E тестов нет** для этого слоя (real GPU forward не воспроизводится в unit-тесте дёшево); полагаемся на mock'и `objc` и `torch.mps`
- **Regression**: полный `pytest tests/` после правок
- **Manual smoke**: `python mp4_notes.py --help` + `recog-service.sh restart` + 24h uptime + `heap <pid>` контроль

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope
- keep plan in sync with actual work done

## Solution Overview

Module-level helper + throttled cache release, без изменения публичного API:

1. Try/except импорт `objc` с булевым флагом `_HAS_AUTORELEASE` (graceful degradation на не-macOS). **Импорт обязательно как `import objc`** (binds `recog.asr.objc`), чтобы тесты могли monkey-patch `recog.asr.objc.autorelease_pool`. На ImportError: `objc = None; _HAS_AUTORELEASE = False`.
2. `@contextlib.contextmanager _mps_autoreleasepool(device)` — оборачивает блок в `with objc.autorelease_pool(): yield` (НЕ ручной `__enter__/__exit__` — иначе exception в теле не дренит pool); fallback `else: yield` для не-MPS / без PyObjC.
3. `ASRModule._mps_inference_count` + `_MPS_EMPTY_CACHE_EVERY = 100` — throttled счётчик.
4. `ASRModule._maybe_empty_mps_cache()` — увеличивает счётчик и каждый 100-й вызов дёргает `torch.mps.empty_cache()`. На CPU/без MPS — no-op. Контракт: «best-effort approximate, race может удлинить интервал, но не укоротить» — счётчик `+=` не атомарный, но throttling не критичен к точности.
5. **Все четыре hot inference paths** оборачивают inference-блок в `_mps_autoreleasepool(self.asr.device)` (или `self.device` в `ASRModule`) и вызывают `_maybe_empty_mps_cache()` после forward'а:
   - `transcribe_batch` (asr.py)
   - `transcribe_with_timestamps` обе ветки (asr.py)
   - `transcribe_simple` per-chunk (asr.py)
   - `_transcribe_direct` (live.py) — отдельная правка, делегирует к `self.asr._maybe_empty_mps_cache()`

## Technical Details

### Helper signatures

```python
# module-level, recog/asr.py
import objc  # внутри try/except — см. ниже
import contextlib
from typing import Iterator  # уже совпадает со стилем модуля (typing.Optional/List)

try:
    import objc
    _HAS_AUTORELEASE = True
except ImportError:
    objc = None  # type: ignore[assignment]
    _HAS_AUTORELEASE = False


@contextlib.contextmanager
def _mps_autoreleasepool(device: str) -> Iterator[None]:
    if device == "mps" and _HAS_AUTORELEASE:
        # `with objc.autorelease_pool(): yield` — exception inside body
        # propagates AFTER pool drain (ContextManager protocol).
        with objc.autorelease_pool():
            yield
    else:
        yield


# ASRModule method
def _maybe_empty_mps_cache(self) -> None:
    """Best-effort throttled MPS allocator cache release.

    Counter increment is not atomic across threads (3 bytecodes); the race
    can drop multiple updates and stretch the firing interval well beyond
    `_MPS_EMPTY_CACHE_EVERY`, or fire back-to-back when two threads cross the
    threshold together. Both are idempotent and harmless — cache-release
    accuracy is not a correctness invariant.
    """
```

### Контракт `_maybe_empty_mps_cache`
- Может вызываться **вне `_model_lock`** (в `transcribe_simple` chunk loop счётчик не под lock — best-effort throttle).
- Под `_model_lock` (transcribe_batch, transcribe_with_timestamps, _transcribe_direct) — точный счёт.
- На CPU/non-MPS device — мгновенный no-op.
- Throttle: счётчик++; если ≥ 100 → `torch.mps.empty_cache()` + сброс счётчика.
- 100 ≈ 20 секунд при 5 Hz live-batch'ах; при низкой нагрузке (refine-only ~0.05 Hz) — реже, что допустимо.

### Контракт `_mps_autoreleasepool`
- На MPS + PyObjC: `with objc.autorelease_pool(): yield` — drain Apple-объектов на exit, **включая exception path** (CM protocol гарантирует `__exit__(exc_type, exc, tb)`).
- На CPU или без PyObjC: пустой `yield` — zero-overhead.
- Используется для **одного inference call** (не на весь loop), чтобы pool не разрастался.

### Точки оборачивания

**`transcribe_batch`** (asr.py, один pool на весь batch):
```python
with self._model_lock, torch.inference_mode(), _mps_autoreleasepool(self.device):
    encoded, encoded_len = model.forward(wav_batch, length_batch)
    decoded = model.decoding.decode(model.head, encoded, encoded_len)
    self._maybe_empty_mps_cache()
```

**`transcribe_with_timestamps`** (asr.py, обе ветки — один pool на каждый `model.transcribe`):
```python
with self._model_lock, _mps_autoreleasepool(self.device):
    result = model.transcribe(audio_or_tmp_path, word_timestamps=True)
    self._maybe_empty_mps_cache()
```

**`transcribe_simple`** (asr.py, per-chunk pool — обоснование ниже):
```python
with torch.inference_mode(), _mps_autoreleasepool(self.device):
    encoded, encoded_len = model.forward(wav, length)
    text = model.decoding.decode(model.head, encoded, encoded_len)[0][0]
    self._maybe_empty_mps_cache()
```

Per-chunk обоснование: `transcribe_simple` обрабатывает до ~80 chunks для часового файла. Per-chunk pool ограничивает peak Apple-object footprint размером chunk'а, не длиной файла. `transcribe_batch`, наоборот, делает один forward на батч до `PCM_MAX_BATCH_SIZE=8` без внутреннего цикла — там одного pool достаточно.

**`_transcribe_direct`** (live.py:466-470):
```python
try:
    with self.asr._model_lock, _mps_autoreleasepool(self.asr.device):
        encoded, encoded_len = model.forward(wav, length)
        text = model.decoding.decode(model.head, encoded, encoded_len)[0][0]
        self.asr._maybe_empty_mps_cache()
finally:
    if refine_worker is not None:
        refine_worker.clear_live_pending()
```

## What Goes Where

- **Implementation Steps**: код в `recog/asr.py` + `recog/live.py`, тесты в `tests/test_asr.py`
- **Post-Completion**: deploy через `recog-service.sh restart`, мониторинг footprint через 24-48 часов uptime, проверка `heap <pid>` на нулевую утечку

## Implementation Steps

### Task 1: TDD — тесты на helper `_mps_autoreleasepool` и `_maybe_empty_mps_cache`

**Files:**
- Modify: `tests/test_asr.py`

- [x] добавить тест `mps device with pyobjc - enters and exits objc.autorelease_pool` (monkeypatch `recog.asr.objc.autorelease_pool` на MagicMock context manager, проверить `__enter__/__exit__` called once)
- [x] добавить тест `cpu device - does not call objc.autorelease_pool` (monkeypatch, assert mock не вызван)
- [x] добавить тест `mps device without pyobjc - no-op fallback` (monkeypatch `recog.asr._HAS_AUTORELEASE = False`, no crash, no objc call)
- [x] добавить тест `mps_autoreleasepool drains on exception inside with-block` (raise inside body; assert `__exit__` invoked with non-None exc_info; exception propagates)
- [x] добавить тест `maybe_empty_mps_cache - 99 calls do not trigger empty_cache, 100th does, counter resets to 0` (monkeypatch `torch.mps.empty_cache`)
- [x] добавить тест `maybe_empty_mps_cache - cpu device is no-op even after 100 calls`
- [x] прогнать `python -m pytest tests/test_asr.py -v -k "autoreleasepool or empty_mps"` — должны падать с ImportError (RED, временно — Task 2 сразу добавляет stub)

### Task 2: Реализация helper и throttle в `recog/asr.py`

**Files:**
- Modify: `recog/asr.py`

- [x] добавить `import contextlib` и `from typing import Iterator` к существующим импортам
- [x] добавить `try: import objc; _HAS_AUTORELEASE = True\nexcept ImportError: objc = None; _HAS_AUTORELEASE = False` (объявленная форма из Solution Overview — критично для определённости monkey-patch'а)
- [x] добавить module-level `@contextlib.contextmanager _mps_autoreleasepool(device: str) -> Iterator[None]` с телом `with objc.autorelease_pool(): yield` (НЕ ручной `__enter__/__exit__`)
- [x] добавить в `ASRModule.__init__` поле `self._mps_inference_count: int = 0` и константу класса `_MPS_EMPTY_CACHE_EVERY: int = 100`
- [x] добавить method `ASRModule._maybe_empty_mps_cache(self) -> None` с docstring про best-effort race
- [x] прогнать тесты Task 1 — должны проходить (GREEN)

### Task 3: TDD — тесты на оборачивание четырёх hot paths

**Files:**
- Modify: `tests/test_asr.py`

Каждый тест **явно ставит `asr.device = "mps"`** и monkey-patch'ит `recog.asr.objc.autorelease_pool` MagicMock'ом, иначе wrapping silently no-op'ит и тест зеленеет по неправильным причинам.

- [x] добавить тест `transcribe_batch on mps wraps inference in pool and calls cache helper once` (`_make_mocked_asr` + `asr.device = "mps"`; spy на `_maybe_empty_mps_cache`; assert pool entered/exited once)
- [x] добавить тест `transcribe_batch on cpu does not enter pool` (negative — `asr.device = "cpu"`; mock автопула; assert NOT entered)
- [x] добавить тест `transcribe_with_timestamps with audio path on mps - wraps in pool + calls cache helper` (`_make_timestamps_asr`)
- [x] добавить тест `transcribe_with_timestamps with numpy array on mps - wraps in pool + calls cache helper`
- [x] добавить тест `transcribe_simple on mps - one pool entry per chunk (N chunks → N enters, N cache helper calls)`
- [x] добавить тест `_transcribe_direct on mps wraps inference in pool and calls cache helper` (импортировать `LiveTranscriber` или замокать `self.asr.model` напрямую; см. `tests/test_live_decomposed.py` как образец)
- [x] прогнать `python -m pytest tests/test_asr.py -v -k "transcribe_ or transcribe_direct"` — новые проверки должны падать (RED)

### Task 4: Обернуть `transcribe_*` методы в `recog/asr.py`

**Files:**
- Modify: `recog/asr.py`

- [x] `transcribe_batch`: добавить `_mps_autoreleasepool(self.device)` в `with`-цепочку (`_model_lock`, `inference_mode`, `_mps_autoreleasepool`); вызвать `self._maybe_empty_mps_cache()` после decode внутри `with`
- [x] `transcribe_with_timestamps` ветка `isinstance(audio, str)`: обернуть `model.transcribe` в `with self._model_lock, _mps_autoreleasepool(self.device):` + `_maybe_empty_mps_cache()`
- [x] `transcribe_with_timestamps` ветка numpy/Tensor: обернуть второй `model.transcribe` аналогично, внутри `try` блока (после `sf.write`)
- [x] `transcribe_simple` chunk loop: обернуть body `with torch.inference_mode(), _mps_autoreleasepool(self.device):` + `_maybe_empty_mps_cache()` после decode (per-chunk pool)
- [x] прогнать тесты Task 3 (только asr-методы) — должны проходить (GREEN)

### Task 5: Обернуть `_transcribe_direct` в `recog/live.py`

**Files:**
- Modify: `recog/live.py`

- [x] импортировать `_mps_autoreleasepool` из `recog.asr` (или через `from .asr import _mps_autoreleasepool`)
- [x] обернуть существующий `with self.asr._model_lock:` блок (lines 466-470) в дополнительный `_mps_autoreleasepool(self.asr.device)`: получится `with self.asr._model_lock, _mps_autoreleasepool(self.asr.device):`
- [x] вызвать `self.asr._maybe_empty_mps_cache()` после decode внутри `with`
- [x] прогнать тест `_transcribe_direct on mps ...` из Task 3 — должен проходить (GREEN)

### Task 6: Smoke + regression — полный test suite

- [x] `python -m compileall recog frontend` — нет синтаксических ошибок
- [x] `python -m pytest tests/test_asr.py -v` — все asr-тесты проходят (31 passed)
- [x] `python -m pytest tests/test_live_decomposed.py tests/test_per_speaker_refine_e2e.py -v` — live-related регрессия (28 passed)
- [x] `python -m pytest tests/ -v` — полный suite зелёный (363 passed, 2 deselected)
- [x] `python mp4_notes.py --help` — CLI грузится без ImportError
- [x] `git diff --stat 5677766..HEAD` — изменения только в четырёх файлах: recog/asr.py, recog/live.py, tests/test_asr.py, docs/plans/2026-04-30-mps-autoreleasepool-leak-fix.md (ADR-0019.md заметка отнесена к Task 7)
- [x] visually review diff — изменения минимальны и по теме: autoreleasepool wrapping всех четырёх hot inference paths (transcribe_simple, transcribe_batch, transcribe_with_timestamps str/ndarray, _transcribe_direct), throttled empty_cache helper, тесты

### Task 7: ADR заметка

**Files:**
- Modify: `adr/0019-stream-refine.md`

- [x] добавить в "Notes" / "Operational concerns" один параграф: ссылка на этот план, краткое описание leak'а (5.7 ГБ Apple Metal objects через 3 суток), упоминание `_mps_autoreleasepool` + throttled `empty_cache` как митигации

### Task 8: [Final] Перенести план в completed

- [x] `mkdir -p docs/plans/completed`
- [x] `git mv docs/plans/2026-04-30-mps-autoreleasepool-leak-fix.md docs/plans/completed/`

## Post-Completion

*Items requiring manual intervention or external systems — no checkboxes, informational only*

**Manual verification (требуется на живом сервисе):**
- `./recog-service.sh restart` после merge
- Сразу после рестарта: `top -l 1 -o mem -pid <new-pid>` — baseline footprint должен быть < 500 МБ
- Через 24 часа uptime: footprint должен быть < 1 ГБ (vs 7+ ГБ раньше)
- `heap <pid> | head -20` — `AGXG16GFamilyCommandBuffer` count должен быть < 50k (vs 2.4M раньше)
- Refine pipeline должен работать: новые сегменты в `meeting/<id>/transcript.jsonl` помечаются `refined: true`
- Mic / fallback-mixed путь: `[live] direct transcribe failed` ошибок не должно появиться в `logs/`

**External system updates:**
- Не требуется — изменения изолированы в `recog/`
- Если в будущем появится Linux/CUDA deploy — fallback ветка `_HAS_AUTORELEASE = False` обеспечит работоспособность

**Risk monitoring:**
- `torch.mps.empty_cache()` блокирует до drain MTLCommandQueue — реальная latency 10-150 мс (не 10-50 как раньше думалось). Срабатывает раз в ~20 с при 5 Hz batch'ах. При cooperative refine abort budget'е (200-500 мс) это в норме, но если в `[PerSpeaker]` p99 latency появятся spike'и → bump `_MPS_EMPTY_CACHE_EVERY` до 500, либо decouple cache release в отдельный utility thread (proper long-term fix).
- Если PyObjC autorelease_pool в будущей версии поменяет API — try/except не покроет AttributeError; добавить `hasattr(objc, "autorelease_pool")` в `_HAS_AUTORELEASE`.
