#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

version="${1:-}"
version="${version#v}"
version="${version%%-rc.*}"
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
    echo "usage: bash scripts/check_release_evidence.sh vX.Y.Z[-rc.N]" >&2
    exit 1
fi

evidence="docs/release-evidence/v${version}.md"
if [[ ! -f "$evidence" ]]; then
    echo "error: missing release evidence: $evidence" >&2
    exit 1
fi
if ! grep -qE "^Version: ${version}$" "$evidence"; then
    echo "error: release evidence version does not match $version" >&2
    exit 1
fi
if ! grep -qE '^Status: PASS$' "$evidence"; then
    echo "error: release evidence status is not PASS" >&2
    exit 1
fi
if grep -qE '^- \[ \]' "$evidence"; then
    echo "error: release evidence still has unchecked items" >&2
    exit 1
fi
if grep -qE ': TBD$|^- TBD$' "$evidence"; then
    echo "error: release evidence still contains TBD metadata" >&2
    exit 1
fi

echo "Native release evidence valid for v$version"
