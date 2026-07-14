# Workspace 模块化重构记录

**状态：** 2026-07-14 已完成第一轮拆分；本文记录当前边界和剩余收口项，
不再作为尚未执行的迁移计划。

## 起因与结果

原 `crates/app/src/workspace.rs` 约 5,910 行，同时承担连接表单、事件处理、
本地/远端浏览、传输、模态框、设置、渲染和测试。它已迁移为
`crates/app/src/workspace/` 下的职责模块，入口仍为 `workspace::Workspace`，
没有改变 `main.rs` 的调用方式。

纯展示辅助进入 `macsftp-ui`；`app` 继续负责 GPUI 状态和业务编排。Transfer
drawer 只展示当前进程 `TransferStore` 中的 Active / Queued / Completed /
Failed，不存在跨启动 History catalog 或 retry rebuild 路径。

## 当前模块边界

| 模块 | 职责 |
| --- | --- |
| `mod.rs` | `Workspace` 状态、生命周期、action 注册和公共入口 |
| `connect_form.rs` | 连接表单状态、校验和提交编排 |
| `profiles.rs` | Settings 中的 profile 编辑交互 |
| `event_handling.rs` | `AppEvent` 分发和状态更新 |
| `transfers.rs` | 当前进程内传输生命周期和计划收尾 |
| `file_ops.rs` | 本地/远端文件操作编排 |
| `panes.rs` | 浏览、选择、打开和路径导航 |
| `nav.rs` | 每个 pane 的会话内 back/forward 状态 |
| `visible_entries.rs` | 过滤、排序和可见行派生 |
| `command_palette.rs` | palette 状态和 action dispatch |
| `drawer_height.rs` | transfer drawer 高度状态机 |
| `modals.rs` | 连接、host key、冲突和确认 modal |
| `render.rs` | Workspace 顶层 surface 渲染 |
| `helpers.rs` | workspace 内无状态辅助函数 |
| `tests.rs` | GPUI workspace 行为测试 |

## 保持的架构约束

- `ui` 只依赖 GPUI 和 `core`，不依赖 `app`、`platform`、`sftp` 或
  `storage`。
- workspace 子模块通过 `pub(crate)` 方法编排同一个 `Workspace`，不创建第二套
  长期状态。
- 网络和大目录工作不进入 GPUI 主线程。
- 公开 action 和 `workspace::Workspace` 入口保持稳定。

## 后续收口

第一轮拆分解决了单文件问题，但 `render.rs`、`modals.rs`、`tests.rs` 仍偏大。
后续只在明确业务边界下继续拆分：按 surface 拆渲染、按 modal 类型拆确认流、
按用户工作流拆测试。不要为满足机械行数目标创建无语义的小文件。

每次拆分必须做到：

1. 行为和公开 API 不变；
2. 不新增跨层依赖；
3. 不与功能修改或全仓格式化混合；
4. `bash scripts/check.sh` 全绿。
