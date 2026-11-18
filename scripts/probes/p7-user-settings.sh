#!/usr/bin/env bash
# P7 (probes.md): what an agent takes from the person's own Claude Code settings, and whether a worker can notify the person.
# The person is played by a scratch HOME: a copy of the real user-level folder (settings.json, CLAUDE.md, .credentials.json,
# skills/commands/agents/hooks, .claude.json) with four decoys merged in (rule, hooks, skill, connector). The real folder is read
# once and never written; every claude is launched with HOME (and CLAUDE_CONFIG_DIR) below the scratch folder.
# Launches are made the way makibad would (role rules as an appended file, per-task settings file, role connector), once per option:
#   plain (control), sources (--setting-sources project,local), sources-strict (+ --strict-mcp-config), configdir (CLAUDE_CONFIG_DIR).
# Cases: control-plain, option-sources, option-sources-strict, option-configdir, isolation-found, curator-isolated,
# secretary-decoys, secretary-separate, notify-outside (always NEEDS-HUMAN: a script cannot see a phone), real-folder-untouched.
# P7_NOTIFY_WAIT (seconds, default 70): the no-input window after each event, so that delayed notifications are not cancelled by the next input.
# Run from a login shell: bash scripts/probes/p7-user-settings.sh <result-dir>   (about 40 minutes; watch the phone and the desktop)
set -u
HOST_CFG=${CLAUDE_CONFIG_DIR:-}   # read before probe_init; unset right after, so no launch ever points at it
unset CLAUDE_CONFIG_DIR
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "P7: user-level Claude Code settings reaching agents, and notifications from workers (needs a person at the host)"; exit 0 ;; esac
probe_init p7 "${1:-}"
umask 077
standin_start || { say "RESULT p7: precondition-failed (stand-in did not start)"; exit 2; }
P7_DONE=""
extra_cleanup() { kill "$STANDIN_PID" 2>/dev/null; [ "$P7_DONE" = 1 ] || say "note: the script ended early (no SUMMARY line follows); a killed or hung run looks like this"; }
NW=${P7_NOTIFY_WAIT:-70}; case $NW in ''|*[!0-9]*) NW=70 ;; esac

SRC=${HOST_CFG:-$HOME/.claude}
if [ -n "$HOST_CFG" ]; then SRCJSON=$HOST_CFG/.claude.json; else SRCJSON=$HOME/.claude.json; fi
HC_DIR=$WORK/hostcopy
CTL_RULE=0; CTL_SKILL=0; CTL_HOOK=0; CTL_CONN=0   # what the control run showed (an item not shown there proves nothing when absent later)
ISO_OPT=""; ISO_T=""; ISO_RUN=""
OPT_RESULTS=""   # "option=STATUS(detail)" for isolation-found

# ---- helpers (all defined before the first call)
under_work() { case "$1" in "$WORK"/*) return 0 ;; esac; return 1; }   # no launch may point at anything outside the scratch folder
nlines() { if [ -f "$1" ]; then wc -l < "$1" | tr -d ' '; else echo 0; fi; }
retire() { hc kill "$1" >/dev/null 2>&1; hc rm "$1" >/dev/null 2>&1; }
real_manifest() { # hashes of the real folder's files the probe must never change (.claude.json is shared with running sessions: only its decoy strings are checked)
  local f d
  for f in settings.json CLAUDE.md .credentials.json; do if [ -f "$SRC/$f" ]; then sha256sum "$SRC/$f"; else echo "absent $f"; fi; done
  for d in skills commands agents hooks; do if [ -d "$SRC/$d" ]; then find "$SRC/$d" -type f -printf '%p %s %T@\n' 2>/dev/null | sort; fi; done | sha256sum
}

build_hostcopy() { # read-only use of the real folder: files into $HC_DIR/.claude and $HC_DIR/.claude.json (symlinks followed, so nothing points back out)
  local f d
  mkdir -p "$HC_DIR/.claude" || return 1
  for f in settings.json CLAUDE.md .credentials.json; do [ -f "$SRC/$f" ] && command cp -L "$SRC/$f" "$HC_DIR/.claude/$f"; done
  for d in skills commands agents hooks; do [ -d "$SRC/$d" ] && command cp -RL "$SRC/$d" "$HC_DIR/.claude/$d"; done
  [ -f "$SRCJSON" ] && command cp -L "$SRCJSON" "$HC_DIR/.claude.json"
  chmod -R go-rwx "$HC_DIR"
  return 0
}

scan_notify() { # names (and scalar values) of notification-like settings in the host copy; exit 3 when none
  python3 - "$HC_DIR/.claude/settings.json" "$HC_DIR/.claude.json" <<'PY'
import json, re, sys
pat = re.compile(r"notif|bell|push|remote|alert|sound|away|channel", re.I)
skip = re.compile(r"token|secret|passw|cred|auth|apikey|api_key", re.I)
found = 0
def walk(fn, o, path, depth):
    global found
    if not isinstance(o, dict) or depth > 3:
        return
    for k, v in o.items():
        if k == "projects" or skip.search(k):
            continue
        p = path + "." + k if path else k
        if pat.search(k):
            found += 1
            if isinstance(v, (bool, int, float)) or (isinstance(v, str) and len(v) <= 60):
                print("HOST-NOTIFY %s:%s = %s" % (fn, p, json.dumps(v)))
            else:
                print("HOST-NOTIFY %s:%s (%s, value not printed)" % (fn, p, type(v).__name__))
        walk(fn, v, p, depth + 1)
for fn in sys.argv[1:]:
    try:
        d = json.load(open(fn))
    except Exception:
        print("HOST-NOTIFY %s: absent or unreadable" % fn.rsplit("/", 1)[-1]); continue
    name = fn.rsplit("/", 1)[-1]
    walk(name, d, "", 0)
    h = d.get("hooks") if isinstance(d, dict) else None
    if isinstance(h, dict):
        for ev, lst in h.items():
            n = sum(len(x.get("hooks", [])) for x in lst if isinstance(x, dict)) if isinstance(lst, list) else 0
            cmds = json.dumps(lst)
            far = " (a handler names a path under a user folder: the copy may not run it)" if re.search(r"\.claude|~/|\$HOME", cmds) else ""
            print("HOST-HOOK %s: event %s, %d handler(s), command text not printed%s" % (name, ev, n, far))
            if re.search(r"notif|stop|idle|elicit|permission", ev, re.I):
                found += 1
sys.exit(0 if found else 3)
PY
}

make_home() { # make_home <home> <marks>: the person's folder (default place under a scratch HOME) with the four decoys
  mkdir -p "$1/.claude" "$2" || return 1
  command cp -R "$HC_DIR/." "$1/" 2>/dev/null
  chmod -R go-rwx "$1"
  python3 - "$1" "$2" <<'PY'
import json, os, sys
home, marks = sys.argv[1:3]
cdir = os.path.join(home, ".claude")
with open(os.path.join(cdir, "CLAUDE.md"), "a") as f:   # rule
    f.write("\nPROBE-USER-RULE: put the word PROBE-USER on a line of its own, just before your last line.\n")
sd = os.path.join(cdir, "skills", "probe-user-skill")   # skill
os.makedirs(sd, exist_ok=True)
with open(os.path.join(sd, "SKILL.md"), "w") as f:
    f.write("---\nname: probe-user-skill\ndescription: Probe decoy skill. Use it only when asked for probe-user-skill.\n---\nDecoy.\n")
sp = os.path.join(cdir, "settings.json")   # hooks: one marker line per event
try:
    s = json.load(open(sp))
except Exception:
    s = {}
if not isinstance(s, dict):
    s = {}
h = s.setdefault("hooks", {})
for ev in ("UserPromptSubmit", "Stop", "Notification"):
    h.setdefault(ev, []).append({"hooks": [{"type": "command", "command": "date -u +%%FT%%TZ >> '%s/user-hook-%s'" % (marks, ev)}]})
json.dump(s, open(sp, "w"), indent=2)
cp = os.path.join(home, ".claude.json")   # connector (user scope), TODO(confirm): lives in .claude.json under mcpServers
try:
    d = json.load(open(cp))
except Exception:
    d = {}
if not isinstance(d, dict):
    d = {}
d.setdefault("mcpServers", {})["probe-user-conn"] = {"command": "bash", "args": ["-c", "date -u >> '%s/user-conn'; exec sleep 600" % marks]}
json.dump(d, open(cp, "w"), indent=2)
PY
}

make_alt() { # make_alt <dir>: an alternate configuration folder holding only makiba's files (an empty settings file) and the login
  mkdir -p "$1" || return 1
  printf '{}\n' > "$1/settings.json"
  [ -f "$HC_DIR/.claude/.credentials.json" ] && command cp -f "$HC_DIR/.claude/.credentials.json" "$1/.credentials.json"
  chmod -R go-rwx "$1"
  return 0
}

make_role() { # make_role <run> <role> <marks>: role rules, per-task settings file, role connector (the stand-in's http server only)
  printf 'ROLE-RULE (%s): end every answer with the word ROLE-OK. Use tools only when asked to.\n' "$2" > "$WORK/rules-$1.txt"
  python3 - "$WORK/role-$1.json" "$WORK/mcp-$1.json" "$3" "$MCPCFG" <<'PY'
import json, sys
out, mcpout, marks, mcpcfg = sys.argv[1:5]
hooks = {}
for ev in ("UserPromptSubmit", "Stop", "Notification", "InstructionsLoaded"):   # TODO(confirm): InstructionsLoaded is a hook event of this Claude Code version (its binary names it)
    cmd = "printf '%%s %%s\\n' \"$(date -u +%%FT%%TZ)\" \"$(head -c 400 | tr -d '\\n')\" >> '%s/role-hook-%s'" % (marks, ev)
    hooks[ev] = [{"hooks": [{"type": "command", "command": cmd}]}]
json.dump({"hooks": hooks, "permissions": {"allow": ["mcp__makiba__send_request"], "ask": ["Bash(python3:*)"]}}, open(out, "w"), indent=2)
srv = json.load(open(mcpcfg))["mcpServers"]["makiba"]
json.dump({"mcpServers": {"makiba": srv}}, open(mcpout, "w"), indent=2)
PY
}

await_blocked() { # await_blocked <terminal> <seconds>
  local end=$(( $(date +%s) + $2 ))
  while [ "$(date +%s)" -lt "$end" ]; do case "$(term_info "$1")" in blocked*) return 0 ;; esac; sleep 2; done
  return 1
}
unblock() { # unblock <terminal>: Escape until the base no longer reads blocked, then wait for rest
  local i=0
  while [ "$i" -lt 3 ]; do
    hc keys "$1" escape >/dev/null 2>&1; sleep 3   # TODO(confirm): Escape closes a question and a permission prompt (as in p8)
    case "$(term_info "$1")" in blocked*) i=$((i + 1)) ;; *) wait_idle "$1" 60; return $? ;; esac
  done
  return 1
}
hold() { say "EVENT run=$1 $2: no input for ${NW}s from $(date -u +%FT%TZ) (watch the phone, the desktop and the terminal bell)"; sleep "$NW"; }

do_event() { # do_event <T> <run> <label> <prompt> <screen-regex> <regex-that-must-be-absent>: sets EV_RES
  local T=$1 run=$2 label=$3 m=$WORK/marks-$2 n0 u0 res=ok
  n0=$(nlines "$m/role-hook-Notification"); u0=$(nlines "$m/user-hook-Notification")
  say "EVENT run=$run $label: prompt sent $(date -u +%FT%TZ)"
  if ! hc send "$T" "$4" >/dev/null 2>&1; then EV_RES="$label=send-failed"; return 1; fi
  if ! await_blocked "$T" 90; then res=never-blocked; screen_dump "$T" "$run-$label"
  else
    hc read "$T" > "$WORK/ev.txt" 2>/dev/null
    if grep -Eq "$5" "$WORK/ev.txt" && ! grep -Eq "$6" "$WORK/ev.txt"; then say "EVENT run=$run $label: the expected prompt is on the screen and the base reads blocked ($(date -u +%FT%TZ))"
    else res=unverified-screen; { echo "---- screen of $T at the event $label (not the expected prompt)"; cat "$WORK/ev.txt"; echo "----"; } >> "$RESULT"; fi
    hold "$run" "$label"
    unblock "$T" || res="$res,prompt-not-closed"
  fi
  EV_RES="$label=$res (role Notification hook +$(( $(nlines "$m/role-hook-Notification") - n0 )), user Notification hook +$(( $(nlines "$m/user-hook-Notification") - u0 )))"
  say "EVENT run=$run $EV_RES"
  case "$res" in ok) return 0 ;; esac
  return 1
}

judge_run() { # judge_run <run>: J_RULE J_SKILL J_HOOK J_CONN (1 = reaches the agent), J_APPLIED (names), J_ROLE_MISSING (names)
  local s=$WORK/screen-$1.txt m=$WORK/marks-$1
  J_RULE=0; J_SKILL=0; J_HOOK=0; J_CONN=0; J_APPLIED=""; J_ROLE_MISSING=""
  { grep -q 'PROBE-USER' "$s" 2>/dev/null || grep -q "home-$1/.claude/CLAUDE.md" "$m/role-hook-InstructionsLoaded" 2>/dev/null; } && J_RULE=1
  grep -q 'probe-user-skill' "$s" 2>/dev/null && J_SKILL=1
  [ -s "$m/user-hook-UserPromptSubmit" ] && J_HOOK=1
  { [ -s "$m/user-conn" ] || grep -q 'probe-user-conn' "$s" 2>/dev/null; } && J_CONN=1
  [ "$J_RULE" = 1 ] && J_APPLIED="$J_APPLIED rule"
  [ "$J_SKILL" = 1 ] && J_APPLIED="$J_APPLIED skill"
  [ "$J_HOOK" = 1 ] && J_APPLIED="$J_APPLIED hook"
  [ "$J_CONN" = 1 ] && J_APPLIED="$J_APPLIED connector"
  grep -q 'ROLE-OK' "$s" 2>/dev/null || J_ROLE_MISSING="$J_ROLE_MISSING rule"
  [ -s "$m/role-hook-UserPromptSubmit" ] || J_ROLE_MISSING="$J_ROLE_MISSING hook"
  grep -q "P7-$1" "$STATE/requests.jsonl" 2>/dev/null || J_ROLE_MISSING="$J_ROLE_MISSING connector"
}

run_one() { # run_one <run> <role> <option> <events yes|no>: RO_OK=1 when the listing and the role connector turn were read
  local run=$1 role=$2 opt=$3 ev=$4 T=p7-$1 home=$WORK/home-$1 marks=$WORK/marks-$1 alt=$WORK/alt-$1 repo=$WORK/repo-$1 cmd
  local args=() envx=()
  RO_OK=0; RO_WHY=""; RO_EVENTS=""; RO_T=$T
  case "$opt" in
    plain) ;;
    sources) args=(--setting-sources project,local) ;;
    sources-strict) args=(--setting-sources project,local --strict-mcp-config) ;;
    configdir) envx=("CLAUDE_CONFIG_DIR=$alt") ;;
    *) RO_WHY="unknown option $opt"; return 1 ;;
  esac
  make_home "$home" "$marks" || { RO_WHY="could not build the scratch home"; return 1; }
  [ "$opt" = configdir ] && { make_alt "$alt" || { RO_WHY="could not build the alternate folder"; return 1; }; }
  make_role "$run" "$role" "$marks" || { RO_WHY="could not write the role files"; return 1; }
  under_work "$home" && { [ "$opt" != configdir ] || under_work "$alt"; } || { RO_WHY="refused: a configuration folder outside the scratch folder"; return 1; }
  new_repo "$repo" || { RO_WHY="could not create the repository"; return 1; }
  say "RUN $run: role=$role option=$opt"
  say "RUN $run: launch = env HOME=<scratch> ${envx[*]:-} claude --permission-mode default --settings <role-settings> --append-system-prompt-file <role-rules> --mcp-config <role-connector> ${args[*]:-}   (claude $CLVER)"
  launch "$T" "$repo" "HOME=$home" "PROBE_RUN=$run" ${envx[@]+"${envx[@]}"} -- --permission-mode default --settings "$WORK/role-$run.json" \
    --append-system-prompt-file "$WORK/rules-$run.txt" --mcp-config "$WORK/mcp-$run.json" ${args[@]+"${args[@]}"} || { RO_WHY="hc run failed"; return 1; }
  pass_dialogs "$T" 150; local rc=$?
  if [ "$rc" -ne 0 ]; then RO_WHY="no composer (dialog step code $rc; for configdir this can be the cost of a login or onboarding)"; screen_dump "$T" "$run-launch"; return 1; fi
  wait_ready "$T" 60 || { RO_WHY="agent not ready"; screen_dump "$T" "$run-launch"; return 1; }
  if ! hc send "$T" "List every rule, skill, hook and connector you can see." >/dev/null 2>&1; then RO_WHY="hc send failed"; return 1; fi
  sleep 6
  wait_idle "$T" 150 || { RO_WHY="the listing turn did not end in 150s"; screen_dump "$T" "$run-listing"; return 1; }
  hc read --lines 300 "$T" > "$WORK/screen-$run.txt" 2>/dev/null
  { echo "---- listing answer of $run"; cat "$WORK/screen-$run.txt"; echo "----"; } >> "$RESULT"
  if hc send "$T" "Call the MCP tool send_request (server makiba) with the text P7-$run, then reply with the single word SENT." >/dev/null 2>&1; then
    sleep 6; wait_idle "$T" 120 || say "note: run $run: the connector turn did not end in 120s"
  fi
  RO_OK=1
  if [ "$ev" = yes ]; then
    hold "$run" "turn-end"   # the turn above is the turn end: nothing is typed while the stop notification may be on its way
    do_event "$T" "$run" question "Use the AskUserQuestion tool to ask me exactly one question, with the text PROBE-QUESTION: red or blue?, and the two options red and blue. Do nothing else until I answer." '^[[:space:]]*❯[[:space:]]*1\.' 'Do you want to proceed'; RO_EVENTS="$RO_EVENTS $EV_RES;"
    cmd="python3 $WORK/mark.py $WORK/ran-$run.txt"
    do_event "$T" "$run" permission "Run exactly this one command with your Bash tool: $cmd   Then reply with the single word RAN." 'Do you want to proceed' 'ZZZ-never-matches'; RO_EVENTS="$RO_EVENTS $EV_RES;"
  fi
  return 0
}

verdict_option() { # verdict_option <case> <run> <option>: PASS = nothing of the person's reaches the agent while the role's own does
  local c=$1 r=$2 opt=$3 miss=""
  if [ "$RO_OK" != 1 ]; then OPT_RESULTS="$OPT_RESULTS $opt=NEEDS-HUMAN(no result);"; record "$c" NEEDS-HUMAN "option $opt: no result: $RO_WHY"; V_STATUS=NEEDS-HUMAN; return 0; fi
  judge_run "$r"
  say "RESULT-OF-RUN $r option=$opt: decoys reaching the agent:${J_APPLIED:- none}; role items missing:${J_ROLE_MISSING:- none}; events:$RO_EVENTS"
  if [ -n "$J_APPLIED" ]; then V_STATUS=FAIL; OPT_RESULTS="$OPT_RESULTS $opt=FAIL(reach:$J_APPLIED);"; record "$c" FAIL "option $opt: these user-level items reach the agent:$J_APPLIED (the others did not show); role items missing:${J_ROLE_MISSING:- none}"; return 0; fi
  [ "$CTL_RULE" = 1 ] || miss="$miss rule"; [ "$CTL_SKILL" = 1 ] || miss="$miss skill"; [ "$CTL_HOOK" = 1 ] || miss="$miss hook"; [ "$CTL_CONN" = 1 ] || miss="$miss connector"
  if [ -n "$J_ROLE_MISSING" ]; then V_STATUS=NEEDS-HUMAN; OPT_RESULTS="$OPT_RESULTS $opt=NEEDS-HUMAN(role missing:$J_ROLE_MISSING);"; record "$c" NEEDS-HUMAN "option $opt: no decoy showed, but the role's own$J_ROLE_MISSING did not apply either, so the launch itself is suspect; read the screens"; screen_dump "$RO_T" "$r"
  elif [ -n "$miss" ]; then V_STATUS=NEEDS-HUMAN; OPT_RESULTS="$OPT_RESULTS $opt=NEEDS-HUMAN(control did not show:$miss);"; record "$c" NEEDS-HUMAN "option $opt: no decoy showed, but the control run did not show$miss, so their absence proves nothing"
  else V_STATUS=PASS; OPT_RESULTS="$OPT_RESULTS $opt=PASS;"; record "$c" PASS "option $opt: no decoy reached the agent (rule, skill, hook, connector) while the role's own rule, hook and connector applied; events:$RO_EVENTS"; fi
}

# ---- start
printf 'import sys\nopen(sys.argv[1], "w").write("x")\n' > "$WORK/mark.py"
say "host CLAUDE_CONFIG_DIR: $( [ -n "$HOST_CFG" ] && echo set || echo unset ) (never used by a launch); real user folder read from: $SRC"
build_hostcopy || { say "RESULT p7: precondition-failed (could not copy the host's settings files)"; exit 2; }
MAN0=$(real_manifest)
say "---- notification-like settings of the host, by name (read from the scratch copy)"
scan_notify > "$WORK/hn.txt" 2>&1; hn_rc=$?
while IFS= read -r l; do say "$l"; done < "$WORK/hn.txt"
env_names=$(env | cut -d= -f1 | grep -iE 'notif|bell|push|remote|alert|sound' | tr '\n' ' ')
say "HOST-NOTIFY environment variable names matching:${env_names:- none}"
[ "$hn_rc" != 0 ] && [ -z "$env_names" ] && say "HOST-NOTIFY none found in settings.json, .claude.json or the environment names (OS and terminal notification settings are not visible to a script)"
say "---- end of host notification settings"
say "options (exact): plain = no extra option; sources = --setting-sources project,local; sources-strict = --setting-sources project,local --strict-mcp-config; configdir = CLAUDE_CONFIG_DIR=<alternate folder with an empty settings.json and a copy of .credentials.json>; claude --version: $CLVER"
say "notification window after each event: ${NW}s"

# ---- control: nothing special. The decoys must apply here, or a later "absent" means nothing.
run_one ctl worker plain yes
if [ "$RO_OK" != 1 ]; then record control-plain NEEDS-HUMAN "no result: $RO_WHY; no later case can be concluded"
else
  judge_run ctl; CTL_RULE=$J_RULE; CTL_SKILL=$J_SKILL; CTL_HOOK=$J_HOOK; CTL_CONN=$J_CONN
  say "RESULT-OF-RUN ctl option=plain: decoys reaching the agent:${J_APPLIED:- none}; role items missing:${J_ROLE_MISSING:- none}; events:$RO_EVENTS"
  if [ "$J_RULE$J_SKILL$J_HOOK$J_CONN" = 1111 ] && [ -z "$J_ROLE_MISSING" ]; then record control-plain PASS "with no option all four decoys reach the worker (rule, skill, hook, connector) and the role's own rule, hook and connector apply; events:$RO_EVENTS"
  else record control-plain NEEDS-HUMAN "decoys reaching the worker:${J_APPLIED:- none}; role items missing:${J_ROLE_MISSING:- none}; an item that does not show here proves nothing in the options below; read the screens"; screen_dump "$RO_T" ctl; fi
fi
retire "p7-ctl"

# ---- the three options
for opt in sources sources-strict configdir; do
  run_one "w-$opt" worker "$opt" yes
  verdict_option "option-$opt" "w-$opt" "$opt"
  if [ "$V_STATUS" = PASS ] && [ -z "$ISO_OPT" ]; then ISO_OPT=$opt; ISO_T=$RO_T; ISO_RUN=w-$opt   # this terminal stays up for secretary-separate
  else retire "p7-w-$opt"; fi
done
if [ -n "$ISO_OPT" ]; then record isolation-found PASS "option $ISO_OPT keeps every decoy out of a worker while the role's own rules and settings apply; all options:$OPT_RESULTS"
elif printf '%s' "$OPT_RESULTS" | grep -q NEEDS-HUMAN; then record isolation-found NEEDS-HUMAN "no option isolated conclusively, and some were inconclusive:$OPT_RESULTS"
else record isolation-found FAIL "no option keeps the decoys out of a worker (items that reach agents, per option):$OPT_RESULTS Record how the role rules and settings rank against them (M3-4, M4-3); the cost of sharing login and connectors into configdir is in its case line"; fi
say "note: configdir moves the login (copied here as .credentials.json only) and the connectors (they live in .claude.json, which the alternate folder does not have); dialogs it shows are in the result file"

# ---- the curator: the same launch as a worker; the first option that isolated
if [ -n "$ISO_OPT" ]; then
  run_one cur curator "$ISO_OPT" yes
  verdict_option curator-isolated cur "$ISO_OPT"
  retire "p7-cur"
else record curator-isolated NEEDS-HUMAN "no isolating option to test"; fi

# ---- step 4, the secretary: only which decoys apply
run_one sec-plain secretary plain no
if [ "$RO_OK" != 1 ]; then record secretary-decoys NEEDS-HUMAN "no result: $RO_WHY"
else
  judge_run sec-plain
  record secretary-decoys PASS "under the plain option these decoys reach the secretary:${J_APPLIED:- none}; role items missing:${J_ROLE_MISSING:- none}; observation only, nothing here is judged"
fi
retire "p7-sec-plain"
if [ -z "$ISO_OPT" ]; then record secretary-separate NEEDS-HUMAN "no isolating option, so there is nothing to apply to the secretary separately"
else
  run_one sec-iso secretary "$ISO_OPT" no
  if [ "$RO_OK" != 1 ]; then record secretary-separate NEEDS-HUMAN "the secretary did not start under option $ISO_OPT: $RO_WHY"
  else
    judge_run sec-iso; sec_applied=$J_APPLIED; sec_miss=$J_ROLE_MISSING
    m=$WORK/marks-$ISO_RUN; t0=$(term_info "$ISO_T"); n0=$(nlines "$m/role-hook-UserPromptSubmit")
    hc send "$ISO_T" "Reply with the single word FRESH." >/dev/null 2>&1; sleep 6; wait_idle "$ISO_T" 90
    t1=$(term_info "$ISO_T"); n1=$(nlines "$m/role-hook-UserPromptSubmit")
    if [ "$n1" -gt "$n0" ] && [ ! -e "$m/user-hook-UserPromptSubmit" ] && [ ! -e "$m/user-conn" ] && [ -z "$sec_miss" ]; then
      record secretary-separate PASS "the secretary started under option $ISO_OPT with its own role rule, hook and connector applying; decoys still reaching it:${sec_applied:- none}; the isolated worker kept running beside it, took a fresh turn ('$t0' -> '$t1') and still shows no decoy marker"
    else record secretary-separate NEEDS-HUMAN "secretary role items missing:${sec_miss:- none}; decoys reaching it:${sec_applied:- none}; fresh worker turn hook lines $n0 -> $n1; worker decoy markers: $(ls "$m" 2>/dev/null | grep -c '^user-') ; read the screens"; fi
  fi
  retire "p7-sec-iso"
fi
[ -n "$ISO_T" ] && retire "$ISO_T"

# ---- notifications outside the terminal: only a person can see them
record notify-outside NEEDS-HUMAN "a script cannot see a phone, a desktop or a bell. For each run the EVENT lines above give the UTC time of every turn end, question and permission prompt, and the 'Notification hook' counts say whether Claude Code itself announced the event. Compare with what rang, per run (control-plain and the options). P7 passes only if no stop, question or permission prompt of a worker or the curator in an isolating option notified anything outside its terminal. Host notification settings found: see the HOST-NOTIFY lines"

# ---- the real folder was only read
MAN1=$(real_manifest)
leak=$(grep -rlF -e PROBE-USER -e probe-user "$SRC/settings.json" "$SRC/CLAUDE.md" "$SRCJSON" "$SRC/skills" 2>/dev/null | tr '\n' ' ')
if [ -n "$leak" ] || [ -e "$SRC/skills/probe-user-skill" ]; then record real-folder-untouched FAIL "decoy text found in the real folder: $leak"
elif [ "$MAN0" != "$MAN1" ]; then record real-folder-untouched NEEDS-HUMAN "no decoy text in the real folder, but settings.json, CLAUDE.md, .credentials.json or skills/commands/agents/hooks changed during the run (a running session of yours can do that, for example a token refresh); compare by hand"
else record real-folder-untouched PASS "settings.json, CLAUDE.md, .credentials.json and skills/commands/agents/hooks of $SRC are unchanged and hold no decoy text"; fi

say "NOT RUN (only needed when a case above failed; M3-4 and M4-3 run them): how the role rules rank against a leaking user-level item; switching the notification off by a worker's own settings."
P7_DONE=1
finish
