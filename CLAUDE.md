# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Repository rules (read first)

- `AGENTS.md` is the mandatory engineering rulebook (in Chinese): think before coding, simplicity first, surgical changes, security invariants, ID/stale-event rules, transfer semantics, UI rules, testing and Git hygiene. Follow it; it takes precedence over convenience.
- `docs/architecture.md` (Chinese) is the authoritative description of ownership, runtime contracts, persistence, security, and UI/UX constraints. Read the relevant section before changing architecture, runtime, SFTP, transfers, host keys, persistence, or UI.
- `CONTRIBUTING.md` documents the PR and documentation-sync requirements. `README.md` has four language versions (`README.zh-CN.md`, `README.ja.md`, `README.ko.md`) that must describe the same product and change together.
- Rust toolchain is pinned to `1.96.1` (`rust-toolchain.toml`), edition 2024, aarch64-apple-darwin, macOS 13+.

## Commands

```bash
cargo run                     # run the app from the workspace
bash scripts/check.sh         # full local gate: fmt, architecture, log safety, tests, clippy
bash scripts/build_app.sh     # unsigned bundle at build/macSFTP.app
```

`scripts/check.sh` is exactly what CI runs:

```bash
cargo fmt --all --check
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

Focused commands:

```bash
cargo test -p macsftp-core <test_name>                    # one unit test
cargo test -p macsftp-app <test_name>                     # one app/UI test
cargo test -p macsftp-sftp --test real_session            # sshd integration suite
MACSFTP_REQUIRE_SSHD=1 cargo test -p macsftp-sftp --test real_session   # fail instead of skip
cargo test -p macsftp-sftp --test password_auth           # peer auth, skips without sshd
```

Dependency changes (`Cargo.toml`, `Cargo.lock`, vendored code) also require:

```bash
cargo deny --locked check advisories bans licenses sources
```

Isolated-platform gates:

```bash
bash scripts/test_keychain.sh       # macOS-only, temporary user Keychain
bash scripts/test_password_auth.sh  # Docker/PAM password + keyboard-interactive
```

Visual evidence for UI changes (headless Metal, writes repo files):

```bash
cargo test -p macsftp-app regenerate_workspace_ux_evidence -- --ignored --nocapture
```

Real-session tests spawn `/usr/sbin/sshd` as the current user on loopback; they skip with an explicit message when `sshd`/`ssh-keygen` is unavailable locally, but CI sets `MACSFTP_REQUIRE_SSHD=1` and fails instead.

## Workspace layout

Dependency direction (enforced by `scripts/check_architecture.sh`):

```text
app -> ui, core, storage, sftp, native-menu
ui -> core
sftp -> core, storage
storage -> core
platform -> core
test_support -> core
```

| Crate (package) | Role |
| --- | --- |
| `crates/core` (`macsftp-core`) | Pure models and state machines: tabs, connection state, command/event types, transfers, conflicts, remote editing, sort, error codes. Must not depend on GPUI, russh, Tokio runtime, or macOS APIs. Library root is a single `src/core.rs` with modules. |
| `crates/app` (`macsftp-app`) | GPUI entry point, windows, `Workspace` entities, actions, event coordination. Sends runtime commands and consumes events; must not call russh or Keychain directly. Binary is `macsftp`. |
| `crates/ui` (`macsftp-ui`) | Theme and reusable GPUI components (file list, tabs, scrollbar, inputs, transfer rows). Presentation only: no network calls, no remote sessions, no persisted state. |
| `crates/sftp` (`macsftp-sftp`) | Tokio runtime, `russh`/`russh-sftp` adapter, physical connection pool, browsing session actor, transfer planner/manager, host trust, auth. Must not touch GPUI entities. Production APIs construct the real SSH backend; mock actors are `#[cfg(test)]` only. |
| `crates/storage` (`macsftp-storage`) | Versioned JSON stores, atomic writes, file locking, profiles, known_hosts, Keychain references, migrations. Secrets never enter ordinary config. |
| `crates/platform` (`macsftp-platform`) | macOS and local-filesystem boundary: app paths, local directory reads, opening editors, permission/mtime/symlink handling, diagnostics. |
| `crates/native-menu` (`macsftp-native-menu`) | AppKit `NSMenu` presenter for input and file-list context menus. Only crate allowed `unsafe` (solely `src/macos.rs`); no GPUI entities or business drafts. |
| `crates/test_support` | Loopback `sshd` fixture for integration tests. Not part of product behavior. |
| `crates/vendor-block`, `crates/vendor-russh` | Patched vendored dependencies via `[patch.crates-io]`; excluded from the workspace. Change only when a dependency fix is the task. |

## Runtime bridge and state ownership

- GPUI (main thread) and Tokio communicate only through bounded `flume` command/event channels. GPUI-side sends are non-blocking; Tokio tasks never hold GPUI `Context`, `Window`, or `Entity` handles. A process-wide `AppEventCoordinator` reduces events inside a GPUI update closure and calls `cx.notify()` / refreshes windows.
- Each browsing tab has its own session actor, but actors share authenticated physical connections through `ConnectionManager`. Connection reuse is keyed by endpoint, username, and saved-profile identity/revision (no secrets).
- Cross-thread events carry `RemoteEventScope { tab_id, session_id, session_epoch }`. Staleness checks belong in `core`; late events for a closed tab or an old epoch must be ignored. `SessionId`/`session_epoch` are allocated by UI/core only — runtime and actors echo the command's values.
- Long-lived state lives in `AppState`/`TabStore`/`TransferStore`/`ModalStore`/process globals, not view structs. Per-window `Workspace` owns that window's state; process globals own profiles, config, recents, residual temps, the shared `TransferStore`, remote-edit sessions, runtime event receiver, and session checkpointing.
- Shutdown must cancel actors/transfers and finish within a bounded timeout; do not rely on dropping a Tokio runtime to wait.

## Transfer model

- Directory transfers are `TransferPlan`s with streamed child jobs — never a single recursive job. Planning publishes progress incrementally.
- `TransferManager` is process-wide and owns planning/queued/running/waiting-for-conflict/completed/skipped/cancelled/failed states. Conflict is a state, not an error; `apply_to_all` is scoped to one `TransferPlanId`.
- Cancellation uses explicit tokens. Retry must reacquire a usable connection rather than holding a failed one.
- Per-plan/connection limits and session reuse (idle `TransferSessionSlot`, pipelined read/write, remote facts cache) are performance-sensitive invariants documented in `docs/architecture.md` §8. Reducing RTTs must not change conflict/failure semantics.
- When a plan reaches a terminal state, reload every tab whose pane shows an affected directory via the reload entry points (preserving selection/scroll), not the navigation entry points that reset them.
- `.macsftp-part-*` files are not normal conflicts; cleanup failures go to `ResidualTempStore` and only recorded paths are cleaned later. Symlinks are copied as links, not dereferenced.

## Security invariants

- Passwords and private-key passphrases live only in macOS Keychain (or zeroizing in-memory containers); ordinary config stores `SecretRef`.
- Host-key mismatch blocks the connection with no one-click override. Unknown hosts require an explicit, request-bound decision. Compare host keys by key data (`key_data()`), never whole `PublicKey`/`Entry` objects that include comments.
- Never log secrets, keyboard-interactive answers, full private-key paths, ProxyCommand text, agent socket/identity data, or unvetted third-party error payloads. `scripts/check_sensitive_logs.sh` audits this.
- Async failures must reach the UI (as `UserFacingError`) or an audited log path. No `unwrap`/`expect` on recoverable paths, and no silently discarded fallible results.

## UI conventions

- GPUI and `gpui-base` come from the pinned `gpui-kit` workspace dependency; component and GPUI types must be the same version.
- Menu bar, keyboard shortcuts, and command palette all reference the same action ids; `app_menus` consistency tests tie the three together. Window/app lifecycle commands stay in standard macOS menu positions and out of the palette.
- File lists are virtualized; selection uses stable ids/paths, never row indices. Long-lived business state stays out of row/view structs.
- UI text may be single-language, but business layers emit error codes/parameters; `app`/`ui` map them to display text. Icon-only buttons need tooltips/labels; modals need title, primary action, cancel path, and request-id binding.

## Testing expectations

- `core` state-machine changes need unit tests there; SFTP adapter/auth changes need real-`sshd` integration tests; runtime bridge changes cover bounded channels, backpressure, shutdown, and stale-event guards; UI changes cover action/modal/tab/focus/transfer drawer behavior.
- Bugfixes start with a test that reproduces the bug. Concurrent tests must use uniquely named temporary paths.
- Visible UI changes require a screenshot or a precise visual-QA note (see `docs/visual-evidence/`).
- Run the smallest verification that matches the risk; run `bash scripts/check.sh` before declaring a change complete.
