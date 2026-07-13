# 阶段 5 设计 — 上手与持久化

**Date:** 2026-07-14 GMT+8  
**来源：** `docs/ux-improvement-plan.md` 阶段 5；`docs/ui-ux-guidelines.md` §2.1 / 状态透明 / 安全。  
**方法：** brainstorming；决策已与用户确认（见末尾 §决策记录）。  
**前置：** 阶段 1–4（文件操作、反馈、导航、键盘/palette）。

---

## 决策摘要（已确认）

1. **Session 恢复：** 退出静默写盘；启动自动恢复布局（不询问）。
2. **Recents：** 独立 `recents` 列表，可挂 `profile_id`；与 Saved profiles 并存。
3. **恢复后连接：** 只恢复布局；**不**自动 Connect。
4. **多窗口：** 全局一份 session（进程退出时写；启动恢复到首窗口）。
5. **空态：** 下一步动作（Connect… 等）+ 最近连接列表；无营销页。
6. **交付：** 方案 1 — 独立 `session.json` + `recents.json`，一次设计做透。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Session 恢复 | 退出后 `session.json` 含 tabs；重启后 tab 标题/profile/local·remote path/active 恢复 |
| 不自动连 | 恢复 tab 的 connection 为 Empty/Disconnected；用户主动 Connect/Reconnect |
| Recents | 成功连接写入 recents；空态可点一项预填/重连 |
| 空态引导 | Disconnected：Connect… + recents；无向导/营销文案 |
| 窗口标题 | 标题反映当前 active tab（host 或 tab.title） |

### 非目标

- 自动 SSH / 启动时批量连接。
- session/recents 中存储 password、passphrase、私钥内容（仅 `profile_id` / 元数据）。
- 恢复 in-flight transfer 队列（沿用现有 transfer history）。
- 每窗口独立 session 文件；云同步。
- 恢复 filter/MRU/sort 细节（可选后续；MVP 可只恢复 path + profile + title）。

---

## 2. Session 快照

### 2.1 存储位置

`AppPaths` 增加：

```text
session_file → ~/Library/Application Support/macSFTP/session.json
```

与 `config.json` / `profiles.json` 同目录；`ensure_directories` 覆盖父路径。

### 2.2 文件格式

```json
{
  "version": 1,
  "active_tab_index": 0,
  "tabs": [
    {
      "title": "example.com",
      "profile_id": 3,
      "host": "example.com",
      "port": 22,
      "username": "alex",
      "local_path": "/Users/alex",
      "remote_path": "/home/alex"
    }
  ]
}
```

**规则：**

- **禁止** secret、密码、私钥路径原文可省略或仅存已脱敏的 profile 引用。
- `profile_id` 可选；缺失时恢复为「仅 host 元数据」的断连 tab，Connect 打开表单预填 host/user/port。
- `local_path` / `remote_path` 可选；缺失则 local 用默认 home，remote 为 None。
- `version` 不支持则 **忽略文件** 并启动默认单 tab（WARN 日志）。
- 损坏 JSON / IO 失败：fallback 默认单 tab，不阻塞启动。

### 2.3 写入时机

| 时机 | 行为 |
| --- | --- |
| App quit（`on_app_quit` / 已有 flush 钩子旁） | 序列化**全局** session 并原子写（tmp + rename） |
| 可选：tab 结构变化 debounce | MVP **仅 quit** 即可；若易实现可在 close/open tab 时 best-effort 写 |

**多窗口（决策 A）：**  
- 进程级只保留一份「权威」snapshot。  
- 退出时：优先取 **最后一个仍存活的窗口** 的 tab 列表，或合并规则简化为「主窗口 / 最后创建的 Workspace」——实现选 **最后一个关闭前仍存在的 window 的 Workspace state**；若多窗口同时活着，quit 时序列化 **tabs 最多** 的那个 Workspace，并 WARN。  
- 启动：只向 **第一个** 打开的窗口恢复；后续 Cmd+N 窗口仍 `open_new_tab` 空连接。

### 2.4 读取 / 恢复流程

```text
main / Workspace::new
  → SessionStore::load(path)  // missing → empty
  → if tabs non-empty:
       for each snapshot: allocate TabId, build TabState
         title, profile_id, local.path + load_local_directory
         remote.path = Some (display only), connection = Empty
       set active from active_tab_index
  → else: open_new_tab() as today
```

**不**发 `ConnectTab`。  
Remote pane 显示 Disconnected/Empty + 空态（§4），path bar 可显示恢复的 remote_path 文案（若 connection 未连，列表为空、path 可选显示为「上次路径」灰色或仅在 Reconnect 后使用——**推荐：** 存 `pending_remote_path` 于 tab 扩展或 `tab.remote.path` 在 Empty 时仍可显示 path 字符串但不 listing，直到 Connect 成功后 `request_remote_directory`）。

更清晰方案：

```rust
// TabState 或 view 旁路
restored_remote_path: Option<RemotePath>
// Empty 时 path bar 显示该路径；Connect 成功后 navigate 到此 path
```

MVP 简化：`tab.remote.path = restored` 且 `entries` 空、`connection = Empty`；Connect 成功后用该 path 发 `ReadRemoteDir`（若服务端无此路径再走 error+Retry）。

### 2.5 安全

- 日志不打印完整用户名可接受；**禁止**日志打印 secret。
- session 文件权限跟随用户 home；不写 Keychain 内容。

---

## 3. 最近连接（Recents）

### 3.1 存储

`AppPaths.recents_file` → `.../recents.json`

```json
{
  "version": 1,
  "entries": [
    {
      "id": 1,
      "host": "example.com",
      "port": 22,
      "username": "alex",
      "profile_id": 3,
      "display_name": "Work",
      "last_remote_path": "/home/alex",
      "last_connected_at": 1710000000
    }
  ]
}
```

- 上限：**20** 条；同 `(host, port, username, profile_id)` 更新时间戳并移到最前（去重）。
- **禁止** secret。
- 原子写；损坏则空列表。

### 3.2 写入时机

- `TabConnected` 成功时：从 tab + `tab_settings` / profile 提取 host/port/user/profile_id/remote_root 或 current remote path，upsert recents。
- 不在失败连接时写入。

### 3.3 使用

空态列表点击一项：

1. 若有 `profile_id` 且 profile 仍在：预填 connect form 或直接 `connect_with` 从 Keychain 取 secret（与现有 profile 流程一致）。  
2. 若无 profile：打开 connect form 预填 host/port/username；用户补密码。  
3. 可选：记录 `last_remote_path` 供连上后导航。

---

## 4. 空态与首次引导

### 4.1 Remote pane `Empty` / `Disconnected`

```text
Not connected
[ Connect… (⌘⇧R) ]   [ 可选：Open Connection 同义 ]

Recent connections
  Work · alex@example.com:22
  lab · root@10.0.0.2:22
  …
```

- **无**「欢迎使用 macSFTP」长文案、无轮播、无营销。
- 无 recents 时只显示 Connect。
- Local pane 保持可浏览（已有 home）；可不在 local 空列表再塞 recents。

### 4.2 无 tab 时

现有 `No connections` + New Tab 保留。

### 4.3 首次安装

无 session、无 recents → 与今相同：一 tab + Connect 空态。即「直达工作区」。

---

## 5. 窗口标题

- 有 active tab：`{tab.title} — macSFTP` 或 `{host} — macSFTP`。  
- 无 tab：`macSFTP`。  
- 多窗口：每窗口按**自己** active tab 更新（标题不要求跨窗口唯一）。  
- 实现：在 `activate_tab` / connect 完成 / disconnect / close 时调用 GPUI window title API（查现有 `Window` 更新 title 方法；若仅创建时设定，则需在生命周期中刷新）。

---

## 6. 架构边界

```text
Workspace / main
  │
  ├─ SessionStore (storage) ── session.json
  ├─ RecentsStore (storage) ── recents.json
  ├─ ProfileStore (existing)
  └─ Keychain (existing, secrets only)

App quit ──► SessionStore.save(snapshot from Workspace)
TabConnected ──► RecentsStore.upsert(...)
Startup ──► SessionStore.load ──► rebuild tabs (no Connect)
```

**禁止：** core 依赖磁盘路径细节；session 写入 secret；GPUI 主线程同步大文件阻塞（session/recents 很小，同步 OK）。

**crate 归属：**  
- `macsftp_storage`：`SessionFile` / `SessionStore`、`RecentsFile` / `RecentsStore`  
- `macsftp_platform`：`AppPaths` 新字段  
- `macsftp_app`：序列化快照、恢复、空态 UI、标题  

---

## 7. 测试计划

### Storage

- session round-trip；损坏 JSON fallback  
- recents upsert 去重与 cap 20  
- 无 secret 字段出现在序列化结果  

### App

- 构造 snapshot → `Workspace` 恢复后 tab 数/path/profile 一致，connection 非 Connected  
- 不发出 `ConnectTab`（可检查 command channel 无 Connect）  
- TabConnected 后 recents 增加  
- 空态展示 recents 项（可测 store + 渲染数据，不必 UI 像素）  

### 手测

- 连服务器、开两 tab、设 local/remote path → 退出 → 重启见 tabs，未自动连  
- Reconnect 成功  
- 空 tab 点 recent 能连  
- 窗口标题随 tab 变  

---

## 8. 改动文件清单

| 文件 | 改动 |
| --- | --- |
| `crates/platform/src/platform.rs` | `session_file` / `recents_file` |
| `crates/storage/src/session.rs`（新） | session load/save |
| `crates/storage/src/recents.rs`（新） | recents load/save/upsert |
| `crates/storage/src/storage.rs` | mod 导出 |
| `crates/app/src/resources.rs` | 挂载 stores |
| `crates/app/src/workspace/mod.rs` | quit flush session；new 时 restore |
| `crates/app/src/workspace/event_handling.rs` | TabConnected → recents |
| `crates/app/src/workspace/render.rs` | 空态 recents；window title 触发点 |
| `crates/app/src/main.rs` | 路径 ensure；标题若在 window 层 |
| 测试 | storage unit + app restore |

---

## 9. 建议 PR 切分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | AppPaths + SessionStore + 启动恢复 + quit 保存 | 无 |
| **PR2** | RecentsStore + TabConnected 写入 + 空态列表 | 无（可并行） |
| **PR3** | 窗口标题 | 无 |
| **PR4** | 空态文案 polish + Connect/recent 一键行为串联 | PR1+PR2 |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| 恢复是否含 filter/sort/nav history | **否**（MVP） |
| 恢复是否含 drawer 开合 | **否** |
| profile 被删后的 tab | 保留 host 元数据；Connect 走表单 |
| 多窗口同时改 tabs | 最后退出写一份；可接受丢失另一窗口 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | Session 恢复策略 | A — 静默自动恢复布局 |
| 2 | Recents 与 profiles | A — 独立 recents + profile 链接 |
| 3 | 恢复后是否自动连 | A — 不自动 Connect |
| 4 | 多窗口 session | A — 全局一份 |
| 5 | 空态引导 | A — 动作 + recents |
| 6 | 交付 | 方案 1 — session + recents 分文件做透 |

---

## Key Decisions

1. **布局恢复 ≠ 自动连接** — 安全与启动可预期性优先。  
2. **Secret 只走 Keychain + profile_id** — session/recents 纯元数据。  
3. **Recents 独立于 profiles** — 覆盖未保存连接。  
4. **单文件全局 session** — 多窗口 MVP 够用。  
5. **空态只给下一步** — 符合规范 §2.1。
