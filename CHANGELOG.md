# Changelog

All notable user-visible and security changes are recorded here. The format is
based on Keep a Changelog, and releases use semantic versioning.

## [Unreleased]

## [0.3.0] - 2026-09-20

### Added

- Add one-hop saved-profile jump hosts and explicit OpenSSH-style
  ProxyCommand transports.
- Add multi-round keyboard-interactive and SSH-agent authentication, with an
  optional explicit agent socket for apps launched outside a shell.
- Add RSA-SHA2 client private-key authentication using AWS-LC for signing,
  without enabling the RustCrypto RSA implementation affected by
  RUSTSEC-2023-0071.

### Changed

- Make remote-edit uploads explicit. Saving in the external editor no longer
  triggers a background watcher; users choose **Upload Modified File**, after
  which macSFTP performs the same live remote-snapshot and conflict checks
  before uploading.
- Make **Settings → Profiles** the only place that creates, edits, or deletes
  saved profiles. The Connect dialog now only selects a saved profile or starts
  a temporary connection, with **Manage…** linking to Settings.
- Expose only the real SSH backend from the production SFTP runtime; mock
  actors and constructors now compile only for crate unit tests.
- Remove protocol commands, events, states, and historical implementation
  documents that no longer had production consumers.
- Migrate `profiles.json` to version 5 and remove the never-consumed
  `group_id` placeholder. Profile grouping remains an explicit future product
  decision rather than a dormant persisted field.
- Close the low-level profile-file writer, test-only connection helper, and
  crate-link probe APIs so production callers use the canonical owners.
- Make connection setup cancellation-aware and let runtime shutdown drain
  session tasks within its configured timeout before forcing termination.
- Keep process-wide transfer, residual-temp, and remote-edit events out of the
  Workspace production reducer, and diagnose residual cleanup commands that
  cannot reach a live actor.
- Redesign the Settings profile list with compact two-line rows, endpoint
  truncation, port badges, clearer selection, result counts, and scrolling.

### Fixed

- Preserve the local editable copy and retryable edit session when macSFTP
  cannot read temporary-file metadata before an explicit upload.

## [0.2.1] - 2026-09-13

### Fixed

- Avoid repeated full-directory scans when selecting all files, extending a
  selection, or shrinking a selection in large local and remote directories.
- Replace selection anchors hidden by filtering so later Shift selection does
  not reselect files from the old range.

## [0.2.0] - 2026-08-24

### Changed

- Mid-session disconnects now show the classified cause in the remote pane —
  "Server closed the connection" or "Connection lost" with a network-timeout
  hint — instead of a generic "Disconnected" message, with the same Reconnect
  and Edit actions as before.
- Private-key profiles now express three passphrase states: no passphrase,
  ask every time without storing it, or remember it in the Keychain. The
  profile editor offers the matching policy toggle; previously any typed
  passphrase was always remembered and a saved one could never be removed.
  Existing profiles.json files migrate automatically.

### Fixed

- Hardened profile storage end to end: phased atomic-write failures keep new
  secrets when a rename lands, unsupported file versions enter read-only
  recovery instead of being overwritten, schema violations are reported as
  corrupt rather than silently repaired, deleted profile ids are never
  reused, and concurrent app instances serialize writes through an advisory
  lock instead of clobbering each other's recents and settings.
- Deleting a profile no longer leaves stale links in recent connections or
  restored tabs; remote-edit sessions follow the live connection identity,
  so re-pointing a profile at another host cannot hit an old server's edit
  session.

## [0.1.1] - 2026-08-05

### Changed

- Redesign the delete confirmation dialog with clearer destructive-action
  hierarchy, file and folder previews, concise permanent-delete guidance, and
  responsive sizing in light and dark themes.

## [0.1.0] - 2026-08-01

### Security

- Prevent host-key fingerprints and complete event payloads from reaching logs.
- Replace credential-derived connection-pool hashes with non-secret profile or
  session identities.
- Persist application-owned state with private permissions, durable atomic
  replacement, and write blocking after a corrupt file is detected.
- Treat matching OpenSSH `@revoked` host entries as a hard connection block.

### Added

- Docker-backed successful password-authentication release gate.
- Isolated real macOS Keychain release gate.
- Native GUI release evidence and tag validation policy.
- OpenSSH hashed-host matching for user known_hosts files.
- macOS local-network privacy declaration and a recovery shortcut to the
  matching System Settings pane.

### Changed

- Make the workspace package version the single source for every first-party
  crate, the About surface, and the macOS bundle metadata.
- Rotate diagnostic logs daily with bounded retention and write payload-free
  crash markers.

### Fixed

- Classify macOS local-network privacy denial separately from SSH protocol
  failures and show actionable recovery guidance without exposing raw transport
  errors.
