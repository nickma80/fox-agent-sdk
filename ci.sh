#!/usr/bin/env bash
# ci.sh — 本地模拟 GitHub CI (ci-rust.yml) 的编译检查
# 用法: ./ci.sh
set -euo pipefail

cd "$(dirname "$0")"

# Step 1: Build (警告即错误)
echo "=== cargo build (RUSTFLAGS=-D warnings) ==="
export RUSTFLAGS="-D warnings"
cargo build --workspace --exclude fox-agent-py

# Step 2: Test
echo -e "\n=== cargo test ==="
cargo test --workspace --exclude fox-agent-py

# Step 3: Clippy (警告即错误)
echo -e "\n=== cargo clippy (-D warnings) ==="
cargo clippy --workspace --exclude fox-agent-py -- -D warnings

# Step 4: 格式化检查
echo -e "\n=== cargo fmt --check ==="
cargo fmt --all --check

echo -e "\n✅ All CI checks passed!"
