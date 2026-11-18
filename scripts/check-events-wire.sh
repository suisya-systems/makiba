#!/usr/bin/env bash
# Owner check for M1-3: print what the real base sends after `events.subscribe`, so that the field
# names in src/base/herdr/events.rs (seq, terminals, event/type/terminal, heartbeat, missed, labels,
# status, exit) can be compared with the wire. Starts a core in a place of its own, subscribes, runs
# and ends one terminal, and prints every line for HC_WATCH_SECONDS (default 20, long enough for one
# 15 s heartbeat). Needs `hc` (HC_BIN, default: hc on PATH) and python3. It cannot show `missed`
# (the base sends it only to a subscriber that fell behind).
set -euo pipefail

HC_BIN="${HC_BIN:-hc}"
root="$(mktemp -d)"
trap '"$HC_BIN" server stop >/dev/null 2>&1 || true; rm -r "$root"' EXIT
export HC_SESSION="makiba-events-$$"
export XDG_CONFIG_HOME="$root/config" XDG_STATE_HOME="$root/state" XDG_RUNTIME_DIR="$root/run"
mkdir -p "$XDG_CONFIG_HOME/herdr-core" "$XDG_STATE_HOME/herdr-core" "$XDG_RUNTIME_DIR/herdr-core"
printf 'notes = false\n' >"$XDG_CONFIG_HOME/herdr-core/config.toml"
unset HC_SOCKET HC_SCOPE HC_TERMINAL_ID HC_TERMINAL_SOCKET

"$HC_BIN" --version
"$HC_BIN" server start >/dev/null
sock="$XDG_RUNTIME_DIR/herdr-core/$HC_SESSION.sock"

(
  sleep 2
  "$HC_BIN" run --bg --no-parent --name wire-check -- sh -c 'echo hi; sleep 3' >/dev/null
  sleep 6
  "$HC_BIN" kill wire-check >/dev/null 2>&1 || true
  "$HC_BIN" rm wire-check >/dev/null 2>&1 || true
) &

python3 -I - "$sock" "${HC_WATCH_SECONDS:-20}" <<'PY'
import json, socket, sys, time
s = socket.socket(socket.AF_UNIX)
s.settimeout(1)
s.connect(sys.argv[1])
req = {"jsonrpc": "2.0", "id": 1, "method": "events.subscribe", "params": {}}
s.sendall((json.dumps(req) + "\n").encode())
end = time.time() + float(sys.argv[2])
buf = b""
while time.time() < end:
    try:
        chunk = s.recv(65536)
    except socket.timeout:
        continue
    if not chunk:
        print("--- the core closed the connection")
        break
    buf += chunk
    while b"\n" in buf:
        line, buf = buf.split(b"\n", 1)
        print(line.decode())
PY
wait
