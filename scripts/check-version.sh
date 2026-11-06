#!/usr/bin/env bash
# 版本号校验（CI 与本地通用）
#
# 规则（写死，可执行）：
#   1. 版本号唯一来源 = Cargo.toml 的 [package] version。
#   2. 必须是 MAJOR.MINOR.PATCH 或 MAJOR.MINOR.PATCH-alpha<序号>。
#   3. 除第一段外，其余每一段数值都不得大于 10（例如 1.11.0 直接失败）。
#   4. alpha 序号从 1 开始、只递增不复用、不带前导零。
#   5. tag 必须与版本号完全一致（不加 v 前缀）。
#   6. 若提供已构建二进制，其 --version 必须打印 "berthio <版本号>"。
#
# 用法：
#   scripts/check-version.sh                         # 只校验版本文件 + 规则
#   scripts/check-version.sh --tag 1.0.0-alpha1      # 额外校验 tag 一致性与序号单调
#   scripts/check-version.sh --binary ./berthio      # 额外校验二进制内版本
# CI 里未传 --tag 时会读 GITHUB_REF_NAME；push 到分支时它是分支名，自动跳过 tag 校验。
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CARGO_TOML="$ROOT/Cargo.toml"

TAG=""
TAG_SET=0
BINARY=""

while [ $# -gt 0 ]; do
  case "$1" in
    --tag) TAG="${2:-}"; TAG_SET=1; shift 2 ;;
    --binary) BINARY="${2:-}"; shift 2 ;;
    --version-file) CARGO_TOML="${2:-}"; shift 2 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "未知参数: $1" >&2; exit 2 ;;
  esac
done

fail() { echo "FAIL: $*" >&2; exit 1; }
info() { echo "  $*"; }

[ -f "$CARGO_TOML" ] || fail "找不到 $CARGO_TOML"

# 只在 [package] 段内取第一处 version，避免依赖表里的 version 干扰
VERSION="$(awk '
  /^\[/ { in_pkg = ($0 == "[package]") ; next }
  in_pkg && /^version[[:space:]]*=/ {
    line = $0
    sub(/^version[[:space:]]*=[[:space:]]*"/, "", line)
    sub(/".*$/, "", line)
    print line
    exit
  }
' "$CARGO_TOML")"
[ -n "$VERSION" ] || fail "Cargo.toml 的 [package] 段里没有 version"

echo "版本文件: $CARGO_TOML"
echo "版本号:   $VERSION"

# 2 + 4：格式
if ! printf '%s' "$VERSION" | grep -Eq '^(0|[1-9][0-9]*)\.[0-9]+\.[0-9]+(-alpha[1-9][0-9]*)?$'; then
  fail "版本号格式不合规（只允许 X.Y.Z 或 X.Y.Z-alphaN，且不使用前导零）: $VERSION"
fi
printf '%s' "$VERSION" | grep -Eq '(^|[.-])0[0-9]+$' && fail "版本号存在前导零: $VERSION"

BASE="${VERSION%%-*}"
ALPHA_SEQ=""
case "$VERSION" in
  *-alpha*) ALPHA_SEQ="${VERSION##*-alpha}" ;;
esac

MAJOR="${BASE%%.*}"
REST="${BASE#*.}"
MINOR="${REST%%.*}"
PATCH="${REST#*.}"

# 3：除第一段外每段 <= 10
for pair in "MINOR:$MINOR" "PATCH:$PATCH"; do
  name="${pair%%:*}"
  value="${pair#*:}"
  if [ "$value" -gt 10 ]; then
    fail "$name=$value 超过上限 10（规则：除第一段外每段不得大于 10）"
  fi
  info "$name=$value ≤ 10  OK"
done
info "MAJOR=$MAJOR（第一段不设上限）"

if [ -n "$ALPHA_SEQ" ]; then
  [ "$ALPHA_SEQ" -ge 1 ] || fail "alpha 序号必须从 1 开始: $VERSION"
  info "alpha 序号=$ALPHA_SEQ  OK"
else
  info "正式版（仅在所有里程碑完成后允许发布）"
fi

# 5：tag 一致性
if [ "$TAG_SET" -eq 0 ] && [ -n "${GITHUB_REF_NAME:-}" ]; then
  case "$GITHUB_REF_NAME" in
    refs/tags/*) TAG="${GITHUB_REF_NAME#refs/tags/}"; TAG_SET=1 ;;
    *-alpha*|[0-9]*.[0-9]*.[0-9]*) TAG="$GITHUB_REF_NAME"; TAG_SET=1 ;;
    *) echo "当前 ref 不是 tag（$GITHUB_REF_NAME），跳过 tag 校验" ;;
  esac
fi

if [ "$TAG_SET" -eq 1 ]; then
  [ "$TAG" = "$VERSION" ] || fail "tag '$TAG' 与版本号 '$VERSION' 不一致（tag 必须与版本号完全相同，不带 v 前缀）"
  info "tag=$TAG 与版本号一致 OK"

  # 4：序号单调递增（同基线的历史 alpha tag 中取最大序号）
  if [ -n "$ALPHA_SEQ" ] && git -C "$ROOT" rev-parse --git-dir >/dev/null 2>&1; then
    PREV_MAX=0
    PREV_TAG=""
    while IFS= read -r t; do
      [ -z "$t" ] && continue
      [ "$t" = "$TAG" ] && continue
      case "$t" in
        "$BASE"-alpha[0-9]*)
          n="${t##*-alpha}"
          case "$n" in *[!0-9]*|'') continue ;; esac
          if [ "$n" -gt "$PREV_MAX" ]; then PREV_MAX="$n"; PREV_TAG="$t"; fi
          ;;
      esac
    done <<< "$(git -C "$ROOT" tag --list)"
    if [ "$PREV_MAX" -gt 0 ]; then
      [ "$ALPHA_SEQ" -gt "$PREV_MAX" ] || fail "alpha 序号必须递增：当前 $ALPHA_SEQ，已有最大 $PREV_MAX（$PREV_TAG）"
      info "alpha 序号 $ALPHA_SEQ > 已有最大 $PREV_MAX（$PREV_TAG）OK"
    else
      info "首个 $BASE alpha OK"
    fi
  fi
else
  echo "未提供 tag，跳过 tag 一致性与序号单调校验"
fi

# 6：二进制内版本
if [ -n "$BINARY" ]; then
  [ -x "$BINARY" ] || fail "二进制不存在或不可执行: $BINARY"
  OUT="$("$BINARY" --version)"
  EXPECT="berthio $VERSION"
  [ "$OUT" = "$EXPECT" ] || fail "二进制 --version 输出 '$OUT'，期望 '$EXPECT'"
  info "二进制 --version = '$OUT' OK"
fi

echo "PASS: 版本校验全部通过"
