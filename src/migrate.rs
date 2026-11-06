//! 3x-ui / x-ui 只读迁移。
//!
//! 硬约束：
//! 1. 绝不写源库：源库存在 `-wal` / `-shm` 时完全不打开源库，改为「文件拷贝到临时目录 + 在副本上 VACUUM INTO」；
//!    源库干净关闭时用只读连接 + `VACUUM INTO` 生成事务级快照。所有 SQL 只读，唯一的写落点是我们自己的临时副本。
//! 2. 先预览后落地：预览返回 import / skip / conflict 三类，apply 必须带上一轮 `preview_id` + `plan_hash` + `confirm: true`；
//!    数据在预览后变化则 409，不写入任何入站。
//! 3. 幂等：`source_ref = 3xui:<源路径指纹>:<源 inbound id>`，重复运行命中即跳过，不产生重复入站。
//!
//! 字段映射以真实 3x-ui v2.9.4 源码（`database/model/model.go`、`xray/client_traffic.go`）为准，
//! 旧版 x-ui 为 `vaxilu/x-ui` 的 `database/model/model.go`。

use crate::error::{ApiError, ApiResult};
use crate::model::{Client, Inbound, RealityConfig, TlsConfig};
use crate::state::Store;
use crate::util;
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

const MAX_DB_BYTES: u64 = 512 * 1024 * 1024;

struct Snapshot {
    work: PathBuf,
    path: PathBuf,
    strategy: String,
    integrity: String,
    source_path: PathBuf,
    source_bytes: u64,
    source_sha256: String,
    source_sha256_after: String,
    wal_present: bool,
    wal_bytes: u64,
    shm_present: bool,
    modified_during_capture: bool,
    consistency: Vec<String>,
}

impl Snapshot {
    fn json(&self) -> Value {
        json!({
            "path": self.source_path.display().to_string(),
            "bytes": self.source_bytes,
            "sha256": self.source_sha256,
            "sha256_after_capture": self.source_sha256_after,
            "modified_during_capture": self.modified_during_capture,
            "wal_present": self.wal_present,
            "wal_bytes": self.wal_bytes,
            "shm_present": self.shm_present,
            "snapshot_strategy": self.strategy,
            "snapshot_path": self.path.display().to_string(),
            "snapshot_integrity": self.integrity,
            "consistency": self.consistency,
        })
    }
}

#[derive(Debug, Clone)]
struct SchemaInfo {
    tables: Vec<String>,
    inbound_columns: Vec<String>,
    flavor: String,
    has_client_traffics: bool,
    notes: Vec<String>,
}

impl SchemaInfo {
    fn json(&self) -> Value {
        json!({
            "flavor": self.flavor,
            "tables": self.tables,
            "inbound_columns": self.inbound_columns,
            "has_client_traffics": self.has_client_traffics,
            "notes": self.notes,
        })
    }
}

#[derive(Debug, Clone)]
struct SourceRow {
    id: i64,
    remark: String,
    enable: bool,
    listen: String,
    port: i64,
    protocol: String,
    settings: String,
    stream_settings: String,
    tag: String,
    up: i64,
    down: i64,
    total: i64,
    expiry_time: i64,
}

#[derive(Debug, Clone, Default)]
struct TrafficRow {
    up: i64,
    down: i64,
    total: i64,
}

struct Item {
    kind: &'static str,
    source_id: i64,
    remark: String,
    protocol: String,
    port: i64,
    listen: String,
    tag: String,
    mapped: Option<Inbound>,
    reasons: Vec<String>,
    warnings: Vec<String>,
    suggestion: String,
    already_imported: bool,
    signature: String,
}

impl Item {
    fn json(&self) -> Value {
        let mapped = self.mapped.as_ref().map(|i| i.redacted());
        json!({
            "kind": self.kind,
            "source_id": self.source_id,
            "remark": self.remark,
            "protocol": self.protocol,
            "port": self.port,
            "listen": self.listen,
            "tag": self.tag,
            "mapped": mapped,
            "private_key_present": self.mapped.as_ref().map(|i| !i.reality.private_key.is_empty()).unwrap_or(false),
            "reasons": self.reasons,
            "warnings": self.warnings,
            "suggestion": self.suggestion,
            "already_imported": self.already_imported,
        })
    }
}

struct Plan {
    snapshot: Snapshot,
    schema: SchemaInfo,
    items: Vec<Item>,
    plan_hash: String,
    preview_id: String,
    source_prefix: String,
}

impl Plan {
    fn counts(&self) -> Value {
        let mut import = 0usize;
        let mut skip = 0usize;
        let mut conflict = 0usize;
        let mut already = 0usize;
        for i in &self.items {
            match i.kind {
                "import" => import += 1,
                "conflict" => conflict += 1,
                _ => skip += 1,
            }
            if i.already_imported {
                already += 1;
            }
        }
        json!({
            "import": import,
            "skip": skip,
            "conflict": conflict,
            "already_imported": already,
            "source_rows": self.items.len(),
        })
    }
}

fn file_sha(path: &Path) -> Result<String, String> {
    let meta = fs::metadata(path).map_err(|e| format!("读取 {} 元数据失败: {e}", path.display()))?;
    if meta.len() > MAX_DB_BYTES {
        return Err(format!(
            "数据库 {} 超过 {} MB 上限，拒绝一次性读入内存",
            path.display(),
            MAX_DB_BYTES / 1024 / 1024
        ));
    }
    let bytes = fs::read(path).map_err(|e| format!("读取 {} 失败: {e}", path.display()))?;
    Ok(util::sha256_hex(&bytes))
}

fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_os_string();
    s.push(suffix);
    PathBuf::from(s)
}

fn integrity_check(conn: &Connection) -> Result<String, String> {
    conn.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
        .map_err(|e| format!("PRAGMA integrity_check 失败: {e}"))
}

fn vacuum_readonly(src: &Path, dst: &Path) -> Result<(), String> {
    // 只用 SQLITE_OPEN_READ_ONLY 句柄：任何 UPDATE/INSERT/DELETE 会在 SQLite 层直接报
    // "attempt to write a readonly database"。不能加 PRAGMA query_only，因为它会把 VACUUM 也一并禁掉
    // （实测：query_only=ON 时 VACUUM INTO 失败）。
    let conn = Connection::open_with_flags(src, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| format!("只读打开源库失败: {e}"))?;
    let dst_sql = dst.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{dst_sql}'"))
        .map_err(|e| format!("VACUUM INTO 失败: {e}"))?;
    drop(conn);
    Ok(())
}

fn copy_snapshot(src: &Path, wal: &Path, dst: &Path) -> Result<(), String> {
    let copy = sidecar(dst, ".copy.db");
    fs::copy(src, &copy).map_err(|e| format!("拷贝源库失败: {e}"))?;
    if wal.is_file() {
        let copy_wal = sidecar(&copy, "-wal");
        fs::copy(wal, &copy_wal).map_err(|e| format!("拷贝 -wal 失败: {e}"))?;
    }
    let conn = Connection::open(&copy).map_err(|e| format!("打开副本失败: {e}"))?;
    let integrity = integrity_check(&conn)?;
    if integrity != "ok" {
        drop(conn);
        let _ = fs::remove_file(&copy);
        let _ = fs::remove_file(sidecar(&copy, "-wal"));
        return Err(format!(
            "拷贝快照未通过 integrity_check（{integrity}）。面板可能正在写入，请稍后重试或在 3x-ui 面板停止时迁移；未修改源库。"
        ));
    }
    let dst_sql = dst.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{dst_sql}'"))
        .map_err(|e| format!("副本 VACUUM INTO 失败: {e}"))?;
    drop(conn);
    let _ = fs::remove_file(&copy);
    let _ = fs::remove_file(sidecar(&copy, "-wal"));
    let _ = fs::remove_file(sidecar(&copy, "-shm"));
    Ok(())
}

fn take_snapshot(store: &Store, src: &Path) -> ApiResult<Snapshot> {
    let meta = fs::metadata(src)
        .map_err(|e| ApiError::bad_request(format!("无法读取数据库 {}: {e}", src.display())))?;
    if !meta.is_file() {
        return Err(ApiError::bad_request(format!(
            "{} 不是普通文件（如果是目录，请指向 x-ui.db）",
            src.display()
        )));
    }
    let sha_before = file_sha(src).map_err(ApiError::bad_request)?;
    let wal = sidecar(src, "-wal");
    let shm = sidecar(src, "-shm");
    let wal_present = wal.is_file();
    let wal_bytes = fs::metadata(&wal).map(|m| m.len()).unwrap_or(0);
    let shm_present = shm.is_file();

    let work = store
        .home_path("tmp/migrate")
        .join(format!("mig_{}", util::new_id("s")));
    fs::create_dir_all(&work).map_err(|e| ApiError::internal(format!("创建迁移临时目录失败: {e}")))?;
    let dst = work.join("snapshot.db");

    let mut consistency: Vec<String> = Vec::new();
    let strategy;

    // 源库带 WAL：为绝对不写源库及其 -shm，连只读连接都不开，直接拷贝文件。
    if wal_present || shm_present {
        copy_snapshot(src, &wal, &dst)
            .map_err(|e| ApiError::new(422, "snapshot_inconsistent", e))?;
        strategy = "file_copy（源库存在 -wal/-shm，未打开源库）".to_string();
        consistency.push(format!(
            "检测到 -wal（{wal_bytes} 字节）：3x-ui 面板很可能正在运行。本次不打开源库，改为拷贝 db + -wal 到临时目录；文件拷贝不是 SQLite 事务级快照，若拷贝期间正好提交可能得到撕裂页，已用 integrity_check 拦截。"
        ));
        consistency.push(
            "建议在面板低写入时段迁移；需要严格事务级一致性时，先停止 3x-ui 面板再迁移（本工具不会替你停进程）。"
                .to_string(),
        );
    } else {
        match vacuum_readonly(src, &dst) {
            Ok(()) => {
                strategy = "vacuum_into_readonly（只读连接 + VACUUM INTO）".to_string();
                consistency.push(
                    "未发现 -wal/-shm：源库可能已干净关闭。使用 SQLITE_OPEN_READ_ONLY 只读连接 + VACUUM INTO 生成事务级快照；只读句柄在 SQLite 层禁止一切写语句。"
                        .to_string(),
                );
            }
            Err(e) => {
                copy_snapshot(src, &wal, &dst).map_err(|e2| {
                    ApiError::new(
                        422,
                        "snapshot_inconsistent",
                        format!("只读快照失败（{e}），回退文件拷贝也失败（{e2}）；未修改源库"),
                    )
                })?;
                strategy = "file_copy（只读快照失败后回退）".to_string();
                consistency.push(format!("只读 VACUUM INTO 失败（{e}），已回退文件拷贝。"));
            }
        }
    }

    if !dst.is_file() {
        return Err(ApiError::internal("快照文件未生成"));
    }
    let sha_after = file_sha(src).unwrap_or_default();
    let modified_during_capture = !sha_before.is_empty() && sha_before != sha_after;
    if modified_during_capture {
        consistency.push(
            "抓取期间源库主文件哈希发生变化：存在并发写入。快照可能落后于最新状态，请重新预览确认差异。"
                .to_string(),
        );
    }

    let conn = Connection::open_with_flags(&dst, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| ApiError::internal(format!("只读打开快照失败: {e}")))?;
    let integrity = integrity_check(&conn).map_err(|e| {
        ApiError::new(
            422,
            "snapshot_inconsistent",
            format!("快照完整性校验失败（{e}）；未修改源库"),
        )
    })?;
    drop(conn);
    if integrity != "ok" {
        return Err(ApiError::new(
            422,
            "snapshot_inconsistent",
            format!("快照 integrity_check 未通过（{integrity}）；未修改源库，请重试或停止面板后迁移"),
        ));
    }

    Ok(Snapshot {
        work,
        path: dst,
        strategy,
        integrity,
        source_path: src.to_path_buf(),
        source_bytes: meta.len(),
        source_sha256: sha_before,
        source_sha256_after: sha_after,
        wal_present,
        wal_bytes,
        shm_present,
        modified_during_capture,
        consistency,
    })
}

fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>, String> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| format!("PRAGMA table_info({table}) 失败: {e}"))?;
    let rows = stmt
        .query_map([], |r| r.get::<_, String>(1))
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn probe(conn: &Connection) -> ApiResult<SchemaInfo> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|e| ApiError::new(422, "unsupported_database", format!("读取 sqlite_master 失败: {e}")))?;
    let tables: Vec<String> = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(|e| ApiError::new(422, "unsupported_database", e.to_string()))?
        .filter_map(|r| r.ok())
        .collect();
    if !tables.iter().any(|t| t == "inbounds") {
        return Err(ApiError::new(
            422,
            "unsupported_database",
            "不是 x-ui / 3x-ui 的 SQLite 数据库：缺少 inbounds 表（PostgreSQL 后端不支持）",
        ));
    }
    let cols = table_columns(conn, "inbounds")
        .map_err(|e| ApiError::new(422, "unsupported_database", e))?;
    for required in [
        "id",
        "remark",
        "enable",
        "listen",
        "port",
        "protocol",
        "settings",
        "stream_settings",
        "tag",
    ] {
        if !cols.iter().any(|c| c == required) {
            return Err(ApiError::new(
                422,
                "unsupported_database",
                format!("inbounds 表缺少必需列 {required}；不是受支持的 x-ui / 3x-ui schema"),
            ));
        }
    }
    let has_client_traffics = tables.iter().any(|t| t == "client_traffics");
    let flavor = if cols.iter().any(|c| c == "exclude_from_sub")
        || cols.iter().any(|c| c == "origin_node_guid")
        || cols.iter().any(|c| c == "disable_flow")
    {
        "3x-ui v3.x"
    } else if cols.iter().any(|c| c == "traffic_reset") || cols.iter().any(|c| c == "all_time") {
        "3x-ui v2.x"
    } else {
        "x-ui（旧版）"
    };

    let mut notes = vec![
        "表名 `inbounds`，字段 `settings` / `stream_settings` 是 JSON 字符串；`port` 是整数，`enable` 是 0/1。"
            .to_string(),
        "3x-ui 的 `client_traffics.email` 全局唯一，按 email 关联客户端流量；旧版 x-ui 没有该表。"
            .to_string(),
        "`settings.clients[].totalGB` 字段名有误导：源码写入前乘 1024^3，库里实际是字节。"
            .to_string(),
        "`expiryTime` 是毫秒时间戳（0 = 不限期；负数在 UI 表示“剩余 N 天”，本工具按 0 处理）。"
            .to_string(),
    ];
    if flavor == "x-ui（旧版）" {
        notes.push(
            "旧版 x-ui 与 3x-ui 的主要差异：无 all_time / traffic_reset / client_traffics；port 列在旧版带 unique 约束，同端口不同 listen 也建不了。"
                .to_string(),
        );
    }
    if flavor == "3x-ui v3.x" {
        notes.push(
            "3x-ui v3 的 inbounds 新增 exclude_from_sub / node_id / share_addr 等列；本工具只读取迁移需要的列，其余忽略。"
                .to_string(),
        );
    }
    Ok(SchemaInfo {
        tables,
        inbound_columns: cols,
        flavor: flavor.to_string(),
        has_client_traffics,
        notes,
    })
}

fn load_rows(conn: &Connection) -> ApiResult<Vec<SourceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, remark, enable, listen, port, protocol, settings, stream_settings, tag, up, down, total, expiry_time FROM inbounds ORDER BY id",
        )
        .map_err(|e| ApiError::new(422, "unsupported_database", format!("读取 inbounds 失败: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok(SourceRow {
                id: r.get::<_, i64>(0)?,
                remark: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                enable: r.get::<_, Option<i64>>(2)?.unwrap_or(1) != 0,
                listen: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                port: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                protocol: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                settings: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
                stream_settings: r.get::<_, Option<String>>(7)?.unwrap_or_default(),
                tag: r.get::<_, Option<String>>(8)?.unwrap_or_default(),
                up: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
                down: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
                total: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
                expiry_time: r.get::<_, Option<i64>>(12)?.unwrap_or(0),
            })
        })
        .map_err(|e| ApiError::new(422, "unsupported_database", format!("遍历 inbounds 失败: {e}")))?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r.map_err(|e| ApiError::internal(format!("读取 inbounds 行失败: {e}")))?);
    }
    Ok(out)
}

fn load_traffic(conn: &Connection, schema: &SchemaInfo) -> ApiResult<HashMap<String, TrafficRow>> {
    let mut out = HashMap::new();
    if !schema.has_client_traffics {
        return Ok(out);
    }
    let mut stmt = match conn.prepare("SELECT email, up, down, total FROM client_traffics") {
        Ok(s) => s,
        Err(_) => return Ok(out),
    };
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                TrafficRow {
                    up: r.get::<_, Option<i64>>(1)?.unwrap_or(0),
                    down: r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                    total: r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                },
            ))
        })
        .map_err(|e| ApiError::internal(format!("读取 client_traffics 失败: {e}")))?;
    for r in rows {
        let (email, t) = r.map_err(|e| ApiError::internal(e.to_string()))?;
        if !email.is_empty() {
            out.insert(email.to_ascii_lowercase(), t);
        }
    }
    Ok(out)
}

fn parse_json(raw: &str, what: &str) -> Result<Value, String> {
    if raw.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(raw).map_err(|e| format!("{what} 不是合法 JSON: {e}"))
}

fn stable_id(prefix: &str, seed: &str) -> String {
    format!("{}_{}", prefix, &util::sha256_hex(seed.as_bytes())[0..12])
}

fn ms_to_iso(ms: i64) -> Option<String> {
    if ms <= 0 {
        return None;
    }
    let secs = (ms / 1000) as u64;
    if secs == 0 {
        None
    } else {
        Some(util::unix_to_iso(secs))
    }
}

fn str_field<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(|x| x.as_str()).unwrap_or("")
}

fn i64_field(v: &Value, key: &str) -> i64 {
    v.get(key).and_then(|x| x.as_i64()).unwrap_or(0)
}

fn bool_field(v: &Value, key: &str, default: bool) -> bool {
    v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
}

struct Candidate {
    item: Item,
}

fn base_item(row: &SourceRow) -> Item {
    Item {
        kind: "skip",
        source_id: row.id,
        remark: row.remark.clone(),
        protocol: row.protocol.clone(),
        port: row.port,
        listen: row.listen.clone(),
        tag: row.tag.clone(),
        mapped: None,
        reasons: Vec::new(),
        warnings: Vec::new(),
        suggestion: String::new(),
        already_imported: false,
        signature: String::new(),
    }
}

fn unknown_reason(protocol: &str) -> String {
    match protocol {
        "vmess" | "trojan" | "shadowsocks" | "hysteria" | "hysteria2" | "tuic" | "wireguard"
        | "http" | "mixed" | "tunnel" | "socks" | "mtproto" | "masquerade" => format!(
            "协议 {protocol} 已在 3x-ui 中存在，但 Berthio 当前导出器只实现 VLESS 渲染；为避免导入后订阅不可用，本条跳过"
        ),
        other => format!("未知协议 {other}（不在 3x-ui v2.9.4 协议枚举内）"),
    }
}

fn build_candidate(
    row: &SourceRow,
    traffic: &HashMap<String, TrafficRow>,
    source_prefix: &str,
    now: &str,
) -> Candidate {
    let mut item = base_item(row);
    let settings = match parse_json(&row.settings, "settings") {
        Ok(v) => v,
        Err(e) => {
            item.reasons.push(e);
            return Candidate { item };
        }
    };
    let stream = match parse_json(&row.stream_settings, "stream_settings") {
        Ok(v) => v,
        Err(e) => {
            item.reasons.push(e);
            return Candidate { item };
        }
    };

    let protocol = row.protocol.to_ascii_lowercase();
    if protocol != "vless" {
        item.reasons.push(unknown_reason(&protocol));
        return Candidate { item };
    }

    let network = {
        let n = str_field(&stream, "network");
        if n.is_empty() { "tcp".to_string() } else { n.to_ascii_lowercase() }
    };
    if !matches!(network.as_str(), "tcp" | "ws" | "xhttp") {
        item.reasons.push(format!(
            "传输方式 {network} 的导出渲染未实现（Berthio 当前支持 tcp / ws / xhttp），导入后会导出错误配置"
        ));
        return Candidate { item };
    }
    if network == "tcp" {
        let header_type = stream
            .get("tcpSettings")
            .and_then(|v| v.get("header"))
            .and_then(|v| v.get("type"))
            .and_then(|v| v.as_str())
            .unwrap_or("none");
        if header_type != "none" && !header_type.is_empty() {
            item.reasons.push(format!(
                "tcpSettings.header.type={header_type}（HTTP 伪装）未实现，导入后导出会丢字段"
            ));
            return Candidate { item };
        }
    }
    if network == "ws" {
        let path = stream
            .get("wsSettings")
            .and_then(|v| v.get("path"))
            .and_then(|v| v.as_str())
            .unwrap_or("/");
        let host = stream
            .get("wsSettings")
            .and_then(|v| v.get("headers"))
            .and_then(|v| v.get("Host"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if path != "/" || !host.is_empty() {
            item.kind = "conflict";
            item.reasons.push(format!(
                "WS 参数 path={path} host={host} 与 Berthio 导出器的固定值（path=/, Host=SNI）不一致，直接导入会导出不可用订阅"
            ));
            item.suggestion = "把源库 ws path 改为 / 且 Host 留空后重新预览，或手工建站".to_string();
            return Candidate { item };
        }
    }
    if network == "xhttp" {
        item.warnings
            .push("xhttp 传输：目标客户端支持情况未验证（与 M1 的风险标记一致）".to_string());
    }

    let security = {
        let s = str_field(&stream, "security");
        if s.is_empty() { "none".to_string() } else { s.to_ascii_lowercase() }
    };
    if !matches!(security.as_str(), "tls" | "reality") {
        item.reasons.push(format!(
            "security={security}：Berthio 当前只渲染 tls / reality 的 VLESS 客户端；无传输加密或其它安全层会导出错误配置"
        ));
        return Candidate { item };
    }

    let mut tls = TlsConfig::default();
    let mut reality = RealityConfig::default();
    let mut tls_paths: Vec<String> = Vec::new();
    if security == "tls" {
        let ts = stream.get("tlsSettings").cloned().unwrap_or(json!({}));
        tls.mode = "tls".to_string();
        tls.server_name = str_field(&ts, "serverName").to_string();
        tls.alpn = ts
            .get("alpn")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        if let Some(certs) = ts.get("certificates").and_then(|v| v.as_array()) {
            if let Some(first) = certs.first() {
                tls.cert_path = str_field(first, "certificateFile").to_string();
                tls.key_path = str_field(first, "keyFile").to_string();
                if !tls.cert_path.is_empty() {
                    tls_paths.push(tls.cert_path.clone());
                }
                if !tls.key_path.is_empty() {
                    tls_paths.push(tls.key_path.clone());
                }
            }
        }
        if tls.server_name.is_empty() {
            item.warnings
                .push("TLS 未设置 serverName；导出客户端将缺少 SNI".to_string());
        }
    } else {
        let rs = stream.get("realitySettings").cloned().unwrap_or(json!({}));
        reality.dest = str_field(&rs, "dest").to_string();
        reality.xver = rs.get("xver").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        reality.server_names = rs
            .get("serverNames")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        reality.short_ids = rs
            .get("shortIds")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        // 3x-ui v2.9.4 把 privateKey 放在 realitySettings 顶层（UI JS 读 json.privateKey）；
        // 个别改版可能塞进 settings，做一次兼容回退，避免误判冲突。
        let mut pk = str_field(&rs, "privateKey").to_string();
        let inner = rs.get("settings").cloned().unwrap_or(json!({}));
        if pk.is_empty() {
            pk = str_field(&inner, "privateKey").to_string();
        }
        reality.private_key = pk;
        reality.public_key = str_field(&inner, "publicKey").to_string();
        reality.fingerprint = {
            let f = str_field(&inner, "fingerprint");
            if f.is_empty() { "chrome".to_string() } else { f.to_string() }
        };
        reality.spider_x = str_field(&inner, "spiderX").to_string();
        if reality.private_key.is_empty() {
            item.kind = "conflict";
            item.reasons.push(
                "Reality 缺少 privateKey：xray 服务端只认 privateKey；没有它无法启动入站（客户端 publicKey 不能替代）"
                    .to_string(),
            );
            item.suggestion =
                "在 3x-ui 中重新生成 Reality 密钥对（privateKey 给服务端、publicKey 给客户端），再重新预览"
                    .to_string();
            return Candidate { item };
        }
        if reality.public_key.is_empty() {
            item.kind = "conflict";
            item.reasons.push(
                "Reality 缺少 settings.publicKey：Berthio 用 publicKey 渲染客户端订阅；缺失会导出不可用节点"
                    .to_string(),
            );
            item.suggestion = "在 3x-ui 的 Reality 设置里确认 publicKey（与 privateKey 成对）后重新预览；若源库确实没有，可用同版本内核派生：xray x25519 -i \"<privateKey>\"（本机 25.6.8 实测会输出对应的 Public key）".to_string();
            return Candidate { item };
        }
        if reality.server_names.is_empty() || reality.short_ids.is_empty() {
            item.kind = "conflict";
            item.reasons.push(
                "Reality 的 serverNames / shortIds 为空：客户端无法完成握手，导出会被 L1 校验拦截".to_string(),
            );
            item.suggestion = "补齐 Reality 的 serverNames 与 shortIds 后重新预览".to_string();
            return Candidate { item };
        }
        if reality.dest.is_empty() {
            item.warnings
                .push("Reality dest 为空：服务端回落目标缺失".to_string());
        }
    }

    if let Some(fallbacks) = settings.get("fallbacks").and_then(|v| v.as_array()) {
        if !fallbacks.is_empty() {
            item.kind = "conflict";
            item.reasons.push(format!(
                "settings.fallbacks 有 {} 条回落链：属于外部依赖，Berthio 入站模型不表达，直接导入会丢链路",
                fallbacks.len()
            ));
            item.suggestion = "先在 3x-ui 移除 fallbacks，或把回落入站手工重建后再迁移".to_string();
            return Candidate { item };
        }
    }

    if let Some(ext) = stream.get("externalProxy") {
        if ext.as_array().map(|a| !a.is_empty()).unwrap_or(false) {
            item.kind = "conflict";
            item.reasons.push(
                "streamSettings.externalProxy 非空：出站/外部地址映射不迁移，导入后客户端地址会错".to_string(),
            );
            item.suggestion = "在 3x-ui 删除 externalProxy 或手工建站".to_string();
            return Candidate { item };
        }
    }

    if row.port <= 0 || row.port > 65535 {
        item.kind = "conflict";
        item.reasons.push(format!("端口 {} 非法（必须 1–65535）", row.port));
        item.suggestion = "在源库改为合法端口后重新预览".to_string();
        return Candidate { item };
    }

    let clients_json = settings
        .get("clients")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    if clients_json.is_empty() {
        item.reasons
            .push("settings.clients 为空：没有可迁移的客户端".to_string());
        return Candidate { item };
    }

    let mut clients: Vec<Client> = Vec::new();
    let mut emails: HashSet<String> = HashSet::new();
    let mut uuids: HashSet<String> = HashSet::new();
    let mut client_conflict = false;
    for (idx, c) in clients_json.iter().enumerate() {
        let uuid = str_field(c, "id").to_string();
        if uuid.is_empty() {
            item.kind = "conflict";
            item.reasons.push(format!("第 {} 个客户端缺少 id（UUID）", idx + 1));
            item.suggestion = "在 3x-ui 重新生成该客户端后再迁移".to_string();
            client_conflict = true;
            break;
        }
        if !uuids.insert(uuid.clone()) {
            item.kind = "conflict";
            item.reasons
                .push(format!("客户端 UUID {uuid} 在同一入站内重复"));
            item.suggestion = "先去重再迁移".to_string();
            client_conflict = true;
            break;
        }
        let mut email = str_field(c, "email").to_string();
        if email.is_empty() {
            email = format!("unnamed-{}-{}@3xui", row.id, idx + 1);
            item.warnings
                .push(format!("第 {} 个客户端没有 email，已生成占位 {email}", idx + 1));
        }
        if !emails.insert(email.to_ascii_lowercase()) {
            item.kind = "conflict";
            item.reasons
                .push(format!("客户端 email {email} 在同一入站内重复（client_traffics 以 email 为唯一键）"));
            item.suggestion = "先把 email 改成唯一值再迁移".to_string();
            client_conflict = true;
            break;
        }
        let t = traffic.get(&email.to_ascii_lowercase());
        let used = t.map(|x| x.up.max(0) + x.down.max(0)).unwrap_or(0);
        let quota_from_settings = i64_field(c, "totalGB").max(0);
        let quota = if quota_from_settings > 0 {
            quota_from_settings
        } else {
            t.map(|x| x.total.max(0)).unwrap_or(0)
        };
        let flow = str_field(c, "flow").to_string();
        let limit_ip = i64_field(c, "limitIp").max(0).min(u32::MAX as i64) as u32;
        let expiry = ms_to_iso(i64_field(c, "expiryTime"));
        let enabled = bool_field(c, "enable", true);
        let mut warnings = Vec::new();
        if t.is_none() {
            warnings.push("没有 client_traffics 行：已用流量按 0 处理（旧版 x-ui 无该表）".to_string());
        }
        if i64_field(c, "totalGB") < 0 {
            warnings.push("totalGB 为负数（3x-ui 里表示“剩余 N 天”或无限），本次按 0 处理".to_string());
        }
        item.warnings.extend(warnings);
        clients.push(Client {
            id: stable_id("cli", &format!("{source_prefix}:{}:{email}", row.id)),
            email: email.clone(),
            uuid,
            password: String::new(),
            flow,
            limit_ip,
            quota_bytes: quota as u64,
            used_bytes: used as u64,
            expires_at: expiry,
            enabled,
            disabled_reason: if enabled { String::new() } else { "源库客户端被停用".to_string() },
            created_at: now.to_string(),
            updated_at: now.to_string(),
        });
    }
    if client_conflict {
        return Candidate { item };
    }

    let tag = {
        let t = row.tag.trim();
        if t.is_empty() {
            format!("{}-{}", util::slug(&row.remark, "inbound"), row.port)
        } else {
            t.to_string()
        }
    };
    let remark = if row.remark.trim().is_empty() {
        format!("3x-ui 入站 {}（端口 {}）", row.id, row.port)
    } else {
        row.remark.clone()
    };
    let listen = if row.listen.trim().is_empty() {
        "0.0.0.0".to_string()
    } else {
        row.listen.trim().to_string()
    };
    let source_ref = format!("3xui:{source_prefix}:{}", row.id);

    let inbound = Inbound {
        id: stable_id("inb", &source_ref),
        tag: tag.clone(),
        remark: remark.clone(),
        protocol: "vless".to_string(),
        listen: listen.clone(),
        port: row.port as u16,
        l4: "tcp".to_string(),
        network: network.clone(),
        security: security.clone(),
        tls,
        reality,
        server_kernel: "xray".to_string(),
        server_version: String::new(),
        enabled: row.enable,
        source: "3x-ui".to_string(),
        source_ref: source_ref.clone(),
        created_at: now.to_string(),
        updated_at: now.to_string(),
        clients: clients.clone(),
    };

    item.kind = "import";
    item.tag = tag;
    item.remark = remark;
    item.listen = listen;
    item.mapped = Some(inbound.clone());

    let mut inherited = vec![
        "remark / tag / listen / port / enabled".to_string(),
        "protocol=vless / network / security".to_string(),
        "客户端 uuid / email / flow / limitIp / 配额 / 到期 / 启用状态".to_string(),
        "客户端已用流量（client_traffics 抓取时点快照）".to_string(),
        if security == "reality" {
            "Reality dest / serverNames / shortIds / publicKey / privateKey / fingerprint / spiderX"
                .to_string()
        } else {
            "TLS serverName / ALPN".to_string()
        },
    ];
    if !tls_paths.is_empty() {
        inherited.push(format!(
            "TLS 证书路径仅作为引用记录：{}（文件本体不迁移）",
            tls_paths.join(", ")
        ));
    }
    item.warnings.push(if security == "reality" {
        "Reality privateKey 随迁移写入 Berthio 状态，仅落盘；API 响应只显示是否存在的占位。".to_string()
    } else {
        "TLS 证书文件不随数据库迁移：certPath/keyPath 只是源服务器路径引用，需在 Berthio 重新签发或上传证书后才可启动服务端。".to_string()
    });
    item.warnings.push(format!(
        "源入站 up={} / down={} / total={} / expiryTime={}：inbound 级统计、配额与到期策略未迁移（Berthio 没有 inbound 级配额模型）",
        row.up, row.down, row.total, row.expiry_time
    ));
    item.warnings.push(
        "sniffing / fallbacks / sockopt / 客户端 subId / tgId / comment / reset / lastOnline 未迁移".to_string(),
    );
    item.warnings.push(
        "客户端流量统计是快照，不随 3x-ui 后续变化同步；在线状态不迁移".to_string(),
    );
    item.signature = {
        let mut parts = vec![
            "import".to_string(),
            row.id.to_string(),
            inbound.tag.clone(),
            inbound.listen.clone(),
            inbound.port.to_string(),
            inbound.network.clone(),
            inbound.security.clone(),
            inbound.reality.public_key.clone(),
            inbound.reality.private_key.clone(),
            inbound.reality.short_ids.join(","),
            inbound.reality.server_names.join(","),
            inbound.reality.dest.clone(),
            inbound.tls.server_name.clone(),
        ];
        for c in &inbound.clients {
            parts.push(format!(
                "{}|{}|{}|{}|{}|{}",
                c.uuid,
                c.email,
                c.flow,
                c.quota_bytes,
                c.expires_at.clone().unwrap_or_default(),
                c.enabled
            ));
        }
        util::sha256_hex(parts.join("\n").as_bytes())
    };
    Candidate { item }
}

fn build_plan(store: &Store, db_path: &str) -> ApiResult<Plan> {
    if db_path.trim().is_empty() {
        return Err(ApiError::bad_request("缺少 db_path（指向 3x-ui 的 x-ui.db）"));
    }
    let src = PathBuf::from(db_path.trim());
    let canonical = fs::canonicalize(&src)
        .map_err(|e| ApiError::bad_request(format!("无法定位数据库 {}: {e}", src.display())))?;
    let snapshot = take_snapshot(store, &canonical)?;
    let conn = Connection::open_with_flags(&snapshot.path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .map_err(|e| ApiError::internal(format!("只读打开快照失败: {e}")))?;
    let schema = probe(&conn)?;
    let rows = load_rows(&conn)?;
    let traffic = load_traffic(&conn, &schema)?;
    drop(conn);

    let state = store.snapshot();
    let now = util::now_iso();
    let source_prefix = util::sha256_hex(canonical.to_string_lossy().as_bytes())[0..12].to_string();

    let existing_refs: HashSet<String> = state.inbounds.iter().map(|i| i.source_ref.clone()).collect();
    let existing_tags: HashSet<String> = state
        .inbounds
        .iter()
        .filter(|i| !i.tag.is_empty())
        .map(|i| i.tag.clone())
        .collect();
    let existing_ports: HashMap<u16, String> = state
        .inbounds
        .iter()
        .map(|i| (i.port, i.remark.clone()))
        .collect();

    let mut items: Vec<Item> = Vec::new();
    let mut accepted_ports: HashMap<u16, i64> = HashMap::new();
    let mut accepted_tags: HashSet<String> = HashSet::new();

    for row in &rows {
        let mut cand = build_candidate(row, &traffic, &source_prefix, &now).item;
        let source_ref = format!("3xui:{source_prefix}:{}", row.id);
        if cand.kind == "import" {
            if existing_refs.contains(&source_ref) {
                cand.kind = "skip";
                cand.already_imported = true;
                cand.mapped = None;
                cand.reasons.push(format!(
                    "已导入（source_ref={source_ref} 命中），幂等跳过；不会覆盖已有入站"
                ));
                cand.signature = format!("already|{}", row.id);
            } else if existing_tags.contains(&cand.tag) {
                cand.kind = "conflict";
                cand.reasons.push(format!("tag {} 已被 Berthio 现有入站占用", cand.tag));
                cand.suggestion = "改源库 tag（或导入后手工改 tag）后重新预览".to_string();
                cand.mapped = None;
                cand.signature = format!("conflict-tag|{}|{}", row.id, cand.tag);
            } else if let Some(who) = existing_ports
                .get(&(row.port as u16))
                .map(|remark| format!("现有入站「{remark}」"))
                .or_else(|| {
                    accepted_ports
                        .get(&(row.port as u16))
                        .map(|id| format!("本次预览中的源入站 #{id}"))
                })
            {
                cand.kind = "conflict";
                cand.reasons
                    .push(format!("端口 {} 已被{who}使用", row.port));
                cand.suggestion =
                    "改源库端口或先在 Berthio 停用/删除冲突入站，再重新预览".to_string();
                cand.mapped = None;
                cand.signature = format!("conflict-port|{}|{}", row.id, row.port);
            } else if accepted_tags.contains(&cand.tag) {
                cand.kind = "conflict";
                cand.reasons.push(format!("tag {} 在本次预览内重复", cand.tag));
                cand.suggestion = "去重后再迁移".to_string();
                cand.mapped = None;
                cand.signature = format!("conflict-tag-dup|{}", row.id);
            } else {
                accepted_ports.insert(row.port as u16, row.id);
                accepted_tags.insert(cand.tag.clone());
            }
        }
        if cand.kind != "import" && cand.mapped.is_none() {
            cand.signature = format!(
                "{}-{}|{}",
                cand.kind,
                row.id,
                cand.reasons.join(";")
            );
        }
        items.push(cand);
    }

    let mut sig_parts = vec![
        format!("prefix={source_prefix}"),
        format!("flavor={}", schema.flavor),
        format!("rows={}", rows.len()),
    ];
    for i in &items {
        sig_parts.push(i.signature.clone());
    }
    let plan_hash = util::sha256_hex(sig_parts.join("\n").as_bytes());
    let preview_id = format!("mig_{}", &plan_hash[0..12]);

    Ok(Plan {
        snapshot,
        schema,
        items,
        plan_hash,
        preview_id,
        source_prefix,
    })
}

fn mapping_notes() -> Vec<String> {
    vec![
        "schema 来源：3x-ui v2.9.4 的 database/model/model.go 与 xray/client_traffic.go；旧版对照 vaxilu/x-ui 的 database/model/model.go。"
            .to_string(),
        "可继承：入站 remark/tag/listen/port/enabled，VLESS 的 network/security，客户端 uuid/email/flow/limitIp/配额/到期/启用，Reality 全套参数。"
            .to_string(),
        "不可继承：TLS 证书文件本体（只记录源路径）、inbound 级流量与到期/重置策略、sniffing、fallbacks、externalProxy、sockopt、客户端 subId/tgId/comment/reset、在线状态。"
            .to_string(),
        "协议范围：本轮只导入 VLESS。vmess/trojan/shadowsocks/hysteria/tuic 等会明确跳过，因为 Berthio 导出器当前只渲染 VLESS；不把「能读出来」冒充「导入后能订阅」。"
            .to_string(),
        "Reality 的 privateKey 是服务端唯一必需项（xray -test 实测：只给 publicKey 会被拒绝）；publicKey 缺失则客户端订阅不可用，因此两者缺一都判为冲突。"
            .to_string(),
        "客户端流量来自 client_traffics（email 全局唯一），是抓取时点快照；旧版 x-ui 没有该表，已用流量按 0 处理。"
            .to_string(),
        "库内单位：totalGB 实际存字节；expiryTime 存毫秒时间戳。"
            .to_string(),
    ]
}

fn cleanup_old(store: &Store, keep: &Path) {
    let root = store.home_path("tmp/migrate");
    let Ok(entries) = fs::read_dir(&root) else { return };
    for e in entries.flatten() {
        let p = e.path();
        if p != *keep {
            let _ = fs::remove_dir_all(p);
        }
    }
}

pub fn preview(store: &Store, db_path: &str) -> ApiResult<Value> {
    let plan = build_plan(store, db_path)?;
    let out = json!({
        "preview_id": plan.preview_id,
        "plan_hash": plan.plan_hash,
        "source_prefix": plan.source_prefix,
        "source": plan.snapshot.json(),
        "schema": plan.schema.json(),
        "counts": plan.counts(),
        "items": plan.items.iter().map(|i| i.json()).collect::<Vec<_>>(),
        "mapping_notes": mapping_notes(),
        "readonly_guarantee": {
            "source_writes": "无：源库永不执行 UPDATE/INSERT/DELETE；带 -wal/-shm 时连只读连接都不打开，只拷贝文件。",
            "snapshot": "快照只写入 Berthio 自己的 tmp/migrate/；源库 sha256 抓取前后对比见 source 字段。",
            "apply_requires_confirmation": "apply 必须同时提交 preview_id + plan_hash + confirm=true；数据变化会 409。",
        },
        "next": "确认 diff 后调用 POST /api/migrate/apply，body 带 db_path / preview_id / plan_hash / confirm=true",
    });
    cleanup_old(store, &plan.snapshot.work);
    store.audit(
        "migrate.preview",
        "local-admin",
        json!({
            "db_path": plan.snapshot.source_path.display().to_string(),
            "source_sha256": plan.snapshot.source_sha256,
            "snapshot_strategy": plan.snapshot.strategy,
            "counts": plan.counts(),
            "plan_hash": plan.plan_hash,
        }),
    );
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
pub fn apply(
    store: &Store,
    db_path: &str,
    preview_id: &str,
    plan_hash: &str,
    confirm: bool,
) -> ApiResult<Value> {
    if !confirm {
        return Err(ApiError::bad_request(
            "apply 需要 confirm=true：请先查看预览 diff，再显式确认写入 Berthio 状态",
        ));
    }
    if preview_id.is_empty() || plan_hash.is_empty() {
        return Err(ApiError::bad_request(
            "apply 需要 preview_id 与 plan_hash（来自最近一次预览）",
        ));
    }
    let plan = build_plan(store, db_path)?;
    if plan.preview_id != preview_id || plan.plan_hash != plan_hash {
        cleanup_old(store, &plan.snapshot.work);
        store.audit(
            "migrate.apply.rejected",
            "local-admin",
            json!({"reason": "plan_changed", "expected": plan_hash, "actual": plan.plan_hash}),
        );
        return Err(ApiError::new(
            409,
            "plan_changed",
            "源库或 Berthio 状态在预览后发生变化，拒绝写入；请重新预览并确认新 diff",
        )
        .with_field("expected_plan_hash", plan_hash)
        .with_field("current_plan_hash", plan.plan_hash.as_str())
        .with_field("current_preview_id", plan.preview_id.as_str()));
    }

    let imports: Vec<Inbound> = plan
        .items
        .iter()
        .filter(|i| i.kind == "import")
        .filter_map(|i| i.mapped.clone())
        .collect();
    let source_refs: Vec<String> = imports.iter().map(|i| i.source_ref.clone()).collect();

    let mut inserted: Vec<String> = Vec::new();
    let mut skipped_existing = 0usize;
    if !imports.is_empty() {
        store.update(|st| {
            // 二次检查：预览到 apply 之间不允许出现 tag/端口冲突，避免半套写入。
            let mut tags: HashSet<String> = st.inbounds.iter().map(|i| i.tag.clone()).collect();
            let mut ports: HashMap<u16, String> =
                st.inbounds.iter().map(|i| (i.port, i.remark.clone())).collect();
            for inb in &imports {
                if st.inbounds.iter().any(|e| e.source_ref == inb.source_ref) {
                    continue;
                }
                if !inb.tag.is_empty() && tags.contains(&inb.tag) {
                    return Err(ApiError::conflict(format!(
                        "tag {} 在 apply 时已被占用；未写入任何入站，请重新预览",
                        inb.tag
                    )));
                }
                if let Some(other) = ports.get(&inb.port) {
                    return Err(ApiError::conflict(format!(
                        "端口 {} 在 apply 时已被「{other}」占用；未写入任何入站，请重新预览",
                        inb.port
                    )));
                }
                tags.insert(inb.tag.clone());
                ports.insert(inb.port, inb.remark.clone());
            }
            for (inb, reference) in imports.iter().zip(source_refs.iter()) {
                if st.inbounds.iter().any(|e| &e.source_ref == reference) {
                    skipped_existing += 1;
                    continue;
                }
                st.inbounds.push(inb.clone());
                inserted.push(inb.id.clone());
            }
            Ok(())
        })?;
    }

    let counts = plan.counts();
    store.audit(
        "migrate.apply",
        "local-admin",
        json!({
            "db_path": plan.snapshot.source_path.display().to_string(),
            "source_sha256": plan.snapshot.source_sha256,
            "snapshot_strategy": plan.snapshot.strategy,
            "plan_hash": plan.plan_hash,
            "counts": counts,
            "imported_ids": inserted,
        }),
    );
    cleanup_old(store, &plan.snapshot.work);

    Ok(json!({
        "ok": true,
        "preview_id": preview_id,
        "plan_hash": plan_hash,
        "counts": counts,
        "imported": inserted.len(),
        "already_imported_at_apply": skipped_existing,
        "inbound_ids": inserted,
        "source": plan.snapshot.json(),
        "note": "只写入 Berthio 自己的 state.json；源库未被修改。重复运行同一预览会幂等跳过。",
    }))
}
