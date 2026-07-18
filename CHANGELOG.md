# Changelog

All notable user-visible and security changes are recorded here. The format is
based on Keep a Changelog, and releases use semantic versioning.

## [Unreleased]

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
