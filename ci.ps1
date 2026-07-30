# ci.ps1 — 本地模拟 GitHub CI (ci-rust.yml) 的编译检查
# 用法: .\ci.ps1

$ErrorActionPreference = "Stop"
$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
Set-Location $RepoRoot

# Step 1: Build (警告即错误)
Write-Host "=== cargo build (RUSTFLAGS=-D warnings) ===" -ForegroundColor Cyan
$env:RUSTFLAGS = "-D warnings"
cargo build --workspace --exclude fox-agent-py
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Step 2: Test
Write-Host "`n=== cargo test ===" -ForegroundColor Cyan
cargo test --workspace --exclude fox-agent-py
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Step 3: Clippy (警告即错误)
Write-Host "`n=== cargo clippy (-D warnings) ===" -ForegroundColor Cyan
cargo clippy --workspace --exclude fox-agent-py -- -D warnings
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

# Step 4: 格式化检查
Write-Host "`n=== cargo fmt --check ===" -ForegroundColor Cyan
cargo fmt --all --check
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

Write-Host "`n✅ All CI checks passed!" -ForegroundColor Green
