# ADR-0021: Кросс-платформенный захват звука и сервисный слой (Linux + macOS)

## Status
Accepted (2026-09-03) — вытесняет часть ADR-0010, отвечавшую за захват
системного звука (BlackHole / Multi-Output Device).

## Context

Recog работал только на macOS одной машины:

- Системный звук — Swift-хелпер `sck_audio_capture` (ScreenCaptureKit) либо
  BlackHole; на Linux источника не было вообще.
- ASR выбирал `mps`-или-`cpu` и никогда CUDA (`recog/asr.py`), то есть машина с
  NVIDIA молча работала на CPU.
- Сервисы — четыре закоммиченных launchd-plist с захардкоженными
  `/Users/vkkovalev` и nvm-путём к `node`.
- `setup_venvs.sh` требовал Homebrew и sibling-checkout `../GigaAM`.
- В git лежали arm64-бинарь хелпера и `speakers*.json` с голосовыми
  эмбеддингами реальных людей; дефолтами были личный Tailscale-URL ASR-gateway
  и `/opt/homebrew/bin/ffmpeg`.

Задача: коллеги (Ubuntu/Debian, Arch/Manjaro, macOS) клонируют репозиторий из
внутреннего stash и поднимают **полный** live-пайплайн — системный звук,
микрофон, Chrome-расширение, web UI, сервисы — без правки исходников. При этом
поведение и потребление памяти на существующем Mac не меняются: MPS, FP16,
единственный экземпляр модели под `_model_lock`, `_mps_autoreleasepool`,
частота `_maybe_empty_mps_cache` и объём аудио-буферов остаются как есть.

## Decision

### D1. Системный звук на Linux — `parec`, а захват становится generic subprocess

Swift-хелпер пишет в stdout сырой моно float32 PCM 16 кГц. `parec --raw
--format=float32le --rate=16000 --channels=1 --latency-msec=100
--device=<monitor>` пишет ровно тот же wire-формат. Поэтому вместо второго
класса захвата бывший `ScreenCaptureAudio` обобщён в
`SubprocessPCMCapture(BufferedAudioCapture)`, принимающий `argv`, а бэкенды
различаются только командой (`recog/system_audio.py`).

Почему `parec`, а не альтернативы:

- **`pw-record`** — только PipeWire; `parec` покрывает и чистый PulseAudio, и
  PipeWire через `pipewire-pulse` (который стоит в дистрибутивах по умолчанию),
  то есть один бэкенд вместо двух.
- **`sounddevice` на monitor-устройстве** — PortAudio показывает мониторы
  нестабильно (зависит от ALSA-плагина `pulse`), теряет их при смене default
  sink и потребовал бы второго набора буферов и второй кодовой ветки; выигрыша
  в сравнении с готовым CLI нет.
- **Прямой libpulse через ctypes** — лишняя FFI-зависимость ради того же
  потока.

Разделяемая часть буфера (overlap, re-anchor к wall-clock, кап
`ASR_PENDING_BUFFER_CAP_S`, `get_chunk() -> (chunk, capture_start_ts)`) вынесена
в `BufferedAudioCapture` (`recog/audio_capture.py`) и переиспользуется
микрофонным `SingleDeviceCapture` — общий контракт «1-D моно float32», один
набор аудиоданных, а не два.

### D2. Смена default sink отслеживается явно, а не именем `@DEFAULT_MONITOR@`

`@DEFAULT_MONITOR@` резолвится в конкретный монитор в момент создания потока и
за последующей сменой default sink не следует. Поэтому `PulseAudioCapture` без
override опрашивает `pactl get-default-sink` и **явно переподключает** `parec`
при изменении. Наблюдение принадлежит capture, живёт и умирает вместе с ним,
запросы имеют timeout, кратковременная ошибка `pactl` не рвёт текущий поток
(rate-limited лог + повтор). Плановое переподключение чистит буфер (граница
источников: overlap через разрыв не склеивается, старым сэмплам не
присваиваются новые timestamps) и **не расходует бюджет crash-retry**.
Непустой `RECOG_PULSE_MONITOR_SOURCE` фиксирует источник и выключает слежение.

`--device` обязателен: без него `parec` пишет дефолтный *source*, то есть
микрофон, а не то, что слышно.

### D3. Restart-политика захвата: не сдаваться молча

Прежний цикл выходил из reader-потока на первой же ошибке respawn (а `parec`
именно так и падает, пока перезапускается `pipewire-pulse`): `running`
оставался `True`, `get_chunk()` навсегда отдавал `None`, `start()` блокировал
восстановление — захват был мёртв, а всё вокруг выглядело здоровым. Теперь
respawn идёт с ограниченным backoff (1/2/5/10 с) и **не покидает цикл, пока
`running`**; после `MAX_RESTARTS` захват сдаётся явно (`_dead`,
`running=False`), а `recog/live.py` пишет один отдельный маркер потери
системного захвата. Этот маркер намеренно не переиспользует
`_emit_pcm_transition_marker`: смерть mixed-захвата не должна утверждать
переход PCM→mixed и не должна ставить refine на паузу.

### D4. ASR-устройство: `auto` = cuda > mps > cpu, явное — fail-fast

`RECOG_ASR_DEVICE` (`auto|cuda|mps|cpu`) в `recog/config.py`, резолв в
`recog/asr.py::resolve_device`. Явно запрошенное недоступное устройство —
`RuntimeError` (не молчаливый CPU). Единственное исключение: **auto-**
резолвленный `cuda`, упавший с OOM при загрузке, демотируется в `cpu` — иначе
занятая или маленькая карта загоняет live-сервис в цикл рестартов systemd. При
**явном** `cuda` fallback не делается: человек попросил конкретное устройство.

CPU — жизнеспособный режим, а не деградация: измерено RTF 0.05–0.2
(3-секундный чанк 150–590 мс), поэтому у коллеги без GPU live-режим работает.
`fp16_encoder` не трогали — `gigaam.load_model` включает его для любого
не-CPU устройства, так что CUDA получает fp16 автоматически, а macOS остаётся
ровно в прежнем режиме.

### D5. Сервисы — шаблоны, а не закоммиченные юниты

Юниты рендерятся из `deploy/launchd/*.plist.in` и `deploy/systemd/*.service.in`
подстановкой `@REPO_DIR@ @HOME@ @PYTHON@ @LOG_DIR@ @NODE_BIN@ @EXTRA_PATH@`
(`sed` — `envsubst` на стоковой macOS нет), с проверкой, что не осталось
`@TOKEN@`. Четыре закоммиченных `com.recog.*.plist` удалены из git и
заигнорены: это сгенерированные артефакты с чужим `$HOME`. Верб-логика
вынесена за контракт `svc_*` в `scripts/service/{common,launchd,systemd}.sh`,
`recog-service.sh` выбирает backend по `uname -s`.

На Linux — `systemd --user` с `Restart=always`, `RestartSec=5` и
`StartLimitIntervalSec=0` **в секции `[Unit]`** (в `[Service]` ключ
игнорируется — `Unknown key`, при этом `systemd-analyze --user verify` всё
равно возвращает 0, поэтому тест ассертит вывод, а не код возврата). Это
сохраняет fail-fast-контракт `_setup_meeting_storage`, который до сих пор
опирался на launchd `KeepAlive`: упасть и быть перезапущенным — штатный способ
сообщить о неготовности.

`systemd --user`, а не system-юниты: пайплайну нужны пользовательская сессия
PulseAudio/PipeWire, `$HOME`, Chrome и tmux пользователя; system-юнит потребовал
бы root, `XDG_RUNTIME_DIR`-обвязки и `User=`. Цена — `loginctl enable-linger`,
о котором напоминает `status`.

### D6. `install` не активирует; `start`/`restart` — за гейтом готовности

`svc_install` только рендерит и регистрирует конфигурацию (launchd — файл без
`load`, systemd — файл + `daemon-reload` без `enable --now`). Активация —
отдельный шаг. Это и позволяет `setup.sh` проверить зависимости **после**
установки юнитов, но **до** запуска: неполная установка не поднимает
неработающий пайплайн и, главное, не останавливает уже работающий (ранее
запущенные сервисы явно `svc_restart`-ятся, остальные `svc_start`-уются).
Те же пробы (`scripts/deps.sh`) выполняет `recog-service.sh start|restart`,
поэтому обойти гейт прямым вызовом сервисного скрипта нельзя (осознанный обход —
`RECOG_SKIP_READINESS=1`). Отдельный `svc_restart` нужен потому, что
`daemon-reload` не перезапускает работающий юнит, а `start` активного — no-op.

### D7. Copilot на Linux живёт в собственной transient-unit

Сессия copilot (tmux + Claude) на Linux запускается через `systemd-run --user
--collect --unit=recog-copilot-session` с `Type=oneshot`,
`RemainAfterExit=yes`, `KillMode=control-group` и без `Restart=`. Иначе она
попадала бы в cgroup supervisor'а и умирала вместе с ним при каждом
`systemctl --user restart recog-copilot-supervisor` — а контракт (общий с
macOS) в том, что `restart` действующую сессию сохраняет, а завершают её только
явный Stop и `uninstall`. `Type=oneshot` + `RemainAfterExit=yes` соответствуют
`tmux new-session -d`, который форкает сервер и сразу возвращается. Unit
никогда не устанавливается и не включается, не связан с supervisor через
`PartOf`/`BindsTo`; watchdog по-прежнему проверяет сам tmux, а не `active`
oneshot-юнита. Перед новым spawn'ом runner останавливает прежний unit и ждёт
выгрузки имени — `systemd-run` отказывается переиспользовать загруженное имя.
`KillMode=process` как обход не используется: он оставил бы Claude сиротой.

### D8. Личное — в конфиг, но литералы `ws_origin` остаются

`RECOG_ASR_REMOTE_URL` теряет дефолт (`remote` без URL — `RuntimeError` на
старте, а не тихий локальный режим с другой памятью и латентностью), `ffmpeg`
резолвится из PATH, `speakers*.json` и `sck_audio_capture` убраны из индекса
(файлы остаются локально), `.env.example` получает placeholder вместо
Tailscale-адреса.

Хосты конференций в `recog/ws_origin.py` **не** параметризуются: это буквальное
зеркало `manifest.json` расширения, и тест проверяет их равенство. Чужой хост
всё равно требует правки `manifest.json` и пересборки расширения, поэтому
добавлять его в allowlist через env отдельно от манифеста бессмысленно.
Расширяемость дана отдельным множеством `RECOG_EXTRA_WS_ORIGINS`.

### D9. Одна команда установки, GigaAM из git-пина

`setup.sh` заменяет `setup_venvs.sh`: детект ОС, проверка Python ≥3.12,
таблица found/missing с подсказками apt/pacman/brew, переиспользование venv,
torch/torchaudio с `RECOG_TORCH_INDEX_URL` (на Linux без NVIDIA — CPU-индекс,
иначе PyPI-колесо тянет ~2–3 ГБ CUDA-рантайма), GigaAM всегда из git-пина
(sibling-checkout `../GigaAM` больше не нужен), конфиги из `.example`, сборка
расширения, компиляция macOS-хелпера, подготовка кеша весов.

Пин обязателен: upstream ушёл вперёд, а recog вызывает API декодера и
`word_timestamps` напрямую. Форка нет — `../GigaAM` оказался чистым upstream'ом
без локальных коммитов и правок, поэтому `RECOG_GIGAAM_URL`/`RECOG_GIGAAM_REF`
достаточно и для будущего форка в stash.

Подготовка кеша (`scripts/prefetch_gigaam.py`) — потоковая проверка и докачка
файлов с блочным хешированием и атомарной публикацией, **без**
`gigaam.load_model`/`torch.load`: setup не создаёт временную модель в RAM
(условие приёмки по памяти).

## Consequences

### Positive

- Полный live-пайплайн на Linux: системный звук, микрофон, расширение, web UI,
  сервисы, CUDA.
- Онбординг коллеги — `git clone` + `./setup.sh`; отсутствующие системные
  пакеты называются вместе с командой установки под его пакетный менеджер.
- Неполная установка больше не может остановить работающую: гейт готовности
  стоит между `install` и активацией.
- Один буферный класс вместо двух почти одинаковых, и одна subprocess-реализация
  на оба бэкенда системного звука.
- Смерть системного захвата теперь видна в транскрипте вместо тихой тишины.
- В репозитории не осталось чужих голосовых эмбеддингов, arm64-бинаря и личных
  URL.

### Negative / Risks

- `parec` — внешний CLI: его отсутствие или падение `pipewire-pulse` видно
  только через respawn-логи и, в пределе, маркер потери захвата.
- Опрос `pactl get-default-sink` — polling, а не событие: переключение выхода
  подхватывается с задержкой в один интервал.
- Стало больше движущихся частей в установке (шаблоны, гейт готовности,
  prefetch) — соответственно больше кода, который может сломаться сам.
- `systemd --user` требует `loginctl enable-linger`, иначе сервисы умрут с
  логаутом; на это указывает только подсказка в `status`.
- CPU-fallback при CUDA OOM спасает от рестарт-цикла, но делает деградацию
  тихой (только строка в логе).
- Существующая установка на Mac требует ручного переноса
  `RECOG_ASR_REMOTE_URL` в `.env` до обновления — `.env` никем не редактируется
  автоматически.

### Mitigation

- `constraints/macos.txt` фиксирует проверенный набор версий (torch/torchaudio
  2.8.0, numpy 2.3.5, pyobjc-core 12.1) и применяется во всех pip-этапах
  macOS-установки, чтобы перенос на Linux не обновил ML-стек попутно.
- `tests/test_system_audio.py`, `tests/test_service_templates.py`,
  `tests/test_setup.py`, `tests/test_copilot_supervisor.py` покрывают argv,
  переподключение при смене sink, give-up-путь, рендер обоих бэкендов,
  «install не активирует», блокировку активации при отсутствии зависимости и
  Linux-жизненный цикл copilot.
- Отсутствие регрессии по памяти на macOS подтверждается отдельным
  before/after сравнением по одинаковому сценарию, а не фактом зелёных тестов.

## Alternatives Considered

- **Оставить macOS-only и дать коллегам remote-ASR к общему gateway.** Не
  решает ни системный звук, ни сервисы, ни личные данные в репозитории.
- **Docker-образ вместо нативной установки.** Пайплайну нужны звук
  пользователя, его Chrome-расширение и его tmux/Claude — контейнер здесь
  добавляет проброс сокетов PulseAudio, X/Wayland и GPU вместо того, чтобы
  что-то упростить.
- **Абстракция «AudioBackend» с плагинами.** Бэкендов два, wire-протокол один;
  плагинный слой был бы кодом без второго потребителя.
- **Генерировать юниты питоном при старте сервиса.** Рендер шаблонов `sed` в
  `install` — единственный шаг, который должен работать до создания venv.

## References

- ADR-0010 (вытеснен в части захвата), ADR-0018, ADR-0019, ADR-0020.
- План: `docs/plans/2026-09-03-linux-support-and-team-onboarding.md`.
- `systemd-run`, transient services и `RemainAfterExit`:
  https://github.com/systemd/systemd/blob/main/man/systemd-run.xml
- Выбор конкретного target в PipeWire (почему `@DEFAULT_MONITOR@` не следит за
  сменой sink):
  https://github.com/PipeWire/pipewire/blob/1.4.7/src/modules/module-protocol-pulse/pulse-server.c#L2041
