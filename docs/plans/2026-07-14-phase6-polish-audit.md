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
| 4 | Narrow window no overflow? | unknown | Task 4 |
| 5 | No decorative cards/gradients? | pass | Theme tokens only |
| 6 | No main-thread block on network? | pass | Runtime bridge; residual risk accepted |
| 7 | 10k entries + multi transfer? | unknown | Task 5 smoke |
| 8 | Icon-only tooltips? | pass | Task 2: all `icon_button` sites + path-bar back/forward + status transfer chip carry labels (`labeled_shortcut` where applicable) |
| 9 | No secrets / internal jargon in UI? | unknown | Task 3 |
| 10 | Modal expiry / session_epoch safety? | pass | Phase 1+ core guards; reaffirm |

Status values: `pass` | `fail` | `unknown` | `accepted risk` (with reason).

## Region matrix

| Region | Tooltip | Focus open | Focus close | Truncate | Notes |
| --- | --- | --- | --- | --- | --- |
| Tab bar + close | pass | n/a | n/a | unknown | `ui/tab.rs` Close Tab via `icon_button`; new-tab has `labeled_shortcut` |
| Path bar (back/up/refresh/copy/…) | pass | n/a | n/a | unknown | `render.rs` `icon_button` + `labeled_shortcut`; back/forward use `text_tooltip` |
| Filter clear | pass | n/a | pass | n/a | Clear Filter (Esc) via `icon_button`; clear restores pane focus |
| Transfer drawer cancel/retry | pass | n/a | n/a | unknown | `transfer_row.rs` Cancel/Retry Transfer |
| Status bar transfer chip | pass | n/a | n/a | unknown | `labeled_shortcut("Toggle Transfers", "ShowTransferDrawer")` |
| Connect form | n/a | pass | pass | unknown | `close_connect_form` → `focus_pane` |
| Host key modal | n/a | pass | pass | unknown | `reject_host_key` / trust paths call `focus_pane` |
| Conflict modal | n/a | pass | pass | unknown | `resolve_transfer_conflict` → `focus_pane` |
| Delete confirm | n/a | pass | pass | unknown | `cancel_delete_confirm` → `focus_pane` |
| Go to Path | n/a | pass | pass | n/a | Open → `modal_focus`; Esc/`close_go_to_path` → `focus_pane` (tested) |
| Command palette | n/a | pass | pass | unknown | Open → `modal_focus`; `close_command_palette` → `focus_pane` |
| Tab switcher | n/a | pass | pass | n/a | Open → `modal_focus`; `close_tab_switcher` → `focus_pane` |
| Context menu / inline edit | n/a | n/a | pass | n/a | Esc closes then `focus_pane` |
| About | n/a | n/a | pass | n/a | Esc + Close → `close_about` → `focus_pane` (tested) |
| Settings surface | n/a | pass | pass | unknown | Open → `workspace_focus`; Esc/Done → Files + `focus_pane` |

## Hand performance smoke (Task 5)

See section filled in Task 5.

## Copy banlist (user-visible)

Forbidden substrings (case-insensitive) in UI labels/status: `runtime`, `actor`, `channel`, `session epoch`, `AppCommand`, `crate`.
Allowed: `Keychain`, host/port/profile/transfer/permission.

## Spot-check log (PR0 / Task 1)

| Check | Result |
| --- | --- |
| `window_min_size` | Confirmed 720×480 in `crates/app/src/main.rs` |
| `icon_button` API | Exists in `crates/ui/src/components.rs`; **requires** `tooltip_label`; call sites in tab bar, path bar, filter clear, transfer rows |
| About Esc path | **Fixed (Task 2):** `cancel_active_modal` → `close_about` → `focus_pane` |
| About Close button | **Fixed (Task 2):** Close click → `close_about` → `focus_pane` |
| About open | `ShowAbout` sets `about_open = true`; Esc is workspace-level `CancelActiveModal` (no modal focus required) |
| Tooltip audit | All `icon_button` sites have non-empty labels; bare clickable icons (back/forward/status chip) use `text_tooltip` |
