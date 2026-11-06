//! 面板自更新：检查清单、多源回退、minisign 完整性校验、下载/仅下载、替换并重启。
//!
//! 口径与 Renomio 一致（交互与容错，不复用其实现）：
//! - 源顺序：所选镜像 → 官方 → 其余镜像；逐源记录失败原因。
//! - 完整性 fail-closed：没有可用公钥、签名无效、版本/文件名与签名绑定不符，一律拒绝。
//! - 下载先写 `.part`，失败或换源立即删除；启动时清理残留；安装前二次校验。
//! - 「仅下载」落到用户下载目录，保留签名旁挂文件；「更新」在临时目录校验后替换当前二进制并重启。

use crate::state::{UpdateConfig, UpdateMirror};
use crate::util;
use base64::Engine;
use minisign_verify::{PublicKey, Signature};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub const HOMEPAGE: &str = "https://github.com/yunyu697/berthio";
pub const DEFAULT_FEED: &str =
    "https://github.com/yunyu697/berthio/releases/download/updates/latest.json";

/// base64(minisign 公钥文件全文)。私钥只在本机 `Documents\Trae-CN\berthio-signing`，不入仓库。
pub const BUILTIN_PUBKEY_B64: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDIxRjVBNTFDQTVGQjJFQTQKUldTa0x2dWxIS1gxSVQ0UGl4R3VMZEFmMkhWcEFRdk9nU3lidThGNDV4bEY0VE02Q2tZZ3ovOVkK";

pub const MAX_UPDATE_BYTES: u64 = 128 * 1024 * 1024;
const MANIFEST_TIMEOUT: Duration = Duration::from_secs(30);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(900);

pub const DEFAULT_MIRRORS: [(&str, &str); 3] = [
    ("ghproxy", "https://ghproxy.net/"),
    ("gh-proxy", "https://gh-proxy.com/"),
    ("ghfast", "https://ghfast.top/"),
];

/* ------------------------------------------------------------------ 版本号 */

/// 本仓库版本规则：`X.Y.Z` 或 `X.Y.Z-alphaN`；除首段外每段 ≤10；
/// alpha 从 1 起、无前导零。比较时 alpha < 正式版。
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub alpha: Option<u64>,
}

impl Ver {
    pub fn parse(raw: &str) -> Result<Self, String> {
        let text = raw.trim();
        if text.is_empty() {
            return Err("版本号为空".into());
        }
        let (base, alpha) = match text.split_once('-') {
            Some((b, pre)) => {
                let n = pre
                    .strip_prefix("alpha")
                    .ok_or_else(|| format!("版本号 {text} 的预发布段只能是 alphaN"))?;
                if n.is_empty() || !n.bytes().all(|c| c.is_ascii_digit()) {
                    return Err(format!("版本号 {text} 的 alpha 序号必须是数字"));
                }
                if n.len() > 1 && n.starts_with('0') {
                    return Err(format!("版本号 {text} 的 alpha 序号不能有前导零"));
                }
                let n: u64 = n
                    .parse()
                    .map_err(|_| format!("版本号 {text} 的 alpha 序号超出范围"))?;
                if n < 1 {
                    return Err(format!("版本号 {text} 的 alpha 序号必须从 1 开始"));
                }
                (b, Some(n))
            }
            None => (text, None),
        };
        let parts: Vec<&str> = base.split('.').collect();
        if parts.len() != 3 {
            return Err(format!("版本号 {text} 必须是 X.Y.Z 或 X.Y.Z-alphaN"));
        }
        let mut nums = [0u64; 3];
        for (i, part) in parts.iter().enumerate() {
            if part.is_empty() || !part.bytes().all(|c| c.is_ascii_digit()) {
                return Err(format!("版本号 {text} 含非数字段"));
            }
            if part.len() > 1 && part.starts_with('0') {
                return Err(format!("版本号 {text} 含前导零"));
            }
            nums[i] = part
                .parse()
                .map_err(|_| format!("版本号 {text} 的数值段超出范围"))?;
        }
        if nums[1] > 10 {
            return Err(format!("版本号 {text} 的 MINOR={} 超过上限 10", nums[1]));
        }
        if nums[2] > 10 {
            return Err(format!("版本号 {text} 的 PATCH={} 超过上限 10", nums[2]));
        }
        Ok(Ver {
            major: nums[0],
            minor: nums[1],
            patch: nums[2],
            alpha,
        })
    }

    /// 正式版 > alpha；alpha 序号越大越新。
    pub fn newer_than(&self, other: &Ver) -> bool {
        if self.major != other.major {
            return self.major > other.major;
        }
        if self.minor != other.minor {
            return self.minor > other.minor;
        }
        if self.patch != other.patch {
            return self.patch > other.patch;
        }
        match (self.alpha, other.alpha) {
            (None, None) => false,
            (None, Some(_)) => true,
            (Some(_), None) => false,
            (Some(a), Some(b)) => a > b,
        }
    }
}

pub fn is_newer(remote: &str, current: &str) -> Result<bool, String> {
    let remote = Ver::parse(remote)?;
    let current = Ver::parse(current)?;
    Ok(remote.newer_than(&current))
}

/* ------------------------------------------------------------------ 清单 */

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    #[serde(default)]
    pub pub_date: String,
    #[serde(default)]
    pub platforms: BTreeMap<String, PlatformAsset>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct PlatformAsset {
    /// base64(.minisig 全文)
    pub signature: String,
    pub url: String,
    #[serde(default)]
    pub size: Option<u64>,
}

pub fn platform_key() -> &'static str {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        "windows-x86_64"
    }
    #[cfg(all(target_os = "windows", target_arch = "aarch64"))]
    {
        "windows-aarch64"
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        "linux-x86_64"
    }
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    {
        "linux-aarch64"
    }
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "aarch64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    {
        "unsupported"
    }
}

fn pick_asset(manifest: &Manifest) -> Result<PlatformAsset, String> {
    manifest
        .platforms
        .get(platform_key())
        .cloned()
        .ok_or_else(|| format!("更新清单里没有当前平台（{}）的安装包", platform_key()))
}

pub fn asset_file_name(url: &str) -> String {
    let raw = url.split(['?', '#']).next().unwrap_or(url);
    let raw = raw.rsplit('/').next().unwrap_or("");
    let cleaned: String = raw
        .chars()
        .map(|c| {
            if c.is_control() || "\\/:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('.').to_string();
    if cleaned.is_empty() {
        "berthio-update.bin".to_string()
    } else if cleaned.chars().count() > 120 {
        let ext = Path::new(&cleaned)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_default();
        let stem: String = cleaned
            .chars()
            .take(120usize.saturating_sub(ext.chars().count()))
            .collect();
        format!("{stem}{ext}")
    } else {
        cleaned
    }
}

/* ------------------------------------------------------------------ 多源 */

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attempt {
    pub source: String,
    pub url: String,
    pub ok: bool,
    pub error: String,
}

/// 候选源顺序：所选镜像 → 官方 → 其余镜像。镜像前缀可指到私有反代，故对所有地址生效。
pub fn url_candidates(feed: &str, mirror: &str, mirrors: &[UpdateMirror]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut push = |source: String, url: String| {
        if !out.iter().any(|(_, existing)| existing == &url) {
            out.push((source, url));
        }
    };
    let selected = mirrors.iter().find(|m| m.id == mirror);
    if let Some(m) = selected {
        push(m.id.clone(), format!("{}{}", m.prefix, feed));
    }
    push("official".into(), feed.to_string());
    for m in mirrors {
        if Some(&m.id) == selected.map(|s| &s.id) {
            continue;
        }
        push(m.id.clone(), format!("{}{}", m.prefix, feed));
    }
    let display = |source: &str| -> String {
        if source == "official" {
            "官方源".to_string()
        } else {
            match mirrors.iter().find(|m| m.id == source) {
                Some(m) => format!("镜像 {}", m.id),
                None => source.to_string(),
            }
        }
    };
    out.into_iter().map(|(s, u)| (display(&s), u)).collect()
}

/* ------------------------------------------------------------------ 校验 */

fn b64_decode(text: &str) -> Result<Vec<u8>, String> {
    base64::engine::general_purpose::STANDARD
        .decode(text.trim())
        .map_err(|e| format!("base64 解码失败：{e}"))
}

fn quote_field<'a>(comment: &'a str, key: &str) -> Option<&'a str> {
    comment
        .split(['\t', ' ', '\n'])
        .find_map(|part| part.strip_prefix(key)?.strip_prefix(':'))
        .map(str::trim)
}

/// 有效公钥：配置覆盖 > 内置。返回原文（minisign 公钥文件内容）；空 = 不可用。
fn effective_pubkey(cfg: &UpdateConfig) -> Result<String, String> {
    let raw = if cfg.pubkey.trim().is_empty() {
        BUILTIN_PUBKEY_B64
    } else {
        cfg.pubkey.trim()
    };
    if raw.is_empty() {
        return Err("未配置更新公钥，已拒绝：完整性校验不可跳过".into());
    }
    let text =
        String::from_utf8(b64_decode(raw)?).map_err(|_| "更新公钥不是 UTF-8 文本".to_string())?;
    Ok(text)
}

/// fail-closed：签名有效 + 签名绑定的版本/文件名与清单一致，缺一不可。
pub fn verify_bytes(
    data: &[u8],
    signature_b64: &str,
    expected_version: &str,
    expected_name: &str,
    pubkey_text: &str,
) -> Result<(), String> {
    if pubkey_text.trim().is_empty() {
        return Err("未配置更新公钥，已拒绝：完整性校验不可跳过".into());
    }
    let public_key = PublicKey::decode(pubkey_text).map_err(|e| format!("更新公钥无效：{e}"))?;
    let sig_text = String::from_utf8(b64_decode(signature_b64)?)
        .map_err(|_| "更新签名不是 UTF-8 文本".to_string())?;
    let signature = Signature::decode(&sig_text).map_err(|e| format!("更新签名格式无效：{e}"))?;
    public_key
        .verify(data, &signature, false)
        .map_err(|_| "安装包签名校验失败：文件内容与签名不匹配".to_string())?;
    let comment = signature.trusted_comment();
    let version =
        quote_field(comment, "version").ok_or_else(|| "签名没有绑定版本号，已拒绝".to_string())?;
    if version != expected_version {
        return Err(format!(
            "签名绑定的版本是 {version}，与清单版本 {expected_version} 不一致，已拒绝"
        ));
    }
    let name =
        quote_field(comment, "file").ok_or_else(|| "签名没有绑定文件名，已拒绝".to_string())?;
    if name != expected_name {
        return Err(format!(
            "签名绑定的文件名是 {name}，与清单里的 {expected_name} 不一致，已拒绝"
        ));
    }
    Ok(())
}

/* ------------------------------------------------------------------ 状态 */

#[derive(Clone, Debug, Serialize)]
pub struct Progress {
    pub phase: String,
    pub received: u64,
    pub total: u64,
    pub percent: f64,
    pub source: String,
    pub attempt: u32,
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedUpdate {
    pub path: String,
    pub version: String,
    pub source: String,
    pub size: u64,
    pub asset_name: String,
    pub signature_b64: String,
    pub sha256: String,
    pub saved_at: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LastInstall {
    pub version: String,
    pub status: String,
    pub at: String,
    #[serde(default)]
    pub restarted_at: String,
}

#[derive(Default, Serialize, Deserialize)]
struct DiskState {
    #[serde(default)]
    saved: Option<SavedUpdate>,
    #[serde(default)]
    last_install: Option<LastInstall>,
}

#[derive(Clone, Debug, Serialize)]
struct CheckState {
    available: bool,
    version: String,
    notes: String,
    pub_date: String,
    source: String,
    asset_name: String,
    asset_size: Option<u64>,
}

struct Inner {
    busy: bool,
    phase: String,
    progress: Option<Progress>,
    manifest: Option<Manifest>,
    check: Option<CheckState>,
    attempts: Vec<Attempt>,
    error: Option<String>,
    saved: Option<SavedUpdate>,
    last_install: Option<LastInstall>,
}

pub struct Manager {
    home: PathBuf,
    current: String,
    demo: bool,
    inner: Mutex<Inner>,
}

impl Manager {
    pub fn new(home: &Path, current: &str, demo: bool) -> Self {
        let mut disk = read_disk(home);
        cleanup_parts(home);
        cleanup_old_exe();
        let mut cleared_saved = false;
        if let Some(li) = disk.last_install.as_mut() {
            if li.status == "started" {
                li.status = "restarted".into();
                li.restarted_at = util::now_iso();
                if let Some(saved) = disk.saved.as_ref() {
                    if saved.version == li.version {
                        cleared_saved = true;
                    }
                }
            }
        }
        if cleared_saved {
            disk.saved = None;
        }
        let manager = Manager {
            home: home.to_path_buf(),
            current: current.to_string(),
            demo,
            inner: Mutex::new(Inner {
                busy: false,
                phase: "idle".into(),
                progress: None,
                manifest: None,
                check: None,
                attempts: Vec::new(),
                error: None,
                saved: disk.saved.clone(),
                last_install: disk.last_install.clone(),
            }),
        };
        if cleared_saved || disk.last_install.is_some() {
            let _ = persist_disk(home, &disk);
        }
        manager
    }

    pub fn status(&self, cfg: &UpdateConfig) -> Value {
        let g = self.inner.lock().unwrap();
        let check = g.check.as_ref();
        json!({
            "current_version": self.current,
            "homepage": HOMEPAGE,
            "feed": cfg.feed,
            "mirror": cfg.mirror,
            "mirrors": cfg.mirrors.iter().map(|m| json!({"id": m.id, "prefix": m.prefix})).collect::<Vec<_>>(),
            "has_pubkey": !cfg.pubkey.trim().is_empty() || !BUILTIN_PUBKEY_B64.is_empty(),
            "pubkey_source": if cfg.pubkey.trim().is_empty() { "builtin" } else { "config" },
            "install_supported": cfg!(any(target_os = "windows", target_os = "linux")),
            "demo": self.demo,
            "phase": g.phase,
            "busy": g.busy,
            "checked": check.is_some(),
            "available": check.map(|c| c.available),
            "version": check.map(|c| c.version.clone()),
            "notes": check.map(|c| c.notes.clone()),
            "pub_date": check.map(|c| c.pub_date.clone()),
            "source": check.map(|c| c.source.clone()),
            "asset_name": check.map(|c| c.asset_name.clone()),
            "asset_size": check.and_then(|c| c.asset_size),
            "progress": g.progress,
            "attempts": g.attempts,
            "error": g.error,
            "saved": g.saved,
            "last_install": g.last_install,
        })
    }

    pub fn start_check(self: &Arc<Self>, cfg: UpdateConfig) -> Result<(), String> {
        {
            let mut g = self.inner.lock().unwrap();
            if g.busy {
                return Err("已有更新任务在进行，请稍候".into());
            }
            g.busy = true;
            g.phase = "checking".into();
            g.error = None;
            g.attempts.clear();
            g.manifest = None;
            g.check = None;
            g.progress = Some(Progress {
                phase: "checking".into(),
                received: 0,
                total: 0,
                percent: 0.0,
                source: String::new(),
                attempt: 0,
                message: "读取更新清单".into(),
            });
        }
        let m = Arc::clone(self);
        std::thread::spawn(move || run_check(m, cfg));
        Ok(())
    }

    pub fn start_download(self: &Arc<Self>, cfg: UpdateConfig, save: bool) -> Result<(), String> {
        {
            let g = self.inner.lock().unwrap();
            if g.busy {
                return Err("已有更新任务在进行，请稍候".into());
            }
        }
        let m = Arc::clone(self);
        {
            let mut g = self.inner.lock().unwrap();
            g.busy = true;
            g.phase = "downloading".into();
            g.error = None;
            g.attempts.clear();
            g.progress = Some(Progress {
                phase: "downloading".into(),
                received: 0,
                total: 0,
                percent: 0.0,
                source: String::new(),
                attempt: 0,
                message: if save { "准备下载" } else { "准备更新" }.into(),
            });
        }
        std::thread::spawn(move || run_download(m, cfg, save));
        Ok(())
    }

    pub fn start_install_saved(
        self: &Arc<Self>,
        cfg: UpdateConfig,
        path: String,
    ) -> Result<(), String> {
        {
            let g = self.inner.lock().unwrap();
            if g.busy {
                return Err("已有更新任务在进行，请稍候".into());
            }
        }
        let m = Arc::clone(self);
        {
            let mut g = self.inner.lock().unwrap();
            g.busy = true;
            g.phase = "installing".into();
            g.error = None;
            g.attempts.clear();
            g.progress = Some(Progress {
                phase: "installing".into(),
                received: 0,
                total: 0,
                percent: 0.0,
                source: "local".into(),
                attempt: 0,
                message: "校验并安装已下载的包".into(),
            });
        }
        std::thread::spawn(move || run_install_saved(m, cfg, path));
        Ok(())
    }

    fn set_progress(
        &self,
        phase: &str,
        received: u64,
        total: u64,
        source: &str,
        attempt: u32,
        message: &str,
    ) {
        let percent = if total > 0 {
            (received as f64 / total as f64 * 100.0).min(100.0)
        } else {
            0.0
        };
        let mut g = self.inner.lock().unwrap();
        g.phase = phase.to_string();
        g.progress = Some(Progress {
            phase: phase.to_string(),
            received,
            total,
            percent,
            source: source.to_string(),
            attempt,
            message: message.to_string(),
        });
    }

    fn fail(&self, message: impl Into<String>, attempts: Vec<Attempt>, phase: &str) {
        let mut g = self.inner.lock().unwrap();
        g.busy = false;
        g.phase = phase.to_string();
        g.progress = None;
        g.error = Some(message.into());
        g.attempts = attempts;
    }
}

fn read_disk(home: &Path) -> DiskState {
    let path = home.join("state/update.json");
    if !path.exists() {
        return DiskState::default();
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn persist_disk(home: &Path, disk: &DiskState) -> Result<(), String> {
    let body = serde_json::to_string_pretty(disk).map_err(|e| e.to_string())?;
    util::write_atomic(&home.join("state/update.json"), body.as_bytes())
}

fn persist_from(manager: &Manager) {
    let disk = {
        let g = manager.inner.lock().unwrap();
        DiskState {
            saved: g.saved.clone(),
            last_install: g.last_install.clone(),
        }
    };
    let _ = persist_disk(&manager.home, &disk);
}

fn cleanup_parts(home: &Path) {
    for dir in [home.join("tmp/update"), home.join("state/update")] {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name.ends_with(".part") || name.ends_with(".tmp") {
                let _ = crate::safety::remove_file(&path, &[dir.clone()]);
            }
        }
    }
}

fn cleanup_old_exe() {
    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    let Some(dir) = exe.parent() else { return };
    let Some(name) = exe.file_name().and_then(|n| n.to_str()) else {
        return;
    };
    let prefix = format!("{name}.old-");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name().to_string_lossy().to_string();
        if fname.starts_with(&prefix) {
            let _ = crate::safety::remove_file(&entry.path(), &[dir.to_path_buf()]);
        }
    }
}

/* ------------------------------------------------------------------ 网络 */

fn agent(timeout: Duration) -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout(timeout)
        .user_agent(concat!(
            "berthio/",
            env!("CARGO_PKG_VERSION"),
            " (self-update)"
        ))
        .build()
}

fn fetch_one(url: &str) -> Result<Manifest, String> {
    let resp = match agent(MANIFEST_TIMEOUT).get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("连接失败：{e}")),
    };
    let mut text = String::new();
    resp.into_reader()
        .take(2 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| format!("读取清单失败：{e}"))?;
    let manifest: Manifest =
        serde_json::from_str(&text).map_err(|e| format!("清单不是合法 JSON：{e}"))?;
    Ver::parse(&manifest.version).map_err(|e| format!("清单版本号不合法：{e}"))?;
    if manifest.version.trim().is_empty() {
        return Err("清单缺少 version".into());
    }
    pick_asset(&manifest)?;
    Ok(manifest)
}

fn fetch_manifest(
    m: &Manager,
    cfg: &UpdateConfig,
    attempts: &mut Vec<Attempt>,
) -> Result<(Manifest, String), String> {
    let candidates = url_candidates(&cfg.feed, &cfg.mirror, &cfg.mirrors);
    for (index, (source, url)) in candidates.iter().enumerate() {
        m.set_progress("checking", 0, 0, source, index as u32 + 1, "读取更新清单");
        match fetch_one(url) {
            Ok(manifest) => {
                attempts.push(Attempt {
                    source: source.clone(),
                    url: url.clone(),
                    ok: true,
                    error: String::new(),
                });
                return Ok((manifest, source.clone()));
            }
            Err(e) => attempts.push(Attempt {
                source: source.clone(),
                url: url.clone(),
                ok: false,
                error: e,
            }),
        }
    }
    Err(format!(
        "无法获取更新清单，已尝试 {} 个源",
        candidates.len()
    ))
}

fn run_check(m: Arc<Manager>, cfg: UpdateConfig) {
    let mut attempts = Vec::new();
    match fetch_manifest(&m, &cfg, &mut attempts) {
        Ok((manifest, source)) => {
            let version = manifest.version.clone();
            let available = match is_newer(&version, &m.current) {
                Ok(v) => v,
                Err(e) => {
                    m.fail(e, attempts, "error");
                    return;
                }
            };
            let asset = match pick_asset(&manifest) {
                Ok(a) => a,
                Err(e) => {
                    m.fail(e, attempts, "error");
                    return;
                }
            };
            let asset_name = asset_file_name(&asset.url);
            if !asset_name.contains(&version) {
                m.fail(
                    format!("清单里的安装包文件名（{asset_name}）与版本 {version} 不一致，已拒绝"),
                    attempts,
                    "error",
                );
                return;
            }
            let check = CheckState {
                available,
                version,
                notes: manifest.notes.clone(),
                pub_date: manifest.pub_date.clone(),
                source,
                asset_name,
                asset_size: asset.size,
            };
            let mut g = m.inner.lock().unwrap();
            g.busy = false;
            g.manifest = Some(manifest);
            g.check = Some(check);
            g.progress = None;
            g.error = None;
            g.attempts = attempts;
            g.phase = if available { "available" } else { "latest" }.into();
        }
        Err(e) => m.fail(e, attempts, "error"),
    }
}

fn download_to(
    m: &Manager,
    url: &str,
    part: &Path,
    source: &str,
    attempt: u32,
) -> Result<(u64, u64, Vec<u8>), String> {
    let resp = match agent(DOWNLOAD_TIMEOUT).get(url).call() {
        Ok(r) => r,
        Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code}")),
        Err(e) => return Err(format!("连接失败：{e}")),
    };
    let total = resp
        .header("content-length")
        .and_then(|v| v.trim().parse::<u64>().ok())
        .unwrap_or(0);
    if total > MAX_UPDATE_BYTES {
        return Err(format!(
            "安装包超过 {} MB，已拒绝",
            MAX_UPDATE_BYTES / 1024 / 1024
        ));
    }
    let mut file = fs::File::create(part).map_err(|e| format!("创建临时文件失败：{e}"))?;
    let mut reader = resp.into_reader();
    let mut buf = [0u8; 64 * 1024];
    let mut received: u64 = 0;
    let mut last = Instant::now();
    loop {
        let n = reader
            .read(&mut buf)
            .map_err(|e| format!("读取下载数据失败：{e}"))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| format!("写入临时文件失败：{e}"))?;
        received += n as u64;
        if received > MAX_UPDATE_BYTES {
            return Err(format!(
                "安装包超过 {} MB，已中止",
                MAX_UPDATE_BYTES / 1024 / 1024
            ));
        }
        if last.elapsed() >= Duration::from_millis(120) {
            last = Instant::now();
            m.set_progress("downloading", received, total, source, attempt, "下载中");
        }
    }
    file.sync_all()
        .map_err(|e| format!("临时文件落盘失败：{e}"))?;
    drop(file);
    if total > 0 && received != total {
        return Err(format!("下载不完整：收到 {received} / {total} 字节"));
    }
    if received == 0 {
        return Err("下载内容为空".into());
    }
    let data = fs::read(part).map_err(|e| format!("读取临时文件失败：{e}"))?;
    Ok((received, total, data))
}

fn run_download(m: Arc<Manager>, cfg: UpdateConfig, save: bool) {
    let mut attempts = Vec::new();
    let existing = {
        let g = m.inner.lock().unwrap();
        g.manifest
            .clone()
            .filter(|_| g.check.as_ref().map(|c| c.available).unwrap_or(false))
    };
    let (manifest, _source) = match existing {
        Some(man) => {
            let source = {
                let g = m.inner.lock().unwrap();
                g.check
                    .as_ref()
                    .map(|c| c.source.clone())
                    .unwrap_or_default()
            };
            (man, source)
        }
        None => match fetch_manifest(&m, &cfg, &mut attempts) {
            Ok(v) => v,
            Err(e) => {
                m.fail(e, attempts, "error");
                return;
            }
        },
    };
    let version = manifest.version.clone();
    if !is_newer(&version, &m.current).unwrap_or(false) {
        m.fail(
            format!("当前已是最新版本（{}），无需下载", m.current),
            attempts,
            "error",
        );
        return;
    }
    let asset = match pick_asset(&manifest) {
        Ok(a) => a,
        Err(e) => {
            m.fail(e, attempts, "error");
            return;
        }
    };
    let asset_name = asset_file_name(&asset.url);
    if !asset_name.contains(&version) {
        m.fail(
            format!("清单里的安装包文件名（{asset_name}）与版本 {version} 不一致，已拒绝"),
            attempts,
            "error",
        );
        return;
    }
    let pubkey = match effective_pubkey(&cfg) {
        Ok(k) => k,
        Err(e) => {
            m.fail(e, attempts, "error");
            return;
        }
    };
    let temp_dir = m.home.join("tmp/update");
    if let Err(e) = fs::create_dir_all(&temp_dir) {
        m.fail(format!("创建临时目录失败：{e}"), attempts, "error");
        return;
    }
    cleanup_parts(&m.home);
    let part = temp_dir.join(format!("{asset_name}.part"));
    let candidates = url_candidates(&asset.url, &cfg.mirror, &cfg.mirrors);

    for (index, (source_name, url)) in candidates.iter().enumerate() {
        let attempt = index as u32 + 1;
        m.set_progress(
            "downloading",
            0,
            asset.size.unwrap_or(0),
            source_name,
            attempt,
            "下载中",
        );
        let outcome = download_to(&m, url, &part, source_name, attempt).and_then(
            |(received, total, data)| {
                m.set_progress(
                    "verifying",
                    received,
                    total,
                    source_name,
                    attempt,
                    "校验签名",
                );
                verify_bytes(&data, &asset.signature, &version, &asset_name, &pubkey)
                    .map_err(|e| format!("{e}（{source_name} 返回的文件不可信）"))?;
                Ok((received, total, data))
            },
        );
        let (size, _total, data) = match outcome {
            Ok(v) => v,
            Err(e) => {
                let _ = fs::remove_file(&part);
                attempts.push(Attempt {
                    source: source_name.clone(),
                    url: url.clone(),
                    ok: false,
                    error: e,
                });
                continue;
            }
        };
        attempts.push(Attempt {
            source: source_name.clone(),
            url: url.clone(),
            ok: true,
            error: String::new(),
        });
        let sha256 = util::sha256_hex(&data);
        if save {
            let _ = fs::remove_file(&part);
            match save_to_downloads(&asset_name, &data, &asset.signature) {
                Ok(target) => {
                    let record = SavedUpdate {
                        path: target.display().to_string(),
                        version: version.clone(),
                        source: source_name.clone(),
                        size,
                        asset_name: asset_name.clone(),
                        signature_b64: asset.signature.clone(),
                        sha256: sha256.clone(),
                        saved_at: util::now_iso(),
                    };
                    {
                        let mut g = m.inner.lock().unwrap();
                        g.saved = Some(record);
                        g.busy = false;
                        g.phase = "ready".into();
                        g.error = None;
                        g.attempts = attempts;
                        g.progress = Some(Progress {
                            phase: "ready".into(),
                            received: size,
                            total: size,
                            percent: 100.0,
                            source: source_name.clone(),
                            attempt,
                            message: "已保存到下载目录".into(),
                        });
                    }
                    persist_from(&m);
                }
                Err(e) => {
                    let _ = fs::remove_file(&part);
                    m.fail(format!("写入下载目录失败：{e}"), attempts, "error");
                }
            }
            return;
        }
        // 更新：先落到临时目录，再替换并重启。
        let staged = temp_dir.join(&asset_name);
        let _ = fs::remove_file(&staged);
        if let Err(e) = fs::rename(&part, &staged) {
            let _ = fs::remove_file(&part);
            m.fail(format!("整理安装包失败：{e}"), attempts, "error");
            return;
        }
        {
            let mut g = m.inner.lock().unwrap();
            g.attempts = attempts.clone();
            g.error = None;
        }
        m.set_progress(
            "installing",
            size,
            size,
            "local",
            attempt,
            "校验通过，准备替换程序",
        );
        // 让 API 响应先写完，再退出进程。
        std::thread::sleep(Duration::from_millis(400));
        if let Err(e) = apply_install(&m, &staged, &version, &sha256, source_name) {
            let _ = fs::remove_file(&staged);
            let mut g = m.inner.lock().unwrap();
            g.busy = false;
            g.phase = "error".into();
            g.error = Some(format!("安装失败：{e}"));
            g.progress = None;
        }
        return;
    }

    let _ = fs::remove_file(&part);
    m.fail(
        format!("下载失败，已尝试 {} 个源", candidates.len()),
        attempts,
        "error",
    );
}

fn downloads_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("BERTHIO_DOWNLOAD_DIR") {
        if !dir.trim().is_empty() {
            return PathBuf::from(dir);
        }
    }
    #[cfg(windows)]
    {
        if let Ok(home) = std::env::var("USERPROFILE") {
            return PathBuf::from(home).join("Downloads");
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(home) = std::env::var("HOME") {
            let home = PathBuf::from(home);
            let d = home.join("Downloads");
            if d.is_dir() {
                return d;
            }
            return home;
        }
    }
    std::env::temp_dir()
}

fn unique_target(dir: &Path, name: &str) -> PathBuf {
    let target = dir.join(name);
    if !target.exists() {
        return target;
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("berthio-update");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .map(|s| format!(".{s}"))
        .unwrap_or_default();
    for n in 2..=999 {
        let candidate = dir.join(format!("{stem} ({n}){ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{stem}-{}{ext}", util::now_secs()))
}

fn save_to_downloads(name: &str, data: &[u8], signature_b64: &str) -> Result<PathBuf, String> {
    let dir = downloads_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("创建下载目录失败：{e}"))?;
    let target = unique_target(&dir, name);
    let part = dir.join(format!(
        "{}.part",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("berthio-update")
    ));
    fs::write(&part, data).map_err(|e| format!("写入下载目录失败：{e}"))?;
    fs::rename(&part, &target).map_err(|e| {
        let _ = fs::remove_file(&part);
        format!("写入下载目录失败：{e}")
    })?;
    // 旁挂签名：手动校验时可用 minisign -Vm <file> -P <pubkey>。
    let sig_text = String::from_utf8(b64_decode(signature_b64)?)
        .map_err(|_| "签名不是 UTF-8 文本".to_string())?;
    let sidecar = target.with_file_name(format!(
        "{}.sig",
        target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("berthio-update")
    ));
    let _ = fs::write(sidecar, sig_text);
    Ok(target)
}

fn run_install_saved(m: Arc<Manager>, cfg: UpdateConfig, path: String) {
    let record = {
        let g = m.inner.lock().unwrap();
        g.saved.clone()
    };
    let Some(record) = record else {
        m.fail(
            "没有已下载的安装包记录，请先「仅下载」",
            Vec::new(),
            "error",
        );
        return;
    };
    if record.path != path {
        m.fail("安装包记录与请求不一致，请重新下载", Vec::new(), "error");
        return;
    }
    let target = PathBuf::from(&record.path);
    let pubkey = match effective_pubkey(&cfg) {
        Ok(k) => k,
        Err(e) => {
            m.fail(e, Vec::new(), "error");
            return;
        }
    };
    m.set_progress("installing", 0, 0, "local", 0, "二次校验安装包");
    let data = match fs::read(&target) {
        Ok(d) => d,
        Err(e) => {
            m.fail(format!("读取安装包失败：{e}"), Vec::new(), "error");
            return;
        }
    };
    if data.len() as u64 != record.size || util::sha256_hex(&data) != record.sha256 {
        m.fail(
            "安装包大小或 SHA-256 与下载记录不一致，已拒绝",
            Vec::new(),
            "error",
        );
        return;
    }
    if let Err(e) = verify_bytes(
        &data,
        &record.signature_b64,
        &record.version,
        &record.asset_name,
        &pubkey,
    ) {
        m.fail(format!("二次校验失败：{e}"), Vec::new(), "error");
        return;
    }
    let staged = m.home.join("tmp/update").join(&record.asset_name);
    if let Err(e) = fs::create_dir_all(m.home.join("tmp/update")) {
        m.fail(format!("创建临时目录失败：{e}"), Vec::new(), "error");
        return;
    }
    if let Err(e) = fs::copy(&target, &staged) {
        m.fail(format!("复制安装包失败：{e}"), Vec::new(), "error");
        return;
    }
    m.set_progress(
        "installing",
        record.size,
        record.size,
        "local",
        0,
        "准备替换程序并重启",
    );
    std::thread::sleep(Duration::from_millis(400));
    if let Err(e) = apply_install(&m, &staged, &record.version, &record.sha256, &record.source) {
        let _ = fs::remove_file(&staged);
        let mut g = m.inner.lock().unwrap();
        g.busy = false;
        g.phase = "error".into();
        g.error = Some(format!("安装失败：{e}"));
        g.progress = None;
    }
}

/* ------------------------------------------------------------------ 安装 */

fn set_last_install(m: &Manager, version: &str) {
    let mut g = m.inner.lock().unwrap();
    g.last_install = Some(LastInstall {
        version: version.to_string(),
        status: "started".into(),
        at: util::now_iso(),
        restarted_at: String::new(),
    });
}

/// 替换当前可执行文件并启动新进程，随后退出。失败时尽量回滚。
fn apply_install(
    m: &Manager,
    staged: &Path,
    version: &str,
    sha256: &str,
    _source: &str,
) -> Result<(), String> {
    let exe = std::env::current_exe().map_err(|e| format!("无法定位当前程序：{e}"))?;
    let exe_dir = exe.parent().ok_or_else(|| "无法定位程序目录".to_string())?;
    let exe_name = exe
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| "无法定位程序文件名".to_string())?;
    let bytes = fs::read(staged).map_err(|e| format!("读取安装包失败：{e}"))?;
    if bytes.len() as u64 != fs::metadata(staged).map_err(|e| e.to_string())?.len()
        || util::sha256_hex(&bytes) != sha256
    {
        return Err("安装包在落盘后发生变化，已拒绝".into());
    }
    // 备份旧版到数据目录（跨平台一致），供人工回滚。
    let backup = m.home.join("state/update").join(format!("{exe_name}.prev"));
    if let Err(e) = fs::create_dir_all(m.home.join("state/update")) {
        return Err(format!("创建备份目录失败：{e}"));
    }
    fs::copy(&exe, &backup).map_err(|e| format!("备份当前程序失败（需要写入程序目录）：{e}"))?;
    set_last_install(m, version);
    persist_from(m);

    #[cfg(windows)]
    {
        let old = exe_dir.join(format!("{exe_name}.old-{}", std::process::id()));
        let _ = fs::remove_file(&old);
        fs::rename(&exe, &old)
            .map_err(|e| format!("无法替换正在运行的程序（没有写入权限）：{e}"))?;
        if let Err(e) = fs::copy(staged, &exe) {
            let _ = fs::rename(&old, &exe);
            return Err(format!("写入新程序失败：{e}"));
        }
        let spawned = spawn_restart(&exe, exe_dir);
        if let Err(e) = spawned {
            let _ = fs::remove_file(&exe);
            let _ = fs::rename(&old, &exe);
            return Err(format!("启动新版本失败：{e}"));
        }
        let _ = fs::remove_file(&old);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let new_path = exe_dir.join(format!("{exe_name}.new"));
        let _ = fs::remove_file(&new_path);
        fs::copy(staged, &new_path).map_err(|e| format!("写入新程序失败：{e}"))?;
        fs::set_permissions(&new_path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("设置执行权限失败：{e}"))?;
        fs::rename(&new_path, &exe).map_err(|e| {
            let _ = fs::remove_file(&new_path);
            format!("替换程序失败：{e}")
        })?;
        if let Err(e) = spawn_restart(&exe, exe_dir) {
            return Err(format!("启动新版本失败：{e}"));
        }
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = staged;
        return Err("当前平台不支持应用内替换，请手动安装".into());
    }

    std::thread::sleep(Duration::from_millis(200));
    std::process::exit(0);
}

fn spawn_restart(exe: &Path, dir: &Path) -> Result<(), String> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().skip(1).collect();
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .current_dir(dir)
        .env("BERTHIO_UPDATE_RESTART", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

/* ------------------------------------------------------------------ 状态文件 */

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn temp_dir(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "berthio-update-test-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    const PUBKEY: &str = include_str!("../scripts/fixtures/update/test-pubkey.txt");
    const OTHER_PUBKEY: &str = include_str!("../scripts/fixtures/update/other-pubkey.txt");
    const SIG: &str = include_str!("../scripts/fixtures/update/asset.sig");
    const SIG_WRONG_VERSION: &str =
        include_str!("../scripts/fixtures/update/asset-wrong-version.sig");
    const ASSET: &[u8] =
        include_bytes!("../scripts/fixtures/update/berthio-1.0.1-windows-x86_64.exe");

    fn sig_b64(text: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(text.as_bytes())
    }

    #[test]
    fn version_rules_match_repo() {
        assert!(Ver::parse("1.0.0").is_ok());
        assert!(Ver::parse("1.10.10").is_ok());
        assert!(Ver::parse("2.3.4-alpha12").is_ok());
        assert!(Ver::parse("1.11.0").is_err());
        assert!(Ver::parse("1.0.11").is_err());
        assert!(Ver::parse("1.0.0-alpha0").is_err());
        assert!(Ver::parse("1.0.0-alpha01").is_err());
        assert!(Ver::parse("1.0.0-beta1").is_err());
        assert!(Ver::parse("01.0.0").is_err());
        assert!(Ver::parse("v1.0.0").is_err());
        assert!(!Ver::parse("1.0.0-alpha2")
            .unwrap()
            .newer_than(&Ver::parse("1.0.0-alpha3").unwrap()));
        assert!(Ver::parse("1.0.0-alpha3")
            .unwrap()
            .newer_than(&Ver::parse("1.0.0-alpha2").unwrap()));
        assert!(Ver::parse("1.0.0")
            .unwrap()
            .newer_than(&Ver::parse("1.0.0-alpha9").unwrap()));
        assert!(!Ver::parse("1.0.0-alpha9")
            .unwrap()
            .newer_than(&Ver::parse("1.0.0").unwrap()));
        assert!(is_newer("1.0.1", "1.0.0").unwrap());
        assert!(!is_newer("1.0.0", "1.0.0").unwrap());
        assert!(!is_newer("1.0.0-alpha4", "1.0.0").unwrap());
    }

    #[test]
    fn verify_accepts_only_matching_signature() {
        let pubkey = PUBKEY.to_string();
        assert!(verify_bytes(
            ASSET,
            &sig_b64(SIG),
            "1.0.1",
            "berthio-1.0.1-windows-x86_64.exe",
            &pubkey
        )
        .is_ok());

        let mut tampered = ASSET.to_vec();
        tampered[100] ^= 0xff;
        let err = verify_bytes(
            &tampered,
            &sig_b64(SIG),
            "1.0.1",
            "berthio-1.0.1-windows-x86_64.exe",
            &pubkey,
        )
        .unwrap_err();
        assert!(err.contains("签名校验失败"), "{err}");

        let err = verify_bytes(
            ASSET,
            &sig_b64(SIG),
            "1.0.1",
            "berthio-1.0.1-windows-x86_64.exe",
            OTHER_PUBKEY.trim(),
        )
        .unwrap_err();
        assert!(err.contains("签名校验失败"), "{err}");

        let err = verify_bytes(
            ASSET,
            &sig_b64(SIG_WRONG_VERSION),
            "1.0.1",
            "berthio-1.0.1-windows-x86_64.exe",
            &pubkey,
        )
        .unwrap_err();
        assert!(err.contains("签名绑定的版本"), "{err}");

        let err =
            verify_bytes(ASSET, &sig_b64(SIG), "1.0.1", "another-name.exe", &pubkey).unwrap_err();
        assert!(err.contains("签名绑定的文件名"), "{err}");
    }

    #[test]
    fn verify_fails_closed_without_pubkey() {
        let err = verify_bytes(
            ASSET,
            &sig_b64(SIG),
            "1.0.1",
            "berthio-1.0.1-windows-x86_64.exe",
            "",
        )
        .unwrap_err();
        assert!(err.contains("未配置更新公钥"), "{err}");
        let cfg = UpdateConfig {
            pubkey: String::new(),
            ..UpdateConfig::default()
        };
        assert!(effective_pubkey(&cfg).is_ok());
    }

    #[test]
    fn candidates_order_selected_then_official_then_rest() {
        let mirrors = UpdateConfig::default().mirrors;
        let feed = "https://github.com/yunyu697/berthio/releases/download/updates/latest.json";
        let auto = url_candidates(feed, "auto", &mirrors);
        assert_eq!(auto[0].1, feed);
        assert!(auto[1..].iter().all(|(_, u)| u.starts_with("https://gh")));
        let picked = url_candidates(feed, "gh-proxy", &mirrors);
        assert_eq!(picked[0].1, format!("https://gh-proxy.com/{feed}"));
        assert_eq!(picked[1].1, feed);
        assert!(picked[2..].iter().all(|(_, u)| u.starts_with("https://gh")));
        // 私有反代可作用于非 GitHub 地址；官方始终在第二位。
        let local = url_candidates("http://127.0.0.1:9999/latest.json", "gh-proxy", &mirrors);
        assert_eq!(
            local[0].1,
            "https://gh-proxy.com/http://127.0.0.1:9999/latest.json"
        );
        assert_eq!(local[1].1, "http://127.0.0.1:9999/latest.json");
        assert_eq!(local.len(), 4);
    }

    #[test]
    fn cleanup_removes_partial_downloads() {
        let home = temp_dir("cleanup");
        fs::create_dir_all(home.join("tmp/update")).unwrap();
        fs::write(home.join("tmp/update/a.exe.part"), b"half").unwrap();
        fs::write(home.join("tmp/update/b.exe"), b"staged").unwrap();
        cleanup_parts(&home);
        assert!(!home.join("tmp/update/a.exe.part").exists());
        assert!(home.join("tmp/update/b.exe").exists());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn saved_state_roundtrip_and_restart_marker() {
        let home = temp_dir("state");
        let saved = SavedUpdate {
            path: home.join("pkg.exe").display().to_string(),
            version: "1.0.1".into(),
            source: "official".into(),
            size: 12,
            asset_name: "berthio-1.0.1-windows-x86_64.exe".into(),
            signature_b64: "sig".into(),
            sha256: "sha".into(),
            saved_at: "now".into(),
        };
        let disk = DiskState {
            saved: Some(saved),
            last_install: Some(LastInstall {
                version: "1.0.1".into(),
                status: "started".into(),
                at: "then".into(),
                restarted_at: String::new(),
            }),
        };
        persist_disk(&home, &disk).unwrap();
        let m = Manager::new(&home, "1.0.0", true);
        let g = m.inner.lock().unwrap();
        assert!(!g.last_install.as_ref().unwrap().restarted_at.is_empty());
        assert_eq!(g.last_install.as_ref().unwrap().status, "restarted");
        // 安装成功重启后，同一版本的已下载包不再保留「安装」入口。
        assert!(g.saved.is_none());
        let _ = fs::remove_dir_all(home);
    }

    #[test]
    fn asset_name_is_sanitized_and_keeps_extension() {
        assert_eq!(
            asset_file_name("https://x/y/berthio-1.0.1-windows-x86_64.exe?x=1"),
            "berthio-1.0.1-windows-x86_64.exe"
        );
        assert_eq!(asset_file_name("https://x/y/a:b.exe"), "a_b.exe");
        assert_eq!(asset_file_name("https://x/y/"), "berthio-update.bin");
    }
}
