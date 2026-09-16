# Stage 3 + 4 执行计划：Workspace UI 字段分组与模块改名

**日期：** 2026-08-21

**前置条件：** Stage 1（962c230）和 Stage 2（dcb5611）已经交付；路线图位于 `2026-08-21-workspace-decentralization-roadmap.md`。

**工作性质：** 本阶段属于机械性重构，不改变行为，也不增加新的抽象语义。其唯一目标是将平铺字段归入职责内聚的状态组。

## 全程不变量

- 继续使用单一 `WorkspaceView`，不拆分 Entity，并且不改变任何用户可见行为。
- 每个 commit 均可独立回滚，并且必须通过 `cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings` 和 `cargo test --workspace`。
- 不得同时修改断言语义、文案或布局值。因此，diff 除字段访问路径外不得包含逻辑差异。

## 访问点规模实测（2026-08-21）

8 组共包含约 48 个字段、约 594 个访问点和 14 个文件。访问点最多的单组是 SettingsUi，约有 120 处；访问点最少的单组是 PaneUi，约有 43 处。编译器可以完整检查这些字段引用，因此无须新增测试。

---

## Stage 4（优先实施，独立 commit）：`mod.rs` → `workspace.rs`

1. 执行 `git mv crates/app/src/workspace/mod.rs crates/app/src/workspace.rs`。
2. 按照 Rust 2018 的模块布局，`main.rs` 中的 `mod workspace;` 将解析到平文件，而子模块目录 `workspace/` 保持不变。因此，全仓代码引用均无须修改。
3. 以构建和测试全部通过作为完成标准。此次改名同时修正 AGENTS.md §2 所述的 `mod.rs` 违规。
4. Stage 3 的所有后续 commit 均基于这一合规模块布局。

## Stage 3 总体机制

新增文件 `crates/app/src/workspace/view_state.rs`，并在其中集中定义全部状态组。该文件是一个边界明确的新逻辑组件，也是 view state 清单的唯一权威来源。`PaneFilter` 类型同时从 `mod.rs` 迁移至该文件。每个状态组提供 `fn new(cx-free)` 或 `Default`；已经确认 `InputState::new()` 不需要参数。

每个 commit 均执行以下固定步骤：

1. 在 `view_state.rs` 中定义当前状态组结构体。
2. 将 `Workspace` 的对应字段替换为状态组实例，并且让 `Workspace::new` 使用该状态组的构造器。
3. 根据编译器的 E0609 错误逐文件修改访问路径；各组的预期文件清单见下文。
4. 依次执行三个门禁命令，并且在全部通过后创建 commit。

### Commit 顺序（从访问点较少的状态组开始）

| # | 组 | 字段映射（旧 → 组内名） | 预估访问点 |
| --- | --- | --- | --- |
| S3-1 | `PaneUi` ×2（`local:` / `remote:`） | `local_filter` → `local.filter`；`local_scroll` → `local.scroll`；`local_scrollbar` → `local.scrollbar`（remote 同构） | ~43 |
| S3-2 | `CommandPaletteUi palette` | `palette_open→open`、`palette_input→input`、`palette_selected→selected`、`command_palette_scroll→scroll`、`command_palette_scrollbar→scrollbar` | ~48 |
| S3-3 | `TabSwitcherUi tab_switcher` | `tab_switcher_open→open`、`tab_switcher_index→index`，两个 scroll 字段采用相同映射。`tab_mru` **保留在顶层**，因为它负责 activate/close 语义，并非 switcher 私有状态 | ~52 |
| S3-4 | `GoToPathUi go_to_path` | `go_to_path_open→open`、`go_to_path_input→input`、`go_to_path_error→error` | ~58 |
| S3-5 | `TransferDrawerUi transfer_drawer` | `drawer_open→open`、`drawer_height→height`、`drawer_resize→resize`、`completed_section_expanded`、`failed_section_expanded`、`transfer_scroll→scroll`、`transfer_scrollbar→scrollbar` | ~75 |
| S3-6 | `ConnectFormUi connect_form_ui` | `connect_form→form`、`connect_form_focus→focus`。但是，必须区分顶层名称 `connect_form` 与类型 `ConnectForm` | ~81 |
| S3-7 | `ModalInputsUi modal_inputs` | `conflict_rename`、`conflict_rename_error`、`delete_confirm`、`inline_edit`、`context_menu`、`large_edit_confirm`、`about_open` 七个字段保持原名并归入该组 | ~117 |
| S3-8 | `SettingsUi settings` | `settings_section→section`、`profile_filter(+_focused)→filter(+focused)`、`external_editor_input(+_focused)`、`selected_profile_id`、`profile_editor`、`profile_delete_confirm`、`profile_picker_scroll(+scrollbar)` | ~120 |

完成后，Workspace 顶层保留 18 个字段：身份和管道字段（window_session_id、state、runtime_client、log_file、_appearance_subscription）、5 个焦点 handle、focused_side、surface、tab_mru、selection_anchor、default_local_path、status_message、config_error、local/remote（PaneUi），以及 8 个状态组。

### 各组的主要相关文件（按当前访问密度）

| 组 | 文件 |
| --- | --- |
| PaneUi | panes.rs、render.rs、tests.rs |
| CommandPaletteUi | command_palette.rs、mod.rs、tests.rs |
| TabSwitcherUi | render.rs（tab bar）、event_handling.rs、tests.rs |
| GoToPathUi | nav 相关 action、file_ops.rs、tests.rs |
| TransferDrawerUi | transfer_render.rs、transfers.rs、drawer_height.rs、event_handling.rs、tests.rs |
| ConnectFormUi | connect_form.rs、event_handling.rs、modals.rs、tests.rs |
| ModalInputsUi | modals.rs、file_ops.rs、remote_edit.rs、tests.rs |
| SettingsUi | profiles.rs、settings_render.rs、mod.rs、tests.rs |

## 已知技术注意点

1. **`remote.scroll` 的运行时替换：** `TabConnected` 发生时，`self.remote_scroll = UniformListScrollHandle::new()` 修改为 `self.remote.scroll = ...`。该修改不改变语义，但是必须确认替换发生在渲染读取之前；当前实现已经满足此顺序。
2. **listener 借用：** `cx.listener` 闭包对状态组字段的借用方式与当前平铺字段的借用方式一致，因此不会引入新的双重可变借用冲突。如果某个方法同时读写同一状态组的两个字段，则使用局部解构（`let Self { open, input } = ...`）或临时变量解决；禁止 clone `InputState`。
3. **`PaneFilter` 迁移：** `PaneFilter` 类型迁移至 `view_state.rs` 后，必须同步更新 panes.rs 和 render.rs 中的 `use crate::workspace::PaneFilter` 路径。
4. **tests.rs（约 174 处）：** 各状态组实施时同步修改相关测试。断言只允许修改访问路径，不得修改期望值。
5. **禁止修改的内容：** `theme.sizes`、按钮 element id、action 名称和 status message 文案均不属于本阶段范围，因此必须保持不变。

## 完成定义（DoD）

- [x] 旧平铺字段名在 app crate 内不存在独立匹配项。编译器负责完整验证，但是新的组内路径（例如 `.settings.profile_filter`）可以保留。
- [x] Workspace 顶层字段减少至 25 个，包括 16 个标量或管道字段和 9 个状态组实例；其中 local/remote 是两个 PaneUi 实例。各组字段数与本计划表一致。
- [x] 全部门禁通过，并且 `cargo test --workspace` 的测试数量不少于 478。2026-08-22 的执行结果全部通过；2026-08-24 再次验证时共有 508 个测试通过。
- [x] 路线图文档已经标记 Stage 3/4 交付，并且包含实际 commit 列表（`5988718`）。
