# ADR-0005: One executor for remote-edit effects

- **Status:** Accepted
- **Date:** 2026-10-28
- **Scope:** `macsftp-app` remote-edit effect execution
- **Related:** `docs/architecture.md` §9; ADR-0002 (the lifecycle produces `EditEffect` values)

## Context

ADR-0002 moved every phase transition into `macsftp_core::remote_edit` and made callers execute the `EditEffect` values it returns. Two app paths execute those effects: a user action handled inside `Workspace`, and a process-wide event applied by `AppEventCoordinator`. Each path carried its own executor with the same policy — dispatch-failure rollback and notice choice, editor resolution, temp-directory cleanup, and listing rebase — roughly 90 lines each.

The copies had already drifted:

- dispatch succeeded only if `Workspace::send_command` / `RuntimeClient::try_send` accepted the command, with different fallback status;
- notices went through the window status or a tab-scoped status;
- the editor's file name came from a remote-path helper in one copy and `std::path::Path` on the local temp path in the other.

A policy change (a new notice, a different rollback mapping) had to be made twice, and window-free policy tests were impossible because the only implementation required GPUI windows.

## Decision

1. `crates/app/src/edit_effects.rs` owns effect execution policy exactly once. `execute_edit_effects` handles `Dispatch` (rollback, notice, follow-up effects), `OpenEditor` (editor resolution from config, launch, notice), `DeleteTempDir`, `RebaseListing`, and `Notice`.
2. The environment is supplied through the `EditEffectHost` seam: `resources`/`resources_mut`, `dispatch(tab_id, command) -> bool`, `notify(tab_id, notice)`, and `sync_listing(tab_id, path, snapshot) -> bool`.
3. There are two real adapters. `WorkspaceEditHost` runs effects for an action in its own window and dispatches through `Workspace::send_command`. `ProcessEditHost` runs effects from `AppEventCoordinator`, resolves the window that owns the effect's tab, and dispatches through that window's runtime client. Both present the edit notice as the final message.
4. Neither `Workspace` nor `event_coordinator` keeps its own executor; each builds a host and calls `execute_edit_effects`.

## Alternatives considered

- **Move the executor into `macsftp-core` with ports.** Rejected: editor launch and window routing are app/platform concerns, and a port-based core executor would need fake ports to test the policy it already tests directly.
- **One `&mut App` executor reused by both paths.** Rejected: GPUI panics on re-entrant entity access, so the executor cannot be handed a second `&mut App` while a `Workspace` update is in progress. That constraint is why the seam takes a host rather than `App`.
- **Keep two executors and test both.** Rejected: the drift above shows the copies already behave differently, and the policy has no single test surface.

## Consequences

- A policy change touches one file. Policy unit tests in `edit_effects.rs` run without GPUI; `Workspace` keeps a behavioral test for the channel-full rollback.
- Dispatch mechanics stay per adapter because their failure UX differs: the workspace keeps its busy/closed status before the notice overwrites it, the coordinator logs when no window owns the tab. The edits a user sees are the same notice.
- Adding an effect variant or an executor capability means updating core, the executor, and both adapters.

## Out of scope

Phase rules and rollback semantics (ADR-0002), runtime command shapes, editor launch mechanics beyond resolution, and UI copy.

## Revisit when

A third execution environment appears, or an adapter can no longer express dispatch or notification through the four capabilities.
