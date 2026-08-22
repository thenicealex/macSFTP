# Workspace 去中心化路线图（Stage 2–4）

**Date:** 2026-08-21
**前置:** Stage 1 已交付（`962c230`）——`local_read_epochs` 守卫与 `tab_nav` 导航历史移入 core `TabState`，删除 view 层两张 per-tab 边表及其手动清理。
**目标:** 把 `Workspace`（app crate，~21k 行）从"64 字段平铺 god object"收敛为"少量核心字段 + 职责内聚的状态组"，全程不改行为、不引入新 GPUI Entity。

## 不变量（每个 Stage 都必须保持）

- 单一 `WorkspaceView` 决策不变（plan §4：按 surface 分文件渲染，不引入额外 view state 或跨层抽象）。
- 零行为变化：全部既有测试语义保持；门禁全绿（fmt / clippy -D warnings / test --workspace / 架构检查 / 敏感日志）。
- 秘密卫生：`ConnectionSettings` 永不进入日志、序列化快照或 `Debug` 明文（现有 zeroize/脱敏机制原样保留）。

---

## Stage 2 — 剩余两张 per-tab 边表回归 core（✅ 已交付，2026-08-21）

> 实施注记：字段命名为 `TabState.connection_settings` / `TabState.restored_target`（比原计划的 `settings` 更明确）；两者均为 `Option<Box<_>>`——冷字段加间接层，避免 `AppEvent::TabOpened(TabSnapshot)` 内嵌的 `TabState` 撑爆 clippy `large_enum_variant` 阈值。快照守卫沿用既有 `session_snapshot_never_persists_credentials` 测试（端到端断言 session.json 无密码），未新增冗余用例。

### 现状（实施前）

| 边表 | 内容 | 读写点 |
| --- | --- | --- |
| `restored_targets: HashMap<TabId, RestoredTabTarget>` | session.json 恢复元数据（host/port/username/profile_id/remote_path），无秘密 | mod.rs（写入 351/1031，读取 390/961）、connect_form.rs:262、event_handling.rs:95–135 |
| `tab_settings: HashMap<TabId, ConnectionSettings>` | **含秘密的连接凭据缓存**（zeroize 类型），供断线重连免重输 | mod.rs（写入 910，读取 843/960/389）、connect_form.rs:258、event_handling.rs:135 |

两者与 Stage 1 的 epoch/nav 同构：生命周期 = tab 生命周期，却用需要手动 `remove()` 的边表表达。

### 方案

1. `RestoredTabTarget`（纯数据）移入 core，挂 `TabState.restored_target: Option<RestoredTabTarget>`。
2. `TabState.settings: Option<ConnectionSettings>` —— 推荐此方案（方案 A）：
   - `ConnectionSettings` 本就是 core 类型且已派生 ZeroizeOnDrop、手写 Debug 全量脱敏；
   - 生命周期随 tab 自动终结；符合 plan §15 "长期状态进 core 模型"。
   - 备选方案 B（进程级 SecretStore global）：否决——重新引入我们正在消灭的 side-table 手动生命周期问题。
   - **已知残余风险与缓解**：`TabState` 若未来被加 `Serialize` 会把秘密写进 session.json。缓解：(a) 在字段上写禁止序列化的 doc comment；(b) 新增守卫测试断言 `SessionTabSnapshot` 只取 host/port 且构建路径不触碰 secret 字段（现有 build_session_snapshot 改造后天然满足，测试固化它）。
3. 删除 close_tab 中两行手动清理；connect_form / event_handling / snapshot 构建改走 `find_tab(tab_id)?.restored_target` / `.settings`。

### 验收

- app 内不再存在任何 `HashMap<TabId, _>` 边表（`rg 'HashMap<TabId' crates/app/src/workspace/mod.rs` 仅剩 0 处业务边表）。
- 新增 core 测试：settings/restored_target 随 TabState 默认值安全（None 起步）；snapshot 守卫测试。
- 断线→重连不丢凭据、session 快照内容不变的既有测试保持绿。

### 规模与风险

中等。触及 core.rs + workspace 5 文件 + tests。风险集中在凭据流回归——靠既有 reconnect/prefill/snapshot 测试兜底。

---

## Stage 3 — 平铺 UI 字段按 feature 分组

### 现状

Stage 1/2 后 Workspace 仍有 ~50 个平铺 UI 字段，其中大量只在与同组字段组合时才有意义。

### 分组清单（8 组，约 45 个字段）

| 子结构 | 收编字段 |
| --- | --- |
| `ConnectFormUi { connect_form, connect_form_focus }` | 2 |
| `CommandPaletteUi { open, input, selected, scroll, scrollbar }` | 5 |
| `TabSwitcherUi { open, index, scroll, scrollbar }` + `tab_mru` | 5 |
| `GoToPathUi { open, input, error }` | 3 |
| `TransferDrawerUi { open, height, resize, completed_expanded, failed_expanded, scroll, scrollbar }` | 7 |
| `SettingsUi { section, profile_filter(+focused), external_editor_input(+focused), selected_profile_id, profile_editor, profile_delete_confirm }` | 8 |
| `ModalInputsUi { conflict_rename, conflict_rename_error, delete_confirm, inline_edit, context_menu, large_edit_confirm, about_open }` | 7 |
| `PaneFilterUi`×2（local/remote 各含 filter+scroll+scrollbar）+ `selection_anchor` | 7 |

结果：Workspace 顶层剩 ~15 个字段（state/runtime_client/focus handles×5/surface/focused_side/default_local_path/status_message/config_error/log_file/window_session_id/_subscription）+ 8 个命名组。

### 执行策略

- 纯机械重命名（`self.palette_open` → `self.palette.open`），无逻辑改动；每组一个独立 commit，组间可独立验证、独立回滚。
- 顺序建议：先小后大——GoToPath → ConnectForm → CommandPalette → TabSwitcher → ModalInputs → PaneFilter → TransferDrawer → Settings（最大最后）。
- tests.rs（~5.7k 行）随各组同步改访问路径；不允许借机改断言语义。
- Stage 4 的模块改名搭第一班车一起做（见下）。

### 验收（每组相同）

该组字段的全部访问点收编完成（`rg` 组内字段名零散落）；全量门禁绿；diff 中不含任何非常量表达式变化。

### 规模与风险

大而纯机械。主要风险是漏改/拼写——由编译器穷举兜底，不需要新测试；价值在于字段所有权可见化与后续功能开发的爆炸半径收缩。

---

## Stage 4 — `workspace/mod.rs` → `workspace.rs`

AGENTS.md §2 明令禁止 mod.rs 路径，`crates/app/src/workspace/mod.rs` 是历史遗留违规。

- 机制：`git mv crates/app/src/workspace/mod.rs crates/app/src/workspace.rs`；`main.rs` 的 `mod workspace;` 解析到平文件，子模块继续落在 `workspace/` 目录。零代码改动。
- 搭载时机：Stage 3 第一个 commit 顺带执行（避免单独占一个 PR）。

---

## 排期建议与依赖

```
Stage 2（状态正确性，中）──► Stage 3（机械分组，分 8 个 commit）──► Stage 4（随 3 首班车）
```

- Stage 2 先行：它是仅剩的"架构规则违规/秘密位置"议题，有真实正确性收益；Stage 3 是纯收益打磨。
- 每个 Stage 独立可交付、可中止；中止不留半成品状态。

## 显式非目标

- 不拆分 Workspace 为多个 GPUI View/Entity（违背 plan §4 既定决策）。
- 不动 event_coordinator / resources / session_coordinator（已解耦良好的进程级组件）。
- 不做 core.rs 拆文件（等下一个业务域加入时顺势进行，避免为拆而拆）。
