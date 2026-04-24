# ADR-0019: Stream-refine — tier-2 rewrite per-speaker live-транскриптов

## Status
Accepted (2026-04-24)

## Context

Per-speaker ASR pipeline (ADR-0018) даёт live-транскрипцию через 3-s чанки GigaAM.
Grid-search на такой конфигурации (`experiments/2026-04-23-gigaam-grid.md`)
показал avg WER ~33 % на take-2 benchmark'е, при том что на 20-s окнах с overlap
1–2 s GigaAM достигает 23–25 %. Модели не хватает контекста — бóльшая часть
ошибок сидит на границах 3-s чанков.

### Развилка

- **Option A**: поднять live `chunk_duration` с 3 s до 10–20 s. Минус:
  first-paint вырастает с ~3 s до ~10 s — видимая UX-регрессия.
- **Option B** (принято): оставить live-путь как есть, добавить параллельный
  refine-pass с 20-s контекстом и word-level timestamps; после refine переписать
  `text` именно у тех live-сегментов, которые попали в окно.

Архитектурное условие Option B — word-level timestamps, чтобы выравнивать
refined-текст обратно на live-сегменты без Levenshtein-эвристик. Upstream GigaAM
PR #65 (`d427e5a`) закрыл это требование, изменение зависит от соответствующего
мёрджа (plus PR #67 `a755fec` для batched RNN-T decoder).

### Constraints

- Не регрессировать first-paint live-пути (уже user-visible).
- Сохранить `segment_id` стабильным — downstream (AI-саммари ADR-0013,
  anchor-response, playback offsets) построены на нём.
- Одна машина, один shared GigaAM (~1.6 GB RSS); второй экземпляр не помещается
  на M1 Pro 16 GB без swap.
- Feature flag `RECOG_PCM_REFINE` default `0`; флип — после operational validation
  в реальных встречах.

## Decision

Добавлен tier-2 контур: background `RefineWorker` с shared GigaAM lock,
per-pid rolling PCM ring, word-timestamp-alignment, inline JSONL rewrite через
существующий `MeetingStorage.update_segments`, WS-broadcast `transcript_refine`
в webapp и extension overlay с visual tier promotion.

### Key decisions (D1–D12 из `openspec/changes/per-speaker-stream-refine/design.md`)

**D1. 1:1 соответствие live ↔ refined сегментов.** Каждый refine-pass переписывает
`text` у не более чем одного live-сегмента на каждый `segment_id`, попавший в
окно. `segment_id`, `ts`, `speaker`, `audio_offset_ms`, `cont` неизменны.
Разведены два clock'а: `audio_offset_ms` остаётся для playback/enrollment (mixed
wav), новый `pid_audio_ms_start/_end` — per-pid монотонный счётчик для alignment.
Alternative N→1 / N→M coalesce отклонён: ломает downstream ID-контракт.

**D2. In-memory ring buffer на pid, не `audio_system.wav`.** `PerSpeakerIngest`
держит `deque` Int16-сэмплов на каждый активный pid с capacity ≈
`PCM_REFINE_WINDOW_S + PCM_CHUNK_DURATION_S ≈ 23 s`. Refine над mixed-каналом
вернул бы overlap-гарбл, ради устранения которого и строился per-speaker
pipeline. RAM ~6 MB при 5 pids — тривиально.

**D3. Gap-driven trigger + monolog fallback.** Refine запускается либо через
`SPEAKER_TURN_GAP = 3 s` тишины (граница реплики — семантически цельный кусок),
либо через `PCM_REFINE_MONOLOG_CAP_S = 15 s` непрерывной речи (монолог не должен
starve'ить refine). Окно — 20 s. `SPEAKER_TURN_GAP` переиспользует константу из
`is_continuation` / UI-границ — один источник правды для "конец реплики".

**D4. JSONL: inline rewrite + `refined: bool`.** `MeetingStorage.update_segments`
расширен опциональным `text`; `TranscriptSegment` получил `refined: bool = False`.
Refine пишет через тот же hardened путь (read-rewrite-write под `_segment_lock`),
что и `_retroactively_correct_unknown_speakers`. Alternative с `superseded_by`
или append-only `refined.jsonl` отклонён — две source-of-truth на одну
логическую реплику.

**D5. UI: tier-1 / tier-2 lifecycle.** Webapp рендерит tier-1 (opacity 0.6 +
dashed accent) до refine, on `transcript_refine` переходит к tier-2 (opacity 1.0,
solid border). Extension overlay делает plain fade-swap без tier-стилей
(пространства у виджета мало). Молчаливая замена отклонена — пользователь уже
прочитал первую версию и не вернётся.

**D6. Shared GigaAM + mutex + live-priority.** Один экземпляр GigaAM,
`threading.RLock` на модель. Refine берёт lock перед inference; live выставляет
`live_pending` флаг, refine-декодер опрашивает его между RNN-T шагами через
`make_abortable_decode` hook (cooperative abort). Live wait ≤ ~187 ms (спайк в
`experiments/2026-04-24-refine-abort-spike.md`). RAM doubling на M1 Pro 16 GB
невозможен, mutex + cooperative abort — компромисс.

**D7. Fallback_mixed = refine off.** Когда `PerSpeakerIngest._degraded == True`
(PCM silence watchdog trip из ADR-0018), `RefineWorker.pause()` дропает очередь и
блокирует новые submissions. Per-speaker аудио в fallback-режиме отсутствует —
refine'ить нечего. На recovery — `resume()`. Специального UX-сигнала не
добавляется, `[PCM silence → mixed fallback]` marker уже в транскрипте.

**D8. Alignment edge cases.** Пустой список слов post-alignment → сегмент
остаётся tier-1 (counter инкрементится). Идентичный текст → no-op, нет
broadcast, нет flip. Subset/superset → emit (tier-2 = "verified with context").
Meeting switch между dispatch и apply → drop (`_meeting_generation` snapshot
pattern, как в `_on_pcm_batch`). Коллизия с retroactive speaker correction —
field-disjoint, commutes под `_segment_lock`.

**D9. Out-of-order across pids.** Refines с разных pid'ов могут приходить в
произвольном порядке. `update_segments` идемпотентен по `id` — ordering не
требуется для корректности. Tier promotion стабилен вне зависимости от
порядка прихода.

**D10. Webapp transport — bridge-push, не JSONL tail.** `frontend/server.py`
подписывается на `ktalk_bridge` через WS-клиент (default `ws://localhost:8766`,
overridable через `RECOG_BRIDGE_WS_URL`) и форвардит `transcript_refine`
клиентам на `ws://127.0.0.1:9080`. JSONL tail watcher остаётся для append-событий
при cold-start replay, но не участвует в refine propagation — `os.replace()`
от `update_segments` ломает strictly-forward `file.seek()` в существующем
watcher. Это latent bug retroactive-speaker-correction (тот же путь); push-канал
чинит оба кейса. Alternative с re-read после inode change отклонён как O(N) +
дубликаты сегментов.

**D11. State-ownership map.** Чёткое разделение владения:
`PerSpeakerIngest` держит per-pid ring + trigger counters + `get_refine_candidate()`.
`LiveTranscriber` держит `_pcm_live_segments`, `_pcm_prev_words`, `_pcm_pid_state`
под `_state_lock` — мост между live и refine. `RefineWorker` держит очередь и
thread, принимает `transcribe_fn` и `apply_fn` через конструктор, не импортирует
`LiveTranscriber`. `MeetingStorage` держит JSONL-I/O. `KTalkBridge` держит
WS egress (`send_refine`). Narrow surface: три callable + один accessor
(`get_pid_audio_ms`).

**D12. Runtime guard rails.** Sanity filter в `_apply_refine`: отклонить refine,
если `abs(len(new.split()) - len(live.split())) > max(3, len(live.split()))` или
`Levenshtein(new, live) > max(len(live), len(new)) * 0.8`. Counter
`refine.rejected_sanity` виден через `/api/pcm-diagnostics`. Runtime toggle:
`POST /api/pcm-diagnostics {"refine_enabled": false}` вызывает
`RefineWorker.pause()` без restart'а сервиса.

## Benchmark results (см. `experiments/2026-04-24-refine-wer.md`)

Take-2 fixtures (per-pid WAVs, два трека ~48 s, 92 reference-слов каждый,
trim 17–65 s, device MPS):

| wav | segs | refined | live WER | refined WER | Δ |
|---|---:|---:|---:|---:|---:|
| `d9e189e5__Example.wav` | 16 | 7 | 37.0 % | 32.6 % | −4.3 % |
| `8ee8e2d9__Example2.wav` | 16 | 7 | 35.9 % | 32.6 % | −3.3 % |
| **avg** |  |  | **36.4 %** | **32.6 %** | **−3.8 %** |

Latency micro-bench (4 parallel pids × 30 s PCM, production cadence, chunk=3 s,
MPS):

| config | n | p50 | p95 | max | mean |
|---|---:|---:|---:|---:|---:|
| `refine=0` | 40 | 358.3 ms | 459.9 ms | 460.2 ms | 360.8 ms |
| `refine=1` | 40 | 382.6 ms | 483.3 ms | 483.8 ms | 366.3 ms |
| **Δ** |  | **+24.3 ms** | **+23.4 ms** |  |  |

Acceptance gates:

- WER improvement direction (Δ < 0): **PASS** на обоих треках.
- WER absolute ≤ 25 % хотя бы на одном треке: **MISSED** (оба пришли к 32.6 %).
- Latency p95 regression ≤ 100 ms: **PASS** (+23.4 ms).

Рекомендация: `RECOG_PCM_REFINE` остаётся default `0` до валидации на реальных
встречах. Synthetic fixtures бедны на turn-boundary triggers (только 7 из 16
сегментов попадают в refine-окно за 48 s); в реальных встречах refine должен
срабатывать чаще, так как `SPEAKER_TURN_GAP` — доминирующий триггер.

## Implementation

Разбито на 4 ralphex-плана в ветке `gigaam-upstream-mps`:

1. `docs/plans/2026-04-24-per-speaker-refine-backend.md` — upstream GigaAM merge
   (word-timestamps + batched decoder), MPS backend, backend-пайплайн refine'а
   (worker, ring buffer, alignment).
2. `docs/plans/2026-04-24-per-speaker-refine-transport-ui.md` — JSONL schema +
   `transcript_refine` WS, webapp tier-1/tier-2 render, extension overlay swap.
3. `docs/plans/2026-04-24-per-speaker-refine-rollout.md` (этот план) — fallback
   integration, тесты, бенчмарки, ADR-0019, CLAUDE.md, `.env.example`.

### Ключевые файлы

- `recog/refine.py` — `RefineWorker`, `RefineTask`, `RefineResult`,
  `make_abortable_decode`.
- `recog/per_speaker.py` — `_pcm_rings`, `_pid_audio_ms`, `_silence_ms`,
  `_continuous_speech_ms`, `get_refine_candidate()`.
- `recog/live.py` — `_pcm_live_segments`, `_pcm_segment_texts`, `_apply_refine`,
  pause/resume wiring на `_degraded` transitions.
- `recog/asr.py` — `transcribe_with_timestamps`, `_model_lock` (RLock — refine
  берёт снаружи, `transcribe_with_timestamps` берёт внутри → no deadlock).
- `recog/config.py` — `PCM_REFINE_WINDOW_S`, `PCM_REFINE_MONOLOG_CAP_S`,
  `pcm_refine_enabled()` (двойной gate: `RECOG_PCM_TRANSPORT=1` +
  `RECOG_PCM_REFINE=1`).
- `meeting_storage.py` — `TranscriptSegment.refined`, `update_segments(text=)`.
- `ktalk_bridge.py` — `send_refine()`, `transcript_refine` WS type.
- `frontend/server.py` + `frontend/bridge_subscriber.py` — WS-клиент к bridge'у
  (default `ws://localhost:8766`, overridable через `RECOG_BRIDGE_WS_URL`).
- `scripts/refine_benchmark.py` — WER + latency micro-bench.

## Consequences

### Positive

- WER улучшение −3.8 % avg на synthetic fixtures'ах; на реальных встречах
  ожидается больше из-за более частых turn-boundaries в единицу времени.
- Граничные ошибки исправляются (bench показал `"умирает за"` →
  `"Умирает зайчик мой"`).
- `segment_id` стабилен — downstream (AI-summary, anchor, playback) не трогается.
- Atomic rollback: `RECOG_PCM_REFINE=0` + restart = refine-контур no-op.
- Runtime kill-switch без restart'а через
  `POST /api/pcm-diagnostics {"refine_enabled": false}`.
- `RefineWorker` паузится во fallback_mixed, pipeline не ломается.

### Negative

- +24 ms p95 first-paint latency под refine load (в рамках ≤ 100 ms бюджета).
- WER ≤ 25 % целевой порог не достигнут на synthetic fixtures — default-flip
  оставлен пользователю после operational validation.
- RefineWorker при длительных монологах генерит дополнительный inference-load
  каждые 15 s; на M1 Pro терпимо, на более слабых машинах может потребовать
  увеличения `PCM_REFINE_MONOLOG_CAP_S`.

### Mitigation

- Feature flag default `0`; operational validation перед default-flip.
- Sanity filter в `_apply_refine` (D12) отбрасывает catastrophic decoder drift.
- Cooperative abort гарантирует, что live никогда не ждёт больше ~187 ms.
- `RECOG_BRIDGE_WS_URL` overridable для нетипичных setup'ов.

## Alternatives Considered

1. **Option A: bump live chunk_duration до 10–20 s.** Отклонено — видимая
   first-paint регрессия 3 s → 10 s без возможности скрыть.
2. **Streaming / partial hypotheses из RNN-T.** Отклонено — decoder-internal,
   конфликтует с tier-1/tier-2 моделью.
3. **Два GigaAM инстанса (live + refine).** Отклонено — ~3.2 GB RSS, swap на
   M1 Pro 16 GB.
4. **`superseded_by` schema / append-only `refined.jsonl`.** Отклонено —
   две source-of-truth на одну реплику, ломает readers.
5. **JSONL tail watchdog для refine broadcast.** Отклонено — `os.replace`
   ломает strictly-forward seek; push-канал из bridge чинит оба кейса
   (refine + retroactive speaker correction).
6. **N→1 / N→M coalesce live-сегментов в один refined.** Отклонено — ломает
   `segment_id` контракт downstream'а.
7. **ONNX / CoreML export.** Отклонено — вне scope'а; MPS backend дал
   достаточный throughput headroom для refine.

## References

- **OpenSpec change** (source of truth для детального дизайна):
  `openspec/changes/per-speaker-stream-refine/` — `proposal.md`,
  `design.md` (D1–D12), `tasks.md`.
- Plans:
  - `docs/plans/2026-04-24-per-speaker-refine-backend.md`
  - `docs/plans/2026-04-24-per-speaker-refine-transport-ui.md`
  - `docs/plans/2026-04-24-per-speaker-refine-rollout.md`
- Experiments:
  - `experiments/2026-04-23-gigaam-grid.md` — grid-search baseline.
  - `experiments/2026-04-24-mps-batch.md` — MPS bit-identity + throughput.
  - `experiments/2026-04-24-refine-abort-spike.md` — cooperative abort spike.
  - `experiments/2026-04-24-refine-wer.md` — acceptance benchmark (WER + latency).
- Bench script: `scripts/refine_benchmark.py`.
- Upstream GigaAM: `salute-developers/GigaAM` через `6e4b027` (PR #65 `d427e5a` +
  PR #67 `a755fec`).
- Prior ADRs: ADR-0010 (live-режим), ADR-0011 (meeting storage), ADR-0013
  (AI-анализ), ADR-0018 (per-speaker ASR pipeline).
