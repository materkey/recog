# Стенд воспроизведения: «macOS утаскивает обратно на Space с Chrome»

Симптом: во время KTalk-встречи переключаешься на другой Mac workspace
(kitty), и через какое-то время macOS сам возвращает на Space с Chrome.
Это значит, что «кто-то» активировал окно Chrome. Стенд отвечает на два
вопроса: **когда** это происходит и **какой вектор** это делает.

## Инструменты

- `focus-lab.html` — страница-лаборатория. Каждая кнопка взводит один
  вектор активации с задержкой 7 секунд: жмёшь → уходишь на другой
  Space → вектор стреляет → видно, вернуло тебя или нет. Все события
  (включая focus/blur самого окна) логируются на странице и в консоль.
- `front-app-monitor.sh` — пишет в терминал каждую смену фронтового
  приложения с таймстампом (без Accessibility-разрешений, через
  `lsappinfo`). Момент «дёргания» = непрошеный переход `front app ->
  Google Chrome`.
- Focus guard в расширении (`src/focus_guard.js`) — на реальной
  KTalk-странице глушит `window.focus()` и логирует с таймстампами:
  `window FOCUS/blur`, `visibilitychange`, `fullscreenchange`,
  `requestPictureInPicture`, `alert/confirm/prompt`, `Notification`.

## Протокол A: изоляция вектора (без KTalk)

1. В kitty: `./front-app-monitor.sh`
2. Открыть `focus-lab.html` в Chrome (можно `file://`, для Notification
   лучше `python3 -m http.server` и http://localhost:8000/focus-lab.html).
3. Нажать кнопку вектора → сразу уйти на Space с kitty.
4. Через 7 секунд смотреть: (а) вернуло ли на Chrome, (б) что написал
   монитор, (в) что в логе страницы.
5. Повторить для каждого вектора. `title flash` — контроль, он не должен
   активировать ничего.

Ожидания Chromium для фоновой вкладки без user gesture: `alert/confirm`
— показывают диалог (могут активировать окно!), `fullscreen`/`PiP`/
`window.open` — отклоняются, `window.focus()` — игнорируется браузером
(но на KTalk может сработать из-за sticky activation — поэтому guard).

## Протокол B: ловля на реальной встрече

1. В kitty: `./front-app-monitor.sh`
2. Идёшь на встречу KTalk как обычно, переключаешься на kitty.
3. Когда дёрнуло: смотришь таймстамп перехода `-> Google Chrome` в
   мониторе, затем в консоли KTalk-таба ищешь `[KTalk Recog]` записи
   в ±2 секунды от него.
4. Если рядом есть `suppressed`/`Notification`/`alert` — виновник
   найден, гасим точечно. Если в консоли тишина — активация пришла не
   из страницы (Chrome-native: клик по нотификации, PiP-окно, диалог
   браузера, или вовсе другое приложение перед Chrome) — тогда лечится
   на уровне macOS: System Settings → Desktop & Dock → выключить
   «When switching to an application, switch to a Space with open
   windows for the application».
