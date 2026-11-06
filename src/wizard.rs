use crate::error::{ApiError, ApiResult};
use crate::kernels;
use crate::model::{Client, Inbound, RealityConfig, TlsConfig};
use crate::state::Store;
use crate::util;
use base64::Engine;
use serde::Deserialize;
use std::net::TcpListener;

const CANDIDATES: [u16; 5] = [443, 8443, 2053, 2083, 2087];

fn port_free(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

fn candidate_hint() -> String {
    CANDIDATES
        .iter()
        .map(|p| p.to_string())
        .collect::<Vec<_>>()
        .join(" → ")
}

pub fn defaults(store: &Store) -> serde_json::Value {
    let xray = kernels::detect(store, "xray");
    let singbox = kernels::detect(store, "sing-box");
    let state = store.snapshot();
    let candidates: Vec<serde_json::Value> = CANDIDATES
        .iter()
        .map(|p| {
            let used = state.inbounds.iter().any(|i| i.port == *p && i.l4 == "tcp");
            serde_json::json!({
                "port": p,
                "l4": "tcp",
                "free": !used && port_free(*p),
                "reason": if used { "已被本面板入站占用" } else if !port_free(*p) { "本机回环端口被占用" } else { "可用（仅探测 127.0.0.1）" }
            })
        })
        .collect();

    serde_json::json!({
        "steps": [
            {"no": 1, "title": "协议与目标", "required": ["remark"]},
            {"no": 2, "title": "端口与监听", "required": []},
            {"no": 3, "title": "Reality 参数", "required": []},
            {"no": 4, "title": "TLS", "required": []},
            {"no": 5, "title": "防火墙", "required": []},
            {"no": 6, "title": "默认客户端与提交", "required": []}
        ],
        "protocols": [
            {
                "id": "vless-reality", "label": "VLESS + Reality（默认）",
                "protocol": "vless", "network": "tcp", "security": "reality", "kernel": "xray",
                "supported": true, "support_level": "unverified",
                "evidence": if xray.present { "本机检测到 xray，但尚未执行 xray -test" } else { "未检测到 xray：创建后可导出，但服务端能力未验证" }
            },
            {
                "id": "vless-ws-tls", "label": "VLESS + WS + TLS",
                "protocol": "vless", "network": "ws", "security": "tls", "kernel": "xray",
                "supported": true, "support_level": "unverified",
                "evidence": "自签证书需用户确认；未执行 xray -test"
            },
            {
                "id": "vless-xhttp-tls", "label": "VLESS + XHTTP + TLS（可选）",
                "protocol": "vless", "network": "xhttp", "security": "tls", "kernel": "xray",
                "supported": true, "support_level": "unverified",
                "evidence": "xhttp 支持情况未知；导出给客户端可能不可用"
            },
            {
                "id": "hysteria2", "label": "hysteria2（sing-box，M2）",
                "protocol": "hysteria2", "network": "tcp", "security": "tls", "kernel": "sing-box",
                "supported": false, "support_level": "unsupported",
                "evidence": if singbox.present { "M1 不接入 sing-box 入站；请等 M2" } else { "未安装 sing-box；M1 不接入该协议" }
            }
        ],
        "targets": [
            {"id": "mihomo", "label": "mihomo", "supported": true},
            {"id": "singbox", "label": "sing-box", "supported": true},
            {"id": "both", "label": "两者", "supported": true}
        ],
        "port_candidates": candidates,
        "defaults": {
            "remark": "",
            "protocol": "vless-reality",
            "target": "both",
            "listen": "0.0.0.0",
            "port": 443,
            "dest": "www.microsoft.com:443",
            "server_names": ["www.microsoft.com"],
            "fingerprint": "chrome",
            "spider_x": "/",
            "flow": "xtls-rprx-vision",
            "client_email_suffix": "-01",
            "preset_rulesets": "daily"
        },
        "server": {
            "xray": {"present": xray.present, "version": xray.version},
            "sing-box": {"present": singbox.present, "version": singbox.version}
        },
        "notes": [
            "默认值来自 INBOUND-SCOPE §10.2；标 unverified 的项不得当成本机已验证",
            "端口探测只绑定 127.0.0.1，未覆盖云安全组与他人机器的占用"
        ]
    })
}

#[derive(Debug, Deserialize, Default)]
pub struct RealityReq {
    #[serde(default)]
    pub dest: String,
    #[serde(default)]
    pub server_names: Vec<String>,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub spider_x: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct TlsReq {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub server_name: String,
}

#[derive(Debug, Deserialize, Default)]
pub struct ClientReq {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub quota_bytes: u64,
}

#[derive(Debug, Deserialize)]
pub struct CreateInboundReq {
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub security: String,
    #[serde(default)]
    pub listen: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub server_kernel: String,
    #[serde(default)]
    pub target: String,
    #[serde(default)]
    pub reality: Option<RealityReq>,
    #[serde(default)]
    pub tls: Option<TlsReq>,
    #[serde(default)]
    pub client: Option<ClientReq>,
    #[serde(default)]
    pub allow_unverified: bool,
}

pub fn create_inbound(
    store: &Store,
    req: CreateInboundReq,
    actor: &str,
    ip: &str,
) -> ApiResult<serde_json::Value> {
    if req.remark.trim().is_empty() {
        return Err(ApiError::bad_request("名称（remark）必填").with_field("remark", "不能为空"));
    }
    if req.remark.chars().count() > 64 {
        return Err(ApiError::bad_request("名称过长").with_field("remark", "最多 64 个字符"));
    }
    let protocol = if req.protocol.is_empty() {
        "vless".to_string()
    } else {
        req.protocol.to_ascii_lowercase()
    };
    let network = if req.network.is_empty() {
        "tcp".to_string()
    } else {
        req.network.to_ascii_lowercase()
    };
    let security = if req.security.is_empty() {
        "reality".to_string()
    } else {
        req.security.to_ascii_lowercase()
    };
    if protocol != "vless" {
        return Err(ApiError::bad_request(format!(
            "M1 仅支持 vless 入站；{protocol} 需 sing-box，规划在 M2"
        ))
        .with_field("protocol", "当前里程碑不支持该协议"));
    }
    if !matches!(network.as_str(), "tcp" | "ws" | "xhttp") {
        return Err(ApiError::bad_request("不支持的传输").with_field("network", "仅 tcp / ws / xhttp"));
    }
    if !matches!(security.as_str(), "reality" | "tls") {
        return Err(ApiError::bad_request("不支持的安全层").with_field("security", "仅 reality / tls"));
    }
    if security == "reality" && network != "tcp" {
        return Err(ApiError::bad_request("Reality 仅支持 tcp 传输").with_field("network", "Reality 下固定 tcp"));
    }

    let listen = if req.listen.is_empty() {
        "0.0.0.0".to_string()
    } else {
        req.listen
    };
    let port = req.port;
    if port == 0 {
        return Err(ApiError::bad_request("端口必须为 1–65535").with_field("port", "未填写端口"));
    }
    let l4 = if matches!(protocol.as_str(), "hysteria2" | "tuic") {
        "udp"
    } else {
        "tcp"
    };
    let kernel = if req.server_kernel.is_empty() {
        "xray".to_string()
    } else {
        req.server_kernel
    };
    if !matches!(kernel.as_str(), "xray" | "sing-box") {
        return Err(ApiError::bad_request("入站内核只支持 xray / sing-box")
            .with_field("server_kernel", "可选 xray 或 sing-box"));
    }
    if kernel == "sing-box" && !kernels::detect(store, "sing-box").present {
        return Err(ApiError::new(422, "binary_missing", "未检测到 sing-box，无法托管该入站")
            .with_field("server_kernel", "安装 sing-box 后重试"));
    }

    {
        let state = store.snapshot();
        if let Some(other) = state
            .inbounds
            .iter()
            .find(|i| i.listen == listen && i.port == port && i.l4 == l4)
        {
            return Err(ApiError::conflict(format!(
                "端口冲突：{} 已被入站 {}（{}）占用",
                format!("{port}/{l4}"),
                other.id,
                other.remark
            ))
            .with_field("port", "换端口 / 停用占用方 / 改监听地址")
            .with_field("candidates", candidate_hint()));
        }
    }
    if !port_free(port) {
        return Err(ApiError::conflict(format!(
            "端口 {port}/tcp 在本机 127.0.0.1 已被占用，拒绝创建"
        ))
        .with_field("port", "换端口 / 停用占用方")
        .with_field("candidates", candidate_hint()));
    }

    let reality_req = req.reality.unwrap_or_default();
    let mut reality = RealityConfig::default();
    if security == "reality" {
        reality.dest = if reality_req.dest.is_empty() {
            "www.microsoft.com:443".to_string()
        } else {
            reality_req.dest.clone()
        };
        reality.server_names = if reality_req.server_names.is_empty() {
            vec![reality.dest.split(':').next().unwrap_or("www.microsoft.com").to_string()]
        } else {
            reality_req.server_names
        };
        reality.fingerprint = if reality_req.fingerprint.is_empty() {
            "chrome".to_string()
        } else {
            reality_req.fingerprint
        };
        reality.spider_x = if reality_req.spider_x.is_empty() {
            "/".to_string()
        } else {
            reality_req.spider_x
        };
        let (private_key, public_key) = generate_x25519();
        reality.private_key = private_key;
        reality.public_key = public_key;
        reality.short_ids = vec![util::random_hex(4)];
        reality.xver = 0;
    }

    let mut tls = TlsConfig::default();
    if security == "tls" {
        let t = req.tls.unwrap_or_default();
        tls.mode = if t.mode.is_empty() { "self-signed".into() } else { t.mode };
        tls.server_name = t.server_name;
        tls.alpn = vec!["h2".into(), "http/1.1".into()];
        tls.key_path = format!("state/certs/{}/privkey.pem", util::slug(&req.remark, "inbound"));
        tls.cert_path = format!("state/certs/{}/fullchain.pem", util::slug(&req.remark, "inbound"));
    }

    let client_req = req.client.unwrap_or_default();
    let email = if client_req.email.trim().is_empty() {
        format!("{}-01", util::slug(&req.remark, "client"))
    } else {
        client_req.email.trim().to_string()
    };
    let flow = if client_req.flow.is_empty() {
        if security == "reality" || security == "tls" {
            "xtls-rprx-vision".to_string()
        } else {
            String::new()
        }
    } else {
        client_req.flow
    };

    let now = util::now_iso();
    let id = util::new_id("inb");
    let tag = unique_tag(store, &protocol, &network, &security, port);

    let inbound = Inbound {
        id: id.clone(),
        tag: tag.clone(),
        remark: req.remark.trim().to_string(),
        protocol: protocol.clone(),
        listen,
        port,
        l4: l4.to_string(),
        network: network.clone(),
        security: security.clone(),
        tls,
        reality,
        server_kernel: kernel.clone(),
        server_version: String::new(),
        enabled: true,
        source: "manual".into(),
        source_ref: String::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
        clients: vec![Client {
            id: util::new_id("cli"),
            email: email.clone(),
            uuid: util::uuid_v4(),
            password: String::new(),
            flow,
            limit_ip: 0,
            quota_bytes: client_req.quota_bytes,
            used_bytes: 0,
            expires_at: None,
            enabled: true,
            disabled_reason: String::new(),
            created_at: now.clone(),
            updated_at: now.clone(),
        }],
    };

    let xray = kernels::detect(store, "xray");
    let mut warnings: Vec<String> = Vec::new();
    if security == "reality" && !inbound.reality.dest.contains(':') {
        warnings.push("dest 未包含端口，已按传入值原样保存".into());
    }
    if !xray.present {
        warnings.push("未检测到 xray 二进制：入站已记录，但服务端能力与配置未验证（不得视为已验证）".into());
    } else if !req.allow_unverified {
        warnings.push("已检测到 xray，但本里程碑尚未执行 xray -test；能力仍标未验证".into());
    }
    if network == "xhttp" {
        warnings.push("xhttp 为目标客户端高风险特性，导出后可能不可用".into());
    }

    let summary = inbound.redacted();
    store.update(|state| {
        if state.inbounds.iter().any(|i| i.remark == inbound.remark) {
            return Err(ApiError::conflict("同名入站已存在").with_field("remark", "请换一个名称"));
        }
        state.inbounds.push(inbound.clone());
        Ok(())
    })?;

    store.audit_op(
        "inbound.created",
        actor,
        ip,
        &id,
        "ok",
        serde_json::json!({
            "inbound_id": id,
            "tag": tag,
            "protocol": protocol,
            "network": network,
            "security": security,
            "port": port,
            "l4": l4,
            "client_email": email,
            "outbound_target": req.target,
            "transaction": "committed",
        }),
    );

    Ok(serde_json::json!({
        "committed": true,
        "item": summary,
        "warnings": warnings,
        "transaction": {
            "steps": [
                {"step": 1, "status": "done", "note": "协议与目标客户端已确认"},
                {"step": 2, "status": "done", "note": "端口在本机回环可用，未写防火墙"},
                {"step": 3, "status": "done", "note": "Reality 密钥对已生成；私钥仅落盘"},
                {"step": 4, "status": "done", "note": "TLS 未启用或配置为自签占位"},
                {"step": 5, "status": "skipped", "note": "防火墙规则未写入（需用户显式确认，M1 不改宿主）"},
                {"step": 6, "status": "done", "note": "默认客户端已创建"}
            ],
            "rollback_inventory": {
                "port_reservations": 0,
                "temp_private_keys": 0,
                "firewall_rules": 0,
                "candidate_configs": 0
            }
        }
    }))
}

fn unique_tag(store: &Store, protocol: &str, network: &str, security: &str, port: u16) -> String {
    let base = format!("{protocol}-{network}-{security}-{port}");
    let state = store.snapshot();
    let mut candidate = base.clone();
    let mut n = 1;
    while state.inbounds.iter().any(|i| i.tag == candidate) {
        n += 1;
        candidate = format!("{base}-{n}");
    }
    candidate
}

pub(crate) fn generate_x25519() -> (String, String) {
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).expect("系统随机源不可用");
    let secret = x25519_dalek::StaticSecret::from(bytes);
    let public = x25519_dalek::PublicKey::from(&secret);
    let engine = base64::engine::general_purpose::URL_SAFE_NO_PAD;
    (
        engine.encode(secret.to_bytes()),
        engine.encode(public.to_bytes()),
    )
}

#[derive(Debug, Deserialize)]
pub struct CreateClientReq {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub quota_bytes: u64,
    #[serde(default)]
    pub limit_ip: u32,
    #[serde(default)]
    pub expires_at: Option<String>,
}

pub fn create_client(
    store: &Store,
    inbound_id: &str,
    req: CreateClientReq,
    actor: &str,
    ip: &str,
) -> ApiResult<serde_json::Value> {
    let state = store.snapshot();
    let inbound = state
        .inbounds
        .iter()
        .find(|i| i.id == inbound_id)
        .ok_or_else(|| ApiError::not_found(format!("入站 {inbound_id} 不存在")))?;
    let email = req.email.trim();
    if email.is_empty() {
        return Err(ApiError::bad_request("客户端 email 必填").with_field("email", "入站内唯一"));
    }
    if inbound.clients.iter().any(|c| c.email == email) {
        return Err(ApiError::conflict("同一入站内 email 必须唯一").with_field("email", "已存在"));
    }
    let now = util::now_iso();
    let client = Client {
        id: util::new_id("cli"),
        email: email.to_string(),
        uuid: util::uuid_v4(),
        password: String::new(),
        flow: inbound.clients.first().map(|c| c.flow.clone()).unwrap_or_default(),
        limit_ip: req.limit_ip,
        quota_bytes: req.quota_bytes,
        used_bytes: 0,
        expires_at: req.expires_at,
        enabled: true,
        disabled_reason: String::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
    };
    let client_id = client.id.clone();
    store.update(|st| {
        let target = st
            .inbounds
            .iter_mut()
            .find(|i| i.id == inbound_id)
            .ok_or_else(|| ApiError::not_found("入站不存在"))?;
        if target.clients.iter().any(|c| c.email == email) {
            return Err(ApiError::conflict("email 已存在"));
        }
        target.clients.push(client.clone());
        target.updated_at = util::now_iso();
        Ok(())
    })?;
    store.audit_op(
        "inbound.client.created",
        actor,
        ip,
        &format!("{inbound_id}/{client_id}"),
        "ok",
        serde_json::json!({"inbound_id": inbound_id, "client_id": client_id, "email": email}),
    );
    Ok(serde_json::json!({
        "id": client.id,
        "email": client.email,
        "uuid": client.uuid,
        "flow": client.flow,
        "quota_bytes": client.quota_bytes,
        "expires_at": client.expires_at,
        "enabled": client.enabled
    }))
}
