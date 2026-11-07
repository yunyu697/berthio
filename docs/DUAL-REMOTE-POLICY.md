# 双端维护规范（GitLab + GitHub）

适用：berthio、luci-app-ferry、sidebay，以及后续从 GitHub 试点迁到 GitLab 的仓库。
迁移后 **GitHub 保留**，两端同时维护；不是单向搬家。

## 1. 主源与同步机制

**主源：GitLab**（`yunyu697/<repo>`，public）。代码提交、tag、Pages、校验 CI 都在 GitLab 产生。
**GitHub 是只读镜像 + 当前发布承载**：保留 Actions 发版流水线、历史 Release 与 `updates/latest.json`。

同步方式：**本地一个工作副本 + 两个 remote，手工 fast-forward 推送**。

```bash
# 提交后
git push gitlab main          # 主源
git push origin main          # 镜像（只允许 fast-forward）
```

理由：

- 免费、可控，两端 SHA 一目了然，不依赖任何第三方桥接服务；
- 不需要把某一端的 token 存进另一端的设置里（GitLab pull mirror 必须存 GitHub PAT，GitHub Action 必须存 GitLab PAT）；
- 出问题时可以用 `git ls-remote` 直接对账，没有“镜像延迟”这类不可见状态。

备选方案与代价：

| 方案 | 代价 |
|---|---|
| GitLab pull mirror（GitHub → GitLab） | 需要 GitLab Premium；必须把 GitHub PAT 存在 GitLab 项目设置里；只同步分支/tag，不同步 Release 附件；轮询有延迟，失败时不会通知 |
| GitHub Action 反向同步（GitHub → GitLab） | 需要把 GitLab PAT 存成 GitHub secret；每次 push 触发一次，容易和人工推送打架；Action 失败时两端悄悄分叉 |
| 单向搬家（迁完停 GitHub） | 与“双端维护”决策冲突；老用户的 `latest.json` 与历史 Release 仍在 GitHub，直接断供 |

## 2. 提交规则

1. **代码只在一端提交**：统一在 GitLab 提交（主源）。GitHub 上不直接 commit、不在网页改文件、不接受直接推到 `main` 的 PR。
2. 每次提交后立刻推 GitHub，**不留隔夜差**；推 GitHub 只允许 fast-forward。
3. GitHub 端如需紧急修补，也必须在 GitLab 先提交，再由同步推送带过去，禁止在 GitHub 单独产生提交。
4. 分支命名、提交信息风格不变；合并请求（MR/PR）以 GitLab 为准，GitHub 侧不合并。

## 3. 分叉处理原则

发现两端 `main` SHA 不一致时：

1. 先 `git fetch --all`，确认差异提交列表（`git log --oneline gitlab/main..origin/main` 与反向）。
2. 若 GitHub 多出提交：**停止**，不要 rebase/merge/force，先定位来源（是谁在 GitHub 直接改的），把内容在 GitLab 重新提交，再删掉 GitHub 上的旁支提交（需要维护者确认）。
3. 若只是 GitLab 领先：正常 FF 推送即可。
4. **禁止** `push --force` 到任一端的 `main`；禁止改写已发布的 tag。
5. 已经打出去的 tag 发现错误时：两端都保留原 tag，发新补丁版本，不移动 tag。

## 4. 发版规则

一次发版，两端都要做，产物必须字节一致。

**GitHub（发布承载，仍是 `latest.json` 的宿主）**

1. 在本地打 annotated tag（`v1.0.3` 或项目既有格式），推两端：`git push gitlab <tag>` + `git push origin <tag>`。
2. GitHub Actions `release.yml` 由 tag 触发，产出二进制、`SHA256SUMS`、`.minisig`、demo 等附件并发 Release。
3. 更新 `updates` tag 下的 `latest.json`（仍由 GitHub 承载）。
4. GitHub 历史 Release **一律不动、不删**。

**GitLab（只保留最新一个 Release）**

1. tag 全量保留（含历史），但**不补建历史 Release**。
2. 只对最新版本建 Release：Generic Package 上传二进制/`SHA256SUMS`/`.minisig`，
   Release 用 `direct_asset_path` 生成 `/-/releases/<版本>/downloads/<文件名>`。
3. 新版本发布后，GitLab 侧仍只保留这一个 Release；旧 Release 可删（tag 不删）。

**字节一致核对（必须做）**

```bash
# 1) 两端同名文件哈希
curl -sSL https://github.com/yunyu697/<repo>/releases/download/<tag>/<file>   -o a.bin
curl -sSL https://gitlab.com/yunyu697/<repo>/-/releases/<tag>/downloads/<file> -o b.bin
sha256sum a.bin b.bin        # 必须完全相同

# 2) 清单与签名
sha256sum -c SHA256SUMS
minisign -Vm <file> -p <公钥文件> -x <file>.minisig -q
```

任何一项不一致就视为发版失败，不允许只在一端“先发”。

## 5. CI 分工（两端并存，避免重复/漏项）

| 检查项 | GitHub | GitLab |
|---|---|---|
| 版本规则 / 版本文件校验 | `ci.yml` check job | `version-rules` |
| 构建 release | `release.yml`（仅 tag） | `build-linux`（每次 push 都构建，验证 `--locked` 可编译） |
| 单元/契约测试 | `ci.yml` | `test` |
| shellcheck（仅脚本类项目） | 无 | `shellcheck` |
| Pages | `gh-pages` 分支（legacy，保留只读） | `pages` job（唯一维护入口） |
| 发版与附件 | `release.yml`（唯一发版流水线） | 不自动发版，Release/附件由发版清单手工/脚本建 |

规则：**同一项检查至少在一端存在**；GitLab 的构建/测试是“镜像是否可构建”的守门，GitHub 的 release 才是发版。
新增检查项时先改 GitLab（主源 CI 先跑），确认稳定后再补到 GitHub `ci.yml`，避免两端同时改导致规则漂移。

## 6. Pages

| 端 | 地址 | 来源 | 一致性要求 |
|---|---|---|---|
| GitHub | `https://yunyu697.github.io/berthio/` | `gh-pages` 分支，冻结不再更新 | 内容与 GitLab 版本一致即可，不要求逐字节相同 |
| GitLab | `https://berthio-70ab2d.gitlab.io/`（`yunyu697.gitlab.io/berthio/` 会跳转） | `public/` 目录 + `pages` job | 唯一维护入口 |

- 修改 demo 只改 `public/index.html`，由 GitLab Pages 发布；GitHub Pages 不再更新，README 顶部的演示链接可保留指向 GitHub（历史链接不断）。
- 两端都必须是**匿名可打开**：私有项目 + `pages_access_level=private` 会 302 到登录页（berthio 已踩过）。

## 7. `latest.json` 与 `DEFAULT_FEED`

- 现状：`DEFAULT_FEED` 指向 GitHub `updates/latest.json`，**本次不切换**。
- 切换条件（全部满足才切）：GitLab 匿名可下载、`latest.json` 已在 GitLab 稳定地址可用、至少一个真实客户端完成“检查→下载→验签→安装”闭环。
- 切换时要改的位置与老用户影响：见 `docs/GITLAB-CUTOVER.md`。
- 回滚条件：切换后任意一项验证失败，把 `DEFAULT_FEED` 改回 GitHub 常量并重发补丁版本。
- 无论指向哪端，GitHub 的 `updates` Release 必须保留（老版本客户端只认它）。

## 8. “必须两边都做”的清单

| 操作 | GitHub | GitLab |
|---|---|---|
| 提交代码 | 只接收 FF 同步推送 | 主源，直接提交 |
| 新增/删除分支 | 同步推送 | 主源 |
| 打 tag | 推 tag（触发 release.yml） | 推 tag（保留历史，不建历史 Release） |
| 发版 Release | 新建（历史保留） | 只建最新一个，旧 Release 可删 |
| 上传附件 | Release assets | Generic Package + `direct_asset_path` |
| `SHA256SUMS` / `.minisig` | 随 Release 附件 | 同版本包 + Release 链接 |
| 更新 `latest.json` | `updates` tag 的 Release assets | 仅切换后才需要 |
| 改 demo | 不再更新（冻结） | `public/` + pages job |
| CI 规则变更 | 补到 `ci.yml` | 先改 |
| README 里的地址/链接 | 同步改 | 主源改 |

## 9. 凭据类型（不要把 token 写进任何文件）

| 用途 | 需要什么 |
|---|---|
| 推 GitLab | Personal/Project Access Token，scope `api` + `write_repository`（建项目、改可见性、传包、建 Release 都要 `api`） |
| 推 GitHub | `gh` 登录态或 PAT，scope `repo`（推 tag 且触发 Actions 时需要 `workflow`） |
| 上传 Release 资产 / 读私有 Release | GitHub token（私有仓库时） |
| minisign 签名 | 本地私钥文件，**不进仓库、不进 CI 变量上传**（GitHub 用 repository secret） |

Token 只在命令行环境变量中临时使用，报告里写 `glpat-…（已使用，未落盘）`。

## 10. 每次发版后的对账命令

```bash
git ls-remote gitlab refs/heads/main refs/tags/*
git ls-remote origin refs/heads/main refs/tags/*
# 两行 main SHA 必须相同；tag 数量与 SHA 必须一一对应
```
