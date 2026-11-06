//! 规则集换源下载（M2）。
//!
//! 设计约束（见 DECISIONS Q36–Q38）：
//! - 官方源永远是校验基准；镜像只做传输加速，镜像给出的文件必须与官方基准 SHA-256 一致才落盘。
//! - 基准不是硬编码，而是运行时以官方源实际内容建立，落盘到 `data/rulesets/manifest.json`；
//!   官方内容变化时更新基准并记录事件，镜像仍必须匹配最新基准。
//! - 自动模式按优先级顺序回退；手动模式只走指定源，失败即报错，不静默换源。

use crate::error::{ApiError, ApiResult};
use crate::rulesets;
use crate::state::Store;
use crate::util;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MAX_DOWNLOAD: u64 = 64 * 1024 * 1024;
const FETCH_TIMEOUT_SECS: u64 = 30;

/// 源配置与清单都是小文件，单进程面板；用一把锁避免并发写坏。
static IO_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    pub label: String,
    /// `official` | `third`
    pub kind: String,
    /// 内置官方源：不可删除、不可禁用、不可改 URL。
    #[serde(default)]
    pub locked: bool,
    #[serde(default)]
    pub enabled: bool,
    pub priority: u32,
    pub url_mihomo: String,
    pub url_singbox: String,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SourceConfig {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    /// `auto` 或具体源 id（手动指定）
    #[serde(default = "default_mode")]
    pub mode: String,
    #[serde(default)]
    pub sources: Vec<Source>,
}

fn default_schema() -> u32 {
    1
}
fn default_mode() -> String {
    "auto".to_string()
}

fn src(
    id: &str,
    label: &str,
    kind: &str,
    priority: u32,
    enabled: bool,
    url_mihomo: &str,
    url_singbox: &str,
    note: &str,
) -> Source {
    Source {
        id: id.to_string(),
        label: label.to_string(),
        kind: kind.to_string(),
        locked: id == "official",
        enabled,
        priority,
        url_mihomo: url_mihomo.to_string(),
        url_singbox: url_singbox.to_string(),
        note: note.to_string(),
    }
}

const GH: &str = "https://raw.githubusercontent.com/MetaCubeX/meta-rules-dat";

/// 内置源目录。镜像可用性以 2026-11 本机实测为准（见 release-notes）。
pub fn default_sources() -> Vec<Source> {
    vec![
        src(
            "official",
            "官方源 · GitHub MetaCubeX/meta-rules-dat",
            "official",
            1,
            true,
            &format!("{GH}/meta/geo/{{kind}}/{{id}}.yaml"),
            &format!("{GH}/sing/geo/{{kind}}/{{id}}.srs"),
            "校验基准来源；不可禁用、不可删除",
        ),
        src(
            "gh-proxy",
            "镜像 · gh-proxy.com",
            "third",
            2,
            true,
            &format!("https://gh-proxy.com/{GH}/meta/geo/{{kind}}/{{id}}.yaml"),
            &format!("https://gh-proxy.com/{GH}/sing/geo/{{kind}}/{{id}}.srs"),
            "2026-11 本机实测可用（约 0.9 s / 小文件）",
        ),
        src(
            "ghproxy-net",
            "镜像 · ghproxy.net",
            "third",
            3,
            true,
            &format!("https://ghproxy.net/{GH}/meta/geo/{{kind}}/{{id}}.yaml"),
            &format!("https://ghproxy.net/{GH}/sing/geo/{{kind}}/{{id}}.srs"),
            "2026-11 本机实测可用（约 1.6 s / 小文件）",
        ),
        src(
            "jsdelivr",
            "镜像 · jsDelivr CDN",
            "third",
            4,
            true,
            "https://cdn.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@meta/geo/{kind}/{id}.yaml",
            "https://cdn.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing/geo/{kind}/{id}.srs",
            "2026-11 本机实测可用（约 2.0 s / 小文件）",
        ),
        src(
            "ghfast",
            "镜像 · ghfast.top",
            "third",
            5,
            false,
            &format!("https://ghfast.top/{GH}/meta/geo/{{kind}}/{{id}}.yaml"),
            &format!("https://ghfast.top/{GH}/sing/geo/{{kind}}/{{id}}.srs"),
            "2026-11 本机实测返回 403；默认禁用，保留作回退反例",
        ),
    ]
}

fn sources_path(home: &Path) -> PathBuf {
    home.join("config/sources.json")
}

fn manifest_path(home: &Path) -> PathBuf {
    home.join("data/rulesets/manifest.json")
}

fn status_path(home: &Path) -> PathBuf {
    home.join("data/rulesets/status.json")
}

/// 读取源配置；缺失时写默认。官方源缺失/被禁用会被强制恢复（保证基准可用）。
fn load_config_locked(home: &Path) -> SourceConfig {
    let path = sources_path(home);
    let mut cfg = if path.exists() {
        util::read_json(&path)
            .ok()
            .and_then(|v| serde_json::from_value::<SourceConfig>(v).ok())
            .unwrap_or_else(|| SourceConfig {
                schema_version: 1,
                mode: default_mode(),
                sources: default_sources(),
            })
    } else {
        SourceConfig {
            schema_version: 1,
            mode: default_mode(),
            sources: default_sources(),
        }
    };

    if !cfg.sources.iter().any(|s| s.id == "official") {
        let official = default_sources().into_iter().next().unwrap();
        cfg.sources.push(official);
    }
    // 官方源永远可用
    for s in cfg.sources.iter_mut() {
        if s.id == "official" {
            s.locked = true;
            s.enabled = true;
            s.kind = "official".to_string();
            let d = default_sources().into_iter().next().unwrap();
            s.url_mihomo = d.url_mihomo;
            s.url_singbox = d.url_singbox;
            s.priority = 1;
        }
        s.priority = s.priority.clamp(1, 99);
    }
    // 去重（保留后者会让人困惑，保留第一次出现的）
    let mut seen = std::collections::HashSet::new();
    cfg.sources.retain(|s| seen.insert(s.id.clone()));
    cfg.sources.sort_by_key(|s| s.priority);

    // 手动模式指向不存在/已禁用的源时回落自动
    if cfg.mode != "auto" {
        let valid = cfg
            .sources
            .iter()
            .any(|s| s.id == cfg.mode && s.enabled);
        if !valid {
            cfg.mode = "auto".to_string();
        }
    }

    if !path.exists() {
        let _ = save_config_locked(home, &cfg);
    }
    cfg
}

fn save_config_locked(home: &Path, cfg: &SourceConfig) -> Result<(), String> {
    let body = serde_json::to_string_pretty(cfg).map_err(|e| e.to_string())?;
    util::write_atomic(&sources_path(home), body.as_bytes())
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Baseline {
    pub sha256: String,
    pub size: u64,
    pub fetched_at: String,
    pub source: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SavedFile {
    pub sha256: String,
    pub size: u64,
    pub saved_at: String,
    pub source: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub baselines: HashMap<String, Baseline>,
    #[serde(default)]
    pub saved: HashMap<String, SavedFile>,
}

impl Default for Manifest {
    fn default() -> Self {
        Self {
            schema_version: 1,
            baselines: HashMap::new(),
            saved: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SourceStatus {
    pub status: String,
    pub ms: u64,
    pub at: String,
    pub detail: String,
    #[serde(default)]
    pub bytes: u64,
}

fn read_manifest(home: &Path) -> Manifest {
    if let Ok(v) = util::read_json(&manifest_path(home)) {
        if let Ok(m) = serde_json::from_value::<Manifest>(v) {
            return m;
        }
    }
    Manifest::default()
}

fn write_manifest(home: &Path, m: &Manifest) -> Result<(), String> {
    let body = serde_json::to_string_pretty(m).map_err(|e| e.to_string())?;
    util::write_atomic(&manifest_path(home), body.as_bytes())
}

fn read_status(home: &Path) -> HashMap<String, SourceStatus> {
    util::read_json(&status_path(home))
        .ok()
        .and_then(|v| serde_json::from_value::<HashMap<String, SourceStatus>>(v).ok())
        .unwrap_or_default()
}

fn write_status(home: &Path, st: &HashMap<String, SourceStatus>) {
    if let Ok(body) = serde_json::to_string_pretty(st) {
        let _ = util::write_atomic(&status_path(home), body.as_bytes());
    }
}

fn record_status(store: &Store, id: &str, status: &str, ms: u64, detail: &str, bytes: u64) {
    let _g = IO_LOCK.lock().unwrap();
    let mut st = read_status(&store.home);
    st.insert(
        id.to_string(),
        SourceStatus {
            status: status.to_string(),
            ms,
            at: util::now_iso(),
            detail: detail.to_string(),
            bytes,
        },
    );
    write_status(&store.home, &st);
}

/// 供 `/api/rulesets/sources` 与 UI 使用。
pub fn public_payload(store: &Store) -> serde_json::Value {
    let _g = IO_LOCK.lock().unwrap();
    let cfg = load_config_locked(&store.home);
    let manifest = read_manifest(&store.home);
    let status = read_status(&store.home);

    let baseline_items: Vec<serde_json::Value> = manifest
        .baselines
        .iter()
        .map(|(k, b)| {
            serde_json::json!({
                "key": k,
                "sha256": b.sha256,
                "size": b.size,
                "fetched_at": b.fetched_at,
                "source": b.source
            })
        })
        .collect();

    let saved_items: Vec<serde_json::Value> = manifest
        .saved
        .iter()
        .map(|(k, s)| {
            let fresh = manifest
                .baselines
                .get(k)
                .map(|b| b.sha256 == s.sha256)
                .unwrap_or(false);
            serde_json::json!({
                "key": k,
                "sha256": s.sha256,
                "size": s.size,
                "saved_at": s.saved_at,
                "source": s.source,
                "path": s.path,
                "matches_baseline": fresh
            })
        })
        .collect();

    let sources: Vec<serde_json::Value> = cfg
        .sources
        .iter()
        .map(|s| {
            let last = status.get(&s.id);
            serde_json::json!({
                "id": s.id,
                "label": s.label,
                "kind": s.kind,
                "locked": s.locked,
                "enabled": s.enabled,
                "priority": s.priority,
                "url_mihomo": s.url_mihomo,
                "url_singbox": s.url_singbox,
                "note": s.note,
                "last": last.map(|l| serde_json::json!({
                    "status": l.status, "ms": l.ms, "at": l.at,
                    "detail": l.detail, "bytes": l.bytes
                })).unwrap_or(serde_json::json!(null))
            })
        })
        .collect();

    serde_json::json!({
        "mode": cfg.mode,
        "selected": cfg.mode,
        "sources": sources,
        "baselines": {"count": baseline_items.len(), "items": baseline_items},
        "local": {"count": saved_items.len(), "items": saved_items},
        "policy": "镜像只做传输；落盘前必须与官方基准 SHA-256 一致；不一致丢弃临时文件并继续回退",
        "note": "官方源不可禁用；镜像默认仅内置 gh-proxy / ghproxy.net / jsDelivr 三个已实测可用条目"
    })
}

/// 已缓存文件 + 基准数量，供规则集目录页显示来源。
pub fn cache_summary(store: &Store) -> serde_json::Value {
    let _g = IO_LOCK.lock().unwrap();
    let m = read_manifest(&store.home);
    let items: Vec<serde_json::Value> = m
        .saved
        .iter()
        .map(|(k, s)| {
            let fresh = m
                .baselines
                .get(k)
                .map(|b| b.sha256 == s.sha256)
                .unwrap_or(false);
            serde_json::json!({
                "key": k, "source": s.source, "size": s.size,
                "sha256": s.sha256, "saved_at": s.saved_at,
                "path": s.path, "matches_baseline": fresh
            })
        })
        .collect();
    serde_json::json!({
        "count": items.len(),
        "baseline_count": m.baselines.len(),
        "items": items
    })
}

fn sanitize(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch.to_ascii_lowercase());
        } else if ch == '.' || ch == '!' {
            out.push('-');
        }
    }
    out
}

fn local_path(home: &Path, format: &str, kind: &str, real_id: &str) -> PathBuf {
    let ext = if format == "singbox" { "srs" } else { "yaml" };
    home.join("data/rulesets")
        .join(format)
        .join(format!("{}-{}.{}", sanitize(kind), sanitize(real_id), ext))
}

fn render_url(template: &str, kind: &str, real_id: &str) -> String {
    template
        .replace("{kind}", kind)
        .replace("{id}", real_id)
}

fn parse_target(id: &str, format: &str) -> ApiResult<(String, String, String)> {
    let (kind, real_id) = id
        .split_once(':')
        .ok_or_else(|| ApiError::bad_request("规则集 id 必须是 kind:real_id，例如 geoip:private"))?;
    if !matches!(kind, "geoip" | "geosite") {
        return Err(ApiError::bad_request("kind 只能是 geoip 或 geosite"));
    }
    if real_id.trim().is_empty() {
        return Err(ApiError::bad_request("real_id 不能为空"));
    }
    let catalog = rulesets::catalog();
    if !catalog
        .iter()
        .any(|r| r.kind == kind && r.real_id == real_id)
    {
        return Err(ApiError::not_found(format!("规则集 {id} 不在目录内")));
    }
    if format != "mihomo" && format != "singbox" {
        return Err(ApiError::bad_request("format 只能是 mihomo 或 singbox"));
    }
    Ok((kind.to_string(), real_id.to_string(), format.to_string()))
}

fn format_key(format: &str, kind: &str, real_id: &str) -> String {
    format!("{format}:{kind}:{real_id}")
}

struct Download {
    bytes: Vec<u8>,
    ms: u64,
}

/// 实际下载；失败返回 (kind, detail)，kind ∈ transport | http_error。
fn download(url: &str) -> Result<Download, (String, String)> {
    let started = Instant::now();
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(Duration::from_secs(FETCH_TIMEOUT_SECS))
        .user_agent(&format!("berthio/{VERSION} (ruleset-mirror-check)"))
        .build();
    let resp = match agent.get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, resp)) => {
            let mut snippet = String::new();
            let _ = resp.into_reader().take(200).read_to_string(&mut snippet);
            return Err((
                "http_error".into(),
                format!("HTTP {code} {}", snippet.trim().replace('\n', " ")),
            ));
        }
        Err(e) => return Err(("transport".into(), format!("连接失败: {e}"))),
    };
    let mut buf = Vec::new();
    if let Err(e) = resp.into_reader().take(MAX_DOWNLOAD).read_to_end(&mut buf) {
        return Err(("transport".into(), format!("读取响应失败: {e}")));
    }
    if buf.len() as u64 >= MAX_DOWNLOAD {
        return Err(("transport".into(), "响应体超过 64 MB，已中止".into()));
    }
    Ok(Download {
        bytes: buf,
        ms: started.elapsed().as_millis() as u64,
    })
}

fn save_success(
    store: &Store,
    manifest: &mut Manifest,
    key: &str,
    format: &str,
    kind: &str,
    real_id: &str,
    source: &str,
    bytes: &[u8],
    sha: &str,
) -> ApiResult<String> {
    let path = local_path(&store.home, format, kind, real_id);
    util::write_atomic(&path, bytes).map_err(ApiError::internal)?;
    manifest.saved.insert(
        key.to_string(),
        SavedFile {
            sha256: sha.to_string(),
            size: bytes.len() as u64,
            saved_at: util::now_iso(),
            source: source.to_string(),
            path: path.display().to_string(),
        },
    );
    Ok(path.display().to_string())
}

pub struct FetchReq {
    pub id: String,
    pub format: String,
    pub source: String,
    pub refresh_baseline: bool,
}

/// 换源下载 + 校验 + 回退。返回结构化结果（ok=false 时也返回完整回退链，便于界面展示）。
pub fn fetch(store: &Store, req: FetchReq) -> ApiResult<serde_json::Value> {
    let (kind, real_id, format) = parse_target(&req.id, &req.format)?;
    let key = format_key(&format, &kind, &real_id);

    let (cfg, mut manifest) = {
        let _g = IO_LOCK.lock().unwrap();
        (load_config_locked(&store.home), read_manifest(&store.home))
    };

    let auto = req.source == "auto" || req.source.is_empty();
    let mut chain: Vec<Source> = if auto {
        cfg.sources.iter().filter(|s| s.enabled).cloned().collect()
    } else {
        cfg.sources
            .iter()
            .filter(|s| s.id == req.source && s.enabled)
            .cloned()
            .collect()
    };
    // 刷新基准：官方必须排在链首；自动模式本来就在链首，这里只处理手动模式。
    if req.refresh_baseline && !chain.iter().any(|s| s.locked) {
        if let Some(official) = cfg.sources.iter().find(|s| s.locked) {
            chain.insert(0, official.clone());
        }
    }
    if chain.is_empty() {
        return Err(ApiError::new(
            409,
            "no_source",
            "没有可用的下载源：手动模式指向的源不存在或已禁用",
        ));
    }

    // 已有官方基准时：镜像优先（加速），官方兜底；没有基准时：先取官方建立基准。
    let has_baseline = manifest.baselines.contains_key(&key);
    let prefer_official = req.refresh_baseline || !has_baseline;
    if prefer_official {
        chain.sort_by_key(|s| (!s.locked, s.priority));
    } else {
        chain.sort_by_key(|s| (s.locked, s.priority));
    }
    // 严格手动模式：只允许一个源，不回退。
    if !auto && !req.refresh_baseline {
        chain.truncate(1);
    }
    // 没有基准的镜像无法校验，直接给出可执行的解释，不做无意义下载。
    if has_baseline == false && !chain.iter().any(|s| s.locked) {
        return Ok(serde_json::json!({
            "ok": false, "id": req.id, "format": format, "kind": kind, "real_id": real_id,
            "key": key, "total_ms": 0, "chosen": null, "baseline": null, "attempts": [],
            "error": {
                "code": "no_baseline",
                "message": "尚无官方基准：镜像内容无法校验，拒绝下载。请先用官方源拉取一次（或点「刷新官方基准」）。"
            }
        }));
    }

    let mut attempts: Vec<serde_json::Value> = Vec::new();
    let started = Instant::now();
    let mut chosen: Option<(Source, Vec<u8>, String, String)> = None;

    for s in chain.iter() {
        let url = render_url(
            if format == "singbox" {
                &s.url_singbox
            } else {
                &s.url_mihomo
            },
            &kind,
            &real_id,
        );
        let dl = match download(&url) {
            Ok(d) => d,
            Err((kind_err, detail)) => {
                record_status(store, &s.id, &kind_err, 0, &detail, 0);
                attempts.push(serde_json::json!({
                    "source": s.id, "label": s.label, "status": kind_err,
                    "ms": 0, "url": url, "detail": detail
                }));
                continue;
            }
        };
        let sha = util::sha256_hex(&dl.bytes);

        if s.locked {
            let previous = manifest.baselines.get(&key).cloned();
            let refreshed = previous.as_ref().map(|b| b.sha256 != sha).unwrap_or(true);
            manifest.baselines.insert(
                key.clone(),
                Baseline {
                    sha256: sha.clone(),
                    size: dl.bytes.len() as u64,
                    fetched_at: util::now_iso(),
                    source: s.id.clone(),
                },
            );
            let saved = save_success(
                store, &mut manifest, &key, &format, &kind, &real_id, &s.id, &dl.bytes, &sha,
            )?;
            if refreshed {
                store.audit(
                    "rulesets.baseline.updated",
                    "local-admin",
                    serde_json::json!({"key": key, "sha256": sha, "bytes": dl.bytes.len()}),
                );
            }
            record_status(store, &s.id, "ok", dl.ms, "官方基准", dl.bytes.len() as u64);
            attempts.push(serde_json::json!({
                "source": s.id, "label": s.label, "status": "ok",
                "ms": dl.ms, "url": url, "bytes": dl.bytes.len(), "sha256": sha,
                "role": "baseline", "baseline_refreshed": refreshed,
                "detail": if refreshed { "官方内容已更新，基准随之更新" } else { "与既有基准一致" }
            }));
            chosen = Some((s.clone(), dl.bytes, sha, saved));
            break;
        }

        // 镜像：必须先有官方基准
        let expected = manifest.baselines.get(&key).map(|b| b.sha256.clone());
        let Some(expected) = expected else {
            record_status(store, &s.id, "no_baseline", dl.ms, "缺少官方基准", dl.bytes.len() as u64);
            attempts.push(serde_json::json!({
                "source": s.id, "label": s.label, "status": "no_baseline",
                "ms": dl.ms, "url": url, "bytes": dl.bytes.len(), "sha256": sha,
                "detail": "尚无官方基准，拒绝落盘；请先用官方源建立基准（刷新官方基准）"
            }));
            continue;
        };

        if expected != sha {
            record_status(
                store,
                &s.id,
                "hash_mismatch",
                dl.ms,
                "与官方基准 SHA-256 不一致",
                dl.bytes.len() as u64,
            );
            attempts.push(serde_json::json!({
                "source": s.id, "label": s.label, "status": "hash_mismatch",
                "ms": dl.ms, "url": url, "bytes": dl.bytes.len(), "sha256": sha,
                "expected": expected,
                "detail": "镜像内容与官方基准不一致，已丢弃，未落盘"
            }));
            continue;
        }

        let saved = save_success(
            store, &mut manifest, &key, &format, &kind, &real_id, &s.id, &dl.bytes, &sha,
        )?;
        record_status(store, &s.id, "ok", dl.ms, "校验通过（匹配官方基准）", dl.bytes.len() as u64);
        attempts.push(serde_json::json!({
            "source": s.id, "label": s.label, "status": "ok",
            "ms": dl.ms, "url": url, "bytes": dl.bytes.len(), "sha256": sha,
            "role": "mirror", "expected": expected,
            "detail": "与官方基准一致，已落盘"
        }));
        chosen = Some((s.clone(), dl.bytes, sha, saved));
        break;
    }

    let total_ms = started.elapsed().as_millis() as u64;
    if chosen.is_some() {
        let _g = IO_LOCK.lock().unwrap();
        write_manifest(&store.home, &manifest).map_err(ApiError::internal)?;
    }
    let base = manifest.baselines.get(&key).cloned();
    let ok = chosen.is_some();
    let payload = serde_json::json!({
        "ok": ok,
        "id": req.id,
        "format": format,
        "kind": kind,
        "real_id": real_id,
        "key": key,
        "mode": if req.source.is_empty() { "auto".to_string() } else { req.source.clone() },
        "refresh_baseline": req.refresh_baseline,
        "total_ms": total_ms,
        "chosen": chosen.as_ref().map(|(s, b, sha, path)| serde_json::json!({
            "source": s.id, "label": s.label, "bytes": b.len(),
            "sha256": sha, "path": path
        })),
        "baseline": base.map(|b| serde_json::json!({
            "sha256": b.sha256, "size": b.size, "fetched_at": b.fetched_at, "source": b.source
        })),
        "attempts": attempts,
        "note": if ok { "文件已通过官方 SHA-256 基准校验并落盘" } else { "未落盘：所有源均未通过校验或不可用" }
    });

    store.audit(
        "rulesets.fetch",
        "local-admin",
        serde_json::json!({
            "key": key, "ok": ok, "chosen": chosen.as_ref().map(|(s,_,_,_)| s.id.clone()),
            "total_ms": total_ms, "attempts": attempts.len()
        }),
    );

    if !ok {
        // 失败也用 200 + ok:false，便于前端完整展示回退链；错误语义在结构化字段里。
        return Ok(payload);
    }
    Ok(payload)
}

/// 单源连通性/一致性测速（不改基准、不落盘）。
pub fn test_source(store: &Store, id: &str) -> ApiResult<serde_json::Value> {
    let (src, manifest) = {
        let _g = IO_LOCK.lock().unwrap();
        let cfg = load_config_locked(&store.home);
        let src = cfg
            .sources
            .iter()
            .find(|s| s.id == id)
            .cloned()
            .ok_or_else(|| ApiError::not_found(format!("源 {id} 不存在")))?;
        if !src.enabled {
            return Err(ApiError::conflict(format!("源 {id} 已禁用，请先启用")));
        }
        (src, read_manifest(&store.home))
    };
    let probe = "geoip:private";
    let (kind, real_id) = probe.split_once(':').unwrap();
    let format = "mihomo";
    let url = render_url(&src.url_mihomo, kind, real_id);

    let result = match download(&url) {
        Ok(dl) => {
            let sha = util::sha256_hex(&dl.bytes);
            let key = format_key(format, kind, real_id);
            let matches = manifest.baselines.get(&key).map(|b| b.sha256 == sha);
            record_status(store, id, "ok", dl.ms, "测速成功", dl.bytes.len() as u64);
            serde_json::json!({
                "ok": true, "source": id, "ms": dl.ms, "bytes": dl.bytes.len(),
                "sha256": sha, "probe": probe, "matches_baseline": matches,
                "detail": match matches {
                    Some(true) => "与官方基准一致",
                    Some(false) => "与官方基准不一致（可能过期或被污染）",
                    None => "尚无官方基准，仅测连通性"
                }
            })
        }
        Err((kind_err, detail)) => {
            record_status(store, id, &kind_err, 0, &detail, 0);
            serde_json::json!({
                "ok": false, "source": id, "status": kind_err, "detail": detail, "probe": probe
            })
        }
    };
    Ok(result)
}

pub fn add_source(
    store: &Store,
    label: &str,
    url_mihomo: &str,
    url_singbox: &str,
    priority: u32,
    enabled: bool,
) -> ApiResult<serde_json::Value> {
    let label = label.trim();
    let url_mihomo = url_mihomo.trim();
    let url_singbox = url_singbox.trim();
    if label.is_empty() || label.chars().count() > 60 {
        return Err(ApiError::bad_request("名称不能为空且不超过 60 字").with_field("label", "必填"));
    }
    for (field, u) in [("url_mihomo", url_mihomo), ("url_singbox", url_singbox)] {
        if !u.is_empty() && !(u.starts_with("https://") || u.starts_with("http://")) {
            return Err(ApiError::bad_request("URL 必须以 http:// 或 https:// 开头")
                .with_field(field, "示例：https://gh-proxy.com/https://raw.githubusercontent.com/..."));
        }
        if u.contains(char::is_whitespace) || u.len() > 500 {
            return Err(ApiError::bad_request("URL 不能含空格且长度不超过 500")
                .with_field(field, "检查粘贴内容"));
        }
    }
    if url_mihomo.is_empty() {
        return Err(ApiError::bad_request("至少需要填写 mihomo YAML 模板").with_field("url_mihomo", "必填"));
    }

    let _g = IO_LOCK.lock().unwrap();
    let mut cfg = load_config_locked(&store.home);
    let id = format!("custom-{}", util::random_hex(4));
    let singbox = if url_singbox.is_empty() {
        url_mihomo.to_string()
    } else {
        url_singbox.to_string()
    };
    cfg.sources.push(Source {
        id: id.clone(),
        label: label.to_string(),
        kind: "third".to_string(),
        locked: false,
        enabled,
        priority: priority.clamp(1, 99),
        url_mihomo: url_mihomo.to_string(),
        url_singbox: singbox,
        note: "用户添加".to_string(),
    });
    save_config_locked(&store.home, &cfg).map_err(ApiError::internal)?;
    store.audit("rulesets.source.added", "local-admin", serde_json::json!({"id": id, "label": label}));
    Ok(serde_json::json!({"ok": true, "id": id}))
}

pub fn patch_source(
    store: &Store,
    id: &str,
    enabled: Option<bool>,
    priority: Option<u32>,
    url_mihomo: Option<String>,
    url_singbox: Option<String>,
) -> ApiResult<serde_json::Value> {
    let _g = IO_LOCK.lock().unwrap();
    let mut cfg = load_config_locked(&store.home);
    let src = cfg
        .sources
        .iter_mut()
        .find(|s| s.id == id)
        .ok_or_else(|| ApiError::not_found(format!("源 {id} 不存在")))?;
    if src.locked {
        return Err(ApiError::conflict("官方源不可修改：它是校验基准，永远是链首"));
    }
    if let Some(v) = enabled {
        src.enabled = v;
    }
    if let Some(v) = priority {
        src.priority = v.clamp(1, 99);
    }
    if let Some(u) = url_mihomo {
        if !u.starts_with("http://") && !u.starts_with("https://") {
            return Err(ApiError::bad_request("URL 必须以 http:// 或 https:// 开头"));
        }
        src.url_mihomo = u;
    }
    if let Some(u) = url_singbox {
        if !u.starts_with("http://") && !u.starts_with("https://") {
            return Err(ApiError::bad_request("URL 必须以 http:// 或 https:// 开头"));
        }
        src.url_singbox = u;
    }
    let enabled_now = src.enabled;
    let id_now = src.id.clone();
    cfg.sources.sort_by_key(|s| s.priority);
    if cfg.mode != "auto" {
        let valid = cfg.sources.iter().any(|s| s.id == cfg.mode && s.enabled);
        if !valid {
            cfg.mode = "auto".to_string();
        }
    }
    save_config_locked(&store.home, &cfg).map_err(ApiError::internal)?;
    store.audit(
        "rulesets.source.patched",
        "local-admin",
        serde_json::json!({"id": id_now, "enabled": enabled_now}),
    );
    Ok(serde_json::json!({"ok": true, "id": id_now, "enabled": enabled_now}))
}

pub fn delete_source(store: &Store, id: &str) -> ApiResult<serde_json::Value> {
    if id == "official" {
        return Err(ApiError::conflict("官方源不可删除"));
    }
    let _g = IO_LOCK.lock().unwrap();
    let mut cfg = load_config_locked(&store.home);
    let before = cfg.sources.len();
    cfg.sources.retain(|s| s.id != id);
    if cfg.sources.len() == before {
        return Err(ApiError::not_found(format!("源 {id} 不存在")));
    }
    if cfg.mode == id {
        cfg.mode = "auto".to_string();
    }
    save_config_locked(&store.home, &cfg).map_err(ApiError::internal)?;
    store.audit("rulesets.source.deleted", "local-admin", serde_json::json!({"id": id}));
    Ok(serde_json::json!({"ok": true, "id": id}))
}

pub fn set_mode(store: &Store, mode: &str) -> ApiResult<serde_json::Value> {
    let _g = IO_LOCK.lock().unwrap();
    let mut cfg = load_config_locked(&store.home);
    if mode != "auto" {
        let ok = cfg.sources.iter().any(|s| s.id == mode && s.enabled);
        if !ok {
            return Err(ApiError::bad_request(format!("源 {mode} 不存在或已禁用")));
        }
    }
    cfg.mode = mode.to_string();
    save_config_locked(&store.home, &cfg).map_err(ApiError::internal)?;
    store.audit("rulesets.mode.updated", "local-admin", serde_json::json!({"mode": mode}));
    Ok(serde_json::json!({"ok": true, "mode": mode}))
}
