# 设计 — Connect 面板 Profile 选择重构

**Date:** 2026-07-14 GMT+8  
**来源：** 用户反馈指出，Connect 内的 Saved profiles 列表占用较多空间，并且选择操作不便；Settings Profiles 已经提供 CRUD。
**方法：** brainstorming；相关决策已由用户确认，详见末尾“决策记录”。
**前置条件：** Profile 管理（Settings → Profiles），以及现有 Connect form 的 `use_profile` / `save_current_profile`。

---

## 决策摘要（已确认）

1. **选择档案：** 顶部使用单行紧凑型 **下拉列表 / popover**，因此不再展示完整列表。
2. **管理档案：** Connect 内移除每行 Delete；CRUD 和删除功能仅位于 Settings。
3. **保存档案：** Save as **默认折叠**；用户选择“Save as profile…”后，界面显示 name 和 Save。
4. **选择完成后的行为：** 系统立即调用 `use_profile` 预填字段，但是字段仍可修改。
5. **列表实现：** 使用轻量 anchored popover，并且支持滚动和内嵌过滤。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| 面积 | 无档案或存在 N 个档案时，Connect 主卡片高度均不随 N 线性增长 |
| 选择档案 | 显示 picker → 选择一项 → host/user/auth 已预填，并且包含 Keychain 中的 secret |
| Manual | 可以选择“Manual entry”；选择后解除 `source_profile_id` 关联，并清空 secret 字段 |
| 过滤 | picker 内可以按照 name/host/username 的子串过滤 |
| 保存 | 默认不显示完整 Save 行；用户展开该区域后，行为与现有 Save profile 一致 |
| 删除 | Connect UI 不包含 Delete 按钮；Settings 的删除确认行为保持不变 |

### 非目标

- 在 Settings 中提供一键 Connect
- 使用原生系统 combobox 控件或新增 crate
- 选择档案后自动 Connect，因为误触成本较高
- 重构 Connect 字段布局；Host/Port 等字段保持现有结构，本次只修改 profile 区域

---

## 2. 现状与问题

当前 `render_connect_form_modal`（`modals.rs`）具有以下结构：

- 顶部为每个 profile 渲染 2 行文本，以及 Use 和 Delete。
- 后续区域包含全部字段、固定展示的 Save as 行，以及 Cancel/Connect。
- 因此档案数量较多时，modal 高度过大；同时，该区域与 Settings Profiles 的职责重复。

---

## 3. 信息架构

```text
Connect to Server
[error if any]

Profile   [ {label} ▾ ]     ← 单行；label = 档案名或 "Manual entry"
          └─ popover（显示时）
               [filter input]
               scrollable rows: name · user@host:port
               ───
               Manual entry

Host / Port / Username / Auth / secrets…   ← 保持不变

▸ Save as profile…          ← 默认折叠；展开后：
    [name optional] [Save profile]

              [Cancel] [Connect]
```

**宽度：** 卡片宽度维持约 `460px`，并允许小幅调整；popover 宽度与触发条一致。

---

## 4. View 状态

扩展 `ConnectForm`。如果必须使用 Workspace 旁路状态，也应优先将状态存入 form：

```text
profile_picker_open: bool
profile_picker_filter: InputState   // 或 String
save_as_expanded: bool              // 默认 false
// 已有: source_profile_id, host, port, …
```

### 行为

| 动作 | 行为 |
| --- | --- |
| 显示 Connect | 设置 `profile_picker_open = false` 和 `save_as_expanded = false`，并且清空 filter |
| 选择 Profile 触发器 | 切换 `profile_picker_open` |
| 修改过滤条件 | 过滤 `resources().profiles`；匹配 name/host/user，忽略大小写，并复用 `profile_matches_filter` |
| 选择档案行 | 调用 `use_profile(id)`，关闭 picker，并将触发器文本设置为 profile.name |
| 选择 Manual entry | 设置 `source_profile_id = None`，清空 password/passphrase；如果手动输入流程要求全新值，也可以清空 key。**MVP 只清空 secret 并解除档案关联，同时保留当前表单中的 host/user/port**，因为用户可能先使用档案，再修改为手动配置 |
| Esc | 如果 picker 已显示，则先关闭 picker；否则关闭 Connect，并沿用现有 CancelActiveModal |
| 选择 Save as 展开项 | 设置 `save_as_expanded = true` |
| Save profile | 调用现有 `save_current_profile`；成功后可以折叠 Save as |
| 在 Settings 中删除档案 | 如果 `source_profile_id` 对应的档案已经删除，那么下一次渲染时，触发器显示 Manual entry |

**触发器标签：**

- 如果 `source_profile_id` 存在，并且 store 中仍有对应档案，那么显示 `profile.name`。
- 否则显示 `Manual entry`。

**修改档案字段后的状态（可选 MVP+）：** 用户修改 host 后，系统不强制清除 `source_profile_id`，因此后续 Save 仍可更新原 id。MVP 不提供“已修改”徽章。

---

## 5. 键盘与无障碍

- Picker 显示时，如果实现成本合理，则使用 ↑/↓ 移动高亮项、Enter 确认选择，并使用 Esc 关闭 picker。
- MVP 可以先支持鼠标、filter 和选择操作；键盘增强功能可以在后续版本实现。
- 触发器和列表行需要支持交互，并且 focus 状态必须可见；icon-only 控件需要 tooltip。
- 无档案时，触发器仍显示 Manual entry；popover 只显示 Manual 和简短文本“No saved profiles — manage in Settings”。

---

## 6. 与 Settings / Recents 的关系

| 入口 | 职责 |
| --- | --- |
| Connect picker | 选择档案并预填字段；用户仍需提交 Connect 才会连接 |
| Settings Profiles | 新建、编辑和删除档案库内容 |
| Empty remote Recents | 预填字段或执行连接；该入口已经存在，因此本期不修改，除非需要统一交叉文案 |

Connect 不再提供 Delete。picker 旁边可以增加可选链接“Manage…”并映射到 `OpenProfiles`；该链接属于单行 secondary 内容，因此不会增加列表高度。

---

## 7. 架构边界

```text
ConnectForm UI (modals.rs / connect_form.rs)
  → use_profile / save_current_profile (existing)
  → profile_matches_filter (profiles.rs)
  → ProfileStore / Keychain (read)
```

- 不修改 sftp。
- 不修改 storage schema。
- 删除确认 modal 仍只由 Settings 触发；如果其他位置已有 `request_delete_profile`，则保留该调用。本次需要移除 Connect 中的删除调用点。

---

## 8. 测试计划

| 用例 | 期望 |
| --- | --- |
| 存在档案时显示 Connect | 不显示每个档案对应的 Use/Delete 行，并且显示 Profile 触发器 |
| 显示 picker 并选择档案 | 已设置 `source_profile_id`，并且 host/user 与档案一致 |
| Manual entry | `source_profile_id` 为 None |
| 过滤 | 只显示匹配行；测试可以使用 pure 或 gpui |
| Save as 折叠 | 默认不显示 profile-name 输入；展开后可以执行 Save |
| Connect Delete 按钮 | 不存在 |
| 回归 | submit connect 和 save profile from expanded Save as 均保持有效 |

---

## 9. 建议 PR 划分

| PR | 内容 |
| --- | --- |
| **PR1** | 移除完整列表，并增加 Profile 触发器和 popover 列表；过滤功能可以后续实现 |
| **PR2** | 增加 picker 过滤、Esc 关闭 picker 和 Manual entry |
| **PR3** | 默认折叠 Save as；可以增加 Manage… → OpenProfiles |
| **PR4** | 改进键盘导航；该 PR 可选 |

---

## 10. 取舍

| 项 | 决议 |
| --- | --- |
| 选择后自动 Connect | **否** |
| Connect 内 Delete | **否** |
| Save as | **折叠保留** |
| Manual 是否清空 host | **否**，只解除档案关联并清空 secret |
| Dirty 徽章 | **MVP 不提供** |

---

## 决策记录

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 档案选择交互 | A — 紧凑下拉列表 / popover |
| 2 | 管理能力 | A — 只提供选择功能，并可以选择保存 |
| 3 | 选择完成后的行为 | A — 预填后仍可修改 |
| 4 | 实现方式 | A — 轻量 popover 与过滤 |
| 5 | §2 行为 | OK，按照方案执行 |

---

## Key Decisions

1. **Connect 负责连接操作。** 档案列表的 CRUD 已经位于 Settings，因此 Connect 不再负责档案管理。
2. **单行触发器替代线性列表。** 因此 Connect 占用的面积不再随档案数量线性增加，并且用户可以通过过滤降低查找成本。
3. **Save as 属于次要操作。** 因此该区域默认折叠，不占用固定的垂直空间。
4. **预填后仍可修改。** 因此用户既可以复用档案，也可以临时调整连接参数。
