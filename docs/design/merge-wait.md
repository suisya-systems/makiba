# Approved pull requests waiting in line (Issue 360): a study

Status: study, for the owner's approval. No code, CI or branch protection changes with it. The
changes the chosen option needs are proposed as build Issues, not designed here.

## 1. The problem, measured

Branch protection of `main` requires the branch to be up to date (`strict: true`) and four checks:
`tests`, `lint`, `real-base`, `acceptance` (Issue 360, from the GitHub API
`branches/main/protection/required_status_checks`). One CI run takes about 10 minutes, `real-base`
being the longest (9.5 to 10 minutes on 2026-11-23). With `strict`, a pull request can land only
when its head contains the current `main`, so after each landing the next one is brought up to date
with `update` (a merge of `main`, `src/daemon/push.rs`) and runs CI again. Six approved pull requests
(346 to 351) landed one at a time on 2026-11-23: the last waited for about five extra runs. Each
`update` also cost a fix-budget push and stopped the task for the person (Issue 358), and some
updates conflicted and went back to a worker.

The wait is O(n) full CI runs for n approved pull requests. The study asks how to land them without
that serial chain, while the merge gate keeps judging something that holds for what lands.

## 2. What makiba's gate judges today

Read from the code at f1a305c.

- The merge is one command, `gh pr merge <n> --repo <o>/<r> --squash|--merge|--rebase
  --match-head-commit <sha>`, and the secretary's hook `makiba hook merge-gate`
  (`src/hooks/merge_gate.rs`) lets it run only when the door verb `gate` answers `allow`. Every
  other way to merge from a command line is refused; the bound against merging around the gate is
  branch protection.
- The gate judges the pull request's head (`gate_outside`, `src/daemon/door.rs`): `gh pr view`
  must name that head, the latest `ci_completed` of the pull request must be `passed` at it, and
  the head is fetched into the clone. `gate::evaluate` (`src/daemon/gate.rs`) then checks that the
  test commit M is the first own commit and that the protected paths are unchanged since M, and
  takes the test-first proof in one of two ways: (b1) the check named in `.github/acceptance-check`
  (here `acceptance`) succeeded at head, while `.github/workflows/` at head equals the base; or (b2)
  a failed `ci_completed` is recorded at M, one CI job concluded `failure` at M and `success` at
  head, and the runs of M and of head were made against one and the same base. The CI reading takes
  only `event=pull_request` runs (`Ci::pr_runs`).
- The CI job `acceptance` (`.github/workflows/ci.yml`) runs only on `pull_request`; it rebuilds the
  test commit and the head itself (`scripts/acceptance-check.sh BASE HEAD`) with the checker taken
  from the base. So path (b1) carries the test-first proof inside one CI run at the head, and the
  failed run at M is needed only by path (b2) and by the push order of L-K9 (Issue 307).
- The review (Issue 263, 344): a code pull request's head must be the head its last diff review
  read, or one whose own changes are byte-equal to it; a clean merge of the base keeps the review
  (`after_review`, `same_changes`).
- `merge_approval_mode` `human`: the gate allows with a warning, and the hook turns that into a
  question to the person. Only the person's yes lets the command run. Under `acceptance` the gate
  decides alone.
- L-K9 (`docs/design/lines.md`): "A merge needs CI on the exact head and the acceptance gate."
- The PR watch (`src/daemon/prwatch.rs`, Issue 388) reads `gh pr view` each interval with
  `state,mergedAt,mergedBy,headRefOid,mergeCommit,title`; Issue 387 adds
  `mergeable,mergeStateStatus` to the same call. On `MERGED` it writes `pr_merged` with
  `merged_via` `gate` when an `acceptance_gate` allow is recorded for that pull request at
  `headRefOid`, else `hand`, and tells the secretary of a hand merge.

What `strict` gives today: with `--squash` of an up-to-date head, the landed tree is the tree of the
head, and that tree passed all four checks. So "CI on the exact head" also means "CI on the exact
tree that lands". Every option below has to say what replaces that.

## 3. The options

Claims about GitHub and other tools that come from my own knowledge, not from a source read in this
study, are marked **[unverified]** and listed in section 5. No web host was reachable for this study
other than the project's own.

### A. GitHub's merge queue

What it is [unverified]: branch protection (or a ruleset) on `main` gets "Require merge queue".
Adding a pull request to the queue makes GitHub build a temporary branch
`gh-readonly-queue/main/pr-<n>-<sha>` whose commit is `main` plus every entry ahead of it plus this
pull request, and fires the workflow event `merge_group` for it. The required checks must pass on
that commit; then `main` is moved to it (a fast-forward, so the landed commit is the tested one). A
failed entry is removed from the queue and the entries behind it are rebuilt without it. Settings:
merge method (one for the queue), build concurrency (how many entries are tested at once, default
5), minimum and maximum group size (batching: several entries merged in one group), a wait time to
fill a group, "only merge non-failing pull requests", and a check timeout. The up-to-date
requirement is not needed: the queue tests the combination itself. Source:
https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue
and https://docs.github.com/en/actions/writing-workflows/choosing-when-your-workflow-runs/events-that-trigger-workflows#merge_group

Plan: a merge queue is available in any public repository owned by an organization, per the
owner's reading of https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue (the secretary's fetch of that page did not capture the sentence, so it is
not confirmed by fetched text). `suisya-systems/makiba` meets it: the secretary checked that it is
public, owned by an organization, on the free plan. Availability for private repositories (GitHub
Enterprise Cloud) is [unverified] and does not matter here. Cost: the queue runs CI once per entry (plus reruns of the
entries behind a failure); on a public repository Actions minutes on GitHub-hosted runners are free
[unverified].

Workflows: `ci.yml` is `on: [push, pull_request]`. A workflow that is to report a required check on
the queue's commit must also have `on: merge_group` [unverified: whether the push to the
`gh-readonly-queue/*` branch fires `push` workflows too, and whether those would satisfy the
check]. Three of the four checks (`tests`, `lint`, `real-base`) mean the same thing on the queue's
commit and need only the trigger. `acceptance` does not: it reads `github.event.pull_request.*`
and has `if: github.event_name == 'pull_request'`, and its question (did the declared test fail at
the test commit and pass at head) is about the pull request's own commits, not about the merged
commit. Today, after an `update`, `acceptance` runs again at the merged head and runs each declared
test target there (`cargo test --test <name>` must run at least one test and pass), so it proves the
declared test runs and passes on the tree that lands. The queue's `tests` and `real-base` do not
prove that: another change in the queue can drop the declared test from discovery and both still
pass (`gate.rs` names this limit). So `acceptance` on `merge_group` should not just report success;
it should run, on the queue's commit, the declared test targets of each pull request in the entry,
with the same "ran at least one test, none ignored, passed" rule the checker applies at head. That
needs the entry's pull request (from the queue branch name or the event payload [unverified]) and
its test commit's `Acceptance-Test` trailers. It is a change to `ci.yml` and the checker (a new
mode), part of build Issue 1. The alternatives, weaker, for the owner to choose against (Q2):
- the queue requires only `tests`, `lint`, `real-base`, and `acceptance` is required on the pull
  request alone. Whether GitHub can require a different set on the pull request and on the queue's
  commit is [unverified]. The landed tree is then not shown to run the declared test;
- `acceptance` reports success on `merge_group` with a line saying it was judged at the pull
  request's head (or a job skipped by `if:` counts as passing [unverified]). Same weakness.

What it means for the gate:
- Which head the gate judges: still the pull request's head, before it is queued. The secretary's
  one command now enqueues instead of merging [unverified: that `gh pr merge` on a branch that
  requires a merge queue adds the pull request to the queue, which flags it then ignores, such as
  `--squash`, and whether `--match-head-commit` is honored as the expected head of the entry]. The
  hook's form can stay, and the gate's checks run unchanged; it never sees the queue's commit.
- The landed commit is not the head the gate judged; it is the queue's commit. What holds for it:
  `tests`, `lint` and `real-base` passed on that exact commit, which is more than `strict` gives
  today only in one way (several pull requests are combined and tested together) and the same in
  the other (the landed tree was tested). That the declared test ran and passed on the landed
  commit holds only with `acceptance` running the declared targets on `merge_group` (above);
  without it, it is a weaker guarantee than today's. The failed run at M is not touched: M is not
  rewritten, and path (b1) or (b2) is judged at the pull request's head, whose base is whatever
  `main` was when it last ran CI; that is the same as today after an `update`.
- The review: this does not carry over as it does today. Today the gate carries a review to a later
  head only when every commit after the reviewed head is a clean merge of the base (`after_review`)
  and the own changes of the two heads against the base are byte-equal (`same_changes`, a binary
  diff from each head's merge base with the base; `gate_review` in `src/daemon/door.rs`). A clean
  `update` that brings a base change near the pull request's own lines can fail `same_changes` (the
  diff's context differs), and the gate then calls the review stale and refuses. Under the queue the
  gate never sees the queue's commit, so that check would be skipped. The contract cannot be kept
  whole with GitHub's queue: the commit that lands is built from entries ahead that change while the
  pull request waits (a rebuild after a failure changes them again), and the check would have to run
  on that commit before it lands, but `same_changes` needs makibad's review record, which a CI job on
  `merge_group` cannot read, and makibad does not act between the queue's build and its merge. Both
  ways open are relaxations of the Issue 344 contract, for the owner to approve as such (Q5):
  (i) the review is judged at the pull request's head only, with the queue's CI as the check on the
  combination; or (ii) narrower: before the enqueue command is allowed, makibad merges the head onto
  the current `main` in the clone and applies `same_changes` there, so a review is stale when `main`
  as it is changed the context of the pull request's own lines, while a change from an entry ahead in
  the queue can still alter that context unseen. A queue that cannot merge cleanly removes the entry,
  so nothing with a conflict resolution lands either way.
- L-K9 would read: a merge needs CI on the exact head and the acceptance gate (at enqueue), and
  CI on the exact commit that lands (the queue's required checks).
- The person's word under `human`: the question is asked at enqueue, for the pull request's head,
  as today. The queue then lands it without asking again, on top of entries the person also let in.
  A pull request removed from the queue and enqueued again goes through the gate again, and under
  `human` the person is asked again. The person can take an entry out of the queue in the web UI.
  This keeps the line "only the person's word merges", if the owner reads "merge" as "enter the
  queue" (question Q3).
- `update` is needed only for a real conflict, not for being behind, so the fix-budget stops of
  Issue 358 for "behind" go away.
- The PR watch: the pull request's `headRefOid` is unchanged by the queue [unverified], so a queued
  merge is read as `merged_via` `gate`. `mergedBy` may name the queue's bot or the account that
  enqueued [unverified]. Two new things to read in the same state call: that the pull request is in
  the queue, and that it was removed from it (the queue's CI failed on the combined commit, or the
  person dequeued it). Whether `gh pr view --json` exposes the queue entry, or a GraphQL read of
  `mergeQueueEntry` is needed, is [unverified]. The CI watch keeps reading the pull request's head;
  the queue's runs are on another commit (and `event=merge_group`), so they do not mix with
  `Ci::pr_runs`.

Speculation and batching in this queue: with build concurrency c, the first c entries are tested at
once, each on top of those ahead; with six approved pull requests and c >= 6, all six land about one
CI run after the last is queued, instead of about six. The speed comes from build concurrency, not
from the merge limits: the merge limits (minimum and maximum group size) say how many tested entries
are merged into `main` at once, and each entry still gets its own build [unverified; the second
reader of this note read the managing-a-merge-queue page that way]. So a larger group does not save
CI runs; it saves only what runs on `main` after each merge (such as `push` CI or a deployment).
Recommended start: maximum group size 1 and build concurrency 5 or more.

### B. Drop `strict`, guard `main` with the CI run after the merge

Meaning: a pull request lands when its head passed CI against some older `main`; the landed commit
(a squash onto the current `main`) has never been tested as a whole. Textual conflicts are still
refused by GitHub (the pull request is not mergeable); a semantic conflict (two changes that each
pass but not together, such as a renamed function and a new call of the old name) lands, and `main`
goes red. `ci.yml` runs on `push`, so `main`'s own run shows it about 10 minutes later.

What it needs: a watch of `main`'s push runs (prwatch watches pull request heads only, so a new
poller over the base branch), a notice to the secretary, and someone to revert: a worker's revert
pull request, which itself needs the gate. A revert declares no acceptance test, so it is
`no_manifest`: under `acceptance` it is refused (not docs-only) and goes to the person; under
`human` the person merges it. While `main` is red every pull request's CI on the new base can fail
for a reason not its own, and path (b2) of the gate (runs at M and at head on one base) keeps
working only for the pull requests whose runs predate the break.

For the gate: the head it judges is the pull request's head, unchanged; the landed commit is not
tested before it lands, so L-K9's "CI on the exact head" no longer covers the landed tree. The
failed run at M and the passed run at head carry over only as "they held on an older base". The
person's word under `human` is unchanged (one question per head, no update). This is the cheapest
change (one branch protection switch) and the weakest guarantee; it is what Chromium's commit queue
plus its sheriffs amount to [unverified], with a human rota makiba does not have.

### C. Batching and speculative runs, as other queues and bots do

[all unverified, from my own knowledge]
- bors-ng: tests a batch of all approved pull requests merged together on a staging branch, lands
  the batch when it passes, bisects a failed batch. It has been archived (2023), and its authors
  pointed to GitHub's merge queue.
- Mergify: a hosted merge queue with speculative checks and batches; free for open source, paid
  otherwise; needs its GitHub App with write access to the repository.
- Kodiak: updates a pull request and merges it when green, one at a time; it automates today's
  serial chain and does not shorten it.
- Zuul (OpenStack): a dependent pipeline tests each change on top of all changes ahead of it in
  parallel; a failure ejects that change and restarts the ones behind it. This is the model GitHub's
  queue with concurrency > 1 follows.
- Chromium's commit queue: runs try jobs on the change rebased on tip of tree, lands without
  requiring it to be up to date with every other landing, and relies on post-submit builders and
  sheriffs to revert (option B with a rota).

What they show: speculative testing (each entry on top of those ahead, in parallel) removes the
serial wait without dropping the test of the landed commit; batching saves CI but needs a bisect
rule; a bot that only automates `update` does not help. A third-party app would hold write access
to the repository and merge on its own, while makiba's lines say only the secretary writes to
GitHub (L-K2) and a merge passes the gate (L-K9); the app's merge would be outside both unless it is
driven by the gate's answer. Building the same
in makibad (a staging branch per batch, CI on it, a merge of the batch) would duplicate what A
gives, and needs a push of a branch that is not a run's branch, which makibad does not do today
(`tests/push_verb.rs`). So C is useful as a design reference for A's settings, not as an option of
its own.

### D. Make CI shorter, or split it

`real-base` runs the whole suite again on the pinned real base, one test thread, because the
fixtures share one base session per process (`ci.yml` comments, M0-3, #5, #81). It is a required
check because `tests/real_base_wire.rs` holds the wire checks that used to be done by hand.

Ways to shorten it [estimates, not measured here]: cache the Cargo build between runs; run the
test binaries in parallel jobs (a matrix over `--test` targets: each target is its own process, so
the one-thread rule holds inside a job); or run only the wire tests on every head and the whole
suite on the real base on `main` after the merge. The last is option B for most of the suite.

What `real-base` needs on every head: the wire checks, since a change that breaks the base's wire
passes with the stand-in base; whether the rest of the suite on the real base finds anything the
stand-in run misses is a question the CI history can answer (question Q4).

For the gate: nothing changes in what it judges; each run is shorter. With `strict` kept, the wait
is still n runs, only shorter runs, and the `update` churn and its fix-budget stops remain. D
reduces the constant; A removes the factor. They combine: the queue's run per entry is shorter too.

## 4. Recommendation

Adopt **A, GitHub's merge queue**, with group size 1 and build concurrency of 5 or more, `strict`
dropped in favour of the queue, and the gate kept where it is: at enqueue, on the pull request's
head. Then D (a cache and a split of `real-base`) as a separate, later improvement.

Why:
- It removes the serial factor: approved pull requests are tested together in parallel, each on
  top of those ahead, and land about one CI run after the last is queued.
- It keeps what `strict` gives: the commit that lands is the commit the required checks passed on.
  B gives that up; D keeps the wait.
- The gate's evidence keeps its meaning with two additions, not a new evidence path in `gate.rs`:
  the test-first proof is still judged at the pull request's head, and `acceptance` on
  `merge_group` runs the declared test targets on the landed commit (Q2), so the declared test is
  shown to run and pass on what lands, as today after an `update`. The review is the one part the
  queue weakens: today's `same_changes` check on the merged head cannot be kept whole; either it is
  given up, or makibad applies it against the current `main` before enqueue, which still misses the
  entries ahead. Both are relaxations the owner approves as such (Q5); I recommend (ii).
- It needs no `update` for being behind, so Issue 358's fix-budget stops for that reason end.
- It is GitHub's own mechanism: no third-party app with write access, and branch protection stays
  the bound.
- The person's word under `human` is asked once per pull request head, at enqueue.

If the queue proves unusable after all (Q1 is answered yes, from the owner's reading), the fallback is D now (shorter runs, `strict` kept), not
B: B lets untested trees land, and makiba has no rota to revert them.

Build Issues the recommendation needs (proposed, to be filed by the secretary after approval):
1. CI: add `merge_group` to `ci.yml`'s `on:`; on `merge_group`, `acceptance` runs the declared test
   targets of the entry's pull request on the queue's commit (a new mode of
   `scripts/acceptance-check.sh`), per Q2. A `.github/` change the owner makes by hand (#62).
2. Branch protection: require the merge queue (merge method squash, group size 1, concurrency 5+),
   drop `strict`; done by the owner.
3. Merge gate and hook: confirm that the one allowed command enqueues with the expected head; if
   not, change the allowed form; say in the gate's answer that the command enqueues. Update L-K9's
   row and `skills/pull-request/SKILL.md` (the merge enqueues; "Main moved under a pushed branch"
   needs `update` only for a conflict). Per Q5, record the relaxed review contract in L-K9 and
   L-K10, and for (ii) have the gate merge the head onto the current `main` and apply
   `same_changes` before it allows the enqueue.
4. PR watch: read the queue state in the same `gh pr view` call (with Issue 387's
   `mergeable,mergeStateStatus`), and tell the secretary when a pull request leaves the queue
   without landing; keep `merged_via` keyed on `headRefOid`. No second poller.
5. Later: cache and split `real-base` (D), after Q4.

## 5. Claims to confirm (unverified)

1. Merge queue availability: answered for this repository by the owner's reading of the
   managing-a-merge-queue page (public, organization-owned); not confirmed by fetched text.
2. The queue's commit is built on `gh-readonly-queue/<base>/pr-<n>-<sha>`, fires `merge_group`, and
   `main` is fast-forwarded to exactly that commit.
3. A workflow needs `on: merge_group` to report checks for the queue; whether a `push` run on the
   queue branch counts.
4. Whether different required checks can apply to the pull request and to the queue's commit; and
   whether a job skipped by `if:` satisfies a required check in the queue.
5. `gh pr merge` on a branch that requires a merge queue enqueues; which flags it ignores; whether
   `--match-head-commit` binds the entry to that head. Source: https://cli.github.com/manual/gh_pr_merge
6. The queue does not change the pull request's `headRefOid`; what `mergedBy` reads after a queued
   merge; how the queue entry and its removal can be read (`gh pr view --json` field or GraphQL
   `mergeQueueEntry`). Source: https://docs.github.com/en/graphql/reference/objects#mergequeueentry
7. The merge limits (group size) decide only how many tested entries are merged at once, each
   entry having its own build; how a failed entry in a group is handled.
8. Actions minutes are free on public repositories with GitHub-hosted runners.
9. The descriptions of bors-ng (archived 2023), Mergify (pricing, app access), Kodiak, Zuul's
   dependent pipelines and Chromium's commit queue in section 3.C.

## 6. Questions for the owner

- Q1. Answered: the repository is public and organization-owned, which the owner reads the
  merge queue page as enough.
- Q2. For `acceptance` under the queue: run the declared test targets on the queue's commit
  (recommended), or accept the weaker guarantee (a `merge_group` step that only reports success, or
  a required-check set that differs between the pull request and the queue, if GitHub allows it)?
- Q3. Under `human`, is "the person's yes lets the pull request into the queue" the person's word
  that merges, or must the person also see the queue's commit land?
- Q4. Is the whole suite on the real base needed on every head, or only `tests/real_base_wire.rs`
  (with the whole suite after the merge)?
- Q5. The review under the queue relaxes Issue 344's `same_changes` rule either way. Which
  relaxation: (i) judged at the pull request's head only, with the queue's CI as the check on the
  combination, or (ii) makibad also checks `same_changes` against the current `main` before it
  allows the enqueue (entries ahead in the queue still unseen; recommended)?
