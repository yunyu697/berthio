# Berthio

Berthio 是一个自建的代理服务端**控制面面板**：单进程、单二进制，用 JSON 文件保存状态，不依赖数据库。它负责生成并校验 xray / sing-box 的服务端配置，以及导出 mihomo（Clash）与 sing-box 的客户端配置，本身不转发流量。

## 这是什么

- 面向单机、单实例的自建场景，替代 3x-ui 中「入站 / 客户端 / 订阅 / 配置生成」这部分工作。
- 后端为 Rust 单 crate，前端是内嵌的单页应用，启动后只需要一个可执行文件。
- 默认只监听 `127.0.0.1:18080`，首次访问时需要设置面板口令。
- 代理内核（xray / sing-box / mihomo）由面板管理，也可以用 `BERTHIO_KERNEL_DIR` 指向已有的外部内核目录。

## 主要能力

### 入站管理

- 从模板创建 xray 或 sing-box 入站，可查看入站详情与客户端列表；入站可编辑（备注/监听/端口/启用）与删除（`?confirm=<id>` 二次确认，不触碰文件系统）。
- 客户端支持新增、修改（email/配额/到期/flow）、启用/停用、重置流量与删除（删除需 `?confirm=<id>`，不触碰文件系统）；以上动作均写审计。
- xray 入站支持 VLESS + REALITY / TLS，传输支持 tcp、ws、xhttp。
- sing-box 入站支持 VLESS、hysteria2、tuic 的配置生成（hysteria2 未经过真实样本验证，见「已知限制」）。
- 创建与导出前执行三级校验：L0 结构、L1 语义、L2 用真实内核二进制自检（`xray -test` / `sing-box check` / `mihomo -t`）；运行期逐请求执行，L2 失败直接拒绝下发（HTTP 422）。

### 配置导出

- 导出 mihomo（Clash）YAML 与 sing-box JSON，导出前用对应内核二进制做语法/配置自检。
- xhttp 传输只有 xray 客户端可用；导出给 mihomo / sing-box 时会在运行期剔除并给出告警。

### 订阅

- 为入站生成订阅链接 `/sub/<token>?target=mihomo|sing-box`（`clash` 作为 mihomo 的别名）。
- 支持令牌创建、吊销、轮换；订阅链接泄露即可被他人访问，需要定期轮换。

### 3x-ui 导入

- 主路径是 3x-ui 的入站 JSON：可以直接粘贴，也可以保存成文件上传。支持单条对象、`inbounds` 数组、整份备份（含 `xray` / `obj` 套层），`settings` / `streamSettings` / `sniffing` 的对象与字符串化 JSON 两种形态都能识别。
- 先预览再落地，显示导入 / 跳过 / 冲突条数与逐条原因；重复导入不会产生重复入站。
- 数据库路径作为批量迁移的兜底：只读打开 `x-ui.db`（必要时连 `-wal` 一起快照到 `tmp/` 后 `VACUUM INTO`），读完立即删除快照，源库内容与权限不变。

### 内核管理

- 自动发现 xray、sing-box、mihomo：优先 `PATH`，其次数据目录下的 `tools/kernel/<内核>/<版本>/` 或 `runtime/kernels/<内核>/<版本>/`，也可以用 `BERTHIO_KERNEL_DIR/<内核>/` 指定。
- 面板不下载内核二进制；需要自行放入上述目录或 `PATH`。
- 提供启动、停止、重启、校验、日志与版本查看。
- 启动前先用内核自身校验配置；记录 PID 与状态到 `state/kernels.json`，进程异常退出会自动拉起。
- 面板重启后能接管仍在运行的内核（PID 不变），不会重复启动。

### 自更新（设置页）

> 1.0.0 发布时还没有更新入口，首次升级到 1.0.1 需要手动替换一次可执行文件；从 1.0.1 起才能在设置页自更新。

- 设置页只有一个「检查更新」入口；未检查时只显示「检查更新」和「打开主页」，检查中按钮禁用并显示进度。
- 只有发现新版本时才出现「更新」和「仅下载」，同时显示版本号、包大小与来源；已是最新时显示「当前已是最新版本」，这两个按钮不出现。
- 「更新」：下载 → 完整性校验 → 拉起安装/替换 → 退出 → 自动重启。
- 「仅下载」：保存到默认下载目录，完成后按钮变「安装」；已下载且校验通过的包，即使随后版本已是最新，也保留「安装」入口。
- 多源逐源尝试：选择镜像后顺序为「所选镜像 → 官方源 → 其余镜像」，自动模式为「官方源 → 其余镜像」；失败原因按源逐条展示，可重试或换源。
- 完整性校验 fail-closed：没有可用公钥就拒绝安装，不提供「跳过校验」；校验失败换源重试而不是放行。
- 下载先写 `.part` 临时文件，失败或换源即删，启动时清理残留；安装前再次校验大小、SHA-256 与签名。
- 默认清单地址为 `https://github.com/yunyu697/berthio/releases/download/updates/latest.json`，内置 ghproxy / gh-proxy / ghfast 三个镜像前缀；`config/panel.json` 的 `update.feed`、`update.mirror`、`update.mirrors` 可覆盖。

## 快速开始

### 演示模式（不需要内核，不改系统配置）

```bash
./berthio --demo
```

Windows：

```powershell
.\berthio-<版本>-windows-x86_64.exe --demo
```

打开 <http://127.0.0.1:18090>。演示模式使用临时数据目录、内置演示数据，不启动真实内核、不写系统配置、不申请 root；端口可用 `BERTHIO_DEMO_PORT` 覆盖。

### 正常启动

```bash
./berthio
```

打开 <http://127.0.0.1:18080>，首次访问时设置面板口令。默认监听地址可用 `BERTHIO_LISTEN` 覆盖。

### 从源码构建

需要 Rust 1.85 或更高版本。

```bash
cargo build --release
./target/release/berthio
```

## 安装方式

1. 从 GitHub Releases 下载对应平台的二进制：
   - `berthio-<版本>-linux-x86_64`
   - `berthio-<版本>-windows-x86_64.exe`
2. 用同一 Release 的 `SHA256SUMS` 校验下载结果（Linux 可用 `sha256sum -c SHA256SUMS`）。
3. 赋予可执行权限后直接运行。仓库不附带安装包、系统服务单元或容器镜像；需要常驻时请用系统的进程管理器自行托管。

Linux 发布二进制以 Debian 12（glibc 2.36）为基线构建，更老的发行版可能无法运行。

发布二进制**没有做系统级代码签名**，Windows 的 SmartScreen 可能给出提示；更新包的完整性与来源由 minisign 清单签名校验，两者是不同层面的机制。

环境变量：

| 变量 | 作用 |
|---|---|
| `BERTHIO_HOME` | 数据目录，默认 `./.berthio`；若 `/opt/berthio` 存在则优先使用它 |
| `BERTHIO_LISTEN` | 覆盖面板监听地址 |
| `BERTHIO_DEMO_PORT` | 演示模式端口，默认 `18090` |
| `BERTHIO_KERNEL_DIR` | 使用外部内核目录，而不是面板管理的内核 |

## 配置与状态文件位置

默认数据目录为 `./.berthio`，或在存在 `/opt/berthio` 时使用 `/opt/berthio`，也可以用 `BERTHIO_HOME` 指定。

| 路径（相对数据目录） | 内容 |
|---|---|
| `config/panel.json` | 面板配置：监听地址、更新源与镜像、日志保留天数、可信代理等（`schema_version` 管理） |
| `state/state.json` | 入站、客户端、订阅令牌等业务状态（`schema_version` 管理） |
| `state/auth.json` | 面板口令与登录会话 |
| `state/kernels.json` | 内核进程 PID、运行状态与接管信息 |
| `state/kernels/<内核>.lkg` | 各内核最近一次通过校验的配置（last known good） |
| `state/update.json` | 自更新状态：已下载包记录、最近一次安装记录 |
| `state/update/<可执行文件>.prev` | 自更新替换前的可执行文件备份 |
| `state/backups/` | schema 升级前的逐字节备份；手动备份为 `state/backups/<name>/`（`manifest.json` + `files/`） |
| `configs/generated/` | 生成的代理配置 |
| `logs/audit/`、`logs/kernels/` | 审计日志与内核日志（按月/按内核轮转） |
| `data/rulesets/`、`data/geo/` | 规则集与地理数据 |
| `tools/kernel/`、`runtime/kernels/` | 面板自动发现的内核二进制（按内核名/版本分目录） |
| `tmp/`、`tmp/update/` | 临时文件与更新下载残片 |

「仅下载」的安装包落在用户默认下载目录（Windows 为 `%USERPROFILE%\Downloads`，Linux 为 `~/Downloads`），并在旁边写出同名 `.sig` 签名文件。

`panel.json` / `state.json` 在版本升级时自动迁移并先写备份；迁移失败会回滚并拒绝启动，`schema_version` 高于当前程序版本时拒绝启动且不改写文件。

手动备份包含 `config/panel.json`、`config/sources.json`、`state/state.json`、`state/auth.json` 以及相对路径配置的内置 TLS 证书/私钥；审计日志、内核 PID、升级进度与可再生缓存不参与备份（设置页会逐项列出包含与排除理由）。恢复前核对每个文件的 SHA-256，校验失败不改动现场；通过后先拍回滚快照，再逐文件原子替换，写入失败自动回滚。

## 版本号规则

- 版本号唯一来源是 `Cargo.toml` 的 `[package] version`。
- 格式为 `MAJOR.MINOR.PATCH` 或 `MAJOR.MINOR.PATCH-alphaN`，例如 `1.0.0-alpha1`、`1.0.0`。
- 除第一段外，其余每段数值不得大于 10。
- alpha 序号从 1 开始，不带前导零，单调递增、不复用。
- Git tag 必须与版本号完全一致，不加 `v` 前缀；二进制 `--version` 输出必须为 `berthio <版本号>`。

仓库内 `scripts/check-version.sh` 是可执行的版本规则校验，CI 在打 tag 时会运行它。

## 已知限制与未验证项

- **hysteria2 入站未验证**：配置生成逻辑已实现，但没有真实样本与端到端连接验证，不建议直接用于生产。
- **mihomo 普通 Reality 未复测**：本轮 REALITY 端到端验证使用的是 xray 25.6.8 客户端；mihomo 1.19.33 只做了静态配置检查。
- **xhttp 只支持 xray 客户端**：sing-box 1.14.4 不接受 xhttp 传输；mihomo 1.19.33 语法通过但真实连接失败。
- **未做系统级代码签名**：Windows 可能提示来源不明。更新清单使用 minisign（Ed25519）签名，内嵌公钥校验，未配置公钥时拒绝安装。
- **面板不带内置 HTTPS**：默认只监听回环地址；对外提供服务必须自行加反向代理、HTTPS 与强口令。
- **单实例、单数据目录**：不支持集群、多机同步或并发多写入者。
- **1.0.0 不含更新入口**：首次升级到 1.0.1 必须手动替换可执行文件；之后才能使用设置页的自更新。
- **自更新替换路径的平台差异**：Windows 上的替换与自动重启已在测试中实跑；Linux 的替换逻辑已实现但未实跑。自更新还需要当前进程对可执行文件所在目录有写权限，放在只读目录或包管理目录时请手动更新。
- **3x-ui 导入的覆盖范围**：JSON 与数据库入口以手头样本（v2.9.4 风格、v3.11.0）验证；更老的版本或非标准字段可能被跳过，预览页会列出原因。
- **内核版本**：已验证 xray 25.6.8、sing-box 1.14.4、mihomo 1.19.33 的配置自检；其他版本未逐一验证。

## 许可

Apache-2.0（见 `Cargo.toml` 的 `license` 字段；仓库目前未附 `LICENSE` 文本）。

## 反馈

问题与建议请提交到 GitHub Issues：<https://github.com/yunyu697/berthio/issues>。
