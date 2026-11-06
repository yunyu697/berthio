use crate::auth::Auth;
use crate::error::{ApiError, ApiResult};
use crate::export::{self, Selection};
use crate::kernels;
use crate::model::TokenRecord;
use crate::rulesets;
use crate::sources;
use crate::state::{self, Config, PanelConfig, RemoteAccess, Store};
use crate::supervisor::{self, Supervisor};
use crate::update::Manager;
use crate::util;
use crate::wizard;
use serde_json::json;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_BODY: usize = 8 * 1024 * 1024;
const SESSION_COOKIE: &str = "berthio_session";
const REMOTE_CONFIRM_PHRASE: &str = "ENABLE-REMOTE";
/// 进程当前是否真的以 TLS 运行，以及生效证书的信息（启动时写入）。
static ACTIVE_TLS: std::sync::OnceLock<crate::tls::CertInfo> = std::sync::OnceLock::new();

struct Request {
    method: String,
    path: String,
    query: HashMap<String, String>,
    body: Vec<u8>,
    headers: HashMap<String, String>,
    peer_ip: String,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(|s| s.as_str())
    }

    fn cookie(&self, name: &str) -> Option<String> {
        let raw = self.header("cookie")?;
        for part in raw.split(';') {
            if let Some((k, v)) = part.split_once('=') {
                if k.trim() == name {
                    return Some(v.trim().to_string());
                }
            }
        }
        None
    }
}

/// 客户端网络信息：默认只信 peer；只有 peer 属于 trusted_proxies（CIDR 或精确 IP）时才解析转发头。
struct NetInfo {
    client_ip: String,
    https: bool,
    behind_proxy: bool,
}

/// 判断 IP 是否在 CIDR 内；也接受精确 IP 条目。IPv4 与 IPv6 分开算，跨族一律不匹配。
fn ip_in_cidr(ip: &str, entry: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr;
    let entry = entry.trim();
    if entry.is_empty() {
        return false;
    }
    let Ok(ip_addr) = IpAddr::from_str(ip.trim()) else { return false };
    let (net_part, prefix) = match entry.split_once('/') {
        Some((n, p)) => match p.parse::<u32>() {
            Ok(p) => (n, Some(p)),
            Err(_) => return false,
        },
        None => (entry, None),
    };
    let Ok(net_addr) = IpAddr::from_str(net_part) else { return false };
    match (ip_addr, net_addr) {
        (IpAddr::V4(a), IpAddr::V4(b)) => {
            let bits = prefix.unwrap_or(32).min(32);
            if bits == 0 {
                return true;
            }
            let mask = u32::MAX << (32 - bits);
            (u32::from(a) & mask) == (u32::from(b) & mask)
        }
        (IpAddr::V6(a), IpAddr::V6(b)) => {
            let bits = prefix.unwrap_or(128).min(128);
            if bits == 0 {
                return true;
            }
            let mask = u128::MAX << (128 - bits);
            (u128::from(a) & mask) == (u128::from(b) & mask)
        }
        _ => false,
    }
}

fn trusted_proxy(cfg: &PanelConfig, ip: &str) -> bool {
    cfg.remote_access
        .trusted_proxies
        .iter()
        .any(|entry| ip_in_cidr(ip, entry))
}

pub fn is_loopback_ip(ip: &str) -> bool {
    use std::net::IpAddr;
    use std::str::FromStr;
    match IpAddr::from_str(ip.trim()) {
        Ok(IpAddr::V4(a)) => a.is_loopback(),
        Ok(IpAddr::V6(a)) => a.is_loopback(),
        Err(_) => false,
    }
}

fn net_info(req: &Request, cfg: &PanelConfig, conn_secure: bool) -> NetInfo {
    let trusted_env = std::env::var("BERTHIO_TRUST_PROXY").ok().as_deref() == Some("1");
    let trusted = trusted_env || trusted_proxy(cfg, &req.peer_ip);
    if !trusted {
        return NetInfo {
            client_ip: req.peer_ip.clone(),
            https: conn_secure,
            behind_proxy: false,
        };
    }
    let mut client_ip = req.peer_ip.clone();
    if let Some(xff) = req.header("x-forwarded-for") {
        let hops: Vec<&str> = xff.split(',').map(|s| s.trim()).filter(|s| !s.is_empty()).collect();
        for hop in hops.iter().rev() {
            if trusted_proxy(cfg, hop) {
                continue;
            }
            client_ip = hop.to_string();
            break;
        }
    }
    let https = conn_secure
        || req
            .header("x-forwarded-proto")
            .map(|v| v.eq_ignore_ascii_case("https"))
            .unwrap_or(false);
    NetInfo {
        client_ip,
        https,
        behind_proxy: true,
    }
}

fn session_cookie(token: &str, max_age: u64, secure: bool) -> String {
    format!(
        "{SESSION_COOKIE}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if secure { "; Secure" } else { "" }
    )
}

fn clear_cookie(secure: bool) -> String {
    format!("{SESSION_COOKIE}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}", if secure { "; Secure" } else { "" })
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

fn read_request<R: Read>(stream: &mut R, peer_ip: &str) -> Result<Request, String> {
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
            let mut headers: HashMap<String, String> = HashMap::new();
            for line in head.split(|b| *b == b'\n') {
                let line = String::from_utf8_lossy(line).trim().to_string();
                if line.is_empty() {
                    continue;
                }
                let Some((k, v)) = line.split_once(':') else {
                    continue;
                };
                let key = k.trim().to_ascii_lowercase();
                let val = v.trim().to_string();
                if key == "content-length" {
                    content_length = val.parse().unwrap_or(0);
                }
                if key == "transfer-encoding" && val.to_ascii_lowercase().contains("chunked") {
                    chunked = true;
                }
                if let Some(existing) = headers.get_mut(&key) {
                    existing.push_str(if key == "cookie" { "; " } else { ", " });
                    existing.push_str(&val);
                } else {
                    headers.insert(key, val);
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
                headers,
                peer_ip: peer_ip.to_string(),
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

/// 自更新替换后由新进程重启时，旧进程可能还占着端口：短暂重试，避免重启失败。
fn bind_listener(listen: &str) -> std::io::Result<TcpListener> {
    let restart = std::env::var("BERTHIO_UPDATE_RESTART").ok().as_deref() == Some("1");
    let mut last_err = None;
    let attempts = if restart { 60 } else { 1 };
    for _ in 0..attempts {
        match TcpListener::bind(listen) {
            Ok(l) => return Ok(l),
            Err(e) => {
                last_err = Some(e);
                if restart {
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
    Err(last_err.unwrap())
}

pub fn serve(listen: &str, store: Arc<Store>, auth: Arc<Auth>, sup: Arc<Supervisor>, demo: bool) {
    let cfg = store.config();
    let (effective, warning) = cfg.effective_listen();
    if warning.is_some() && effective != listen {
        eprintln!("[berthio] 监听地址被安全策略覆盖：{listen} -> {effective}");
    }
    let listener = match bind_listener(listen) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[berthio] 监听 {listen} 失败: {e}");
            std::process::exit(1);
        }
    };
    let tls_rt = if cfg.tls.enabled {
        match crate::tls::load(&store.home, &cfg.tls.cert, &cfg.tls.key) {
            Ok(rt) => Some(Arc::new(rt)),
            Err(e) => {
                eprintln!("[berthio] 启用内置 TLS 失败：{e}");
                eprintln!("[berthio] 修好证书/私钥路径后重启；未自动降级为明文，避免静默失去 TLS。");
                std::process::exit(1);
            }
        }
    } else {
        None
    };
    if let Some(rt) = &tls_rt {
        let _ = ACTIVE_TLS.set(rt.info.clone());
        if let Some(w) = &rt.warning {
            eprintln!("[berthio] {w}");
        }
        crate::tls::spawn_expiry_watch(rt.info.clone());
        println!(
            "[berthio] TLS 证书：{}（到期 {}，SAN {} 项）",
            rt.info.fingerprint,
            rt.info.not_after,
            rt.info.sans.len()
        );
    }
    let scheme = if tls_rt.is_some() { "https" } else { "http" };
    println!(
        "[berthio] {}{} 已启动: {}://{}  (home={}, auth={}, plain_policy={})",
        VERSION,
        if demo { " (demo)" } else { "" },
        scheme,
        listen,
        store.home.display(),
        if demo { "demo" } else { "session+csrf" },
        cfg.tls.plain_policy
    );
    if tls_rt.is_none() && !state::listen_is_loopback(listen) {
        eprintln!(
            "[berthio] 警告：明文监听对外网开放，plain_policy={}；建议启用内置 TLS 或确保可信反代终止 TLS",
            cfg.tls.plain_policy
        );
    }
    let upd = Arc::new(Manager::new(&store.home, VERSION, demo));
    for stream in listener.incoming() {
        match stream {
            Ok(s) => {
                let store = Arc::clone(&store);
                let auth = Arc::clone(&auth);
                let sup = Arc::clone(&sup);
                let upd = Arc::clone(&upd);
                let tls_rt = tls_rt.clone();
                std::thread::spawn(move || {
                    let _ = s.set_read_timeout(Some(Duration::from_secs(15)));
                    let peer_ip = s
                        .peer_addr()
                        .map(|a| a.ip().to_string())
                        .unwrap_or_else(|_| "unknown".into());
                    match tls_rt {
                        Some(rt) => {
                            let conn = match rustls::ServerConnection::new(Arc::clone(&rt.config)) {
                                Ok(c) => c,
                                Err(e) => {
                                    eprintln!("[berthio] TLS 会话初始化失败: {e}");
                                    return;
                                }
                            };
                            let mut stream = rustls::StreamOwned::new(conn, s);
                            handle_conn(&mut stream, &peer_ip, true, &store, &auth, &sup, &upd, demo);
                        }
                        None => {
                            let mut s = s;
                            handle_conn(&mut s, &peer_ip, false, &store, &auth, &sup, &upd, demo);
                        }
                    }
                });
            }
            Err(e) => eprintln!("[berthio] accept 失败: {e}"),
        }
    }
}

fn handle_conn<R: Read + Write>(
    stream: &mut R,
    peer_ip: &str,
    secure: bool,
    store: &Store,
    auth: &Auth,
    sup: &Supervisor,
    upd: &Arc<Manager>,
    demo: bool,
) {
    let resp = match read_request(stream, peer_ip) {
        Ok(req) => route(&req, store, auth, sup, upd, demo, secure),
        Err(e) => Response::json(400, &json!({"error":{"code":"bad_request","message":e}})),
    };
    let _ = write_response(stream, resp);
}

fn write_response<W: Write>(stream: &mut W, resp: Response) -> std::io::Result<()> {
    let reason = match resp.status {
        200 => "OK",
        201 => "Created",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        422 => "Unprocessable Entity",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        _ => "OK",
    };
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\nX-Frame-Options: DENY\r\nReferrer-Policy: no-referrer\r\nCache-Control: no-store\r\n",
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

fn route(
    req: &Request,
    store: &Store,
    auth: &Auth,
    sup: &Supervisor,
    upd: &Arc<Manager>,
    demo: bool,
    conn_secure: bool,
) -> Response {
    let cfg = store.config();
    let net = net_info(req, &cfg, conn_secure);
    let token = req.cookie(SESSION_COOKIE);
    let path = req.path.as_str();
    // 明文策略：只有“确认不是 HTTPS”的来源才可能被拒；回环与可信反代终止 TLS 的请求不受影响。
    if !net.https && !is_loopback_ip(&net.client_ip) {
        let policy = cfg.tls.plain_policy.as_str();
        let plaintext_login = matches!(req.method.as_str(), "POST")
            && matches!(path, "/api/auth/login" | "/api/auth/setup" | "/api/auth/password");
        if policy == "strict" || (policy == "loopback-only" && plaintext_login) {
            let msg = if policy == "strict" {
                "面板要求 HTTPS：当前请求来自非回环地址且未使用 TLS，已拒绝"
            } else {
                "明文策略 loopback-only：拒绝非回环来源的明文登录，请使用 HTTPS"
            };
            return Response::json(
                403,
                &json!({"error": {"code": "https_required", "message": msg, "policy": policy, "client_ip": net.client_ip}}),
            )
            .with("X-Berthio-HTTPS", "required");
        }
    }
    let is_public = matches!(path, "/" | "/index.html" | "/app.js" | "/favicon.ico" | "/healthz")
        || matches!(path, "/api/auth/status" | "/api/auth/setup" | "/api/auth/login")
        || path.starts_with("/sub/");
    let mut actor = if demo { "demo-admin" } else { "anonymous" }.to_string();
    if path.starts_with("/api/") && !is_public {
        match auth.require(token.as_deref(), &net.client_ip) {
            Ok(s) => actor = s.actor,
            Err(e) => return Response::json(e.status, &e.to_json()),
        }
        if matches!(req.method.as_str(), "POST" | "PUT" | "PATCH" | "DELETE") {
            if let Err(e) = auth.check_csrf(
                token.as_deref().unwrap_or(""),
                req.header("x-berthio-csrf"),
                &net.client_ip,
            ) {
                return Response::json(e.status, &e.to_json());
            }
        }
    }
    let result: ApiResult<Response> = match (req.method.as_str(), path) {
        ("GET", "/") | ("GET", "/index.html") => Ok(Response::html(include_str!("../web/index.html"))),
        ("GET", "/app.js") => Ok(Response::text(
            200,
            "application/javascript; charset=utf-8",
            include_str!("../web/app.js").to_string(),
        )),
        ("GET", "/favicon.ico") => Ok(Response::text(204, "image/x-icon", String::new())),
        ("GET", "/healthz") => Ok(Response::json(200, &json!({"status":"ok","version":VERSION}))),
        ("GET", "/api/auth/status") => {
            Ok(Response::json(200, &auth.status(token.as_deref(), &net.client_ip)))
        }
        ("POST", "/api/auth/setup") => auth_setup(req, store, auth),
        ("POST", "/api/auth/login") => auth_login(req, store, auth, &net),
        ("POST", "/api/auth/logout") => auth_logout(req, store, auth, token.as_deref(), &net),
        ("POST", "/api/auth/password") => auth_password(req, store, auth, &net),
        ("GET", "/api/panel") => Ok(Response::json(200, &panel_payload(store, auth, demo, &cfg, &net))),
        ("GET", "/api/tls") => Ok(Response::json(200, &tls_payload(&cfg, &net))),
        ("POST", "/api/tls/generate") => tls_generate(store, &actor, &net),
        ("PATCH", "/api/panel") => patch_panel(req, store, &actor, &net),
        ("GET", "/api/update/status") => Ok(Response::json(200, &upd.status(&cfg.update))),
        ("POST", "/api/update/check") => update_check(upd, store, demo, &actor, &net.client_ip),
        ("POST", "/api/update/download") => update_download(req, upd, store, demo, &actor, &net.client_ip),
        ("POST", "/api/update/install") => update_install(req, upd, store, demo, &actor, &net.client_ip),
        ("GET", "/api/kernels") => Ok(Response::json(200, &kernel_payload(store, sup))),
        ("POST", p) if p.starts_with("/api/kernels/") => kernel_action(store, sup, p, &actor, &net.client_ip),
        ("GET", p) if p.starts_with("/api/kernels/") && p.ends_with("/log") => {
            let name = &p["/api/kernels/".len()..p.len() - "/log".len()];
            let tail = req
                .query
                .get("tail")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(120);
            Ok(Response::json(200, &sup.log_tail(name, tail)))
        }
        ("GET", "/api/rulesets") => Ok(Response::json(200, &rulesets_payload(store))),
        ("GET", "/api/rulesets/sources") => {
            Ok(Response::json(200, &sources::public_payload(store)))
        }
        ("POST", "/api/rulesets/sources") => add_source(req, store),
        ("POST", "/api/rulesets/sources/test") => test_source(req, store),
        ("PUT", "/api/rulesets/sources/mode") => set_source_mode(req, store),
        ("POST", "/api/rulesets/fetch") => fetch_ruleset(req, store),
        ("PATCH", p) if p.starts_with("/api/rulesets/sources/") => {
            let id = &p["/api/rulesets/sources/".len()..];
            patch_source(req, store, id)
        }
        ("DELETE", p) if p.starts_with("/api/rulesets/sources/") => {
            let id = &p["/api/rulesets/sources/".len()..];
            sources::delete_source(store, id).map(|v| Response::json(200, &v))
        }
        ("GET", "/api/inbound/inbounds") => Ok(Response::json(200, &inbound_list(store))),
        ("POST", "/api/inbound/inbounds") => create_inbound(req, store, &actor, &net.client_ip),
        ("GET", "/api/inbound/wizard/defaults") => Ok(Response::json(200, &wizard::defaults(store))),
        ("GET", "/api/export/preview") => export_preview(req, store, &actor, &net.client_ip),
        ("GET", "/api/export/file") => export_file(req, store, &actor, &net.client_ip),
        ("POST", "/api/export/tokens") => create_token(req, store),
        ("POST", "/api/migrate/preview") => migrate_preview(req, store, &actor, &net.client_ip),
        ("POST", "/api/migrate/apply") => migrate_apply(req, store, &actor, &net.client_ip),
        ("GET", "/api/logs") => Ok(Response::json(200, &logs_payload(store))),
        ("GET", "/api/audit") => Ok(Response::json(200, &audit_payload(store, req))),
        ("GET", "/api/audit.csv") => audit_csv(store, req),
        ("GET", p) if p.starts_with("/sub/") => subscription(req, &p[5..], store),
        ("PATCH", p) if p.starts_with("/api/inbound/inbounds/") => {
            let id = &p["/api/inbound/inbounds/".len()..];
            if id.contains('/') {
                Err(ApiError::not_found(format!("未知入站接口 {p}")))
            } else {
                update_inbound(req, store, id, &actor, &net.client_ip)
            }
        }
        ("DELETE", p) if p.starts_with("/api/inbound/inbounds/") => {
            let id = &p["/api/inbound/inbounds/".len()..];
            if id.contains('/') {
                Err(ApiError::not_found(format!("未知入站接口 {p}")))
            } else {
                delete_inbound(req, store, id, &actor, &net.client_ip)
            }
        }
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
            create_client(req, store, id, &actor, &net.client_ip)
        }
        ("POST", p) if p.starts_with("/api/export/tokens/") && p.ends_with("/revoke") => {
            let id = &p["/api/export/tokens/".len()..p.len() - "/revoke".len()];
            revoke_token(store, id)
        }
        ("POST", p) if p.starts_with("/api/export/tokens/") && p.ends_with("/rotate") => {
            let id = &p["/api/export/tokens/".len()..p.len() - "/rotate".len()];
            rotate_token(store, id)
        }
        _ => Err(ApiError::not_found(format!("{} {}", req.method, req.path))),
    };
    let mut resp = match result {
        Ok(r) => r,
        Err(e) => Response::json(e.status, &e.to_json()),
    };
    // TLS（或可信反代终止 TLS）生效时统一加 HSTS；明文访问不加，避免把浏览器锁死在不可用的 HTTPS。
    if net.https {
        resp.extra.push((
            "Strict-Transport-Security",
            "max-age=31536000; includeSubDomains".to_string(),
        ));
    }
    resp
}

fn auth_setup(req: &Request, store: &Store, auth: &Auth) -> ApiResult<Response> {
    let body = body_json(req)?;
    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    let confirm = body.get("confirm").and_then(|v| v.as_str()).unwrap_or("");
    let out = auth.setup(password, confirm)?;
    store.audit_op(
        "auth.setup",
        "anonymous",
        &req.peer_ip,
        "panel",
        "ok",
        json!({"initialized": true, "hash_algo": "pbkdf2-hmac-sha256", "plaintext_logged": false}),
    );
    Ok(Response::json(200, &out))
}

fn auth_login(req: &Request, store: &Store, auth: &Auth, net: &NetInfo) -> ApiResult<Response> {
    let body = body_json(req)?;
    let password = body.get("password").and_then(|v| v.as_str()).unwrap_or("");
    if password.is_empty() {
        return Err(ApiError::bad_request("缺少口令").with_field("password", "请输入管理员口令"));
    }
    match auth.login(password, &net.client_ip) {
        Ok((payload, token, _csrf)) => {
            store.audit_op(
                "auth.login",
                "admin",
                &net.client_ip,
                "session",
                "ok",
                json!({"peer_ip": req.peer_ip, "behind_proxy": net.behind_proxy, "secure_cookie": net.https}),
            );
            let max_age = auth.session_ttl();
            Ok(Response::json(200, &payload).with("Set-Cookie", session_cookie(&token, max_age, net.https)))
        }
        Err(e) => {
            let (event, result) = if e.status == 429 {
                ("auth.login.locked", "locked")
            } else {
                ("auth.login.failed", "failed")
            };
            store.audit_op(
                event,
                "anonymous",
                &net.client_ip,
                "session",
                result,
                json!({"peer_ip": req.peer_ip, "code": e.code, "message": e.message}),
            );
            Err(e)
        }
    }
}

fn auth_logout(
    req: &Request,
    store: &Store,
    auth: &Auth,
    token: Option<&str>,
    net: &NetInfo,
) -> ApiResult<Response> {
    let _ = req;
    let token = token.ok_or_else(|| ApiError::new(401, "unauthenticated", "需要登录"))?;
    let out = auth.logout(token)?;
    store.audit("auth.logout", "admin", json!({"ip": net.client_ip}));
    Ok(Response::json(200, &out).with("Set-Cookie", clear_cookie(net.https)))
}

fn auth_password(req: &Request, store: &Store, auth: &Auth, net: &NetInfo) -> ApiResult<Response> {
    let body = body_json(req)?;
    let current = body.get("current").and_then(|v| v.as_str()).unwrap_or("");
    let new = body.get("new").and_then(|v| v.as_str()).unwrap_or("");
    let confirm = body.get("confirm").and_then(|v| v.as_str()).unwrap_or("");
    let out = auth.change_password(current, new, confirm, &net.client_ip)?;
    store.audit(
        "auth.password.changed",
        "admin",
        json!({"ip": net.client_ip, "sessions_revoked": out["sessions_revoked"], "plaintext_logged": false}),
    );
    Ok(Response::json(200, &out).with("Set-Cookie", clear_cookie(net.https)))
}

fn panel_payload(
    store: &Store,
    auth: &Auth,
    demo: bool,
    cfg: &PanelConfig,
    net: &NetInfo,
) -> serde_json::Value {
    let state = store.snapshot();
    let clients: usize = state.inbounds.iter().map(|i| i.clients.len()).sum();
    let (effective, forced) = cfg.effective_listen();
    json!({
        "version": VERSION,
        "version_source": "Cargo.toml（构建时以 CARGO_PKG_VERSION 注入，单一来源）",
        "listen": cfg.listen,
        "listen_effective": effective,
        "panel_address": state.panel.panel_address,
        "home": store.home.display().to_string(),
        "demo": demo,
        "demo_label": if demo { "演示数据" } else { "" },
        "created_at": state.panel.created_at,
        "counts": {"inbounds": state.inbounds.len(), "clients": clients},
        "remote_access": {
            "enabled": cfg.remote_access.enabled,
            "acknowledged_at": cfg.remote_access.acknowledged_at,
            "trusted_proxies": cfg.remote_access.trusted_proxies,
            "forced_loopback": forced.is_some(),
            "warning": forced.unwrap_or_default(),
            "confirm_phrase_required": REMOTE_CONFIRM_PHRASE,
            "risks": remote_risks(),
        },
        "tls": {
            "configured": cfg.tls.enabled,
            "active": ACTIVE_TLS.get().map(|_| true).unwrap_or(false),
            "cert_path": cfg.tls.cert,
            "key_path": cfg.tls.key,
            "plain_policy": cfg.tls.plain_policy,
            "insecure_request": !net.https,
            "client_ip": net.client_ip,
            "cert": ACTIVE_TLS.get().map(|i| i.to_json()),
        },
        "auth": {
            "mode": if demo { "demo" } else { "session" },
            "initialized": auth.initialized(),
            "session_ttl_seconds": auth.session_ttl(),
            "password_storage": "PBKDF2-HMAC-SHA256（随机盐 + 迭代，只存哈希）",
            "cookie": "HttpOnly; SameSite=Strict; HTTPS 下自动加 Secure",
            "csrf": "所有写操作要求 X-Berthio-CSRF",
            "forwarded_headers": "默认不信任 X-Forwarded-For / X-Forwarded-Proto；只有 peer 属于 trusted_proxies（支持 CIDR）时才解析",
        }
    })
}

/// TLS 现状：既给出配置，也给出真实生效的证书信息（只有以 TLS 启动过才有）。
fn tls_payload(cfg: &PanelConfig, net: &NetInfo) -> serde_json::Value {
    let active = ACTIVE_TLS.get();
    json!({
        "configured": cfg.tls.enabled,
        "active": active.is_some(),
        "cert_path": cfg.tls.cert,
        "key_path": cfg.tls.key,
        "plain_policy": cfg.tls.plain_policy,
        "policies": ["warn", "loopback-only", "strict"],
        "insecure_request": !net.https,
        "client_ip": net.client_ip,
        "cert": active.map(|i| i.to_json()),
        "scheme_this_request": if net.https { "https" } else { "http" },
        "hsts": if net.https { "max-age=31536000; includeSubDomains" } else { "" },
        "note": "证书/密钥/策略变更需重启进程生效；生成自签证书不会自动启用",
    })
}

fn tls_generate(store: &Store, actor: &str, net: &NetInfo) -> ApiResult<Response> {
    let info = crate::tls::generate_self_signed(&store.home)
        .map_err(|e| ApiError::internal(format!("生成自签证书失败: {e}")))?;
    let mut panel = store.config();
    panel.tls.cert = crate::tls::SELF_SIGNED_CERT.to_string();
    panel.tls.key = crate::tls::SELF_SIGNED_KEY.to_string();
    let cfg = Config { home: store.home.clone() };
    cfg.save_panel(&panel)?;
    store.set_config(panel.clone());
    store.audit_op(
        "tls.cert.generated",
        actor,
        &net.client_ip,
        "tls",
        "ok",
        json!({"fingerprint": info.fingerprint, "not_after": info.not_after, "sans": info.sans}),
    );
    Ok(Response::json(
        200,
        &json!({
            "ok": true,
            "cert_path": crate::tls::SELF_SIGNED_CERT,
            "key_path": crate::tls::SELF_SIGNED_KEY,
            "cert": info.to_json(),
            "restart_required": true,
            "note": "私钥已以 0600 写入数据目录；启用 TLS 并重启后生效",
        }),
    ))
}

fn remote_risks() -> Vec<&'static str> {
    vec![
        "远程监听会把面板暴露到网络：必须使用 HTTPS（或可信反代）与强口令",
        "登录接口会暴露给公网，速率限制只能减缓暴力破解，不能替代网络层防护",
        "订阅链接 /sub/<token> 一旦泄露即可被他人访问，请定期轮换",
        "面板不会自动申请证书，也不会静默提升权限",
    ]
}

fn patch_panel(req: &Request, store: &Store, actor: &str, net: &NetInfo) -> ApiResult<Response> {
    let body = body_json(req)?;
    let mut panel: PanelConfig = store.config();
    let mut changed = false;
    if let Some(address) = body.get("panel_address").and_then(|v| v.as_str()) {
        let cleaned = address
            .trim()
            .trim_start_matches("https://")
            .trim_start_matches("http://")
            .trim_end_matches('/')
            .to_string();
        if cleaned.is_empty() {
            return Err(ApiError::bad_request("对外地址不能为空")
                .with_field("panel_address", "填域名或公网 IP"));
        }
        if cleaned.contains(char::is_whitespace) || cleaned.contains('/') {
            return Err(ApiError::bad_request("对外地址只能是域名或 IP（可带端口）")
                .with_field("panel_address", "不要带路径或空格"));
        }
        panel.panel_address = cleaned.clone();
        store.update(|state| {
            state.panel.panel_address = cleaned;
            Ok(())
        })?;
        changed = true;
    }
    let mut listen_changed = false;
    if let Some(listen) = body.get("listen").and_then(|v| v.as_str()) {
        let listen = listen.trim();
        let (_, port) = listen
            .rsplit_once(':')
            .ok_or_else(|| ApiError::bad_request("监听地址格式必须是 host:port")
                .with_field("listen", "例如 127.0.0.1:18080"))?;
        if port.parse::<u16>().map(|p| p == 0).unwrap_or(true) {
            return Err(ApiError::bad_request("监听端口不合法").with_field("listen", "1–65535"));
        }
        if listen != panel.listen {
            panel.listen = listen.to_string();
            listen_changed = true;
            changed = true;
        }
    }
    let remote_enabled = body.get("remote_enabled").and_then(|v| v.as_bool());
    if let Some(enable) = remote_enabled {
        if enable {
            let confirm = body
                .get("confirm_remote")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if confirm != REMOTE_CONFIRM_PHRASE {
                let mut err = ApiError::bad_request("开启远程访问需要二次确认")
                    .with_field("confirm_remote", format!("请显式提交 confirm_remote={REMOTE_CONFIRM_PHRASE}"))
                    .with_field("acknowledge", "确认已知晓以下风险后重试");
                for r in remote_risks() {
                    err = err.with_field("risk", r);
                }
                return Err(err);
            }
            if !panel.remote_access.enabled {
                panel.remote_access = RemoteAccess {
                    enabled: true,
                    acknowledged_at: util::now_iso(),
                    acknowledged_by: actor.to_string(),
                    trusted_proxies: panel.remote_access.trusted_proxies.clone(),
                };
            }
        } else {
            panel.remote_access.enabled = false;
            panel.remote_access.acknowledged_at.clear();
            panel.remote_access.acknowledged_by.clear();
        }
        changed = true;
    }
    if let Some(proxies) = body.get("trusted_proxies").and_then(|v| v.as_array()) {
        let mut list = Vec::new();
        for p in proxies {
            let Some(s) = p.as_str() else { continue };
            let s = s.trim();
            if s.is_empty() {
                continue;
            }
            if s.contains(char::is_whitespace) || !(s.contains('.') || s.contains(':')) {
                return Err(ApiError::bad_request("trusted_proxies 只能填 IP")
                    .with_field("trusted_proxies", format!("非法条目: {s}")));
            }
            list.push(s.to_string());
        }
        panel.remote_access.trusted_proxies = list;
        changed = true;
    }
    if let Some(mirror) = body.get("update_mirror").and_then(|v| v.as_str()) {
        let mirror = mirror.trim();
        if mirror != "auto" && !panel.update.mirrors.iter().any(|m| m.id == mirror) {
            return Err(ApiError::bad_request("更新镜像不存在")
                .with_field("update_mirror", format!("可选：auto / {}", panel.update.mirrors.iter().map(|m| m.id.as_str()).collect::<Vec<_>>().join(" / "))));
        }
        if mirror != panel.update.mirror {
            panel.update.mirror = mirror.to_string();
            changed = true;
        }
    }
    let mut tls_changed = false;
    if let Some(cert) = body.get("tls_cert").and_then(|v| v.as_str()) {
        let cert = cert.trim();
        if cert.is_empty() {
            return Err(ApiError::bad_request("证书路径不能为空")
                .with_field("tls_cert", "可先点「生成自签证书」"));
        }
        if cert != panel.tls.cert {
            panel.tls.cert = cert.to_string();
            tls_changed = true;
            changed = true;
        }
    }
    if let Some(key) = body.get("tls_key").and_then(|v| v.as_str()) {
        let key = key.trim();
        if key.is_empty() {
            return Err(ApiError::bad_request("私钥路径不能为空")
                .with_field("tls_key", "可先点「生成自签证书」"));
        }
        if key != panel.tls.key {
            panel.tls.key = key.to_string();
            tls_changed = true;
            changed = true;
        }
    }
    if let Some(policy) = body.get("plain_policy").and_then(|v| v.as_str()) {
        let policy = policy.trim();
        if !state::plain_policy_known(policy) {
            return Err(ApiError::bad_request("明文策略不支持")
                .with_field("plain_policy", "可选：warn / loopback-only / strict"));
        }
        if policy != panel.tls.plain_policy {
            panel.tls.plain_policy = policy.to_string();
            tls_changed = true;
            changed = true;
        }
    }
    if let Some(enabled) = body.get("tls_enabled").and_then(|v| v.as_bool()) {
        if enabled && !panel.tls.enabled {
            // 启用前先真实加载一次，证书/私钥不可用就不让配置落盘，避免重启直接起不来。
            crate::tls::load(&store.home, &panel.tls.cert, &panel.tls.key).map_err(|e| {
                ApiError::bad_request("证书/私钥无法用于 TLS").with_field("tls", e)
            })?;
        }
        if enabled != panel.tls.enabled {
            panel.tls.enabled = enabled;
            tls_changed = true;
            changed = true;
        }
    }
    if !panel.remote_access.enabled && !state::listen_is_loopback(&panel.listen) {
        let mut err = ApiError::bad_request("监听非回环地址必须先确认开启远程访问")
            .with_field("listen", panel.listen.clone())
            .with_field("action", format!("同时提交 remote_enabled=true 与 confirm_remote={REMOTE_CONFIRM_PHRASE}"));
        for r in remote_risks() {
            err = err.with_field("risk", r);
        }
        return Err(err);
    }
    if changed {
        let cfg = Config { home: store.home.clone() };
        cfg.save_panel(&panel)?;
        store.set_config(panel.clone());
        store.audit(
            "panel.config.updated",
            actor,
            json!({
                "listen": panel.listen,
                "remote_enabled": panel.remote_access.enabled,
                "trusted_proxies": panel.remote_access.trusted_proxies,
                "tls_enabled": panel.tls.enabled,
                "plain_policy": panel.tls.plain_policy,
                "tls_cert": panel.tls.cert,
                "ip": net.client_ip,
                "peer_ip": req.peer_ip,
            }),
        );
    }
    let (effective, forced) = panel.effective_listen();
    Ok(Response::json(
        200,
        &json!({
            "ok": true,
            "panel_address": panel.panel_address,
            "listen": panel.listen,
            "listen_effective": effective,
            "restart_required": listen_changed || changed,
            "tls": {
                "enabled": panel.tls.enabled,
                "cert_path": panel.tls.cert,
                "key_path": panel.tls.key,
                "plain_policy": panel.tls.plain_policy,
                "restart_required": tls_changed || listen_changed,
            },
            "update": {
                "feed": panel.update.feed,
                "mirror": panel.update.mirror,
                "mirrors": panel.update.mirrors.iter().map(|m| m.id.clone()).collect::<Vec<_>>(),
            },
            "remote_access": {
                "enabled": panel.remote_access.enabled,
                "acknowledged_at": panel.remote_access.acknowledged_at,
                "trusted_proxies": panel.remote_access.trusted_proxies,
                "warning": forced.unwrap_or_default(),
            },
            "note": "监听/远程开关变更需重启面板进程后生效；当前连接不受影响",
        }),
    ))
}

fn update_check(upd: &Arc<Manager>, store: &Store, demo: bool, actor: &str, ip: &str) -> ApiResult<Response> {
    if demo {
        return Err(ApiError::bad_request("演示模式不检查更新"));
    }
    let cfg = store.config().update;
    upd.start_check(cfg.clone())
        .map_err(|e| ApiError::conflict(e).with_field("action", "等待当前更新任务结束后重试"))?;
    store.audit_op(
        "update.check",
        actor,
        ip,
        "update",
        "ok",
        json!({"feed": cfg.feed, "mirror": cfg.mirror}),
    );
    Ok(Response::json(202, &json!({"started": true})))
}

fn update_download(req: &Request, upd: &Arc<Manager>, store: &Store, demo: bool, actor: &str, ip: &str) -> ApiResult<Response> {
    if demo {
        return Err(ApiError::bad_request("演示模式不执行下载"));
    }
    let body = body_json(req)?;
    let save = body.get("save").and_then(|v| v.as_bool()).unwrap_or(false);
    let cfg = store.config().update;
    upd.start_download(cfg, save)
        .map_err(|e| ApiError::conflict(e).with_field("action", "等待当前更新任务结束后重试"))?;
    store.audit_op(
        "update.download",
        actor,
        ip,
        "update",
        "ok",
        json!({"save": save}),
    );
    Ok(Response::json(202, &json!({"started": true, "save": save})))
}

fn update_install(req: &Request, upd: &Arc<Manager>, store: &Store, demo: bool, actor: &str, ip: &str) -> ApiResult<Response> {
    if demo {
        return Err(ApiError::bad_request("演示模式不执行安装"));
    }
    let body = body_json(req)?;
    let path = body.get("path").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    if path.is_empty() {
        return Err(ApiError::bad_request("缺少已下载安装包路径")
            .with_field("path", "先「仅下载」，再用状态里的 saved.path 安装"));
    }
    let cfg = store.config().update;
    upd.start_install_saved(cfg, path.clone())
        .map_err(|e| ApiError::conflict(e).with_field("action", "等待当前更新任务结束后重试"))?;
    store.audit_op(
        "update.install",
        actor,
        ip,
        &path,
        "ok",
        json!({"path": path}),
    );
    Ok(Response::json(202, &json!({"installing": true})))
}

fn kernel_payload(store: &Store, sup: &Supervisor) -> serde_json::Value {
    let mut base = kernels::report(store);
    base["runtime"] = sup.runtime_json(store);
    base["demo"] = json!(sup.is_demo());
    base["supervisor"] = json!({
        "check_commands": {
            "xray": "xray -test -config <config>",
            "sing-box": "sing-box check -c <config>",
            "mihomo": "mihomo -t -f <config>"
        },
        "start_rule": "先配置自检 → 再端口占用检测 → 才 spawn；失败回滚 last-known-good",
        "ports": "启动前逐个 bind 检测；冲突时通过 netstat/ss 找占用者 PID 与进程名",
        "persistence": "state/kernels.json 记录 PID/状态；面板重启核对归属，孤儿清理",
        "auto_restart": "异常退出自动拉起，默认上限 3 次，超限停止并写审计告警",
        "logs": "logs/kernels/<kernel>.log，启动时按 BERTHIO_KERNEL_LOG_MAX（默认 5 MB）轮转，保留 3 份",
        "privilege": "需要 root/特权端口的动作只列出操作并拒绝，不静默提权",
        "demo": "demo 模式绝不 spawn 真实内核",
    });
    base
}

fn kernel_action(store: &Store, sup: &Supervisor, path: &str, actor: &str, ip: &str) -> ApiResult<Response> {
    let rest = &path["/api/kernels/".len()..];
    let (kernel, action) = rest
        .split_once('/')
        .ok_or_else(|| ApiError::not_found(format!("未知内核接口 {path}")))?;
    if !supervisor::is_kernel(kernel) {
        return Err(ApiError::not_found(format!("未知内核 {kernel}")));
    }
    let event = match action {
        "start" => "kernel.start",
        "stop" => "kernel.stop",
        "restart" => "kernel.restart",
        "validate" => "kernel.validate",
        other => other,
    };
    let out = match action {
        "start" => sup.start(store, kernel),
        "stop" => sup.stop(kernel),
        "restart" => sup.restart(store, kernel),
        "validate" => sup.validate(store, kernel),
        _ => return Err(ApiError::not_found(format!("未知动作 {action}"))),
    };
    match out {
        Ok(v) => {
            store.audit_op(
                event,
                actor,
                ip,
                kernel,
                "ok",
                json!({"kernel": kernel, "action": action}),
            );
            Ok(Response::json(200, &v))
        }
        Err(e) => {
            store.audit_op(
                event,
                actor,
                ip,
                kernel,
                "failed",
                json!({"kernel": kernel, "action": action, "code": e.code, "message": e.message}),
            );
            Err(e)
        }
    }
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

fn update_inbound(req: &Request, store: &Store, id: &str, actor: &str, ip: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let payload: wizard::UpdateInboundReq = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("字段解析失败: {e}")))?;
    let out = wizard::update_inbound(store, id, payload, actor, ip)?;
    Ok(Response::json(200, &out))
}

fn delete_inbound(req: &Request, store: &Store, id: &str, actor: &str, ip: &str) -> ApiResult<Response> {
    let confirm = req.query.get("confirm").map(|s| s.as_str()).unwrap_or("");
    let out = wizard::delete_inbound(store, id, confirm, actor, ip)?;
    Ok(Response::json(200, &out))
}

fn create_inbound(req: &Request, store: &Store, actor: &str, ip: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    if body.is_null() {
        return Err(ApiError::bad_request("缺少 JSON 请求体"));
    }
    let payload: wizard::CreateInboundReq = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("字段解析失败: {e}")))?;
    let out = wizard::create_inbound(store, payload, actor, ip)?;
    Ok(Response::json(201, &out))
}

fn create_client(req: &Request, store: &Store, id: &str, actor: &str, ip: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let payload: wizard::CreateClientReq = serde_json::from_value(body)
        .map_err(|e| ApiError::bad_request(format!("字段解析失败: {e}")))?;
    let out = wizard::create_client(store, id, payload, actor, ip)?;
    Ok(Response::json(201, &out))
}

fn export_preview(req: &Request, store: &Store, actor: &str, ip: &str) -> ApiResult<Response> {
    let sel = selection_from(req);
    let payload = export::render_payload(store, sel)?;
    store.audit_op(
        "export.preview",
        actor,
        ip,
        "-",
        "ok",
        json!({"render_id": payload["render_id"], "format": payload["format"], "entries": payload["entries"]}),
    );
    Ok(Response::json(200, &payload))
}

fn export_file(req: &Request, store: &Store, actor: &str, ip: &str) -> ApiResult<Response> {
    let sel = selection_from(req);
    let payload = export::render_payload(store, sel)?;
    if payload["validation"]["overall"] == "FAIL" {
        store.audit_op(
            "export.download",
            actor,
            ip,
            "-",
            "denied",
            json!({"render_id": payload["render_id"], "reason": "validation_fail"}),
        );
        return Err(ApiError::new(422, "export_blocked", "校验 FAIL，拒绝导出").with_field(
            "validation",
            "先修正 L0/L1/L2 中的失败项",
        ));
    }
    let content = payload["content"].as_str().unwrap_or("").to_string();
    let name = payload["file_name"].as_str().unwrap_or("berthio-config.txt");
    store.audit_op(
        "export.download",
        actor,
        ip,
        name,
        "ok",
        json!({"render_id": payload["render_id"], "file_name": name, "bytes": content.len()}),
    );
    Ok(Response::file(content.into_bytes(), name).with(
        "X-Berthio-Render-Id",
        payload["render_id"].as_str().unwrap_or(""),
    ))
}

fn migrate_json_text(body: &serde_json::Value) -> String {
    if let Some(s) = body.get("json_text").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            return s.to_string();
        }
    }
    if let Some(s) = body.get("payload").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            return s.to_string();
        }
    }
    match body.get("json") {
        Some(serde_json::Value::String(s)) if !s.trim().is_empty() => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    }
}

fn migrate_label(body: &serde_json::Value) -> String {
    let raw = body
        .get("label")
        .or_else(|| body.get("file_name"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let clean: String = raw
        .chars()
        .filter(|c| !c.is_control())
        .take(80)
        .collect();
    if clean.trim().is_empty() {
        "pasted".to_string()
    } else {
        clean.trim().to_string()
    }
}

fn migrate_preview(req: &Request, store: &Store, actor: &str, ip: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let db_path = body.get("db_path").and_then(|v| v.as_str()).unwrap_or("");
    let json_text = migrate_json_text(&body);
    let label = migrate_label(&body);
    let out = crate::migrate::preview(store, db_path, &json_text, &label, actor, ip)?;
    Ok(Response::json(200, &out))
}

fn migrate_apply(req: &Request, store: &Store, actor: &str, ip: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let out = crate::migrate::apply(
        store,
        body.get("db_path").and_then(|v| v.as_str()).unwrap_or(""),
        &migrate_json_text(&body),
        &migrate_label(&body),
        body.get("preview_id").and_then(|v| v.as_str()).unwrap_or(""),
        body.get("plan_hash").and_then(|v| v.as_str()).unwrap_or(""),
        body.get("confirm").and_then(|v| v.as_bool()).unwrap_or(false),
        actor,
        ip,
    )?;
    Ok(Response::json(200, &out))
}

/// 订阅目标归一化：只接受两种真实导出格式，`clash` 作为 mihomo 的兼容别名。
fn normalize_target(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "mihomo" | "clash" | "clash-verge" | "clash.meta" | "verge" | "stash" => Some("mihomo"),
        "sing-box" | "singbox" | "sb" | "sfa" | "sfi" | "sft" => Some("sing-box"),
        _ => None,
    }
}

fn target_from_ua(ua: &str) -> Option<&'static str> {
    let ua = ua.to_ascii_lowercase();
    if ua.contains("sing-box") || ua.contains("singbox") || ua.contains("sfa/") || ua.contains("sfi/") {
        return Some("sing-box");
    }
    if ua.contains("mihomo") || ua.contains("clash") || ua.contains("stash") || ua.contains("verge") {
        return Some("mihomo");
    }
    None
}

fn create_token(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let client_id = body.get("client_id").and_then(|v| v.as_str()).unwrap_or("");
    let target_raw = body
        .get("target")
        .and_then(|v| v.as_str())
        .unwrap_or("mihomo");
    let target = normalize_target(target_raw).ok_or_else(|| {
        ApiError::bad_request("target 只支持 mihomo / sing-box（clash 作为 mihomo 别名）")
            .with_field("target", "mihomo | sing-box")
    })?;
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

fn rotate_token(store: &Store, id: &str) -> ApiResult<Response> {
    let mut new_token = String::new();
    store.update(|st| {
        let t = st
            .tokens
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| ApiError::not_found("token 不存在"))?;
        if t.revoked_at.is_some() {
            return Err(ApiError::conflict(
                "已吊销的令牌不能轮换；请为该客户端新建 token（吊销是终态）",
            ));
        }
        new_token = format!("bto_{}", util::random_hex(24));
        t.token_hash = util::sha256_hex(new_token.as_bytes());
        t.last_access = None;
        Ok(())
    })?;
    store.audit(
        "export.token.rotated",
        "local-admin",
        json!({"token_id": id, "note": "旧 token 立即失效"}),
    );
    Ok(Response::json(
        200,
        &json!({
            "ok": true,
            "id": id,
            "token": new_token,
            "note": "旧 token 已立即失效；服务端只存新 token 的 SHA-256，明文仅本次返回",
        }),
    ))
}

fn subscription(req: &Request, token: &str, store: &Store) -> ApiResult<Response> {
    if token.trim().is_empty() {
        return Err(ApiError::new(
            401,
            "subscription_unauthorized",
            "缺少订阅令牌；/sub 链接里的 token 等同于读取该客户端配置的凭据",
        ));
    }
    let hash = util::sha256_hex(token.as_bytes());
    let state = store.snapshot();
    // 无效 token 一律 401，且不区分“格式对但不存在”，避免枚举。
    let rec = state
        .tokens
        .iter()
        .find(|t| t.token_hash == hash)
        .ok_or_else(|| {
            ApiError::new(401, "subscription_unauthorized", "订阅令牌无效或已被轮换")
        })?;
    if rec.revoked_at.is_some() {
        store.audit(
            "sub.revoked_denied",
            "subscription",
            json!({"token_id": rec.id, "reason": "revoked"}),
        );
        return Err(ApiError::new(403, "revoked", "订阅已被吊销（吊销立即失效）"));
    }

    let explicit = req
        .query
        .get("target")
        .map(|s| s.as_str())
        .filter(|s| !s.trim().is_empty());
    let target = match explicit {
        Some(raw) => normalize_target(raw).ok_or_else(|| {
            ApiError::bad_request("target 只支持 mihomo / sing-box")
                .with_field("target", "mihomo | sing-box")
        })?,
        None => target_from_ua(req.header("user-agent").unwrap_or(""))
            .or_else(|| normalize_target(&rec.target))
            .unwrap_or("mihomo"),
    };
    let format = if target == "sing-box" { "singbox" } else { "clash" };

    let mut inbound_ids = Vec::new();
    let mut client_enabled = true;
    let mut found_client = false;
    for i in &state.inbounds {
        if let Some(c) = i.clients.iter().find(|c| c.id == rec.client_id) {
            found_client = true;
            client_enabled = c.enabled;
            inbound_ids.push(i.id.clone());
        }
    }
    if !found_client {
        return Err(ApiError::not_found("订阅绑定的客户端已不存在"));
    }
    if !client_enabled {
        store.audit(
            "sub.disabled_denied",
            "subscription",
            json!({"token_id": rec.id, "reason": "client_disabled"}),
        );
        return Err(ApiError::new(
            403,
            "client_disabled",
            "该客户端在 Berthio 中已停用，订阅不再下发",
        ));
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
    let overall = rendered
        .validation
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or("FAIL")
        .to_string();
    let render_id = rendered.render_id.clone();
    let warnings = rendered.warnings.clone();
    if overall == "FAIL" {
        let detail = rendered
            .validation
            .get("layers")
            .and_then(|v| v.as_array())
            .and_then(|layers| {
                layers
                    .iter()
                    .find(|l| l.get("status").and_then(|s| s.as_str()) == Some("FAIL"))
            })
            .map(|layer| {
                let item_msg = layer
                    .get("items")
                    .and_then(|v| v.as_array())
                    .and_then(|items| {
                        items
                            .iter()
                            .find(|i| i.get("level").and_then(|s| s.as_str()) == Some("fail"))
                    })
                    .and_then(|i| i.get("message").and_then(|s| s.as_str()))
                    .map(|s| s.to_string());
                item_msg
                    .or_else(|| {
                        layer
                            .get("output")
                            .and_then(|s| s.as_str())
                            .map(|s| s.trim().to_string())
                    })
                    .unwrap_or_default()
            })
            .unwrap_or_default();
        store.audit(
            "sub.blocked",
            "subscription",
            json!({"token_id": rec.id, "target": target, "render_id": render_id, "reason": "validation_fail", "detail": detail}),
        );
        let mut err = ApiError::new(
            422,
            "subscription_blocked",
            "订阅输出未通过 L0/L1/L2 校验，拒绝下发；修复后重试",
        )
        .with_field("render_id", render_id.as_str())
        .with_field("validation", overall.as_str());
        if !detail.is_empty() {
            err = err.with_field("detail", detail.as_str());
        }
        return Err(err);
    }
    let _ = store.update(|st| {
        if let Some(t) = st.tokens.iter_mut().find(|t| t.id == rec.id) {
            t.last_access = Some(util::now_iso());
        }
        Ok(())
    });
    store.audit(
        "sub.access",
        "subscription",
        json!({
            "token_id": rec.id,
            "target": target,
            "render_id": render_id,
            "validation": overall,
            "clients": 1,
            "inbounds": rendered.entries,
        }),
    );
    let ct = if format == "singbox" {
        "application/json; charset=utf-8"
    } else {
        "text/yaml; charset=utf-8"
    };
    Ok(Response::text(200, ct, rendered.content)
        .with("X-Berthio-Render-Id", render_id)
        .with("X-Berthio-Target", target)
        .with("X-Berthio-Validation", overall)
        .with("Cache-Control", "no-store")
        .with(
            "X-Berthio-Warnings",
            if warnings.is_empty() { "0".to_string() } else { warnings.len().to_string() },
        ))
}

fn rulesets_payload(store: &Store) -> serde_json::Value {
    let cache = sources::cache_summary(store);
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
        "cache": cache,
        "sources": {
            "official": {
                "id":"official",
                "label":"官方源（GitHub MetaCubeX/meta-rules-dat）",
                "base": "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat",
                "enabled": true,
                "note": "校验基准仍以官方 SHA-256 为准"
            },
            "mirror_note": "M2 已接入：内置多个加速镜像 + 自动回退 + 手动指定 + 官方 SHA-256 拦截"
        }
    })
}

fn add_source(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let label = body.get("label").and_then(|v| v.as_str()).unwrap_or("");
    let url_mihomo = body
        .get("url_mihomo")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let url_singbox = body
        .get("url_singbox")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    let priority = body
        .get("priority")
        .and_then(|v| v.as_u64())
        .unwrap_or(6) as u32;
    let enabled = body
        .get("enabled")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let out = sources::add_source(store, label, url_mihomo, url_singbox, priority, enabled)?;
    Ok(Response::json(201, &out))
}

fn patch_source(req: &Request, store: &Store, id: &str) -> ApiResult<Response> {
    let body = body_json(req)?;
    let enabled = body.get("enabled").and_then(|v| v.as_bool());
    let priority = body
        .get("priority")
        .and_then(|v| v.as_u64())
        .map(|v| v as u32);
    let url_mihomo = body
        .get("url_mihomo")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let url_singbox = body
        .get("url_singbox")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let out = sources::patch_source(store, id, enabled, priority, url_mihomo, url_singbox)?;
    Ok(Response::json(200, &out))
}

fn test_source(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    if id.is_empty() {
        return Err(ApiError::bad_request("缺少 id"));
    }
    let out = sources::test_source(store, id)?;
    Ok(Response::json(200, &out))
}

fn set_source_mode(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let mode = body.get("mode").and_then(|v| v.as_str()).unwrap_or("auto");
    let out = sources::set_mode(store, mode)?;
    Ok(Response::json(200, &out))
}

fn fetch_ruleset(req: &Request, store: &Store) -> ApiResult<Response> {
    let body = body_json(req)?;
    let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("");
    let format = body
        .get("format")
        .and_then(|v| v.as_str())
        .unwrap_or("mihomo");
    let source = body
        .get("source")
        .and_then(|v| v.as_str())
        .unwrap_or("auto");
    let refresh = body
        .get("refresh_baseline")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let out = sources::fetch(
        store,
        sources::FetchReq {
            id: id.to_string(),
            format: format.to_string(),
            source: source.to_string(),
            refresh_baseline: refresh,
        },
    )?;
    Ok(Response::json(200, &out))
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

fn audit_query(req: &Request) -> crate::auditlog::Query {
    let get = |k: &str| {
        req.query
            .get(k)
            .map(|v| v.trim().to_string())
            .unwrap_or_default()
    };
    crate::auditlog::Query {
        from: get("from"),
        to: get("to"),
        actor: get("actor"),
        action: get("action"),
        result: get("result"),
        keyword: get("q"),
        limit: get("limit").parse::<usize>().unwrap_or(200),
    }
}

fn audit_payload(store: &Store, req: &Request) -> serde_json::Value {
    let q = audit_query(req);
    let out = crate::auditlog::query(&store.home, &q);
    json!({
        "items": out.items,
        "count": out.items.len(),
        "scanned": out.scanned,
        "files": out.files,
        "dir": crate::auditlog::DIR,
        "retention": {
            "keep_days": store.config().log_keep_days,
            "max_bytes": crate::auditlog::max_bytes(),
        },
        "health": crate::auditlog::health(),
        "readonly": true,
        "note": "只追加、只读；没有删除或清空接口。密码/私钥/订阅密文/uuid/token 在写入前已脱敏。",
    })
}

fn audit_csv(store: &Store, req: &Request) -> ApiResult<Response> {
    let q = audit_query(req);
    let out = crate::auditlog::query(&store.home, &q);
    let mut csv = String::from("时间,操作者,来源IP,动作,目标,结果,详情\n");
    for item in &out.items {
        let ts = item.get("ts").and_then(|v| v.as_str()).unwrap_or("");
        let actor = item.get("actor").and_then(|v| v.as_str()).unwrap_or("");
        let ip = item.get("ip").and_then(|v| v.as_str()).unwrap_or("");
        let action = item.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let target = item.get("target").and_then(|v| v.as_str()).unwrap_or("");
        let result = item.get("result").and_then(|v| v.as_str()).unwrap_or("");
        let detail = item
            .get("detail")
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".to_string());
        let row = [ts, actor, ip, action, target, result, &detail]
            .iter()
            .map(|c| csv_cell(c))
            .collect::<Vec<_>>()
            .join(",");
        csv.push_str(&row);
        csv.push('\n');
    }
    let name = format!("berthio-audit-{}.csv", util::now_iso().replace(':', ""));
    let mut resp = Response::file(csv.into_bytes(), &name);
    resp.content_type = "text/csv; charset=utf-8";
    // 审计导出本身也是一次操作，但不写回审计日志（避免导出引起日志自增循环）。
    Ok(resp)
}

/// CSV 单元：双引号包裹 + 转义；以 = + - @ 开头的值加前缀，避免表格公式注入。
fn csv_cell(raw: &str) -> String {
    let mut s = raw.to_string();
    if let Some(first) = s.chars().next() {
        if matches!(first, '=' | '+' | '-' | '@') {
            s.insert(0, '\'');
        }
    }
    format!("\"{}\"", s.replace('"', "\"\""))
}
