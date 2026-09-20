# Contributing to macSFTP

Thank you for helping improve macSFTP. Small, focused pull requests are easier
to review and safer to merge.

## Before starting

1. Search existing issues and pull requests.
2. Open an issue before a large feature, architectural change, or security-
   sensitive redesign.
3. Read `AGENTS.md` and the relevant sections of
   `docs/gpui-russh-plan.md` before changing runtime, SFTP, transfer, host-key,
   persistence, or UI behavior.

Do not include unrelated cleanup, formatting, or renaming in a functional
change.

## Development workflow

Create a topic branch, make one coherent change, and run:

```bash
bash scripts/check.sh
```

For SFTP changes, also confirm that real-session tests ran rather than being
skipped. CI enforces this with `MACSFTP_REQUIRE_SSHD=1`. When changing
`Cargo.toml`, `Cargo.lock`, or vendored code, install `cargo-deny` and run:

```bash
cargo deny --locked check advisories bans licenses sources
```

For visible UI changes, test a narrow/short window, light and dark themes,
keyboard focus, and include a screenshot or a precise visual QA note.

Commit subjects use an imperative Conventional Commit form, for example:

```text
fix(storage): preserve the old secret when profile save fails
refactor(app): isolate transfer drawer rendering
```

## Architectural boundaries

- `core` must not depend on GPUI, russh, or Tokio runtime details.
- `ui` must not perform network requests or own remote sessions.
- `app` must not call russh or Keychain directly.
- `sftp` must not manipulate GPUI entities.
- `storage` owns profiles, known hosts, Keychain references, and migrations.
- Long-lived state belongs in the model/store layer, not a rendered row.

Keep GPUI callbacks non-blocking. Tokio tasks communicate through bounded
commands and events and must never retain GPUI contexts or entities.

Keep these current ownership rules intact:

- **Settings → Profiles** is the only product UI that creates, updates, or
  deletes profiles. The Connect dialog only selects a profile or creates a
  temporary connection.
- `ProfileStore::save_request` is the only public profile-save API. Do not add
  UI-specific storage adapters around it.
- Remote editing never watches local saves. **Upload Modified File** explicitly
  starts the authoritative remote metadata check and upload flow.
- Production runtime constructors always use the real SSH backend. Mock actors
  and mock constructors remain private and test-only.

## Security expectations

- Never commit or log passwords, passphrases, private keys, tokens, or test
  credentials.
- Do not weaken host-key mismatch handling.
- Treat stale session/request identifiers as a correctness and security issue.
- Handle fallible cleanup explicitly; do not silently discard its result.
- Use unique temporary paths in tests that can run concurrently.

Report vulnerabilities according to `SECURITY.md`, not through a public issue.

## Documentation

Update documentation in the same change when behavior or an architectural
boundary changes:

- `README.md` for supported user workflows and build prerequisites;
- `CHANGELOG.md` for unreleased user-visible, security, or compatibility
  changes;
- `docs/gpui-russh-plan.md` for ownership and runtime contracts;
- `docs/ui-ux-guidelines.md` for durable interaction rules;
- `docs/release-process.md` or the evidence template for release-policy changes.

Published files under `docs/release-evidence/` are historical records. Do not
rewrite them to describe current behavior.

## Pull requests

Every pull request must explain the problem, solution, verification, risks, and
what was deliberately left unchanged. Draft pull requests are welcome for
early design feedback, but required checks must pass before merge.
