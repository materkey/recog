# Linux package repositories

apt and pacman install Recog from GitHub Releases. Each release carries the
indexes of its own packages, and both package managers read
`https://github.com/materkey/recog/releases/latest/download/`, so the latest
published release is the repository and nothing else is hosted.

| Asset | Read by | Made by |
| --- | --- | --- |
| `recog_X.Y.Z-1_amd64.deb` | apt (`Filename` in `Packages`) | release workflow |
| `recog-X.Y.Z-1-x86_64.pkg.tar.zst` | pacman (`%FILENAME%` in `recog.db`) | release workflow |
| `Packages`, `Release` | apt (a flat repository, suite `./`) | release workflow |
| `recog.db` | pacman (repository `[recog]`) | release workflow |
| `InRelease` | apt: the clear-signed `Release` | signing Mac |
| `recog.db.sig` | pacman: detached signature of `recog.db` | signing Mac |

`scripts/linux_repo.py index` writes the unsigned indexes next to the packages
(`scripts/build_linux_packages.sh`). The chain is the usual one: the signature
covers `Release`, which holds the SHA-256 of `Packages`, which holds the
SHA-256 of the `.deb`; for pacman the signature covers `recog.db`, which holds
the SHA-256 of the package, so packages need no signatures of their own
(`SigLevel = PackageOptional DatabaseRequired`). The packages have versioned
names because pacman caches downloads by file name.

## Signing a release

The key is an Ed25519 OpenPGP key, fingerprint
`6E66571BB28E6EC3827CD813E2E87FE565091BC0`. Its public half is
`deploy/landing/recog.asc`, served on the landing page as `/recog.asc`; the
private half is the `com.recog.linux-packages` item in the login Keychain of
the signing Mac, the same Mac that signs the Sparkle feed
([sparkle-updates.md](sparkle-updates.md)). It never goes to CI.

1. After the release workflow has made the draft, sign it from the release
   workstation together with the Sparkle feed:

   ```sh
   python3 scripts/sign_release.py "$TAG" "$SCRATCH/signed" --upload
   ```

   `linux_repo.py prepare` (run by it) downloads `Packages`, `Release` and
   `recog.db`, checks each against the SHA-256 GitHub reports for the asset and
   that they describe exactly the draft's `.deb` and Arch package of version
   `$TAG`, and lists the draft's asset digests in `assets.tsv`. On the signing
   Mac, offline, `linux_repo.py sign` repeats those checks against
   `assets.tsv`, signs with the Keychain key and verifies the signatures
   against `deploy/landing/recog.asc`; the workstation verifies them again,
   uploads `InRelease` and `recog.db.sig` and runs
   `python3 scripts/linux_repo.py ready "$TAG"`, which repeats every check on
   the draft's own assets, signatures included.
2. **Publish only after `ready` passes.** `pacman -Syu` synchronises every
   repository or none: a latest release whose `recog.db.sig` is missing or
   invalid stops the whole system upgrade on every Arch system that uses the
   repository (apt only skips Recog with a warning).
3. After publishing, `python3 scripts/linux_repo.py verify --tag "$TAG"` reads
   the repository through `releases/latest/download/` as apt and pacman do and
   checks the signatures and every package byte.

The **Linux repository guard** workflow (`.github/workflows/release-guard.yml`)
runs the same `verify` when a release is published. If the new latest release
fails it, the workflow makes the most recent earlier release whose own
repository verifies the latest one again, and fails so that the owner sees it.

## Upgrades reach each user's services

A package installs Recog in `/usr/lib/recog`, but each user's services run a
copy under `~/.local/share/recog/runtime` that `recog setup` makes. The
package's install and upgrade scripts write the installed version to
`/var/lib/recog/installed-release`; `recog setup` enables the user units
`recog-upgrade.path`, which watches that file, and `recog-upgrade.service`,
which runs `recog setup --no-env` when the package's release differs from
`services-release` in the copy: after an upgrade, and at login for an upgrade
made while the user was away. Setup writes `services-release` only after it
has restarted the services, because it replaces the copy's `recog-release`
before that. A failed run is retried 30 seconds to 10 minutes later: setup
refuses while a call is being recorded, and on Arch the upgrade's pacman hook
re-executes every user's service manager while setup is restarting the
services. `journalctl --user -u recog-upgrade` shows those runs.

## Creating the key

`python3 scripts/linux_repo.py new-key` made the key once: it refuses to
replace an existing Keychain item and writes the public key to
`deploy/landing/recog.asc`. Run it at the signing Mac itself, in its
graphical session: the Keychain refuses an ssh session. Keep a backup of the Keychain item under the
owner's control: the trusted key lives outside the package
(`/etc/apt/keyrings/recog.asc`, the pacman keyring), so after losing or
replacing it every installation has to trust the new key by hand.
