# Changelog

All notable user-visible and security changes are recorded here. The format is
based on Keep a Changelog, and releases use semantic versioning.

## [Unreleased]

### Added

- **Go to Folder** (⌘⇧G) opens pre-filled with the folder the pane is showing,
  selected, so reaching a sibling directory means editing one segment instead
  of retyping the whole path.
- The connect form's password and key-passphrase fields have a **Show** toggle
  for checking a long or jump-host secret without retyping it.
- Double-clicking (or pressing Enter on, or right-click Open for) a local file
  now hands it to its default application, matching Finder. Remote files keep
  the existing edit-in-temp-copy flow.
- Dragging a row that is part of a multi-selection moves every selected file;
  dropping it on the other pane transfers the whole group in one plan.
- Completed transfer rows gain a **Show in Finder** button that reveals the
  downloaded file; an upload's destination lives on the server and says so
  instead.

### Changed

- Status-bar messages now expire after six seconds instead of lingering until
  the next action happens to overwrite them.
- Finishing an upload or download now re-reads the pane that received the
  files, so new names appear without pressing ⌘R. The reload keeps the pane's
  selection and scroll position, and leaves panes showing other folders alone.
- Downloading several server files (or a folder) no longer stops at a
  single-file check: multiple sources land side by side in the local folder.
- **Copy Path** now copies the selected entries' own paths, one per line,
  matching Finder; an empty selection still copies the pane's folder. Right
  clicking a file and choosing Copy Path therefore yields
  `/a/b/file.txt` instead of `/a/b`.

## [0.6.0] - 2026-10-19

### Added

- Three-section Settings surface: **General** for appearance, behavior and
  transfer preferences, **Editor** for the external editor and its association
  rules, and **Saved Connections** for profile management.
- Immediate **Confirm before delete**, **Show hidden files**, **Preserve
  modification time** and **Preserve permissions** switches. The delete prompt's
  “Don’t ask again” choice is recoverable from General.
- Keyboard-reachable Settings sections and switches with standard toggle
  semantics (`aria_toggled`, Space/Enter activation).

### Changed

- Transfer metadata preferences apply to new uploads and downloads. Retries keep
  the policy captured by the original plan, and remote-edit uploads always keep
  their timestamp so the conflict baseline stays valid.
- Configuration save errors appear above the active Settings section, so Editor
  and Saved Connections failures are visible instead of being hidden with the
  General form.
- File-table sort indicators use vector arrows, selected rows keep their size
  and modified text readable, and available upload/download actions are
  emphasized in the path bar.
- Recent connections show a server icon, display name and a truncatable
  `user@host:port` endpoint with the full value in a tooltip.
- The transfer drawer uses a centered grabber, and Host Key and Transfer
  Conflict detail rows share an inset information block.

## [0.5.0] - 2026-10-19

### Added

- Pipelined SFTP downloads: 16 in-flight read requests, 256 KiB chunks, a 4 MiB
  channel window, and up to four concurrent transfers per plan (six overall).
- A `●` dirty marker on remote files whose temporary edit copy changed on disk,
  with an Accent-highlighted status bar indicator.
- External editor rules in **Settings → General**, allowing per-glob or per-extension
  routing to specific external applications (`open -a <app>`) with safe fallback to `/usr/bin/open -t`.
- **Compare Changes…** in the remote-change conflict modal, lazy-fetching a fresh
  copy of the remote file and opening a detected system diff tool (`opendiff` or `code --diff`).
- Dedicated `⌘⇧U` shortcut for **Upload Edited File**, preserving `⌘U` for **Upload Selection**.

### Changed

- Process-wide FSEvents monitoring watches saves of the temporary edit copy with
  an 800ms debounce and self-write suppression based on baseline size and mtime.
  Uploading stays strictly explicit: nothing is uploaded until you choose **Upload Edited File** (`⌘⇧U`).
- Expand `copy_file_chunks` buffer from 64 KiB to 256 KiB and configure 16 concurrent writes with a 30s timeout on transfer SFTP sessions.
- Replace recursive glob matching with an iterative single-backtrack matcher for editor rules, preventing exponential backtracking on complex patterns.
- Transfer scheduling runs a download plan's root directory job exclusively before child transfers, preventing destination clearing races.

### Fixed

- Prevent a 4 GiB length wrap in pipelined download chunk calculation and re-request short reads.
- Re-check cancellation after progress flushing in `transfer_planner` to prevent cancelled plans from emitting completion events.
- Ensure retried download plan roots lose destination clearing permissions once child jobs have started.

## [0.4.0] - 2026-10-18

### Added

- Add continuous (Shift-click) and discrete (Command-click or Ctrl-click) multi-selection
  in file panes, matching macOS Finder keyboard-and-mouse selection conventions.
- Replace self-drawn context menus with native AppKit `NSMenu` for both file panes and
  input fields, featuring mouse-following popups, keyboard navigation, item highlighting,
  separators, confirmation ellipsis, and VoiceOver support.
- Complete the macOS application menu bar: provide standard `Edit` actions (Undo, Redo,
  Cut, Copy, Paste, Select All via `MenuItem::os_action`), enrich `File`, `View`, and
  `Window` menus, and dynamically synchronize window titles to the active tab title.
- Add Finder-style type-select to file lists: typing letters on a focused list jumps the
  selection to the first matching entry without filtering out non-matching rows.
- Add Apple-native physical motion and tactile feedback: spring physics animation for the
  transfer drawer (`spring(0.3, 1.0)`), smooth `Presence` entry/exit transitions for modals
  and overlays, progress bar transition easing, and animated activity spinners, with
  automatic degradation when macOS "Reduce Motion" is enabled.
- Add accessibility metadata across the workspace: file rows, headers, and the status bar
  now expose standard `Role::List`, `Role::Button`, and detailed descriptions.
- Add hover tooltips displaying complete error messages for truncated transfer failure details.

### Changed

- Adopt `gpui-kit` as the single unified UI dependency, consolidating GPUI Base components,
  themes, and controls.
- Reorganize the path bar into navigation, breadcrumb, and action clusters with visual
  separators, with higher shrink rates on parent breadcrumbs to protect current-directory tail
  visibility on narrow viewports (§12.1).
- Indent the table header "Name" sort column by `ICON_SIZE + 8px` to align vertically with
  row file names.
- Improve Command Palette and dialog ergonomics: adapt Command Palette height to the window
  viewport to prevent bottom shortcut hint clipping on 480pt short windows, and use standard
  `IconName::Trash` for delete actions.
- Unify elevated surface tokens, shadows, and Settings section title typography.
- Change tab close buttons to reveal only on hover or when active, following Safari conventions.
- Upgrade core runtime and platform dependencies (`tokio`, `aws-lc-rs`, `rustix`, `objc2`).

### Fixed

- Safely recover poisoned locks on transfer channel tracking in `session_actor` without panicking,
  and unify warning logs across retries and cancellations.
- Remove obsolete dead-code variant `TransferPlanState::Running` and correct timestamp conversion
  documentation.
- Ensure selection anchors hidden by active filters do not corrupt subsequent Shift-selection ranges.

## [0.3.0] - 2026-09-20

### Added

- Add one-hop saved-profile jump hosts and explicit OpenSSH-style
  ProxyCommand transports.
- Add multi-round keyboard-interactive and SSH-agent authentication, with an
  optional explicit agent socket for apps launched outside a shell.
- Add RSA-SHA2 client private-key authentication using AWS-LC for signing,
  without enabling the RustCrypto RSA implementation affected by
  RUSTSEC-2023-0071.

### Changed

- Make remote-edit uploads explicit. Saving in the external editor no longer
  triggers a background watcher; users choose **Upload Modified File**, after
  which macSFTP performs the same live remote-snapshot and conflict checks
  before uploading.
- Make **Settings → Profiles** the only place that creates, edits, or deletes
  saved profiles. The Connect dialog now only selects a saved profile or starts
  a temporary connection, with **Manage…** linking to Settings.
- Expose only the real SSH backend from the production SFTP runtime; mock
  actors and constructors now compile only for crate unit tests.
- Remove protocol commands, events, states, and historical implementation
  documents that no longer had production consumers.
- Migrate `profiles.json` to version 5 and remove the never-consumed
  `group_id` placeholder. Profile grouping remains an explicit future product
  decision rather than a dormant persisted field.
- Close the low-level profile-file writer, test-only connection helper, and
  crate-link probe APIs so production callers use the canonical owners.
- Make connection setup cancellation-aware and let runtime shutdown drain
  session tasks within its configured timeout before forcing termination.
- Keep process-wide transfer, residual-temp, and remote-edit events out of the
  Workspace production reducer, and diagnose residual cleanup commands that
  cannot reach a live actor.
- Redesign the Settings profile list with compact two-line rows, endpoint
  truncation, port badges, clearer selection, result counts, and scrolling.

### Fixed

- Preserve the local editable copy and retryable edit session when macSFTP
  cannot read temporary-file metadata before an explicit upload.

## [0.2.1] - 2026-09-13

### Fixed

- Avoid repeated full-directory scans when selecting all files, extending a
  selection, or shrinking a selection in large local and remote directories.
- Replace selection anchors hidden by filtering so later Shift selection does
  not reselect files from the old range.

## [0.2.0] - 2026-08-24

### Changed

- Mid-session disconnects now show the classified cause in the remote pane —
  "Server closed the connection" or "Connection lost" with a network-timeout
  hint — instead of a generic "Disconnected" message, with the same Reconnect
  and Edit actions as before.
- Private-key profiles now express three passphrase states: no passphrase,
  ask every time without storing it, or remember it in the Keychain. The
  profile editor offers the matching policy toggle; previously any typed
  passphrase was always remembered and a saved one could never be removed.
  Existing profiles.json files migrate automatically.

### Fixed

- Hardened profile storage end to end: phased atomic-write failures keep new
  secrets when a rename lands, unsupported file versions enter read-only
  recovery instead of being overwritten, schema violations are reported as
  corrupt rather than silently repaired, deleted profile ids are never
  reused, and concurrent app instances serialize writes through an advisory
  lock instead of clobbering each other's recents and settings.
- Deleting a profile no longer leaves stale links in recent connections or
  restored tabs; remote-edit sessions follow the live connection identity,
  so re-pointing a profile at another host cannot hit an old server's edit
  session.

## [0.1.1] - 2026-08-05

### Changed

- Redesign the delete confirmation dialog with clearer destructive-action
  hierarchy, file and folder previews, concise permanent-delete guidance, and
  responsive sizing in light and dark themes.

## [0.1.0] - 2026-08-01

### Security

- Prevent host-key fingerprints and complete event payloads from reaching logs.
- Replace credential-derived connection-pool hashes with non-secret profile or
  session identities.
- Persist application-owned state with private permissions, durable atomic
  replacement, and write blocking after a corrupt file is detected.
- Treat matching OpenSSH `@revoked` host entries as a hard connection block.

### Added

- Docker-backed successful password-authentication release gate.
- Isolated real macOS Keychain release gate.
- Native GUI release evidence and tag validation policy.
- OpenSSH hashed-host matching for user known_hosts files.
- macOS local-network privacy declaration and a recovery shortcut to the
  matching System Settings pane.

### Changed

- Make the workspace package version the single source for every first-party
  crate, the About surface, and the macOS bundle metadata.
- Rotate diagnostic logs daily with bounded retention and write payload-free
  crash markers.

### Fixed

- Classify macOS local-network privacy denial separately from SSH protocol
  failures and show actionable recovery guidance without exposing raw transport
  errors.
