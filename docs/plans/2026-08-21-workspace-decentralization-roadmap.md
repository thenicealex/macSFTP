# Workspace 职责分解路线图（Stage 2–4）

**日期：** 2026-08-21
**前置条件：** Stage 1 已交付（`962c230`）。`local_read_epochs` 守卫与 `tab_nav` 导航历史已经归属于 core `TabState`，因此 view 层的两张 per-tab 边表及其手动清理逻辑已经删除。
**目标：** 将 `Workspace`（app crate，约 21k 行）顶层定义的 64 个字段调整为少量核心字段与按职责组织的状态组，但是不改变行为，也不引入新的 GPUI Entity。

## 不变量（每个 Stage 均须满足）

- 继续使用单一 `WorkspaceView`（plan §4）：按照 surface 将渲染实现分布在不同文件中，并且不增加额外的 view state 或跨层抽象。
- 不得改变行为：所有既有测试的语义保持不变，并且 fmt、clippy `-D warnings`、`test --workspace`、架构检查和敏感日志检查均须通过。
- 必须保证 secret 安全：`ConnectionSettings` 不得以明文形式出现在日志、序列化快照或 `Debug` 输出中。因此，现有 zeroize 和脱敏机制必须保持不变。

---

## Stage 2 — 将剩余两张 per-tab 边表归入 core（✅ 已交付，2026-08-21）

> 实施注记：字段命名为 `TabState.connection_settings` 和 `TabState.restored_target`，因此比原计划中的 `settings` 更明确。两者均使用 `Option<Box<_>>`，为低频字段增加间接存储，从而避免 `AppEvent::TabOpened(TabSnapshot)` 内嵌的 `TabState` 超过 clippy `large_enum_variant` 阈值。快照保护继续使用既有的 `session_snapshot_never_persists_credentials` 测试；该测试通过端到端断言确认 session.json 不含密码，因此没有增加重复用例。

### 现状（实施前）

| 边表 | 内容 | 读写点 |
| --- | --- | --- |
| `restored_targets: HashMap<TabId, RestoredTabTarget>` | session.json 恢复元数据（host/port/username/profile_id/remote_path），无秘密 | mod.rs（写入 351/1031，读取 390/961）、connect_form.rs:262、event_handling.rs:95–135 |
| `tab_settings: HashMap<TabId, ConnectionSettings>` | **含秘密的连接凭据缓存**（zeroize 类型），供断线重连免重输 | mod.rs（写入 910，读取 843/960/389）、connect_form.rs:258、event_handling.rs:135 |

这两个边表与 Stage 1 的 epoch/nav 具有相同的生命周期关系：其生命周期等于 tab 生命周期，但是当前实现使用需要手动调用 `remove()` 的边表表示该关系。

### 方案

1. 在 core 中定义纯数据类型 `RestoredTabTarget`，并通过 `TabState.restored_target: Option<RestoredTabTarget>` 保存其状态。
2. 使用 `TabState.settings: Option<ConnectionSettings>`，即推荐的方案 A：
   - `ConnectionSettings` 本就是 core 类型且已派生 ZeroizeOnDrop、手写 Debug 全量脱敏；
   - 其生命周期会随 tab 自动终止，因此符合 plan §15 中“长期状态进入 core 模型”的要求。
   - 备选方案 B 是进程级全局 `SecretStore`，但是该方案会重新引入 side-table 的手动生命周期问题，因此不采用。
   - **已知残余风险与缓解措施：** 如果未来为 `TabState` 增加 `Serialize`，secret 可能被写入 session.json。因此需要：(a) 在字段的 doc comment 中明确禁止序列化；(b) 增加保护测试，断言 `SessionTabSnapshot` 仅获取 host/port，并且构建路径不访问 secret 字段。修改后的现有 `build_session_snapshot` 已经满足此条件，而测试用于固定该约束。
3. 删除 `close_tab` 中的两行手动清理逻辑；同时，connect_form、event_handling 和 snapshot 构建统一通过 `find_tab(tab_id)?.restored_target` 或 `.settings` 访问状态。

### 验收

- app 中不再存在任何 `HashMap<TabId, _>` 边表；因此，`rg 'HashMap<TabId' crates/app/src/workspace/mod.rs` 的结果中没有业务边表。
- 增加 core 测试，验证 settings/restored_target 在 `TabState` 默认状态下均为 `None`，并增加 snapshot 保护测试。
- 既有测试继续通过，并验证断线后重新连接不会丢失凭据，而且 session 快照内容保持不变。

### 规模与风险

规模为中等，需要修改 core.rs、workspace 中的 5 个文件以及 tests。主要风险是凭据流程出现回归，因此使用既有 reconnect、prefill 和 snapshot 测试进行验证。

---

## Stage 3 — 将 UI 字段按照 feature 分组（✅ 已交付，2026-08-21）

> **详细执行计划见 `2026-08-21-stage3-stage4-plan.md`**，其中包含逐字段映射表、访问点实测和 commit 序列。
>
> **交付记录：** 8 个状态组均已实现，并且集中定义于 `workspace/view_state.rs`。Workspace 顶层字段数量由 60 个减少至 25 个，其中 10 个是状态组实例。提交序列：S3-1 PaneUi `2f026ae`、S3-2 CommandPaletteUi `1e71423`、S3-3 TabSwitcherUi `a0f00e1`、S3-4 GoToPathUi `9d1660f`、S3-5 TransferDrawerUi `4a02d5f`、S3-6 ConnectFormUi `31dab1e`、S3-7 ModalInputsUi `8ef4950`、S3-8 SettingsUi `eb094db`。

## Stage 4 — 将 mod.rs 改为 workspace.rs（✅ 已交付，2026-08-21，早于 Stage 3 实施）

此阶段通过 `b6d6591` 交付，其中也包含 Stage 3/4 计划文档。因此，代码已经完全符合 AGENTS.md §2 中禁止使用 mod.rs 路径的要求。

### 现状

Stage 1/2 完成后，Workspace 顶层仍定义约 50 个 UI 字段。其中许多字段只有与同一功能的其他字段组合时才具有完整语义。

### 分组清单（8 组，约 45 个字段）

| 子结构 | 包含的字段 |
| --- | --- |
| `ConnectFormUi { connect_form, connect_form_focus }` | 2 |
| `CommandPaletteUi { open, input, selected, scroll, scrollbar }` | 5 |
| `TabSwitcherUi { open, index, scroll, scrollbar }` + `tab_mru` | 5 |
| `GoToPathUi { open, input, error }` | 3 |
| `TransferDrawerUi { open, height, resize, completed_expanded, failed_expanded, scroll, scrollbar }` | 7 |
| `SettingsUi { section, profile_filter(+focused), external_editor_input(+focused), selected_profile_id, profile_editor, profile_delete_confirm }` | 8 |
| `ModalInputsUi { conflict_rename, conflict_rename_error, delete_confirm, inline_edit, context_menu, large_edit_confirm, about_open }` | 7 |
| `PaneFilterUi`×2（local/remote 各含 filter+scroll+scrollbar）+ `selection_anchor` | 7 |

完成后，Workspace 顶层保留约 15 个字段（state/runtime_client/focus handles×5/surface/focused_side/default_local_path/status_message/config_error/log_file/window_session_id/_subscription）以及 8 个命名状态组。

### 执行策略

- 仅执行字段访问路径重命名（`self.palette_open` → `self.palette.open`），不修改逻辑。每个状态组使用一个独立 commit，因此各组可以独立验证和回滚。
- 建议按照字段数量由少到多实施：GoToPath → ConnectForm → CommandPalette → TabSwitcher → ModalInputs → PaneFilter → TransferDrawer → Settings，其中 Settings 最后实施。
- tests.rs 约有 5.7k 行，需要随各状态组同步修改访问路径，但是不得同时修改断言语义。
- Stage 4 的模块改名与 Stage 3 的第一个状态组一并实施，具体说明见下文。

### 验收（每组相同）

该状态组的全部访问点均通过组实例访问，因此 `rg` 结果中不再存在独立访问的组内字段名。所有检查必须通过，并且 diff 不得包含任何非常量表达式变化。

### 规模与风险

规模较大，但是修改仅涉及字段访问路径。主要风险是遗漏访问点或拼写错误，而编译器可以完整识别这些问题，因此不需要增加测试。完成后，字段所有权会更加明确，并且后续功能修改所影响的范围会减小。

---

## Stage 4 — 将 `workspace/mod.rs` 改为 `workspace.rs`（✅ 已交付，见上方交付记录）

原执行备注已经完成：使用 `git mv` 将模块定义改为普通文件，同时保持子模块目录不变。实际实施时，该修改使用独立的首个 commit，而没有与 Stage 3 的第一个状态组合并。

---

## 实施顺序建议与依赖关系

```
Stage 2（状态正确性，中）──► Stage 4（模块改名）──► Stage 3（字段分组，共 8 个 commit）
```

- Stage 2 优先实施，因为它处理仅剩的架构规则违规和 secret 存储位置问题，并且能够直接改善正确性。Stage 3 只改善代码组织。
- 每个 Stage 均可独立交付或中止；如果在 Stage 边界中止，那么 repository 不会处于部分完成状态。

## 显式非目标

- 不将 Workspace 拆分为多个 GPUI View/Entity，因为这会违反 plan §4 的既定决策。
- 不修改 event_coordinator、resources 或 session_coordinator，因为这些进程级组件已经具有清晰的职责边界。
- 不拆分 core.rs。后续增加新的业务域时再评估文件划分，因此本阶段不进行缺少业务需求的文件拆分。
