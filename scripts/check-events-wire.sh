#!/usr/bin/env bash
# Owner check for M1-3: print what the real base sends after `events.subscribe`, so that the field
# names in src/base/herdr/events.rs (seq, terminals, event/type/terminal, heartbeat, missed, labels,
# status, exit) can be compared with the wire. Starts a core in a place of its own, subscribes, runs
# and ends one terminal, and prints every line for HC_WATCH_SECONDS (default 30, long enough for one
# 15 s heartbeat and the test agent's run). Needs `hc` (HC_BIN, default: hc on PATH) and python3; run `cargo build --bin makiba-fake-agent`
# first (FAKE_AGENT names another build). Also runs the test agent, to show status and turn notices. It cannot show `missed`
# (the base sends it only to a subscriber that fell behind).
set -euo pipefail

HC_BIN="${HC_BIN:-hc}"
# The test agent (tests/common/fake_agent.rs); FAKE_AGENT overrides the path.
agent="${FAKE_AGENT:-$(cd "$(dirname "$0")/.." && pwd)/target/debug/makiba-fake-agent}"
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
  # The test agent reports working, blocked (permission), idle and done as OSC 7501 records, so that
  # the status and turn notices of a program that reports a status are in the output.
  if [ -x "$agent" ]; then
    "$HC_BIN" run --bg --no-parent --name wire-agent -- "$agent" --record "$root/agent.log" \
      idle working sleep:1500 blocked:permission sleep:1500 idle sleep:1500 working sleep:1500 \
      done sleep:2000 exit >/dev/null
    sleep 10
    "$HC_BIN" kill wire-agent >/dev/null 2>&1 || true
    "$HC_BIN" rm wire-agent >/dev/null 2>&1 || true
  else
    echo "--- no test agent at $agent: build it with: cargo build --bin makiba-fake-agent" >&2
  fi
) &

python3 -I - "$sock" "${HC_WATCH_SECONDS:-30}" <<'PY'
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

# Issue 560: three kept terminals as makiba launches them (`keep`, labels), read with
# `terminal.list`: one never lost, one whose program is killed (SIGKILL), and one across a core stop
# and start. A kept terminal whose program is lost is replaced with the machine's shell and stays
# `live`; src/base/herdr.rs reads it by its `argv` against the `makiba.argv` label. Compare `state`,
# `reason`, `exit_code`, `argv`, `foreground` and `labels` of each with that reading.
rpc() {
  python3 -I - "$sock" "$1" "$2" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(5)
s.connect(sys.argv[1])
req = {"jsonrpc": "2.0", "id": 1, "method": sys.argv[2], "params": json.loads(sys.argv[3])}
s.sendall((json.dumps(req) + "\n").encode())
buf = b""
while b"\n" not in buf:
    chunk = s.recv(65536)
    if not chunk:
        break
    buf += chunk
print(json.dumps(json.loads(buf.split(b"\n", 1)[0]), indent=1, sort_keys=True))
PY
}
kept() {
  echo "=== run $1"
  rpc terminal.run "{\"argv\": [\"sleep\", \"600\"], \"cwd\": \"$root\", \"name\": \"$1\",
    \"labels\": {\"makiba.check\": \"$1\"}, \"keep\": true, \"no_parent\": true, \"no_notify\": true}"
}
kept never-lost
killed="$(kept killed-kept | tee /dev/stderr | python3 -I -c '
import json, sys
text = sys.stdin.read().split("\n", 1)[1]
r = json.loads(text)["result"]
print(r.get("terminal", r)["pid"])')"
sleep 1
kill -KILL "$killed" || true
sleep 2
echo "=== list after SIGKILL"
rpc terminal.list '{"all": true}'
kept restart-kept
sleep 1
"$HC_BIN" server stop >/dev/null
"$HC_BIN" server start >/dev/null
sleep 2
echo "=== list after stop and start"
rpc terminal.list '{"all": true}'
