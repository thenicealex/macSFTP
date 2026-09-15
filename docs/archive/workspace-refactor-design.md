# Workspace 模块化重构记录

**状态：** 第一轮拆分已于 2026-07-14 完成。因此，本文记录当前模块边界和剩余整理事项，
不再作为待执行的迁移计划。

## 起因与结果

原 `crates/app/src/workspace.rs` 约有 5,910 行，其中同时包含连接表单、事件处理、
本地与远端浏览、传输、模态框、设置、渲染和测试。该文件的内容已经按照职责迁移至
`crates/app/src/workspace/` 下的多个模块。但是，对外入口仍为 `workspace::Workspace`，
因此 `main.rs` 的调用方式没有变化。

纯展示辅助逻辑归属于 `macsftp-ui`，而 `app` 继续负责 GPUI 状态和业务编排。
Transfer drawer 只展示当前进程 `TransferStore` 中的 Active、Queued、Completed
和 Failed 状态。因此，系统不包含跨启动的 History catalog 或 retry rebuild 路径。

## 当前模块边界

| 模块 | 职责 |
| --- | --- |
| `mod.rs` | `Workspace` 状态、生命周期、action 注册和公共入口 |
| `connect_form.rs` | 连接表单状态、校验和提交编排 |
| `profiles.rs` | Settings 中的 profile 编辑交互 |
| `event_handling.rs` | `AppEvent` 分发和状态更新 |
| `transfers.rs` | 当前进程内传输生命周期和计划完成处理 |
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
- workspace 子模块通过 `pub(crate)` 方法编排同一个 `Workspace`，因此不会创建第二套
  长期状态。
- 网络操作和大目录处理不能占用 GPUI 主线程。
- 公开 action 和 `workspace::Workspace` 入口保持稳定。

## 后续整理事项

第一轮拆分解决了单文件规模过大的问题，但是 `render.rs`、`modals.rs` 和 `tests.rs`
仍然偏大。后续拆分必须以明确的业务边界为依据：渲染代码按照 surface 分类，确认流程
按照 modal 类型分类，测试按照用户工作流分类。不能为了满足机械性的行数目标而创建缺少
独立语义的小文件。

每次拆分必须做到：

1. 行为和公开 API 不变；
2. 不新增跨层依赖；
3. 不与功能修改或全仓格式化混合；
4. `bash scripts/check.sh` 的全部检查均通过。
