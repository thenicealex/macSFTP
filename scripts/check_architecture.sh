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

assert_no_direct_dependency macsftp-core gpui russh russh-sftp tokio security-framework
assert_no_direct_dependency macsftp-ui macsftp-app macsftp-sftp macsftp-storage macsftp-platform russh russh-sftp tokio security-framework
assert_no_direct_dependency macsftp-sftp gpui macsftp-app macsftp-ui
assert_no_direct_dependency macsftp-storage gpui macsftp-app macsftp-ui macsftp-sftp russh russh-sftp tokio
assert_no_direct_dependency macsftp-platform gpui macsftp-app macsftp-ui macsftp-sftp macsftp-storage russh russh-sftp tokio

broad_import_allows="$(
    find crates/app/src/workspace -type f -name '*.rs' ! -name 'tests.rs' \
        -exec grep -H '^#!\[allow(unused_imports)\]' {} + || true
)"
if [[ -n "$broad_import_allows" ]]; then
    echo "broad unused-import suppression is forbidden in production workspace modules:" >&2
    echo "$broad_import_allows" >&2
    exit 1
fi
