#!/usr/bin/env bash
# acceptance-check: says whether the tests a branch declares failed at its test commit and pass at
# head. Carried from paddock (scripts/acceptance-check.sh); declared tests run with pytest.
# Run from the repository root on full history:
#   bash scripts/acceptance-check.sh BASE_SHA HEAD_SHA
# The branch's own commits are HEAD's first-parent line not reachable from BASE; the oldest of
# them (whose parent is then in BASE) may declare tests with "Acceptance-Test: <path>" trailers.
# The base may be merged in after it (no force push). When it declares:
#   - it is the only commit that declares, and it changes only declared paths and test helpers;
#   - every declared path is a test target or a test helper (a file under tests/ that is not a
#     target: protected, not run); at least one is a test target; every target fails at that commit and passes at HEAD. A Cargo
#     target is tests/<name>.rs or tests/<name>/ holding main.rs (or that main.rs), run as
#     `cargo test --locked --test <name>`; a Python test is tests/**/test_*.py (a directory: every
#     test_*.py under it), run as `python3 -m pytest <path>`. See docs/acceptance-check.md;
#   - the test commit is not a merge, and the branch's own commits after it leave the declared
#     paths, test helpers (files under tests/ that are not test targets), .github/ and this
#     script unchanged (what a merge brings from its other parents does not count).
# Last line: ACCEPTANCE: pass | none | fail. Exit 0 pass or none (nothing declared), 1 fail, 2 usage.
#   bash scripts/acceptance-check.sh --test-commit SHA
# prints only the paths the test commit SHA may not change (rule 2), one per line, sorted: exit 0
# none, 1 some, 2 usage. The merge gate in makibad runs this mode, so both judge by one rule.
set -u
# The declared paths, each trimmed and with empty and "." segments dropped, as the merge gate reads
# them (src/daemon/gate.rs, find_manifest): "./tests/a.rs" and "tests//a.rs" declare tests/a.rs.
declared() {
  git log -1 --format='%(trailers:key=Acceptance-Test,valueonly)' "$1" |
    sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e 's#//*#/#g' -e ':a' -e 's#\(^\|/\)\./#\1#' -e 'ta' \
      -e 's#/\.$##' -e 's#^\.$##' -e '/^$/d'
}
under() { # under <file> <path>: the file is the path or inside it
  local p=${2%/}; [ "$1" = "$p" ] || case $1 in "$p"/*) true ;; *) false ;; esac
}
rs_name() { # rs_name <file>: the Cargo target name of tests/<name>.rs or tests/<name>/main.rs
  local r=${1#tests/}
  [ "$r" != "$1" ] || return 1
  case $r in */*/*) return 1 ;; */main.rs) echo "${r%/main.rs}" ;; */*) return 1 ;; *.rs) echo "${r%.rs}" ;; *) return 1 ;; esac
}
is_py() { case $1 in tests/*) case ${1##*/} in test_*.py) true ;; *) false ;; esac ;; *) false ;; esac; }
is_helper() { case $1 in tests/*) ! is_py "$1" && ! rs_name "$1" >/dev/null ;; *) false ;; esac; }
is_declared() { local p; while IFS= read -r p; do under "$1" "$p" && return 0; done <<<"$paths"; return 1; }
# The one rule of what a test commit may change (Issue 361): a path under a path the commit
# declares, or a test helper. Prints every other path the commit changes, one per line. The merge
# gate (src/daemon/gate.rs, test_commit_refusals) runs it through --test-commit.
refused_in_test_commit() { # refused_in_test_commit <sha>
  local paths f
  paths=$(declared "$1")
  # NUL-delimited: git would quote a name with non-ASCII bytes, a tab or a quote, and the quoted
  # form would match no declared path and no helper.
  while IFS= read -r -d '' f; do
    [ -n "$f" ] || continue
    is_declared "$f" || is_helper "$f" || printf '%s\n' "$f"
  done < <(git diff --no-renames --name-only -z "$1^" "$1")
}
if [ "${1:-}" = --test-commit ]; then
  [ $# -eq 2 ] || { echo "usage: acceptance-check.sh --test-commit SHA" >&2; exit 2; }
  sha=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown commit $2" >&2; exit 2; }
  git rev-parse --verify -q "$sha^" >/dev/null || { echo "acceptance-check: commit $2 has no parent" >&2; exit 2; }
  refused=$(refused_in_test_commit "$sha" | LC_ALL=C sort -u)
  [ -n "$refused" ] || exit 0
  printf '%s\n' "$refused"; exit 1
fi
[ $# -eq 2 ] || { echo "usage: acceptance-check.sh BASE_SHA HEAD_SHA | --test-commit SHA" >&2; exit 2; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "acceptance-check: not in a git repository" >&2; exit 2; }
head=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown head $2" >&2; exit 2; }
mb=$(git merge-base "$1" "$head" 2>/dev/null) || { echo "acceptance-check: no merge base of $1 and $2" >&2; exit 2; }

bad=0
no() { echo "acceptance-check: $*"; bad=1; }

commits=$(git rev-list --reverse --first-parent "$head" --not "$1")
first=$(printf '%s\n' "$commits" | head -1)
[ -n "$first" ] && paths=$(declared "$first") || paths=
for c in $(printf '%s\n' "$commits" | tail -n +2); do
  [ -z "$(declared "$c")" ] || no "commit $c declares Acceptance-Test but is not the first commit of the branch"
done
if [ -z "$paths" ]; then
  [ "$bad" -eq 0 ] || { echo "ACCEPTANCE: fail"; exit 1; }
  echo "acceptance-check: no Acceptance-Test trailer on the first commit; nothing to check"
  echo "ACCEPTANCE: none"; exit 0
fi
echo "acceptance-check: test commit $first declares: $(echo $paths)"
[ "$(git rev-list --parents -n 1 "$first" | wc -w)" -eq 2 ] || no "test commit $first is a merge; no join commit may come before or at the test commit"

# 1. the test commit changes only declared paths and test helpers
while IFS= read -r f; do
  no "test commit changes $f, which is neither declared nor a test helper"
done < <(refused_in_test_commit "$first")

# 2. protected paths unchanged by the branch's own commits after the test commit. A merge (of
# the base) counts only for files it leaves different from every parent: its own resolution.
# Names are read unquoted (-z), as in refused_in_test_commit, so a non-ASCII helper is still seen.
own_changes() {
  local c ps p
  for c in $(printf '%s\n' "$commits" | tail -n +2); do
    ps=$(git rev-list --parents -n 1 "$c" | cut -d' ' -f2-)
    if [ "$(echo $ps | wc -w)" -eq 1 ]; then
      git diff --no-renames --name-only -z "$c^" "$c" | tr '\0' '\n'
    else
      local f
      for p in $ps; do git diff --no-renames --name-only -z "$p" "$c" | tr '\0' '\n'; done | sort | uniq -c |
        while read -r n f; do [ "$n" -lt "$(echo $ps | wc -w)" ] || printf '%s\n' "$f"; done
    fi
  done | sed '/^$/d' | sort -u
}
# A merge may bring in only the base: a side branch's own commits are off the first-parent line.
for c in $(printf '%s\n' "$commits" | tail -n +2); do
  for p in $(git rev-list --parents -n 1 "$c" | cut -d' ' -f3-); do
    git merge-base --is-ancestor "$p" "$1" || no "merge $c brings in $p, which is not in the base"
  done
done
while IFS= read -r f; do
  [ -n "$f" ] || continue
  if is_declared "$f" || is_helper "$f" || under "$f" .github || [ "$f" = scripts/acceptance-check.sh ]; then
    no "$f changed after the test commit (declared tests, test helpers, .github/ and this script are protected)"
  fi
done < <(own_changes)

# 3. declared tests fail at the test commit and pass at head
work=$(mktemp -d)
trap 'git worktree remove --force "$work/t" >/dev/null 2>&1; git worktree remove --force "$work/h" >/dev/null 2>&1; rm -rf "$work"' EXIT
git worktree add -q --detach "$work/t" "$first" && git worktree add -q --detach "$work/h" "$head" \
  || { echo "acceptance-check: git worktree add failed" >&2; echo "ACCEPTANCE: fail"; exit 1; }
# Each test is "py <path>" or "rs <path> <target name>".
tests=
while IFS= read -r p; do
  p=${p%/}
  if [ -f "$work/t/$p" ]; then
    if n=$(rs_name "$p"); then tests+="rs $p $n"$'\n'
    elif is_py "$p"; then tests+="py $p"$'\n'
    elif is_helper "$p"; then echo "acceptance-check: $p is a declared test helper (protected, not run)"
    else no "declared path $p is not a test target"
    fi
  elif [ -d "$work/t/$p" ]; then
    found=
    n=$(rs_name "$p/main.rs") && [ -f "$work/t/$p/main.rs" ] && found+="rs $p $n"$'\n'
    found+=$(cd "$work/t" && find "$p" -type f -name 'test_*.py' | sort | while IFS= read -r f; do is_py "$f" && echo "py $f"; done)
    if [ -n "$(printf '%s' "$found" | sed '/^$/d')" ]; then tests+=$found$'\n'
    elif is_helper "$p"; then echo "acceptance-check: $p is a declared test helper (protected, not run)"
    else no "declared path $p is not a test target"
    fi
  else
    no "declared path $p does not exist at the test commit"
  fi
done <<<"$paths"
tests=$(printf '%s' "$tests" | sed '/^$/d')
[ -n "$tests" ] || no "no declared path is a test target; declare at least one that fails at the test commit"

# Each worktree builds into its own target/, whatever the environment or a cargo config file says:
# one folder shared by the two worktrees (or by two runs) lets cargo reuse a build made from the
# other's sources (Issue #232). Python tests get no target folder from the checker at all.
unset CARGO_TARGET_DIR CARGO_BUILD_TARGET_DIR CARGO_BUILD_BUILD_DIR
run_rs() { # run_rs <tree> <name> <where>: output in $work/out, Cargo's exit status returned
  echo "acceptance-check: building $2 at $3 in target folder $1/target"
  (cd "$1" && export CARGO_TERM_COLOR=never CARGO_TARGET_DIR="$1/target" CARGO_BUILD_BUILD_DIR="$1/target" &&
    cargo fetch --locked && cargo test --locked --test "$2") >"$work/out" 2>&1
}
ran() { sed -n 's/^running \([0-9][0-9]*\) tests\{0,1\}$/\1/p' "$work/out" | head -1; }
while read -r kind t n; do
  [ -n "$kind" ] || continue
  if [ "$kind" = py ]; then
    (cd "$work/t" && python3 -m pytest -q -p no:cacheprovider "$t") >"$work/out" 2>&1
    case $? in
      0) no "$t passes at the test commit $first; it must fail before the fix" ;;
      5) no "$t collects no test at the test commit $first; it must fail before the fix" ;;
      *) echo "acceptance-check: $t fails at the test commit (expected)" ;;
    esac
    if (cd "$work/h" && python3 -m pytest -q -p no:cacheprovider "$t") >"$work/out" 2>&1; then
      echo "acceptance-check: $t passes at head"
    else
      no "$t fails at head $head:"; sed 's/^/  /' "$work/out" | tail -20
    fi
    continue
  fi
  run_rs "$work/t" "$n" "the test commit"; rc=$?
  if grep -qF "error: could not compile \`makiba\` (test \"$n\")" "$work/out"; then
    echo "acceptance-check: $t does not compile at the test commit (expected: counts as failing)"
  elif [ "$(ran)" = 0 ]; then
    no "$t runs no test at the test commit $first; it must fail before the fix"
  elif [ "$rc" -eq 0 ]; then
    no "$t passes at the test commit $first; it must fail before the fix"
  elif [ -n "$(ran)" ]; then
    echo "acceptance-check: $t fails at the test commit (expected)"
  else
    no "could not run $t at the test commit $first:"; sed 's/^/  /' "$work/out" | tail -20
  fi
  run_rs "$work/h" "$n" head; rc=$?
  res=$(grep '^test result: ' "$work/out" | head -1)
  if [ "$rc" -ne 0 ]; then
    no "$t fails at head $head:"; sed 's/^/  /' "$work/out" | tail -20
  elif [ "$(ran)" = 0 ] || [ -z "$res" ]; then
    no "$t runs no test at head $head"
  elif ! printf '%s\n' "$res" | grep -q '; 0 ignored;'; then
    no "$t ignores a test at head $head: $res"
  elif printf '%s\n' "$res" | grep -qE '^test result: ok\. [1-9][0-9]* passed; 0 failed; 0 ignored;'; then
    echo "acceptance-check: $t passes at head"
  else
    no "$t does not pass at head $head: $res"
  fi
done <<<"$tests"

if [ "$bad" -eq 0 ]; then echo "ACCEPTANCE: pass"; exit 0; fi
echo "ACCEPTANCE: fail"; exit 1
