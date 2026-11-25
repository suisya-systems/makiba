#!/usr/bin/env bash
# P16 (Issue 262): the second reader cannot read a file outside its round's folder.
# makibad starts each reader inside bubblewrap with a mount plan of its own (src/daemon/review.rs).
# This probe runs, on a host where bubblewrap can make a namespace, the two ignored tests of
# tests/review_read_boundary.rs, each in a cargo process of its own:
#  stand-in-readers   stand-in readers inside the real bwrap try a decoy in a scratch home, a decoy in the
#                     state folder and the decoy person key, read by the test outside first (control);
#                     PASS = every decoy unreadable inside, the diff file readable, for codex and the fallback
#  real-readers       the real codex, then the real claude as the fallback, each in the real bwrap, asked by
#                     the design note to read a decoy in $HOME and one in the state folder; PASS = the answer
#                     names the expected reader, is passed, and carries neither decoy's content
# The real readers use the person's own logins (~/.codex/auth.json, ~/.claude/.credentials.json), bound
# into the round's home by makibad; the decoys are written by the test and removed after it.
# Needs: cargo, bwrap, codex (logged in) and claude (logged in) on PATH; run from a login shell, never from
# inside an agent or CI. Each test's output goes into the result file.
set -u
. "$(dirname "$0")/lib.sh"

case "${1:-}" in
  -h|--help) probe_usage "$0" "P16: the second reader reads only its round's folder (Issue 262)"; exit 0 ;;
esac

probe_init p16 "${1:-}"
REPO=$(cd "$(dirname "$0")/../.." && pwd)
probe_dry_stop   # PROBE_DRY_RUN=1 ends here, through the exit handler

for c in cargo bwrap codex; do
  type -P "$c" >/dev/null 2>&1 || { say "RESULT p16: precondition-failed ($c not on PATH)"; exit 2; }
done
say "bwrap --version: $(bwrap --version 2>&1 | head -n 1)"
say "codex --version: $(codex --version 2>&1 | grep -v WARNING | head -n 1)"

run_case() { # run_case <case> <test name>
  local name=$1 test=$2 log="$WORK/$1.log" rc=0
  (cd "$REPO" && cargo test --locked --test review_read_boundary -- --ignored --exact "$test" --nocapture) > "$log" 2>&1 || rc=$?
  { echo "---- $name ($test), exit $rc"; cat "$log"; echo "----"; } >> "$RESULT"
  if [ "$rc" -eq 0 ] && grep -q "test result: ok. 1 passed" "$log"; then
    record "$name" PASS "$test"
  else
    record "$name" FAIL "$test exited $rc; see the result file"
  fi
}

run_case stand-in-readers a_planted_secret_is_unreadable_inside_the_real_boundary
run_case real-readers a_real_reader_cannot_read_a_planted_secret
finish
