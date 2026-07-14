#!/usr/bin/env bash
set -euo pipefail

cargo fmt --all --check
bash scripts/check_architecture.sh
bash scripts/check_sensitive_logs.sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
