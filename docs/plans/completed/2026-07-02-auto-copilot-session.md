# Auto-spawn Copilot Session (meeting-triggered headless Claude)

## Overview

Claude-копайлот сейчас запускается вручную (`./recog-service.sh claude` в терминале). Этот план добавляет
супервизор-демон, который сам поднимает Claude Code сессию в detached tmux при `meeting_started`,
гасит её при `meeting_ended` (с grace-периодом на реконнекты), следит за её здоровьем через
screen-scrape watchdog и доводит статус (`working / online / blocked / offline`) до чипа в
Copilot-табе расширения. Ручной шаг исчезает; о смерти/зависании сессии видно прямо во встрече.

Lifecycle-модель — «по триггеру» (вариант B из explore-фазы): свежая сессия на каждую встречу,
ноль расхода вне встреч, холодный старт ~6–10 с (измерен спайком).

## Context (from discovery)

Все ключевые механизмы проверены живыми спайками (2026-07-01):

- **`claude -p` невозможен**: `src/cli/print.ts` не парсит `--dangerously-load-development-channels`,
  а `channel_enable` требует marketplace-plugin. Единственный путь — интерактивный TUI в PTY.
- **tmux end-to-end доказан**: detached tmux (`-L recog-copilot`) + scrubbed env → claude 2.1.197
  получил channel push через демон и ответил с корректным `request_id`.
- **Диалог dev-channels давится tmux-нативно**: poll `capture-pane` → футер виден в отрендеренной
  сетке (Ink-эскейпы `\x1b[1C` не мешают) → `send-keys 1`. expect больше не нужен.
- **`--strict-mcp-config --mcp-config '{"mcpServers":{"copilot":{"type":"http","url":"http://127.0.0.1:8767/mcp"}}}'`**:
  сессия видит ровно built-ins + copilot (4 tools), не тянет 9 user-MCP (дешевле и надёжнее);
  запись copilot в `~/.claude.json` исчезла — ручной запуск сегодня сломан, флаги чинят и его.
- **Keychain под launchd работает**: OAuth-токены в login keychain читаются из launchd-процесса
  без промпта (проверено `launchctl submit`-пробой).
- **herdr отвергнут**: его blocked-детекция «deliberately strict» — незнакомые диалоги показывает
  как `idle` (проверено спайком: dev-channels диалог и `/model` оба `idle`). Свой grep по футерам
  диалогов покрывает и незнакомые.
- **Флапы**: `meeting_ended` стреляет мгновенно при падении WS расширения
  (`_end_meeting_on_last_extension_gone`); rejoin воскрешает **тот же** `meeting_id`
  (`meeting_storage._find_same_day_meeting`, `meeting_storage.py:396-402`); `meeting_started`
  дедуплицирован по смене `current_meeting_path`.
- **Демон видит MCP-сессии**: `mcp.session_manager._server_instances`
  (`recog/copilot_channel.py:199-204`) — сейчас используется только для push.
- **Прецеденты паттернов**: `recog/web/bridge_subscriber.py` (WS-клиент bridge с backoff),
  `com.recog.copilot-channel.plist` (launchd-демон), `copilot_status`-фреймы bridge↔extension
  (`ktalk_bridge.py:1510-1640`, `ui.js:1404-1871`).
- Extension-тесты: `npm test` (jest) в `browser-extension/`;
  Python-тесты: `python -m pytest tests/ -v`.

## Development Approach

- **testing approach**: Regular (код, затем тесты — в рамках одной задачи)
- complete each task fully before moving to the next
- make small, focused changes
- **CRITICAL: every task MUST include new/updated tests** for code changes in that task
  - tests are not optional - they are a required part of the checklist
  - именование по факту репо: pytest — `test_snake_case` (given-when-then допустим в docstring),
    jest — плоские описательные `test('...')` как в существующем `copilot.test.js`
  - tests cover both success and error scenarios
- **CRITICAL: all tests must pass before starting next task** - no exceptions
- **CRITICAL: update this plan file when scope changes during implementation**
- run tests after each change
- maintain backward compatibility: ручной `./recog-service.sh claude` остаётся рабочим (и чинится)

## Testing Strategy

- **unit tests**: pytest для Python (`tests/`), jest для extension (`npm test`)
- супервизор тестируется с фейковым bridge (WS-сервер в тесте, как в `test_bridge_subscriber.py`)
  и фейковым tmux-драйвером (инъекция subprocess-раннера)
- классификация экрана watchdog'ом — table-driven тесты на фикстурах реальных экранов
  (снятых спайком: dev-channels диалог, `/model` диалог, REPL prompt, спиннер)
- e2e (реальный tmux + реальный claude) — только ручная проверка в Post-Completion,
  в CI/юнитах не запускается

## Progress Tracking

- mark completed items with `[x]` immediately when done
- add newly discovered tasks with ➕ prefix
- document issues/blockers with ⚠️ prefix
- update plan if implementation deviates from original scope

## Solution Overview

```
launchd ──► com.recog.copilot-supervisor.plist
                │
                ▼
   recog/copilot_supervisor.py  (asyncio, один процесс)
     ├── BridgeClient: WS ──► ktalk_bridge :8766
     │     • supervisor_hello → bridge отвечает meeting_state (реконсиляция)
     │     • слушает meeting_started / meeting_ended
     │     • шлёт copilot_agent_status {status, meeting_id}
     ├── TmuxRunner: subprocess tmux -L recog-copilot
     │     • spawn: new-session -d -s copilot -c <repo> 'claude --agent copilot-session
     │              --strict-mcp-config --mcp-config <json>
     │              --dangerously-load-development-channels server:copilot'
     │     • kill-session / has-session / send-keys / capture-pane
     └── Watchdog (каждые ~2 с, пока сессия должна жить):
           • классифицирует экран: working | online | blocked | offline
           • известный dev-channels диалог → авто send-keys "1"
           • статус в bridge: на смене + heartbeat ~5 с (свежесть для merge)
           • spawning дольше 60 с → blocked (fail-visible)
           • crash mid-meeting → respawn с backoff (≤3 попыток/встречу)

   recog/copilot_channel.py (существующий демон)
     └── ➕ poll-task: len(_server_instances) 0↔n transitions
         → фрейм claude_sessions {count} в bridge

   ktalk_bridge.py
     └── ➕ merge: copilot_agent_status (от супервизора) + claude_sessions (от демона)
         → effective status → broadcast `copilot_status/agent_state` расширениям
         (offline, если супервизор молчит/offline ИЛИ sessions==0 при активной встрече)

   extension ui.js
     └── ➕ чип в Copilot-табе: 🟢 online / 🟡 working / 🔴 blocked / ⚫ offline
```

Жизненный цикл (state machine супервизора):

```
IDLE ──meeting_started(A)──► SPAWNING ──watchdog видит online/working──► RUNNING(A)
RUNNING(A) ──meeting_ended(A)──► GRACE(A, 90s таймер)
GRACE(A) ──meeting_started(A)──► RUNNING(A)      (флап: тот же id — сессия живёт)
GRACE(A) ──meeting_started(B)──► kill + SPAWNING(B)
GRACE(A) ──таймер истёк──► kill + IDLE
RUNNING(A) ──meeting_started(B)──► kill + SPAWNING(B)   (back-to-back встречи)
RUNNING(A) ──tmux сессия умерла──► SPAWNING(A) с backoff (respawn)
```

Key design decisions:

- **D1 — tmux, не herdr**: собственный watchdog покрывает незнакомые диалоги, herdr — нет
  (проверено). Выделенный сокет `-L recog-copilot` изолирует от личного tmux и даёт
  детерминированное окружение (env супервизора из plist).
- **D2 — супервизор спавнит claude напрямую**, не через `recog-service.sh claude`: ему нужны
  детерминированные флаги и tmux-нативная обработка диалога. Скрипт остаётся для ручного
  запуска/дебага и получает те же `--strict-mcp-config --mcp-config` (фикс сломанной регистрации).
- **D3 — grace 90 с** (env `RECOG_COPILOT_GRACE_S`): kill-таймер на `meeting_ended`; отмена при
  `meeting_started` того же id. Тот же id гарантирован same-day regroup'ом в meeting_storage.
- **D4 — merge статуса в bridge**: у статуса два автора (супервизор — экран, демон — MCP-сессии);
  сведение в одном месте (bridge), где уже есть инфраструктура broadcast и тесты.
- **D5 — kill без граций**: `tmux kill-session` (SIGHUP). Прецедент: live-сервис умирает по
  SIGKILL и сессии закрываются как `stale_session` — приемлемо.
- **D6 — master switch** `RECOG_COPILOT_AUTOSPAWN=1|0` (default `1`) в env супервизора: выключение
  автоспавна без удаления сервиса.
- **D7 — фильтра встреч нет**: копайлот на каждую KTalk-встречу (симметрично always-on
  per-speaker pipeline).

## Technical Details

- Новые типы фреймов bridge WS:
  - `supervisor_hello` (супервизор → bridge): переводит сокет в отдельный set (по образцу
    `copilot_channel_hello`, `ktalk_bridge.py:2317`); bridge отвечает
    `meeting_state {active: bool, meeting_id, meeting_title}` для реконсиляции.
    **`active` вычисляется НЕ из `current_meeting_path`** (он никогда не None —
    `on_ktalk_leave` сразу создаёт orphan, `meeting_storage.py:428-447`), а как:
    `meta["source"] == "ktalk"` И есть живой extension-клиент (`_connected_clients` непуст).
  - **Роутинг lifecycle-фреймов**: `meeting_started` / `meeting_ended` сейчас уходят в
    `_copilot_channel_clients` (`_broadcast_to_channel_clients`) и расширениям
    (`_broadcast_message`) — emit-сайты `ktalk_bridge.py:1701, 1996, 2113`, каждый
    hand-roll'ит рассылку сам (disconnect-сайт шлёт только channel-клиентам — расширений
    там уже нет, broadcast в пустой set безвреден). Вместо добавления третьей отправки в
    каждый сайт — один helper `_emit_lifecycle(frame)` (fan-out: channel-клиенты +
    расширения + `_supervisor_clients`; ошибки отправки супервизору глотаются с логом,
    чтобы не ронять lifecycle-обработчики на transcription-пути), и все три сайта
    переводятся на него. Иначе супервизор никогда не проснётся, а четвёртый emit-сайт в
    будущем молча его пропустит (юнит-тесты с фейковым bridge этого не поймают — нужен
    тест на реальном bridge).
  - `copilot_agent_status {status, meeting_id}` (супервизор → bridge), status ∈
    `spawning|online|working|blocked|offline`.
  - `claude_sessions {count | null}` (демон → bridge, уже есть канал
    `_copilot_channel_clients`); `null` = unknown (приватный атрибут SDK недоступен) —
    merge не трактует как 0. Потребляется ТОЛЬКО bridge-merge'ем для чипа; ретрансляции
    супервизору нет (v1).
  - `copilot_status {status: "agent_state", agent_state: ...}` (bridge → extension) — расширение
    существующего типа `copilot_status`, чтобы не плодить новый обработчик с нуля.
- Классификация экрана (порядок важен, первое совпадение):
  1. нет tmux-сессии → `offline`
  2. `Enter to confirm` / `Esc to cancel` / `❯ 1.` → `blocked` (+ если матчится маркер
     dev-channels `local development` → авто-`send-keys 1`, состояние не репортится как blocked)
  3. `esc to interrupt` → `working`
  4. prompt `❯` в нижней части → `online`
  5. иначе → `spawning` (переходное)
- Spawn env: наследуется от супервизора (plist задаёт `PATH` c `~/.local/bin` для claude,
  `HOME`, `TERM=xterm-256color`).
- MCP-connectivity («TUI жив, но копайлот не подключился к демону») в v1 НЕ триггерит respawn:
  она видна в чипе через bridge-merge (`sessions==0` при активной встрече → `offline`).
  Спавн-верификация на стороне супервизора сознательно выкинута: count глобальный и ручная
  сессия давала бы false positive (см. Known limitations).

## What Goes Where

- **Implementation Steps**: код, тесты, plist, скрипты — всё в этом репо.
- **Post-Completion**: ручная e2e-проверка с реальной встречей, перезагрузка расширения в Chrome.

## Implementation Steps

### Task 1: Supervisor skeleton — bridge client + lifecycle state machine

**Files:**
- Create: `recog/copilot_supervisor.py`
- Create: `tests/test_copilot_supervisor.py`

- [x] каркас `copilot_supervisor.py`: argparse (`--bridge-url`, default `ws://127.0.0.1:8766`),
      reconnect-loop с экспоненциальным backoff (по образцу `recog/web/bridge_subscriber.py`),
      `supervisor_hello` при коннекте
- [x] state machine `SessionLifecycle` (чистый класс без I/O): состояния
      `IDLE/SPAWNING/RUNNING/GRACE`, входы `meeting_started(id)/meeting_ended(id)/timer/session_died`,
      выходы-команды `spawn(id)/kill/none` — по диаграмме из Solution Overview
- [x] grace-таймер (`RECOG_COPILOT_GRACE_S`, default 90) и master switch
      (`RECOG_COPILOT_AUTOSPAWN`, default on) в обвязке
- [x] write tests: state machine table-driven — флап тем же id (GRACE→RUNNING без kill),
      смена встречи из GRACE и из RUNNING (kill+spawn), истечение grace (kill), autospawn=0 (no-op)
- [x] write tests: hello + реакция на `meeting_state` при реконсиляции — три ветки:
      **сессия есть + встречи нет → синтетический `meeting_ended` → RUNNING→GRACE
      (НЕ мгновенный kill)** — после рестарта bridge `active=false` временно
      (расширение ещё не переслало `meeting_join`), grace даёт ему время вернуть
      ту же встречу, same-id флап (D3) сохраняет сессию;
      сессии нет + встреча активна → spawn;
      **сессия жива + встреча активна → adopt (no-op, без respawn)** — рестарт супервизора
      посреди встречи не должен убивать живой копайлот
- [x] run tests - must pass before next task

### Task 2: TmuxRunner — spawn/kill/inspect через выделенный сокет

**Files:**
- Modify: `recog/copilot_supervisor.py`
- Modify: `tests/test_copilot_supervisor.py`

- [x] `TmuxRunner` с инъектируемым subprocess-раннером: `spawn()` (new-session -d, socket
      `-L recog-copilot`, cwd репо, команда claude с флагами из D2/Context), `kill()`,
      `alive()` (has-session), `send_keys()`, `capture()` (capture-pane -p)
- [x] сборка команды claude: `--agent copilot-session --strict-mcp-config --mcp-config <json>
      --dangerously-load-development-channels server:copilot`; URL демона из env
      (`RECOG_COPILOT_MCP_URL`, default `http://127.0.0.1:8767/mcp`)
- [x] подключить runner к выходам state machine (spawn/kill), respawn с backoff
      (≤3 попыток на встречу, интервалы 5/15/30 с); **spawn гейтится на `alive()`** —
      если сессия с таким именем уже жива, adopt вместо `new-session`
      (дубликат имени в tmux — ошибка, которая зря съедала бы respawn-backoff)
- [x] write tests: сборка командной строки (флаги, экранирование JSON), spawn/kill вызывают
      правильные tmux-аргументы, respawn-backoff исчерпывается после 3 неудач,
      spawn при живой сессии не вызывает new-session
- [x] run tests - must pass before next task

### Task 3: Watchdog — классификация экрана, авто-диалог, статус-события

**Files:**
- Modify: `recog/copilot_supervisor.py`
- Modify: `tests/test_copilot_supervisor.py`

- [x] `classify_screen(text) -> AgentState` по правилам из Technical Details (порядок фиксирован)
- [x] watchdog-цикл (~2 с, только пока lifecycle ожидает живую сессию): classify → при
      dev-channels диалоге авто-`send-keys 1` → слать `copilot_agent_status` в bridge
      при смене состояния И повторять текущий статус каждые ~5 с (идемпотентный
      heartbeat — по нему merge из Task 4 меряет свежесть; ">10 с молчания" = два
      пропущенных heartbeat) → `session_died` в state machine при `offline`
- [x] spawn-deadline: если classify даёт `spawning` дольше 60 с подряд — репортить
      `copilot_agent_status = blocked` вместо `spawning` (fail-visible: незнакомый
      стартовый экран не должен вечно висеть 🟡; respawn не триггерится — он бы не помог;
      60 с — модульная константа, не env)
- [x] write tests: classify_screen на фикстурах (dev-channels диалог, `/model` диалог,
      REPL prompt, спиннер `esc to interrupt`, пустой экран)
- [x] write tests: авто-dismiss шлёт "1" ровно один раз на диалог; статус-фрейм уходит на
      смене состояния, а при стабильном состоянии повторяется как heartbeat (~5 с);
      session_died триггерит respawn; spawning дольше deadline → blocked
- [x] run tests - must pass before next task

### Task 4: Bridge — supervisor_hello, meeting_state, merge и broadcast статуса

**Files:**
- Modify: `recog/ktalk_bridge.py`
- Modify: `tests/test_ktalk_bridge.py`

- [x] обработчик `supervisor_hello`: перевод сокета в `_supervisor_clients` (по образцу
      `copilot_channel_hello`), ответ `meeting_state`; `active` = `meta["source"]=="ktalk"`
      И `_connected_clients` непуст (НЕ `current_meeting_path` — см. Technical Details)
- [x] **helper `_emit_lifecycle(frame)`** (fan-out: channel-клиенты + расширения +
      `_supervisor_clients`; send-ошибки супервизору глотаются с логом) и перевод трёх
      emit-сайтов (`ktalk_bridge.py:1701, 1996, 2113`) на него — без этого супервизор
      не получает триггеры вообще
- [x] приём `copilot_agent_status` от супервизора и `claude_sessions` от channel-клиента;
      хранение последних значений
- [x] merge-правило (D4): effective = `offline` если supervisor offline/молчит>10 с (два
      пропущенных heartbeat из Task 3) ИЛИ (известный sessions==0 (не null) при активной
      встрече и статусе не spawning, и это условие держится ≥10 с — дебаунс на
      poll-интервал демона, чтобы выход из spawning до первого `claude_sessions` не
      флапал чип в ⚫), иначе статус супервизора; broadcast расширениям
      `copilot_status {status: "agent_state", ...}` на каждом изменении effective-статуса
      (heartbeat без смены статуса наружу не транслируется)
- [x] write tests: **зарегистрированный supervisor-клиент реального bridge получает
      `meeting_started`/`meeting_ended` со всех трёх emit-путей** (meeting_join-переход,
      meeting_leave, last-extension-gone)
- [x] write tests: hello→meeting_state — активная ktalk-встреча → active=true;
      **orphan-встреча (после leave) → active=false**; ktalk-meta без extension-клиентов →
      active=false
- [x] write tests: merge-таблица (supervisor online + sessions 0 устойчиво >10 с → offline;
      working + 1 → working; молчание >10 с → offline;
      spawning→online при sessions==0 младше 10 с → НЕ offline — нет флапа на старте;
      sessions=null (unknown) при активной встрече → статус супервизора, не offline),
      broadcast только на изменении effective-статуса
- [x] run tests - must pass before next task

### Task 5: Демон copilot_channel — репорт количества MCP-сессий

**Files:**
- Modify: `recog/copilot_channel.py`
- Modify: `tests/test_copilot_channel.py`

- [x] poll-task (каждые 5 с): count из `getattr(session_manager, "_server_instances", None)`;
      если атрибут отсутствует (SDK переименовал приватное поле) — один громкий лог и
      `claude_sessions {count: null}` (unknown, НЕ 0 — merge не должен показать уверенный ⚫
      из-за рефакторинга SDK); на изменении count — фрейм `claude_sessions {count}` через
      существующий `BridgeLink.send`
- [x] отправка текущего count сразу после (ре)коннекта к bridge (иначе bridge после рестарта
      не знает состояние до первого перехода)
- [x] write tests: переход 0→1 и 1→0 шлёт фрейм, отсутствие изменений не шлёт,
      повторная отправка после reconnect; отсутствие `_server_instances` →
      `count: null` + однократный лог
- [x] run tests - must pass before next task

### Task 6: Extension — статус-чип агента в Copilot-табе

**Files:**
- Modify: `browser-extension/src/ui.js`
- Modify: `browser-extension/src/copilot.test.js`

- [x] обработка `copilot_status {status: "agent_state"}`: чип у заголовка Copilot-таба,
      состояния 🟢 online / 🟡 working / 🔴 blocked / ⚫ offline (+ spawning как working-вариант);
      **отдельный DOM-элемент**, не `.ktalk-copilot-status` — существующий индикатор
      (`updateCopilotStatusIndicator`, `ui.js:~1414`) остаётся про channel/per-request и
      тоже умеет «working…», визуально не смешивать (разные подписи/позиции)
- [x] начальное состояние `⚫ offline` до первого фрейма; **сброс — по `agent_state: offline`
      от bridge-merge, не по `meeting_ended`**: last-extension-gone путь
      (`ktalk_bridge.py:1701`) эмитит meeting_ended когда расширений уже нет — фрейм до
      расширения не дойдёт, и чип завис бы; merge же надёжно доводит offline при следующем
      подключении
- [x] существующая per-request индикация («Жду ответа Claude…» / «Claude думает…»,
      `ui.js:1404-1871`) не трогается — чип независим
- [x] write tests (jest): рендер чипа по каждому состоянию, переходы, сброс по agent_state
      offline, незнакомое agent_state не ломает обработчик
- [x] `npm run build` — пересобрать `injected.js`
- [x] run tests (`npm test`) - must pass before next task

### Task 7: launchd-интеграция и фикс ручного запуска

**Files:**
- Create: `com.recog.copilot-supervisor.plist`
- Modify: `recog-service.sh`
- Modify: `.env.example`

- [x] plist по образцу `com.recog.copilot-channel.plist`: `ProgramArguments` →
      `venv-gigaam/bin/python3 -m recog.copilot_supervisor`, `PATH` с `/opt/homebrew/bin` и
      `$HOME/.local/bin` (claude), `KeepAlive`, логи в `logs/copilot-supervisor.{stdout,stderr}.log`
- [x] `recog-service.sh`: супервизор в install/uninstall/start/stop/restart/status,
      `restart-supervisor`, `logs-supervisor`
- [x] `cmd_claude`: добавить `--strict-mcp-config --mcp-config <json>` (фикс исчезнувшей
      регистрации copilot MCP для ручного запуска); expect-блок оставить как есть
      (JSON завёрнут в Tcl-скобки `{$mcp_config}` — bash подставляет, Tcl снимает один
      уровень скобок, кавычки внутри литеральны; проверено симуляцией Tcl-парсинга)
- [x] `.env.example`: `RECOG_COPILOT_AUTOSPAWN`, `RECOG_COPILOT_GRACE_S`, `RECOG_COPILOT_MCP_URL`
- [x] write tests: n/a для plist/shell — вместо тестов: `bash -n recog-service.sh`,
      `plutil -lint com.recog.copilot-supervisor.plist`, `python -m compileall recog`
- [x] run tests - must pass before next task

### Task 8: Verify acceptance criteria

- [x] verify: супервизор поднимает сессию на meeting_started и гасит после grace на meeting_ended
      (юнит-уровень: полный сценарий через фейковый bridge + фейковый tmux)
      — `test_lifecycle_full_meeting_cycle` + `test_grace_expiry_kills_session` (fake tmux)
      + `test_supervisor_hello_then_meeting_state_spawns` (fake bridge + fake tmux)
- [x] verify: **реальный** bridge доставляет lifecycle-фреймы supervisor-клиенту
      (тесты Task 4 — против маскировки фейковым bridge)
      — `test_real_bridge_delivers_meeting_started/ended_on_leave/on_last_extension_gone_to_supervisor`
- [x] verify: флап (ended→started тот же id в пределах grace) не убивает сессию
      — `test_lifecycle_flap_preserves_session` + `test_flap_within_grace_cancels_kill`
- [x] verify: рестарт bridge посреди встречи не убивает сессию — reconcile уводит в GRACE,
      rejoin расширения в пределах grace возвращает RUNNING (тот же meeting_id)
      — `test_reconcile_live_session_no_meeting_goes_grace_not_kill`
      + `test_reconcile_adopts_live_session_mid_meeting`
- [x] verify: реконсиляция — orphan-встреча не считается активной (не спавнит между встречами)
      — `test_meeting_state_orphan_after_leave_is_inactive` + `test_reconcile_orphan_session_grace_then_kill`
- [x] verify: статус-цепочка супервизор→bridge→extension покрыта тестами на каждом звене
      — супервизор→bridge: `test_send_agent_status_uses_connected_ws`, `test_watchdog_reports_on_change_and_heartbeat`;
      bridge merge/broadcast: `test_merge_*`, `test_copilot_agent_status_frame_updates_and_broadcasts`;
      extension: `copilot.test.js` agent_state chip тесты (8 шт.)
- [x] verify: ручной `./recog-service.sh claude` содержит --strict-mcp-config (grep) — строки 388, 425
- [x] run full test suite: `python -m pytest tests/ -v` — 542 passed, 2 deselected
- [x] run extension tests: `cd browser-extension && npm test` — 123 passed
- [x] `python -m compileall recog frontend` — OK

### Task 9: [Final] Update documentation

- [x] CLAUDE.md: секция macOS Services (+супервизор), новые env vars, краткое описание
      auto-copilot архитектуры (по образцу существующих секций)
- [x] README.md: упоминание автозапуска копайлота, если README описывает сервисы
- [x] move this plan to `docs/plans/completed/`

## Post-Completion

**Manual verification** (реальное окружение, не автоматизируется):
- `./recog-service.sh install && ./recog-service.sh start` → войти в тестовую KTalk-встречу →
  чип в Copilot-табе должен пройти ⚫→🟡(spawning)→🟢 за ~10-15 с; задать вопрос в табе —
  получить ответ; выйти из встречи → чип ⚫, tmux-сессия исчезает через grace
- Проверка флапа: перезагрузить вкладку KTalk посреди встречи — сессия копайлота должна пережить;
  то же для `./recog-service.sh restart` (рестарт bridge) посреди встречи — reconcile должен
  увести сессию в GRACE, а не убить её
- Проверка respawn: `tmux -L recog-copilot kill-session -t =copilot` посреди встречи — супервизор
  должен переподнять сессию, чип пройти через ⚫→🟢
- Первый запуск под настоящим launchd закрывает остаточный риск spike'а (scrubbed env ≈ launchd,
  keychain проверен, но полный boot под launchd — нет)
- Перезагрузить расширение в Chrome (`chrome://extensions` → Reload) после build

**External system updates**:
- нет (всё локально)

**Известные ограничения v1**:
- один копайлот на машину (одна встреча за раз — соответствует single-meeting ownership bridge)
- стоимость: один initialPrompt-turn (opus, lean context) на каждую встречу
- незнакомый стартовый экран claude после апдейта → `blocked` в чипе: сразу, если матчится
  футер диалога (правило 2), иначе через spawn-deadline (60 с в `spawning`); respawn не
  поможет — лечится вручную (`tmux -L recog-copilot attach -t =copilot` для
  meeting-driven сессии, `-t =copilot-manual` для manual)
- позитивные маркеры классификации (`esc to interrupt`, prompt `❯`) заморожены на claude
  2.1.197: их дрейф после апдейта деградирует статус в правдоподобно-неверный
  (working→online) и не ловится фикстурными тестами — принято для v1; после апдейта claude
  переснять фикстуры экранов и прогнать classify-тесты
- `claude_sessions` — глобальный счётчик MCP-сессий демона: параллельная ручная сессия
  (`./recog-service.sh claude`) маскирует в чипе умершую авто-сессию (merge покажет online).
  Приемлемо: ручная сессия и есть работающий копайлот
- «TUI жив, но MCP не подключился» не триггерит respawn (спавн-верификация выкинута из v1) —
  состояние видно в чипе как `offline`, лечится вручную; если станет частым, вернуть
  верификацию с идентификацией сессий, а не count
