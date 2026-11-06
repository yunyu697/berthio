//! 远程访问鉴权（M3）。
//!
//! 设计约束（见 DECISIONS Q39–Q43）：
//! - 单管理员；口令只存 PBKDF2-HMAC-SHA256（随机盐 + 迭代次数），不落明文、不进日志。
//! - 会话令牌 32 字节随机数，仅通过 HttpOnly + SameSite=Strict Cookie 传输；CSRF 用独立随机令牌 +
//!   自定义请求头（双提交），写操作必须同时具备会话与 CSRF。
//! - 登录失败按客户端 IP 计数并临时锁定；成功后清零。
//! - 默认只监听回环；开启远程监听需要显式确认（见 http 的 PATCH /api/panel）。
//! - 反代转发头只在 peer 属于 `trusted_proxies` 时才信任，默认不信任任何 X-Forwarded-*。

use crate::error::{ApiError, ApiResult};
use crate::util;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

const SCHEMA: u32 = 1;
const DEFAULT_ITERATIONS: u32 = 120_000;
const DEFAULT_SESSION_TTL: u64 = 12 * 3600;
const DEFAULT_MAX_FAILS: u32 = 5;
const DEFAULT_LOCK_SECS: u64 = 300;
const FAIL_WINDOW_SECS: u64 = 900;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PasswordRecord {
    pub algo: String,
    pub iterations: u32,
    pub salt: String,
    pub hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthConfig {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub initialized: bool,
    #[serde(default)]
    pub record: Option<PasswordRecord>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

fn default_schema() -> u32 {
    SCHEMA
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA,
            initialized: false,
            record: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SessionView {
    pub actor: String,
}

struct Session {
    csrf: String,
    expires_at: u64,
}

#[derive(Debug, Default, Clone)]
struct FailState {
    fails: u32,
    last: u64,
    locked_until: u64,
}

struct Inner {
    config: AuthConfig,
    sessions: HashMap<String, Session>,
    failures: HashMap<String, FailState>,
}

pub struct Auth {
    path: PathBuf,
    inner: Mutex<Inner>,
    demo: bool,
    session_ttl: u64,
    max_fails: u32,
    lock_secs: u64,
}

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &u64| *v > 0)
        .unwrap_or(default)
}

fn hm0(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        let d = Sha256::digest(key);
        k[..32].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; 64];
    let mut opad = [0x5cu8; 64];
    for i in 0..64 {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(ih);
    let out = outer.finalize();
    let mut a = [0u8; 32];
    a.copy_from_slice(&out);
    a
}

/// PBKDF2-HMAC-SHA256（只使用一个输出块，足够 256 位）。
pub fn pbkdf2(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut first = Vec::new();
    first.extend_from_slice(salt);
    first.extend_from_slice(&[0, 0, 0, 1]);
    let mut u = hm0(password, &first);
    let mut out = u;
    for _ in 1..iterations.max(1) {
        u = hm0(password, &u);
        for i in 0..32 {
            out[i] ^= u[i];
        }
    }
    out
}

fn ct_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.as_bytes().iter().zip(b.as_bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

fn password_problem(password: &str) -> Option<ApiError> {
    let chars = password.chars().count();
    if chars < 10 {
        return Some(
            ApiError::bad_request("口令至少 10 个字符").with_field("password", "至少 10 个字符"),
        );
    }
    if chars > 256 {
        return Some(ApiError::bad_request("口令过长").with_field("password", "最多 256 个字符"));
    }
    let has_letter = password.chars().any(|c| c.is_alphabetic());
    let has_digit = password.chars().any(|c| c.is_ascii_digit());
    if !has_letter || !has_digit {
        return Some(
            ApiError::bad_request("口令需同时包含字母与数字")
                .with_field("password", "例如：Berthio#2026"),
        );
    }
    None
}

fn hash_password(password: &str, iterations: u32) -> PasswordRecord {
    let salt = util::random_hex(16);
    let hash = util::hex(&pbkdf2(password.as_bytes(), salt.as_bytes(), iterations));
    PasswordRecord {
        algo: "pbkdf2-hmac-sha256".into(),
        iterations,
        salt,
        hash,
    }
}

fn verify_password(record: &PasswordRecord, password: &str) -> bool {
    let got = util::hex(&pbkdf2(
        password.as_bytes(),
        record.salt.as_bytes(),
        record.iterations,
    ));
    ct_eq(&got, &record.hash)
}

impl Auth {
    pub fn new(home: &Path) -> ApiResult<Self> {
        let path = home.join("state/auth.json");
        let config = if path.exists() {
            util::read_json(&path)
                .and_then(|v| {
                    serde_json::from_value::<AuthConfig>(v).map_err(|e| format!("auth.json 解析失败: {e}"))
                })
                .map_err(ApiError::internal)?
        } else {
            AuthConfig::default()
        };
        Ok(Self {
            path,
            inner: Mutex::new(Inner {
                config,
                sessions: HashMap::new(),
                failures: HashMap::new(),
            }),
            demo: false,
            session_ttl: env_u64("BERTHIO_SESSION_TTL_SECS", DEFAULT_SESSION_TTL),
            max_fails: env_u64("BERTHIO_LOGIN_MAX_FAILS", DEFAULT_MAX_FAILS as u64) as u32,
            lock_secs: env_u64("BERTHIO_LOGIN_LOCK_SECS", DEFAULT_LOCK_SECS),
        })
    }

    pub fn demo() -> Self {
        Self {
            path: PathBuf::new(),
            inner: Mutex::new(Inner {
                config: AuthConfig {
                    initialized: true,
                    ..AuthConfig::default()
                },
                sessions: HashMap::new(),
                failures: HashMap::new(),
            }),
            demo: true,
            session_ttl: DEFAULT_SESSION_TTL,
            max_fails: DEFAULT_MAX_FAILS,
            lock_secs: DEFAULT_LOCK_SECS,
        }
    }

    pub fn session_ttl(&self) -> u64 {
        self.session_ttl
    }

    fn persist(&self, config: &AuthConfig) -> ApiResult<()> {
        let body = serde_json::to_string_pretty(config).map_err(|e| ApiError::internal(e.to_string()))?;
        util::write_atomic(&self.path, body.as_bytes()).map_err(ApiError::internal)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&self.path) {
                let mut p = meta.permissions();
                p.set_mode(0o600);
                let _ = std::fs::set_permissions(&self.path, p);
            }
        }
        Ok(())
    }

    pub fn initialized(&self) -> bool {
        if self.demo {
            return true;
        }
        self.inner.lock().unwrap().config.initialized
    }

    /// 公开状态：未初始化时只暴露 initialized=false，不泄露任何哈希。
    pub fn status(&self, token: Option<&str>, peer: &str) -> serde_json::Value {
        if self.demo {
            return serde_json::json!({
                "initialized": true,
                "authenticated": true,
                "demo": true,
                "actor": "demo-admin",
                "csrf_token": "demo-csrf",
                "session_ttl_seconds": self.session_ttl,
                "note": "演示模式：鉴权逻辑与真实模式同构，但不校验口令、不落盘会话"
            });
        }
        let mut inner = self.inner.lock().unwrap();
        let initialized = inner.config.initialized;
        let mut authenticated = false;
        let mut csrf = String::new();
        let mut actor = String::new();
        let mut expires = 0u64;
        if let Some(t) = token {
            if let Some(sess) = lookup(&mut inner, t, peer) {
                authenticated = true;
                csrf = sess.csrf.clone();
                actor = "admin".to_string();
                expires = sess.expires_at;
            }
        }
        serde_json::json!({
            "initialized": initialized,
            "authenticated": authenticated,
            "demo": false,
            "actor": actor,
            "csrf_token": if authenticated { csrf } else { String::new() },
            "session_ttl_seconds": self.session_ttl,
            "expires_at": if authenticated { util::unix_to_iso(expires) } else { String::new() },
        })
    }

    pub fn setup(&self, password: &str, confirm: &str) -> ApiResult<serde_json::Value> {
        if password_problem(password).is_some() {
            return Err(password_problem(password).unwrap());
        }
        if password != confirm {
            return Err(ApiError::bad_request("两次输入的口令不一致")
                .with_field("confirm", "与口令不一致"));
        }
        let mut inner = self.inner.lock().unwrap();
        if inner.config.initialized {
            return Err(ApiError::conflict("管理员口令已设置；如需修改请登录后在设置页更新"));
        }
        let now = util::now_iso();
        inner.config = AuthConfig {
            schema_version: SCHEMA,
            initialized: true,
            record: Some(hash_password(password, DEFAULT_ITERATIONS)),
            created_at: now.clone(),
            updated_at: now,
        };
        let cfg = inner.config.clone();
        drop(inner);
        self.persist(&cfg)?;
        Ok(serde_json::json!({
            "ok": true,
            "initialized": true,
            "password_storage": "PBKDF2-HMAC-SHA256（随机盐 + 迭代），只存哈希，不存明文",
            "iterations": DEFAULT_ITERATIONS,
        }))
    }

    pub fn login(&self, password: &str, peer: &str) -> ApiResult<(serde_json::Value, String, String)> {
        let mut inner = self.inner.lock().unwrap();
        if !inner.config.initialized {
            return Err(ApiError::new(409, "setup_required", "尚未设置管理员口令")
                .with_field("action", "先完成首次初始化"));
        }
        let now = util::now_secs();
        let fail = inner.failures.get(peer).cloned().unwrap_or_default();
        if fail.locked_until > now {
            let secs = fail.locked_until - now;
            return Err(ApiError::new(
                429,
                "login_locked",
                format!("登录失败次数过多，已临时锁定，请在 {secs} 秒后重试"),
            )
            .with_field("retry_after_seconds", secs.to_string())
            .with_field("locked_until", util::unix_to_iso(fail.locked_until)));
        }
        let record = inner.config.record.clone();
        let ok = record
            .as_ref()
            .map(|r| verify_password(r, password))
            .unwrap_or(false);
        if !ok {
            let fs = inner.failures.entry(peer.to_string()).or_default();
            if now.saturating_sub(fs.last) > FAIL_WINDOW_SECS {
                fs.fails = 0;
            }
            fs.fails += 1;
            fs.last = now;
            let fails = fs.fails;
            if fails >= self.max_fails {
                fs.locked_until = util::now_secs() + self.lock_secs;
            }
            let locked = inner
                .failures
                .get(peer)
                .map(|f| f.locked_until > util::now_secs())
                .unwrap_or(false);
            if locked {
                return Err(ApiError::new(429, "login_locked", "登录失败次数过多，已临时锁定")
                    .with_field("retry_after_seconds", self.lock_secs.to_string())
                    .with_field("failed_attempts", fails.to_string()));
            }
            return Err(ApiError::new(401, "invalid_credentials", "口令错误")
                .with_field("failed_attempts", fails.to_string())
                .with_field("remaining", (self.max_fails - fails).to_string()));
        }
        inner.failures.remove(peer);
        let token = util::random_hex(32);
        let csrf = util::random_hex(24);
        let expires_at = util::now_secs() + self.session_ttl;
        let key = util::sha256_hex(token.as_bytes());
        inner.sessions.insert(
            key,
            Session {
                csrf: csrf.clone(),
                expires_at,
            },
        );
        Ok((
            serde_json::json!({
                "ok": true,
                "authenticated": true,
                "actor": "admin",
                "csrf_token": csrf,
                "expires_at": util::unix_to_iso(expires_at),
                "ttl_seconds": self.session_ttl,
            }),
            token,
            csrf,
        ))
    }

    pub fn logout(&self, token: &str) -> ApiResult<serde_json::Value> {
        if self.demo {
            return Ok(serde_json::json!({"ok": true, "demo": true}));
        }
        let key = util::sha256_hex(token.as_bytes());
        let mut inner = self.inner.lock().unwrap();
        let removed = inner.sessions.remove(&key).is_some();
        Ok(serde_json::json!({"ok": true, "session_revoked": removed}))
    }

    pub fn require(&self, token: Option<&str>, peer: &str) -> ApiResult<SessionView> {
        if self.demo {
            return Ok(SessionView {
                actor: "demo-admin".into(),
            });
        }
        let Some(t) = token else {
            return Err(ApiError::new(401, "unauthenticated", "需要登录")
                .with_field("action", "先通过 /api/auth/login 登录"));
        };
        let mut inner = self.inner.lock().unwrap();
        lookup(&mut inner, t, peer).ok_or_else(|| {
            ApiError::new(401, "session_invalid", "会话不存在或已过期").with_field("action", "重新登录")
        })?;
        Ok(SessionView {
            actor: "admin".into(),
        })
    }

    pub fn check_csrf(&self, token: &str, header: Option<&str>, peer: &str) -> ApiResult<()> {
        if self.demo {
            return Ok(());
        }
        let Some(h) = header else {
            return Err(ApiError::new(403, "csrf_required", "写操作缺少 CSRF 令牌")
                .with_field("header", "X-Berthio-CSRF"));
        };
        let mut inner = self.inner.lock().unwrap();
        let sess = lookup(&mut inner, token, peer).ok_or_else(|| {
            ApiError::new(401, "session_invalid", "会话不存在或已过期")
        })?;
        if !ct_eq(h, &sess.csrf) {
            return Err(ApiError::new(403, "csrf_invalid", "CSRF 令牌不匹配"));
        }
        Ok(())
    }

    pub fn change_password(&self, current: &str, new: &str, confirm: &str, peer: &str) -> ApiResult<serde_json::Value> {
        if password_problem(new).is_some() {
            return Err(password_problem(new).unwrap());
        }
        if new != confirm {
            return Err(ApiError::bad_request("两次输入的新口令不一致")
                .with_field("confirm", "与新口令不一致"));
        }
        let now = util::now_secs();
        let fail = {
            let inner = self.inner.lock().unwrap();
            inner.failures.get(peer).cloned().unwrap_or_default()
        };
        if fail.locked_until > now {
            return Err(ApiError::new(429, "login_locked", "尝试次数过多，已临时锁定"));
        }
        let mut inner = self.inner.lock().unwrap();
        let Some(record) = inner.config.record.clone() else {
            return Err(ApiError::new(409, "setup_required", "尚未设置管理员口令"));
        };
        if !verify_password(&record, current) {
            let fs = inner.failures.entry(peer.to_string()).or_default();
            fs.fails += 1;
            fs.last = now;
            if fs.fails >= self.max_fails {
                fs.locked_until = util::now_secs() + self.lock_secs;
            }
            return Err(ApiError::new(401, "invalid_credentials", "当前口令错误"));
        }
        let revoked = inner.sessions.len();
        inner.sessions.clear();
        inner.config.record = Some(hash_password(new, DEFAULT_ITERATIONS));
        inner.config.updated_at = util::now_iso();
        let cfg = inner.config.clone();
        drop(inner);
        self.persist(&cfg)?;
        Ok(serde_json::json!({
            "ok": true,
            "sessions_revoked": revoked,
            "relogin_required": true,
            "note": "口令已更新；所有会话立即失效，请重新登录",
        }))
    }
}

fn lookup<'a>(inner: &'a mut Inner, token: &str, peer: &str) -> Option<&'a Session> {
    let key = util::sha256_hex(token.as_bytes());
    let now = util::now_secs();
    let expired = inner
        .sessions
        .get(&key)
        .map(|s| s.expires_at <= now)
        .unwrap_or(false);
    if expired {
        inner.sessions.remove(&key);
        return None;
    }
    let _ = peer;
    if inner.sessions.contains_key(&key) {
        return inner.sessions.get(&key);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pbkdf2_matches_reference_vector() {
        // 参考值由 Python hashlib.pbkdf2_hmac('sha256', b'password', b'salt', 1) 计算。
        let out = pbkdf2(b"password", b"salt", 1);
        assert_eq!(
            util::hex(&out),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
    }

    #[test]
    fn password_roundtrip() {
        let rec = hash_password("Berthio#2026", 1000);
        assert!(verify_password(&rec, "Berthio#2026"));
        assert!(!verify_password(&rec, "berthio#2026"));
        assert_ne!(rec.hash, "Berthio#2026");
    }

    #[test]
    fn password_policy() {
        assert!(password_problem("short").is_some());
        assert!(password_problem("abcdefghij").is_some());
        assert!(password_problem("Berthio2026").is_none());
        assert!(password_problem("Berthio#2026").is_none());
    }
}
