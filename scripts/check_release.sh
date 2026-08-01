#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

tag="${1:-}"
if [[ ! "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[1-9][0-9]*)?$ ]]; then
    echo "usage: bash scripts/check_release.sh vX.Y.Z[-rc.N]" >&2
    exit 1
fi

tag_version="${tag#v}"
base_version="${tag_version%%-rc.*}"
package_id="$(cargo pkgid --manifest-path "$repo_root/Cargo.toml" -p macsftp-app)"
package_version="${package_id##*@}"

if [[ "$package_version" != "$base_version" ]]; then
    echo "error: tag base $base_version does not match app version $package_version" >&2
    exit 1
fi

first_party_manifests=(
    crates/app/Cargo.toml
    crates/core/Cargo.toml
    crates/platform/Cargo.toml
    crates/sftp/Cargo.toml
    crates/storage/Cargo.toml
    crates/test_support/Cargo.toml
    crates/ui/Cargo.toml
)
if grep -nE '^version[[:space:]]*=' "${first_party_manifests[@]}"; then
    echo "error: first-party crate versions must inherit workspace.package.version" >&2
    exit 1
fi

if ! grep -qF '<string>@VERSION@</string>' packaging/macos/Info.plist.in; then
    echo "error: Info.plist must derive its version from the app package" >&2
    exit 1
fi

if [[ "$tag_version" == "$base_version" ]]; then
    if ! grep -qE "^## \[$base_version\] - [0-9]{4}-[0-9]{2}-[0-9]{2}$" CHANGELOG.md; then
        echo "error: final release requires a dated CHANGELOG section for $base_version" >&2
        exit 1
    fi
elif ! grep -qE "^## \[($base_version|Unreleased)\]" CHANGELOG.md; then
    echo "error: release candidate has no matching CHANGELOG content" >&2
    exit 1
fi

bash scripts/check_release_evidence.sh "$tag"

if ! git diff --quiet || ! git diff --cached --quiet; then
    echo "error: release validation requires a clean worktree" >&2
    exit 1
fi

bash scripts/check_sensitive_logs.sh
echo "Release metadata valid for $tag"
