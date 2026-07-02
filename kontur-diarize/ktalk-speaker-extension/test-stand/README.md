# Стенд воспроизведения: «macOS утаскивает обратно на Space с Chrome»

> **Разгадка (2026-07-02):** space-yank-trace показал, что через ~0.5с
> после ухода на другой Space Chrome сам вызывает SetFrontProcessWithInfo
> (его собственный conn id). Причина — PiP-окно, которое KTalk открывает
> при visibility=hidden (в консоли это его же «Can't open PIP:
> NotAllowedError», код 7514.js). Открытие PiP активирует Chrome →
> macOS переключает Space. Дёргает «часто, но не всегда», потому что
> PiP без user gesture проходит только пока жива transient activation
> (~5с после последнего взаимодействия с вкладкой).
> Фикс в расширении (focus guard):
> - глушится регистрация mediaSession-хендлера `enterpictureinpicture`
>   (авто-PiP Chrome для видеозвонков);
> - `documentPictureInPicture.requestWindow` и
>   `video.requestPictureInPicture` отклоняются c NotAllowedError, когда
>   документ скрыт — ручной PiP с видимой страницы работает как раньше.
> Дублирующий фикс на уровне Chrome: Settings → Site settings →
> Automatic picture-in-picture → Block для example.ktalk.ru (та же
> настройка стоит и для meet.google.com).

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
- `space-yank-trace.sh` — атрибуция на уровне WindowServer: каждая
  активация приложения печатает `<CONN>[SetFrontProcess]` (кто
  запросил) и `frontmostProcess: ... (App) mainConnectionID: ...`
  (кто стал фронтом). Если при «дёргании» CONN совпадает с
  mainConnectionID самого Chrome — Chrome активировал сам себя
  (страница, расширение с chrome.windows.update, клик по нотификации,
  диалог). Если CONN — это Dock, Space переключила система (жест,
  hot corner, Mission Control). Сначала откалибруйте: кликните на
  Chrome вручную и запомните его CONN.

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

1. В kitty: `./space-yank-trace.sh` (он же покрывает роль
   front-app-monitor, но с атрибуцией). Кликнуть на Chrome один раз —
   откалибровать его CONN.
2. Идёшь на встречу KTalk как обычно, переключаешься на kitty.
3. Когда дёрнуло: смотришь последний `SetFrontProcess` для Chrome.
   - CONN == mainConnectionID Chrome → Chrome активировал сам себя.
     Дальше смотри консоль KTalk-таба (`[KTalk Recog]` в ±2 секунды):
     есть записи — гасим вектор точечно; тишина — виновник другое
     расширение или Chrome-native (следующий шаг: временно выключить
     все расширения кроме KTalk-овского в chrome://extensions и
     воспроизвести снова).
   - CONN == Dock → Space переключила система (жест, hot corner,
     Mission Control, поведение «switch to a Space with open
     windows»). Лечится на уровне macOS: System Settings → Desktop &
     Dock → выключить «When switching to an application, switch to a
     Space with open windows for the application».
