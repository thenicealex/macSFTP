# macSFTP — 项目进展分析（实测核对版）

> **⚠️ 后续更新（2026-07-13 晚）：** 本文是 01:44 的快照。此后完成了 **多窗口支持（post-MVP）** 一整轮工作，并顺带清理了本文 §3 记录的"Connection Pool app 层半成品接线"（那些未提交的 `next_tab_id` 静态计数器、`ConfigStore: Clone`、`render.rs` 坏掉的 import 等 —— 其中 `next_tab_id` 静态方案已被更干净的注入式 `Arc<AtomicU64>` 计数器取代，`render.rs` 的错误 import 已修复）。最新状态请以 **`docs/progress-analysis-2026-07-13-multiwindow.md`** 为准。

**Date:** 2026-07-13 01:44 GMT+8
**Basis:** 直接核对工作区 —— 源码结构、真实编译/格式化/clippy 输出、`git` 历史、CI 配置、计划文档 §20/§21。
**注意:** 本文是对 `docs/progress-analysis-2026-07-12.md` 的纠正。该文档声称 "gate 100% 绿 / 181 测试 / Git+CI 已完成"，其中前半部分为**过时且不准确**的表述（见第 4 节）。

## 0. 一句话结论

MVP 功能（M0–M7）按原计划已全部交付；`workspace.rs` 单体重构已完成并提交；并且已经越出 MVP 范围，提交了 **Connection Pool（SSH 多路复用）** 这一 post-MVP 功能。**但是：当前 verify gate 是红色的**（`cargo fmt` 与 `cargo clippy -D warnings` 均不通过），且存在一个**未提交的进行中功能（Connection Pool 的 app 层接线）** 和一处 **git 误跟踪的 `.bak` 文件**。文档声称的"全绿"与现实不符。

## 1. 已核实的真实状态（地面真相）

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| Git 初始化 & 提交 | ✅ 真 | 9 个 commit（`git log`），最新 `37ceb63 feat: implement ssh multiplexing via Connection Pool` |
| CI 配置 | ✅ 真 | `.github/workflows/ci.yml` 存在（510 B） |
| `workspace.rs` 重构 | ✅ 已提交 | 原 5,910 LOC 已拆分为 `crates/app/src/workspace/`（9 个子模块）+ `crates/ui/src/workspace_widgets.rs`；commit `bf07c3a` / `7837e70` |
| M0–M7 功能交付 | ✅ 按计划 | 计划文档 §20 标注全部 delivered，MVP ≈ 100% |
| **Verify gate（fmt→test→clippy）** | ❌ **红色** | `cargo fmt --all --check` 在 sftp `session_actor.rs`/`sftp.rs`、app `workspace/mod.rs`/`connect_form.rs`/`render.rs` 等多个文件失败；`cargo clippy --workspace --all-targets -D warnings` 也会失败（见第 4 节） |
| 编译（`cargo check`） | ✅ 通过 | 0 error，仅有 warning（未使用的 import） |
| Connection Pool 功能 | ✅ 已提交并接入 | `crates/sftp/src/pool.rs`(131) + `physical_connection.rs`(392)；`runtime.rs:307/329` 已调用 `ConnectionManager::get_or_connect` |

## 2. 代码规模（实测 LOC）

| Crate | LOC | 文件数 | 备注 |
| --- | --- | --- | --- |
| sftp | 6,389 | 9 | 因 Connection Pool 增长最多 |
| app | 6,469 | 13 | 含 `workspace/` 拆分 + tests |
| core | 2,603 | 1 | |
| ui | 1,446 | 9 | 含 `workspace_widgets.rs`(114) |
| storage | 1,410 | 5 | |
| platform | 279 | 1 | |
| test_support | 229 | 1 | |
| **合计** | **≈18,825** | **49** | |

## 3. 当前进行中 / 未提交工作

工作区有 **11 个文件被修改但未提交**（不属于任何 commit）：

- **Connection Pool 的 app 层接线（半成品）**：
  - `crates/app/Cargo.toml` 新增 `tokio = "1.52.3"` 依赖；
  - `core/src/core.rs`：`next_tab_id()` 由 `&self` 改为静态 `AtomicU64`（跨 session 唯一 ID）；
  - `storage/src/config.rs`：`ConfigStore` 派生 `Clone`；
  - `workspace/render.rs`：补充 import 块（引入 `DragPreview`、`connection_status`、`transfer_title` 等来自 `ui` 的辅助函数）。
- 看起来是 Connection Pool 集成的 **Phase 3（app/UI 消费侧）**，已开始但**未做完、未提交**。

## 4. 关键问题：Verify gate 是红色的（与文档宣称相反）

经验证：

1. `cargo fmt --all --check` **失败** —— 在已提交的 `sftp/src/session_actor.rs`、`sftp/src/sftp.rs`，以及重构提交的 `app/src/workspace/mod.rs`、`connect_form.rs`、`render.rs` 等大量文件上均有格式差异。`scripts/check.sh` 在第一步即 `set -e` 中止，**根本不会跑到 test / clippy**。
2. `cargo check --workspace` 虽然编译通过，但输出大量 `unused import` 警告（集中在 `workspace/mod.rs`、`render.rs`、`connect_form.rs`）。由于 gate 第三步是 `cargo clippy --workspace --all-targets -D warnings`，这些警告会被当作 **error**，clippy 同样不通过。

→ **结论：CI 虽已配置，但当前任何一次 push/PR 都会因为 fmt/clippy 失败而红。** "gate 100% 绿" 的文档表述不成立。

## 5. 卫生问题

- ⚠️ `crates/ui/src/workspace_widgets.rs.bak` 被 **git 跟踪**（`git ls-files` 可见）。这是重构遗留的备份文件，应从 git 与磁盘删除。

## 6. 测试数量（待最终确认）

文档记录的 **181 passing / 0 failed / 1 ignored** 发生在 Connection Pool 重构**之前**（文档时间 07-12 04:36，pool commit 时间 07-12 20:36）。该重构删除了 `session_actor.rs` 443 行并重写了 `runtime.rs`，测试数量必然已变化。当前正在后台重跑 `cargo test --workspace` 以获取真实数字，结果将补充至此文档。

## 7. 风险排序

| 级别 | 风险 | 影响 |
| --- | --- | --- |
| **高（自伤）** | Verify gate 红色（fmt + clippy） | CI 对任何 PR 都失败，"CI 已配置" 的红利被抵消；无法保证合入质量 |
| 中 | Connection Pool app 接线半成品且未提交 | 工作树脏、易在切换/reset 时丢失；功能不完整 |
| 中 | 单人 bus factor = 1 | 无评审、知识集中 |
| 低-中 | GPUI 0.2.2 未到 1.0，API 易变 | 已用版本锁定 + UI 层集中缓解 |
| 低 | 转译依赖 `block v0.1.6` future-incompat | 非本项目代码，仅日志噪音 |
| 低 | git 误跟踪 `.bak` 文件 | 仓库整洁度 |

## 8. 建议（按优先级）

1. **P0 — 立即修复 gate**：`cargo fmt --all`；清理 `workspace/*` 子模块中的未使用 import；从 git 与磁盘删除 `workspace_widgets.rs.bak`；随后 `bash scripts/check.sh` 必须全绿，否则 CI 形同虚设。
2. **P1 — 收尾或进行中功能**：把 Connection Pool 的 app 层接线**做完并提交**，或明确 `git stash`，不要让它半提交地悬在工作树里。
3. **P2 — 刷新文档**：将 `docs/progress-analysis` 更新为"gate 红色、新增 Connection Pool（post-MVP）、重构已完成、记录真实测试数"，避免误导后续判断。

## 9. 里程碑达成情况

- M0–M7：按原定计划全部 delivered，MVP ≈ 100%。**这一结论仍然成立。**
- 工程任务（Git 基线、CI、单体重构）：git/CI/重构均已完成，**但 gate 实际未绿**，需按第 8 节修复。
- 超出原计划的部分：Connection Pool（原计划列为 P3 未来探索项）已作为 post-MVP 功能提交并接入 runtime。
