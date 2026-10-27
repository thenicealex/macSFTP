# ADR-0001: One execution path for app-level commands

- **Status:** Accepted
- **Date:** 2026-10-27
- **Scope:** `macsftp-app` command surface (menu bar, keybindings, command palette)
- **Related:** `docs/architecture.md` §12.2

## Context

The app exposes app-level commands through five production declarations and one test-only table:

- action types in `crates/app/src/app_actions.rs`;
- keybindings in the same file;
- the menu tree in `crates/app/src/app_menus.rs`;
- palette rows (title, keywords, chord, availability) in `crates/app/src/palette_commands.rs`;
- a string dispatcher, `dispatch_palette_id`, in `crates/app/src/workspace/command_palette.rs`;
- a documented-commands table in the `app_menus` tests that ties menu path, chord, and palette id together.

At the time of this decision the palette had 34 rows and the dispatcher had 34 arms; the sets were identical, and every one of those 34 ids also had a real `.on_action` handler on the Workspace root. The dispatcher was therefore a second implementation of behavior that already existed, not an adapter over it. The same six-condition "is it safe to switch surface" guard for `OpenSettings`, `OpenProfiles`, and `ShowAbout` existed verbatim in both the dispatcher and the handlers.

GPUI already supplies the shared key. Every action type registers itself by name (`Action::name_for_type`). `FocusHandle::dispatch_action` reaches the handlers synchronously, but palette commands run from listeners on the Workspace itself, and a synchronous dispatch would re-enter the Workspace update, which GPUI rejects. Deferring the dispatch past the current update removes that re-entry. The Workspace root both tracks `workspace_focus` and carries all the `.on_action` handlers, and all 34 palette commands have exactly one handler, on that root.

## Decision

1. A palette row carries a constructor for its action in addition to its id, title, keywords, chord, and availability. The palette stops knowing any Workspace method, guard, or command semantics.
2. Executing a palette row copies the constructor, closes the palette, and dispatches the action on the Workspace root in a deferred effect. The root, unlike the focused pane, renders on every surface. `dispatch_palette_id` is deleted.
3. The surface-switch guard becomes one private Workspace predicate used by the three handlers. The dispatcher's copies disappear with the dispatcher.
4. Menu topology, search metadata, and chord display remain separate declarations. They change for different reasons — HIG layout, discoverability copy, key bindings — and share exactly one thing: the action.

The deepened module is the command execution path. The action type and its single handler are the interface; the palette becomes a view over it.

## Alternatives considered

- **One static command table owning action, menu placement, search metadata, and chord.** Rejected. It concentrates data, not complexity: deleting it leaves each presentation concern with its own small declaration, so the deletion test does not concentrate anything. Its interface would be one wide record that unrelated changes all edit, and it would couple HIG layout to palette copy to key bindings.
- **Registry lookup by name (`App::build_action("macsftp::…")`) instead of a constructor field.** Rejected. The lookup is runtime-fallible, requires translating short ids to the namespaced registry key, and moves a stale-id failure from compile time to a test. A constructor field is compile-time checked and keeps the row self-contained.
- **Only deduplicating the surface guard.** Rejected. It leaves two execution paths, which is the source of the drift; the guard duplication is a symptom.

## Consequences

- Adding or changing a palette command now requires the row and its handler; there is no second dispatch to keep in sync.
- The palette data module gains a dependency on `gpui_kit::Action` through a function pointer per row. Accepted: the app crate already depends on the kit, and the row is the natural owner of "which action this row runs".
- 34 mechanical row edits and one field; roughly 60 lines net removed from production code, with the freed budget going into palette execution tests.
- The menu/keymap/palette consistency test stays: menu placement, palette id, and chord must still agree. The chord display stays hand-written because GPUI can enumerate bindings but does not format them as macOS glyphs, and the palette's focus context hides pane-scoped bindings.
- New tests: every row's action name matches `macsftp::{id}`; policy parity — with a modal open, executing the `OpenSettings` row leaves the surface unchanged, without one it switches; and a row runs from the Settings surface, where the file panes are not rendered.
- Palette execution tests assert the palette closed inside the update, then flush effects before asserting what the action did, because dispatch is deferred.
- `docs/architecture.md` §12.2 gains one sentence: palette rows dispatch the action and never call a handler directly.

## Out of scope

Parameterized actions (`ConnectRecent`, `CopyRecentEndpoint`, `ForgetRecent`), Edit-menu `os_action` input commands, and window/app lifecycle commands (New Window, Close Window, Minimize, Zoom, Bring All to Front, Hide/Quit) keep their current placement and do not enter the palette.

## Revisit when

- Command declarations start drifting again despite the shared action key. Then reconsider a single table, with the deletion test in hand.
- GPUI changes deferred effects or allows re-entrant dispatch. If a palette row must complete before its update ends, revisit the deferral; until then the deferred path is the one that works.
- The palette's chord strings and the keymap diverge in review. Derive or generate the display string instead of adding another copy.
