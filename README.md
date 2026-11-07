# Berthio

> 在线界面演示：<https://yunyu697.github.io/berthio/>
> 这是纯前端界面演示（内置示例数据，不需要后端，也不会发起任何真实连接），**不是可连接真实节点的在线服务**。

Berthio 是一个自建的代理服务端**控制面面板**：单进程、单二进制，用 JSON 文件保存状态，不依赖数据库。它负责生成并校验 xray / sing-box 的服务端配置，以及导出 mihomo（Clash）与 sing-box 的客户端配置，本身不转发流量。

## 这是什么

- 面向单机、单实例的自建场景，替代 3x-ui 中「入站 / 客户端 / 订阅 / 配置生成」这部分工作。
- 后端为 Rust 单 crate，前端是内嵌的单页应用，启动后只需要一个可执行文件。
- 默认只监听 `127.0.0.1:18080`，首次访问时需要设置面板口令。
- 代理内核（xray / sing-box / mihomo）由面板管理，也可以用 `BERTHIO_KERNEL_DIR` 指向已有的外部内核目录。

## 主要能力

### 入站管理

- 从模板创建 xray 或 sing-box 入站，可查看入站详情与客户端列表。
- 入站可编辑，但只开放四类字段：备注、监听地址、端口、启用状态。协议、安全层、传输方式与客户端列表不在编辑范围；端口与其他入站冲突、或被系统占用时返回 409；提交没有任何实际变更时返回 400。
- 入站删除需二次确认 `DELETE /api/inbound/inbounds/<id>?confirm=<id>`，`confirm` 与 `id` 不一致返回 400，且这次拒绝也会写审计。删除只从内存状态移除并原子重写 `state/state.json`，**不调用任何文件删除接口**，审计 detail 固定记 `files_deleted: 0`。
- 客户端支持新增、修改、启用/停用、重置流量与删除。可改字段为 **email、配额（`quota_bytes`）、到期时间（`expires_at`，空字符串表示清空）、启用状态（`enabled`）、flow**；协议、安全层、UUID 不可改；同一入站内 email 重复返回 409，无实际变更返回 400。
- 停用与启用走同一个 PATCH 接口，界面停用需二次确认；重置流量只把 `used_bytes` 归零，不动配额、到期与启用状态。
- 客户端删除为 `DELETE /api/inbound/inbounds/<id>/clients/<cid>?confirm=<cid>`，同样只改 `state.json`、不触碰文件系统。以上动作均写审计。
- xray 入站支持 VLESS + REALITY / TLS，传输支持 tcp、ws、xhttp。
- sing-box 入站支持 VLESS、hysteria2、tuic 的配置生成（hysteria2 未经过真实样本验证，见「已知限制」）。
- 创建与导出前执行三级校验：L0 结构、L1 语义、L2 用真实内核二进制自检（`xray -test` / `sing-box check` / `mihomo -t`）；运行期逐请求执行，L2 失败直接拒绝下发（HTTP 422）。

### 配置导出

- 导出 mihomo（Clash）YAML 与 sing-box JSON，导出前用对应内核二进制做语法/配置自检。
- xhttp 传输只有 xray 客户端可用；导出给 mihomo / sing-box 时会在运行期剔除并给出告警。

### 订阅与令牌

- 为入站生成订阅链接 `/sub/<token>?target=mihomo|sing-box`（`clash` 作为 mihomo 的别名）。
- 令牌列表 `GET /api/export/tokens` 返回 id、关联入站与客户端、email、target、状态、创建时间、吊销时间、最后访问时间。
- **只存 SHA-256**：列表既不返回明文 token，也不返回 hash；签发与轮换返回的新链接只在响应里出现一次，界面提示自行复制保存。
- 轮换 `POST /api/export/tokens/<id>/rotate` 生成新 token 并作废旧 token；对已吊销令牌轮换返回 409。
- 吊销 `POST /api/export/tokens/<id>/revoke` 是终态，不提供物理删除；吊销后该链接立即不可用。
- 订阅链接泄露即可被他人访问，需要定期轮换。

### 内置 HTTPS 与明文策略

- 可配置 PEM 证书与私钥路径，也可一键自签；自签证书落在数据目录 `config/tls/`，SAN 覆盖 `localhost`、`127.0.0.1`、`::1` 与本机全部网卡地址，有效期 825 天。
- 面板显示证书 SHA-256 指纹、到期时间与剩余天数；距到期不足阈值时启动阶段写日志告警。
- 明文策略三档 `warn` / `loopback-only` / `strict`，**默认 `warn`，升级后行为不变**：`loopback-only` 只放行回环来源的明文登录，非回环直接返回 403；`strict` 一律拒绝明文并返回 403。
- 只有配置了可信代理 CIDR 时，才采信 `X-Forwarded-Proto` / `X-Forwarded-For`。
- 内置 TLS 生效时统一加 `Strict-Transport-Security`；会话 Cookie 为 `HttpOnly; SameSite=Strict`，HTTPS 下加 `Secure`。

### 证书删除

- `DELETE /api/tls/cert?confirm=delete`：`confirm` 必须为 `delete`，不匹配返回 400 并写审计。
- 前置条件是内置 TLS 未启用（配置或运行态任一启用都返回 409），避免重启后起不来。
- 证书与私钥路径都先过路径安全校验，目标必须位于数据目录内；任一越界即整体拒绝，一个文件都不删。
- 删除前记录证书指纹与到期时间；成功记 `tls.deleted`，拒绝记 `reason`（`confirm_mismatch` / `tls_enabled` / `outside_data_dir`）。

### 操作审计日志

- 只追加 JSONL，按月归档到 `logs/audit/YYYY-MM.jsonl`；单文件超过 8 MB 立即轮转，保留 `log_keep_days` 天（默认 14），最多每 24 小时清理一次。
- 字段为：时间、操作者、来源 IP、动作、目标、结果、详情。
- 覆盖登录成功/失败、面板口令初始化与修改、入站增/改/删、客户端增/改/停用/重置/删、3x-ui 导入、更新检查/下载/安装、内核启动/停止/重启/校验、配置导出、TLS 自签与删除、备份创建/恢复、面板设置变更。
- 脱敏在写入前递归执行：键名包含 `password` / `passwd` / `secret` / `token` / `private` / `credential` / `uuid` / `cookie` / `authorization` / `subscription_url` / `sub_url` 的值，或键名为 `key`、以 `key` 结尾且不是 `_path` 的值，统一替换为 `[redacted]`；`_id` / `_path` / `id` 豁免。
- 日志页支持按时间范围、操作者、动作、结果、关键字筛选；CSV 导出对以 `= + - @` 开头的单元格加 `'` 前缀，避免表格公式注入。
- 没有删除或清空接口；写入失败不会影响业务操作，失败次数与最后错误在 `/api/audit` 响应的 `health` 字段里给出。

### 面板数据备份 / 恢复

- `GET /api/backup` 返回包含文件清单（相对路径、用途、字节数）、明确排除项及理由、已有备份列表。
- `POST /api/backup` 写入 `state/backups/manual-<时间戳>-<随机>/`（`manifest.json` + `files/`），每个文件写完后回读校验 SHA-256。
- `POST /api/backup/restore` 需 `confirm=<备份名>`：先校验清单 `kind` / `schema`，再把所有文件读入内存逐个核对 SHA-256（校验失败在改动现场之前返回 400）；通过后先拍 `prerestore-*` 回滚快照，再逐文件原子替换，任一步失败按逆序回滚。
- 纳入备份：`config/panel.json`、`config/sources.json`、`state/state.json`、`state/auth.json`，以及相对路径配置的内置 TLS 证书/私钥。
- 明确排除：`logs/audit/*`、`state/kernels.json`、`state/update.json`、`state/backups/*`、内核日志、规则/Geo 缓存、`configs/generated/*`、`tmp/*`；绝对路径的证书/私钥不猜测、不越界复制，清单里标注未纳入。
- 设置页「数据目录与备份」显示数据根目录、逐项列出包含与排除理由、列出已有备份，可「立即备份」与「恢复」，恢复完成后提示需重启。

### 能力矩阵与契约测试

- [`docs/CAPABILITY-MATRIX.md`](docs/CAPABILITY-MATRIX.md) 与 [`src/capability.rs`](src/capability.rs) 同源，共 61 项 / 17 类实体：必需 46、仅后端 6、有意不做 9（有意不做均附理由）。
- 4 个契约测试：清单唯一性、「有意不做」必须有理由且不得声明路由、`src/http.rs` 必须含每条声明的路由分支、`web/app.js` 必须含每个声明的前端调用。
- 测试直接扫描源码文本，删掉真实路由或前端调用即报红；反向验证的红/绿原始输出保存在 [`docs/evidence/`](docs/evidence/)。

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
| `state/backups/` | schema 升级前的逐字节备份；手动备份为 `state/backups/<name>/`（`manifest.json` + `files/`）；恢复前的回滚快照为 `prerestore-*` |
| `config/tls/` | 一键自签生成的证书与私钥（私钥在 Unix 上写为 0600） |
| `configs/generated/` | 生成的代理配置 |
| `logs/audit/`、`logs/kernels/` | 审计日志与内核日志（按月/按内核轮转） |
| `data/rulesets/`、`data/geo/` | 规则集与地理数据 |
| `tools/kernel/`、`runtime/kernels/` | 面板自动发现的内核二进制（按内核名/版本分目录） |
| `tmp/`、`tmp/update/` | 临时文件与更新下载残片 |

「仅下载」的安装包落在用户默认下载目录（Windows 为 `%USERPROFILE%\Downloads`，Linux 为 `~/Downloads`），并在旁边写出同名 `.sig` 签名文件。

`panel.json` / `state.json` 在版本升级时自动迁移并先写备份；迁移失败会回滚并拒绝启动，`schema_version` 高于当前程序版本时拒绝启动且不改写文件。

手动备份包含 `config/panel.json`、`config/sources.json`、`state/state.json`、`state/auth.json` 以及相对路径配置的内置 TLS 证书/私钥；审计日志、内核 PID、升级进度与可再生缓存不参与备份（设置页会逐项列出包含与排除理由）。恢复前逐个核对文件 SHA-256，校验失败在改动现场之前返回、不动现场；通过后先拍 `prerestore-*` 快照，再逐文件原子替换，写入失败按逆序自动回滚。恢复完成后需要重启进程，运行内存里仍是旧配置；恢复也不是跨文件事务，进程若在替换中被打断，需从快照人工恢复（见「已知限制」）。

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
- **内置 HTTPS 已支持，但没有 ACME**：可配置 PEM 证书或一键自签，不自动申请公网证书，也不做证书自动续期；`strict` 明文策略一律返回 403，不做 HTTP → HTTPS 跳转；证书、私钥或明文策略变更后需重启才生效。
- **恢复不是跨文件事务**：`config/` 与 `state/` 下的文件无法作为整体原子替换。写入失败会按逆序自动回滚，但进程若在恢复过程中被强制杀死，仍可能留下部分文件已替换的状态，需从 `prerestore-*` 快照人工恢复。
- **恢复后需重启**：恢复只替换文件，运行进程内存中仍是旧配置，界面会提示重启，不会自动重启。
- **Windows 下 0600 不生效**：私钥权限 0600 是 Unix 权限位，Windows 上只能依赖数据目录本身的访问控制。
- **入站与客户端可改字段有限**：入站只改备注/监听/端口/启用；客户端只改 email/配额/到期/启用/flow；协议、安全层、UUID 不可改。
- **没有退出登录按钮**：`POST /api/auth/logout` 可用，但界面未放入口，需清除 Cookie 或等会话过期。
- **部分流程只有代码接线、未逐条实跑**：更新流程的审计事件没有用真实更新链路逐条核对；`export.download` 只验证了拒绝路径，成功下载的审计未实跑确认；日志页筛选与 CSV 导出未在真实浏览器逐项点击验证。
- **单实例、单数据目录**：不支持集群、多机同步或并发多写入者。
- **1.0.0 不含更新入口**：首次升级到 1.0.1 必须手动替换可执行文件；之后才能使用设置页的自更新。
- **自更新替换路径的平台差异**：Windows 上的替换与自动重启已在测试中实跑；Linux 的替换逻辑已实现但未实跑。自更新还需要当前进程对可执行文件所在目录有写权限，放在只读目录或包管理目录时请手动更新。
- **3x-ui 导入的覆盖范围**：JSON 与数据库入口以手头样本（v2.9.4 风格、v3.11.0）验证；更老的版本或非标准字段可能被跳过，预览页会列出原因。
- **内核版本**：已验证 xray 25.6.8、sing-box 1.14.4、mihomo 1.19.33 的配置自检；其他版本未逐一验证。

## 文档索引

- [`docs/CAPABILITY-MATRIX.md`](docs/CAPABILITY-MATRIX.md)：能力清单，以及「仅后端 / 有意不做」的判断依据。
- [`docs/PATH-SAFETY-AUDIT.md`](docs/PATH-SAFETY-AUDIT.md)：路径安全审计（删除、恢复、证书操作的边界）。
- [`docs/INCIDENT-2026-11-06.md`](docs/INCIDENT-2026-11-06.md)：一次事故的复盘记录。
- [`docs/evidence/`](docs/evidence/)：契约反向验证的红/绿原始输出。
- [`release-notes/`](release-notes/)：各版本发布说明，当前版本入口为 [`1.0.3.md`](release-notes/1.0.3.md)。

## 许可

Apache-2.0（见 `Cargo.toml` 的 `license` 字段；仓库目前未附 `LICENSE` 文本）。

## 反馈

问题与建议请提交到 GitHub Issues：<https://github.com/yunyu697/berthio/issues>。
