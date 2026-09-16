# 设计 — Profile 管理 UI/UX

**Date:** 2026-07-14 GMT+8

**来源：** 用户请求「更好的 profile 管理」；`docs/ui-ux-guidelines.md` §2.1 / §9.1 / §11 / 安全。

**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。

**前置：** Phase 5 recents + 现有 Connect 表单内 profile Use/Save/Delete；`ProfileStore` + Keychain。

---

## 决策摘要（已确认）

1. **目标：** 提供独立管理界面，因此不优先实现「仅加速连接」或合并 Recents。
2. **位置：** Settings 内 **Profiles** 分区（侧栏已有 General）。
3. **MVP 操作：** 列表、搜索过滤、新建、编辑和删除确认。
4. **与 Connect 的关系：** 提供两个入口；Settings 管理档案库，而 Connect 负责连接当前 tab，并保留 Use / Save。
5. **架构：** 使用纯 `app` UI 和现有 `ProfileStore` / Keychain，但是不合并 Recents。
6. **布局：** 左侧显示列表（搜索 + New），右侧显示详情编辑界面。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Settings → Profiles | 显示 Settings 后可切换到 Profiles 分区，并查看全部档案 |
| 过滤 | 按 name / host / username 子串过滤列表（case-insensitive） |
| 新建 | New Profile → 空白草稿 → Save 写入 profiles.json 和 Keychain |
| 编辑 | 改 name/host/port/user/auth/paths；Save 更新同一 `ProfileId` |
| 删除 | 确认后删除 JSON 和 Keychain secret；随后刷新列表 |
| Connect 共存 | Connect 仍可通过 Use 预填，也可 Save profile；Settings 不能阻断连接流程 |
| 安全 | 密文不能进入 UI 日志或 status 详情；显示编辑界面时不预填密码 |

### 非目标

- Profile 分组 UI、拖拽排序以及导入或导出 OpenSSH config。
- Recents 与 Profiles 合并为一个「Connections」列表。
- Settings 内一键 Connect 到 tab；该能力不实现，因此 Settings 与 Connect 的职责保持清晰。
- 云同步、多设备以及修改 SFTP 协议。
- 不为共享表单提取完整的 `ProfileEditor` crate；如果后续出现重复字段，则可以再提取该 crate。

---

## 2. 现状与摩擦

| 能力 | 现状 |
| --- | --- |
| 列表 / Use / Delete | 仅 Connect modal 内 |
| Save | Connect 表单底部 |
| Settings | 仅 General / Appearance 侧栏一项 |
| 模型 | `ConnectionProfile` 已含 name、host、port、user、auth、`default_remote_path`、`group_id`、`last_local_path` |
| Recents | 独立 `recents.json`，可挂 `profile_id` |

**现有问题：** 存在多个档案时必须先显示 Connect；系统缺少独立管理和搜索能力；删除操作没有统一的确认流程；Settings 无法管理档案库。

---

## 3. 信息架构

```text
Settings (⌘, / OpenSettings)
├── 侧栏
│   ├── General      → Appearance（现有）
│   └── Profiles     → 档案库（新）
└── Profiles 主区
    ├── 左栏
    │   ├── 过滤输入
    │   ├── [New Profile]
    │   └── 列表行：name · user@host:port（选中高亮）
    └── 右栏（详情）
        ├── 空库 / 未选中：引导文案 + New
        └── 编辑中：字段 + [Save] [Delete…]
```

**Connect form（保留）：**

- Saved profiles 列表：`Use` 预填字段，并且从 Keychain 读取密文。
- `Save profile` 仍可从当前连接表单写入/更新库。
- `Delete`：如果保留，则必须与 Settings 使用**相同的确认语义**，并且禁止未经确认直接删除；主要删除入口位于 Settings。

**Done / Esc：** 离开 Settings 后返回 Files surface，并将焦点恢复至 pane（现有行为）。

---

## 4. View 状态

以下状态存储在 `Workspace` 中（view-local，不持久化）：

```text
settings_section: General | Profiles   // 默认 General
profile_filter: String                 // 过滤查询
selected_profile_id: Option<ProfileId>
profile_editor: Option<ProfileEditorState>
profile_delete_confirm: Option<ProfileId>
```

`ProfileEditorState`（概念字段，实现可用 `InputState`）：

- `is_new: bool`
- `profile_id: Option<ProfileId>`（new 时为 None）
- `name`, `host`, `port`, `username`
- `auth_method`: Password | PrivateKey
- `password`, `key_path`, `passphrase`（编辑已有时 password/passphrase 默认空）
- `default_remote_path`（可选字符串）
- `error: Option<SharedString>`
- `secret_present_hint: bool`（已有 profile 且 Keychain 存在条目时为 true，并且仅用于提示文案）

### 导航行为

| 动作 | 行为 |
| --- | --- |
| 进入 Profiles 分区 | 如果列表非空且 selection 无效，则选中第一项并 load editor |
| 单击列表行 | 设置 `selected_profile_id = id`，并从 store 加载 editor，但是不预填 secret |
| New Profile | `selected_profile_id = None`，`is_new` 草稿，port=22 |
| 切换 General ↔ Profiles | 在同一次 Settings 会话内保留 filter/selection |
| 关闭 Settings | 无需强制 flush 未保存草稿；未 Save 的编辑会丢弃，因此 MVP 不实现 dirty 警告（YAGNI） |

---

## 5. 密钥与保存语义

| 场景 | 行为 |
| --- | --- |
| 显示已有 profile | 不预填 password/passphrase；可以显示 “Password saved in Keychain” 或同类口令提示 |
| Save，密文字段空，已有 profile | **不**修改 Keychain，只更新元数据 JSON |
| Save，密文字段非空 | 先执行 Keychain `store`，然后写入 JSON |
| Save，New + Password 且密码空 | 校验失败：Password is required（新建） |
| Save，New + PrivateKey 且 key 空 | 校验失败 |
| Delete 确认后 | 执行 `delete_profile_secrets` 和 `delete_profile`，然后清空 selection / editor |

**禁止：** secret 不能进入 status 详情、错误 detail 或日志；私钥完整路径的诊断信息必须脱敏（现有 AGENTS 规则）。

复用现有逻辑：`save_current_profile` / `use_profile` / `delete_profile` 可以提取 shared helpers，因此不会形成两套不同的 Keychain 映射。

---

## 6. UI 细节（规范对齐）

- **密度：** 列表采用紧凑布局；右侧表单的 max 宽约为 560px，并与 General 保持一致。
- **危险操作：** Delete 确认 modal：标题、档案名、主操作 Delete（危险色）、Cancel；绑定 `profile_id`。
- **空态：** “No saved profiles” + New Profile（无营销文案）。
- **过滤无结果：** “No matches”。
- **文案：** 使用英文单语；可以使用 “Keychain”，但是不能使用 runtime/actor。
- **可访问：** icon-only 控件必须提供 tooltip；Settings 侧栏可以通过单击切换，而快捷键可在后续增加。
- **窄窗：** 列表和详情使用 `min_w_0`，并且对名称行应用 truncate。

### Palette / 菜单（可选 MVP+）

- `OpenSettings` 已有；可选的 `OpenProfiles` 会显示 Settings，并设置 `settings_section = Profiles`。
- 用户可以从 Settings 中发现该功能，因此无需增加 OS 菜单项。

---

## 7. 架构边界

```text
Workspace Settings UI
  → ProfileStore (profiles.json)
  → KeychainStore (secrets only)
  → (unchanged) Connect form / Recents
```

| 允许 | 禁止 |
| --- | --- |
| `app` 读写 `resources().profiles` / `keychain` | `ui` 直接访问磁盘 |
| 复用 `ConnectionProfile` / `AuthMethod` | core 依赖 GPUI |
| 测试 memory Keychain | sftp 因 profile UI 而修改协议 |

---

## 8. 测试计划

| 用例 | 期望 |
| --- | --- |
| 显示 Settings → Profiles | section 切换；列表长度 = store |
| 过滤 | 仅匹配 name/host/user 的行 |
| New + Save password profile | store 增加；Keychain 存在 secret |
| 编辑 host，密码留空 Save | host 更新；secret 仍可 load |
| Delete 确认 | 确认后数量减少；取消后不删除 |
| Connect Use 仍可用 | 预填 host/user（回归） |
| 未保存草稿关闭 Settings | 再次显示 Settings 时不出现未保存数据 |

---

## 9. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | `settings_section` + 侧栏 Profiles + 只读列表 + 选中 | 无 |
| **PR2** | 详情编辑 + New + Save + Keychain 语义 | PR1 |
| **PR3** | Delete 确认 + Connect Delete 语义一致性 | PR2 |
| **PR4** | 过滤、空态 polish、可选 OpenProfiles palette | PR1+ |

---

## 10. 开放问题与取舍

| 项 | 决议 |
| --- | --- |
| Dirty 未保存离开警告 | **MVP 不做**（丢弃草稿） |
| Settings 一键 Connect | **不做** |
| group_id UI | **不做**（字段保留） |
| Connect 内是否移除 Delete | 可以保留，但是必须确认；主要入口位于 Settings |
| 与 Recents 去重展示 | **不做合并**；仅 profile_id 链接 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | 优先问题 | A — 独立管理面 |
| 2 | 放置位置 | A — Settings 内 Profiles |
| 3 | MVP 范围 | A — 列表+编辑+删除+新建 |
| 4 | 与 Connect | A — 双入口清晰 |
| 5 | 架构 | A — app UI + 现有 store |
| 6 | 布局 | 左列表 / 右详情 |

---

## Key Decisions

1. **管理档案库 ≠ 连接**：Settings 管理档案库，而 Connect 连接当前 tab。
2. **空密码更新 = 不修改密钥**：因此不会错误清除 Keychain 数据。
3. **删除必须确认**：因此危险操作具有明确的确认步骤。
4. **Recents 保持独立**：Recents 覆盖未保存连接，但是不合并至 Profiles 列表。
5. **按阶段提交 PR**：先实现导航与列表，然后实现写入路径与删除。
