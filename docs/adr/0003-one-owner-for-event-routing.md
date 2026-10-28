# ADR-0003: One owner for event routing

- **Status:** Accepted
- **Date:** 2026-10-27
- **Scope:** the runtime event bridge across `macsftp-core` and `macsftp-app`
- **Related:** `docs/architecture.md` §4, §5, §6; ADR-0001 and ADR-0002 (same shape: one path for one classification)

## Context

`AppEvent` is the flat wire enum the Tokio runtime sends to the GPUI main thread. Its 31 variants belong to two different owners:

- **Process-owned**: 12 transfer events applied to the global `TransferStore` and rate book, 3 remote-edit check outcomes, 2 residual-temp records, and the local-edit notice. These are applied once per process, not per window.
- **Window-owned**: 13 events that describe one tab's browsing session (connect/disconnect, host trust, directory listings, entry resolution, operation failures). Every window receives them and may only apply the ones whose `RemoteEventScope` is still live.

Before this decision, ownership existed only as call-site discipline. `AppEventCoordinator` checked `AppEvent::is_transfer_event()` twice (batch dispatch and single-event dispatch) and then dispatched; `Workspace::handle_app_event` ended in an 18-variant arm whose body was `debug_assert!(false, "process-owned event reached a Workspace")`. That assertion disappears in release builds, so a misrouted event silently did nothing. A `#[cfg(test)]` adapter inside `Workspace` applied transfer events for tests, which meant tests exercised a path production never used. The remote-scope rule lived on the wire enum, mixing the stale-event guard with the owned/not-owned question.

Adding an `AppEvent` variant therefore required remembering three independent classifications, and nothing forced them to agree.

## Decision

1. `AppEvent` remains the flat enum the runtime produces; producers and the runtime protocol do not change.
2. `AppEvent::route()` is the single classification point. It consumes the event and returns `RoutedEvent::Process(ProcessEvent)` or `RoutedEvent::Window(WindowEvent)`.
3. `ProcessEvent` names the process sinks: `Transfer(TransferEvent)`, `EditCheck(EditCheckEvent)`, `ResidualTempCreated`, `ResidualTempCleared`, `LocalEditModified`. `TransferEvent` and `EditCheckEvent` are themselves enums so the sinks accept typed events instead of slicing the wire enum with `_ =>` catch-alls:
   - `TransferStore::apply_event(&TransferEvent, now)`
   - `ActiveTransfers::apply_transfer_event(&TransferEvent, now)`
   - the coordinator's edit-check handler takes `&EditCheckEvent` and matches it exhaustively
4. `WindowEvent` contains the 13 window-owned variants and owns `WindowEvent::remote_scope()`. `AppState::should_accept_event(&WindowEvent)` applies the stale-event guard before any window sees the event.
5. `AppEventCoordinator` routes every event exactly once, applies `ProcessEvent` itself, and broadcasts `WindowEvent` to every workspace. `Workspace::handle_window_event` matches `WindowEvent` exhaustively; a process-owned event is a compile error, not a runtime no-op.
6. The `#[cfg(test)]` transfer adapter in `Workspace` is deleted. Tests that need transfer state apply the same `TransferEvent` the coordinator applies, and tests that need a conflict modal present it through the same `present_transfer_conflict` entry point production uses.

The classification lives in `macsftp-core`, next to the event types. `macsftp-app` consumes `RoutedEvent`; it does not re-derive ownership.

## Alternatives considered

- **Nest the wire enum (`AppEvent::Process(ProcessEvent)` / `AppEvent::Window(WindowEvent)`).** Rejected: it changes roughly 250 construction sites in runtime, actors, transfer manager, and tests without changing the ownership question. `route()` gives the same single classification with a mechanical migration.
- **Keep the flat enum and add a second `is_*` predicate.** Rejected: predicates are additive and can disagree; the release-mode `debug_assert!` problem remains.
- **Classify inside each consumer (coordinator and each window).** Rejected: that is the status quo, and the two classifications already drifted.

## Consequences

- Ownership is decided once. A new `AppEvent` variant fails to compile until `route()` names its owner.
- `Workspace` loses 18 unreachable arms and the test-only adapter; its event match is exhaustive over live behavior.
- Process sinks are typed, so adding a `TransferEvent` or `EditCheckEvent` variant fails at the sink instead of falling into a catch-all.
- `core` owns the routing table and the remote-scope extraction; `app` keeps only execution of the routed events.
- `WindowEvent::remote_scope()` returns `None` for `RemoteEntryResolveFailed` (epoch-correlated, filtered by its handler) and for tab lifecycle/local-directory events, matching the guard that §6 describes.

## Out of scope

- Changing the runtime protocol, event batching, backpressure, or channel bounds.
- Moving transfer or edit-check state into a window.
- The wording or shape of user-facing errors.

## Revisit when

- A third owner appears (for example, events owned by a dedicated window group); then `RoutedEvent` gains a variant instead of a predicate.
- The transfer and edit-check event enums need to share a payload or a common trait; introduce that only when a second consumer needs it.
