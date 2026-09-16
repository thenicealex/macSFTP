# 阶段 5 实施计划：初始引导与持久化

> **Agent 实施要求：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，并按任务实施本计划。各步骤使用复选框（`- [ ]`）记录状态。

**目标：** 在应用退出和重新启动之间持久化 workspace 会话布局，但是不得自动连接；同时使用独立的 recents 列表记录成功连接，在空 remote pane 中显示 recents 和 Connect，并使窗口标题与当前 tab 保持一致。

**架构：** 在 `macsftp_storage` 中增加两个具有版本号的 JSON store（`session.json`、`recents.json`），其路径由 `AppPaths` 提供，并由进程级 `AppResources` 统一管理。第一个窗口将会话布局恢复至 `TabState`，同时将连接状态设置为 `ConnectionState::Empty`，因此不会发送 `ConnectTab`。成功的 `TabConnected` 事件更新或新增 recents 条目。因此，空 remote pane 将 recents 显示为可选操作，而窗口标题通过 `Window::set_window_title` 更新。

**技术栈：** Rust、GPUI（`on_app_quit`、`Window::set_window_title`）、serde JSON、现有 `ProfileStore` / 原子 tmp+rename 模式和 `AppResources` 全局资源。

**规格文档：** `docs/plans/2026-07-14-phase5-onboarding-persistence-design.md`

## 全局约束

- **恢复布局不表示自动连接。** 恢复后的 tab 保持 `Empty`/`Disconnected`，因此启动时不得发送 `AppCommand::ConnectTab`。
- `session.json` 和 `recents.json` 中**不得包含 secret**，只能包含 `profile_id` 以及 host/port/username/path 元数据。
- **使用单一全局会话文件。** 应用仅使用一个 `session.json`；第一个窗口恢复会话，而后续窗口创建空白新 tab。
- **Recents 最多包含 20 项。** 去重键为 `(host, port, username, profile_id)`，并且仅在连接成功后写入。
- **空状态不得包含营销文案。** 仅显示 "Not connected"、Connect… 和可选的 recents 列表。
- **MVP 仅保存以下会话字段：** title、profile_id、host、port、username、local_path、remote_path、active_tab_index；不得保存 filter/MRU/sort/drawer。
- 文件写入必须采用原子方式，即先写入临时文件，然后执行 rename，并与 `ProfilesFile` / `TransferHistoryFile` 保持一致。
- 如果文件损坏或版本不受支持，则使用空数据并记录 WARN；但是不得阻止应用启动。
- 根据 AGENTS.md §5，可恢复路径不得使用 `unwrap`/`expect`。同时，优先使用 `src/foo.rs`，而不是 `mod.rs`。
- 不得修改 SFTP protocol、Keychain 或 transfer restore。

## 文件清单

| 文件 | 职责 |
| --- | --- |
| **修改** `crates/platform/src/platform.rs` | 在 `AppPaths` 中增加 `session_file` 和 `recents_file`，并纳入 `ensure_directories` 与路径测试 |
| **新建** `crates/storage/src/session.rs` | 实现 `SessionFile`、`SessionTabSnapshot`、`SessionStore` 的加载和保存 |
| **新建** `crates/storage/src/recents.rs` | 实现 `RecentsFile`、`RecentEntry`、`RecentsStore` 的加载、保存和 upsert |
| **修改** `crates/storage/src/storage.rs` | 声明并重新导出 `session` / `recents` 模块 |
| **修改** `crates/app/src/resources.rs` | 在 `AppResources` 中增加 `session: SessionStore` 和 `recents: RecentsStore` |
| **修改** `crates/app/src/main.rs` | 仅向第一个窗口传递 `restore_session` |
| **修改** `crates/app/src/workspace/mod.rs` | 增加 `restore_session` 标志、tab 恢复、退出保存、标题辅助函数和恢复元数据 |
| **修改** `crates/app/src/workspace/event_handling.rs` | 收到 `TabConnected` 后 upsert recents，并优先使用恢复的远程路径 |
| **修改** `crates/app/src/workspace/connect_form.rs` | 根据恢复元数据预填表单，或者根据 recent 条目显示表单 |
| **修改** `crates/app/src/workspace/render.rs` | 实现 Empty/Disconnected 空状态和 recents 行 |
| **修改** `crates/app/src/workspace/tests.rs` | 验证恢复、recents、标题和不自动连接 |
| **不得修改** | `crates/sftp/**` 和 core transfer/session protocol；如果测试 fixture 需要路径，也应尽量避免修改 sftp 文件 |

## 建议的 PR 划分（仅在堆叠 PR 时采用）

| PR | 任务 | 说明 |
| --- | --- | --- |
| PR1 | 任务 1–3 | 路径、SessionStore、恢复和退出保存 |
| PR2 | 任务 4–5 | RecentsStore、TabConnected 和空状态 |
| PR3 | 任务 6 | 窗口标题 |
| PR4 | 任务 7 | Recent 选择后的预填和连接行为 |

以下任务按照单一顺序实施流程排列。但是，如果能够明确隔离并行修改，那么 PR2 可以在任务 1 完成后开始。

---

### 任务 1：AppPaths 中的 `session_file` / `recents_file`

**文件：**
- 修改：`crates/platform/src/platform.rs`
- 测试：同一文件内的 `#[cfg(test)]` 模块（`builds_expected_macos_app_paths`）

**接口：**
- 提供：
  - `AppPaths.session_file: LocalPath` → `{app_support}/session.json`
  - `AppPaths.recents_file: LocalPath` → `{app_support}/recents.json`
  - 两个路径均包含在 `ensure_directories` 的父目录创建列表中

- [ ] **步骤 1：扩展现有路径单元测试（字段缺失时应先失败）**

在 `builds_expected_macos_app_paths` 中增加：

```rust
assert_eq!(
    paths.session_file.as_str(),
    "/Users/alex/Library/Application Support/macSFTP/session.json"
);
assert_eq!(
    paths.recents_file.as_str(),
    "/Users/alex/Library/Application Support/macSFTP/recents.json"
);
```

- [ ] **步骤 2：运行测试，并确认因字段未知而失败**

```bash
cargo test -p macsftp-platform builds_expected_macos_app_paths -- --nocapture
```

- [ ] **步骤 3：实现字段**

```rust
// AppPaths struct — add:
pub session_file: LocalPath,
pub recents_file: LocalPath,

// from_home_dir:
session_file: LocalPath::new(format!("{app_support_dir}/session.json")),
recents_file: LocalPath::new(format!("{app_support_dir}/recents.json")),

// ensure_directories — add to the `for file in [...]` list:
&self.session_file,
&self.recents_file,
```

- [ ] **步骤 4：再次运行测试，并确认测试通过**

```bash
cargo test -p macsftp-platform builds_expected_macos_app_paths -- --nocapture
```

- [ ] **步骤 5：提交**

```bash
git add crates/platform/src/platform.rs
git commit -m "feat(platform): add session.json and recents.json AppPaths"
```

---

### 任务 2：SessionStore（storage）

**文件：**
- 新建：`crates/storage/src/session.rs`
- 修改：`crates/storage/src/storage.rs`，声明模块并重新导出
- 测试：在 `session.rs` 中编写单元测试

**接口：**
- 提供：
  - `SessionTabSnapshot { title, profile_id: Option<u64>, host, port, username, local_path: Option<String>, remote_path: Option<String> }`
  - `SessionFile { version: u32, active_tab_index: usize, tabs: Vec<SessionTabSnapshot> }` with `CURRENT_VERSION = 1`
  - `SessionStore { path, file }` with:
    - `open(path) -> Result<Self, StorageError>`
    - `open_or_empty(path) -> Self`；文件缺失、损坏或版本不受支持时返回空数据
    - `file(&self) -> &SessionFile`
    - `replace(&mut self, file: SessionFile)`
    - `save(&self) -> Result<(), StorageError>`，使用原子 tmp+rename
  - Re-export: `pub use session::{SessionFile, SessionStore, SessionTabSnapshot};`

**规则（必须准确实现）：**
- 文件缺失 → 返回空 session（`tabs: []`、`active_tab_index: 0`）
- 解析错误或 `version > CURRENT_VERSION` → `open_or_empty` 返回空数据，但是不删除原文件
- JSON 中的 `SessionTabSnapshot` **不得**包含 password、passphrase 或 key material 字段

- [ ] **步骤 1：在 `session.rs` 中编写预期失败的单元测试**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use macsftp_core::LocalPath;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn temp_path(label: &str) -> LocalPath {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let seq = SEQ.fetch_add(1, Ordering::SeqCst);
        LocalPath::new(format!(
            "{}/macsftp-session-{}-{}-{}.json",
            std::env::temp_dir().display(),
            label,
            std::process::id(),
            seq
        ))
    }

    #[test]
    fn session_round_trip_preserves_tabs_and_active_index() {
        let path = temp_path("roundtrip");
        let mut store = SessionStore::open_or_empty(path.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
            active_tab_index: 1,
            tabs: vec![
                SessionTabSnapshot {
                    title: "a.example".into(),
                    profile_id: Some(3),
                    host: "a.example".into(),
                    port: 22,
                    username: "alex".into(),
                    local_path: Some("/Users/alex".into()),
                    remote_path: Some("/home/alex".into()),
                },
                SessionTabSnapshot {
                    title: "b.example".into(),
                    profile_id: None,
                    host: "b.example".into(),
                    port: 2222,
                    username: "root".into(),
                    local_path: None,
                    remote_path: None,
                },
            ],
        });
        store.save().expect("save session");
        let reloaded = SessionStore::open(path).expect("reopen");
        assert_eq!(reloaded.file().active_tab_index, 1);
        assert_eq!(reloaded.file().tabs.len(), 2);
        assert_eq!(reloaded.file().tabs[0].profile_id, Some(3));
        assert_eq!(reloaded.file().tabs[1].port, 2222);
    }

    #[test]
    fn corrupt_json_open_or_empty_yields_empty() {
        let path = temp_path("corrupt");
        std::fs::write(path.as_str(), "{not json").expect("write corrupt");
        let store = SessionStore::open_or_empty(path);
        assert!(store.file().tabs.is_empty());
    }

    #[test]
    fn unsupported_version_open_or_empty_yields_empty() {
        let path = temp_path("badver");
        std::fs::write(
            path.as_str(),
            r#"{"version":99,"active_tab_index":0,"tabs":[{"title":"x","host":"x","port":22,"username":"u"}]}"#,
        )
        .expect("write");
        let store = SessionStore::open_or_empty(path);
        assert!(store.file().tabs.is_empty());
    }

    #[test]
    fn serialized_session_has_no_secret_keys() {
        let path = temp_path("nosecret");
        let mut store = SessionStore::open_or_empty(path.clone());
        store.replace(SessionFile {
            version: SessionFile::CURRENT_VERSION,
            active_tab_index: 0,
            tabs: vec![SessionTabSnapshot {
                title: "h".into(),
                profile_id: None,
                host: "h".into(),
                port: 22,
                username: "u".into(),
                local_path: None,
                remote_path: None,
            }],
        });
        store.save().expect("save");
        let raw = std::fs::read_to_string(path.as_str()).expect("read");
        for forbidden in ["password", "passphrase", "secret", "auth", "key_path"] {
            assert!(
                !raw.to_lowercase().contains(forbidden),
                "session json must not contain {forbidden}: {raw}"
            );
        }
    }
}
```

- [ ] **步骤 2：运行测试，并确认因模块缺失而失败**

```bash
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **步骤 3：实现 `session.rs`**

实现方式应与 `transfer_history.rs` / `ProfilesFile` 模式一致：

```rust
use macsftp_core::LocalPath;
use serde::{Deserialize, Serialize};

use super::StorageError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionTabSnapshot {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<u64>,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFile {
    pub version: u32,
    #[serde(default)]
    pub active_tab_index: usize,
    #[serde(default)]
    pub tabs: Vec<SessionTabSnapshot>,
}

impl SessionFile {
    pub const CURRENT_VERSION: u32 = 1;

    pub fn empty() -> Self {
        Self {
            version: Self::CURRENT_VERSION,
            active_tab_index: 0,
            tabs: Vec::new(),
        }
    }

    pub fn load(path: &LocalPath) -> Result<Self, StorageError> {
        match std::fs::read_to_string(path.as_str()) {
            Ok(contents) => {
                let parsed: SessionFile =
                    serde_json::from_str(&contents).map_err(|error| StorageError::Parse {
                        message: error.to_string(),
                    })?;
                if parsed.version > Self::CURRENT_VERSION {
                    return Err(StorageError::Parse {
                        message: format!(
                            "unsupported session version {} (max {})",
                            parsed.version,
                            Self::CURRENT_VERSION
                        ),
                    });
                }
                Ok(parsed)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::empty()),
            Err(error) => Err(StorageError::Io {
                path: path.as_str().to_string(),
                message: error.to_string(),
            }),
        }
    }

    pub fn save(&self, path: &LocalPath) -> Result<(), StorageError> {
        let json = serde_json::to_string_pretty(self).map_err(|error| StorageError::Parse {
            message: error.to_string(),
        })?;
        let path_str = path.as_str();
        let temp_path = format!("{path_str}.tmp");
        std::fs::write(&temp_path, &json).map_err(|error| StorageError::Io {
            path: temp_path.clone(),
            message: error.to_string(),
        })?;
        std::fs::rename(&temp_path, path_str).map_err(|error| StorageError::Io {
            path: path_str.to_string(),
            message: error.to_string(),
        })?;
        Ok(())
    }
}

pub struct SessionStore {
    path: LocalPath,
    file: SessionFile,
}

impl SessionStore {
    pub fn open(path: LocalPath) -> Result<Self, StorageError> {
        let file = SessionFile::load(&path)?;
        Ok(Self { path, file })
    }

    pub fn open_or_empty(path: LocalPath) -> Self {
        match Self::open(path.clone()) {
            Ok(store) => store,
            Err(_) => Self {
                path,
                file: SessionFile::empty(),
            },
        }
    }

    pub fn path(&self) -> &LocalPath {
        &self.path
    }

    pub fn file(&self) -> &SessionFile {
        &self.file
    }

    pub fn replace(&mut self, file: SessionFile) {
        self.file = file;
    }

    pub fn save(&self) -> Result<(), StorageError> {
        self.file.save(&self.path)
    }
}
```

在 `storage.rs` 中声明并导出：

```rust
pub mod session;
pub use session::{SessionFile, SessionStore, SessionTabSnapshot};
```

- [ ] **步骤 4：运行测试，并确认测试通过**

```bash
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **步骤 5：提交**

```bash
git add crates/storage/src/session.rs crates/storage/src/storage.rs
git commit -m "feat(storage): add SessionStore for session.json"
```

---

### 任务 3：在第一个窗口恢复会话，并在退出时保存

**文件：**
- 修改：`crates/app/src/resources.rs`
- 修改：`crates/app/src/main.rs`（`open_workspace_window`、`Workspace::new` 调用）
- 修改：`crates/app/src/workspace/mod.rs`（`Workspace::new` 签名、恢复、保存、`build_session_snapshot`）
- 修改：`crates/app/src/workspace/event_handling.rs`（处理 `TabConnected` 时优先使用恢复的远程路径）
- 修改：`crates/app/src/workspace/connect_form.rs`（显示表单时根据恢复元数据预填）
- 测试：`crates/app/src/workspace/tests.rs`

**接口：**
- 提供：
  - `AppResources.session: SessionStore`
  - `Workspace::new(..., restore_session: bool, ...)`
  - `Workspace.session_flushed: bool`，其防重复机制与 transfer history 一致
  - `Workspace.restored_targets: HashMap<TabId, RestoredTabTarget>`，其中：

```rust
#[derive(Debug, Clone)]
pub(crate) struct RestoredTabTarget {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_id: Option<ProfileId>,
    pub remote_path: Option<RemotePath>,
}
```

  - `fn build_session_snapshot(&self) -> SessionFile`
  - `fn flush_session(&mut self, cx: &mut Context<Self>)`
  - `fn restore_session_tabs(&mut self, window, cx)`，仅在 `restore_session && !file.tabs.is_empty()` 时执行
  - 第一个窗口使用 `restore_session = true`；Cmd+N 创建的窗口使用 `false`，因此仅执行 `open_new_tab`

**恢复规则：**
1. 对每个 snapshot 依次调用 `next_tab_id()` 和 `TabState::new(id, title)`，然后设置 `profile_id`；设置 `local.path` 并调用 `load_local_directory`；根据 snapshot 设置 `remote.path`，同时保持 entries 为空；最后设置 `connection = Empty`。
2. 保存 `RestoredTabTarget`，以便预填表单以及在连接后访问目标路径。
3. 将 `active_tab_index` 限制在 `tabs.len()-1` 以内，并设置 `active_tab_id`；然后按照 tab 顺序初始化 `tab_mru`，并将当前 tab 更新为最近使用项。
4. **不得**调用 `send_command(ConnectTab)`。
5. 如果 session 为空，则使用现有 `open_new_tab` 路径。

**TabConnected 路径优先级（恢复远程路径所必需）：**

```rust
// In AppEvent::TabConnected — after finding tab:
let preferred_remote = self
    .restored_targets
    .get(&tab_id)
    .and_then(|t| t.remote_path.clone())
    .or_else(|| {
        // If path was set on the tab before connect (restore MVP), keep it
        // when non-empty; otherwise use remote_root from the event.
        None
    });
let navigate_to = preferred_remote.unwrap_or(remote_root.clone());
// clear restored remote preference after use so reconnect uses live path
if let Some(target) = self.restored_targets.get_mut(&tab_id) {
    target.remote_path = None;
}
// then set tab.remote.path = Some(navigate_to.clone()) and request_remote_directory
```

实现时也可以使用以下等价的简化形式：

```rust
let navigate_to = {
    let restored = self
        .restored_targets
        .get(&tab_id)
        .and_then(|t| t.remote_path.clone());
    restored.unwrap_or(remote_root)
};
if let Some(target) = self.restored_targets.get_mut(&tab_id) {
    target.remote_path = None;
}
```

**退出时保存：**

```rust
// In Workspace::new after construction, alongside transfer history:
cx.on_app_quit(|workspace, cx| {
    workspace.flush_transfer_history(cx);
    workspace.flush_session(cx);
    async {}
})
.detach();
```

```rust
pub(crate) fn build_session_snapshot(&self) -> SessionFile {
    let tabs: Vec<SessionTabSnapshot> = self
        .state
        .tabs
        .tabs
        .iter()
        .map(|tab| {
            let settings = self.tab_settings.get(&tab.id);
            let restored = self.restored_targets.get(&tab.id);
            SessionTabSnapshot {
                title: tab.title.clone(),
                profile_id: tab.profile_id.map(|id| id.0),
                host: settings
                    .map(|s| s.host.clone())
                    .or_else(|| restored.map(|r| r.host.clone()))
                    .unwrap_or_else(|| tab.title.clone()),
                port: settings
                    .map(|s| s.port)
                    .or_else(|| restored.map(|r| r.port))
                    .unwrap_or(22),
                username: settings
                    .map(|s| s.username.clone())
                    .or_else(|| restored.map(|r| r.username.clone()))
                    .unwrap_or_default(),
                local_path: tab.local.path.as_ref().map(|p| p.as_str().to_string()),
                remote_path: tab
                    .remote
                    .path
                    .as_ref()
                    .map(|p| p.as_str().to_string())
                    .or_else(|| {
                        restored
                            .and_then(|r| r.remote_path.as_ref().map(|p| p.as_str().to_string()))
                    }),
            }
        })
        .collect();
    let active_tab_index = self
        .state
        .tabs
        .active_tab_id
        .and_then(|id| self.state.tabs.tabs.iter().position(|t| t.id == id))
        .unwrap_or(0);
    SessionFile {
        version: SessionFile::CURRENT_VERSION,
        active_tab_index,
        tabs,
    }
}

pub(crate) fn flush_session(&mut self, cx: &mut Context<Self>) {
    if self.session_flushed {
        return;
    }
    self.session_flushed = true;
    // Multi-window MVP: each workspace overwrites session.json on quit.
    // Last writer wins; if >1 window, log once (design accepts loss of other windows).
    if cx.windows().len() > 1 {
        tracing::warn!(
            windows = cx.windows().len(),
            "multiple windows open; session.json will reflect this workspace only"
        );
    }
    let snapshot = self.build_session_snapshot();
    let session = &mut cx.resources_mut().session;
    session.replace(snapshot);
    if let Err(error) = session.save() {
        tracing::warn!(error = %error, "could not save session.json");
    }
}
```

**`main.rs` 中的第一个窗口标志：**

```rust
fn open_workspace_window(cx: &mut App) -> gpui::Result<()> {
    let restore_session = cx.windows().is_empty();
    // ... existing client/receiver ...
    cx.open_window(
        WindowOptions { /* unchanged */ },
        |window, cx| {
            cx.new(|cx| {
                Workspace::new(runtime_client, event_receiver, restore_session, window, cx)
            })
        },
    )?;
    // ...
}
```

**`Workspace::new` 修改：**

```rust
pub fn new(
    runtime_client: RuntimeClient,
    mut event_receiver: EventReceiver,
    restore_session: bool,
    window: &mut Window,
    cx: &mut Context<Self>,
) -> Self {
    // ... build workspace fields; add:
    // restored_targets: HashMap::new(),
    // session_flushed: false,
    //
    // quit hook: flush_transfer_history + flush_session
    //
    if restore_session {
        workspace.restore_session_tabs(window, cx);
    }
    if workspace.state.tabs.tabs.is_empty() {
        workspace.open_new_tab(window, cx);
    }
    workspace
}
```

```rust
fn restore_session_tabs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let file = cx.resources().session.file().clone();
    if file.tabs.is_empty() {
        return;
    }
    let mut restored_ids = Vec::new();
    for snap in &file.tabs {
        let tab_id = cx.resources().next_tab_id();
        let title = if snap.title.is_empty() {
            snap.host.clone()
        } else {
            snap.title.clone()
        };
        let mut tab = TabState::new(tab_id, title);
        tab.profile_id = snap.profile_id.map(ProfileId);
        tab.connection = ConnectionState::Empty;
        if let Some(local) = &snap.local_path {
            let path = LocalPath::new(local.clone());
            if let Some(message) = Self::load_local_directory(&path, &mut tab) {
                self.status_message = Some(message.into());
            }
        } else if let Some(message) =
            Self::load_local_directory(&self.default_local_path, &mut tab)
        {
            self.status_message = Some(message.into());
        }
        if let Some(remote) = &snap.remote_path {
            tab.remote.path = Some(RemotePath::new(remote.clone()));
            tab.remote.entries.clear();
        }
        self.restored_targets.insert(
            tab_id,
            RestoredTabTarget {
                host: snap.host.clone(),
                port: snap.port,
                username: snap.username.clone(),
                profile_id: snap.profile_id.map(ProfileId),
                remote_path: snap.remote_path.as_ref().map(|p| RemotePath::new(p.clone())),
            },
        );
        self.state.tabs.open_tab(tab);
        restored_ids.push(tab_id);
        self.touch_mru(tab_id);
    }
    let active_index = file.active_tab_index.min(restored_ids.len().saturating_sub(1));
    if let Some(active_id) = restored_ids.get(active_index).copied() {
        self.state.tabs.active_tab_id = Some(active_id);
        self.touch_mru(active_id);
    }
    self.clear_filters();
    self.reset_scroll_positions();
    self.focus_pane(PaneSide::Local, window, cx);
    cx.notify();
}
```

**根据恢复元数据预填连接表单：**

在 `open_connect_form` 中，如果没有 `tab_settings`，则创建不含 secret 的临时预填内容：

```rust
pub(crate) fn open_connect_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let tab_id = self.state.tabs.active_tab_id;
    let form = tab_id
        .and_then(|id| self.tab_settings.get(&id))
        .map(ConnectForm::prefilled)
        .or_else(|| {
            let id = tab_id?;
            let restored = self.restored_targets.get(&id)?;
            // Prefer live profile if still present
            if let Some(profile_id) = restored.profile_id {
                if let Some(profile) = cx.resources().profiles.find_profile(profile_id) {
                    return Some(ConnectForm::from_profile(profile));
                }
            }
            let mut form = ConnectForm::empty();
            form.host = InputState::with_value(restored.host.clone());
            form.port = InputState::with_value(restored.port.to_string());
            form.username = InputState::with_value(restored.username.clone());
            form.source_profile_id = restored.profile_id;
            Some(form)
        })
        .unwrap_or_else(ConnectForm::empty);
    self.connect_form = Some(form);
    window.focus(&self.connect_form_focus);
    cx.notify();
}
```

**`resources.rs`：**

```rust
use macsftp_storage::{
    ConfigStore, KeychainStore, ProfileStore, RecentsStore, ResidualTempStore, SessionStore,
    TransferHistoryStore,
};

pub struct AppResources {
    // ...existing...
    pub session: SessionStore,
    pub recents: RecentsStore, // add empty stub in Task 4 if not yet; see note
}

// In load:
let session = SessionStore::open_or_empty(app_paths.session_file.clone());
```

> **说明：** 如果按照 PR 划分实施，则仅在任务 4 的实现存在后增加临时的 `// recents in Task 4`。任务 4 完成时，优先在 resources 中同时增加两个 store；但是仅实施任务 3 时，只增加 `session`。

更新测试和 main 中所有 `Workspace::new(...)` 调用位置。测试默认使用 `restore_session: false`，但是明确验证恢复行为的测试可以使用 true。

- [ ] **步骤 1：编写预期失败的 app 测试**

```rust
#[gpui::test]
fn restore_session_rebuilds_tabs_without_connect(cx: &mut TestAppContext) {
    // 1) Write a session.json via SessionStore into temp_app_paths before init
    // 2) Init AppResources from those paths, Workspace::new(..., restore_session: true, ...)
    // 3) assert tabs.len() == 2, titles match, connection is Empty
    // 4) assert command channel has no ConnectTab (try_recv empty or only non-connect)
}

#[gpui::test]
fn build_session_snapshot_round_trips_active_tab_and_paths(cx: &mut TestAppContext) {
    // open two tabs, set titles/paths/profile_id via tab mut
    // snapshot = build_session_snapshot()
    // assert active index + local/remote path strings
}

#[gpui::test]
fn flush_session_writes_session_json(cx: &mut TestAppContext) {
    // mutate tabs, flush_session, reopen SessionStore from same path, tabs non-empty
}
```

恢复测试可以采用以下辅助函数结构（示意）：

```rust
fn init_workspace_with_paths(
    cx: &mut TestAppContext,
    app_paths: AppPaths,
    restore_session: bool,
) -> (Entity<Workspace>, VisualTestContext, BridgeChannels) {
    let config = macsftp_storage::ConfigStore::with_defaults(app_paths.config_file.clone());
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        app_actions::init(cx);
        cx.set_global(crate::resources::AppResources::load(
            app_paths,
            config,
            macsftp_storage::KeychainStore::new_memory(),
        ));
        cx.set_global(crate::resources::SharedTransfers::default());
    });
    let channels = BridgeChannels::new(&RuntimeBridgeConfig::default());
    let client = RuntimeClient::new(channels.command_tx.clone());
    let (_event_tx, receiver) = macsftp_sftp::test_event_channel(
        RuntimeBridgeConfig::default().event_channel_capacity,
    );
    let window = cx.add_window(|window, cx| {
        Workspace::new(client, receiver, restore_session, window, cx)
    });
    // ...
}
```

- [ ] **步骤 2：运行测试，并确认测试失败**

```bash
cargo test -p macsftp-app --bin macsftp restore_session build_session_snapshot flush_session -- --nocapture
```

- [ ] **步骤 3：实现恢复、保存、resources.session、main 标志和 TabConnected 路径优先级**

- [ ] **步骤 4：运行测试，并确认测试通过**

```bash
cargo test -p macsftp-app --bin macsftp restore_session build_session_snapshot flush_session -- --nocapture
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **步骤 5：提交**

```bash
git add crates/app/src/resources.rs crates/app/src/main.rs \
  crates/app/src/workspace/mod.rs crates/app/src/workspace/event_handling.rs \
  crates/app/src/workspace/connect_form.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): restore session layout on launch and save on quit"
```

---

### 任务 4：RecentsStore（storage）

**文件：**
- 新建：`crates/storage/src/recents.rs`
- 修改：`crates/storage/src/storage.rs`
- 修改：`crates/app/src/resources.rs`，在资源中增加 `recents`
- 测试：在 `recents.rs` 中编写单元测试

**接口：**
- 提供：

```rust
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub id: u64,
    pub host: String,
    pub port: u16,
    pub username: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_remote_path: Option<String>,
    /// Unix seconds
    pub last_connected_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentsFile {
    pub version: u32,
    pub entries: Vec<RecentEntry>,
}

pub struct RecentsStore {
    path: LocalPath,
    file: RecentsFile,
    next_id: u64,
}

impl RecentsStore {
    pub const MAX_ENTRIES: usize = 20;
    pub fn open_or_empty(path: LocalPath) -> Self;
    pub fn entries(&self) -> &[RecentEntry];
    /// Upsert by (host, port, username, profile_id); move to front; cap 20; save.
    pub fn upsert(&mut self, entry: RecentEntryInput) -> Result<(), StorageError>;
    pub fn save(&self) -> Result<(), StorageError>;
}

/// Input without id/timestamp (store assigns).
pub struct RecentEntryInput {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub profile_id: Option<u64>,
    pub display_name: Option<String>,
    pub last_remote_path: Option<String>,
    pub last_connected_at: u64,
}
```

如果 `(host, port, username, profile_id)` 全部相同，则判定为重复项。因此，需要更新字段和 `last_connected_at`，并将条目置于索引 0。否则，使用新的 `id` 在列表首部增加条目，并将列表限制为 20 项。

- [ ] **步骤 1：编写预期失败的测试**

```rust
#[test]
fn upsert_dedupes_and_moves_to_front() { /* same key twice → len 1, newer timestamp */ }

#[test]
fn upsert_caps_at_twenty() {
    let mut store = RecentsStore::open_or_empty(temp_path("cap"));
    for i in 0..25 {
        store
            .upsert(RecentEntryInput {
                host: format!("h{i}"),
                port: 22,
                username: "u".into(),
                profile_id: None,
                display_name: None,
                last_remote_path: None,
                last_connected_at: i,
            })
            .expect("upsert");
    }
    assert_eq!(store.entries().len(), 20);
    assert_eq!(store.entries()[0].host, "h24");
}

#[test]
fn corrupt_recents_open_or_empty() { /* same as session */ }

#[test]
fn serialized_recents_has_no_secret_keys() { /* same forbidden list as session */ }
```

- [ ] **步骤 2：运行测试，并确认测试失败**

```bash
cargo test -p macsftp-storage upsert_ caps_at corrupt_recents serialized_recents -- --nocapture
```

- [ ] **步骤 3：实现并导出类型，同时增加 `AppResources.recents`**

```rust
// resources load:
let recents = RecentsStore::open_or_empty(app_paths.recents_file.clone());
```

- [ ] **步骤 4：确认测试通过并提交**

```bash
cargo test -p macsftp-storage -- --nocapture
git add crates/storage/src/recents.rs crates/storage/src/storage.rs crates/app/src/resources.rs
git commit -m "feat(storage): add RecentsStore for recents.json"
```

---

### 任务 5：根据 TabConnected 更新 recents，并实现空状态列表 UI

**文件：**
- 修改：`crates/app/src/workspace/event_handling.rs`
- 修改：`crates/app/src/workspace/render.rs`
- 修改：`crates/app/src/workspace/mod.rs`；如果结构更清晰，则增加辅助方法 `record_recent_connection`
- 测试：`crates/app/src/workspace/tests.rs`

**接口：**
- 提供：
  - `Workspace::record_recent_for_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>)`
  - Empty-state for `ConnectionState::Empty` and `Disconnected` includes:
    - 现有的主要 Connect / Reconnect 按钮
    - 如果 `cx.resources().recents.entries()` 非空，则在 `"Recent connections"` 标签下显示垂直列表
    - 每行显示 `display_name · user@host:port`；如果没有 display_name，则显示 `user@host:port`
    - 用户选择条目后调用 `open_recent_connection(entry_id, window, cx)`；任务 7 完整实现该方法，而任务 5 可以仅显示预填表单

**TabConnected 事件处理（在 complete_connect / directory request 后执行）：**

```rust
self.record_recent_for_tab(tab_id, cx);
```

```rust
pub(crate) fn record_recent_for_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>) {
    let Some(tab) = self.state.tabs.find_tab(tab_id) else {
        return;
    };
    let settings = self.tab_settings.get(&tab_id);
    let restored = self.restored_targets.get(&tab_id);
    let host = settings
        .map(|s| s.host.clone())
        .or_else(|| restored.map(|r| r.host.clone()))
        .unwrap_or_else(|| tab.title.clone());
    let port = settings
        .map(|s| s.port)
        .or_else(|| restored.map(|r| r.port))
        .unwrap_or(22);
    let username = settings
        .map(|s| s.username.clone())
        .or_else(|| restored.map(|r| r.username.clone()))
        .unwrap_or_default();
    if host.is_empty() || username.is_empty() {
        return;
    }
    let profile_id = tab.profile_id.map(|id| id.0);
    let display_name = profile_id.and_then(|id| {
        cx.resources()
            .profiles
            .find_profile(ProfileId(id))
            .map(|p| p.name.clone())
    });
    let last_remote_path = tab
        .remote
        .path
        .as_ref()
        .map(|p| p.as_str().to_string());
    let last_connected_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    if let Err(error) = cx.resources_mut().recents.upsert(macsftp_storage::RecentEntryInput {
        host,
        port,
        username,
        profile_id,
        display_name,
        last_remote_path,
        last_connected_at,
    }) {
        tracing::warn!(error = %error, "could not save recents.json");
    }
}
```

**空状态 UI 示意（`render.rs` 中的 Empty 状态）：**

```rust
Some(ConnectionState::Empty) => {
    let recent_buttons = /* map recents.entries() to text_button or clickable row */;
    let mut actions = vec![connect_button("connect-remote", "Connect… (⌘⇧R)")];
    // Prefer listing recents as separate elements below empty_state title,
    // or extend empty_state children. If empty_state only takes action buttons,
    // build a custom v_flex:
    Some(
        v_flex()
            .gap_3()
            .child(empty_state("Not connected", actions, cx))
            .when(!recents.is_empty(), |this| {
                this.child(label("Recent connections"))
                    .children(recents.iter().map(|entry| {
                        let id = entry.id;
                        let label = format_recent_label(entry);
                        text_button(SharedString::from(format!("recent-{id}")), label)
                            .on_click(cx.listener(move |workspace, _, window, cx| {
                                workspace.open_recent_connection(id, window, cx);
                            }))
                    }))
            })
            .into_any_element(),
    )
}
```

对于 `Disconnected`，保留 Reconnect 和 Edit Connection，并且在其下方显示相同的 recents 列表。

**不得增加营销文案，**因此不要增加 "Welcome to macSFTP"。

- [ ] **步骤 1：编写预期失败的测试**

```rust
#[gpui::test]
fn tab_connected_upserts_recents(cx: &mut TestAppContext) {
    // connect_with + inject TabConnected
    // assert recents.entries().len() == 1
    // host/username match test_settings
}

#[gpui::test]
fn tab_connected_dedupes_recents(cx: &mut TestAppContext) {
    // two successful connects same host → still 1 entry
}

#[test]
fn format_recent_label_uses_display_name_when_present() {
    // pure helper unit test if extracted
}
```

- [ ] **步骤 2：确认失败，然后实现功能并确认测试通过**

```bash
cargo test -p macsftp-app --bin macsftp tab_connected_upserts tab_connected_dedupes -- --nocapture
```

- [ ] **步骤 3：提交**

```bash
git add crates/app/src/workspace/event_handling.rs crates/app/src/workspace/render.rs \
  crates/app/src/workspace/mod.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): record recents on connect and show them in empty remote pane"
```

---

### 任务 6：窗口标题与当前 tab 保持一致

**文件：**
- 修改：`crates/app/src/workspace/mod.rs`（`update_window_title` 及其调用位置）
- 可选修改：`event_handling.rs`（连接完成、断开连接或标题变化）
- 测试：在 `crates/app/src/workspace/tests.rs` 中使用 `VisualTestContext` / window title API

**接口：**
- 提供：

```rust
pub(crate) fn update_window_title(&self, window: &mut Window) {
    let title = match self.active_tab() {
        Some(tab) if !tab.title.is_empty() => format!("{} — macSFTP", tab.title),
        _ => "macSFTP".to_string(),
    };
    window.set_window_title(&title);
}
```

在 `open_new_tab`、`close_tab_by_id`、`activate_tab`、`connect_with`（标题设置为 host）之后调用该方法。如果 `TabConnected` / fail / disconnect 处理会修改标题，那么也应调用该方法；此外，还需在 `Workspace::new` / restore 结束时调用一次。

GPUI 0.2.2 提供 `window.set_window_title(&str)` API。如果 `TestAppContext` 提供窗口标题读取方法，那么测试可以使用 test context 的 `window_title()`。

- [ ] **步骤 1：编写预期失败的测试**

```rust
#[gpui::test]
fn window_title_follows_active_tab(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    // After new: "New Connection — macSFTP" or "macSFTP" depending on default tab title
    workspace.update_in(&mut cx, |workspace, window, _cx| {
        if let Some(tab) = workspace.active_tab_mut() {
            tab.title = "example.com".into();
        }
        workspace.update_window_title(window);
    });
    // assert window title contains "example.com" and "macSFTP"
    // Use cx.window_title() if exposed on VisualTestContext; otherwise read via
    // window update probe. If test harness cannot read title, assert the pure
    // format helper instead and call update_window_title in an integration smoke.
}
```

如果 GPUI 测试难以读取标题，那么提取以下函数：

```rust
pub(crate) fn window_title_for_active_tab(tab_title: Option<&str>) -> String {
    match tab_title {
        Some(t) if !t.is_empty() => format!("{t} — macSFTP"),
        _ => "macSFTP".to_string(),
    }
}
```

为该辅助函数编写单元测试，但是生产代码的调用位置仍需调用 `set_window_title`。

- [ ] **步骤 2–4：实现功能、确认测试通过并提交**

```bash
cargo test -p macsftp-app --bin macsftp window_title -- --nocapture
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/event_handling.rs \
  crates/app/src/workspace/tests.rs
git commit -m "feat(app): set window title from active tab"
```

---

### 任务 7：根据 recent 条目预填表单或使用 profile 连接

**文件：**
- 修改：`crates/app/src/workspace/mod.rs` 或 `connect_form.rs`
- 修改：`crates/app/src/workspace/render.rs`，任务 5 已经配置相应事件
- 测试：`crates/app/src/workspace/tests.rs`

**接口：**
- 提供：

```rust
pub(crate) fn open_recent_connection(
    &mut self,
    recent_id: u64,
    window: &mut Window,
    cx: &mut Context<Self>,
) {
    let Some(entry) = cx
        .resources()
        .recents
        .entries()
        .iter()
        .find(|e| e.id == recent_id)
        .cloned()
    else {
        return;
    };

    // Remember last_remote_path for post-connect navigation
    if let Some(tab_id) = self.state.tabs.active_tab_id {
        let remote_path = entry
            .last_remote_path
            .as_ref()
            .map(|p| RemotePath::new(p.clone()));
        self.restored_targets.insert(
            tab_id,
            RestoredTabTarget {
                host: entry.host.clone(),
                port: entry.port,
                username: entry.username.clone(),
                profile_id: entry.profile_id.map(ProfileId),
                remote_path,
            },
        );
        if let Some(tab) = self.state.tabs.find_tab_mut(tab_id) {
            tab.profile_id = entry.profile_id.map(ProfileId);
            if let Some(path) = remote_path {
                tab.remote.path = Some(path);
            }
        }
    }

    // Profile still exists → use_profile (Keychain) then leave form open OR auto-connect:
    // Design: "预填 connect form 或直接 connect_with". Prefer:
    // - if profile_id present AND profile exists AND secret loads: connect_with
    // - else: open form prefilled (from profile or host meta)
    if let Some(profile_id) = entry.profile_id.map(ProfileId) {
        if cx.resources().profiles.find_profile(profile_id).is_some() {
            self.use_profile(profile_id, cx);
            // Attempt build_settings from form after use_profile; if secrets present, submit
            if let Some(form) = &self.connect_form {
                if form.build_settings().is_ok() {
                    self.submit_connect_form(window, cx);
                    return;
                }
            }
            window.focus(&self.connect_form_focus);
            cx.notify();
            return;
        }
    }

    let mut form = ConnectForm::empty();
    form.host = InputState::with_value(entry.host);
    form.port = InputState::with_value(entry.port.to_string());
    form.username = InputState::with_value(entry.username);
    form.source_profile_id = entry.profile_id.map(ProfileId);
    self.connect_form = Some(form);
    window.focus(&self.connect_form_focus);
    cx.notify();
}
```

- [ ] **步骤 1：编写预期失败的测试**

```rust
#[gpui::test]
fn open_recent_without_profile_prefills_form(cx: &mut TestAppContext) {
    // seed recents with host/user/port, no profile
    // open_recent_connection
    // assert connect_form host/user/port match; no ConnectTab yet
}

#[gpui::test]
fn open_recent_with_profile_and_keychain_connects(cx: &mut TestAppContext) {
    // save profile + keychain memory secret
    // seed recent with profile_id
    // open_recent_connection → Connecting state and/or ConnectTab on channel
}
```

- [ ] **步骤 2–4：实现功能、确认测试通过并提交**

```bash
cargo test -p macsftp-app --bin macsftp open_recent -- --nocapture
git add crates/app/src/workspace/
git commit -m "feat(app): connect from recent entries with profile or form prefill"
```

---

### 任务 8：完整回归验证与人工检查清单

**文件：** 不增加新文件。如果 `docs/gpui-russh-plan.md` 需要一行 session 说明，则可以选择修改该文件；但是仅在本次实施已经涉及架构文档时才进行该修改。

- [ ] **步骤 1：运行专项测试和较完整的测试**

```bash
cargo test -p macsftp-platform -- --nocapture
cargo test -p macsftp-storage -- --nocapture
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

预期结果：所有测试均通过；如果存在与本次修改无关的既有失败，也不得新增失败。

- [ ] **步骤 2：执行人工或本地冒烟验证**

1. 连接真实或 mock host，创建 2 个 tab，并修改本地和远程路径，然后退出应用。
2. 重新启动应用，确认 tab、标题和路径均已恢复，并且**没有**自动连接。
3. 确认重新连接成功；如果条件允许，远程 pane 应访问恢复的路径。
4. 确认空 remote pane 显示 recents；选择条目后应显示表单或开始连接。
5. 确认窗口标题显示 `{tab} — macSFTP`，并且在切换 tab 后更新。
6. 检查 `~/Library/Application Support/macSFTP/session.json` 和 `recents.json`，确认其中没有 password 字段。

- [ ] **步骤 3：仅在存在必要的完善修改时进行最终提交**

```bash
git status
# commit only intentional leftovers
```

---

## 自审（实施计划与设计文档对照）

| 设计要求 | 对应任务 |
| --- | --- |
| 退出时静默保存 Session | 任务 3 |
| 启动时恢复布局，但是不自动 Connect | 任务 3 |
| session.json schema、版本和损坏回退 | 任务 2 |
| AppPaths session/recents | 任务 1 |
| 独立 Recents、profile_id 和 20 项限制 | 任务 4–5 |
| TabConnected 写入 recents | 任务 5 |
| 空状态 Connect 和 recents，不含营销文案 | 任务 5 |
| 选择 Recent 条目后预填或使用 profile 连接 | 任务 7 |
| 窗口标题显示当前 tab | 任务 6 |
| 多窗口使用单一 session 文件，并且第一个窗口恢复会话 | 任务 3（`restore_session` 标志） |
| JSON 不含 secret | 任务 2/4 的测试 |
| 连接后使用恢复的远程路径 | 任务 3 的 TabConnected 路径优先级和任务 7 |
| PR1–4 划分 | 文档前部的 PR 划分表 |

**占位内容检查：** 不存在 TBD/TODO 步骤，并且已经包含具体类型和命令。

**类型一致性：** `SessionTabSnapshot.profile_id: Option<u64>` 与 `ProfileId(u64)` 对应；`RecentEntry.id` 使用 `u64`；任务 3/5/7 共享 `RestoredTabTarget`；main 和测试均更新 `Workspace::new(..., restore_session: bool, ...)`。

---

## 实施交接

本计划保存于 `docs/plans/2026-07-14-phase5-onboarding-persistence-impl.md`，并且按照项目约定与设计文档位于同一目录。

**实施方式有以下两种：**

1. **Subagent-Driven（推荐）**：每个任务使用新的 subagent，并且在任务之间进行审查（`superpowers:subagent-driven-development`）
2. **Inline Execution**：在当前会话中使用 `superpowers:executing-plans`，并设置检查点

实施时需要选择其中一种方式。
