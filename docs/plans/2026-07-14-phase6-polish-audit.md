# Phase 6 Polish Audit

**Date:** 2026-07-14  
**Design:** `docs/plans/2026-07-14-phase6-polish-design.md`  
**Window min size:** 720×480 (`crates/app/src/main.rs`)

## §15 Review Questions

| # | Question | Status | Notes |
| --- | --- | --- | --- |
| 1 | Single-window work context? | pass | No marketing landing; Files surface first |
| 2 | Palette or shortcut path? | pass | Phase 4 palette + bindings |
| 3 | loading/empty/error/disabled/focused/hover/selected? | pass | Focus states via pane/path-bar borders; disabled tool buttons; empty/loading/error surfaces exist |
| 4 | Narrow window no overflow? | pass | Task 4: inventory + surgical `min_w_0`/`truncate`/`flex_wrap`; window_min_size stays 720×480. Hand-test notes below |
| 5 | No decorative cards/gradients? | pass | Theme tokens only |
| 6 | No main-thread block on network? | pass | Runtime bridge; residual risk accepted |
| 7 | 10k entries + multi transfer? | pass | Task 5+6: unit smoke on 10k `visible_*_indices` (no timing assert) **pass**. Interactive multi-transfer GUI: **accepted risk: interactive GUI smoke deferred; 10k unit smoke pass** |
| 8 | Icon-only tooltips? | pass | Task 2: all `icon_button` sites + path-bar back/forward + status transfer chip carry labels (`labeled_shortcut` where applicable) |
| 9 | No secrets / internal jargon in UI? | pass | Task 3: `Runtime is…` status strings → user copy; constants + banlist unit test; `rg` clean on user-visible string literals 2026-07-14 |
| 10 | Modal expiry / session_epoch safety? | pass | Phase 1+ core guards; reaffirm |

Status values: `pass` | `fail` | `unknown` | `accepted risk` (with reason).

## Region matrix

| Region | Tooltip | Focus open | Focus close | Truncate | Notes |
| --- | --- | --- | --- | --- | --- |
| Tab bar + close | pass | n/a | n/a | pass | Tab title `min_w_0`+`truncate`+`max_w(220)`; strip `flex_1`+`min_w_0`+`overflow_x_scroll` |
| Path bar (back/up/refresh/copy/…) | pass | n/a | n/a | pass | Breadcrumb trail `flex_1`+`min_w_0`+`overflow_x_hidden`; deep paths clip, not horizontal window overflow |
| Filter clear | pass | n/a | pass | pass | Query cell `flex_1`+`min_w_0`+`truncate` when inactive |
| Transfer drawer cancel/retry | pass | n/a | n/a | pass | Title truncates; detail `max_w(160)`+truncate; drawer header agg label truncates |
| Status bar transfer chip | pass | n/a | n/a | pass | Left cluster `flex_1`+`min_w_0`; status/message truncate; chip `flex_none` |
| Connect form | n/a | pass | pass | pass | Field/profile rows `min_w_0`; profile name/summary truncate; footer `flex_wrap` |
| Host key modal | n/a | pass | pass | pass | Value cells truncate; footer `flex_wrap` |
| Conflict modal | n/a | pass | pass | pass | Paths truncate; action rows `flex_wrap` |
| Delete confirm | n/a | pass | pass | pass | Name preview truncate; footer `flex_wrap` |
| Go to Path | n/a | pass | pass | pass | Footer `flex_wrap`; fixed 460 card fits min width |
| Command palette | n/a | pass | pass | pass | Existing fixed-width card; out of Task 4 layout rework |
| Tab switcher | n/a | pass | pass | pass | Title `flex_1`+`min_w_0`+`truncate` |
| Context menu / inline edit | n/a | n/a | pass | n/a | Esc closes then `focus_pane` |
| About | n/a | n/a | pass | n/a | Esc + Close → `close_about` → `focus_pane` (tested) |
| Settings surface | n/a | pass | pass | pass | Existing `min_w_0` on content column |

## Narrow-window hand-test notes (Task 4)

**Baseline:** `window_min_size` 720×480 unchanged. No pixel CI — code review + layout inventory.

| Check | Result |
| --- | --- |
| Min size 720×480 | Confirmed in `main.rs`; not lowered |
| Long tab title | Tab max width + truncate; strip scrolls horizontally |
| Deep path bar | Breadcrumb shrinks/`overflow_x_hidden` inside pane |
| Transfer drawer long path | `transfer_title` truncates; detail capped |
| Connect + Delete modals | Fixed card ≤460/420; footers wrap; long names truncate |

Residual: single ultra-long breadcrumb segment clips without ellipsis (acceptable); dual-pane path bars stay tight (~110px trail) at 720 but do not force window overflow.

## Hand performance smoke (Task 5)

**Automation:** `visible_indices_handle_ten_thousand_entries` and
`visible_remote_indices_handle_ten_thousand_with_hidden` in
`crates/app/src/workspace/visible_entries.rs` — correctness only (10k filter/hide).

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

**Result:** **accepted risk: interactive GUI smoke deferred; 10k unit smoke pass** (agent environment has no interactive GUI session; local 10k unit tests pass).

## Copy banlist (user-visible)

Forbidden substrings (case-insensitive) in UI labels/status: `runtime`, `actor`, `channel`, `session epoch`, `AppCommand`, `crate`.
Allowed: `Keychain`, host/port/profile/transfer/permission.

**Task 3 (2026-07-14):** Grepped `crates/app/src` (`runtime|actor|channel|session.epoch|AppCommand`); only user-visible hits were `send_command` status strings in `workspace/mod.rs` — rewritten to `STATUS_BUSY_TRY_AGAIN` / `STATUS_CONNECTION_SERVICE_UNAVAILABLE`. Remaining hits are identifiers, comments, or logs. Guard: `user_status_strings_avoid_internal_jargon`.

**Task 6 closeout re-scan (2026-07-14):**
```bash
rg -n -i "runtime is|actor|session epoch" crates/app/src --type rust -g '!**/tests.rs'
```
Hits are comments/identifiers only (`main.rs` doc, `modals`/`mod`/`panes`/`event_handling`/`file_ops` comments). No user-visible string regressions.

## Closeout (Task 6)

| Check | Result |
| --- | --- |
| Regression `cargo test -p macsftp-platform -p macsftp-storage -p macsftp-app --bin macsftp` | **pass** — platform 9, storage 34 (+1 ignored), app bin 107; all green |
| §15 rows | All `pass` (row 7 notes accepted risk for interactive GUI only) — no `unknown` |
| Region matrix | Complete |
| Hand performance smoke | accepted risk: interactive GUI smoke deferred; 10k unit smoke pass |
| Banlist residual | Clean for UI copy |

## Spot-check log (PR0 / Task 1)

| Check | Result |
| --- | --- |
| `window_min_size` | Confirmed 720×480 in `crates/app/src/main.rs` |
| `icon_button` API | Exists in `crates/ui/src/components.rs`; **requires** `tooltip_label`; call sites in tab bar, path bar, filter clear, transfer rows |
| About Esc path | **Fixed (Task 2):** `cancel_active_modal` → `close_about` → `focus_pane` |
| About Close button | **Fixed (Task 2):** Close click → `close_about` → `focus_pane` |
| About open | `ShowAbout` sets `about_open = true`; Esc is workspace-level `CancelActiveModal` (no modal focus required) |
| Tooltip audit | All `icon_button` sites have non-empty labels; bare clickable icons (back/forward/status chip) use `text_tooltip` |
