# 删除 / 移动 / 覆盖操作审计（2026-11-06）

范围：整个仓库，含 `scripts/`、`src/`、`web/`、CI 配置。

结论概要：

- 仓库内**没有 Go 代码**（`*.go` 为 0 个），因此指令中的 “Go 护栏” 以 Rust 等价实现 `src/safety.rs` 落地。
- 所有 PowerShell 文件级删除 / 移动 / 覆盖已收敛到 `scripts/lib/path-guard.ps1`。
- 所有递归删除（Rust `remove_dir_all`）已收敛到 `src/safety.rs`；单文件、固定派生路径的安装 / 状态替换保留原逻辑，见下表说明。
- Shell 脚本的 `rm -rf` 已收敛到 `scripts/lib/path-guard.sh`（默认 dry-run，显式 `--apply`）。
- 所有 `scripts/*.ps1` 已加 `Set-StrictMode -Version Latest`，测试临时目录默认改为仓库 `tmp/`（D: 盘），不再默认写 `$env:TEMP`。

## 1. PowerShell

| 位置 | 操作 | 作用路径 | 前置校验 | 处置 |
| --- | --- | --- | --- | --- |
| `scripts/lib/path-guard.ps1:214` | `Remove-Item -LiteralPath -Recurse -Force` | 护栏函数内部，唯一执行点 | 允许根、非根/主目录、非空、非未展开变量、执行前复核、`-Apply` | 新增，唯一合法删除入口 |
| `scripts/lib/path-guard.ps1:216` | `Remove-Item -LiteralPath -Force` | 同上（单文件） | 同上 | 新增 |
| `scripts/lib/path-guard.ps1:219` | `Copy-Item -LiteralPath -Force -Recurse` | 目标必须在允许根下 | 源仅做安全校验，目标必须过允许根 | 新增 |
| `scripts/lib/path-guard.ps1:220` | `Move-Item -LiteralPath -Force` | 源与目标都必须在允许根下 | 双向校验 | 新增 |
| `scripts/test-update.ps1:40` | 删除测试工作目录下的子项 | `$Work` 下的 `feed/`、`home/` 等 | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:145` | 复制测试二进制 | 目标 `$FeedRoot/bin-ok.exe` | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:247` | 删除 `state/update.json` | `$HomeDir/state/update.json` | `-AllowedRoot $HomeDir` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:252` | 复制清单 | 目标 `$FeedRoot/ui-new.json` | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:254` | 复制清单 | 目标 `$FeedRoot/ui-latest.json` | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:270` | 复制二进制 | 目标 `$Work` 下安装副本 | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:271` | 复制二进制 | 目标 `$FeedRoot/bin.exe` | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:274` | 复制签名 | 目标 `$FeedRoot/install.sig` | `-AllowedRoot $Work` + `-Apply` | 已改造 |
| `scripts/test-update.ps1:96,97,285,286` | `Remove-Item Env:\BERTHIO_*` | 环境变量，不是文件 | 无需路径校验 | 保留（仅清理进程环境） |
| `scripts/test-m3-auth.ps1:183` | 递归删除测试 home | `$bh`（仓库 `tmp/` 下） | `-AllowedRoot $tmpRoot` + `-Apply` | 已改造 |
| `scripts/test-m3-kernels.ps1:190` | 删除 `state/kernels.json` | `$bh/state/kernels.json` | `-AllowedRoot $bh` + `-Apply` | 已改造 |
| `scripts/test-m3-kernels.ps1:226` | 递归删除测试 home | `$bh`（仓库 `tmp/` 下） | `-AllowedRoot 仓库 tmp` + `-Apply` | 已改造 |
| `scripts/test-m5-runtime.ps1:62` | `Remove-Item Env:BERTHIO_TRUST_PROXY` | 环境变量 | 无需路径校验 | 保留 |
| `scripts/test-m5-soak.ps1:124` | `Remove-Item Env:BERTHIO_TRUST_PROXY` | 环境变量 | 无需路径校验 | 保留 |
| `scripts/test-m5-upgrade.ps1:121` | `Remove-Item Env:BERTHIO_LISTEN` | 环境变量 | 无需路径校验 | 保留 |
| `scripts/test-path-guard.ps1:57,60,69,73,76` | 护栏自测中的删除 / 复制 / 移动 | 仓库 `tmp/path-guard-*` | 只允许 D: 盘；全部经护栏 | 新增 |

其余 PowerShell 脚本（`build-demo.ps1`、`test-m2-mirrors.ps1`、`test-m4-migrate.ps1`、`test-m5-json.ps1`）：无文件删除 / 移动 / 覆盖；它们只写自己创建的测试目录，临时目录已改到仓库 `tmp/`。

说明：`$env:TEMP` 默认路径已从所有测试脚本移除，避免在 C: 盘用户目录附近创建 / 删除测试数据。

## 2. Shell

| 位置 | 操作 | 作用路径 | 前置校验 | 处置 |
| --- | --- | --- | --- | --- |
| `scripts/lib/path-guard.sh` | `rm -rf --` | 仅 `guard_rm_rf` 内部 | 非空、非 `/`、非主目录/祖先/内部、必须在允许根下、打印条目数、`--apply` | 新增，唯一删除入口 |
| `scripts/test-check-version.sh:10` | `trap ... rm -rf $TMP` | `mktemp -d` 生成的临时目录 | 改为 `guard_rm_rf "$TMP" "$TMP_ROOT" --apply` | 已改造 |
| `scripts/linux/test-linux-migrate.sh:16` | 清空 `/work/home` | `/work/home` | 改为 `guard_rm_rf "$H" /work --apply`，随后重建 | 已改造 |
| `scripts/linux/Dockerfile:12` | `rm -rf /var/lib/apt/lists/*` | 构建容器内 apt 缓存 | 容器一次性构建层，不触碰宿主 | 保留（非宿主路径） |

## 3. Rust

| 位置 | 操作 | 作用路径 | 前置校验 | 处置 |
| --- | --- | --- | --- | --- |
| `src/safety.rs:186` | `fs::remove_dir_all` | 仅护栏内部 | 绝对化/解链接、允许根、非根/主目录/祖先、非未展开变量、打印路径与条目数 | 新增，唯一递归删除入口 |
| `src/safety.rs:201` | `fs::remove_file` | 仅护栏内部 | 同上 | 新增 |
| `src/main.rs:77` | `remove_dir_all`（`--demo`） | `%TEMP%/berthio-demo-<pid>` | 允许根=`%TEMP%`；目标等于用户主目录/根会被拒绝 | 已接护栏 |
| `src/migrate.rs:1248` | `remove_dir_all`（迁移快照） | `<home>/tmp/migrate-*` | 允许根=`<home>/tmp` | 已接护栏 |
| `src/export.rs:609` | `remove_file`（导出失败清理） | 导出目标文件 | 允许根=目标父目录 | 已接护栏 |
| `src/update.rs:637` | `remove_file`（`.part`/`.tmp` 清理） | `<home>/tmp/update` 下 | 允许根=该目录 | 已接护栏 |
| `src/update.rs:658` | `remove_file`（旧程序清理） | 程序目录下 `*.old-*` | 允许根=程序目录 | 已接护栏 |
| `src/migrate.rs:221,224` | `fs::copy`（快照副本） | `<home>/tmp/migrate-*/…` | 路径由固定家目录派生；目标为快照目录内固定后缀 | 保留（非删除；写入固定临时目录） |
| `src/migrate.rs:230,231,240,241,242` | `remove_file`（快照 sidecar） | 快照目录内 `*.copy.db` / `-wal` / `-shm` | 路径由快照目录派生、单文件、非递归 | 保留（审计留痕） |
| `src/update.rs:937,955,988,996,998,1018,1028,1099` | `remove_file`（下载失败/换源清理） | 下载目录或临时目录下的 `.part` / `staged` | 固定派生文件名、同目录、单文件 | 保留（审计留痕） |
| `src/update.rs:997,1098` | `fs::rename`（`.part` → 目标） | 下载目录内 | 同一目录内原子替换；`target` 由镜像 URL 文件名派生 | 保留 |
| `src/update.rs:1172,1232,1242,1259` | `fs::copy`（暂存 / 备份 / 写新程序） | 临时目录 → 程序目录 | 目标为当前 exe / 备份 / 暂存，路径由进程自身位置派生 | 保留 |
| `src/update.rs:1239,1248,1252,1258,1263` | `remove_file` / `rename`（安装回滚） | 程序目录内 exe / `.old-*` / `.new` | 单文件、固定后缀、非递归 | 保留（审计留痕） |
| `src/util.rs:131,132,134` | `remove_file` / `rename`（原子写状态/配置） | `<home>/state`、`<home>/config` 下固定文件 | 固定文件名、同目录原子替换 | 保留（审计留痕） |
| `src/supervisor.rs:569,571,572` | `rename` / `remove_file`（日志轮转） | 日志目录下 `*.log` / `*.log.1..4` | 固定文件名、同目录 | 保留 |
| `src/supervisor.rs:898` | `fs::copy`（last-known-good） | 日志目录下固定文件 | 固定文件名 | 保留 |
| `src/update.rs:1315,1458,1490` | `remove_dir_all`（`#[cfg(test)]`） | 测试临时目录 | 仅测试代码 | 保留（测试代码，不随二进制发布） |

关于单文件操作的处置理由：它们不是“按用户输入拼接路径后递归删除”，目标要么由进程自身位置派生（exe / 备份 / 旧程序），要么是固定后缀的暂存文件；不存在把用户主目录当目标的路径。递归删除与可被外部输入影响的路径已全部接入护栏。

## 4. Web / CI / 其他

| 位置 | 操作 | 说明 |
| --- | --- | --- |
| `web/app.js` | `DELETE /api/mirrors/<id>` | 只删除配置中的镜像条目，不涉及文件系统 |
| `.github/workflows/release.yml` | 无删除 / 移动；仅 `cp` 产物到 `dist/` | 目标为工作区内固定路径；`cp` 会覆盖同名产物，属预期打包行为 |
| Go 代码 | 无 | 仓库内无 `.go` 文件 |

## 5. 自测

`scripts/test-path-guard.ps1` 覆盖：

- 必须拒绝：`C:\Users\Tyler Zhang`、`C:\`、`$env:USERPROFILE`、`""`、`$null`、`..\..\Users`、`$env:USERPROFILE` 字面量、`%USERPROFILE%`、用户主目录内的路径；
- 必须放行：允许根下的临时目录；
- 默认 dry-run 不删除；显式 `-Apply` 才删除；
- 复制 / 移动同样经过护栏。

`src/safety.rs` 单元测试覆盖：空路径、`/`、主目录及其祖先、`$HOME` / `%TEMP%`、允许根等于目标、临时目录子项放行与实际删除。
