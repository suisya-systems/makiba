# shellcheck shell=bash
# Shared helpers of the runbook scripts (docs/runbooks/self-hosting.md). Sourced, not run.
# Conventions of scripts/probes/lib.sh: say prints and appends to the results file, a refusal
# names the missing precondition and how to fix it, a dry run prints the commands and runs none.

DRY=0           # 1: print what would run, run nothing, write no file
RESULT=""       # the results file the secretary reads (empty in a dry run)
FAILS=0         # CHECK lines that failed

say() { printf '%s\n' "$*"; if [ -n "$RESULT" ]; then printf '%s\n' "$*" >> "$RESULT"; fi; }

# refuse <what is missing> <how to fix it>: the script stops, nothing has been changed by this step.
refuse() {
  say "REFUSED: $1"
  say "  fix: $2"
  say "RESULT ${STAGE_NAME:-runbook}: refused"
  exit 3
}

# check <name> <PASS|FAIL|INFO> <detail>: one line of the results (the rows of runbook section 10).
check() {
  say "CHECK $1: $2  $3"
  if [ "$2" = FAIL ]; then FAILS=$((FAILS + 1)); fi
  return 0
}

# run <command...>: prints the command; runs it unless this is a dry run.
run() {
  say "+ $*"
  if [ "$DRY" = 1 ]; then return 0; fi
  "$@"
}

# plan <text>: a line of what a dry run would do where the next step depends on a command's output.
plan() { if [ "$DRY" = 1 ]; then say "  (dry run) would: $*"; fi; return 0; }

have() { command -v "$1" >/dev/null 2>&1; }

# The state database of the organization this shell would talk to, read only, one query per call,
# the columns joined by "|" and a NULL as empty. python3's sqlite3 module; no sqlite3 program.
db_path() { printf '%s/state.db' "${MAKIBA_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/makiba}"; }
sq() {
  python3 -I -c 'import sqlite3, sys
c = sqlite3.connect("file:" + sys.argv[1] + "?mode=ro", uri=True)
for r in c.execute(sys.argv[2]):
    print("|".join("" if v is None else str(v) for v in r))' "$(db_path)" "$1"
}

# wait_for <seconds> <what> <command...>: polls every 5 s until the command succeeds; 1 on timeout.
wait_for() {
  local limit=$1 what=$2 waited=0; shift 2
  say "  waiting for: $what (up to $limit s)"
  while ! "$@" >/dev/null 2>&1; do
    if [ "$waited" -ge "$limit" ]; then say "  timed out after $limit s: $what"; return 1; fi
    sleep 5; waited=$((waited + 5))
  done
  say "  done after $waited s: $what"
}

# confirm: the irreversible stages ask once. --yes skips it; no terminal and no --yes refuses.
confirm() {
  if [ "$DRY" = 1 ] || [ "${ASSUME_YES:-0}" = 1 ]; then return 0; fi
  [ -t 0 ] || refuse "this stage changes the real organization and asks to be confirmed, and there is no terminal" "run it from your own terminal, or add --yes"
  printf 'Type yes to go on: '
  local a; read -r a
  [ "$a" = yes ] || refuse "not confirmed" "run the line again and type yes"
}
