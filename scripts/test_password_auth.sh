#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
image="macsftp-password-sshd:${GITHUB_RUN_ID:-local}"
container="macsftp-password-sshd-${GITHUB_RUN_ID:-$$}"

if ! command -v docker >/dev/null 2>&1; then
    echo "error: Docker is required for the password-authentication gate" >&2
    exit 1
fi

cleanup() {
    docker stop "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT

docker build --tag "$image" "$repo_root/packaging/test-sshd"
docker run --detach --rm \
    --name "$container" \
    --publish 127.0.0.1::2222 \
    "$image" >/dev/null

port="$(docker port "$container" 2222/tcp | awk -F: 'NR == 1 { print $NF }')"
if [[ -z "$port" ]]; then
    echo "error: could not determine password fixture port" >&2
    exit 1
fi

ready=0
for _attempt in $(seq 1 100); do
    if (echo >/dev/tcp/127.0.0.1/"$port") >/dev/null 2>&1; then
        ready=1
        break
    fi
    sleep 0.1
done
if [[ "$ready" != "1" ]]; then
    docker logs "$container" >&2 || true
    echo "error: password fixture did not become ready" >&2
    exit 1
fi

MACSFTP_REQUIRE_PASSWORD_TEST=1 \
MACSFTP_PASSWORD_TEST_HOST=127.0.0.1 \
MACSFTP_PASSWORD_TEST_PORT="$port" \
MACSFTP_PASSWORD_TEST_USERNAME=macsftp \
MACSFTP_PASSWORD_TEST_PASSWORD=macsftp-test-password \
    cargo test --manifest-path "$repo_root/Cargo.toml" \
        -p macsftp-sftp --test password_auth -- --nocapture
