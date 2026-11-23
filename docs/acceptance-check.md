# Acceptance check in CI

Carried from paddock (`docs/design/acceptance-check.md`, `scripts/acceptance-check.sh`). Issue #3; reworked for Rust tests.

## What runs

The `acceptance` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on every pull request. It installs the toolchain of `rust-toolchain.toml` and Python, then runs:

```sh
bash scripts/acceptance-check.sh <base sha> <head sha>
```

[`scripts/acceptance-check.sh`](../scripts/acceptance-check.sh) takes the branch's own commits: the head's first-parent line that the base cannot reach. The oldest of them is the test commit (its parent is in the base). If it carries `Acceptance-Test: <path>` trailers, the check fails unless all of these hold:

1. The test commit is not a merge, and no other own commit declares. So no join commit comes before the test commit.
2. The test commit changes only declared paths and test helpers. This is the one rule of what a test commit may change; the merge gate judges by it too (below).
3. At least one declared path is a test target; every declared target fails at the test commit and passes at head (below).
4. The own commits after the test commit leave the declared paths, test helpers, `.github/` and the checker itself unchanged. New test targets may be added.

The base may be merged into the branch after the test commit; a merge that brings in anything not in the base fails. A base merge counts in rule 4 only for files it leaves different from every parent (its own conflict resolution). Renames count as both paths.

No trailer: the job passes and prints `ACCEPTANCE: none`. The last line of the log is always `ACCEPTANCE: pass`, `none` or `fail`. Tests: [`tests/test_acceptance_check.py`](../tests/test_acceptance_check.py).

## Rule 3

**Test targets.** A declared path is a test target: a file `tests/<name>.rs` directly under `tests/`, or a folder `tests/<name>/` holding `main.rs` (or that `main.rs` itself); its target name is `<name>`. For the development tools, a file `tests/**/test_*.py` is also a declared test (a declared folder: every `test_*.py` under it). A declared path that is neither a target nor a test helper, at the test commit, fails the check; so does a declaration with no target at all. Test helpers are the other files under `tests/` (`tests/common/`, fixtures, the test agent's source): the test commit may add them, declared or not, and rule 4 protects them. A declared helper is not run and need not fail at the test commit.

**How a target runs.** In each of the two worktrees (test commit and head), with `CARGO_TERM_COLOR=never`: `cargo fetch --locked` first, so that a download failure is not mistaken for a test result; then `cargo test --locked --test <name>`, with stdout and stderr kept. Each worktree builds into its own `target/` (the checker sets `CARGO_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` to it, whatever the environment or a cargo config file says, and prints the folder of each build): one folder shared by the two worktrees, or by two runs, would let Cargo reuse a build made from the other's sources (Issue 232). Python tests run with `CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR` and `CARGO_BUILD_BUILD_DIR` unset. With `--test` Cargo builds the library, the binaries and that one test, and runs no other test and no doc test, so the output holds at most one `running N tests` line and one `test result:` line.

**Fails at the test commit** when either holds: (a) the output contains ``error: could not compile `makiba` (test "<name>")``: the test does not compile, as an import error did in Python; only this target, since the test commit changes nothing but tests and the base builds; (b) the output contains `running N tests` with N at least 1 and Cargo exits non-zero (a failed assertion, a panic, or a crash of the test binary). Anything else does not count as failing and fails the check: exit 0 (the test passes), `running 0 tests` (nothing ran, as pytest's exit 5), or a non-zero exit with neither (a fetch, lock-file or library build failure: the check prints the last lines and says it could not run the test).

**Passes at head** only when Cargo exits 0 and the `test result: ok.` line reads P passed, 0 failed and 0 ignored, with P at least 1. An ignored test at head fails the check; so does a target that runs no test.

**A Python test** (`tests/**/test_*.py`) runs as before: `python3 -m pytest <path>` from the tree root; exit 5 (no test collected) is not failing.

## One rule for the checker and the merge gate

Rule 2 is held in one place: the function `refused_in_test_commit` of `scripts/acceptance-check.sh`. `bash scripts/acceptance-check.sh --test-commit <sha>` prints the paths that commit changes and may not (neither under a path it declares nor a test helper), one per line, sorted; it exits 0 when there is none, 1 when there are some, 2 on a usage error or an unknown commit. It runs no test. Both read a declared path the same way, trimmed and without empty or `.` segments (`./tests/a.rs` declares `tests/a.rs`), and read file names unquoted, so a helper with a non-ASCII name is a helper.

The merge gate in makibad (`src/daemon/gate.rs`, condition (a)) runs that mode from the copy of the script built into makibad, in the project's clone with git hooks off, and refuses a manifest commit with any such path ("also changes undeclared paths"). So a helper added to `tests/common/` in the test commit passes both, and a code change, a `pyproject.toml` or a workflow in the test commit is refused by both (Issue 361: PR 353 passed the checker and was refused by the gate). Every path the test commit changed is then protected by the gate as well, and is not reported as a test edited after the test commit. The gate's copy is the one makibad was built with; CI takes the base's copy: they agree while makibad is built from the base it judges. Test: [`tests/acceptance_rule_shared.rs`](../tests/acceptance_rule_shared.rs).

## The landing gate

[`.github/acceptance-check`](../.github/acceptance-check) holds one line, `acceptance`: the name of the job above. When it is on the base branch, claude-org-ja's landing gate (`tools/acceptance_gate.py`) takes that job's success at head as the test-first evidence. Rename the job and the file together; `tests/test_acceptance_check.py` checks that they match.

## Settings only the owner can make

Branch protection on `main`: require the checks `acceptance`, `tests` and `lint`, and require branches to be up to date before merging.

## Known limits

- A pull request without a declaration passes (`none`).
- A test that needs a new dev-dependency cannot add it in the test commit (rule 2), so it fails to compile there for that reason and counts as failing; the reviewer sees it in the log.
- `rust-toolchain.toml` is not protected by rule 4; a change to it after the test commit is a review matter.
- The checker is taken from the base revision, but the workflow file is read from the pull request, so a PR that edits the `acceptance` job itself is not stopped by the job. Review of `.github/` changes stays with the person.
- A pull request that changes the checker or CI after its test commit fails rule 4 of its own `acceptance` job; the owner lifts the `acceptance` requirement of branch protection for it, and a person merges it.
