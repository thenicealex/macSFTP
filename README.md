# macSFTP

macSFTP is a native macOS SFTP client written in Rust. It uses GPUI for the
interface and `russh`/`russh-sftp` for asynchronous SSH and SFTP operations.

The project currently targets macOS and is distributed as an unsigned app
bundle. App Store distribution, notarization, automatic updates, and protocols
other than SFTP are outside the current scope.

## Current capabilities

- Multiple windows and connection tabs
- Password and private-key authentication backed by macOS Keychain
- OpenSSH-compatible host-key verification
- Local and remote browsing, navigation, filtering, sorting, and file operations
- Upload/download plans with progress, conflict handling, cancellation, and retry
- Session-scoped transfer drawer; transfer jobs are not restored after relaunch

## Requirements

- macOS
- Xcode Command Line Tools
- Rust `1.96.1` or newer with `rustfmt` and `clippy`
- `/usr/sbin/sshd`, `ssh-keygen`, and `ssh-keyscan` for real-session tests

## Build and verify

```bash
cargo run -p macsftp-app
bash scripts/check.sh
bash scripts/build_app.sh
```

The last command creates the unsigned bundle at `build/macSFTP.app`.

## Architecture

The workspace is intentionally split by responsibility:

| Crate | Responsibility |
| --- | --- |
| `macsftp-core` | Pure models and state machines |
| `macsftp-ui` | Reusable GPUI presentation components |
| `macsftp-app` | Windows, actions, UI state, and event orchestration |
| `macsftp-sftp` | Tokio runtime, russh adapters, sessions, and transfers |
| `macsftp-storage` | Profiles, known hosts, Keychain references, and migrations |
| `macsftp-platform` | macOS and local-filesystem boundaries |
| `macsftp-test-support` | Shared integration-test fixtures |

The dependency rules and runtime model are documented in
[`docs/gpui-russh-plan.md`](docs/gpui-russh-plan.md). Contributor requirements
are defined in [`AGENTS.md`](AGENTS.md) and
[`CONTRIBUTING.md`](CONTRIBUTING.md).

## Security

Do not report credentials, private-key material, host fingerprints, or other
sensitive data in a public issue. See [`SECURITY.md`](SECURITY.md) for the
private reporting process.

## License

macSFTP is available under the [MIT License](LICENSE).
