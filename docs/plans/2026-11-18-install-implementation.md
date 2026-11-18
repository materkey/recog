# Installation and first public release

Implementation order for the [installation design](2026-11-10-install-ux-design.md).
Each change keeps the current installation entry points working until their
replacements pass the same checks.

1. Remove the service script as the checkout marker. Make Copilot tools optional
   for setup and service readiness. Check a missing audio dependency still blocks
   activation.
2. Render the four service definitions with `recog-install`. Compare both
   backends byte for byte with the shell runner. Launch foreground Copilot using
   the supervisor's arguments and the caller's terminal.
3. Gate capture in a fresh live unit on membership in any supported conference.
   Keep the recording mode on updates. Test idle, join, leave and owner disconnect
   with the real bridge and storage and simulated devices.
4. Move readiness, service activation, status and logs into Rust. Keep stop and
   uninstall available when the data root is conflicted. Test with isolated unit
   directories and stand-in service managers. Migrate backup and data relocation
   against the existing collision, privacy and resume cases.
5. Implement `cargo setup`, staged binary installation and `recog update`. Check
   conference state before pulling or replacing installed files. Exercise dry run,
   repeated setup, failed build/configuration, and first installation. Remove each
   legacy script only after its callers and tests use the replacement.
6. Build tagged archives with program resources and the prebuilt Chrome extension.
   Install into stable paths, apply configured conference hosts locally, and guide
   manual extension loading and reload. Test install and upgrade without a checkout.
7. Guide the macOS grants through the installed Recog launcher and the optional
   Zoom helper. Report readiness in the terminal and web interface. Verify on a
   fresh Mac through the first transcript and an upgrade. Check Linux user services
   on Z270XX.
8. Publish the source and platform archives in `materkey/recog`, and the formula
   with verified release checksums in `materkey/homebrew-apps`. Before visibility
   changes, detach personal runners from the public repository. External changes
   run on GitHub's machines; private builders take only reviewed revisions.
