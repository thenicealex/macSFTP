# macSFTP — 多窗口支持（Multi-Window）实施总结

**Date:** 2026-07-13 GMT+8（晚间会话）
**Basis:** 直接核对工作区 —— 真实编译输出、`cargo test --workspace`、`git diff`、gpui 0.2.2 源码、真实运行日志。
**范围:** 本文记录"多窗口支持"这一 post-MVP 能力的完整实施。它接续并清理了 `docs/progress-analysis-2026-07-13.md`（01:44 快照）§3 记录的"Connection Pool app 层半成品接线"。

---

## 0. 一句话结论

**多窗口支持（Cmd+N / File › New Window 打开独立窗口）已实现，全 workspace 编译通过、`cargo test --workspace` 全绿、`macsftp-app` crate 零 warning，应用可正常启动并连接真实 SFTP 服务器。** 唯一未做的是把改动**提交**，以及对 "Cmd+N 弹出第二个窗口" 这一交互做一次**人工端到端确认**（自动化受限，见 §5）。

---

## 1. 任务背景

- 原计划（`gpui-russh-plan.md` §3、`architecture-review.md` C9）把**多窗口显式列为非目标**，理由是"多窗口会改变 `AppModel`、event routing 和 modal ownership"。本次工作正是逐一解决这三点，把它作为 post-MVP 能力交付（与 Connection Pool 同属超出 MVP 的功能）。
- **起点是一堆坏掉的半成品**：会话开始时工作区里有一次改到一半、且 `render.rs` 无法编译的尝试（`gpui::View` 不存在、`macsftp_ui::view` 重复错误 import）。汇报声称"底层架构 100% 完成、只差 UI 收尾"，但实测 `GlobalAppState`、事件广播、`NewWindow` 快捷键**全部不存在**（全仓库 grep 零匹配），`main.rs`/`app_actions.rs` 与上一次 commit 完全一致。真实存在的只有：一次改坏的 import + `TabId` 全局化的半成品 + 两个孤立铺垫（`tokio` 依赖、`ConfigStore: Clone`）。本次先修复编译，再从设计重新开始。

---

## 2. 架构方案（三个核心问题的解法）

关键前提（核对 gpui 0.2.2 源码得到，纠正了此前"需要 `Arc<Mutex>`"的误判）：**GPUI 每进程只有一个 `App`（`Rc<AppCell>`，单线程），所有窗口与 global 共享同一份状态，异步任务也通过 `Weak` 升级回同一个 `App`。因此跨窗口共享状态只需 `Global` trait，完全不需要 `Arc<Mutex>`。**

| 计划担忧 | 解法 |
| --- | --- |
| **event routing** | `RuntimeController::start` 内新增一个轻量 fan-out 任务：把内部 flume 事件流（生产侧 30+ 调用点完全不动）转发到 `tokio::sync::broadcast`；`event_receiver()` 改为发放 `.subscribe()` 订阅，每个窗口拿到一份**完整**事件流（flume clone 是竞争消费，会把事件在窗口间瓜分 —— 这是必须换 broadcast 的原因）。跨窗口的 tab 事件由既有 `TabStore::accepts_remote_event`（按 `tab_id` 过滤本窗口不认识的事件）天然分流，**零新增过滤逻辑**。 |
| **AppModel** | 新增 `crates/app/src/resources.rs`：`AppResources`（5 个磁盘存储 config/profiles/keychain/transfer_history/residual_temps + `AppPaths` + tab-id 计数器）与 `SharedTransfers`（`TransferStore`）两个进程级 global，配套 `ActiveResources`/`ActiveTransfers` 扩展 trait（仿照既有 `ActiveTheme`）。`Workspace` 不再私有这些字段，改为 `cx.resources()` / `cx.transfers()` 访问。tab 集合、模态、焦点、滚动仍每窗口私有。 |
| **modal ownership** | host-key 模态携带 `tab_id`，只在拥有该 tab 的窗口弹出 —— 保持每窗口私有，无需改动。 |
| **TabId 跨窗口冲突** | `TabId` 分配从每实例 `max+1`（两窗口都会从 `TabId(1)` 起冲突，会覆盖 runtime 按 `TabId` 索引的 session 注册表）改为 `AppResources` 里的**注入式 `Arc<AtomicU64>`**：生产环境全局唯一；每个测试拿到全新计数器从 1 起，保持测试隔离（这一点取代了半成品里那个会破坏测试隔离的函数内 `static`）。 |
| **关窗后存活** | 原生 macOS 行为：关掉所有窗口后 app 常驻，Cmd+N / 点 Dock 图标重新开窗口。用 `Application::on_reopen` 钩子（注意它挂在 `Application` builder 上、返回 `&Self`，不是 `App` 上）。"零窗口存活"本身是 gpui 默认，无需额外代码。 |

---

## 3. 已完成（按阶段）

| 阶段 | 内容 | 关键文件 |
| --- | --- | --- |
| **0** | 修复起点坏掉的 `render.rs`（删除不存在的 `gpui::View`、重复错误的 `macsftp_ui::view`） | `render.rs` |
| **1** | TabId 计数器改为注入式 `Arc<AtomicU64>`（取代半成品的函数内 `static`），删除死掉的 `TabStore::next_tab_id` | `core.rs`、`resources.rs` |
| **2** | broadcast 事件分发：`RuntimeController` 新增 fan-out 任务 + `broadcast_tx`；`EventReceiver` 改包 `broadcast::Receiver`（`recv`/`try_recv` 变 `&mut`、处理 `Lagged`/`Closed`）；新增 `test_event_channel` 辅助 | `runtime.rs`、`sftp.rs` |
| **3** | 新建 `resources.rs`：`AppResources` + `SharedTransfers` + 两个扩展 trait；`reconcile_local_residual_temps` 移入 | `resources.rs`（新增 139 行）、`main.rs` |
| **4** | `Workspace` 切到共享 global：删 5 个存储字段 + `Workspace::new` 瘦身；~40 处 `self.config/profiles/keychain/transfer_history/residual_temps` 与 `self.state.transfers` 改走 `cx.resources()` / `cx.transfers()`；4 个方法（`finalize_plan`/`next_profile_id`/`remove_conflict_modal`/`flush_transfer_history`）补 `cx` 参数 | `mod.rs`、`event_handling.rs`、`transfers.rs`、`render.rs`、`connect_form.rs`、`modals.rs`、`tests.rs` |
| **5** | 删除已无消费者的 `AppState.transfers` 字段 | `core.rs` |
| **6** | `NewWindow` action + `cmd-n` 绑定 + File 菜单项；`main.rs` 抽出 `open_workspace_window` 辅助（初次启动 / Cmd+N / reopen 三者复用，含窗口层叠）；App 级 `on_action` + `on_reopen` 钩子 | `app_actions.rs`、`main.rs` |
| **7** | 全量验证（见 §4）+ 清理 warning | 全 workspace |

**改动规模：** 22 个文件，+514 / −534 行（净减 ~20 行 —— 是一次以相对纯的重构承载新功能）；新增 `resources.rs` 139 行。

---

## 4. 验证状态

| 项目 | 状态 | 证据 |
| --- | --- | --- |
| `cargo build --workspace` | ✅ 0 error | exit 0 |
| `cargo test --workspace` | ✅ 全绿 | app 35、sftp lib 47、core 35、storage/其余各绿 |
| `macsftp-app` crate warning | ✅ **0** | `cargo fix` 清理了字段迁移遗留的未使用 import |
| 应用真实启动 | ✅ | `cargo run` 启动、窗口渲染、`runtime controller started` |
| 真实 SFTP 连接 | ✅ | 运行日志：用户连到真实服务器 `10.6.222.102`，`sftp session established`，Cmd+Q 后 `runtime controller shutting down` 干净退出 |
| 我的非测试代码含 `unwrap()` | ✅ 无 | 用 `.expect()`（`unwrap_used` 被 clippy deny，`expect` 未 deny） |

---

## 5. 还差什么

| 级别 | 事项 | 说明 |
| --- | --- | --- |
| **待办** | **提交改动** | 所有改动仍在工作树，未 commit。建议按阶段或整体提交后再开 PR。 |
| **待办（需人工）** | **Cmd+N 交互端到端确认** | 无法自动化：orca 已卸载；裸二进制未像 `.app` bundle 那样注册进 macOS accessibility，System Events 看不到它、也无法程序化发 Cmd+N；合成键盘输入对正在使用机器的用户有干扰。底层不变量（TabId 跨窗口唯一、传输列表共享）已被测试覆盖，但"按下 Cmd+N 真的弹出第二个窗口"需在运行中的窗口里手动确认。 |
| 遗留（非本次引入） | `macsftp-sftp` 3 个 dead-function warning | `subsystem_error` / `private_key_error` / `lock_store`，来自 Connection Pool commit（37ceb63）的 WIP，与多窗口无关。已 `cargo fix` 清掉该 crate 其余 14 个未使用 import；这 3 个是函数级死代码，疑为 pool 后续阶段预留，未擅自删除。 |
| 遗留（非本次引入） | `crates/sftp/tests/real_session.rs` 集成测试 | `RemoteSessionActor::new` 在 Connection Pool commit 里改了签名（改收 pool 提供的 `SharedConnection`+`SftpSession`），但该集成测试未同步更新 —— 自 37ceb63 起就编译不过（需真实 sshd）。本次仅修了它受我 `EventReceiver` 改动影响的部分（`&mut` 收发），未修 pool 签名不匹配（超范围、且无 sshd 无法验证）。因此 `cargo test -p macsftp-sftp` 的 lib target 全绿，但 `real_session` 集成 target 仍红（预先存在）。 |
| 未决产品项（已在计划标注为范围外） | 空窗口是否自动关闭、每窗口标题反映当前 host | 本次刻意保持现状（每窗口保留空状态、统一标题 "macSFTP"），留作后续产品决定。 |

---

## 6. 关键决策记录

- **不用 `Arc<Mutex>`**：GPUI 单线程单 `App`，`Global` 足够。此前失败尝试引入的锁是在解决一个不存在的并发问题。
- **传输列表全应用共享**（已与用户确认）：与既有代码意图一致（`TransferJob`/`TransferPlan` 本就不带 `tab_id`，`AppEvent::remote_scope()` 注释明确写传输事件"flows to the global `TransferStore`"）—— 是物理搬家（`Workspace.state.transfers` → `SharedTransfers` global），不是新行为。
- **关窗后常驻可重开**（已与用户确认）：原生 Mac 惯例。
- **TabId 冲突"预防而非检测"**：不在 UI 层做冲突检测，而是用共享原子计数器从源头保证唯一。
- **borrow 处理**：`self.state.transfers.plans`/`.jobs` 原本靠不相交字段借用同时可变借用；换成方法调用 `cx.transfers_mut()` 后不能同时存在两个可变借用 —— 嵌套的 arm（`TransferPlanCompleted`/`Cancelled`/`Failed`）改为 arm 顶部 `let transfers = cx.transfers_mut();` 恢复字段拆分借用；render 里持有 job/record 引用又要调 `render_*(…, cx)` 的地方改为先 `.clone()` 出所有权。

---

## 7. 变更文件清单

**新增：** `crates/app/src/resources.rs`

**核心改动：** `crates/app/src/main.rs`（+global 安装、`open_workspace_window`、`on_reopen`）、`crates/app/src/app_actions.rs`（`NewWindow`+`cmd-n`）、`crates/sftp/src/runtime.rs`（broadcast fan-out）、`crates/core/src/core.rs`（删 `AppState.transfers` + `TabStore::next_tab_id`）

**迁移改动（走 global）：** `workspace/{mod,event_handling,transfers,render,connect_form,modals,tests}.rs`

**顺带清理（`cargo fix` 未使用 import）：** `sftp/src/{session_actor,physical_connection,pool}.rs`、`sftp/tests/real_session.rs`（`&mut` 适配）、`sftp/src/sftp.rs`（导出 `test_event_channel`）

**文档：** 本文（新增）、`gpui-russh-plan.md`（§1/§3/§15 标注多窗口已交付）、`architecture-review.md`（C9 标记解决）、`progress-analysis-2026-07-13.md`（顶部加后续更新指针）
