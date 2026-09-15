# 阶段 5 设计 — 初次使用与持久化

**日期：** 2026-07-14 GMT+8
**来源：** `docs/ux-improvement-plan.md` 阶段 5；`docs/ui-ux-guidelines.md` §2.1、状态透明和安全要求。
**方法：** brainstorming；用户已经确认相关决策，具体内容参见末尾“决策记录”。
**前置条件：** 阶段 1–4 已经完成，其中包括文件操作、反馈、导航以及键盘/palette。

---

## 决策摘要（已确认）

1. **Session 恢复：** 退出时静默写入文件；启动时自动恢复布局，并且不询问用户。
2. **Recents：** 使用独立的 `recents` 列表，其中可以包含 `profile_id`；该列表与 Saved profiles 同时存在。
3. **恢复后连接：** 只恢复布局，**不**自动 Connect。
4. **多窗口：** 全局使用一份 session。进程退出时写入文件，启动时在首个窗口中恢复。
5. **空状态：** 显示后续操作（Connect… 等）和最近连接列表，但是不显示营销页面。
6. **交付：** 采用方案 1，也就是独立的 `session.json` 和 `recents.json`，并在一份设计中完整说明。

---

## 1. 目标与非目标

### 目标

| 子项 | 成功标准 |
| --- | --- |
| Session 恢复 | 退出后，`session.json` 包含 tabs；应用重新启动后，tab 标题、profile、local/remote path 和 active 状态均恢复 |
| 不自动连接 | 恢复后的 tab 使用 Empty/Disconnected connection；用户需要主动执行 Connect/Reconnect |
| Recents | 成功连接后写入 recents；用户可以在空状态中选择一项并预填信息或重新连接 |
| 空状态指引 | Disconnected 状态显示 Connect… 和 recents，但是不显示向导或营销文案 |
| 窗口标题 | 标题反映当前 active tab，优先显示 host 或 tab.title |

### 非目标

- 自动建立 SSH 连接，或在启动时批量连接。
- 在 session/recents 中存储 password、passphrase 或私钥内容；这些文件只包含 `profile_id` 和元数据。
- 恢复 in-flight transfer 队列；transfer history 继续使用现有行为。
- 为每个窗口提供独立 session 文件，或提供云同步。
- 恢复 filter、MRU 和 sort 的细节。MVP 只需恢复 path、profile 和 title，其他内容可以后续评估。

---

## 2. Session 快照

### 2.1 存储位置

为 `AppPaths` 增加：

```text
session_file → ~/Library/Application Support/macSFTP/session.json
```

该文件与 `config.json` 和 `profiles.json` 位于同一目录，并且 `ensure_directories` 需要包含其父目录。

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

- 文件中**禁止**包含 secret 和密码。私钥路径原文可以省略，或者仅保存已经脱敏的 profile 引用。
- `profile_id` 是可选字段。如果该字段缺失，则恢复为只包含 host 元数据的断开连接 tab；用户执行 Connect 后，表单预填 host、user 和 port。
- `local_path` 和 `remote_path` 是可选字段。如果 `local_path` 缺失，则使用默认 home；如果 `remote_path` 缺失，则使用 None。
- 如果 `version` 不受支持，则**忽略该文件**，记录 WARN 日志，并使用默认的单 tab 启动状态。
- 如果 JSON 损坏或 IO 失败，则使用默认的单 tab 启动状态，因此该错误不能阻塞启动。

### 2.3 写入时机

| 时机 | 行为 |
| --- | --- |
| App quit（`on_app_quit` / 现有 flush hook 附近） | 序列化**全局** session，并使用 tmp + rename 原子写入 |
| 可选：tab 结构变化 debounce | MVP 只需在 quit 时写入；如果实现成本较低，也可以在 close/open tab 时执行 best-effort 写入 |

**多窗口（决策 A）：**

- 进程级只保存一份权威 snapshot。
- 退出时，优先使用最后一个仍然存在的窗口所包含的 tab 列表。也可以将规则简化为主窗口或最后创建的 `Workspace`，但是实现选择“关闭前最后一个仍然存在的 window 的 Workspace state”。如果退出时仍有多个窗口，则序列化 tabs 数量最多的 Workspace，并记录 WARN。
- 启动时，只在第一个窗口中恢复。后续通过 Cmd+N 创建的窗口仍然使用 `open_new_tab` 的空连接状态。

### 2.4 读取与恢复流程

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

该流程**不得**发送 `ConnectTab`。

Remote pane 显示 Disconnected/Empty 和空状态（§4）。如果快照包含 remote_path，那么 path bar 可以显示该路径。由于 connection 尚未建立，因此列表为空；界面可以将路径标示为“上次路径”，或者在 Reconnect 后再使用该路径。推荐在 tab 扩展状态中保存 `pending_remote_path`，或者在 Empty 状态下继续通过 `tab.remote.path` 保存路径字符串，但是在连接成功前不请求 listing。

更明确的结构如下：

```rust
// TabState 或 view 旁路
restored_remote_path: Option<RemotePath>
// Empty 时 path bar 显示该路径；Connect 成功后 navigate 到此 path
```

MVP 可以简化为：`tab.remote.path = restored`、`entries` 为空，并且 `connection = Empty`。Connect 成功后，使用该 path 发送 `ReadRemoteDir`；如果服务端不存在该路径，则显示 error 和 Retry。

### 2.5 安全

- 日志无需记录完整用户名，并且**禁止**包含 secret。
- session 文件使用用户 home 的文件权限，并且不写入 Keychain 内容。

---

## 3. 最近连接（Recents）

### 3.1 存储

`AppPaths.recents_file` 指向 `.../recents.json`。

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

- 最多保存 **20** 项。相同 `(host, port, username, profile_id)` 的记录只更新时间戳并移动到列表首位，因此列表不会包含重复项。
- 文件中**禁止**包含 secret。
- 使用原子写入；如果文件损坏，则使用空列表。

### 3.2 写入时机

- `TabConnected` 成功后，从 tab、`tab_settings` 或 profile 中提取 host、port、user、profile_id、remote_root 或当前 remote path，然后 upsert recents。
- 连接失败时不写入 recents。

### 3.3 使用方式

用户在空状态中选择一项后：

1. 如果存在 `profile_id` 并且对应 profile 仍然存在，则预填 connect form；也可以通过现有 profile 流程执行 `connect_with`，并从 Keychain 读取 secret。
2. 如果不存在 profile，则显示 connect form，并预填 host、port 和 username；用户需要提供密码。
3. 可以保存 `last_remote_path`，并在连接成功后访问该路径。

---

## 4. 空状态与首次使用指引

### 4.1 Remote pane `Empty` / `Disconnected`

```text
Not connected
[ Connect… (⌘⇧R) ]   [ 可选：Open Connection 同义 ]

Recent connections
  Work · alex@example.com:22
  lab · root@10.0.0.2:22
  …
```

- 界面中**不显示**“欢迎使用 macSFTP”等长文案，也不显示轮播或营销内容。
- 如果 recents 为空，则只显示 Connect。
- Local pane 继续支持浏览现有 home；local 空列表无需显示 recents。

### 4.2 不存在 tab 时

保留现有 `No connections` 和 New Tab。

### 4.3 首次安装

如果 session 和 recents 均不存在，则显示与当前实现相同的状态，也就是一个 tab 和 Connect 空状态。因此用户启动应用后即可进入工作区。

---

## 5. 窗口标题

- 存在 active tab 时，标题为 `{tab.title} — macSFTP` 或 `{host} — macSFTP`。
- 不存在 tab 时，标题为 `macSFTP`。
- 多窗口情况下，每个窗口根据**自身** active tab 更新标题，并且标题无需在窗口之间保持唯一。
- 在 `activate_tab`、连接完成、disconnect 和 close 时调用 GPUI window title API。如果现有 `Window` API 只允许在创建时设置标题，则需要在生命周期事件中增加刷新逻辑。

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

core **不得**依赖磁盘路径的实现细节；session **不得**写入 secret。session 和 recents 文件很小，因此 GPUI 主线程可以同步处理这些文件，但是不得同步处理大型文件并造成阻塞。

**crate 归属：**

- `macsftp_storage`：`SessionFile` / `SessionStore`、`RecentsFile` / `RecentsStore`。
- `macsftp_platform`：`AppPaths` 新字段。
- `macsftp_app`：快照序列化、恢复、空状态 UI 和标题。

---

## 7. 测试计划

### Storage

- 验证 session round-trip，以及损坏 JSON 的 fallback。
- 验证 recents upsert 的去重和 20 项上限。
- 验证序列化结果中不存在 secret 字段。

### App

- 使用 snapshot 构造 `Workspace`，然后验证恢复后的 tab 数量、path 和 profile 均一致，同时 connection 不是 Connected。
- 验证恢复期间不发送 `ConnectTab`；可以检查 command channel 中不存在 Connect。
- 验证 TabConnected 后 recents 增加一项。
- 验证空状态显示 recents 项；该测试可以检查 store 和渲染数据，而无需进行 UI 像素测试。

### 手工测试

- 连接服务器，创建两个 tab，设置 local/remote path，然后退出并重新启动；tabs 应恢复，但是应用不得自动连接。
- 验证 Reconnect 成功。
- 在空 tab 中选择 recent，并验证能够连接。
- 切换 tab 后，验证窗口标题随之更新。

---

## 8. 修改文件清单

| 文件 | 修改内容 |
| --- | --- |
| `crates/platform/src/platform.rs` | `session_file` 和 `recents_file` |
| `crates/storage/src/session.rs`（新） | session load/save |
| `crates/storage/src/recents.rs`（新） | recents load/save/upsert |
| `crates/storage/src/storage.rs` | mod 导出 |
| `crates/app/src/resources.rs` | 注册 stores |
| `crates/app/src/workspace/mod.rs` | quit 时 flush session；new 时 restore |
| `crates/app/src/workspace/event_handling.rs` | TabConnected 后更新 recents |
| `crates/app/src/workspace/render.rs` | 空状态 recents 和 window title 更新点 |
| `crates/app/src/main.rs` | 路径 ensure；如果标题属于 window 层，则包含标题逻辑 |
| 测试 | storage unit 和 app restore |

---

## 9. 建议的 PR 划分

| PR | 内容 | 依赖 |
| --- | --- | --- |
| **PR1** | AppPaths、SessionStore、启动恢复和 quit 保存 | 无 |
| **PR2** | RecentsStore、TabConnected 写入和空状态列表 | 无，因此可并行实施 |
| **PR3** | 窗口标题 | 无 |
| **PR4** | 完善空状态文案，并整合 Connect/recent 的单次选择行为 | PR1+PR2 |

---

## 10. 开放问题与明确取舍

| 项 | 决议 |
| --- | --- |
| 恢复是否包含 filter/sort/nav history | **否**，MVP 不包含 |
| 恢复是否包含 drawer 展开状态 | **否** |
| profile 被删除后的 tab | 保留 host 元数据；Connect 使用表单 |
| 多窗口同时修改 tabs | 最后退出的窗口写入一份；另一窗口的状态可能不会保存，该限制可以接受 |

---

## 决策记录（brainstorming）

| # | 问题 | 选择 |
| --- | --- | --- |
| 1 | Session 恢复策略 | A — 静默自动恢复布局 |
| 2 | Recents 与 profiles | A — 独立 recents + profile 链接 |
| 3 | 恢复后是否自动连接 | A — 不自动 Connect |
| 4 | 多窗口 session | A — 全局一份 |
| 5 | 空状态指引 | A — 操作 + recents |
| 6 | 交付 | 方案 1 — session 和 recents 使用独立文件 |

---

## Key Decisions

1. **布局恢复不包含自动连接。** 该选择优先保证安全性和启动行为的可预测性。
2. **Secret 只通过 Keychain 和 profile_id 使用。** 因此 session 和 recents 只包含元数据。
3. **Recents 独立于 profiles。** 因此未保存为 profile 的连接也可以出现在最近连接中。
4. **全局只使用一个 session 文件。** 该形式可以满足多窗口 MVP 的要求。
5. **空状态只显示后续操作。** 该内容符合规范 §2.1。
