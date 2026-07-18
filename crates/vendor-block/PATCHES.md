# Vendored `block` 0.1.6

Source: [`block` 0.1.6](https://crates.io/crates/block/0.1.6), whose upstream
repository is `SSheldon/rust-block`. The workspace overrides crates.io through
the root `[patch.crates-io]` entry.

## Local changes

`src/lib.rs.patch` is the complete local delta from the published 0.1.6
source:

- allow the legacy `missing_abi` lint used by this unmaintained FFI crate;
- replace the fieldless `Class` enum with an opaque `#[repr(C)]` zero-sized
  struct, removing the compiler's future-incompatibility warning without
  changing the pointer-only FFI representation.

The patch exists only to keep GPUI's transitive dependency compiling cleanly
with the workspace toolchain. Do not add macSFTP behavior to this directory.

## Updating or removing the override

1. Check whether GPUI still resolves `block` 0.1.6 with `cargo tree -i block`.
2. Prefer removing the root override when the transitive dependency is upgraded
   or eliminated.
3. If the override remains necessary, start from the exact published crate,
   reapply `src/lib.rs.patch`, and run `bash scripts/check.sh` on macOS.
4. Review the full vendored diff and update this file in the same commit.
