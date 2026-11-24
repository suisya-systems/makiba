# Open test commits told of each landing that may change their checks (Issue 461)

Design note for Issue 461, follow-up of Issue 379. Goal clause G3: past incidents do not come back.

## 1. What happened, and what is true today

- PR 454 (Issue 419) declared `tests/base_version.rs` in its test commit M on base 02212db. While its fix was written, PR 437 (Issue 379) landed `tools/check_test_executables.py`, which the `tests` job runs (through `tests/test_check_test_executables.py`) over every `*.rs` file under `tests/`. M's declared test made executables in its own process (`fs::copy` at line 281, a `0o755` mode at line 412). A declared test cannot change after its test commit (`scripts/acceptance-check.sh`, rule 2; the gate's `test_commit_refusals`), so no fix commit could repair it: the failure showed only at the fix head's CI, the pull request was closed unmerged and the Issue dispatched again.
- What makibad sees of a landing today: the PR watch's state poller (`src/daemon/prwatch.rs`, Issue 388) writes `pr_merged` (`repo`, `pr`, `merge_commit`) for each pull request a run follows. The belt's clone check (`src/daemon/belt.rs`, `in_clone`, Issue 284) fetches the default branch of the project's clone from its `origin` into `refs/remotes/origin/<branch>` and tells whether a merge commit is in it; only design-only runs' dependencies are checked (`landed_in_clone`), and `ahead.rs` tells those runs of the landing. No open run with a test commit is told of a landing.
- makibad runs git and `gh` only, never project code (docs/design/test-commit-ci.md, option D).

## 2. The person's decision

A first version of this note had makibad run the checks a project listed over each open test commit. The person answered no (pending entry 50): nothing runs outside a sandbox. This note is the redesign the answer asks for: notice only. At a landing that changes `tools/`, `.github/` or `tests/common/`, a notice names every open run with a pushed test commit, and each such worker runs the checks in its own sandbox before its fix. Nothing stops a merge, and the gate does not change.

## 3. Design

### 3.1 The owner

A new module, `src/daemon/landing.rs`. The nearest owner, the belt's landing check (`belt.rs` clone checks, `landed_in_clone`, settled by `ahead.rs`), answers one question for one kind of run: has a design-only run's dependency landed. This duty starts from any landing of a project and reads other runs, so it is not an extension of that list; it reuses the belt's fetch (`belt::in_clone`, made `pub(super)`, not copied) and the gate's readers (`gate::fetch`, `gate::find_manifest`, both `pub` today). It runs git only, as `door::pr_before_ci` and the belt's read do: no project code, no command a project names.

### 3.2 When a landing is scanned

Per project. A project's scan is due when its newest `pr_merged` that names a merge commit (of any run of the project, by `projects.id`; one recorded without a merge commit cannot be looked for in the clone) has a larger event id than the `landed_event` of the project's newest `landing_scanned`, or the project has a `pr_merged` and no `landing_scanned`. Only the newest `pr_merged` P is taken: it is compared with the tip, which holds the older ones (3.3, step 2).

### 3.3 The scan, on a thread

Each pass of the loop, one thread at a time, its result written by the loop's thread in one transaction (as the belt's read):

1. `in_clone(clone, default_branch, P.merge_commit)`: fetch, then the tip T. A fetch failure, or P's merge commit not yet in T, writes nothing; it is tried again after `MAKIBA_LANDING_RETRY_MS` (the belt's). So does any merge not scanned yet, recorded within the last hour (`MERGE_WAIT`), that T does not hold: the records need not come in the order of the merges, and one recorded before P but landed after it must not be passed over. An older one that never reaches T (a merge into another branch) holds nothing.
2. The paths changed: `git diff --name-only <from> T`, where `<from>` is the `base_head` of the project's newest `landing_scanned` when that commit is an ancestor of T, else the common ancestor (`git merge-base --octopus`) of the first parents, held by T, of every `pr_merged` of the project newer than its newest scan (the order of the records need not be the order of the merges), else the first parent of P's merge commit. So two landings between scans, landings before the first scan, or a push no run follows, are seen together. The paths are read as git prints them with `core.quotePath=true` (always ASCII) and each quoted one is unquoted to its bytes, so a path past ASCII is matched and a name that is no UTF-8 does not stop the scan. `matched`: the changed paths under `tools/`, `.github/` or `tests/common/` (a fixed list in the module, as the person named it).
3. With `matched` empty, the scan ends there (no run is read).
4. Otherwise, each candidate run R: open (`queued`, `in_use`, `review`, `suspended`, `parked`, the open statuses `prwatch::followed` reads), of the same project, not P's own run, with a current pull request (`belt::current_pr_sql`) and a newest recorded head H (`pr_opened` or `fix_pushed`). For each, `gate::fetch_with(clone, repo, pr, branch, cancel)` (the gate's `fetch`, with the scan's cancel flag so that makibad's stop kills the git child; the pull request's head ref, as the gate reads it) and `gate::find_manifest(clone, T, H)`. A run with a manifest M is named, with M and its declared paths. A run whose fetch or manifest read fails is named as not read, with the reason. A run without a manifest is not named.

### 3.4 The record and the notices

One new event kind, `landing_scanned`, by `makibad`, on P's run: `repo`, `pr`, `merge_commit` (P's), `landed_event` (P's event id), `base`, `base_head` (T), `from`, `matched` (the paths of step 2, at most 50) and `matched_count`, `tasks` (each `task_id`, `repo`, `pr`, `head`, `test_commit`, `declared`) and `not_read` (each `task_id`, `why`). It is written once per P, also when `matched` is empty, so the scan is not due again.

In the same transaction, when `tasks` or `not_read` is not empty, the loop keeps only the runs still open whose current pull request is the one read (a run closed, or moved to another pull request, while the scan ran is dropped from both lists), then writes:

- One notice to the secretary (`door::notice` with P's task):

  > `<repo>#<pr>` landed (merge `<7>`); origin/`<base>` is now `<7 of T>` and changed `<matched, at most 10, then "and n more">` since `<7 of from>`. These open runs have a pushed test commit, whose declared tests cannot change; each is told to run the repository's checks over its declared tests at that tip in its own sandbox before its fix: `<task>` (test commit `<7 of M>`: `<declared>`), .... Not read: `<task>` (`<why>`). Nothing is stopped.

- One notice to each named run's worker (`door::notice` to `worker_agent_id(task)`; a parked worker is woken by it, as `message_row` does today):

  > `<repo>#<pr>` landed; origin/`<base>` is now `<7 of T>`, and it changed `<matched>`. Your test commit `<7 of M>` declares `<declared>`, which cannot change after it. Before you write or push your fix: git fetch origin, and in your sandbox run, at origin/`<base>` with your declared files as your test commit has them, the repository's checks that read tests/ (the commands of the CI jobs, such as `python3 tools/check_test_executables.py`). If one fails on your declared files, report blocked naming the check and the lines; else report done in one line: "checked at `<7 of T>`: the declared tests pass the checks".

  A run named as not read gets no notice of its own: the secretary's notice names it.

The kind is documented in `docs/design/spec.md` as `landed_in_clone` is: in the prose beside section 5.8's landing paragraph and in the sentence after the event table that lists the kinds kept out of it, not as a row. `src/spec.rs` and `tests/spec_matches_code.rs` do not change (they hold the table's kinds only; `landed_in_clone` is not there either).

### 3.5 State, ordering and restart

No state in memory beside the ledger but the running thread and its cancel flag (as the belt's read). What is due is read from the ledger each pass. A restart drops the running thread; the scan is still due and runs again. One P is written at most once: the loop writes a scan only while the project has no `landing_scanned` with `landed_event` at P or newer. A newer merge while a scan runs leaves that scan to be written (at its T); the project is then due at the newer merge, from that T.

### 3.6 Files the build changes

- New: `src/daemon/landing.rs`; the declared test `tests/landing_test_commits.rs`.
- `src/daemon/mod.rs`: the module and its pass in the loop, beside the belt's read.
- `src/daemon/belt.rs`: `in_clone` made `pub(super)`; no other change.
- `src/daemon/gate.rs`: `fetch` becomes `fetch_with` with a cancel flag, and `fetch` calls it with one never set; no change to what the gate reads or judges (added at the diff review).
- `docs/design/spec.md`: the landing paragraph of section 5.8 gains the scan and its notices; the sentence listing kinds kept out of the event table gains `landing_scanned`.
- No existing test, no file under `.github/`, no script, no skill, no gate change, no schema change (events are rows of the existing table).

## 4. The declared test: `tests/landing_test_commits.rs`

The daemon in this process on `Local` with the test agent, the door called with the person's, the secretary's and a worker's keys (as `tests/belt_ahead.rs`), and the repository world of `tests/pr_open_test_commit.rs`: the project's clone has a GitHub-shaped origin URL naming the repository, rewritten to a local bare repository with `url.<local>.insteadOf` (`gate::origin_of` reads the URL as configured), and that bare repository publishes each pull request's `refs/pull/<n>/head`; a `gh` stand-in answers `pr view` (state, `mergeCommit`) from files and refuses anything else. Every helper is inside the file. The runs are in `review`, as a run is after its worker reported done with its test commit pushed (an `in_use` run with no terminal of the stub base would be read gone at makibad's start). Its assertions:

1. PR 454's case. Main at B0 has no `tools/check_test_executables.py`. Task A's branch has one commit M with `Acceptance-Test: tests/exe_made.rs` (a file with an `fs::copy(` and a `.mode(0o755)` line); `pr opened` at M is recorded for A. Before any landing, after several passes: no `landing_scanned` and no notice naming A. Then the landing: task B's pull request has one commit L adding `tools/check_test_executables.py`; L is put on origin's main and the stand-in answers B's `pr view` `MERGED` with `mergeCommit` L. Within the budget: exactly one `landing_scanned`, with `merge_commit` L, `base_head` L, `matched` `["tools/check_test_executables.py"]` and `tasks` naming A with `test_commit` M and `declared` `["tests/exe_made.rs"]`; exactly one notice to the secretary naming A's task id, M's first 7, `tests/exe_made.rs` and `tools/check_test_executables.py`; exactly one notice to A's worker naming M's first 7, `tests/exe_made.rs`, L's first 7 and the words "in your sandbox".
2. In the same world: task C's open pull request has no commit with the trailer, task D's run has a test commit but was closed before the landing, and B (the landed run) has a test commit too: none of them is in `tasks`, and no notice goes to their workers.
3. makibad is stopped and started again after those results: after several passes there is still exactly one `landing_scanned` of L, one notice to the secretary naming A and one to A's worker.
4. A landing L2 that changes only `README.md`: one `landing_scanned` of L2 with `matched` `[]` and `tasks` `[]`; no new notice naming A.
5. Landings that change only `tests/common/helper.rs` (L3), then only `.github/workflows/extra.yml` (L4), each recorded and put on main after the previous scan: each has its `landing_scanned` naming A, and A's worker has one more notice for each.
6. Two landings between scans: L5 changes `tools/new_check.py`, L6 only `README.md`; both `pr_merged` are recorded while neither is on origin's main, then both are put on main: one `landing_scanned`, of L6, with `from` L4, `matched` `["tools/new_check.py"]` and `tasks` naming A.
7. A run of another project with a test commit is never named by this project's landings.

## 5. Known limits

- The notice asks; it proves nothing. Whether the declared tests fail the new checks is known only when the worker runs them, or at the fix head's CI as today.
- A landing makibad does not see as a `pr_merged` (a direct push, a pull request no run follows) starts no scan; the next scan's diff from the last scanned tip holds its paths.
- The three folders are a fixed list: a check over `tests/` that lives elsewhere (another folder, a `build.rs`) is not seen. Making the list a project's setting is a possible follow-up.
- A notice for a landing that changed `tools/` but no check over `tests/` costs each named worker one run of the checks.
