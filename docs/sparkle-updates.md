# Signing the Finder update feed

The Finder app checks `https://github.com/materkey/recog/releases/latest/download/appcast.xml`.
Each stable release hosts its own feed and disk image. The feed names the
**versioned** image URL so the signed bytes still match after the next release.
GitHub Actions builds the disk image using the public Ed25519 key in
`deploy/macos/dmg/sparkle-public-key.txt`. Sparkle verifies both the feed
and the disk image before extraction.

## Release signing

1. Check the draft was built by the release workflow for the intended tagged
   commit. Download the exact `Recog-macOS-arm64.dmg` asset and note its
   GitHub-reported SHA-256 digest. Transfer that image to the trusted signing
   Mac. Use the reviewed release scripts from this repository.
2. On that Mac, use the pinned Sparkle tools installed by
   `scripts/fetch_sparkle.sh /path/to/Sparkle`. The signing identity is the
   `com.recog.manager.sparkle` account in the login Keychain. Use the local
   graphical login if Keychain requires confirmation. The private key stays
   on the Mac. The signing command checks the archive digest, the embedded
   runtime release, the application's feed URL and its public key first.

   ```sh
   python3 scripts/sparkle_appcast.py sign "$TAG" Recog-macOS-arm64.dmg appcast.xml \
     --sdk /path/to/Sparkle --sha256 "$GITHUB_ASSET_SHA256"
   ```

   Supply the 64 hexadecimal digits of the digest without the `sha256:`
   prefix. Sparkle generates and signs the appcast from the disk image.
3. Transfer only `appcast.xml` back to the release workstation. On a Mac,
   verification needs the public key and the Apple command line tools:

   ```sh
   python3 scripts/sparkle_appcast.py verify "$TAG" Recog-macOS-arm64.dmg appcast.xml
   gh release upload "$TAG" appcast.xml --repo materkey/recog
   ```

4. Publish the draft only after the feed and both signatures verify. Download
   the feed again from the application's stable URL and repeat verification
   against the release image. Leave all previously published assets intact.

The built-in updater first ships after 0.2.0. An installation of the original
0.2.0 Finder app needs one manual replacement from the next disk image.
Homebrew and archive installations keep their existing update commands.

## Key continuity

Keep a recoverable backup of the signing identity under the owner's control.
Replacing or losing the key without a supported rotation can require a manual
reinstall for existing users. The Sparkle key authenticates updates and is
separate from an Apple Developer ID certificate. See the
[Sparkle signing and key rotation guide](https://sparkle-project.org/documentation/#3-segue-for-security-concerns).
