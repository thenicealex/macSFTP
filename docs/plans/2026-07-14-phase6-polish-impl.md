# Phase 6 Polish & Guidelines Compliance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan one task at a time. Checkbox syntax (`- [ ]`) records implementation progress.

**Goal:** Complete the UX improvements required by guidelines §11–§15. The result includes a maintainable audit checklist, tooltips for icon-only controls, modal focus restoration, user-facing text without internal terminology, narrow-window overflow corrections, and lightweight smoke tests for lists with 10k entries. This phase does not add features or animations.

**Architecture:** The audit determines the required refinements, and no new crates are introduced. Record deficiencies in `docs/plans/…-phase6-polish-audit.md`, and then address them by theme in this order: a11y → copy → narrow → perf. Changes in `app`/`ui` should remain limited to the identified requirements, whereas `visible_entries` should use pure tests.

**Tech Stack:** Rust, GPUI (`FocusHandle`, `icon_button`, `text_tooltip`, `min_w_0`/`truncate`), existing workspace modals and rendering, and pure `visible_entries` helpers.

**Spec:** `docs/plans/2026-07-14-phase6-polish-design.md`

## Global Constraints

- **No new animations:** Retain only the existing hover opacity, and do not add drawer, tab, or modal transitions.
- **No new product features:** Do not add transfer policies, redesign multi-window sessions, introduce an i18n framework, or modify SFTP behavior.
- **Keychain** can appear in the user UI. However, user-visible strings cannot contain **runtime / actor / channel / session epoch / crate / AppCommand**.
- **Performance:** Use lightweight correctness smoke tests, but do not add millisecond thresholds that can make CI tests unstable.
- **Tooltip:** Every interactive icon-only control must have a tooltip or label, whereas decorative icons are exempt.
- Modal focus: displaying a modal focuses a suitable control, and closing it or using Esc restores **pane** focus. A complete focus stack is outside this scope.
- Recoverable paths cannot use `unwrap` or `expect` according to AGENTS.md §5. Prefer `src/foo.rs` to `mod.rs`.
- Do **not** modify `crates/sftp/**` behavior or core transfer/session protocols.
- Changes must remain limited to the identified requirements, and unrelated refactoring is prohibited.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `docs/plans/2026-07-14-phase6-polish-audit.md` | §15 checklist and region matrix; update them as tasks complete |
| **Modify** `crates/app/src/workspace/modals.rs` | `cancel_active_modal` focus gaps (About, etc.) |
| **Modify** `crates/app/src/workspace/render.rs` | Tooltip labels, narrow flex/`truncate`, and status bar chip |
| **Modify** `crates/app/src/workspace/mod.rs` | user-facing status strings (`Runtime is…`) |
| **Modify** `crates/app/src/workspace/connect_form.rs` / `transfers.rs` / `file_ops.rs` / `panes.rs` / `event_handling.rs` | Revise user-visible text if it contains internal terminology |
| **Modify** `crates/ui/src/tab.rs` / `transfer_row.rs` / `components.rs` | tooltip completeness; narrow layout if gaps |
| **Modify** `crates/app/src/workspace/visible_entries.rs` | 10k filter smoke tests |
| **Modify** `crates/app/src/workspace/tests.rs` | modal Esc → pane focus tests |
| **Modify** `docs/plans/2026-07-14-phase6-polish-audit.md` (again) | Record pass status after each theme |
| **Do not modify** | `crates/sftp/**` production paths; session/recents storage (phase 5 done) |

## Suggested PR mapping

| PR | Tasks | Notes |
| --- | --- | --- |
| PR0+A | Task 1–2 | Audit document and a11y/focus |
| PR-B | Task 3 | Copy |
| PR-C | Task 4 | Narrow |
| PR-D | Task 5 | Performance smoke test and manual-test section in the audit |
| Completion | Task 6 | All checklist items resolved and full regression completed |

---

### Task 1: Audit checklist document (PR0)

**Files:**
- Create: `docs/plans/2026-07-14-phase6-polish-audit.md`

**Interfaces:**
- Produces: a maintained checklist for later tasks; Tasks 2–6 update its status cells.

- [ ] **Step 1: Create the audit file** with the following structure. Determine the initial **Status** from code inspection, and use `unknown` until Tasks 2–5 provide verification.

```markdown
# Phase 6 Polish Audit

**Date:** 2026-07-14  
**Design:** `docs/plans/2026-07-14-phase6-polish-design.md`  
**Window min size:** 720×480 (`crates/app/src/main.rs`)

## §15 Review Questions

| # | Question | Status | Notes |
| --- | --- | --- | --- |
| 1 | Single-window work context? | pass | No marketing landing; Files surface first |
| 2 | Palette or shortcut path? | pass | Phase 4 palette + bindings |
| 3 | loading/empty/error/disabled/focused/hover/selected? | unknown | Re-check after a11y |
| 4 | Narrow window no overflow? | unknown | Task 4 |
| 5 | No decorative cards/gradients? | pass | Theme tokens only |
| 6 | No main-thread block on network? | pass | Runtime bridge; residual risk accepted |
| 7 | 10k entries + multi transfer? | unknown | Task 5 smoke |
| 8 | Icon-only tooltips? | unknown | Task 2 |
| 9 | No secrets / internal jargon in UI? | unknown | Task 3 |
| 10 | Modal expiry / session_epoch safety? | pass | Phase 1+ core guards; reaffirm |

Status values: `pass` | `fail` | `unknown` | `accepted risk` (with reason).

## Region matrix

| Region | Tooltip | Focus open | Focus close | Truncate | Notes |
| --- | --- | --- | --- | --- | --- |
| Tab bar + close | | n/a | n/a | | `ui/tab.rs` Close Tab |
| Path bar (back/up/refresh/copy/…) | | | | | `render.rs` |
| Filter clear | | | | | Clear Filter (Esc) |
| Transfer drawer cancel/retry | | | | | `transfer_row.rs` |
| Status bar transfer chip | | | | | |
| Connect form | | | | | |
| Host key modal | | | | | |
| Conflict modal | | | | | |
| Delete confirm | | | | | |
| Go to Path | | | | | |
| Command palette | | | | | |
| About | | | | | Esc may miss pane focus |
| Settings surface | | | | | returns to Files |

## Hand performance smoke (Task 5)

See section filled in Task 5.

## Copy banlist (user-visible)

Forbidden substrings (case-insensitive) in UI labels/status: `runtime`, `actor`, `channel`, `session epoch`, `AppCommand`, `crate`.
Allowed: `Keychain`, host/port/profile/transfer/permission.
```

- [ ] **Step 2: Inspect the relevant code** and set every status that current evidence establishes. For example, `icon_button` call sites exist, and the About Esc path in `cancel_active_modal` does not call `focus_pane`.

From `modals.rs` today:

```rust
if self.about_open {
    self.about_open = false;
    cx.notify();
    return; // gap: no focus_pane
}
```

- [ ] **Step 3: Commit**

```bash
git add docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "docs: add Phase 6 polish audit checklist"
```

---

### Task 2: A11y — tooltips and modal focus (PR-A)

**Files:**
- Modify: `crates/app/src/workspace/modals.rs` (`cancel_active_modal`, any close helpers)
- Modify: `crates/app/src/workspace/render.rs` (tooltip labels if incomplete/inaccurate)
- Modify: `crates/ui/src/tab.rs`, `crates/ui/src/transfer_row.rs` if labels weak
- Test: `crates/app/src/workspace/tests.rs`
- Update: `docs/plans/2026-07-14-phase6-polish-audit.md` region matrix and §15 #3/#8

**Interfaces:**
- Consumes: existing `focus_pane`, `close_connect_form`, `close_command_palette`, `close_go_to_path`, `cancel_delete_confirm`
- Produces:
  - Every Esc/close path for overlays restores focus to a keyboard-usable control
  - Test(s): `about_escape_restores_pane_focus`, `go_to_path_escape_restores_pane_focus` (or one parameterized pair)

**Required correction for a known deficiency:**

```rust
// cancel_active_modal — About branch
if self.about_open {
    self.about_open = false;
    self.focus_pane(self.focused_side, window, cx); // add
    cx.notify();
    return;
}
```

Inspect **all** branches of `cancel_active_modal` and every dedicated close helper:

| Overlay | Expected close focus |
| --- | --- |
| palette | `close_command_palette` → `focus_pane` (already implemented) |
| tab switcher | must restore pane |
| go_to_path | `close_go_to_path` → pane (already implemented) |
| about | **required correction** → pane |
| settings surface | `workspace_focus` OK if keyboard still works; prefer pane if Files |
| delete_confirm | `cancel_delete_confirm` → pane |
| context_menu / inline_edit | pane or list |
| connect_form | `close_connect_form` → pane |
| host key / conflict | Some reject/resolve paths already call `focus_pane`; verify every path |

**Tooltip audit procedure for this task:**

```bash
# List every icon_button call; each must pass a non-empty user string as tooltip
rg -n "icon_button\(" crates/app/src crates/ui/src --type rust

# Interactive icons that are not icon_button; add a tooltip or convert the control
rg -n "\.on_click\(" crates/app/src/workspace/render.rs -A2 | head -80
```

For each path-bar button, use `labeled_shortcut("Label", "ActionId")` when a corresponding palette action exists, consistent with the phase 4 pattern.

- [ ] **Step 1: Failing tests for focus restoration**

In `tests.rs`:

```rust
#[gpui::test]
fn about_escape_restores_pane_focus(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        ws.about_open = true;
        window.focus(&ws.modal_focus); // or whatever About uses
        ws.cancel_active_modal(window, cx);
        assert!(!ws.about_open);
        // After fix: focused handle is a pane focus handle
        let pane = ws.pane_focus(ws.focused_side).clone();
        assert!(
            pane.is_focused(window),
            "Esc from About must restore pane focus"
        );
    });
}

#[gpui::test]
fn go_to_path_escape_restores_pane_focus(cx: &mut TestAppContext) {
    let (workspace, mut cx, _channels) = init_workspace(cx);
    workspace.update_in(&mut cx, |ws, window, cx| {
        ws.open_go_to_path(window, cx);
        assert!(ws.go_to_path_open);
        ws.cancel_active_modal(window, cx);
        assert!(!ws.go_to_path_open);
        let pane = ws.pane_focus(ws.focused_side).clone();
        assert!(pane.is_focused(window), "Esc from Go to Path restores pane");
    });
}
```

Adapt the `is_focused` API to the interface provided by the current GPUI version, such as `FocusHandle::is_focused` or a window focus query. If the test harness cannot inspect focus, then verify a behavioral proxy: after Esc, the `SelectNextEntry` action still changes the selection, which demonstrates that the keyboard path remains functional.

- [ ] **Step 2: Execute the tests.** Expect the About test to fail; the go_to_path result depends on the current implementation.

```bash
cargo test -p macsftp-app --bin macsftp about_escape go_to_path_escape -- --nocapture
```

- [ ] **Step 3: Correct focus behavior and tooltip labels**

```rust
// modals.rs — About
if self.about_open {
    self.about_open = false;
    self.focus_pane(self.focused_side, window, cx);
    cx.notify();
    return;
}
```

Inspect every `rg icon_button` result. Correct empty or inaccurate labels, and convert any interactive bare icon to an appropriate control.

- [ ] **Step 4: Execute the tests again; all listed tests must pass**

```bash
cargo test -p macsftp-app --bin macsftp about_escape go_to_path_escape -- --nocapture
```

- [ ] **Step 5: Update the audit matrix.** Set §15 #8 to pass if the audit is complete, and set the About focus row to pass.

- [ ] **Step 6: Commit**

```bash
git add crates/app/src/workspace/modals.rs crates/app/src/workspace/render.rs \
  crates/ui/src/tab.rs crates/ui/src/transfer_row.rs \
  crates/app/src/workspace/tests.rs docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "fix(app): restore pane focus from About and complete icon tooltips"
```

---

### Task 3: Review user-facing text (PR-B)

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` (status messages)
- Modify: other app modules if the banlist identifies user-visible text
- Test: optional unit test for message constants; or assert via existing status tests
- Update: audit §15 #9

**Interfaces:**
- Produces: user-visible UI paths that do not contain banlist strings

**Required renames (minimum):**

| Current | Replacement |
| --- | --- |
| `Runtime is unavailable` | `Connection service is unavailable.` |
| `Runtime is busy — action dropped, try again` | `Busy — try again in a moment.` |

- [ ] **Step 1: Inspect banlist occurrences in app UI sources**

```bash
rg -n -i "runtime|actor|channel|session.epoch|AppCommand" \
  crates/app/src --type rust -g '!**/tests.rs'
```

Classify each result:

- Revise **user-visible** content, including `status_message`, button labels, empty_state, and modal body.
- Preserve comments, logs, and tracing output.
- Preserve code identifiers.

- [ ] **Step 2: Apply renames**

```rust
// send_command error paths in mod.rs
self.status_message = Some("Busy — try again in a moment.".into());
// ...
self.status_message = Some("Connection service is unavailable.".into());
```

Do **not** change `Keychain` strings, the host-key fingerprint display required by users, or file paths presented as paths.

- [ ] **Step 3: Guard test (optional but preferred)**

```rust
#[test]
fn user_status_strings_avoid_internal_jargon() {
    // Keep the two known status templates as named constants or re-export
    // from a tiny module if you extract them; otherwise document manual grep
    // in audit. If extracted:
    // assert!(!BUSY_MSG.to_lowercase().contains("runtime"));
}
```

If constants are not extracted, record in the audit that the `rg` review on 2026-07-14 found no prohibited terms in user-visible paths.

- [ ] **Step 4: Execute app tests that might assert the previous strings**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

Update every test that expected the previous text.

- [ ] **Step 5: Set audit §15 #9 to pass, and then commit**

```bash
git add crates/app/src docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "fix(app): replace internal jargon in user-facing status strings"
```

---

### Task 4: Narrow window layout (PR-C)

**Files:**
- Modify: `crates/app/src/workspace/render.rs` (tab strip parent, path bar, status bar, drawer header)
- Modify: `crates/app/src/workspace/modals.rs` (modal footer button rows if needed)
- Modify: `crates/ui/src/tab.rs` / `transfer_row.rs` only if still overflow
- Update: audit §15 #4 + region Truncate column

**Interfaces:**
- Produces: flex children that must shrink use `.min_w_0()`, whereas long text uses `.truncate()`.

**Baseline:** `main.rs` defines `window_min_size: size(px(720.0), px(480.0))`. Do **not** reduce this value without a design change.

- [ ] **Step 1: Identify overflow risks**

```bash
rg -n "min_w_0|truncate" crates/app/src/workspace/render.rs crates/ui/src
```

Perform the following manual checks, and record the results in the audit Notes:

1. Start the app and resize the window to approximately 720×480.
2. Long tab title (connect to host with long name or rename title).
3. Deep path in path bar.
4. Display the transfer drawer with a job that has a long path.
5. Display the Connect and Delete modals.

- [ ] **Step 2: Correct each confirmed overflow site**

Patterns:

```rust
// Parent of truncating text
div().flex().min_w_0().flex_1().child(
    div().min_w_0().truncate().child(long_text)
)

// Modal action row: allow wrap instead of fixed single row overflow
div().flex().flex_wrap().gap_2().justify_end().children(buttons)
```

Do not introduce responsive breakpoints. Use only shrink, truncate, or wrap behavior.

- [ ] **Step 3: Do not add an automated pixel test.** Record the manual-test result in audit §15 #4 as `pass`, or as `accepted risk` with the remaining issues.

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/modals.rs \
  crates/ui/src docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "fix(ui): tighten narrow-window truncation and flex shrink"
```

If the manual test shows that no code change is required, update the audit to `pass` and commit **only the documentation**:

```bash
git commit -m "docs: mark Phase 6 narrow-window audit pass"
```

---

### Task 5: Performance smoke tests and manual-test section (PR-D)

**Files:**
- Modify: `crates/app/src/workspace/visible_entries.rs` (tests module)
- Update: `docs/plans/2026-07-14-phase6-polish-audit.md` (manual-test procedure and §15 #7)

**Interfaces:**
- Consumes: `visible_local_indices`, `visible_remote_indices`
- Produces: unit tests with 10_000 synthetic entries. These tests use **no** strict duration assertion in CI.

- [ ] **Step 1: Add smoke tests with 10k entries**

```rust
// visible_entries.rs tests
#[test]
fn visible_indices_handle_ten_thousand_entries() {
    let entries: Vec<LocalEntry> = (0..10_000)
        .map(|i| LocalEntry {
            name: format!("file-{i:05}.txt"),
            path: LocalPath::new(format!("/tmp/bulk/file-{i:05}.txt")),
            kind: FileKind::File,
            size: Some(i as u64),
            permissions: None,
            modified_at: None,
            link_target: None,
        })
        .collect();
    let all = visible_local_indices(&entries, true, "");
    assert_eq!(all.len(), 10_000);

    let filtered = visible_local_indices(&entries, true, "file-099");
    assert!(
        !filtered.is_empty() && filtered.len() < 10_000,
        "substring filter must reduce 10k set"
    );
    // Correctness only — do not assert elapsed time (flaky on CI)
}

#[test]
fn visible_remote_indices_handle_ten_thousand_with_hidden() {
    let mut entries: Vec<RemoteEntry> = (0..10_000)
        .map(|i| RemoteEntry {
            name: if i % 50 == 0 {
                format!(".hidden-{i}")
            } else {
                format!("entry-{i}")
            },
            path: RemotePath::new(format!("/data/{i}")),
            kind: FileKind::File,
            size: None,
            permissions: None,
            modified_at: None,
            link_target: None,
        })
        .collect();
    let shown = visible_remote_indices(&entries, false, "");
    assert_eq!(shown.len(), 10_000 - (10_000 / 50));
    let _ = visible_remote_indices(&entries, true, "entry-1");
}
```

If the example no longer matches `macsftp_core`, adjust it to the actual `LocalEntry` and `RemoteEntry` field sets.

- [ ] **Step 2: Execute the tests**

```bash
cargo test -p macsftp-app --bin macsftp visible_indices_handle_ten_thousand visible_remote_indices_handle_ten_thousand -- --nocapture
```

Expected result: PASS.

- [ ] **Step 3: Complete the manual-test section in the audit**

```markdown
## Hand performance smoke

**Setup**
1. Generate a local directory: `mkdir -p /tmp/macsftp-10k && seq -w 1 10000 | xargs -I{} touch /tmp/macsftp-10k/f{}`
2. Start macSFTP and navigate the local pane to that directory or a symlink to it.
3. Connect remote with large listing if available (or mock backend).
4. Start up to 4 transfers and retain 3 tabs.

**Observe**
- Scroll the file list; no operation should become unresponsive for multiple seconds.
- Use type-to-filter; the filter must update without incorrectly clearing the selection.
- Switch tabs and toggle the drawer; both interactions must remain responsive.
- Verify that progress updates remain throttled according to phase 2.

**Result:** _complete after manual verification_ — pass / issues
```

- [ ] **Step 4: Set §15 #7 to `pass` for automation**, and include the manual-test result.

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/visible_entries.rs docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "test(app): add 10k visible-entry smoke tests for phase 6"
```

---

### Task 6: Completion — full regression and checklist completion

**Files:**
- Modify: only `docs/plans/2026-07-14-phase6-polish-audit.md`, unless final verification identifies a Critical defect

- [ ] **Step 1: Execute the regression tests**

```bash
cargo test -p macsftp-platform -p macsftp-storage -p macsftp-app --bin macsftp 2>&1 | tail -40
```

Expected result: all tests pass, including at least 101 app tests and the new tests.

- [ ] **Step 2: Finalize the audit**

- Every §15 row is `pass` or `accepted risk` with a reason; therefore no `unknown` value remains.
- Complete the region matrix.
- Record the manual smoke-test result. If no large remote is available, use `accepted risk: no large remote available` and include a local-only note.

- [ ] **Step 3: Verify that no prohibited user-visible term remains**

```bash
rg -n -i "runtime is|actor|session epoch" crates/app/src --type rust -g '!**/tests.rs' || true
```

- [ ] **Step 4: Commit**

```bash
git add docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "docs: complete Phase 6 polish audit checklist"
```

If Step 1 identifies regressions, correct them in a separate `fix(app): …` commit before finalizing the audit.

---

## Self-Review (plan vs design)

| Design requirement | Task |
| --- | --- |
| Audit checklist §15 and region matrix | Task 1, updated by Tasks 2–6 |
| No new animations | Global Constraints |
| Tooltip completeness | Task 2 |
| Modal focus restore | Task 2 |
| Copy banlist and Runtime strings | Task 3 |
| Narrow window | Task 4 |
| 10k smoke test and manual-test documentation | Task 5 |
| PR division A–D | PR mapping and Tasks |
| No sftp changes | File Map / Constraints |

**Placeholder review:** No TBD steps remain; all strings, paths, and commands are concrete.

**Type consistency:** `focus_pane`, `cancel_active_modal`, and `visible_*_indices` match existing crate APIs. However, the test focus API may require the GPUI-specific adjustment described in Task 2.

---

## Execution Handoff

The plan is stored at `docs/plans/2026-07-14-phase6-polish-impl.md` according to project convention.

Two execution options are available:

1. **Subagent-Driven (recommended):** Assign each task to a new subagent, and review the result before the next task.
2. **Inline Execution:** Use this session with executing-plans checkpoints.

Select one execution option before implementation begins.
