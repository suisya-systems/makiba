# When a pull request is ready for the merge queue (Issue 475): a design

Status: approved by the person on 2026-11-25 with Q1, Q2 and Q3 answered as this note proposes
(section 7), and built with it. Reviewed once as a design (round 1: 4 blockers, 4 majors, each
answered in this text; section 11). Code is cited at 3bb84aa. No GitHub host was reachable while
this note was written; a claim about GitHub is marked [unverified].

## 1. The rule

The person's rule (Issue 475, narrowed on 2026-11-25), which this note states once; the code holds
it in one place, `src/daemon/ready.rs` (section 4), and every other text points there instead of
repeating the list.

- A pull request's head is **ready** to ask for the merge queue when its CI passed, it does not
  conflict with its base, and no **landing** since the base of that CI changed **what the checks
  check**.
- What the checks check: a file under `tests/common/`, `tools/` or `.github/`. Other files under
  `tests/` do not count. The person narrowed `.github/` to changes that alter which checks run or
  what a check runs (not triggers, concurrency or caching, as PR 473); makibad cannot tell those
  apart reliably without a YAML parser, and the line reader first built for it kept letting YAML
  edges through review, so on 2026-11-25 the person chose that every `.github/` change counts
  (section 4.2). A scheduling-only change then costs one update more than needed, never a miss.
- Not ready because of a conflict or such a landing: makibad brings the branch up to date itself
  (the existing `update` job: a merge, never a rebase) and watches the new head's CI.
- Any other move of the base is no reason to update: the queue tests the combination.

**The base of the CI run, as makibad holds it** (Q1): the ledger does not record the base a CI run
checked out. makibad takes `C = git merge-base <head> <base tip>`, the newest base commit the head
itself holds. A `pull_request` run checks out GitHub's merge of the head with the base as it was
when the run started [unverified], which is at or after `C`. So `C` is a conservative
approximation: it can count a landing the CI did in fact test (one update more than needed), never
miss one it did not. The notices say "the head holds <b> at <C>", not "CI passed on".

## 2. Who owns it

The PR watch (`src/daemon/prwatch.rs`). It already follows each pull request a run follows
(`followed`), reads its head, its CI and its merge state (`STATE_FIELDS`, `MergeRead`), and holds a
passed result until the merge state of that head is read (`merge_of`). Readiness is one more
judgement of that same loop, on what it already reads, plus a git reading of the base; there is no
second GitHub call and no second owner. Its git readers are threads of `PrWatch`, started and
stopped by its pass as the state pollers are. The new module `src/daemon/ready.rs` holds only the
path rule and the git reading those threads call; it owns no thread, table or timer.

## 3. When it judges, and what it writes

### 3.1 The candidates (a function of the ledger)

Each pass, after the CI results and the merge reads are written, `PrWatch::readiness` lists the
candidates from the ledger, as `scout_candidates` lists the scouts'. A candidate is a followed pull
request (`followed`) whose run has a code project with a git repository in the repositories folder
(`$MAKIBA_REPOS_DIR/<project>`, as the `update` job finds it; none: never a candidate, nothing
written; every launched code run has one, `prepare.rs`), where:

1. the head is the run's newest recorded head (the query `apply_merge` uses), and its newest
   `ci_completed` is `passed`;
2. the pull request has no act awaiting its landing (`door::act_awaiting`, merge-act.md section 7);
3. no `update` command of the task is `accepted`;
4. the newest merge read of that head (`self.merges`, begun after the CI result was received, as
   `merge_of` requires; after a restart, any read) has a computed class other than `draft`;
5. the head has no `queue_readiness` yet (**first judgement**), or its newest one is `ready`
   (**watched**). A head decided `update` or `unknown` is not judged again; its next judgement is
   of the new head.

### 3.2 The readers (git, on threads)

Each candidate has one reader thread, keyed by task and head; the pass stops a reader whose key is
no longer a candidate, as `state_pollers` stops a state poller. A reader runs git in the project's
repository and touches no database. With `<b>` the project's default branch
(`projects.default_branch`, the base `update` judges against, `push.rs` `judged_base`), a reading
is:

1. Fetch from `origin`, with `--no-write-fetch-head --no-auto-gc --refmap=` (no opportunistic
   update of `refs/remotes/origin/*`; every git call bounded at 120 s and stopped by the reader's
   cancel), `+refs/heads/<b>` into
   `refs/makiba/ready/<task>/base` and `+refs/heads/<branch>` into `refs/makiba/ready/<task>/head`.
   Refs of its own: the `update` job fetches into `refs/remotes/origin/*`, and two fetches of one
   ref collide on git's ref lock (Issue 369). A branch at origin not at the candidate's head: the
   reading is `stale`.
2. `B` = the fetched base; `C` = `git merge-base <head> B`.
3. The landings since: `git rev-list --first-parent --reverse C..B`. For each landing `L`:
   `git diff-tree -r --no-renames --name-status L^1 L` (a rename as a delete and an add, so both
   paths are judged), each path judged by section 4.
4. The result: `B`, `C`, the landings that count (sha and the paths that count), or git's message.

A **first-judgement** reader reads once and sends its result. A **watching** reader (a `ready`
head) holds `covered`, the base tip its verdict covers (the record's `base_tip`, then each tip it
found nothing in), and each interval of the PR watch fetches the base again: an unchanged `B`
sends nothing; a moved `B` is read whole (`C..B`, step 3) and sent, counting or not. So every move
of the base is seen, whoever landed it (a pull request makiba never followed, a push by hand), and
a landing that comes after a fetch is seen by the next one: nothing depends on the order of the
ledger's landing events. The cost is one fetch of one branch per ready head per interval (30 s).

### 3.3 The decision (the loop, one transaction)

The loop writes a result in one transaction, after checking the candidate's rule (3.1) again in it
for the same head (a head recorded, an act or an update started while the thread read: dropped, and
the reader stopped by the pass):

| Result | Verdict | Written |
| --- | --- | --- |
| merge class `conflict` (no git reading needed) | `update`, reason `conflict` | the event, the `update` command, one notice |
| a landing counts | `update`, reason `checks_changed` | the event, the `update` command, one notice |
| nothing counts, first judgement | `ready` | the event, one notice |
| nothing counts, watching | none | nothing; `covered` is set to `B` in memory |
| `stale` | none | nothing (a newer head is coming) |
| git failed | none, or `unknown` | nothing, read again next interval; after 300 s from the head's first failure in this process (the PR watch's budget) the event with verdict `unknown` and one notice |

A reading is decided on the head's merge state at the time it is written (the newest read of
that head, begun after its CI result): a draft or a state not computed writes nothing (the reader
is started again once it is computed), and a conflict makes it `update`, reason `conflict`,
whatever the landings. The conflict row applies to a first judgement and to a watched `ready` head alike: the pass sees a
`conflict` read of a candidate's head in `self.merges` and writes it without waiting for a reader
(`base_tip` the last `covered`).

The event `queue_readiness` (actor `makibad`, on the task's run): `repo`, `pr`, `head_oid`,
`task_id`, `base` (`<b>`), `base_tip` (`B`), `ci_base` (`C`), `merge_state` (the class),
`verdict` (`ready`, `update` or `unknown`), `reason` (`conflict`, `checks_changed`, or git's message
for `unknown`), `landings` (`[{"sha", "paths"}]`, the ones that count), `watched` (whether a
`ready` verdict of this head preceded it), and `command` (the `update` command's id).

The `update` command is inserted as the door's `update` verb records one: `commands` with actor
`makibad`, verb `update`, status `accepted`, args `{"task", "base": <b>, "repo", "pr", "head",
"readiness": <event id>}`. The run must have a code worktree, as the door's `push` requires;
without one the verdict is still `update`, no command is written, and the notice says the update
cannot run and the secretary has the worker merge the base. The `Push` pass runs it as any
`update`, with the same checks (Issue 307 included) and the same one-job-per-project rule.

### 3.4 The update's end

The update thread, for a job whose args carry `readiness`, also computes `base_merge` for the head
it ends with (`gate::pushed_base_merge` of the project's repository, the base, the args' `head` and
that head: true only for a clean merge of the base), as the door computes it for `pr pushed`
outside its lock. The job merges only the judged head: a worktree behind it (the worker has not fast-forwarded after
an earlier update of makibad) is let through, and one that holds commits of its own (the worker
committed since) fails with nothing merged or pushed. `push.rs` `finish`, for such a job, and only while the
run's current pull request is still the one judged with the judged head its newest record (a head
or pull request recorded while the update ran is never overwritten):

- `Done::Updated`, and `Done::Current` naming a head other than the args' `head` (the restart case
  `update` already handles: the merge was pushed and not recorded): in the same transaction, the
  head is recorded as the `pr` verb records a `pushed` (`fix_pushed` with that `base_merge`, the
  watch of the new head, and Issue 431's rule for a queue act), through the door's `pr` recording
  function made callable from `push.rs`, not a copy of it. A head already recorded is not recorded
  again (the `pr` recording leaves a `completed` or `watching` row as it is). The notice says the
  head is recorded and watched, and keeps `sync_request` for the worker.
- `Done::Current` naming the args' `head`, and a failure: as today. A conflict the merge cannot
  resolve goes to the secretary with the files, as today.

So the new head's CI is watched with no step of the secretary, and its passed result is a first
judgement of a new head.

### 3.5 Notices (to the secretary, `door::notice`; the texts in substance)

- ready: "<repo>#<pr> at <7> can be queued: its CI passed, the head holds <b> at <C 7>, and no
  landing since changed what the checks check. <https://github.com/<repo>/pull/<pr>>"
- update: "makibad is updating <repo>#<pr> at <7> with <b> (update command <id>): <it conflicts
  with its base (DIRTY) | landings since <b> at <C 7>, which the head holds, changed what the
  checks check: <sha 7> (<up to 3 paths>), ...>. It is not to be queued at this head; makibad
  records and watches the new head. <link>" When the head was told ready before, the text begins
  "<repo>#<pr> at <7> is no longer ready for the queue:".
- unknown: "makibad cannot judge whether <repo>#<pr> at <7> is ready for the queue: <git message>.
  Judge it by hand by the rule of docs/design/queue-ready.md section 1. <link>"

Only the ready notice says "can be queued", so the phrase is the one ready signal.

### 3.6 The `BEHIND` sentence

The CI notice's merge sentence for `BEHIND` (`prwatch.rs` `merge_sentence`) today tells the
secretary to update the branch, against the rule. It becomes: " It is behind its base (BEHIND);
that alone is no reason to update it: makibad tells you when it is ready for the queue, or updates
it itself (docs/design/queue-ready.md)." The assertion of `tests/pr_merge_state.rs` line 261 that the notice names "update-branch"
changes to "no reason to update"; that file is declared with the new test (section 9), so the
change is in the test commit and the gate does not read it as an edited test.

## 4. The path rule (`src/daemon/ready.rs`)

### 4.1 The list

One constant, `CHECKED_PATHS`, `["tests/common/", "tools/", ".github/"]`; a landing's path counts
when it lies under one of them. Nothing else in code or docs lists the paths: the skill and spec.md
name the module and this note.

### 4.2 `.github/`: every change counts

makiba has no YAML parser (`Cargo.toml`), and adding one is a dependency decision this Issue does
not make. A conservative line reader of the workflow files was built first (it cleared changes
whose every changed line lay under `on`, `concurrency` or an `actions/cache` step) and reviewed in
diff rounds 1 to 6; each of rounds 1, 4, 5 and 7 found a YAML form it misread (block scalars, a
binary attribute, tags, anchors in flow collections, plain scalars spanning lines). On the person's
word of 2026-11-25 (the design's Q2, its second option) it was replaced: every change under
`.github/` counts. Known limits: a change of how runs are scheduled (PR 473) makes ready heads
take one update more than needed; branch protection's required checks are not in the repository
and are not seen. Telling the kinds apart again waits for a decision on a YAML parser.

## 5. What is kept in memory, and its rules

The ledger is the record. `PrWatch` keeps, beside its other maps:

- `readers`: by task, the head and mode of its reader, its thread and its cancel flag.
- `covered`: by (repository, pr, head), the base tip a watching reader last found nothing in.
- `read_failing`: by (repository, pr, head), when its first failed reading came back.

Ordering: the readiness pass runs after `apply_with` and `apply_merge` of the same pass, so a CI
result and a merge read are in the ledger and in `self.merges` before a candidate is listed. The
event, the command and the notice are one transaction, so a decision is never half written. A
result is written only for the head its reader was started for, after the candidate rule is checked
again in that transaction.

Restart: all three are emptied. The candidates are listed again from the ledger: a head with no
`queue_readiness` is judged once its merge state is read again; a `ready` head gets a watching
reader whose `covered` starts at the record's `base_tip`, so a landing made while makibad was down
is read on its first fetch; the failure budget starts again. An `accepted` `update` is run again by
`Push` after a restart, as today, and `finish` records the head it ends with (3.4).

## 6. How it meets the merge act (merge-act.md)

Both directions are closed in transactions:

- Readiness never updates under an act: a pull request with an act awaiting its landing is no
  candidate (3.1 rule 2), checked again in the decision's transaction.
- The act never starts under an update or on a head judged not ready: the `merge` verb's check
  (`door.rs` `merge_check`, run before the gate and again in the final transaction, merge-act.md
  section 3.1) also refuses with `conflict`, nothing judged or sent, when the task has an
  `accepted` `update` command, or when the newest `queue_readiness` of the pull request's newest
  recorded head has verdict `update`: "<repo>#<pr> at <7> is not ready for the queue (makibad's
  queue_readiness <id>: <reason>); wait for its update and the new head's notice". A head with no
  record, or `ready`, or `unknown`, is judged by the gate as today. So the act uses the same
  readiness, read from the ledger, with no second git reading.
- The ready notice tells the secretary the head can be queued; how it asks (the `merge` verb, the
  person's approval under `human`) is the pull-request skill's "Merging" (section 8).

Range-8 branches touching the same code: PR 493 (#434) changes `prwatch.rs`'s state read
(`STATE_FIELDS`, `MergeRead`, `apply_state`, `followed`); this change adds fields at the end of
`PrWatch`, one call in `pass` before `state_pollers`, functions of its own, one sentence of
`merge_sentence`, and reads `MergeRead` only through `self.merges` (`head`, `status`, `draft`,
`started`), so either order merges with textual conflicts only in `PrWatch`'s fields and
`from_env`. PR 494 (#432) changes the `merge` verb (pending entries and `approve`, which call
`merge_check`); this change adds two rules at the end of `merge_check`, which hold for `approve`
too. PR 492 (#433) changes the hook, not the functions named here.

## 7. Questions (answered by the person, 2026-11-25)

Answers: Q1, the merge base is the rule to hold; Q2, first the line reader, then (2026-11-25,
after diff round 7) every `.github/` change counts, section 4.2; Q3, the refusals are in this
change.

- **Q1, the base of the CI run.** Section 1 holds the merge base, a conservative approximation (it
  can cost one needless update, never a missed landing). The exact base would need GitHub's base of
  the run (the workflow run's `pull_requests[].base.sha` [unverified]) recorded in `ci_completed`.
  Is the approximation the rule to hold?
- **Q2, `.github/`.** Is section 4.2's line reader the rule to hold (anything it cannot read
  counts), or should every `.github/` change count until a YAML parser is decided?
- **Q3, the merge verb.** Section 6 adds two refusals to `merge_check` (the merge gate's verb). Is
  that in this change, as the design reader asked, or a separate Issue?

## 8. Files the change touches

New: `src/daemon/ready.rs`, `tests/queue_ready.rs` (the declared test), this note.

Changed:

- `tests/pr_merge_state.rs` (declared too; section 3.6): line 261's word "update-branch" becomes
  "no reason to update". No other existing test changes.
- `src/daemon/prwatch.rs`: `PrWatch` fields, `from_env` reads the repositories folder as `Places`
  does, `readiness` in `pass`, the `BEHIND` sentence.
- `src/daemon/push.rs`: the update thread's `base_merge` for a readiness job; `finish` records the
  head (3.4).
- `src/daemon/door.rs`: the `pr` recording function callable from `push.rs` (behaviour unchanged);
  `merge_check`'s two refusals (section 6).
- `src/daemon/mod.rs`: `mod ready`.
- `src/spec.rs` and `docs/design/spec.md`: the event `queue_readiness` in the event table; a
  paragraph in section 5.6; the `BEHIND` sentence quoted in 5.6; the `update` row (makibad also
  runs it on its own judgement, and records the head); the `merge` row (the two refusals).
  `tests/spec_matches_code.rs` is not changed: it compares the two.
- `skills/pull-request/SKILL.md`: "The CI notice": for a followed pull request a passed notice is
  never the ready signal, whatever its merge sentence; wait for the readiness notice. `BEHIND` is
  no reason to update. A new section "A queue-readiness notice": "can be queued" is the only ready
  signal of a followed pull request (quote it); "updating" means wait for the new head's notices;
  "cannot judge" means judge by hand by the rule in this note and tell the person. "A merge-state
  notice": a change to `DIRTY` of a ready head is followed by makibad's update notice. "Merging":
  both modes act only on a "can be queued" notice of the head (a pull request not followed: by the
  rule, by hand); stop line 8 becomes "no readiness notice says the head can be queued". "Main
  moved under a pushed branch": update only on a conflict the PR watch does not follow, or when
  the person asks; makibad updates followed pull requests itself.
- `skills/update-branch/SKILL.md`: `when_to_use` says makibad updates a followed pull request on
  its own when the rule says so.

No `.github/` file, script, acceptance checker or hook changes, and no schema change
(`commands.actor` takes `makibad`).

## 9. The declared tests

`Acceptance-Test: tests/queue_ready.rs, tests/pr_merge_state.rs`.

`tests/pr_merge_state.rs`: the one changed assertion of section 3.6.

`tests/queue_ready.rs`: makibad (`tests/common/makibad.rs` `run_until`, with a `Stub` base) runs in
the test process on a state database of its own, with `MAKIBA_PRWATCH_INTERVAL_MS` 50 and the
repositories and workers folders under a temporary root. Three projects, each with its own
repository and local bare `origin`, and one `in_use` code run each, whose worktree is made with
`run_repo::create` and whose branch is at origin; each pull request is recorded with
`prwatch::register` (`pr_opened`). `gh` is a stand-in (`MAKIBA_GH`, written through
`tests/common/exe.rs`) that answers `gh pr view` with `state` `OPEN`, `headRefOid` read from the
branch at that project's origin, and the `mergeStateStatus` of a file per pull request; it answers
every CI read as passed and finished. The door runs in the process for the merge request of test 3.

1. `a_head_green_before_a_landing_under_tools_is_updated_not_ready` (project `pa`, PR 1): the
   branch forks at M0; then main lands L1, which adds `tools/check.py`. Asserts:
   - exactly one `queue_readiness` of the first head, `verdict` `update`, `reason`
     `checks_changed`, `ci_base` M0, its `landings` naming L1 with `tools/check.py`;
   - one `update` command with actor `makibad` for the task, finished `done`; one
     `branch_updated` whose `new` is origin's branch, which holds L1;
   - the new head is recorded (`fix_pushed` with `head_oid` the new head) and has a `pr_watches`
     row;
   - no notice containing "o/pa#1 at <first head 7> can be queued"; exactly one notice containing
     "o/pa#1 at <new head 7> can be queued", and a `queue_readiness` of the new head with
     `verdict` `ready`.
2. `a_head_green_after_a_tools_landing_with_main_moved_under_tests_only_is_ready_once` (project
   `pb`, PR 2): main lands L1 (`tools/check.py`), the branch forks after it, then main lands L2,
   which adds `tests/other.rs`. Asserts: exactly one `queue_readiness` of the head, `verdict`
   `ready`, `ci_base` L1, `landings` empty; exactly one notice containing "can be queued" and
   "https://github.com/o/pb/pull/2"; no `update` command of the task; origin's branch unchanged.
   Then main lands L3 (`tests/more.rs`), with no event in the ledger; once
   `refs/makiba/ready/pb-task/base` (the task's id) in the project's repository reads L3 (a
   watching reader fetched it) and ten intervals more: still exactly one `queue_readiness`, one
   "can be queued" notice, no `update` command, origin's branch unchanged.
3. `a_conflicting_head_is_updated` (project `pc`, PR 3): the branch and main change `shared.txt`
   differently; the stand-in reads `DIRTY`. Asserts: exactly one `queue_readiness`, `verdict`
   `update`, `reason` `conflict`; one `update` command with actor `makibad`, finished `failed`; a
   notice containing "conflicts in shared.txt"; no notice containing "can be queued". Then the
   secretary's `merge` request (`repo` `o/pc`, `pr` 3, `head` the head) through the door is refused
   with kind `conflict` and a message containing "not ready for the queue", and no
   `merge_attempted` is written.

Added after the test commit (new targets are allowed there): a unit test in `src/daemon/ready.rs`
that PR 473's workflow change counts (section 4.2), and `tests/queue_ready_watch.rs` (diff review
round 7): a head told ready is revoked and updated by a landing under `tools/` made while makibad
runs, and by one made while makibad is down; the new head is recorded and told ready once.

## 10. Proposed build Issues (after this one)

None required by this Issue (Q1 was answered "the merge base"). If an exact base is wanted
later: the CI watch records the run's base in `ci_completed`, and readiness reads it instead of the
merge base.

## 11. The design review (round 1, reader codex: passed, 4 blockers, 4 majors)

- Blocker, re-judgement only on `pr_merged` misses landings of pull requests makiba does not
  follow: replaced by watching readers that fetch the base each interval (3.2).
- Blocker, a landing between the fetch and the commit is missed: a watching reader compares each
  new fetch with `covered`, so the next fetch reads it; no ledger watermark is relied on (3.2).
- Blocker, the merge act can start under a readiness update: `merge_check` refuses it (section 6,
  Q3).
- Blocker, blank and `#` lines inside block scalars are not neutral: block scalars are tracked
  (4.2), with fixtures (section 9).
- Major, `Done::Current` after a restart leaves the pushed head unrecorded: recorded too (3.4).
- Major, the merge base is not the CI's base: stated as a conservative approximation, the notices
  no longer say "CI passed on", Q1 asks (section 1).
- Major, the `BEHIND` sentence orders an update: changed in this change, with its assertion, in
  the test commit (3.6).
- Major, the skill still lets the secretary proceed from a "mergeable now" CI notice: the skill's
  CI notice, merge-state, Merging and stop line 8 wait for the readiness notice (section 8).
