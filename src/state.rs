use crate::error::{ApiError, ApiResult};
use crate::model::State;
use crate::util;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const DEFAULT_LISTEN: &str = "127.0.0.1:18080";

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
}

fn default_schema() -> u32 {
    1
}
fn default_listen() -> String {
    DEFAULT_LISTEN.to_string()
}

impl Default for PanelConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            listen: DEFAULT_LISTEN.to_string(),
            panel_address: String::new(),
            log_keep_days: 14,
        }
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
            "logs/audit",
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

    pub fn load_panel(&self) -> PanelConfig {
        let path = self.panel_config_path();
        if path.exists() {
            if let Ok(v) = util::read_json(&path) {
                if let Ok(mut cfg) = serde_json::from_value::<PanelConfig>(v) {
                    if let Ok(env_listen) = std::env::var("BERTHIO_LISTEN") {
                        cfg.listen = env_listen;
                    }
                    return cfg;
                }
            }
        }
        let mut cfg = PanelConfig::default();
        if let Ok(env_listen) = std::env::var("BERTHIO_LISTEN") {
            cfg.listen = env_listen;
        }
        let _ = util::write_atomic(
            &path,
            serde_json::to_string_pretty(&cfg)
                .unwrap_or_default()
                .as_bytes(),
        );
        cfg
    }

    pub fn save_panel(&self, cfg: &PanelConfig) -> ApiResult<()> {
        let path = self.panel_config_path();
        let body = serde_json::to_string_pretty(cfg).map_err(|e| ApiError::internal(e.to_string()))?;
        util::write_atomic(&path, body.as_bytes()).map_err(ApiError::internal)
    }
}

pub struct Store {
    pub home: PathBuf,
    pub config: PanelConfig,
    inner: Mutex<State>,
}

impl Store {
    pub fn load(cfg: &Config) -> ApiResult<Self> {
        let panel = cfg.load_panel();
        let path = cfg.home.join("state/state.json");
        let mut state: State = if path.exists() {
            util::read_json(&path)
                .and_then(|v| {
                    serde_json::from_value::<State>(v).map_err(|e| format!("state schema 解析失败: {e}"))
                })
                .map_err(ApiError::internal)?
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
            config: panel,
            inner: Mutex::new(state),
        };
        store.persist(&store.inner.lock().unwrap())?;
        Ok(store)
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

pub fn init_home(home: &Path) {
    for sub in [
        "config",
        "state",
        "logs/audit",
        "configs/generated",
        "data/rulesets",
        "data/geo",
        "tmp",
    ] {
        let _ = fs::create_dir_all(home.join(sub));
    }
}
