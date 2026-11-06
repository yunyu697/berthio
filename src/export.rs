use crate::error::{ApiError, ApiResult};
use crate::kernels;
use crate::model::{Client, Inbound};
use crate::rulesets;
use crate::state::Store;
use crate::util;
use serde_json::{json, Map, Value};
use std::process::Command;

pub struct Selection {
    pub format: String,
    pub scenario: String,
    pub bucket: String,
    pub inbound_ids: Vec<String>,
    pub client_ids: Vec<String>,
    pub rules: Vec<(String, String)>,
}

pub struct Rendered {
    pub content: String,
    pub content_hash: String,
    pub render_id: String,
    pub file_name: String,
    pub entries: usize,
    pub nodes: Vec<Value>,
    pub sections: Value,
    pub validation: Value,
    pub warnings: Vec<String>,
    pub format: String,
    pub scenario: String,
    pub bucket: String,
}

fn bucket_built(bucket: &str) -> bool {
    matches!(bucket, "111" | "112" | "113" | "114")
}

fn action_tag(action: &str) -> &'static str {
    match action {
        "direct" => "DIRECT",
        "reject" => "REJECT",
        _ => "节点选择",
    }
}

fn sb_action_tag(action: &str) -> &'static str {
    match action {
        "direct" => "direct",
        "reject" => "block",
        _ => "节点选择",
    }
}

fn rule_tag(id: &str) -> String {
    id.replace(':', "-").replace('!', "-not")
}

fn effective_rules(sel: &Selection) -> Vec<(String, String)> {
    if !sel.rules.is_empty() {
        return sel.rules.clone();
    }
    let preset = rulesets::preset_defs();
    if let Some(defs) = preset.as_array() {
        if let Some(daily) = defs.iter().find(|d| d["id"] == "daily") {
            if let Some(items) = daily["items"].as_array() {
                return items
                    .iter()
                    .filter_map(|i| {
                        Some((
                            i["id"].as_str()?.to_string(),
                            i["action"].as_str()?.to_string(),
                        ))
                    })
                    .collect();
            }
        }
    }
    Vec::new()
}

fn selected_entries(state: &crate::model::State, sel: &Selection) -> Vec<(Inbound, Client)> {
    let mut out = Vec::new();
    for inbound in &state.inbounds {
        if !sel.inbound_ids.is_empty() && !sel.inbound_ids.contains(&inbound.id) {
            continue;
        }
        for client in &inbound.clients {
            if !sel.client_ids.is_empty() && !sel.client_ids.contains(&client.id) {
                continue;
            }
            if client.enabled && inbound.enabled {
                out.push((inbound.clone(), client.clone()));
            }
        }
    }
    out
}

fn entry_name(inbound: &Inbound, client: &Client) -> String {
    format!("{} / {}", inbound.remark, client.email)
}

pub fn render(store: &Store, sel: Selection) -> ApiResult<Rendered> {
    let state = store.snapshot();
    let mut warnings: Vec<String> = Vec::new();
    let mut l0: Vec<Value> = Vec::new();
    let mut l1: Vec<Value> = Vec::new();

    // xhttp 实测（1.0.0）：xray 25.6.8 客户端可连；sing-box 1.14.4 直接拒绝 xhttp transport；
    // mihomo 1.19.33 的 -t 能过但真实连接失败。订阅只输出 mihomo / sing-box，因此运行期直接剔除，
    // 避免下发「配置自检通过但连不上」的节点。
    let mut entries = Vec::new();
    let mut xhttp_skipped = Vec::new();
    for (inbound, client) in selected_entries(&state, &sel) {
        if inbound.network == "xhttp" {
            xhttp_skipped.push(format!("{} / {}", inbound.remark, client.email));
        } else {
            entries.push((inbound, client));
        }
    }
    let rules = effective_rules(&sel);
    let catalog = rulesets::catalog();

    if !bucket_built(&sel.bucket) {
        l1.push(json!({"code":"bucket_not_built","level":"fail","message":format!("版本档 {} 未建档，禁止导出", sel.bucket),"fix":"选择 1.11.x–1.14.x"}));
    }
    if sel.inbound_ids.is_empty() {
        l0.push(json!({"code":"no_inbound_selected","level":"fail","message":"未选择入站","fix":"在节点多选中至少选 1 个客户端"}));
    }
    if !xhttp_skipped.is_empty() {
        let detail = xhttp_skipped.join("、");
        warnings.push(format!(
            "已跳过 {} 个 xhttp 客户端（{detail}）：xray 客户端可用，但 sing-box 1.14.4 拒绝 xhttp（unknown transport type），mihomo 1.19.33 自检通过却真实连接失败",
            xhttp_skipped.len()
        ));
    }
    if entries.is_empty() {
        if !xhttp_skipped.is_empty() {
            l0.push(json!({"code":"xhttp_unsupported","level":"fail","message":"所选客户端全部是 xhttp，目标客户端均不可用，未生成订阅","fix":"改用 ws / tcp 传输，或用 xray 客户端直连"}));
        } else {
            l0.push(json!({"code":"no_entries","level":"fail","message":"所选入站下没有可导出的启用客户端","fix":"检查客户端是否被停用，或重新选择"}));
        }
    }
    for (inbound, client) in &entries {
        if client.uuid.is_empty() {
            l0.push(json!({"code":"client_uuid_missing","level":"fail","message":format!("客户端 {} 缺少 UUID", client.email),"fix":"重新生成客户端"}));
        }
        if inbound.security == "reality" {
            if inbound.reality.public_key.is_empty() || inbound.reality.short_ids.is_empty() {
                l1.push(json!({"code":"reality_incomplete","level":"fail","message":format!("入站 {} 的 Reality 公钥 / shortId 缺失", inbound.remark),"fix":"重新生成 Reality 参数"}));
            }
            if inbound.reality.server_names.is_empty() {
                l1.push(json!({"code":"reality_sni_missing","level":"fail","message":format!("入站 {} 缺少 serverNames", inbound.remark),"fix":"填写 dest / serverNames"}));
            }
        }
    }
    for (id, action) in &rules {
        if !catalog.iter().any(|r| &r.id == id) {
            l1.push(json!({"code":"ruleset_unknown","level":"fail","message":format!("规则集 {id} 不在 40 条库内"),"fix":"从规则集列表选择"}));
        }
        if !matches!(action.as_str(), "direct" | "proxy" | "reject" | "off") {
            l1.push(json!({"code":"ruleset_action_invalid","level":"fail","message":format!("规则集 {id} 的动作 {action} 非法"),"fix":"直连 / 代理 / 拒绝 / 不启用"}));
        }
    }
    let panel_address = state.panel.panel_address.trim().to_string();
    if panel_address.is_empty() {
        l1.push(json!({"code":"panel_address_missing","level":"fail","message":"未设置面板对外地址，导出 server 字段无值","fix":"在设置页填写域名或公网 IP"}));
    }
    if sel.format == "clash" && sel.bucket.starts_with("11") {
        // 版本档在本项目里主要约束 sing-box；mihomo 侧只做语法校验。
    }

    let nodes: Vec<Value> = entries
        .iter()
        .map(|(inbound, client)| node_entry(inbound, client, &panel_address, &sel))
        .collect();

    let (content, sections) = if sel.format == "singbox" {
        render_singbox(&sel, &state, &nodes, &rules, &panel_address)?
    } else {
        render_mihomo(&sel, &state, &nodes, &rules, &panel_address)?
    };

    let content_hash = util::sha256_hex(content.as_bytes());
    let render_id = format!("rid_{}", &content_hash[0..12]);
    let ext = if sel.format == "singbox" { "json" } else { "yaml" };
    let file_name = format!("berthio-{}-{}.{}", sel.scenario, sel.bucket, ext);

    // 落盘候选配置，供排障与后续审计（不参与内核启停）。
    let out_dir = store.home_path("configs/generated/export");
    let _ = std::fs::create_dir_all(&out_dir);
    let _ = util::write_atomic(
        &out_dir.join(format!("{render_id}.{ext}")),
        content.as_bytes(),
    );

    let l2 = run_binary_check(store, &sel.format, &render_id, ext, &content);
    // L2 是逐请求的二进制自检：二进制缺失=UNVERIFIED（WARN），
    // 二进制在但拒绝该配置=FAIL（与 L0/L1 一样直接判 FAIL，订阅/下载必须拒绝）。
    let overall = if l0.iter().any(|i| i["level"] == "fail")
        || l1.iter().any(|i| i["level"] == "fail")
        || l2["status"] == "FAIL"
    {
        "FAIL"
    } else if sel.format == "singbox" && !bucket_built(&sel.bucket) {
        "FAIL"
    } else if l2["status"] == "PASS" {
        if warnings.is_empty() { "PASS" } else { "WARN" }
    } else {
        "WARN"
    };

    let validation = json!({
        "overall": overall,
        "render_id": render_id,
        "content_hash": content_hash,
        "layers": [
            {"layer":"L0","label":"结构 / 引用完整性","status": if l0.iter().any(|i| i["level"]=="fail") {"FAIL"} else {"PASS"},"items": l0},
            {"layer":"L1","label":"枚举 / 版本门禁","status": if l1.iter().any(|i| i["level"]=="fail") {"FAIL"} else {"PASS"},"items": l1},
            l2
        ],
        "binary_note": "只有实际执行 mihomo -t / sing-box check 且通过才是 PASS；二进制缺失时为 UNVERIFIED，禁止写成已验证"
    });

    Ok(Rendered {
        content,
        content_hash,
        render_id,
        file_name,
        entries: entries.len(),
        nodes,
        sections,
        validation,
        warnings,
        format: sel.format,
        scenario: sel.scenario,
        bucket: sel.bucket,
    })
}

fn node_entry(
    inbound: &Inbound,
    client: &Client,
    panel_address: &str,
    sel: &Selection,
) -> Value {
    json!({
        "inbound_id": inbound.id,
        "client_id": client.id,
        "name": entry_name(inbound, client),
        "remark": inbound.remark,
        "email": client.email,
        "protocol": inbound.protocol,
        "network": inbound.network,
        "security": inbound.security,
        "server": panel_address,
        "port": inbound.port,
        "uuid": client.uuid,
        "flow": client.flow,
        "sni": if inbound.security == "reality" { inbound.reality.server_names.first().cloned().unwrap_or_default() } else { inbound.tls.server_name.clone() },
        "public_key": inbound.reality.public_key,
        "short_id": inbound.reality.short_ids.first().cloned().unwrap_or_default(),
        "fingerprint": if inbound.reality.fingerprint.is_empty() { "chrome".to_string() } else { inbound.reality.fingerprint.clone() },
        "format": sel.format,
        "xhttp": inbound.network == "xhttp",
        "_warnings": if inbound.network == "xhttp" { vec!["xhttp 高风险"] } else { Vec::<&str>::new() },
    })
}

fn mihomo_proxy(node: &Value) -> Value {
    let mut p = Map::new();
    p.insert("name".into(), node["name"].clone());
    p.insert("type".into(), json!("vless"));
    p.insert("server".into(), node["server"].clone());
    p.insert("port".into(), node["port"].clone());
    p.insert("uuid".into(), node["uuid"].clone());
    p.insert("network".into(), node["network"].clone());
    p.insert("udp".into(), json!(true));
    p.insert("tls".into(), json!(true));
    let sni = node["sni"].as_str().unwrap_or("");
    if !sni.is_empty() {
        p.insert("servername".into(), json!(sni));
    }
    let flow = node["flow"].as_str().unwrap_or("");
    if !flow.is_empty() {
        p.insert("flow".into(), json!(flow));
    }
    let fp = node["fingerprint"].as_str().unwrap_or("chrome");
    p.insert("client-fingerprint".into(), json!(fp));
    if node["security"] == "reality" {
        p.insert(
            "reality-opts".into(),
            json!({
                "public-key": node["public_key"],
                "short-id": node["short_id"],
            }),
        );
    } else {
        // 自签证书在客户端侧只能跳过校验；不伪造 CA。
        p.insert("skip-cert-verify".into(), json!(true));
    }
    if node["network"] == "ws" {
        let host = if sni.is_empty() { "" } else { sni };
        p.insert(
            "ws-opts".into(),
            json!({"path": "/", "headers": {"Host": host}}),
        );
    }
    Value::Object(p)
}

fn render_mihomo(
    sel: &Selection,
    state: &crate::model::State,
    nodes: &[Value],
    rules: &[(String, String)],
    panel_address: &str,
) -> ApiResult<(String, Value)> {
    let names: Vec<String> = nodes
        .iter()
        .map(|n| n["name"].as_str().unwrap_or("").to_string())
        .collect();
    let scenario = sel.scenario.as_str();

    let mut root = Map::new();
    root.insert("log-level".into(), json!(if scenario == "desktop" { "info" } else { "warning" }));
    root.insert("mixed-port".into(), json!(7890));
    root.insert("allow-lan".into(), json!(scenario != "desktop"));
    root.insert("mode".into(), json!("rule"));
    root.insert("ipv6".into(), json!(scenario == "desktop"));
    root.insert("unified-delay".into(), json!(true));

    let dns = if scenario == "side" {
        json!({"enable": false})
    } else if scenario == "router" {
        json!({
            "enable": true,
            "listen": "0.0.0.0:1053",
            "enhanced-mode": "fake-ip",
            "fake-ip-range": "198.18.0.1/16",
            "nameserver": ["223.5.5.5", "1.1.1.1"]
        })
    } else {
        json!({
            "enable": true,
            "listen": "0.0.0.0:1053",
            "enhanced-mode": "normal",
            "nameserver": ["223.5.5.5", "1.1.1.1"]
        })
    };
    root.insert("dns".into(), dns);

    let proxies: Vec<Value> = nodes.iter().map(mihomo_proxy).collect();
    root.insert("proxies".into(), Value::Array(proxies.clone()));

    let mut members = names.clone();
    members.push("DIRECT".into());
    root.insert(
        "proxy-groups".into(),
        json!([{"name":"节点选择","type":"select","proxies": members}]),
    );

    let catalog = rulesets::catalog();
    let mut providers = Map::new();
    let mut rule_lines: Vec<String> = Vec::new();
    for (id, action) in rules {
        if action == "off" {
            continue;
        }
        let Some(rs) = catalog.iter().find(|r| &r.id == id) else {
            continue;
        };
        let tag = rule_tag(id);
        let behavior = if rs.kind == "geoip" { "ipcidr" } else { "domain" };
        providers.insert(
            tag.clone(),
            json!({
                "type": "http",
                "behavior": behavior,
                "url": rulesets::mihomo_url(rs),
                "path": format!("./ruleset/{tag}.yaml"),
                "interval": 86400
            }),
        );
        rule_lines.push(format!("RULE-SET,{tag},{}", action_tag(action)));
    }
    if !providers.is_empty() {
        root.insert("rule-providers".into(), Value::Object(providers));
    }
    rule_lines.push("MATCH,节点选择".into());
    root.insert("rules".into(), json!(rule_lines));

    if scenario == "side" {
        root.insert(
            "tun".into(),
            json!({"enable": false}),
        );
    } else {
        root.insert(
            "tun".into(),
            json!({
                "enable": true,
                "stack": if scenario == "router" { "system" } else { "mixed" },
                "auto-route": true,
                "auto-detect-interface": true,
                "dns-hijack": ["any:53"]
            }),
        );
    }
    root.insert("experimental".into(), json!({"sniffer": {"enable": true}}));

    let value = Value::Object(root);
    let content = serde_yaml::to_string(&value)
        .map_err(|e| ApiError::internal(format!("YAML 序列化失败: {e}")))?;
    let sections = json!({
        "nodes": serde_yaml::to_string(&json!({"proxies": proxies})).unwrap_or_default(),
        "dns": serde_yaml::to_string(&json!({"dns": value.get("dns").cloned().unwrap_or(Value::Null)})).unwrap_or_default(),
        "rules": serde_yaml::to_string(&json!({
            "rule-providers": value.get("rule-providers").cloned().unwrap_or(Value::Null),
            "rules": rule_lines
        })).unwrap_or_default(),
        "other": serde_yaml::to_string(&json!({
            "log-level": value.get("log-level"),
            "mixed-port": value.get("mixed-port"),
            "allow-lan": value.get("allow-lan"),
            "tun": value.get("tun"),
            "mode": value.get("mode"),
            "ipv6": value.get("ipv6"),
            "unified-delay": value.get("unified-delay"),
            "experimental": value.get("experimental"),
        })).unwrap_or_default()
    });
    let _ = state;
    let _ = panel_address;
    Ok((content, sections))
}

fn singbox_outbound(node: &Value) -> Value {
    let mut o = Map::new();
    o.insert("type".into(), json!("vless"));
    o.insert("tag".into(), node["name"].clone());
    o.insert("server".into(), node["server"].clone());
    o.insert("server_port".into(), node["port"].clone());
    o.insert("uuid".into(), node["uuid"].clone());
    let flow = node["flow"].as_str().unwrap_or("");
    if !flow.is_empty() {
        o.insert("flow".into(), json!(flow));
    }
    let sni = node["sni"].as_str().unwrap_or("");
    let mut tls = Map::new();
    tls.insert("enabled".into(), json!(true));
    if !sni.is_empty() {
        tls.insert("server_name".into(), json!(sni));
    }
    tls.insert(
        "utls".into(),
        json!({"enabled": true, "fingerprint": node["fingerprint"]}),
    );
    if node["security"] == "reality" {
        tls.insert(
            "reality".into(),
            json!({
                "enabled": true,
                "public_key": node["public_key"],
                "short_id": node["short_id"],
            }),
        );
    } else {
        tls.insert("insecure".into(), json!(true));
    }
    o.insert("tls".into(), Value::Object(tls));
    if node["network"] == "ws" {
        o.insert(
            "transport".into(),
            json!({"type": "ws", "path": "/", "headers": {"Host": sni}}),
        );
    } else if node["network"] == "xhttp" {
        o.insert("transport".into(), json!({"type": "xhttp", "path": "/"}));
    }
    Value::Object(o)
}

fn render_singbox(
    sel: &Selection,
    state: &crate::model::State,
    nodes: &[Value],
    rules: &[(String, String)],
    _panel_address: &str,
) -> ApiResult<(String, Value)> {
    let scenario = sel.scenario.as_str();
    let names: Vec<String> = nodes
        .iter()
        .map(|n| n["name"].as_str().unwrap_or("").to_string())
        .collect();
    let mut outbounds: Vec<Value> = nodes.iter().map(singbox_outbound).collect();
    let mut selector_members = names.clone();
    selector_members.push("direct".into());
    outbounds.push(json!({"type":"selector","tag":"节点选择","outbounds":selector_members,"default":"节点选择"}));
    outbounds.push(json!({"type":"direct","tag":"direct"}));
    outbounds.push(json!({"type":"block","tag":"block"}));

    let inbounds = if scenario == "side" {
        json!([{"type":"mixed","tag":"mixed-in","listen":"127.0.0.1","listen_port":7890}])
    } else {
        json!([
            {"type":"tun","tag":"tun-in","address":["172.19.0.1/30"],"auto_route":true,"strict_route":scenario=="desktop","stack":if scenario=="router" {"system"} else {"mixed"}},
            {"type":"mixed","tag":"mixed-in","listen":"127.0.0.1","listen_port":7890}
        ])
    };

    let dns = if scenario == "side" {
        json!({"servers":[{"tag":"local","type":"udp","server":"223.5.5.5"}],"final":"local"})
    } else if scenario == "router" {
        json!({"servers":[{"tag":"local","type":"udp","server":"223.5.5.5"}],"final":"local","independent_cache":true})
    } else {
        json!({"servers":[
            {"tag":"local","type":"udp","server":"223.5.5.5"},
            {"tag":"remote","type":"https","server":"1.1.1.1","detour":"节点选择"}
        ],"final":"local"})
    };

    let catalog = rulesets::catalog();
    let mut rule_set = Vec::new();
    let mut route_rules = Vec::new();
    for (id, action) in rules {
        if action == "off" {
            continue;
        }
        let Some(rs) = catalog.iter().find(|r| &r.id == id) else {
            continue;
        };
        let tag = rule_tag(id);
        rule_set.push(json!({
            "tag": tag,
            "type": "remote",
            "format": "binary",
            "url": rulesets::singbox_url(rs)
        }));
        route_rules.push(json!({"rule_set": tag, "outbound": sb_action_tag(action)}));
    }

    // sing-box 1.12.0 起缺少 route.default_domain_resolver 会在 1.14.0 变成硬错误（实测 1.14.4 check 报 ERROR）
    let mut route = json!({
        "rules": route_rules,
        "rule_set": rule_set,
        "final": "节点选择",
        "auto_detect_interface": true
    });
    let bucket_num: u16 = sel.bucket.parse().unwrap_or(112);
    if bucket_num >= 112 {
        route["default_domain_resolver"] = json!({"server": "local"});
    }

    let root = json!({
        "log": {"level": if scenario=="desktop" {"info"} else {"warn"}, "timestamp": true},
        "dns": dns,
        "inbounds": inbounds,
        "outbounds": outbounds,
        "route": route,
        "experimental": {
            "cache_file": {"enabled": scenario != "side", "path": "cache.db"}
        }
    });
    let _ = state;
    let content = serde_json::to_string_pretty(&root)
        .map_err(|e| ApiError::internal(format!("JSON 序列化失败: {e}")))?;
    let sections = json!({
        "nodes": serde_json::to_string_pretty(&json!({"outbounds": nodes})).unwrap_or_default(),
        "dns": serde_json::to_string_pretty(&json!({"dns": root.get("dns").cloned().unwrap_or(Value::Null)})).unwrap_or_default(),
        "rules": serde_json::to_string_pretty(&json!({"route": root.get("route").cloned().unwrap_or(Value::Null)})).unwrap_or_default(),
        "other": serde_json::to_string_pretty(&json!({
            "log": root.get("log"),
            "inbounds": root.get("inbounds"),
            "experimental": root.get("experimental"),
        })).unwrap_or_default()
    });
    Ok((content, sections))
}

fn run_binary_check(store: &Store, format: &str, rid: &str, ext: &str, content: &str) -> Value {
    let kernel = if format == "singbox" { "sing-box" } else { "mihomo" };
    let info = kernels::detect(store, kernel);
    if !info.present {
        return json!({
            "layer":"L2",
            "label":"对应版本档的二进制",
            "status":"UNVERIFIED",
            "binary": kernel,
            "version":"",
            "command":"",
            "output":"未找到二进制：未执行二进制校验，不得写成已验证",
            "verified": false
        });
    }
    let path = store.tmp_path(&format!("{rid}.{ext}"));
    if let Err(e) = std::fs::write(&path, content) {
        return json!({
            "layer":"L2","status":"UNVERIFIED","binary":kernel,
            "output":format!("写临时配置失败: {e}"),"verified":false
        });
    }
    let (cmd, args) = if format == "singbox" {
        ("sing-box", vec!["check".to_string(), "-c".to_string(), path.display().to_string()])
    } else {
        ("mihomo", vec!["-t".to_string(), "-f".to_string(), path.display().to_string()])
    };
    let output = Command::new(&info.path).args(&args).output();
    if let Some(parent) = path.parent() {
        let _ = crate::safety::remove_file(&path, &[parent.to_path_buf()]);
    }
    match output {
        Ok(o) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            let ok = o.status.success();
            json!({
                "layer":"L2",
                "label":"对应版本档的二进制",
                "status": if ok {"PASS"} else {"FAIL"},
                "binary": kernel,
                "version": info.version,
                "command": format!("{} {}", cmd, args.join(" ")),
                "output": truncate(text.trim(), 800),
                "verified": ok
            })
        }
        Err(e) => json!({
            "layer":"L2","label":"对应版本档的二进制","status":"UNVERIFIED",
            "binary": kernel,"version": info.version,
            "command": format!("{} {}", cmd, args.join(" ")),
            "output": format!("执行失败: {e}"),"verified": false
        }),
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

pub fn render_payload(store: &Store, sel: Selection) -> ApiResult<Value> {
    let r = render(store, sel)?;
    Ok(json!({
        "render_id": r.render_id,
        "content_hash": r.content_hash,
        "file_name": r.file_name,
        "format": r.format,
        "scenario": r.scenario,
        "bucket": r.bucket,
        "bytes": r.content.len(),
        "entries": r.entries,
        "nodes": r.nodes,
        "content": r.content,
        "sections": r.sections,
        "validation": r.validation,
        "warnings": r.warnings,
    }))
}
