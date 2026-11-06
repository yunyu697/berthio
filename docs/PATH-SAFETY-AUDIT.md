# 删除 / 移动 / 覆盖操作审计（2026-11-06）

范围：整个仓库，含 `scripts/`、`src/`、`web/`、`tools/`、CI 配置。

## 0. 结论

- 审计条目 **44 条**：
  - PowerShell 21 条（含 2 条仅清理环境变量）
  - Rust 17 条
  - Shell 4 条
  - Web / CI 2 条
- 其中**文件系统删除 / 移动 / 覆盖** 40 条；环境变量清理 2 条、配置项删除 1 条、容器内构建清理 1 条。
- 所有 PowerShell 文件级破坏性操作已收敛到 `scripts/lib/path-guard.ps1`；
- 所有 Rust 递归删除已收敛到 `src/safety.rs`；
- Go 侧新增 `tools/pathguard`，所有 Go 删除必须走 `RemoveAll` / `RemoveFile`；
- Shell 的 `rm -rf` 已收敛到 `scripts/lib/path-guard.sh`。
- 护栏删除前会把“解析后的绝对路径 + 条目数”写入确认清单（JSONL），并打印清单路径；默认 dry-run，显式开关才执行。

## 1. PowerShell

“用户可配置”指路径是否来自参数 / 环境变量；“风险”按误操作后果评估。

| # | 位置 | 操作 | 作用路径 | 用户可配置 | 风险 | 处置 |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | `scripts/lib/path-guard.ps1:251` | `Remove-Item -LiteralPath -Recurse -Force` | 护栏内部唯一递归删除点 | 是（调用方传入） | 高 | 允许根 + 主目录/根拒绝 + 清单 + `-Apply` |
| 2 | `scripts/lib/path-guard.ps1:253` | `Remove-Item -LiteralPath -Force` | 护栏内部单文件删除点 | 是 | 高 | 同上 |
| 3 | `scripts/lib/path-guard.ps1:256` | `Copy-Item -LiteralPath -Force -Recurse` | 目标必须在允许根下 | 是 | 中 | 目标过允许根，源仅做安全校验 |
| 4 | `scripts/lib/path-guard.ps1:257` | `Move-Item -LiteralPath -Force` | 源与目标都必须在允许根下 | 是 | 高 | 双向校验 + 清单 + `-Apply` |
| 5 | `scripts/test-update.ps1:40` | 删除测试工作子目录 | `$Work`（默认仓库 `tmp/`） | 是（`-Work`） | 中 | `-AllowedRoot $Work` + `-Apply` |
| 6 | `scripts/test-update.ps1:145` | 复制测试二进制 | `$FeedRoot/bin-ok.exe` | 是 | 低 | `-AllowedRoot $Work` + `-Apply` |
| 7 | `scripts/test-update.ps1:247` | 删除 `state/update.json` | `$HomeDir/state/update.json` | 是 | 低 | `-AllowedRoot $HomeDir` + `-Apply` |
| 8 | `scripts/test-update.ps1:252` | 复制清单 | `$FeedRoot/ui-new.json` | 是 | 低 | 同上 |
| 9 | `scripts/test-update.ps1:254` | 复制清单 | `$FeedRoot/ui-latest.json` | 是 | 低 | 同上 |
| 10 | `scripts/test-update.ps1:270` | 复制二进制 | `$Work` 下安装副本 | 是 | 中 | 同上 |
| 11 | `scripts/test-update.ps1:271` | 复制二进制 | `$FeedRoot/bin.exe` | 是 | 低 | 同上 |
| 12 | `scripts/test-update.ps1:274` | 复制签名 | `$FeedRoot/install.sig` | 是 | 低 | 同上 |
| 13 | `scripts/test-update.ps1:96,97,285,286` | `Remove-Item Env:\BERTHIO_*` | 进程环境变量 | 否 | 低 | 不涉及文件系统，保留 |
| 14 | `scripts/test-m3-auth.ps1:183` | 递归删除测试 home | `$bh`（仓库 `tmp/`） | 否 | 中 | `-AllowedRoot $tmpRoot` + `-Apply` |
| 15 | `scripts/test-m3-kernels.ps1:190` | 删除 `state/kernels.json` | `$bh/state/kernels.json` | 否 | 低 | `-AllowedRoot $bh` + `-Apply` |
| 16 | `scripts/test-m3-kernels.ps1:226` | 递归删除测试 home | `$bh`（仓库 `tmp/`） | 否 | 中 | `-AllowedRoot 仓库 tmp` + `-Apply` |
| 17 | `scripts/test-m5-runtime.ps1:62` | `Remove-Item Env:BERTHIO_TRUST_PROXY` | 进程环境变量 | 否 | 低 | 保留 |
| 18 | `scripts/test-m5-soak.ps1:124` | `Remove-Item Env:BERTHIO_TRUST_PROXY` | 进程环境变量 | 否 | 低 | 保留 |
| 19 | `scripts/test-m5-upgrade.ps1:121` | `Remove-Item Env:BERTHIO_LISTEN` | 进程环境变量 | 否 | 低 | 保留 |
| 20 | `scripts/test-path-guard.ps1:60,64,74,78,81` | 删除 / 复制 / 移动 | 仓库 `tmp/path-guard-*` | 否 | 低 | 自测全部经护栏；只写 D: |
| 21 | `scripts/build-demo.ps1`、`test-m2`、`test-m4`、`test-m5-json` | 无删除 / 移动 / 覆盖 | 仅写自建测试目录 | 否 | 低 | 临时目录已改到仓库 `tmp/` |

## 2. Rust

| # | 位置 | 操作 | 作用路径 | 用户可配置 | 风险 | 处置 |
| --- | --- | --- | --- | --- | --- | --- |
| 22 | `src/safety.rs:196` | `fs::remove_dir_all` | 护栏内部唯一递归删除点 | 是 | 高 | 允许根 + 主目录/根拒绝 + 打印路径与条目数 |
| 23 | `src/safety.rs:207` | `fs::remove_file` | 护栏内部单文件删除点 | 是 | 高 | 同上 |
| 24 | `src/main.rs:77` | `remove_dir_all`（`--demo`） | `%TEMP%/berthio-demo-<pid>` | 否 | 中 | 允许根=`%TEMP%`；目标等于主目录/根会被拒 |
| 25 | `src/migrate.rs:1248` | `remove_dir_all`（迁移快照） | `<home>/tmp/migrate-*` | 否 | 中 | 允许根=`<home>/tmp` |
| 26 | `src/export.rs:609` | `remove_file`（导出失败清理） | 导出目标文件 | 是 | 中 | 允许根=目标父目录 |
| 27 | `src/update.rs:637` | `remove_file`（`.part`/`.tmp`） | `<home>/tmp/update` | 否 | 中 | 允许根=该目录 |
| 28 | `src/update.rs:658` | `remove_file`（旧程序） | 程序目录 `*.old-*` | 否 | 中 | 允许根=程序目录 |
| 29 | `src/migrate.rs:221,224` | `fs::copy`（快照副本） | 快照目录固定后缀 | 否 | 低 | 固定派生路径，保留 |
| 30 | `src/migrate.rs:230,231,240,241,242` | `remove_file`（快照 sidecar） | 快照目录固定后缀 | 否 | 低 | 单文件、非递归，保留 |
| 31 | `src/update.rs:937,955,988,996,998,1018,1028,1099` | `remove_file`（下载失败/换源） | 下载/临时目录 `.part`、`staged` | 否 | 中 | 固定派生名、单文件，保留 |
| 32 | `src/update.rs:997,1098` | `fs::rename`（`.part` → 目标） | 下载目录内 | 否 | 中 | 同目录原子替换，保留 |
| 33 | `src/update.rs:1172,1232,1242,1259` | `fs::copy`（暂存/备份/写新程序） | 临时目录 → 程序目录 | 否 | 中 | 目标由进程自身位置派生，保留 |
| 34 | `src/update.rs:1239,1240,1243,1248,1249,1252,1258,1262,1263` | `remove_file` / `rename`（安装回滚） | 程序目录 exe / `.old-*` / `.new` | 否 | 中 | 单文件、固定后缀，保留 |
| 35 | `src/util.rs:131,132,134` | `remove_file` / `rename`（原子写） | `<home>/state`、`<home>/config` 固定文件 | 否 | 低 | 同目录原子替换，保留 |
| 36 | `src/supervisor.rs:569,571,572` | `rename` / `remove_file`（日志轮转） | 日志目录固定文件 | 否 | 低 | 保留 |
| 37 | `src/supervisor.rs:898` | `fs::copy`（last-known-good） | 日志目录固定文件 | 否 | 低 | 保留 |
| 38 | `src/update.rs:1315,1458,1490` | `remove_dir_all`（`#[cfg(test)]`） | 测试临时目录 | 否 | 低 | 仅测试代码，不随二进制发布 |

关于第 29–37 条：目标不是“用户输入拼接后递归删除”，而是进程自身位置或固定后缀的暂存/状态文件；递归删除与可被外部输入影响的路径已全部接入护栏。

## 3. Shell

| # | 位置 | 操作 | 作用路径 | 用户可配置 | 风险 | 处置 |
| --- | --- | --- | --- | --- | --- | --- |
| 39 | `scripts/lib/path-guard.sh` | `rm -rf --` | 护栏内部唯一删除点 | 是 | 高 | 非空/非根/非主目录/允许根/清单/`--apply` |
| 40 | `scripts/test-check-version.sh:10` | `trap ... rm -rf $TMP` | `mktemp -d` 临时目录 | 否 | 中 | 改为 `guard_rm_rf "$TMP" "$TMP_ROOT" --apply` |
| 41 | `scripts/linux/test-linux-migrate.sh:16` | 清空 `/work/home` | `/work/home` | 否 | 中 | 改为 `guard_rm_rf "$H" /work --apply`，随后重建 |
| 42 | `scripts/linux/Dockerfile:12` | `rm -rf /var/lib/apt/lists/*` | 构建容器内 apt 缓存 | 否 | 低 | 一次性构建层，不触碰宿主，保留 |

## 4. Web / CI / Go

| # | 位置 | 操作 | 说明 | 风险 | 处置 |
| --- | --- | --- | --- | --- | --- |
| 43 | `web/app.js` | `DELETE /api/mirrors/<id>` | 只删配置项，不涉及文件系统 | 低 | 保留 |
| 44 | `.github/workflows/release.yml` | `cp` 产物到 `dist/` | 工作区内固定路径，同名覆盖属预期打包行为 | 低 | 保留 |
| — | `tools/pathguard` | Go 护栏 | 仓库原先无 Go 代码；本版新增，所有 Go 删除必须经 `RemoveAll` / `RemoveFile` | — | 新增 |

## 5. 护栏自测

PowerShell `scripts/test-path-guard.ps1`：

- 拒绝用例 **11 项全部通过**：`C:\Users\Tyler Zhang`、`C:\`、`$env:USERPROFILE`、空串、`$null`、`..\..\Users`、`..\..\Users\Tyler Zhang`、`$env:USERPROFILE` 字面量、`%USERPROFILE%`、`%TEMP%`、用户主目录内部路径；
- 放行与执行：允许根下临时目录、dry-run 不删除、确认清单落盘、`-Apply` 删除、复制、移动、清理，共 6 项通过；
- 自测只在 D: 盘项目 `tmp/` 下创建与删除。

Go `tools/pathguard`：

- `go test -v` 3 个测试全部通过；
- 拒绝用例 10 项：空串、`C:\Users\Tyler Zhang`、`C:\`、`$env:USERPROFILE`、`..\..\Users\Tyler Zhang`、`%TEMP%`、含 `$VAR`、用户主目录本身、主目录祖先、主目录内部；
- 放行 / dry-run / 清单落盘 / `-Apply` / 拒绝允许根本身均通过。

Rust `src/safety.rs`：单元测试覆盖空路径、文件系统根、主目录及祖先、未展开变量、允许根本身被拒；临时目录子项放行并删除。
