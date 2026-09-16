# Connect 面板 Profile Picker 实现计划

> **Agent 执行要求：** 必须使用 `superpowers:subagent-driven-development`（推荐）或 `superpowers:executing-plans`，并且按任务顺序执行本计划。各步骤使用复选框（`- [ ]`）记录状态。

**目标：** 将 Connect modal 中展开显示的 Saved profiles 列表（每行包含 Use/Delete）替换为单行 profile popover picker；Save as 默认折叠；同时移除 Connect 中的 profile 删除功能。因此，Settings 是 profile 的管理界面。

**架构：** 为 `ConnectForm` 增加 picker 和 Save as 的 UI 状态，并且在 `render_connect_form_modal` 中重建 profile 区域。继续使用 `use_profile`、`save_current_profile` 和 `profile_matches_filter`。用户按 Esc 时先关闭 picker，再关闭 Connect。

**技术栈：** Rust、GPUI modal render、`InputState`，以及 `profiles.rs` / `connect_form.rs` 中现有的 profile helper。

**设计文档：** `docs/plans/2026-07-14-connect-panel-profile-picker-design.md`

## 全局约束

- Connect card 的高度不得随 profile 数量线性增加。
- Connect 中不得为每个 profile 提供 Delete；删除功能只存在于 Settings。
- 用户选择 profile 后，通过 `use_profile` 预填字段；但是字段仍可编辑，并且不得自动执行 Connect。
- 用户选择 Manual entry 时，清除 `source_profile_id` 和 secret；但是保留 host、port 和 username 的值。
- Save as 默认折叠；展开后的保存操作继续使用现有的 `save_current_profile`。
- 继续使用 `profiles.rs` 中的 `profile_matches_filter`。
- 不修改 sftp 或 storage schema。
- 不新增 crate，因此本次改动仅限必要的 UI 变更。
- 更新依赖 Connect Delete 的测试：移除 `connect_form_delete_opens_confirm_not_immediate`，或者将其调整为 Settings 测试。

## 文件范围

| 文件 | 职责 |
| --- | --- |
| **修改** `crates/app/src/workspace/connect_form.rs` | `ConnectForm` 字段；`empty`/`from_profile`/`prefilled` 初始化；`clear_to_manual_entry`；打开表单时重置 picker 状态；key handler 中的 Esc 处理 |
| **修改** `crates/app/src/workspace/modals.rs` | 使用 trigger 和 popover 替换 saved profiles 列表；默认折叠 Save as；可选的 Manage… |
| **修改** `crates/app/src/workspace/modals.rs` 中的 `cancel_active_modal` | 如果 Connect form 和 picker 均已打开，那么只关闭 picker |
| **修改** `crates/app/src/workspace/tests.rs` | picker、manual、Save as 和无 Delete 操作的测试 |
| **不得修改** | Settings Profiles CRUD、sftp、recents |

## 建议的 PR 划分

| PR | 任务 |
| --- | --- |
| PR1 | Task 1–2（picker 容器及 select/manual） |
| PR2 | Task 3（filter 和 Esc） |
| PR3 | Task 4（Save as 折叠和 Manage…） |
| — | Task 5 回归验证 |

---

### Task 1：增加 ConnectForm 状态并移除展开列表 UI

**文件：**
- 修改：`connect_form.rs`
- 修改：`modals.rs`（`render_connect_form_modal`）
- 测试：`tests.rs`

**接口：**
- 在 `ConnectForm` 中增加：

```rust
pub(crate) profile_picker_open: bool,
pub(crate) profile_picker_filter: InputState,
pub(crate) save_as_expanded: bool,
// existing fields unchanged
```

```rust
// In empty(), from_profile, prefilled — set:
// profile_picker_open: false
// profile_picker_filter: InputState::new()
// save_as_expanded: false

pub(crate) fn profile_trigger_label(&self, profiles: &[ConnectionProfile]) -> String {
    if let Some(id) = self.source_profile_id {
        if let Some(p) = profiles.iter().find(|p| p.id == id) {
            return p.name.clone();
        }
    }
    "Manual entry".into()
}
```

- [ ] **Step 1：先增加失败测试，确认打开 Connect 后不存在逐行 Delete**

```rust
#[gpui::test]
fn connect_form_has_no_inline_profile_delete_rows(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    // seed 2 profiles into store
    workspace.update_in(&mut cx, |ws, window, cx| {
        // save two profiles via store + optional keychain
        ws.open_connect_form(window, cx);
        assert!(ws.connect_form.is_some());
        let form = ws.connect_form.as_ref().unwrap();
        assert!(!form.profile_picker_open);
        assert!(!form.save_as_expanded);
    });
    // Behavioral: calling paths that only existed as Delete buttons are gone —
    // assert request_delete_profile is NOT needed from connect UI.
    // Stronger: after open, profile_delete_confirm is None and store len unchanged
    // when we only open form (always true). Prefer unit test on render inventory
    // or: ensure open_connect_form does not set picker open.
}
```

Task 2 会提供更具体的测试；但是 Task 1 先增加以下测试：

```rust
#[gpui::test]
fn open_connect_form_resets_picker_and_save_as_flags(cx: &mut TestAppContext) {
    let (workspace, mut cx, _) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        ws.open_connect_form(window, cx);
        let form = ws.connect_form.as_mut().unwrap();
        form.profile_picker_open = true;
        form.save_as_expanded = true;
        ws.close_connect_form(window, cx);
        ws.open_connect_form(window, cx);
        let form = ws.connect_form.as_ref().unwrap();
        assert!(!form.profile_picker_open);
        assert!(!form.save_as_expanded);
    });
}
```

- [ ] **Step 2：实现字段并移除列表 UI**

在 `render_connect_form_modal` 中，**删除**将 `saved_profiles` 映射为 Use/Delete 行的代码块。该代码当前大约位于 `modals.rs` 的 571–660 行。

使用单个 **Profile** 行替换该代码块：

```rust
// Pseudo-structure
let profiles = cx.resources().profiles.profiles().to_vec();
let trigger_label = form.profile_trigger_label(&profiles);

card = card.child(
    div().flex().items_center().gap_2()
        .child(label "Profile" width 96)
        .child(
            div()
                .id("profile-picker-trigger")
                .flex_1().min_w_0()
                .px_2().py_1()
                .border_1()...
                .child(div().truncate().child(trigger_label))
                .child("▾") // or chevron
                .on_click(|ws| {
                    if let Some(f) = &mut ws.connect_form {
                        f.profile_picker_open = !f.profile_picker_open;
                    }
                    cx.notify();
                })
        )
);

// If profile_picker_open: child popover panel under the row
if form.profile_picker_open {
    card = card.child(render_profile_picker_popover(...));
}
```

Task 1 的 popover 可以将**全部** profile 显示为可点击行，并且在 Task 2 中实现选择行为；也可以在 Task 2 之前只显示空容器。为了保证视觉完整性，优先显示 profile 名称。如果分离实现更清晰，那么在 Task 2 中再关联点击行为。

**Task 1 的 MVP：** 实现 picker trigger、打开/关闭切换，以及调用 `use_profile` 的列表行。如果变更规模较小，可以合并 Task 1 和 Task 2。评审条件允许时，trigger 和 select 优先使用**一个 commit**；但是计划仍保留独立的验收阶段。

如果 Task 1 只包含字段、列表移除、picker trigger 和**不支持选择**的静态列表，那么功能不完整。因此，在执行时间受限时，由同一实现者连续完成 Task 1 和 Task 2；但是两个任务的复选框仍须分别保留。

如果必须严格区分 Task 1，那么移除列表，并增加 trigger 和内容为 “Select a profile” 的空 popover。

- [ ] **Step 3：确认测试通过并提交**

```bash
git commit -m "feat(app): replace Connect saved-profiles list with picker trigger"
```

---

### Task 2：选择 profile 和 Manual entry

**文件：**
- 修改：`connect_form.rs` 中的 `clear_manual_secrets` / `switch_to_manual_entry`
- 修改：`modals.rs` 中的 popover rows
- 测试：`tests.rs`

**接口：**

```rust
impl ConnectForm {
    /// Clear source_profile_id and secret fields; keep host/port/username/key_path path text as design.
    /// Design: clear password/passphrase; keep host/port/user.
    pub(crate) fn switch_to_manual_entry(&mut self) {
        self.source_profile_id = None;
        self.password = InputState::new();
        self.passphrase = InputState::new();
        // keep host, port, username, key_path, auth_method
        self.profile_picker_open = false;
    }
}

// Workspace
pub(crate) fn select_connect_profile(&mut self, id: ProfileId, cx: &mut Context<Self>) {
    self.use_profile(id, cx);
    if let Some(form) = &mut self.connect_form {
        form.profile_picker_open = false;
        form.profile_picker_filter = InputState::new();
    }
    cx.notify();
}
```

注意：`use_profile` 通过 `from_profile` **替换**整个 form。因此，如果 `from_profile` / `empty` 将 picker flag 设为 false，那么这些 flag 会被重置。必须确保 `use_profile` 完成后 picker 处于关闭状态：

```rust
// end of use_profile
form.profile_picker_open = false;
form.save_as_expanded = false; // or leave as-was
self.connect_form = Some(form);
```

Popover 行：

```rust
for profile in profiles {
    row.on_click → select_connect_profile(profile.id)
}
// footer
Manual entry → form.switch_to_manual_entry()
```

可选的次要操作：`Manage…` → `OpenProfiles`，该操作在 Task 4 中实现。

- [ ] **Step 1：增加测试**

```rust
#[gpui::test]
fn connect_picker_select_profile_prefills_form(cx: &mut TestAppContext) {
    // seed profile "Work" host example.com user alex + keychain password
    // open connect, select_connect_profile(id)
    // assert source_profile_id, host, username, password filled
    // assert !profile_picker_open
}

#[gpui::test]
fn connect_manual_entry_clears_profile_link_and_secrets(cx: &mut TestAppContext) {
    // select profile first, then switch_to_manual_entry
    // assert source_profile_id None, password empty, host still example.com
}
```

- [ ] **Step 2：实现功能，确认测试通过，然后提交**

```bash
git commit -m "feat(app): select profile or manual entry from Connect picker"
```

---

### Task 3：Filter 和 Esc 优先关闭 picker

**文件：**
- 修改：`modals.rs` 的 popover，将 filter `text_field` 绑定到 `profile_picker_filter`
- 修改：`cancel_active_modal` / `handle_connect_form_key`
- 测试：filter 和 Esc

**接口：**

```rust
// cancel_active_modal — before close_connect_form:
if let Some(form) = &mut self.connect_form {
    if form.profile_picker_open {
        form.profile_picker_open = false;
        cx.notify();
        return;
    }
}
```

如果 form 会在 `CancelActiveModal` 之前处理按键，那么还需要在 `handle_connect_form_key` 中处理 Escape。

列表过滤方式：

```rust
profiles.iter().filter(|p| profile_matches_filter(p, form.profile_picker_filter.value()))
```

如果过滤结果为空，那么显示 “No matches”；但是仍须显示 Manual entry 行。

- [ ] **Step 1：增加测试**

```rust
#[gpui::test]
fn connect_picker_filter_narrows_profiles(cx: &mut TestAppContext) {
    // two profiles; set filter; open picker; assert filtered count via helper
    // can test pure: profile_matches_filter already unit-tested — integration:
    workspace.update... form.profile_picker_filter.set_value("work");
    let n = ws.filtered_connect_profiles(cx).len();
    assert_eq!(n, 1);
}

// helper on Workspace:
fn filtered_connect_profiles<'a>(&'a self, cx: &'a App) -> Vec<&'a ConnectionProfile> {
    let q = self.connect_form.as_ref().map(|f| f.profile_picker_filter.value().to_string()).unwrap_or_default();
    cx.resources().profiles.profiles().iter().filter(|p| profile_matches_filter(p, &q)).collect()
}

#[gpui::test]
fn escape_closes_profile_picker_before_connect_form(cx: &mut TestAppContext) {
    // open connect, open picker, cancel_active_modal
    // assert connect_form.is_some() && !picker_open
    // cancel again → connect_form none
}
```

- [ ] **Step 2：实现功能并提交**

```bash
git commit -m "feat(app): filter Connect profile picker and Esc dismisses picker first"
```

---

### Task 4：折叠 Save as，并且按需增加 Manage…

**文件：**
- 修改：`modals.rs`，替换始终可见的 Save as 行
- 可选：在 Profile trigger 旁增加 Manage… → `OpenProfiles`。可以关闭 Connect，也可以使其保持打开；为了简化状态，优先**先关闭 Connect，再执行 OpenProfiles**。

**UI：**

```rust
if form.save_as_expanded {
    // existing name field + Save profile button
} else {
    text_button or clickable label "Save as profile…"
        .on_click → save_as_expanded = true
}
```

`save_current_profile` 成功后，可以将 `save_as_expanded` 设为 false，以恢复默认状态。

如果 Connect 中仍有 Delete 相关 UI，那么必须全部移除。Task 1 原则上已经完成该要求。

- [ ] **Step 1：增加测试**

```rust
#[gpui::test]
fn connect_save_as_collapsed_by_default(cx: &mut TestAppContext) {
    open connect; assert !save_as_expanded
}

#[gpui::test]
fn connect_save_as_expand_and_save_still_works(cx: &mut TestAppContext) {
    // expand, fill host/user/password/name, save_current_profile, assert store has profile
}
```

更新或移除 `connect_form_delete_opens_confirm_not_immediate`：**删除该测试**，或者将其改写为只验证 Settings 中的删除行为。Settings 已有相关测试。

- [ ] **Step 2：实现功能并提交**

```bash
git commit -m "feat(app): collapse Save as profile on Connect form"
```

---

### Task 5：回归验证与完成检查

- [ ] **Step 1：执行完整测试**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

修正所有依赖 Use/Delete 行或 Connect form 直接删除行为的测试。

- [ ] **Step 2：执行人工检查，并在报告中记录结果**

1. profile 数量为 0 时打开 Connect：显示 Manual entry，并且表单高度较小。
2. profile 数量至少为 5 时：card 高度稳定，并且 picker 可以滚动。
3. 选择 profile 后：字段已填充，并且 Connect 仍然有效。
4. 选择 Manual entry 后：secret 已清除。
5. 展开 Save as 后：保存功能有效。
6. Settings 中的 Delete 仍然有效。

- [ ] **Step 3：仅在仍有修正内容时提交**

---

## 实现说明

### 不使用 absolute positioning 的 popover 布局

如果 GPUI absolute popover 的实现较复杂，那么可以在 Profile 行正下方使用 **inline dropdown panel**，并且该 panel 仍位于 card 内。关闭时，这种实现满足“高度不随 N 线性增加”的要求；打开时，panel 使用 `max_h(px(200))` 和 `overflow_y_scroll`。因此，优先采用该方案，避免增加不必要的 z-index 处理。

```rust
div()
  .flex().flex_col().gap_1()
  .max_h(px(200.0))
  .overflow_y_scroll() // if available; else cap children
```

### `use_profile` 与 form 替换

`use_profile` 根据 profile 创建新的 `ConnectForm`，因此必须在 `from_profile` 中或构造完成后初始化新增的 picker 字段。

### 选择 profile 时的 Keychain 行为

该行为保持不变：`use_profile` 将 secret 加载到 form 字段中，供 Connect 提交使用。

---

## 自查：计划与设计的一致性

| 设计要求 | 任务 |
| --- | --- |
| 单行 picker，不显示线性列表 | Task 1 |
| Select → use_profile | Task 2 |
| Manual entry | Task 2 |
| Filter | Task 3 |
| Esc 优先关闭 picker | Task 3 |
| Save as 默认折叠 | Task 4 |
| Connect 中无 Delete | Task 1/4 |
| 可选的 Manage… | Task 4 |
| 不自动执行 Connect | 全局约束 |

**占位内容检查：** 字段、helper 和测试名称均已明确。
**类型一致性：** `profile_picker_open`、`save_as_expanded`、`switch_to_manual_entry`、`select_connect_profile`。

---

## 执行方式

本计划保存在 `docs/plans/2026-07-14-connect-panel-profile-picker-impl.md`。

**可以采用以下两种执行方式：**

1. **Subagent-Driven（推荐）**
2. **Inline Execution**

执行前需要从以上两种方式中选择一种。
