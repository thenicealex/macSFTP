# Release process

The workspace version in the root `Cargo.toml` is the single product-version
source. Every first-party crate inherits it; About uses `CARGO_PKG_VERSION`, and
`scripts/build_app.sh` injects the app package version into `Info.plist`.

## Tags

- Release candidate: `vX.Y.Z-rc.N`
- Final release: `vX.Y.Z`
- Tags are annotated. Git tag signing is optional until a signing policy is
  adopted.
- A tag never publishes an application binary automatically.

The project currently builds an unsigned app bundle. Until Developer ID
signing and notarization are enabled, a passing tag means the source and release
gates passed; it does not make the bundle suitable for public distribution.

## Preparing a release

1. Set `workspace.package.version` in the root `Cargo.toml` and refresh
   `Cargo.lock`.
2. Move the relevant `CHANGELOG.md` entries from `Unreleased` into
   `## [X.Y.Z] - YYYY-MM-DD` for a final release. Release candidates may keep
   the entries under `Unreleased`.
3. Copy `docs/release-evidence/template.md` to
   `docs/release-evidence/vX.Y.Z.md`, complete every check, and set
   `Status: PASS`.
4. Run the full automated and native GUI gates documented in the evidence.
5. Run `bash scripts/check_release.sh vX.Y.Z` (or the release-candidate tag).
6. Commit the release metadata, then create an annotated tag.

Never edit a published tag. Fix the issue, increment the version, and produce a
new release candidate or patch release.
