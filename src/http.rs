use crate::error::{ApiError, ApiResult};
use crate::export::{self, Selection};
use crate::kernels;
use crate::model::TokenRecord;
use crate::rulesets;
use crate::state::{Config, PanelConfig, Store};
use crate::util;
use crate::wizard;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_BODY: usize = 2 * 1024 * 1024;

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Vec<u8>,
}

struct Response {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
    extra: Vec<(&'static str, String)>,
}

impl Response {
    fn json(status: u16, value: &serde_json::Value) -> Self {
        Self {
            status,
            content_type: "application/json; charset=utf-8",
            body: serde_json::to_vec_pretty(value).unwrap_or_default(),
            extra: Vec::new(),
        }
    }
    fn text(status: u16, content_type: &'static str, body: String) -> Self {
        Self {
            status,
            content_type,
            body: body.into_bytes(),
            extra: Vec::new(),
        }
    }
    fn html(body: &'static str) -> Self {
        Self {
            status: 200,
            content_type: "text/html; charset=utf-8",
            body: body.as_bytes().to_vec(),
            extra: Vec::new(),
        }
    }
    fn file(body: Vec<u8>, name: &str) -> Self {
        let mut r = Self {
            status: 200,
            content_type: "application/octet-stream",
            body,
            extra: Vec::new(),
        };
        r.extra.push(("Content-Disposition", format!("attachment; filename=\"{name}\"")));
        r
    }
    fn with(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.extra.push((k, v.into()));
        self
    }
}

fn percent_decode(s: &str) -> String {
    let mut out = Vec::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                if let (Some(a), Some(b)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                    out.push(a * 16 + b);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn parse_query(raw: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for part in raw.split('&') {
        if part.is_empty() {
            continue;
        }
        let mut kv = part.splitn(2, '=');
        let k = percent_decode(kv.next().unwrap_or(""));
        let v = percent_decode(kv.next().unwrap_or(""));
        map.insert(k, v);
    }
    map
}

fn read_request(stream: &mut TcpStream) -> Result<Request, String> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    loop {
        let n = stream.read(&mut tmp).map_err(|e| e.to_string())?;
        if n == 0 {
            return Err("连接提前关闭".into());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = buf[..pos].to_vec();
            let body_start = pos + 4;
            let mut content_length = 0usize;
            let mut chunked = false;
            for line in head.split(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(line).trim().to_string();
                let lower = line.to_ascii_lowercase();
                if lower.starts_with("content-length:") {
                    content_length = lower[15..].trim().parse().unwrap_or(0);
                }
                if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
                    chunked = true;
                }
            }
            if chunked {
                return Err("不支持 chunked 请求体".into());
            }
            if content_length > MAX_BODY {
                return Err("请求体过大".into());
            }
            while buf.len() < body_start + content_length {
                let n = stream.read(&mut tmp).map_err(|e| e.to_string())?;
                if n == 0 {
                    break;
                }
                buf.extend_from_slice(&tmp[..n]);
            }
            let head_text = String::from_utf8_lossy(&head).to_string();
            let mut lines = head_text.lines();
            let first = lines.next().unwrap_or("");
            let mut parts = first.split_whitespace();
            let method = parts.next().unwrap_or("GET").to_string();
            let target = parts.next().unwrap_or("/").to_string();
            let (path, query) = match target.split_once('?') {
                Some((p, q)) => (p.to_string(), parse_query(q)),
                None => (target, HashMap::new()),
            };
            let body = buf[body_start..(body_start + content_length).min(buf.len())].to_vec();
            return Ok(Request {
                method,
                path,
                query,
                body,
            });
        }
        if buf.len() > 64 * 1024 {
            return Err("请求头过大".into());
        }
    }
}

fn body_json(req: &Request) -> ApiResult<serde_json::Value> {
    if req.body.is_empty() {
        return Ok(serde_json::Value::Null);
    }
    serde_json::from_slice(&req.body).map_err(|e| {
        ApiError::bad_request(format!("JSON 解析失败: {e}")).with_field("body", "必须是合法 JSON")
    })
}

fn query_list(req: &Request, key: &str) -> Vec<String> {
    req.query
        .get(key)
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

fn parse_rules(req: &Request) -> Vec<(String, String)> {
    let Some(raw) = req.query.get("rules") else {
        return Vec::new();
    };
    raw.split(',')
        .filter_map(|part| {
            let (id, action) = part.split_once('@')?;
            Some((id.trim().to_string(), action.trim().to_string()))
        })
        .collect()
}

fn selection_from(req: &Request) -> Selection {
    let format = match req.query.get("format").map(|s| s.as_str()) {
        Some("singbox") | Some("sing-box") => "singbox",
        _ => "clash",
    };
    let scenario = match req.query.get("scenario").map(|s| s.as_str()) {
        Some("router") => "router",
        Some("side") => "side",
        _ => "desktop",
    };
    let bucket = req
        .query
        .get("bucket")
        .cloned()
        .unwrap_or_else(|| "112".to_string());
    Selection {
        format: format.to_string(),
        scenario: scenario.to_string(),
        bucket,
        inbound_ids: query_list(req, "inbound_ids"),
        client_ids: query_list(req, "client_ids"),
        rules: parse_rules(req),
    }
}

pub fn serve(listen: &str, store: Arc<Store>, demo: bool) {
    let listener = match TcpListener::bind(listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[berthio] 监听 {listen} 失败: {e}");
            std::process::exit(1);
        }
    };
    println!(
        "[berthio] {}{} 已启动: http://{}  (home={})",
        VERSION,
        if demo { " (demo)" } else { "" },
        listen,
        store.home.display()
    );
    for stream in listener.incoming() {
        match stream {
            Ok(mut s) => {
                let store = Arc::clone(&store);
                std::thread::spawn(move || {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
                    let resp = match read_request(&mut s) {
                        Ok(req) => route(&req, &store, demo),
                        Err(e) => Response::json(400, &json!({"error":{"code":"bad_request","message":e}})),
                    };
                    let _ = write_response(&mut s, resp);
                });
            }
            Err(e) => eprintln!("[berthio] accept 失败: {e}"),
        }
    }
}

fn write_response(stream: &mut TcpStream, resp: Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nCache-Control: no-store\r\n",
        resp.status,
        reason,
        resp.content_type,
        resp.body.len()
    );
    for (k, v) in resp.extra {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&resp.body)?;
    stream.flush()
}

fn route(req: &Request, store: &Store, demo: bool) -> Response {
    let path = req.path.as_str();
    let result: ApiResult<Response> = match (req.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => Ok(Response::html(include_str!("../web/index.html"))),
        ("GET", "/app.js") => Ok(Response::text(
            200,
            "application/javascript; charset=utf-8",
            include_str!("../web/app.js").to_string(),
        )),
        ("GET", "/favicon.ico") => Ok(Response::text(204, "image/x-icon", String::new())),
        ("GET", "/healthz") => Ok(Response::json(200, &json!({"status":"ok","version":VERSION}))),
        ("GET", "/api/panel") => Ok(Response::json(200, &panel_payload(store, demo))),
        ("PATCH", "/api/panel") => patch_panel(req, store),
        ("GET", "/api/kernels") => Ok(Response::json(200, &kernels::report(store))),
        ("GET", "/api/rulesets") => Ok(Response::json(200, &rulesets_payload())),
        ("GET", "/api/inbound/inbounds") => Ok(Response::json(200, &inbound_list(store))),
        ("POST", "/api/inbound/inbounds") => create_inbound(req, store),
        ("GET", "/api/inbound/wizard/defaults") => Ok(Response::json(200, &wizard::defaults(store))),
        ("GET", "/api/export/preview") => export_preview(req, store),
        ("GET", "/api/export/file") => export_file(req, store),
        ("POST", "/api/export/tokens") => create_token(req, store),
        ("GET", "/api/logs") => Ok(Response::json(200, &logs_payload(store))),
        ("GET", p) if p.starts_with("/sub/") => subscription(&req.query, &p[5..], store),
        ("GET", p) if p.starts_with("/api/inbound/inbounds/") => {
            let rest = &p["/api/inbound/inbounds/".len()..];
            if let Some(id) = rest.strip_suffix("/clients") {
                Ok(Response::json(200, &client_list(store, id)))
            } else {
                inbound_detail(store, rest)
            }
        }
        ("POST", p) if p.starts_with("/api/inbound/inbounds/") && p.ends_with("/clients") => {
            let id = &p["/api/inbound/inbounds/".len()..p.len() - "/clients".len()];
            create_client(req, store, id)
        }
        ("POST", p) if p.starts_with("/api/export/tokens/") && p.ends_with("/revoke") => {
            let id = &p["/api/export/tokens/".len()..p.len() - "/revoke".len()];
            revoke_token(store, id)
        }
        _ => Err(ApiError::not_found(format!("{} {}", req.method, req.path))),
    };
    match result {
        Ok(r) => r,
        Err(e) => Response::json(e.status, &e.to_json()),
    }
}

fn panel_payload(store: &Store, demo: bool) -> serde_json::Value {
    let state = store.snapshot();
    let clients: usize = state.inbounds.iter().map(|i| i.clients.len()).sum();
    json!({
        "version": VERSION,
        "version_source": "Cargo.toml（构建时以 CARGO_PKG_VERSION 注入，单一来源）",
        "listen": store.config.listen,
        "panel_address": state.panel.panel_address,
        "home": store.home.display().to_string(),
        "demo": demo,
        "demo_label": if demo { "演示数据" } else { "" },
        "created_at": state.panel.created_at,
        "counts": {"inbounds": state.inbounds.len(), "clients": clients},
        "auth": {"mode":"local-admin","note":"M1 只监听本机；远程访问鉴权在 M3"}
    })
}

fn patch_panel(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let address = body
        .get("panel_address")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_string();
    let cleaned = address
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_string();
    if cleaned.is_empty() {
        return Err(ApiError::bad_request("对外地址不能为空").with_field("panel_address", "填域名或公网 IP"));
    }
    if cleaned.contains(char::is_whitespace) || cleaned.contains('/') {
        return Err(ApiError::bad_request("对外地址只能是域名或 IP（可带端口）")
            .with_field("panel_address", "不要带路径或空格"));
    }
    let mut panel: PanelConfig = store.config.clone();
    panel.panel_address = cleaned.clone();
    let cfg = Config { home: store.home.clone() };
    cfg.save_panel(&panel)?;
    store.update(|state| {
        state.panel.panel_address = cleaned.clone();
        Ok(())
    })?;
    store.audit("panel.address.updated", "local-admin", json!({"panel_address": cleaned}));
    Ok(Response::json(200, &json!({"ok":true,"panel_address":cleaned})))
}

fn inbound_list(store: &Store) -> serde_json::Value {
    let state = store.snapshot();
    let items: Vec<serde_json::Value> = state
        .inbounds
        .iter()
        .map(|i| {
            let clients = i.clients.len();
            let enabled_clients = i.clients.iter().filter(|c| c.enabled).count();
            json!({
                "id": i.id,
                "tag": i.tag,
                "remark": i.remark,
                "protocol": i.protocol,
                "network": i.network,
                "security": i.security,
                "listen": i.listen,
                "port": i.port,
                "l4": i.l4,
                "server_kernel": i.server_kernel,
                "server_version": i.server_version,
                "enabled": i.enabled,
                "clients": clients,
                "enabled_clients": enabled_clients,
                "created_at": i.created_at,
                "updated_at": i.updated_at,
                "source": i.source,
                "status": if i.enabled { "待核验" } else { "已停用" },
                "verification": "未验证",
                "today_bytes": 0,
            })
        })
        .collect();
    let mut kernels = serde_json::Map::new();
    for i in &state.inbounds {
        let k = kernels.entry(i.server_kernel.clone()).or_insert_with(|| json!(0));
        if let Some(n) = k.as_i64() {
            *k = json!(n + 1);
        }
    }
    json!({
        "items": items,
        "total": state.inbounds.len(),
        "kernels": kernels,
        "selection": {
            "inbound_count": state.inbounds.len(),
            "client_count": state.inbounds.iter().map(|i| i.clients.len()).sum::<usize>(),
        },
        "note": "状态列「未验证」表示尚未执行内核配置自检，不等于不可用"
    })
}

fn inbound_detail(store: &Store, id: &str) -> ApiResult<Response> {
    let state = store.snapshot();
    let inbound = state
        .inbounds
        .iter()
        .find(|i| i.id == id)
        .ok_or_else(|| ApiError::not_found(format!("入站 {id} 不存在")))?;
    Ok(Response::json(200, &inbound.redacted()))
}

fn client_list(store: &Store, id: &str) -> serde_json::Value {
    let state = store.snapshot();
    match state.inbounds.iter().find(|i| i.id == id) {
        Some(i) => json!({
            "inbound_id": i.id,
            "items": i.clients.iter().map(|c| json!({
                "id": c.id,
                "email": c.email,
                "uuid": c.uuid,
                "flow": c.flow,
                "quota_bytes": c.quota_bytes,
                "used_bytes": c.used_bytes,
                "limit_ip": c.limit_ip,
                "expires_at": c.expires_at,
                "enabled": c.enabled,
            })).collect::<Vec<_>>()
        }),
        None => json!({"inbound_id": id, "items": []}),
    }
}

fn create_inbound(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    if body.is_null() {
        return Err(ApiError::bad_request("缺少 JSON 请求体"));
    }
    let payload: wizard::CreateInboundReq = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("字段解析失败: {e}")))?;
    let out = wizard::create_inbound(store, payload)?;
    Ok(Response::json(201, &out))
}

fn create_client(req: &Request, store: &Store, id: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let payload: wizard::CreateClientReq = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("字段解析失败: {e}")))?;
    let out = wizard::create_client(store, id, payload)?;
    Ok(Response::json(201, &out))
}

fn export_preview(req: &Request, store: &Store) -> ApiResult<Response> {
    let sel = selection_from(req);
    let payload = export::render_payload(store, sel)?;
    store.audit(
        "export.preview",
        "local-admin",
        json!({"render_id": payload["render_id"], "format": payload["format"], "entries": payload["entries"]}),
    );
    Ok(Response::json(200, &payload))
}

fn export_file(req: &Request, store: &Store) -> ApiResult<Response> {
    let sel = selection_from(req);
    let payload = export::render_payload(store, sel)?;
    if payload["validation"]["overall"] == "FAIL" {
        return Err(ApiError::new(422, "export_blocked", "校验 FAIL，拒绝导出").with_field(
            "validation",
            "先修正 L0/L1 中的失败项",
        ));
    }
    let content = payload["content"].as_str().unwrap_or("").to_string();
    let name = payload["file_name"].as_str().unwrap_or("berthio-config.txt");
    store.audit(
        "export.download",
        "local-admin",
        json!({"render_id": payload["render_id"], "file_name": name, "bytes": content.len()}),
    );
    Ok(Response::file(content.into_bytes(), name).with(
        "X-Berthio-Render-Id",
        payload["render_id"].as_str().unwrap_or(""),
    ))
}

fn create_token(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let client_id = body.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
    let target = body
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("clash");
    if client_id.is_empty() {
        return Err(ApiError::bad_request("缺少 client_id").with_field("client_id", "从节点列表选择"));
    }
    let state = store.snapshot();
    let mut found = None;
    for i in &state.inbounds {
        if let Some(c) = i.clients.iter().find(|c| c.id == client_id) {
            found = Some((i.id.clone(), c.email.clone()));
        }
    }
    let Some((inbound_id, email)) = found else {
        return Err(ApiError::not_found("客户端不存在"));
    };
    let token = format!("bto_{}", util::random_hex(24));
    let token_hash = util::sha256_hex(token.as_bytes());
    let id = util::new_id("tok");
    let record = TokenRecord {
        id: id.clone(),
        client_id: client_id.to_string(),
        token_hash,
        target: target.to_string(),
        created_at: util::now_iso(),
        revoked_at: None,
        last_access: None,
    };
    store.update(|st| {
        st.tokens.push(record.clone());
        Ok(())
    })?;
    store.audit(
        "export.token.created",
        "local-admin",
        json!({"token_id": id, "client_id": client_id, "target": target}),
    );
    Ok(Response::json(
        201,
        &json!({
            "id": id,
            "client_id": client_id,
            "email": email,
            "inbound_id": inbound_id,
            "target": target,
            "url": format!("/sub/{token}?target={target}"),
            "token": token,
            "note": "token 只在此次响应返回；服务端只存 SHA-256",
        }),
    ))
}

fn revoke_token(store: &Store, id: &str) -> ApiResult<Response> {
    store.update(|st| {
        let t = st
            .tokens
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ApiError::not_found("token 不存在"))?;
        t.revoked_at = Some(util::now_iso());
        Ok(())
    })?;
    store.audit("export.token.revoked", "local-admin", json!({"token_id": id}));
    Ok(Response::json(200, &json!({"ok":true,"id":id})))
}

fn subscription(query: &HashMap<String, String>, token: &str, store: &Store) -> ApiResult<Response> {
    let hash = util::sha256_hex(token.as_bytes());
    let state = store.snapshot();
    let rec = state
        .tokens
        .iter()
        .find(|t| t.token_hash == hash)
        .ok_or_else(|| ApiError::not_found("订阅不存在"))?;
    if rec.revoked_at.is_some() {
        return Err(ApiError::new(403, "revoked", "订阅已被吊销"));
    }
    let target = query.get("target").cloned().unwrap_or_else(|| rec.target.clone());
    let format = if target.contains("sing") { "singbox" } else { "clash" };
    let mut inbound_ids = Vec::new();
    for i in &state.inbounds {
        if i.clients.iter().any(|c| c.id == rec.client_id) {
            inbound_ids.push(i.id.clone());
        }
    }
    if inbound_ids.is_empty() {
        return Err(ApiError::not_found("订阅绑定的客户端已不存在"));
    }
    let sel = Selection {
        format: format.to_string(),
        scenario: "desktop".to_string(),
        bucket: "112".to_string(),
        inbound_ids,
        client_ids: vec![rec.client_id.clone()],
        rules: Vec::new(),
    };
    let rendered = export::render(store, sel)?;
    let _ = store.update(|st| {
        if let Some(t) = st.tokens.iter_mut().find(|t| t.id == rec.id) {
            t.last_access = Some(util::now_iso());
        }
        Ok(())
    });
    store.audit("sub.access", "subscription", json!({"token_id": rec.id, "target": target}));
    let ct = if format == "singbox" {
        "application/json; charset=utf-8"
    } else {
        "text/yaml; charset=utf-8"
    };
    Ok(Response::text(200, ct, rendered.content).with("X-Berthio-Render-Id", rendered.render_id))
}

fn rulesets_payload() -> serde_json::Value {
    json!({
        "catalog": rulesets::catalog(),
        "groups": ["直连与私网","广告与恶意","流媒体","AI 与开发","社交","游戏","下载与测速"],
        "presets": rulesets::preset_defs(),
        "actions": [
            {"id":"proxy","label":"代理"},
            {"id":"direct","label":"直连"},
            {"id":"reject","label":"拒绝"},
            {"id":"off","label":"不启用"}
        ],
        "sources": {
            "official": {
                "id":"official",
                "label":"官方源（GitHub MetaCubeX/meta-rules-dat）",
                "base": "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat",
                "enabled": true,
                "note": "校验基准仍以官方 SHA-256 为准"
            },
            "mirrors": [],
            "mirror_note": "M2 接入加速镜像：内置列表 + 自动回退 + 手动指定 + SHA-256 拦截"
        }
    })
}

fn logs_payload(store: &Store) -> serde_json::Value {
    let stamp = util::now_iso();
    let month = &stamp[0..7];
    let path = store.home_path(&format!("logs/audit/{month}.jsonl"));
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    let items: Vec<serde_json::Value> = raw
        .lines()
        .rev()
        .take(80)
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .collect();
    json!({"items": items, "month": month, "note": "审计日志只追加，不记录明文 token 与私钥"})
}
