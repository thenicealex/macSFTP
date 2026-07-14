#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "error: the OS Keychain gate requires macOS" >&2
    exit 1
fi

keychain_dir="$(mktemp -d "${TMPDIR:-/tmp}/macsftp-keychain.XXXXXX")"
keychain="$keychain_dir/test.keychain-db"
password="macsftp-keychain-test-$$"
original_default="$(
    security default-keychain -d user \
        | sed -E 's/^[[:space:]]*"//; s/"[[:space:]]*$//'
)"
original_keychains=()
while IFS= read -r keychain_line; do
    keychain_line="${keychain_line#"${keychain_line%%[![:space:]]*}"}"
    keychain_line="${keychain_line%"${keychain_line##*[![:space:]]}"}"
    keychain_line="${keychain_line#\"}"
    keychain_line="${keychain_line%\"}"
    original_keychains+=("$keychain_line")
done < <(security list-keychains -d user)

cleanup() {
    if [[ "${#original_keychains[@]}" -gt 0 ]]; then
        security list-keychains -d user -s "${original_keychains[@]}" >/dev/null 2>&1 || true
    fi
    if [[ -n "$original_default" ]]; then
        security default-keychain -d user -s "$original_default" >/dev/null 2>&1 || true
    fi
    security delete-keychain "$keychain" >/dev/null 2>&1 || true
    rmdir "$keychain_dir" >/dev/null 2>&1 || true
}
trap cleanup EXIT

security create-keychain -p "$password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$password" "$keychain"
security list-keychains -d user -s "$keychain"
security default-keychain -d user -s "$keychain"

cargo test --manifest-path "$repo_root/Cargo.toml" \
    -p macsftp-storage \
    keychain::tests::os_backend_stores_loads_and_deletes \
    -- --ignored --exact
