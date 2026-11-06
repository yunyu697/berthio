#!/usr/bin/env python3
"""生成用于 M4 迁移测试的 3x-ui / 旧版 x-ui SQLite 样本库（不接触任何真实面板）。

用法：
  python scripts/make-3xui-fixture.py <输出目录>

产物：
  v2.db            3x-ui v2.9.4 结构，覆盖 import / skip / conflict 三类
  v2-collision.db  与 v2.db 相同结构，但只有一条 tag 与 Berthio 现有入站冲突的记录
  old.db           旧版 x-ui 结构（无 client_traffics / all_time / traffic_reset）
  v2-wal.db        带残留 -wal / -shm 的 3x-ui 库，用于验证“绝不打开源库”的文件拷贝路径
"""

import json
import os
import sqlite3
import subprocess
import sys

PRIV = "wCmzHKJR4pef2-B7uIv60Myqv1q-1RF74KZBwDYZaFU"
PUB = "qiOGSbhAOlBTvto_J5F05B8ELVjqqrmoq1lpyFK0gBs"
UUID_A = "11111111-1111-4111-8111-111111111111"
UUID_B = "22222222-2222-4222-8222-222222222222"
UUID_C = "33333333-3333-4333-8333-333333333333"
UUID_D = "44444444-4444-4444-8444-444444444444"
UUID_E = "55555555-5555-4555-8555-555555555555"

INBOUND_COLS = """(
 id integer primary key autoincrement,
 user_id integer,
 up integer, down integer, total integer, all_time integer,
 remark text, enable numeric, expiry_time integer,
 traffic_reset text default 'never', last_traffic_reset_time integer default 0,
 listen text, port integer, protocol text,
 settings text, stream_settings text, tag text, sniffing text
)"""

OLD_COLS = """(
 id integer primary key autoincrement,
 user_id integer,
 up integer, down integer, total integer,
 remark text, enable numeric, expiry_time integer,
 listen text, port integer, protocol text,
 settings text, stream_settings text, tag text, sniffing text
)"""

CLIENT_TRAFFIC_COLS = """(
 id integer primary key autoincrement,
 inbound_id integer, enable numeric, email text,
 up integer, down integer, all_time integer,
 expiry_time integer, total integer, reset integer, last_online integer
)"""


def reality_stream(host="www.microsoft.com", private_key=PRIV, public_key=PUB, network="tcp"):
    return {
        "network": network,
        "security": "reality",
        "realitySettings": {
            "show": False,
            "dest": f"{host}:443",
            "xver": 0,
            "serverNames": [host],
            "privateKey": private_key,
            "shortIds": ["0123456789abcdef", ""],
            "settings": {"publicKey": public_key, "fingerprint": "chrome", "spiderX": "/"},
        },
    }


def tls_ws_stream(server_name="sg.example.com"):
    return {
        "network": "ws",
        "security": "tls",
        "wsSettings": {"path": "/", "headers": {}},
        "tlsSettings": {
            "serverName": server_name,
            "alpn": ["h2", "http/1.1"],
            "certificates": [
                {
                    "certificateFile": "/etc/ssl/sg/fullchain.pem",
                    "keyFile": "/etc/ssl/sg/key.pem",
                }
            ],
        },
    }


def vless_settings(clients):
    return {"clients": clients, "decryption": "none", "fallbacks": []}


def client(uuid, email, flow="", total_gb=0, expiry_ms=0, enable=True, limit_ip=0):
    return {
        "id": uuid,
        "flow": flow,
        "email": email,
        "limitIp": limit_ip,
        "totalGB": total_gb,
        "expiryTime": expiry_ms,
        "enable": enable,
        "tgId": 0,
        "subId": "sub-" + email,
        "comment": "",
        "reset": 0,
    }


def insert_inbound(conn, id_, remark, port, tag, protocol, settings, stream, enable=1):
    conn.execute(
        "INSERT INTO inbounds (id, user_id, up, down, total, all_time, remark, enable, expiry_time,"
        " traffic_reset, last_traffic_reset_time, listen, port, protocol, settings, stream_settings, tag, sniffing)"
        " VALUES (?,1,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)",
        (
            id_, 123456, 789012, 0, 0, remark, enable, 0, "never", 0,
            "0.0.0.0", port, protocol,
            json.dumps(settings, ensure_ascii=False),
            json.dumps(stream, ensure_ascii=False),
            tag,
            json.dumps({"enabled": False, "destOverride": []}),
        ),
    )


def build_v2(path):
    if os.path.exists(path):
        os.remove(path)
    conn = sqlite3.connect(path)
    conn.executescript(
        f"""
        CREATE TABLE users (id integer primary key autoincrement, username text unique, password text);
        CREATE TABLE inbounds {INBOUND_COLS};
        CREATE TABLE client_traffics {CLIENT_TRAFFIC_COLS};
        CREATE TABLE settings (id integer primary key autoincrement, key text unique, value text);
        CREATE UNIQUE INDEX idx_inbounds_tag ON inbounds(tag);
        """
    )
    conn.execute("INSERT INTO users (username,password) VALUES ('admin','hash')")
    conn.execute("INSERT INTO settings (key,value) VALUES ('webPort','2053')")

    # 1) 可导入：VLESS Reality
    insert_inbound(
        conn, 1, "东京-REALITY", 443, "jp-reality", "vless",
        vless_settings([
            client(UUID_A, "alice@demo", flow="xtls-rprx-vision", total_gb=10 * 1024**3, expiry_ms=1893456000000, limit_ip=3),
            client(UUID_B, "bob@demo", flow="xtls-rprx-vision", total_gb=0, expiry_ms=0, limit_ip=1),
        ]),
        reality_stream(),
    )
    # 2) 可导入：VLESS TLS + WS
    insert_inbound(
        conn, 2, "新加坡-TLS-WS", 8443, "sg-tls", "vless",
        vless_settings([client(UUID_C, "carol@demo", total_gb=5 * 1024**3, expiry_ms=1893456000000)]),
        tls_ws_stream(),
    )
    # 3) 跳过：trojan（Berthio 导出器未实现）
    insert_inbound(
        conn, 3, "旧-TROJAN", 2053, "old-trojan", "trojan",
        {"clients": [{"password": "trojan-pw", "email": "troy@demo", "enable": True}], "fallbacks": []},
        {"network": "tcp", "security": "tls", "tlsSettings": {"serverName": "trojan.example.com", "certificates": []}},
    )
    # 4) 冲突：Reality 缺 privateKey
    bad_stream = reality_stream()
    bad_stream["realitySettings"]["privateKey"] = ""
    insert_inbound(
        conn, 4, "坏-REALITY-只认publicKey", 7443, "bad-reality", "vless",
        vless_settings([client(UUID_D, "dave@demo")]),
        bad_stream,
    )
    # 5) 冲突：与 #1 端口重复（3x-ui v2 的 port 无 unique 约束，真实可出现）
    insert_inbound(
        conn, 5, "端口重复-REALITY", 443, "dup-port", "vless",
        vless_settings([client(UUID_E, "eve@demo", flow="xtls-rprx-vision")]),
        reality_stream("www.bing.com"),
    )
    # 6) 冲突：fallbacks 非空
    fb_settings = vless_settings([client(UUID_A, "fallback@demo")])
    fb_settings["fallbacks"] = [{"name": "web", "dest": 80, "xver": 0}]
    insert_inbound(
        conn, 6, "带-fallback", 8444, "with-fallback", "vless",
        fb_settings, reality_stream(),
    )
    # 7) 冲突：WS path 不是 /
    ws_stream = tls_ws_stream()
    ws_stream["wsSettings"]["path"] = "/custom"
    insert_inbound(
        conn, 7, "WS-自定义路径", 8445, "ws-custom", "vless",
        vless_settings([client(UUID_C, "ws-custom@demo")]),
        ws_stream,
    )
    # 8) 跳过：security=none
    insert_inbound(
        conn, 8, "明文-VLESS", 8080, "plain-vless", "vless",
        vless_settings([client(UUID_E, "plain@demo")]),
        {"network": "tcp", "security": "none"},
    )
    # 9) 跳过：未知协议
    insert_inbound(
        conn, 9, "未知协议", 9999, "unknown-proto", "foobar",
        {"clients": []}, {"network": "tcp", "security": "none"},
    )
    # 10) 跳过：无客户端
    insert_inbound(
        conn, 10, "无客户端", 7446, "no-clients", "vless",
        vless_settings([]), reality_stream(),
    )
    # 11) 跳过：grpc 传输
    insert_inbound(
        conn, 11, "VLESS-gRPC", 7447, "vless-grpc", "vless",
        vless_settings([client(UUID_B, "grpc@demo")]),
        reality_stream(network="grpc"),
    )
    # 12) 可导入但源端停用
    insert_inbound(
        conn, 12, "已停用-REALITY", 9443, "paused-reality", "vless",
        vless_settings([client(UUID_D, "paused@demo")]),
        reality_stream("www.cloudflare.com"),
        enable=0,
    )

    traffic = [
        (1, "alice@demo", 1024 * 1024 * 1024, 2048 * 1024 * 1024),
        (1, "bob@demo", 10 * 1024 * 1024, 20 * 1024 * 1024),
        (2, "carol@demo", 5 * 1024 * 1024, 9 * 1024 * 1024),
        (12, "paused@demo", 0, 0),
    ]
    for inbound_id, email, up, down in traffic:
        conn.execute(
            "INSERT INTO client_traffics (inbound_id,enable,email,up,down,all_time,expiry_time,total,reset,last_online)"
            " VALUES (?,?,?,?,?,?,?,?,?,?)",
            (inbound_id, 1, email, up, down, up + down, 0, 0, 0, 0),
        )
    conn.commit()
    conn.close()


def build_collision(path):
    if os.path.exists(path):
        os.remove(path)
    conn = sqlite3.connect(path)
    conn.executescript(
        f"""
        CREATE TABLE inbounds {INBOUND_COLS};
        CREATE TABLE client_traffics {CLIENT_TRAFFIC_COLS};
        CREATE UNIQUE INDEX idx_inbounds_tag ON inbounds(tag);
        """
    )
    insert_inbound(
        conn, 20, "与-Berthio-已有-tag-冲突", 4431, "jp-reality", "vless",
        vless_settings([client(UUID_E, "collision@demo")]),
        reality_stream("www.bing.com"),
    )
    conn.commit()
    conn.close()


def build_old(path):
    if os.path.exists(path):
        os.remove(path)
    conn = sqlite3.connect(path)
    conn.executescript(
        f"""
        CREATE TABLE users (id integer primary key autoincrement, username text unique, password text);
        CREATE TABLE inbounds {OLD_COLS};
        CREATE TABLE settings (id integer primary key autoincrement, key text unique, value text);
        CREATE UNIQUE INDEX idx_inbounds_port ON inbounds(port);
        CREATE UNIQUE INDEX idx_inbounds_tag ON inbounds(tag);
        """
    )
    conn.execute("INSERT INTO users (username,password) VALUES ('admin','old-hash')")
    conn.execute(
        "INSERT INTO inbounds (id,user_id,up,down,total,remark,enable,expiry_time,listen,port,protocol,"
        "settings,stream_settings,tag,sniffing) VALUES (?,1,0,0,0,?,1,0,'0.0.0.0',?,?,?,?,?,?)",
        (
            1, "旧版-REALITY", 443, "vless",
            json.dumps(vless_settings([client(UUID_A, "old-alice@demo", flow="xtls-rprx-vision")]), ensure_ascii=False),
            json.dumps(reality_stream(), ensure_ascii=False),
            "old-jp", json.dumps({"enabled": False}),
        ),
    )
    conn.execute(
        "INSERT INTO inbounds (id,user_id,up,down,total,remark,enable,expiry_time,listen,port,protocol,"
        "settings,stream_settings,tag,sniffing) VALUES (?,1,0,0,0,?,1,0,'0.0.0.0',?,?,?,?,?,?)",
        (
            2, "旧版-TROJAN", 2053, "trojan",
            json.dumps({"clients": [{"password": "old-pw", "email": "old-troy@demo"}]}, ensure_ascii=False),
            json.dumps({"network": "tcp", "security": "tls", "tlsSettings": {"serverName": "t.example.com"}}, ensure_ascii=False),
            "old-trojan", json.dumps({"enabled": False}),
        ),
    )
    conn.commit()
    conn.close()


def build_wal(path):
    """子进程写入后 os._exit，故意留下 -wal/-shm。"""
    if os.path.exists(path):
        os.remove(path)
    child = """
import json, os, sqlite3, sys
p = sys.argv[1]
c = sqlite3.connect(p)
c.execute("PRAGMA journal_mode=WAL")
c.execute("PRAGMA wal_autocheckpoint=0")
c.executescript('''CREATE TABLE inbounds (id integer primary key autoincrement, user_id integer,
 up integer, down integer, total integer, all_time integer, remark text, enable numeric, expiry_time integer,
 traffic_reset text, last_traffic_reset_time integer, listen text, port integer, protocol text, settings text,
 stream_settings text, tag text, sniffing text);
 CREATE TABLE client_traffics (id integer primary key autoincrement, inbound_id integer, enable numeric,
 email text, up integer, down integer, all_time integer, expiry_time integer, total integer, reset integer,
 last_online integer);''')
settings = json.dumps({"clients":[{"id":"%s","email":"wal@demo","flow":"xtls-rprx-vision","totalGB":0,"expiryTime":0,"enable":True}]})
stream = json.dumps({"network":"tcp","security":"reality","realitySettings":{"dest":"www.microsoft.com:443","xver":0,
 "serverNames":["www.microsoft.com"],"privateKey":"%s","shortIds":["0123456789abcdef"],
 "settings":{"publicKey":"%s","fingerprint":"chrome","spiderX":"/"}}})
c.execute("INSERT INTO inbounds (id,user_id,up,down,total,all_time,remark,enable,expiry_time,traffic_reset,last_traffic_reset_time,listen,port,protocol,settings,stream_settings,tag,sniffing) VALUES (1,1,0,0,0,0,'WAL-REALITY',1,0,'never',0,'0.0.0.0',443,'vless',?,?,'wal-reality','{}')", (settings, stream))
c.commit()
os._exit(0)
""" % (UUID_A, PRIV, PUB)
    subprocess.run([sys.executable, "-c", child, path], check=True)


def main():
    if len(sys.argv) >= 3 and sys.argv[1] == "--wal-child":
        return
    out = sys.argv[1] if len(sys.argv) > 1 else "."
    os.makedirs(out, exist_ok=True)
    build_v2(os.path.join(out, "v2.db"))
    build_collision(os.path.join(out, "v2-collision.db"))
    build_old(os.path.join(out, "old.db"))
    build_wal(os.path.join(out, "v2-wal.db"))
    print("fixtures written to", os.path.abspath(out))
    for name in ("v2.db", "v2-collision.db", "old.db", "v2-wal.db"):
        p = os.path.join(out, name)
        print(name, os.path.getsize(p), "bytes")
        for side in ("-wal", "-shm"):
            sp = p + side
            if os.path.exists(sp):
                print("  ", side, os.path.getsize(sp), "bytes")


if __name__ == "__main__":
    main()
