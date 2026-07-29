# Cancel All Transfers & Clear Transfer Records — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add one-click "Cancel All" and "Clear Records" buttons to the transfer drawer header

**Architecture:** View-side batch operations; no new AppCommand or runtime changes. "Cancel All" iterates cancellable jobs and reuses the existing per-transfer cancellation path. "Clear Records" adds a `clear_terminal()` method to `TransferStore` and flushes terminal jobs/plans from the view-side global state.

**Tech Stack:** Rust, GPUI, macsftp_core, macsftp_ui

## Global Constraints

- No new AppCommand / AppEvent variants
- No runtime or TransferManager changes
- No confirmation modal
- Buttons only visible when there is something to act on
- Follow existing code patterns (`icon_button` component, `ActiveTransfers` trait)

---

## File Structure

| File | Action | Purpose |
|------|--------|---------|
| `crates/app/assets/icons/trash.svg` | Create | Trash icon SVG for "Clear Records" button |
| `crates/ui/src/icon.rs` | Modify | Add `Trash` variant to `IconName` |
| `crates/app/src/assets.rs` | Modify | Register `trash.svg` in embedded icons macro + test |
| `crates/core/src/core.rs` | Modify | Add `TransferStore::clear_terminal()` method |
| `crates/app/src/resources.rs` | Modify | Add `clear_terminal_transfers()` to `ActiveTransfers` trait + impl |
| `crates/app/src/workspace/transfers.rs` | Modify | Add `cancel_all_transfers()` and `clear_transfer_records()` |
| `crates/app/src/workspace/transfer_render.rs` | Modify | Add two icon buttons to drawer header |

---

### Task 1: Add Trash Icon

**Files:**
- Create: `crates/app/assets/icons/trash.svg`
- Modify: `crates/ui/src/icon.rs:6-19`
- Modify: `crates/app/src/assets.rs:26-40` and `crates/app/src/assets.rs:64-77`

**Interfaces:**
- Produces: `IconName::Trash` variant with path `"icons/trash.svg"`

- [ ] **Step 1: Create trash.svg**

Write `crates/app/assets/icons/trash.svg`:

```svg
<svg xmlns="http://www.w3.org/2000/svg" width="14" height="14" viewBox="0 0 14 14" fill="none">
  <path d="M3 4h8M5.5 4V2.5a.5.5 0 0 1 .5-.5h2a.5.5 0 0 1 .5.5V4M4.5 4v7.5a1 1 0 0 0 1 1h3a1 1 0 0 0 1-1V4" stroke="currentColor" stroke-width="1.2" stroke-linecap="round" stroke-linejoin="round"/>
</svg>
```

- [ ] **Step 2: Add Trash to IconName enum**

In `crates/ui/src/icon.rs`, add `Trash,` variant after `Symlink,` (line 16):

```rust
    Symlink,
    Trash,
    ChevronDown,
```

In the same file, add the path mapping after `Self::Symlink` (line 34):

```rust
            Self::Trash => "icons/trash.svg",
```

- [ ] **Step 3: Register in embedded_icons! macro**

In `crates/app/src/assets.rs`, add `"trash.svg",` after `"symlink.svg",` (line 36):

```rust
    "symlink.svg",
    "trash.svg",
    "chevron_down.svg",
```

- [ ] **Step 4: Add to icon resolution test**

In `crates/app/src/assets.rs`, add `IconName::Trash,` after `IconName::Symlink,` (line 74):

```rust
            IconName::Symlink,
            IconName::Trash,
            IconName::ChevronDown,
```

- [ ] **Step 5: Run icon test**

```bash
cargo test -p macsftp-app -- assets::tests::every_icon_name_resolves_to_an_embedded_asset
```

Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/app/assets/icons/trash.svg crates/ui/src/icon.rs crates/app/src/assets.rs
git commit -m "feat: add Trash icon for clear-transfer-records button"
```

---

### Task 2: TransferStore::clear_terminal()

**Files:**
- Modify: `crates/core/src/core.rs:1110` (end of `impl TransferStore` block, before `ModalStore`)

**Interfaces:**
- Consumes: `TransferStore.jobs`, `TransferStore.plans`, `TransferState` variants
- Produces: `pub fn clear_terminal(&mut self) -> bool`

- [ ] **Step 1: Add clear_terminal method**

Insert before the closing `}` of `impl TransferStore` (before line 1111, before the `ModalStore` definition):

```rust
    /// Remove every job in a terminal state (Completed, Skipped, Failed)
    /// and any plan whose jobs have all been removed.
    ///
    /// Returns `true` when at least one job or plan is removed.
    pub fn clear_terminal(&mut self) -> bool {
        let before = self.jobs.len();
        self.jobs.retain(|job| {
            !matches!(
                job.state,
                TransferState::Completed
                    | TransferState::Skipped
                    | TransferState::Failed { .. }
            )
        });
        let remaining_ids: std::collections::HashSet<TransferId> =
            self.jobs.iter().map(|job| job.id).collect();
        self.plans.retain(|plan| {
            remaining_ids.contains(&plan.root_job_id)
                || plan
                    .child_jobs
                    .iter()
                    .any(|id| remaining_ids.contains(id))
        });
        self.jobs.len() != before
    }
```

- [ ] **Step 2: Write unit tests for clear_terminal**

Append to the test module in `crates/core/src/core.rs` (find the `#[cfg(test)] mod tests` block in the file):

```rust
    #[test]
    fn clear_terminal_removes_completed_jobs_and_orphaned_plans() {
        let mut store = TransferStore {
            jobs: vec![
                TransferJob {
                    id: TransferId(1),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                    state: TransferState::Completed,
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
                TransferJob {
                    id: TransferId(2),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/b.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/b.txt")),
                    state: TransferState::Running {
                        bytes_done: 512,
                        bytes_total: Some(1024),
                        started_at: Timestamp::from_secs_since_epoch(0),
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
            ],
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(1),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferPlanState::Completed,
                planned_count: 1,
                total_bytes: Some(10),
                child_jobs: vec![TransferId(1)],
                conflict_policy: ConflictPolicy::Ask,
            }],
            pending_conflicts: Vec::new(),
        };

        let changed = store.clear_terminal();
        assert!(changed);
        assert_eq!(store.jobs.len(), 1);
        assert_eq!(store.jobs[0].id, TransferId(2));
        assert_eq!(
            store.plans.len(),
            0,
            "orphaned plan should be removed with its last job"
        );
    }

    #[test]
    fn clear_terminal_preserves_plan_with_remaining_jobs() {
        let mut store = TransferStore {
            jobs: vec![
                TransferJob {
                    id: TransferId(1),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                    state: TransferState::Completed,
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
                TransferJob {
                    id: TransferId(2),
                    direction: TransferDirection::Upload,
                    source: TransferEndpoint::Local(LocalPath::new("/tmp/b.txt")),
                    destination: TransferEndpoint::Remote(RemotePath::new("/srv/b.txt")),
                    state: TransferState::Running {
                        bytes_done: 0,
                        bytes_total: Some(1024),
                        started_at: Timestamp::from_secs_since_epoch(0),
                    },
                    metadata_policy: MetadataPolicy::default(),
                    conflict_policy: ConflictPolicy::Ask,
                    warnings: Vec::new(),
                    created_at: Timestamp::from_secs_since_epoch(0),
                },
            ],
            plans: vec![TransferPlan {
                id: TransferPlanId(1),
                root_job_id: TransferId(9),
                source_root: TransferEndpoint::Local(LocalPath::new("/tmp")),
                destination_root: TransferEndpoint::Remote(RemotePath::new("/srv")),
                state: TransferPlanState::Queued,
                planned_count: 2,
                total_bytes: Some(20),
                child_jobs: vec![TransferId(1), TransferId(2)],
                conflict_policy: ConflictPolicy::Ask,
            }],
            pending_conflicts: Vec::new(),
        };

        store.clear_terminal();
        assert_eq!(store.jobs.len(), 1);
        assert_eq!(store.plans.len(), 1, "plan kept because job 2 still exists");
    }

    #[test]
    fn clear_terminal_returns_false_when_nothing_to_clear() {
        let mut store = TransferStore {
            jobs: vec![TransferJob {
                id: TransferId(1),
                direction: TransferDirection::Upload,
                source: TransferEndpoint::Local(LocalPath::new("/tmp/a.txt")),
                destination: TransferEndpoint::Remote(RemotePath::new("/srv/a.txt")),
                state: TransferState::Queued,
                metadata_policy: MetadataPolicy::default(),
                conflict_policy: ConflictPolicy::Ask,
                warnings: Vec::new(),
                created_at: Timestamp::from_secs_since_epoch(0),
            }],
            plans: Vec::new(),
            pending_conflicts: Vec::new(),
        };

        assert!(!store.clear_terminal());
    }
```

- [ ] **Step 3: Run tests**

```bash
cargo test -p macsftp-core -- clear_terminal
```

Expected: 3 tests PASS

- [ ] **Step 4: Commit**

```bash
git add crates/core/src/core.rs
git commit -m "feat: add TransferStore::clear_terminal() to remove completed/failed transfer records"
```

---

### Task 3: ActiveTransfers::clear_terminal_transfers()

**Files:**
- Modify: `crates/app/src/resources.rs:161-173` (trait definition) and `crates/app/src/resources.rs:227-241` (impl block end)

**Interfaces:**
- Consumes: `TransferStore::clear_terminal()`, `TransferRateBook::clear()`
- Produces: `pub fn clear_terminal_transfers(&mut self) -> bool` on `ActiveTransfers` trait

- [ ] **Step 1: Add trait method declaration**

In `crates/app/src/resources.rs`, add after `remove_transfer_conflict` (after line 166):

```rust
    fn clear_terminal_transfers(&mut self) -> bool;
```

- [ ] **Step 2: Add impl for App**

In `crates/app/src/resources.rs`, add after the `remove_transfer_conflict` impl block (after line 228):

```rust

    fn clear_terminal_transfers(&mut self) -> bool {
        let transfers = self.global_mut::<SharedTransfers>();
        let removed = transfers.store.clear_terminal();
        if removed {
            let terminal_ids: Vec<TransferId> = transfers
                .store
                .jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        TransferState::Completed
                            | TransferState::Skipped
                            | TransferState::Failed { .. }
                    )
                })
                .map(|job| job.id)
                .collect();
            for id in &terminal_ids {
                transfers.rates.clear(*id);
            }
            self.refresh_windows();
        }
        removed
    }
```

Wait — the `clear_terminal` method already removes the terminal jobs from the store. So after it runs, the store won't have terminal jobs anymore. We should clear the rates for the ids that *were* just removed. Let me adjust: `clear_terminal` returns true if anything changed, and the store's jobs are now clean. We should clear all rates since any rate sampler whose id no longer has a job in the store is orphaned. Simpler approach:

- [ ] **Step 2 (revised): Add impl for App**

In `crates/app/src/resources.rs`, add after the `remove_transfer_conflict` impl block (after line 228):

```rust

    fn clear_terminal_transfers(&mut self) -> bool {
        let transfers = self.global_mut::<SharedTransfers>();
        let removed = transfers.store.clear_terminal();
        if removed {
            let remaining_ids: std::collections::HashSet<TransferId> =
                transfers.store.jobs.iter().map(|job| job.id).collect();
            let rate_ids: Vec<TransferId> = transfers
                .rates
                .sampler_ids()
                .into_iter()
                .filter(|id| !remaining_ids.contains(id))
                .collect();
            for id in rate_ids {
                transfers.rates.clear(id);
            }
            self.refresh_windows();
        }
        removed
    }
```

Hmm, `TransferRateBook` doesn't expose a `sampler_ids()` method. Let me reconsider. The simplest correct approach: since `clear_terminal` already removed terminal jobs, we just need to clear all rate samplers. But `TransferRateBook` only has `clear(id)`, `observe`, `snapshot`, and `aggregate` — no `clear_all`.

I'll add a `clear_orphaned` method or just iterate all known ids. Actually, the simplest: after `clear_terminal`, all remaining jobs are non-terminal. Any rate sampler whose id isn't in the remaining jobs is orphaned. But we need to get the sampler ids.

Let me check `TransferRateBook`.

Actually, let me just add a method to `TransferRateBook` or keep it simple. Since the rate book is a `HashMap<TransferId, RateSampler>`, I can add a `retain` method or just check the existing store job ids. The simplest: store job ids before clearing, then clear rates for those that were removed.

Revised approach:

```rust
    fn clear_terminal_transfers(&mut self) -> bool {
        let transfers = self.global_mut::<SharedTransfers>();
        let terminal_ids: Vec<TransferId> = transfers
            .store
            .jobs
            .iter()
            .filter(|job| {
                matches!(
                    job.state,
                    TransferState::Completed
                        | TransferState::Skipped
                        | TransferState::Failed { .. }
                )
            })
            .map(|job| job.id)
            .collect();
        let removed = transfers.store.clear_terminal();
        if removed {
            for id in &terminal_ids {
                transfers.rates.clear(*id);
            }
            self.refresh_windows();
        }
        removed
    }
```

Wait no — `clear_terminal()` takes `&mut self` and so does `self.global_mut`, and `terminal_ids` holds an immutable borrow. Rust borrow checker issue. Let me use a different approach: collect terminal ids first (via an immutable borrow of the shared state), then mutable-borrow and clear.

Actually, `self.global::<SharedTransfers>()` returns an immutable reference. But `global_mut` requires mutable. I need to scope it:

```rust
    fn clear_terminal_transfers(&mut self) -> bool {
        let terminal_ids: Vec<TransferId> = {
            let store = &self.global::<SharedTransfers>().store;
            store.jobs.iter()
                .filter(|job| matches!(job.state,
                    TransferState::Completed | TransferState::Skipped | TransferState::Failed { .. }
                ))
                .map(|job| job.id)
                .collect()
        };
        let removed = self.global_mut::<SharedTransfers>().store.clear_terminal();
        if removed {
            let transfers = self.global_mut::<SharedTransfers>();
            for id in &terminal_ids {
                transfers.rates.clear(*id);
            }
            self.refresh_windows();
        }
        removed
    }
```

This works. Let me finalize the plan with this approach.

- [ ] **Step 2 (final): Add impl for App**

In `crates/app/src/resources.rs`, add after the `remove_transfer_conflict` impl block (after line 228):

```rust

    fn clear_terminal_transfers(&mut self) -> bool {
        let terminal_ids: Vec<TransferId> = {
            let store = &self.global::<SharedTransfers>().store;
            store
                .jobs
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        TransferState::Completed
                            | TransferState::Skipped
                            | TransferState::Failed { .. }
                    )
                })
                .map(|job| job.id)
                .collect()
        };
        let removed = self.global_mut::<SharedTransfers>().store.clear_terminal();
        if removed {
            let transfers = self.global_mut::<SharedTransfers>();
            for id in &terminal_ids {
                transfers.rates.clear(*id);
            }
            self.refresh_windows();
        }
        removed
    }
```

- [ ] **Step 3: Check compilation**

```bash
cargo check -p macsftp-app
```

Expected: compiles without errors

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/resources.rs
git commit -m "feat: add clear_terminal_transfers() to ActiveTransfers trait"
```

---

### Task 4: Workspace methods + UI buttons

**Files:**
- Modify: `crates/app/src/workspace/transfers.rs` (append two new methods)
- Modify: `crates/app/src/workspace/transfer_render.rs:209-230` (insert buttons in header)

**Interfaces:**
- Consumes: `visible_transfer_jobs()`, `ActiveTransfers`, `cancel_transfer()`, `icon_button()`
- Produces: `cancel_all_transfers()`, `clear_transfer_records()`, two header buttons

- [ ] **Step 1: Add Workspace methods**

Append to `crates/app/src/workspace/transfers.rs` (after `clean_remote_residual_temps`, before the closing `}`):

```rust
    pub(crate) fn cancel_all_transfers(&mut self, cx: &mut Context<Self>) {
        let job_ids: Vec<macsftp_core::TransferId> =
            visible_transfer_jobs(cx.transfers())
                .iter()
                .filter(|job| {
                    matches!(
                        job.state,
                        macsftp_core::TransferState::Queued
                            | macsftp_core::TransferState::Running { .. }
                            | macsftp_core::TransferState::Planning
                            | macsftp_core::TransferState::WaitingForConflictDecision { .. }
                    )
                })
                .map(|job| job.id)
                .collect();
        for id in job_ids {
            self.cancel_transfer(id, cx);
        }
    }
    pub(crate) fn clear_transfer_records(&mut self, cx: &mut Context<Self>) {
        if cx.clear_terminal_transfers() {
            self.completed_section_expanded = false;
            self.failed_section_expanded = false;
            cx.notify();
        }
    }
```

- [ ] **Step 2: Add UI buttons in drawer header**

In `crates/app/src/workspace/transfer_render.rs`, modify the header div (lines 209-230). Replace the current header block starting at line 209 (`drawer = drawer.child(div()...agg_label)...` through 230) with:

```rust
        let workspace_entity = cx.entity();
        drawer = drawer.child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .h(px(28.0))
                .px_2()
                .min_w_0()
                .text_size(px(11.0))
                .text_color(theme.colors.text_muted)
                .child(icon(IconName::Transfers, theme.colors.text_muted))
                .child(div().flex_none().child("Transfers"))
                .when(active_jobs.len() + queued_jobs.len() > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button(
                            "cancel-all-transfers",
                            IconName::Close,
                            "Cancel All Transfers",
                        )
                        .icon_color(theme.colors.warning)
                        .on_click(move |_event, _window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.cancel_all_transfers(cx);
                            });
                        }),
                    )
                })
                .when(completed_jobs.len() + failed_jobs.len() > 0, |header| {
                    let workspace = workspace_entity.clone();
                    header.child(
                        icon_button(
                            "clear-transfer-records",
                            IconName::Trash,
                            "Clear Transfer History",
                        )
                        .on_click(move |_event, _window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.clear_transfer_records(cx);
                            });
                        }),
                    )
                })
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_right()
                        .child(agg_label),
                ),
        );
```

Note: The `workspace_entity` clone is already available at line 68. We need to move it before the header block to use it in the closures. The existing clone at line 68 should be moved up or reused. Check the existing code — `workspace_entity` is already defined at line 68.

- [ ] **Step 3: Check compilation**

```bash
cargo check -p macsftp-app
```

Expected: compiles without errors

- [ ] **Step 4: Run tests**

```bash
cargo test -p macsftp-core -- clear_terminal
cargo test -p macsftp-app -- transfer
```

Expected: all tests PASS

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/transfers.rs crates/app/src/workspace/transfer_render.rs
git commit -m "feat: add Cancel All and Clear Records buttons to transfer drawer header"
```

---

### Task 5: Integration verification

- [ ] **Step 1: Full build check**

```bash
cargo check
```

Expected: no errors across all crates

- [ ] **Step 2: Run all related tests**

```bash
cargo test -p macsftp-core
cargo test -p macsftp-app -- assets
cargo test -p macsftp-app -- transfer
```

Expected: all tests PASS

- [ ] **Step 3: Clippy**

```bash
cargo clippy -p macsftp-core -p macsftp-app -- -D warnings
```

Expected: no warnings

- [ ] **Step 4: Final commit (if any fixups)**

```bash
git add -A
git commit -m "chore: final verification pass for cancel-all and clear-records"
```
