# Signing the Finder update feed

The Finder app checks `https://github.com/materkey/recog/releases/latest/download/appcast.xml`.
Each stable release hosts its own feed and disk image. The feed names the
**versioned** image URL so the signed bytes still match after the next release.
GitHub Actions builds the disk image using the public Ed25519 key in
`deploy/macos/dmg/sparkle-public-key.txt`. Sparkle verifies both the feed
and the disk image before extraction.

## Release signing

The private key stays on the signing Mac: the `com.recog.manager.sparkle`
account of its login Keychain, next to the Linux repository key
([linux-packages.md](linux-packages.md)). The release workstation reaches it
over ssh, at the host named by `RECOG_SIGNING_MAC` (or `--mac`), kept with
the other release settings in `~/.config/recog/release.env` outside the
repository, and signs both in one run:

```sh
python3 scripts/sign_release.py "$TAG" "$SCRATCH/signed" --upload
```

The script downloads the draft's `Recog-macOS-arm64.dmg` and checks it
against the SHA-256 GitHub reports for the asset, copies it with the reviewed
signing scripts to the signing Mac and runs the signing there as a one-shot
launchd job in the user's graphical session (`gui/<uid>`): the Keychain
refuses an ssh session ("User interaction is not allowed"), and the job is
removed afterwards. If the Keychain asks for access, allow it on that Mac.
There
`scripts/sparkle_appcast.py sign` checks the image digest, the embedded runtime
release, the application's feed URL and its public key, then Sparkle generates
and signs the appcast. Only the signed files come back; the workstation
verifies `appcast.xml` against the image and the public key before uploading
it. Without `--upload` the files stay in the output directory.

The signing Mac needs the pinned Sparkle tools in
`~/.local/share/recog-signing/Sparkle-2.10.1` (from `scripts/fetch_sparkle.sh`)
and the Apple command line tools.

Publish the draft only after the feed and both signatures verify. Download the
feed again from the application's stable URL and repeat verification against
the release image (`scripts/sparkle_appcast.py verify`). Leave all previously
published assets intact.

The built-in updater first ships after 0.2.0. An installation of the original
0.2.0 Finder app needs one manual replacement from the next disk image.
Homebrew and archive installations keep their existing update commands.

## Key continuity

Keep a recoverable backup of the signing identity under the owner's control.
Replacing or losing the key without a supported rotation can require a manual
reinstall for existing users. The Sparkle key authenticates updates and is
separate from an Apple Developer ID certificate. See the
[Sparkle signing and key rotation guide](https://sparkle-project.org/documentation/#3-segue-for-security-concerns).
