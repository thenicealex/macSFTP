# ADR-0002: One owner for the remote-edit lifecycle

- **Status:** Accepted
- **Date:** 2026-10-27
- **Scope:** remote editing across `macsftp-core`, `macsftp-app`, `macsftp-platform`, `macsftp-sftp`
- **Related:** `docs/architecture.md` §9, §12.11; ADR-0001 (same shape: one execution path for a command surface)

## Context

A remote-edit session has five live phases (`Downloading`, `Editing`, `CheckingRemote`, `UploadingBack`, `RemoteConflict`) and several invariants that must hold on every transition: the pending check id, the tab/session epoch, the write-through `io_path`, the local mtime captured at check dispatch, and the temp-file baseline. Today those rules are enforced by whichever caller happens to run next:

- `crates/core/src/core.rs`: `EditSessionStore` is a public map with public `EditSession` fields. It allocates ids and performs one transition (reconnect resets `CheckingRemote` to `Editing`).
- `crates/app/src/workspace/remote_edit.rs`: user intents — begin, download start, upload request, conflict resolution — set phases directly and roll them back when a channel send fails.
- `crates/app/src/event_coordinator.rs`: async completions — download finished, upload finished, authoritative check result — set phases, rebase snapshots, open the editor, delete temp dirs on failure, and surface status to the owning window.
- `crates/app/src/workspace/event_handling.rs`: directory refreshes adopt listing baselines, but only for `Editing` sessions and only at whole-second agreement.
- Cleanup is split across tab close (`cleanup_edit_sessions_for_tab`), window close (`cleanup_orphaned_edit_sessions`), failed downloads, and `DiscardLocal` conflict resolution.

Fourteen production files across four crates read or write `EditSession`. No module owns a phase transition, so correctness depends on call-site discipline: a new caller can set `phase` without clearing `pending_check_id`/`checking_local_mtime`, and the phase rules cannot be tested without GPUI or transfer plumbing.

## Decision

1. A new `macsftp_core::remote_edit` module exports `RemoteEditLifecycle` (wrapping the existing `EditSessionStore`), which owns the sessions, id allocation, temp-path namespace, and **every** phase transition. The five phases stay as they are.
2. Callers express intent, not state: `begin`, `start_download`, `abort_download`, `request_upload`, `revert_check`, `on_check_result`, `on_transfer_finished`, `resolve_conflict`, `plan_conflict_compare`, `dispatch_failed`, `record_local_modified`, `adopt_listing_baselines`, `update_epoch_for_tab`, `remove_for_tab`. Each returns the resulting effects or commands. `begin` distinguishes a fresh edit (`BeginEdit::Start`, carrying cleanup effects for stale sessions it reaped) from a request it handled itself (`BeginEdit::Handled`).
3. Side effects leave the module as values: `EditEffect::Dispatch { command, rollback }`, `OpenEditor`, `DeleteTempDir`, `RebaseListing`, and `Notice`. `macsftp-app` executes them. A failed dispatch applies the effect's declared rollback (`RevertCheck`, `RestoreConflict`, `AbortDownload`) through the same module.
4. The lifecycle performs no filesystem I/O and holds no GPUI, Tokio, or platform types. Local facts (size, mtime) and transfer endpoints are passed in by the app, which reads them at the edge.
5. The session store becomes private. The view reads through queries (`session`, `find_for_tab_path`, `sessions`, `conflict_sessions`); no caller writes `phase` directly.
6. The app keeps what is genuinely its own: connected-tab and filename validation, the large-file confirmation modal, conflict modal state, editor resolution and launch, watcher wiring, runtime dispatch, and the mapping from `EditNotice` to display copy. `platform` and `sftp` keep their adapter roles; the runtime still only echoes the ids the UI allocated.

The deepened module is the remote-edit lifecycle. Its interface is the intent methods and the effect list; the phase machine, guards, temp cleanup, and baseline rules sit behind it.

## Alternatives considered

- **App-side module with port traits (send, open editor, delete temp).** Rejected. It needs fake ports to test phases, and `core` would keep a public mutable store, so the invariants stay outside the deep module.
- **Add transition methods to `EditSessionStore` and leave orchestration in app.** Rejected. Cleanup, editor launch, and conflict decisions would still have two owners, which is where the current drift lives.
- **Keep status quo and add more tests.** Rejected. Tests would pin the current call-site behavior without giving future callers a safe interface.

## Consequences

- Adding a phase or a guard means editing one module, not every caller. Reconnect, conflict, and transfer completions share the same rollback and cleanup rules.
- `core` gains a `remote_edit` module and effect/notice types; `app` loses direct store mutation and the transition bodies (roughly 400–500 lines move).
- Phase tests run in `macsftp-core` without GPUI, covering the guards, rollbacks, and baseline rules. App tests keep their behavioral coverage by driving the lifecycle and asserting effects; fixtures that need a session in an arbitrary phase seed through a small `test-support` feature hook instead of reaching into production state.
- Status copy moves from call sites to one app-side mapping of `EditNotice`.
- Two notices are unified: opening an already-open copy and finishing a download both use the download-completion copy. The rare channel-full status for a user-initiated upload becomes "The upload couldn't start. Try again." instead of the generic busy message.
- `docs/architecture.md` §9 and §3 record the ownership; §12.11 keeps the UX rules.

## Out of scope

- Content-hash or server-version conflict detection, session persistence across launch, and multi-window coordination changes.
- Watcher transport, editor launch mechanics, and runtime command shapes.
- Moving the large-file threshold or conflict modal state out of the view.

## Revisit when

- The effect or notice lists grow past what a caller can exhaustively match, or a second lifecycle (for example edits that survive quit) needs different adapters.
- A phase rule needs data the intent methods cannot receive as a fact; then introduce a port instead of reading the filesystem or windows inside the module.
