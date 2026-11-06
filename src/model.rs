use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelState {
    #[serde(default)]
    pub panel_address: String,
    #[serde(default)]
    pub created_at: String,
}

impl Default for PanelState {
    fn default() -> Self {
        Self {
            panel_address: String::new(),
            created_at: String::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TlsConfig {
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub server_name: String,
    #[serde(default)]
    pub cert_path: String,
    #[serde(default)]
    pub key_path: String,
    #[serde(default)]
    pub alpn: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RealityConfig {
    #[serde(default)]
    pub dest: String,
    #[serde(default)]
    pub xver: u8,
    #[serde(default)]
    pub server_names: Vec<String>,
    #[serde(default)]
    pub short_ids: Vec<String>,
    #[serde(default)]
    pub public_key: String,
    /// 私钥只落盘，绝不进入任何 API 响应。
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub fingerprint: String,
    #[serde(default)]
    pub spider_x: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Client {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub uuid: String,
    #[serde(default)]
    pub password: String,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub limit_ip: u32,
    #[serde(default)]
    pub quota_bytes: u64,
    #[serde(default)]
    pub used_bytes: u64,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub disabled_reason: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Inbound {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub tag: String,
    #[serde(default)]
    pub remark: String,
    #[serde(default)]
    pub protocol: String,
    #[serde(default)]
    pub listen: String,
    #[serde(default)]
    pub port: u16,
    #[serde(default)]
    pub l4: String,
    #[serde(default)]
    pub network: String,
    #[serde(default)]
    pub security: String,
    #[serde(default)]
    pub tls: TlsConfig,
    #[serde(default)]
    pub reality: RealityConfig,
    #[serde(default)]
    pub server_kernel: String,
    #[serde(default)]
    pub server_version: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub source_ref: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub clients: Vec<Client>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRecord {
    pub id: String,
    pub client_id: String,
    pub token_hash: String,
    pub target: String,
    pub created_at: String,
    #[serde(default)]
    pub revoked_at: Option<String>,
    #[serde(default)]
    pub last_access: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct State {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub panel: PanelState,
    #[serde(default)]
    pub inbounds: Vec<Inbound>,
    #[serde(default)]
    pub tokens: Vec<TokenRecord>,
}

fn default_schema() -> u32 {
    SCHEMA_VERSION
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            panel: PanelState::default(),
            inbounds: Vec::new(),
            tokens: Vec::new(),
        }
    }
}

impl Inbound {
    /// 对外展示的脱敏版本：不含私钥、证书私钥路径。
    pub fn redacted(&self) -> serde_json::Value {
        serde_json::json!({
            "id": self.id,
            "tag": self.tag,
            "remark": self.remark,
            "protocol": self.protocol,
            "listen": self.listen,
            "port": self.port,
            "l4": self.l4,
            "network": self.network,
            "security": self.security,
            "server_kernel": self.server_kernel,
            "server_version": self.server_version,
            "enabled": self.enabled,
            "source": self.source,
            "source_ref": self.source_ref,
            "created_at": self.created_at,
            "updated_at": self.updated_at,
            "tls": {
                "mode": self.tls.mode,
                "server_name": self.tls.server_name,
                "alpn": self.tls.alpn,
            },
            "reality": {
                "dest": self.reality.dest,
                "xver": self.reality.xver,
                "server_names": self.reality.server_names,
                "short_ids": self.reality.short_ids,
                "public_key": self.reality.public_key,
                "fingerprint": self.reality.fingerprint,
                "spider_x": self.reality.spider_x,
                "private_key": "••••（仅落盘，不通过 API 返回）",
            },
            "clients": self.clients.iter().map(|c| serde_json::json!({
                "id": c.id,
                "email": c.email,
                "uuid": c.uuid,
                "flow": c.flow,
                "limit_ip": c.limit_ip,
                "quota_bytes": c.quota_bytes,
                "used_bytes": c.used_bytes,
                "expires_at": c.expires_at,
                "enabled": c.enabled,
                "disabled_reason": c.disabled_reason,
            })).collect::<Vec<_>>(),
        })
    }
}
