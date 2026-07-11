# macSFTP — Project Progress Analysis

**Date:** 2026-07-12 (refresh of the 00:11 baseline; M5/M6 residual gap and M7 gaps now closed)
**Basis:** Direct inspection of the workspace — source LOC, real `cargo test --workspace` results, plan doc §20/§21, build output, and git state. Numbers below are measured, not transcribed from milestone markers.

## Snapshot

- **Code:** 19,237 Rust LOC across 7 crates / 29 files.
- **Tests:** **181 passing, 0 failed, 1 ignored** (authoritative, from `cargo test --workspace`). Breakdown: app 32, core 36, platform 5, sftp 52 unit + 21 integration, storage 24, ui 10, test_support 1.
- **Gate:** `scripts/check.sh` (fmt → test → clippy `-D warnings`) is green; only the pre-existing transitive `block v0.1.6` future-incompat note remains (not a `-D warnings` failure).
- **Version control:** **0 commits.** Git repo exists but every file is untracked.
- **CI:** none (no `.github`). Integration tests run only against local `/usr/sbin/sshd`.
- **App bundle:** `build/macSFTP.app` present and valid — `plutil -lint` OK, package version `0.1.0`, `AppIcon.icns` (1.1 MB) with full size set.
- **Velocity:** Cargo.toml created 2026-07-10 01:29 → all M0–M7 feature-complete by 2026-07-12 01:32 (~48 h).

## 1. Completed core modules & current state

| Module | State | Evidence |
| --- | --- | --- |
| M0 skeleton | done | workspace, lint/test scripts, ADRs embedded in plan |
| M1 shell | done | tab bar, split panes, drawer, 10k virtualized list (GPUI `uniform_list`) |
| M2a bridge | done | `runtime.rs` flume channels, `try_send`, `shutdown_timeout` |
| M2b stale guard | done | `core` epoch/scope guard + dedicated tests |
| M2c mock actor | done | trust registry, mock actor, modal id routing |
| M3 russh | done | real OpenSSH, host-key 3-way, password + key auth, real backend wired |
| M4 remote browse | done | read_dir, nav, sort, refresh, error handling, multi-tab independence |
| M5 transfers | **100%** | plan, queue (4 slots), cancel, retry, conflict modal, metadata, symlink, `.macsftp-part` hardlink commit, **cross-connection residual-temp persistence + cleanup now closed** |
| M6 conflict+meta | **100%** | conflict modal + apply-to-all + perms/mtime/symlink preservation, covered by real sshd tests |
| M7 polish/pkg | **100%** | tracing logs, single-window settings + `config.json` v1, unsigned `.app` + `Info.plist` + `AppIcon.icns`, macOS menu + About, **visual-polish verification + manual test matrix + `m7_` regression suite now closed** |

**Overall MVP feature completeness ≈ 100%** of the scope defined in the plan. All seven milestones are functionally delivered.

## 2. In-progress & completion %

There is **no feature work currently in progress** — every MVP deliverable in the plan is implemented and verified. The only open items are *process/hardening* tasks that have not been started:

- **Feature completeness: 100%.** Nothing in the plan's M0–M7 scope remains open.
- **Process completeness: ~30%.** git baseline, CI, and module decomposition are not started (see §3).
- The prior "in progress" items (M5/M6 residual, M7 polish/matrix/regression) are now closed and verified by `scripts/check.sh`.

## 3. Backlog / todo (priority-ordered)

- **P0 — `git init` + baseline commit now.** 0 commits, 19.2k LOC, 181 tests. A single disk failure or stray `git clean -fdx` is total loss; no bisect. *Do this before anything else.*
- **P0 — Stand up CI (`.github`) with Docker OpenSSH.** Plan §19 mandates a Docker integration gate; today 21 integration tests run only against the local sshd and never execute in automation.
- **P1 — De-risk the `workspace.rs` monolith.** 5.9k LOC in one file (the largest in the repo). UI logic has leaked from `ui` into `app`, violating the layered-design intent. Extract connect-form, settings, transfer-drawer, history modules.
- **P1 — Reconcile tracking from code (mostly done this session).** Plan now marks M5/M6/M7 delivered. Remaining: the memory journal's "next step" list still frames git/CI/refactor as the open work — accurate.
- **P2 — Resolve transitive `block v0.1.6` future-incompat note.** Noise from a dependency; pin/yank or ignore explicitly so the log is clean.
- **P2 — Add dated milestones to the plan** so future schedule deviation is measurable (see §5).
- **P3 — Out of MVP scope (plan-explicit):** sync, remote edit, terminal, multi-window, App Store, signing/notarization, DMG/PKG, auto-update, S3/WebDAV/FTP, Keychain-backed profiles, connection pool.

## 4. Risks (ranked) & impact

1. **No version control (Critical).** 0 commits, ~48 h of work, 19.2k LOC, 181 tests. Single disk/operator error = unrecoverable. Likelihood rises with LOC and time.
2. **No CI (High).** 21 integration tests never run in automation; russh/transfer regressions can ship undetected. Plan §19 requires Docker CI — currently unmet.
3. **Bus factor = 1 (High).** All design + implementation solo, no review. Single point of knowledge and execution failure.
4. **`workspace.rs` monolith (Medium-High).** 5.9k LOC single file; hard to review/merge; UI logic bled into `app`, eroding the `core`/`ui`/`app` layering the plan mandates.
5. **GPUI pre-1.0 API churn (Low-Medium).** Mitigated by version pin + centralized API usage in `ui`.
6. **russh/russh-sftp compatibility (Low-Medium).** Server differences, `setstat` inconsistencies. Mitigated by real OpenSSH integration tests + clear error classification.
7. **Multi-tab resource pressure (Low-Medium).** Per-tab sessions can hit `MaxSessions`/`MaxStartups` on constrained servers. Mitigated by default connection caps; no pool yet.
8. **Transitive `block v0.1.6` future-incompat (Low).** Not our code; cosmetic log noise.
9. *(Resolved)* **Cross-connection residual temp cleanup** — was the only open M5/M6 risk; now closed via `ResidualTempStore` + `AppEvent` persistence + reconnect cleanup.

## 5. Milestone timeline comparison

- The plan defines milestones by **logical order, not calendar dates** — there is no baseline schedule, so slippage in *days* cannot be measured. This is itself a tracking gap.
- Reconstructed from filesystem + git mtime: project born **2026-07-10 01:29**; all M0–M7 feature-complete by **2026-07-12 01:32** (~48 h). Observed velocity ≈ 400 LOC/h average (bursty), far ahead of any reasonable plan.
- **Deviation is positive on feature velocity, negative on process hygiene.** The real schedule risk is not feature delivery (done) but the absence of git/CI and single-developer concentration.
- Recommendation: add dated milestones and track % complete from code signals (test counts, LOC, bundle presence) rather than from memory or milestone labels.

## 6. Resource & workload distribution

Solo developer. LOC concentration (share of 19,237):

| Crate | LOC | Share | Role |
| --- | --- | --- | --- |
| sftp | 7,469 | 38.8% | transfer/backend engine (heaviest) |
| app | 5,889 | 30.6% | UI glue + `workspace.rs` monolith |
| core | 2,635 | 13.7% | business state machine / models |
| storage | 1,409 | 7.3% | history, config, residual-temp store |
| ui | 1,327 | 6.9% | presentational components (underweight vs plan) |
| platform | 279 | 1.5% | app paths / dirs |
| test_support | 229 | 1.2% | fixtures |

- **~70% of code lives in just two crates** (`sftp` + `app`) → review/merge bottleneck and bus-factor concentration there.
- Test distribution is healthy and **proportional-to-criticality**: sftp 73 (40.3%), core 36 (19.9%), app 32 (17.7%), storage 24 (13.3%), ui 10 (5.5%), platform 5 (2.8%), test_support 1 (0.6%). The transfer engine — the riskiest subsystem — carries the most tests, which is the correct shape.
- `ui` remains underweight vs the plan's intent; the 5.9k-LOC `workspace.rs` shows UI logic accumulated in `app`. Extracting it is the highest-leverage maintainability refactor.

## 7. Data-driven insights / deviations

- **Feature scope is complete; the project has crossed from "building" to "hardening."** All 7 milestones are delivered and the verify gate is green with 181 tests. Any further effort should go to process (git/CI) and structure (refactor), not new features.
- **The residual-temp gap (the last open M5/M6 item) is closed and verified** — `cargo test --workspace` exercises `ResidualTempStore` (5 new storage tests) and the connect/reconnect cleanup path. This removes the only crash-orphan risk in transfers.
- **M7 was a documentation gap, not a code gap.** Visual polish, the manual test matrix, and the `m7_` regression suite were authored and verified this session; the implementation already satisfied the deliverables. The 15% "gap" was entirely missing verification artifacts.
- **Tracking lag is resolved but must not regress.** Plan now reflects M5/M6/M7 = 100%. Going forward, milestone labels should be updated *as code lands*, not in a separate reconciliation pass.
- **Process, not technology, is the bottleneck.** Gates green, 181 tests pass, bundle valid. The only Critical/High risks are non-functional: no git, no CI, solo bus factor, monolith.

## 8. Recommended next actions (this week)

1. `git init` + initial commit — snapshot current state. *Now (P0).*
2. Stand up CI with Docker OpenSSH (plan §19) — run the 21 integration tests in automation.
3. Extract `workspace.rs` modules; reduce the 5.9k-LOC monolith and restore `ui`/`app` layering.
4. Pin/yank the `block v0.1.6` transitive note so the log is clean.
5. Add dated milestones to the plan for future schedule tracking.
6. (Optional, post-MVP) Keychain-backed profiles, connection pool, signing/notarization.
