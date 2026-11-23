# The CI run at a test commit (Issue 391)

Decision note for Issue 391. It picks how makibad records the failed CI run at a test commit (the commit with the `Acceptance-Test:` trailer, pushed alone, M below), and what that does to the merge gate's evidence. Goal clause G2: the flow of work does not wait for the person's turn, nor for a result nobody needs.

## 1. What is true today

- **The wait.** `src/daemon/prwatch.rs` (`poll`) concludes a watch only when no check is pending: "a failed check with a sibling still running waits for the sibling" (the carried reading; spec section 5.6, the row "Checks decided"). The secretary is told, and `gate::before_ci` lets the rest of the branch go, only after that `ci_completed`. At PR 390's test commit 75e0b29, `acceptance` failed at 18:27:50 and `real-base` ended at 18:36:04: about 8 minutes of waiting. PR 389's test commit took about 11.5 minutes from push to notice.
- **Who reads the failed run at M.** Three readers, all through `door::failed_heads` (the heads with a failed `ci_completed` of the pull request):
  1. `gate::before_ci` (Issue 307), at `pr opened|pushed` and at `push`: a head past M is refused until a failed run is recorded at M. It reads the status only (`failed`), not which check failed.
  2. The merge gate, condition (b), path 1 (`src/daemon/gate.rs`): when the base names a check in `.github/acceptance-check` and `.github/workflows/` at head equals the base, every run of that check at head must have succeeded. This path does not read the run at M at all.
  3. The merge gate, condition (b), path 2 (no named check, or the workflows changed): a failed `ci_completed` at M is recorded, and the same CI job (app, check name, workflow) concluded `failure` at M and `success` at head, both against one base. The job conclusions are read live from GitHub (`ci.check_runs`, `ci.run_bases`) when the gate runs, not from the ledger.
- **What the `acceptance` job at M shows.** At M, head is M. `scripts/acceptance-check.sh` runs the declared target at "the test commit" and at "head", the same commit. If the test fails at M it then "fails at head"; if it passes at M it "passes at the test commit". Either way the job ends `failure`. So the conclusion of `acceptance` at M says nothing about the declared test; only its log does. The same holds of `tests` and `real-base`: they fail at M for the declared test, or for a broken build, and their conclusion cannot tell which.
- **Where the declared test's failure is proven.** In makiba's own repository (it has `.github/acceptance-check`), by the `acceptance` job at the final head: the checker, taken from the base, builds M in a clean worktree on GitHub's runner and requires the declared target to fail there by a compile error of that target or by a run of at least one test with a non-zero exit, never by a fetch, lock-file or library build failure (docs/acceptance-check.md, rule 3). Path 1 of the gate takes that job's success at head. A lint failure, or a broken build, at M is not that proof under any option below.

So the failed `ci_completed` at M is an ordering token (Issue 307: M was pushed and checked alone before the rest), not the evidence of the test's failure. Path 2 is the one place where the conclusions at M are evidence, and they are read from GitHub, later.

## 2. Options

### A. makibad concludes a test-commit watch at its first failed check

A watch of a head that is the branch's test commit concludes `failed` at the first reading with a failed check (check run or commit status), without waiting for its siblings. Later results of that head are not polled; the gate still reads them from GitHub when it runs.

- **What the gate reads as "a failed run at the test commit".** Unchanged: a `ci_completed` with `status` `failed` at M (`failed_heads`). The verdict is the same one the watch would reach by waiting: `summarize` says `failed` as soon as one check failed, and no later result of the same attempt turns a failure into a pass (a rerun is a new activation, Issue 375). Only `fail_count` and `pending_count` differ (`pending_count` may be above 0).
- **A failure for another reason.** A lint failure at M concludes the watch as it does today; it is still only the ordering token. Path 1 proves the test's failure from the checker at head; under path 2 the job flip is read from GitHub after M's jobs have ended (they are not cancelled: `.github/workflows/ci.yml` has no `concurrency` group). A is no weaker than C on either path.
- **Time.** The notice comes at the first failure: at 75e0b29 that is `acceptance` at about 1.5 minutes, instead of about 8.
- **Every project.** It is makibad's, so it holds for every project makibad watches, not only makiba's own CI.

### B. CI itself runs less at a test-commit head

At a head whose last commit carries the trailer, CI runs only the jobs needed (or cancels the others when `acceptance` fails).

- **Gate.** Skipped jobs read `skipping` and cancelled ones `fail` in `bucket`; the watch's verdict is still `failed`, so path 1 is unchanged. Path 2 loses evidence: a job skipped or cancelled at M has no `failure` to flip, so a project without `.github/acceptance-check` could be refused where it passes today.
- **Cost.** An edit of `.github/workflows/ci.yml` (review of `.github/` stays with the person; the acceptance check's rule 4 makes a pull request that edits it need the owner), reading a commit's trailers inside a workflow, and it helps makiba's own repository only. It saves CI minutes, which G2 does not ask for.

### C. Keep waiting for every check

The gate's evidence is as today. Every test commit waits for its slowest job (8 to 11.5 minutes measured), against G2.

### D. makibad runs the declared test at M itself (the owner's comment)

makibad builds M in a clean checkout of the project's clone, outside the worker's sandbox, runs the declared target, and records that failed run as the gate's evidence in place of CI's.

- **What it proves that CI's run does not.** Only that the test fails on the owner's machine. CI's run proves the same on a runner nobody in makiba controls, from the base's checker; that is the G5 property (what must not happen is stopped by something outside the agent), and makibad is outside the agent too, but it is not outside the owner's machine.
- **What it costs.**
  - It runs code a worker wrote (a test, a `build.rs` at the base, any test helper) outside every sandbox, as the user that holds the person key, the merge gate's key and the person's `gh` and git credentials. A test commit may change only tests, but a test is arbitrary code. This is a new trust boundary, and today makibad runs no project code at all (the gate "runs no test").
  - A cold `cargo build` and the run of one target at M: minutes of CPU on the machine where the workers already build, for every test commit of every project. Socket and real-base targets need the base installed for that project.
  - A record of a new kind (the local run's result), read by `before_ci` and the gate in place of, or beside, `ci_completed`: a ledger and gate change, and every project needs makibad to know how to run its tests.
- **Time.** No faster than A: the `acceptance` job fails at about 1.5 minutes, a local cold build of makiba takes as long or longer.
- **Gate.** CI's run at M would still happen (the push of M alone is what starts it); path 1 would not change, so D adds a second proof without removing a wait A does not remove as well.

## 3. Decision: A

A takes the wait away for every project, leaves every reader of the failed run at M reading what it reads today, and runs no new code on the owner's machine. B is a possible later saving of CI minutes for makiba's own repository, not needed for G2. D adds a trust boundary and machine load for no gain in time or in proof.

### 3.1 What a "test-commit head" is, and who decides it

The door decides it, in `door::pr_before_ci`, which already fetches the pull request into the project's clone and runs `gate::find_manifest(clone, base, head)` through `gate::before_ci`. A recorded head is a test-commit head when all hold:

1. the repository is a project's clone with an origin naming it (otherwise `pr_before_ci` lets the record through unchecked, and the head is not marked);
2. `find_manifest(base, head)` finds M and M equals the head;
3. the base names a check in `.github/acceptance-check` (`gate::repo_check`), and `.github/workflows/` at the head equals the base's (the two conditions of path 1, `gate.rs` at the `workflows(base)? == workflows(head)?` test).

Condition 3 keeps A to the heads whose pull request the gate judges by path 1, where the run at M is only an ordering token. A project without a named check, or a test commit whose workflows differ from the base's, is judged by path 2, whose evidence is the conclusions at M; there a CI that cancels M's jobs when the next head is pushed would turn early notice into a refusal, so such a head keeps waiting (C). Condition 3 is read at M: a later commit that changes `.github/workflows/` also sends the gate to path 2 after M was concluded early. In makiba's own repository the acceptance check's rule 4 refuses such a pull request anyway; elsewhere it is a known limit (section 5).

The door passes the mark to `prwatch::register_with`, as it passes `base_merge` (Issue 358): the `pr_opened` or `fix_pushed` event carries `test_commit` `true`. Nothing else stores it.

### 3.2 The owner of the early conclusion

`src/daemon/prwatch.rs`, the existing owner of the CI reading. No second watcher, poller or record. In `PrWatch::pass`, beside `is_rerun`, a reader `is_test_commit(repo, pr, head)`: some `pr_opened` or `fix_pushed` of that pull request (repository compared as `door::same_pr` does) at that head carries `test_commit` `true`. It is passed to the poller with `rerun`. In `poll` (not `poll_rerun`: a rerun keeps Issue 375's rules), when the watch is of a test-commit head and a reading has `fail_count > 0`, it concludes `Concluded(summary)` with that reading, pending checks and all. A reading with no failure behaves as today (a pass still waits for every workflow run of the head; a test commit whose checks all pass concludes `passed`, and `before_ci` keeps refusing the rest, as today).

`apply` is unchanged apart from the notice text, which for an early conclusion says: "CI failed for <repo>#<pr> at <7>: <k> of <n> checks failed, <p> still running; a test commit needs only a failed run (Issue 391), the rest is not waited for."

### 3.3 State, ordering and restart

No state in memory beside the ledger. The mark is read from the ledger at each activation, as `is_rerun` is, so a restart of makibad starts the poller with the same mark. A head recorded before this change has no mark and is watched as today.

### 3.4 Files the build changes

- `src/daemon/door.rs`: `pr_before_ci` returns the mark beside `base_merge`; the `pr opened|pushed` path passes it to `register_with`.
- `src/daemon/gate.rs`: one function for conditions 2 and 3 of section 3.1 (it uses `find_manifest` and `repo_check`, which are there); no change to what the gate judges.
- `src/daemon/prwatch.rs`: `register_with` writes `test_commit`; `is_test_commit`; `poll` takes the mark; the notice text; the head comment names Issue 391 beside the carried reading.
- `docs/design/spec.md`: section 5.6 (the early conclusion), the row "Checks decided" (a test-commit head concludes at its first failure), the event table row `pr_opened` / `fix_pushed` (`test_commit` `true`), and the `pr` verb row in section 12 (the door marks the head). The event kinds do not change, so `src/spec.rs` and `tests/spec_matches_code.rs` do not change.
- A new test, `tests/pr_watch_test_commit.rs`, declared by the build Issue's test commit. No existing test changes: `tests/pr_watch.rs` records heads of a repository that is no project's, so they are never marked and its test "a red check waits for its running sibling" still holds. No file under `.github/`, no script, no skill.

## 4. The build Issue's declared test

`tests/pr_watch_test_commit.rs`, built as `tests/pr_open_test_commit.rs` builds its world (a local origin publishing `refs/pull/<n>/head`, the project's clone in the repositories folder, a `gh` stand-in answering `pr view`, `check-runs`, `status` and `actions/runs` from files), with every helper inside the file. Its assertions:

1. A base holding `.github/acceptance-check` (`acceptance`), a branch whose only commit M declares `tests/a.rs`; `pr opened` at M; the stand-in answers `acceptance` `failure`, `tests` and `real-base` `in_progress`. Within the poll budget the watch row of M is `completed` with conclusion `failed`; one `ci_completed` at M with `status` `failed`, `fail_count` 1, `pending_count` 2; one notice to the secretary naming M; the `pr_opened` event carries `test_commit` `true`.
2. Then `pr pushed` at a head past M is let through (`before_ci` reads the early failed run).
3. The same world, but the base has no `.github/acceptance-check`: with `acceptance` `failure` and `tests` `in_progress`, the watch of M stays `watching` after several polls, and concludes only once `tests` has concluded; `pr_opened` carries no `test_commit`. The same holds when the base names the check but M's `.github/workflows/` differs from the base's (a test commit cannot change it under rule 2 of the checker, so the world's base is moved on after M instead: M's parent holds another workflow file than the base the pull request reads).
4. A head past M on a base with `.github/acceptance-check` (not the test commit), one check `failure` and one `in_progress`: the watch stays `watching` until the sibling concludes (the carried reading, unchanged).
5. Restart: with M's watch `watching` and the checks still all `in_progress`, makibad is stopped and started again, then `acceptance` turns `failure` with the others still running: the watch concludes `failed` early (the mark is read from the ledger).
6. A test commit whose checks all pass concludes `passed` only when no workflow run is going (unchanged), and a head past it is still refused.

## 5. Known limits

- The notice comes at the first failure of any check, which at M may be a broken build or a lint error rather than the declared test. That is no change in what the gate proves (section 1); a test commit whose test does not fail at M is still refused at the end by `acceptance` at head, only later than a person reading M's log could have seen.
- A project without `.github/acceptance-check`, and a test commit whose workflows differ from the base's, keep today's wait (section 3.1, condition 3).
- Condition 3 is read at M. A pull request that changes `.github/workflows/` after M is judged by path 2 with M concluded early; if its CI cancels M's running jobs on the next push, the gate refuses it (never a false pass). makiba's own checker refuses such a pull request by rule 4.
- Path 2 already counts any job that failed at M and succeeded at head, a lint job included. That is a weakness of today's gate, not of this change: proposed as a separate Issue.
