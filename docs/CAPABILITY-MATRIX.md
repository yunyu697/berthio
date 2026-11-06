# 能力清单（Capability Matrix）

本文件是 Berthio 实体能力的权威声明，与 `src/capability.rs` 中的结构化声明表同源。任何新增/删除路由、界面入口，都必须先改这里，再由契约测试校验。

规则：

- `必需`：必须同时存在后端路由与前端入口；缺一即构建失败。
- `仅后端`：对外/脚本接口，界面无需入口（已注明原因）。
- `有意不做`：明确不做，必须写理由，不允许用「遗漏」表达「不做」。

## 入站（inbound）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 增 | 必需 | `POST /api/inbound/inbounds` | 新建向导 |
| 查 | 必需 | `GET /api/inbound/inbounds` | 列表页 |
| 查详情 | 仅后端 | `GET /api/inbound/inbounds/{id}` | 列表字段已覆盖，无独立详情页 |
| 改 | 必需 | `PATCH /api/inbound/inbounds/{id}` | 行内「编辑」（备注/监听/端口/启用） |
| 删 | 必需 | `DELETE /api/inbound/inbounds/{id}?confirm={id}` | 行内「删除」+ 弹窗二次确认 |

## 入站客户端（inbound.client）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 增 | 必需 | `POST /api/inbound/inbounds/{id}/clients` | 编辑入站弹窗内「添加客户端」 |
| 查 | 必需 | `GET /api/inbound/inbounds/{id}/clients` | 编辑入站弹窗内客户端列表 |
| 改 | 必需 | `PATCH /api/inbound/inbounds/{id}/clients/{cid}` | 客户端「编辑」（email/配额/到期/flow） |
| 禁用 / 启用 | 必需 | `PATCH .../clients/{cid}`（`enabled=false/true`） | 「停用」（二次确认）/「启用」 |
| 重置流量 | 必需 | `POST .../clients/{cid}/reset` | 「重置流量」+ 弹窗二次确认 |
| 删 | 必需 | `DELETE .../clients/{cid}?confirm={cid}` | 「删除」+ 弹窗二次确认 |

删除与停用只改状态、不触碰文件系统，均写审计。

## 规则集（ruleset）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查 | 必需 | `GET /api/rulesets` | 规则页 |
| 拉取/更新文件 | 必需 | `POST /api/rulesets/fetch` | 规则页「拉取」 |
| 增/改/删实体 | 有意不做 | — | 内置静态目录（`rulesets.rs`），规则来自来源，不提供自建实体 |

## 规则集来源（ruleset.source）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查 | 必需 | `GET /api/rulesets/sources` | 规则页来源列表 |
| 增 | 必需 | `POST /api/rulesets/sources` | 「添加来源」 |
| 改 | 必需 | `PATCH /api/rulesets/sources/{id}` | 启用开关、优先级、URL |
| 删 | 必需 | `DELETE /api/rulesets/sources/{id}` | 「删除」（官方源禁删） |
| 测速/测试 | 必需 | `POST /api/rulesets/sources/test` | 「测试」 |
| 模式 | 必需 | `PUT /api/rulesets/sources/mode` | 模式下拉 |

## 内核（kernel）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查 | 必需 | `GET /api/kernels` | 内核卡片 |
| 日志 | 必需 | `GET /api/kernels/{k}/log` | 「查看日志」 |
| 启动/停止/重启/校验 | 必需 | `POST /api/kernels/{k}/{action}` | 卡片按钮 |
| 安装 | 有意不做 | — | 面板不下载内核二进制，需手动放入 `kernels/` |
| 卸载 | 有意不做 | — | 需手动删除内核目录；不提供删文件入口 |
| 切换版本 | 有意不做 | — | 以目录/环境变量替换，不在面板内切换 |

## 证书（tls）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查状态 | 仅后端 | `GET /api/tls` | 证书信息随 `GET /api/panel` 返回 |
| 生成 / 重新签发 | 必需 | `POST /api/tls/generate` | 设置页「一键自签」（重签=再点一次） |
| 启用/停用 | 必需 | `PATCH /api/panel`（`tls_enabled`） | 设置页开关 |
| 删除证书 | 有意不做（本版） | — | 计划在 1.0.3-alpha3 补齐，删除前走 `src/safety.rs` |

## 订阅令牌（export.token）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 签发 | 必需 | `POST /api/export/tokens` | 导出页「生成订阅链接」 |
| 查列表 | 必需 | `GET /api/export/tokens` | 导出页令牌列表（只回显元信息，不含明文/hash） |
| 轮换 | 必需 | `POST /api/export/tokens/{id}/rotate` | 「轮换」，新链接仅显示一次 |
| 吊销（失效） | 必需 | `POST /api/export/tokens/{id}/revoke` | 「吊销」+ 确认 |
| 删除记录 | 有意不做 | — | 吊销为终态，保留审计痕迹 |

## 面板用户（panel.user）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查状态 | 必需 | `GET /api/auth/status` | 登录页/面板 |
| 初始化 | 必需 | `POST /api/auth/setup` | 首次设置 |
| 登录 | 必需 | `POST /api/auth/login` | 登录页 |
| 登出 | 必需 | `POST /api/auth/logout` | — |
| 改密 | 必需 | `POST /api/auth/password` | 设置页「修改口令」 |
| 新增第二个用户 | 有意不做 | — | 单用户设计：一个实例只维护一份口令记录 |
| 删除用户 | 有意不做 | — | 同上；改密即等同于重置凭据 |

## 面板设置（panel）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查 | 必需 | `GET /api/panel` | 设置页 |
| 改 | 必需 | `PATCH /api/panel` | 设置页各表单 |

## 审计日志（audit）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查 | 必需 | `GET /api/audit` | 日志页筛选 |
| 导出 CSV | 必需 | `GET /api/audit.csv` | 日志页「导出 CSV」 |
| 删除 / 清空 | 有意不做 | — | 只追加，不提供删除/清空接口或入口 |

## 备份 / 导出（export / migrate）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 导出预览 | 必需 | `GET /api/export/preview` | 导出页 |
| 导出文件 | 仅后端 | `GET /api/export/file` | 界面改用 preview + Blob 下载，接口供脚本 |
| 3x-ui 导入预览 | 必需 | `POST /api/migrate/preview` | 导入页 |
| 3x-ui 导入应用 | 必需 | `POST /api/migrate/apply` | 导入页 |
| 面板数据备份 / 恢复 | 有意不做（本版） | — | 计划在 1.0.3-alpha3 补齐；本版只有 `state/backups/` 的升级前自动备份 |

## 更新（update）

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 查状态 | 必需 | `GET /api/update/status` | 设置页 |
| 检查 | 必需 | `POST /api/update/check` | 「检查更新」 |
| 下载 | 必需 | `POST /api/update/download` | 「仅下载」/「更新」 |
| 安装 | 必需 | `POST /api/update/install` | 「更新」 |

## 其它

| 动作 | 状态 | 后端 | 前端 |
|---|---|---|---|
| 健康检查 | 仅后端 | `GET /healthz` | 进程/脚本探活 |
| 订阅分发 | 仅后端 | `GET /sub/{token}` | 交给客户端软件读取 |
| 运行日志 | 必需 | `GET /api/logs` | 日志页 |
