# Local user profile for Copilot

Status: implemented. Profile persistence, API, onboarding/settings UI, Live/Copilot
updates, backup and data migration are implemented. Existing personal agent-name
assumptions have been removed.

## User flow

1. `./setup.sh` installs Recog and prints the web UI URL as it does today.
   Installation remains noninteractive and works under automation.
2. On the first web UI visit without a saved profile, show one welcome step:
   **Как к вам обращаться?**
   - **Ваше имя** — the name to use for the local participant.
   - **Другие варианты обращения** — optional nicknames, short forms and
     names in other languages, entered as individual values.
   - **Сохранить** and **Позже**.
3. Prefill the name from a nonempty, non-generic `MIC_SPEAKER_NAME` on an
   existing installation, but require Save before persisting a profile.
   Never extract the user's identity from a random transcript participant,
   operating-system account, repository instructions or language-model guess.
4. Save applies the profile immediately and returns to the requested screen.
   Later does not block recording or manual Copilot chat; suppress the welcome
   prompt for the current browser session and retain a small setup link beside
   Copilot. Name-addressed proactive suggestions stay disabled while identity
   is unknown; the other existing proactive triggers retain their behavior.
5. **Настройки → Мой профиль** opens the same form at any time. All browsers
   connecting to this Recog instance see the same saved profile. Editing or
   clearing the profile affects future events, not historical recordings.

Microphone recordings and speaker enrollment are not required for this step:
a name/addressing profile is distinct from a voice embedding database.

## Stored data and precedence

Store `user-profile.json` under `get_data_dir()` (normally `~/.recog`), outside
Git and independent of the checkout's location. The server is the sole writer;
save atomically. Never edit `.env` from the web UI or setup script.

```json
{
  "version": 1,
  "name": "Александр",
  "aliases": ["Саша", "Alex", "Alexander"]
}
```

The canonical name is always part of the effective address set, so users need
not repeat it in aliases. Trim values, drop empty/duplicate aliases and bound
input lengths/counts. Matching is case-insensitive and respects name boundaries;
short substrings must not identify an unrelated participant. Do not generate
or save additional aliases without the user's explicit selection.

Effective display name: explicit `--mic-speaker-name` for the current run,
then a saved profile name, then legacy `MIC_SPEAKER_NAME`, then `Me`.
Saving the profile is the explicit transition from the old environment default
to the new user-facing setting. Before that, existing installations behave as
before. A one-run CLI override does not rewrite the saved profile; include both
the override and profile name in the effective address set when they are real
names. `Me` and empty values are not name-matching identities.

## Wiring and live updates

Use a shared profile resolver for the microphone label and Copilot identity.
Today `LiveTranscriber` can use a CLI override while `KTalkBridge` imports the
module-level `MIC_SPEAKER_NAME` directly; the new design must remove this split.
Pass the effective live identity to the bridge rather than independently
reconstructing it from environment defaults.

Add a small GET/PUT profile API and a bridge update carrying a profile revision.
The web server acknowledges Save only after persistence and reports whether
the live process applied that revision. On a disconnected bridge, retain the
saved profile and show that application to the live session is pending;
reconcile on reconnect. Do not claim immediate application when delivery failed.

Every relevant Copilot event (`meeting_started`, `transcript_tick`,
`copilot_user_message`) carries a complete current identity snapshot:
`local_user_name`, `local_user_aliases`, and `local_user_profile_revision`.
This covers manual sessions without periodic ticks and agent/bridge reconnects.
The channel serializer already JSON-encodes non-string metadata, but its key
allowlist must explicitly forward aliases and revision. The skill treats this
snapshot as the authoritative profile and replaces old aliases when revision
changes; it never assumes names from examples or previous sessions.

Existing transcript rows retain their original speaker label and `is_local`.
New local segments use the effective name after a profile update; all user
attribution continues to use `is_local`/participant identity rather than a
name-only comparison. No service or active Copilot session restart is required.

## Acceptance checks

- Fresh profile: first web visit offers setup; Later leaves recording/chat usable.
- Existing environment name: prefilled but not silently written; `.env` unchanged.
- Save/reload/second browser: one backend profile is shown consistently.
- Profile changes and deletion reach an already running Copilot; removed aliases
  cannot keep triggering name-addressed suggestions.
- Alias lists survive bridge and MCP channel serialization, including Cyrillic.
- Explicit CLI name reaches both live transcript and Copilot; no identity split.
- Ambiguous names or another participant's matching name do not by themselves
  prove a question is directed at the local user. Profile strings are data,
  not instructions to the agent.
- Bridge downtime: saved revision is replayed after reconnect; the UI distinguishes
  saved from applied. Historical meeting metadata/transcripts are byte-preserved.
- Backups include the profile as personal configuration, with the same privacy
  treatment as speaker databases; fresh clones contain no profile.

## Implementation notes

- `recog/user_profile.py`: private atomic store, bounded names/aliases, UUID revision,
  persisted empty tombstones and shared identity resolver.
- `recog/web/user_profile.py`: saved/applied coordinator, persistence/ACK serialization,
  connection-generation guard and bridge reconnect refresh.
- `recog/web/static/user-profile.js`: native accessible dialog, session Later dismissal,
  settings, errors and pending application updates.
- `user_profile_updated` proactively refreshes identity without a chat reply; the
  transcript tool also returns identity for reconnect recovery.
- Readiness checks reject a malformed stored profile before service activation.
  The profile is included in the common data-directory migration set.

Validation includes store/API/runtime tests, deterministic stale-ACK and reconnect
races, frontend tests, and a browser smoke against an isolated real bridge/frontend.
The browser smoke covered welcome, Save, persisted settings, and removal of previous
aliases. Test names were written only under an isolated test data directory.
