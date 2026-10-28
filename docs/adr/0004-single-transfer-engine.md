# ADR-0004: One transfer execution path

- **Status:** Accepted
- **Date:** 2026-10-27
- **Scope:** `macsftp-sftp` transfer scheduling, execution, and integration tests
- **Related:** `docs/architecture.md` §7–8

## Context

The runtime submits transfer plans to the process-wide `TransferManager`. The browsing session actor also contained a separate queue and conflict-resolution request path used only by integration tests. Both paths called the same transfer execution code, but tests driving the actor could pass without exercising the production scheduler or runtime commands. Maintaining the unused path widened the actor interface and duplicated scheduling and conflict state.

## Decision

1. `TransferManager` is the sole transfer scheduler. Transfers, cancellation, retries, and conflict decisions enter through the runtime command path.
2. Remove the actor's test-only transfer queue and its transfer execution, cancellation, retry, and conflict-decision request variants. The actor continues to provide a connection lease and a dedicated channel for remote download planning.
3. Move shared transfer execution, remote download planning, conflict handling, and session reuse into the internal `transfer_job` module. `TransferManager` owns scheduling and uses that module to execute jobs.
4. Exercise transfer behavior in real-session integration tests through `RuntimeController` and `AppCommand`. Directory plans cover decisions applied across multiple jobs, including conflicts already waiting when a decision is made.

## Alternatives considered

- **Keep the actor queue for tests.** Rejected: it tests a path the product never uses and can diverge from the process-wide scheduler.
- **Remove only the queue and leave execution in the actor module.** Rejected: the actor would still appear to own execution even though the manager runs the jobs.
- **Expose a new direct transfer test interface.** Rejected: the existing runtime command path already provides the production seam.

## Consequences

- Transfer scheduling, cancellation, retry, and plan-scoped conflict state have one owner; browsing tab closure does not own the lifetime of queued transfers.
- The actor retains browsing operations and the connection and planning requests needed by the runtime. Transfer jobs continue to reuse sessions within their plan, preserve streamed planning, and follow the existing cancellation, conflict, temporary-file, metadata, and symlink rules.
- An upload skipped during conflict resolution can return its session to the plan: no file I/O has started and the conflict metadata requests have completed. This avoids exhausting server session limits when queued jobs immediately follow multiple skipped conflicts. Sessions interrupted during file I/O, and downloads skipped during conflict resolution, are not reused.
- Transfer integration tests cover the runtime and manager together; unit tests remain near the implementation they exercise.

## Out of scope

Changes to transfer conflict decisions or terminal states, concurrency limits, persistence, user interface, and the runtime command contract.
