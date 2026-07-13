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
| 8 | Icon-only tooltips? | unknown | Task 2 — `icon_button` requires tooltip; audit remaining icon-only |
| 9 | No secrets / internal jargon in UI? | unknown | Task 3 |
| 10 | Modal expiry / session_epoch safety? | pass | Phase 1+ core guards; reaffirm |

Status values: `pass` | `fail` | `unknown` | `accepted risk` (with reason).

## Region matrix

| Region | Tooltip | Focus open | Focus close | Truncate | Notes |
| --- | --- | --- | --- | --- | --- |
| Tab bar + close | pass | n/a | n/a | unknown | `ui/tab.rs` Close Tab via `icon_button`; new-tab has tooltip |
| Path bar (back/up/refresh/copy/…) | pass | n/a | n/a | unknown | `render.rs` `icon_button` + `labeled_shortcut`; back/forward use `text_tooltip` |
| Filter clear | pass | unknown | unknown | n/a | Clear Filter (Esc) via `icon_button` |
| Transfer drawer cancel/retry | pass | n/a | n/a | unknown | `transfer_row.rs` Cancel/Retry Transfer |
| Status bar transfer chip | unknown | n/a | n/a | unknown | Task 2 |
| Connect form | n/a | unknown | unknown | unknown | Text controls; Task 2 focus audit |
| Host key modal | n/a | unknown | pass? | unknown | `reject_host_key` calls `focus_pane`; open path Task 2 |
| Conflict modal | n/a | unknown | pass? | unknown | resolve path uses focus; Task 2 |
| Delete confirm | n/a | unknown | unknown | unknown | Task 2 |
| Go to Path | n/a | pass? | pass | n/a | Open focuses `modal_focus`; `close_go_to_path` → `focus_pane` |
| Command palette | n/a | unknown | unknown | unknown | Task 2 |
| About | n/a | fail? | fail | n/a | Esc/`cancel_active_modal` and Close button set `about_open=false` only — **no `focus_pane`** (`modals.rs` ~239–242; Close click ~1832–1836) |
| Settings surface | n/a | pass? | pass? | unknown | Esc returns to Files + `workspace_focus`; not pane focus |

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
| About Esc path | **Gap confirmed:** `cancel_active_modal` about branch returns without `focus_pane` |
| About Close button | Same gap: sets `about_open = false` only |
| About open | `ShowAbout` sets `about_open = true` without focusing About key context / pane restore bookkeeping |
