#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

status=0

check_pattern() {
    local message="$1"
    shift

    if rg "$@"; then
        echo "error: $message" >&2
        status=1
    else
        local result=$?
        if [[ "$result" -ne 1 ]]; then
            echo "error: sensitive log scan failed (rg exit $result)" >&2
            exit "$result"
        fi
    fi
}

check_pattern "tracing call references sensitive credential or fingerprint data" -n -U \
    '(?s)(trace|debug|info|warn|error)!\([^;]{0,800}\b(password|passphrase|fingerprint|key_path|proxy_command|agent_socket|keyboard_response)\b' \
    crates --glob '*.rs'

check_pattern "tracing call formats a complete event payload" -n \
    '(trace|debug|info|warn|error)!\([^)]*\?event\b' \
    crates --glob '*.rs'

check_pattern "SSH agent tracing exposes identity or signing material" -n \
    '(trace|debug)!\([^;]{0,800}(identities:|sign_request|self\.buf|public =)' \
    crates/vendor-russh/src/keys/agent/client.rs

exit "$status"
