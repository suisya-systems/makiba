#!/usr/bin/env bash
# Owner check for M1-1: print what the real base answers to `hello`, so that the field names in
# src/base/herdr.rs (generation, version) can be compared with the wire. Starts a core in a place
# of its own and stops only that. Needs `hc` (HC_BIN, default: hc on PATH) and python3.
set -euo pipefail

HC_BIN="${HC_BIN:-hc}"
root="$(mktemp -d)"
trap '"$HC_BIN" server stop >/dev/null 2>&1 || true; rm -r "$root"' EXIT
export HC_SESSION="makiba-hello-$$"
export XDG_CONFIG_HOME="$root/config" XDG_STATE_HOME="$root/state" XDG_RUNTIME_DIR="$root/run"
mkdir -p "$XDG_CONFIG_HOME/herdr-core" "$XDG_STATE_HOME/herdr-core" "$XDG_RUNTIME_DIR/herdr-core"
printf 'notes = false\n' >"$XDG_CONFIG_HOME/herdr-core/config.toml"
unset HC_SOCKET HC_SCOPE HC_TERMINAL_ID HC_TERMINAL_SOCKET

"$HC_BIN" --version
"$HC_BIN" server start >/dev/null
sock="$XDG_RUNTIME_DIR/herdr-core/$HC_SESSION.sock"

for generation in 1 2; do
  echo "--- hello with generation $generation"
  python3 -I - "$sock" "$generation" <<'PY'
import json, socket, sys
s = socket.socket(socket.AF_UNIX)
s.settimeout(5)
s.connect(sys.argv[1])
req = {"jsonrpc": "2.0", "id": 1, "method": "hello", "params": {"generation": int(sys.argv[2])}}
s.sendall((json.dumps(req) + "\n").encode())
print(s.makefile().readline().strip())
PY
done
