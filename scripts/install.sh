#!/usr/bin/env bash
# Berthio 一键安装（Linux / systemd）。
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=lib/berthio-common.sh
. "$SCRIPT_DIR/lib/berthio-common.sh"

DRY_RUN=0
PKG=""
CHECKSUMS=""
OFFLINE_MANIFEST=""
REQ_VERSION=""

usage() {
    cat <<EOF
用法: sudo bash scripts/install.sh [选项]

来源：
  （默认）在线安装最新正式版
  --package <文件>        使用本地离线包（同目录需有 SHA256SUMS 与 latest.json）
  --checksums <文件>      离线模式的 SHA256SUMS 路径
  --manifest <文件>       离线模式的 latest.json 路径
  --version <X.Y.Z>       在线安装指定版本（默认取 latest.json 的 version）

安装位置：
  --prefix <目录>         程序目录（默认 $INSTALL_DIR）
  --data-dir <目录>       数据目录（默认 $DATA_DIR）
  --service-dir <目录>    systemd unit 目录（默认 $SERVICE_DIR）
  --service-name <名称>   服务名（默认 $SERVICE_NAME）
  --user <名称>           专用系统用户（默认 $SERVICE_USER）
  --listen <地址:端口>    面板监听地址（默认 $LISTEN_ADDR）

其它：
  --dry-run               只打印将要做的事，不下载、不写文件、不改服务
  -h, --help              显示本帮助
EOF
}

while [ $# -gt 0 ]; do
    case "$1" in
        --dry-run) DRY_RUN=1 ;;
        --package) PKG=${2:?--package 需要参数}; shift ;;
        --checksums) CHECKSUMS=${2:?--checksums 需要参数}; shift ;;
        --manifest) OFFLINE_MANIFEST=${2:?--manifest 需要参数}; shift ;;
        --version) REQ_VERSION=${2:?--version 需要参数}; shift ;;
        --prefix) INSTALL_DIR=${2:?--prefix 需要参数}; BIN_PATH="$INSTALL_DIR/berthio"; shift ;;
        --data-dir) DATA_DIR=${2:?--data-dir 需要参数}; shift ;;
        --service-dir) SERVICE_DIR=${2:?--service-dir 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        --service-name) SERVICE_NAME=${2:?--service-name 需要参数}; UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"; shift ;;
        --user) SERVICE_USER=${2:?--user 需要参数}; shift ;;
        --listen) LISTEN_ADDR=${2:?--listen 需要参数}; shift ;;
        -h|--help) usage; exit 0 ;;
        *) usage_error "未知参数：$1" ;;
    esac
    shift
done

[ -n "$CHECKSUMS" ] && BERTHIO_CHECKSUMS=$CHECKSUMS
[ -n "$OFFLINE_MANIFEST" ] && BERTHIO_MANIFEST=$OFFLINE_MANIFEST

if [ "$DRY_RUN" -eq 0 ]; then
    require_root
    need_systemd
    need_cmd curl
    need_cmd sha256sum
    need_cmd base64
    need_cmd minisign
    [ -n "$PKG" ] || need_cmd python3
else
    log "[dry-run] 不执行任何写操作，仅打印计划"
fi

TMPDIR_RUN=$(mktemp -d "${TMPDIR:-/tmp}/berthio-install.XXXXXX")
cleanup() { rm -rf "$TMPDIR_RUN"; }
trap cleanup EXIT

# ---------------------------------------------------------------- 解析来源
VERSION=""
VERIFIED_PKG=""
VERIFIED_NAME=""
if [ -n "$PKG" ]; then
    log "来源：本地离线包 $PKG"
    if [ "$DRY_RUN" -eq 1 ]; then
        [ -f "$PKG" ] || die "离线包不存在：$PKG"
        sumf=${BERTHIO_CHECKSUMS:-$(dirname "$PKG")/SHA256SUMS}
        manf=${BERTHIO_MANIFEST:-$(dirname "$PKG")/latest.json}
        [ -s "$sumf" ] || die "离线模式缺少 SHA256SUMS（期望 $sumf）"
        [ -s "$manf" ] || die "离线模式缺少 latest.json（期望 $manf）"
        VERSION=$(json_get "$manf" "version") || die "离线清单缺少 version"
        log "[dry-run] 将校验 SHA-256（$sumf）与 minisign 签名（$manf）"
    else
        verify_local_package "$PKG"
        VERSION=$VERIFIED_VERSION
    fi
else
    MANIFEST="$TMPDIR_RUN/latest.json"
    if [ "$DRY_RUN" -eq 1 ]; then
        if download "$BERTHIO_FEED" "$MANIFEST" 2>/dev/null; then
            VERSION=$(json_get "$MANIFEST" "version") || VERSION=""
        fi
        [ -n "$VERSION" ] || VERSION="最新正式版（运行时读取 latest.json）"
        log "[dry-run] 将读取 latest.json 并下载 $(asset_name "${REQ_VERSION:-<version>}")"
    else
        fetch_manifest "$MANIFEST"
        VERSION=${REQ_VERSION:-$(json_get "$MANIFEST" "version")}
        valid_version "$VERSION" || die "清单版本号格式无效：$VERSION"
        fetch_and_verify_online "$VERSION" "$TMPDIR_RUN"
    fi
fi
if [ -n "$REQ_VERSION" ] && [ "$DRY_RUN" -eq 0 ] && [ -n "$PKG" ]; then
    warn "使用离线包时忽略 --version $REQ_VERSION，以离线清单版本 $VERSION 为准"
fi

# ---------------------------------------------------------------- 安装计划
log "安装版本：${VERSION}"
log "程序目录：$INSTALL_DIR"
log "数据目录：$DATA_DIR"
log "服务名称：$SERVICE_NAME（unit: $UNIT_PATH）"

CUR_VERSION=$(installed_version || true)
if [ -n "$CUR_VERSION" ]; then
    if ver_eq "$CUR_VERSION" "$VERSION"; then
        log "已安装同版本 $CUR_VERSION，将只校正服务单元与文件权限（幂等）"
    else
        log "检测到已安装版本 $CUR_VERSION，将替换为 $VERSION（旧二进制保留为 berthio.prev-$CUR_VERSION）"
    fi
fi

if [ "$DRY_RUN" -eq 1 ]; then
    log "[dry-run] 将创建系统用户（不存在时）：$SERVICE_USER"
    log "[dry-run] 将创建目录：$INSTALL_DIR、$DATA_DIR"
    log "[dry-run] 将写入二进制：$BIN_PATH（先写临时文件再原子替换）"
    log "[dry-run] 将写入 systemd unit：$UNIT_PATH"
    log "[dry-run] 将执行：systemctl daemon-reload && systemctl enable --now $SERVICE_NAME"
    log "[dry-run] 完成后访问：http://$LISTEN_ADDR/"
    exit 0
fi

# ---------------------------------------------------------------- 用户与目录
if ! id -u "$SERVICE_USER" >/dev/null 2>&1; then
    log "创建系统用户 $SERVICE_USER"
    shell_path=/usr/sbin/nologin
    [ -x "$shell_path" ] || shell_path=/bin/false
    useradd --system --home-dir "$DATA_DIR" --no-create-home --shell "$shell_path" "$SERVICE_USER" \
        || die "创建系统用户 $SERVICE_USER 失败。"
else
    log "系统用户 $SERVICE_USER 已存在"
fi

mkdir -p "$INSTALL_DIR" "$DATA_DIR" "$(dirname "$UNIT_PATH")" \
    || die "创建目录失败：$INSTALL_DIR / $DATA_DIR"
chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR" || die "无法把数据目录归属给 $SERVICE_USER。"
chmod 0750 "$DATA_DIR" || true

# ---------------------------------------------------------------- 二进制
if [ -n "$CUR_VERSION" ] && [ "$CUR_VERSION" != "$VERSION" ] && [ -x "$BIN_PATH" ]; then
    cp -p "$BIN_PATH" "$INSTALL_DIR/berthio.prev-$CUR_VERSION" \
        || die "保留旧二进制失败，已中止。"
    log "旧二进制已保留：$INSTALL_DIR/berthio.prev-$CUR_VERSION"
fi

NEW_TMP="$INSTALL_DIR/.berthio.new.$$"
cp "$VERIFIED_PKG" "$NEW_TMP" || die "写入 $NEW_TMP 失败（磁盘空间或权限不足）。"
chmod 0755 "$NEW_TMP"
mv -f "$NEW_TMP" "$BIN_PATH" || die "原子替换 $BIN_PATH 失败。"
chown root:root "$BIN_PATH" 2>/dev/null || true
log "二进制已就位：$BIN_PATH"

# ---------------------------------------------------------------- systemd
NEW_UNIT="$TMPDIR_RUN/unit.service"
unit_content > "$NEW_UNIT"
if [ -f "$UNIT_PATH" ] && cmp -s "$NEW_UNIT" "$UNIT_PATH"; then
    log "systemd unit 未变化：$UNIT_PATH"
else
    cp "$NEW_UNIT" "$UNIT_PATH" || die "写入 unit 失败：$UNIT_PATH"
    log "已写入 systemd unit：$UNIT_PATH"
    systemctl daemon-reload || die "systemctl daemon-reload 失败。"
fi

systemctl enable "$SERVICE_NAME" >/dev/null 2>&1 || die "systemctl enable $SERVICE_NAME 失败。"
systemctl restart "$SERVICE_NAME" || die "启动服务失败。查看原因：journalctl -u $SERVICE_NAME -n 50"
sleep 2
if ! systemctl is-active --quiet "$SERVICE_NAME"; then
    printf '\n' >&2
    journalctl -u "$SERVICE_NAME" -n 30 --no-pager >&2 || true
    die "服务 $SERVICE_NAME 未处于 active 状态，安装未完成。"
fi

# ---------------------------------------------------------------- 收尾
NOW_VERSION=$(installed_version || true)
log "服务已启动，回读版本：${NOW_VERSION:-未知}"
log ""
log "访问地址：http://$LISTEN_ADDR/"
log "数据目录：$DATA_DIR"
log "配置文件：$DATA_DIR/config/panel.json"
log "服务单元：$UNIT_PATH"
log "下一步：浏览器打开上面的地址，按页面提示设置面板口令；对外暴露前请先配置 HTTPS。"
