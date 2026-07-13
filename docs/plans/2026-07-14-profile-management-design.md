# 设计 — Profile 管理 UI/UX

**Date:** 2026-07-14 GMT+8  
**来源：** 用户请求「更好的 profile 管理」；`docs/ui-ux-guidelines.md` §2.1 / §9.1 / §11 / 安全。  
**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。  
**前置：** Phase 5 recents + 现有 Connect 表单内 profile Use/Save/Delete；`ProfileStore` + Keychain。

---

## 决策摘要（已确认）

1. **目标：** 独立管理面（不优先做「仅加速连接」或合并 Recents）。
2. **位置：** Settings 内 **Profiles** 分区（侧栏已有 General）。
3. **MVP 操作：** 列表 + 搜索过滤 + 新建 + 编辑 + 删除确认。
4. **与 Connect：** 双入口 — Settings 管库；Connect 负责连当前 tab（保留 Use / Save）。
5. **架构：** 纯 `app` UI + 现有 `ProfileStore` / Keychain；不合并 Recents。
6. **布局：** 左列表（搜索 + New）/ 右详情编辑。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Settings → Profiles | 打开 Settings 可切换到 Profiles 分区并看到全部档案 |
| 过滤 | 按 name / host / username 子串过滤列表（case-insensitive） |
| 新建 | New Profile → 空白草稿 → Save 写入 profiles.json + Keychain |
| 编辑 | 改 name/host/port/user/auth/paths；Save 更新同一 `ProfileId` |
| 删除 | 确认后删 JSON + Keychain secret；列表刷新 |
| Connect 共存 | Connect 仍可 Use 预填、Save profile；连接流程不被 Settings 阻断 |
| 安全 | 密文不进 UI 日志/status 详情；编辑打开时不预填密码 |

### 非目标

- Profile 分组 UI、拖拽排序、导入/导出 OpenSSH config。
- Recents 与 Profiles 合并为一个「Connections」列表。
- Settings 内一键 Connect 到 tab（明确不做，保持职责清晰）。
- 云同步、多设备、改 SFTP 协议。
- 为共享表单抽出完整 `ProfileEditor` crate（可后续从重复字段再抽）。

---

## 2. 现状与摩擦

| 能力 | 现状 |
| --- | --- |
| 列表 / Use / Delete | 仅 Connect modal 内 |
| Save | Connect 表单底部 |
| Settings | 仅 General / Appearance 侧栏一项 |
| 模型 | `ConnectionProfile` 已含 name、host、port、user、auth、`default_remote_path`、`group_id`、`last_local_path` |
| Recents | 独立 `recents.json`，可挂 `profile_id` |

**摩擦：** 多档案时必须先开 Connect；无独立管理与搜索；删除无确认路径统一；Settings 无法管库。

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

- Saved profiles 列表：`Use` 预填（含 Keychain 读密）。
- `Save profile` 仍可从当前连接表单写入/更新库。
- `Delete`：若保留，必须与 Settings **同一确认语义**（禁止无确认直删）；主删除路径在 Settings。

**Done / Esc：** 离开 Settings 回 Files surface，焦点回 pane（现有行为）。

---

## 4. View 状态

挂在 `Workspace`（view-local，非持久）：

```text
settings_section: General | Profiles   // 默认 General
profile_filter: String                 // 过滤查询
selected_profile_id: Option<ProfileId>
profile_editor: Option<ProfileEditorState>
profile_delete_confirm: Option<ProfileId>
```

`ProfileEditorState`（概念字段，实现可用 `InputState`）：

- `is_new: bool`
- `profile_id: Option<ProfileId>`（new 时 None）
- `name`, `host`, `port`, `username`
- `auth_method`: Password | PrivateKey
- `password`, `key_path`, `passphrase`（编辑已有时 password/passphrase 默认空）
- `default_remote_path`（可选字符串）
- `error: Option<SharedString>`
- `secret_present_hint: bool`（已有 profile 且 Keychain 有条目时 true，仅用于提示文案）

### 导航行为

| 动作 | 行为 |
| --- | --- |
| 进入 Profiles 分区 | 若列表非空且 selection 无效 → 选中第一项并 load editor |
| 点列表行 | `selected_profile_id = id`，从 store 加载 editor（不预填 secret） |
| New Profile | `selected_profile_id = None`，`is_new` 草稿，port=22 |
| 切换 General ↔ Profiles | 保留 filter/selection（同一次 Settings 会话内） |
| 关闭 Settings | 不必强制 flush 未保存草稿；未 Save 的编辑丢弃（YAGNI；可不做 dirty 警告 MVP） |

---

## 5. 密钥与保存语义

| 场景 | 行为 |
| --- | --- |
| 打开已有 profile | 不预填 password/passphrase；可显示 “Password saved in Keychain” / 口令类似提示 |
| Save，密文字段空，已有 profile | **不**改 Keychain；只更新元数据 JSON |
| Save，密文字段非空 | Keychain `store` 后写 JSON |
| Save，New + Password 且密码空 | 校验失败：Password is required（新建） |
| Save，New + PrivateKey 且 key 空 | 校验失败 |
| Delete 确认后 | `delete_profile_secrets` + `delete_profile`；清 selection / editor |

**禁止：** secret 进 status 详情、错误 detail、日志；私钥完整路径诊断需脱敏（现有 AGENTS 规则）。

复用现有逻辑：`save_current_profile` / `use_profile` / `delete_profile` 可拆出 shared helpers，避免两套 Keychain 映射分叉。

---

## 6. UI 细节（规范对齐）

- **密度：** 列表紧凑；右侧表单 max 宽约 560px（对齐 General）。
- **危险操作：** Delete 确认 modal：标题、档案名、主操作 Delete（危险色）、Cancel；绑定 `profile_id`。
- **空态：** “No saved profiles” + New Profile（无营销文案）。
- **过滤无结果：** “No matches”。
- **文案：** 英文单语；可用 “Keychain”；禁 runtime/actor。
- **可访问：** icon-only 必 tooltip；Settings 键盘：侧栏切换可用点击 + 可选快捷键后续。
- **窄窗：** 列表/详情 `min_w_0` + truncate 名称行。

### Palette / 菜单（可选 MVP+）

- `OpenSettings` 已有；可选 `OpenProfiles`：打开 Settings 并 `settings_section = Profiles`。
- 菜单：Settings 内发现即可；不必新 OS 菜单项。

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
| `app` 读改 `resources().profiles` / `keychain` | `ui` 直接磁盘 |
| 复用 `ConnectionProfile` / `AuthMethod` | core 依赖 GPUI |
| 测试 memory Keychain | sftp 为 profile UI 改协议 |

---

## 8. 测试计划

| 用例 | 期望 |
| --- | --- |
| 打开 Settings → Profiles | section 切换；列表长度 = store |
| 过滤 | 仅匹配 name/host/user 的行 |
| New + Save password profile | store 增加；Keychain 有 secret |
| 编辑 host，密码留空 Save | host 变；secret 仍可 load |
| Delete 确认 | 确认后减少；取消不删 |
| Connect Use 仍可用 | 预填 host/user（回归） |
| 未保存草稿关闭 Settings | 再打开不出现脏数据 |

---

## 9. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | `settings_section` + 侧栏 Profiles + 只读列表 + 选中 | 无 |
| **PR2** | 详情编辑 + New + Save + Keychain 语义 | PR1 |
| **PR3** | Delete 确认 + Connect Delete 对齐 | PR2 |
| **PR4** | 过滤、空态 polish、可选 OpenProfiles palette | PR1+ |

---

## 10. 开放问题与取舍

| 项 | 决议 |
| --- | --- |
| Dirty 未保存离开警告 | **MVP 不做**（丢弃草稿） |
| Settings 一键 Connect | **不做** |
| group_id UI | **不做**（字段保留） |
| Connect 内是否去掉 Delete | 可保留但必须确认；主路径 Settings |
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

1. **管库 ≠ 连接** — Settings 管档案库，Connect 连当前 tab。  
2. **空密码更新 = 不改密钥** — 避免误擦 Keychain。  
3. **删除必须确认** — 危险操作显式。  
4. **Recents 保持独立** — 覆盖未保存连接；不混进 Profiles 列表。  
5. **小步 PR** — 先导航与列表，再写路径与删除。
