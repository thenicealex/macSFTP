#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

status=0

if rg -n -U \
    '(?s)(trace|debug|info|warn|error)!\([^;]{0,800}\b(password|passphrase|fingerprint|key_path)\b' \
    crates --glob '*.rs'; then
    echo "error: tracing call references sensitive credential or fingerprint data" >&2
    status=1
fi

if rg -n \
    '(trace|debug|info|warn|error)!\([^)]*\?event\b' \
    crates --glob '*.rs'; then
    echo "error: tracing call formats a complete event payload" >&2
    status=1
fi

exit "$status"
