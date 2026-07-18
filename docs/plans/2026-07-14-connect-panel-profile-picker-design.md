# 设计 — Connect 面板 Profile 选择重构

**Date:** 2026-07-14 GMT+8  
**来源：** 用户反馈 Connect 内 Saved profiles 列表占面积、选择不便；Settings Profiles 已具备 CRUD。  
**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。  
**前置：** Profile 管理（Settings → Profiles）；现有 Connect form `use_profile` / `save_current_profile`。

---

## 决策摘要（已确认）

1. **选档案：** 顶部一行紧凑 **下拉 / popover**，不再展开全列表。  
2. **管理：** Connect 内 **去掉** 每行 Delete；CRUD/删除只在 Settings。  
3. **保存：** Save as **默认折叠**（「Save as profile…」展开后 name + Save）。  
4. **选中后：** 立即 `use_profile` 预填，字段仍可改。  
5. **列表实现：** 轻量 anchored popover，可滚动 + 内嵌过滤。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| 面积 | 无档案 / 有 N 个档案时，Connect 主卡片高度不随 N 线性增长 |
| 选档案 | 打开 picker → 点一项 → host/user/auth 已预填（含 Keychain 密） |
| Manual | 可选「Manual entry」：断开 `source_profile_id`，清空 secret 字段 |
| 过滤 | picker 内按 name/host/username 子串过滤 |
| 保存 | 默认不显示完整 Save 行；展开后行为与现 Save profile 一致 |
| 删除 | Connect UI 无 Delete 按钮；Settings 确认删除不变 |

### 非目标

- Settings 一键 Connect  
- 原生系统 combobox 控件或新 crate  
- 选中后自动 Connect（误触成本高）  
- 重构 Connect 字段布局（Host/Port 等保持现结构，只改 profile 区）

---

## 2. 现状与摩擦

当前 `render_connect_form_modal`（`modals.rs`）：

- 顶部对 **每个** profile 渲染 2 行文案 + Use + Delete。  
- 再 + 全字段 + Save as 固定行 + Cancel/Connect。  
- 档案多时 modal 过高；与 Settings Profiles 职责重复。

---

## 3. 信息架构

```text
Connect to Server
[error if any]

Profile   [ {label} ▾ ]     ← 一行；label = 档案名 或 "Manual entry"
          └─ popover (open 时)
               [filter input]
               scrollable rows: name · user@host:port
               ───
               Manual entry

Host / Port / Username / Auth / secrets…   ← 不变

▸ Save as profile…          ← 折叠；展开：
    [name optional] [Save profile]

              [Cancel] [Connect]
```

**宽度：** 保持约 `460px` 卡片宽（可微调）；popover 宽度对齐触发条。

---

## 4. View 状态

扩展 `ConnectForm`（或 Workspace 旁路，优先挂在 form 上）：

```text
profile_picker_open: bool
profile_picker_filter: InputState   // 或 String
save_as_expanded: bool              // 默认 false
// 已有: source_profile_id, host, port, …
```

### 行为

| 动作 | 行为 |
| --- | --- |
| 打开 Connect | `profile_picker_open = false`，`save_as_expanded = false`；filter 清空 |
| 点 Profile 触发器 | toggle `profile_picker_open` |
| 过滤变更 | 过滤 `resources().profiles`（name/host/user，case-insensitive；复用 `profile_matches_filter`） |
| 点档案行 | `use_profile(id)`；关闭 picker；触发器文案 = profile.name |
| 点 Manual entry | `source_profile_id = None`；清空 password/passphrase（及可选 key 若需干净手填）；**保留**已改的 host 字段或全部清空？→ **MVP：清空 secret + 断链；host/user/port 保留当前表单值**（用户可能从档案改到手动） |
| Esc | 若 picker open → 先关 picker；否则关 Connect（现有 CancelActiveModal） |
| 点展开 Save as | `save_as_expanded = true` |
| Save profile | 现有 `save_current_profile`；成功后可选折叠 Save as |
| 打开 Settings 删除档案 | 若 `source_profile_id` 已删，下次渲染触发器显示 Manual entry |

**触发器标签：**

- `source_profile_id` 且 store 中仍存在 → `profile.name`  
- 否则 → `Manual entry`

**偏离档案（可选 MVP+）：** 用户改 host 后不强制清 `source_profile_id`（更新 Save 仍可落到原 id）。MVP **不**做「已修改」徽章。

---

## 5. 键盘与无障碍

- Picker 打开时：↑/↓ 移动高亮（若易实现）；Enter 选中；Esc 关闭 picker。  
- MVP 可先鼠标 + filter + 点击；键盘增强为 follow-up。  
- 触发器与列表行需可点、focus 可见；icon-only 用 tooltip。  
- 无档案时：触发器仍为 Manual entry；popover 仅 Manual + 短文案 “No saved profiles — manage in Settings”。

---

## 6. 与 Settings / Recents 关系

| 入口 | 职责 |
| --- | --- |
| Connect picker | 选档案预填 → 连 |
| Settings Profiles | 新建/编辑/删除库 |
| Empty remote Recents | 预填/连（已有）；不改本期范围，除非文案交叉 |

Connect **不**再提供 Delete；用户从 picker 旁可放可选链接文案 “Manage…” → `OpenProfiles`（可选，一行 secondary，不占列表高度）。

---

## 7. 架构边界

```text
ConnectForm UI (modals.rs / connect_form.rs)
  → use_profile / save_current_profile (existing)
  → profile_matches_filter (profiles.rs)
  → ProfileStore / Keychain (read)
```

- 无 sftp 变更；无 storage schema 变更。  
- 删除确认 modal 仍仅从 Settings（及若别处 `request_delete_profile`）触发；Connect 调用点删除。

---

## 8. 测试计划

| 用例 | 期望 |
| --- | --- |
| 有档案时打开 Connect | 无每档案 Use/Delete 行；有 Profile 触发器 |
| 打开 picker 选档案 | `source_profile_id` 设置；host/user 匹配 |
| Manual entry | `source_profile_id` None |
| 过滤 | 仅匹配行（pure 或 gpui） |
| Save as 折叠 | 默认无 profile-name 输入；展开后可 Save |
| Connect Delete 按钮 | 不存在 |
| 回归 | submit connect、save profile from expanded Save as |

---

## 9. 建议 PR 切分

| PR | 内容 |
| --- | --- |
| **PR1** | 去掉全列表 + Profile 触发器 + popover 列表（无过滤可先） |
| **PR2** | picker 过滤 + Esc 关 picker + Manual entry |
| **PR3** | Save as 折叠；可选 Manage… → OpenProfiles |
| **PR4** | 键盘导航 polish（可选） |

---

## 10. 取舍

| 项 | 决议 |
| --- | --- |
| 选中后自动 Connect | **否** |
| Connect 内 Delete | **否** |
| Save as | **折叠保留** |
| Manual 是否清空 host | **否**（只断链 + 清 secret） |
| Dirty 徽章 | **MVP 不做** |

---

## 决策记录

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 选档案交互 | A — 紧凑下拉/popover |
| 2 | 管理能力 | A — 只选 + 可选保存 |
| 3 | 选中后 | A — 预填可改 |
| 4 | 实现 | A — 轻量 popover + 过滤 |
| 5 | §2 行为 | OK 按方案 |

---

## Key Decisions

1. **Connect 是连接器，不是档案管理器** — 列表 CRUD 已在 Settings。  
2. **一行触发器换线性列表** — 解决面积与扫描成本。  
3. **Save as 次要化** — 默认不占垂直空间。  
4. **预填可改** — 兼顾档案与临时改参。
