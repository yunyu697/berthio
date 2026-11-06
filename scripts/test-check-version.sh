#!/usr/bin/env bash
# check-version.sh 的反例自测：每条都应被拦住，或应放行。
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SCRIPT="$ROOT/scripts/check-version.sh"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

pass_n=0
fail_n=0

make_toml() {
  cat > "$TMP/Cargo.toml" <<EOF
[package]
name = "berthio"
version = "$1"
edition = "2021"
EOF
}

expect_ok() {
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    echo "PASS  $name"
    pass_n=$((pass_n + 1))
  else
    echo "FAIL  $name（预期通过但被拒绝）"
    fail_n=$((fail_n + 1))
  fi
}

expect_reject() {
  local name="$1"; shift
  if "$@" >/dev/null 2>&1; then
    echo "FAIL  $name（预期拒绝但通过了）"
    fail_n=$((fail_n + 1))
  else
    echo "PASS  $name（已拦下）"
    pass_n=$((pass_n + 1))
  fi
}

# 清掉 CI 环境里的 GITHUB_REF_NAME，否则脚本会自动拿发布 tag 来比对，污染反例测试
check() { GITHUB_REF_NAME= bash "$SCRIPT" --version-file "$TMP/Cargo.toml" "$@"; }

make_toml "1.0.0"
expect_ok "正式版 1.0.0" check

make_toml "1.0.0-alpha1"
expect_ok "alpha 1.0.0-alpha1" check

make_toml "1.0.0-alpha1"
expect_reject "tag 与版本号不一致 (1.0.0-alpha2)" check --tag 1.0.0-alpha2

make_toml "1.11.0"
expect_reject "1.11.0 第二段超过 10" check

make_toml "1.0.11"
expect_reject "1.0.11 第三段超过 10" check

make_toml "1.0.0-alpha0"
expect_reject "alpha 序号从 0 开始" check

make_toml "1.0.0-alpha01"
expect_reject "alpha 序号带前导零" check

make_toml "1.0.0-alpha"
expect_reject "alpha 缺序号" check

make_toml "1.0"
expect_reject "缺少 PATCH 段" check

make_toml "1.0.0-beta1"
expect_reject "非 alpha 预发布" check

make_toml "01.0.0"
expect_reject "主版本号带前导零" check

echo
echo "自测结果: $pass_n 通过 / $fail_n 失败"
[ "$fail_n" -eq 0 ]
