---
name: release-recog
description: Publish a new Recog release - pick the version, tag origin/main, wait for the release workflow, check and publish the draft, update the Homebrew formula. Use when the user says "release", "publish", "new version", "cut a release", "зарелизь", "выпусти релиз", "сделай релиз", or asks to make the latest main available to Homebrew and archive installations.
disable-model-invocation: true
---

# Release Recog

Publish a new version of Recog for the macOS disk image, Homebrew on macOS
(`materkey/apps/recog`) and the standalone archives. The download button on the
site points to `releases/latest/download/Recog-macOS-arm64.dmg`, so keep that
asset name the same across releases.
A checkout installation follows `main` and does not need a release.

**Input**: optionally a bump type or an explicit version. If omitted, infer it from the
changes since the last published release.

The version lives only in the tag. `Cargo.toml` stays at its version, `CHANGELOG.md` is
kept by date in the commits themselves, so a release adds no commit to this repository.

## Steps

### 1. Decide what is released

- `git fetch origin --tags`
- Last published release: `gh api repos/materkey/recog/releases/latest --jq .tag_name`.
  Highest tag: `git tag --sort=-v:refname | head -1`. They differ when an earlier
  release stayed a draft; the new version must be above the highest tag, and the
  changes are counted from the last published one, since nobody received the drafts.
- Changes: `git log --oneline <last-published>..origin/main`. None: stop with
  "Nothing to release".
- The release is cut from `origin/main`. If local `main` is ahead of it, stop and ask
  whether to push first: pushing `main` is its own decision with its own checks.
- CI must be green on that exact commit, because the release workflow builds the
  archives and runs no tests:
  `gh run list --repo materkey/recog --workflow CI --branch main --commit "$(git rev-parse origin/main)" --json status,conclusion`.
  Still running: wait with `gh run watch <id> --repo materkey/recog --exit-status`.
  Failed or absent: stop and report.
- Bump from the changes: breaking for an existing installation → major, new
  capability → minor, fixes only → patch.
- Confirm the version with `AskUserQuestion` (suggested one first, alternatives after),
  showing the list of changes it covers.

### 2. Check the runners

The public repository uses GitHub-hosted macOS and Linux runners. Check the
repository visibility and `.github/workflows/release.yml` before checking local
runner availability. Only a private workflow configured for those runners needs
the following checks.

- `gh api repos/materkey/recog/actions/runners --jq '.runners[]|[.name,.status,.busy]|@tsv'`
- A macOS runner and a Linux runner must be `online`. Otherwise report which one is
  down (`docs/ci-runners.md` tells how to bring it back) and do not tag yet.

### 3. Tag and wait for the workflow

- `git tag vX.Y.Z origin/main && git push origin vX.Y.Z`
- The tag starts `Release archives` (`.github/workflows/release.yml`): one archive per
  platform and a disk image on the Mac, then a **draft** with the downloads and checksums.
- Find the run (it appears a few seconds after the push; empty answer: retry in 10 s):
  `gh run list --repo materkey/recog --workflow release.yml --limit 5 --json databaseId,headBranch --jq '.[]|select(.headBranch=="vX.Y.Z")|.databaseId'`
- `gh run watch <id> --repo materkey/recog --exit-status` in the background. With free
  runners it takes about 15 minutes; it took 45 when the runners were busy.
- On failure report the failing job and re-run it (`gh run rerun <id> --failed --repo materkey/recog`).
  Keep the tag. If the `draft` job failed after creating the release, delete that draft
  first (`gh release delete vX.Y.Z --repo materkey/recog --yes` keeps the tag).

### 4. Check the draft before anyone can install it

A published release is what `recog update` installs on the next run, so the draft is
the last place to catch a broken archive.

- `gh release view vX.Y.Z --repo materkey/recog --json isDraft,assets --jq '.isDraft, (.assets[].name)'`:
  a draft containing both versioned tar archives, `Recog-macOS-arm64.dmg`,
  `Recog-macOS-arm64.dmg.sha256`, `SHA256SUMS` and `recog.rb`.
- Verify the disk image against its checksum. Mount it read-only and verify
  the Applications link, the Recog.app signature and the embedded release
  version before publishing.
- Download `SHA256SUMS`, `recog.rb` and the archive of this machine's platform into a
  new scratch directory (`gh release download vX.Y.Z --repo materkey/recog --pattern …`).
- `shasum -a 256 -c` the archive against `SHA256SUMS`; the `sha256` and `version` in
  `recog.rb` must be the macOS archive's hash and `X.Y.Z`.
- Unpack the archive: `recog/recog-release` holds `vX.Y.Z` and `recog/bin/recog --help`
  runs. Do not run its `setup`: that would install the release over this machine.

### 5. Publish with release notes

- The generated body is only a compare link. Write the notes to a file in the scratch
  directory:
  - what changed for a user, from the lines added to `CHANGELOG.md`
    (`git diff --unified=0 <last-published>..vX.Y.Z -- CHANGELOG.md`), without the date
    headings, **Action required** items first. Reword nothing into internals; the
    changelog is already written for users. If the range has user-visible commits and
    no changelog lines, say so in the report instead of inventing entries.
  - an `## Update` section with `recog update`;
  - `**Full Changelog**: https://github.com/materkey/recog/compare/<last-published>...vX.Y.Z`
    (the generated link compares with the previous tag, which may be an unpublished draft).
- `gh release edit vX.Y.Z --repo materkey/recog --draft=false --latest --notes-file <file>`
- Verify: `gh api repos/materkey/recog/releases/latest --jq .tag_name` is `vX.Y.Z`.

### 6. Update the Homebrew formula

Nothing copies the formula into the tap; until this step Homebrew installations stay on
the old version while the archive ones already update.

- The tap is `materkey/homebrew-apps`, checked out at `~/projects/homebrew-apps`
  (clone it there if absent). `git pull --ff-only`.
- Replace `Formula/recog.rb` with the release's own `recog.rb`, byte for byte: its hash
  was computed from the published archive.
- Commit `Update Recog to X.Y.Z` with a body saying what the new version gives its
  users, then `git push origin main`. Publishing the formula is part of the release the
  user asked for.
- If the tap is installed here (`brew tap | grep materkey/apps`):
  `brew update && brew info materkey/apps/recog` shows `X.Y.Z`.

### 7. Report

Only after steps 5 and 6 are verified:

```
Release vX.Y.Z published.

CI on the released commit: green
Release workflow: completed (archives for macOS arm64 and Linux x86-64)
Archive check: checksum, version file and --help passed on <platform>
Latest release: vX.Y.Z
Homebrew formula: X.Y.Z pushed to materkey/homebrew-apps

Release page: https://github.com/materkey/recog/releases/tag/vX.Y.Z
Update: `recog update` (Homebrew and archive installations)
```

List the drafts of earlier versions that are still unpublished, if any, and leave them
to the user.

## Important notes

- **Tag only a commit that is on `origin/main` with green CI.** The tag push is the
  point of no return: the workflow starts at once.
- **Never move or reuse a tag.** The formula and `SHA256SUMS` pin the archive of that
  tag. A broken version stays a draft, and the fix goes out as the next patch version.
- **Do not update or restart this machine's services as part of a release.** A restart
  interrupts a recording in progress; installing is a separate request.
- **macOS permissions.** If the release changes `crates/recog-live/macos/` (the
  launcher, its icon, the capture helper), macOS may ask users for the microphone and
  screen recording again. Say so in the release notes; `CHANGELOG.md` should already
  carry the line.
- Verify that the published downloads and the landing page's disk image link
  work without GitHub credentials.
- `scripts/release.py` builds the archive (`archive`) and renders the formula
  (`formula`); the workflow calls both. Run them locally only to debug the packaging.
