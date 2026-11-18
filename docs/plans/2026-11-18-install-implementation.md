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
8. Keep release archives in the private source repository. Publish the formula in
   `materkey/homebrew-apps` once installation from a private release is verified.

## Acceptance evidence, 2026-11-18

- The native checkout bootstrap was exercised with a build handover, dry run,
  failed configuration/build, optional Copilot dependencies and an active
  conference. Service rendering matches frozen shell output for all eight units.
- The macOS release archive installed into an empty temporary home, applied
  custom extension hosts, and preserved the launcher on repeated setup. Homebrew
  installed the public formula and fetched the matching private archive with
  the installing user’s GitHub login. An anonymous request returned 404.
- On Z270XX, the Linux x86-64 release archive for v0.1.2 ran a real two-participant
  Jitsi call in an isolated network. The installed unpacked extension opened
  capture on join, displayed the first transcript and closed capture on leave.
  Separate Russian clips played through a virtual microphone and the system
  output produced distinct local and remote transcript rows. Both audio files
  stopped growing after leave. The default microphone regression this exercise
  found is covered in `crates/recog-live/tests/live_policy.rs`.
- The Mac acceptance host upgraded from the Python installation and kept its
  archive and recording mode. All four Rust services and the web readiness route
  responded. The installed launcher saw the microphone grant after the interactive
  request. The system audio grant is pending. The native Zoom helper separately
  reported missing Accessibility when enabled. The denial/recovery checks and
  the Mac test call before and after upgrade still require an interactive session.
- The shared Homebrew tap is public. The source repository remains private.
