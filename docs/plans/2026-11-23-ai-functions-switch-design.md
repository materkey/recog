# One switch for the AI functions

Agreed with the owner on 2026-11-23.

## Purpose

Not everyone wants transcripts sent to an outside model. Today only the Copilot can be
switched off; the other paths send the transcript on their own:

| Path | Started by | Switch before this change |
|---|---|---|
| Summary after a conference (`IncrementGenerator::generate_all`) | the bridge, on leave | none |
| Title of a recording without one (`meeting_title_update`) | the meeting storage, on close | none |
| AI Analysis, Ask, Generate Title | a click in the web UI | none |
| Copilot | a click or the meeting autostart | "Copilot включён" |
| Recent segments on the Copilot channel's request | any agent on the local channel | none |

recog cannot tell an outside model from a local one: both providers are the `claude`
and `codex` programs, and `claude` can be pointed at a local model. What recog can
promise is that it does not start either program.

## The setting

One field of `ai-settings.json` (the web UI's state, not `config.toml`: it is the
user's choice, made in the UI and effective at once):

| `access` | Meaning |
|---|---|
| `unasked` | a new installation that has not answered; works as `off` |
| `off` | recog starts neither CLI |
| `manual` | only on a click: AI Analysis, Ask, Generate Title, the Copilot started by hand |
| `automatic` | `manual`, plus the summary and title after each meeting and the Copilot autostart |

What each level allows:

| Path | unasked, off | manual | automatic |
|---|---|---|---|
| Summary after a conference | no | no | yes |
| Title of a recording without one | no | no | yes |
| AI Analysis, Ask, Generate Title | no | yes | yes |
| Copilot started by hand | no | yes | yes |
| Copilot autostart on a meeting | no | no | yes, when `RECOG_COPILOT_AUTOSPAWN` is on |
| Recent segments on the channel's request | no | while the Copilot is on | while the Copilot is on |

The Copilot's own switch stays; the Copilot runs when both allow it.

## New and existing installations

- A file without the field is an installation from before this change and counts as
  `automatic`, so nothing changes for it.
- At startup the live process and the web server settle a missing file once, under
  the store's lock: a meeting of an earlier run (a `meta.json` with `ended_at`) means
  an existing installation → `automatic`, written as a file without the field; none →
  `unasked`. The answer is written, so the next start does not guess again. An
  existing file is never rewritten by this. The current run's own recording has no
  `ended_at` and does not count.
- No file at all (the settling has not run, or failed) counts as `unasked`: a failure
  fails closed.
- Known limitation: a file that carries the field (a new installation, or any level
  chosen in the UI) cannot be read by a build from before this change, which accepts
  exactly its four fields; a file without the field still can.
- An installation restored from a backup, or a new checkout over old data, counts as
  existing: the data is the same person's.
- An existing installation without a single finished recording is asked. Harmless.

## Behaviour

- Every provider is created through `ProviderFactory::create`, which refuses unless the
  level allows a click (`manual` or `automatic`). The automatic paths check for
  `automatic` before they start, so an installation at `manual` neither logs errors
  nor claims a summary version per meeting; a refused Regenerate claims none either.
- The Copilot is on when its switch and the level allow it: the bridge's policy and the
  supervisor's task carry that, so the existing gates (spawn, chat, ticks, lifecycle
  frames, replies) follow. Switching to `off` stops a running session, as the
  Copilot's switch does. Going from `automatic` to `manual` only stops new autostarts:
  the running session is visible and has Stop, and the meeting's end still ends it.
  Below `automatic` nothing starts, kills or replaces a session for a conference; a
  session already running for one keeps running, also through a bridge restart, until
  the meeting ends. Going back to `automatic` during a conference starts its Copilot.
- Known limitation of `manual`: the Copilot cannot be started by hand while a
  conference is on, because the bridge refuses a manual start during any conference
  (`conference_active`), as it already does with `RECOG_COPILOT_AUTOSPAWN=0`. This
  change leaves that as it is.
- The bridge answers the channel's request for recent segments with none while the
  Copilot is off.
- A generation already started finishes: the transcript has already been sent.

## Web UI

- **Промпты и модели** starts with the level: three choices, saved at once.
- While the level is `unasked`, the welcome flow asks it right after the profile
  question; "Позже" leaves it unasked for that browser session. The AI buttons stay
  visible, and a click on one asks the same question first.
- While the level is `off`, the controls that start an AI path are hidden: Ask,
  Generate Title, Recog Copilot, and inside the AI Analysis window the «Новый анализ»
  tab, «Запустить анализ», Regenerate and «Создать сохранённый результат». The AI
  Analysis button itself stays, so saved summaries stay readable.
- The conference page's extension shows the bridge's existing refusal ("Copilot
  выключен в настройках."), which already points to the settings.

## Out of scope

- Remote speech recognition: audio goes to a gateway only when configured explicitly,
  with no default address, to the owner's own server.
- A lock that the web UI cannot lift (an organisation's policy).
- Moving the Copilot autostart out of `.env` into the web UI.

## Documents

`CHANGELOG.md`: new installations start with the AI functions off until answered;
existing ones keep everything, at `automatic`; the new choice in **Промпты и модели**.
README: the analysis "on request" wording matches the levels.
