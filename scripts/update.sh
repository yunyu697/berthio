#!/usr/bin/env bash
# Berthio 一键更新（Linux / systemd），复用 updates/latest.json 通道。
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/berthio-common.sh
. "$SCRIPT_DIR/lib/berthio-common.sh"

DRY_RUN=0

usage() {
    cat <<EOF
用法: sudo bash scripts/update.sh [选项]

  --health-url <URL>   重启后回读版本的地址（默认 $HEALTH_URL）
  --prefix <目录>      程序目录（默认 $INSTALL_DIR）
  --service-dir <目录> systemd unit 目录（默认 $SERVICE_DIR）
  --service-name <名>  服务名（默认 $SERVICE_NAME）
  --dry-run            只打印将要做的事，不下载、不替换、不重启
  -h, --help           显示本帮助

行为：仅在 latest.json 的版本高于当前安装版本时才动作；更新失败自动回滚旧二进制。
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --health-url) HEALTH_URL=${2:?--health-url 需要参数}; shift ;;
        --prefix) INSTALL_DIR=${2:?--prefix 需要参数}; BIN_PATH="$INSTALL_DIR/berthio"; shift ;;
        --service-dir) SERVICE_DIR=${2:?--service-dir 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        --service-name) SERVICE_NAME=${2:?--service-name 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage_error "未知参数：$1" ;;
    esac
    shift
done

if [ "$DRY_RUN" -eq 0 ]; then
    require_root
    need_systemd
    need_cmd curl
    need_cmd sha256sum
    need_cmd base64
    need_cmd minisign
    [ -f "$BIN_PATH" ] || die "未找到已安装的 $BIN_PATH，请先运行 install.sh。"
    systemctl cat "$SERVICE_NAME" >/dev/null 2>&1 || die "未找到 systemd 服务 $SERVICE_NAME，请先运行 install.sh。"
else
    log "[dry-run] 不执行任何写操作，仅打印计划"
fi

CUR_VERSION=$(installed_version || true)
[ -n "$CUR_VERSION" ] || die "无法读取当前安装版本（$BIN_PATH --version 失败）。"
log "当前版本：$CUR_VERSION"

TMPDIR_RUN=$(mktemp -d "${TMPDIR:-/tmp}/berthio-update.XXXXXX")
cleanup() { rm -rf "$TMPDIR_RUN"; }
trap cleanup EXIT

MANIFEST="$TMPDIR_RUN/latest.json"
fetch_manifest "$MANIFEST"
TARGET_VERSION=$(json_get "$MANIFEST" "version") || die "清单缺少 version，已中止。"
valid_version "$TARGET_VERSION" || die "清单版本号格式无效：$TARGET_VERSION"
log "清单版本：$TARGET_VERSION"

if ! ver_gt "$TARGET_VERSION" "$CUR_VERSION"; then
    if ver_eq "$TARGET_VERSION" "$CUR_VERSION"; then
        log "已是最新版本 $CUR_VERSION，无需更新。"
    else
        log "当前版本 $CUR_VERSION 高于清单版本 $TARGET_VERSION（可能使用了预发布或回退版本），不执行更新。"
    fi
    exit 0
fi

if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] 将下载并校验 $(asset_name "$TARGET_VERSION")"
    log "[dry-run] 将把旧二进制保留为 $INSTALL_DIR/berthio.prev-$CUR_VERSION"
    log "[dry-run] 将原子替换 $BIN_PATH 并执行 systemctl restart $SERVICE_NAME"
    log "[dry-run] 将在 $HEALTH_URL 回读版本，确认 $TARGET_VERSION；失败则回滚"
    exit 0
fi

fetch_and_verify_online "$TARGET_VERSION" "$TMPDIR_RUN"

PREV_PATH="$INSTALL_DIR/berthio.prev-$CUR_VERSION"
cp -p "$BIN_PATH" "$PREV_PATH" || die "保留旧二进制失败，已中止（当前服务未改动）。"
log "旧二进制已保留：$PREV_PATH"

NEW_TMP="$INSTALL_DIR/.berthio.new.$$"
cp "$VERIFIED_PKG" "$NEW_TMP" || die "写入新二进制失败，已中止（当前服务未改动）。"
chmod 0755 "$NEW_TMP"
mv -f "$NEW_TMP" "$BIN_PATH" || die "替换 $BIN_PATH 失败，当前服务仍在运行旧进程。"

health_version() {
    curl -fsS --max-time 3 "$HEALTH_URL" 2>/dev/null \
        | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | head -1
}

wait_for_version() {
    local want=$1 i
    for i in $(seq 1 15); do
        if [ "$(health_version)" = "$want" ]; then return 0; fi
        sleep 2
    done
    return 1
}

log "重启服务 $SERVICE_NAME"
if systemctl restart "$SERVICE_NAME" && wait_for_version "$TARGET_VERSION"; then
    log "更新成功：$CUR_VERSION -> $TARGET_VERSION（$HEALTH_URL 回读一致）"
    log "回滚命令（如需）：cp -p '$PREV_PATH' '$BIN_PATH' && systemctl restart $SERVICE_NAME"
    exit 0
fi

warn "更新后校验失败，开始回滚到 $CUR_VERSION"
if cp -p "$PREV_PATH" "$BIN_PATH" && systemctl restart "$SERVICE_NAME"; then
    if wait_for_version "$CUR_VERSION"; then
        die "更新失败，已回滚到 $CUR_VERSION 并恢复服务。查看日志：journalctl -u $SERVICE_NAME -n 50"
    fi
    die "更新失败，已回滚二进制并重启，但 $HEALTH_URL 未回读旧版本 $CUR_VERSION，请手动检查服务。"
fi
die "更新失败且回滚失败：$BIN_PATH 可能已是新版本，请手动用 $PREV_PATH 恢复。"
