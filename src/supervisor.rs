//! 内核进程管理（M3）。
//!
//! 设计约束（见 DECISIONS Q44–Q48）：
//! - 启动前必须先用内核自身做配置校验：`xray -test` / `sing-box check` / `mihomo -t`；
//!   校验失败不启动，回显具体错误，并回滚到上一次可用配置（last-known-good）。
//! - 启动前先检测端口占用；冲突时指出被谁占用（PID + 进程名）。
//! - PID 与状态落盘到 `state/kernels.json`；面板重启后核对进程归属，能接管就接管，
//!   不能确认归属的不 kill；由本面板留下但状态已丢失的进程按「孤儿」清理。
//! - 异常退出自动拉起，次数上限内重试；超过上限停止并告警（写审计）。
//! - 日志落盘 `logs/kernels/<kernel>.log`，启动时按大小轮转（默认 5 MB，保留 3 份）。
//! - demo 模式只返回演示状态，绝不启动真实内核。

use crate::error::{ApiError, ApiResult};
use crate::kernels;
use crate::model::Inbound;
use crate::state::Store;
use crate::util;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

pub const KERNELS: [&str; 3] = ["xray", "sing-box", "mihomo"];
const DEFAULT_RESTART_LIMIT: u32 = 3;
const MONITOR_INTERVAL_MS: u64 = 1000;
const START_SETTLE_MS: u64 = 1200;

pub fn is_kernel(name: &str) -> bool {
    KERNELS.contains(&name)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuntimeEntry {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub pid: u32,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub started_at_secs: u64,
    #[serde(default)]
    pub stopped_at: String,
    #[serde(default)]
    pub config_path: String,
    #[serde(default)]
    pub config_sha256: String,
    #[serde(default)]
    pub last_good_sha256: String,
    #[serde(default)]
    pub bin_path: String,
    #[serde(default)]
    pub bin_version: String,
    #[serde(default)]
    pub restarts: u32,
    #[serde(default = "default_true")]
    pub auto_restart: bool,
    #[serde(default = "default_restart_limit")]
    pub restart_limit: u32,
    #[serde(default)]
    pub last_exit_code: Option<i32>,
    #[serde(default)]
    pub last_exit_at: String,
    #[serde(default)]
    pub last_error: String,
    #[serde(default)]
    pub alert: String,
    #[serde(default)]
    pub rolled_back_to: String,
    #[serde(default)]
    pub adopted: bool,
    #[serde(default)]
    pub log_path: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub last_action: String,
}

fn default_true() -> bool {
    true
}
fn default_restart_limit() -> u32 {
    DEFAULT_RESTART_LIMIT
}

impl Default for RuntimeEntry {
    fn default() -> Self {
        Self {
            status: "stopped".into(),
            pid: 0,
            started_at: String::new(),
            started_at_secs: 0,
            stopped_at: String::new(),
            config_path: String::new(),
            config_sha256: String::new(),
            last_good_sha256: String::new(),
            bin_path: String::new(),
            bin_version: String::new(),
            restarts: 0,
            auto_restart: true,
            restart_limit: DEFAULT_RESTART_LIMIT,
            last_exit_code: None,
            last_exit_at: String::new(),
            last_error: String::new(),
            alert: String::new(),
            rolled_back_to: String::new(),
            adopted: false,
            log_path: String::new(),
            updated_at: String::new(),
            last_action: String::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct RuntimeFile {
    #[serde(default = "default_schema")]
    schema_version: u32,
    #[serde(default)]
    kernels: HashMap<String, RuntimeEntry>,
}

fn default_schema() -> u32 {
    1
}

pub struct Supervisor {
    home: PathBuf,
    demo: bool,
    rt: Mutex<HashMap<String, RuntimeEntry>>,
    children: Mutex<HashMap<String, Arc<Mutex<Child>>>>,
    monitor_started: AtomicBool,
}

fn exe_name(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    }
}

fn resolve_path(home: &Path, p: &str) -> String {
    let pb = PathBuf::from(p);
    if pb.is_absolute() {
        p.to_string()
    } else {
        home.join(pb).display().to_string()
    }
}

fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        return s.to_string();
    }
    let head: String = s.chars().take(n).collect();
    format!("{head}…")
}

fn ps(script: &str) -> Option<String> {
    for exe in ["powershell", "pwsh"] {
        let out = Command::new(exe)
            .args(["-NoProfile", "-NonInteractive", "-Command", script])
            .output();
        if let Ok(o) = out {
            if o.status.success() {
                return Some(String::from_utf8_lossy(&o.stdout).to_string());
            }
        }
    }
    None
}

#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub name: String,
    pub cmdline: String,
}

fn process_info(pid: u32) -> Option<ProcInfo> {
    if pid == 0 {
        return None;
    }
    if cfg!(windows) {
        let script = format!(
            "Get-CimInstance Win32_Process -Filter 'ProcessId={pid}' | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json -Compress"
        );
        let out = ps(&script)?;
        let v: Value = serde_json::from_str(out.trim()).ok()?;
        let obj = v.as_array().and_then(|a| a.first()).cloned().unwrap_or(v);
        if obj.is_null() {
            return None;
        }
        Some(ProcInfo {
            pid,
            name: obj["Name"].as_str().unwrap_or("").to_string(),
            cmdline: obj["CommandLine"].as_str().unwrap_or("").to_string(),
        })
    } else {
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm")).ok()?;
        let raw = std::fs::read(format!("/proc/{pid}/cmdline")).ok()?;
        let cmdline = String::from_utf8_lossy(&raw).replace('\0', " ");
        Some(ProcInfo {
            pid,
            name: comm.trim().to_string(),
            cmdline: cmdline,
        })
    }
}

fn list_kernel_processes() -> Vec<ProcInfo> {
    if cfg!(windows) {
        let script = "Get-CimInstance Win32_Process -Filter \"Name='xray.exe' OR Name='sing-box.exe' OR Name='mihomo.exe'\" | Select-Object ProcessId,Name,CommandLine | ConvertTo-Json -Compress";
        let Some(out) = ps(script) else {
            return Vec::new();
        };
        let Ok(v) = serde_json::from_str::<Value>(out.trim()) else {
            return Vec::new();
        };
        let list = if let Some(a) = v.as_array() {
            a.clone()
        } else if v.is_null() {
            Vec::new()
        } else {
            vec![v]
        };
        list.iter()
            .filter_map(|o| {
                let pid = o["ProcessId"].as_u64()? as u32;
                Some(ProcInfo {
                    pid,
                    name: o["Name"].as_str().unwrap_or("").to_string(),
                    cmdline: o["CommandLine"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect()
    } else {
        let mut out = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return out;
        };
        for e in entries.flatten() {
            let Ok(pid) = e.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            if let Some(info) = process_info(pid) {
                if matches!(info.name.as_str(), "xray" | "sing-box" | "mihomo") {
                    out.push(info);
                }
            }
        }
        out
    }
}

fn process_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if cfg!(windows) {
        let out = Command::new("tasklist")
            .args(["/FI", &format!("PID eq {pid}"), "/NH"])
            .output();
        match out {
            Ok(o) => {
                let text = String::from_utf8_lossy(&o.stdout);
                text.contains(&pid.to_string())
            }
            Err(_) => process_info(pid).is_some(),
        }
    } else {
        process_info(pid).is_some()
    }
}

fn kill_process(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    if cfg!(windows) {
        let out = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/F"])
            .output();
        return out.map(|o| o.status.success()).unwrap_or(false);
    }
    let _ = Command::new("kill").args(["-TERM", &pid.to_string()]).status();
    for _ in 0..20 {
        if !process_alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    let _ = Command::new("kill").args(["-KILL", &pid.to_string()]).status();
    true
}

fn host_port(dest: &str) -> (String, u16) {
    match dest.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse().unwrap_or(443)),
        None => (dest.to_string(), 443),
    }
}

fn port_free(listen: &str, port: u16, l4: &str) -> bool {
    let host = if listen.is_empty() { "127.0.0.1" } else { listen };
    if l4 == "udp" {
        UdpSocket::bind((host, port)).is_ok()
    } else {
        TcpListener::bind((host, port)).is_ok()
    }
}

fn port_owner(port: u16, l4: &str) -> Value {
    if cfg!(windows) {
        let cmd = if l4 == "udp" {
            "Get-NetUDPEndpoint"
        } else {
            "Get-NetTCPConnection"
        };
        let state = "";
        let script = format!(
            "$c = {cmd} -LocalPort {port}{state} -ErrorAction SilentlyContinue | Select-Object -First 1; if ($c) {{ $p = Get-Process -Id $c.OwningProcess -ErrorAction SilentlyContinue; Write-Output (\"{{0}}|{{1}}\" -f $c.OwningProcess, $p.ProcessName) }}"
        );
        if let Some(out) = ps(&script) {
            let line = out.trim();
            if let Some((pid, name)) = line.split_once('|') {
                if let Ok(pid) = pid.trim().parse::<u32>() {
                    return json!({
                        "known": !name.trim().is_empty(),
                        "pid": pid,
                        "name": name.trim(),
                        "source": cmd,
                    });
                }
            }
        }
        let proto = if l4 == "udp" { "udp" } else { "tcp" };
        let out = Command::new("netstat").args(["-ano", "-p", proto]).output();
        let Ok(o) = out else {
            return json!({"known": false, "note": "netstat 不可用"});
        };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            let cols: Vec<&str> = line.split_whitespace().collect();
            if cols.len() < 4 {
                continue;
            }
            let local = cols[1];
            let port_matches = local
                .rsplit(':')
                .next()
                .and_then(|p| p.parse::<u16>().ok())
                .map(|p| p == port)
                .unwrap_or(false);
            if !port_matches {
                continue;
            }
            if l4 == "tcp" && !cols.iter().any(|c| *c == "LISTENING") {
                continue;
            }
            let pid = cols
                .iter()
                .rev()
                .find_map(|c| c.parse::<u32>().ok())
                .unwrap_or(0);
            let name = process_info(pid).map(|p| p.name).unwrap_or_default();
            return json!({"known": !name.is_empty(), "pid": pid, "name": name, "raw": line.trim(), "source": "netstat"});
        }
        json!({"known": false, "note": "netstat 未找到监听项"})
    } else {
        let out = Command::new("ss").args(["-lntup"]).output();
        let Ok(o) = out else {
            return json!({"known": false, "note": "ss 不可用"});
        };
        for line in String::from_utf8_lossy(&o.stdout).lines() {
            if !line.contains(&format!(":{port} ")) {
                continue;
            }
            let pid = line
                .split("pid=")
                .nth(1)
                .and_then(|s| s.split([',', ')']).next())
                .and_then(|s| s.parse::<u32>().ok())
                .unwrap_or(0);
            let name = line
                .split("((\"")
                .nth(1)
                .and_then(|s| s.split('"').next())
                .unwrap_or("")
                .to_string();
            return json!({"known": !name.is_empty(), "pid": pid, "name": name, "raw": line.trim()});
        }
        json!({"known": false, "note": "ss 未找到监听项"})
    }
}

fn privileged_port_problem(ports: &[(String, u16, String)]) -> Option<Vec<String>> {
    if !cfg!(unix) {
        return None;
    }
    if !ports.iter().any(|(_, p, _)| *p < 1024) {
        return None;
    }
    let uid = Command::new("id").arg("-u").output().ok().map(|o| {
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    });
    if uid.as_deref() == Some("0") {
        return None;
    }
    Some(
        ports
            .iter()
            .filter(|(_, p, _)| *p < 1024)
            .map(|(listen, p, l4)| {
                format!("绑定 {listen}:{p}/{l4}（特权端口；需要 root 或给内核二进制 CAP_NET_BIND_SERVICE）")
            })
            .collect(),
    )
}

fn command_args(kernel: &str, config: &str) -> Vec<String> {
    match kernel {
        "xray" => vec!["-config".into(), config.into()],
        "sing-box" => vec!["run".into(), "-c".into(), config.into()],
        "mihomo" => vec!["-f".into(), config.into()],
        _ => vec![],
    }
}

fn check_args(kernel: &str) -> (&'static [&'static str], &'static str) {
    match kernel {
        "xray" => (&["-test", "-config"], "xray -test -config"),
        "sing-box" => (&["check", "-c"], "sing-box check -c"),
        "mihomo" => (&["-t", "-f"], "mihomo -t -f"),
        _ => (&[], ""),
    }
}

impl Supervisor {
    pub fn new(home: &Path, demo: bool) -> Self {
        let path = home.join("state/kernels.json");
        let mut map: HashMap<String, RuntimeEntry> = if path.exists() {
            util::read_json(&path)
                .ok()
                .and_then(|v| serde_json::from_value::<RuntimeFile>(v).ok())
                .map(|f| f.kernels)
                .unwrap_or_default()
        } else {
            HashMap::new()
        };
        for k in KERNELS {
            map.entry(k.to_string()).or_insert_with(|| {
                let mut e = RuntimeEntry::default();
                e.status = if demo { "demo".into() } else { "stopped".into() };
                e
            });
        }
        if demo {
            for e in map.values_mut() {
                e.status = "demo".into();
                e.pid = 0;
            }
        }
        Self {
            home: home.to_path_buf(),
            demo,
            rt: Mutex::new(map),
            children: Mutex::new(HashMap::new()),
            monitor_started: AtomicBool::new(false),
        }
    }

    pub fn is_demo(&self) -> bool {
        self.demo
    }

    pub fn runtime_json(&self, store: &Store) -> Value {
        let rt = self.rt.lock().unwrap();
        let mut out = Map::new();
        for k in KERNELS {
            let e = rt.get(k).cloned().unwrap_or_default();
            let bin = kernels::detect(store, k);
            let uptime = if e.pid > 0 && e.started_at_secs > 0 {
                util::now_secs().saturating_sub(e.started_at_secs)
            } else {
                0
            };
            out.insert(
                k.to_string(),
                json!({
                    "kernel": k,
                    "status": if self.demo { "demo" } else { e.status.as_str() },
                    "pid": e.pid,
                    "uptime_secs": uptime,
                    "started_at": e.started_at,
                    "stopped_at": e.stopped_at,
                    "config_path": e.config_path,
                    "config_sha256": e.config_sha256,
                    "last_good_sha256": e.last_good_sha256,
                    "restarts": e.restarts,
                    "auto_restart": e.auto_restart,
                    "restart_limit": e.restart_limit,
                    "last_exit_code": e.last_exit_code,
                    "last_exit_at": e.last_exit_at,
                    "last_error": e.last_error,
                    "alert": e.alert,
                    "rolled_back_to": e.rolled_back_to,
                    "adopted": e.adopted,
                    "log_path": e.log_path,
                    "last_action": e.last_action,
                    "updated_at": e.updated_at,
                    "binary": {"present": bin.present, "path": bin.path, "version": bin.version},
                    "demo": self.demo,
                    "note": if self.demo { "演示模式：不会启动真实内核，启停按钮只返回 409 demo_mode" } else { "" }
                }),
            );
        }
        Value::Object(out)
    }

    fn persist(&self) -> ApiResult<()> {
        let rt = self.rt.lock().unwrap();
        let file = RuntimeFile {
            schema_version: 1,
            kernels: rt.clone(),
        };
        let body = serde_json::to_string_pretty(&file).map_err(|e| ApiError::internal(e.to_string()))?;
        util::write_atomic(&self.home.join("state/kernels.json"), body.as_bytes())
            .map_err(ApiError::internal)
    }

    pub fn config_path(&self, kernel: &str) -> PathBuf {
        let ext = if kernel == "mihomo" { "yaml" } else { "json" };
        self.home
            .join(format!("configs/generated/runtime-{kernel}.{ext}"))
    }

    fn lkg_path(&self, kernel: &str) -> PathBuf {
        self.home.join(format!("state/kernels/{kernel}.lkg"))
    }

    pub fn log_path(&self, kernel: &str) -> PathBuf {
        self.home.join(format!("logs/kernels/{kernel}.log"))
    }

    fn rotate_log(&self, kernel: &str) {
        let max = std::env::var("BERTHIO_KERNEL_LOG_MAX")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(5 * 1024 * 1024);
        let log = self.log_path(kernel);
        let Ok(meta) = std::fs::metadata(&log) else {
            return;
        };
        if meta.len() <= max {
            return;
        }
        for n in (1..3).rev() {
            let from = log.with_extension(format!("log.{n}"));
            let to = log.with_extension(format!("log.{}", n + 1));
            let _ = std::fs::rename(&from, &to);
        }
        let _ = std::fs::rename(&log, log.with_extension("log.1"));
        let _ = std::fs::remove_file(log.with_extension("log.4"));
    }

    pub fn log_tail(&self, kernel: &str, lines: usize) -> Value {
        let path = self.log_path(kernel);
        let Ok(mut f) = std::fs::File::open(&path) else {
            return json!({"kernel": kernel, "path": path.display().to_string(), "lines": [], "note": "日志文件不存在"});
        };
        let len = f.metadata().map(|m| m.len()).unwrap_or(0);
        let start = len.saturating_sub(256 * 1024);
        let _ = f.seek(SeekFrom::Start(start));
        let mut buf = Vec::new();
        let _ = f.read_to_end(&mut buf);
        let content = String::from_utf8_lossy(&buf).to_string();
        let items: Vec<String> = content
            .lines()
            .rev()
            .take(lines.clamp(1, 1000))
            .map(|s| s.to_string())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        json!({"kernel": kernel, "path": path.display().to_string(), "lines": items, "size": len})
    }

    fn ports_for(store: &Store, kernel: &str) -> Vec<(String, u16, String)> {
        store
            .snapshot()
            .inbounds
            .into_iter()
            .filter(|i| i.enabled && i.server_kernel == kernel && i.port > 0)
            .map(|i| (i.listen, i.port, i.l4))
            .collect()
    }

    pub fn validate(&self, store: &Store, kernel: &str) -> ApiResult<Value> {
        if self.demo {
            return Err(ApiError::new(409, "demo_mode", "演示模式不允许对真实内核执行配置校验"));
        }
        if !is_kernel(kernel) {
            return Err(ApiError::not_found(format!("未知内核 {kernel}")));
        }
        let bin = kernels::detect(store, kernel);
        if !bin.present {
            return Err(ApiError::new(422, "binary_missing", format!("未找到 {kernel} 二进制"))
                .with_field("kernel", kernel)
                .with_field("action", "安装内核或设置 BERTHIO_KERNEL_DIR"));
        }
        let rendered = self.render(store, kernel)?;
        let path = self.config_path(kernel);
        util::write_atomic(&path, rendered.content.as_bytes()).map_err(ApiError::internal)?;
        let sha = util::sha256_hex(rendered.content.as_bytes());
        let (prefix, label) = check_args(kernel);
        let mut cmd = Command::new(&bin.path);
        for a in prefix {
            cmd.arg(a);
        }
        cmd.arg(&path);
        let output = cmd.output();
        let (ok, text) = match output {
            Ok(o) => (
                o.status.success(),
                truncate(
                    &format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                    2000,
                ),
            ),
            Err(e) => (false, format!("执行失败: {e}")),
        };
        Ok(json!({
            "ok": ok,
            "kernel": kernel,
            "binary": {"path": bin.path, "version": bin.version},
            "config_path": path.display().to_string(),
            "config_sha256": sha,
            "inbounds": rendered.inbounds,
            "ports": rendered.ports,
            "command": format!("{label} {}", path.display()),
            "output": text,
            "warnings": rendered.warnings,
        }))
    }

    fn render(&self, store: &Store, kernel: &str) -> ApiResult<Rendered> {
        let state = store.snapshot();
        let inbounds: Vec<Inbound> = state
            .inbounds
            .iter()
            .filter(|i| i.enabled && i.server_kernel == kernel)
            .cloned()
            .collect();
        let ports: Vec<Value> = inbounds
            .iter()
            .map(|i| json!({"listen": i.listen, "port": i.port, "l4": i.l4, "tag": i.tag}))
            .collect();
        let mut warnings = Vec::new();
        let content = match kernel {
            "xray" => render_xray(&self.home, &inbounds, &mut warnings)?,
            "sing-box" => render_sing_box(&self.home, &inbounds, &mut warnings)?,
            "mihomo" => render_mihomo(&inbounds, &mut warnings)?,
            _ => return Err(ApiError::not_found(format!("未知内核 {kernel}"))),
        };
        Ok(Rendered {
            content,
            inbounds: inbounds.len(),
            ports,
            warnings,
        })
    }

    pub fn start(&self, store: &Store, kernel: &str) -> ApiResult<Value> {
        self.start_internal(store, kernel, false)
    }

    pub fn restart(&self, store: &Store, kernel: &str) -> ApiResult<Value> {
        if self.demo {
            return Err(ApiError::new(409, "demo_mode", "演示模式不启动真实内核"));
        }
        let _ = self.stop(kernel);
        self.start_internal(store, kernel, false)
    }

    fn start_internal(&self, store: &Store, kernel: &str, auto: bool) -> ApiResult<Value> {
        if self.demo {
            return Err(ApiError::new(409, "demo_mode", "演示模式不启动真实内核")
                .with_field("note", "demo 只返回状态，不会 spawn 任何子进程"));
        }
        if !is_kernel(kernel) {
            return Err(ApiError::not_found(format!("未知内核 {kernel}")));
        }
        {
            let rt = self.rt.lock().unwrap();
            if let Some(e) = rt.get(kernel) {
                if matches!(e.status.as_str(), "running" | "starting" | "restarting") && e.pid > 0 {
                    return Err(ApiError::conflict(format!(
                        "{kernel} 已在运行（pid {}），先停止再启动",
                        e.pid
                    )));
                }
            }
        }
        let bin = kernels::detect(store, kernel);
        if !bin.present {
            return Err(ApiError::new(422, "binary_missing", format!("未找到 {kernel} 二进制"))
                .with_field("kernel", kernel)
                .with_field("action", "安装内核或设置 BERTHIO_KERNEL_DIR"));
        }
        let rendered = self.render(store, kernel)?;
        let path = self.config_path(kernel);
        util::write_atomic(&path, rendered.content.as_bytes()).map_err(ApiError::internal)?;
        let sha = util::sha256_hex(rendered.content.as_bytes());
        self.rotate_log(kernel);

        let ports = Self::ports_for(store, kernel);
        if let Some(ops) = privileged_port_problem(&ports) {
            return Err(ApiError::new(409, "privilege_required", "该内核要绑定的端口需要特权，面板不会自行提权")
                .with_field("operations", ops.join("；"))
                .with_field("action", "请以 root 启动面板，或给内核二进制 CAP_NET_BIND_SERVICE 后重试")
                .with_field("note", "面板不会在后台静默提权"));
        }
        for (listen, port, l4) in &ports {
            if *port == 0 {
                continue;
            }
            if !port_free(listen, *port, l4) {
                let owner = port_owner(*port, l4);
                return Err(ApiError::new(
                    409,
                    "port_in_use",
                    format!("端口 {port}/{l4} 已被占用，拒绝启动 {kernel}"),
                )
                .with_field("listen", listen.clone())
                .with_field("port", port.to_string())
                .with_field("l4", l4.clone())
                .with_field("owner", owner.to_string())
                .with_field("action", "停止占用者 / 换端口 / 停用对应入站后重试"));
            }
        }

        let (prefix, label) = check_args(kernel);
        let mut check = Command::new(&bin.path);
        for a in prefix {
            check.arg(a);
        }
        check.arg(&path);
        let check_out = check.output();
        let (check_ok, check_text) = match check_out {
            Ok(o) => (
                o.status.success(),
                truncate(
                    &format!(
                        "{}{}",
                        String::from_utf8_lossy(&o.stdout),
                        String::from_utf8_lossy(&o.stderr)
                    ),
                    2000,
                ),
            ),
            Err(e) => (false, format!("执行失败: {e}")),
        };
        if !check_ok {
            let rolled = self.rollback(kernel, &sha)?;
            self.set_state(kernel, |e| {
                e.status = "config_invalid".into();
                e.last_error = format!("{label} 校验失败：{check_text}");
                e.config_path = path.display().to_string();
                e.config_sha256 = sha.clone();
                e.bin_path = bin.path.clone();
                e.bin_version = bin.version.clone();
                e.pid = 0;
                e.rolled_back_to = rolled.clone();
                e.last_action = if auto { "auto_start" } else { "start" }.into();
            })?;
            store.audit(
                "kernel.config.invalid",
                if auto { "system" } else { "admin" },
                json!({"kernel": kernel, "command": format!("{label} {}", path.display()), "output": check_text, "rolled_back_to": rolled}),
            );
            return Err(ApiError::new(422, "config_check_failed", format!("{kernel} 配置自检失败，未启动"))
                .with_field("command", format!("{label} {}", path.display()))
                .with_field("output", check_text)
                .with_field("rolled_back", if rolled.is_empty() { "无 last-known-good".into() } else { format!("已回滚到 {rolled}") }));
        }

        std::fs::create_dir_all(self.home.join("logs/kernels")).ok();
        let log = self.log_path(kernel);
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log)
            .map_err(|e| ApiError::internal(format!("打开内核日志失败: {e}")))?;
        let mut cmd = Command::new(&bin.path);
        for a in command_args(kernel, &path.display().to_string()) {
            cmd.arg(a);
        }
        cmd.stdin(Stdio::null())
            .stdout(Stdio::from(
                log_file
                    .try_clone()
                    .map_err(|e| ApiError::internal(format!("复制日志句柄失败: {e}")))?,
            ))
            .stderr(Stdio::from(log_file));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        let child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => {
                let rolled = self.rollback(kernel, &sha)?;
                self.set_state(kernel, |r| {
                    r.status = "failed".into();
                    r.last_error = format!("spawn 失败: {e}");
                    r.rolled_back_to = rolled.clone();
                    r.last_action = "start".into();
                })?;
                return Err(ApiError::internal(format!("启动 {kernel} 失败: {e}"))
                    .with_field("rolled_back", rolled));
            }
        };
        let pid = child.id();
        self.children
            .lock()
            .unwrap()
            .insert(kernel.to_string(), Arc::new(Mutex::new(child)));
        let started = util::now_iso();
        self.set_state(kernel, |e| {
            e.status = "starting".into();
            e.pid = pid;
            e.started_at = started.clone();
            e.started_at_secs = util::now_secs();
            e.stopped_at.clear();
            e.config_path = path.display().to_string();
            e.config_sha256 = sha.clone();
            e.bin_path = bin.path.clone();
            e.bin_version = bin.version.clone();
            e.last_error.clear();
            e.alert.clear();
            e.rolled_back_to.clear();
            e.last_exit_code = None;
            e.adopted = false;
            e.log_path = log.display().to_string();
            e.last_action = if auto { "auto_start" } else { "start" }.into();
        })?;

        thread::sleep(Duration::from_millis(START_SETTLE_MS));
        let early_exit = {
            let children = self.children.lock().unwrap();
            match children.get(kernel) {
                Some(c) => {
                    let mut guard = c.lock().unwrap();
                    guard.try_wait().ok().flatten().map(|s| s.code())
                }
                None => None,
            }
        };
        if let Some(code) = early_exit {
            self.children.lock().unwrap().remove(kernel);
            let rolled = self.rollback(kernel, &sha)?;
            let tail = self.log_tail(kernel, 20);
            self.set_state(kernel, |e| {
                e.status = "failed".into();
                e.pid = 0;
                e.last_exit_code = code;
                e.last_exit_at = util::now_iso();
                e.last_error = format!("进程启动后立即退出（exit={code:?}）");
                e.rolled_back_to = rolled.clone();
            })?;
            store.audit(
                "kernel.start.failed",
                if auto { "system" } else { "admin" },
                json!({"kernel": kernel, "pid": pid, "exit_code": code, "rolled_back_to": rolled, "log_tail": tail["lines"]}),
            );
            return Err(ApiError::internal(format!("{kernel} 启动后立即退出（exit={code:?}）"))
                .with_field("log_tail", tail["lines"].to_string())
                .with_field("rolled_back", rolled));
        }

        let lkg = self.lkg_path(kernel);
        let _ = std::fs::create_dir_all(lkg.parent().unwrap_or(Path::new(&self.home)));
        let _ = std::fs::copy(&path, &lkg);
        self.set_state(kernel, |e| {
            e.status = "running".into();
            e.last_good_sha256 = sha.clone();
            e.updated_at = util::now_iso();
            if !auto {
                e.restarts = 0;
            }
        })?;
        store.audit(
            "kernel.start",
            if auto { "system" } else { "admin" },
            json!({"kernel": kernel, "pid": pid, "config_sha256": sha, "auto": auto}),
        );
        Ok(json!({
            "ok": true,
            "kernel": kernel,
            "status": "running",
            "pid": pid,
            "config_path": path.display().to_string(),
            "config_sha256": sha,
            "binary": {"path": bin.path, "version": bin.version},
            "ports": rendered.ports,
            "warnings": rendered.warnings,
            "auto_restart": auto,
        }))
    }

    fn rollback(&self, kernel: &str, attempted_sha: &str) -> ApiResult<String> {
        let lkg = self.lkg_path(kernel);
        if !lkg.exists() {
            return Ok(String::new());
        }
        let bytes = std::fs::read(&lkg).map_err(|e| ApiError::internal(e.to_string()))?;
        let sha = util::sha256_hex(&bytes);
        if sha == attempted_sha {
            return Ok(sha);
        }
        let path = self.config_path(kernel);
        util::write_atomic(&path, &bytes).map_err(ApiError::internal)?;
        Ok(sha)
    }

    fn set_state(&self, kernel: &str, f: impl FnOnce(&mut RuntimeEntry)) -> ApiResult<()> {
        {
            let mut rt = self.rt.lock().unwrap();
            let e = rt.entry(kernel.to_string()).or_insert_with(RuntimeEntry::default);
            f(e);
            e.updated_at = util::now_iso();
        }
        self.persist()
    }

    pub fn stop(&self, kernel: &str) -> ApiResult<Value> {
        if self.demo {
            return Err(ApiError::new(409, "demo_mode", "演示模式不操作真实内核"));
        }
        if !is_kernel(kernel) {
            return Err(ApiError::not_found(format!("未知内核 {kernel}")));
        }
        let pid = {
            let rt = self.rt.lock().unwrap();
            rt.get(kernel).map(|e| e.pid).unwrap_or(0)
        };
        if pid == 0 {
            self.set_state(kernel, |e| {
                e.status = "stopped".into();
                e.pid = 0;
                e.last_action = "stop".into();
                e.stopped_at = util::now_iso();
            })?;
            return Ok(json!({"ok": true, "kernel": kernel, "status": "stopped", "note": "进程未运行"}));
        }
        self.set_state(kernel, |e| {
            e.status = "stopping".into();
            e.last_action = "stop".into();
        })?;
        kill_process(pid);
        if let Some(c) = self.children.lock().unwrap().remove(kernel) {
            let mut guard = c.lock().unwrap();
            let _ = guard.try_wait();
        }
        self.set_state(kernel, |e| {
            e.status = "stopped".into();
            e.pid = 0;
            e.stopped_at = util::now_iso();
            e.adopted = false;
            e.alert.clear();
            e.last_error.clear();
        })?;
        Ok(json!({"ok": true, "kernel": kernel, "status": "stopped", "pid": pid}))
    }

    /// 面板启动时核对：能确认是本面板的进程就接管；不能确认归属的不杀；留下的孤儿清理。
    pub fn reconcile(&self, store: &Store) {
        if self.demo {
            return;
        }
        let entries: Vec<(String, RuntimeEntry)> = {
            let rt = self.rt.lock().unwrap();
            rt.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
        };
        for (k, e) in &entries {
            if e.pid == 0 || !matches!(e.status.as_str(), "running" | "starting" | "restarting" | "stopping") {
                continue;
            }
            if let Some(info) = process_info(e.pid) {
                let expected = exe_name(k);
                let name_ok = info.name.eq_ignore_ascii_case(&expected)
                    || info.name.eq_ignore_ascii_case(k);
                let cmd_ok = info.cmdline.contains(&e.config_path) || e.config_path.is_empty();
                if name_ok && cmd_ok {
                    let _ = self.set_state(k, |x| {
                        x.status = "running".into();
                        x.adopted = true;
                        x.last_action = "adopt".into();
                    });
                    store.audit("kernel.adopted", "system", json!({"kernel": k, "pid": e.pid, "config": e.config_path}));
                } else {
                    let _ = self.set_state(k, |x| {
                        x.status = "stopped".into();
                        x.pid = 0;
                        x.last_error = "pid 已被其它进程复用，未执行 kill".into();
                        x.last_action = "reconcile".into();
                    });
                    store.audit("kernel.pid.reused", "system", json!({"kernel": k, "pid": e.pid}));
                }
            } else {
                let _ = self.set_state(k, |x| {
                    x.status = "stopped".into();
                    x.pid = 0;
                    x.last_error = "面板重启时进程已不存在".into();
                    x.last_action = "reconcile".into();
                });
                store.audit("kernel.reconcile.missing", "system", json!({"kernel": k, "pid": e.pid}));
            }
        }

        // 孤儿：命令行里带本面板 unique 配置目录，但状态文件里没有对应运行记录。
        let marker = self.home.join("configs/generated").display().to_string();
        let known: Vec<u32> = self
            .rt
            .lock()
            .unwrap()
            .values()
            .filter(|e| e.pid > 0 && matches!(e.status.as_str(), "running" | "starting"))
            .map(|e| e.pid)
            .collect();
        for p in list_kernel_processes() {
            if !p.cmdline.contains(&marker) {
                continue;
            }
            if known.contains(&p.pid) {
                continue;
            }
            if kill_process(p.pid) {
                store.audit(
                    "kernel.orphan.killed",
                    "system",
                    json!({"pid": p.pid, "name": p.name, "cmdline": truncate(&p.cmdline, 300)}),
                );
            }
        }
    }

    pub fn start_monitor(self: &Arc<Self>, store: Arc<Store>) {
        if self.demo || self.monitor_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let sup = Arc::clone(self);
        thread::spawn(move || loop {
            thread::sleep(Duration::from_millis(MONITOR_INTERVAL_MS));
            let mut exited: Vec<(String, Option<i32>)> = Vec::new();
            {
                let children = sup.children.lock().unwrap();
                for (k, c) in children.iter() {
                    let mut guard = c.lock().unwrap();
                    match guard.try_wait() {
                        Ok(Some(s)) => exited.push((k.clone(), s.code())),
                        Ok(None) => {}
                        Err(_) => exited.push((k.clone(), None)),
                    }
                }
            }
            if !exited.is_empty() {
                sup.children.lock().unwrap().retain(|k, _| {
                    !exited.iter().any(|(x, _)| x == k)
                });
            }
            for (k, code) in exited {
                sup.handle_exit(&store, &k, code);
            }

            // 被接管的进程没有 Child 句柄，只能靠进程表核对。
            let adopted: Vec<(String, u32)> = {
                let rt = sup.rt.lock().unwrap();
                rt.iter()
                    .filter(|(_, e)| e.status == "running" && e.adopted && e.pid > 0)
                    .map(|(k, e)| (k.clone(), e.pid))
                    .collect()
            };
            let managed: Vec<String> = sup.children.lock().unwrap().keys().cloned().collect();
            for (k, pid) in adopted {
                if managed.contains(&k) {
                    continue;
                }
                if !process_alive(pid) {
                    sup.handle_exit(&store, &k, None);
                }
            }
        });
    }

    fn handle_exit(&self, store: &Store, kernel: &str, code: Option<i32>) {
        let action = {
            let mut rt = self.rt.lock().unwrap();
            let Some(e) = rt.get_mut(kernel) else {
                return;
            };
            if matches!(e.status.as_str(), "stopping" | "stopped") {
                e.status = "stopped".into();
                e.pid = 0;
                e.stopped_at = util::now_iso();
                Some(("stopped", String::new()))
            } else {
                e.pid = 0;
                e.last_exit_code = code;
                e.last_exit_at = util::now_iso();
                if !e.auto_restart {
                    e.status = "failed".into();
                    e.alert = "进程异常退出；自动拉起已关闭".into();
                    Some(("failed", String::new()))
                } else if e.restarts >= e.restart_limit {
                    e.status = "failed".into();
                    e.alert = format!(
                        "自动拉起已达上限（{} 次），已停止尝试；请查看日志后手动启动",
                        e.restart_limit
                    );
                    Some(("limit", String::new()))
                } else {
                    e.restarts += 1;
                    e.status = "auto_restarting".into();
                    e.last_error = format!("异常退出（exit={code:?}），第 {} 次自动拉起", e.restarts);
                    Some(("restart", String::new()))
                }
            }
        };
        let _ = self.persist();
        let Some((kind, _)) = action else { return };
        match kind {
            "stopped" => {
                store.audit("kernel.exited", "system", json!({"kernel": kernel, "exit_code": code, "status": "stopped"}));
                return;
            }
            "failed" => {
                let alert = {
                    let rt = self.rt.lock().unwrap();
                    rt.get(kernel).map(|e| e.alert.clone()).unwrap_or_default()
                };
                store.audit("kernel.alert", "system", json!({"kernel": kernel, "exit_code": code, "alert": alert}));
                return;
            }
            "limit" => {
                let alert = {
                    let rt = self.rt.lock().unwrap();
                    rt.get(kernel).map(|e| e.alert.clone()).unwrap_or_default()
                };
                store.audit("kernel.alert", "system", json!({"kernel": kernel, "exit_code": code, "alert": alert}));
                return;
            }
            _ => {}
        }
        let restarts = {
            let rt = self.rt.lock().unwrap();
            rt.get(kernel).map(|e| e.restarts).unwrap_or(0)
        };
        store.audit(
            "kernel.auto_restart",
            "system",
            json!({"kernel": kernel, "exit_code": code, "attempt": restarts}),
        );
        thread::sleep(Duration::from_millis(500));
        let still = {
            let rt = self.rt.lock().unwrap();
            rt.get(kernel)
                .map(|e| e.status == "auto_restarting")
                .unwrap_or(false)
        };
        if !still {
            return;
        }
        if let Err(err) = self.start_internal(store, kernel, true) {
            let _ = self.set_state(kernel, |e| {
                e.status = "failed".into();
                e.alert = format!("自动拉起失败：{}", err.message);
                e.last_error = err.message.clone();
            });
            store.audit(
                "kernel.alert",
                "system",
                json!({"kernel": kernel, "alert": err.message, "attempt": restarts}),
            );
        }
    }
}

struct Rendered {
    content: String,
    inbounds: usize,
    ports: Vec<Value>,
    warnings: Vec<String>,
}

fn clients_for(inbound: &Inbound) -> Vec<Value> {
    inbound
        .clients
        .iter()
        .filter(|c| c.enabled)
        .map(|c| {
            let mut o = Map::new();
            o.insert("id".into(), json!(c.uuid));
            if !c.flow.is_empty() {
                o.insert("flow".into(), json!(c.flow));
            }
            o.insert("email".into(), json!(c.email));
            Value::Object(o)
        })
        .collect()
}

fn render_xray(home: &Path, inbounds: &[Inbound], warnings: &mut Vec<String>) -> ApiResult<String> {
    let mut list = Vec::new();
    for i in inbounds {
        if i.protocol != "vless" {
            warnings.push(format!("{}: xray 渲染器暂只实现 vless，已跳过", i.tag));
            continue;
        }
        let mut inbound = Map::new();
        inbound.insert("tag".into(), json!(i.tag));
        inbound.insert("listen".into(), json!(if i.listen.is_empty() { "0.0.0.0" } else { &i.listen }));
        inbound.insert("port".into(), json!(i.port));
        inbound.insert("protocol".into(), json!("vless"));
        inbound.insert(
            "settings".into(),
            json!({"clients": clients_for(i), "decryption": "none"}),
        );
        if clients_for(i).is_empty() {
            warnings.push(format!("{}: 没有启用的客户端，配置自检可能失败", i.tag));
        }
        let mut stream = Map::new();
        stream.insert("network".into(), json!(if i.network.is_empty() { "tcp" } else { &i.network }));
        stream.insert("security".into(), json!(if i.security.is_empty() { "none" } else { &i.security }));
        if i.security == "reality" {
            stream.insert(
                "realitySettings".into(),
                json!({
                    "show": false,
                    "dest": i.reality.dest,
                    "xver": i.reality.xver,
                    "serverNames": i.reality.server_names,
                    "privateKey": i.reality.private_key,
                    "shortIds": i.reality.short_ids,
                }),
            );
        } else if i.security == "tls" {
            stream.insert(
                "tlsSettings".into(),
                json!({
                    "serverName": i.tls.server_name,
                    "alpn": i.tls.alpn,
                    "certificates": [{
                        "certificateFile": resolve_path(home, &i.tls.cert_path),
                        "keyFile": resolve_path(home, &i.tls.key_path),
                    }],
                }),
            );
        }
        if i.network == "ws" {
            stream.insert("wsSettings".into(), json!({"path": "/"}));
        } else if i.network == "xhttp" {
            stream.insert("xhttpSettings".into(), json!({"path": "/"}));
        }
        inbound.insert("streamSettings".into(), Value::Object(stream));
        list.push(Value::Object(inbound));
    }
    let cfg = json!({
        "log": {"loglevel": "warning"},
        "inbounds": list,
        "outbounds": [{"protocol": "freedom", "tag": "direct"}],
    });
    serde_json::to_string_pretty(&cfg).map_err(|e| ApiError::internal(e.to_string()))
}

fn sb_tls(home: &Path, i: &Inbound) -> Value {
    if i.security == "reality" {
        let (host, port) = host_port(&i.reality.dest);
        json!({
            "enabled": true,
            "server_name": i.reality.server_names.first().cloned().unwrap_or(host.clone()),
            "reality": {
                "enabled": true,
                "handshake": {"server": host, "server_port": port},
                "private_key": i.reality.private_key,
                "short_id": i.reality.short_ids,
            }
        })
    } else {
        json!({
            "enabled": true,
            "server_name": i.tls.server_name,
            "certificate_path": resolve_path(home, &i.tls.cert_path),
            "key_path": resolve_path(home, &i.tls.key_path),
            "alpn": i.tls.alpn,
        })
    }
}

/// sing-box 的 VLESS 用户不接受 xray 的 `email` 字段，用 `name` 承载备注。
fn sb_users(inbound: &Inbound) -> Vec<Value> {
    inbound
        .clients
        .iter()
        .filter(|c| c.enabled)
        .map(|c| {
            let mut o = Map::new();
            o.insert("uuid".into(), json!(c.uuid));
            if !c.flow.is_empty() {
                o.insert("flow".into(), json!(c.flow));
            }
            if !c.email.is_empty() {
                o.insert("name".into(), json!(c.email));
            }
            Value::Object(o)
        })
        .collect()
}

fn render_sing_box(home: &Path, inbounds: &[Inbound], warnings: &mut Vec<String>) -> ApiResult<String> {
    let mut list = Vec::new();
    for i in inbounds {
        let mut inbound = Map::new();
        inbound.insert("type".into(), json!(i.protocol));
        inbound.insert("tag".into(), json!(i.tag));
        inbound.insert("listen".into(), json!(if i.listen.is_empty() { "0.0.0.0" } else { &i.listen }));
        inbound.insert("listen_port".into(), json!(i.port));
        match i.protocol.as_str() {
            "vless" => {
                inbound.insert("users".into(), json!(sb_users(i)));
                if i.security != "none" && !i.security.is_empty() {
                    inbound.insert("tls".into(), sb_tls(home, i));
                }
                if i.network == "ws" {
                    inbound.insert("transport".into(), json!({"type": "ws", "path": "/"}));
                } else if i.network == "xhttp" {
                    warnings.push(format!("{}: sing-box 未实现 xhttp 传输，配置自检可能失败", i.tag));
                }
            }
            "hysteria2" => {
                let users: Vec<Value> = i
                    .clients
                    .iter()
                    .filter(|c| c.enabled)
                    .map(|c| json!({"password": if c.password.is_empty() { &c.uuid } else { &c.password }}))
                    .collect();
                inbound.insert("users".into(), json!(users));
                inbound.insert("tls".into(), sb_tls(home, i));
            }
            "tuic" => {
                let users: Vec<Value> = i
                    .clients
                    .iter()
                    .filter(|c| c.enabled)
                    .map(|c| json!({"uuid": c.uuid, "password": c.password}))
                    .collect();
                inbound.insert("users".into(), json!(users));
                inbound.insert("tls".into(), sb_tls(home, i));
            }
            other => {
                warnings.push(format!("{}: sing-box 渲染器未实现协议 {other}，已跳过", i.tag));
                continue;
            }
        }
        list.push(Value::Object(inbound));
    }
    let cfg = json!({
        "log": {"level": "warn", "timestamp": true},
        "inbounds": list,
        "outbounds": [{"type": "direct", "tag": "direct"}],
    });
    serde_json::to_string_pretty(&cfg).map_err(|e| ApiError::internal(e.to_string()))
}

fn render_mihomo(inbounds: &[Inbound], warnings: &mut Vec<String>) -> ApiResult<String> {
    if !inbounds.is_empty() {
        warnings.push("mihomo 不是入站内核：这些入站不会被 mihomo 承载（仅作为进程与配置校验目标）".into());
    }
    let cfg = json!({
        "log-level": "warning",
        "mode": "direct",
        "allow-lan": false,
        "ipv6": false,
        "proxies": [],
        "rules": ["MATCH,DIRECT"],
    });
    serde_yaml::to_string(&cfg).map_err(|e| ApiError::internal(e.to_string()))
}
