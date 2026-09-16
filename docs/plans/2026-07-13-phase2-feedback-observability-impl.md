# 阶段 2 实施计划：反馈与可观测性

> **Agent 实施要求：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，并按任务实施本计划。各步骤使用复选框（`- [ ]`）记录状态。

**目标：** 使用视图侧的实际采样结果替代传输速度和 ETA 占位内容；同时在当前位置显示远程加载、连接和错误状态，并允许用户恢复操作；此外，完善状态栏中的传输状态提示。

**架构：** 纯视图侧的 `TransferRateBook` 与共享传输状态位于同一层，其中滑动窗口采样器以 `TransferId` 为键。进度事件更新采样数据，因此渲染层可以计算 MB/s、ETA 和 Stalled 状态。加载状态使用居中的 spinner；连接取消操作复用 `DisconnectTab`；目录错误继续在 pane 中提供 Retry，并复用现有的 `request_remote_directory`。

**技术栈：** Rust、GPUI（`crates/app`、`crates/ui`）以及现有的 `macsftp_core::{TransferId, TransferState, TransferProgress}`，并且不增加新 crate。

**规格文档：** `docs/plans/2026-07-13-phase2-feedback-observability-design.md`

## 全局约束

- 不得向 `core` 的 `TransferProgress` / `TransferState` 增加 rate/ETA 字段，这是设计决策 1 的要求。
- 不得实现 skeleton rows、Fs 命令重放或 command palette。
- 可恢复路径不得使用 `unwrap`/`expect`，而且 fallible operation 不得通过 `let _ =` 静默忽略结果，详情见 AGENTS.md §5。
- 进度事件继续由 runtime 源头节流，因此 UI 仅聚合已经收到的采样数据。
- 停滞的传输必须显示纯文本 "Stalled"，不得使用虚假进度动画，详情见 guidelines §7。
- 实现应符合现有 workspace 风格：文件位于 `crates/app/src/workspace/*.rs`，而 `#[cfg(test)]` 测试与实现同文件或位于 `tests.rs`。
- 根据 AGENTS.md，优先使用 `src/foo.rs`，而不是 `mod.rs`。

## 文件清单

| 文件 | 职责 |
| --- | --- |
| **新建** `crates/app/src/workspace/rate_sampler.rs` | 定义 `RateSample`、`RateSampler`、`TransferRateBook`、格式化函数和纯单元测试 |
| **修改** `crates/app/src/workspace/mod.rs` | 增加 `mod rate_sampler;` |
| **修改** `crates/app/src/resources.rs` | 由 `SharedTransfers` 保存 `TransferRateBook`，并提供 rate 访问方法 |
| **修改** `crates/app/src/workspace/event_handling.rs` | 在进度和终态传输事件中更新或清除 rate |
| **修改** `crates/app/src/workspace/render.rs` | 实现 Running 详情、drawer 聚合、首次加载 spinner、连接取消、状态栏选中数量和失败颜色 |
| **修改** `crates/app/src/workspace/panes.rs` 或 **modals** / **helpers** | 如果 render listener 不内联逻辑，则提供 `cancel_connect` 辅助方法 |
| **修改** `crates/ui/src/components.rs`（并从 `ui.rs` 重新导出） | 可选的 `loading_indicator` 或适合 spinner 的 empty_state 辅助方法 |
| **修改** `crates/app/src/workspace/tests.rs` | 验证连接取消、rate 连接关系和状态栏选择状态 |
| **不得修改** | `crates/core` 进度类型、`session_actor` 进度 payload、阶段 1 Fs 命令路径 |

---

### 任务 1：RateSampler 纯模块（TDD）

**文件：**
- 新建：`crates/app/src/workspace/rate_sampler.rs`
- 修改：`crates/app/src/workspace/mod.rs`，增加 `mod rate_sampler;`
- 测试：在 `rate_sampler.rs` 中编写单元测试

**接口：**
- 提供：
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
- 使用：`macsftp_core::TransferId`、`std::time::Instant`、`std::collections::{HashMap, VecDeque}`

**常量（为了测试而导出）：**

```rust
pub const WINDOW_SECS: f64 = 4.0;
pub const WARMUP_SECS: f64 = 0.5;
pub const STALL_SECS: f64 = 3.0;
```

- [ ] **步骤 1：创建模块框架和预期失败的测试**

在 `crates/app/src/workspace/mod.rs` 的其他 `mod` 声明附近增加：

```rust
mod rate_sampler;
```

创建 `rate_sampler.rs`，并且先编写测试。在实现完成前，类型可以使用能够通过编译但无法通过断言的最小存根：

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

- [ ] **步骤 2：运行测试，并确认出现编译错误或测试失败**

```bash
cargo test -p macsftp-app --bin macsftp rate_sampler -- --nocapture
```

预期结果：出现编译错误（未找到 `TransferRateBook`）或测试失败。

- [ ] **步骤 3：实现 `rate_sampler.rs`**

实现至少应包含以下内容：

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

**说明：** `format_size` 位于 `macsftp_ui`。app 已经依赖 ui，因此单元测试可以链接 `macsftp_ui`。但是仍需确认 `format_size` 是否需要 `&App`；当前 `file_list.rs` 中的 `format_size` 是纯函数，因此应使用该函数。

如果 `format_size` 返回 `SharedString`，那么使用 `.to_string()` 或 `as_ref()` 转换。

- [ ] **步骤 4：运行测试，并确认测试通过**

```bash
cargo test -p macsftp-app --bin macsftp rate_sampler -- --nocapture
```

预期结果：所有 `rate_sampler::tests::*` 测试均通过。

- [ ] **步骤 5：提交**

```bash
git add crates/app/src/workspace/rate_sampler.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): add view-side transfer rate sampler"
```

---

### 任务 2：在 SharedTransfers 中保存 rate，并处理相关事件

**文件：**
- 修改：`crates/app/src/resources.rs`
- 修改：`crates/app/src/workspace/event_handling.rs`
- 修改：如果 API 发生变化，则修改所有创建 `SharedTransfers(…)` 或访问 `.0` 的调用位置
- 测试：通过小型 app 层测试扩展 `rate_sampler` 使用场景，或者保留单元测试覆盖并人工检查 event_handling 路径

**接口：**
- 使用：任务 1 中的 `TransferRateBook`
- 提供：
  - `SharedTransfers { store: TransferStore, rates: TransferRateBook }`
  - `ActiveTransfers::transfers` / `transfers_mut` 继续返回 `TransferStore`
  - `ActiveTransfers::rates` / `rates_mut` → `&TransferRateBook` / `&mut TransferRateBook`

- [ ] **步骤 1：扩展 `SharedTransfers`**

在 `resources.rs` 中增加：

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

**如果 `resources` 无法导入 `workspace`：** 将模块移至 `crates/app/src/rate_sampler.rs`，并且在 `main.rs` 或没有 lib 的二进制 crate root 中声明 `mod rate_sampler;`。该位置可以避免循环依赖，因此应优先采用：

- 创建 `crates/app/src/rate_sampler.rs`；如有必要，从 workspace 移动现有文件
- 在 `main.rs` 中声明 `mod rate_sampler;`
- `resources.rs` 和 `workspace` 均使用 `use crate::rate_sampler::…`

**本任务推荐的文件位置：** 如果任务 1 的文件尚未位于 crate root，则将其移至 `crates/app/src/rate_sampler.rs`，并且更新 `mod` 声明。

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

同时更新 trait 定义。如果 tuple 字段 `.0` 的访问方式引发编译错误，那么修改对应调用位置。

- [ ] **步骤 2：在 `event_handling.rs` 中处理事件**

收到 `TransferProgress` 时执行：

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

应用 `TransferRunning` 快照时，如果状态为 `Running { bytes_done, .. }`，那么也需要调用 `observe`。

收到终态事件时执行：

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

状态设置为 `Cancelling` 时，可以保留 sampler，因为界面显示 Cancelling 标签，而不显示 rate。

- [ ] **步骤 3：编译检查**

```bash
cargo check -p macsftp-app 2>&1
```

预期结果：检查成功。

- [ ] **步骤 4：运行现有 app 测试**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
```

预期结果：此前的所有测试均通过。如果 `.0` / `SharedTransfers` 变化导致失败，则修改对应代码。

- [ ] **步骤 5：提交**

```bash
git add crates/app/src/resources.rs crates/app/src/workspace/event_handling.rs crates/app/src/rate_sampler.rs crates/app/src/main.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): wire transfer rate book into shared transfers"
```

---

### 任务 3：在传输行中显示实际速度和 ETA

**文件：**
- 修改：`crates/app/src/workspace/render.rs`（`render_transfer_job`，约第 807–832 行）

**接口：**
- 使用：`cx.rates().snapshot(...)`、`format_running_detail`
- 提供：不含永久占位内容的 Running `detail` 字符串

- [ ] **步骤 1：替换 Running 详情分支**

替换以下代码：

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

替换为：

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

如果访问 rate 需要 `ActiveTransfers`，那么增加对应 import。

- [ ] **步骤 2：编译检查**

```bash
cargo check -p macsftp-app
```

- [ ] **步骤 3：提交**

```bash
git add crates/app/src/workspace/render.rs
git commit -m "feat(app): show live transfer speed and ETA on rows"
```

---

### 任务 4：Drawer 聚合标题区

**文件：**
- 修改：`crates/app/src/workspace/render.rs`（`render_transfer_drawer`）

**接口：**
- 使用：`TransferRateBook::aggregate`，以及来自 `cx.transfers().jobs` 的 running job 列表

- [ ] **步骤 1：汇总 running job 并计算聚合数据**

在 `render_transfer_drawer` 前部复制 jobs 后，增加以下代码：

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

- [ ] **步骤 2：在 drawer 顶部区域显示 `agg_label`**

将其置于现有 drawer 标题或 section header 附近，并在 drawer 顶部使用一行弱化文本显示。但是不得修改行高 token。

- [ ] **步骤 3：提交**

```bash
git add crates/app/src/workspace/render.rs
git commit -m "feat(app): aggregate transfer speed and ETA in drawer header"
```

---

### 任务 5：远程目录首次加载 spinner

**文件：**
- 修改：`crates/app/src/workspace/render.rs`（约位于 `entry_count == 0 && is_remote_refreshing` 的列表占位分支）
- 可选修改：`crates/ui/src/components.rs`，并且在 `ui.rs` 中重新导出

**接口：**
- 提供：首次加载显示居中的 spinner 和 "Loading…"；已有条目时刷新仍显示列表，并在 path bar 中显示 "Refreshing…"

- [ ] **步骤 1：按需增加 UI 辅助函数**

如果 `empty_state` 已经满足要求，则使用：

```rust
empty_state("Loading…", vec![], cx)
```

仅当实现成本较低时，才增加小型可视 spinner。根据设计决策 2（spinner 和简短文本），仅显示 `"Loading…"` 文本也可以接受。最小实现如下：

```rust
// crates/ui/src/components.rs
pub fn loading_state(message: impl Into<SharedString>, cx: &App) -> impl IntoElement {
    empty_state(message, vec![], cx)
}
```

动画可以在后续阶段实现；但是文案必须包含 **"Loading…"**，不能仅使用 "Loading directory…"。

- [ ] **步骤 2：区分首次加载与空目录**

在 `render_pane` 的列表选择逻辑中增加：

```rust
} else if entry_count == 0 && is_remote_refreshing {
    empty_state("Loading…", vec![], cx).into_any_element()
} else if entry_count == 0 {
    empty_state("Empty directory", vec![], cx).into_any_element()
```

当 `is_remote_refreshing && entry_count > 0` 时，继续在 path bar 中显示 `Refreshing…`，现有代码已经提供该行为。

- [ ] **步骤 3：提交**

```bash
git add crates/app/src/workspace/render.rs crates/ui/src/components.rs crates/ui/src/ui.rs
git commit -m "feat(app): show Loading state for first remote directory fetch"
```

---

### 任务 6：取消连接

**文件：**
- 修改：`crates/app/src/workspace/render.rs`（Connecting / AwaitingHostKey empty_state）
- 修改：`crates/app/src/workspace/mod.rs`，或者在 `panes.rs` / `helpers` 中为 `Workspace` 增加新方法
- 修改：`crates/app/src/workspace/tests.rs`

**接口：**
- 提供：`Workspace::cancel_connect(&mut self, window, cx)`
- 行为：
  1. 如果状态为 `AwaitingHostKey { request_id, .. }`，则执行 `RejectHostKey { request_id }` 和本地 disconnect；现有 `reject_host_key` 路径可能已经满足要求。
  2. 否则，执行 `AppCommand::DisconnectTab { tab_id }` 和 `tab.disconnect(UserRequested)`，并清除 remote pane 字段。
  3. 执行 `drain_expired_modals`，然后将焦点设置到 pane。

- [ ] **步骤 1：实现 `cancel_connect`**

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

根据需要导入 `DisconnectReason` 和 `AppCommand`。

- [ ] **步骤 2：为 empty_state 按钮配置事件**

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

- [ ] **步骤 3：测试连接过程中的取消操作**

在 `tests.rs` 中增加：

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

根据现有测试使用的实际 `BridgeChannels` API（`channels.command_rx`）调整代码。

- [ ] **步骤 4：运行测试**

```bash
cargo test -p macsftp-app --bin macsftp cancel_connect -- --nocapture
```

预期结果：测试通过。

- [ ] **步骤 5：提交**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/panes.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): cancel in-flight connect from remote pane"
```

---

### 任务 7：完善目录 Retry、状态栏选择数量和失败颜色

**文件：**
- 修改：`crates/app/src/workspace/render.rs`（`retry_directory_button`、`render_status_bar`）
- 修改：如果需要新增断言，则修改 `crates/app/src/workspace/tests.rs`

**接口：**
- Retry 已经调用 `refresh_focused_pane`。因此需要确认 focused side 为 Remote 时，该操作会清除错误，并通过 `request_remote_directory` 请求当前路径。
- 状态栏：数量大于 0 时显示 `N selected`，并且失败信息区域使用错误颜色。

- [ ] **步骤 1：完善 Retry 行为**

当界面显示远程错误时，修改 retry 按钮，使其始终以远程路径为目标：

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

按照设计要求，标签使用简短的 **"Retry"**。后续仍可以增加可选的次要操作。

调用 `request_remote_directory` 并开始刷新时，需要确认 `remote.error = None`。`panes.rs` 中的现有代码已经设置 error None，但是仍需验证。

- [ ] **步骤 2：在状态栏显示选中数量**

在 `render_status_bar` 中增加：

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

- [ ] **步骤 3：设置失败数量的颜色**

创建 transfer summary 子元素时，如果 `failed_count > 0`，则使用 `.text_color(theme.colors.error)` 渲染失败信息区域，并将 active 与 failed 分为两个 `div` 子元素。

保留现有的单击切换行为和 tooltip。

- [ ] **步骤 4：测试选择数量标签（可选的轻量测试）**

```rust
#[gpui::test]
fn status_bar_selection_count_tracks_focused_pane(cx: &mut TestAppContext) {
    // set_local_path fixture with files, select 2 local paths, focused_side Local
    // assert via reading selection count helper or status — if hard to assert rendered
    // text, assert a small Workspace helper selected_count_for_status() instead.
}
```

如果难以断言渲染结果，那么增加：

```rust
pub(crate) fn focused_selection_count(&self) -> usize { ... }
```

并且为该方法编写单元测试。

- [ ] **步骤 5：运行完整 app 测试套件**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
cargo test -p macsftp-app --bin macsftp rate_sampler 2>&1
```

预期结果：所有测试均通过。

- [ ] **步骤 6：提交**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/panes.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): polish directory Retry and status bar selection"
```

---

### 任务 8：最终验证清单

- [ ] **步骤 1：自动验证**

```bash
cargo test -p macsftp-app --bin macsftp 2>&1
cargo clippy -p macsftp-app -- -D warnings 2>&1 || cargo clippy -p macsftp-app 2>&1 | tail -40
```

- [ ] **步骤 2：人工冒烟验证（如果创建 PR，则在 PR 中记录结果）**

1. 上传或下载数 MB 的文件 → 传输行显示非占位的 speed/ETA
2. 暂停网络或使传输停滞 → 显示 **Stalled**
3. 首次访问远程目录 → 显示 **Loading…**
4. 刷新已有条目的目录 → 列表保持显示，并且 path bar 显示 Refreshing…
5. 开始连接后选择 Cancel → 连接终止，并且可以重新连接
6. 使用无效的远程路径或不足的权限 → Retry 重新加载目录
7. 折叠 drawer → 状态栏显示 active/failed；单击后显示 drawer；选择文件后显示 "N selected"

- [ ] **步骤 3：确认不存在遗留的永久占位内容**

```bash
rg "— MB/s · ETA —" crates/app
```

预期结果：非测试生产路径中没有匹配项。warmup 期间仍可由 `format_speed(None)` 单独生成 `— MB/s`，该结果符合要求；但是组合后的永久占位字符串不得以硬编码格式存在于 `render_transfer_job` 中。

---

## 自审（实施计划与规格文档对照）

| 规格章节 | 对应任务 |
| --- | --- |
| 2a RateSampler、算法和格式 | 任务 1 |
| 2a 事件 observe/clear | 任务 2 |
| 2a 传输行详情 | 任务 3 |
| 2a drawer 聚合 | 任务 4 |
| 2b 首次加载 spinner/Loading | 任务 5 |
| 2b 连接 Cancel | 任务 6 |
| 2c Retry ReadDir | 任务 7（验证并完善现有实现） |
| 2d 状态栏选择数量、失败颜色和单击行为 | 任务 7（单击行为已经存在） |
| 纯测试和 app 测试 | 任务 1、6、7、8 |
| 非目标（不修改 core rate、不实现 Fs 重放、不实现 skeleton） | 全局约束 |

**占位内容检查：** 不保留任何有意设置的永久占位内容。
**类型一致性：** 统一使用 `TransferRateBook`、`RateSnapshot`、`AggregateRate`、`observe/clear/snapshot/aggregate` 和 `format_running_detail`。

**文件位置说明：** 如果出现 `resources` ↔ `workspace` 循环依赖，那么将 `rate_sampler` 保留在 `crates/app/src/rate_sampler.rs`（crate root）。任务 2 已经说明该处理方式。

---

## 实施交接

本计划保存于 `docs/plans/2026-07-13-phase2-feedback-observability-impl.md`。

**实施方式有以下两种：**

1. **Subagent-Driven（推荐）**：每个任务使用新的 subagent，并且在任务之间进行审查
2. **Inline Execution**：在当前会话中使用 `executing-plans`，并按检查点分批实施

实施时需要选择其中一种方式。
