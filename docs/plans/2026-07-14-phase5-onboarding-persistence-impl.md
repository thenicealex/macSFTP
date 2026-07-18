# Phase 5 Onboarding & Persistence Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist workspace session layout across quit/relaunch (without auto-connect), track successful connections in an independent recents list, surface recents + Connect on the empty remote pane, and keep the window title in sync with the active tab.

**Architecture:** Two new versioned JSON stores in `macsftp_storage` (`session.json`, `recents.json`) behind `AppPaths`, mounted on process-global `AppResources`. First window restores session layout into `TabState` with `ConnectionState::Empty` and never sends `ConnectTab`. Successful `TabConnected` upserts recents. Empty remote pane lists recents as next-step actions. Window title updates via `Window::set_window_title`.

**Tech Stack:** Rust, GPUI (`on_app_quit`, `Window::set_window_title`), serde JSON, existing `ProfileStore` / atomic tmp+rename pattern, `AppResources` globals.

**Spec:** `docs/plans/2026-07-14-phase5-onboarding-persistence-design.md`

## Global Constraints

- **Layout restore ≠ auto-connect** — restored tabs stay `Empty`/`Disconnected`; no `AppCommand::ConnectTab` on startup.
- **No secrets** in `session.json` or `recents.json` — only `profile_id` + host/port/username/path metadata.
- **Single global session file** — one `session.json`; first window restores; later windows open a blank new tab.
- **Recents cap 20**; dedupe key `(host, port, username, profile_id)`; write only on successful connect.
- **No marketing empty state** — "Not connected" + Connect… + optional recents list only.
- **MVP session fields only:** title, profile_id, host, port, username, local_path, remote_path, active_tab_index — no filter/MRU/sort/drawer.
- Atomic write: temp file + rename (same as `ProfilesFile` / `TransferHistoryFile`).
- Corrupt / unsupported version → empty fallback + WARN; never block startup.
- No `unwrap`/`expect` on recoverable paths (AGENTS.md §5). Prefer `src/foo.rs` over `mod.rs`.
- Do **not** change SFTP protocol, Keychain, or transfer restore.

## File Map

| File | Responsibility |
| --- | --- |
| **Modify** `crates/platform/src/platform.rs` | `session_file`, `recents_file` on `AppPaths`; include in `ensure_directories` + path tests |
| **Create** `crates/storage/src/session.rs` | `SessionFile`, `SessionTabSnapshot`, `SessionStore` load/save |
| **Create** `crates/storage/src/recents.rs` | `RecentsFile`, `RecentEntry`, `RecentsStore` load/save/upsert |
| **Modify** `crates/storage/src/storage.rs` | `mod session` / `mod recents`; re-export |
| **Modify** `crates/app/src/resources.rs` | `session: SessionStore`, `recents: RecentsStore` on `AppResources` |
| **Modify** `crates/app/src/main.rs` | Pass `restore_session` into first window only |
| **Modify** `crates/app/src/workspace/mod.rs` | `restore_session` flag; restore tabs; quit flush; title helper; restored meta |
| **Modify** `crates/app/src/workspace/event_handling.rs` | `TabConnected` → recents upsert; prefer restored remote path |
| **Modify** `crates/app/src/workspace/connect_form.rs` | Prefill from restored meta / open from recent |
| **Modify** `crates/app/src/workspace/render.rs` | Empty/Disconnected empty-state + recents rows |
| **Modify** `crates/app/src/workspace/tests.rs` | Restore / recents / title / no-connect tests |
| **Do not modify** | `crates/sftp/**` (except if a test fixture needs a path — avoid), core transfer/session protocols |

## Suggested PR mapping (optional when stacking)

| PR | Tasks | Notes |
| --- | --- | --- |
| PR1 | Task 1–3 | Paths + SessionStore + restore + quit save |
| PR2 | Task 4–5 | RecentsStore + TabConnected + empty state |
| PR3 | Task 6 | Window title |
| PR4 | Task 7 | Recent click → prefill/connect polish |

Tasks below are ordered for a single sequential implementation; PR2 can start after Task 1 if parallelized carefully.

---

### Task 1: AppPaths — `session_file` / `recents_file`

**Files:**
- Modify: `crates/platform/src/platform.rs`
- Test: same file `#[cfg(test)]` module (`builds_expected_macos_app_paths`)

**Interfaces:**
- Produces:
  - `AppPaths.session_file: LocalPath` → `{app_support}/session.json`
  - `AppPaths.recents_file: LocalPath` → `{app_support}/recents.json`
  - Both included in `ensure_directories` parent creation list

- [ ] **Step 1: Extend the existing path unit test (fail first if fields missing)**

In `builds_expected_macos_app_paths`, add:

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

- [ ] **Step 2: Run test — expect FAIL (unknown fields)**

```bash
cargo test -p macsftp-platform builds_expected_macos_app_paths -- --nocapture
```

- [ ] **Step 3: Implement fields**

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

- [ ] **Step 4: Re-run test — PASS**

```bash
cargo test -p macsftp-platform builds_expected_macos_app_paths -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/platform/src/platform.rs
git commit -m "feat(platform): add session.json and recents.json AppPaths"
```

---

### Task 2: SessionStore (storage)

**Files:**
- Create: `crates/storage/src/session.rs`
- Modify: `crates/storage/src/storage.rs` (mod + re-export)
- Test: unit tests inside `session.rs`

**Interfaces:**
- Produces:
  - `SessionTabSnapshot { title, profile_id: Option<u64>, host, port, username, local_path: Option<String>, remote_path: Option<String> }`
  - `SessionFile { version: u32, active_tab_index: usize, tabs: Vec<SessionTabSnapshot> }` with `CURRENT_VERSION = 1`
  - `SessionStore { path, file }` with:
    - `open(path) -> Result<Self, StorageError>`
    - `open_or_empty(path) -> Self` (missing/corrupt/unsupported version → empty)
    - `file(&self) -> &SessionFile`
    - `replace(&mut self, file: SessionFile)`
    - `save(&self) -> Result<(), StorageError>` atomic tmp+rename
  - Re-export: `pub use session::{SessionFile, SessionStore, SessionTabSnapshot};`

**Rules (implement exactly):**
- Missing file → empty session (`tabs: []`, `active_tab_index: 0`)
- Parse error / `version > CURRENT_VERSION` → `open_or_empty` returns empty (do not delete file)
- JSON must **not** include password / passphrase / key material fields on `SessionTabSnapshot`

- [ ] **Step 1: Write failing unit tests** in `session.rs`

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

- [ ] **Step 2: Run — expect FAIL (module missing)**

```bash
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **Step 3: Implement `session.rs`**

Mirror `transfer_history.rs` / `ProfilesFile` patterns:

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

Wire in `storage.rs`:

```rust
pub mod session;
pub use session::{SessionFile, SessionStore, SessionTabSnapshot};
```

- [ ] **Step 4: Run tests — PASS**

```bash
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/storage/src/session.rs crates/storage/src/storage.rs
git commit -m "feat(storage): add SessionStore for session.json"
```

---

### Task 3: Restore session on first window + flush on quit

**Files:**
- Modify: `crates/app/src/resources.rs`
- Modify: `crates/app/src/main.rs` (`open_workspace_window`, `Workspace::new` call)
- Modify: `crates/app/src/workspace/mod.rs` (`Workspace::new` signature, restore, flush, `build_session_snapshot`)
- Modify: `crates/app/src/workspace/event_handling.rs` (prefer restored remote path on `TabConnected`)
- Modify: `crates/app/src/workspace/connect_form.rs` (prefill from restored meta when opening form)
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:
  - `AppResources.session: SessionStore`
  - `Workspace::new(..., restore_session: bool, ...)`
  - `Workspace.session_flushed: bool` (guard like transfer history)
  - `Workspace.restored_targets: HashMap<TabId, RestoredTabTarget>` where:

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
  - `fn restore_session_tabs(&mut self, window, cx)` — only when `restore_session && !file.tabs.is_empty()`
  - First window: `restore_session = true`; Cmd+N windows: `false` → `open_new_tab` only

**Restore rules:**
1. For each snapshot: `next_tab_id()`, `TabState::new(id, title)`, set `profile_id`, `local.path` + `load_local_directory`, `remote.path` from snapshot (entries empty), `connection = Empty`.
2. Store `RestoredTabTarget` for form prefill / post-connect navigation.
3. Clamp `active_tab_index` to `tabs.len()-1`; set `active_tab_id`; seed `tab_mru` in tab order then touch active.
4. **Do not** call `send_command(ConnectTab)`.
5. Empty session → existing `open_new_tab` path.

**TabConnected path preference (critical for restored remote path):**

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

Simpler equivalent used in implementation:

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

**Quit flush:**

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

**First window flag in `main.rs`:**

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

**`Workspace::new` change:**

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

**Connect form prefill from restored meta:**

In `open_connect_form`, if no `tab_settings`, build a temporary non-secret prefill:

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

**`resources.rs`:**

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

> **Note:** If implementing PR-style, add a temporary `// recents in Task 4` only after Task 4 exists. Prefer adding both stores in resources when Task 4 lands; for Task 3 alone, add only `session`.

Update all `Workspace::new(...)` call sites (tests + main) with `restore_session: false` by default in tests unless a test opts in.

- [ ] **Step 1: Failing app tests**

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

Helper pattern for restore tests (sketch):

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

- [ ] **Step 2: Run — expect FAIL**

```bash
cargo test -p macsftp-app --bin macsftp restore_session build_session_snapshot flush_session -- --nocapture
```

- [ ] **Step 3: Implement restore + flush + resources.session + main flag + TabConnected path preference**

- [ ] **Step 4: Run tests — PASS**

```bash
cargo test -p macsftp-app --bin macsftp restore_session build_session_snapshot flush_session -- --nocapture
cargo test -p macsftp-storage session_ -- --nocapture
```

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/resources.rs crates/app/src/main.rs \
  crates/app/src/workspace/mod.rs crates/app/src/workspace/event_handling.rs \
  crates/app/src/workspace/connect_form.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): restore session layout on launch and save on quit"
```

---

### Task 4: RecentsStore (storage)

**Files:**
- Create: `crates/storage/src/recents.rs`
- Modify: `crates/storage/src/storage.rs`
- Modify: `crates/app/src/resources.rs` (mount `recents`)
- Test: unit tests in `recents.rs`

**Interfaces:**
- Produces:

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

Dedupe equality: `(host, port, username, profile_id)` all match → update fields, set `last_connected_at`, move to index 0. Else push front with new `id`, truncate to 20.

- [ ] **Step 1: Failing tests**

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

- [ ] **Step 2: Run — FAIL**

```bash
cargo test -p macsftp-storage upsert_ caps_at corrupt_recents serialized_recents -- --nocapture
```

- [ ] **Step 3: Implement + export + `AppResources.recents`**

```rust
// resources load:
let recents = RecentsStore::open_or_empty(app_paths.recents_file.clone());
```

- [ ] **Step 4: PASS + commit**

```bash
cargo test -p macsftp-storage -- --nocapture
git add crates/storage/src/recents.rs crates/storage/src/storage.rs crates/app/src/resources.rs
git commit -m "feat(storage): add RecentsStore for recents.json"
```

---

### Task 5: TabConnected → recents + empty-state list UI

**Files:**
- Modify: `crates/app/src/workspace/event_handling.rs`
- Modify: `crates/app/src/workspace/render.rs`
- Modify: `crates/app/src/workspace/mod.rs` (helper `record_recent_connection` if cleaner)
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:
  - `Workspace::record_recent_for_tab(&mut self, tab_id: TabId, cx: &mut Context<Self>)`
  - Empty-state for `ConnectionState::Empty` and `Disconnected` includes:
    - primary Connect / Reconnect button (existing)
    - if `cx.resources().recents.entries()` non-empty: a vertical list under label `"Recent connections"`
    - each row: `display_name · user@host:port` (if no display_name: `user@host:port`)
    - click → `open_recent_connection(entry_id, window, cx)` (implement fully in Task 7; Task 5 may open form prefilled only)

**TabConnected hook (after complete_connect / directory request):**

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

**Empty state UI sketch (`render.rs` for Empty):**

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

For `Disconnected`, keep Reconnect + Edit Connection; still show the same recents list below.

**No marketing copy** — do not add "Welcome to macSFTP".

- [ ] **Step 1: Failing tests**

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

- [ ] **Step 2: FAIL → implement → PASS**

```bash
cargo test -p macsftp-app --bin macsftp tab_connected_upserts tab_connected_dedupes -- --nocapture
```

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/workspace/event_handling.rs crates/app/src/workspace/render.rs \
  crates/app/src/workspace/mod.rs crates/app/src/workspace/tests.rs
git commit -m "feat(app): record recents on connect and show them in empty remote pane"
```

---

### Task 6: Window title tracks active tab

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` (`update_window_title`, call sites)
- Optionally: `event_handling.rs` (connect complete / disconnect / title change)
- Test: `crates/app/src/workspace/tests.rs` using `VisualTestContext` / window title API

**Interfaces:**
- Produces:

```rust
pub(crate) fn update_window_title(&self, window: &mut Window) {
    let title = match self.active_tab() {
        Some(tab) if !tab.title.is_empty() => format!("{} — macSFTP", tab.title),
        _ => "macSFTP".to_string(),
    };
    window.set_window_title(&title);
}
```

Call after: `open_new_tab`, `close_tab_by_id`, `activate_tab`, `connect_with` (title set to host), `TabConnected` / fail / disconnect handlers if title changes, and once at end of `Workspace::new` / restore.

GPUI API: `window.set_window_title(&str)` (`gpui` 0.2.2). Tests can use `TestAppContext` window title reader if available (`window_title()` on test context).

- [ ] **Step 1: Failing test**

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

If reading title from GPUI test is awkward, extract:

```rust
pub(crate) fn window_title_for_active_tab(tab_title: Option<&str>) -> String {
    match tab_title {
        Some(t) if !t.is_empty() => format!("{t} — macSFTP"),
        _ => "macSFTP".to_string(),
    }
}
```

Unit-test that helper; still call `set_window_title` at the production call sites.

- [ ] **Step 2–4: implement, pass, commit**

```bash
cargo test -p macsftp-app --bin macsftp window_title -- --nocapture
git add crates/app/src/workspace/mod.rs crates/app/src/workspace/event_handling.rs \
  crates/app/src/workspace/tests.rs
git commit -m "feat(app): set window title from active tab"
```

---

### Task 7: Open recent — prefill / profile connect path

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` or `connect_form.rs`
- Modify: `crates/app/src/workspace/render.rs` (wire already from Task 5)
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:

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

- [ ] **Step 1: Failing tests**

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

- [ ] **Step 2–4: implement, pass, commit**

```bash
cargo test -p macsftp-app --bin macsftp open_recent -- --nocapture
git add crates/app/src/workspace/
git commit -m "feat(app): connect from recent entries with profile or form prefill"
```

---

### Task 8: Full regression + manual checklist

**Files:** none new (docs only if `docs/gpui-russh-plan.md` needs a one-line session note — optional; only update if you already touch architecture docs)

- [ ] **Step 1: Run focused + broader tests**

```bash
cargo test -p macsftp-platform -- --nocapture
cargo test -p macsftp-storage -- --nocapture
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

Expected: all PASS (or only pre-existing unrelated failures — do not leave new failures).

- [ ] **Step 2: Manual smoke (human or local run)**

1. Connect to a real/mock host, open 2 tabs, change local/remote paths → Quit.
2. Relaunch → tabs restored, titles/paths present, **not** auto-connected.
3. Reconnect succeeds; remote lands on restored path when possible.
4. Empty remote pane shows recents; click opens form or connects.
5. Window title shows `{tab} — macSFTP` and updates on tab switch.
6. Inspect `~/Library/Application Support/macSFTP/session.json` and `recents.json` — no password fields.

- [ ] **Step 3: Final commit only if polish leftovers exist**

```bash
git status
# commit only intentional leftovers
```

---

## Self-Review (plan vs design)

| Design requirement | Task |
| --- | --- |
| Session silent save on quit | Task 3 |
| Startup restore layout, no auto Connect | Task 3 |
| session.json schema + version/corrupt fallback | Task 2 |
| AppPaths session/recents | Task 1 |
| Recents independent + profile_id + cap 20 | Task 4–5 |
| TabConnected writes recents | Task 5 |
| Empty state Connect + recents, no marketing | Task 5 |
| Recent click prefill / profile connect | Task 7 |
| Window title active tab | Task 6 |
| Multi-window single session file, first window restore | Task 3 (`restore_session` flag) |
| No secrets in JSON | Task 2/4 tests |
| Restored remote path after connect | Task 3 TabConnected preference + Task 7 |
| PR split PR1–4 | Header mapping |

**Placeholder scan:** no TBD/TODO steps; concrete types and commands included.

**Type consistency:** `SessionTabSnapshot.profile_id: Option<u64>` ↔ `ProfileId(u64)`; `RecentEntry.id: u64`; `RestoredTabTarget` shared by Tasks 3/5/7; `Workspace::new(..., restore_session: bool, ...)` updated in main + tests.

---

## Execution Handoff

Plan saved to `docs/plans/2026-07-14-phase5-onboarding-persistence-impl.md` (project convention; same directory as design).

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks (`superpowers:subagent-driven-development`)
2. **Inline Execution** — this session with `superpowers:executing-plans` and checkpoints

Which approach?
