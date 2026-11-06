//! 能力清单与契约测试。
//!
//! 权威说明见 `docs/CAPABILITY-MATRIX.md`。这里的声明表是同一份清单的机器可读版本：
//! - `Required`：必须同时存在后端路由与前端入口；
//! - `BackendOnly`：对外/脚本接口，界面无需入口；
//! - `Intentional`：有意不做，必须写理由。
//!
//! 测试直接扫描 `src/http.rs` 与 `web/app.js` 的源码文本。删掉一条真实路由或一处真实调用，
//! 对应的探针就会消失，测试立即失败。

#[derive(Debug, PartialEq, Eq)]
pub enum Status {
    Required,
    BackendOnly,
    Intentional,
}

pub struct Capability {
    pub entity: &'static str,
    pub action: &'static str,
    pub status: Status,
    /// 必须出现在 `src/http.rs` 中的路由分支文本；`Intentional` 为 `None`。
    pub route: Option<&'static str>,
    /// 必须出现在 `web/app.js` 中的调用文本。
    pub frontend: &'static [&'static str],
    /// `Intentional` 的理由；其它状态可为空。
    pub reason: &'static str,
}

pub const CAPABILITIES: &[Capability] = &[
    // ---------------- 入站 ----------------
    Capability {
        entity: "inbound",
        action: "create",
        status: Status::Required,
        route: Some(r#"("POST", "/api/inbound/inbounds")"#),
        frontend: &["api('/api/inbound/inbounds', { method: 'POST'"],
        reason: "",
    },
    Capability {
        entity: "inbound",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/inbound/inbounds")"#),
        frontend: &["api('/api/inbound/inbounds')"],
        reason: "",
    },
    Capability {
        entity: "inbound",
        action: "detail",
        status: Status::BackendOnly,
        route: Some(r#"("GET", p) if p.starts_with("/api/inbound/inbounds/")"#),
        frontend: &[],
        reason: "列表与导出页已覆盖所需字段，不单独做详情页",
    },
    Capability {
        entity: "inbound",
        action: "update",
        status: Status::Required,
        route: Some(r#"("PATCH", p) if p.starts_with("/api/inbound/inbounds/")"#),
        frontend: &["encodeURIComponent(id), { method: 'PATCH'"],
        reason: "",
    },
    Capability {
        entity: "inbound",
        action: "delete",
        status: Status::Required,
        route: Some(r#"("DELETE", p) if p.starts_with("/api/inbound/inbounds/")"#),
        frontend: &["'?confirm=' + encodeURIComponent(id),"],
        reason: "",
    },
    // ---------------- 入站客户端 ----------------
    Capability {
        entity: "inbound.client",
        action: "create",
        status: Status::Required,
        route: Some(r#"if p.starts_with("/api/inbound/inbounds/") && p.ends_with("/clients")"#),
        frontend: &["'/clients', { method: 'POST'"],
        reason: "",
    },
    Capability {
        entity: "inbound.client",
        action: "list",
        status: Status::Required,
        route: Some(r#"strip_suffix("/clients")"#),
        frontend: &["+ '/clients').then"],
        reason: "",
    },
    Capability {
        entity: "inbound.client",
        action: "update",
        status: Status::Required,
        route: Some(r#"("PATCH", p) if p.starts_with("/api/inbound/inbounds/") && p.contains("/clients/")"#),
        frontend: &["'/clients/' + encodeURIComponent(clientId)"],
        reason: "",
    },
    Capability {
        entity: "inbound.client",
        action: "disable",
        status: Status::Required,
        route: Some(r#"("PATCH", p) if p.starts_with("/api/inbound/inbounds/") && p.contains("/clients/")"#),
        frontend: &["{ enabled: false }"],
        reason: "",
    },
    Capability {
        entity: "inbound.client",
        action: "reset_traffic",
        status: Status::Required,
        route: Some(r#"p.contains("/clients/") && p.ends_with("/reset")"#),
        frontend: &["+ '/reset'"],
        reason: "",
    },
    Capability {
        entity: "inbound.client",
        action: "delete",
        status: Status::Required,
        route: Some(r#"("DELETE", p) if p.starts_with("/api/inbound/inbounds/") && p.contains("/clients/")"#),
        frontend: &["'?confirm=' + encodeURIComponent(c.id)"],
        reason: "",
    },
    // ---------------- 规则集 ----------------
    Capability {
        entity: "ruleset",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/rulesets")"#),
        frontend: &["api('/api/rulesets')"],
        reason: "",
    },
    Capability {
        entity: "ruleset",
        action: "fetch",
        status: Status::Required,
        route: Some(r#"("POST", "/api/rulesets/fetch")"#),
        frontend: &["api('/api/rulesets/fetch'"],
        reason: "",
    },
    Capability {
        entity: "ruleset",
        action: "crud",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "规则集是内置静态目录（rulesets.rs），规则来自来源，不提供自建/编辑/删除实体",
    },
    // ---------------- 规则集来源 ----------------
    Capability {
        entity: "ruleset.source",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/rulesets/sources")"#),
        frontend: &["api('/api/rulesets/sources')"],
        reason: "",
    },
    Capability {
        entity: "ruleset.source",
        action: "create",
        status: Status::Required,
        route: Some(r#"("POST", "/api/rulesets/sources")"#),
        frontend: &["api('/api/rulesets/sources', { method: 'POST'"],
        reason: "",
    },
    Capability {
        entity: "ruleset.source",
        action: "update",
        status: Status::Required,
        route: Some(r#"("PATCH", p) if p.starts_with("/api/rulesets/sources/")"#),
        frontend: &["api('/api/rulesets/sources/' + encodeURIComponent("],
        reason: "",
    },
    Capability {
        entity: "ruleset.source",
        action: "delete",
        status: Status::Required,
        route: Some(r#"("DELETE", p) if p.starts_with("/api/rulesets/sources/")"#),
        frontend: &["encodeURIComponent(t.dataset.del), { method: 'DELETE'"],
        reason: "",
    },
    Capability {
        entity: "ruleset.source",
        action: "test",
        status: Status::Required,
        route: Some(r#"("POST", "/api/rulesets/sources/test")"#),
        frontend: &["api('/api/rulesets/sources/test'"],
        reason: "",
    },
    Capability {
        entity: "ruleset.source",
        action: "mode",
        status: Status::Required,
        route: Some(r#"("PUT", "/api/rulesets/sources/mode")"#),
        frontend: &["api('/api/rulesets/sources/mode', { method: 'PUT'"],
        reason: "",
    },
    // ---------------- 内核 ----------------
    Capability {
        entity: "kernel",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/kernels")"#),
        frontend: &["api('/api/kernels')"],
        reason: "",
    },
    Capability {
        entity: "kernel",
        action: "action",
        status: Status::Required,
        route: Some(r#"("POST", p) if p.starts_with("/api/kernels/")"#),
        frontend: &["api('/api/kernels/' + k + '/' + act, { method: 'POST' }"],
        reason: "",
    },
    Capability {
        entity: "kernel",
        action: "log",
        status: Status::Required,
        route: Some(r#"p.starts_with("/api/kernels/") && p.ends_with("/log")"#),
        frontend: &["api('/api/kernels/' + k + '/log?tail=120')"],
        reason: "",
    },
    Capability {
        entity: "kernel",
        action: "install",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "面板不下载内核二进制，需用户手动放入 kernels/",
    },
    Capability {
        entity: "kernel",
        action: "uninstall",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "卸载需手动删除内核目录；不提供删文件入口，避免越界删除",
    },
    Capability {
        entity: "kernel",
        action: "switch_version",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "版本切换靠替换目录或环境变量，不在面板内切换",
    },
    // ---------------- 证书 ----------------
    Capability {
        entity: "tls",
        action: "status",
        status: Status::BackendOnly,
        route: Some(r#"("GET", "/api/tls")"#),
        frontend: &[],
        reason: "证书状态随 GET /api/panel 一起返回",
    },
    Capability {
        entity: "tls",
        action: "generate",
        status: Status::Required,
        route: Some(r#"("POST", "/api/tls/generate")"#),
        frontend: &["api('/api/tls/generate'"],
        reason: "",
    },
    Capability {
        entity: "tls",
        action: "delete",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "本版不做证书删除；计划在 1.0.3-alpha3 补齐，删除前走 src/safety.rs",
    },
    // ---------------- 订阅令牌 ----------------
    Capability {
        entity: "export.token",
        action: "create",
        status: Status::Required,
        route: Some(r#"("POST", "/api/export/tokens")"#),
        frontend: &["api('/api/export/tokens', { method: 'POST'"],
        reason: "",
    },
    Capability {
        entity: "export.token",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/export/tokens")"#),
        frontend: &["api('/api/export/tokens').then"],
        reason: "",
    },
    Capability {
        entity: "export.token",
        action: "rotate",
        status: Status::Required,
        route: Some(r#"p.starts_with("/api/export/tokens/") && p.ends_with("/rotate")"#),
        frontend: &["'/rotate'"],
        reason: "",
    },
    Capability {
        entity: "export.token",
        action: "revoke",
        status: Status::Required,
        route: Some(r#"p.starts_with("/api/export/tokens/") && p.ends_with("/revoke")"#),
        frontend: &["'/revoke'"],
        reason: "",
    },
    Capability {
        entity: "export.token",
        action: "delete",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "吊销为终态，保留审计痕迹；不提供物理删除记录",
    },
    // ---------------- 面板用户 ----------------
    Capability {
        entity: "panel.user",
        action: "status",
        status: Status::Required,
        route: Some(r#"("GET", "/api/auth/status")"#),
        frontend: &["api('/api/auth/status')"],
        reason: "",
    },
    Capability {
        entity: "panel.user",
        action: "setup",
        status: Status::Required,
        route: Some(r#"("POST", "/api/auth/setup")"#),
        frontend: &["/api/auth/setup'"],
        reason: "",
    },
    Capability {
        entity: "panel.user",
        action: "login",
        status: Status::Required,
        route: Some(r#"("POST", "/api/auth/login")"#),
        frontend: &["/api/auth/login'"],
        reason: "",
    },
    Capability {
        entity: "panel.user",
        action: "logout",
        status: Status::BackendOnly,
        route: Some(r#"("POST", "/api/auth/logout")"#),
        frontend: &[],
        reason: "会话由 Cookie 管理；当前无退出按钮，需清除 Cookie 或等会话过期",
    },
    Capability {
        entity: "panel.user",
        action: "password",
        status: Status::Required,
        route: Some(r#"("POST", "/api/auth/password")"#),
        frontend: &["api('/api/auth/password'"],
        reason: "",
    },
    Capability {
        entity: "panel.user",
        action: "create_second_user",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "单用户设计：一个实例只维护一份口令记录",
    },
    Capability {
        entity: "panel.user",
        action: "delete_user",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "同上；改密即等同于重置凭据",
    },
    // ---------------- 面板设置 ----------------
    Capability {
        entity: "panel",
        action: "get",
        status: Status::Required,
        route: Some(r#"("GET", "/api/panel")"#),
        frontend: &["api('/api/panel')"],
        reason: "",
    },
    Capability {
        entity: "panel",
        action: "update",
        status: Status::Required,
        route: Some(r#"("PATCH", "/api/panel")"#),
        frontend: &["api('/api/panel', { method: 'PATCH'"],
        reason: "",
    },
    // ---------------- 审计 ----------------
    Capability {
        entity: "audit",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/audit")"#),
        frontend: &["api('/api/audit?' + auditQuery()"],
        reason: "",
    },
    Capability {
        entity: "audit",
        action: "csv",
        status: Status::Required,
        route: Some(r#"("GET", "/api/audit.csv")"#),
        frontend: &["/api/audit.csv?"],
        reason: "",
    },
    Capability {
        entity: "audit",
        action: "delete",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "审计只追加，不提供删除/清空接口或入口",
    },
    // ---------------- 导出 / 导入 ----------------
    Capability {
        entity: "export",
        action: "preview",
        status: Status::Required,
        route: Some(r#"("GET", "/api/export/preview")"#),
        frontend: &["'/api/export/preview?format='"],
        reason: "",
    },
    Capability {
        entity: "export",
        action: "file",
        status: Status::BackendOnly,
        route: Some(r#"("GET", "/api/export/file")"#),
        frontend: &[],
        reason: "界面用 preview + Blob 下载，file 接口供脚本/外部调用",
    },
    Capability {
        entity: "migrate",
        action: "preview",
        status: Status::Required,
        route: Some(r#"("POST", "/api/migrate/preview")"#),
        frontend: &["api('/api/migrate/preview'"],
        reason: "",
    },
    Capability {
        entity: "migrate",
        action: "apply",
        status: Status::Required,
        route: Some(r#"("POST", "/api/migrate/apply")"#),
        frontend: &["api('/api/migrate/apply'"],
        reason: "",
    },
    Capability {
        entity: "backup",
        action: "create_restore",
        status: Status::Intentional,
        route: None,
        frontend: &[],
        reason: "本版不做面板数据备份/恢复；计划在 1.0.3-alpha3 补齐，只有升级前自动备份",
    },
    // ---------------- 更新 ----------------
    Capability {
        entity: "update",
        action: "status",
        status: Status::Required,
        route: Some(r#"("GET", "/api/update/status")"#),
        frontend: &["api('/api/update/status')"],
        reason: "",
    },
    Capability {
        entity: "update",
        action: "check",
        status: Status::Required,
        route: Some(r#"("POST", "/api/update/check")"#),
        frontend: &["api('/api/update/check'"],
        reason: "",
    },
    Capability {
        entity: "update",
        action: "download",
        status: Status::Required,
        route: Some(r#"("POST", "/api/update/download")"#),
        frontend: &["api('/api/update/download'"],
        reason: "",
    },
    Capability {
        entity: "update",
        action: "install",
        status: Status::Required,
        route: Some(r#"("POST", "/api/update/install")"#),
        frontend: &["api('/api/update/install'"],
        reason: "",
    },
    // ---------------- 其它 ----------------
    Capability {
        entity: "health",
        action: "check",
        status: Status::BackendOnly,
        route: Some(r#"("GET", "/healthz")"#),
        frontend: &[],
        reason: "供进程管理与脚本探活",
    },
    Capability {
        entity: "subscription",
        action: "serve",
        status: Status::BackendOnly,
        route: Some(r#"("GET", p) if p.starts_with("/sub/")"#),
        frontend: &[],
        reason: "订阅内容由客户端软件读取，面板不直接打开",
    },
    Capability {
        entity: "logs",
        action: "list",
        status: Status::Required,
        route: Some(r#"("GET", "/api/logs")"#),
        frontend: &["api('/api/logs')"],
        reason: "",
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    const HTTP_SRC: &str = include_str!("http.rs");
    const APP_JS: &str = include_str!("../web/app.js");

    #[test]
    fn matrix_entries_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for c in CAPABILITIES {
            let key = format!("{}.{}", c.entity, c.action);
            assert!(seen.insert(key.clone()), "能力清单重复声明：{key}");
        }
    }

    #[test]
    fn intentional_absent_are_documented() {
        for c in CAPABILITIES {
            if c.status == Status::Intentional {
                assert!(
                    !c.reason.trim().is_empty(),
                    "有意不做的能力必须写理由：{}.{}",
                    c.entity,
                    c.action
                );
                assert!(
                    c.route.is_none(),
                    "有意不做的能力不应声明后端路由：{}.{}",
                    c.entity,
                    c.action
                );
            }
        }
    }

    #[test]
    fn backend_routes_match_matrix() {
        let mut missing = Vec::new();
        for c in CAPABILITIES {
            if let Some(probe) = c.route {
                if !HTTP_SRC.contains(probe) {
                    missing.push(format!("{}.{} 缺少后端路由探针：{}", c.entity, c.action, probe));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "路由表与能力清单不一致（{} 项）：\n{}",
            missing.len(),
            missing.join("\n")
        );
    }

    #[test]
    fn frontend_entries_match_matrix() {
        let mut missing = Vec::new();
        for c in CAPABILITIES {
            if c.status == Status::Required {
                assert!(
                    !c.frontend.is_empty(),
                    "{}.{} 声明为必需，但没有前端探针",
                    c.entity,
                    c.action
                );
            }
            for probe in c.frontend {
                if !APP_JS.contains(probe) {
                    missing.push(format!("{}.{} 缺少前端入口探针：{}", c.entity, c.action, probe));
                }
            }
        }
        assert!(
            missing.is_empty(),
            "界面入口与能力清单不一致（{} 项）：\n{}",
            missing.len(),
            missing.join("\n")
        );
    }
}
