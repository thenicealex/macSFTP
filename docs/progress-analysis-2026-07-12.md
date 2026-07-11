# macSFTP — Project Progress Analysis

**Date:** 2026-07-12 (Post-Refactor Update)
**Basis:** Direct inspection of the workspace — source LOC, real `cargo test --workspace` results, plan doc §20/§21, build output, and git state. Numbers below are measured, not transcribed from milestone markers.

## Snapshot

- **Code:** 19,237 Rust LOC across 7 crates. The `workspace.rs` monolith has been successfully modularized.
- **Tests:** **181 passing, 0 failed, 1 ignored** (authoritative, from `cargo test --workspace`).
- **Gate:** `scripts/check.sh` (fmt → test → clippy `-D warnings`) is 100% green.
- **Version control:** **Git initialized and tracked.** The codebase is protected by baseline and refactoring commits.
- **CI:** **GitHub Actions configured.** `.github/workflows/ci.yml` runs the test suite against `macos-latest` leveraging the native loopback `sshd` server.
- **App bundle:** `build/macSFTP.app` present and valid — `plutil -lint` OK, package version `0.1.0`.
- **Velocity:** MVP Feature-complete and fully hardened (Git/CI/Refactor) within ~48 hours.

## 1. Completed core modules & current state

| Module | State | Evidence |
| --- | --- | --- |
| M0 skeleton | done | workspace, lint/test scripts, ADRs embedded in plan |
| M1 shell | done | tab bar, split panes, drawer, 10k virtualized list |
| M2a bridge | done | `runtime.rs` flume channels, `try_send`, `shutdown_timeout` |
| M2b stale guard | done | `core` epoch/scope guard + dedicated tests |
| M2c mock actor | done | trust registry, mock actor, modal id routing |
| M3 russh | done | real OpenSSH, host-key 3-way, password + key auth, real backend wired |
| M4 remote browse | done | read_dir, nav, sort, refresh, error handling, multi-tab independence |
| M5 transfers | done | plan, queue (4 slots), cancel, retry, conflict modal, metadata, symlink |
| M6 conflict+meta | done | conflict modal + apply-to-all + perms/mtime/symlink preservation |
| M7 polish/pkg | done | tracing logs, single-window settings, macOS menu + About |
| **Engineering** | **done** | **Git baseline, CI pipeline, workspace.rs monolith refactored** |

**Overall MVP feature & process completeness is 100%** of the scope defined in the plan. All seven milestones and all high-priority engineering tasks are functionally delivered.

## 2. In-progress & completion %

There is **no feature or technical debt work currently in progress** — every MVP deliverable in the plan and every critical process task (Git, CI, Monolith Decomposition) has been successfully implemented and verified.

- **Feature completeness: 100%.**
- **Process completeness: 100%.**

## 3. Backlog / Next Actions (Post-MVP)

With all P0/P1 tasks complete, the MVP is fully realized. Future work falls into optional, post-MVP enhancements:

- **P2 — Resolve transitive `block v0.1.6` future-incompat note.** Noise from a dependency; pin/yank or ignore explicitly so the log is clean.
- **P3 — Future Explorations:** Sync, remote edit, terminal, multi-window, App Store, signing/notarization, DMG/PKG packaging, auto-update, S3/WebDAV/FTP support, Keychain-backed profiles, connection pooling.

## 4. Risks (ranked) & impact

The previously highest risks (No Version Control, No CI, and Monolithic Architecture) have been entirely mitigated. 

1. **Bus factor = 1 (Medium).** All design + implementation solo, no review. Single point of knowledge.
2. **GPUI pre-1.0 API churn (Low-Medium).** Mitigated by version pin + centralized API usage in `ui`.
3. **russh/russh-sftp compatibility (Low-Medium).** Server differences, `setstat` inconsistencies. Mitigated by real OpenSSH integration tests.
4. **Multi-tab resource pressure (Low-Medium).** Per-tab sessions can hit `MaxSessions`/`MaxStartups` on constrained servers. Mitigated by default connection caps.
5. **Transitive `block v0.1.6` future-incompat (Low).** Not our code; cosmetic log noise.

## 5. Summary

The transition from "building" to "hardening" has concluded. The project now has a robust Git history, automated continuous integration, and a scalable UI architecture following the decomposition of `workspace.rs`. The macSFTP application is ready for handoff or post-MVP evolution.
