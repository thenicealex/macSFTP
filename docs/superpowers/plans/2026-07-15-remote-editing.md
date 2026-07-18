# 远程编辑（Remote Editing）实现计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 为 macSFTP 增加"下载 → 外部编辑 → 自动回传"的远程文件编辑能力，复用现有传输管线。

**Architecture:** 纯逻辑（`EditSession` / `EditSessionStore` / 状态机）落在 `macsftp-core`，可独立单测；`macsftp-platform` 封装 `open` 与 edits 目录；`macsftp-storage` 加一个持久化配置项；`macsftp-app` 用一个 `cx.spawn` 轮询循环（仿 `AppEventCoordinator`）驱动本地 mtime 监视，命中即通过现有 `StartTransfer` 命令回传，会话注册表挂在跨窗口共享的 `AppResources` 上。

**Tech Stack:** Rust、GPUI、russh/russh-sftp、flume、tokio。测试用 `cargo test` + `gpui::test`。

## Global Constraints

- Rust `1.96.1` 或更新，含 `rustfmt` 和 `clippy`；提交前跑 `bash scripts/check.sh`（fmt + tests + clippy）。
- **不新增第三方依赖**：不引入 `notify`/FSEvents。轮询用 `std::fs::metadata`，定时用 GPUI `background_executor().timer()`。项目有 `deny.toml` 依赖审查。
- **不加内容哈希**：变更检测全程只用 `(size, mtime)`。
- 仅 macOS；打开编辑器用 `/usr/bin/open`。
- 遵循现有 crate 边界：`core` 纯逻辑无 I/O；`platform` 管 macOS 边界；`app` 管编排与 UI。
- 大文件阈值 `EDIT_SIZE_WARN_THRESHOLD = 100 * 1024 * 1024`（100 MB）。
- 轮询间隔 `POLL_INTERVAL = Duration::from_secs(1)`。
- 临时文件布局：`<app_support>/macSFTP/edits/<edit-session-id>/<原始文件名>`。
- `core.rs` 是单文件（无子模块）；新类型追加到 `crates/core/src/core.rs`，测试进其 `#[cfg(test)] mod tests`。

---
## 任务概览

- Task 1: `RemoteSnapshot` + `EditPhase` + `EditSessionId`（core 类型）
- Task 2: `EditSession` 结构 + `local_changed` / `remote_diverged` 状态机（core）
- Task 3: `EditSessionStore` 注册表（core，含 dedup 查重）
- Task 4: `AppPaths.edits_dir` + 启动清理（platform）
- Task 5: `open_in_editor` 编辑器启动（platform）
- Task 6: `AppConfig.external_editor` 持久化配置（storage）
- Task 7: `EditSessionStore` 挂到 `AppResources` + 启动清理接线（app）
- Task 8: Edit 动作：大文件确认 + 发起下载（app）
- Task 9: 下载完成 → 登记会话 + 打开编辑器（app）
- Task 10: `EditWatcher` 轮询循环 + 回传（app）
- Task 11: 远程冲突弹窗（app）
- Task 12: UI 接线：右键菜单 Edit + 双击/回车 + 设置项（app）
- Task 13: 退出清理 + 手动验证

---
### Task 1: core 基础类型（RemoteSnapshot / EditPhase / EditSessionId）

**Files:**
- Modify: `crates/core/src/core.rs`（在 `TransferJob`/`TransferPlan` 附近追加类型；测试进文件底部 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: 现有 `Timestamp`（已派生 `PartialOrd`/`Ord`）、`UserFacingError`
- Produces:
  - `pub struct EditSessionId(pub u64)` — 派生 `Debug, Clone, Copy, PartialEq, Eq, Hash`
  - `pub struct RemoteSnapshot { pub size: Option<u64>, pub modified_at: Option<Timestamp> }` — 派生 `Debug, Clone, Copy, PartialEq, Eq`
  - `pub enum EditPhase { Downloading, Editing, UploadingBack, RemoteConflict, Failed { error: UserFacingError } }` — 派生 `Debug, Clone, PartialEq, Eq`

- [ ] **Step 1: 写失败测试**

在 `crates/core/src/core.rs` 底部 `mod tests` 内新增：

```rust
#[test]
fn edit_phase_and_snapshot_construct() {
    let snap = crate::RemoteSnapshot { size: Some(10), modified_at: Some(crate::Timestamp::from_secs_since_epoch(5)) };
    assert_eq!(snap.size, Some(10));
    assert_eq!(crate::EditSessionId(3), crate::EditSessionId(3));
    let phase = crate::EditPhase::Editing;
    assert_eq!(phase, crate::EditPhase::Editing);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-core edit_phase_and_snapshot_construct`
Expected: FAIL，编译错误 `cannot find type RemoteSnapshot` 等。

- [ ] **Step 3: 追加类型定义**

在 `crates/core/src/core.rs` 的 `TransferJob` 定义之前追加：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EditSessionId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RemoteSnapshot {
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditPhase {
    Downloading,
    Editing,
    UploadingBack,
    RemoteConflict,
    Failed { error: UserFacingError },
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-core edit_phase_and_snapshot_construct`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/core.rs
git commit -m "feat(core): add remote-edit base types"
```

---
### Task 2: EditSession 结构 + 状态机（local_changed / remote_diverged）

**Files:**
- Modify: `crates/core/src/core.rs`

**Interfaces:**
- Consumes: Task 1 的 `EditSessionId`、`RemoteSnapshot`、`EditPhase`；现有 `RemotePath`、`TabId`、`ProfileId`、`LocalPath`、`TransferId`、`Timestamp`
- Produces:
  - `pub struct EditSession { pub id, pub remote_path, pub tab_id, pub session_epoch: u64, pub profile_id, pub local_temp_path, pub phase, pub remote_snapshot, pub local_mtime: Option<Timestamp>, pub active_transfer: Option<TransferId> }`
  - `pub fn EditSession::local_changed(&self, current_mtime: Option<Timestamp>) -> bool`
  - `pub fn EditSession::remote_diverged(&self, current: RemoteSnapshot) -> bool`

- [ ] **Step 1: 写失败测试**

在 `mod tests` 内新增（含一个构造 helper，供后续任务复用）：

```rust
fn sample_edit_session(phase: crate::EditPhase) -> crate::EditSession {
    crate::EditSession {
        id: crate::EditSessionId(1),
        remote_path: crate::RemotePath::new("/srv/a.txt"),
        tab_id: crate::TabId(1),
        session_epoch: 1,
        profile_id: crate::ProfileId(1),
        local_temp_path: crate::LocalPath::new("/tmp/edits/1/a.txt"),
        phase,
        remote_snapshot: crate::RemoteSnapshot { size: Some(10), modified_at: Some(crate::Timestamp::from_secs_since_epoch(100)) },
        local_mtime: Some(crate::Timestamp::from_secs_since_epoch(200)),
        active_transfer: None,
    }
}

#[test]
fn local_changed_only_in_editing_phase() {
    let newer = Some(crate::Timestamp::from_secs_since_epoch(300));
    assert!(sample_edit_session(crate::EditPhase::Editing).local_changed(newer));
    assert!(!sample_edit_session(crate::EditPhase::Downloading).local_changed(newer));
    assert!(!sample_edit_session(crate::EditPhase::UploadingBack).local_changed(newer));
}

#[test]
fn local_changed_detects_newer_mtime() {
    let s = sample_edit_session(crate::EditPhase::Editing);
    assert!(s.local_changed(Some(crate::Timestamp::from_secs_since_epoch(300))));
    assert!(!s.local_changed(Some(crate::Timestamp::from_secs_since_epoch(200))));
    assert!(!s.local_changed(None));
}

#[test]
fn remote_diverged_on_size_or_mtime_and_false_when_identical() {
    let s = sample_edit_session(crate::EditPhase::Editing);
    assert!(s.remote_diverged(crate::RemoteSnapshot { size: Some(11), modified_at: Some(crate::Timestamp::from_secs_since_epoch(100)) }));
    assert!(s.remote_diverged(crate::RemoteSnapshot { size: Some(10), modified_at: Some(crate::Timestamp::from_secs_since_epoch(101)) }));
    assert!(!s.remote_diverged(crate::RemoteSnapshot { size: Some(10), modified_at: Some(crate::Timestamp::from_secs_since_epoch(100)) }));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-core local_changed_only_in_editing_phase remote_diverged_on_size_or_mtime_and_false_when_identical local_changed_detects_newer_mtime`
Expected: FAIL，`cannot find type EditSession`。

- [ ] **Step 3: 追加结构与状态机**

在 Task 1 类型之后追加：

```rust
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSession {
    pub id: EditSessionId,
    pub remote_path: RemotePath,
    pub tab_id: TabId,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub local_temp_path: LocalPath,
    pub phase: EditPhase,
    pub remote_snapshot: RemoteSnapshot,
    pub local_mtime: Option<Timestamp>,
    pub active_transfer: Option<TransferId>,
}

impl EditSession {
    /// 仅 Editing 阶段、且本地 mtime 严格变新时返回 true。
    pub fn local_changed(&self, current_mtime: Option<Timestamp>) -> bool {
        if self.phase != EditPhase::Editing {
            return false;
        }
        match (self.local_mtime, current_mtime) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        }
    }

    /// 远程 (size, mtime) 与下载时快照不一致即视为已改动。
    pub fn remote_diverged(&self, current: RemoteSnapshot) -> bool {
        current != self.remote_snapshot
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-core local_changed remote_diverged`
Expected: PASS（3 个测试）

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/core.rs
git commit -m "feat(core): add EditSession with change/divergence state machine"
```

---
### Task 3: EditSessionStore 注册表（含 dedup 查重）

**Files:**
- Modify: `crates/core/src/core.rs`

**Interfaces:**
- Consumes: Task 2 的 `EditSession`；`EditSessionId`、`TransferId`、`ProfileId`、`RemotePath`、`LocalPath`
- Produces:
  - `pub struct EditSessionStore`（内部 `Vec<EditSession>` + `next_id: u64`）
  - `pub fn EditSessionStore::new() -> Self`
  - `pub fn register(&mut self, session: EditSession) -> EditSessionId`
  - `pub fn next_id(&mut self) -> EditSessionId`
  - `pub fn get(&self, id: EditSessionId) -> Option<&EditSession>`
  - `pub fn get_mut(&mut self, id: EditSessionId) -> Option<&mut EditSession>`
  - `pub fn find_by_transfer(&self, transfer: TransferId) -> Option<&EditSession>`
  - `pub fn find_by_temp_path(&self, path: &LocalPath) -> Option<&EditSession>` — **主要关联方式**：传输完成事件只带 `transfer_id`，无法在发起时预知；通过 `find_job(transfer_id).destination`（下载）或 `.source`（上传）得到本地 temp 路径，再匹配会话
  - `pub fn find_active(&self, profile_id: ProfileId, remote_path: &RemotePath) -> Option<&EditSession>`
  - `pub fn remove(&mut self, id: EditSessionId) -> Option<EditSession>`
  - `pub fn editing_sessions(&self) -> impl Iterator<Item = &EditSession>`（phase == Editing）

- [ ] **Step 1: 写失败测试**

```rust
fn store_session(id_hint: u64, profile: u64, path: &str, phase: crate::EditPhase, transfer: Option<crate::TransferId>) -> crate::EditSession {
    crate::EditSession {
        id: crate::EditSessionId(id_hint),
        remote_path: crate::RemotePath::new(path),
        tab_id: crate::TabId(1),
        session_epoch: 1,
        profile_id: crate::ProfileId(profile),
        local_temp_path: crate::LocalPath::new(format!("/tmp/edits/{id_hint}")),
        phase,
        remote_snapshot: crate::RemoteSnapshot { size: None, modified_at: None },
        local_mtime: None,
        active_transfer: transfer,
    }
}

#[test]
fn store_register_find_and_remove() {
    let mut store = crate::EditSessionStore::new();
    let id = store.next_id();
    let mut s = store_session(id.0, 1, "/srv/a.txt", crate::EditPhase::Downloading, Some(crate::TransferId(7)));
    s.id = id;
    store.register(s);
    assert!(store.find_by_transfer(crate::TransferId(7)).is_some());
    assert!(store.find_by_temp_path(&crate::LocalPath::new(format!("/tmp/edits/{}", id.0))).is_some());
    assert!(store.find_active(crate::ProfileId(1), &crate::RemotePath::new("/srv/a.txt")).is_some());
    assert!(store.get(id).is_some());
    assert!(store.remove(id).is_some());
    assert!(store.get(id).is_none());
    assert!(store.find_by_transfer(crate::TransferId(7)).is_none());
}

#[test]
fn store_dedup_by_profile_and_remote_path() {
    let mut store = crate::EditSessionStore::new();
    let id1 = store.next_id();
    let mut s1 = store_session(id1.0, 1, "/srv/a.txt", crate::EditPhase::Editing, None);
    s1.id = id1;
    store.register(s1);
    // 同 profile+path 已有活跃会话可被查到；不同 path 查不到。
    assert!(store.find_active(crate::ProfileId(1), &crate::RemotePath::new("/srv/a.txt")).is_some());
    assert!(store.find_active(crate::ProfileId(1), &crate::RemotePath::new("/srv/b.txt")).is_none());
    assert!(store.find_active(crate::ProfileId(2), &crate::RemotePath::new("/srv/a.txt")).is_none());
}

#[test]
fn store_editing_sessions_filters_phase() {
    let mut store = crate::EditSessionStore::new();
    let a = store.next_id(); let mut sa = store_session(a.0, 1, "/a", crate::EditPhase::Editing, None); sa.id = a; store.register(sa);
    let b = store.next_id(); let mut sb = store_session(b.0, 1, "/b", crate::EditPhase::Downloading, None); sb.id = b; store.register(sb);
    assert_eq!(store.editing_sessions().count(), 1);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-core store_register_find_and_remove store_dedup_by_profile_and_remote_path store_editing_sessions_filters_phase`
Expected: FAIL，`cannot find type EditSessionStore`。

- [ ] **Step 3: 实现 EditSessionStore**

```rust
#[derive(Debug, Default)]
pub struct EditSessionStore {
    sessions: Vec<EditSession>,
    next_id: u64,
}

impl EditSessionStore {
    pub fn new() -> Self {
        Self { sessions: Vec::new(), next_id: 1 }
    }

    pub fn next_id(&mut self) -> EditSessionId {
        let id = EditSessionId(self.next_id);
        self.next_id += 1;
        id
    }

    pub fn register(&mut self, session: EditSession) -> EditSessionId {
        let id = session.id;
        self.sessions.push(session);
        id
    }

    pub fn get(&self, id: EditSessionId) -> Option<&EditSession> {
        self.sessions.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: EditSessionId) -> Option<&mut EditSession> {
        self.sessions.iter_mut().find(|s| s.id == id)
    }

    pub fn find_by_transfer(&self, transfer: TransferId) -> Option<&EditSession> {
        self.sessions.iter().find(|s| s.active_transfer == Some(transfer))
    }

    pub fn find_by_temp_path(&self, path: &LocalPath) -> Option<&EditSession> {
        self.sessions.iter().find(|s| &s.local_temp_path == path)
    }

    pub fn find_active(&self, profile_id: ProfileId, remote_path: &RemotePath) -> Option<&EditSession> {
        self.sessions.iter().find(|s| s.profile_id == profile_id && &s.remote_path == remote_path)
    }

    pub fn remove(&mut self, id: EditSessionId) -> Option<EditSession> {
        let index = self.sessions.iter().position(|s| s.id == id)?;
        Some(self.sessions.remove(index))
    }

    pub fn editing_sessions(&self) -> impl Iterator<Item = &EditSession> {
        self.sessions.iter().filter(|s| s.phase == EditPhase::Editing)
    }
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-core store_`
Expected: PASS（3 个测试）

- [ ] **Step 5: 提交**

```bash
git add crates/core/src/core.rs
git commit -m "feat(core): add EditSessionStore registry with dedup lookup"
```

---
### Task 4: AppPaths.edits_dir + edits 目录清理（platform）

**Files:**
- Modify: `crates/platform/src/platform.rs`（`AppPaths` 结构 + `from_home_dir` + `ensure_directories`；测试进文件底部 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: 现有 `LocalPath`、`AppPaths`
- Produces:
  - `AppPaths` 新增字段 `pub edits_dir: LocalPath`（目录，非文件）
  - `pub fn clear_edits_dir(edits_dir: &LocalPath) -> std::io::Result<()>`（删除并重建空目录；NotFound 视为成功）

- [ ] **Step 1: 写失败测试**

在 `crates/platform/src/platform.rs` 的 `mod tests` 内新增：

```rust
#[test]
fn app_paths_expose_edits_dir_under_app_support() {
    let paths = super::AppPaths::from_home_dir("/Users/tester");
    assert_eq!(
        paths.edits_dir.as_str(),
        "/Users/tester/Library/Application Support/macSFTP/edits"
    );
}

#[test]
fn clear_edits_dir_removes_contents_and_recreates() {
    let base = std::env::temp_dir().join(format!("macsftp-edits-clear-{}", std::process::id()));
    let edits = macsftp_core::LocalPath::new(base.to_string_lossy().to_string());
    std::fs::create_dir_all(base.join("session-1")).unwrap();
    std::fs::write(base.join("session-1/a.txt"), b"x").unwrap();
    super::clear_edits_dir(&edits).unwrap();
    assert!(base.exists());
    assert_eq!(std::fs::read_dir(&base).unwrap().count(), 0);
    std::fs::remove_dir_all(&base).ok();
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-platform edits_dir clear_edits_dir`
Expected: FAIL，`no field edits_dir` / `cannot find function clear_edits_dir`。

- [ ] **Step 3: 加字段与函数**

在 `AppPaths` 结构末尾加字段 `pub edits_dir: LocalPath,`。在 `from_home_dir` 的 `Self { ... }` 里加 `edits_dir: LocalPath::new(format!("{app_support_dir}/edits")),`。在 `ensure_directories` 的文件数组中不需加（edits 是目录，由 `clear_edits_dir` 在启动时创建）。在文件末尾（`mod tests` 之前）追加：

```rust
/// 删除整个 edits 目录并重建为空。启动时调用以清掉上次运行的残留
/// 编辑临时文件（编辑会话不跨重启持久化）。NotFound 视为成功。
pub fn clear_edits_dir(edits_dir: &LocalPath) -> std::io::Result<()> {
    match std::fs::remove_dir_all(edits_dir.as_str()) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::create_dir_all(edits_dir.as_str())?;
    std::fs::set_permissions(edits_dir.as_str(), std::fs::Permissions::from_mode(0o700))
}
```

注意 `set_permissions`/`from_mode` 需要 `use std::os::unix::fs::PermissionsExt;`（文件顶部可能已有；若无则加）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-platform edits_dir clear_edits_dir`
Expected: PASS（2 个测试）

- [ ] **Step 5: 提交**

```bash
git add crates/platform/src/platform.rs
git commit -m "feat(platform): add edits_dir path and clear_edits_dir cleanup"
```

---
### Task 5: open_in_editor 编辑器启动（platform）

**Files:**
- Modify: `crates/platform/src/platform.rs`

**Interfaces:**
- Consumes: 现有 `LocalPath`
- Produces:
  - `pub fn build_open_command(temp: &LocalPath, editor: Option<&str>) -> std::process::Command`（纯构造，可测参数拼接，不 spawn）
  - `pub fn open_in_editor(temp: &LocalPath, editor: Option<&str>) -> std::io::Result<()>`（调用 `build_open_command` 并 `.spawn()`）

- [ ] **Step 1: 写失败测试**

```rust
#[test]
fn build_open_command_uses_system_default_when_no_editor() {
    let cmd = super::build_open_command(&macsftp_core::LocalPath::new("/tmp/edits/1/a.txt"), None);
    assert_eq!(cmd.get_program(), "/usr/bin/open");
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert_eq!(args, vec!["/tmp/edits/1/a.txt"]);
}

#[test]
fn build_open_command_uses_named_editor() {
    let cmd = super::build_open_command(&macsftp_core::LocalPath::new("/tmp/edits/1/a.txt"), Some("Visual Studio Code"));
    let args: Vec<_> = cmd.get_args().map(|a| a.to_string_lossy().to_string()).collect();
    assert_eq!(args, vec!["-a", "Visual Studio Code", "/tmp/edits/1/a.txt"]);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-platform build_open_command`
Expected: FAIL，`cannot find function build_open_command`。

- [ ] **Step 3: 实现**

在文件末尾（`mod tests` 之前）追加：

```rust
/// 构造用 macOS `open` 打开临时文件的命令。editor 为 None 时用系统默认
/// 关联应用；为 Some(app) 时用 `open -a <app>`。拆出以便单测参数拼接。
pub fn build_open_command(temp: &LocalPath, editor: Option<&str>) -> std::process::Command {
    let mut cmd = std::process::Command::new("/usr/bin/open");
    match editor {
        None => {
            cmd.arg(temp.as_str());
        }
        Some(app) => {
            cmd.args(["-a", app, temp.as_str()]);
        }
    }
    cmd
}

/// 打开临时文件进行编辑。`open` 立即返回，不绑定子进程生命周期，
/// 天然满足应用级监视。
pub fn open_in_editor(temp: &LocalPath, editor: Option<&str>) -> std::io::Result<()> {
    build_open_command(temp, editor).spawn().map(|_| ())
}
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-platform build_open_command`
Expected: PASS（2 个测试）

- [ ] **Step 5: 提交**

```bash
git add crates/platform/src/platform.rs
git commit -m "feat(platform): add open_in_editor via macOS open"
```

---
### Task 6: AppConfig.external_editor 持久化配置（storage）

**Files:**
- Modify: `crates/storage/src/config.rs`（`AppConfig` 结构 + `Default` + 新 setter；测试进文件底部 `#[cfg(test)] mod tests`）

**Interfaces:**
- Consumes: 现有 `AppConfig`、`ConfigStore`、`ConfigError`、`set_confirm_delete` 模式
- Produces:
  - `AppConfig` 新增字段 `pub external_editor: Option<String>`（默认 `None`）
  - `pub fn ConfigStore::set_external_editor(&mut self, editor: Option<String>) -> Result<(), ConfigError>`

- [ ] **Step 1: 写失败测试**

在 `crates/storage/src/config.rs` 的 `mod tests` 内新增（仿现有 `show_hidden_files_defaults_false_and_round_trips`）：

```rust
#[test]
fn external_editor_defaults_none_and_round_trips() {
    let dir = std::env::temp_dir().join(format!("macsftp-editor-cfg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = crate::LocalPath::new(dir.join("config.json").to_string_lossy().to_string());
    let mut store = super::ConfigStore::with_defaults(path.clone());
    assert_eq!(store.config().external_editor, None);
    store.set_external_editor(Some("Visual Studio Code".to_string())).expect("persist editor");
    let restored = super::ConfigStore::open(path).expect("reopen");
    assert_eq!(restored.config().external_editor.as_deref(), Some("Visual Studio Code"));
    std::fs::remove_dir_all(&dir).ok();
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-storage external_editor_defaults_none_and_round_trips`
Expected: FAIL，`no field external_editor`。

- [ ] **Step 3: 加字段、默认值、setter**

在 `AppConfig` 结构末尾加：

```rust
    /// 远程编辑用的外部编辑器应用名。None = 系统默认关联应用。
    #[serde(default)]
    pub external_editor: Option<String>,
```

在 `impl Default for AppConfig` 的 `Self { ... }` 末尾加 `external_editor: None,`。

在 `ConfigStore` 的 `set_show_hidden_files` 之后追加（严格复用同款回滚模式）：

```rust
    pub fn set_external_editor(&mut self, editor: Option<String>) -> Result<(), ConfigError> {
        let previous = self.config.external_editor.clone();
        self.config.external_editor = editor;
        let result = self.save();
        if result.is_ok() {
            self.initial_error = None;
        } else {
            self.config.external_editor = previous;
        }
        result
    }
```

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p macsftp-storage external_editor_defaults_none_and_round_trips`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/storage/src/config.rs
git commit -m "feat(storage): persist external_editor config"
```

---
### Task 7: EditSessionStore 挂到 AppResources + 启动清理接线（app）

**Files:**
- Modify: `crates/app/src/resources.rs`（`AppResources` 结构 + `load_with_profiles`）
- Modify: `crates/app/src/main.rs`（启动时调 `clear_edits_dir`）

**Interfaces:**
- Consumes: Task 3 的 `EditSessionStore`；Task 4 的 `AppPaths.edits_dir` + `clear_edits_dir`
- Produces:
  - `AppResources` 新增字段 `pub edit_sessions: EditSessionStore`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/resources.rs` 底部新增 `#[cfg(test)] mod edit_tests`（若已有 tests mod 则并入）：

```rust
#[cfg(test)]
mod edit_tests {
    use super::AppResources;
    use macsftp_platform::AppPaths;
    use macsftp_storage::ConfigStore;

    #[test]
    fn app_resources_start_with_empty_edit_sessions() {
        let home = std::env::temp_dir().join(format!("macsftp-res-edit-{}", std::process::id()));
        let app_paths = AppPaths::from_home_dir(home.to_string_lossy().as_ref());
        let config = ConfigStore::with_defaults(app_paths.config_file.clone());
        let resources = AppResources::load_for_test(app_paths, config);
        assert_eq!(resources.edit_sessions.editing_sessions().count(), 0);
    }
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app app_resources_start_with_empty_edit_sessions`
Expected: FAIL，`no field edit_sessions`。

- [ ] **Step 3: 加字段并初始化**

在 `crates/app/src/resources.rs`：`use macsftp_core::EditSessionStore;`（顶部 use 区）。`AppResources` 结构加 `pub edit_sessions: EditSessionStore,`。在 `load_with_profiles` 的 `Self { ... }` 里加 `edit_sessions: EditSessionStore::new(),`。

- [ ] **Step 4: 启动清理接线（main.rs）**

在 `crates/app/src/main.rs` 中，`cx.set_global(AppResources::load(...))` **之前**，加一次残留清理（`app_paths` 此处仍可访问）：

```rust
if let Err(error) = macsftp_platform::clear_edits_dir(&app_paths.edits_dir) {
    warn!(error = %error, "could not clear stale edits directory at launch");
}
```

确认 `warn` 已 `use tracing::warn;`（main.rs 已用 tracing；若无则加）。

- [ ] **Step 5: 跑测试 + 构建确认通过**

Run: `cargo test -p macsftp-app app_resources_start_with_empty_edit_sessions && cargo build -p macsftp-app`
Expected: PASS + 构建成功

- [ ] **Step 6: 提交**

```bash
git add crates/app/src/resources.rs crates/app/src/main.rs
git commit -m "feat(app): hold EditSessionStore in AppResources and clear edits at launch"
```

---
### Task 8: Edit 动作 — 大文件确认 + 发起下载（app）

**背景**：编辑的临时目标路径是 `<edits_dir>/<edit-session-id>/<原始文件名>`。发起时
先分配 `EditSessionId`（`edit_sessions.next_id()`），登记一个 `Downloading` 阶段的
会话（`active_transfer: None`，因为 transfer_id 要等运行时分配），再发 `StartTransfer`
下载命令，目标 = 该临时路径。传输完成事件在 Task 9 通过 `find_by_temp_path` 关联回会话。

**Files:**
- Create: `crates/app/src/workspace/remote_edit.rs`（新模块，放全部远程编辑的 workspace 方法）
- Modify: `crates/app/src/workspace/mod.rs`（声明 `mod remote_edit;`；加 `large_edit_confirm: Option<PendingEdit>` 字段）

**Interfaces:**
- Consumes: Task 3 `EditSessionStore`/`EditSession`；Task 4 `edits_dir`；现有 `StartTransferCommand`、`send_command`、`connected_transfer_session`、`RemoteEntry`、`AppResources`
- Produces:
  - `pub(crate) struct PendingEdit { pub remote_path: RemotePath, pub size: Option<u64>, pub modified_at: Option<Timestamp>, pub session_epoch: u64, pub profile_id: ProfileId, pub tab_id: TabId }`
  - `pub(crate) fn Workspace::begin_edit(&mut self, entry: &RemoteEntry, cx)` — 入口：查重、大文件判定
  - `pub(crate) fn Workspace::start_edit_download(&mut self, pending: PendingEdit, cx)` — 分配会话 + 发下载
  - `const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/workspace/tests.rs`（现有集成测试文件）新增。先看该文件里已有的 workspace 构造 helper（如 `new_test_workspace` 之类）并复用；下面用占位名 `test_workspace_connected(cx)` 表示"构造一个已连接、有活跃 tab 的 workspace"，实现时替换为该文件真实 helper：

```rust
#[gpui::test]
fn edit_small_file_starts_download_to_edits_dir(cx: &mut gpui::TestAppContext) {
    // Arrange: 已连接 workspace + 一个远程小文件 entry。
    // Act: workspace.begin_edit(&entry, cx)
    // Assert: edit_sessions 里新增一个 Downloading 会话，
    //   其 local_temp_path 落在 <edits_dir>/<id>/<filename> 下，
    //   且发出了一条 StartTransfer(Download) 命令（用 mock runtime 捕获）。
    // 具体断言按 tests.rs 现有 runtime-capture 模式书写。
}

#[gpui::test]
fn edit_large_file_requires_confirmation_first(cx: &mut gpui::TestAppContext) {
    // size > 100MB 的 entry: begin_edit 不直接发下载，
    // 而是设置 workspace.large_edit_confirm = Some(pending)。
}
```

> 注：`tests.rs` 已有完整的 workspace + mock runtime 搭建模式（见文件内 `RuntimeClient`/`BridgeChannels` 用法）。实现测试时严格套用该文件既有 helper，不要新造一套。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app edit_small_file_starts_download_to_edits_dir edit_large_file_requires_confirmation_first`
Expected: FAIL，`no method begin_edit` / `no field large_edit_confirm`。

- [ ] **Step 3: 建模块 + 字段**

`crates/app/src/workspace/mod.rs`：在其它 `mod` 声明处加 `mod remote_edit;`。`Workspace` 结构加 `large_edit_confirm: Option<remote_edit::PendingEdit>,`；`Workspace::new` 的初始化加 `large_edit_confirm: None,`。

创建 `crates/app/src/workspace/remote_edit.rs`：

```rust
use gpui::Context;
use macsftp_core::{
    EditPhase, EditSession, LocalPath, ProfileId, RemoteEntry, RemotePath, RemoteSnapshot,
    StartTransferCommand, TabId, Timestamp, TransferDirection, TransferEndpoint, AppCommand,
    MetadataPolicy, ConflictPolicy,
};

use crate::resources::{ActiveResources};
use crate::workspace::helpers::connected_transfer_session;
use crate::workspace::Workspace;

pub(crate) const EDIT_SIZE_WARN_THRESHOLD: u64 = 100 * 1024 * 1024;

#[derive(Debug, Clone)]
pub(crate) struct PendingEdit {
    pub remote_path: RemotePath,
    pub size: Option<u64>,
    pub modified_at: Option<Timestamp>,
    pub session_epoch: u64,
    pub profile_id: ProfileId,
    pub tab_id: TabId,
}
```

- [ ] **Step 4: 实现 begin_edit + start_edit_download**

续写 `remote_edit.rs`：

```rust
impl Workspace {
    pub(crate) fn begin_edit(&mut self, entry: &RemoteEntry, cx: &mut Context<Self>) {
        let Some(tab) = self.active_tab() else { return; };
        let Some((session_epoch, profile_id)) = connected_transfer_session(tab) else {
            self.status_message = Some("Connect before editing".into());
            cx.notify();
            return;
        };
        let tab_id = tab.id;
        // 查重：同 profile + 远程路径已有活跃会话 → 复用，不重复下载。
        if self.resources().edit_sessions.find_active(profile_id, &entry.path).is_some() {
            self.status_message = Some("This file is already open for editing".into());
            cx.notify();
            return;
        }
        let pending = PendingEdit {
            remote_path: entry.path.clone(),
            size: entry.size,
            modified_at: entry.modified_at,
            session_epoch,
            profile_id,
            tab_id,
        };
        if pending.size.unwrap_or(0) > EDIT_SIZE_WARN_THRESHOLD {
            self.large_edit_confirm = Some(pending);
            cx.notify();
            return;
        }
        self.start_edit_download(pending, cx);
    }

    pub(crate) fn start_edit_download(&mut self, pending: PendingEdit, cx: &mut Context<Self>) {
        let edits_dir = self.resources().app_paths.edits_dir.clone();
        let id = self.resources_mut().edit_sessions.next_id();
        let file_name = std::path::Path::new(pending.remote_path.as_str())
            .file_name().map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "file".to_string());
        let local_temp_path = LocalPath::new(format!("{}/{}/{}", edits_dir.as_str(), id.0, file_name));
        // 建会话目录（忽略已存在）。
        if let Some(parent) = std::path::Path::new(local_temp_path.as_str()).parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let session = EditSession {
            id,
            remote_path: pending.remote_path.clone(),
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            local_temp_path: local_temp_path.clone(),
            phase: EditPhase::Downloading,
            remote_snapshot: RemoteSnapshot { size: pending.size, modified_at: pending.modified_at },
            local_mtime: None,
            active_transfer: None,
        };
        self.resources_mut().edit_sessions.register(session);
        let command = AppCommand::StartTransfer(StartTransferCommand {
            tab_id: pending.tab_id,
            session_epoch: pending.session_epoch,
            profile_id: pending.profile_id,
            direction: TransferDirection::Download,
            sources: vec![TransferEndpoint::Remote(pending.remote_path)],
            destination: TransferEndpoint::Local(local_temp_path),
            metadata_policy: MetadataPolicy::default(),
            conflict_policy: ConflictPolicy::default(),
        });
        if self.send_command(command, cx) {
            self.status_message = Some("Opening for edit…".into());
            cx.notify();
        }
    }
}
```

> 若上面 `use` 的某个类型不在 `macsftp_core` 根导出，按 `transfers.rs` 的既有 `use` 路径修正（该文件已导入 `StartTransferCommand`/`TransferEndpoint` 等，照抄其导入来源）。

- [ ] **Step 5: 跑测试 + 构建**

Run: `cargo test -p macsftp-app edit_small_file_starts_download_to_edits_dir edit_large_file_requires_confirmation_first && cargo build -p macsftp-app`
Expected: PASS + 构建成功

- [ ] **Step 6: 提交**

```bash
git add crates/app/src/workspace/remote_edit.rs crates/app/src/workspace/mod.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): begin_edit downloads remote file to edits dir with large-file guard"
```

---
### Task 9: 下载完成 → 登记 mtime + 打开编辑器（app）

**背景**：传输事件由 `AppEventCoordinator::dispatch_event`（`event_coordinator.rs`）进程级
处理一次，编辑会话也在进程级 `AppResources` 全局里——所以关联逻辑放这里，而非 per-window
workspace。完成事件只带 `transfer_id`；用 `cx.transfers().find_job(transfer_id)` 取 job，
job 的 `destination`（下载）本地路径匹配会话 `local_temp_path`。为可测，关联+推进逻辑抽成
一个纯函数在 core 之外的 app 侧 helper，`open_in_editor` 通过可注入函数指针替换。

**Files:**
- Modify: `crates/app/src/event_coordinator.rs`（`dispatch_event` 的 transfer 分支后加编辑推进）
- Modify: `crates/app/src/resources.rs`（加一个 `pub trait EditSessions { fn advance_edit_on_transfer(...) }` 或直接自由函数——见下）

**Interfaces:**
- Consumes: Task 3 `EditSessionStore`（`find_by_temp_path` / `get_mut` / `remove`）；Task 5 `open_in_editor`；现有 `TransferStore::find_job`、`TransferEndpoint`、`AppEvent::TransferCompleted { transfer_id }` / `TransferFailed(failure)`
- Produces:
  - `crates/app/src/event_coordinator.rs` 内自由函数 `fn advance_edit_sessions(event: &AppEvent, cx: &mut App)`
  - 可注入编辑器打开钩子：`type EditorOpener = fn(&LocalPath, Option<&str>) -> std::io::Result<()>;`，默认 `macsftp_platform::open_in_editor`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/event_coordinator.rs` 的 `mod tests` 内新增（复用文件里的 `test_app_paths` + `AppResources` 搭建）：

```rust
#[gpui::test]
fn download_completion_moves_edit_session_to_editing(cx: &mut gpui::TestAppContext) {
    // Arrange: 全局装好 AppResources；手动 register 一个 Downloading 会话，
    //   active_transfer=Some(T)，local_temp_path 指向一个真实存在的临时文件；
    //   TransferStore 里插入一个 destination = 该 temp path、id=T 的 Completed job。
    // 用一个记录调用的 mock EditorOpener（AtomicBool）替换默认 opener。
    // Act: dispatch TransferCompleted{transfer_id:T}。
    // Assert: 会话 phase 变 Editing、local_mtime = Some(...)、mock opener 被调用一次。
}

#[gpui::test]
fn download_failure_moves_edit_session_to_failed(cx: &mut gpui::TestAppContext) {
    // Downloading 会话 + TransferFailed → 会话 phase 变 Failed，opener 不被调用。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app download_completion_moves_edit_session_to_editing download_failure_moves_edit_session_to_failed`
Expected: FAIL，`cannot find function advance_edit_sessions`。

- [ ] **Step 3: 实现关联+推进**

在 `event_coordinator.rs` 顶部加：

```rust
use macsftp_core::{EditPhase, LocalPath, Timestamp, TransferEndpoint};
use crate::resources::ActiveTransfers;

type EditorOpener = fn(&LocalPath, Option<&str>) -> std::io::Result<()>;

/// 生产默认打开器；测试通过 set_edit_opener 替换。
static EDIT_OPENER: std::sync::atomic::AtomicPtr<()> = std::sync::atomic::AtomicPtr::new(std::ptr::null_mut());

fn edit_opener() -> EditorOpener {
    let ptr = EDIT_OPENER.load(std::sync::atomic::Ordering::Relaxed);
    if ptr.is_null() {
        macsftp_platform::open_in_editor
    } else {
        // SAFETY: 仅由 set_edit_opener(test) 写入合法函数指针。
        unsafe { std::mem::transmute::<*mut (), EditorOpener>(ptr) }
    }
}

#[cfg(test)]
pub(crate) fn set_edit_opener(opener: EditorOpener) {
    EDIT_OPENER.store(opener as *mut (), std::sync::atomic::Ordering::Relaxed);
}
```

> 若项目风格不喜欢 `AtomicPtr`/`transmute`，改用一个 `thread_local!` 的 `RefCell<Option<EditorOpener>>` 更安全；实现者按 clippy 反馈择一，功能等价即可。

- [ ] **Step 4: 实现 advance_edit_sessions 并接线**

续写 `event_coordinator.rs`：

```rust
fn advance_edit_sessions(event: &AppEvent, cx: &mut App) {
    let (transfer_id, succeeded) = match event {
        AppEvent::TransferCompleted { transfer_id } => (*transfer_id, true),
        AppEvent::TransferFailed(failure) => (failure.transfer_id, false),
        _ => return,
    };
    // 用完成的 job 的本地目标路径关联到编辑会话（下载：destination 是本地）。
    let temp_path = cx.transfers().find_job(transfer_id).and_then(|job| match &job.destination {
        TransferEndpoint::Local(path) => Some(path.clone()),
        TransferEndpoint::Remote(_) => None,
    });
    let Some(temp_path) = temp_path else { return; };
    let session_id = match cx.resources().edit_sessions.find_by_temp_path(&temp_path) {
        Some(session) if session.phase == EditPhase::Downloading => session.id,
        _ => return,
    };
    if !succeeded {
        if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
            session.phase = EditPhase::Failed {
                error: macsftp_core::UserFacingError::new(
                    macsftp_core::ErrorCode::TransferFailed, "Download failed; not opened for edit",
                ),
            };
        }
        return;
    }
    // 读下载后的本地 mtime 作为监视基准。
    let mtime = std::fs::metadata(temp_path.as_str()).ok()
        .and_then(|m| m.modified().ok()).map(Timestamp::from_system_time);
    let editor = cx.resources().config.config().external_editor.clone();
    if let Some(session) = cx.resources_mut().edit_sessions.get_mut(session_id) {
        session.phase = EditPhase::Editing;
        session.local_mtime = mtime;
        session.active_transfer = None;
    }
    if let Err(error) = edit_opener()(&temp_path, editor.as_deref()) {
        warn!(error = %error, "could not open editor for remote edit");
    }
}
```

在 `dispatch_event` 的 transfer 分支里，`cx.apply_transfer_event(...)` 之后、`return` 之前加一行 `advance_edit_sessions(&event, cx);`。

> `ErrorCode::TransferFailed`、`UserFacingError::new` 的确切签名以 `core.rs` 为准（`UserFacingError::new(code, message)` 见 line ~2041 附近的同名构造模式；若签名不同则照 core 现有构造方式改写）。

- [ ] **Step 5: 跑测试 + 构建**

Run: `cargo test -p macsftp-app download_completion_moves_edit_session_to_editing download_failure_moves_edit_session_to_failed && cargo build -p macsftp-app`
Expected: PASS + 构建成功

- [ ] **Step 6: 提交**

```bash
git add crates/app/src/event_coordinator.rs crates/app/src/resources.rs
git commit -m "feat(app): open editor and start watching when edit download completes"
```

---
### Task 10: EditWatcher 轮询循环 + 回传（app）

**背景**：仿 `AppEventCoordinator::start` 的 `cx.spawn` 长驻循环，每 `POLL_INTERVAL`
醒来，对每个 `Editing` 会话 `stat` 本地临时文件。命中 `local_changed` → 先取远程当前
`(size, mtime)` 快照判定 `remote_diverged`：一致则回传（`StartTransfer(Upload)`，
source = 本地 temp，destination = 远程原路径），会话进 `UploadingBack`；不一致则进
`RemoteConflict`（Task 11 弹窗）。回传完成/失败在 Task 9 的 `advance_edit_sessions`
里对 `UploadingBack` 会话做收尾（完成 → 刷新基准回 `Editing`；失败 → 回 `Editing`）。

**远程快照来源**：轮询循环在主线程只能读已有的 `TabState.remote.entries` 列表——即
以最近一次远程目录列表里该文件的 `(size, mtime)` 为"当前远程快照"。这是零额外 SFTP 往返
的近似（与浏览刷新一致）；足够满足"检测他人改动并警告"的目标。

**Files:**
- Create: `crates/app/src/edit_watcher.rs`
- Modify: `crates/app/src/main.rs`（启动 `EditWatcher::start(cx)` 并存为 global，仿 `event_coordinator`）
- Modify: `crates/app/src/event_coordinator.rs`（`advance_edit_sessions` 增加 `UploadingBack` 收尾分支）

**Interfaces:**
- Consumes: Task 3 store；Task 8 的上传命令构造模式；`EditSession::local_changed` / `remote_diverged`；GPUI `cx.background_executor().timer()`
- Produces:
  - `pub struct EditWatcher { _task: Task<()> }` + `pub fn EditWatcher::start(cx: &mut App) -> Self`
  - `pub(crate) fn poll_edit_sessions(cx: &mut App)`（纯 app 逻辑，测试可直接调用而不等定时器）
  - `const POLL_INTERVAL: Duration = Duration::from_secs(1);`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/edit_watcher.rs` 的 `mod tests` 内：

```rust
#[gpui::test]
fn poll_uploads_when_local_file_changed_and_remote_unchanged(cx: &mut gpui::TestAppContext) {
    // Arrange: Editing 会话，local_temp_path 指向真实文件；写入后把 mtime 记为旧值，
    //   再 touch 文件让磁盘 mtime 变新；远程快照与会话 remote_snapshot 一致
    //   （通过 tab.remote.entries 里放同 size+mtime 的 entry）。
    // Act: poll_edit_sessions(cx)。
    // Assert: 发出 StartTransfer(Upload) 命令；会话 phase 变 UploadingBack。
}

#[gpui::test]
fn poll_ignores_unchanged_files(cx: &mut gpui::TestAppContext) {
    // 磁盘 mtime 未变 → 不发命令，phase 保持 Editing。
}

#[gpui::test]
fn poll_flags_conflict_when_remote_diverged(cx: &mut gpui::TestAppContext) {
    // 本地变了、但 tab.remote.entries 里该文件 size 与快照不同 →
    //   会话 phase 变 RemoteConflict，不发上传命令。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app poll_uploads_when_local_file_changed_and_remote_unchanged poll_ignores_unchanged_files poll_flags_conflict_when_remote_diverged`
Expected: FAIL，`cannot find function poll_edit_sessions`。

- [ ] **Step 3: 实现 EditWatcher::start**

创建 `crates/app/src/edit_watcher.rs`：

```rust
use std::time::Duration;
use gpui::{App, Global, Task};

pub(crate) const POLL_INTERVAL: Duration = Duration::from_secs(1);

pub struct EditWatcher {
    _task: Task<()>,
}

impl Global for EditWatcher {}

impl EditWatcher {
    pub fn start(cx: &mut App) -> Self {
        let task = cx.spawn(async move |cx| {
            loop {
                cx.background_executor().timer(POLL_INTERVAL).await;
                if cx.update(poll_edit_sessions).is_err() {
                    break; // app 关闭
                }
            }
        });
        Self { _task: task }
    }
}
```

- [ ] **Step 4: 实现 poll_edit_sessions**

续写 `edit_watcher.rs`（`upload_edit_back` 复用 Task 8 的命令构造；把公共构造提到 `remote_edit.rs` 的一个 `pub(crate) fn build_edit_upload_command(...)` 并在此调用，避免重复）：

```rust
use macsftp_core::{
    AppCommand, EditPhase, EditSessionId, LocalPath, RemoteSnapshot, StartTransferCommand,
    Timestamp, TransferDirection, TransferEndpoint, MetadataPolicy, ConflictPolicy,
};
use crate::resources::ActiveResources;

pub(crate) fn poll_edit_sessions(cx: &mut App) {
    // 1) 收集 Editing 会话的 (id, temp_path, remote_path, remote_snapshot)。
    let candidates: Vec<_> = cx.resources().edit_sessions.editing_sessions()
        .map(|s| (s.id, s.local_temp_path.clone(), s.remote_path.clone(), s.remote_snapshot, s.local_mtime, s.session_epoch, s.profile_id, s.tab_id))
        .collect();
    for (id, temp, remote_path, snapshot, last_mtime, epoch, profile, tab_id) in candidates {
        let current = std::fs::metadata(temp.as_str()).ok()
            .and_then(|m| m.modified().ok()).map(Timestamp::from_system_time);
        // 临时文件被删 → 结束会话。
        if current.is_none() && std::fs::metadata(temp.as_str()).is_err() {
            cx.resources_mut().edit_sessions.remove(id);
            continue;
        }
        let changed = match (last_mtime, current) {
            (Some(last), Some(now)) => now > last,
            _ => false,
        };
        if !changed { continue; }
        // 远程当前快照 = 最近一次目录列表里该文件的 (size, mtime)（见任务背景）。
        let remote_now = current_remote_snapshot(cx, tab_id, &remote_path);
        if remote_now.map_or(false, |now| now != snapshot) {
            if let Some(s) = cx.resources_mut().edit_sessions.get_mut(id) {
                s.phase = EditPhase::RemoteConflict;
                s.local_mtime = current; // 记住这次的本地 mtime，避免重复弹
            }
            cx.refresh_windows();
            continue;
        }
        // 一致 → 回传。
        let command = build_upload_command(&temp, &remote_path, epoch, profile, tab_id);
        if let Some(s) = cx.resources_mut().edit_sessions.get_mut(id) {
            s.phase = EditPhase::UploadingBack;
            s.local_mtime = current;
        }
        dispatch_edit_command(cx, command);
    }
}
```

`current_remote_snapshot`、`build_upload_command`、`dispatch_edit_command` 三个 helper 也在本文件实现：`current_remote_snapshot` 遍历该 tab 的 workspace `remote.entries` 找同 `remote_path` 的 entry 取 `RemoteSnapshot{size, modified_at}`；`build_upload_command` 构造 `AppCommand::StartTransfer(Upload)`；`dispatch_edit_command` 拿到任一 workspace window 的 `runtime_client` 发送（仿 event_coordinator 的 `workspace_windows(cx)`）。

> **注意跨窗口发送**：轮询在 `App` 级，没有 `Workspace` 上下文。实现 `dispatch_edit_command` 时，取 `tab_id` 归属的那个 window 的 `runtime_client`（workspace 暴露一个 `pub(crate) fn runtime_client(&self) -> RuntimeClient` 的 clone getter；若不存在则在本任务顺带加一个）。

- [ ] **Step 5: UploadingBack 收尾（event_coordinator.rs）**

在 `advance_edit_sessions` 里，对 `find_job(transfer_id).source` 为本地路径、且会话 phase 为 `UploadingBack` 的情况：完成 → 刷新 `remote_snapshot`（用回传后的本地 size + 当前时间近似，或下次目录刷新纠正）并回 `Editing`；失败 → 回 `Editing` 并 `status`/`warn` 提示回传失败可重试。具体分支：

```rust
// advance_edit_sessions 内，在 Downloading 处理之外，追加对 UploadingBack 的处理：
// 用 job.source（本地 temp）匹配 find_by_temp_path，phase==UploadingBack 时：
//   succeeded → session.phase = Editing; 刷新 remote_snapshot 基准。
//   !succeeded → session.phase = Editing;（保留临时文件，可重试）
```

- [ ] **Step 6: main.rs 启动**

在 `crates/app/src/main.rs`，`AppEventCoordinator::start` 之后加：

```rust
let edit_watcher = EditWatcher::start(cx);
cx.set_global(edit_watcher);
```

并 `use crate::edit_watcher::EditWatcher;`，`mod edit_watcher;`（在 crate 根 `main.rs` 的 mod 声明处）。

- [ ] **Step 7: 跑测试 + 构建**

Run: `cargo test -p macsftp-app poll_ && cargo build -p macsftp-app`
Expected: PASS（3 个）+ 构建成功

- [ ] **Step 8: 提交**

```bash
git add crates/app/src/edit_watcher.rs crates/app/src/main.rs crates/app/src/event_coordinator.rs crates/app/src/workspace/remote_edit.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): poll edited temp files and upload changes back"
```

---
### Task 11: 编辑弹窗 — 大文件确认 + 远程冲突（app）

**背景**：两个模态都渲染在 workspace（有 `Window` 上下文），仿 `render_delete_confirm_modal`
（file_ops.rs:455）。①大文件确认：Task 8 已在 `large_edit_confirm` 存了 `PendingEdit`，
这里加确认/取消处理 + 渲染。②远程冲突：Task 10 把会话置为 `RemoteConflict`；workspace 在
render 时扫描全局 `edit_sessions` 里属于当前 tab 的 `RemoteConflict` 会话并弹窗，给三个选项：
**覆盖上传**（强制回传，刷新快照基准）、**放弃本地改动**（重新下载远程覆盖本地 temp）、**稍后**（关闭弹窗，会话回 `Editing` 但基准 mtime 设为当前，避免立刻再弹）。

**Files:**
- Modify: `crates/app/src/workspace/remote_edit.rs`（确认/取消/冲突处理方法 + 渲染）
- Modify: `crates/app/src/workspace/mod.rs`（render 链里 `.children(self.render_large_edit_modal(cx))` 和 `.children(self.render_edit_conflict_modal(window, cx))`）

**Interfaces:**
- Consumes: Task 8 `PendingEdit`/`large_edit_confirm`/`start_edit_download`；Task 3 store；Task 10 `build_upload_command`
- Produces:
  - `fn Workspace::confirm_large_edit(&mut self, cx)` / `fn cancel_large_edit(&mut self, cx)`
  - `fn Workspace::render_large_edit_modal(&self, cx) -> Option<impl IntoElement>`
  - `fn Workspace::resolve_edit_conflict(&mut self, id: EditSessionId, choice: ConflictChoice, cx)`
  - `pub(crate) enum ConflictChoice { Overwrite, DiscardLocal, Later }`
  - `fn Workspace::render_edit_conflict_modal(&self, window, cx) -> Option<impl IntoElement>`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/workspace/tests.rs`：

```rust
#[gpui::test]
fn confirm_large_edit_starts_download(cx: &mut gpui::TestAppContext) {
    // large_edit_confirm = Some(pending) → confirm_large_edit → 发出下载命令，
    //   large_edit_confirm 清空，新增 Downloading 会话。
}

#[gpui::test]
fn resolve_conflict_overwrite_uploads(cx: &mut gpui::TestAppContext) {
    // RemoteConflict 会话 + Overwrite → 发出 StartTransfer(Upload)，phase 变 UploadingBack。
}

#[gpui::test]
fn resolve_conflict_later_returns_to_editing(cx: &mut gpui::TestAppContext) {
    // RemoteConflict + Later → phase 回 Editing，不发命令。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app confirm_large_edit_starts_download resolve_conflict_overwrite_uploads resolve_conflict_later_returns_to_editing`
Expected: FAIL，`no method confirm_large_edit`。

- [ ] **Step 3: 实现大文件确认处理**

在 `remote_edit.rs` 的 `impl Workspace` 内追加：

```rust
pub(crate) fn confirm_large_edit(&mut self, cx: &mut Context<Self>) {
    if let Some(pending) = self.large_edit_confirm.take() {
        self.start_edit_download(pending, cx);
    }
}

pub(crate) fn cancel_large_edit(&mut self, cx: &mut Context<Self>) {
    self.large_edit_confirm = None;
    cx.notify();
}
```

- [ ] **Step 4: 实现冲突解决**

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConflictChoice { Overwrite, DiscardLocal, Later }

impl Workspace {
    pub(crate) fn resolve_edit_conflict(&mut self, id: EditSessionId, choice: ConflictChoice, cx: &mut Context<Self>) {
        let Some(session) = self.resources().edit_sessions.get(id).cloned() else { return; };
        match choice {
            ConflictChoice::Overwrite => {
                let command = crate::edit_watcher::build_upload_command(
                    &session.local_temp_path, &session.remote_path,
                    session.session_epoch, session.profile_id, session.tab_id,
                );
                if let Some(s) = self.resources_mut().edit_sessions.get_mut(id) {
                    s.phase = EditPhase::UploadingBack;
                }
                self.send_command(command, cx);
            }
            ConflictChoice::DiscardLocal => {
                // 重新下载远程覆盖本地 temp；会话回 Downloading。
                let pending = PendingEdit {
                    remote_path: session.remote_path.clone(),
                    size: session.remote_snapshot.size,
                    modified_at: session.remote_snapshot.modified_at,
                    session_epoch: session.session_epoch,
                    profile_id: session.profile_id,
                    tab_id: session.tab_id,
                };
                self.resources_mut().edit_sessions.remove(id);
                self.start_edit_download(pending, cx);
            }
            ConflictChoice::Later => {
                if let Some(s) = self.resources_mut().edit_sessions.get_mut(id) {
                    s.phase = EditPhase::Editing;
                }
            }
        }
        cx.notify();
    }
}
```

- [ ] **Step 5: 渲染两个模态**

仿 `render_delete_confirm_modal`（file_ops.rs:455-610）实现 `render_large_edit_modal` 和 `render_edit_conflict_modal`：文案分别为 "This file is large (<size>). Download for editing?"（确认/取消）和 "Remote file changed since you started editing"（Overwrite remote / Discard local changes / Later 三按钮）。`render_edit_conflict_modal` 遍历 `self.resources().edit_sessions` 找 `phase == RemoteConflict && tab_id == 当前活跃 tab` 的第一个会话来渲染。在 `mod.rs` 的 render 链 `.children(self.render_delete_confirm_modal(cx))` 附近加两行 `.children(...)`。

> 具体元素 DSL（`div()`/`.child()`/按钮 on_click 用 `cx.listener`）逐字照抄 `render_delete_confirm_modal` 的结构，仅替换文案与回调。

- [ ] **Step 6: 跑测试 + 构建**

Run: `cargo test -p macsftp-app confirm_large_edit resolve_conflict && cargo build -p macsftp-app`
Expected: PASS + 构建成功

- [ ] **Step 7: 提交**

```bash
git add crates/app/src/workspace/remote_edit.rs crates/app/src/workspace/mod.rs
git commit -m "feat(app): add large-file and remote-conflict edit modals"
```

---
### Task 12: UI 入口 — 右键菜单/回车 + 设置项（app）

**背景**：三个入口触发编辑：①远程文件右键 "Edit"；②远程 pane 选中文件按回车/双击时，若是
文件（非目录）走编辑（`open_entry_at` 当前对文件不做事，见 panes.rs:322-333）；③设置里
配置 `external_editor`。`request_edit_selection` 把当前选中的远程 entry 解析成
`(RemotePath, size, modified_at)`（仿 `download_selection`，从 `tab.remote.entries`
按选中路径取 size/mtime），再调 Task 8 的 `begin_edit`。

**Files:**
- Modify: `crates/app/src/workspace/file_ops.rs`（`render_context_menu` 加 "Edit" 项）
- Modify: `crates/app/src/workspace/panes.rs`（`open_entry_at` 远程文件分支 → `request_edit_selection`）
- Modify: `crates/app/src/workspace/remote_edit.rs`（`request_edit_selection`）
- Modify: `crates/app/src/workspace/settings_render.rs`（external editor 文本输入）

**Interfaces:**
- Consumes: Task 8 `begin_edit(remote_path, size, modified_at, cx)`；现有 `download_selection` 的选择解析模式；Task 2 config `external_editor`/`set_external_editor`
- Produces: `pub(crate) fn Workspace::request_edit_selection(&mut self, cx: &mut Context<Self>)`

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/workspace/tests.rs`：

```rust
#[gpui::test]
fn request_edit_on_remote_file_starts_edit(cx: &mut gpui::TestAppContext) {
    // 选中一个远程文件（tab.remote.entries 里有 size 小于阈值的 file），
    //   request_edit_selection → 直接新增 Downloading 会话（未过大文件确认）。
}

#[gpui::test]
fn request_edit_on_remote_directory_is_noop(cx: &mut gpui::TestAppContext) {
    // 选中远程目录 → 不新增会话、不发命令。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p macsftp-app request_edit_on_remote_file_starts_edit request_edit_on_remote_directory_is_noop`
Expected: FAIL，`no method request_edit_selection`。

- [ ] **Step 3: 实现 request_edit_selection**

在 `remote_edit.rs`：

```rust
pub(crate) fn request_edit_selection(&mut self, cx: &mut Context<Self>) {
    let Some(tab) = self.active_tab() else { return; };
    // 取第一个选中的远程文件（非目录）。
    let selected: Option<(RemotePath, Option<u64>, Option<Timestamp>)> = tab.selection
        .selected_paths.iter().find_map(|path| match path {
            EntryPath::Remote(p) => tab.remote.entries.iter()
                .find(|e| &e.path == p && e.kind != FileKind::Directory)
                .map(|e| (e.path.clone(), e.size, e.modified_at)),
            EntryPath::Local(_) => None,
        });
    let Some((remote_path, size, modified_at)) = selected else {
        self.status_message = Some("Select one remote file to edit".into());
        cx.notify();
        return;
    };
    self.begin_edit(remote_path, size, modified_at, cx);
}
```

- [ ] **Step 4: 右键菜单 "Edit" 项**

在 `render_context_menu`（file_ops.rs:690 的 `has_entry && !is_local && connected` 块内，Download 之前）加：

```rust
items.push(
    text_button("ctx-edit", "Edit")
        .on_click(cx.listener(|workspace, _event, _window, cx| {
            workspace.context_menu = None;
            workspace.focused_side = PaneSide::Remote;
            workspace.request_edit_selection(cx);
        }))
        .into_any_element(),
);
```

- [ ] **Step 5: 回车/双击打开编辑**

在 `panes.rs` 的 `open_entry_at` 远程分支（panes.rs:322-332），把当前"仅目录导航"扩展为文件走编辑：

```rust
PaneSide::Remote => {
    let Some(entry) = tab.remote.entries.get(real_index) else { return; };
    if entry.kind == FileKind::Directory {
        let path = entry.path.clone();
        self.navigate_pane_remote(path, HistoryOp::Push, cx);
    } else {
        let (path, size, modified_at) = (entry.path.clone(), entry.size, entry.modified_at);
        self.begin_edit(path, size, modified_at, cx);
    }
}
```

> 注意：原分支借用 `tab` 后调 `self.navigate_*`，需先 clone 出需要的值再调 `self.begin_edit`，避免可变借用冲突（与文件其余处理同一模式）。

- [ ] **Step 6: 设置项 external_editor**

在 `settings_render.rs`，仿现有 `show_hidden_files`/`confirm_delete` 的行，加一个文本输入行 "External editor (leave blank for system default)"，绑定读 `config().external_editor`，提交时调 `set_external_editor(Some(text))`（空串存 `None`）。具体输入控件复用该文件里已有的文本输入模式。

- [ ] **Step 7: 跑测试 + 构建 + clippy**

Run: `cargo test -p macsftp-app request_edit && cargo build -p macsftp-app && cargo clippy -p macsftp-app --all-targets`
Expected: PASS + 构建成功 + 无 clippy 警告

- [ ] **Step 8: 提交**

```bash
git add crates/app/src/workspace/file_ops.rs crates/app/src/workspace/panes.rs crates/app/src/workspace/remote_edit.rs crates/app/src/workspace/settings_render.rs
git commit -m "feat(app): wire Edit action into context menu, enter key, and settings"
```

---
### Task 13: 退出清理 + 端到端验证（app）

**背景**：应用级会话在退出时统一清理临时文件。`main.rs:179` 的 `cx.on_app_quit` 里已有
`checkpoint_before_quit(cx)`；在其中追加清理 `edits_dir` 下所有临时文件。启动时的兜底清理已
在 Task 4 的 `EditSessionStore::open`（清空 `edits_dir`）覆盖——因为编辑会话是纯内存态
（不像 `residual_temps` 持久化），进程重启后没有活跃会话，直接清空整个 `edits_dir` 即可，
无需持久化 reconcile。

**Files:**
- Modify: `crates/app/src/main.rs`（`on_app_quit` 内追加 `cleanup_edit_temps`）
- Modify: `crates/app/src/resources.rs`（`AppResources::load` 里构造 `EditSessionStore`，Task 7 已加；此处确认启动清空）

**Interfaces:**
- Consumes: Task 4 `edits_dir`；Task 3/7 `EditSessionStore`
- Produces: `fn cleanup_edit_temps(cx: &App)`（删除 `edits_dir` 全部内容）

- [ ] **Step 1: 写失败测试**

在 `crates/app/src/resources.rs` 的 `mod tests`（复用 `test_app_paths`）：

```rust
#[test]
fn edit_store_open_clears_stale_temps() {
    let paths = test_app_paths();
    std::fs::create_dir_all(&paths.edits_dir).unwrap();
    let stale = paths.edits_dir.join("stale-session.txt");
    std::fs::write(&stale, b"leftover").unwrap();
    let _store = EditSessionStore::open(paths.edits_dir.clone());
    assert!(!stale.exists(), "启动时应清空 edits_dir 残留");
}
```

> 若 Task 3 的 `EditSessionStore::open` 已含清空逻辑并已有等价测试，则跳过重复，直接进 Step 3 做端到端。

- [ ] **Step 2: 跑测试确认失败/通过**

Run: `cargo test -p macsftp-app edit_store_open_clears_stale_temps`
Expected: 若 Task 3 已实现清空则 PASS（幂等确认）；否则 FAIL 后在 Task 3 的 `open` 补清空。

- [ ] **Step 3: 实现退出清理**

在 `main.rs` 顶部 helper 区加：

```rust
fn cleanup_edit_temps(cx: &gpui::App) {
    if !cx.has_global::<AppResources>() { return; }
    let dir = cx.global::<AppResources>().app_paths.edits_dir.clone();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}
```

在 `cx.on_app_quit` 闭包里 `checkpoint_before_quit(cx);` 之后加 `cleanup_edit_temps(cx);`。

- [ ] **Step 4: 全量测试 + 构建 + clippy**

Run: `cargo test -p macsftp-app -p macsftp-core -p macsftp-platform && cargo build && cargo clippy --all-targets`
Expected: 全绿，无警告

- [ ] **Step 5: cargo-deny 审查（依赖纪律）**

Run: `cargo deny check`
Expected: PASS。本计划**未引入任何新依赖**（监视用轮询 + `std::fs`，无 `notify`），故应无新增 advisory/license 问题。若失败，检查是否误加依赖。

- [ ] **Step 6: 手动端到端验证**

Run: `cargo run`
手动走查（无法自动化 GUI，必须人工确认）：
1. 连接一个 SFTP 服务器，远程 pane 选中一个小文本文件 → 右键 "Edit"（或按回车）。
2. 确认文件下载到 `~/Library/Application Support/macSFTP/edits/` 并用默认应用打开。
3. 在编辑器里改动并保存 → ~1s 内传输 drawer 出现一条回传记录，远程文件更新。
4. 设置里填一个自定义编辑器路径 → 再次 Edit，确认用该编辑器打开。
5. 编辑一个大文件（超阈值）→ 确认弹出大文件确认框。
6. 制造冲突：编辑中在服务器端用别的方式改动同一文件并刷新远程列表 → 保存本地 → 确认弹出冲突框，三个选项行为正确。
7. 退出 app → 确认 `edits/` 目录被清空。

> 这是 GUI 功能，类型检查与单测只验证代码正确性，不验证功能正确性。若无法接入真实 SFTP 服务器，明确说明"未能端到端验证第 X 步"，不要声称通过。

- [ ] **Step 7: 提交**

```bash
git add crates/app/src/main.rs crates/app/src/resources.rs
git commit -m "feat(app): clean up edit temp files on quit and startup"
```

---

## 自查（写完计划后对照 spec）

**Spec 覆盖**：
- Edit 模式（下载→外部编辑→自动回传）→ Task 8/9/10 ✅
- 系统默认应用 + 可配置自定义编辑器 → Task 5/12 ✅
- FSEvents 自动监视保存即回传 → **改为轮询**（方案 B，spec 已批准）Task 10 ✅
- 应用级会话生命周期，退出清理 → Task 3/7/13 ✅
- 回传前冲突检测并警告 → Task 6/10/11 ✅
- 不限文件类型，大文件弹确认 → Task 8/11 ✅

**占位符扫描**：无 TBD/TODO；每个代码步骤含实际代码。少数 render DSL 步骤指向"逐字照抄
`render_delete_confirm_modal`"——因该函数 150 行且为纯样板，指明精确行号（file_ops.rs:455-610）
比重抄更可靠，符合"跟随既有模式"。

**类型一致性**：`EditSession`/`EditPhase`/`EditSessionStore`/`RemoteSnapshot`/`PendingEdit`/
`ConflictChoice` 跨任务命名一致；`build_upload_command` 在 Task 10 定义、Task 11 复用；
`begin_edit`/`start_edit_download`/`request_edit_selection` 签名跨 Task 8/12 一致。














