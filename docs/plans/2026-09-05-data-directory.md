# Data directory migration

**Goal:** Keep meetings, logs and all `speakers*.json` outside the checkout in
`~/.recog`, with `RECOG_DATA_DIR` as an explicit override. `.env` stays put.

**Architecture:** One stdlib-only path resolver in `recog/config.py` supplies
Python consumers and shell scripts. Existing installs keep their entire legacy
root until an explicit migration; competing legacy and default roots fail visibly.
Migration uses same-filesystem rename, never merges or overwrites destinations.

**Constraints:** Preserve file bytes and metadata; do not edit user environment
files or speaker databases; commit only when requested and do not push. Keep service working directories
at the code checkout. Stop writers before moving data and re-render units afterwards.

- [x] Test fresh/default/override/legacy/conflicting paths and explicit storage paths.
- [x] Implement shared paths for storage, frontend, enrollment, setup, services and backup.
- [x] Test and implement a dry-run-first migration with collision and cross-device
  preflight, resumable after partial renames, and no data copying or deletion.
- [x] Document migration and run `./scripts/check.sh`.
- [x] Stop services, capture file hashes and inode identities, rename all data,
  compare hashes before starting anything, install rendered units and restart.
- [x] Verify meeting-list and speaker APIs, log destinations and fresh recording paths.

## Validation

- `./scripts/check.sh` after migration: 1133 passed, 1 skipped, 2 deselected.
- Compared SHA-256, inode, mode and mtime for all 10,063 migrated files before
  restarting; all matched. Both environment files also matched.
- Four services running; their eight log descriptors and the live WAV writers
  point under `~/.recog`. The speaker API still returns five speakers, the
  meeting list includes the new recording, and migrated audio playback returns 200.
- Metadata reading resolves WAVs in the current meeting directory ahead of
  historical absolute paths, so playback and enrollment survive the move without
  rewriting stored `meta.json`. Covered with absent and reused old paths.
- No legacy data entries remain.
