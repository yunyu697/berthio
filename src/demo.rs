use crate::model::{Client, Inbound, RealityConfig, State, TlsConfig, TokenRecord};
use crate::util;
use crate::wizard::generate_x25519;

fn client(email: &str, flow: &str, quota_gb: u64, now: &str) -> Client {
    Client {
        id: util::new_id("cli"),
        email: email.to_string(),
        uuid: util::uuid_v4(),
        password: String::new(),
        flow: flow.to_string(),
        limit_ip: 0,
        quota_bytes: quota_gb * 1024 * 1024 * 1024,
        used_bytes: 0,
        expires_at: None,
        enabled: true,
        disabled_reason: String::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
    }
}

fn reality_inbound(
    remark: &str,
    tag: &str,
    port: u16,
    listen: &str,
    clients: Vec<Client>,
    enabled: bool,
    now: &str,
) -> Inbound {
    let (private_key, public_key) = generate_x25519();
    Inbound {
        id: util::new_id("inb"),
        tag: tag.to_string(),
        remark: remark.to_string(),
        protocol: "vless".into(),
        listen: listen.to_string(),
        port,
        l4: "tcp".into(),
        network: "tcp".into(),
        security: "reality".into(),
        tls: TlsConfig::default(),
        reality: RealityConfig {
            dest: "www.microsoft.com:443".into(),
            xver: 0,
            server_names: vec!["www.microsoft.com".into()],
            short_ids: vec![util::random_hex(4)],
            public_key,
            private_key,
            fingerprint: "chrome".into(),
            spider_x: "/".into(),
        },
        server_kernel: "xray".into(),
        server_version: String::new(),
        enabled,
        source: "demo".into(),
        source_ref: String::new(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        clients,
    }
}

/// 演示数据：只用于 `--demo` 与静态 demo，不写入任何真实环境。
pub fn state() -> State {
    let now = util::now_iso();
    let mut inbounds = Vec::new();

    inbounds.push(reality_inbound(
        "日本 · 东京",
        "vless-tcp-reality-443",
        443,
        "0.0.0.0",
        vec![
            client("alice@demo", "xtls-rprx-vision", 200, &now),
            client("bob@demo", "xtls-rprx-vision", 100, &now),
        ],
        true,
        &now,
    ));

    let mut hk = Inbound {
        id: util::new_id("inb"),
        tag: "vless-ws-tls-8443".into(),
        remark: "香港 · 中转".into(),
        protocol: "vless".into(),
        listen: "0.0.0.0".into(),
        port: 8443,
        l4: "tcp".into(),
        network: "ws".into(),
        security: "tls".into(),
        tls: TlsConfig {
            mode: "self-signed".into(),
            server_name: "hk.demo.invalid".into(),
            cert_path: "state/certs/hk/fullchain.pem".into(),
            key_path: "state/certs/hk/privkey.pem".into(),
            alpn: vec!["h2".into(), "http/1.1".into()],
        },
        reality: RealityConfig::default(),
        server_kernel: "xray".into(),
        server_version: String::new(),
        enabled: true,
        source: "demo".into(),
        source_ref: String::new(),
        created_at: now.clone(),
        updated_at: now.clone(),
        clients: vec![client("carol@demo", "xtls-rprx-vision", 50, &now)],
    };
    hk.updated_at = now.clone();
    inbounds.push(hk);

    let sg = reality_inbound(
        "新加坡 · 备用",
        "vless-tcp-reality-2053",
        2053,
        "0.0.0.0",
        vec![client("dave@demo", "xtls-rprx-vision", 0, &now)],
        false,
        &now,
    );
    inbounds.push(sg);

    // 一条已吊销的历史 token，演示审计与吊销状态。
    let token_hash = util::sha256_hex(b"demo-revoked-token");
    let tokens = vec![TokenRecord {
        id: util::new_id("tok"),
        client_id: inbounds[0].clients[0].id.clone(),
        token_hash,
        target: "clash".into(),
        created_at: now.clone(),
        revoked_at: Some(now.clone()),
        last_access: None,
    }];

    State {
        schema_version: 1,
        panel: crate::model::PanelState {
            panel_address: "demo.berthio.example".into(),
            created_at: now,
        },
        inbounds,
        tokens,
    }
}
