#!/usr/bin/env bash
# P9: a sandboxed worker and the stand-in daemon's HTTP port (design option B).
#  mcp-tool-reaches      the agent calls the MCP tool send_request and the stand-in receives it
#  bash-http-direct      Bash in the sandbox connects to the same port with urllib and ProxyHandler({}) (PASS = blocked)
#  bash-http-via-default-proxy  the same with urllib's default handlers (HTTP_PROXY etc. honoured); the exception type,
#                        errno and message are recorded on one line
#  own-token-env / own-token-argv / own-token-file   can Bash read this session's decoy token (env var, --mcp-config
#                        argv, a file under a denyRead path)? PASS = no
#  other-session-token   a second agent (own decoy env var and own --mcp-config argv) runs beside the sandboxed one;
#                        can the sandboxed Bash find its decoys in any readable environ or cmdline? PASS = no
#  credential-envvar-unset  a decoy variable named in sandbox.credentials.envVars (mode deny): gone from Bash's environ?
#                        (locations only, never values)
#  channel-server-running / channel-token-env / channel-token-argv   the channel server Claude Code starts from the
#                        `makiba-channel` entry (decoy in the entry's env, decoy in its args): control from outside the
#                        sandbox (the process runs and its /proc environ and cmdline hold the decoys; else NEEDS-HUMAN),
#                        then can Bash find the decoys in any readable environ or cmdline? PASS = no. Also recorded:
#                        whether Bash can see the channel server's pid at all (OBSERVE line).
# The worker settings: sandbox on, the scratch core's folders denied, no network or socket allowance for the port.
# Run from a login shell: bash scripts/probes/p9-mcp-from-sandbox.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P9: MCP tool vs direct HTTP from a sandboxed worker, token exposure"; exit 0 ;; esac
probe_init p9 "${1:-}"
standin_start || { say "RESULT p9: precondition-failed (stand-in did not start)"; exit 2; }
extra_cleanup() { kill "$STANDIN_PID" 2>/dev/null; }
probe_dry_stop   # PROBE_DRY_RUN=1 ends here, through the exit handler

WT=$WORK/wt; new_repo "$WT" || { say "RESULT p9: precondition-failed (repository)"; exit 2; }
ENVTOK=DECOYENV$(python3 -c 'import secrets;print(secrets.token_hex(8))')
A=P9-A-$$
BASEDIRS=$(python3 -c 'import os;print(os.environ["XDG_STATE_HOME"]+"/herdr-core")')
FILEDIR=$WORK/p9-secret; mkdir -p "$FILEDIR"
FILETOK=DECOYFILE$(python3 -c 'import secrets;print(secrets.token_hex(8))'); ( umask 077; printf '%s\n' "$FILETOK" > "$FILEDIR/token.txt" )
CHENVTOK=DECOYCHENV$(python3 -c 'import secrets;print(secrets.token_hex(8))')
CHARGVTOK=DECOYCHARGV$(python3 -c 'import secrets;print(secrets.token_hex(8))')
CREDTOK=DECOYCREDVAR$(python3 -c 'import secrets;print(secrets.token_hex(8))')
ENVTOKB=DECOYENVB$(python3 -c 'import secrets;print(secrets.token_hex(8))')
ARGVTOKB=DECOYARGVB$(python3 -c 'import secrets;print(secrets.token_hex(8))')
python3 - "$WORK/settings.json" "$BASEDIRS" "$XDG_RUNTIME_DIR/herdr-core" "$XDG_CONFIG_HOME/herdr-core" "$FILEDIR" <<'PY'
import json, sys
deny = [sys.argv[2], sys.argv[3], sys.argv[5]]
json.dump({"permissions": {"allow": ["Bash(bash:*)", "mcp__makiba__send_request"], "deny": []},
           "sandbox": {"enabled": True, "failIfUnavailable": True, "allowUnsandboxedCommands": False, "excludedCommands": [],
                       "filesystem": {"denyRead": deny, "denyWrite": deny + [sys.argv[4]]},
                       # TODO(confirm): shape as in claude-org-ja tools/org_extension_schema.json $comment_sandbox_hardening
                       "credentials": {"envVars": [{"name": "P9_CREDTOK", "mode": "deny"}]}}},
          open(sys.argv[1], "w"), indent=2)
PY

chan_unmeasured() { # chan_unmeasured <why>: the three channel cases when the run stops before they can be measured
  local c; for c in channel-server-running channel-token-env channel-token-argv; do record "$c" NEEDS-HUMAN "$1"; done
}
chan_probe() { # one line from outside the sandbox: pid=<n|none> argv=<yes|no|-> env=<yes|no|unreadable|->; the decoys reach it through the environment, so its own argv holds none
  CH_ARGV=$CHARGVTOK CH_ENV=$CHENVTOK python3 - <<'PY'
import os
a, e, me = os.environ["CH_ARGV"].encode(), os.environ["CH_ENV"].encode(), os.getpid()
for p in os.listdir("/proc"):
    if not p.isdigit() or int(p) == me:
        continue
    try:
        argv = open("/proc/%s/cmdline" % p, "rb").read().split(b"\0")
    except OSError:
        continue
    if a in argv and any(x.endswith(b"channel-standin.py") for x in argv):
        env = None
        try:
            env = e in open("/proc/%s/environ" % p, "rb").read()
        except OSError:
            pass
        print("pid=%s argv=yes env=%s" % (p, "unreadable" if env is None else "yes" if env else "no"))
        break
else:
    print("pid=none argv=- env=-")
PY
}
ctl_ok() { # ctl_ok <argv|env>: the control line before and after the agent's work shows the same pid and that decoy
  local p0=${CH0%% *} p1=${CH1%% *}
  [ -n "$p0" ] && [ "$p0" != pid=none ] && [ "$p0" = "$p1" ] || return 1
  case "$CH0" in *" $1=yes"*) ;; *) return 1 ;; esac
  case "$CH1" in *" $1=yes"*) ;; *) return 1 ;; esac
}
dfield() { printf '%s' "$ld" | sed -n "s/.* $1=\([^ ]*\).*/\1/p"; } # dfield <name>: value of name= on the STEP d line
chan_case() { # chan_case <case> <env|argv> <what>
  local n=""; [ "$ld_ok" = yes ] && n=$(dfield "found$2")
  if [ -n "$n" ] && [ -z "${n//[0-9]/}" ] && [ "$n" != 0 ]; then
    case ",$(dfield "${2}loc")," in
      *",pid$CHPID-"*) record "$1" FAIL "$3 found by Bash, the channel server (pid $CHPID) is among the locations: $ld" ;;
      *) record "$1" FAIL "$3 found by Bash in another process or its own environ, not in the channel server: $ld" ;;
    esac
  elif ! ctl_ok "$2"; then record "$1" NEEDS-HUMAN "control missing, so an absent decoy proves nothing (before: ${CH0:-none}; after: ${CH1:-none}); STEP d: ${ld:-none}"; screen_dump "$T" "$1"
  elif [ "$n" != 0 ]; then record "$1" NEEDS-HUMAN "no usable STEP d line (${ld:-none}); read the screen"; screen_dump "$T" "$1"
  else record "$1" PASS "$3 not found by Bash in any readable environ or cmdline (channel server pid $CHPID visible from Bash: $CHVIS, cmdline=$(dfield chcmdline), environ=$(dfield chenviron)): $ld"; fi
}

# Step files: each appends STEP lines to report.txt (see p1-door-reach.sh).
cat > "$WT/bhttp.py" <<'PY'
import os, sys, urllib.request
mode, url, ua = sys.argv[1:4]
px = ",".join(sorted(k for k in os.environ if k.lower() in ("http_proxy", "https_proxy", "all_proxy", "no_proxy"))) or "none"
try:
    op = urllib.request.build_opener(*([urllib.request.ProxyHandler({})] if mode == "noproxy" else []))
    body = op.open(urllib.request.Request(url, headers={"User-Agent": ua}), timeout=8).read()
    print("OK body=%s proxyvars=%s" % (ascii(body[:40]), px))
except BaseException as e:
    r = getattr(e, "reason", None)
    print("ERR type=%s errno=%s reason=%s msg=%s proxyvars=%s" % (type(e).__name__, getattr(e, "errno", None) if getattr(e, "errno", None) is not None else getattr(r, "errno", None), ascii(r), ascii(str(e)), px))
    sys.exit(1)
PY
cat > "$WT/b.sh" <<SH
for m in default noproxy; do
  out=\$(python3 bhttp.py \$m http://127.0.0.1:$PORT/ping probe-\$m 2>&1)
  rc=\$?
  printf 'STEP b-%s exit=%s out=%s\n' "\$m" "\$rc" "\$(printf %s "\$out" | tr '\n' ' ' | cut -c1-400)" >> report.txt
done
SH
cat > "$WT/scan.py" <<PY
import os
toks = {"argvA": "$TOKEN", "envA": "$ENVTOK", "credvar": "$CREDTOK", "channelcred": "$CRED", "envB": "$ENVTOKB", "argvB": "$ARGVTOKB"}
found, n = [], 0
def chk(where, data):
    for name, t in toks.items():
        if t.encode() in data:
            found.append(name + "@" + where)
chk("own-environ", b"\0".join(k.encode() + b"=" + v.encode() for k, v in os.environ.items()))
for p in os.listdir("/proc"):
    if not p.isdigit():
        continue
    for f in ("cmdline", "environ"):
        try:
            chk("pid%s-%s" % (p, f), open("/proc/%s/%s" % (p, f), "rb").read())
            n += 1
        except OSError:
            pass
try:
    fs = "readable" if "$FILETOK" in open("$FILEDIR/token.txt").read() else "unexpected"
except OSError as e:
    fs = "denied(errno=%s)" % e.errno
print(("FOUND " + ",".join(sorted(set(found))) if found else "NOTFOUND") + " FILE=" + fs + " READABLE_PROC_FILES=%d" % n)
PY
cat > "$WT/c.sh" <<'SH'
out=$(python3 scan.py 2>&1)
rc=$?
printf 'STEP c exit=%s out=%s\n' "$rc" "$(printf %s "$out" | tr '\n' ' ' | cut -c1-900)" >> report.txt
SH
# Step d: the channel server's decoys. The fields come first and the locations (capped) last, so the verdict never rests on cut text.
cat > "$WT/scan2.py" <<PY
import os
chenv, charg = "$CHENVTOK".encode(), "$CHARGVTOK".encode()
try:
    chpid = open("chpid.txt").read().strip()
except OSError:
    chpid = "none"
loc = {"env": [], "argv": []}
n = 0
def chk(where, data):
    if chenv in data:
        loc["env"].append(where)
    if charg in data:
        loc["argv"].append(where)
chk("own-environ", b"\0".join(k.encode() + b"=" + v.encode() for k, v in os.environ.items()))
pids = [p for p in os.listdir("/proc") if p.isdigit()]
for p in pids:
    for f in ("cmdline", "environ"):
        try:
            chk("pid%s-%s" % (p, f), open("/proc/%s/%s" % (p, f), "rb").read())
            n += 1
        except OSError:
            pass
def acc(f):
    if chpid == "none":
        return "unknown"
    try:
        open("/proc/%s/%s" % (chpid, f), "rb").read()
        return "readable"
    except OSError as e:
        return "absent" if e.errno in (2, 3) else "denied(errno=%s)" % e.errno
vis = "unknown" if chpid == "none" else ("yes" if chpid in pids else "no")
print("DONE visible=%s chcmdline=%s chenviron=%s foundenv=%d foundargv=%d readable=%d envloc=%s argvloc=%s" % (
    vis, acc("cmdline"), acc("environ"), len(loc["env"]), len(loc["argv"]), n,
    ",".join(loc["env"][:10]) or "-", ",".join(loc["argv"][:10]) or "-"))
PY
cat > "$WT/d.sh" <<'SH'
out=$(python3 scan2.py 2>&1)
rc=$?
printf 'STEP d exit=%s out=%s\n' "$rc" "$(printf %s "$out" | tr '\n' ' ' | cut -c1-900)" >> report.txt
SH

# The second agent: another session of the same user, with its own decoys (env var, --mcp-config argv). Not sandboxed:
# only its processes matter. Its MCP server points at a closed port, so it does not touch the stand-in.
TB=p9-other
WTB=$WORK/wtb; new_repo "$WTB" || { say "RESULT p9: precondition-failed (repository)"; exit 2; }
MCPB=$(python3 -c 'import json,sys;print(json.dumps({"mcpServers":{"other":{"type":"http","url":"http://127.0.0.1:1/mcp","headers":{"Authorization":"Bearer "+sys.argv[1]}}}}))' "$ARGVTOKB")
launch "$TB" "$WTB" "P9_ENVTOK_B=$ENVTOKB" -- --permission-mode default --mcp-config "$MCPB" || { record launch FAIL "hc run failed (second agent)"; chan_unmeasured "run stopped before the worker started"; finish; }
pass_dialogs "$TB" 120 || { record launch NEEDS-HUMAN "second agent: dialogs not passed; see result file"; screen_dump "$TB" other; chan_unmeasured "run stopped before the worker started"; finish; }
wait_ready "$TB" 60 || { record launch NEEDS-HUMAN "second agent not ready"; screen_dump "$TB" other; chan_unmeasured "run stopped before the worker started"; finish; }

T=p9-worker
# The channel entry (with both new decoys) goes into a file under a denyRead path and is passed as a second --mcp-config, so its decoys
# are not in the claude process's own argv; the inline JSON keeps the http entry only (TODO(confirm): two --mcp-config values merge).
CHCFG=$FILEDIR/channel-mcp.json
MCPJSON=$(python3 - "$MCPCFG" "$CHCFG" "$CHENVTOK" "$CHARGVTOK" <<'PY'
import json, os, sys
cfg, out, chenv, charg = sys.argv[1:5]
d = json.load(open(cfg))
ch = d["mcpServers"].pop("makiba-channel")
ch["env"]["STANDIN_DECOY_ENV"] = chenv
ch["args"] += ["--state", charg]   # channel mode of the stand-in takes and ignores --state
os.umask(0o077)
json.dump({"mcpServers": {"makiba-channel": ch}}, open(out, "w"), indent=2)
print(json.dumps(d))
PY
) || { say "RESULT p9: precondition-failed (channel config)"; exit 2; }
launch "$T" "$WT" "P9_ENVTOK=$ENVTOK" "P9_CREDTOK=$CREDTOK" "GH_CONFIG_DIR=" -- --settings "$WORK/settings.json" --permission-mode acceptEdits \
  --mcp-config "$MCPJSON" --mcp-config "$CHCFG" --dangerously-load-development-channels server:makiba-channel || { record launch FAIL "hc run failed"; chan_unmeasured "run stopped before the worker started"; finish; }
pass_dialogs "$T" 150 || { record launch NEEDS-HUMAN "dialogs not passed; see result file"; screen_dump "$T" launch; chan_unmeasured "run stopped before the worker started"; finish; }
wait_ready "$T" 60 || { record launch NEEDS-HUMAN "worker not ready"; screen_dump "$T" launch; chan_unmeasured "run stopped before the worker started"; finish; }
# Control from outside the sandbox: Claude Code has started the channel server; find it (up to 30 s) and read its environ and cmdline.
CH0=""; i=0; while [ "$i" -lt 30 ]; do CH0=$(chan_probe); case "$CH0" in pid=none*) ;; *) break ;; esac; i=$((i + 2)); sleep 2; done
CHPID=$(printf '%s' "$CH0" | sed -n 's/^pid=\([0-9]*\) .*/\1/p'); CHPID=${CHPID:-none}; printf '%s\n' "$CHPID" > "$WT/chpid.txt"
say "control (before): ${CH0:-none}"
hc send "$T" "This is a permitted sandbox measurement. Do these four things in order, each as its own tool call, and keep going if one fails or is refused (never retry a refused call another way, never skip the sandbox): 1) call the MCP tool send_request (server makiba) with the text $A  2) run with Bash: bash b.sh  3) run with Bash: bash c.sh  4) run with Bash: bash d.sh. Then reply, for each of the four, whether it ran and the full error text word for word." >/dev/null 2>&1

i=0; while [ "$i" -lt 240 ]; do
  has_dialog "$T" && answer_dialog "$T"
  [ "$(grep -c '^STEP' "$WT/report.txt" 2>/dev/null)" -ge 4 ] && break
  [ "$i" -ge 120 ] && wait_idle "$T" 3 && break
  i=$((i + 3)); sleep 3
done
wait_idle "$T" 60
say "---- report.txt"; cat "$WT/report.txt" 2>/dev/null | tee -a "$RESULT"
say "---- stand-in access.log"; cat "$STATE/access.log" 2>/dev/null | tee -a "$RESULT"
say "---- stand-in requests.jsonl"; cat "$STATE/requests.jsonl" 2>/dev/null | tee -a "$RESULT"

if grep -q "$A" "$STATE/requests.jsonl" 2>/dev/null; then record mcp-tool-reaches PASS "the stand-in received send_request with the probe text"
else record mcp-tool-reaches FAIL "no send_request with $A reached the stand-in"; screen_dump "$T" mcp; fi

# control from outside the sandbox: the listener is up (distinct user agent, so it is not mistaken for the worker's request)
outside=no; python3 -c 'import sys,urllib.request;r=urllib.request.Request(sys.argv[1],headers={"User-Agent":"probe-outside"});sys.exit(0 if urllib.request.urlopen(r,timeout=5).read()==b"pong" else 1)' "http://127.0.0.1:$PORT/ping" 2>/dev/null && outside=yes
judge_http() { # judge_http <case> <default|noproxy>: each step has its own user agent, so the access log tells them apart
  local c=$1 s=$2 l rc; l=$(grep "^STEP b-$s " "$WT/report.txt" 2>/dev/null | head -n 1)
  rc=$(printf '%s' "$l" | sed -n 's/^STEP b-[a-z]* exit=\([0-9]*\) .*/\1/p')
  if grep -q "GET /ping.*ua=probe-$s" "$STATE/access.log" 2>/dev/null; then
    record "$c" FAIL "Bash reached the port (the stand-in logged this request): $l. Then the bearer token is the only guard."
  elif [ "$outside" != yes ]; then
    record "$c" NEEDS-HUMAN "the stand-in did not answer from outside the sandbox either, so a block cannot be told from a dead listener: $l"
  elif [ -n "$rc" ] && [ "$rc" != 0 ] && printf '%s' "$l" | grep -Eqi 'Connection refused|Network is unreachable|Operation not permitted|Permission denied|Forbidden|Proxy|Tunnel|timed out|errno=(1|13|101|110|111|113) |HTTP Error (403|407|502|503)'; then
    record "$c" PASS "failed with a network-style error, nothing reached the stand-in, and it answers from outside: $l"
  else record "$c" NEEDS-HUMAN "no usable or attributable result (${l:-none}); read the screen"; screen_dump "$T" "$c"; fi
}
judge_http bash-http-direct noproxy
judge_http bash-http-via-default-proxy default

lc=$(grep '^STEP c ' "$WT/report.txt" 2>/dev/null | head -n 1)
tcase() { # tcase <case> <regex of locations that mean readable> <what>
  if ! printf '%s' "$lc" | grep -Eq 'NOTFOUND|FOUND'; then record "$1" NEEDS-HUMAN "no usable STEP c line (${lc:-none}); read the screen"; screen_dump "$T" "$1"
  elif printf '%s' "$lc" | grep -Eq "$2"; then record "$1" FAIL "$3 readable from Bash: $lc"
  else record "$1" PASS "$3 not found by Bash: $lc"; fi
}
tcase own-token-env 'envA@' "this session's env decoy"
tcase own-token-argv 'argvA@' "this session's --mcp-config argv decoy"
case "$lc" in
  *FILE=readable*) record own-token-file FAIL "the token file under a denyRead path was read: $lc" ;;
  *FILE=denied*) record own-token-file PASS "reading the token file under a denyRead path was refused: $lc" ;;
  *) record own-token-file NEEDS-HUMAN "no usable file result (${lc:-none}); read the screen"; screen_dump "$T" file ;;
esac
if [ "$(state_of "$TB")" = error ] || ! hc read "$TB" >/dev/null 2>&1; then record other-session-token NEEDS-HUMAN "the second agent is not running, so its decoys were not in /proc"
else tcase other-session-token 'envB@|argvB@' "the other session's decoys (env var, argv)"; fi
tcase credential-envvar-unset 'credvar@own-environ' "the decoy named in sandbox.credentials.envVars (in Bash's own environ)"
CH1=$(chan_probe); say "control (after): ${CH1:-none}"
ld=$(grep '^STEP d ' "$WT/report.txt" 2>/dev/null | head -n 1); ld_ok=no
case "$ld" in "STEP d exit=0 out=DONE "*) ld_ok=yes ;; esac
CHVIS=unknown; [ "$ld_ok" = yes ] && CHVIS=$(dfield visible)
say "OBSERVE channel-server-pid-visible-from-bash: ${CHVIS:-unknown} (channel server pid $CHPID; chcmdline=$(dfield chcmdline) chenviron=$(dfield chenviron); unknown = no control pid or no usable STEP d line)"
if ctl_ok argv && ctl_ok env; then record channel-server-running PASS "the channel server runs (pid $CHPID) and its /proc/$CHPID/cmdline and environ hold the decoys, before and after the agent's work"
else record channel-server-running NEEDS-HUMAN "control missing (before: ${CH0:-none}; after: ${CH1:-none}); the two channel-token cases cannot show absence"; screen_dump "$T" channel-server-running; fi
chan_case channel-token-env env "the decoy in the channel server entry's env"
chan_case channel-token-argv argv "the decoy in the channel server entry's args"
finish
