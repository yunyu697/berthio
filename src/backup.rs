//! 面板数据主动备份/恢复。
//!
//! 设计要点：
//!   - 备份是“数据目录内的文件快照”，不是 tar：每个文件按相对路径复制到
//!     `state/backups/<name>/files/`，清单 `manifest.json` 记下每个文件的
//!     SHA-256、字节数、用途，能明确说出“这次备份包含哪些文件”。
//!   - 恢复先做完整性校验（清单版本 + 每个文件 hash），校验不过不动现场；
//!     通过后先给当前文件拍一份 `prerestore-*` 回滚快照，再逐文件原子替换；
//!     任一文件写入失败就按逆序回滚已写入的文件，避免“半截恢复”。
//!   - 审计日志（`logs/audit`）只追加，不参与覆盖式恢复；备份自身也不纳入备份。

use crate::util;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const BACKUP_KIND: &str = "berthio-panel-backup";
const BACKUP_SCHEMA: u32 = 1;

/// 需要随备份一起走的文件：相对数据目录的路径 + 用途说明。
const CORE_FILES: &[(&str, &str)] = &[
    ("config/panel.json", "面板设置（监听/TLS/更新源）"),
    ("config/sources.json", "规则集来源"),
    ("state/state.json", "入站、客户端、订阅令牌"),
    ("state/auth.json", "面板口令哈希与会话密钥"),
];

/// 明确不纳入备份的目录/文件及理由（“有意不做”必须写清楚，不能靠遗漏表达）。
const EXCLUDED: &[(&str, &str)] = &[
    ("state/kernels.json", "运行态 PID，恢复到新进程后无意义"),
    ("state/update.json", "升级进度快照，属运行态"),
    ("state/backups/*", "备份自身，避免递归包含"),
    ("logs/audit/*", "审计只追加，不参与覆盖式恢复，避免篡改历史"),
    ("logs/kernels/*", "内核运行日志，非配置数据"),
    ("data/rulesets/*", "规则缓存，可从来源重新拉取"),
    ("data/geo/*", "Geo 数据缓存，可重新获取"),
    ("configs/generated/*", "由入站配置重新生成"),
    ("tmp/*", "临时目录"),
];

struct Include {
    rel: String,
    abs: PathBuf,
    purpose: &'static str,
}

/// 证书/私钥如果是相对数据目录的路径，就纳入备份；绝对路径可能指向数据目录之外，
/// 不猜测、不越界复制，只在清单里标为“未纳入”并说明原因。
fn tls_extra(home: &Path) -> (Vec<(String, &'static str)>, Vec<String>) {
    let panel = std::fs::read_to_string(home.join("config/panel.json"))
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok());
    let cert = panel
        .as_ref()
        .and_then(|v| v.pointer("/tls/cert"))
        .and_then(|v| v.as_str())
        .unwrap_or(crate::tls::SELF_SIGNED_CERT);
    let key = panel
        .as_ref()
        .and_then(|v| v.pointer("/tls/key"))
        .and_then(|v| v.as_str())
        .unwrap_or(crate::tls::SELF_SIGNED_KEY);
    let mut included = Vec::new();
    let mut skipped = Vec::new();
    for (p, purpose) in [(cert, "内置 TLS 证书"), (key, "内置 TLS 私钥（含密钥，勿外传）")] {
        if p.trim().is_empty() {
            continue;
        }
        if Path::new(p).is_absolute() {
            skipped.push(format!("{p}（绝对路径，可能不在数据目录内，未纳入备份）"));
            continue;
        }
        included.push((p.to_string(), purpose));
    }
    (included, skipped)
}

fn plan(home: &Path) -> (Vec<Include>, Vec<String>) {
    let mut items = Vec::new();
    for (rel, purpose) in CORE_FILES {
        let abs = home.join(rel);
        if abs.is_file() {
            items.push(Include {
                rel: (*rel).to_string(),
                abs,
                purpose,
            });
        }
    }
    let (tls, skipped) = tls_extra(home);
    for (rel, purpose) in tls {
        let abs = home.join(&rel);
        if abs.is_file() && !items.iter().any(|i| i.rel == rel) {
            items.push(Include { rel, abs, purpose });
        }
    }
    (items, skipped)
}

fn backups_dir(home: &Path) -> PathBuf {
    home.join("state/backups")
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(util::sha256_hex(&bytes))
}

/// `GET /api/backup`：可备份清单（含理由）+ 已有备份列表。
pub fn overview(home: &Path) -> Value {
    let (items, skipped) = plan(home);
    let include: Vec<Value> = items
        .iter()
        .map(|i| {
            json!({
                "path": i.rel,
                "purpose": i.purpose,
                "bytes": std::fs::metadata(&i.abs).map(|m| m.len()).unwrap_or(0),
            })
        })
        .collect();
    let excluded: Vec<Value> = EXCLUDED
        .iter()
        .map(|(p, why)| json!({"path": p, "reason": why}))
        .chain(
            skipped
                .iter()
                .map(|s| json!({"path": s, "reason": "证书路径为绝对路径，未纳入"})),
        )
        .collect();
    json!({
        "schema": BACKUP_SCHEMA,
        "include": include,
        "excluded": excluded,
        "backups": list(home),
        "layout": "state/backups/<name>/（manifest.json + files/ 按相对路径存放）",
        "note": "恢复会覆盖 include 中的文件；审计日志只追加，不参与恢复",
    })
}

/// 列出已有备份：目录型（带 manifest）与升级前的单文件自动备份都认。
fn list(home: &Path) -> Vec<Value> {
    let dir = backups_dir(home);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out: Vec<Value> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().to_string();
            if path.is_dir() {
                let manifest = std::fs::read_to_string(path.join("manifest.json")).ok()?;
                let m: Value = serde_json::from_str(&manifest).ok()?;
                Some(json!({
                    "name": name,
                    "kind": m.get("kind").and_then(|v| v.as_str()).unwrap_or("unknown"),
                    "created_at": m.get("created_at").cloned().unwrap_or(json!("")),
                    "app_version": m.get("app_version").cloned().unwrap_or(json!("")),
                    "files": m.get("files").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0),
                    "paths": m.get("files").and_then(|v| v.as_array()).map(|a| {
                        a.iter().filter_map(|f| f.get("path").cloned()).collect::<Vec<_>>()
                    }).unwrap_or_default(),
                    "bytes": m.get("total_bytes").cloned().unwrap_or(json!(0)),
                    "restorable": m.get("kind").and_then(|v| v.as_str()) == Some(BACKUP_KIND),
                }))
            } else if name.ends_with(".json") {
                Some(json!({
                    "name": name,
                    "kind": "auto-upgrade",
                    "created_at": "",
                    "app_version": "",
                    "files": 1,
                    "bytes": e.metadata().map(|m| m.len()).unwrap_or(0),
                    "restorable": false,
                }))
            } else {
                None
            }
        })
        .collect();
    out.sort_by(|a, b| {
        b.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(a.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    out
}

/// `POST /api/backup`：落一份新的手动备份，返回清单摘要。
pub fn create(home: &Path, app_version: &str) -> Result<Value, String> {
    let (items, _) = plan(home);
    if items.is_empty() {
        return Err("数据目录里没有可备份的文件".into());
    }
    let name = format!("manual-{}-{}", util::now_secs(), util::random_hex(3));
    let dir = backups_dir(home).join(&name);
    let files_dir = dir.join("files");
    std::fs::create_dir_all(&files_dir)
        .map_err(|e| format!("创建备份目录失败 {}: {e}", files_dir.display()))?;

    let mut entries = Vec::new();
    let mut total = 0u64;
    for item in &items {
        let bytes = std::fs::read(&item.abs)
            .map_err(|e| format!("读取 {} 失败: {e}", item.abs.display()))?;
        let hash = util::sha256_hex(&bytes);
        let dest = files_dir.join(&item.rel);
        util::write_atomic(&dest, &bytes)?;
        let readback = sha256_file(&dest)?;
        if readback != hash {
            return Err(format!("备份回读校验失败：{}", item.rel));
        }
        total += bytes.len() as u64;
        entries.push(json!({
            "path": item.rel,
            "purpose": item.purpose,
            "bytes": bytes.len(),
            "sha256": hash,
        }));
    }
    let manifest = json!({
        "kind": BACKUP_KIND,
        "schema": BACKUP_SCHEMA,
        "app_version": app_version,
        "created_at": util::now_iso(),
        "total_bytes": total,
        "files": entries,
        "excluded": EXCLUDED.iter().map(|(p, why)| json!({"path": p, "reason": why})).collect::<Vec<_>>(),
    });
    let body = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("序列化清单失败: {e}"))?;
    util::write_atomic(&dir.join("manifest.json"), body.as_bytes())?;
    Ok(json!({
        "ok": true,
        "name": name,
        "path": format!("state/backups/{name}"),
        "manifest": manifest,
        "note": "备份包含的文件与字节数见 manifest.files；恢复前会再做一次 SHA-256 校验",
    }))
}

/// 备份名只允许是 `state/backups` 下的单层目录名，防止路径穿越。
fn backup_dir(home: &Path, name: &str) -> Result<PathBuf, String> {
    let bad = name.trim().is_empty()
        || name.contains('/')
        || name.contains('\\')
        || name == "."
        || name == ".."
        || name.contains(':');
    if bad {
        return Err(format!("非法备份名：{name}"));
    }
    let dir = backups_dir(home).join(name);
    if !dir.is_dir() {
        return Err(format!("备份不存在：{name}"));
    }
    Ok(dir)
}

fn safe_rel(rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if p.is_absolute() {
        return Err(format!("清单里的路径必须是相对路径：{rel}"));
    }
    for c in p.components() {
        if !matches!(c, std::path::Component::Normal(_)) {
            return Err(format!("清单里的路径不能包含 . / ..：{rel}"));
        }
    }
    Ok(p.to_path_buf())
}

/// `POST /api/backup/restore`：校验 → 回滚快照 → 逐文件原子替换 → 失败自动回滚。
pub fn restore(home: &Path, name: &str, confirm: &str) -> Result<Value, String> {
    if confirm != name {
        return Err("恢复需要二次确认：confirm 必须等于备份名".into());
    }
    let dir = backup_dir(home, name)?;
    let manifest_path = dir.join("manifest.json");
    let raw = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("读取备份清单失败: {e}"))?;
    let manifest: Value =
        serde_json::from_str(&raw).map_err(|e| format!("备份清单不是有效 JSON: {e}"))?;
    if manifest.get("kind").and_then(|v| v.as_str()) != Some(BACKUP_KIND) {
        return Err("不是 Berthio 面板备份（kind 不匹配）".into());
    }
    let schema = manifest.get("schema").and_then(|v| v.as_u64()).unwrap_or(0);
    if schema > BACKUP_SCHEMA as u64 {
        return Err(format!(
            "备份 schema=v{schema} 高于当前支持 v{BACKUP_SCHEMA}；请升级程序后再恢复"
        ));
    }
    let entries = manifest
        .get("files")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "备份清单缺少 files".to_string())?;
    if entries.is_empty() {
        return Err("备份清单里没有文件".into());
    }

    // 1) 先把所有待恢复文件读进内存并校验 hash；任何一项不过就原地不动。
    struct Staged {
        rel: PathBuf,
        bytes: Vec<u8>,
    }
    let mut staged: Vec<Staged> = Vec::new();
    for e in entries {
        let rel = e.get("path").and_then(|v| v.as_str()).unwrap_or("");
        let rel_path = safe_rel(rel)?;
        let hash = e.get("sha256").and_then(|v| v.as_str()).unwrap_or("");
        let src = dir.join("files").join(&rel_path);
        let bytes = std::fs::read(&src)
            .map_err(|err| format!("读取备份文件 {rel} 失败: {err}"))?;
        let got = util::sha256_hex(&bytes);
        if got != hash {
            return Err(format!("备份文件校验失败：{rel}（期望 {hash}，实际 {got}）"));
        }
        staged.push(Staged {
            rel: rel_path,
            bytes,
        });
    }

    // 2) 给当前现场拍一份回滚快照（只含本次会被覆盖的文件）。
    let snap_name = format!("prerestore-{}-{}", util::now_secs(), util::random_hex(3));
    let snap = backups_dir(home).join(&snap_name);
    std::fs::create_dir_all(&snap)
        .map_err(|e| format!("创建回滚快照失败 {}: {e}", snap.display()))?;
    let mut originals: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    for s in &staged {
        let target = home.join(&s.rel);
        let before = std::fs::read(&target).ok();
        if let Some(bytes) = &before {
            util::write_atomic(&snap.join(&s.rel), bytes)?;
        }
        originals.push((target, before));
    }

    // 3) 逐文件原子替换；失败则逆序回滚，避免半截状态。
    let mut applied = 0usize;
    for (idx, s) in staged.iter().enumerate() {
        let target = home.join(&s.rel);
        if let Err(e) = util::write_atomic(&target, &s.bytes) {
            for (t, before) in originals.iter().take(applied).rev() {
                match before {
                    Some(bytes) => {
                        let _ = util::write_atomic(t, bytes);
                    }
                    None => {
                        let _ = crate::safety::remove_file(
                            t,
                            &[home.to_path_buf()],
                        );
                    }
                }
            }
            return Err(format!(
                "恢复第 {} 个文件失败：{e}；已自动回滚已写入的 {applied} 个文件（回滚快照 {snap_name}）",
                idx + 1
            ));
        }
        applied += 1;
    }

    Ok(json!({
        "ok": true,
        "name": name,
        "restored": applied,
        "files": staged.iter().map(|s| s.rel.to_string_lossy().to_string()).collect::<Vec<_>>(),
        "rollback_snapshot": format!("state/backups/{snap_name}"),
        "restart_required": true,
        "note": "文件已原子替换；内存中的配置仍是旧值，请重启面板后生效",
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static SEQ: AtomicUsize = AtomicUsize::new(0);

    fn test_root() -> PathBuf {
        std::env::var_os("BERTHIO_TEST_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(std::env::temp_dir)
    }

    fn temp_home(tag: &str) -> PathBuf {
        let n = SEQ.fetch_add(1, Ordering::SeqCst);
        let home = test_root().join(format!(
            "berthio-backup-{}-{}-{}",
            std::process::id(),
            tag,
            n
        ));
        std::fs::create_dir_all(home.join("config")).unwrap();
        std::fs::create_dir_all(home.join("state")).unwrap();
        home
    }

    fn seed(home: &Path) {
        std::fs::create_dir_all(home.join("config/tls")).unwrap();
        std::fs::write(
            home.join("config/panel.json"),
            br#"{"listen":"127.0.0.1:18080","tls":{"cert":"config/tls/panel.crt","key":"config/tls/panel.key"}}"#,
        )
        .unwrap();
        std::fs::write(home.join("config/sources.json"), br#"{"sources":[]}"#).unwrap();
        std::fs::write(home.join("state/state.json"), br#"{"schema_version":3,"inbounds":[]}"#)
            .unwrap();
        std::fs::write(home.join("state/auth.json"), br#"{"salt":"aa"}"#).unwrap();
        std::fs::write(home.join("config/tls/panel.crt"), b"cert").unwrap();
        std::fs::write(home.join("config/tls/panel.key"), b"key").unwrap();
    }

    #[test]
    fn create_backup_lists_files_and_restores_them() {
        let home = temp_home("roundtrip");
        seed(&home);
        let created = create(&home, "1.0.3-alpha3").unwrap();
        let manifest = &created["manifest"];
        let paths: Vec<String> = manifest["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["path"].as_str().unwrap().to_string())
            .collect();
        assert!(paths.contains(&"config/panel.json".to_string()));
        assert!(paths.contains(&"state/state.json".to_string()));
        assert!(paths.contains(&"config/tls/panel.crt".to_string()));
        assert!(paths.contains(&"config/tls/panel.key".to_string()));
        assert!(manifest["total_bytes"].as_u64().unwrap() > 0);

        // 改坏现场，恢复后应回到备份内容。
        std::fs::write(home.join("state/state.json"), b"{\"broken\":true}").unwrap();
        let name = created["name"].as_str().unwrap();
        let out = restore(&home, name, name).unwrap();
        assert_eq!(out["restored"].as_u64().unwrap(), 6);
        let restored = std::fs::read_to_string(home.join("state/state.json")).unwrap();
        assert!(restored.contains("schema_version"), "{restored}");
        assert!(home.join("state/backups").join(name).is_dir());
        assert!(out["rollback_snapshot"].as_str().unwrap().starts_with("state/backups/prerestore-"));
    }

    #[test]
    fn restore_requires_confirm_and_rejects_tampered_files() {
        let home = temp_home("guard");
        seed(&home);
        let created = create(&home, "1.0.3-alpha3").unwrap();
        let name = created["name"].as_str().unwrap();

        assert!(restore(&home, name, "wrong").is_err());

        // 篡改备份里的文件：必须在改动现场之前失败。
        let staged = home.join("state/backups").join(name).join("files/state/state.json");
        std::fs::write(&staged, b"{\"forged\":true}").unwrap();
        let before = std::fs::read_to_string(home.join("state/state.json")).unwrap();
        let err = restore(&home, name, name).unwrap_err();
        assert!(err.contains("校验失败"), "{err}");
        let after = std::fs::read_to_string(home.join("state/state.json")).unwrap();
        assert_eq!(before, after, "校验失败时不得改动现场");
    }

    #[test]
    fn manifest_path_traversal_is_rejected() {
        let home = temp_home("traversal");
        seed(&home);
        let created = create(&home, "1.0.3-alpha3").unwrap();
        let name = created["name"].as_str().unwrap();
        let dir = home.join("state/backups").join(name);
        let mut manifest: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("manifest.json")).unwrap())
                .unwrap();
        manifest["files"][0]["path"] = json!("../escape.txt");
        std::fs::write(
            dir.join("manifest.json"),
            serde_json::to_string(&manifest).unwrap().as_bytes(),
        )
        .unwrap();
        let err = restore(&home, name, name).unwrap_err();
        assert!(err.contains("清单里的路径"), "{err}");
    }
}
