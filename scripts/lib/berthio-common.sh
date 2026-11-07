#!/usr/bin/env bash
# Berthio 安装/更新/卸载脚本的公共函数。不要直接执行本文件。
# 设计约束：不写死任何个人路径；只依赖 curl、sha256sum、minisign 与 python3（或 jq）。

# 仓库与发布通道（可用环境变量覆盖，便于镜像/分支测试）
BERTHIO_REPO="${BERTHIO_REPO:-yunyu697/berthio}"
BERTHIO_FEED="${BERTHIO_FEED:-https://github.com/${BERTHIO_REPO}/releases/download/updates/latest.json}"
BERTHIO_MIRRORS="${BERTHIO_MIRRORS:-https://ghproxy.net/ https://gh-proxy.com/ https://ghfast.top/}"

# 内嵌 minisign 公钥（base64 的公钥文件全文），与面板自更新使用的公钥一致。
BERTHIO_PUBKEY_B64="${BERTHIO_PUBKEY_B64:-dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDIxRjVBNTFDQTVGQjJFQTQKUldTa0x2dWxIS1gxSVQ0UGl4R3VMZEFmMkhWcEFRdk9nU3lidThGNDV4bEY0VE02Q2tZZ3ovOVkK}"

INSTALL_DIR="${BERTHIO_INSTALL_DIR:-/opt/berthio}"
DATA_DIR="${BERTHIO_DATA_DIR:-/var/lib/berthio}"
SERVICE_DIR="${BERTHIO_SERVICE_DIR:-/etc/systemd/system}"
SERVICE_NAME="${BERTHIO_SERVICE_NAME:-berthio}"
SERVICE_USER="${BERTHIO_USER:-berthio}"
LISTEN_ADDR="${BERTHIO_LISTEN:-127.0.0.1:18080}"
HEALTH_URL="${BERTHIO_HEALTH_URL:-http://127.0.0.1:18080/healthz}"

BIN_PATH="$INSTALL_DIR/berthio"
UNIT_PATH="$SERVICE_DIR/$SERVICE_NAME.service"

log()  { printf '[berthio] %s\n' "$*"; }
warn() { printf '[berthio] 警告: %s\n' "$*" >&2; }
die()  { printf '[berthio] 错误: %s\n' "$*" >&2; exit 1; }

usage_error() {
    die "$*（用 --help 查看用法）"
}

need_cmd() {
    command -v "$1" >/dev/null 2>&1 || die "缺少命令 $1。请先安装后再运行本脚本。"
}

require_root() {
    [ "$(id -u)" -eq 0 ] || die "需要 root 权限：请用 sudo 运行，或先用 --dry-run 查看将要执行的动作。"
}

need_systemd() {
    command -v systemctl >/dev/null 2>&1 || die "未找到 systemctl，本脚本按 systemd 服务方式安装。"
}

arch_name() {
    case "$(uname -m)" in
        x86_64|amd64) printf 'x86_64' ;;
        *) die "当前架构 $(uname -m) 暂无发布包（仅提供 linux-x86_64）。" ;;
    esac
}

asset_name() {
    local ver=$1 arch
    arch=$(arch_name)
    printf 'berthio-%s-linux-%s' "$ver" "$arch"
}

asset_url() {
    local ver=$1 name
    name=$(asset_name "$ver")
    printf 'https://github.com/%s/releases/download/%s/%s' "$BERTHIO_REPO" "$ver" "$name"
}

sums_url() {
    local ver=$1
    printf 'https://github.com/%s/releases/download/%s/SHA256SUMS' "$BERTHIO_REPO" "$ver"
}

# ------------------------------------------------------------------ 版本

valid_version() {
    printf '%s' "$1" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+(-alpha[0-9]+)?$'
}

ver_key() {
    local v=$1 base alpha=999
    case "$v" in
        *-alpha*) base=${v%-alpha*}; alpha=${v##*-alpha};;
        *) base=$v;;
    esac
    local x y z
    IFS=. read -r x y z <<< "$base"
    printf '%04d%04d%04d%03d' "$x" "$y" "$z" "$alpha"
}

ver_gt() { [ "$(ver_key "$1")" -gt "$(ver_key "$2")" ]; }
ver_eq() { [ "$(ver_key "$1")" -eq "$(ver_key "$2")" ]; }

installed_version() {
    [ -x "$BIN_PATH" ] || return 1
    "$BIN_PATH" --version 2>/dev/null | awk '{print $2}'
}

# ------------------------------------------------------------------ JSON

json_get() {
    local file=$1 key=$2
    [ -s "$file" ] || return 1
    if command -v python3 >/dev/null 2>&1; then
        python3 - "$file" "$key" <<'PY'
import json, sys
with open(sys.argv[1], encoding="utf-8") as fh:
    data = json.load(fh)
cur = data
for part in sys.argv[2].split("."):
    if not isinstance(cur, dict) or part not in cur:
        sys.exit(0)
    cur = cur[part]
if isinstance(cur, bool):
    print("true" if cur else "false")
elif cur is not None:
    print(cur)
PY
    elif command -v jq >/dev/null 2>&1; then
        jq -r "(.${key} // empty)" "$file"
    else
        die "解析 latest.json 需要 python3 或 jq，请先安装其中一个。"
    fi
}

# ------------------------------------------------------------------ 下载

download_one() {
    local url=$1 out=$2
    curl -fsSL --retry 3 --retry-delay 2 --connect-timeout 15 --max-time 600 -o "$out" "$url"
}

download() {
    local url=$1 out=$2 mirror
    if download_one "$url" "$out"; then
        return 0
    fi
    for mirror in $BERTHIO_MIRRORS; do
        log "直连失败，尝试镜像 $mirror"
        if download_one "$mirror$url" "$out"; then
            return 0
        fi
    done
    return 1
}

fetch_manifest() {
    local out=$1
    log "读取更新清单：$BERTHIO_FEED"
    download "$BERTHIO_FEED" "$out" || die "无法下载 latest.json（已尝试官方地址与镜像）。"
    [ -s "$out" ] || die "latest.json 为空，已中止。"
}

# ------------------------------------------------------------------ 校验

verify_sha256() {
    local file=$1 name=$2 sums=$3 want got
    [ -s "$sums" ] || die "SHA256SUMS 为空或不存在，已中止。"
    want=$(awk -v n="$name" '{ f=$2; sub(/^\*/, "", f) } f==n { print $1; exit }' "$sums")
    [ -n "$want" ] || die "SHA256SUMS 中没有 $name 的条目，已中止。"
    got=$(sha256sum "$file" | awk '{print $1}')
    if [ "$got" != "$want" ]; then
        die "SHA-256 不匹配（期望 $want，实际 $got），已中止，未安装。"
    fi
    log "SHA-256 校验通过：$got"
}

verify_minisign() {
    local file=$1 name=$2 version=$3 sig_b64=$4
    local tmpdir=${TMPDIR:-/tmp}/berthio-verify.$$
    mkdir -p "$tmpdir" || die "无法创建临时目录 $tmpdir"

    printf '%s' "$BERTHIO_PUBKEY_B64" | base64 -d > "$tmpdir/pubkey" 2>/dev/null \
        || die "内嵌公钥无法解码，已中止。"
    printf '%s' "$sig_b64" | base64 -d > "$tmpdir/sig" 2>/dev/null \
        || die "清单里的签名不是有效的 base64，已中止。"

    need_cmd minisign
    minisign -Vm "$file" -p "$tmpdir/pubkey" -x "$tmpdir/sig" -q \
        || die "minisign 签名校验失败：安装包与签名不匹配，已中止，未安装。"

    local comment
    comment=$(minisign -Vm "$file" -p "$tmpdir/pubkey" -x "$tmpdir/sig" -Q 2>/dev/null) \
        || die "无法读取签名的 trusted comment，已中止。"
    case "$comment" in
        *"version:$version"*) ;;
        *) die "签名绑定的版本与清单不一致（$comment），已中止。" ;;
    esac
    case "$comment" in
        *"file:$name"*) ;;
        *) die "签名绑定的文件名与清单不一致（$comment），已中止。" ;;
    esac
    log "minisign 签名校验通过：$comment"
}

# 下载清单里指定平台的包，并完成尺寸、SHA-256、签名三项校验。
# 结果路径写入全局变量 VERIFIED_PKG。
fetch_and_verify_online() {
    local version=$1
    local tmpdir=$2
    local name url sums sig size
    name=$(asset_name "$version")
    url=$(asset_url "$version")
    sums=$(sums_url "$version")
    sig=$(json_get "$MANIFEST" "platforms.linux-x86_64.signature") \
        || die "清单缺少 linux-x86_64.signature，已中止。"
    size=$(json_get "$MANIFEST" "platforms.linux-x86_64.size") || size=""

    log "下载 $name"
    download "$url" "$tmpdir/$name" || die "下载安装包失败：$url（已尝试镜像）。"
    download "$sums" "$tmpdir/SHA256SUMS" || die "下载 SHA256SUMS 失败，已中止。"

    if [ -n "$size" ] && [ "$(wc -c < "$tmpdir/$name" | tr -d ' ')" != "$size" ]; then
        die "安装包大小与清单不一致（期望 $size 字节），已中止。"
    fi
    verify_sha256 "$tmpdir/$name" "$name" "$tmpdir/SHA256SUMS"
    verify_minisign "$tmpdir/$name" "$name" "$version" "$sig"
    VERIFIED_PKG="$tmpdir/$name"
    VERIFIED_NAME="$name"
}

# 离线包：要求同目录存在同名 SHA256SUMS 与 latest.json。
verify_local_package() {
    local pkg=$1 name version sums manifest sig size
    [ -f "$pkg" ] || die "离线包不存在：$pkg"
    name=$(basename "$pkg")
    sums="${BERTHIO_CHECKSUMS:-$(dirname "$pkg")/SHA256SUMS}"
    manifest="${BERTHIO_MANIFEST:-$(dirname "$pkg")/latest.json}"
    [ -s "$sums" ] || die "离线模式缺少 SHA256SUMS（期望 $sums）。可用 --checksums 指定。"
    [ -s "$manifest" ] || die "离线模式缺少 latest.json（期望 $manifest）。可用 --manifest 指定。"

    version=$(json_get "$manifest" "version") || die "离线清单缺少 version，已中止。"
    valid_version "$version" || die "离线清单版本号格式无效：$version"
    [ "$name" = "$(asset_name "$version")" ] \
        || warn "离线包文件名与清单版本推断的名称不同（继续以清单为准）"

    sig=$(json_get "$manifest" "platforms.linux-x86_64.signature") \
        || die "离线清单缺少 linux-x86_64.signature，已中止。"
    size=$(json_get "$manifest" "platforms.linux-x86_64.size") || size=""
    if [ -n "$size" ] && [ "$(wc -c < "$pkg" | tr -d ' ')" != "$size" ]; then
        die "安装包大小与清单不一致（期望 $size 字节），已中止。"
    fi
    verify_sha256 "$pkg" "$name" "$sums"
    verify_minisign "$pkg" "$name" "$version" "$sig"
    VERIFIED_PKG="$pkg"
    VERIFIED_NAME="$name"
    VERIFIED_VERSION="$version"
}

# ------------------------------------------------------------------ 服务

unit_content() {
    cat <<EOF
[Unit]
Description=Berthio 控制面板
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=$SERVICE_USER
Group=$SERVICE_USER
Environment=BERTHIO_HOME=$DATA_DIR
Environment=BERTHIO_LISTEN=$LISTEN_ADDR
ExecStart=$BIN_PATH
Restart=on-failure
RestartSec=3
NoNewPrivileges=true
ProtectSystem=full
ProtectHome=true
PrivateTmp=true

[Install]
WantedBy=multi-user.target
EOF
}
