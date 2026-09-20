# macSFTP

macSFTP is a native macOS SFTP client written in Rust. It uses GPUI for the
interface and `russh`/`russh-sftp` for asynchronous SSH and SFTP operations.

The project currently targets macOS and is distributed as an unsigned app
bundle. App Store distribution, notarization, automatic updates, and protocols
other than SFTP are outside the current scope.

## Current capabilities

- Multiple windows and connection tabs
- Saved connection profiles managed in Settings, plus one-off connections
- Password, multi-round keyboard-interactive, SSH-agent, and private-key authentication
- Ed25519/ECDSA plus RSA-SHA2 client keys; direct RSA signing uses AWS-LC
- Saved-profile jump hosts and explicit OpenSSH-style ProxyCommand routes
- OpenSSH-compatible host-key verification
- Local and remote browsing, navigation, filtering, sorting, and file operations
- Upload/download plans with progress, conflict handling, cancellation, and retry
- External-editor workflow with an explicit, conflict-checked **Upload Modified File** action
- Process-wide transfer queue shown in each window; jobs are not restored after relaunch

Connection profiles are created, edited, and deleted only in
**Settings → Profiles**. The Connect dialog can select a saved profile or make
a temporary connection; its **Manage…** button opens the profile editor.

Remote editing is intentionally explicit: double-click a remote file to open a
temporary copy in the configured editor, save it there, reselect the remote
file in macSFTP, and choose **Upload Modified File** in the status bar. macSFTP
checks current remote metadata before uploading and asks for a decision if the
remote file changed. It does not watch local saves or upload them automatically.

## Requirements

- macOS
- Xcode with the Metal Toolchain (`xcodebuild -downloadComponent MetalToolchain`)
- Rust `1.96.1` or newer with `rustfmt` and `clippy`
- `/usr/sbin/sshd`, `ssh-keygen`, and `ssh-keyscan` for real-session tests

## Build and verify

```bash
cargo run -p macsftp-app
bash scripts/check.sh
bash scripts/build_app.sh
```

The last command creates the unsigned bundle at `build/macSFTP.app`.

RSA-SHA2 server host keys and direct RSA client signatures are handled with
AWS-LC. The affected RustCrypto RSA private-key path covered by
RUSTSEC-2023-0071 remains disabled. Client RSA keys must be at least 2048 bits;
SHA-1 `ssh-rsa` signatures are not negotiated.

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

The production SFTP runtime exposes only the real SSH backend. Mock actors and
constructors are compiled only for crate tests.

## Security

Do not report credentials, private-key material, host fingerprints, or other
sensitive data in a public issue. See [`SECURITY.md`](SECURITY.md) for the
private reporting process.

## License

macSFTP is available under the [MIT License](LICENSE).
