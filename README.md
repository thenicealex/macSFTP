# macSFTP

**English** | [简体中文](README.zh-CN.md) | [日本語](README.ja.md) | [한국어](README.ko.md)

A native macOS SFTP client, built in Rust. macSFTP keeps local and remote files
side by side in one window: connection tabs on top, two file panes in the
middle, transfers below. The interface is drawn with GPUI and GPUI Base; SSH and
SFTP run on `russh` and `russh-sftp` over Tokio.

macSFTP is pre-release software. It builds an unsigned app bundle for local use,
and public releases provide source only. Developer ID signing, notarization,
automatic updates, and protocols other than SFTP are outside the current scope.

## Screenshots

**Local files on the left, the server pane on the right**

![macSFTP dark workspace with sample files](docs/screenshots/workspace-dark.jpg)

**Connect with a saved profile or a one-off connection**

![macSFTP connection dialog with example connection details](docs/screenshots/connect-dark.jpg)

**Light appearance and the external editor setting**

![macSFTP light appearance and external editor setting](docs/screenshots/settings-light.jpg)

These captures use an isolated preview with local sample files. The server pane
is disconnected; `demo.example` and `deploy` are example connection details.

## Features

**Connections and security**

- Multiple windows, each with its own connection tabs
- Saved profiles, plus ad-hoc connections that stay out of the profile list
- Password, multi-round keyboard-interactive, SSH agent, and private-key
  authentication
- Ed25519, ECDSA, and RSA-SHA2 client keys; direct RSA signing uses AWS-LC
- Saved-profile jump hosts and explicit OpenSSH-style `ProxyCommand` routes
- OpenSSH-compatible host-key verification against a `known_hosts` subset
- Unknown hosts ask for a decision; a key that no longer matches the stored one
  blocks the connection
- Passwords and passphrases are kept in macOS Keychain; only short-lived copies
  stay in memory

**Browsing**

- Local and remote panes with back/forward history, breadcrumbs, and go-to-path
- Virtualized file lists that stay responsive at 10k entries
- Filtering, sorting, multi-select, Finder-style type-select, and hidden files
- Rename, new folder, delete, and copy path in either pane; Show in Finder for
  local files
- File and input context menus presented through native AppKit menus

**Transfers**

- Directory uploads and downloads modeled as plans, with streamed progress
- Pipelined downloads: 16 in-flight reads, 256 KiB chunks, up to 4 concurrent
  jobs per plan and 6 overall
- Conflict handling with per-plan "apply to all", cancellation, and retry
- A process-wide transfer drawer shared by every window
- Metadata preservation failures surface as warnings while the transferred data
  stays successful
- Symlinks are copied as links, not dereferenced

**Remote editing**

- Open a remote file (double-click, `↩`, or the context menu) to edit a
  temporary copy in your editor
- A `●` marker shows copies you changed; uploads start only from an explicit
  action
- An explicit **Upload Changes** action checks live remote metadata first
- Remote changes open a conflict decision, with **Compare Changes…** available
- Per-glob editor rules and dirty-state tracking in the status bar

**Interface**

- One Light and One Dark themes, following the system or pinned explicitly
- Command palette for app-level actions; window and app lifecycle commands stay
  in their standard menu positions
- Keyboard-navigable lists, modals, tabs, and Settings
- Transfer progress, errors, and empty states that state what to do next

## Connect to a server

Press `⌘T` to add a connection tab, then choose **Connect…** in the server pane
to enter the server, port, username, and sign-in method. With **Enter Details**
selected, the connection stays temporary: it is not saved as a profile and its
credentials are not stored, though the endpoint appears under Recent
Connections. To reuse it as a profile, pick a saved one, or open **Manage
Connections…**; profiles are created and edited in **Settings → Saved
Connections**.

Successful connections are remembered in the server pane's **Recent
Connections** list (up to six), with the display name, `user@host:port`, and the
last remote folder. Choosing one reconnects when its saved profile still has
usable credentials; otherwise the Connect form opens prefilled.

The first connection to an unknown host shows the server address, key type,
SHA-256 fingerprint, and the `known_hosts` file it would be saved to. The
primary action, **Trust and Save**, is an explicit decision; a key that no longer
matches the stored one blocks the connection.

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| `⌘N` / `⌘⇧W` | New window / close window |
| `⌘T` / `⌘W` | New connection tab / close tab |
| `⌘⇧]` / `⌘⇧[` | Next / previous tab |
| `⌃Tab` | Tab switcher, most recently used first |
| `⌘1` / `⌘2` | Focus local / server pane |
| `⌘↑` | Parent directory |
| `⌘[` / `⌘]` | Back / forward |
| `⌘⇧G` | Go to folder |
| `⌘F` | Filter files |
| `⌘⇧.` | Show hidden files |
| `↩` / `F2` | Open / rename |
| `⌘⇧N` / `⌘⌫` | New folder / delete |
| `⌘⇧C` | Copy path |
| `⌘U` / `⌘D` | Upload / download selection |
| `⌘⇧U` | Upload edited file |
| `⌘J` | Show transfers |
| `⌘R` / `⌘⇧R` | Refresh / reconnect |
| `⌘⇧P` | Command palette |
| `⌘,` | Settings |

Most commands also appear in the menu bar, and the command palette (`⌘⇧P`)
searches the app-level actions for the current window.

## Remote editing workflow

Remote editing is explicit: saving in your editor does not upload anything.

1. Open a remote file (double-click, `↩`, or the context menu). macSFTP
   downloads a temporary copy and opens it in your editor.
2. Edit and save the copy. The file list marks it `●` when the temporary copy
   changes.
3. Reselect the remote file and choose **Upload Changes** in the status bar, or
   **Upload Edited File** (`⌘⇧U`).
4. macSFTP reads the live remote metadata. If the file changed on the server, it
   asks before overwriting and offers **Compare Changes…**.
5. On failure, the local copy is kept and you can retry.

**Compare Changes…** downloads a fresh copy of the remote file into its own
directory each time, so an old snapshot is not treated as the current server
version. Closing the tab or window ends the editing session and removes its
temporary directory.

## Requirements

- macOS 13 or newer. arm64 is the tested architecture — the toolchain pins that
  target and release evidence covers it
- Xcode
- Rust `1.96.1` (pinned in `rust-toolchain.toml`) with `rustfmt` and `clippy`
- Python `3.11` or newer for architecture checks (CI uses `3.13`)
- `/usr/sbin/sshd`, `ssh-keygen`, and `ssh-keyscan` for real-session tests

Metal shaders are compiled when the app starts, so the separate Metal Toolchain
component is not needed to build macSFTP.

## Build and run

```bash
cargo run                 # run from the workspace
bash scripts/check.sh     # fmt, architecture, log-safety, tests, clippy
bash scripts/build_app.sh # unsigned bundle at build/macSFTP.app
```

Open the bundle with `open build/macSFTP.app`. It is unsigned and meant for
local testing; distribution needs Developer ID signing and notarization.

`check.sh` runs the same formatting, architecture, log-safety, test, and Clippy
gate that CI runs. SFTP integration tests connect to a local `sshd`; they skip
with an explicit message when it is unavailable locally, while CI sets
`MACSFTP_REQUIRE_SSHD=1` and fails instead of skipping. When changing
`Cargo.toml`, `Cargo.lock`, or vendored code, also run:

```bash
cargo deny --locked check advisories bans licenses sources
```

## Where macSFTP stores data

| Path | Contents |
| --- | --- |
| `~/Library/Application Support/macSFTP/config.json` | Preferences |
| `~/Library/Application Support/macSFTP/profiles.json` | Saved connection profiles, without secrets |
| `~/Library/Application Support/macSFTP/known_hosts` | Host keys you trusted |
| `~/Library/Application Support/macSFTP/session.json` | Window and tab restore data |
| `~/Library/Application Support/macSFTP/recents.json` | Recent connection targets |
| `~/Library/Application Support/macSFTP/edits/` | Temporary copies for remote editing |
| `~/Library/Application Support/macSFTP/residual_temp.json` | Temporary files left behind by interrupted transfers |
| `~/Library/Logs/macSFTP/macsftp.log` | Diagnostics |

Passwords and private-key passphrases are stored in macOS Keychain; profiles
only reference them. Use **Show Logs** in the command palette to open the log
folder. The log is written with secrets, full private-key paths, and
ProxyCommand text redacted.

## Architecture

The workspace is split by responsibility:

| Crate | Responsibility |
| --- | --- |
| `macsftp-core` | Pure models and state machines |
| `macsftp-ui` | Project-styled GPUI Base controls, themes, and presentation |
| `macsftp-native-menu` | Isolated macOS AppKit presenter for input and file-pane context menus |
| `macsftp-app` | Windows, actions, UI state, and event orchestration |
| `macsftp-sftp` | Tokio runtime, russh adapters, sessions, and transfers |
| `macsftp-storage` | Profiles, known hosts, Keychain references, and migrations |
| `macsftp-platform` | macOS and local-filesystem boundaries |
| `macsftp-test-support` | Shared integration-test fixtures |

GPUI and Tokio communicate through bounded command and event channels, and
network work stays off the UI thread. Stale events from closed tabs or
superseded sessions are rejected in `core` before they can change state. The
dependency rules, runtime model, and UI constraints are documented in
[`docs/architecture.md`](docs/architecture.md).

The production SFTP runtime builds against the real SSH backend; mock actors and
constructors are compiled for crate tests only.

## Not in scope

FTP, WebDAV, S3, a built-in terminal, directory sync, multi-hop jump-host
chains, full `~/.ssh/config` parsing, transfer restoration across launches,
App Store distribution, notarized releases, and background transfers after quit.

## Security

Keep credentials, private-key material, host fingerprints, and other sensitive
data out of public issues. See [`SECURITY.md`](SECURITY.md) for the private
reporting process.

## Contributing

Read [`AGENTS.md`](AGENTS.md) and [`CONTRIBUTING.md`](CONTRIBUTING.md) before
changing runtime, SFTP, transfer, host-key, persistence, or UI behavior. Keep
pull requests small, explain verification, and attach visual evidence for
visible UI changes.

## License

macSFTP is available under the [MIT License](LICENSE). The native menu adapter
uses the [Apache-2.0 License](crates/native-menu/LICENSE-APACHE) and preserves
attribution for its adapted AppKit implementation.
