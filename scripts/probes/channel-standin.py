#!/usr/bin/env python3
"""Minimal stand-in for makibad, for the channel probes (python3 standard library only).

  channel-standin.py serve   --port N --state DIR --secrets FILE   (FILE: {"token":..,"cred":..,"admin":..})
      HTTP on 127.0.0.1 with:
        POST /mcp    (Authorization: Bearer T)  MCP over plain JSON POST: initialize, tools/list,
                     tools/call send_request{text} -> appended to DIR/requests.jsonl
        POST /poll   (Bearer C)  the channel server takes queued messages (at most once)
        POST /admin  (Bearer A)  {"op":"enqueue","content":"..."} or {"op":"pause","value":true|false}
        GET  /ping   no auth, answers "pong"
      Every request is logged to DIR/access.log: time, method, path, auth ok|bad|none, user agent.
  channel-standin.py channel
      The stdio MCP server Claude Code loads with --dangerously-load-development-channels
      server:<name>. Tool-less; declares experimental{claude/channel}; polls /poll about every
      0.5 s and emits notifications/claude/channel. Env: STANDIN_URL, STANDIN_CRED, STANDIN_NAME.

Message shapes follow claude-org-runtime's broker (server.py do_POST, channel_sidecar.py), read
2026-11-18; nothing is imported from it. Only ASCII is printed.
"""
import argparse
import json
import os
import secrets
import sys
import threading
import time
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROTOS = ("2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05")
TOOL = {
    "name": "send_request",
    "description": "Send a request to the probe's stand-in daemon. Argument text is the request.",
    "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]},
}


def serve(a):
    state = a.state
    lock = threading.Lock()
    queue, sessions, paused = [], set(), [False]
    counter = [0]

    def append(name, line):
        with lock:
            with open(os.path.join(state, name), "a", encoding="utf-8") as f:
                f.write(line + "\n")

    class H(BaseHTTPRequestHandler):
        def log_message(self, *x):
            pass

        def _send(self, code, obj=None, headers=None, raw=None):
            body = raw if raw is not None else (b"" if obj is None else json.dumps(obj).encode())
            self.send_response(code)
            if body:
                self.send_header("Content-Type", "application/json" if raw is None else "text/plain")
            for k, v in (headers or {}).items():
                self.send_header(k, v)
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            if body:
                self.wfile.write(body)

        def _auth(self, want):
            h = self.headers.get("Authorization", "")
            got = h[7:].strip() if h.startswith("Bearer ") else None
            ok = "none" if got is None else ("ok" if got == want else "bad")
            append("access.log", "%s %s %s auth=%s ua=%s" % (
                time.strftime("%H:%M:%S"), self.command, self.path, ok, self.headers.get("User-Agent", "-")[:40]))
            return ok == "ok"

        def do_GET(self):
            if self.path == "/ping":
                append("access.log", "%s GET /ping auth=none ua=%s" % (time.strftime("%H:%M:%S"), self.headers.get("User-Agent", "-")[:40]))
                self._send(200, raw=b"pong")
            else:
                self._send(405)

        def do_POST(self):
            n = int(self.headers.get("Content-Length") or 0)
            try:
                req = json.loads(self.rfile.read(n) or b"{}")
            except ValueError:
                self._send(400)
                return
            path = self.path.rstrip("/")
            if path == "/admin":
                if not self._auth(a.admin):
                    return self._send(401)
                if req.get("op") == "enqueue":
                    with lock:
                        counter[0] += 1
                        queue.append({"id": counter[0], "content": req.get("content", "")})
                elif req.get("op") == "pause":
                    paused[0] = bool(req.get("value"))
                return self._send(200, {"ok": True, "queued": len(queue), "paused": paused[0]})
            if path == "/poll":
                if not self._auth(a.cred):
                    return self._send(401)
                with lock:
                    rows = [] if paused[0] else queue[:]
                    if not paused[0]:
                        del queue[:]
                return self._send(200, {"rows": rows})
            if path != "/mcp":
                return self._send(404)
            if not self._auth(a.token):
                return self._send(401, {"jsonrpc": "2.0", "id": None, "error": {"code": -32001, "message": "unauthorized"}})
            method, rid = req.get("method", ""), req.get("id")
            if method != "initialize" and self.headers.get("Mcp-Session-Id") not in sessions:
                return self._send(404, {"jsonrpc": "2.0", "id": rid, "error": {"code": -32001, "message": "initialize first"}})
            if rid is None:
                return self._send(202)
            if method == "initialize":
                want = (req.get("params") or {}).get("protocolVersion", "")
                sid = secrets.token_hex(16)
                sessions.add(sid)
                return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {
                    "protocolVersion": want if want in PROTOS else PROTOS[0],
                    "capabilities": {"tools": {}}, "serverInfo": {"name": "makiba-standin", "version": "0.1.0"}}},
                    headers={"Mcp-Session-Id": sid})
            if method == "tools/list":
                return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {"tools": [TOOL]}})
            if method == "tools/call":
                args = (req.get("params") or {}).get("arguments") or {}
                append("requests.jsonl", json.dumps({"ts": time.time(), "text": args.get("text", "")}))
                return self._send(200, {"jsonrpc": "2.0", "id": rid, "result": {
                    "content": [{"type": "text", "text": json.dumps({"ok": True, "result": {"accepted": True}})}]}})
            return self._send(200, {"jsonrpc": "2.0", "id": rid, "error": {"code": -32601, "message": "method not found"}})

    os.makedirs(state, exist_ok=True)
    srv = ThreadingHTTPServer(("127.0.0.1", a.port), H)
    open(os.path.join(state, "ready"), "w").write(str(srv.server_address[1]))
    srv.serve_forever()


def channel():
    url, cred = os.environ["STANDIN_URL"], os.environ["STANDIN_CRED"]
    name = os.environ.get("STANDIN_NAME", "makiba-channel")
    started, out_lock = threading.Event(), threading.Lock()

    def write(obj):
        with out_lock:
            sys.stdout.buffer.write((json.dumps(obj, ensure_ascii=False) + "\n").encode("utf-8"))
            sys.stdout.buffer.flush()

    def poll_loop():
        started.wait()
        while True:
            try:
                req = urllib.request.Request(url + "/poll", data=b"{}", method="POST",
                                             headers={"Authorization": "Bearer " + cred, "Content-Type": "application/json"})
                rows = json.loads(urllib.request.urlopen(req, timeout=5).read()).get("rows", [])
                for r in rows:
                    write({"jsonrpc": "2.0", "method": "notifications/claude/channel", "params": {
                        "content": r["content"],
                        # sent_at must be a string, or Claude Code drops the notification (runtime #80)
                        "meta": {"from_id": "probe", "from_name": "probe", "sent_at": str(time.time()), "msg_id": str(r["id"])}}})
            except Exception as e:
                print("poll error: %s" % e, file=sys.stderr, flush=True)
            time.sleep(0.5)

    threading.Thread(target=poll_loop, daemon=True).start()
    for raw in sys.stdin.buffer:
        try:
            msg = json.loads(raw.decode("utf-8"))
        except ValueError:
            continue
        m, mid = msg.get("method"), msg.get("id")
        if m == "initialize":
            want = (msg.get("params") or {}).get("protocolVersion", "")
            write({"jsonrpc": "2.0", "id": mid, "result": {
                "protocolVersion": want if want in PROTOS else PROTOS[1],
                "capabilities": {"experimental": {"claude/channel": {}}},
                "serverInfo": {"name": name, "version": "0.1.0"},
                "instructions": "Messages arrive as <channel source=\"%s\"> tags. There is no tool to call." % name}})
        elif m == "notifications/initialized":
            started.set()
        elif m == "ping":
            write({"jsonrpc": "2.0", "id": mid, "result": {}})
        elif m in ("tools/list", "resources/list", "prompts/list"):
            write({"jsonrpc": "2.0", "id": mid, "result": {m.split("/")[0]: []}})
        elif mid is not None:
            write({"jsonrpc": "2.0", "id": mid, "error": {"code": -32601, "message": "method not found"}})


if __name__ == "__main__":
    p = argparse.ArgumentParser(description="stand-in for makibad (channel probes)")
    p.add_argument("mode", choices=["serve", "channel"])
    p.add_argument("--port", type=int, default=0)
    p.add_argument("--state", default=".")
    p.add_argument("--secrets", default="", help="JSON file {token,cred,admin}: secrets stay out of argv and environ")
    a = p.parse_args()
    if a.mode == "serve":
        for k, v in json.load(open(a.secrets)).items():
            setattr(a, k, v)
    serve(a) if a.mode == "serve" else channel()
