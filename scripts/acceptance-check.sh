#!/usr/bin/env bash
# acceptance-check: says whether the tests a branch declares failed at its test commit and pass at
# head. Carried from paddock (scripts/acceptance-check.sh); declared tests run with pytest.
# Run from the repository root on full history:
#   bash scripts/acceptance-check.sh BASE_SHA HEAD_SHA
# The branch's own commits are HEAD's first-parent line not reachable from BASE; the oldest of
# them (whose parent is then in BASE) may declare tests with "Acceptance-Test: <path>" trailers.
# The base may be merged in after it (no force push). When it declares:
#   - it is the only commit that declares, and it changes only declared paths and test helpers;
#   - every declared path is a test target and fails at that commit and passes at HEAD. A Cargo
#     target is tests/<name>.rs or tests/<name>/ holding main.rs (or that main.rs), run as
#     `cargo test --locked --test <name>`; a Python test is tests/**/test_*.py (a directory: every
#     test_*.py under it), run as `python3 -m pytest <path>`. See docs/acceptance-check.md;
#   - the test commit is not a merge, and the branch's own commits after it leave the declared
#     paths, test helpers (files under tests/ that are not test targets), .github/ and this
#     script unchanged (what a merge brings from its other parents does not count).
# Last line: ACCEPTANCE: pass | none | fail. Exit 0 pass or none (nothing declared), 1 fail, 2 usage.
set -u
[ $# -eq 2 ] || { echo "usage: acceptance-check.sh BASE_SHA HEAD_SHA" >&2; exit 2; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "acceptance-check: not in a git repository" >&2; exit 2; }
head=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown head $2" >&2; exit 2; }
mb=$(git merge-base "$1" "$head" 2>/dev/null) || { echo "acceptance-check: no merge base of $1 and $2" >&2; exit 2; }

bad=0
no() { echo "acceptance-check: $*"; bad=1; }
declared() { git log -1 --format='%(trailers:key=Acceptance-Test,valueonly)' "$1" | sed '/^[[:space:]]*$/d'; }

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

# 1. the test commit changes only declared paths and test helpers
while IFS= read -r f; do
  [ -n "$f" ] || continue
  is_declared "$f" || is_helper "$f" || no "test commit changes $f, which is neither declared nor a test helper"
done < <(git diff --no-renames --name-only "$first^" "$first")

# 2. protected paths unchanged by the branch's own commits after the test commit. A merge (of
# the base) counts only for files it leaves different from every parent: its own resolution.
own_changes() {
  local c ps p
  for c in $(printf '%s\n' "$commits" | tail -n +2); do
    ps=$(git rev-list --parents -n 1 "$c" | cut -d' ' -f2-)
    if [ "$(echo $ps | wc -w)" -eq 1 ]; then
      git diff --no-renames --name-only "$c^" "$c"
    else
      local f
      for p in $ps; do git diff --no-renames --name-only "$p" "$c"; done | sort | uniq -c |
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
    else no "declared path $p is not a test target"
    fi
  elif [ -d "$work/t/$p" ]; then
    found=
    n=$(rs_name "$p/main.rs") && [ -f "$work/t/$p/main.rs" ] && found+="rs $p $n"$'\n'
    found+=$(cd "$work/t" && find "$p" -type f -name 'test_*.py' | sort | while IFS= read -r f; do is_py "$f" && echo "py $f"; done)
    [ -n "$(printf '%s' "$found" | sed '/^$/d')" ] || no "declared path $p is not a test target"
    tests+=$found$'\n'
  else
    no "declared path $p does not exist at the test commit"
  fi
done <<<"$paths"
tests=$(printf '%s' "$tests" | sed '/^$/d')

run_rs() { # run_rs <tree> <name>: output in $work/out, Cargo's exit status returned
  (cd "$1" && export CARGO_TERM_COLOR=never && cargo fetch --locked && cargo test --locked --test "$2") >"$work/out" 2>&1
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
  run_rs "$work/t" "$n"; rc=$?
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
  run_rs "$work/h" "$n"; rc=$?
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
