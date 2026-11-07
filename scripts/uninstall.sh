#!/usr/bin/env bash
# Berthio 一键卸载（Linux / systemd）。默认保留数据目录，只有 --purge 才删数据。
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/berthio-common.sh
. "$SCRIPT_DIR/lib/berthio-common.sh"

DRY_RUN=0
PURGE=0
CONFIRM=""

usage() {
    cat <<EOF
用法: sudo bash scripts/uninstall.sh [选项]

  （默认）      停止并禁用服务、移除 systemd unit、删除程序目录；**保留数据目录**
  --purge       额外删除数据目录（会先逐条打印绝对路径并要求二次确认）
  --confirm-purge=DELETE
                非交互模式下的显式确认参数（必须与 --purge 同时使用）
  --prefix <目录>       程序目录（默认 $INSTALL_DIR）
  --data-dir <目录>     数据目录（默认 $DATA_DIR）
  --service-dir <目录>  unit 目录（默认 $SERVICE_DIR）
  --service-name <名称> 服务名（默认 $SERVICE_NAME）
  --user <名称>         专用系统用户（默认 $SERVICE_USER）
  --dry-run             只打印将要做的事，不删除任何文件
  -h, --help            显示本帮助
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --purge) PURGE=1 ;;
        --confirm-purge) CONFIRM=${2:?--confirm-purge 需要参数}; shift ;;
        --confirm-purge=*) CONFIRM=${1#--confirm-purge=} ;;
        --prefix) INSTALL_DIR=${2:?--prefix 需要参数}; BIN_PATH="$INSTALL_DIR/berthio"; shift ;;
        --data-dir) DATA_DIR=${2:?--data-dir 需要参数}; shift ;;
        --service-dir) SERVICE_DIR=${2:?--service-dir 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        --service-name) SERVICE_NAME=${2:?--service-name 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        --user) SERVICE_USER=${2:?--user 需要参数}; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage_error "未知参数：$1" ;;
    esac
    shift
done

if [ "$DRY_RUN" -eq 0 ]; then
    require_root
fi

normalize_dir() {
    case "$1" in
        /) printf '/' ;;
        *) printf '%s' "${1%/}" ;;
    esac
}
INSTALL_DIR=$(normalize_dir "$INSTALL_DIR")
DATA_DIR=$(normalize_dir "$DATA_DIR")
UNIT_PATH=${UNIT_PATH%/}

is_dangerous_path() {
    local path=$1 p home user_home
    case "$path" in
        /*) ;;
        *) return 0 ;;
    esac
    p=${path%/}
    case "$p" in
        ""|"/"|/bin|/boot|/dev|/etc|/home|/lib|/lib64|/media|/mnt|/opt|/proc|/root|/run|/sbin|/srv|/sys|/tmp|/usr|/var|/var/lib|/var/log|/var/tmp) return 0 ;;
    esac
    case "$p" in *'*'*|*'?'*|*'['*|*']'*) return 0 ;;
    esac
    case "/$p/" in *"/../"*) return 0 ;;
    esac
    home=${HOME:-}
    if [ -n "$home" ]; then
        [ "$p" = "$home" ] && return 0
        case "$home/" in "${p}/"*) return 0 ;; esac
    fi
    return 1
}

# 白名单：只允许卸载脚本自己推导出的具体路径，不接受额外输入。
TARGETS=()
TARGETS+=("$INSTALL_DIR")
TARGETS+=("$UNIT_PATH")
if [ "$PURGE" -eq 1 ]; then
    TARGETS+=("$DATA_DIR")
fi

for target in "${TARGETS[@]}"; do
    if is_dangerous_path "$target"; then
        die "拒绝卸载：路径 $target 属于危险路径（根目录、系统目录、主目录或其祖先）。未删除任何文件。"
    fi
    case "$target" in
        *"*"*|*"?"*|*"["*) die "拒绝卸载：路径 $target 含通配符，已中止。" ;;
    esac
done

if [ "$PURGE" -eq 1 ]; then
    log "将要删除的绝对路径（逐条白名单校验通过）："
    for target in "${TARGETS[@]}"; do
        printf '  - %s\n' "$target"
    done
    printf '  - 系统用户 %s（若存在）\n' "$SERVICE_USER"

    if [ "$DRY_RUN" -eq 1 ]; then
        log "[dry-run] 以上路径不会真的被删除"
    elif [ -n "$CONFIRM" ]; then
        [ "$CONFIRM" = "DELETE" ] || die "--confirm-purge 的值必须是 DELETE。未删除任何文件。"
        log "已收到显式确认 --confirm-purge=DELETE，继续。"
    elif [ -t 0 ]; then
        printf '\n这会永久删除上面的数据目录，且不可恢复。输入 yes 继续：'
        read -r answer
        [ "$answer" = "yes" ] || die "未得到 yes，已取消，未删除任何文件。"
    else
        die "非交互模式执行 --purge 需要额外显式参数：--confirm-purge=DELETE。未删除任何文件。"
    fi
fi

if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] 将执行：systemctl disable --now $SERVICE_NAME"
    log "[dry-run] 将删除 unit：$UNIT_PATH，然后 systemctl daemon-reload"
    log "[dry-run] 将删除程序目录：$INSTALL_DIR"
    if [ "$PURGE" -eq 1 ]; then
        log "[dry-run] 将删除数据目录：$DATA_DIR 与用户 $SERVICE_USER"
    else
        log "[dry-run] 保留数据目录：$DATA_DIR（含 config/panel.json）"
    fi
    exit 0
fi

if command -v systemctl >/dev/null 2>&1; then
    systemctl stop "$SERVICE_NAME" 2>/dev/null || true
    systemctl disable "$SERVICE_NAME" 2>/dev/null || true
    log "服务已停止并禁用：$SERVICE_NAME"
else
    warn "未找到 systemctl，跳过服务停止步骤"
fi

if [ -f "$UNIT_PATH" ]; then
    rm -f -- "$UNIT_PATH" || die "删除 unit 失败：$UNIT_PATH"
    systemctl daemon-reload 2>/dev/null || true
    log "已移除 unit：$UNIT_PATH"
else
    log "unit 不存在，跳过：$UNIT_PATH"
fi

if [ -d "$INSTALL_DIR" ]; then
    rm -rf -- "$INSTALL_DIR" || die "删除程序目录失败：$INSTALL_DIR"
    log "已删除程序目录：$INSTALL_DIR"
else
    log "程序目录不存在，跳过：$INSTALL_DIR"
fi

if [ "$PURGE" -eq 1 ]; then
    if [ -d "$DATA_DIR" ]; then
        rm -rf -- "$DATA_DIR" || die "删除数据目录失败：$DATA_DIR"
        log "已删除数据目录：$DATA_DIR"
    else
        log "数据目录不存在，跳过：$DATA_DIR"
    fi
    if id -u "$SERVICE_USER" >/dev/null 2>&1; then
        userdel "$SERVICE_USER" 2>/dev/null || warn "删除系统用户 $SERVICE_USER 失败（可能仍有进程占用）"
        log "已删除系统用户：$SERVICE_USER"
    fi
    log "卸载完成（--purge：数据已删除）"
else
    log "卸载完成；数据目录已保留：$DATA_DIR"
    log "如需彻底清除数据，重新运行：sudo bash scripts/uninstall.sh --purge"
fi
