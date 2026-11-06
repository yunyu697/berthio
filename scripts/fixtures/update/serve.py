"""离线更新源（只用于本机测试）。不联网、不签名；签名文件由测试夹具提供。

路由：
  /latest.json                              读 <root>/latest.json（测试中途会改写它）
  /asset/ok/<文件名>                        读 <root>/bin-ok.exe
  /asset/tamper/<文件名>                    同上但翻转一个字节（用于「错包拒收」）
  /asset/truncate/<文件名>                  声明完整 Content-Length，只发一半后断开（用于「下载中断」）
  /asset/exe/<文件名>                       读 <root>/bin.exe（真实替换重启测试用）
  /mirror/...                               镜像前缀；<root>/mirror.ok 存在才代理，否则 500
"""
import argparse
import http.server
import os
import sys


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, fmt, *args):
        sys.stderr.write("%s %s\n" % (self.path, fmt % args))

    def _send(self, code, body=b"", ctype="application/octet-stream"):
        self.send_response(code)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def do_GET(self):
        root = self.server.root
        raw = self.path.split("?")[0]
        if raw.startswith("/mirror") and not os.path.exists(os.path.join(root, "mirror.ok")):
            self._send(500, b"mirror down", "text/plain; charset=utf-8")
            return
        parts = [p for p in raw.strip("/").split("/") if p]
        if not parts:
            self._send(404, b"not found", "text/plain")
            return
        if parts[-1] == "latest.json":
            path = os.path.join(root, "latest.json")
            if not os.path.exists(path):
                self._send(404, b"no manifest", "text/plain")
                return
            with open(path, "rb") as f:
                self._send(200, f.read(), "application/json; charset=utf-8")
            return
        mode = parts[-2] if len(parts) >= 2 else ""
        if mode not in ("ok", "tamper", "truncate", "exe"):
            self._send(404, b"not found", "text/plain")
            return
        path = os.path.join(root, "bin.exe" if mode == "exe" else "bin-ok.exe")
        if not os.path.exists(path):
            self._send(404, b"no asset", "text/plain")
            return
        with open(path, "rb") as f:
            data = f.read()
        if mode == "tamper":
            data = bytearray(data)
            data[5] ^= 0xFF
            data = bytes(data)
        if mode == "truncate":
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data[: len(data) // 2])
            self.wfile.flush()
            self.close_connection = True
            return
        self._send(200, data)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--port", type=int, required=True)
    ap.add_argument("--root", required=True)
    args = ap.parse_args()
    httpd = http.server.ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    httpd.root = args.root
    print("listening %d root=%s" % (args.port, args.root), flush=True)
    httpd.serve_forever()


if __name__ == "__main__":
    main()
