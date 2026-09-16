# Profile Management UI 实施计划

> **Agent 执行要求：** 必须使用 superpowers:subagent-driven-development（推荐）或 superpowers:executing-plans，并按任务实施本计划。各步骤使用 checkbox（`- [ ]`）记录状态。

**目标：** 在 Settings → Profiles 中增加由 Keychain 支持的连接 profile 列表、查询、新建、编辑和删除功能。Connect form 仍然负责连接 active tab，并保留 Use / Save。

**架构：** `Workspace` 保存 view state，包括 `settings_section`、filter、selection、`ProfileEditorState` 和 delete confirm。UI 位于 Settings surface，并沿用现有 sidebar 模式。持久化使用现有 `ProfileStore` 和 `KeychainStore`。Connect form 的删除操作使用与 Settings 相同的确认 modal。

**技术栈：** Rust、GPUI Settings surface、`InputState`、`ProfileStore` / `KeychainStore`、`ConnectionProfile` / `AuthMethod` / `SecretRef`。

**设计文档：** `docs/plans/2026-07-14-profile-management-design.md`

## 全局约束

- **Profile 管理与连接职责不同。** Settings 管理 profile 库，而 Connect 连接 active tab。因此，MVP 不在 Settings 中提供一键 Connect。
- **编辑时以空 password/passphrase 执行 Save**表示保留 Keychain secret，因此不得清除 secret，也不得用空值覆盖 secret。
- **新建 password profile** 时 password 必须非空；新建 key profile 时 key path 必须非空。
- **Settings 和 Connect 中的 Delete 均必须确认。**
- status detail、日志和现有 profile 的 editor 预填内容均不得包含 secret。
- **Recents 保持独立，**因此不并入 Profiles UI。
- 不提供 `group_id` UI、import/export 或 drag sort。
- 可恢复路径不得使用 `unwrap`，而且不得修改 sftp。
- editor helper 应优先位于 `crates/app/src/workspace/profiles.rs`，不得使用 `mod.rs` 路径。
- 修改范围必须精确，并与现有 Settings / Connect form 样式一致。

## 文件职责

| 文件 | 职责 |
| --- | --- |
| **新建** `crates/app/src/workspace/profiles.rs` | `SettingsSection`、`ProfileEditorState`、filter helper，以及 editor 的初始化、保存和删除 API |
| **修改** `crates/app/src/workspace/mod.rs` | 字段、`mod profiles` 和 Settings 显示时的初始化 |
| **修改** `crates/app/src/workspace/render.rs` | `render_settings` sidebar 和 Profiles pane |
| **修改** `crates/app/src/workspace/modals.rs` | profile 删除确认 modal；Connect delete 改为请求确认 |
| **修改** `crates/app/src/workspace/connect_form.rs` | 可选：共享 validation；delete 控件仅请求确认 |
| **修改** `crates/app/src/main.rs` | 必要时增加 `mod` |
| **修改** `crates/app/src/app_actions.rs` 和 `palette_commands.rs` | 可选的 `OpenProfiles`，对应 Task 5 |
| **修改** `crates/app/src/workspace/tests.rs` | GPUI 和 store 测试 |
| **不得修改** | `crates/sftp/**`、recents schema |

## 建议的 PR 划分

| PR | Tasks |
| --- | --- |
| PR1 | Task 1 |
| PR2 | Task 2 |
| PR3 | Task 3 |
| PR4 | Task 4–5 |

---

### Task 1：Settings section 和 Profiles 只读列表

**文件：**
- 新建：`crates/app/src/workspace/profiles.rs`
- 修改：`crates/app/src/workspace/mod.rs`
- 修改：`crates/app/src/workspace/render.rs` 中的 `render_settings`
- 修改：workspace module tree，在 `mod.rs` 或 parent 中增加 `mod profiles`
- 测试：`crates/app/src/workspace/tests.rs`

**接口：**
本任务提供以下类型、字段和方法：

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsSection {
    #[default]
    General,
    Profiles,
}

// Workspace fields:
settings_section: SettingsSection,
profile_filter: String, // InputState optional; String + InputState for filter box
selected_profile_id: Option<ProfileId>,
// profile_editor: None until Task 2 — for Task 1, selection only

pub(crate) fn set_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>);
pub(crate) fn select_profile_in_settings(&mut self, id: ProfileId, cx: &mut Context<Self>);
pub(crate) fn filtered_profiles<'a>(&self, profiles: &'a [ConnectionProfile]) -> Vec<&'a ConnectionProfile>;
// Task 1 的 filter 可以仅支持空字符串，即始终显示全部；可以先显示尚未关联逻辑的 filter input，也可以在 Task 1 中提前实现过滤
```

**Filter helper：** Task 1 实现纯函数。但是，如果 Task 4 更适合处理 UI 关联，则可以在 Task 4 中将该函数用于 UI。

```rust
pub(crate) fn profile_matches_filter(profile: &ConnectionProfile, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    profile.name.to_lowercase().contains(&q)
        || profile.host.to_lowercase().contains(&q)
        || profile.username.to_lowercase().contains(&q)
}
```

- [ ] **Step 1：增加当前会失败的测试**

```rust
#[gpui::test]
fn settings_profiles_section_lists_saved_profiles(cx: &mut TestAppContext) {
    let (workspace, mut cx, _ch) = init_workspace(cx);
    // 通过现有 connect_form 路径或 resources_mut().profiles.save_profile 保存一个 profile
    workspace.update_in(&mut cx, |ws, window, cx| {
        // 使用测试中的 memory keychain 将 profile 写入 store
        ws.surface = WorkspaceSurface::Settings;
        ws.set_settings_section(SettingsSection::Profiles, cx);
        assert_eq!(ws.settings_section, SettingsSection::Profiles);
        let n = cx.resources().profiles.profiles().len();
        assert!(n >= 1);
        // 进入非空 profiles 列表时默认选择第一项
        assert!(ws.selected_profile_id.is_some());
    });
}

#[test]
fn profile_matches_filter_name_host_user() {
    // 构造 ConnectionProfile；断言其匹配 "work"、"example" 和 "alex"
}
```

- [ ] **Step 2：执行测试，并确认测试失败**

```bash
cargo test -p macsftp-app --bin macsftp settings_profiles_section profile_matches_filter -- --nocapture
```

- [ ] **Step 3：实现功能**

`profiles.rs`:

```rust
use macsftp_core::{ConnectionProfile, ProfileId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SettingsSection {
    #[default]
    General,
    Profiles,
}

pub(crate) fn profile_matches_filter(profile: &ConnectionProfile, query: &str) -> bool {
    if query.trim().is_empty() {
        return true;
    }
    let q = query.to_lowercase();
    profile.name.to_lowercase().contains(&q)
        || profile.host.to_lowercase().contains(&q)
        || profile.username.to_lowercase().contains(&q)
}

pub(crate) fn profile_list_label(profile: &ConnectionProfile) -> String {
    format!(
        "{} · {}@{}:{}",
        profile.name, profile.username, profile.host, profile.port
    )
}
```

`Workspace::new` 初始化 `settings_section: General`、`profile_filter: String::new()` 或 `InputState::new()`，以及 `selected_profile_id: None`。

```rust
pub(crate) fn set_settings_section(&mut self, section: SettingsSection, cx: &mut Context<Self>) {
    self.settings_section = section;
    if section == SettingsSection::Profiles {
        let profiles = cx.resources().profiles.profiles();
        if self
            .selected_profile_id
            .is_none_or(|id| profiles.iter().all(|p| p.id != id))
        {
            self.selected_profile_id = profiles.first().map(|p| p.id);
        }
        // Task 2：同时调用 load_editor_from_selection(cx)
    }
    cx.notify();
}
```

`render_settings` 应满足以下要求：

- Sidebar 包含 General 和 Profiles 两个可点击行；选中行使用 `element_selected` 背景。
- Body 在 General 状态下显示现有 Appearance block；在 Profiles 状态下显示 split pane。
  - 左侧约 220px，包含 New Profile 按钮和 profile 列表。Task 2 之前，按钮可以不执行操作或调用 stub。
  - 右侧在 Task 1 中显示“Select a profile”占位内容，或者显示选中项的 name 和 host 只读摘要。

```rust
// Sidebar item helper
fn sidebar_item(id, label, selected, on_click) -> impl IntoElement { /* match General styling */ }
```

Open Settings action 存在保留上次 `settings_section` 和重置为 General 两种方案。但是，为了保证行为可预测，**每次显示 Settings 时均重置为 General**：

```rust
// OpenSettings handler
workspace.surface = WorkspaceSurface::Settings;
workspace.settings_section = SettingsSection::General;
```

- [ ] **Step 4：确认测试通过并提交**

```bash
cargo test -p macsftp-app --bin macsftp settings_profiles_section profile_matches_filter -- --nocapture
git add crates/app/src/workspace/
git commit -m "feat(app): add Settings Profiles section with profile list"
```

---

### Task 2：Profile editor 的新建、编辑、保存与 Keychain 语义

**文件：**
- 修改：`crates/app/src/workspace/profiles.rs`
- 修改：`crates/app/src/workspace/mod.rs`，增加 `profile_editor` 字段
- 修改：`crates/app/src/workspace/render.rs`，增加右侧 pane 的表单
- 仅在提取共享 secret 存储 helper 时修改 `crates/app/src/workspace/connect_form.rs`；优先调用现有的 `store_profile_secrets` / `next_profile_id`
- 测试：`tests.rs`

**接口：**
本任务提供以下类型和方法：

```rust
pub(crate) struct ProfileEditorState {
    pub is_new: bool,
    pub profile_id: Option<ProfileId>,
    pub name: InputState,
    pub host: InputState,
    pub port: InputState,
    pub username: InputState,
    pub auth_method: AuthMethodKind, // reuse connect_form enum or duplicate Copy enum in profiles.rs
    pub password: InputState,
    pub key_path: InputState,
    pub passphrase: InputState,
    pub default_remote_path: InputState,
    pub error: Option<SharedString>,
    pub secret_present_hint: bool,
}

impl ProfileEditorState {
    pub fn blank() -> Self; // port "22", is_new true
    pub fn from_profile(profile: &ConnectionProfile, secret_present: bool) -> Self;
    // 不预填 password
}

// Workspace methods:
pub(crate) fn start_new_profile(&mut self, cx: &mut Context<Self>);
pub(crate) fn load_profile_editor(&mut self, id: ProfileId, cx: &mut Context<Self>);
pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>);
```

**保存语义：** 必须严格按照以下规则实现。

```rust
pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>) {
    let Some(editor) = &self.profile_editor else { return };
    // 1. 校验 host/user/port，规则与 ConnectForm::build_settings 的 metadata 校验相同
    // 2. 确定 profile_id：现有值或 next_profile_id
    // 3. 处理 Auth：
    //    - Password + is_new + password 为空 → error "Password is required."
    //    - Password + !is_new + password 为空 → 保留原有 AuthMethod::Password { secret_ref }
    //    - Password + password 非空 → 写入 Keychain，然后创建包含 SecretRef 的 AuthMethod
    //    - PrivateKey + key 为空 → error
    //    - PrivateKey + 更新时 passphrase 为空 → 保留原有 passphrase_ref（如果存在）
    //    - PrivateKey + passphrase 非空 → 写入 Keychain
    // 4. 构造 ConnectionProfile；如果 name 为空，则使用 user@host；default_remote_path 使用字段值
    // 5. 调用 store 的 save_profile；成功后将 editor 更新为非新建状态，设置 secret_present_hint，并清除 error
    // 6. Keychain 失败时显示用户可见的 status_message，因此不得写入 JSON
}
```

尽量复用 `SecretRef::keychain_ref` 和 `store_profile_secrets`。但是，在保留 secret 的分支中，**不得**以空字符串调用 `store`。

Auth method UI 使用 Password / Private Key 两个按钮，并与 Connect form 一致。

- [ ] **Step 1：增加当前会失败的测试**

```rust
#[gpui::test]
fn settings_new_profile_save_persists(cx: &mut TestAppContext) {
    // 进入 profiles section，调用 start_new_profile，并通过 update 设置 editor 字段；
    // 调用 save_profile_editor，然后断言 profiles().len()==1，且 keychain.load(password ref) 为 Some
}

#[gpui::test]
fn settings_edit_host_keeps_keychain_secret_when_password_blank(cx: &mut TestAppContext) {
    // 预置 profile 和 secret，初始化 editor，仅修改 host，并在 password 为空时保存
    // 断言 host 已更新，而且 keychain 仍返回原 password
}

#[gpui::test]
fn settings_new_password_profile_requires_password(cx: &mut TestAppContext) {
    // 新建状态下填写 host/user，但 password 为空 → 保存后保留 error，且 store 仍为空
}
```

- [ ] **Step 2：确认测试失败，实现 editor UI 和保存逻辑，然后确认测试通过**

右侧 pane 使用紧凑布局，最大宽度为 560，并包含以下字段：

- Name、Host、Port、Username
- Auth toggle
- Password，或者 Key path 和 Passphrase
- Default remote path
- `secret_present_hint` 为 true 时显示提示文本
- Save 按钮；Task 3 之前禁用或隐藏 Delete 占位按钮

New Profile 按钮调用 `start_new_profile`，而列表项的 click event 调用 `load_profile_editor`。

- [ ] **Step 3：提交变更**

```bash
git add crates/app/src/workspace/
git commit -m "feat(app): edit and save profiles from Settings Profiles"
```

---

### Task 3：Settings 和 Connect 的删除确认

**文件：**
- 修改：`crates/app/src/workspace/mod.rs`，增加 `profile_delete_confirm: Option<ProfileId>`
- 修改：`crates/app/src/workspace/profiles.rs`，增加 `request_delete_profile`、`confirm_delete_profile` 和 `cancel_delete_profile`
- 修改：`crates/app/src/workspace/modals.rs`，渲染确认 modal；Connect Delete 调用 `request_delete_profile`
- 修改：`crates/app/src/workspace/render.rs`，在 editor 中增加 Delete… 按钮
- 测试：`tests.rs`

**接口：**

```rust
pub(crate) fn request_delete_profile(&mut self, id: ProfileId, cx: &mut Context<Self>) {
    self.profile_delete_confirm = Some(id);
    cx.notify();
}
pub(crate) fn confirm_delete_profile(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(id) = self.profile_delete_confirm.take() else { return };
    self.delete_profile(id, cx); // existing helper
    if self.selected_profile_id == Some(id) {
        self.selected_profile_id = None;
        self.profile_editor = None;
        // re-select first remaining if any
        self.set_settings_section(SettingsSection::Profiles, cx); // or inline reselect
    }
    self.focus_pane(...); // if needed
    cx.notify();
}
pub(crate) fn cancel_delete_profile(&mut self, cx: &mut Context<Self>) {
    self.profile_delete_confirm = None;
    cx.notify();
}
```

**Modal 文案：**

- Title: `Delete Profile?`
- Body: `Delete "{name}" ({user}@{host})? This cannot be undone.`
- Primary：`Delete`；如果 theme 提供 danger/error color，则使用该颜色
- Cancel

`cancel_active_modal` 应优先判断 `profile_delete_confirm.is_some()`；如果为 true，则取消本次删除。

Connect form 的 Delete 按钮使用以下逻辑：

```rust
// 原逻辑：workspace.delete_profile(profile_id, cx)
workspace.request_delete_profile(profile_id, cx);
```

- [ ] **Step 1：增加当前会失败的测试**

```rust
#[gpui::test]
fn delete_profile_requires_confirmation(cx: &mut TestAppContext) {
    // 预置 profile，调用 request_delete，并断言 store 中仍存在该 profile
    // 取消后仍然存在
    // 再次请求并确认后，profile 不再存在，而且 keychain 为空
}

#[gpui::test]
fn connect_form_delete_opens_confirm_not_immediate(cx: &mut TestAppContext) {
    // 进入 Connect，预置 profile，并使 click event 调用 request_delete_profile
    // 断言 profile_delete_confirm.is_some()，而且 store 中仍存在该 profile
}
```

- [ ] **Step 2：实现 modal 和事件关联，然后确认测试通过**

- [ ] **Step 3：提交变更**

```bash
git commit -m "feat(app): confirm before deleting connection profiles"
```

---

### Task 4：Filter 和空状态完善

**文件：**
- 修改：`profiles.rs` / `render.rs`
- 测试：如果 Task 1 尚未增加 filter 测试，则在本任务中增加

**接口：**
- `profile_filter: InputState`；也可以保留 String，并在 key event 中更新
- profile 库为空时显示“No saved profiles”和 New Profile
- Filter 无结果时显示“No matches”
- 列表使用 `profile_matches_filter`

- [ ] **Step 1：增加测试**

```rust
#[gpui::test]
fn settings_profile_filter_narrows_list(cx: &mut TestAppContext) {
    // 创建 host 不同的两个 profile，设置 filter，然后断言 filtered helper 的结果数量为 1
}
```

- [ ] **Step 2：在列表上方增加 UI filter 字段，并实现空状态**

- [ ] **Step 3：提交变更**

```bash
git commit -m "feat(app): filter and empty states for Settings Profiles"
```

---

### Task 5：可选的 OpenProfiles 和回归测试

**文件：**
- 修改：`app_actions.rs`，增加 `OpenProfiles` action；可以不设置 keybinding，也可以使用无冲突的 cmd-shift-,
- 修改：`palette_commands.rs`，使“Manage Profiles”进入 Settings Profiles section
- 修改：`mod.rs` 中的 action handler
- 测试：验证 palette 或 action 可以切换 section
- 执行完整测试套件

```rust
// OpenProfiles
workspace.surface = WorkspaceSurface::Settings;
workspace.set_settings_section(SettingsSection::Profiles, cx);
```

- [ ] **Step 1：测试 action 可以进入 Profiles section**

- [ ] **Step 2：实现 palette 条目 `Manage Profiles`，其关键词为 profile 和 credentials**

- [ ] **Step 3：执行完整回归测试**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
cargo test -p macsftp-storage -- --nocapture
```

- [ ] **Step 4：提交变更**

```bash
git commit -m "feat(app): open Settings Profiles from command palette"
```

如果暂缓 palette，则跳过相关代码，仅执行回归测试，并在报告中说明。但是，该功能规模较小，因此优先实现。

---

### Task 6：完成检查

- [ ] 确认 Connect Use / Save 仍能通过现有的 `connect_form_save_use_update_and_delete_profile`；由于 Delete 已增加确认流程，因此必要时更新测试。
- [ ] 确认保存流程的日志不包含 password。
- [ ] 记录手动测试结果：Settings → Profiles CRUD、Connect Use，以及 Delete 确认流程中的取消和确认行为。

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

- [ ] 仅在最终修正产生变更时提交。

---

## 通用实现说明

### AuthMethodKind

`profiles.rs` 应优先复用 `connect_form::AuthMethodKind`（`pub(crate)`），因此不要定义重复类型。

### 在没有 password 的情况下更新 `ConnectionProfile`

```rust
// 保留 password secret 时：
let auth = AuthMethod::Password {
    secret_ref: SecretRef::keychain_ref(profile_id, "password"),
};
// 保留原 profile 的 private key path 和可选 passphrase_ref 时
```

不得以空 password 调用 `ConnectionProfile::from_connection_settings`。该方法仍会映射 SecretRef，但是调用 store 会使 Keychain 获得空值。因此，必须显式区分该分支。

### Connect form 的保存流程

Connect Save 保持现有行为，并继续通过 `build_settings` 要求 password。仅修改 metadata 时使用 Settings editor。

---

## 自检：计划与设计的对应关系

| 设计内容 | 对应任务 |
| --- | --- |
| Settings Profiles section | Task 1 |
| 列表和选择操作 | Task 1 |
| 新建、编辑、保存和 Keychain 空值保留语义 | Task 2 |
| 两个 surface 的删除确认 | Task 3 |
| Filter 和空状态 | Task 4 |
| 可选的 OpenProfiles / palette | Task 5 |
| Settings 不提供一键 Connect | 全局约束 |
| Recents 不并入 Profiles | 全局约束 |

**占位内容检查：** 类型和保存规则均已明确，而且不存在 TBD。

**类型一致性：** 全文对 `SettingsSection`、`ProfileEditorState`、`request_delete_profile` / `confirm_delete_profile` 的使用保持一致。

---

## 实施方式

本计划位于 `docs/plans/2026-07-14-profile-management-impl.md`。

**可以选择以下两种实施方式：**

1. **Subagent-Driven（推荐）**：每个任务使用一个新的 subagent，并在完成后进行评审。
2. **Inline Execution**：在当前 session 中按检查点实施。

请选择实施方式。
