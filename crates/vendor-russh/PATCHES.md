# Vendored `russh` 0.62.2

Source: [`russh` 0.62.2](https://crates.io/crates/russh/0.62.2), upstream
commit `c4be19f1915c8682f4615c3fd50008512b474491`. The workspace overrides
crates.io through the root `[patch.crates-io]` entry.

## Why this override exists

Upstream ties RSA host-signature verification to its full `rsa` feature. That
feature also compiles RustCrypto RSA private-key operations affected by
RUSTSEC-2023-0071. macSFTP needs to connect to password-authenticated legacy
gateways that expose only RSA host keys without enabling RSA client signing.

## Local changes

- add `rsa-sha2-host-key`, backed only by the existing AWS-LC backend;
- verify RSA-SHA2-256/512 server signatures using public modulus/exponent data;
- accept RSA host keys down to 1024 bits, matching the macOS OpenSSH default,
  while leaving SHA-1 `ssh-rsa` out of macSFTP's negotiation list;
- keep the upstream verifier for Ed25519/ECDSA and other supported algorithms;
- cover valid and tampered RSA-1024/SHA-512 signatures with a fixed test vector;
- silence one no-RSA unused-value warning exposed by this feature split.
- omit upstream examples from the vendored build input; one example logs a
  password and is intentionally excluded by macSFTP's sensitive-log gate.

The complete RSA client-private-key feature remains disabled in
`crates/sftp/Cargo.toml`, and macSFTP also rejects RSA private keys before
authentication.

## Updating or removing the override

1. Check whether upstream has split public RSA host verification from private
   RSA signing, preferably using a constant-time maintained backend.
2. Prefer removing this override when an audited upstream release provides the
   same boundary.
3. Otherwise start from the exact published crate, reapply the changes above,
   and review the full vendored diff.
4. Run the commands below and update this file in the same change.

## Verify

```bash
cargo test --no-default-features \
  --manifest-path crates/vendor-russh/Cargo.toml \
  --features aws-lc-rs,flate2,rsa-sha2-host-key \
  verifies_legacy_rsa1024_sha512_host_signature_with_aws_lc
bash scripts/check.sh
cargo deny check advisories
bash scripts/build_app.sh
```

The target-gateway smoke test must confirm that the SSH handshake reaches host
trust and authentication without using a real credential.
