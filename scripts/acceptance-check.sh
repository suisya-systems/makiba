#!/usr/bin/env bash
# acceptance-check: says whether the tests a branch declares failed at its test commit and pass at
# head. Carried from paddock (scripts/acceptance-check.sh); declared tests run with pytest.
# Run from the repository root on full history:
#   bash scripts/acceptance-check.sh BASE_SHA HEAD_SHA
# The branch's own commits are HEAD's first-parent line not reachable from BASE; the oldest of
# them (whose parent is then in BASE) may declare tests with "Acceptance-Test: <path>" trailers.
# The base may be merged in after it (no force push). When it declares:
#   - it is the only commit that declares, and it changes only declared paths and test helpers
#     (files under tests/common/ or tests/fixtures/ that are not test targets: helpers by place,
#     Issue 372; any other file under tests/, a test of another language included, is refused
#     unless declared);
#   - every declared path is a test target or a declared test file (a file under tests/ that is
#     not a target: protected, not run); at least one is a test target; every target fails at that commit and passes at HEAD. A Cargo
#     target is tests/<name>.rs or tests/<name>/ holding main.rs (or that main.rs), run as
#     `cargo test --locked --test <name>`; a Python test is tests/**/test_*.py (a directory: every
#     test_*.py under it), run as `python3 -m pytest <path>`. See docs/acceptance-check.md;
#   - the test commit is not a merge, and the branch's own commits after it leave the declared
#     paths, every path the test commit changed, the test infrastructure by file name wherever it
#     is (gate.rs INFRA_NAMES, INFRA_PREFIXES; Issue 576), every file under tests/ that is not a
#     test target and not a test helper, .github/ and this script unchanged. A test helper the test commit left alone may change, be
#     added or be deleted; each is named before the last line, and the person accepts it at the
#     merge (Issue 514). A base merge counts only for its own resolution: what differs from the
#     clean merge of its parents, or a conflict not resolved as the branch's own version.
# The two worktrees are made under the repository's git folder (<git common dir>/acceptance-check/),
# not under the system's temporary folder, and removed at exit (Issue 558). It fails at once when
# that git folder is in a temporary folder while the checkout is not, or when a folder above the
# work folder, other than CARGO_HOME, holds a Cargo config. A declared Python test runs with no
# conftest.py and no pytest config from above its worktree (--confcutdir; -c, --rootdir).
# Last line: ACCEPTANCE: pass | none | fail. Exit 0 pass or none (nothing declared), 1 fail, 2 usage.
# On a fail, each failure is printed once more in canonical form before the last line (Issue 589,
# docs/design/acceptance-approval.md 4.1): "acceptance-check: approvable changed <path>",
# "acceptance-check: approvable ignored <path> <n>", and "acceptance-check: not approvable" once
# when any other failure was found.
#   bash scripts/acceptance-check.sh --approvals DIR BASE_SHA HEAD_SHA
# judges as above and, when it would fail with only approvable failures, passes on a file of DIR
# that holds the owner's approval of exactly that repository ($GITHUB_REPOSITORY), pull request
# ($GITHUB_REF refs/pull/<n>/merge), head and failure set, signed by a key of the base's
# .github/acceptance-approvers (4.3). See docs/acceptance-check.md.
#   bash scripts/acceptance-check.sh --test-commit SHA
# prints only the paths the test commit SHA may not change (rule 2), one per line, sorted: exit 0
# none, 1 some, 2 usage. The merge gate in makibad runs this mode, so both judge by one rule.
#   bash scripts/acceptance-check.sh --scope BASE HEAD
# says whether HEAD is its test commit alone (docs/design/test-commit-scoped-ci.md, 3.1): it prints
# "test-commit <sha>" and one line per declared test ("rs <cargo target name>" or "py <path>") when
# HEAD's own commits are HEAD alone, not a merge, declaring, changing nothing rule 2 refuses, and
# its declared paths are test targets and helpers with at least one target (rule 3's
# classification, at HEAD); otherwise "full: <reason>". Exit 0 either way, 2 on usage or git error.
# It runs no test. CI runs the base's copy to decide whether a head runs only its declared tests.
set -u
# The declared paths, each trimmed and with empty and "." segments dropped, as the merge gate reads
# them (src/daemon/gate.rs, find_manifest): "./tests/a.rs" and "tests//a.rs" declare tests/a.rs.
# A declaration that is nothing but such segments ("." or "./") stays declared, as ".", and fails
# as no test target; it does not vanish into "nothing declared".
declared() {
  git log -1 --format='%(trailers:key=Acceptance-Test,valueonly)' "$1" |
    sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//' -e '/^$/d' -e 's#//*#/#g' -e ':a' -e 's#\(^\|/\)\./#\1#' -e 'ta' \
      -e 's#/\.$##' -e 's#^$#.#'
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
# A file under tests/ that is not a test target: what rule 4 protects and what a declared path that
# is not a target is ("helper": protected, not run).
is_test_file() { case $1 in tests/*) ! is_py "$1" && ! rs_name "$1" >/dev/null ;; *) false ;; esac; }
# A test helper by place (Issue 372), which a test commit may change without declaring it.
is_helper() { case $1 in tests/common/* | tests/fixtures/*) is_test_file "$1" ;; *) false ;; esac; }
is_declared() { local p; while IFS= read -r p; do under "$1" "$p" && return 0; done <<<"$paths"; return 1; }
# The one rule of what a test commit may change (Issue 361): a path under a path the commit
# declares, or a test helper. Prints every other path the commit changes, one per line. The merge
# gate (src/daemon/gate.rs, test_commit_refusals) runs it through --test-commit.
refused_in_test_commit() { # refused_in_test_commit <sha>; fails when git cannot list the changes
  local paths f list
  paths=$(declared "$1")
  # NUL-delimited: git would quote a name with non-ASCII bytes, a tab or a quote, and the quoted
  # form would match no declared path and no helper. Listed into a file first, so that a failed
  # git diff fails this function instead of reading as "nothing changed".
  list=$(mktemp) || return 2
  git diff --no-renames --name-only -z "$1^" "$1" >"$list" || { rm -f "$list"; return 2; }
  while IFS= read -r -d '' f; do
    [ -n "$f" ] || continue
    is_declared "$f" || is_helper "$f" || printf '%s\n' "$f"
  done <"$list"
  rm -f "$list"
}
# The refused paths of <sha> in $refused, kept whole: the trailing "x" stops the command
# substitution from dropping a name made only of newlines. Fails as refused_in_test_commit does.
read_refused() { refused=$(refused_in_test_commit "$1" && printf x) || return 2; refused=${refused%x}; }
# Rule 3's classification of the declared paths ($paths) as they are in the tree <root>, one line
# per result: "rs <path> <target name>", "py <path>", "helper <path>" or "bad <reason>". The main
# mode (in the test commit's tree) and --scope (in HEAD's) both read it, so they cannot disagree on
# what a test target is.
classify() { # classify <root>
  local p n found
  while IFS= read -r p; do
    p=${p%/}
    if [ -f "$1/$p" ]; then
      if n=$(rs_name "$p"); then echo "rs $p $n"
      elif is_py "$p"; then echo "py $p"
      elif is_test_file "$p"; then echo "helper $p"
      else echo "bad declared path $p is not a test target"
      fi
    elif [ -d "$1/$p" ]; then
      found=
      n=$(rs_name "$p/main.rs") && [ -f "$1/$p/main.rs" ] && found+="rs $p $n"$'\n'
      found+=$(cd "$1" && find "$p" -type f -name 'test_*.py' | sort | while IFS= read -r f; do is_py "$f" && echo "py $f"; done)
      if [ -n "$(printf '%s' "$found" | sed '/^$/d')" ]; then printf '%s\n' "$found" | sed '/^$/d'
      elif is_test_file "$p"; then echo "helper $p"
      else echo "bad declared path $p is not a test target"
      fi
    else
      echo "bad declared path $p does not exist at the test commit"
    fi
  done <<<"$paths"
}
if [ "${1:-}" = --scope ]; then
  [ $# -eq 3 ] || { echo "usage: acceptance-check.sh --scope BASE_SHA HEAD_SHA" >&2; exit 2; }
  git rev-parse --git-dir >/dev/null 2>&1 || { echo "acceptance-check: not in a git repository" >&2; exit 2; }
  base=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown base $2" >&2; exit 2; }
  head=$(git rev-parse --verify "$3^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown head $3" >&2; exit 2; }
  full() { echo "full: $*"; exit 0; }
  # 1. the head's own commits, read as the main mode reads them, are exactly one: HEAD
  commits=$(git rev-list --first-parent "$head" --not "$base") || { echo "acceptance-check: cannot list the commits of $3" >&2; exit 2; }
  [ "$commits" = "$head" ] || full "the head has $(printf '%s\n' "$commits" | sed '/^$/d' | wc -l | tr -d ' ') own commits, not one"
  # 2. not a merge (nor a commit without a parent)
  [ "$(git rev-list --parents -n 1 "$head" | wc -w)" -eq 2 ] || full "the head $head is a merge or has no parent"
  # 3. it declares
  paths=$(declared "$head")
  [ -n "$paths" ] || full "the head $head declares no Acceptance-Test path"
  # 4. rule 2: it changes only declared paths and test helpers
  read_refused "$head" || { echo "acceptance-check: cannot list the changes of commit $3" >&2; exit 2; }
  [ -z "$refused" ] || full "the head $head also changes $(printf '%s' "$refused" | LC_ALL=C sort -u | tr '\n' ' ' | sed 's/ $//'), neither declared nor a test helper"
  # 5. rule 3: every declared path is a target or a helper at HEAD, and at least one is a target
  work=$(mktemp -d) || exit 2
  trap 'git worktree remove --force "$work/h" >/dev/null 2>&1; rm -rf "$work"' EXIT
  git worktree add -q --detach "$work/h" "$head" >/dev/null 2>&1 || { echo "acceptance-check: git worktree add failed" >&2; exit 2; }
  out=
  while IFS= read -r line; do
    kind=${line%% *} rest=${line#* }
    case $kind in
      rs) out+="rs ${rest##* }"$'\n' ;;
      py) out+="py $rest"$'\n' ;;
      bad) full "$rest" ;;
    esac
  done < <(classify "$work/h")
  [ -n "$out" ] || full "no declared path is a test target"
  echo "test-commit $head"
  printf '%s' "$out" | awk '!seen[$0]++'
  exit 0
fi
if [ "${1:-}" = --test-commit ]; then
  [ $# -eq 2 ] || { echo "usage: acceptance-check.sh --test-commit SHA" >&2; exit 2; }
  sha=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown commit $2" >&2; exit 2; }
  git rev-parse --verify -q "$sha^" >/dev/null || { echo "acceptance-check: commit $2 has no parent" >&2; exit 2; }
  read_refused "$sha" || { echo "acceptance-check: cannot list the changes of commit $2" >&2; exit 2; }
  [ -n "$refused" ] || exit 0
  printf '%s' "$refused" | LC_ALL=C sort -u; exit 1
fi
approvals=
if [ "${1:-}" = --approvals ]; then
  [ $# -ge 2 ] && [ -d "$2" ] || { echo "usage: acceptance-check.sh --approvals DIR BASE_SHA HEAD_SHA (DIR a directory)" >&2; exit 2; }
  approvals=$(cd -P "$2" && pwd -P) || exit 2
  shift 2
fi
[ $# -eq 2 ] || { echo "usage: acceptance-check.sh [--approvals DIR] BASE_SHA HEAD_SHA | --test-commit SHA | --scope BASE_SHA HEAD_SHA" >&2; exit 2; }
git rev-parse --git-dir >/dev/null 2>&1 || { echo "acceptance-check: not in a git repository" >&2; exit 2; }
head=$(git rev-parse --verify "$2^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown head $2" >&2; exit 2; }
mb=$(git merge-base "$1" "$head" 2>/dev/null) || { echo "acceptance-check: no merge base of $1 and $2" >&2; exit 2; }

bad=0
# The failure set (4.1): the approvable records, and other=1 when any failure is not approvable.
# Every failure goes through no() (not approvable) unless its site calls approvable().
other=0
records=()
no() { echo "acceptance-check: $*"; bad=1; other=1; }
plain() ( LC_ALL=C; case $1 in '' | *[!!-~]*) exit 1 ;; esac ) # every byte is 0x21..0x7e
approvable() { # approvable <record> <path> <text>: a failure the owner may approve for this head
  echo "acceptance-check: $3"; bad=1
  if plain "$2"; then records+=("$1"); else other=1; fi
}
sha256() { if command -v sha256sum >/dev/null 2>&1; then sha256sum; else shasum -a 256; fi | cut -d' ' -f1; }
# approved: 0 when a file of $approvals holds the owner's signed approval of this run's failure set
# (4.3). The signers come from the base only. Prints why each file is not accepted.
approved() {
  local repo=${GITHUB_REPOSITORY:-} ref=${GITHUB_REF:-} pr set digest signers f name text sig p expect
  if [ "$other" -ne 0 ] || [ "${#records[@]}" -eq 0 ]; then
    echo "acceptance-check: approvals not read: a failure is not approvable"; return 1
  fi
  plain "$repo" || { echo "acceptance-check: approvals not read: GITHUB_REPOSITORY is not set"; return 1; }
  pr=${ref#refs/pull/}; pr=${pr%/merge}
  case $ref in "refs/pull/$pr/merge") ;; *) pr= ;; esac
  case $pr in '' | *[!0-9]*) pr= ;; esac
  [ -n "$pr" ] || { echo "acceptance-check: approvals not read: GITHUB_REF is not refs/pull/<n>/merge"; return 1; }
  signers=$(mktemp) || return 1
  if ! git show "$base:.github/acceptance-approvers" >"$signers" 2>/dev/null || [ ! -s "$signers" ]; then
    echo "acceptance-check: approvals not read: the base $base holds no .github/acceptance-approvers"; rm -f "$signers"; return 1
  fi
  set=$(printf '%s\n' "${records[@]}")$'\n'
  digest=$(printf '%s' "$set" | sha256)
  text=$(mktemp) && sig=$(mktemp) || { rm -f "$signers"; return 1; }
  while IFS= read -r -d '' f; do
    name=${f##*/}
    [ -f "$f" ] && [ ! -L "$f" ] || continue
    # The approval text: the lines before the first line "---"; the signature: the lines after it.
    awk -v t="$text" -v s="$sig" 'f { print > s; next } $0 == "---" { f = 1; next } { print > t }' "$f" </dev/null
    p=$(sed -n '6s/^pending \([0-9][0-9]*\)$/\1/p' "$text")
    expect="makiba-acceptance-approval v1"$'\n'"repo $repo"$'\n'"pr $pr"$'\n'"head $head"$'\n'"failures $digest"$'\n'"pending $p"$'\n'"$set"
    if [ -z "$p" ]; then
      echo "acceptance-check: approval $name not accepted: not of the form makiba-acceptance-approval v1"
    elif ! printf '%s' "$expect" | cmp -s - "$text"; then
      echo "acceptance-check: approval $name not accepted: it names another repository, pull request, head or failure set"
    elif ! ssh-keygen -Y verify -f "$signers" -I makiba-owner -n makiba-acceptance-approval -s "$sig" <"$text" >/dev/null 2>&1; then
      echo "acceptance-check: approval $name not accepted: no valid signature by a key of the base's .github/acceptance-approvers"
    else
      echo "acceptance-check: approved by the owner (pending $p): $(printf '%s\n' "${records[@]}" | paste -sd ';' - | sed 's/;/; /g')"
      rm -f "$signers" "$text" "$sig"; return 0
    fi
    : >"$text"; : >"$sig"
  done < <(find "$approvals" -mindepth 1 -maxdepth 1 -print0 | LC_ALL=C sort -z)
  rm -f "$signers" "$text" "$sig"
  return 1
}
# finish: the canonical lines (4.1), the approval when asked (4.3), and the last line.
finish() {
  [ "$bad" -ne 0 ] || { echo "ACCEPTANCE: pass"; exit 0; }
  local r
  if [ "${#records[@]}" -gt 0 ]; then
    mapfile -t records < <(printf '%s\n' "${records[@]}" | LC_ALL=C sort -u)
    for r in "${records[@]}"; do echo "acceptance-check: approvable $r"; done
  fi
  [ "$other" -eq 0 ] || echo "acceptance-check: not approvable"
  if [ -n "$approvals" ] && approved; then echo "ACCEPTANCE: pass"; exit 0; fi
  echo "ACCEPTANCE: fail"; exit 1
}
base=$(git rev-parse --verify "$1^{commit}" 2>/dev/null) || { echo "acceptance-check: unknown base $1" >&2; exit 2; }

commits=$(git rev-list --reverse --first-parent "$head" --not "$1")
first=$(printf '%s\n' "$commits" | head -1)
[ -n "$first" ] && paths=$(declared "$first") || paths=
for c in $(printf '%s\n' "$commits" | tail -n +2); do
  [ -z "$(declared "$c")" ] || no "commit $c declares Acceptance-Test but is not the first commit of the branch"
done
if [ -z "$paths" ]; then
  [ "$bad" -eq 0 ] || finish
  echo "acceptance-check: no Acceptance-Test trailer on the first commit; nothing to check"
  echo "ACCEPTANCE: none"; exit 0
fi
echo "acceptance-check: test commit $first declares: $(echo $paths)"
[ "$(git rev-list --parents -n 1 "$first" | wc -w)" -eq 2 ] || no "test commit $first is a merge; no join commit may come before or at the test commit"

# 1. the test commit changes only declared paths and test helpers
if read_refused "$first"; then
  [ -z "$refused" ] || while IFS= read -r f; do
    no "test commit changes $f, which is neither declared nor a test helper"
  done < <(printf '%s' "$refused")
else
  no "cannot list the changes of the test commit $first"
fi

# 2. protected paths unchanged by the branch's own commits after the test commit. A merge (of
# the base) counts only for its own resolution (merge_own). Names are read unquoted and kept
# NUL-delimited to the end (-z), so a non-ASCII helper is still seen and a name holding a newline
# stays one name. A failed git fails the function (pipefail), so it is not read as "nothing changed".
# The gate's test-infrastructure names (src/daemon/gate.rs INFRA_NAMES, INFRA_PREFIXES): a file
# whose name is one of them stays protected wherever it is (a root pytest.ini, a helper
# tests/common/conftest.py; Issue 576). tests/test_acceptance_helper_change.py checks
# that the two lists are equal.
INFRA_NAMES='conftest.py pytest.ini .pytest.ini pytest.toml .pytest.toml pyproject.toml setup.cfg tox.ini package.json'
INFRA_PREFIXES='vitest.config. vitest.workspace. vite.config.'
is_infra() { # by file name at any depth, as gate.rs is_infra
  local n=${1##*/} x
  for x in $INFRA_NAMES; do [ "$n" = "$x" ] && return 0; done
  for x in $INFRA_PREFIXES; do case $n in "$x"*) return 0 ;; esac; done
  return 1
}
# merge_own <commit> <p1> <p2>: the paths a two-parent merge made itself, NUL-delimited. Against T,
# the tree `git merge-tree --write-tree` makes of the parents (as the gate's clean merge, Issue
# 344): a path it merged cleanly counts when the commit holds it otherwise than T (an own edit, or
# a file set back to either parent); a conflicted path counts unless the commit holds it as p1
# (the branch's own version kept). Returns 2 when git fails.
merge_own() {
  local tree f rc out
  local -A conf=() own=()
  out=$(mktemp) || return 2
  git merge-tree --write-tree --name-only -z "$2" "$3" >"$out"; rc=$?
  [ "$rc" -le 1 ] || { rm -f "$out"; return 2; }
  { IFS= read -r -d '' tree; while IFS= read -r -d '' f && [ -n "$f" ]; do conf[$f]=1; done; } <"$out"
  [ -n "$tree" ] && git diff --no-renames --name-only -z "$tree" "$1" >"$out" || { rm -f "$out"; return 2; }
  while IFS= read -r -d '' f; do [ -z "$f" ] || [ -n "${conf[$f]+x}" ] || own[$f]=1; done <"$out"
  git diff --no-renames --name-only -z "$2" "$1" >"$out" || { rm -f "$out"; return 2; }
  while IFS= read -r -d '' f; do [ -z "$f" ] || [ -z "${conf[$f]+x}" ] || own[$f]=1; done <"$out"
  rm -f "$out"
  for f in "${!own[@]}"; do printf '%s\0' "$f"; done
}
own_changes() (
  set -o pipefail
  for c in $(printf '%s\n' "$commits" | tail -n +2); do
    read -r -a ps <<<"$(git rev-list --parents -n 1 "$c" | cut -d' ' -f2-)"
    if [ "${#ps[@]}" -eq 1 ]; then
      git diff --no-renames --name-only -z "$c^" "$c" || exit 2
    elif [ "${#ps[@]}" -eq 2 ]; then
      merge_own "$c" "${ps[0]}" "${ps[1]}" || exit 2
    else # an octopus merge: the files it leaves different from every parent
      for p in "${ps[@]}"; do git diff --no-renames --name-only -z "$p" "$c" || exit 2; done | sort -z | uniq -zc |
        while IFS= read -r -d '' l; do
          l=${l#"${l%%[![:space:]]*}"}
          [ "${l%% *}" -lt "${#ps[@]}" ] || printf '%s\0' "${l#* }"
        done || exit 2
    fi
  done | sort -zu
)
# A merge may bring in only the base: a side branch's own commits are off the first-parent line.
for c in $(printf '%s\n' "$commits" | tail -n +2); do
  for p in $(git rev-list --parents -n 1 "$c" | cut -d' ' -f3-); do
    git merge-base --is-ancestor "$p" "$1" || no "merge $c brings in $p, which is not in the base"
  done
done
# Rule 4 (Issue 514): protected are the declared paths, every path the test commit changed,
# .github/, this script, the test infrastructure by file name at any depth (Issue 576) and every
# other file under tests/ that is not a test target; a test helper by place that the test commit
# left alone may change, be added or be deleted, and is named before the last line (the person
# accepts it at the merge).
declare -A in_test_commit=()
helpers_changed=()
list=$(mktemp) || exit 2
if git diff --no-renames --name-only -z "$first^" "$first" >"$list"; then
  while IFS= read -r -d '' f; do [ -z "$f" ] || in_test_commit[$f]=1; done <"$list"
else
  no "cannot list the changes of the test commit $first"
fi
own_changes >"$list" || no "cannot list the changes of the commits after the test commit"
while IFS= read -r -d '' f; do
  [ -n "$f" ] || continue
  why="$f changed after the test commit (declared tests, paths the test commit changed, test infrastructure by file name anywhere, test files outside tests/common/ and tests/fixtures/, .github/ and this script are protected)"
  # The precedence of docs/design/acceptance-approval.md section 5: a path under .github/ or test
  # infrastructure is never approvable; a declared path, a path the test commit changed or this
  # script is; any other protected file under tests/ is not.
  if under "$f" .github || is_infra "$f"; then
    no "$why"
  elif is_declared "$f" || [ -n "${in_test_commit[$f]+x}" ] || [ "$f" = scripts/acceptance-check.sh ]; then
    approvable "changed $f" "$f" "$why"
  elif is_test_file "$f" && ! is_helper "$f"; then
    no "$why"
  elif is_helper "$f"; then
    helpers_changed+=("$f")
  fi
done <"$list"
rm -f "$list"

# 3. declared tests fail at the test commit and pass at head
# The work folder is under the repository's git folder, not the system's temporary folder: a
# declared test then sees in the acceptance job the kind of path it sees in the tests job, also
# when the code under test treats a temporary folder otherwise (Issue 558). A git folder in a
# temporary folder while the checkout is not would bring that back, so it fails the check.
tmp_root() { # tmp_root <resolved dir>: prints the temporary folder that holds it, if any
  local r
  for r in /tmp /private/tmp /var/tmp ${TMPDIR:+"$TMPDIR"}; do
    r=$(cd -P "$r" 2>/dev/null && pwd -P) || continue
    under "$1" "$r" && { echo "$r"; return 0; }
  done
  return 1
}
gitdir=$(git rev-parse --path-format=absolute --git-common-dir 2>/dev/null) && gitdir=$(cd -P "$gitdir" && pwd -P) &&
  mkdir -p "$gitdir/acceptance-check" && work=$(mktemp -d "$gitdir/acceptance-check/run.XXXXXX") ||
  { no "cannot make a work folder under the repository's git folder"; finish; }
trap 'git worktree remove --force "$work/t" >/dev/null 2>&1; git worktree remove --force "$work/h" >/dev/null 2>&1; rm -rf "$work"; rmdir "$gitdir/acceptance-check" 2>/dev/null' EXIT
if root=$(tmp_root "$work"); then
  if top=$(git rev-parse --show-toplevel 2>/dev/null) && top=$(cd -P "$top" && pwd -P) && ! tmp_root "$top" >/dev/null; then
    no "the git folder $gitdir is under a temporary folder ($root) while the checkout $top is not; declared tests would see a path the tests job does not"
    finish
  fi
fi
# Cargo reads .cargo/config.toml (or .cargo/config) in every folder above the one it builds in. A
# folder above the work folder is outside both worktrees (in CI, the checkout, which is the head),
# so its config would shape the test commit's build too: the check fails instead. CARGO_HOME's
# config is read by every build anyway and is left out.
cargo_home=$(cd -P "${CARGO_HOME:-$HOME/.cargo}" 2>/dev/null && pwd -P) || cargo_home=
d=$work
while [ "$d" != / ]; do
  d=$(dirname "$d")
  [ -n "$cargo_home" ] && [ "$d/.cargo" = "$cargo_home" ] && continue
  for f in "$d/.cargo/config" "$d/.cargo/config.toml"; do
    [ -e "$f" ] || continue
    no "$f lies above the work folder $work; Cargo would read it in the builds of both the test commit and head, so a config outside them could shape the test commit's build"
    finish
  done
done
git worktree add -q --detach "$work/t" "$first" && git worktree add -q --detach "$work/h" "$head" \
  || { no "git worktree add failed"; finish; }
# Each test is "py <path>" or "rs <path> <target name>".
tests=
while IFS= read -r line; do
  kind=${line%% *} rest=${line#* }
  case $kind in
    rs | py) tests+="$line"$'\n' ;;
    helper) echo "acceptance-check: $rest is a declared test helper (protected, not run)" ;;
    bad) no "$rest" ;;
  esac
done < <(classify "$work/t")
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
# pytest looks above the worktree too: for a config file (whose folder then becomes the rootdir,
# and conftest.py files up to it are loaded) when the worktree has none. Above the worktree is the
# checkout, in CI the head, which would then shape the test commit's run (Issue 558). So no
# conftest.py above the worktree is loaded (--confcutdir), and unless the config file pytest's
# discovery picks lies inside the worktree, the run gets an empty one (-c, --rootdir) instead.
# Which file that is, is asked of pytest's own discovery function (locate_config), which reads
# files and starts nothing: no plugin, no pythonpath, no conftest. Its rules (empty tables, quoted
# TOML keys, the version's file names) are not copied here. It runs from the work folder, so no
# module of a worktree can shadow what it imports; when it cannot be asked, the check fails.
: >"$work/empty-pytest.ini" # a name pytest's own search never takes
py_found() { # py_found <tree> <test path>: prints the config file pytest's discovery picks, or nothing
  (cd "$work" && python3 - "$1" "$1/$2" 2>"$work/py-err" <<'PY'
import inspect
import sys
from pathlib import Path

from _pytest.config.findpaths import locate_config

tree, test = Path(sys.argv[1]), [Path(sys.argv[2])]
n = len(inspect.signature(locate_config).parameters)
if n == 2:
    found = locate_config(tree, test)
elif n == 1:
    found = locate_config(test)
else:
    raise SystemExit(f"locate_config takes {n} parameters")
print(found[1] or "")
PY
  )
}
own_config() { # own_config <tree> <config file or empty>: the file lies inside <tree>
  local f=$2
  [ -n "$f" ] && f=$(cd -P "$(dirname "$f")" 2>/dev/null && pwd -P)/${f##*/} && under "$f" "$1"
}
run_py() { # run_py <tree> <test path> <config file or empty>: output in $work/out, pytest's exit status returned
  if own_config "$1" "$3"; then
    (cd "$1" && python3 -m pytest -q -p no:cacheprovider --confcutdir="$1" "$2") >"$work/out" 2>&1
  else
    (cd "$1" && python3 -m pytest -q -p no:cacheprovider --confcutdir="$1" -c "$work/empty-pytest.ini" --rootdir="$1" "$2") >"$work/out" 2>&1
  fi
}
cannot_ask() { no "cannot ask pytest which config file $1 uses at $2: $(tail -1 "$work/py-err")"; }
ran() { sed -n 's/^running \([0-9][0-9]*\) tests\{0,1\}$/\1/p' "$work/out" | head -1; }
while read -r kind t n; do
  [ -n "$kind" ] || continue
  if [ "$kind" = py ]; then
    if ! cfg_t=$(py_found "$work/t" "$t"); then
      cannot_ask "$t" "the test commit $first"
    else
      run_py "$work/t" "$t" "$cfg_t"
      case $? in
        0) no "$t passes at the test commit $first; it must fail before the fix" ;;
        5) no "$t collects no test at the test commit $first; it must fail before the fix" ;;
        *) echo "acceptance-check: $t fails at the test commit (expected)" ;;
      esac
    fi
    if ! cfg_h=$(py_found "$work/h" "$t"); then
      cannot_ask "$t" "head $head"
    elif run_py "$work/h" "$t" "$cfg_h"; then
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
    approvable "ignored $t $(printf '%s\n' "$res" | sed -n 's/.*; \([0-9][0-9]*\) ignored;.*/\1/p')" "$t" "$t ignores a test at head $head: $res"
  elif printf '%s\n' "$res" | grep -qE '^test result: ok\. [1-9][0-9]* passed; 0 failed; 0 ignored;'; then
    echo "acceptance-check: $t passes at head"
  else
    no "$t does not pass at head $head: $res"
  fi
done <<<"$tests"

for f in "${helpers_changed[@]}"; do
  echo "acceptance-check: test helper $f changed after the test commit; the person accepts it at the merge"
done
finish
