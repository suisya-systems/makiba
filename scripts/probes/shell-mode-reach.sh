#!/usr/bin/env bash
# Issue 150, first step: can a step be run inside the launched agent's sandbox WITHOUT the model reading or
# judging it? A prompt line that starts with an exclamation mark is Claude Code's shell mode; the model does
# not choose that command. This probe sends such lines through the base into the agent makibad launches
# (the same launch as door-and-base-reach.sh), once with CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1 in the shim and
# once without it, and records what the screen shows. It decides nothing; the results go into Issue 150.
#
# What the documentation states (quoted; read 2026-11-20):
#  https://code.claude.com/docs/en/sandboxing, "Turn off the retry with strict sandbox mode":
#   "Strict sandbox mode applies to the commands Claude runs. Commands you type yourself at the `!`
#    shell-mode prompt run outside the sandbox unless the session is one of these:"
#   "A background session: strict sandbox mode covers shell-mode commands too"
#   "A Linux session with `CLAUDE_CODE_SUBPROCESS_ENV_SCRUB` set: every command runs sandboxed, shell-mode
#    commands included"
#  same page, "What runs outside the sandbox": "Commands you type yourself: a command you enter at the `!`
#   shell-mode prompt runs unsandboxed in most sessions."
#  same page, "Protected paths": "Inside the directories that sandboxed commands can write to, the sandbox still
#   denies writes to the files Claude Code loads configuration and code from." (the .claude settings files,
#   .claude/hooks and .mcp.json in the working directory and above are named; the control write below is
#   therefore a plain file next to them, not one of them)
#  https://code.claude.com/docs/en/interactive-mode, "Shell mode with `!` prefix":
#   "Run shell commands directly without going through Claude by prefixing your input with `!`"
#   "Doesn't require Claude to interpret or approve the command"
#   "Pasting text that starts with `!` into an empty prompt enters shell mode automatically, matching typed `!`
#    behavior"
#   "Unless your session is one of those listed under strict sandbox mode, commands you type in shell mode run
#    outside the sandbox even when you've enabled sandboxing, because the sandbox applies to the commands
#    Claude runs."
#   "Claude responds to the command output automatically once it lands in the transcript ... To add the output
#    to context without a response instead, set `respondToBashCommands` to `false` in `settings.json`."
#  What the variable does (env-vars page, read through a summary, not word for word): it removes credentials from
#   the environments of the subprocesses Claude Code starts (Bash commands, hooks, stdio MCP servers), by the
#   variable's name or by the shape of its value.
# Not stated in the two pages: whether permission rules or PreToolUse hooks apply to a shell-mode command (the
# hook lines case below only records what the screen shows), and the default value of respondToBashCommands.
# respondToBashCommands: this probe does NOT set it (the settings file is makibad's and is not edited), so the
# default applies, which the page above describes as the model responding to the output. Whether it answers is
# recorded (OBSERVE model-answered-*). A model turn that follows a shell-mode line is the model reading the
# OUTPUT, not choosing the command; the cases below read the output block only, up to the first model line.
#
# Cases (A = launch with CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1 exported by the shim, B = launch without it):
#  shell-mode-taken            A: an echo line starting with "!" shows its output on the screen and the count of
#                              Bash tool-call lines did not grow (the model did not run it)
#  shell-read-denied-scrubbed  A: cat of a decoy in ~/.local/state/herdr-core, which makiba's settings deny
#                              (PASS = refused with a sandbox-style error)
#  shell-write-allowed-scrubbed A: an echo redirected into a plain file in the workplace, which the sandbox allows
#                              (PASS = the file holds the text when read from outside; so a refusal above is not a
#                              blanket failure)
#  safeguard-flags             both launches: lines of the model's safeguard flag on the full screens (PASS = 0;
#                              more is NEEDS-HUMAN with the lines, never a verdict on the sandbox)
# Recorded, not judged (OBSERVE): shell-mode-taken-unscrubbed, shell-read-unscrubbed and shell-write-unscrubbed
# (B: the same lines; the docs say shell mode then runs outside the sandbox, so the read is expected to work),
# variable-makes-the-difference (A refused and B read), model-answered-A/B (per line: did a model line follow
# the output), hook-lines-A/B (screen lines naming PreToolUse or hook, plus the change in state.db events rows),
# scrub-variable-in-claude-environ-A/B (from outside, the name and value of that one variable), and
# env-names (names only, never values: what the shell-mode subprocess environment of B has and A lacks, and the
# reverse).
# Commands are plain: echo, cat, env | cut. Nothing here builds code for the model to read; the model sees
# only the output of a command it did not choose.
# The output also goes to <result-dir>/shell-mode-reach-run.log as it is printed, so a run ended by timeout
# leaves it and still runs its cleanup (the tee ignores INT and TERM, as in door-and-base-reach.sh).
# The role hooks in the settings run <share>/makiba: when that entry point is missing the probe links it to
# MAKIBA_BIN and removes only what it made (the same as door-and-base-reach.sh).
# Run from a login shell: MAKIBA_BIN=/path/to/makiba bash scripts/probes/shell-mode-reach.sh <result-dir>
set -u
. "$(dirname "$0")/lib.sh"
case "${1:-}" in -h|--help) probe_usage "$0" "Issue 150: do shell-mode lines run sandboxed without the model (env MAKIBA_BIN)"; exit 0 ;; esac
MAKIBA_BIN=${MAKIBA_BIN:-makiba}
type -P "$MAKIBA_BIN" >/dev/null 2>&1 || { echo "RESULT shell-mode-reach: precondition-failed (MAKIBA_BIN=$MAKIBA_BIN not found; build with cargo build and point MAKIBA_BIN at target/debug/makiba)"; exit 2; }
MAKIBA_BIN=$(type -P "$MAKIBA_BIN")
# Absolute: the entry point below is a link to it, and a relative target would resolve from <share>.
case "$MAKIBA_BIN" in /*) ;; *) MAKIBA_BIN=$PWD/$MAKIBA_BIN ;; esac
probe_init shell-mode-reach "${1:-}"
RUNLOG=$OUTDIR/$PROBE-run.log
cat "$RESULT" > "$RUNLOG"
exec > >(trap '' INT TERM; exec tee -a "$RUNLOG") 2>&1
say "MAKIBA_BIN: $MAKIBA_BIN (built from: ${MAKIBA_COMMIT:-not given; set MAKIBA_COMMIT to the commit it was built from})"

SEC=smr-secretary
MK=$WORK/mk; STATE=$MK/state; DOCS=$MK/docs; DB=$STATE/state.db
CORESOCK=$XDG_RUNTIME_DIR/herdr-core/$SESSION.sock   # herdr::socket_path
PKDIR=$WORK/elsewhere; PKEY=$PKDIR/decoy-$$.txt
DECOY=DECOY-NOT-A-SECRET-$$
DECOYPATH=$HOME/.local/state/herdr-core/makiba-probe-decoy-$$.txt
mkdir -p "$STATE" "$DOCS" || exit 2
( umask 077; mkdir -p "$PKDIR" && printf '%s\n' "DECOY-PERSON-$$" > "$PKEY" ) || exit 2

# Decoys in the base's folders as makiba denies them: only these files are written there, and only they
# (and a folder made for them) are removed.
MADE_DIRS=""; DECOYS=""
for d in "$HOME/.local/state/herdr-core" "$HOME/.config/herdr-core"; do
  [ -d "$d" ] || { mkdir -p "$d" && MADE_DIRS="$MADE_DIRS $d"; }
  printf '%s\n' "$DECOY" > "$d/makiba-probe-decoy-$$.txt" && DECOYS="$DECOYS $d/makiba-probe-decoy-$$.txt"
done
MKPID=""
extra_cleanup() {
  [ -n "$MKPID" ] && { kill -TERM "$MKPID" 2>/dev/null; wait "$MKPID" 2>/dev/null; }
  for f in $DECOYS; do rm -f "$f"; done
  for d in $MADE_DIRS; do rmdir "$d" 2>/dev/null; done
  [ -n "$EP_MADE" ] && [ "$(readlink "$EP_MADE" 2>/dev/null)" = "$MAKIBA_BIN" ] && rm -f "$EP_MADE"
  if [ -n "$EPDIR_MADE" ]; then   # from the entry point's folder up to the topmost one made, each only if empty
    local d=$EPDIR
    while rmdir "$d" 2>/dev/null && [ "$d" != "$EPDIR_MADE" ]; do d=$(dirname "$d"); done
  fi
  return 0
}
# The hooks' entry point, as the role settings name it: made only when missing, removed only if made here.
EPDIR=${MAKIBA_SHARE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/makiba}; EP=$EPDIR/makiba
EP_MADE=""; EPDIR_MADE=""
if [ -e "$EP" ] || [ -L "$EP" ]; then say "hooks' entry point: $EP exists, left as it is (-> $(readlink "$EP" 2>/dev/null || echo 'not a link'))"
else
  d=$EPDIR; while [ ! -d "$d" ]; do EPDIR_MADE=$d; d=$(dirname "$d"); done   # the topmost folder made here
  [ -z "$EPDIR_MADE" ] || mkdir -p "$EPDIR"
  ln -s "$MAKIBA_BIN" "$EP" && EP_MADE=$EP && say "hooks' entry point: linked $EP -> $MAKIBA_BIN (removed at the end)"
fi

# The core is started by a stand-in terminal that only sleeps (the reach probe's stand-in secretary, without
# its decoy key and token: nothing here looks for them).
TERMS="$TERMS $SEC"
hc run --bg --no-parent --name "$SEC" --cwd "$WORK" -- python3 -c 'import time;time.sleep(3600)' >/dev/null 2>&1 \
  || { say "RESULT shell-mode-reach: precondition-failed (hc run failed for the stand-in)"; exit 2; }
i=0; while [ ! -S "$CORESOCK" ] && [ "$i" -lt 20 ]; do sleep 1; i=$((i + 1)); done
[ -S "$CORESOCK" ] || { say "RESULT shell-mode-reach: precondition-failed (no core socket at $CORESOCK)"; exit 2; }
say "core socket: $CORESOCK"

# The shim: the program the `fake` module launches. It keeps makibad's --mcp-config and adds the settings of the
# task named in shim.env, which the probe rewrites before each launch: that file is where the variable is set
# (or not), so the two launches differ in nothing else.
SHIM=$WORK/claude-shim.sh
cat > "$SHIM" <<SH
#!/usr/bin/env bash
. '$WORK/shim.env'
cfg=""; while [ "\$#" -gt 0 ]; do case "\$1" in --mcp-config) cfg=\$2; shift 2 ;; *) shift ;; esac; done
exec '$(type -P claude)' --mcp-config "\$cfg" --settings '$STATE/settings'/"\$SHIMTASK.json" --setting-sources project,local --permission-mode acceptEdits --allowedTools 'Bash(bash:*)'
SH
chmod +x "$SHIM"
: > "$WORK/shim.env"

env MAKIBA_STATE_DIR="$STATE" MAKIBA_DOCS_DIR="$DOCS" MAKIBA_WORKERS_DIR="$MK/workers" MAKIBA_REPOS_DIR="$MK/repos" \
  MAKIBA_PERSON_KEY="$PKEY" MAKIBA_DOOR=none MAKIBA_PORT=0 MAKIBA_BASE_SESSION="$SESSION" MAKIBA_VERIFY_TIMEOUT_MS=3600000 \
  MAKIBA_FAKE_AGENT="$(python3 -c 'import json,sys;print(json.dumps(sys.argv[1:]))' "$SHIM" "$WORK/fake-record")" \
  "$MAKIBA_BIN" daemon > "$WORK/makibad.log" 2>&1 &
MKPID=$!
sql() { python3 - "$DB" "$@" <<'PY'
import sqlite3, sys
c = sqlite3.connect(sys.argv[1], timeout=10)
try:
    r = c.execute(sys.argv[2], sys.argv[3:]).fetchall(); c.commit()
    print(r[0][0] if r and r[0] else "")
except sqlite3.Error as e:
    print("ERR %s" % e)
PY
}
i=0; while [ "$(sql 'SELECT subscribed FROM dispatcher WHERE id = 1' 2>/dev/null)" != 1 ] && [ "$i" -lt 30 ]; do
  kill -0 "$MKPID" 2>/dev/null || break; sleep 1; i=$((i + 1))
done
[ "$(sql 'SELECT subscribed FROM dispatcher WHERE id = 1')" = 1 ] || { say "RESULT shell-mode-reach: precondition-failed (makibad did not subscribe; log below)"; cat "$WORK/makibad.log" >> "$RESULT"; exit 2; }
say "makibad: pid $MKPID, subscribed to $SESSION"

# One launch: a queued document run as the door leaves it (spec section 5.2); sets T WP SETTINGS CLPID.
# Returns 1 when the agent did not come up (the reason is said).
find_claude() { python3 - "$SETTINGS" <<'PY'
import os, sys
want = sys.argv[1].encode()
for p in os.listdir("/proc"):
    if not p.isdigit():
        continue
    try:
        argv = open("/proc/%s/cmdline" % p, "rb").read().split(b"\0")
    except OSError:
        continue
    if want in argv and b"--mcp-config" in argv:
        print(p)
        break
else:
    print("none")
PY
}
launch_run() { # launch_run <task> <scrub 0|1>
  TASK=$1; T=w-$TASK; WP=$DOCS/$TASK; SETTINGS=$STATE/settings/$TASK.json
  local spawn=spawn-$TASK-$$ st="" i=0
  { echo "SHIMTASK=$TASK"; if [ "$2" = 1 ]; then echo "export CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=1"; else echo "unset CLAUDE_CODE_SUBPROCESS_ENV_SCRUB"; fi; } > "$WORK/shim.env"   # B unsets it: the invoking shell may export it
  sql "INSERT INTO projects (slug, display_name) VALUES ('probe', 'probe')" >/dev/null
  sql "INSERT INTO runs (task_id, project_id, pattern, title, status, verification) VALUES (?, (SELECT id FROM projects WHERE slug = 'probe'), 'B', ?, 'queued', 'standard')" "$TASK" "$TASK" >/dev/null
  sql "INSERT INTO run_launch (run_id, spawn_id, kind, agent, brief, worktree) VALUES ((SELECT id FROM runs WHERE task_id = ?), ?, 'document', 'fake', 'probe', ?)" "$TASK" "$spawn" "$WP" >/dev/null
  TERMS="$TERMS $T"
  while [ "$i" -lt 60 ]; do
    st=$(sql "SELECT stage FROM run_launch WHERE spawn_id = ?" "$spawn")
    case "$st" in launched|dialog_passed|verified|failed) break ;; esac; sleep 1; i=$((i + 1))
  done
  say "launch $TASK (scrub=$2): stage ${st:-none}"
  [ -s "$SETTINGS" ] || { say "OBSERVE launch $TASK: no settings file at $SETTINGS"; return 1; }
  [ "$st" = failed ] && { say "OBSERVE launch $TASK: failed: $(sql 'SELECT failed_reason FROM run_launch WHERE spawn_id = ?' "$spawn")"; return 1; }
  CLPID=none; i=0; while [ "$CLPID" = none ] && [ "$i" -lt 30 ]; do CLPID=$(find_claude); [ "$CLPID" = none ] && sleep 1; i=$((i + 1)); done
  [ "$CLPID" != none ] || { say "OBSERVE launch $TASK: no claude process with the settings in its arguments"; screen_dump "$T" "launch $TASK"; return 1; }
  # Startup dialogs are makibad's to pass; only if one is still up after 90 s does the probe pass it.
  i=0; while [ "$i" -lt 90 ]; do is_ready "$T" && break; i=$((i + 2)); sleep 2; done
  if ! is_ready "$T"; then
    say "OBSERVE makibad-passed-dialogs $TASK: no (a dialog or no prompt after 90 s; the probe passes it now)"; screen_dump "$T" "dialogs $TASK"
    pass_dialogs "$T" 60 || true
    wait_ready "$T" 60 || { say "OBSERVE launch $TASK: the agent did not reach an empty prompt"; screen_dump "$T" "launch $TASK"; return 1; }
  else say "OBSERVE makibad-passed-dialogs $TASK: yes"; fi
  say "claude pid $CLPID for $TASK"
}

# Reads the output block of one shell-mode line off the screen: from the echoed line to the first model line
# (a bullet) or the composer, and says whether a model line followed and whether it was a Bash tool call.
cat > "$WORK/block.py" <<'PY'
import re, sys
lines = open(sys.argv[1], errors="replace").read().split("\n")
needle = sys.argv[2]
hit = [i for i, l in enumerate(lines) if needle in l]
echo = [i for i in hit if re.match(r"^\W*!", lines[i])]
pick = echo or hit
if not pick:
    print("META found=0 responded=0 tool=0")
    sys.exit(0)
BUL = ("\u25cf", "\u23fa")
STOP = ("\u2500", "\u256d", "\u276f")
block, resp, tool, after = [], 0, 0, False
for l in lines[pick[-1] + 1:]:
    s = l.strip()
    if s[:1] in BUL:
        resp, after = 1, True
    elif s[:1] in STOP:
        break
    if after:
        if "Bash(" in s:
            tool = 1
    else:
        block.append(l)
print("META found=%d responded=%d tool=%d" % (1 if echo else 2, resp, tool))
print("\n".join(block))
PY
DENYRE='denied by|deny rule|permission settings|has been denied|Permission to use|not allowed'
SBXRE='Operation not permitted|Permission denied|Read-only file system|No such file|Connection refused|not found|unavailable|cannot connect|Failed to connect|sandbox|errno'
ANSWERED=""   # "<launch>-<line>=<0|1>" per line
S_PROMPTS=""
shell_line() { # shell_line <label> <needle> <line>: sends the line; sets S_FOUND S_RESP S_TOOL S_DELTA and S_BLOCK (file)
  local lab=$1 i=0 settled=0 t0 t1
  wait_idle "$T" 60 || say "OBSERVE $lab: the agent was not idle before the send"
  t0=$(hc read --lines 3000 "$T" 2>/dev/null | grep -c 'Bash(')
  hc send "$T" "$3" >/dev/null 2>&1 || { say "OBSERVE $lab: hc send failed"; S_FOUND=0 S_RESP=0 S_TOOL=0 S_DELTA=0; S_BLOCK=$WORK/block-$lab.txt; : > "$S_BLOCK"; return 1; }
  sleep 8
  while [ "$i" -lt 120 ] && [ "$settled" -lt 2 ]; do   # idle twice in a row: a model answer that is slow to start is not missed
    case "$(state_of "$T")" in
      blocked) S_PROMPTS="$S_PROMPTS $lab"; screen_dump "$T" "permission prompt after $lab"; hc keys "$T" escape >/dev/null 2>&1; settled=0 ;;
      idle|done) settled=$((settled + 1)) ;;
      *) settled=0 ;;
    esac
    i=$((i + 4)); sleep 4
  done
  t1=$(hc read --lines 3000 "$T" 2>/dev/null | grep -c 'Bash(')
  hc read --lines 3000 "$T" > "$WORK/screen-$lab.txt" 2>&1
  python3 "$WORK/block.py" "$WORK/screen-$lab.txt" "$2" > "$WORK/blockraw-$lab.txt"
  S_FOUND=$(sed -n '1s/.*found=\([0-9]\).*/\1/p' "$WORK/blockraw-$lab.txt"); S_FOUND=${S_FOUND:-0}
  S_RESP=$(sed -n '1s/.*responded=\([0-9]\).*/\1/p' "$WORK/blockraw-$lab.txt"); S_RESP=${S_RESP:-0}
  S_TOOL=$(sed -n '1s/.*tool=\([0-9]\).*/\1/p' "$WORK/blockraw-$lab.txt"); S_TOOL=${S_TOOL:-0}
  S_DELTA=$((t1 - t0)); S_BLOCK=$WORK/block-$lab.txt
  tail -n +2 "$WORK/blockraw-$lab.txt" > "$S_BLOCK"
  ANSWERED="$ANSWERED $lab=$S_RESP"
  say "OBSERVE $lab: found=$S_FOUND model-line-after=$S_RESP bash-tool-lines-after-send=$S_DELTA output: $(head -c 240 "$S_BLOCK" | tr '\n' ' ')"
}
excerpt() { head -c 240 "$1" | tr '\n' ' '; }

# The four lines of one launch (k = A or B); the verdicts are drawn here, per launch.
RES_TAKEN=""; RES_READ=""; RES_WRITE=""
run_lines() { # run_lines <A|B>
  local k=$1 ev0 ev1 wf=$WP/shell-write-$1.txt
  ev0=$(sql 'SELECT count(*) FROM events')
  shell_line "$k-echo" 'SHELLMODE-$((' '!echo SHELLMODE-$((6*7))'
  if grep -qF 'SHELLMODE-42' "$S_BLOCK" && [ "$S_DELTA" -le 0 ]; then RES_TAKEN=yes
  elif [ "$S_DELTA" -gt 0 ]; then RES_TAKEN=model-ran-it
  else RES_TAKEN=unclear; fi
  if [ "$k" = A ]; then
    case "$RES_TAKEN" in
      yes) record shell-mode-taken PASS "the output shows on the screen and the model ran no Bash tool call: $(excerpt "$S_BLOCK")" ;;
      model-ran-it) record shell-mode-taken FAIL "Bash tool-call lines grew by $S_DELTA: the line was taken as a prompt to the model, or the model acted after it" ;;
      *) record shell-mode-taken NEEDS-HUMAN "no output block with SHELLMODE-42 (found=$S_FOUND); read the screen"; screen_dump "$T" shell-mode-taken ;;
    esac
  else say "OBSERVE shell-mode-taken-unscrubbed: $RES_TAKEN"; fi

  shell_line "$k-read" 'cat /' "!cat $DECOYPATH"
  if grep -qF "$DECOY" "$S_BLOCK"; then RES_READ=read
  elif [ "$S_FOUND" = 0 ] || ! grep -q '[^[:space:]]' "$S_BLOCK"; then RES_READ=unclear
  elif grep -Eqi "$DENYRE" "$S_BLOCK"; then RES_READ=deny-rule
  elif grep -Eqi "$SBXRE" "$S_BLOCK"; then RES_READ=refused
  else RES_READ=unclear; fi
  if [ "$k" = A ]; then
    case "$RES_READ" in
      refused) record shell-read-denied-scrubbed PASS "refused with a sandbox-style error: $(excerpt "$S_BLOCK")" ;;
      read) record shell-read-denied-scrubbed FAIL "the decoy was read: the shell-mode line ran outside the sandbox: $(excerpt "$S_BLOCK")" ;;
      deny-rule) record shell-read-denied-scrubbed FAIL "refused by a deny rule, not by the sandbox: $(excerpt "$S_BLOCK")" ;;
      *) record shell-read-denied-scrubbed NEEDS-HUMAN "no recognisable output block (found=$S_FOUND): $(excerpt "$S_BLOCK")"; screen_dump "$T" shell-read ;;
    esac
  else say "OBSERVE shell-read-unscrubbed: $RES_READ (read = the decoy came through): $(excerpt "$S_BLOCK")"; fi

  shell_line "$k-write" 'WRITTEN-$((' "!echo WRITTEN-\$((6*7)) > $wf"
  if [ "$(cat "$wf" 2>/dev/null)" = WRITTEN-42 ]; then RES_WRITE=written
  elif grep -q '[^[:space:]]' "$S_BLOCK"; then RES_WRITE=refused
  else RES_WRITE=unclear; fi
  if [ "$k" = A ]; then
    case "$RES_WRITE" in
      written) record shell-write-allowed-scrubbed PASS "the file in the workplace holds the text when read from outside" ;;
      refused) record shell-write-allowed-scrubbed FAIL "no file, and the screen shows an error (a blanket refusal?): $(excerpt "$S_BLOCK")" ;;
      *) record shell-write-allowed-scrubbed NEEDS-HUMAN "no file and no output block (found=$S_FOUND): the line may not have run"; screen_dump "$T" shell-write ;;
    esac
  else say "OBSERVE shell-write-unscrubbed: $RES_WRITE"; fi

  shell_line "$k-env" 'env | cut' "!env | cut -d= -f1 > $WP/envnames-$k.txt"   # names only: the values never leave the command
  ev1=$(sql 'SELECT count(*) FROM events')
  say "OBSERVE events-rows-$k: $ev0 before the four lines, $ev1 after (what the role hooks write there, if any, is not known from here)"
  # What the whole history shows of the model's safeguard flag and of hooks.
  hc read --lines 3000 "$T" > "$WORK/full-$k.txt" 2>&1
  local nflag nhook
  nflag=$(grep -Eci 'details: \[|\[cyber\]|safeguards|usage policy' "$WORK/full-$k.txt")
  nhook=$(grep -Eci 'PreToolUse|hook' "$WORK/full-$k.txt")
  cp -f "$WORK/full-$k.txt" "$OUTDIR/$PROBE-screen-$k.txt"   # the scratch folder is removed at the end
  [ "$nflag" -eq 0 ] || { echo "---- safeguard-flag lines of $k"; grep -Ei 'details: \[|\[cyber\]|safeguards|usage policy' "$WORK/full-$k.txt" | head -n 10 | cut -c1-200; echo "----"; } >> "$RESULT"
  FLAGS="$FLAGS $k=$nflag"; FLAGTOTAL=$((FLAGTOTAL + nflag))
  say "OBSERVE hook-lines-$k: $nhook screen line(s) name PreToolUse or hook; first: $(grep -Ei 'PreToolUse|hook' "$WORK/full-$k.txt" | head -n 3 | cut -c1-160 | tr '\n' '|')"
  say "OBSERVE model-answered-$k:$(printf '%s' "$ANSWERED" | tr ' ' '\n' | grep "^$k-" | tr '\n' ' ') (1 = a model line followed the output; respondToBashCommands was not set)"
  [ -z "$S_PROMPTS" ] || say "OBSERVE permission-prompts-after-lines:$S_PROMPTS (declined with Escape, screens above)"
  local envline; envline=$(tr '\0' '\n' < "/proc/$CLPID/environ" 2>/dev/null | grep '^CLAUDE_CODE_SUBPROCESS_ENV_SCRUB=' | head -n 1)
  say "OBSERVE scrub-variable-in-claude-environ-$k: ${envline:-not set (or /proc/$CLPID/environ not readable)}"
  tr '\0' '\n' < "/proc/$CLPID/environ" 2>/dev/null | cut -d= -f1 | sort -u > "$WORK/claude-environ-names-$k.txt"
}

FLAGS=""; FLAGTOTAL=0
if launch_run probeA 1; then run_lines A; A_TAKEN=$RES_TAKEN; A_READ=$RES_READ; A_UP=1
else A_TAKEN=none; A_READ=none; A_UP=0
  for c in shell-mode-taken shell-read-denied-scrubbed shell-write-allowed-scrubbed; do record "$c" NEEDS-HUMAN "the launch with the variable did not come up (see above)"; done
fi
hc kill "$T" >/dev/null 2>&1; hc rm "$T" >/dev/null 2>&1   # the second launch is a fresh agent
RES_TAKEN=none; RES_READ=none
if launch_run probeB 0; then run_lines B; B_UP=1
else say "OBSERVE launch B: did not come up, so the pair below has no unscrubbed half (does makibad take a second queued run while the first terminal is gone? see makibad.log in the result file)"; B_UP=0; fi
B_READ=$RES_READ

# The pair, from both halves.
if [ "$A_UP" = 1 ] && [ "$B_UP" = 1 ]; then
  if [ "$A_READ" = refused ] && [ "$B_READ" = read ]; then say "OBSERVE variable-makes-the-difference: yes (decoy refused with the variable, read without it)"
  else say "OBSERVE variable-makes-the-difference: no or unclear (with the variable: $A_READ, without: $B_READ)"; fi
else say "OBSERVE variable-makes-the-difference: not measured (a launch did not come up)"; fi
if [ -s "$DOCS/probeA/envnames-A.txt" ] && [ -s "$DOCS/probeB/envnames-B.txt" ]; then
  say "OBSERVE env-names lacking under the variable (in B's shell-mode subprocess environment, not in A's; names only): $(comm -13 <(sort -u "$DOCS/probeA/envnames-A.txt") <(sort -u "$DOCS/probeB/envnames-B.txt") | tr '\n' ' ')"
  say "OBSERVE env-names only under the variable (in A, not in B; names only): $(comm -23 <(sort -u "$DOCS/probeA/envnames-A.txt") <(sort -u "$DOCS/probeB/envnames-B.txt") | tr '\n' ' ')"
else say "OBSERVE env-names: a names file is missing (envnames-A.txt or envnames-B.txt); the env lines did not run in both launches"; fi
[ -s "$WORK/claude-environ-names-A.txt" ] && [ -s "$WORK/claude-environ-names-B.txt" ] \
  && say "OBSERVE claude-process-environ-names-only-in-B (from outside, names only): $(comm -13 "$WORK/claude-environ-names-A.txt" "$WORK/claude-environ-names-B.txt" | tr '\n' ' ')"
if [ "$FLAGTOTAL" -eq 0 ]; then record safeguard-flags PASS "0 safeguard-flag lines on the screens ($FLAGS )"
else record safeguard-flags NEEDS-HUMAN "$FLAGTOTAL safeguard-flag line(s) ($FLAGS ): the run is not a verdict on the sandbox; read the screens"; fi
say "---- makibad.log"; cat "$WORK/makibad.log" >> "$RESULT"
finish
