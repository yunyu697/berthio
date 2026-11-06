use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// 当前 UTC 秒级时间戳。
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// 当前 UTC 时间，ISO-8601（秒级，带 Z）。不引入 chrono，避免额外依赖。
pub fn now_iso() -> String {
    unix_to_iso(now_secs())
}

pub fn unix_to_iso(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}Z",
        y,
        m,
        d,
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// Howard Hinnant 的 days -> civil 算法，避免时区库依赖。
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

pub fn random_bytes(n: usize) -> Vec<u8> {
    let mut buf = vec![0u8; n];
    getrandom::getrandom(&mut buf).expect("系统随机源不可用");
    buf
}

pub fn hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

pub fn random_hex(n: usize) -> String {
    hex(&random_bytes(n))
}

/// 业务主键：`inb_` + 12 位十六进制。避免引入 ULID 依赖，同时保证可读与低碰撞。
pub fn new_id(prefix: &str) -> String {
    format!("{}_{}", prefix, random_hex(6))
}

pub fn uuid_v4() -> String {
    let mut b = random_bytes(16);
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    format!(
        "{}-{}-{}-{}-{}",
        hex(&b[0..4]),
        hex(&b[4..6]),
        hex(&b[6..8]),
        hex(&b[8..10]),
        hex(&b[10..16])
    )
}

pub fn sha256_hex(data: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(data);
    hex(&h.finalize())
}

/// 只保留小写字母数字与连字符，供 tag / email 使用。
pub fn slug(input: &str, fallback: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        fallback.to_string()
    } else {
        out
    }
}

/// 先写临时文件、再原子替换；保留上一版为 `.bak`。
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("创建目录失败 {}: {}", parent.display(), e))?;
    }
    let tmp: PathBuf = path.with_extension("tmp");
    {
        let mut f = fs::File::create(&tmp)
            .map_err(|e| format!("写临时文件失败 {}: {}", tmp.display(), e))?;
        f.write_all(bytes)
            .map_err(|e| format!("写临时文件失败 {}: {}", tmp.display(), e))?;
        f.sync_all().ok();
    }
    if path.exists() {
        let bak = path.with_extension("bak");
        let _ = fs::remove_file(&bak);
        let _ = fs::rename(path, &bak);
    }
    fs::rename(&tmp, path).map_err(|e| format!("替换 {} 失败: {}", path.display(), e))?;
    Ok(())
}

pub fn read_json(path: &Path) -> Result<serde_json::Value, String> {
    let raw = fs::read_to_string(path).map_err(|e| format!("读取 {} 失败: {}", path.display(), e))?;
    serde_json::from_str(&raw).map_err(|e| format!("解析 {} 失败: {}", path.display(), e))
}

pub fn append_jsonl(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let line = serde_json::to_string(value).map_err(|e| e.to_string())?;
    let mut f = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| format!("打开 {} 失败: {}", path.display(), e))?;
    f.write_all(line.as_bytes())
        .and_then(|_| f.write_all(b"\n"))
        .map_err(|e| e.to_string())
}

#[allow(dead_code)]
pub fn format_bytes(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", n, U[i])
    } else {
        format!("{:.1} {}", v, U[i])
    }
}
