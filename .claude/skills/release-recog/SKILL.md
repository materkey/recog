---
name: release-recog
description: Publish a new Recog release - pick the version, tag origin/main, wait for the release workflow, check the draft and sign it on the signing Mac, publish it, deploy the landing page, update the Homebrew formula. Use when the user says "release", "publish", "new version", "cut a release", "зарелизь", "выпусти релиз", "сделай релиз", or asks to make the latest main available to Homebrew and archive installations.
disable-model-invocation: true
---

# Release Recog

Publish a new version of Recog for the macOS disk image, Homebrew on macOS
(`materkey/apps/recog`), the apt and pacman repositories and the standalone
archives. The download button on the
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
  `Recog-macOS-arm64.dmg.sha256`, `SHA256SUMS`, `recog.rb`, the versioned
  `recog_X.Y.Z-1_amd64.deb` and `recog-X.Y.Z-1-x86_64.pkg.tar.zst`,
  `linux-packages.sha256`, `Packages`, `Release` and `recog.db`.
- Verify the disk image against its checksum. Mount it read-only and verify
  the Applications link, the Recog.app signature and the embedded release
  version before publishing.
- Sign the update feed and the Linux repository on the signing Mac, the one
  Mac whose login Keychain holds both private keys
  ([Signing the update feed](../../../docs/sparkle-updates.md),
  [Linux package repositories](../../../docs/linux-packages.md)). The keys are
  not on the release workstation: signing there fails with a missing Keychain
  item. The hosts of the signing Mac and of the site live outside the
  repository: load them first with `set -a; . ~/.config/recog/release.env; set +a`.
  From the workstation run
  `python3 scripts/sign_release.py vX.Y.Z <scratch>/signed --upload`. It
  reaches the signing Mac over ssh (host `RECOG_SIGNING_MAC`) and
  signs there in a one-shot launchd job of the user's graphical session,
  because the Keychain does not answer an ssh session; it brings back `appcast.xml`, `InRelease` and `recog.db.sig`, verifies
  them here, uploads them and ends with `linux_repo.py ready`. If the Keychain
  asks for access, the owner allows it on that Mac. **Do not publish until it
  passes.** The installed Finder app's feed, apt and pacman all read the latest
  release: without `appcast.xml` every Sparkle installation's check breaks,
  and without valid `InRelease` and `recog.db.sig` `pacman -Syu` stops as a
  whole on every Arch system that uses the repository.
- Download `SHA256SUMS`, `recog.rb` and the archive of this machine's platform into a
  new scratch directory (`gh release download vX.Y.Z --repo materkey/recog --pattern …`).
- `shasum -a 256 -c` the archive against `SHA256SUMS`; the `sha256` and `version` in
  `recog.rb` must be the macOS archive's hash and `X.Y.Z`.
- Unpack the archive: `recog/recog-release` holds `vX.Y.Z` and `recog/bin/recog --help`
  runs. Do not run its `setup`: that would install the release over this machine.

For a published tag missing the disk image, dispatch `release.yml` from `main`
with input `tag=vX.Y.Z`. This read-only run downloads and verifies that tag's
macOS archive and produces the `recog-macos-dmg` workflow artifact. Check the
image and its checksum before attaching them with `gh release upload`. Keep
the tag and previously published assets intact.

### 5. Publish with release notes

- The generated body is only a compare link. Write the notes to a file in the scratch
  directory:
  - what changed for a user, from the lines added to `CHANGELOG.md`
    (`git diff --unified=0 <last-published>..vX.Y.Z -- CHANGELOG.md`), without the date
    headings, **Action required** items first. Reword nothing into internals; the
    changelog is already written for users. If the range has user-visible commits and
    no changelog lines, say so in the report instead of inventing entries.
  - an `## Update` section: `recog update` for Homebrew and archive
    installations, **Recog → Проверить обновления…** in the Finder app, and
    `sudo apt update && sudo apt upgrade` or `sudo pacman -Syu` for the Linux
    packages;
  - `**Full Changelog**: https://github.com/materkey/recog/compare/<last-published>...vX.Y.Z`
    (the generated link compares with the previous tag, which may be an unpublished draft).
- `gh release edit vX.Y.Z --repo materkey/recog --draft=false --latest --notes-file <file>`
- Verify: `gh api repos/materkey/recog/releases/latest --jq .tag_name` is `vX.Y.Z`.
  Download `releases/latest/download/appcast.xml` anonymously and verify it
  against the public key and the image downloaded from its versioned enclosure.
- `python3 scripts/linux_repo.py verify --tag vX.Y.Z` reads the repository as
  apt and pacman do and must pass. The `Linux repository guard` run of the
  release (`gh run list --repo materkey/recog --workflow release-guard.yml --limit 1`)
  must be green: when it fails it has made an earlier release the latest
  again. Then fix the signatures and `gh release edit vX.Y.Z --latest`.
- Deploy the landing page from the tag right away:
  `scripts/deploy_landing.sh vX.Y.Z` (host and directory from
  `RECOG_SITE_HOST` and `RECOG_SITE_DIR`). The README and the release notes send
  Linux users to its `install.sh` and `recog.asc`, and nothing else publishes
  them. The script switches the site to `deploy/landing` of the tag and checks
  that the site serves the tag's files.
- Where a Linux machine with Docker is at hand, install the release the way a
  user does: in fresh `ubuntu:24.04` and `archlinux` containers, as a user with
  sudo, `curl -fsSL https://recog.vkkovalev.work/install.sh | sh -s -- --no-setup`
  must end with `dpkg-query -W recog` or `pacman -Q recog` at `X.Y.Z-1`
  (an Arch image needs `pacman-key --init` and `pacman -Syu` first: the
  Docker image ships without a keyring, and the installer's
  `pacman-key --lsign-key` fails with "no secret key available to sign with").

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
- If the tap is installed here (`brew tap | grep materkey/apps`): after
  `brew update`, `grep version "$(brew --repository materkey/apps)/Formula/recog.rb"`
  shows `X.Y.Z`. `brew info` refuses a tap the user has not trusted
  (`brew trust`); leave that setting to the user.

### 7. Report

Only after steps 5 and 6 are verified:

```
Release vX.Y.Z published.

CI on the released commit: green
Release workflow: completed (archives for macOS arm64 and Linux x86-64)
Archive check: checksum, version file and --help passed on <platform>
Linux repository: signed, verified through releases/latest/download
Landing: deploy/landing of vX.Y.Z served at https://recog.vkkovalev.work
Latest release: vX.Y.Z
Homebrew formula: X.Y.Z pushed to materkey/homebrew-apps

Release page: https://github.com/materkey/recog/releases/tag/vX.Y.Z
Update: `recog update` (Homebrew and archive installations), apt or pacman (Linux packages)
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
