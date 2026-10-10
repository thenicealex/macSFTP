#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

assert_no_direct_dependency() {
    local package="$1"
    shift

    local dependencies
    dependencies="$(
        cargo tree --locked --package "$package" --depth 1 --edges normal --prefix none \
            | tail -n +2 \
            | awk '{print $1}'
    )"

    local forbidden
    for forbidden in "$@"; do
        if grep -Fqx "$forbidden" <<<"$dependencies"; then
            echo "architecture violation: $package directly depends on $forbidden" >&2
            return 1
        fi
    done
}

assert_no_direct_dependency macsftp-core gpui gpui-pre gpui-component gpui-base gpui-kit gpui-pre-platform russh russh-sftp tokio security-framework
assert_no_direct_dependency macsftp-ui macsftp-app macsftp-sftp macsftp-storage macsftp-platform russh russh-sftp tokio security-framework
assert_no_direct_dependency macsftp-sftp gpui gpui-pre gpui-component gpui-base gpui-kit gpui-pre-platform macsftp-app macsftp-ui
assert_no_direct_dependency macsftp-storage gpui gpui-pre gpui-component gpui-base gpui-kit gpui-pre-platform macsftp-app macsftp-ui macsftp-sftp russh russh-sftp tokio
assert_no_direct_dependency macsftp-platform gpui gpui-pre gpui-component gpui-base gpui-kit gpui-pre-platform macsftp-app macsftp-ui macsftp-sftp macsftp-storage russh russh-sftp tokio

broad_import_allows="$(
    find crates/app/src/workspace -type f -name '*.rs' ! -name 'tests.rs' \
        -exec grep -H '^#!\[allow(unused_imports)\]' {} + || true
)"
if [[ -n "$broad_import_allows" ]]; then
    echo "broad unused-import suppression is forbidden in production workspace modules:" >&2
    echo "$broad_import_allows" >&2
    exit 1
fi

direct_keychain_access="$(
    find crates/app/src -type f -name '*.rs' ! -name 'tests.rs' \
        -exec grep -EH 'KeychainStore|\.keychain' {} + || true
)"
if [[ -n "$direct_keychain_access" ]]; then
    echo "app must use the storage profile boundary instead of Keychain directly:" >&2
    echo "$direct_keychain_access" >&2
    exit 1
fi

PYTHONDONTWRITEBYTECODE=1 python3 scripts/test_unsafe_boundary.py
python3 scripts/check_unsafe_boundary.py

assert_no_direct_dependency macsftp-native-menu macsftp-app macsftp-ui macsftp-core macsftp-sftp macsftp-storage macsftp-platform russh russh-sftp tokio
