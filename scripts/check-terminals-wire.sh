#!/usr/bin/env bash
# Owner check for M1-2: print what the real base answers to the terminal methods, so that the
# field names and reply shapes read in src/base/herdr.rs (id, labels, status, text, ...) can be
# compared with the wire. Starts a core in a place of its own and stops only that.
# Needs `hc` (HC_BIN, default: hc on PATH) and python3.
set -euo pipefail

HC_BIN="${HC_BIN:-hc}"
root="$(mktemp -d)"
trap '"$HC_BIN" server stop >/dev/null 2>&1 || true; rm -r "$root"' EXIT
export HC_SESSION="makiba-terms-$$"
export XDG_CONFIG_HOME="$root/config" XDG_STATE_HOME="$root/state" XDG_RUNTIME_DIR="$root/run"
mkdir -p "$XDG_CONFIG_HOME/herdr-core" "$XDG_STATE_HOME/herdr-core" "$XDG_RUNTIME_DIR/herdr-core"
printf 'notes = false\n' >"$XDG_CONFIG_HOME/herdr-core/config.toml"
unset HC_SOCKET HC_SCOPE HC_TERMINAL_ID HC_TERMINAL_SOCKET

"$HC_BIN" --version
"$HC_BIN" server start >/dev/null
sock="$XDG_RUNTIME_DIR/herdr-core/$HC_SESSION.sock"

python3 -I - "$sock" "$root" <<'PY'
import json, socket, sys, time

sock_path, root = sys.argv[1], sys.argv[2]


def call(method, params):
    s = socket.socket(socket.AF_UNIX)
    s.settimeout(10)
    s.connect(sock_path)
    req = {"jsonrpc": "2.0", "id": 1, "method": method, "params": params}
    s.sendall((json.dumps(req) + "\n").encode())
    reply = s.makefile().readline().strip()
    print(f"--- {method} {json.dumps(params)}\n{reply}")
    return json.loads(reply).get("result") or {}


run = call("terminal.run", {
    "argv": ["env", "-u", "GH_TOKEN", "sh", "-c", "echo makiba-wire-line; sleep 60"],
    "cwd": root, "name": "wire-check",
    "labels": {"makiba.org": "wire-check", "makiba.role": "worker"},
    "env": {"WIRE_CHECK": "1"},
    "keep": True, "no_parent": True, "no_notify": True,
})
t = run.get("id") or (run.get("terminal") or {}).get("id") or run.get("terminal") or "wire-check"
time.sleep(2)
call("terminal.list", {"labels": {"makiba.org": "wire-check"}})
call("terminal.list", {"labels": {"makiba.org": "someone-else"}})
call("terminal.get", {"terminal": t})
call("terminal.read", {"terminal": t, "lines": 20})
call("terminal.keys", {"terminal": t, "keys": ["enter"]})
call("terminal.send", {"terminal": t, "text": "echo sent", "if": "idle"})
call("terminal.kill", {"terminal": t})
call("terminal.rm", {"terminal": t})
call("terminal.get", {"terminal": t})
PY
