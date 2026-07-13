# Phase 6 Polish & Guidelines Compliance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Close out the UX improvement track against guidelines §11–§15: maintainable audit checklist, icon-only tooltips + modal focus restoration, user-facing copy without internal jargon, narrow-window overflow fixes, and light 10k list smoke tests — without new features or animations.

**Architecture:** Audit-driven polish. No new crates. Document gaps in `docs/plans/…-phase6-polish-audit.md`, then fix by theme (a11y → copy → narrow → perf). Prefer surgical edits in `app`/`ui` and pure tests in `visible_entries`.

**Tech Stack:** Rust, GPUI (`FocusHandle`, `icon_button`, `text_tooltip`, `min_w_0`/`truncate`), existing workspace modals/render, `visible_entries` pure helpers.

**Spec:** `docs/plans/2026-07-14-phase6-polish-design.md`

## Global Constraints

- **No new animations** — keep existing hover opacity only; no drawer/tab/modal transitions.
- **No new product features** — no transfer policy, multi-window session redesign, i18n framework, SFTP changes.
- **Keychain** may appear in user UI; ban **runtime / actor / channel / session epoch / crate / AppCommand** in user-visible strings.
- **Performance:** light correctness smoke only — no flaky CI millisecond gates.
- **Tooltip:** every **clickable** icon-only control must have tooltip/label; decorative icons exempt.
- Modal focus: open focuses a sensible control; close/Esc restores **pane** focus (not a full focus stack).
- No `unwrap`/`expect` on recoverable paths (AGENTS.md §5). Prefer `src/foo.rs` over `mod.rs`.
- Do **not** modify `crates/sftp/**` behavior or core transfer/session protocols.
- Surgical diffs only — no drive-by refactors.

## File Map

| File | Responsibility |
| --- | --- |
| **Create** `docs/plans/2026-07-14-phase6-polish-audit.md` | §15 checklist + region matrix; updated as tasks complete |
| **Modify** `crates/app/src/workspace/modals.rs` | `cancel_active_modal` focus gaps (About, etc.) |
| **Modify** `crates/app/src/workspace/render.rs` | tooltip labels, narrow flex/`truncate`, status bar chip |
| **Modify** `crates/app/src/workspace/mod.rs` | user-facing status strings (`Runtime is…`) |
| **Modify** `crates/app/src/workspace/connect_form.rs` / `transfers.rs` / `file_ops.rs` / `panes.rs` / `event_handling.rs` | copy sweep if jargon found |
| **Modify** `crates/ui/src/tab.rs` / `transfer_row.rs` / `components.rs` | tooltip completeness; narrow layout if gaps |
| **Modify** `crates/app/src/workspace/visible_entries.rs` | 10k filter smoke tests |
| **Modify** `crates/app/src/workspace/tests.rs` | modal Esc → pane focus tests |
| **Modify** `docs/plans/2026-07-14-phase6-polish-audit.md` (again) | mark pass after each theme |
| **Do not modify** | `crates/sftp/**` production paths; session/recents storage (phase 5 done) |

## Suggested PR mapping

| PR | Tasks | Notes |
| --- | --- | --- |
| PR0+A | Task 1–2 | Audit doc + a11y/focus |
| PR-B | Task 3 | Copy |
| PR-C | Task 4 | Narrow |
| PR-D | Task 5 | Perf smoke + hand-test section in audit |
| Closeout | Task 6 | Checklist all green + full regression |

---

### Task 1: Audit checklist document (PR0)

**Files:**
- Create: `docs/plans/2026-07-14-phase6-polish-audit.md`

**Interfaces:**
- Produces: living checklist consumed by later tasks (status cells updated in Tasks 2–6)

- [ ] **Step 1: Create the audit file** with the following structure (fill initial **Status** honestly from code inspection; use `unknown` until Task 2–5 verify):

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

- [ ] **Step 2: Spot-check code once** and set any cells you already know (e.g. `icon_button` call sites exist; About Esc path in `cancel_active_modal` does not call `focus_pane`).

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

### Task 2: A11y — tooltips + modal focus (PR-A)

**Files:**
- Modify: `crates/app/src/workspace/modals.rs` (`cancel_active_modal`, any close helpers)
- Modify: `crates/app/src/workspace/render.rs` (tooltip labels if incomplete/inaccurate)
- Modify: `crates/ui/src/tab.rs`, `crates/ui/src/transfer_row.rs` if labels weak
- Test: `crates/app/src/workspace/tests.rs`
- Update: `docs/plans/2026-07-14-phase6-polish-audit.md` region matrix + §15 #3/#8

**Interfaces:**
- Consumes: existing `focus_pane`, `close_connect_form`, `close_command_palette`, `close_go_to_path`, `cancel_delete_confirm`
- Produces:
  - Every Esc/close path for overlays restores keyboard-usable focus
  - Test(s): `about_escape_restores_pane_focus`, `go_to_path_escape_restores_pane_focus` (or one parameterized pair)

**Known gap to fix (required):**

```rust
// cancel_active_modal — About branch
if self.about_open {
    self.about_open = false;
    self.focus_pane(self.focused_side, window, cx); // add
    cx.notify();
    return;
}
```

Audit **all** branches of `cancel_active_modal` and dedicated close helpers:

| Overlay | Expected close focus |
| --- | --- |
| palette | `close_command_palette` → `focus_pane` (already) |
| tab switcher | must restore pane |
| go_to_path | `close_go_to_path` → pane (already) |
| about | **fix** → add pane |
| settings surface | `workspace_focus` OK if keyboard still works; prefer pane if Files |
| delete_confirm | `cancel_delete_confirm` → pane |
| context_menu / inline_edit | pane or list |
| connect_form | `close_connect_form` → pane |
| host key / conflict | reject/resolve paths already call `focus_pane` in places — verify |

**Tooltip audit procedure (do in this task):**

```bash
# List every icon_button call — each must pass a non-empty user string as tooltip
rg -n "icon_button\(" crates/app/src crates/ui/src --type rust

# Clickable icons that are NOT icon_button (must gain tooltip or convert)
rg -n "\.on_click\(" crates/app/src/workspace/render.rs -A2 | head -80
```

For each path-bar button, prefer `labeled_shortcut("Label", "ActionId")` when a palette action exists (phase 4 pattern).

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

Adjust `is_focused` API to whatever GPUI version exposes (`FocusHandle::is_focused` / window focus query). If test harness cannot read focus, assert behavioral proxy: after Esc, `SelectNextEntry` action still moves selection (keyboard path works).

- [ ] **Step 2: Run — expect FAIL on About (and pass/fail on go_to_path depending on current code)**

```bash
cargo test -p macsftp-app --bin macsftp about_escape go_to_path_escape -- --nocapture
```

- [ ] **Step 3: Implement focus fixes + tooltip label fixes**

```rust
// modals.rs — About
if self.about_open {
    self.about_open = false;
    self.focus_pane(self.focused_side, window, cx);
    cx.notify();
    return;
}
```

Walk `rg icon_button` results; fix empty/wrong labels; convert clickable bare icons if any.

- [ ] **Step 4: Re-run tests — PASS**

```bash
cargo test -p macsftp-app --bin macsftp about_escape go_to_path_escape -- --nocapture
```

- [ ] **Step 5: Update audit matrix** (§15 #8 pass if complete; About focus row pass)

- [ ] **Step 6: Commit**

```bash
git add crates/app/src/workspace/modals.rs crates/app/src/workspace/render.rs \
  crates/ui/src/tab.rs crates/ui/src/transfer_row.rs \
  crates/app/src/workspace/tests.rs docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "fix(app): restore pane focus from About and complete icon tooltips"
```

---

### Task 3: User-facing copy sweep (PR-B)

**Files:**
- Modify: `crates/app/src/workspace/mod.rs` (status messages)
- Modify: other app modules if banlist hits (grep-driven)
- Test: optional unit test for message constants; or assert via existing status tests
- Update: audit §15 #9

**Interfaces:**
- Produces: banlist strings absent from user-visible UI paths

**Required renames (minimum):**

| Current | Replacement |
| --- | --- |
| `Runtime is unavailable` | `Connection service is unavailable.` |
| `Runtime is busy — action dropped, try again` | `Busy — try again in a moment.` |

- [ ] **Step 1: Grep banlist in app UI sources**

```bash
rg -n -i "runtime|actor|channel|session.epoch|AppCommand" \
  crates/app/src --type rust -g '!**/tests.rs'
```

Classify each hit:
- **User-visible** (`status_message`, button labels, empty_state, modal body) → rewrite
- **Comment / log / tracing** → leave
- **Code identifiers** → leave

- [ ] **Step 2: Apply renames**

```rust
// send_command error paths in mod.rs
self.status_message = Some("Busy — try again in a moment.".into());
// ...
self.status_message = Some("Connection service is unavailable.".into());
```

Do **not** change: `Keychain` strings, host-key technical fingerprint display (user-needed), file paths shown as paths.

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

If not extracting constants, record in audit: `rg` clean on 2026-07-14 for user paths.

- [ ] **Step 4: Run app tests that might assert old strings**

```bash
cargo test -p macsftp-app --bin macsftp -- --nocapture
```

Fix any test that expected old copy.

- [ ] **Step 5: Update audit §15 #9 → pass; commit**

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
- Produces: flex children that must shrink use `.min_w_0()`; long text uses `.truncate()`

**Baseline:** `window_min_size: size(px(720.0), px(480.0))` in `main.rs` — **do not lower** without design change.

- [ ] **Step 1: Inventory overflow risks**

```bash
rg -n "min_w_0|truncate" crates/app/src/workspace/render.rs crates/ui/src
```

Hand-check (document in audit Notes):

1. Launch app, resize to ~720×480.
2. Long tab title (connect to host with long name or rename title).
3. Deep path in path bar.
4. Open transfer drawer with long path job.
5. Open Connect + Delete modals.

- [ ] **Step 2: Fix concrete overflow sites found**

Patterns:

```rust
// Parent of truncating text
div().flex().min_w_0().flex_1().child(
    div().min_w_0().truncate().child(long_text)
)

// Modal action row: allow wrap instead of fixed single row overflow
div().flex().flex_wrap().gap_2().justify_end().children(buttons)
```

Do not invent responsive breakpoints; only shrink/truncate/wrap.

- [ ] **Step 3: No automated pixel test** — note hand-test result in audit §15 #4 → `pass` or `accepted risk` with remaining issues.

- [ ] **Step 4: Commit**

```bash
git add crates/app/src/workspace/render.rs crates/app/src/workspace/modals.rs \
  crates/ui/src docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "fix(ui): tighten narrow-window truncation and flex shrink"
```

If hand-test finds zero code changes needed, still update audit to `pass` and commit **docs only**:

```bash
git commit -m "docs: mark Phase 6 narrow-window audit pass"
```

---

### Task 5: Performance smoke — tests + hand-test section (PR-D)

**Files:**
- Modify: `crates/app/src/workspace/visible_entries.rs` (tests module)
- Update: `docs/plans/2026-07-14-phase6-polish-audit.md` (hand-test procedure + §15 #7)

**Interfaces:**
- Consumes: `visible_local_indices`, `visible_remote_indices`
- Produces: unit tests with 10_000 synthetic entries; **no** strict duration assert in CI

- [ ] **Step 1: Write 10k smoke tests**

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

Match real `LocalEntry` / `RemoteEntry` field sets from `macsftp_core` if the sketch drifts.

- [ ] **Step 2: Run**

```bash
cargo test -p macsftp-app --bin macsftp visible_indices_handle_ten_thousand visible_remote_indices_handle_ten_thousand -- --nocapture
```

Expected: PASS.

- [ ] **Step 3: Fill hand-test section in audit**

```markdown
## Hand performance smoke

**Setup**
1. Generate local dir: `mkdir -p /tmp/macsftp-10k && seq -w 1 10000 | xargs -I{} touch /tmp/macsftp-10k/f{}`
2. Open macSFTP, navigate local pane to that dir (or symlink).
3. Connect remote with large listing if available (or mock backend).
4. Start up to 4 transfers; keep 3 tabs.

**Observe**
- Scroll file list: no multi-second freezes
- Type-to-filter: filter updates without clearing selection incorrectly
- Switch tabs / toggle drawer: responsive
- Progress updates remain throttled (phase 2)

**Result:** _fill after hand run_ — pass / issues
```

- [ ] **Step 4: Update §15 #7** to `pass` (automation) + hand result note

- [ ] **Step 5: Commit**

```bash
git add crates/app/src/workspace/visible_entries.rs docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "test(app): add 10k visible-entry smoke tests for phase 6"
```

---

### Task 6: Closeout — full regression + checklist completion

**Files:**
- Modify: `docs/plans/2026-07-14-phase6-polish-audit.md` only (unless last-minute Critical fixes)

- [ ] **Step 1: Run regression**

```bash
cargo test -p macsftp-platform -p macsftp-storage -p macsftp-app --bin macsftp 2>&1 | tail -40
```

Expected: all pass (app ≥ 101 tests + new ones).

- [ ] **Step 2: Finalize audit**

- Every §15 row is `pass` or `accepted risk` with reason (no `unknown` left).
- Region matrix complete.
- Hand smoke Result filled (or `accepted risk: no large remote available` with local-only note).

- [ ] **Step 3: Self-scan for residual banlist**

```bash
rg -n -i "runtime is|actor|session epoch" crates/app/src --type rust -g '!**/tests.rs' || true
```

- [ ] **Step 4: Commit**

```bash
git add docs/plans/2026-07-14-phase6-polish-audit.md
git commit -m "docs: complete Phase 6 polish audit checklist"
```

If Step 1 finds regressions, fix in a separate commit first (`fix(app): …`), then finalize audit.

---

## Self-Review (plan vs design)

| Design requirement | Task |
| --- | --- |
| Audit checklist §15 + region matrix | Task 1, updated 2–6 |
| No new animations | Global Constraints |
| Tooltip completeness | Task 2 |
| Modal focus restore | Task 2 |
| Copy banlist + Runtime strings | Task 3 |
| Narrow window | Task 4 |
| 10k smoke + hand test doc | Task 5 |
| PR split A–D | PR mapping + Tasks |
| No sftp changes | File Map / Constraints |

**Placeholder scan:** no TBD steps; concrete strings, paths, commands.

**Type consistency:** `focus_pane` / `cancel_active_modal` / `visible_*_indices` match existing crate APIs; test focus API may need GPUI-specific adjustment noted in Task 2.

---

## Execution Handoff

Plan saved to `docs/plans/2026-07-14-phase6-polish-impl.md` (project convention).

**Two execution options:**

1. **Subagent-Driven (recommended)** — fresh subagent per task, review between tasks  
2. **Inline Execution** — this session with executing-plans checkpoints  

Which approach?
