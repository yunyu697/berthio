use crate::util;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// 自签证书与私钥的固定落盘位置（相对数据目录），全部在数据目录内，不写系统目录。
pub const SELF_SIGNED_CERT: &str = "config/tls/panel.crt";
pub const SELF_SIGNED_KEY: &str = "config/tls/panel.key";
/// 自签证书有效期（天）。825 天是主流浏览器对私有 CA 之外自签证书的容忍上限。
pub const SELF_SIGNED_DAYS: i64 = 825;
/// 剩余有效期低于该值时打印告警。
pub const EXPIRY_WARN_DAYS: i64 = 30;

#[derive(Debug, Clone)]
pub struct CertInfo {
    /// SHA-256（DER）指纹，冒号分隔大写十六进制。
    pub fingerprint: String,
    pub not_before: String,
    pub not_after: String,
    pub not_after_epoch: i64,
    pub subject: String,
    pub sans: Vec<String>,
}

impl CertInfo {
    pub fn days_left(&self, now: i64) -> i64 {
        (self.not_after_epoch - now) / 86400
    }

    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "fingerprint": self.fingerprint,
            "not_before": self.not_before,
            "not_after": self.not_after,
            "days_left": self.days_left(util::now_secs() as i64),
            "subject": self.subject,
            "sans": self.sans,
        })
    }
}

pub struct Runtime {
    pub config: Arc<rustls::ServerConfig>,
    pub info: CertInfo,
    /// 距到期不足阈值时给出的告警文本（启动时算一次）。
    pub warning: Option<String>,
}

struct Tlv {
    tag: u8,
    start: usize,
    end: usize,
}

fn read_tlv(data: &[u8], mut i: usize) -> Option<(Tlv, usize)> {
    if i >= data.len() {
        return None;
    }
    let tag = data[i];
    i += 1;
    if i >= data.len() {
        return None;
    }
    let first = data[i];
    i += 1;
    let len = if first & 0x80 == 0 {
        first as usize
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || i + n > data.len() {
            return None;
        }
        let mut v = 0usize;
        for _ in 0..n {
            v = (v << 8) | data[i] as usize;
            i += 1;
        }
        v
    };
    if i + len > data.len() {
        return None;
    }
    Some((Tlv { tag, start: i, end: i + len }, i + len))
}

fn parse_asn1_time(data: &[u8], tag: u8) -> Option<i64> {
    let s = String::from_utf8_lossy(data).to_string();
    let (year, rest) = if tag == 0x17 {
        let y: i64 = s.get(0..2)?.parse().ok()?;
        (if y >= 70 { 1900 + y } else { 2000 + y }, &s[2..])
    } else if tag == 0x18 {
        (s.get(0..4)?.parse().ok()?, &s[4..])
    } else {
        return None;
    };
    let month: i64 = rest.get(0..2)?.parse().ok()?;
    let day: i64 = rest.get(2..4)?.parse().ok()?;
    let hour: i64 = rest.get(4..6)?.parse().ok()?;
    let min: i64 = rest.get(6..8)?.parse().ok()?;
    let sec: i64 = rest.get(8..10)?.parse().ok()?;
    Some(days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec)
}

/// Howard Hinnant 的 civil_from_days 逆运算，避免引入日期库。
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn validity(der: &[u8]) -> Option<(i64, i64)> {
    let (cert, _) = read_tlv(der, 0)?;
    if cert.tag != 0x30 {
        return None;
    }
    let (tbs, _) = read_tlv(der, cert.start)?;
    let mut i = tbs.start;
    if der.get(i) == Some(&0xa0) {
        let (_, next) = read_tlv(der, i)?;
        i = next;
    }
    for _ in 0..3 {
        let (_, next) = read_tlv(der, i)?;
        i = next;
    }
    let (val, _) = read_tlv(der, i)?;
    if val.tag != 0x30 {
        return None;
    }
    let (nb, i2) = read_tlv(der, val.start)?;
    let (na, _) = read_tlv(der, i2)?;
    Some((
        parse_asn1_time(&der[nb.start..nb.end], nb.tag)?,
        parse_asn1_time(&der[na.start..na.end], na.tag)?,
    ))
}

fn oid_after(der: &[u8], oid: &[u8]) -> Option<usize> {
    der.windows(oid.len()).position(|w| w == oid).map(|p| p + oid.len())
}

fn common_name(der: &[u8]) -> Option<String> {
    let mut at = oid_after(der, &[0x06, 0x03, 0x55, 0x04, 0x03])?;
    let (s, next) = read_tlv(der, at)?;
    if matches!(s.tag, 0x0c | 0x13 | 0x16) {
        at = next;
        let _ = at;
        return Some(String::from_utf8_lossy(&der[s.start..s.end]).to_string());
    }
    None
}

fn san_entries(der: &[u8]) -> Vec<String> {
    let mut out = Vec::new();
    let Some(mut at) = oid_after(der, &[0x06, 0x03, 0x55, 0x1d, 0x11]) else {
        return out;
    };
    // 跳过 extnValue 的 OCTET STRING 包装，直接进 GeneralNames。
    if let Some((oct, next)) = read_tlv(der, at) {
        if oct.tag == 0x04 {
            at = oct.start;
            let _ = next;
        }
    }
    let Some((names, _)) = read_tlv(der, at) else { return out };
    if names.tag != 0x30 {
        return out;
    }
    let mut i = names.start;
    while let Some((item, next)) = read_tlv(der, i) {
        match item.tag {
            0x82 => out.push(String::from_utf8_lossy(&der[item.start..item.end]).to_string()),
            0x87 => {
                let raw = &der[item.start..item.end];
                match raw.len() {
                    4 => out.push(format!("{}.{}.{}.{}", raw[0], raw[1], raw[2], raw[3])),
                    16 => out.push(format_ipv6(raw)),
                    _ => {}
                }
            }
            _ => {}
        }
        i = next;
    }
    out
}

/// RFC 5952 简化版：压缩最长的一段连续 0 组，便于界面直接显示 `::1` 这类地址。
fn format_ipv6(raw: &[u8]) -> String {
    let mut groups = [0u16; 8];
    for (i, g) in groups.iter_mut().enumerate() {
        *g = ((raw[i * 2] as u16) << 8) | raw[i * 2 + 1] as u16;
    }
    let (mut best_start, mut best_len) = (usize::MAX, 0usize);
    let (mut cur_start, mut cur_len) = (0usize, 0usize);
    for (i, g) in groups.iter().enumerate() {
        if *g == 0 {
            if cur_len == 0 {
                cur_start = i;
            }
            cur_len += 1;
            if cur_len > best_len {
                best_start = cur_start;
                best_len = cur_len;
            }
        } else {
            cur_len = 0;
        }
    }
    if best_len < 2 {
        return groups.iter().map(|g| format!("{g:x}")).collect::<Vec<_>>().join(":");
    }
    let left = groups[..best_start]
        .iter()
        .map(|g| format!("{g:x}"))
        .collect::<Vec<_>>()
        .join(":");
    let right = groups[best_start + best_len..]
        .iter()
        .map(|g| format!("{g:x}"))
        .collect::<Vec<_>>()
        .join(":");
    format!("{left}::{right}")
}

fn fingerprint(der: &[u8]) -> String {
    let digest = Sha256::digest(der);
    digest
        .iter()
        .map(|b| format!("{:02X}", b))
        .collect::<Vec<_>>()
        .join(":")
}

pub fn inspect(der: &[u8]) -> Option<CertInfo> {
    let (nb, na) = validity(der)?;
    Some(CertInfo {
        fingerprint: fingerprint(der),
        not_before: util::unix_to_iso(nb.max(0) as u64),
        not_after: util::unix_to_iso(na.max(0) as u64),
        not_after_epoch: na,
        subject: common_name(der).unwrap_or_else(|| "(未识别)".to_string()),
        sans: san_entries(der),
    })
}

/// 从 PEM 文本里取第一张证书并解析（删除前记录指纹/到期用）。
pub fn inspect_pem(text: &str) -> Option<CertInfo> {
    let der = pem_blocks(text, "CERTIFICATE").into_iter().next()?;
    inspect(&der)
}

fn pem_blocks(text: &str, label: &str) -> Vec<Vec<u8>> {
    let begin = format!("-----BEGIN {label}-----");
    let end = format!("-----END {label}-----");
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(p) = rest.find(&begin) {
        let after = &rest[p + begin.len()..];
        let Some(q) = after.find(&end) else { break };
        let body: String = after[..q].chars().filter(|c| !c.is_whitespace()).collect();
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(body) {
            out.push(bytes);
        }
        rest = &after[q + end.len()..];
    }
    out
}

fn load_key(raw: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, String> {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer};
    if let Some(der) = pem_blocks(raw, "PRIVATE KEY").into_iter().next() {
        return Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)));
    }
    if let Some(der) = pem_blocks(raw, "RSA PRIVATE KEY").into_iter().next() {
        return Ok(PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der)));
    }
    if let Some(der) = pem_blocks(raw, "EC PRIVATE KEY").into_iter().next() {
        return Ok(PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der)));
    }
    // 未带 PEM 头时按 PKCS#8 DER 处理（部分工具直接给 DER 文本）。
    if raw.trim().starts_with("-----") {
        return Err("私钥格式不被支持：需要 PKCS#8 / PKCS#1 / SEC1 PEM".into());
    }
    let body: String = raw.chars().filter(|c| !c.is_whitespace()).collect();
    let der = base64::engine::general_purpose::STANDARD
        .decode(body)
        .map_err(|_| "私钥既不是 PEM 也不是 base64 DER".to_string())?;
    Ok(PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der)))
}

fn resolve(home: &Path, p: &str) -> PathBuf {
    let path = PathBuf::from(p.trim());
    if path.is_absolute() { path } else { home.join(path) }
}

/// 供删除/备份等调用方复用同一套路径解析口径（相对数据目录或绝对路径）。
pub fn resolve_path(home: &Path, p: &str) -> PathBuf {
    resolve(home, p)
}

/// 解析用户配置的证书/私钥并构建 rustls 配置。
pub fn load(home: &Path, cert: &str, key: &str) -> Result<Runtime, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let cert_path = resolve(home, cert);
    let key_path = resolve(home, key);
    let cert_text = std::fs::read_to_string(&cert_path)
        .map_err(|e| format!("读取证书失败 {}: {e}", cert_path.display()))?;
    let key_text = std::fs::read_to_string(&key_path)
        .map_err(|e| format!("读取私钥失败 {}: {e}", key_path.display()))?;
    let chain: Vec<rustls::pki_types::CertificateDer<'static>> = pem_blocks(&cert_text, "CERTIFICATE")
        .into_iter()
        .map(rustls::pki_types::CertificateDer::from)
        .collect();
    if chain.is_empty() {
        return Err(format!("证书文件里没有 CERTIFICATE PEM 块: {}", cert_path.display()));
    }
    let info = inspect(&chain[0]).ok_or_else(|| "证书解析失败（不是有效 X.509）".to_string())?;
    let key_der = load_key(&key_text)?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(chain, key_der)
        .map_err(|e| format!("证书与私钥不匹配或不受支持: {e}"))?;
    let warning = expiry_warning(&info);
    Ok(Runtime {
        config: Arc::new(config),
        info,
        warning,
    })
}

fn expiry_warning(info: &CertInfo) -> Option<String> {
    let days = info.days_left(util::now_secs() as i64);
    if days < 0 {
        Some(format!("[tls] 证书已过期（{}），HTTPS 访问会失败，请立即更换", info.not_after))
    } else if days <= EXPIRY_WARN_DAYS {
        Some(format!("[tls] 证书将在 {days} 天后到期（{}），请尽快更换", info.not_after))
    } else {
        None
    }
}

fn local_addresses() -> Vec<String> {
    let mut out = Vec::new();
    if let Ok(list) = if_addrs::get_if_addrs() {
        for nic in list {
            let ip = nic.ip();
            let s = ip.to_string();
            // 去掉 IPv6 的 scope 后缀（fe80::1%eth0）。
            let clean = s.split('%').next().unwrap_or(&s).to_string();
            if !clean.is_empty() && !out.contains(&clean) {
                out.push(clean);
            }
        }
    }
    out
}

pub fn self_signed_sans() -> Vec<String> {
    use std::net::IpAddr;
    use std::str::FromStr;
    let mut sans = vec!["localhost".to_string(), "127.0.0.1".to_string(), "::1".to_string()];
    let mut seen: Vec<IpAddr> = sans
        .iter()
        .filter_map(|s| IpAddr::from_str(s).ok())
        .collect();
    for ip in local_addresses() {
        // 已用 ::1 / 127.0.0.1 表达回环，避免同名地址出现 “0:0:0:0:0:0:0:1” 这种重复项。
        let Ok(parsed) = IpAddr::from_str(&ip) else { continue };
        if seen.contains(&parsed) {
            continue;
        }
        seen.push(parsed);
        sans.push(if parsed.is_loopback() { parsed.to_string() } else { ip });
    }
    sans
}

/// 一键生成自签证书：落到数据目录 `config/tls/`，私钥权限 0600（Unix）。
pub fn generate_self_signed(home: &Path) -> Result<CertInfo, String> {
    let dir = home.join("config/tls");
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建 {} 失败: {e}", dir.display()))?;
    let sans = self_signed_sans();
    let mut params = rcgen::CertificateParams::new(sans.clone());
    params
        .distinguished_name
        .push(rcgen::DnType::CommonName, "Berthio Panel");
    params.distinguished_name
        .push(rcgen::DnType::OrganizationName, "Berthio self-signed");
    params.is_ca = rcgen::IsCa::NoCa;
    params.alg = &rcgen::PKCS_ECDSA_P256_SHA256;
    let today = util::now_iso();
    let y: i32 = today[0..4].parse().unwrap_or(2026);
    let m: u8 = today[5..7].parse().unwrap_or(1);
    let d: u8 = today[8..10].parse().unwrap_or(1);
    params.not_before = rcgen::date_time_ymd(y - 1, m, d);
    let end = util::unix_to_iso((util::now_secs() + SELF_SIGNED_DAYS as u64 * 86400) as u64);
    let ey: i32 = end[0..4].parse().unwrap_or(y + 2);
    let em: u8 = end[5..7].parse().unwrap_or(m);
    let ed: u8 = end[8..10].parse().unwrap_or(d);
    params.not_after = rcgen::date_time_ymd(ey, em, ed);
    let cert = rcgen::Certificate::from_params(params).map_err(|e| format!("签发证书失败: {e}"))?;
    let cert_pem = cert.serialize_pem().map_err(|e| format!("序列化证书失败: {e}"))?;
    let key_pem = cert.serialize_private_key_pem();
    let cert_path = home.join(SELF_SIGNED_CERT);
    let key_path = home.join(SELF_SIGNED_KEY);
    std::fs::write(&cert_path, cert_pem.as_bytes())
        .map_err(|e| format!("写入 {} 失败: {e}", cert_path.display()))?;
    write_private_key(&key_path, key_pem.as_bytes())?;
    let der = pem_blocks(&cert_pem, "CERTIFICATE")
        .into_iter()
        .next()
        .ok_or_else(|| "生成后无法回读证书".to_string())?;
    inspect(&der).ok_or_else(|| "生成的证书无法解析".to_string())
}

fn write_private_key(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts
        .open(path)
        .map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| format!("写入 {} 失败: {e}", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 启动后后台巡检证书到期，每 12 小时一次，只在跨过阈值时打印一次。
pub fn spawn_expiry_watch(info: CertInfo) {
    let shown = Arc::new(Mutex::new(false));
    std::thread::spawn(move || loop {
        let days = info.days_left(util::now_secs() as i64);
        let need = days <= EXPIRY_WARN_DAYS;
        let mut fired = shown.lock().unwrap_or_else(|e| e.into_inner());
        if need && !*fired {
            *fired = true;
            if days < 0 {
                eprintln!("[tls] 证书已过期（{}）", info.not_after);
            } else {
                eprintln!("[tls] 证书 {} 天后到期（{}）", days, info.not_after);
            }
        }
        if !need {
            *fired = false;
        }
        drop(fired);
        std::thread::sleep(std::time::Duration::from_secs(12 * 3600));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn self_signed_has_expected_sans_and_validity() {
        let root = std::env::var_os("BERTHIO_TEST_ROOT")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .unwrap_or_else(std::env::temp_dir);
        let dir = root.join(format!("berthio-tls-{}-{}", std::process::id(), util::now_secs()));
        let info = generate_self_signed(&dir).expect("生成自签证书");
        assert!(info.sans.iter().any(|s| s == "localhost"));
        assert!(info.sans.iter().any(|s| s == "127.0.0.1"));
        assert_eq!(info.fingerprint.split(':').count(), 32);
        assert!(info.days_left(util::now_secs() as i64) > 700);
        let rt = load(&dir, SELF_SIGNED_CERT, SELF_SIGNED_KEY).expect("加载自签证书");
        assert!(rt.warning.is_none());
    }

    #[test]
    fn ipv6_san_formatting_compresses_zeros() {
        let mut raw = [0u8; 16];
        raw[15] = 1;
        assert_eq!(format_ipv6(&raw), "::1");
        let full: [u8; 16] = [0x24, 0x08, 0x82, 0x56, 0x14, 0x80, 0x4b, 0x4f,
                              0xf8, 0xdc, 0xc4, 0xdb, 0x63, 0x6f, 0x80, 0xd6];
        assert_eq!(format_ipv6(&full), "2408:8256:1480:4b4f:f8dc:c4db:636f:80d6");
    }

    #[test]
    fn expired_and_near_expiry_produce_warning() {
        let mut info = CertInfo {
            fingerprint: "AA".into(),
            not_before: "2026-01-01T00:00:00Z".into(),
            not_after: "2030-01-01T00:00:00Z".into(),
            not_after_epoch: util::now_secs() as i64,
            subject: "Berthio Panel".into(),
            sans: vec![],
        };
        info.not_after_epoch = util::now_secs() as i64 + 5 * 86400;
        let w = expiry_warning(&info).expect("5 天内应告警");
        assert!(w.contains("5 天后到期"), "{w}");
        info.not_after_epoch = util::now_secs() as i64 - 86400;
        let w = expiry_warning(&info).expect("已过期应告警");
        assert!(w.contains("已过期"), "{w}");
        info.not_after_epoch = util::now_secs() as i64 + 400 * 86400;
        assert!(expiry_warning(&info).is_none());
    }
}
