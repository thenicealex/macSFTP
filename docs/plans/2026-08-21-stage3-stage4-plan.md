# Stage 3 + 4 执行计划：Workspace UI 字段分组与模块改名

**Date:** 2026-08-21
**前置:** Stage 1（962c230）、Stage 2（dcb5611）已交付；路线图见 `2026-08-21-workspace-decentralization-roadmap.md`。
**性质:** 纯机械重构。零行为变化、零新抽象语义——只是把平铺字段收进职责内聚的状态组。

## 全程不变量

- 单一 `WorkspaceView` 不拆 Entity；不改任何用户可见行为。
- 每个 commit 独立可回滚，且必须通过：`cargo fmt --all --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`。
- 不允许借机改断言语义/文案/布局值；diff 中除字段访问路径外不得出现逻辑差异。

## 访问点规模实测（2026-08-21）

8 组共 ~48 字段、~594 个访问点、14 个文件。最大单组 SettingsUi ≈120 处，最小 PaneUi ≈43 处。全部由编译器穷举兜底，无需新增测试。

---

## Stage 4（先行，独立 commit）：`mod.rs` → `workspace.rs`

1. `git mv crates/app/src/workspace/mod.rs crates/app/src/workspace.rs`
2. Rust 2018 布局：`main.rs` 的 `mod workspace;` 解析到平文件，子模块目录 `workspace/` 保持不变；全仓无代码引用需要改动。
3. 验证：构建 + 测试全绿即完成；同时修复 AGENTS.md §2 的 mod.rs 违规。
4. 之后所有 Stage 3 的 commit 都落在合规布局上。

## Stage 3 总体机制

**新文件 `crates/app/src/workspace/view_state.rs`**：集中定义全部状态组（这是一个新的清晰逻辑组件——view state 清单的唯一权威位置），`PaneFilter` 类型从 mod.rs 一并迁入。各组提供 `fn new(cx-free)` 或 `Default`（`InputState::new()` 无参，已核实）。

**每个 commit 的固定动作**：
1. 在 `view_state.rs` 定义该组结构体；
2. `Workspace` 字段替换为组实例，`Workspace::new` 改用组构造器；
3. 按编译器 E0609 报错逐文件重写访问路径（预期文件清单见各组）；
4. 门禁三连 + commit。

### Commit 顺序（由小到大，按访问点数）

| # | 组 | 字段映射（旧 → 组内名） | 预估访问点 |
| --- | --- | --- | --- |
| S3-1 | `PaneUi` ×2（`local:` / `remote:`） | `local_filter` → `local.filter`；`local_scroll` → `local.scroll`；`local_scrollbar` → `local.scrollbar`（remote 同构） | ~43 |
| S3-2 | `CommandPaletteUi palette` | `palette_open→open`、`palette_input→input`、`palette_selected→selected`、`command_palette_scroll→scroll`、`command_palette_scrollbar→scrollbar` | ~48 |
| S3-3 | `TabSwitcherUi tab_switcher` | `tab_switcher_open→open`、`tab_switcher_index→index`、两个 scroll 对同上。`tab_mru` **留在顶层**（驱动 activate/close 语义，非 switcher 私有） | ~52 |
| S3-4 | `GoToPathUi go_to_path` | `go_to_path_open→open`、`go_to_path_input→input`、`go_to_path_error→error` | ~58 |
| S3-5 | `TransferDrawerUi transfer_drawer` | `drawer_open→open`、`drawer_height→height`、`drawer_resize→resize`、`completed_section_expanded`、`failed_section_expanded`、`transfer_scroll→scroll`、`transfer_scrollbar→scrollbar` | ~75 |
| S3-6 | `ConnectFormUi connect_form_ui` | `connect_form→form`、`connect_form_focus→focus`。注意顶层名 `connect_form` 与类型 `ConnectForm` 区分 | ~81 |
| S3-7 | `ModalInputsUi modal_inputs` | `conflict_rename`、`conflict_rename_error`、`delete_confirm`、`inline_edit`、`context_menu`、`large_edit_confirm`、`about_open` 七字段原样入组 | ~117 |
| S3-8 | `SettingsUi settings` | `settings_section→section`、`profile_filter(+_focused)→filter(+focused)`、`external_editor_input(+_focused)`、`selected_profile_id`、`profile_editor`、`profile_delete_confirm`、`profile_picker_scroll(+scrollbar)` | ~120 |

完成后 Workspace 顶层剩 18 字段：身份/管道（window_session_id、state、runtime_client、log_file、_appearance_subscription）、焦点 handles×5、focused_side、surface、tab_mru、selection_anchor、default_local_path、status_message、config_error、local/remote(PaneUi)、8 个状态组。

### 各组主要触及文件（按当前访问密度）

| 组 | 文件 |
| --- | --- |
| PaneUi | panes.rs、render.rs、tests.rs |
| CommandPaletteUi | command_palette.rs、mod.rs、tests.rs |
| TabSwitcherUi | render.rs(tab bar)、event_handling.rs、tests.rs |
| GoToPathUi | nav 相关 action、file_ops.rs、tests.rs |
| TransferDrawerUi | transfer_render.rs、transfers.rs、drawer_height.rs、event_handling.rs、tests.rs |
| ConnectFormUi | connect_form.rs、event_handling.rs、modals.rs、tests.rs |
| ModalInputsUi | modals.rs、file_ops.rs、remote_edit.rs、tests.rs |
| SettingsUi | profiles.rs、settings_render.rs、mod.rs、tests.rs |

## 已知技术注意点

1. **`remote.scroll` 的运行时替换**：`TabConnected` 时 `self.remote_scroll = UniformListScrollHandle::new()` 变为 `self.remote.scroll = ...`——语义不变，但确认替换发生在渲染读取前（现状如此）。
2. **listener 借用**：`cx.listener` 闭包里对组字段的借用模式与今天对平铺字段的借用一致，不引入新的双可变借用冲突；若某方法同时读写同组两字段，用局部解构（`let Self { open, input } = ...` 或临时变量）解决，禁止 clone InputState。
3. **`PaneFilter` 迁移**：类型移入 view_state.rs 后，panes.rs/render.rs 的 `use crate::workspace::PaneFilter` 路径同步更新。
4. **tests.rs（~174 处）**：逐组随行改写；断言只改访问路径，不改期望值。
5. **禁改清单**：`theme.sizes`、按钮 element id、action 名、status message 文案——这些不是本 stage 的内容。

## 完成定义（DoD）

- [ ] `rg 'palette_open|go_to_path_open|drawer_open|...'`（全部旧字段名）在 app crate 内零命中
- [ ] Workspace 顶层字段 ≤18 且每组字段数与本计划表一致
- [ ] 全量门禁绿；`cargo test --workspace` 数量不少于 478
- [ ] 路线图文档标记 Stage 3/4 交付，附实际 commit 列表
