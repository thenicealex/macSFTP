# SSH compatibility expansion

Date: 2026-09-16

## Goal

Add four connection capabilities without weakening the existing host-key,
secret-storage, stale-event, and bounded-channel guarantees:

- one saved-profile jump host;
- an explicit OpenSSH-style `ProxyCommand` transport;
- multi-round keyboard-interactive authentication;
- SSH-agent authentication, including RSA identities;
- direct RSA private-key authentication using AWS-LC for signing.

Existing password and Ed25519/ECDSA profiles must continue to load and connect
without user action.

## Product model

`ConnectionProfile` gains a transport route whose default is `Direct`:

- `Direct` connects to the target TCP address;
- `JumpHost { profile_id }` resolves one other saved profile and tunnels the
  target connection through an SSH `direct-tcpip` channel;
- `ProxyCommand { command }` runs a user-supplied command and treats its
  stdin/stdout as the target SSH byte stream.

Jump profiles must themselves be direct. Self-reference, missing profiles, and
jump chains are rejected before a command reaches the runtime. This keeps the
first implementation to one auditable hop and prevents cycles.

`AuthMethod` gains `KeyboardInteractive` and `SshAgent`. Keyboard-interactive
answers are never persisted. SSH-agent profiles persist no secret and try the
identities returned by the agent in order until one succeeds. Agent comments
and key material are not logged.

## Runtime protocol

Keyboard-interactive is a connection-scoped request/response protocol. The
runtime allocates `KeyboardInteractiveRequestId`, registers a bounded one-shot
response, emits a remote-scoped prompt event, and waits with the same stale-tab
and cancellation behavior as host trust. The UI returns exactly one response
per prompt or cancels the request. Closing a tab, reconnecting, or shutting down
expires pending requests.

The runtime is authoritative for keyboard-interactive request IDs because the
server creates each round after the UI command has already been dispatched.
The ID is always paired with `RemoteEventScope`; therefore an old round cannot
act on a reconnected tab.

## Transport ownership

All transports implement Tokio `AsyncRead + AsyncWrite + Unpin + Send` and feed
`russh::client::connect_stream`:

- direct TCP owns a `TcpStream`;
- jump-host transport owns both the authenticated jump handle and the
  `direct-tcpip` channel stream;
- ProxyCommand transport owns the child process plus its stdin/stdout and uses
  `kill_on_drop(true)` so cancellation cannot leak a proxy process.

ProxyCommand uses `/bin/sh -c` because OpenSSH defines it as a shell command.
Only an explicitly configured profile may execute one. `%h`, `%p`, `%r`, and
`%%` are expanded with shell-quoted target values. The command and stderr are
never copied to logs or user-facing errors.

## RSA security boundary

The RustCrypto `rsa` crate remains affected by RUSTSEC-2023-0071 as of
2026-09-14, including `0.10.0-rc.18`. The project therefore continues to keep
russh's `rsa` feature disabled.

Direct RSA private keys are decoded by the existing `ssh-key` parser, converted
to PKCS#1 DER without performing private-key arithmetic, and signed with
AWS-LC. Authentication negotiates only RSA-SHA2-512 or RSA-SHA2-256; SHA-1
`ssh-rsa` signatures remain disabled. RSA keys below 2048 bits are rejected for
client authentication.

SSH-agent RSA signing remains delegated to the user's agent. macSFTP never
receives that private key.

## Persistence and pooling

The profiles schema gains a new version. Missing route fields migrate to
`Direct`. Route changes increment the profile revision. A jump connection's
pool identity includes both target and jump profile revisions, so editing the
jump profile cannot reuse a connection authenticated with stale settings.
ProxyCommand text participates in the non-secret connection identity but is
not emitted to logs.

## Verification

- core tests for new auth/route types, redacted debug output, and stale prompt
  scope;
- storage migration, validation, Keychain, and jump-cycle tests;
- runtime tests for prompt response/cancellation and command routing;
- real sshd tests for keyboard-interactive, agent Ed25519/RSA, direct RSA-SHA2,
  ProxyCommand, and one-hop jump-host SFTP;
- UI tests for form validation, profile round trips, modal cancellation, focus,
  narrow layout, and reconnect behavior;
- dependency audit plus the full project gate.

Direct connections, host-key mismatch blocking, transfer/session ownership,
and secret redaction must not change.
