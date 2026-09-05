# ADR-0018: Per-speaker ASR pipeline для KTalk-встреч

## Status
Accepted

## Context

До этого изменения live-транскрипция KTalk-встреч работала через mixed-path:
системный звук захватывался одним каналом (BlackHole или ScreenCaptureKit),
GigaAM транскрибировал смешанный поток, а атрибуция по спикеру строилась
ретроспективно через `get_dominant_speaker_for_period()` по сигналам speaker-state
из Chrome-расширения (ADR-0010, ADR-0007).

### Проблема
Когда два участника говорят одновременно (overlap), mixed ASR отдаёт один
сегмент со словами обоих говорящих; dominant-speaker attribution вынужденно
приписывает его одному из них, слова второго теряются или склеиваются
с текстом первого. На длинных встречах это даёт цепочки неправильно
атрибутированных реплик и ломает downstream AI-саммари (ADR-0013).

### Что подтвердили исследования
- `tests/e2e/research/` (коммит BASE_REVISION) — реальные overlap'ы в KTalk
  воспроизводимо дают одну mixed-строку вместо двух.
- `scripts/benchmarks/bench_gigaam_rtf.py` (коммит BASE_REVISION) — GigaAM на батче B=8
  даёт ~60× realtime; CPU-бюджет свободно держит 5–8 параллельных
  per-speaker потоков.
- KTalk (Jitsi) на уровне браузера отдаёт per-participant `MediaStream`
  через внутренний API (`conference._audioTrack$.value.jitsiTrack.stream`),
  тот же селектор уже используется в `audio-analyzer.js:92-112`.

## Decision

Переключить KTalk-путь на **per-speaker PCM transport**: Chrome-расширение
аттачит `AudioWorkletNode` к каждому remote `MediaStream`, downsample'ит
48 kHz → 16 kHz Int16 PCM, шлёт чанки в `ktalk_bridge` по WS. Python-сторона
батчит чанки с разных pid'ов и гоняет через общий GigaAM в batched forward.

### Locked decisions (см. docs/plans/2026-04-22-per-speaker-asr-pipeline.md)

| # | Решение | Значение |
|---|---|---|
| D1 | PCM encoding | base64 внутри JSON (localhost, парсинг уже в `_process_message`) |
| D2 | Feature flag | env `RECOG_PCM_TRANSPORT` (default `0`), helper `pcm_transport_enabled()` |
| D3 | `speaker_name` в фрейме | включён прямо в PCM-чанк (живое имя на момент захвата) |
| D4 | Downsampler | `AudioWorkletNode` 3-tap low-pass + decimation (fallback `ScriptProcessorNode` допустим) |
| D5 | Chunk cadence | payload 3300 ms, stride 3000 ms |
| D6 | Batch window | 200 ms, max batch 8 |
| D7 | Chunk overlap | 300 ms (производная D5) |
| D8 | Mid-meeting WS-disconnect | watchdog 30 s → fallback на mixed-path с gap-маркером |
| D9 | Local mic | остаётся через `sounddevice`, не через worklet |
| D10 | ECAPA cross-session link | out of scope |

### Server-authoritative gate

Один источник правды — `RECOG_PCM_TRANSPORT` на Python-стороне. Bridge на
WS-connect шлёт расширению `{"type": "pcm_transport_enabled", "enabled": bool}`.
Extension аттачит worklet'ы только при `enabled: true`; отдельный popup-toggle
намеренно не введён, чтобы избежать матрицы silent-mismatch (popup-off +
env-on = muted meeting; popup-on + env-off = wasted uplink).

### Watchdog fallback

`PerSpeakerIngest` ведёт таймер последнего принятого PCM-чанка. Если в
активной KTalk-встрече тишина >30 s (MV3 worker eviction, extension reload,
WS flap), ingest выставляет `_degraded=True`. В этом режиме `_process_chunk`
переключается на существующий mixed-path с dominant-speaker attribution,
помечая записи `source="fallback_mixed"` и вставляя в транскрипт явный
gap-маркер `[PCM silence >30s — switched to mixed fallback]`. При возврате
PCM — recovery-маркер `[PCM recovered — back to per-speaker]`.

## Implementation

Разбито на 6 атомарных коммитов в ветке `per-speaker-asr-pipeline`:

1. `pcm: add protocol scaffold + config flag for per-speaker transport` — `recog/config.py` константы, `recog/ktalk_bridge.py` парсер `pcm_chunk`, handshake `pcm_transport_enabled`.
2. `extension: add AudioWorklet PCM transport per participant` — `src/audio-worklet/downsampler.js`, `src/pcm_transport.js`, интеграция в `src/platforms/ktalk.js`, manifest bump.
3. `asr: per-speaker ingest + batched transcribe_batch in ASRModule` — `recog/per_speaker.py` (ring buffer, reordering, batch collector, watchdog), `ASRModule.transcribe_batch`.
4. `live: wire per-speaker PCM ingest behind RECOG_PCM_TRANSPORT flag` — `LiveTranscriber._setup_per_speaker_ingest`, generation-guard в batch callback, fallback-переключение в `_process_chunk`.
5. `e2e: add overlap attribution test for per-speaker pipeline` — `tests/e2e/test_per_speaker_overlap.py` + degraded-mode сценарий.
6. `docs: ADR-0018 + rollout default for per-speaker ASR` — этот ADR, обновлённые `CLAUDE.md` и `browser-extension/README.md`.

### Ключевые файлы

- `recog/config.py` — константы `PCM_*` + `pcm_transport_enabled()`.
- `recog/per_speaker.py` — `PerSpeakerIngest` (ring buffer, batcher, watchdog).
- `recog/asr.py` — `ASRModule.transcribe_batch`.
- `recog/live.py` — `LiveTranscriber` с per-speaker ingest + generation-guard.
- `recog/ktalk_bridge.py` — `PcmChunk` dataclass, `set_pcm_chunk_callback`, handshake.
- `browser-extension/src/audio-worklet/downsampler.js` — worklet.
- `browser-extension/src/pcm_transport.js` — attach/detach,
  локальный `AudioContext({sampleRate: 48000})`.

## Consequences

### Positive

- Overlap решён: два одновременно говорящих дают два раздельных transcript-сегмента с правильной атрибуцией.
- Атрибуция теперь detection-free для remote-участников: `speaker_name` живёт прямо в PCM-фрейме, `get_dominant_speaker_for_period()` не дёргается для PCM-пути.
- Пайплайн устойчив к MV3 worker eviction и WS-flap'у: watchdog 30 s + fallback на mixed-path с явным gap-маркером, не тихое muting.
- Атомарный rollback: `RECOG_PCM_TRANSPORT=0` + restart — bridge на следующем handshake выключает PCM на всех подключённых расширениях, popup не трогаем.

### Negative

- Jitsi internal API (`_audioTrack$.value.jitsiTrack.stream`) — зависимость на непубличный контракт KTalk. Риск разделяется с существующим `audio-analyzer.js`, так что новый не добавляется.
- AudioWorklet + webpack — новая интеграция в проекте, возможен регресс при обновлениях webpack/manifest V3. Снижено fallback'ом на `ScriptProcessorNode` в рамках той же итерации.
- Base64-JSON для PCM — ~33 % накладные расходы на bandwidth. Для localhost и 5 участников (~160 KB/s) пренебрежимо; бинарный frame — follow-up, если профайл покажет заметную стоимость парсинга.

### Mitigation

- `recog/per_speaker.py` несёт watchdog + drain на `_meeting_generation` bump.
- E2E покрытие: `tests/e2e/test_per_speaker_overlap.py` (overlap + degraded-mode).
- Раскатка за feature flag — в первом релизе `RECOG_PCM_TRANSPORT=0` остаётся default'ом; флип на `1` производится после ручного smoke на 15+ мин встрече (см. Task 6.4/6.5 в plan'е).

## Alternatives Considered

1. **Улучшенная server-side diarization** (pyannote-community v3.1). Отклонено: ADR-0006 и ADR-0007 показали, что qualitatively overlap-случаи не решаются кластеризацией post-hoc, пока ASR видит mixed chunk.
2. **VAD-гейтинг в расширении** (не шлём чанки, когда no-speech). Отклонено для MVP — добавляет поверхность для багов (false-silence); вернёмся как follow-up поверх рабочего PCM-пути.
3. **Унификация local mic в PCM pipe**. Отклонено (D9): host-tab не отдаёт local mic как remote track, унификация = лишняя работа без выгоды.
4. **Remote ASR worker на Z270XX**. Отклонено: бенч показал запас 30× на локальной машине, инфраструктурная сложность не оправдана.
5. **Бинарный WS-frame вместо base64 JSON**. Отклонено для MVP (D1), оставлено как follow-up при профайл-индикации.

## References

- Plan: `docs/plans/2026-04-22-per-speaker-asr-pipeline.md`
- Design doc: `docs/plans/2026-04-22-per-speaker-asr-pipeline-design.md`
- OpenSpec (параллельная формулировка): `openspec/changes/per-speaker-asr-pipeline/`
- Research: `tests/e2e/research/README.md` (commit BASE_REVISION)
- Bench: `scripts/benchmarks/bench_gigaam_rtf.py`, `/tmp/bench_gigaam_rtf.json` (commit BASE_REVISION)
- Prior ADRs: ADR-0010 (live-режим), ADR-0011 (meeting storage), ADR-0013 (AI-анализ)
- Prior plan: `docs/plans/2026-02-19-ktalk-diarization-optimization-design.md`
