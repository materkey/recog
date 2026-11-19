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
- Upgrading that Linux installation to the v0.1.3 release build changed the
  extension's configured host list. Reload in Chrome's Extensions page preserved
  the extension ID and enabled the new host. A second two-party Jitsi call
  produced visible text from both audio sources and closed both inputs on exit.
- The Mac acceptance host upgraded from the Python installation and kept its
  archive and recording mode. All four Rust services and the web readiness route
  responded. The installed launcher saw the microphone grant after the interactive
  request. The native Zoom helper separately reported missing Accessibility when
  enabled.
- The shared Homebrew tap is public. The source repository remains private.

## Private release update verification, 2026-11-19

- Homebrew upgraded 0.1.0 to 0.1.3 from the published formula. Setup in the same
  isolated home retained the launcher inode and digest and the unpacked extension
  directory.
- On Z270XX the standalone 0.1.1 updater fetched 0.1.3 using the installing
  account, installed it and restarted four isolated user services. The web live
  status responded, and a second update reported the installed version as current.
- Both release asset digests matched GitHub. The bundled Swift helpers target
  macOS 13. The formula is public and an anonymous release asset request returned 404.

## Interactive Mac acceptance, 2026-11-19

- Before the icon update, two automated participants joined a private Jitsi
  call on the Mac that had migrated from Python. The extension displayed the
  test phrase. Recog wrote 3 seconds of microphone audio and 5.5 seconds of
  system audio. The microphone also heard the test phrase through the speakers.
- With native Zoom detection enabled for the diagnostic, the helper initially
  reported missing Accessibility. After its switch was enabled in System
  Settings, a fresh `recog doctor` reported all three grants.
- Installing the launcher with the new icon invalidated both capture grants.
  The previously granted Zoom helper retained Accessibility. On macOS 26.3.1,
  the old Recog entries still appeared enabled in System Settings. Removing
  the screen recording entry and adding the installed app again restored
  system audio in both the terminal and web readiness checks. Switching the
  new entry off and back on made both checks follow it. Toggling the old
  microphone entry did not grant the new app access. Its fresh consent
  prompt and a call with both tracks still need owner confirmation.
- On v0.1.5 with only system audio granted, two participants joined a second
  private Jitsi call. A controlled clip played on the Mac during the call
  produced a visible transcript and 13 seconds of system audio. The microphone
  file contained zero samples. The call ended and the original conference
  configuration was restored.
- In a separate empty home, the packaged macOS runtime upgraded from v0.1.3
  to v0.1.5. The installed app's signature verified, its icon matched the
  release resource and a repeated setup preserved the launcher inode.
