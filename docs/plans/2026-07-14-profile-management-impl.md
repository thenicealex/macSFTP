# Profile Management UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a Settings → Profiles section for list/search/create/edit/delete of connection profiles (Keychain-backed), while keeping the Connect form as the path to connect the active tab (Use / Save retained).

**Architecture:** View state on `Workspace` (`settings_section`, filter, selection, `ProfileEditorState`, delete confirm). UI lives under Settings surface (existing sidebar pattern). Persistence via existing `ProfileStore` + `KeychainStore`. Connect-form delete gains the same confirmation modal as Settings.

**Tech Stack:** Rust, GPUI Settings surface, `InputState`, `ProfileStore` / `KeychainStore`, `ConnectionProfile` / `AuthMethod` / `SecretRef`.

**Spec:** `docs/plans/2026-07-14-profile-management-design.md`

## Global Constraints

- **管库 ≠ 连接** — Settings manages the profile library; Connect connects the active tab. No one-click Connect from Settings in MVP.
- **Empty password/passphrase on edit Save** — means “keep Keychain secret”; do **not** erase or overwrite with empty.
- **New password profile** requires non-empty password; new key profile requires non-empty key path.
- **Delete always confirmed** (Settings and Connect).
- **No secrets** in status detail, logs, or editor prefill for existing profiles.
- **Recents stay separate** — do not merge into Profiles UI.
- No `group_id` UI, import/export, or drag sort.
- No `unwrap` on recoverable paths; no sftp changes.
- Prefer `crates/app/src/workspace/profiles.rs` for editor helpers (not `mod.rs` path).
- Surgical diffs; match existing Settings / Connect form styling.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `crates/app/src/workspace/profiles.rs` | `SettingsSection`, `ProfileEditorState`, filter helper, load/save/delete editor APIs |
| **Modify** `crates/app/src/workspace/mod.rs` | fields; `mod profiles`; open Settings init |
| **Modify** `crates/app/src/workspace/render.rs` | `render_settings` sidebar + Profiles pane |
| **Modify** `crates/app/src/workspace/modals.rs` | profile delete confirm modal; Connect delete → confirm |
| **Modify** `crates/app/src/workspace/connect_form.rs` | optional: share validation; delete entry point only opens confirm |
| **Modify** `crates/app/src/main.rs` | `mod` if needed |
| **Modify** `crates/app/src/app_actions.rs` + `palette_commands.rs` | optional `OpenProfiles` (Task 5) |
| **Modify** `crates/app/src/workspace/tests.rs` | gpui + store tests |
| **Do not modify** | `crates/sftp/**`, recents schema |

## Suggested PR mapping

| PR | Tasks |
| --- | --- |
| PR1 | Task 1 |
| PR2 | Task 2 |
| PR3 | Task 3 |
| PR4 | Task 4–5 |

---

### Task 1: Settings section + Profiles list (read-only)

**Files:**
- Create: `crates/app/src/workspace/profiles.rs`
- Modify: `crates/app/src/workspace/mod.rs`
- Modify: `crates/app/src/workspace/render.rs` (`render_settings`)
- Modify: workspace module tree (`mod profiles` in `mod.rs` or parent)
- Test: `crates/app/src/workspace/tests.rs`

**Interfaces:**
- Produces:

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
// Task 1 filter can be empty string only (always show all); wire filter input UI with no logic yet OR implement filter early in Task 1 for free
```

**Filter helper (implement in Task 1 pure fn, wire UI in Task 4 if preferred):**

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

- [ ] **Step 1: Failing tests**

```rust
#[gpui::test]
fn settings_profiles_section_lists_saved_profiles(cx: &mut TestAppContext) {
    let (workspace, mut cx, _ch) = init_workspace(cx);
    // save one profile via existing connect_form path or resources_mut().profiles.save_profile
    workspace.update_in(&mut cx, |ws, window, cx| {
        // insert profile into store with memory keychain as tests do
        ws.surface = WorkspaceSurface::Settings;
        ws.set_settings_section(SettingsSection::Profiles, cx);
        assert_eq!(ws.settings_section, SettingsSection::Profiles);
        let n = cx.resources().profiles.profiles().len();
        assert!(n >= 1);
        // selected defaults to first when entering profiles with non-empty list
        assert!(ws.selected_profile_id.is_some());
    });
}

#[test]
fn profile_matches_filter_name_host_user() {
    // build ConnectionProfile; assert matches "work", "example", "alex"
}
```

- [ ] **Step 2: Run — expect FAIL**

```bash
cargo test -p macsftp-app --bin macsftp settings_profiles_section profile_matches_filter -- --nocapture
```

- [ ] **Step 3: Implement**

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

`Workspace::new`: init `settings_section: General`, `profile_filter: String::new()` or `InputState::new()`, `selected_profile_id: None`.

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
        // Task 2: also load_editor_from_selection(cx)
    }
    cx.notify();
}
```

`render_settings`:

- Sidebar: two clickable rows General / Profiles; selected bg `element_selected`.
- Body: if General → existing Appearance block; if Profiles → split pane:
  - Left (~220px): New Profile button (can no-op until Task 2 or call stub), list of profile rows.
  - Right: placeholder “Select a profile” / show selected name + host read-only summary for Task 1.

```rust
// Sidebar item helper
fn sidebar_item(id, label, selected, on_click) -> impl IntoElement { /* match General styling */ }
```

Open Settings action: keep `settings_section` as-is (last section) or reset to General — **reset to General** on each open to match predictable UX:

```rust
// OpenSettings handler
workspace.surface = WorkspaceSurface::Settings;
workspace.settings_section = SettingsSection::General;
```

- [ ] **Step 4: PASS + commit**

```bash
cargo test -p macsftp-app --bin macsftp settings_profiles_section profile_matches_filter -- --nocapture
git add crates/app/src/workspace/
git commit -m "feat(app): add Settings Profiles section with profile list"
```

---

### Task 2: Profile editor — New / Edit / Save + Keychain semantics

**Files:**
- Modify: `crates/app/src/workspace/profiles.rs`
- Modify: `crates/app/src/workspace/mod.rs` (fields `profile_editor`)
- Modify: `crates/app/src/workspace/render.rs` (right pane form)
- Modify: `crates/app/src/workspace/connect_form.rs` only if extracting shared secret store helpers (prefer call existing `store_profile_secrets` / `next_profile_id`)
- Test: `tests.rs`

**Interfaces:**
- Produces:

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
    // NO password prefill
}

// Workspace methods:
pub(crate) fn start_new_profile(&mut self, cx: &mut Context<Self>);
pub(crate) fn load_profile_editor(&mut self, id: ProfileId, cx: &mut Context<Self>);
pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>);
```

**Save semantics (implement exactly):**

```rust
pub(crate) fn save_profile_editor(&mut self, cx: &mut Context<Self>) {
    let Some(editor) = &self.profile_editor else { return };
    // 1. Validate host/user/port (same rules as ConnectForm::build_settings metadata)
    // 2. Resolve profile_id: existing or next_profile_id
    // 3. Auth:
    //    - Password + is_new + password empty → error "Password is required."
    //    - Password + !is_new + password empty → keep previous AuthMethod::Password { secret_ref }
    //    - Password + password non-empty → store Keychain then AuthMethod with SecretRef
    //    - PrivateKey + key empty → error
    //    - PrivateKey + passphrase empty on update → keep previous passphrase_ref if any
    //    - PrivateKey + passphrase non-empty → store
    // 4. Build ConnectionProfile { name (default user@host if empty), default_remote_path from field, ... }
    // 5. save_profile on store; on success reload editor as non-new with secret_present_hint true; clear error
    // 6. On Keychain fail: status_message user-facing, no JSON write
}
```

Reuse `SecretRef::keychain_ref`, `store_profile_secrets` where possible. For “keep secret” path, **do not** call `store` with empty string.

Auth method UI: two buttons Password / Private Key like Connect form.

- [ ] **Step 1: Failing tests**

```rust
#[gpui::test]
fn settings_new_profile_save_persists(cx: &mut TestAppContext) {
    // open profiles section, start_new_profile, fill editor fields via update,
    // save_profile_editor, assert profiles().len()==1 and keychain.load(password ref) Some
}

#[gpui::test]
fn settings_edit_host_keeps_keychain_secret_when_password_blank(cx: &mut TestAppContext) {
    // seed profile + secret, load editor, change host only, password left empty, save
    // assert host updated and keychain still returns original password
}

#[gpui::test]
fn settings_new_password_profile_requires_password(cx: &mut TestAppContext) {
    // new + host/user filled, password empty → save leaves error, store empty
}
```

- [ ] **Step 2: FAIL → implement editor UI + save → PASS**

Right pane fields (compact, max_w 560):

- Name, Host, Port, Username  
- Auth toggle  
- Password **or** Key path + Passphrase  
- Default remote path  
- Hint line when `secret_present_hint`  
- Save button; Delete placeholder disabled or hidden until Task 3  

Wire `start_new_profile` on New Profile button; list click → `load_profile_editor`.

- [ ] **Step 3: Commit**

```bash
git add crates/app/src/workspace/
git commit -m "feat(app): edit and save profiles from Settings Profiles"
```

---

### Task 3: Delete confirmation (Settings + Connect)

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` — `profile_delete_confirm: Option<ProfileId>`
- Modify: `crates/app/src/workspace/profiles.rs` — `request_delete_profile`, `confirm_delete_profile`, `cancel_delete_profile`
- Modify: `crates/app/src/workspace/modals.rs` — render confirm modal; Connect Delete → `request_delete_profile`
- Modify: `crates/app/src/workspace/render.rs` — Delete… button on editor
- Test: `tests.rs`

**Interfaces:**

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

**Modal copy:**

- Title: `Delete Profile?`
- Body: `Delete "{name}" ({user}@{host})? This cannot be undone.`
- Primary: `Delete` (danger/error color if theme has it)
- Cancel

`cancel_active_modal`: if `profile_delete_confirm.is_some()`, cancel delete first.

Connect form Delete button:

```rust
// was: workspace.delete_profile(profile_id, cx)
workspace.request_delete_profile(profile_id, cx);
```

- [ ] **Step 1: Failing tests**

```rust
#[gpui::test]
fn delete_profile_requires_confirmation(cx: &mut TestAppContext) {
    // seed profile, request_delete, assert still in store
    // cancel → still there
    // request + confirm → gone, keychain empty
}

#[gpui::test]
fn connect_form_delete_opens_confirm_not_immediate(cx: &mut TestAppContext) {
    // open connect, seed profile, click path = request_delete_profile
    // assert profile_delete_confirm.is_some() && store still has profile
}
```

- [ ] **Step 2: Implement modal + wire → PASS**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(app): confirm before deleting connection profiles"
```

---

### Task 4: Filter + empty states polish

**Files:**
- Modify: `profiles.rs` / `render.rs`
- Test: filter tests (if not done in Task 1)

**Interfaces:**
- `profile_filter: InputState` (or keep String + set on key)
- Empty library: “No saved profiles” + New Profile
- Filter no hits: “No matches”
- List uses `profile_matches_filter`

- [ ] **Step 1: Test**

```rust
#[gpui::test]
fn settings_profile_filter_narrows_list(cx: &mut TestAppContext) {
    // two profiles different hosts; set filter; assert filtered helper length 1
}
```

- [ ] **Step 2: UI filter field above list; empty states**

- [ ] **Step 3: Commit**

```bash
git commit -m "feat(app): filter and empty states for Settings Profiles"
```

---

### Task 5: Optional OpenProfiles + regression

**Files:**
- Modify: `app_actions.rs` — `OpenProfiles` action (optional keybinding none or cmd-shift-, avoid conflict)
- Modify: `palette_commands.rs` — “Manage Profiles” → open Settings Profiles section
- Modify: `mod.rs` open handler
- Test: palette or action switches section
- Full suite

```rust
// OpenProfiles
workspace.surface = WorkspaceSurface::Settings;
workspace.set_settings_section(SettingsSection::Profiles, cx);
```

- [ ] **Step 1: Test action opens Profiles section**

- [ ] **Step 2: Implement palette entry `Manage Profiles` keywords: profile, credentials**

- [ ] **Step 3: Full regression**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
cargo test -p macsftp-storage -- --nocapture
```

- [ ] **Step 4: Commit**

```bash
git commit -m "feat(app): open Settings Profiles from command palette"
```

If palette is deferred, skip code and only run regression + note in report — **prefer implementing** (small).

---

### Task 6: Closeout checklist

- [ ] Confirm Connect Use / Save still pass existing `connect_form_save_use_update_and_delete_profile` (update test if Delete now confirms).
- [ ] Confirm no password in logs on save path.
- [ ] Document hand-test: Settings → Profiles CRUD; Connect Use; Delete confirm cancel/accept.

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

- [ ] Commit only if last fixes needed.

---

## Implementation notes (shared)

### AuthMethodKind

Prefer **reuse** `connect_form::AuthMethodKind` (`pub(crate)`) from `profiles.rs` rather than duplicating.

### Updating `ConnectionProfile` without password

```rust
// When keeping password secret:
let auth = AuthMethod::Password {
    secret_ref: SecretRef::keychain_ref(profile_id, "password"),
};
// When keeping private key path + optional passphrase_ref from previous profile
```

Do **not** use `ConnectionProfile::from_connection_settings` with empty password — it still maps SecretRef but Keychain would get empty if you called store. Branch explicitly.

### Connect form save path

Unchanged for Connect Save (still requires password via `build_settings`). Settings editor is the path for metadata-only updates.

---

## Self-Review (plan vs design)

| Design | Task |
| --- | --- |
| Settings Profiles section | Task 1 |
| List + select | Task 1 |
| New/Edit/Save + Keychain empty-keep | Task 2 |
| Delete confirm both surfaces | Task 3 |
| Filter + empty states | Task 4 |
| Optional OpenProfiles / palette | Task 5 |
| No Settings one-click Connect | Constraints |
| No Recents merge | Constraints |

**Placeholder scan:** concrete types and save rules; no TBD.

**Type consistency:** `SettingsSection`, `ProfileEditorState`, `request_delete_profile` / `confirm_delete_profile` used consistently.

---

## Execution Handoff

Plan saved to `docs/plans/2026-07-14-profile-management-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task + review  
2. **Inline Execution** — this session with checkpoints  

Which approach?
