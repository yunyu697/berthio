//! 操作审计日志：追加写、按月 + 按大小轮转、按保留天数清理、只读查询。
//!
//! 设计约束（对应需求）：
//! - 只有追加与轮转/保留清理，**没有**删除或清空接口；
//! - 记录字段固定为 ts / actor / ip / action / target / result / detail；
//! - 写入前对 detail 做敏感键脱敏，密码、私钥、订阅密文、uuid、token 都不落盘；
//! - 写入失败绝不影响业务操作，只累加失败计数并记录最近一次错误文本；
//! - 清理只发生在 `<home>/logs/audit` 目录内、且文件名符合本模块命名规则的文件。

use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// 审计日志相对数据目录的位置。
pub const DIR: &str = "logs/audit";
/// 单个日志文件的轮转阈值（默认 8 MB，可用 `BERTHIO_AUDIT_MAX_BYTES` 覆盖）。
pub const DEFAULT_MAX_BYTES: u64 = 8 * 1024 * 1024;
const RETENTION_INTERVAL_SECS: u64 = 24 * 3600;
const MAX_STRING: usize = 200;
const MAX_DEPTH: usize = 6;
const MAX_ARRAY: usize = 30;

static WRITE_FAILURES: AtomicU64 = AtomicU64::new(0);
static LAST_ERROR: Mutex<Option<String>> = Mutex::new(None);
static LAST_RETENTION: AtomicU64 = AtomicU64::new(0);

pub fn max_bytes() -> u64 {
    std::env::var("BERTHIO_AUDIT_MAX_BYTES")
        .ok()
        .and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|v| *v >= 64 * 1024)
        .unwrap_or(DEFAULT_MAX_BYTES)
}

/// 一次性写入失败状态，供面板展示（`/api/audit` 与 `/api/panel`）。
pub fn health() -> Value {
    json!({
        "failures": WRITE_FAILURES.load(Ordering::SeqCst),
        "last_error": LAST_ERROR.lock().unwrap().clone(),
        "max_bytes": max_bytes(),
        "rotation": "按月 + 单文件超过 8 MB 立即轮转；保留天数取 config/panel.json 的 log_keep_days（默认 14）",
    })
}

/// 写入一条审计记录。所有失败都被吞掉，只留痕迹。
pub fn write(
    home: &Path,
    keep_days: u32,
    action: &str,
    actor: &str,
    ip: &str,
    target: &str,
    result: &str,
    detail: Value,
) {
    let stamp = crate::util::now_iso();
    let month = stamp.get(0..7).unwrap_or("0000-00").to_string();
    let dir = home.join(DIR);
    let active = dir.join(format!("{month}.jsonl"));
    if let Ok(meta) = fs::metadata(&active) {
        if meta.len() >= max_bytes() {
            rotate(&active, &month, &stamp);
        }
    }
    let detail = sanitize(&detail, 0);
    let line = json!({
        "ts": stamp,
        // event 保留旧字段名，兼容既有面板与脚本；action 是标准字段。
        "event": clean(action),
        "action": clean(action),
        "actor": clean(actor),
        "ip": clean(ip),
        "target": clean(target),
        "result": clean(result),
        "detail": detail,
    });
    // 单条记录不应无限增长：sanitize 已限制字符串 200 字符、数组 30 项、深度 6 层。
    if let Err(e) = crate::util::append_jsonl(&active, &line) {
        WRITE_FAILURES.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut slot) = LAST_ERROR.lock() {
            *slot = Some(format!("{} {}", crate::util::now_iso(), e));
        }
        eprintln!("[audit] 写入失败（业务操作未受影响）：{e}");
    }
    retention_if_due(&dir, keep_days);
}

/// 从旧调用点的 `detail` 里推导来源 IP。没有就记 `-`，绝不猜。
pub fn derive_ip(detail: &Value) -> String {
    for k in ["ip", "client_ip", "peer_ip"] {
        if let Some(s) = detail.get(k).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return s.to_string();
            }
        }
    }
    "-".to_string()
}

/// 从 detail 推导操作目标；优先级覆盖入站、客户端、内核、文件、令牌。
pub fn derive_target(detail: &Value) -> String {
    for k in [
        "inbound_id",
        "client_id",
        "kernel",
        "file_name",
        "path",
        "token_id",
        "id",
    ] {
        if let Some(s) = detail.get(k).and_then(|v| v.as_str()) {
            if !s.trim().is_empty() {
                return s.to_string();
            }
        }
    }
    "-".to_string()
}

/// 从 detail / 事件名推导结果，避免旧调用点把失败事件记成 ok。
pub fn derive_result(event: &str, detail: &Value) -> String {
    if let Some(s) = detail.get("result").and_then(|v| v.as_str()) {
        if !s.trim().is_empty() {
            return s.to_string();
        }
    }
    let e = event.to_ascii_lowercase();
    if e.contains("failed") {
        "failed".to_string()
    } else if e.contains("denied") || e.contains("blocked") || e.contains("rejected") {
        "denied".to_string()
    } else if e.contains("alert") || e.contains("missing") || e.contains("reused") {
        "failed".to_string()
    } else {
        "ok".to_string()
    }
}

/// 查询条件。空字符串表示不限；`limit` 会被夹到 1..=2000。
#[derive(Debug, Default, Clone)]
pub struct Query {
    pub from: String,
    pub to: String,
    pub actor: String,
    pub action: String,
    pub result: String,
    pub keyword: String,
    pub limit: usize,
}

#[derive(Debug, Default)]
pub struct Outcome {
    pub items: Vec<Value>,
    pub scanned: usize,
    pub files: usize,
}

pub fn query(home: &Path, q: &Query) -> Outcome {
    let dir = home.join(DIR);
    let mut out = Outcome::default();
    let limit = q.limit.clamp(1, 2000);
    let mut files: Vec<(u64, PathBuf)> = Vec::new();
    if let Ok(entries) = fs::read_dir(&dir) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if !is_audit_file(&name) {
                continue;
            }
            let Ok(meta) = e.metadata() else { continue };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
                .unwrap_or(0);
            files.push((mtime, e.path()));
        }
    }
    // 新的文件先读；文件内从尾部（最新）往前读。
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    out.files = files.len();
    'outer: for (_, path) in &files {
        let Ok(raw) = fs::read_to_string(path) else {
            continue;
        };
        for line in raw.lines().rev() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            out.scanned += 1;
            if !matches(&v, q) {
                continue;
            }
            out.items.push(v);
            if out.items.len() >= limit {
                break 'outer;
            }
        }
    }
    out
}

fn matches(v: &Value, q: &Query) -> bool {
    let ts = v.get("ts").and_then(|x| x.as_str()).unwrap_or("");
    if !ts_ge(ts, &q.from) || !ts_le(ts, &q.to) {
        return false;
    }
    if !q.actor.is_empty() {
        let actor = v.get("actor").and_then(|x| x.as_str()).unwrap_or("");
        if !actor.to_ascii_lowercase().contains(&q.actor.to_ascii_lowercase()) {
            return false;
        }
    }
    if !q.action.is_empty() {
        let action = v.get("action").and_then(|x| x.as_str()).unwrap_or("");
        if !action
            .to_ascii_lowercase()
            .contains(&q.action.to_ascii_lowercase())
        {
            return false;
        }
    }
    if !q.result.is_empty() {
        let result = v.get("result").and_then(|x| x.as_str()).unwrap_or("");
        if !result.eq_ignore_ascii_case(q.result.trim()) {
            return false;
        }
    }
    if !q.keyword.is_empty() {
        let line = v.to_string().to_ascii_lowercase();
        if !line.contains(&q.keyword.to_ascii_lowercase()) {
            return false;
        }
    }
    true
}

/// 日期比较：传入 `YYYY-MM-DD` 时按天比较，传入完整时间时按整串比较。
fn ts_ge(ts: &str, bound: &str) -> bool {
    let bound = bound.trim();
    if bound.is_empty() {
        return true;
    }
    if bound.len() > 10 {
        ts >= bound
    } else {
        ts.get(0..10).unwrap_or("") >= bound
    }
}

fn ts_le(ts: &str, bound: &str) -> bool {
    let bound = bound.trim();
    if bound.is_empty() {
        return true;
    }
    if bound.len() > 10 {
        ts <= bound
    } else {
        ts.get(0..10).unwrap_or("") <= bound
    }
}

/// 轮转：把当前月文件改名成 `YYYY-MM.rot-<秒>-<n>.jsonl`，下一条写入落到新文件。
fn rotate(active: &Path, month: &str, stamp: &str) {
    let Some(dir) = active.parent() else { return };
    let secs = stamp
        .get(11..19)
        .map(|s| s.replace(':', ""))
        .unwrap_or_else(|| "0".to_string());
    let mut n = 0u32;
    let mut candidate = dir.join(format!("{month}.rot-{secs}-{n}.jsonl"));
    while candidate.exists() {
        n += 1;
        if n > 9999 {
            return;
        }
        candidate = dir.join(format!("{month}.rot-{secs}-{n}.jsonl"));
    }
    let _ = fs::rename(active, candidate);
}

/// 保留策略：最多每 24 小时清理一次；只删本目录内、符合命名规则的普通文件。
fn retention_if_due(dir: &Path, keep_days: u32) {
    let now = crate::util::now_secs();
    if now == 0 {
        return;
    }
    let last = LAST_RETENTION.load(Ordering::SeqCst);
    if last != 0 && now.saturating_sub(last) < RETENTION_INTERVAL_SECS {
        return;
    }
    // 用 CAS 避免并发多条写入重复清理。
    if LAST_RETENTION
        .compare_exchange(last, now, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    let keep = keep_days.max(1) as u64;
    let cutoff = now.saturating_sub(keep * 86_400);
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().to_string();
        if !is_audit_file(&name) {
            continue;
        }
        let Ok(meta) = e.metadata() else { continue };
        if !meta.is_file() {
            continue;
        }
        let mtime = meta
            .modified()
            .ok()
            .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // 时间拿不到就不删，宁可留着也不误删。
        if mtime > 0 && mtime < cutoff && e.path().parent() == Some(dir) {
            if let Err(err) = fs::remove_file(e.path()) {
                eprintln!("[audit] 清理过期日志失败 {}: {err}", e.path().display());
            }
        }
    }
}

/// 只认本模块自己生成的文件名：`YYYY-MM.jsonl` / `YYYY-MM.rot-HHMMSS-n.jsonl`。
fn is_audit_file(name: &str) -> bool {
    let Some(stem) = name.strip_suffix(".jsonl") else {
        return false;
    };
    let b = stem.as_bytes();
    if b.len() < 7 || !b[0..4].iter().all(|c| c.is_ascii_digit()) || b[4] != b'-' {
        return false;
    }
    if !b[5..7].iter().all(|c| c.is_ascii_digit()) {
        return false;
    }
    // 不允许任何路径分隔符或不可见字符。
    stem.chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
}

fn clean(s: &str) -> String {
    let filtered: String = s
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_STRING)
        .collect();
    filtered.trim().to_string()
}

fn is_sensitive_key(key: &str) -> bool {
    let k = key.to_ascii_lowercase();
    // 标识符不是秘密：token_id / client_id 可以保留，便于追溯。
    if k.ends_with("_id") || k.ends_with("_path") || k == "id" {
        return false;
    }
    if k == "key" || k.ends_with("_key") || k.ends_with("key") {
        return true;
    }
    [
        "password",
        "passwd",
        "secret",
        "token",
        "private",
        "credential",
        "uuid",
        "cookie",
        "authorization",
        "subscription_url",
        "sub_url",
    ]
    .iter()
    .any(|s| k.contains(s))
}

fn sanitize(value: &Value, depth: usize) -> Value {
    if depth > MAX_DEPTH {
        return Value::String("[depth-limit]".to_string());
    }
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                if is_sensitive_key(k) {
                    out.insert(k.clone(), Value::String("[redacted]".to_string()));
                } else {
                    out.insert(k.clone(), sanitize(v, depth + 1));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => {
            let mut out = Vec::new();
            for v in arr.iter().take(MAX_ARRAY) {
                out.push(sanitize(v, depth + 1));
            }
            if arr.len() > MAX_ARRAY {
                out.push(Value::String(format!(
                    "[truncated {} more]",
                    arr.len() - MAX_ARRAY
                )));
            }
            Value::Array(out)
        }
        Value::String(s) => Value::String(clean(s)),
        other => other.clone(),
    }
}

#[cfg(test)] // 审计脱敏与文件名规则的单测
mod tests {
    use super::*;

    #[test]
    fn sensitive_keys_are_redacted() {
        let detail = json!({
            "password": "hunter2",
            "tls_key": "-----BEGIN PRIVATE KEY-----",
            "token": "bto_secret",
            "subscription_url": "https://x/sub/bto_abc",
            "token_id": "tok-1",
            "client_uuid": "11111111-2222",
            "nested": {"secret_key": "abc", "path": "/tmp/x", "keep": "ok"},
            "arr": [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]
        });
        let s = sanitize(&detail, 0);
        let text = s.to_string();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("BEGIN PRIVATE"));
        assert!(!text.contains("bto_secret"));
        assert!(!text.contains("11111111"));
        assert!(text.contains("tok-1"));
        assert!(text.contains("/tmp/x"));
        assert!(text.contains("[truncated 2 more]"));
    }

    #[test]
    fn audit_file_name_rule_rejects_path_escape() {
        assert!(is_audit_file("2026-11.jsonl"));
        assert!(is_audit_file("2026-11.rot-120000-0.jsonl"));
        assert!(!is_audit_file("../../state.json"));
        assert!(!is_audit_file("2026-11.json"));
        assert!(!is_audit_file("2026-11/evil.jsonl"));
        assert!(!is_audit_file("20261-1.jsonl"));
    }

    #[test]
    fn result_derivation() {
        assert_eq!(derive_result("auth.login.failed", &json!({})), "failed");
        assert_eq!(derive_result("sub.blocked", &json!({})), "denied");
        assert_eq!(derive_result("auth.login", &json!({})), "ok");
        assert_eq!(
            derive_result("x", &json!({"result": "denied"})),
            "denied"
        );
    }

    #[test]
    fn date_filter_compares_day_or_full_ts() {
        assert!(ts_ge("2026-11-07T10:00:00Z", "2026-11-07"));
        assert!(!ts_ge("2026-11-06T10:00:00Z", "2026-11-07"));
        assert!(ts_le("2026-11-07T10:00:00Z", "2026-11-07"));
        assert!(ts_ge("2026-11-07T10:00:00Z", "2026-11-07T09:00:00Z"));
        assert!(ts_ge("2026-11-07T10:00:00Z", ""));
        assert!(ts_le("2026-11-07T10:00:00Z", ""));
    }
}
