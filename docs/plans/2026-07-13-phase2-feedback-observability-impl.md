# Phase 2 Feedback & Observability Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace transfer speed/ETA placeholders with real view-side sampling, make remote loading/connecting/errors in-place and recoverable, and finish status-bar transfer discoverability.

**Architecture:** Pure view-side `TransferRateBook` (sliding-window samplers keyed by `TransferId`) lives next to shared transfers. Progress events update samples; render derives MB/s, ETA, and Stalled. Loading uses a centered spinner; connect Cancel reuses `DisconnectTab`; directory errors keep pane Retry via existing `request_remote_directory`.

**Tech Stack:** Rust, GPUI (`crates/app`, `crates/ui`), existing `macsftp_core::{TransferId, TransferState, TransferProgress}`, no new crates.

**Spec:** `docs/plans/2026-07-13-phase2-feedback-observability-design.md`

## Global Constraints

- Do **not** add rate/ETA fields to `core` `TransferProgress` / `TransferState` (design decision 1).
- Do **not** implement skeleton rows, Fs command replay, or command palette.
- No `unwrap`/`expect` on recoverable paths; no silent `let _ =` on fallible ops (AGENTS.md §5).
- Progress remains throttled at runtime source; UI only aggregates received samples.
- Stalled transfers must show plain "Stalled" text — never fake progress animation (guidelines §7).
- Match existing workspace style: `crates/app/src/workspace/*.rs`, `#[cfg(test)]` colocated or in `tests.rs`.
- Prefer `src/foo.rs` over `mod.rs` (AGENTS.md).

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/rate_sampler.rs` | `RateSample`, `RateSampler`, `TransferRateBook`, formatters, pure unit tests |
| **Modify** `crates/app/src/workspace/mod.rs` | `mod rate_sampler;` |
| **Modify** `crates/app/src/resources.rs` | `SharedTransfers` holds `TransferRateBook`; accessors for rates |
| **Modify** `crates/app/src/workspace/event_handling.rs` | On progress/terminal transfer events, update/clear rates |
| **Modify** `crates/app/src/workspace/render.rs` | Running detail, drawer aggregate, first-load spinner, Cancel connect, status selected count + failed color |
| **Modify** `crates/app/src/workspace/panes.rs` or **modals** / **helpers** | `cancel_connect` helper if not inlined in render listeners |
| **Modify** `crates/ui/src/components.rs` (+ `ui.rs` re-export) | Optional `loading_indicator` / spinner-friendly empty_state helper |
| **Modify** `crates/app/src/workspace/tests.rs` | Cancel connect, rate wiring smoke, status selection |
| **Do not modify** | `crates/core` progress types, `session_actor` progress payload, phase-1 Fs command path |

---

### Task 1: RateSampler pure module (TDD)

**Files:**
- Create: `crates/app/src/workspace/rate_sampler.rs`
- Modify: `crates/app/src/workspace/mod.rs` (add `mod rate_sampler;`)
- Test: unit tests inside `rate_sampler.rs`

**Interfaces:**
- Produces:
  - `pub struct TransferRateBook` with `Default`
  - `pub fn observe(&mut self, id: TransferId, bytes_done: u64, now: Instant)`
  - `pub fn clear(&mut self, id: TransferId)`
  - `pub fn snapshot(&self, id: TransferId, bytes_done: u64, bytes_total: Option<u64>, now: Instant) -> RateSnapshot`
  - `pub fn aggregate(&self, running: &[(TransferId, u64, Option<u64>)], now: Instant) -> AggregateRate`
  - `pub struct RateSnapshot { pub speed_bps: Option<f64>, pub stalled: bool, pub eta_secs: Option<f64> }`
  - `pub struct AggregateRate { pub speed_bps: Option<f64>, pub eta_secs: Option<f64> }`
  - `pub fn format_speed(bps: Option<f64>) -> String`
  - `pub fn format_eta(secs: Option<f64>) -> String`
  - `pub fn format_running_detail(done: u64, total: Option<u64>, snap: &RateSnapshot) -> String`
- Consumes: `macsftp_core::TransferId`, `std::time::Instant`, `std::collections::{HashMap, VecDeque}`

**Constants (export for tests):**

```rust
pub const WINDOW_SECS: f64 = 4.0;
pub const WARMUP_SECS: f64 = 0.5;
pub const STALL_SECS: f64 = 3.0;
```

- [ ] **Step 1: Create module skeleton and failing tests**

Add to `crates/app/src/workspace/mod.rs` near other `mod` lines:

```rust
mod rate_sampler;
```

Create `rate_sampler.rs` with tests first (types can be minimal stubs that fail assertions until implemented):

```rust
//! View-side sliding-window transfer rate / ETA (phase 2).
//! Not part of core protocol — see design doc §2.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use macsftp_core::TransferId;

pub const WINDOW_SECS: f64 = 4.0;
pub const WARMUP_SECS: f64 = 0.5;
pub const STALL_SECS: f64 = 3.0;

// ... implement after tests compile ...

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn id(n: u64) -> TransferId {
        TransferId(n)
    }

    #[test]
    fn speed_uses_window_endpoints() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 1_000_000, t0 + Duration::from_secs(1));
        let snap = book.snapshot(id(1), 1_000_000, Some(10_000_000), t0 + Duration::from_secs(1));
        assert!(snap.speed_bps.unwrap() > 900_000.0 && snap.speed_bps.unwrap() < 1_100_000.0);
        assert!(!snap.stalled);
        assert!(snap.eta_secs.unwrap() > 8.0 && snap.eta_secs.unwrap() < 12.0);
    }

    #[test]
    fn warmup_yields_no_speed() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 100, t0 + Duration::from_millis(100));
        let snap = book.snapshot(id(1), 100, Some(1000), t0 + Duration::from_millis(100));
        assert!(snap.speed_bps.is_none());
        assert!(!snap.stalled);
    }

    #[test]
    fn stalled_when_bytes_unchanged_past_threshold() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 500, t0);
        book.observe(id(1), 500, t0 + Duration::from_secs(1));
        book.observe(id(1), 500, t0 + Duration::from_secs(4));
        let snap = book.snapshot(id(1), 500, Some(1000), t0 + Duration::from_secs(4));
        assert!(snap.stalled);
        assert!(snap.eta_secs.is_none());
    }

    #[test]
    fn clear_removes_sampler() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.clear(id(1));
        let snap = book.snapshot(id(1), 0, Some(100), t0 + Duration::from_secs(2));
        assert!(snap.speed_bps.is_none());
    }

    #[test]
    fn format_running_detail_stalled_and_normal() {
        let stalled = RateSnapshot {
            speed_bps: Some(0.0),
            stalled: true,
            eta_secs: None,
        };
        let s = format_running_detail(1_000_000, Some(2_000_000), &stalled);
        assert!(s.contains("Stalled"), "{s}");
        assert!(!s.contains("— MB/s") || s.contains("Stalled"));

        let normal = RateSnapshot {
            speed_bps: Some(1_048_576.0),
            stalled: false,
            eta_secs: Some(10.0),
        };
        let s = format_running_detail(1_000_000, Some(2_000_000), &normal);
        assert!(s.contains("MB/s") || s.contains("KB/s"), "{s}");
        assert!(s.contains("ETA"), "{s}");
        assert!(!s.contains("— MB/s · ETA —"), "{s}");
    }

    #[test]
    fn aggregate_sums_running_speeds() {
        let t0 = Instant::now();
        let mut book = TransferRateBook::default();
        book.observe(id(1), 0, t0);
        book.observe(id(1), 2_000_000, t0 + Duration::from_secs(1));
        book.observe(id(2), 0, t0);
        book.observe(id(2), 2_000_000, t0 + Duration::from_secs(1));
        let now = t0 + Duration::from_secs(1);
        let agg = book.aggregate(
            &[
                (id(1), 2_000_000, Some(10_000_000)),
                (id(2), 2_000_000, Some(10_000_000)),
            ],
            now,
        );
        assert!(agg.speed_bps.unwrap() > 3_500_000.0);
        assert!(agg.eta_secs.is_some());
    }
}
```

- [ ] **Step 2: Run tests — expect compile failure or FAIL**

```bash
cargo test -p macsftp-app --bin macsftp rate_sampler -- --nocapture
```

Expected: compile error (`TransferRateBook` not found) or test FAIL.

- [ ] **Step 3: Implement `rate_sampler.rs`**

Implement at least:

```rust
#[derive(Debug, Clone, Copy)]
pub struct RateSnapshot {
    pub speed_bps: Option<f64>,
    pub stalled: bool,
    pub eta_secs: Option<f64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct AggregateRate {
    pub speed_bps: Option<f64>,
    pub eta_secs: Option<f64>,
}

#[derive(Debug, Clone)]
struct RateSample {
    at: Instant,
    bytes_done: u64,
}

#[derive(Debug, Default)]
struct RateSampler {
    samples: VecDeque<RateSample>,
    last_bytes_change_at: Option<Instant>,
    last_bytes: Option<u64>,
}

#[derive(Debug, Default)]
pub struct TransferRateBook {
    samplers: HashMap<TransferId, RateSampler>,
}

impl TransferRateBook {
    pub fn observe(&mut self, id: TransferId, bytes_done: u64, now: Instant) {
        let sampler = self.samplers.entry(id).or_default();
        if sampler.last_bytes.is_none_or(|b| bytes_done > b) {
            sampler.last_bytes_change_at = Some(now);
            sampler.last_bytes = Some(bytes_done);
        }
        sampler.samples.push_back(RateSample { at: now, bytes_done });
        let cutoff = now - std::time::Duration::from_secs_f64(WINDOW_SECS);
        while sampler
            .samples
            .front()
            .is_some_and(|s| s.at < cutoff && sampler.samples.len() > 2)
        {
            sampler.samples.pop_front();
        }
        // Also drop if only one sample older than window — keep last two max logic simple:
        while let Some(front) = sampler.samples.front() {
            if front.at < cutoff && sampler.samples.len() > 1 {
                sampler.samples.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn clear(&mut self, id: TransferId) {
        self.samplers.remove(&id);
    }

    pub fn snapshot(
        &self,
        id: TransferId,
        bytes_done: u64,
        bytes_total: Option<u64>,
        now: Instant,
    ) -> RateSnapshot {
        let Some(sampler) = self.samplers.get(&id) else {
            return RateSnapshot {
                speed_bps: None,
                stalled: false,
                eta_secs: None,
            };
        };
        let speed_bps = speed_from_samples(&sampler.samples, now);
        let stalled = is_stalled(sampler, speed_bps, now);
        let eta_secs = match (stalled, speed_bps, bytes_total) {
            (true, _, _) | (_, None, _) | (_, Some(s), _) if s <= f64::EPSILON => None,
            (false, Some(speed), Some(total)) if total >= bytes_done => {
                Some((total - bytes_done) as f64 / speed)
            }
            _ => None,
        };
        RateSnapshot {
            speed_bps,
            stalled,
            eta_secs,
        }
    }

    pub fn aggregate(
        &self,
        running: &[(TransferId, u64 /* done */, Option<u64> /* total */)],
        now: Instant,
    ) -> AggregateRate {
        let mut sum_speed = 0.0;
        let mut any_speed = false;
        let mut remaining: u64 = 0;
        let mut any_remaining = false;
        for &(id, done, total) in running {
            let snap = self.snapshot(id, done, total, now);
            if let Some(s) = snap.speed_bps {
                if !snap.stalled {
                    sum_speed += s;
                    any_speed = true;
                }
            }
            if let Some(t) = total {
                remaining = remaining.saturating_add(t.saturating_sub(done));
                any_remaining = true;
            }
        }
        let speed_bps = any_speed.then_some(sum_speed);
        let eta_secs = match (speed_bps, any_remaining) {
            (Some(s), true) if s > f64::EPSILON => Some(remaining as f64 / s),
            _ => None,
        };
        AggregateRate { speed_bps, eta_secs }
    }
}

fn speed_from_samples(samples: &VecDeque<RateSample>, _now: Instant) -> Option<f64> {
    let first = samples.front()?;
    let last = samples.back()?;
    if samples.len() < 2 {
        return None;
    }
    let elapsed = last.at.duration_since(first.at).as_secs_f64();
    if elapsed < WARMUP_SECS {
        return None;
    }
    let delta = last.bytes_done.saturating_sub(first.bytes_done) as f64;
    Some(delta / elapsed)
}

fn is_stalled(sampler: &RateSampler, speed_bps: Option<f64>, now: Instant) -> bool {
    let Some(changed_at) = sampler.last_bytes_change_at else {
        return false;
    };
    let idle = now.duration_since(changed_at).as_secs_f64() >= STALL_SECS;
    if !idle {
        return false;
    }
    match speed_bps {
        None => true, // past stall threshold with no usable speed
        Some(s) => s <= f64::EPSILON,
    }
}

pub fn format_speed(bps: Option<f64>) -> String {
    match bps {
        None => "— MB/s".into(),
        Some(s) if s >= 1_000_000.0 => format!("{:.1} MB/s", s / 1_000_000.0),
        Some(s) => format!("{:.1} KB/s", s / 1000.0),
    }
}

pub fn format_eta(secs: Option<f64>) -> String {
    match secs {
        None => "—".into(),
        Some(s) if s < 60.0 => format!("{}s", s.ceil() as u64),
        Some(s) if s < 3600.0 => {
            let m = (s / 60.0).floor() as u64;
            let sec = (s % 60.0).ceil() as u64;
            format!("{m}m {sec}s")
        }
        Some(s) => {
            let h = (s / 3600.0).floor() as u64;
            let m = ((s % 3600.0) / 60.0).floor() as u64;
            format!("{h}h {m}m")
        }
    }
}

pub fn format_running_detail(done: u64, total: Option<u64>, snap: &RateSnapshot) -> String {
    use macsftp_ui::format_size;
    let done_s = format_size(Some(done)).to_string();
    if snap.stalled {
        return match total {
            Some(t) => format!("{} / {} · Stalled", done_s, format_size(Some(t))),
            None => format!("{done_s} · Stalled"),
        };
    }
    let speed_s = format_speed(snap.speed_bps);
    match total {
        Some(t) => format!(
            "{} / {} · {} · ETA {}",
            done_s,
            format_size(Some(t)),
            speed_s,
            format_eta(snap.eta_secs)
        ),
        None => format!("{done_s} · {speed_s}"),
    }
}
```

**Note:** `format_size` lives in `macsftp_ui`. If linking `macsftp_ui` from unit tests in app is fine (app already depends on ui). If `format_size` needs `&App` — check; current `format_size` is pure in `file_list.rs`. Use that.

If `format_size` returns `SharedString`, convert with `.to_string()` or `as_ref()`.

- [ ] **Step 4: Run tests — expect PASS**

```bash
cargo test -p macsftp-app --bin macsftp rate_sampler -- --nocapture
```

Expected: all `rate_sampler::tests::*` PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/rate_sampler.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): add view-side transfer rate sampler"
```

---

### Task 2: Store rates on SharedTransfers + event hooks

**Files:**
- Modify: `crates/app/src/resources.rs`
- Modify: `crates/app/src/workspace/event_handling.rs`
- Modify: any call sites that construct `SharedTransfers(…)` or access `.0` if API changes
- Test: extend `rate_sampler` usage via a small app-level test or keep coverage in unit tests + manual event_handling paths

**Interfaces:**
- Consumes: `TransferRateBook` from Task 1
- Produces:
  - `SharedTransfers { store: TransferStore, rates: TransferRateBook }`
  - `ActiveTransfers::transfers` / `transfers_mut` still return `TransferStore`
  - `ActiveTransfers::rates` / `rates_mut` → `&TransferRateBook` / `&mut TransferRateBook`

- [ ] **Step 1: Expand `SharedTransfers`**

In `resources.rs`:

```rust
use crate::workspace::rate_sampler::TransferRateBook;
// OR move TransferRateBook to resources/rate_sampler if circular mod issues.
// Prefer: rate_sampler under workspace; resources imports workspace::rate_sampler
// ONLY if resources can depend on workspace — currently resources is parent.
// Circular risk: workspace uses resources.
//
// FIX: put rate_sampler at crates/app/src/rate_sampler.rs (crate root) instead
// if workspace↔resources cycle appears.
```

**If `resources` cannot import `workspace`:** relocate module to `crates/app/src/rate_sampler.rs` and `mod rate_sampler;` in `main.rs` / `lib`-less bin root. Prefer that to avoid cycles:

- Create `crates/app/src/rate_sampler.rs` (move from workspace if needed)
- `main.rs`: `mod rate_sampler;`
- `resources.rs` and `workspace` both `use crate::rate_sampler::…`

**Recommended layout for this task:** move Task 1 file to `crates/app/src/rate_sampler.rs` if not already crate-root. Update `mod` declarations.

```rust
#[derive(Default)]
pub struct SharedTransfers {
    pub store: TransferStore,
    pub rates: crate::rate_sampler::TransferRateBook,
}

impl ActiveTransfers for App {
    fn transfers(&self) -> &TransferStore {
        &self.global::<SharedTransfers>().store
    }
    fn transfers_mut(&mut self) -> &mut TransferStore {
        &mut self.global_mut::<SharedTransfers>().store
    }
    fn rates(&self) -> &crate::rate_sampler::TransferRateBook {
        &self.global::<SharedTransfers>().rates
    }
    fn rates_mut(&mut self) -> &mut crate::rate_sampler::TransferRateBook {
        &mut self.global_mut::<SharedTransfers>().rates
    }
}
```

Update trait definition accordingly. Fix compile errors from tuple field `.0` access.

- [ ] **Step 2: Hook events in `event_handling.rs`**

On `TransferProgress`:

```rust
AppEvent::TransferProgress(progress) => {
    // existing job state update...
    cx.rates_mut().observe(
        progress.transfer_id,
        progress.bytes_done,
        std::time::Instant::now(),
    );
}
```

Also `observe` when applying `TransferRunning` snapshot if state is `Running { bytes_done, .. }`.

On terminal events:

```rust
AppEvent::TransferCompleted { transfer_id }
| AppEvent::TransferSkipped { transfer_id } => {
    cx.rates_mut().clear(transfer_id);
    // existing finalize...
}
AppEvent::TransferFailed(failure) => {
    cx.rates_mut().clear(failure.transfer_id);
    // existing...
}
```

When setting `Cancelling`, optional: leave sampler (display uses Cancelling label, not rate).

- [ ] **Step 3: Compile**

```bash
cargo check -p macsftp-app 2>&1
```

Expected: success.

- [ ] **Step 4: Run existing app tests**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
```

Expected: all prior tests PASS (fix any `.0` / `SharedTransfers` breakage).

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/resources.rs crates/app/src/workspace/event_handling.rs crates/app/src/rate_sampler.rs crates/app/src/main.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): wire transfer rate book into shared transfers"
```

---

### Task 3: Render real speed/ETA on transfer rows

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (`render_transfer_job`, ~lines 807–832)

**Interfaces:**
- Consumes: `cx.rates().snapshot(...)`, `format_running_detail`
- Produces: Running `detail` string without permanent placeholder

- [ ] **Step 1: Replace Running detail branch**

Replace:

```rust
TransferState::Running {
    bytes_done,
    bytes_total,
    ..
} => format!(
    "{} / {} · — MB/s · ETA —",
    ...
)
```

With:

```rust
TransferState::Running {
    bytes_done,
    bytes_total,
    ..
} => {
    let snap = cx.rates().snapshot(
        job_id,
        *bytes_done,
        *bytes_total,
        std::time::Instant::now(),
    );
    crate::rate_sampler::format_running_detail(*bytes_done, *bytes_total, &snap).into()
}
```

Ensure imports for `ActiveTransfers` rates if needed.

- [ ] **Step 2: Build**

```bash
cargo check -p macsftp-app
```

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/workspace/render.rs
git commit -m "feat(app): show live transfer speed and ETA on rows"
```

---

### Task 4: Drawer aggregate header

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (`render_transfer_drawer`)

**Interfaces:**
- Consumes: `TransferRateBook::aggregate`, list of running jobs from `cx.transfers().jobs`

- [ ] **Step 1: Collect running jobs and aggregate**

Near top of `render_transfer_drawer`, after cloning jobs:

```rust
let now = std::time::Instant::now();
let running: Vec<(TransferId, u64, Option<u64>)> = jobs
    .iter()
    .filter_map(|job| match &job.state {
        TransferState::Running {
            bytes_done,
            bytes_total,
            ..
        } => Some((job.id, *bytes_done, *bytes_total)),
        _ => None,
    })
    .collect();
let agg = cx.rates().aggregate(&running, now);
let agg_label = {
    let n = running.len();
    let mut s = format!("{n} active");
    if let Some(bps) = agg.speed_bps {
        s.push_str(&format!(" · {}", crate::rate_sampler::format_speed(Some(bps))));
    }
    if let Some(eta) = agg.eta_secs {
        s.push_str(&format!(
            " · ETA {}",
            crate::rate_sampler::format_eta(Some(eta))
        ));
    }
    s
};
```

- [ ] **Step 2: Show `agg_label` in drawer chrome**

Place next to existing drawer title / section header (top of drawer, one muted line). Do not change row height tokens.

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/workspace/render.rs
git commit -m "feat(app): aggregate transfer speed and ETA in drawer header"
```

---

### Task 5: First-load spinner (remote)

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (list placeholder branch ~entry_count == 0 && is_remote_refreshing)
- Optional: `crates/ui/src/components.rs` + re-export in `ui.rs`

**Interfaces:**
- Produces: first load shows centered spinner + "Loading…"; refresh with entries keeps list + path bar "Refreshing…"

- [ ] **Step 1: Optional UI helper**

If `empty_state` is enough:

```rust
empty_state("Loading…", vec![], cx)
```

Prefer adding a small visual spinner only if cheap — e.g. text `"Loading…"` is acceptable per design decision 2 (spinner + short text). Minimal:

```rust
// crates/ui/src/components.rs
pub fn loading_state(message: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    empty_state(message, vec![], cx)
}
```

Or animate later; **required** copy: **"Loading…"** (not only "Loading directory…").

- [ ] **Step 2: Branch first load vs empty directory**

In `render_pane` list selection logic:

```rust
} else if entry_count == 0 && is_remote_refreshing {
    empty_state("Loading…", vec![], cx).into_any_element()
} else if entry_count == 0 {
    empty_state("Empty directory", vec![], cx).into_any_element()
```

Keep path bar `Refreshing…` when `is_remote_refreshing && entry_count > 0` (already present).

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/ui/src/components.rs crates/ui/src/ui.rs
git commit -m "feat(app): show Loading state for first remote directory fetch"
```

---

### Task 6: Cancel connect

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (Connecting / AwaitingHostKey empty_state)
- Modify: `crates/app/src/workspace/mod.rs` or new method on `Workspace` in `panes.rs` / `helpers`
- Modify: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces: `Workspace::cancel_connect(&mut self, window, cx)`
- Behavior:
  1. If `AwaitingHostKey { request_id, .. }` → `RejectHostKey { request_id }` **and** local disconnect (existing `reject_host_key` path may suffice)
  2. Else → `AppCommand::DisconnectTab { tab_id }` + `tab.disconnect(UserRequested)` + clear remote pane fields
  3. `drain_expired_modals`; focus pane

- [ ] **Step 1: Implement `cancel_connect`**

```rust
// panes.rs or modals.rs
pub(crate) fn cancel_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(tab) = self.active_tab() else { return };
    let tab_id = tab.id;
    match &tab.connection {
        ConnectionState::AwaitingHostKey { request_id, .. } => {
            let request_id = *request_id;
            self.reject_host_key(request_id, window, cx);
            return;
        }
        ConnectionState::Connecting { .. }
        | ConnectionState::Reconnecting { .. } => {}
        _ => return,
    }
    self.send_command(AppCommand::DisconnectTab { tab_id }, cx);
    if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
        tab.disconnect(DisconnectReason::UserRequested);
        tab.remote.entries.clear();
        tab.remote.path = None;
        tab.remote.is_refreshing = false;
        tab.remote.error = None;
    }
    let _ = self.state.drain_expired_modals();
    self.focus_pane(self.focused_side, window, cx);
    cx.notify();
}
```

Import `DisconnectReason`, `AppCommand` as needed.

- [ ] **Step 2: Wire empty_state buttons**

```rust
Some(ConnectionState::Connecting { .. } | ConnectionState::Reconnecting { .. }) => {
    Some(
        empty_state(
            format!("Connecting to {target_host}…"),
            vec![
                text_button("cancel-connect", "Cancel").on_click(cx.listener(
                    |workspace, _e, window, cx| workspace.cancel_connect(window, cx),
                )),
            ],
            cx,
        )
        .into_any_element(),
    )
}
Some(ConnectionState::AwaitingHostKey { .. }) => Some(
    empty_state(
        format!("Waiting for host key · {target_host}"),
        vec![
            text_button("cancel-host-key", "Cancel").on_click(cx.listener(
                |workspace, _e, window, cx| workspace.cancel_connect(window, cx),
            )),
        ],
        cx,
    )
    .into_any_element(),
),
```

- [ ] **Step 3: Test — cancel while connecting**

In `tests.rs`:

```rust
#[gpui::test]
fn cancel_connect_sends_disconnect_and_clears_connecting(cx: &mut TestAppContext) {
    let (workspace, mut cx, channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |workspace, window, cx| {
        // drive into Connecting the same way other tests do (connect_with / begin_connect)
        workspace.connect_with(test_settings(), None, cx);
        assert!(matches!(
            workspace.active_tab().unwrap().connection,
            ConnectionState::Connecting { .. }
        ));
        workspace.cancel_connect(window, cx);
        assert!(matches!(
            workspace.active_tab().unwrap().connection,
            ConnectionState::Disconnected { .. }
        ));
    });
    let cmd = channels
        .command_rx
        .try_iter()
        .find(|c| matches!(c, AppCommand::DisconnectTab { .. } | AppCommand::ConnectTab(_)));
    // Drain: expect DisconnectTab present among commands after ConnectTab
    let mut saw_disconnect = false;
    while let Ok(c) = channels.command_rx.try_recv() {
        if matches!(c, AppCommand::DisconnectTab { .. }) {
            saw_disconnect = true;
        }
    }
    assert!(saw_disconnect, "DisconnectTab must be sent");
}
```

Adjust to match actual `BridgeChannels` API used in existing tests (`channels.command_rx`).

- [ ] **Step 4: Run tests**

```bash
cargo test -p macsftp-app --bin macsftp cancel_connect -- --nocapture
```

Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/panes.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): cancel in-flight connect from remote pane"
```

---

### Task 7: Directory Retry polish + status bar selection / failed color

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (`retry_directory_button`, `render_status_bar`)
- Modify: `crates/app/src/workspace/tests.rs` if new assertions needed

**Interfaces:**
- Retry already calls `refresh_focused_pane` — ensure it clears error / uses `request_remote_directory` for current path when focused side is Remote.
- Status bar: `N selected` when count > 0; failed segment uses error color.

- [ ] **Step 1: Harden Retry**

Change retry button to always target remote path when remote error is shown:

```rust
let retry_directory_button = |id: &'static str| {
    text_button(id, "Retry").on_click(cx.listener(|workspace, _e, window, cx| {
        let Some(tab) = workspace.active_tab() else { return };
        let tab_id = tab.id;
        if let Some(path) = tab.remote.path.clone() {
            workspace.request_remote_directory(tab_id, path, cx);
        } else {
            workspace.focused_side = PaneSide::Remote;
            workspace.refresh_focused_pane(window, cx);
        }
    }))
};
```

Label: **"Retry"** (design) — keep short. Optional secondary still possible later.

On `request_remote_directory`, ensure `remote.error = None` when starting refresh (already sets error None in panes.rs — verify).

- [ ] **Step 2: Status bar selected count**

In `render_status_bar`:

```rust
let selected_count = self.active_tab().map(|tab| {
    tab.selection.selected_paths.iter().filter(|p| match (self.focused_side, p) {
        (PaneSide::Local, EntryPath::Local(_)) => true,
        (PaneSide::Remote, EntryPath::Remote(_)) => true,
        _ => false,
    }).count()
}).unwrap_or(0);

// in left cluster children:
.when(selected_count > 0, |row| {
    row.child(div().child(format!("{selected_count} selected")))
})
```

- [ ] **Step 3: Failed count color**

When building transfer summary child, if `failed_count > 0` render failed portion with `.text_color(theme.colors.error)` (split active/failed into two `div` children).

Keep click toggle + tooltip as today.

- [ ] **Step 4: Test selection label (optional lightweight)**

```rust
#[gpui::test]
fn status_bar_selection_count_tracks_focused_pane(cx: &mut TestAppContext) {
    // set_local_path fixture with files, select 2 local paths, focused_side Local
    // assert via reading selection count helper or status — if hard to assert rendered
    // text, assert a small Workspace helper selected_count_for_status() instead.
}
```

If rendering is hard to assert, add:

```rust
pub(crate) fn focused_selection_count(&self) -> usize { ... }
```

and unit-test that.

- [ ] **Step 5: Full app test suite**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
cargo test -p macsftp-app --bin macsftp rate_sampler 2>&1
```

Expected: all PASS.

- [ ] **Step 6: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/panes.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): polish directory Retry and status bar selection"
```

---

### Task 8: Final verification checklist

- [ ] **Step 1: Automated**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
cargo clippy -p macsftp-app -- -D warnings 2>&1 || cargo clippy -p macsftp-app 2>&1 | tail -40
```

- [ ] **Step 2: Manual smoke (document results in PR if opening one)**

1. Upload/download multi-MB file → row shows non-placeholder speed/ETA  
2. Pause network or stall transfer → **Stalled**  
3. First open remote dir → **Loading…**  
4. Refresh populated dir → list stays, path bar Refreshing…  
5. Connect then Cancel → disconnect, can reconnect  
6. Force bad remote path / permission → Retry reloads  
7. Collapse drawer → status shows active/failed; click opens drawer; select files → "N selected"

- [ ] **Step 3: No leftover permanent placeholder**

```bash
rg "— MB/s · ETA —" crates/app
```

Expected: no matches in non-test production paths (warmup may still produce `— MB/s` via `format_speed(None)` alone — that is OK; the combined permanent placeholder string must not appear as a hard-coded format in `render_transfer_job`).

---

## Self-Review (plan vs spec)

| Spec section | Task |
| --- | --- |
| 2a RateSampler + algorithm + format | Task 1 |
| 2a event observe/clear | Task 2 |
| 2a row detail | Task 3 |
| 2a drawer aggregate | Task 4 |
| 2b first-load spinner/Loading | Task 5 |
| 2b connect Cancel | Task 6 |
| 2c Retry ReadDir | Task 7 (existing + harden) |
| 2d status selected + failed color + click | Task 7 (click already exists) |
| Tests pure + app | Tasks 1, 6, 7, 8 |
| Non-goals (no core rate, no Fs replay, no skeleton) | Global Constraints |

**Placeholder scan:** none intentional.  
**Type consistency:** `TransferRateBook`, `RateSnapshot`, `AggregateRate`, `observe/clear/snapshot/aggregate`, `format_running_detail` used uniformly.

**Layout note:** If `resources` ↔ `workspace` cycle appears, keep `rate_sampler` at `crates/app/src/rate_sampler.rs` (crate root) — Task 2 spells this out.

---

## Execution Handoff

Plan saved to `docs/plans/2026-07-13-phase2-feedback-observability-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session, `executing-plans`, batch with checkpoints  

Which approach?
