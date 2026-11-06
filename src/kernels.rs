use crate::state::Store;
use crate::util;
use std::path::PathBuf;
use std::process::Command;

const KERNELS: [&str; 3] = ["xray", "sing-box", "mihomo"];

pub struct BinaryInfo {
    pub name: &'static str,
    pub present: bool,
    pub path: String,
    pub version: String,
    pub sha256: String,
    pub probe_command: String,
    pub probe_ok: bool,
    pub probe_output: String,
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(&exe);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

fn find_in_home(store: &Store, name: &str) -> Option<PathBuf> {
    let exe = exe_name(name);
    let mut roots = vec![
        store.home.join("tools/kernel").join(name),
        store.home.join("runtime/kernels").join(name),
    ];
    if let Ok(extra) = std::env::var("BERTHIO_KERNEL_DIR") {
        roots.push(PathBuf::from(extra).join(name));
    }
    for root in roots {
        let Ok(entries) = std::fs::read_dir(&root) else {
            continue;
        };
        let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        dirs.sort();
        dirs.reverse();
        for dir in dirs {
            let direct = dir.join(&exe);
            if direct.is_file() {
                return Some(direct);
            }
            if dir.is_file() && dir.file_name().map(|n| n.to_string_lossy() == exe).unwrap_or(false) {
                return Some(dir);
            }
        }
    }
    None
}

fn probe_args(name: &str) -> Vec<&'static str> {
    match name {
        "xray" => vec!["-version"],
        "sing-box" => vec!["version"],
        "mihomo" => vec!["-v"],
        _ => vec!["--version"],
    }
}

pub fn detect(store: &Store, name: &'static str) -> BinaryInfo {
    let path = find_in_path(name).or_else(|| find_in_home(store, name));
    let (present, path_str, version, sha, ok, output) = match path {
        Some(p) => {
            let args = probe_args(name);
            let output = Command::new(&p).args(&args).output();
            let (ok, combined, version) = match output {
                Ok(o) => {
                    let text = format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    );
                    let ok = o.status.success();
                    let version = parse_version(&text);
                    (ok, text.trim().to_string(), version)
                }
                Err(e) => (false, format!("执行失败: {e}"), String::new()),
            };
            let sha = std::fs::read(&p)
                .map(|b| util::sha256_hex(&b))
                .unwrap_or_else(|_| String::new());
            (
                true,
                p.display().to_string(),
                version,
                sha,
                ok,
                truncate(&combined, 240),
            )
        }
        None => (
            false,
            String::new(),
            String::new(),
            String::new(),
            false,
            "未找到二进制".to_string(),
        ),
    };
    BinaryInfo {
        name,
        present,
        path: path_str,
        version,
        sha256: sha,
        probe_command: format!("{} {}", name, probe_args(name).join(" ")),
        probe_ok: ok,
        probe_output: output,
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

/// 从版本输出里提取第一个形如 `1.8.24` / `25.9.11` 的片段。
fn parse_version(text: &str) -> String {
    for token in text.split_whitespace() {
        let t = token.trim_matches(|c: char| c == 'v' || c == 'V' || c == ',' || c == '(');
        let parts: Vec<&str> = t.split('.').collect();
        if parts.len() >= 2
            && parts[0].chars().all(|c| c.is_ascii_digit())
            && parts[1].chars().all(|c| c.is_ascii_digit())
            && !parts[0].is_empty()
            && !parts[1].is_empty()
        {
            return t.to_string();
        }
    }
    String::new()
}

pub fn matrix() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "vless-reality",
            "label": "VLESS + Reality（服务端入站）",
            "xray": "支持（文档结论；本机未跑 xray -test）",
            "sing-box": "不作为主力；入站能力未验证",
            "mihomo": "仅客户端导出，不支持服务端入站",
            "status": "未验证"
        },
        {
            "id": "vless-ws-tls",
            "label": "VLESS + WS + TLS（服务端入站）",
            "xray": "支持（文档结论；本机未跑 xray -test）",
            "sing-box": "未验证",
            "mihomo": "仅客户端导出",
            "status": "未验证"
        },
        {
            "id": "hysteria2",
            "label": "hysteria2（服务端入站）",
            "xray": "不支持（推断）",
            "sing-box": "支持（VERSION-MATRIX §3，已用官方二进制 check 过 1.11–1.14）",
            "mihomo": "仅客户端导出",
            "status": "部分核实"
        },
        {
            "id": "tuic-v5",
            "label": "tuic v5（服务端入站）",
            "xray": "不支持（推断）",
            "sing-box": "支持（1.11–1.14 check 通过）",
            "mihomo": "仅客户端导出",
            "status": "部分核实"
        },
        {
            "id": "xhttp",
            "label": "XHTTP 传输",
            "xray": "配置可渲染；未真机验证",
            "sing-box": "未验证",
            "mihomo": "客户端 mihomo -t 接受 network: xhttp；真机导入未验证",
            "status": "未验证"
        },
        {
            "id": "anytls",
            "label": "anytls（服务端入站）",
            "xray": "未知",
            "sing-box": "可选，未验证",
            "mihomo": "未验证",
            "status": "未验证"
        }
    ])
}

pub fn bucket_matrix() -> serde_json::Value {
    serde_json::json!([
        {"bucket": "111", "label": "sing-box 1.11.x", "built": true, "status": "已验证（check）"},
        {"bucket": "112", "label": "sing-box 1.12.x", "built": true, "status": "已验证（check）"},
        {"bucket": "113", "label": "sing-box 1.13.x", "built": true, "status": "已验证（check）"},
        {"bucket": "114", "label": "sing-box 1.14.x", "built": true, "status": "已验证（check）"},
        {"bucket": "115", "label": "sing-box 1.15.x", "built": false, "status": "未建档（无稳定版）"},
        {"bucket": "116", "label": "sing-box 1.16.x", "built": false, "status": "已删除（虚构档位）"}
    ])
}

pub fn report(store: &Store) -> serde_json::Value {
    let bins: Vec<serde_json::Value> = KERNELS.iter().map(|k| {
        let info = detect(store, *k);
        serde_json::json!({
            "name": info.name,
            "present": info.present,
            "path": info.path,
            "version": info.version,
            "sha256": info.sha256,
            "probe_command": info.probe_command,
            "probe_ok": info.probe_ok,
            "probe_output": info.probe_output,
            "verified": false,
            "note": if info.present { "仅完成版本探测；尚未用该二进制跑配置自检" } else { "未安装：所有依赖它的能力标为未验证" }
        })
    }).collect();
    serde_json::json!({
        "probed_at": util::now_iso(),
        "binaries": bins,
        "capabilities": matrix(),
        "buckets": bucket_matrix(),
        "policy": {
            "verify_rule": "只有实际执行 xray -test / sing-box check / mihomo -t 且通过，才标记「已验证」",
            "missing_binary": "二进制缺失时降级为未验证，不得写成已验证",
        }
    })
}
