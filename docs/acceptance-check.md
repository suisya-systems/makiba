# Acceptance check in CI

Carried from paddock (`docs/design/acceptance-check.md`, `scripts/acceptance-check.sh`). Issue #3; reworked for Rust tests.

## What runs

The `acceptance` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on every pull request. It installs the toolchain of `rust-toolchain.toml` and Python with pytest and pyyaml (as the `tests` job has them, so a declared test that reads `ci.yml` can be collected), then runs:

```sh
bash scripts/acceptance-check.sh <base sha> <head sha>
```

[`scripts/acceptance-check.sh`](../scripts/acceptance-check.sh) takes the branch's own commits: the head's first-parent line that the base cannot reach. The oldest of them is the test commit (its parent is in the base). If it carries `Acceptance-Test: <path>` trailers, the check fails unless all of these hold:

1. The test commit is not a merge, and no other own commit declares. So no join commit comes before the test commit.
2. The test commit changes only declared paths and test helpers (files under `tests/common/` or `tests/fixtures/`; see rule 3). This is the one rule of what a test commit may change; the merge gate judges by it too (below).
3. At least one declared path is a test target; every declared target fails at the test commit and passes at head (below).
4. The own commits after the test commit leave unchanged the declared paths, every path the test commit changed, every file that is test infrastructure, wherever it is in the tree (Issue 576): a file named `conftest.py`, `pytest.ini`, `.pytest.ini`, `pytest.toml`, `.pytest.toml`, `pyproject.toml`, `setup.cfg`, `tox.ini` or `package.json`, or whose name starts with `vitest.config.`, `vitest.workspace.` or `vite.config.` (`INFRA_NAMES` and `INFRA_PREFIXES` of `src/daemon/gate.rs`, judged by file name as the merge gate's condition (a) judges them; so a root `pytest.ini`, a `src/conftest.py` and a `tests/common/conftest.py` all count; the checker holds the same list, and a test fails when the two differ), every other file under `tests/` that is neither a test target nor a test helper, `.github/` and the checker itself. A test helper the test commit left alone may be changed, added or deleted; each is printed before the last line as `acceptance-check: test helper <path> changed after the test commit; the person accepts it at the merge`, and the last line stays `ACCEPTANCE: pass` (Issue 514, below). New test targets may be added.

The base may be merged into the branch after the test commit; a merge that brings in anything not in the base fails. A base merge counts in rule 4 only for its own resolution, judged against the tree `git merge-tree --write-tree <p1> <p2>` makes of its two parents (as the gate's clean merge, Issue 344): a file git merged with no conflict counts when the merge commit holds it otherwise than that tree (an edit of its own, or the file set back to either parent's version); a file merge-tree reports in conflict counts unless the merge commit holds it as its first parent (the branch's own version kept). So a declared test that git merged cleanly with a change from the base does not count; a declared test set back to the base's version does. A merge of more than two parents counts the files it leaves different from every parent. Renames count as both paths. File names are read whole (NUL-delimited), a newline in one included.

No trailer: the job passes and prints `ACCEPTANCE: none`. On a pull request the last line of the log is always `ACCEPTANCE: pass`, `none` or `fail`. Tests: [`tests/test_acceptance_check.py`](../tests/test_acceptance_check.py); test infrastructure outside `tests/`: [`tests/test_acceptance_root_infra.py`](../tests/test_acceptance_root_infra.py).

## A failure by construction, approved by the owner

Some pull requests fail `acceptance` by construction: a declared test that had to change after its test commit (#530), a declared Cargo target that ignores a test (#520, #535), a change of this checker (#534). They used to land only by the lift. The owner chose instead a signed approval for exactly that head and failure set ([`docs/design/acceptance-approval.md`](design/acceptance-approval.md), option 1, Issue 574). Its build is in five steps (Issues 589 to 593). This checker's part (Issue 589) is below. makibad's ask and record are Issues 590 to 592. The workflow step that hands the approvals to the checker, and the signers file, are the owner's Issue 593. Until that step is in, nothing passes `--approvals`, and the route is inert.

**Canonical lines.** On a fail, before the last line, the checker prints each failure once more in a form that holds no duration, no test output and no sha (the note's 4.1):

- `acceptance-check: approvable changed <path>`: rule 4, for a declared path, for a path the test commit changed, or for `scripts/acceptance-check.sh`;
- `acceptance-check: approvable ignored <path> <n>`: rule 3, a declared Cargo target that ignores `n` tests at head while at least one passes and none fails (`<path>` as declared);
- `acceptance-check: not approvable`, once, when any other failure was found. This also covers a path with a byte outside printable ASCII without space (`0x21` to `0x7e`): such a name could split a record or pass for another, so it keeps the lift.

The records (`changed <path>`, `ignored <path> <n>`), sorted bytewise and each followed by a newline, are the **failure set**. Its SHA-256 is the **failure digest**. Without `--approvals` nothing else changes: the same failure lines, the same last line, the same exit status.

**Allowed kinds** (the note's section 5). Approvable:

- a declared path or a path the test commit changed, changed after the test commit, whether it is a test target, a declared non-target file or a helper the test commit changed;
- this checker changed after the test commit;
- a declared Cargo target that ignores a test at head while at least one of its tests passes (one whose every test is ignored runs no test, and keeps the lift).

Never approvable, whatever else the path is: anything under `.github/` and test infrastructure by file name (`INFRA_NAMES`, `INFRA_PREFIXES`). Also not approvable, so still by the lift:

- another file under `tests/` that is neither a target nor a helper, changed after the test commit;
- a declared test that fails at head, passes or runs no test at the test commit, does not compile at head, or runs no test at head;
- rule 1, a second declaring commit, a merge test commit, a merge that brings in a non-base commit;
- a work folder, worktree, listing or Cargo config failure.

Widening the list is a change of the base's checker, so it is itself such a pull request.

**`--approvals DIR`.** `bash scripts/acceptance-check.sh --approvals DIR <base sha> <head sha>` judges as above (`DIR` must be a directory: else usage, exit 2). Only when the check would fail does it read the approvals, as the note's 4.3 sets out:

1. When any failure is not approvable, or there is no record, it prints `acceptance-check: approvals not read: ...` and fails as before.
2. The repository is `$GITHUB_REPOSITORY`, and the pull request is `<n>` of `$GITHUB_REF` = `refs/pull/<n>/merge`, which Actions sets on `pull_request`. If either is missing, it reads nothing and fails.
3. The signers come from the **base**: `git show <base>:.github/acceptance-approvers`, in `ssh-keygen` allowed-signers form (`makiba-owner namespaces="makiba-acceptance-approval" <public key>`). Neither the head nor the checkout is read. If the file is absent or empty, it reads nothing and fails.
4. Each regular file directly in `DIR`, in name order, is a comment body: the approval text, a line `---`, then the armored `SSH SIGNATURE` block. Two checks decide it, and it is accepted only when both pass:
   - the text is exactly `makiba-acceptance-approval v1`, `repo <repository>`, `pr <n>`, `head <the head judged>`, `failures <this run's failure digest>`, `pending <digits>`, and then this run's failure set, one record per line, each line ending in a newline;
   - `ssh-keygen -Y verify -f <signers> -I makiba-owner -n makiba-acceptance-approval` accepts its signature over that text.

   The failure set and its digest are computed in this run, never read from a log. A file that is not accepted is named, with the reason, in `acceptance-check: approval <name> not accepted: ...`.
5. On the first accepted file it prints `acceptance-check: approved by the owner (pending <id>): <records joined by "; ">`, then `ACCEPTANCE: pass`, exit 0. With none accepted it ends with `ACCEPTANCE: fail` as before.

So an approval signed by another key, for another head, pull request or repository, one that leaves out a failure or names one that is not there, one with a wrong digest, and any approval while a failure is not approvable, all fail. Test: [`tests/test_acceptance_approval.py`](../tests/test_acceptance_approval.py).

Limits:

- Inert until Issue 593 passes the flag.
- The merge queue's commit is not judged, so no approval is read there (the note's section 6).
- A body with CRLF line ends is not the signed text, so it fails.
- The repository and the pull request number come from the runner's environment.
- The pull request's code runs in the same job before the verification, as it can already change today's verdict (the note's section 9). Against hostile code, the bound is makiba's gate and the person's reading.

## A fix that must change a test helper

A fix that is a change to a test helper (Issue 506's `tests/common/local.rs`), or a clean base merge into a declared test (Issue 475's `tests/pr_merge_state.rs`), used to fail rule 4 and landed only by the lift. The person decided ([`docs/design/helper-fix.md`](design/helper-fix.md), option D): CI lets these through and names them, and the person's key at the merge accepts them.

- Here: a helper by place (`tests/common/`, `tests/fixtures/`) that the test commit did not change may change, be added or be deleted after it. Rule 3 still holds (the declared test fails at the test commit and passes at head), and each such helper is printed before the last line: `acceptance-check: test helper <path> changed after the test commit; the person accepts it at the merge`.
- At the gate: makibad's merge gate names the same helpers (`helpers_changed`), a declared test that head holds as the clean merge of the test commit and the base (`merged_tests`), and a helper the base changed after the test commit (`base_helpers`), and warns under both merge approval modes; only the person's `makiba merge approve` lands it (Issue 513).
- Still by the lift: a change after the test commit to a declared path, to a path the test commit changed, to test infrastructure anywhere in the tree (a root `pytest.ini` or `pyproject.toml`, `tests/conftest.py`, `tests/common/conftest.py` and the other names of the list), to another file under `tests/` that is not a helper, to `.github/` or to this checker.

Test: [`tests/test_acceptance_helper_change.py`](../tests/test_acceptance_helper_change.py).

## On the merge queue's commit

`ci.yml` also runs on `merge_group`, the event of GitHub's merge queue, so `tests`, `lint`, `real-base` and `acceptance` each report on the commit the queue would land. There `acceptance` runs no test and reads no pull request payload (one step, which reads only the runner's environment): it succeeds and prints `ACCEPTANCE: judged at the pull request's head (...)`. The declared test is judged on the pull request side, by this job on the pull request and by makiba's gate before the enqueue ([`docs/design/merge-wait.md`](design/merge-wait.md), Q2, answered; Issue 401). Every other step of the job runs on `pull_request` alone, unchanged; on `push` the job is skipped. Test: [`tests/test_ci_merge_group.py`](../tests/test_ci_merge_group.py), which also shows that it fails when `merge_group` is dropped or when `acceptance` there reads the payload or fails.

## Rule 3

**Test targets.** A declared path is a test target: a file `tests/<name>.rs` directly under `tests/`, or a folder `tests/<name>/` holding `main.rs` (or that `main.rs` itself); its target name is `<name>`. For the development tools, a file `tests/**/test_*.py` is also a declared test (a declared folder: every `test_*.py` under it). A declared path that is neither a target nor another file under `tests/`, at the test commit, fails the check; so does a declaration with no target at all. Test helpers are defined by place (Issue 372): the files under `tests/common/` or `tests/fixtures/` that are not test targets. The test commit may add, change or delete them without declaring them. Every other file under `tests/` that is not a target (a JavaScript `tests/calc.test.js`, a Go `tests/calc_test.go`, `tests/conftest.py`, a data file such as `tests/hook_cases.json`) is refused in the test commit unless it is declared; so an existing test of a language the checker does not run cannot be changed or deleted there unseen. A declared non-target file is protected and not run, and need not fail at the test commit. Rule 4 protects every file under `tests/` that is not a target, except a helper by place that the test commit left alone and that is not test infrastructure, and test infrastructure anywhere in the tree.

**How a target runs.** In each of the two worktrees (test commit and head), with `CARGO_TERM_COLOR=never`: `cargo fetch --locked` first, so that a download failure is not mistaken for a test result; then `cargo test --locked --test <name>`, with stdout and stderr kept. Each worktree builds into its own `target/` (the checker sets `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to it, whatever the environment or a cargo config file says, and prints the folder of each build): one folder shared by the two worktrees, or by two runs, would let Cargo reuse a build made from the other's sources (Issue 232). Python tests run with `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` unset. With `--test` Cargo builds the library, the binaries and that one test, and runs no other test and no doc test, so the output holds at most one `running N tests` line and one `test result:` line.

**Where the worktrees are.** The two worktrees, and so their `target/` folders and Cargo's `CARGO_TARGET_TMPDIR`, are made in a fresh folder `<git common dir>/acceptance-check/run.XXXXXX` of the repository being checked, not in the system's temporary folder, and removed at exit (the `acceptance-check` folder too, once empty). On a runner the git folder is inside the workspace, so a declared test sees in the `acceptance` job the kind of path it sees in the `tests` job, also when the code under test treats a temporary folder (`/tmp`, `/private/tmp`, `/var/tmp`, `TMPDIR`) otherwise (Issue 558; the case was Issue 272's `notify_command`, which refuses a command under a temporary folder). The folder is outside the working tree, so `git status` stays clean. When the folder cannot be made, the check fails. When the git folder is under a temporary folder while the checkout is not (a separate git folder, or a linked worktree of a repository there), the check fails at once with `the git folder <dir> is under a temporary folder (<root>) while the checkout <top> is not`; when the repository itself is in a temporary folder, it proceeds, since the `tests` job would see the same kind of path there. Cargo reads `.cargo/config.toml` (or `.cargo/config`) in every folder above the one it builds in, and a folder above the work folder lies outside both worktrees (in CI it is the checkout, the head): its config would shape the test commit's build too, so the check fails at once with `<file> lies above the work folder <work>; ...` when any such folder holds one, except `CARGO_HOME`'s (default `~/.cargo`), which every build reads anyway. pytest also looks above the worktree, for a config file (whose folder then becomes the rootdir, and the `conftest.py` files up to it are loaded) when the worktree has none; refusing such files would refuse most Python repositories, so a declared Python test runs with `--confcutdir=<worktree>` (no `conftest.py` above the worktree is loaded) and, unless the config file pytest's discovery picks for that run lies inside the worktree, with `-c <an empty file in the work folder> --rootdir=<worktree>`, so pytest's search does not reach the checkout; a worktree's own config is still read, and its requirements (`required_plugins`, `minversion`) still fail the run as in the `tests` job. Which file that is, is asked before each run of the installed pytest's own discovery function, `_pytest.config.findpaths.locate_config` (a private function; both its signatures, of pytest 7 and of pytest 8 and 9, are handled), run from the work folder. It reads files and starts nothing: no plugin, no `pythonpath`, no `conftest.py`, so a checkout's config cannot run code in the test commit's check, and pytest's own rules (an empty `[tool.pytest]` table, which pytest 9 skips; quoted TOML keys; the file names of its version) decide. When it cannot be asked (pytest not importable, the function gone or changed, a config it cannot read), the check fails with `cannot ask pytest which config file <test> uses at <where>: <error>`. Tests: [`tests/test_acceptance_check_outside_tmp.py`](../tests/test_acceptance_check_outside_tmp.py) (the declared test: where a declared test is built) and [`tests/test_acceptance_check_work_folder.py`](../tests/test_acceptance_check_work_folder.py) (the refusals, and the cases that proceed).

**Fails at the test commit** when either holds: (a) the output contains ``error: could not compile `makiba` (test "<name>")``: the test does not compile, as an import error did in Python; only this target, since the test commit changes nothing but tests and the base builds; (b) the output contains `running N tests` with N at least 1 and Cargo exits non-zero (a failed assertion, a panic, or a crash of the test binary). Anything else does not count as failing and fails the check: exit 0 (the test passes), `running 0 tests` (nothing ran, as pytest's exit 5), or a non-zero exit with neither (a fetch, lock-file or library build failure: the check prints the last lines and says it could not run the test).

**Passes at head** only when Cargo exits 0 and the `test result: ok.` line reads P passed, 0 failed and 0 ignored, with P at least 1. An ignored test at head fails the check; so does a target that runs no test.

**A Python test** (`tests/**/test_*.py`) runs as before: `python3 -m pytest <path>` from the tree root; exit 5 (no test collected) is not failing.

## One rule for the checker and the merge gate

Rule 2 is held in one place: the function `refused_in_test_commit` of `scripts/acceptance-check.sh`. `bash scripts/acceptance-check.sh --test-commit <sha>` prints the paths that commit changes and may not (neither under a path it declares nor a test helper), one per line, sorted; it exits 0 when there is none, 1 when there are some, 2 on a usage error or an unknown commit. It runs no test. Both read a declared path the same way, trimmed and without empty or `.` segments (`./tests/a.rs` declares `tests/a.rs`), and read file names unquoted, so a helper with a non-ASCII name is a helper.

The merge gate in makibad (`src/daemon/gate.rs`, condition (a)) runs that mode from the copy of the script built into makibad, in the project's clone with git hooks off, and refuses a manifest commit with any such path ("also changes undeclared paths"). So a helper added to `tests/common/` in the test commit passes both, and a code change, a `pyproject.toml`, a workflow, or an undeclared change to an existing `tests/calc.test.js` in the test commit is refused by both (Issue 361: PR 353 passed the checker and was refused by the gate; Issue 372). Test: [`tests/acceptance_helper_by_place.rs`](../tests/acceptance_helper_by_place.rs). Every path the test commit changed is then protected by the gate as well, is part of the digest the gate pins at its first verdict (so a force push that rewrites only a helper is seen), and is not reported as a test edited after the test commit. A declaration of nothing but `.` segments fails both. The gate's copy is the one makibad was built with; CI takes the base's copy: they agree while makibad is built from the base it judges. Test: [`tests/acceptance_rule_shared.rs`](../tests/acceptance_rule_shared.rs).

## Is the head its test commit alone (`--scope`)

`bash scripts/acceptance-check.sh --scope <base> <head>` says whether the head is a test commit and nothing else, and which tests it declares ([`docs/design/test-commit-scoped-ci.md`](design/test-commit-scoped-ci.md), 3.1). It runs no test. It prints `test-commit <head sha>`, then one line per declared test, `rs <Cargo target name>` or `py <path>` (a declared folder: one `py` line per `test_*.py` under it; a declared helper: no line), exactly when all of these hold:

1. the head's own commits (`git rev-list --first-parent <head> --not <base>`, as the main mode reads them) are exactly one, the head itself;
2. it is not a merge;
3. it declares `Acceptance-Test` paths;
4. it changes nothing rule 2 refuses (`refused_in_test_commit`);
5. every declared path is, at the head, a test target or a test helper by rule 3's classification, and at least one is a target.

Otherwise it prints one line, `full: <reason>`: a head with more than one own commit (a fix, or a base merge after the test commit), a merge, no declaration, a test commit that also changes code, a declaration of only helpers, a missing declared path. It exits 0 in both cases, and 2 on a usage error, an unknown commit or a git error. Rule 3's classification is one function, `classify`, which the main mode (in the test commit's tree) and `--scope` (in the head's tree, a temporary worktree) both call, so they agree on what a test target is. CI runs the base's copy of this mode to decide whether a pull request's head runs only its declared tests (below). Test: [`tests/test_acceptance_scope.py`](../tests/test_acceptance_scope.py).

## At a test-commit head

A pull request's head that is its test commit alone runs only its declared tests (Issue 481, [`docs/design/test-commit-scoped-ci.md`](design/test-commit-scoped-ci.md) 3.2). The job `scope` of `ci.yml` decides, on every event:

- On any event other than `pull_request` (`push` to main, `merge_group`) it writes `test_commit=false` and reads nothing.
- On `pull_request` it checks out the head with full history and runs the **base's** `scripts/acceptance-check.sh` (`git show <base sha>:scripts/acceptance-check.sh`; no fallback to the pull request's copy) as `--scope <base sha> <head sha>`. It writes `test_commit=true`, and the target lists `rs` and `py`, only when that exits 0, its first line is exactly `test-commit <head sha>`, and every other line is `rs <name>` or `py <path>` with a name of `[A-Za-z0-9_./-]`. Anything else (no checker at the base, a base checker without `--scope`, exit 2, `full: <reason>`, any other line) writes `test_commit=false` and prints why. The step does not fail on the checker's answer.

`tests`, `real-base` and `acceptance` take `needs: scope` and `if: ${{ !cancelled() && ... }}`, so a failed `scope` job does not skip them: its output is then empty, they run in full, and the failed `scope` check fails the run. `lint` has no `needs` and runs as before. With T = `github.event_name == 'pull_request' && needs.scope.outputs.test_commit == 'true'` (the exact string), every shortened step holds only under T and every full step only under its negation:

| Job | At T | Otherwise |
|---|---|---|
| `tests` | the same checkout, cache and Python setup, then one step: `cargo test --locked --no-fail-fast --test <n> ...` for every declared Rust target, then `python3 -m pytest -q <paths>` for the declared Python tests whatever Cargo returned (a kind with none declared is not run); it fails when either failed | `cargo test --locked`, then `python3 -m pytest -q tests` |
| `real-base` | one step that prints why and succeeds; no install, no build, no negative check | the whole suite on the real base, as above |
| `acceptance` | one step that prints why, then `ACCEPTANCE: fail`, and exits 1; no checkout, no build | the checker on `pull_request`, the judged-at-head step on `merge_group` |

`acceptance` fails at T, and is not skipped, so a head that ran only its declared tests never reads as passed: a skipped job reads as passing to branch protection and as `skipping` to makiba's PR watch. At M the checker could only fail anyway (head is the test commit). The declared tests are judged by `acceptance` when the fix is pushed, and that head, like every head past the test commit, runs in full. Tests: [`tests/test_ci_test_commit_scope.py`](../tests/test_ci_test_commit_scope.py) (the `scope` step and the shortened `tests` step run in temporary repositories) and [`tests/test_ci_merge_group.py`](../tests/test_ci_merge_group.py).

## Merge order

A declared test cannot change after its test commit (rules 2 and 4). So a pull request that adds or tightens a check that test targets must pass would leave every other open pull request with a test commit unable to pass it, as PR 314 (a scan over `tests/` and its helper `tests/common/makibad.rs`) did to PR 315 and PR 316, which were rebuilt. The rule (Issue 331, [`docs/design/merge-order.md`](design/merge-order.md)): such a pull request is not merged while another open pull request has a test commit; it waits until those land, or the person decides otherwise. No check gets an exception.

makiba's merge gate holds it, git only, since makibad runs no project code. A pull request is taken as adding a check when its own changes (from its merge base with the base) touch `tools/`, `.github/`, `tests/common/` or `scripts/acceptance-check.sh` (`CHECKS` in `src/daemon/landing.rs`). The other open pull requests are those makibad follows in the repository into the same base branch, not read closed; for each, the gate fetches its head and its base and looks for its test commit. One with a test commit holds the merge, and so does one it cannot read. Under `merge_approval_mode` `acceptance` the gate refuses with verdict `merge_order`, naming them; under `human` it warns and the person decides. Test: [`tests/merge_order.rs`](../tests/merge_order.rs).

Limits: whether the check would really refuse the other test is not judged (every open test commit holds); two pull requests that both add a check and both have test commits hold each other, and the person decides their order; a check added without touching those paths (a new test target that scans `tests/` alone) is not seen; pull requests makibad does not follow are not seen.

An order the person sets (Issue 338, [`docs/design/merge-order.md`](design/merge-order.md) section 6) is a second reason for the same hold: "pull request A merges only after Issue or pull request B has landed", recorded with the `merge` tool's action `order`. While B has not landed (read from makiba's own records of landings), the gate refuses A with verdict `merge_order` under either merge approval mode, naming B; once B has landed it allows A as before. `makiba status --json` shows the order on A (`pull_requests[].merge_order`). Test: [`tests/merge_order_person.rs`](../tests/merge_order_person.rs).

## The landing gate

[`.github/acceptance-check`](../.github/acceptance-check) holds one line, `acceptance`: the name of the job above. When it is on the base branch, claude-org-ja's landing gate (`tools/acceptance_gate.py`) takes that job's success at head as the test-first evidence. Rename the job and the file together; `tests/test_acceptance_check.py` checks that they match.

## Settings only the owner can make

Branch protection on `main`: require the checks `acceptance`, `tests` and `lint`, and require branches to be up to date before merging.

## Known limits

- A pull request without a declaration passes (`none`).
- The places of test helpers are fixed (`tests/common/`, `tests/fixtures/`), not set per project. A helper kept elsewhere (`tests/support/`, a module of a multi-file Cargo target beside its `main.rs`, `tests/conftest.py`, a data file at the top of `tests/`) must be declared when a test commit changes it.
- On the merge queue's commit the declared test is not run again (above). Another entry in the queue can drop a declared test from discovery and the queue's `tests` and `real-base` still pass, so the commit that lands is not shown to run the declared test. Accepted by the owner (merge-wait.md, Q2), not fixed.
- A test that needs a new dev-dependency cannot add it in the test commit (rule 2), so it fails to compile there for that reason and counts as failing; the reviewer sees it in the log.
- `rust-toolchain.toml` is not protected by rule 4; a change to it after the test commit is a review matter.
- The checker is taken from the base revision, but the workflow file is read from the pull request, so a PR that edits the `acceptance` job itself is not stopped by the job. Review of `.github/` changes stays with the person.
- A pull request that changes the checker or CI after its test commit fails rule 4 of its own `acceptance` job; the owner lifts the `acceptance` requirement of branch protection for it, and a person merges it. The pull requests of Issue 514 and Issue 576 are such: each changes the checker after its test commit and lands by the lift. Once the workflow passes `--approvals` (Issue 593), a failure of an allowed kind lands instead on the owner's signed approval of that head (above). Issue 589's own pull request is the last checker change this route needs to land by the lift. A change under `.github/` keeps the lift.
- `acceptance` passing no longer proves that no test helper changed after the test commit: the helper line in its log says which did, and makiba's merge gate asks the person's key for them. A web-UI merge, or claude-org-ja's landing gate (which reads only the check), sees them only in the log.
- A test helper that is test infrastructure (a name of `INFRA_NAMES` or `INFRA_PREFIXES`, wherever it is under `tests/`) keeps the lift; so does a helper the test commit changed.
- A base merge made with other merge settings than merge-tree's defaults (a merge driver in `.gitattributes`, renormalization) can differ from merge-tree's tree and then counts as the branch's own.
- At a test-commit head the other test targets are not run (`lint` only builds them), and nothing runs on the real base. A test helper that breaks them is found at the fix head; one the test commit added or changed can no longer change there.
- The shortened `tests` runs on GitHub's merge of the head into the current base, as the full job does, while the checker builds the test commit itself. When the base has moved on, `tests` at the test commit can pass (the base already does what the test asserts; the checker then fails at the final head) or fail for the base's reason.
- The scope is read from the payload's base. A pull request whose base changes after the run keeps that run's scope.
- A pull request that edits `ci.yml` after its test commit changes what its later heads run (the limit above: the workflow is read from the pull request); rule 4 fails it.
- The PR watch's notice at the test commit does not say whether the declared tests passed there; the `tests` job at that head does.
- Every run, fix heads and the merge queue's commit included, waits for the `scope` job before `tests`, `real-base` and `acceptance` start.
