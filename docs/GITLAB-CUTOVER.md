# GitLab 托管切换清单（Berthio）

本文件是**切换当天照做的清单**，不是已经执行的动作。当前 `DEFAULT_FEED` 仍指 GitHub，
Berthio 的更新通道没有切换到 GitLab。

实测时间：2026-11-07。GitLab 项目：<https://gitlab.com/yunyu697/berthio>（public）。

## 0. 已被实测确认的前提

| 项 | 结果 |
|---|---|
| 项目可见性 | `visibility=public`，匿名可读 |
| 匿名 clone | 空 HOME、无 credential helper、`GIT_TERMINAL_PROMPT=0`，`rc=0` |
| Pages | `https://berthio-70ab2d.gitlab.io/` 与 `https://yunyu697.gitlab.io/berthio/` 均匿名 200 |
| Pages 内容 | 与 `public/index.html` 字节一致（sha256 `0495f8ab…59c5d`），291,611 字节 |
| Release | 只有 `1.0.3` 一个；`https://gitlab.com/yunyu697/berthio/-/releases/1.0.3` |
| Release 附件 | 6 个匿名可下载：Linux/Windows 二进制、demo、`SHA256SUMS`、两个 `.minisig` |
| 附件一致性 | 与 GitHub 同名资产 SHA-256 全部一致（见第 4 节） |
| 历史 tag | 12 个全部存在，但**不建历史 Release**（政策：GitLab 上只保留最新一个 Release） |
| GitHub 侧 | 仓库、12 个历史 Release、tag 均未改动，发布能力保留 |

## 1. 地址对照（GitLab 侧已可用）

```
仓库        https://gitlab.com/yunyu697/berthio
Release     https://gitlab.com/yunyu697/berthio/-/releases/1.0.3
附件基址    https://gitlab.com/yunyu697/berthio/-/releases/1.0.3/downloads/<文件名>
包注册表    https://gitlab.com/api/v4/projects/88632907/packages/generic/berthio/1.0.3/<文件名>
Pages       https://berthio-70ab2d.gitlab.io/（yunyu697.gitlab.io/berthio/ 会跳到它）
```

附件文件名沿用 GitHub 的命名（`berthio-<版本>-linux-x86_64`、
`berthio-<版本>-windows-x86_64.exe`、`SHA256SUMS`、`<文件名>.minisig`），
这样 `latest.json` 的 `platforms.*.url` 只需要换基址。

## 2. 切换当天需要改的位置（共 4 处代码 + 1 处文档 + 1 处外部资产）

| # | 位置 | 现在 | 切换为 |
|---|---|---|---|
| 1 | `src/update.rs:23-24` `DEFAULT_FEED` | `https://github.com/yunyu697/berthio/releases/download/updates/latest.json` | GitLab 的 `latest.json` 稳定地址（见 2.1） |
| 2 | `src/update.rs:22` `HOMEPAGE` | GitHub 仓库地址 | `https://gitlab.com/yunyu697/berthio` |
| 3 | `scripts/lib/berthio-common.sh:9` `BERTHIO_FEED`，及 `:62` / `:67` 的资产与 `SHA256SUMS` 拼接 | 全是 `https://github.com/%s/releases/download/...` | 换成 GitLab 的附件基址与包地址 |
| 4 | `scripts/lib/berthio-common.ps1:5` `$BerthioFeed`，及 `:67` / `:72` 同上 | 同上 | 同上 |
| 5 | `web/app.js:50-51`（仅演示模式的默认值） | GitHub 主页 + GitHub feed | 与真实配置保持一致，否则 demo 里显示的地址会误导 |
| 6 | `web/index.html:2352`「打开主页」链接 | GitHub 仓库 | GitLab 仓库（或两个都给） |
| 7 | `README.md:107` 等文档里的清单地址说明 | GitHub feed | 同步改，并保留“可用 `config/panel.json` 覆盖”的说明 |

`src/state.rs:101` 的 `default_update_feed()` 只是读取 `DEFAULT_FEED` 常量，不用单独改。
`DEFAULT_MIRRORS`（`src/update.rs:33-37`）见第 3 节，不能照搬。

### 2.1 `latest.json` 放在哪

GitLab Release 的附件目录是按版本绑定的，`updates` 这种“跟着版本走的清单”不适合放 Release。
建议用 Generic Package 单独建一个版本号：

```
上传：PUT /projects/88632907/packages/generic/berthio/updates/latest.json
地址：https://gitlab.com/api/v4/projects/88632907/packages/generic/berthio/updates/latest.json
```

该地址在 public 项目下匿名可取（与本次实测的 Release 附件同一套匿名下载路径）。
切换当天必须先把 `latest.json` 传上去，再改 `DEFAULT_FEED`，否则新安装的用户会 404。

### 2.2 切换后的 `latest.json` 形状

字段结构不变，只换 `url`（必须是绝对地址）：

```json
{
  "version": "1.0.3",
  "notes": "…",
  "pub_date": "…",
  "platforms": {
    "linux-x86_64": {
      "url": "https://gitlab.com/yunyu697/berthio/-/releases/1.0.3/downloads/berthio-1.0.3-linux-x86_64",
      "size": 4590672,
      "signature": "<base64，原样保留>"
    },
    "windows-x86_64": {
      "url": "https://gitlab.com/yunyu697/berthio/-/releases/1.0.3/downloads/berthio-1.0.3-windows-x86_64.exe",
      "size": 4375040,
      "signature": "<base64，原样保留>"
    }
  }
}
```

`signature` 与 `.minisig` 文件是 minisign 对**文件字节**的签名，和托管主机无关，
只要文件字节一致就不用重签；本次实测两边 SHA-256 完全一致。

## 3. 国内镜像前缀不适用于 GitLab

`DEFAULT_MIRRORS` 是 `ghproxy.net` / `gh-proxy.com` / `ghfast.top`，
`url_candidates()`（`src/update.rs:235-264`）会把前缀**拼在任意地址前面**，
所以换到 GitLab 后会生成
`https://gh-proxy.com/https://gitlab.com/...` 这种地址。
这三个服务都是面向 GitHub 的反代，拼接 GitLab 地址大概率失败，
等于把“国内可下载”的兜底丢掉（建议切换前先各测一次，失败就按下述替代方案处理）。

替代方案，按代价从低到高：

1. **只改配置，不改代码**：在 `config/panel.json` 的 `update.feed` 填自建反代地址，
   `update.mirrors` 填 `[{"id":"mirror","prefix":"https://<自建域名>/"}]`。
   `mirrors` 的语义是“前缀 + 原地址”，自建反代只要能按路径反代 GitLab 就能用。
2. **自建对象存储/CDN**：把二进制与 `latest.json` 同步放一份到支持直链的对象存储，
   `DEFAULT_FEED` 直接指过去（签名与 SHA-256 校验仍成立）。
3. **保留 GitHub 作为下载源**：`DEFAULT_FEED` 不动，GitLab 只作为代码托管与 CI，
   发布资产继续由 GitHub 承载——这是最省事、最不影响老用户的选择。

## 4. 对已安装用户的影响

- **1.0.3 及更早版本的用户**：`DEFAULT_FEED` 是编译进二进制的常量，
  他们只会访问 GitHub 的 `updates/latest.json`。改代码不会影响他们。
- **只改 GitHub 上的 `latest.json`**：老用户仍能读到清单；如果清单里的 `url` 指向 GitLab，
  由于目标仓库已 public，下载与验签都能成功（本次已实测匿名下载 + 哈希一致）。
- **GitHub 仓库归档或删除**：老用户“检查更新”会直接失败（清单 404），
  除非他们手动改 `config/panel.json` 的 `update.feed`。所以 GitHub 的 `updates` Release
  必须保留到确认老用户基本升级完毕（建议至少保留到 1.1.x 发布之后）。
- **企业内网白名单**：如果只放行了 `github.com`，切到 `gitlab.com` 后需要补放行，
  否则更新检查会超时；这属于部署侧改动，代码里无法兜底。

## 5. 切换当天的执行顺序

1. 在 GitLab Generic Package 上传 `latest.json`（版本号 `updates`）并匿名核对内容。
2. 发布新版本时，二进制、`SHA256SUMS`、`.minisig` 同时上传到对应版本的包与 Release，
   并用 `direct_asset_path` 生成 `/-/releases/<版本>/downloads/<文件名>`。
3. 改第 2 节的 4 处代码 + 文档，跑 `cargo test --locked` 与 `bash scripts/check-version.sh`。
4. 用 `config/panel.json` 的 `update.feed` 先在一台机器上验证新版能检查、下载、验签、安装。
5. 确认无误后再发版；**不要**在发版前把 GitHub 的 `latest.json` 指向尚未验证的地址。
6. 回滚：把 `DEFAULT_FEED` 改回 GitHub 常量即可；GitHub 上的资产一直没动。

## 6. 本次实测的哈希（可用于切换时复核字节一致）

```
e83508bb00825bc8e7e79292d38cf7c9573f931de2a1b8f132199d34051e6d09  berthio-1.0.3-linux-x86_64        4,590,672 B
449c3867d83df79f7f8983a1e3a684205581e760636e175cdfc8cdee3f2cfc75  berthio-1.0.3-windows-x86_64.exe  4,375,040 B
0495f8abd5ce69cbc33108dd75706d3f8274083ec77b85f9732bb3504e059c5d  berthio-1.0.3-demo-m5.html          291,611 B
ae23f70e31098c350c705d0579431f77d029a109efef46fbcf70df59a97280f9  SHA256SUMS                              285 B
dbd35d8a2e7c653a72af4a5ec9c21f6dc876526b0162e50e805c4274952c9d24  berthio-1.0.3-linux-x86_64.minisig       331 B
5e2a329fd5bb3e133bdf35661e8cacaf213325ca3240840fd09f3d5dea90f9d2  berthio-1.0.3-windows-x86_64.exe.minisig 337 B
```
