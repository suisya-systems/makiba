# A test commit's CI runs only its declared tests (Issue 478)

Decision note for Issue 478. It picks how makiba's own CI (`.github/workflows/ci.yml`) runs at a test-commit head (the pull request's first commit, holding only the declared tests, pushed alone; M below): only the declared tests, in minutes, while the acceptance check and both paths of the merge gate keep what they judge. Goal clause G2: the flow of work does not wait for the person's turn. Background: [test-commit-ci.md](test-commit-ci.md) (Issue 391, the two gate paths, the cancellation note) and [../acceptance-check.md](../acceptance-check.md).

## 1. What is true today

- **The run at M.** On 2026-11-24 the test commits of about fifteen tasks each spent a full run: `tests` (the whole suite, about 9 minutes), `real-base` (the whole suite on the real base, about 12.5 minutes), `lint` and `acceptance`. PR 473 (Issue 471) added a Rust cache, one run per head, and a concurrency group per pull request that cancels a superseded run.
- **What M's run is for.** Three readers use it, all through `door::failed_heads` (test-commit-ci.md section 1):
  1. `gate::before_ci` refuses a head past M until a failed `ci_completed` is recorded at M. It reads only the status.
  2. Gate path 1 (`gate::evaluate`): the base names `acceptance` in `.github/acceptance-check`, `.github/workflows/` at head equals the base's, and every run of `acceptance` at head succeeded. It does not read M's run at all. The `acceptance` job at head runs the base's `scripts/acceptance-check.sh`, which builds M itself in a clean worktree and requires the declared targets to fail there (rule 3).
  3. Gate path 2: a failed `ci_completed` at M, and some job (app, name, workflow) concluded `failure` at M and `success` at head, on one base. It reads M's job conclusions from GitHub when the gate runs.
- **The door's precondition.** Before either path, the `gate` verb refuses a head whose latest `ci_completed` is not `passed` (test-commit-ci.md section 1). So the head the gate can pass is a head where every check passed or was skipped.
- **`acceptance` at M always fails.** At M, head is M: the checker runs the declared target "at the test commit" and "at head", the same commit, so it fails either way (it fails at head, or it passed at the test commit). Its conclusion at M says nothing; its log does.
- **M's tree.** Rule 2 of the checker and the gate's condition (a) allow M to change only declared paths and test helpers. Everything else at M (the library, the binaries under `src/`) is M's parent's; helpers may be code, such as `tests/common/fake_agent.rs`, built as a binary.
- **The wait.** `prwatch::poll_into` concludes a head only when no check is pending ("a failed check with a sibling still running waits for the sibling"). Issue 391 decided to end a test-commit head's watch at its first failure (option A), but that is not built: the sibling wait is what keeps M's run complete before a fix head is pushed, and with it path 2's evidence, now that a newer push cancels an older head's run.

## 2. Options

- **A (the Issue's).** CI detects a test-commit head and runs only the declared tests in `tests`, skipping or shortening `real-base`; `lint` and `acceptance` as today.
- **B.** makibad marks the head as a test commit and passes that to CI. makibad cannot: a `pull_request` run takes no input, makibad holds no GitHub write credential (the secretary pushes), and a mark set outside the run (a label, a status) would be a second source of truth that a rerun or a hand push does not carry. Rejected.
- **C.** Keep the full run and make it cheaper (the cache of PR 473). It leaves `real-base` at about 12.5 minutes on every test commit. Rejected as the answer; the cache stays.
- **A' (chosen).** A, with three changes:
  1. `acceptance` at a test-commit head does not run as today (two cold builds of the same commit that can only end `failure`) and is not skipped: it **fails at once**, without building, with a line saying why. This keeps its conclusion at M what it is today, and it is the guard of the whole design: no head that ran the shortened suite can ever read as passed (section 4, I2).
  2. `real-base` at a test-commit head runs nothing and succeeds; the declared tests run once, in `tests`, on the stand-in base, which is where the checker judges them too.
  3. The recognition is one function of the checker (`scripts/acceptance-check.sh --scope`), taken from the base, read once per run by a small `scope` job whose output the other jobs read; any doubt reads as "full".

## 3. Decision: A'

The person approved this design on 2026-11-25, and decided against building Issue 391's option A (3.4).

### 3.1 How a test-commit head is recognised, and by whom

**By CI only**, in one place. makibad does not recognise anything new: `before_ci`, the PR watch and the gate are unchanged (section 3.4), and the proof in section 4 does not depend on makibad agreeing with CI.

A new job `scope`, the only one the others depend on, runs on every event:

- On any event other than `pull_request` (`push` to main, `merge_group`) it writes `test_commit=false` and reads nothing: the merge queue's commit and main are never shortened, whatever their commits say.
- On `pull_request` it checks out the head (`github.event.pull_request.head.sha`, `fetch-depth: 0`), takes `scripts/acceptance-check.sh` **from the base** (`git show <base sha>:scripts/acceptance-check.sh`, base being `github.event.pull_request.base.sha`; no fallback to the pull request's copy, so a pull request cannot choose its own scope), and runs `bash <copy> --scope <base> <head>`.
- It writes `test_commit=true` only when that exits 0 and its first line is exactly `test-commit <head sha>`; then it also writes the Cargo target names (`rs`) and Python paths (`py`) the checker printed. Every other outcome (exit 2, a base copy without `--scope` as on the pull request that adds it, any other output) writes `test_commit=false` and prints why. The step itself never fails on the checker's answer.

The new checker mode `--scope BASE HEAD` prints `test-commit <sha>` and one line per declared test (`rs <target name>` or `py <path>`), or `full: <reason>`; exit 0 either way, 2 on a usage or git error. It answers `test-commit` only when all hold, each by the checker's existing rule and functions:

1. the head's own commits (`git rev-list --first-parent HEAD --not BASE`, as the main mode reads them) are exactly one, and it is HEAD;
2. it is not a merge;
3. it declares (`declared` is not empty);
4. `refused_in_test_commit` lists its changes and finds none (rule 2, the gate's condition (a));
5. every declared path resolves, at HEAD, to a test target or a test helper by rule 3's classification, and at least one is a target. The classification loop of the main mode becomes one function both modes call, so they cannot disagree on what a target is.

Any other case is `full`: a head with a base merge after M, a fix head, a test commit that also changes code, a declaration with no target. Those run everything, as today.

### 3.2 What each job does

`T` below is "`github.event_name == 'pull_request' && needs.scope.outputs.test_commit == 'true'`"; every shortened step is conditioned on `T`, every full step on its negation. The comparison is with the exact string `'true'`, so an empty or missing output (the `scope` job failed) runs the full suite.

| Job | At a test-commit head (`T`) | Anywhere else (fix head, any non-test-commit head, `merge_group`, `push`) |
|---|---|---|
| `scope` | answers `true`, with the targets | answers `false` |
| `tests` | the same checkout as today (GitHub's merge of the head into the base, the tree full `tests` runs on), then one step: `cargo test --locked --no-fail-fast --test <n> ...` for every declared Rust target, then `python3 -m pytest -q <paths>` for the declared Python tests whatever Cargo's status was; a kind with none declared is not run; the step exits non-zero when either did. So every declared test runs and its result is in the log, and the job's conclusion is the declared tests' result. | as today: `cargo test --locked`, then `python3 -m pytest -q tests` |
| `real-base` | one step that prints "test commit: the declared tests run in `tests`; the real base runs at the fix" and succeeds; no install, no build, no negative check | as today |
| `lint` | as today (no `needs`, so it starts at once) | as today |
| `acceptance` | one step that prints "the head is the test commit alone: the declared tests are judged by this job when the fix is pushed" then `ACCEPTANCE: fail`, and exits 1; no checkout, no build | as today: the checker on `pull_request`, the judged-at-head step on `merge_group` |

`tests`, `real-base` and `acceptance` take `needs: scope` and `if: ${{ !cancelled() && ... }}` (`acceptance` keeps its event condition inside), so a failed `scope` job does not skip them: they run full, and the failed `scope` check fails the run. `lint` is left alone: it compiles every target (`clippy --all-targets`), the one place at M where a helper that breaks another test target is seen, and a format or clippy error in a declared file must be found at M because rule 4 protects that file afterwards.

**Why `acceptance` fails and is not skipped.** A skipped job reads as passing to GitHub's required checks and as `skipping` to the PR watch, so a shortened head with a skipped `acceptance` and passing declared tests would read as passed CI on a run that never ran the suite. Failing keeps branch protection blocking, keeps M's `ci_completed` `failed` in every case (as today, so `before_ci` lets the fix through exactly when it does today), and makes I2 below hold.

**The merge queue's commit and a fix head keep the full suite.** `merge_group` and `push` are excluded by the event before any commit is read; a fix head has at least two own commits, and a head with a base merge after M has two, so condition 1 of 3.1 fails. `acceptance` on `merge_group` stays the step that prints `ACCEPTANCE: judged at the pull request's head` (Issue 401).

**Time.** At M the slowest job becomes `lint` or the shortened `tests` (one library build and the declared targets, on the PR 473 cache) instead of `real-base`; `real-base` and `acceptance` take seconds. `scope` adds its own short run before `tests`, `real-base` and `acceptance` start, on every head. The figures are not measured here: build Issue 2 records the first test commit's run after it lands.

### 3.3 The cost

Every run, fix heads and the queue included, waits for the `scope` job (a checkout of the history and one `git` call per own commit) before three of its four jobs start. The two build pull requests change the checker and `.github/` after their test commits, so each fails rule 4 of its own `acceptance` job and is merged by a person with the owner lifting that requirement (docs/acceptance-check.md, known limits).

### 3.4 What it does not change

- `src/daemon/gate.rs`: nothing. Path 1 reads `acceptance` at head, which is never a shortened head (I1). Path 2 reads M's conclusions, which only lose flips (I3). The built-in copy of the checker (`CHECKER`, `include_str!`) changes with the script, but the gate runs only `--test-commit`, whose output does not change (`tests/acceptance_rule_shared.rs` keeps holding).
- `src/daemon/prwatch.rs`: nothing. The sibling wait stays and now waits minutes at M; the extra `scope` check is one more check of the run. The rerun path (Issue 375) is unchanged.
- `src/daemon/door.rs`, the ledger, `docs/design/spec.md`: nothing.
- Issue 391's option A (end a test-commit watch at its first failure): this design does not need it. With M's run short, A saves little in makiba's own repository and brings back the hazard test-commit-ci.md section 3.1 names (a fix head pushed early cancels M's running jobs, and path 2 loses flips). The person decided not to build A (2026-11-25); the sibling wait stays.

## 4. Proof: neither gate path passes falsely or loses the failure at M

Invariants of the design:

- **I1.** A shortened run happens only on `pull_request` at a head whose only own commit is a test commit (3.1). Every other head, the merge queue's commit and main run in full.
- **I2.** At every shortened head `acceptance` concludes `failure`. So the run at that head is `failed` (the PR watch's `summarize`: any failed check), no `ci_completed` there is `passed`, and the door refuses the gate at that head before either path; branch protection, which requires `acceptance`, refuses a hand merge of it too.
- **I3.** Under shortening, the jobs that fail at M are a subset of those that fail there today: shortened `tests` runs a subset of what full `tests` runs on the same tree, so it fails only when full `tests` would (flakiness aside, which a rerun handles as in case 6); `acceptance` fails at M in both; `real-base` succeeds where today it may fail; `lint` runs as today; `scope` is new and never fails on the checker's answer. Path 2's flips (failure at M, success at head) are therefore a subset of today's.
- **I4.** M changes, relative to its parent, only declared paths and test helpers (rule 2, condition (a), and condition 4 of 3.1). Helpers are code too: `tests/common/`, fixtures, and `tests/common/fake_agent.rs`, which `Cargo.toml` builds as the binary `makiba-fake-agent`. Everything else (the library, the binaries under `src/`) is M's parent's, and in the run's checkout the base's, since GitHub merges the head into the current base. What a full run at M would find beyond the declared tests is therefore an effect of M's helpers on other tests, and the final head's full run must pass with the same helpers, which rule 4 and condition (a) keep unchanged.

From I1 and I2: the head the gate can pass ran the full suite, and path 1's evidence (the checker at that head, building M itself) is produced exactly as today. From I3: path 2 passes under the new CI only where it passes today. The cases:

1. **A declared test fails at M for another reason.**
   - *Compile error in the declared target* (a typo, not the missing fix): `tests` fails at M; the checker at the final head counts it as failing at M (rule 3a), as today, but the target must then compile and pass at head unchanged (rule 4), so a typo the fix cannot cure fails `acceptance` at head and the head is not passed. No change from today.
   - *Compile error elsewhere*: the library and `src/` binaries are not M's (I4), so their failure at M means a broken base or toolchain; a helper binary M broke (`makiba-fake-agent`) is built by `cargo test --test <n>` too, since Cargo builds the package's binaries for an integration test. Either way `tests` fails at M as the full run would, and the checker at head reports "could not run" (the failure is not the target's own compile error) and fails, as today. Another test target broken by M's helper is not built by the shortened `tests`, but `lint` (`clippy --all-targets`) builds it at M, and the final head's full `tests` must build it; repairing it means editing a protected helper (refused) or an existing test (reported by `edited_tests`, refused under `acceptance`). Seen later at worst, never passed.
   - *A broken fixture* (a helper in M): the declared test fails at M for the fixture; the fixture is protected, so the head passes only if the fix makes the same fixture work, which is the fix. Otherwise `tests` and `acceptance` fail at head.
2. **A head wrongly recognised as a test commit.** Recognition requires the checker's own definition of a test commit at that head (3.1, conditions 1 to 5), so a recognised head is a test commit by the checker's rule; whether makibad's `find_manifest` against its own recorded base agrees does not matter, because by I2 a shortened head is never passed by the gate or by branch protection. A head that is not recognised (a base merge after M, the base's checker without `--scope`, a failed `scope` job) runs in full, as today: slower, never weaker.
3. **The merge queue's commit.** `merge_group` is excluded by the event before any commit is read (I1); `tests`, `lint` and `real-base` run in full there and `acceptance` keeps its judged-at-head step. Unchanged from Issue 401.
4. **A declared test that passes at M.** Shortened `tests` succeeds; `acceptance` fails (I2), so M's `ci_completed` is `failed` and the fix may be pushed, as today. At the final head the checker finds the test passing at the test commit and fails `acceptance`, so that head is not passed. Path 2 is not reached at a failed head. Today's outcome, and today's timing (the problem is reported at the end; the `tests` job's green at M shows it earlier).
5. **Cancellation by a newer push.** The push verb refuses a head past M until a failed `ci_completed` is recorded at M, and the PR watch records it only when no check of M is pending, so M's run is complete before a fix head is pushed through makiba; the run is now minutes, so the window is shorter. A fix pushed by hand earlier cancels M's running jobs (concurrency group, Issue 471): cancelled jobs conclude `cancelled`, not `failure`, so path 2 finds fewer flips and refuses, never passes; path 1 does not read M. The same as today.
6. **A rerun.** The scope is a function of the run's fixed inputs (the event, the payload's base and head, the base's checker). A rerun of failed jobs keeps the `scope` job's outputs; a rerun of all jobs computes the same answer. So a rerun at M is shortened again and a rerun at a fix head is full again. Path 2 reads the latest completed run per job, as today: a rerun where a flaky declared test now passes removes a flip and can only make path 2 refuse.
7. **Path 2 at all, in makiba's repository.** The base names `acceptance`. At a passed head `acceptance` succeeded, so path 1 passes whenever `.github/workflows/` equals the base's; when it does not, rule 4 fails `acceptance` at head and the head is not passed (unless the pull request edits the `acceptance` job itself, the known limit of docs/acceptance-check.md). Path 2 is reached here only in that limit, and there I3 still holds for M's side.

What the gate can lose relative to today is a flip from `real-base` or from an undeclared test at M. Each matters only where the declared tests did not fail at M on the stand-in base, and then `acceptance` at head fails (case 4), so no pull request the gate passes today is refused for it.

## 5. Files the build changes

- `scripts/acceptance-check.sh`: the `--scope BASE HEAD` mode (3.1); rule 3's classification of declared paths into one function the main mode and `--scope` share. The main mode and `--test-commit` print what they print today.
- `.github/workflows/ci.yml`: the `scope` job; `needs: scope` and `!cancelled()` on `tests`, `real-base` and `acceptance`; the shortened and full steps of 3.2. `lint`, the cache, `concurrency` and `on:` unchanged.
- `tests/test_ci_merge_group.py`: `holds(cond, event)` becomes `holds(cond, event, scope)`, reading the new terms (`!cancelled()`, read as true; `needs.scope.outputs.test_commit == 'true'` and `!= 'true'`, read against `scope`) and raising on any other term as today. `problems()` judges `merge_group` with `scope` `false` (the value the `scope` job writes there) and requires there what it requires today; on `pull_request` it keeps today's requirements (checkout of the head with full history, the last step the checker on base and head) for `scope` `false` only, and for `scope` `true` requires instead that `acceptance` has no checkout and its steps exit 1 with the last line `ACCEPTANCE: fail`. The mutation fixtures whose matched text the new conditions change (`_acceptance_pr_only`, `_checkout_runs_on_queue`, `_queue_step_on_every_event`) are rewritten to the new text, and one is added: a shortened `acceptance` step that also holds on `merge_group`.
- `tests/test_acceptance_check.py`: unchanged unless the refactor of rule 3's loop needs it; its existing cases are the check that the main mode is unchanged.
- New: `tests/test_acceptance_scope.py`, `tests/test_ci_test_commit_scope.py` (the declared tests of the two build Issues).
- `docs/acceptance-check.md`: the `--scope` mode, a section "At a test-commit head" (3.2 and I2), and its known limits (section 6).
- `docs/design/test-commit-ci.md`: section 1's bullets "What the `acceptance` job at M shows" and "Where the declared test's failure is proven" say what runs at M under this design, and the note on Issue 391's option A points here (3.4). This note is linked from its head now.
- Not changed: `src/daemon/gate.rs`, `src/daemon/prwatch.rs`, `src/daemon/door.rs`, `.github/acceptance-check`, `docs/design/spec.md`, `docs/design/merge-wait.md`.

## 6. Known limits

- At M, other test targets are not run (only built, by `lint`), and nothing runs on the real base. A helper that breaks them is found at the fix head, where it can no longer be changed. Accepted: it is not evidence the gate reads, and it is found before anything lands.
- The shortened `tests` runs on GitHub's merge of M into the current base, as full `tests` does today, while the checker at the final head builds M itself. When the base has moved on since M's parent, the two trees differ: a base that already fixed what the test asserts makes `tests` green at M (case 4: refused at the final head), and a base that breaks the declared test makes it fail at M for the base's reason, which the checker's own run of M at the final head does not count. Kept on purpose: I3 compares the run with today's run on the same tree, and the gate's evidence is the checker's, not this job's.
- The recognition reads the payload's base. A pull request whose base is changed after the run keeps that run's scope; path 2 already refuses runs of M and head on different bases.
- A pull request that edits `ci.yml` after M can change what its later heads run. That is the existing limit of the workflow being read from the pull request (docs/acceptance-check.md); rule 4 fails it, and review of `.github/` stays with the person.
- The PR watch's notice at M does not say that the declared tests passed there (case 4). A worker reads `tests` at M; a notice for it is a possible later Issue.

## 7. Build Issues (in order)

1. **acceptance-check.sh says whether a head is its test commit alone (`--scope`)** (Issue 478, build 1). Definition of done: `bash scripts/acceptance-check.sh --scope BASE HEAD` prints `test-commit <sha>` and `rs <name>` / `py <path>` lines exactly when conditions 1 to 5 of 3.1 hold, and `full: <reason>` otherwise (exit 0 in both; 2 on usage or git error); rule 3's classification is one function used by both modes; the main mode's and `--test-commit`'s output unchanged (`tests/test_acceptance_check.py`, `tests/acceptance_rule_shared.rs` pass); `docs/acceptance-check.md` documents the mode. Declared test `tests/test_acceptance_scope.py` (temporary repositories, as `tests/test_acceptance_check.py` builds them): `test-commit` for a one-commit head declaring a Rust file target, a Rust folder target, a Python test, a declared directory, and a target with a helper; `full` for a head with two own commits, a base merge after M, a merge as the test commit, a test commit that changes code, a declaration of only helpers, a missing declared path, no declaration; exit 2 on bad arguments and unknown commits. Merged by a person (rule 4).
2. **CI runs only the declared tests at a test-commit head** (Issue 478, build 2; after build 1 is on main). Definition of done: `ci.yml` as 3.2; `tests/test_ci_merge_group.py` updated and passing; `docs/acceptance-check.md` and `docs/design/test-commit-ci.md` as section 5; the first test commit after it lands is seen to run the shortened jobs, and its run time is recorded in this note. Declared test `tests/test_ci_test_commit_scope.py` (reads `ci.yml` with pyyaml, as `tests/test_ci_merge_group.py` does, and runs the extracted steps in temporary repositories): the `scope` step answers `true` with the targets at a test-commit head, `false` at a fix head, on `merge_group` and `push`, when the base's checker has no `--scope` (exit 2) and when it prints anything else; `tests`, `real-base` and `acceptance` need `scope` and hold under `!cancelled()`, `lint` has no `needs`; every shortened step's condition requires `pull_request` and the exact string `'true'`; `acceptance`'s steps at a test-commit head exit 1 with the last line `ACCEPTANCE: fail` and do no checkout; the shortened `tests` step, run with stand-in `cargo` and `python3` on `PATH`, passes `--no-fail-fast` and every declared target to one Cargo call, runs pytest on the declared Python paths after a failing Cargo call, and exits non-zero when either failed and 0 only when both passed (cases: two Rust targets both failing, Rust failing with Python passing, Rust passing with Python failing, only one kind declared); and mutated copies (`!cancelled()` dropped, `acceptance` skipped instead of failing, a condition `!= 'false'`, `merge_group` shortened) are each flagged. Merged by a person (rule 4).
Issue 391's option A is not built (decided 2026-11-25, 3.4).
