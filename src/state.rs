use crate::error::{ApiError, ApiResult};
use crate::model::{State, SCHEMA_VERSION};
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:18080";

/// config/panel.json 的 schema 版本。
///
/// - v1：alpha 系列。
/// - v2：1.0.0。监听地址/日志保留天数/可信代理做一次规范化；
///   旧版本启动时自动迁移并留备份。
pub const CONFIG_SCHEMA_VERSION: u32 = 2;

/// 一次文件级 schema 升级的记录（启动时打印并写入审计）。
#[derive(Debug, Clone)]
pub struct UpgradeNote {
    pub kind: &'static str,
    pub from: u32,
    pub to: u32,
    pub backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RemoteAccess {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub acknowledged_at: String,
    #[serde(default)]
    pub acknowledged_by: String,
    #[serde(default)]
    pub trusted_proxies: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateMirror {
    pub id: String,
    pub prefix: String,
}

/// 自更新配置。`pubkey` 为空时用内置项目公钥；显式配置可覆盖（自建源/派生版本）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpdateConfig {
    #[serde(default = "default_update_feed")]
    pub feed: String,
    /// `auto`（官方 → 镜像）或镜像 id（该镜像 → 官方 → 其余镜像）。
    #[serde(default = "default_update_mirror")]
    pub mirror: String,
    /// base64(minisign 公钥文件全文)。
    #[serde(default)]
    pub pubkey: String,
    #[serde(default = "default_update_mirrors")]
    pub mirrors: Vec<UpdateMirror>,
}

fn default_update_feed() -> String {
    crate::update::DEFAULT_FEED.to_string()
}
fn default_update_mirror() -> String {
    "auto".to_string()
}
fn default_update_mirrors() -> Vec<UpdateMirror> {
    crate::update::DEFAULT_MIRRORS
        .iter()
        .map(|(id, prefix)| UpdateMirror {
            id: id.to_string(),
            prefix: prefix.to_string(),
        })
        .collect()
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            feed: default_update_feed(),
            mirror: default_update_mirror(),
            pubkey: String::new(),
            mirrors: default_update_mirrors(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PanelConfig {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default = "default_listen")]
    pub listen: String,
    /// 首次引导写入；导出 server 字段取它。
    #[serde(default)]
    pub panel_address: String,
    #[serde(default)]
    pub log_keep_days: u32,
    /// 远程监听必须显式确认；未确认时强制回环。
    #[serde(default)]
    pub remote_access: RemoteAccess,
    #[serde(default)]
    pub update: UpdateConfig,
}

fn default_schema() -> u32 {
    CONFIG_SCHEMA_VERSION
}
fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            schema_version: CONFIG_SCHEMA_VERSION,
            listen: DEFAULT_LISTEN.to_string(),
            panel_address: String::new(),
            log_keep_days: 14,
            remote_access: RemoteAccess::default(),
            update: UpdateConfig::default(),
        }
    }
}

pub fn listen_is_loopback(listen: &str) -> bool {
    let host = listen.rsplit_once(':').map(|(h, _)| h).unwrap_or(listen);
    host.starts_with("127.") || host == "::1" || host == "localhost" || host == "[::1]"
}

impl PanelConfig {
    /// 实际生效的监听地址：未确认远程访问时强制回环。
    pub fn effective_listen(&self) -> (String, Option<String>) {
        if listen_is_loopback(&self.listen) || self.remote_access.enabled {
            return (self.listen.clone(), None);
        }
        let port = self
            .listen
            .rsplit_once(':')
            .map(|(_, p)| p.to_string())
            .unwrap_or_else(|| "18080".to_string());
        (
            format!("127.0.0.1:{port}"),
            Some(format!(
                "配置的监听地址 {} 属于非回环地址，但远程访问未确认；本次强制绑定 127.0.0.1:{port}",
                self.listen
            )),
        )
    }
}

pub struct Config {
    pub home: PathBuf,
}

impl Config {
    /// 数据根：`BERTHIO_HOME` > `/opt/berthio`（存在时）> 当前目录 `.berthio`。
    pub fn resolve() -> Self {
        let home = std::env::var("BERTHIO_HOME")
            .map(PathBuf::from)
            .ok()
            .or_else(|| {
                let opt = PathBuf::from("/opt/berthio");
                if opt.exists() {
                    Some(opt)
                } else {
                    None
                }
            })
            .unwrap_or_else(|| PathBuf::from(".berthio"));
        let cfg = Config { home };
        cfg.ensure_layout();
        cfg
    }

    pub fn ensure_layout(&self) {
        for sub in [
            "config",
            "state",
            "state/backups",
            "logs/audit",
            "logs/kernels",
            "state/kernels",
            "configs/generated",
            "data/rulesets",
            "data/geo",
            "tmp",
        ] {
            let _ = fs::create_dir_all(self.home.join(sub));
        }
    }

    pub fn panel_config_path(&self) -> PathBuf {
        self.home.join("config/panel.json")
    }

    /// 读取 `config/panel.json`；旧 schema 会先备份再自动迁移。
    pub fn load_panel(&self) -> ApiResult<(PanelConfig, Vec<UpgradeNote>)> {
        let path = self.panel_config_path();
        let mut notes = Vec::new();
        let mut cfg = if path.exists() {
            let backups = self.home.join("state/backups");
            let (value, note) = upgrade_file(
                &path,
                &backups,
                "panel",
                CONFIG_SCHEMA_VERSION,
                migrate_panel,
                |v| {
                    serde_json::from_value::<PanelConfig>(v.clone())
                        .map(|_| ())
                        .map_err(|e| format!("config schema 校验失败: {e}"))
                },
            )
            .map_err(|e| ApiError::internal(format!("配置文件升级失败: {e}")))?;
            if let Some(n) = note {
                notes.push(n);
            }
            serde_json::from_value::<PanelConfig>(value)
                .map_err(|e| ApiError::internal(format!("配置解析失败: {e}")))?
        } else {
            PanelConfig::default()
        };
        if let Ok(env_listen) = std::env::var("BERTHIO_LISTEN") {
            cfg.listen = env_listen;
        }
        if !path.exists() {
            let _ = util::write_atomic(
                &path,
                serde_json::to_string_pretty(&cfg)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
        Ok((cfg, notes))
    }

    pub fn save_panel(&self, cfg: &PanelConfig) -> ApiResult<()> {
        let path = self.panel_config_path();
        let body = serde_json::to_string_pretty(cfg).map_err(|e| ApiError::internal(e.to_string()))?;
        util::write_atomic(&path, body.as_bytes()).map_err(ApiError::internal)
    }
}

pub struct Store {
    pub home: PathBuf,
    config: Mutex<PanelConfig>,
    inner: Mutex<State>,
    notes: Vec<UpgradeNote>,
}

impl Store {
    pub fn load(cfg: &Config) -> ApiResult<Self> {
        let (panel, mut notes) = cfg.load_panel()?;
        let path = cfg.home.join("state/state.json");
        let mut state: State = if path.exists() {
            let backups = cfg.home.join("state/backups");
            let (value, note) = upgrade_file(
                &path,
                &backups,
                "state",
                SCHEMA_VERSION,
                migrate_state,
                |v| {
                    let s: State = serde_json::from_value(v.clone())
                        .map_err(|e| format!("state schema 校验失败: {e}"))?;
                    if s.schema_version != SCHEMA_VERSION {
                        return Err(format!(
                            "state schema 迁移后版本仍为 v{}（期望 v{SCHEMA_VERSION}）",
                            s.schema_version
                        ));
                    }
                    Ok(())
                },
            )
            .map_err(|e| ApiError::internal(format!("状态升级失败: {e}")))?;
            if let Some(n) = note {
                notes.push(n);
            }
            serde_json::from_value::<State>(value)
                .map_err(|e| ApiError::internal(format!("state schema 解析失败: {e}")))?
        } else {
            State::default()
        };
        if state.panel.created_at.is_empty() {
            state.panel.created_at = util::now_iso();
        }
        if state.panel.panel_address.is_empty() && !panel.panel_address.is_empty() {
            state.panel.panel_address = panel.panel_address.clone();
        }
        let store = Self {
            home: cfg.home.clone(),
            config: Mutex::new(panel),
            inner: Mutex::new(state),
            notes,
        };
        for note in &store.notes {
            store.audit(
                "schema.upgrade",
                "system",
                json!({
                    "file": note.kind,
                    "from": note.from,
                    "to": note.to,
                    "backup": note.backup.as_ref().map(|p| p.display().to_string()),
                }),
            );
        }
        store.persist(&store.inner.lock().unwrap())?;
        Ok(store)
    }

    /// 启动时发生的 schema 升级（供 main 打印，不含敏感数据）。
    pub fn startup_notes(&self) -> Vec<String> {
        self.notes
            .iter()
            .map(|n| {
                format!(
                    "{} schema v{} → v{} 已自动迁移，备份：{}",
                    n.kind,
                    n.from,
                    n.to,
                    n.backup
                        .as_ref()
                        .map(|p| p.display().to_string())
                        .unwrap_or_else(|| "（无）".to_string())
                )
            })
            .collect()
    }

    pub fn config(&self) -> PanelConfig {
        self.config.lock().unwrap().clone()
    }

    pub fn set_config(&self, panel: PanelConfig) {
        *self.config.lock().unwrap() = panel;
    }

    pub fn snapshot(&self) -> State {
        self.inner.lock().unwrap().clone()
    }

    /// 在锁内执行变更；闭包返回错误时不会写盘。
    pub fn update<T>(&self, f: impl FnOnce(&mut State) -> ApiResult<T>) -> ApiResult<T> {
        let mut guard = self.inner.lock().unwrap();
        let out = f(&mut guard)?;
        self.persist(&guard)?;
        Ok(out)
    }

    pub fn persist(&self, state: &State) -> ApiResult<()> {
        let path = self.home.join("state/state.json");
        let body = serde_json::to_string_pretty(state).map_err(|e| ApiError::internal(e.to_string()))?;
        util::write_atomic(&path, body.as_bytes()).map_err(ApiError::internal)
    }

    pub fn audit(&self, event: &str, actor: &str, detail: serde_json::Value) {
        let stamp = util::now_iso();
        let month = &stamp[0..7];
        let line = serde_json::json!({
            "ts": stamp,
            "event": event,
            "actor": actor,
            "detail": detail,
        });
        let path = self.home.join(format!("logs/audit/{month}.jsonl"));
        let _ = util::append_jsonl(&path, &line);
    }

    pub fn home_path(&self, rel: &str) -> PathBuf {
        self.home.join(rel)
    }

    /// 仅 demo 模式调用：写入内置演示数据（临时目录，随进程结束丢弃）。
    pub fn seed_demo(&self) -> ApiResult<()> {
        let demo = crate::demo::state();
        self.update(|state| {
            *state = demo;
            Ok(())
        })
    }

    pub fn tmp_path(&self, name: &str) -> PathBuf {
        self.home.join("tmp").join(name)
    }
}

fn schema_of(v: &Value) -> u32 {
    v.get("schema_version")
        .and_then(|x| x.as_u64())
        .map(|x| x.min(u32::MAX as u64) as u32)
        .unwrap_or(1)
}

fn backup_stamp() -> String {
    util::now_iso()
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// 读取 JSON 文件并按需升级：
/// 1. 版本高于本程序 → 拒绝（避免降级覆盖）；
/// 2. 版本更低 → 先备份原始字节，再迁移、校验、原子写回、回读校验；
/// 3. 任一步失败 → 原文件保持（或从备份回滚），返回错误。
fn upgrade_file(
    path: &Path,
    backup_dir: &Path,
    kind: &'static str,
    current: u32,
    migrate: impl Fn(&mut Value, u32),
    validate: impl Fn(&Value) -> Result<(), String>,
) -> Result<(Value, Option<UpgradeNote>), String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    let mut value: Value =
        serde_json::from_str(&raw).map_err(|e| format!("解析 {} 失败: {e}", path.display()))?;
    let from = schema_of(&value);
    if from > current {
        return Err(format!(
            "{} 的 schema_version=v{from} 高于本程序支持的 v{current}；拒绝启动以免降级覆盖数据。请升级程序，或用 state/backups 下的备份恢复",
            path.display()
        ));
    }
    if from == current {
        return Ok((value, None));
    }
    fs::create_dir_all(backup_dir)
        .map_err(|e| format!("创建备份目录 {} 失败: {e}", backup_dir.display()))?;
    let backup = backup_dir.join(format!("{kind}.v{from}.{}.json", backup_stamp()));
    util::write_atomic(&backup, raw.as_bytes())
        .map_err(|e| format!("写入升级备份 {} 失败: {e}", backup.display()))?;
    migrate(&mut value, from);
    value["schema_version"] = json!(current);
    validate(&value)?;
    let body = serde_json::to_string_pretty(&value).map_err(|e| format!("序列化失败: {e}"))?;
    util::write_atomic(path, body.as_bytes())
        .map_err(|e| format!("写回 {} 失败: {e}", path.display()))?;
    let readback = fs::read_to_string(path)
        .map_err(|e| format!("回读 {} 失败: {e}", path.display()))
        .and_then(|s| serde_json::from_str::<Value>(&s).map_err(|e| e.to_string()))
        .and_then(|v| validate(&v));
    if let Err(e) = readback {
        let rollback = fs::read_to_string(&backup)
            .map_err(|e| e.to_string())
            .and_then(|s| util::write_atomic(path, s.as_bytes()));
        return Err(match rollback {
            Ok(()) => format!(
                "迁移写回校验失败：{e}；已回滚到 {}",
                backup.display()
            ),
            Err(re) => format!("迁移写回校验失败：{e}；回滚也失败：{re}"),
        });
    }
    Ok((
        value,
        Some(UpgradeNote {
            kind,
            from,
            to: current,
            backup: Some(backup),
        }),
    ))
}

/// panel.json v1 → v2：监听地址/日志保留天数/可信代理规范化。
fn migrate_panel(v: &mut Value, from: u32) {
    if from < 2 {
        if let Some(listen) = v.get("listen").and_then(|x| x.as_str()) {
            if !listen.contains(':') {
                v["listen"] = json!(DEFAULT_LISTEN);
            }
        }
        let keep = v.get("log_keep_days").and_then(|x| x.as_u64()).unwrap_or(0);
        if keep == 0 || keep > 3650 {
            v["log_keep_days"] = json!(14);
        }
        if let Some(proxies) = v
            .get_mut("remote_access")
            .and_then(|x| x.get_mut("trusted_proxies"))
            .and_then(|x| x.as_array_mut())
        {
            let cleaned: Vec<Value> = proxies
                .iter()
                .filter_map(|p| p.as_str())
                .map(|p| p.trim())
                .filter(|p| !p.is_empty())
                .map(|p| json!(p))
                .collect();
            *proxies = cleaned;
        }
    }
}

/// state.json v1 → v2：token target 归一化、入站 source 补齐。
fn migrate_state(v: &mut Value, from: u32) {
    if from < 2 {
        if let Some(tokens) = v.get_mut("tokens").and_then(|t| t.as_array_mut()) {
            for t in tokens.iter_mut() {
                let cur = t
                    .get("target")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                let canon = match cur.as_str() {
                    "" | "clash" | "clash-verge" | "clash.meta" | "mihomo" | "verge" | "stash" => {
                        "mihomo"
                    }
                    "singbox" | "sing-box" => "sing-box",
                    other => other,
                };
                t["target"] = json!(canon);
            }
        }
        if let Some(inbounds) = v.get_mut("inbounds").and_then(|i| i.as_array_mut()) {
            for i in inbounds.iter_mut() {
                let source = i
                    .get("source")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .trim();
                if source.is_empty() {
                    i["source"] = json!("manual");
                }
            }
        }
    }
}

pub fn init_home(home: &Path) {
    for sub in [
        "config",
        "state",
        "state/backups",
        "logs/audit",
        "configs/generated",
        "data/rulesets",
        "data/geo",
        "tmp",
    ] {
        let _ = fs::create_dir_all(home.join(sub));
    }
}
