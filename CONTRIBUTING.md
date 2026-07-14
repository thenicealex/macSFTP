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

## Security expectations

- Never commit or log passwords, passphrases, private keys, tokens, or test
  credentials.
- Do not weaken host-key mismatch handling.
- Treat stale session/request identifiers as a correctness and security issue.
- Handle fallible cleanup explicitly; do not silently discard its result.
- Use unique temporary paths in tests that can run concurrently.

Report vulnerabilities according to `SECURITY.md`, not through a public issue.

## Pull requests

Every pull request must explain the problem, solution, verification, risks, and
what was deliberately left unchanged. Draft pull requests are welcome for
early design feedback, but required checks must pass before merge.
