# Security Policy

## Supported versions

macSFTP is pre-release software. Security fixes are made on the latest default
branch; older snapshots and locally modified builds are not maintained.

## Reporting a vulnerability

Do not open a public issue for a suspected vulnerability. Once this repository
is published on GitHub, use **Security → Advisories → Report a vulnerability**
to send a private report. Repository maintainers must enable GitHub private
vulnerability reporting before accepting public contributions.

If that form is unavailable, contact the repository owner through their GitHub
profile without including vulnerability details, credentials, private keys,
host fingerprints, or reproduction data. Agree on a private channel before
sending sensitive information.

Include only the minimum information needed to reproduce the issue:

- affected commit or version;
- impact and preconditions;
- minimal reproduction steps;
- whether credentials, host trust, path traversal, symlinks, or stale sessions
  are involved;
- a proposed remediation, if known.

Maintainers should acknowledge a complete report within seven days, keep the
reporter informed while a fix is prepared, and coordinate disclosure after a
patched version or commit is available.

## Security boundaries

- Passwords and private-key passphrases belong only in macOS Keychain and
  short-lived in-memory input state.
- Host-key mismatch always blocks a connection.
- Unknown-host decisions are scoped to the active request and session epoch.
- Secrets and full private-key paths must not appear in logs, errors, UI detail,
  configuration files, fixtures, screenshots, or snapshots.
