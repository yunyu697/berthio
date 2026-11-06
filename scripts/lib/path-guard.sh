#!/usr/bin/env bash
# 路径护栏（shell 版）：与 scripts/lib/path-guard.ps1、src/safety.rs 同口径。
# 默认 dry-run；第三参数显式传 --apply 才真正删除。
guard_rm_rf() {
  local target="${1:-}"
  local root="${2:-}"
  local apply="${3:-}"
  if [ -z "$target" ]; then
    echo "[guard] 拒绝：空路径" >&2
    return 1
  fi
  case "$target" in
    *'$'*) echo "[guard] 拒绝：含未展开的 \$ 变量: $target" >&2; return 1 ;;
  esac
  case "$target" in
    *%*%*) echo "[guard] 拒绝：含未展开的 %VAR%: $target" >&2; return 1 ;;
  esac
  local real root_real home_real
  real="$(realpath -m -- "$target" 2>/dev/null)" || { echo "[guard] 拒绝：路径无法解析: $target" >&2; return 1; }
  case "$real" in
    /) echo "[guard] 拒绝：文件系统根 /" >&2; return 1 ;;
  esac
  home_real=""
  if [ -n "${HOME:-}" ]; then home_real="$(realpath -m -- "$HOME" 2>/dev/null || true)"; fi
  if [ -n "$home_real" ]; then
    if [ "$real" = "$home_real" ]; then echo "[guard] 拒绝：用户主目录 $real" >&2; return 1; fi
    case "$home_real" in
      "$real"/*) echo "[guard] 拒绝：用户主目录的祖先 $real" >&2; return 1 ;;
    esac
    case "$real" in
      "$home_real"/*) echo "[guard] 拒绝：用户主目录之内 $real" >&2; return 1 ;;
    esac
  fi
  if [ -z "$root" ]; then echo "[guard] 拒绝：未提供允许根" >&2; return 1; fi
  root_real="$(realpath -m -- "$root" 2>/dev/null)" || { echo "[guard] 拒绝：允许根无法解析: $root" >&2; return 1; }
  case "$root_real" in
    /) echo "[guard] 拒绝：允许根不能是 /" >&2; return 1 ;;
  esac
  if [ "$real" = "$root_real" ]; then echo "[guard] 拒绝：目标就是允许根本身 $real" >&2; return 1; fi
  case "$real" in
    "$root_real"/*) : ;;
    *) echo "[guard] 拒绝：目标不在允许根之下 $real（允许根=$root_real）" >&2; return 1 ;;
  esac
  if [ ! -e "$real" ]; then
    echo "[guard] 目标不存在，跳过: $real"
    return 0
  fi
  local entries
  entries="$(find "$real" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"
  if [ "$apply" = "--apply" ]; then
    echo "[guard] APPLY remove -rf $real（条目=$entries）"
    rm -rf -- "$real"
  else
    echo "[guard] DRY-RUN remove -rf $real（条目=$entries）；显式传 --apply 才执行"
  fi
}
