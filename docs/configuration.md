# Installation configuration

Settings that differ between installations of the same repository and are not
secrets live in one TOML file outside the checkout:

```
~/.config/recog/config.toml
```

`$XDG_CONFIG_HOME/recog/config.toml` when `XDG_CONFIG_HOME` is set.
`RECOG_CONFIG_FILE` names another file (`~/` is expanded, a relative path is
resolved against the repository root); pass it to the services and to the
extension build alike. A missing file means built-in behaviour only.

```toml
# Conference servers beyond the built-in hosts (docs/conference-hosts.md).
[conferences]
jitsi = ["https://meet.example.org"]
matrix = ["https://element.example.org"]

# Source names of old recordings and the platform they were.
[sources.aliases]
old-meet = "jitsi"

# Web UI names of the sources your own importers write; recog names the
# sources it records itself, any other source is shown as stored.
[sources.labels]
phone = "Phone"
```

`deploy/config.example.toml` lists every setting with its explanation. Besides
the tables above it has `[asr]`, `[live]`, `[copilot]`, `[web]` and `[macos]`:
each key is the default of one environment variable the code reads (for example
`[asr] backend` of `RECOG_ASR_BACKEND`, `[web] external_meetings_paths` of
`EXTERNAL_MEETINGS_PATHS`), typed as in the example: `true`/`false` for flags,
numbers for durations and counts, arrays for lists. The variable, from the
process environment or from `.env`, still overrides the file; flags accept
`1/0`, `true/false`, `yes/no` and `on/off`, a list is comma-separated, and an
empty value counts as unset. A malformed value in either place stops startup
with the setting's name. A setting set to a different value in both places is
reported on stderr at every start (`recog: RECOG_COPILOT_AUTOSPAWN from the
environment or .env overrides [copilot] autospawn of config.toml`): delete
the line from `.env`, since the file's value does nothing until then.

`recog/install_config.py` loads `.env`, reads the file, lays the environment
over it once at import and exposes the typed result as `INSTALL_CONFIG`
(`INSTALL_CONFIG.asr.backend`, `INSTALL_CONFIG.copilot.grace_s`, ...): code
reads that, never the environment. Each field declares its variable, type and
default there. `RECOG_CONFIG_FILE` may be set in `.env` too.

Unknown tables or keys are errors, not ignored, so a typo cannot silently drop
a setting. A malformed file stops the services at startup
(`./recog-service.sh start` reports it before activating anything) and fails
the extension build.

Secrets, and the legacy fallbacks `.env.example` still ships, stay in `.env`.
State the web UI writes (`ai-settings.json`,
`user-profile.json`, `privacy.json` under the data directory) is not
configuration and stays where it is.

One parser reads the file, `recog/install_config.py`. The extension build takes
the conference hosts from `python -m recog.install_config`, using
`RECOG_PYTHON`, else `venv-gigaam/bin/python`, else `python3` (3.11 or newer).

`scripts/backup-data.sh` copies the file into its snapshot as `config.toml`.

After a change, rebuild the extension when conference hosts changed
(`npm run build --prefix browser-extension`) and restart the services
(`./recog-service.sh restart`).

## Moving from `.local/conference-hosts.json`

Installations used to keep conference hosts in `.local/conference-hosts.json`
(or the file named by `RECOG_CONFERENCE_HOSTS_FILE`). That file is still read,
with a warning, while `config.toml` does not exist. With both present the
services refuse to start: move `jitsi` and `matrix` into `[conferences]` and
`source_aliases` into `[sources.aliases]`, then delete the old file.
