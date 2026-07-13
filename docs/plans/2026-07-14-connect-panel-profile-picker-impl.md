# Connect Panel Profile Picker Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the expanded Saved-profiles list (Use/Delete per row) on the Connect modal with a one-row profile popover picker, fold Save-as by default, and remove Connect-side profile delete — keeping Settings as the management surface.

**Architecture:** Extend `ConnectForm` with picker/save-as UI state. Rebuild the profile chrome in `render_connect_form_modal`. Reuse `use_profile`, `save_current_profile`, and `profile_matches_filter`. Esc closes the picker before dismissing Connect.

**Tech Stack:** Rust, GPUI modal render, `InputState`, existing profile helpers in `profiles.rs` / `connect_form.rs`.

**Spec:** `docs/plans/2026-07-14-connect-panel-profile-picker-design.md`

## Global Constraints

- Connect card height must **not** grow linearly with profile count.
- **No** per-profile Delete on Connect (Settings only).
- Select profile → `use_profile` prefill; fields remain editable; **no** auto-Connect.
- Manual entry: clear `source_profile_id` + secrets; **keep** host/port/username values.
- Save as **collapsed** by default; expanded Save uses existing `save_current_profile`.
- Reuse `profile_matches_filter` from `profiles.rs`.
- No sftp / storage schema changes.
- No new crates; surgical UI change only.
- Update tests that assume Connect Delete (`connect_form_delete_opens_confirm_not_immediate` → remove or retarget Settings).

## File Map

| File | Responsibility |
| --- | --- |
| **Modify** `crates/app/src/workspace/connect_form.rs` | `ConnectForm` fields; `empty`/`from_profile`/`prefilled` init; `clear_to_manual_entry`; open form resets picker state; Esc in key handler |
| **Modify** `crates/app/src/workspace/modals.rs` | Replace saved-profiles list UI with trigger + popover; fold Save as; optional Manage… |
| **Modify** `crates/app/src/workspace/modals.rs` `cancel_active_modal` | If connect form + picker open → close picker only |
| **Modify** `crates/app/src/workspace/tests.rs` | Picker / manual / save-as / no delete tests |
| **Do not modify** | Settings Profiles CRUD, sftp, recents |

## Suggested PR mapping

| PR | Tasks |
| --- | --- |
| PR1 | Task 1–2 (picker shell + select/manual) |
| PR2 | Task 3 (filter + Esc) |
| PR3 | Task 4 (Save as fold + Manage…) |
| — | Task 5 regression |

---

### Task 1: ConnectForm state + remove expanded list UI chrome

**Files:**
- Modify: `connect_form.rs`
- Modify: `modals.rs` (`render_connect_form_modal`)
- Test: `tests.rs`

**Interfaces:**
- Produces on `ConnectForm`:

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

- [ ] **Step 1: Failing test — open connect has no per-row delete**

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

Better concrete tests in Task 2. For Task 1:

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

- [ ] **Step 2: Implement fields + remove list UI**

In `render_connect_form_modal`, **delete** the block that maps `saved_profiles` to Use/Delete rows (approx lines 571–660 in current `modals.rs`).

Replace with a single **Profile** row:

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

Task 1 popover can list **all** profiles as clickable rows (select in Task 2) **or** only the shell empty until Task 2 — prefer listing names for visual completeness, wire click in Task 2 if split is cleaner.

**MVP in Task 1:** shell trigger + open/close toggle + list rows that call `use_profile` (merge Task 1–2 if small). Prefer **one commit** for trigger+select if reviews allow; plan splits for gates.

Implement Task 1 as: fields + remove old list + trigger toggles picker + static list **without** select (rows no-op) is too incomplete. **Do Task 1+2 together in one implementer if timeboxed**, but keep checkboxes separate.

For strict Task 1: remove list, add trigger + open empty popover "Select a profile".

- [ ] **Step 3: PASS + commit**

```bash
git commit -m "feat(app): replace Connect saved-profiles list with picker trigger"
```

---

### Task 2: Select profile + Manual entry

**Files:**
- Modify: `connect_form.rs` — `clear_manual_secrets` / `switch_to_manual_entry`
- Modify: `modals.rs` — popover rows
- Test: `tests.rs`

**Interfaces:**

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

Note: `use_profile` **replaces** entire form via `from_profile` — that resets picker flags if `from_profile`/`empty` sets them false. Ensure `use_profile` ends with picker closed:

```rust
// end of use_profile
form.profile_picker_open = false;
form.save_as_expanded = false; // or leave as-was
self.connect_form = Some(form);
```

Popover rows:

```rust
for profile in profiles {
    row.on_click → select_connect_profile(profile.id)
}
// footer
Manual entry → form.switch_to_manual_entry()
```

Optional secondary: `Manage…` → `OpenProfiles` (Task 4).

- [ ] **Step 1: Tests**

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

- [ ] **Step 2: Implement → PASS → commit**

```bash
git commit -m "feat(app): select profile or manual entry from Connect picker"
```

---

### Task 3: Filter + Esc closes picker first

**Files:**
- Modify: `modals.rs` popover — filter `text_field` bound to `profile_picker_filter`
- Modify: `cancel_active_modal` / `handle_connect_form_key`
- Test: filter + esc

**Interfaces:**

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

Also in `handle_connect_form_key` for Escape if form handles keys before CancelActiveModal.

Filter list:

```rust
profiles.iter().filter(|p| profile_matches_filter(p, form.profile_picker_filter.value()))
```

Empty filter results: show "No matches" + still show Manual entry row.

- [ ] **Step 1: Tests**

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

- [ ] **Step 2: Implement → commit**

```bash
git commit -m "feat(app): filter Connect profile picker and Esc dismisses picker first"
```

---

### Task 4: Fold Save as + optional Manage…

**Files:**
- Modify: `modals.rs` — replace always-visible Save as row
- Optional: Manage… next to Profile trigger → `OpenProfiles` (close connect or leave open — **close Connect then OpenProfiles** cleaner)

**UI:**

```rust
if form.save_as_expanded {
    // existing name field + Save profile button
} else {
    text_button or clickable label "Save as profile…"
        .on_click → save_as_expanded = true
}
```

After successful `save_current_profile`, set `save_as_expanded = false` (optional polish).

Remove Connect Delete entirely if any residual (Task 1 should have).

- [ ] **Step 1: Tests**

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

Update/remove `connect_form_delete_opens_confirm_not_immediate` — **delete test** or rewrite to Settings delete only (Settings already covered).

- [ ] **Step 2: Implement → commit**

```bash
git commit -m "feat(app): collapse Save as profile on Connect form"
```

---

### Task 5: Regression + closeout

- [ ] **Step 1: Full tests**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

Fix any test expecting Use/Delete rows or immediate connect-form delete.

- [ ] **Step 2: Manual checklist (document in report)**

1. Open Connect with 0 profiles — Manual entry, short form.  
2. With 5+ profiles — card height stable; picker scrolls.  
3. Select profile → fields filled → Connect still works.  
4. Manual entry → secrets cleared.  
5. Expand Save as → save works.  
6. Settings Delete still works.

- [ ] **Step 3: Commit only if fixes remain**

---

## Implementation notes

### Popover layout without absolute positioning

If GPUI absolute popover is awkward, **inline dropdown panel** immediately below the Profile row (still inside the card) is acceptable and matches "height not linear in N" when closed — when open, panel has `max_h(px(200))` + `overflow_y_scroll`. Prefer this over fighting z-index.

```rust
div()
  .flex().flex_col().gap_1()
  .max_h(px(200.0))
  .overflow_y_scroll() // if available; else cap children
```

### `use_profile` and form replacement

`use_profile` builds a new `ConnectForm` from profile — must initialize new picker fields in `from_profile` / after construction.

### Keychain on select

Unchanged: `use_profile` loads secrets into form fields for connect submit.

---

## Self-Review (plan vs design)

| Design | Task |
| --- | --- |
| One-row picker, no linear list | Task 1 |
| Select → use_profile | Task 2 |
| Manual entry | Task 2 |
| Filter | Task 3 |
| Esc picker first | Task 3 |
| Save as fold | Task 4 |
| No Connect Delete | Task 1/4 |
| Optional Manage… | Task 4 |
| No auto-connect | Constraints |

**Placeholder scan:** concrete fields, helpers, test names.  
**Type consistency:** `profile_picker_open`, `save_as_expanded`, `switch_to_manual_entry`, `select_connect_profile`.

---

## Execution Handoff

Plan saved to `docs/plans/2026-07-14-connect-panel-profile-picker-impl.md`.

**Two execution options:**

1. **Subagent-Driven (recommended)**  
2. **Inline Execution**  

Which approach?
