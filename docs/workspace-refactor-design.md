# workspace.rs 模块化重构设计

## 现状

- `crates/app/src/workspace.rs` 当前 **5,910 LOC**，是项目最大文件。
- 包含职责混杂：连接表单、事件处理、状态管理、本地/远端浏览、传输队列、历史记录、模态框、设置、关于、UI 渲染、工具函数、单测。
- 大量 UI 渲染辅助函数（`section_header_static`、`connection_status`、`transfer_title` 等）与业务编排耦合在一起。
- `ui` crate（`crates/ui`）已提供基础组件（`file_list`、`tab`、`icon`、`components` 等），但 `app` 层仍重复实现了大量展示层逻辑。

## 重构目标

1. 明确 `app` 层与 `ui` 层职责边界：
   - `ui`：无状态/纯展示组件、通用渲染辅助函数、只依赖 `core` 数据类型。
   - `app`：`Workspace` 控制器、业务编排、状态管理、事件分发、与 runtime/storage 交互。
2. 将 `workspace.rs` 按职责拆分为多个内聚子模块，每个模块控制在合理行数（目标 < 1,000 LOC）。
3. 保持现有公开 API（`Workspace` 的公共字段/方法、`main.rs` 的入口）不变。
4. 行为与原文件完全一致：`scripts/check.sh` 全绿。
5. 避免循环引用：`ui` 不依赖 `app`/`platform`/`sftp`/`storage`。

## 模块设计

### 1. 迁移到 `crates/ui/src` 的纯 UI 辅助

新增 `crates/ui/src/workspace_widgets.rs`：

- `section_header_static(label, count, theme)`
- `connection_status(connection, theme)`
- `transfer_title(job)`
- `copy_name(destination)`
- `transfer_history_title(record)`
- `transfer_history_detail(record)`
- `DragPreview` 结构体及 `Render` 实现（从 `app` 迁移）

`crates/ui/src/ui.rs` 重新导出这些类型/函数。

### 2. `crates/app/src/workspace/` 子模块

目录结构：

```text
crates/app/src/workspace/
├── mod.rs          # Workspace 结构体、公共字段、Render/Focusable impl、公开 API
├── connect_form.rs # ConnectForm 结构体及全部方法
├── event_handling.rs # handle_app_event 及事件处理分发
├── transfers.rs    # 传输业务：start/upload/download/cancel/retry/history/finalize_plan
├── panes.rs        # 本地/远端面板：导航、选择、打开、刷新、路径设置
├── modals.rs       # 模态框渲染：connect、host key、transfer conflict
├── render.rs       # 顶层 UI 渲染：tab_bar、pane、transfer_drawer、status_bar、settings、about
├── helpers.rs      # 纯工具函数：append_remote_name、append_local_name、expand_home 等
└── tests.rs        # 原 #[cfg(test)] 模块整体迁移
```

各模块职责：

| 模块 | 职责 | 主要方法/类型 |
|------|------|--------------|
| `mod.rs` | `Workspace` 结构体；构造与生命周期；`Render`/`Focusable`；公开 action 入口 | `Workspace`、`new`、`render` |
| `connect_form.rs` | 连接表单状态与交互 | `ConnectForm`、表单字段/聚焦/提交/验证 |
| `event_handling.rs` | 所有 `AppEvent` 处理 | `handle_app_event` 及其内部分支 |
| `transfers.rs` | 传输生命周期与历史 | `begin_upload`、`begin_download`、`cancel_transfer`、`retry_transfer`、`finalize_plan`、`flush_transfer_history`、`retry_history_transfer` |
| `panes.rs` | 文件面板浏览与选择 | `focus_pane`、`move_selection`、`open_entry_at`、`set_local_path`、`request_remote_directory`、`go_to_parent_directory`、`refresh_focused_pane` |
| `modals.rs` | 模态框渲染与焦点 | `render_connect_form_modal`、`render_host_key_modal`、`render_transfer_conflict_modal`、`active_host_key_prompt` 等 |
| `render.rs` | 把 `Workspace` 状态渲染为 GPUI 元素 | `render_tab_bar`、`render_pane`、`render_transfer_drawer`、`render_status_bar`、`render_settings`、`render_about` |
| `helpers.rs` | 无状态业务/字符串工具 | `append_remote_name`、`append_local_name`、`expand_home`、`connection_in_flight` |
| `tests.rs` | 原测试模块 | `init_workspace`、`temp_app_paths`、`test_settings` 及全部测试函数 |

## 依赖处理

- `ui` 只依赖 `gpui` + `macsftp-core`，不引入 `app`/`platform`/`sftp`/`storage`。
- `app` 依赖 `ui`（已存在），引入 `workspace_widgets` 的辅助函数替代本地实现。
- `workspace` 子模块之间通过 `pub(crate)` 的 `Workspace` 方法共享状态；各子模块文件中出现 `impl Workspace { ... }` 块。
- `mod.rs` 声明所有子模块：`mod connect_form; mod event_handling; ...`。
- `main.rs` 保持 `mod workspace;` 不变；通过 `workspace::Workspace` 访问类型。

## 迁移步骤

1. 在 `ui` 新建 `workspace_widgets.rs`，复制并适配 `workspace.rs` 中的纯 UI 辅助函数与 `DragPreview`。
2. 在 `ui/src/ui.rs` 重新导出新增项。
3. 在 `app` 新建 `workspace/` 目录，创建 `mod.rs` 及上述子模块文件。
4. 按顺序从原 `workspace.rs` 切分内容到新文件：
   - `helpers.rs`：工具函数
   - `connect_form.rs`：`ConnectForm`
   - `event_handling.rs`：事件处理
   - `transfers.rs`：传输相关
   - `panes.rs`：面板相关
   - `modals.rs`：模态框
   - `render.rs`：渲染
   - `tests.rs`：测试
   - `mod.rs`：剩余结构体 + 公开 API + 子模块声明
5. 删除原 `crates/app/src/workspace.rs`，将 `workspace/mod.rs` 作为入口。
6. 修改 `app` 中所有对本地辅助函数的调用为 `macsftp_ui::workspace_widgets::*`。
7. 运行 `cargo check`、`cargo test`、`cargo clippy -D warnings` 直到全绿。
8. 运行 `scripts/check.sh` 最终验证。

## 验收标准

- `cargo test --workspace` 全通过。
- `cargo clippy --workspace --all-targets -D warnings` 无错误。
- `scripts/check.sh` 全绿。
- 行数分布：无子模块超过 1,000 行；`workspace/` 目录总 LOC 与原先基本持平（含注释/测试）。
- `main.rs` 公开 API 不变（`mod workspace`、`use workspace::Workspace` 等仍可用）。
