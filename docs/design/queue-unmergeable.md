# A pull request that turns UNMERGEABLE in the merge queue (Issue 575): a design

Status: approved by the owner (2026-11-27, Q1, Q2 and Q3 as recommended), then **narrowed by the
owner on 2026-11-28** (section 10): makibad no longer updates the branch after the wait; it tells
the secretary which update to run. This text is the narrowed design, whole. The declared test is
`tests/queue_unmergeable_notice.rs`. Code is cited at origin/main adbae89 (the first build's
citations were at 9666228). No GitHub host is reachable from the worker; a claim about GitHub is
marked [unverified]. Goal clause G2: the flow of work does not wait for the person's turn.

## 1. What happens today, and what the Issue asks

- A queue act (makibad's `merge_enqueued`, merge-act.md section 7) and a stay queued outside
  makibad (`merge_queue_entered`, merge-queue-watch.md) are read by the PR watch's state poller
  through `STATE_QUERY` (`src/daemon/prwatch.rs`), which asks for `mergeQueueEntry{id state
  position}`. The entry's `state` is read but **not carried**: `MergeRead` keeps `isInMergeQueue`,
  the timeline and the removal's reason only, and `apply_queue` / `stay_queue` never look at it.
- An entry that turns `UNMERGEABLE` stays in the queue until the owner removes it; then
  `apply_queue` (two reads out of the queue with a removal) writes `merge_dequeued` (`why` `left
  the queue`) and tells the secretary. Everything after it (update, worker, review, CI, queue
  again) is the secretary's and the owner's turn (PR #520 three times, #534 once, 2026-11-26/27).

The Issue: makibad sees the `UNMERGEABLE` entry of a pull request it follows, takes it out of the
queue, waits for the entries ahead, and then tells the secretary to update the branch (narrowed,
section 10: it does not update or push the branch itself); the new head is reported ready once
its CI passed.

### Where the Issue's text and the code disagree

1. "`mergeQueueEntry.state`, already read for readiness": readiness (`src/daemon/ready.rs`,
   `PrWatch::readiness`) reads git only. The field is read by `STATE_QUERY` (queue act and stay)
   and by the membership read (`MEMBER_FIELDS`), and is dropped before `MergeRead`. This design
   carries it into `MergeRead`; no new GitHub read is added for detection.
2. "dequeues it with the same GraphQL path the merge verb uses": the merge pass
   (`src/daemon/merge.rs`) has no dequeue; it has the `enqueuePullRequest` mutation sent through
   `gh api graphql` with its error classes (`DEFINITIVE_GRAPHQL`). The dequeue is a new mutation on
   that same path, in that module (section 3.2). `src/hooks/merge_gate.rs` already lets
   `dequeuePullRequest` through for agents; it is not touched (makibad does not go through it).
3. "no earlier entry that conflicts with it": **not established which entry conflicts**. GitHub's
   entry carries a `state` and a `position`, not the entry it conflicts with [unverified that no
   field names it]. makibad waits for **every** entry ahead of it (a smaller `position` at the
   dequeue's read). Over-approximation: it may wait for an entry that did not conflict; it never
   updates while the conflicting one is still queued. What would decide it: a field of
   `MergeQueueEntry` naming the conflicting entry, read on a real `UNMERGEABLE` entry.
4. "once the new head's CI passed and a review read it, report it ready again": readiness
   (queue-ready.md section 1) reads CI, the merge state and the landings, not reviews; the review
   is the merge gate's (`gate`/`merge`, at the merge request). This design reports with the
   existing readiness notice ("can be queued") and leaves the review to the gate ("an unreviewed
   new head stops as today"). Q2 asks whether that is what the owner meant.
5. "(and queue it, where the merge verb queues)": makibad never enqueues without a `merge`
   request; under `human` the person approves with their key. This design does **not** queue on
   its own: the ready notice is the secretary's signal to ask with `merge`, which judges the gate
   again. Q1.

## 2. Owners, and how 567 and 573 fit

- **Reading the queue** stays with `src/daemon/prwatch.rs` (head comment: "makibad's merge act"
  and "A pull request queued outside makibad"): the state poller's `STATE_QUERY`, `apply_queue`,
  `stay_queue`, and the membership read. Detection (3.1) and the wait (3.3) extend those; no new
  poller or thread kind.
- **Acting on GitHub's queue** stays with `src/daemon/merge.rs` (head comment: "this pass is the
  only writer of the attempts' outcomes"): the enqueue is there, the dequeue joins it (3.2), with
  the same ledger-first rule (the request is committed before anything is sent, never sent twice
  by one start, only read by a later start).
- **Updating the branch** is the secretary's: the door's `update` verb (Issue 183) and its job in
  `src/daemon/push.rs` (a merge, never a rebase or a force; its `Before` checks: failed heads, the
  test commit's design wait). makibad's PR watch writes no `update` command for a wait. A branch
  reset to an ancestor of the pushed head during the wait is refused at that push as not a
  fast-forward. The new head enters readiness as any head recorded with `pr pushed` does. The wait
  is a `queue_readiness` with a new reason, `unmergeable`, so the merge verb's existing refusal
  (`door::merge_check`: newest `queue_readiness` of the head is `update`) holds the pull request
  out of the queue during the wait and after it, until the head is judged again, with no change to
  the merge gate.
- **Issues 567 and 573**: origin/main at 9666228 has no design note of either (no file under
  `docs/design/` names them). They fit the same owners: 567 changes the readiness judgement
  (`ready.rs` and `PrWatch::readiness`), 573 orders pull requests touching the same files and
  updates the next one, which is one more `update` reason of the same `queue_readiness` record
  and the same command path. All three write one record (`queue_readiness`), read by one refusal
  (`merge_check`); their conflicts are textual in `PrWatch::readiness` and `ready_candidates`.

## 3. Design

### 3.1 Detection (PR watch, loop's thread)

- `MergeRead` gains `entry: Option<Value>` (`mergeQueueEntry` as read: `id`, `state`, `position`;
  `None` for `gh pr view`). `poll_state` fills it from the GraphQL read.
- In `apply_queue`, for a queue act not held (`act_holds` false), a read at the act's head with
  `isInMergeQueue` true and `entry.state` `UNMERGEABLE` writes **`queue_dequeue_requested`** when
  the act has no request yet, or only requests ended `not_sent` (3.2). Payload: `repo`, `pr`,
  `head`, `task_id`, `attempt` (the act's attempt), `stay` null, `entry`, `boot`
  (`merge::boot()`). No notice (the outcome tells).
- In `stay_queue`, the same for an open stay (`attempt` null, `stay` the `merge_queue_entered`
  id), **only when** `MAKIBA_UNMERGEABLE_HAND` is `dequeue` (read once in `PrWatch::from_env`;
  unset or any other value: nothing new is written, and the stay is told when it leaves the
  queue, as today). Q3 asks whether an environment setting is the switch the owner wants.
- One read suffices: the merge pass reads the entry again before it sends (3.2).
- While a request of the act or stay is **open** (no `queue_dequeue_ended` names it), `apply_queue`
  and `stay_queue` do not end it as `left the queue`: makibad's own dequeue is ended by its own
  outcome, so the "left the merge queue without landing ... fix the cause, then ask again with
  merge" notice is not sent for it. Every other ending is **not** held: `head moved`
  (`apply_queue`), `closed` (`apply_state`), `head recorded` (`door::pr`, merge.rs's `finish`) and
  a landing end the act or stay as today. The outcome writer revalidates instead (3.2), so a late
  outcome never writes a second ending.

### 3.2 The dequeue (merge pass, a thread per request, gh outside any transaction)

Each tick, every open request without a thread gets one, as every open attempt does. A request of
this start (`boot`) not yet started is **run**:

1. Read the pull request (new `DEQUEUE_READ`: `id state headRefOid baseRefName isInMergeQueue
   mergeQueueEntry{id state position}`), then the whole queue of its base (`QUEUE_READ`:
   `repository.mergeQueue(branch:$base){entries(first:100,after:$cursor){pageInfo{hasNextPage
   endCursor} nodes{position pullRequest{number}}}}`, page after page while `hasNextPage`, at most
   10 pages) [unverified: `Repository.mergeQueue`, `MergeQueueEntry.pullRequest`; the connection
   is paginated as GitHub's reference shows]. A page that fails is a read that fails (below);
   more than 10 pages, or an entry without a readable `position` or `pullRequest.number` (GitHub's
   fields are nullable; diff review round 1), is `failed`, nothing sent (for an earlier start's
   request read out of the queue: `unknown`): an incomplete list is never taken as every entry
   ahead.
2. Not `OPEN`, another head, out of the queue, an entry `id` other than the request's `entry.id`
   (the entry was replaced), or an entry state other than `UNMERGEABLE`: outcome `not_sent` (with
   what was read). Nothing was sent, and this is the only outcome that lets the watch write a new
   request for the act or stay.
3. Otherwise `ahead` = the numbers of the entries with a position smaller than its own (from the
   queue's own snapshot). The pull request is then **read again** (diff review round 2: it may
   have been updated and queued again while the pages were read): anything step 2 names, or
   another node, base or entry id than the first read's, is `not_sent`. Then
   `DEQUEUE_MUTATION` `mutation($id:ID!){dequeuePullRequest(input:{id:$id}){mergeQueueEntry{id}}}`
   with the pull request's node id [unverified: the input `id` is the pull request's].
4. Data for the mutation: outcome `dequeued`. GraphQL errors of a `DEFINITIVE_GRAPHQL` type with no
   data: `failed`. Anything else (a timeout, other errors): read again (step 1); `OPEN`, out of
   the queue, at its head: `dequeued`; anything else read: `unknown`; the read failing: as below.

A request of an earlier start is only **read** (step 1), never sent, since makibad cannot tell
whether that start sent it; every successful read is terminal:

- `OPEN`, out of the queue, at its head: `dequeued` (`ahead` every entry now in the queue, its own
  position being gone: conservative);
- still in the queue (any entry): `unknown` ("an earlier start may have sent the dequeue; GitHub
  shows it still queued"), never `not_sent`, so no second request follows and nothing is sent
  again;
- `MERGED`, `CLOSED`, or another head: `moot` (nothing for makibad to do; the PR watch records the
  landing, the close or the head moved as today).

A read that fails (in either mode) is tried again for up to 300 s from its first failure in this
process, then `unreadable`. So every request ends.

The outcome, on the loop's thread, in one transaction (nothing when the request has an outcome
already):

- **`queue_dequeue_ended`** (`request`, `outcome`: `dequeued`, `not_sent`, `failed`, `unknown`,
  `moot` or `unreadable`; `ahead`; `state`, what was read; `why`).
- **Revalidation.** `merge_dequeued` and the wait record are written only when, in this
  transaction, the request's act is still the one awaiting its landing (`door::act_awaiting`
  names the request's `attempt`) or its stay is still open (`open_stay` is the request's `stay`),
  the run still follows the pull request at the generation it was requested for (`followed`), and
  the run's newest recorded head is the request's head. Otherwise the act or stay was already
  ended by another path (`closed`, `head recorded`, `head moved`, a landing): only
  `queue_dequeue_ended` is written, with a notice for `dequeued` ("makibad took <repo>#<pr> out
  of the merge queue; its queue act had already ended: <the ending>"), and no second
  `merge_dequeued`.
- For `dequeued`, while the act (or stay) is still the pull request's: `merge_dequeued` (`why`
  `unmergeable`, `attempt` the act's or null, `reason` null) through `door::merge_dequeued_with`,
  which ends the act or stay as today; and the **wait record** (3.3), written by
  `prwatch::unmergeable_wait` (prwatch owns `queue_readiness`; merge.rs calls it as it calls
  `door::merge_dequeued`). One notice (the `unmergeable` arm of `merge_dequeued_with`):
  "makibad took <repo>#<pr> at <head> out of the merge queue: GitHub marked its entry
  UNMERGEABLE (it conflicts with an entry ahead of it). It waits for #<a>, #<b> to land or leave
  the queue, then tells you to update the branch with <base>; makibad does not update it itself.
  Do not queue it meanwhile." (with no entry ahead: "It has no entry ahead of it; once GitHub
  shows it out of the queue, makibad tells you to update the branch with <base>; ...").
- For `failed`, `unknown`, `unreadable`: one notice: "makibad could not take <repo>#<pr> at <7>
  out of the merge queue (UNMERGEABLE): <why>. Take it out on GitHub; the PR watch tells you when
  it leaves." Then the watch's rules apply again (the owner's removal is told as today). No
  second request is written for that act or stay (one per act or stay but for `not_sent`).
- For `not_sent` and `moot`: no notice.

### 3.3 The wait and its end (PR watch)

The **wait record** is a `queue_readiness` of the head (verdict `update`, reason `unmergeable`,
`waiting` true, `waits_for` `ahead`, `dequeue` the `merge_dequeued` id, `pr_base` the dequeue
read's `baseRefName` (kept as read; the end uses the base its own read shows), `command` null, the other
fields of queue-ready.md section 3.3 null or as read: `base` the project's default branch,
`merge_state` null). Because its verdict is `update`:

- the `merge` verb refuses the head ("not ready for the queue"), as for any `update` verdict
  (queue-ready.md section 6), with no change to `merge_check`;
- the head is no readiness candidate (rule 5).

A wait is **open** while it is the newest `queue_readiness` of its head, the head is the run's
newest recorded head, and the pull request is followed and not read closed. The membership read
(`PrWatch::memberships`), per repository, adds the numbers of every open wait's `waits_for` to its
aliased query (the same `p<i>`/`n<i>` form and `MEMBER_FIELDS`; a number already followed is read
once). So no call is added for a repository the membership read already reads, and one call per
interval for a repository with waits only. The entries ahead are usually pull requests makibad
does **not** follow, and today `memberships` keeps only answers that match a `Followed` (the
thread's `filter_map`). So `Membership` gains a second list, `waited: Vec<(i64, Value)>`, every
answer by number, kept whether or not that number is followed; `apply_entered` keeps reading
`reads` only, and the wait's end reads `waited` only.

A wait **ends** only from a membership read started after the wait record (`mark` at or above
its id), made for the pull request's current follow (the same generation: no close, reopen or new
head recorded since the read began; diff review round 3), that answers for every number of `waits_for` as gone (`state` `MERGED` or `CLOSED`, or
`OPEN` with `isInMergeQueue` false) and for the pull request **itself** `OPEN`, out of the queue,
with its own `baseRefName` (the read names the pull request itself; a wait with `waits_for` empty
needs that answer too, never the base the dequeue read). A number answered null, the pull request
itself unanswered, without its base, or read queued again, or a failed read, ends nothing (it is
read again next interval). An entry ahead answered `OPEN` out of the queue counts as gone only when
an earlier membership read of the same wait (begun before it: each read has a sequence number)
answered it gone too; `MERGED` and `CLOSED` count at once. One answer of a pull request that is
landing may show it out of the queue before it shows it merged (the declared test's stand-in can
answer so, and once did on CI); the next read then says `merged`. How each entry left (`left`) is
the newest read's answer. An entry answered still queued forgets the earlier answers.

The end, on the loop's thread, in one transaction (`end_wait`), after checking that the wait is
still open for the same follow and head, that no act awaits the pull request's landing, that it
has no open stay, and that no `update` of the task is `accepted`:

- a second `queue_readiness` of the head (verdict `update`, reason `unmergeable`, `waiting` false,
  `waits_for` [], `left`: each number and how it left, `dequeue_wait`: the wait record's id,
  `pr_base`: the pull request's own `baseRefName` as that read showed it, `command` null);
- **one notice** to the secretary: "makibad's wait for <repo>#<pr> at <7> has ended: <why>.
  makibad does not update the branch itself: call update with task <task> and base <pr_base>.
  Once the new head is recorded (pr pushed) and its CI passes, makibad tells you whether it can be
  queued; if the update merges nothing, makibad judges <7> again. Do not queue it at <7>. <link>".
  `<why>`: "the entries ahead of it in the merge queue have left it (#<a> merged, #<b> left the
  queue)", or "it was taken out of the merge queue as UNMERGEABLE, with no entry ahead of it". A
  run without a code worktree (the door refuses its `update`) is told instead "... Its run has no
  code worktree, so update cannot run: have the branch take <pr_base> in and record the new head
  with pr pushed; if it already holds <pr_base>, record <head> (all 40 characters) again with pr
  pushed and makibad judges it again. Do not queue it at <7>. <link>".

No `commands` row is written, and makibad never updates or pushes the branch after a wait. The base
named is the pull request's own, so a pull request into another branch needs no case of its own.

After the end, the existing paths apply:

- the closing record's verdict `update` keeps the `merge` verb refusing the head and keeps it out
  of the readiness candidates;
- the secretary's `update` (`Job.readiness` is `None`: nothing links it to the wait):
  - **merged and pushed**: `branch_updated` and the notice "... Record it with pr pushed and this
    head"; the secretary records the new head with `pr pushed`, its CI is watched, and its passed
    result is a first judgement: the existing "can be queued" notice at the new head (or an
    `update` by readiness's own rule);
  - **a conflict**: `failed` with the files, to the secretary and the worker, as today; the
    worker's head is recorded with `pr pushed` and judged when its CI passes;
  - **nothing to merge** (the entries ahead left without landing; `Done::Current`): rule 5's case
    below.
- `ready_candidates` rule 5 gains one case (`wait_released`): a head whose newest
  `queue_readiness` R is reason `unmergeable`, `waiting` false, is judged again as a first
  judgement when, after R, either (a) an `update` command of the task whose `base` equals R's
  `pr_base`, received at or after R's `occurred_at`, ended `done` and no `branch_updated` of the
  task has an id above R's (an update of another base, or one that merged something, releases
  nothing), or (b) the same head is recorded again (`fix_pushed`, id above R's): the secretary's
  statement that the branch already holds its base, the only way for a run without a code
  worktree. Readiness then judges the head by its own rule (a conflict or a counting landing is
  `update` again), so a wrong statement costs a judgement, not a queueing. A `failed` update is
  not judged again (the worker's turn).

### 3.4 Records

No schema change (`events.kind` is free text). New kinds, kept out of the checked event table as
the merge act's are (so `src/spec.rs` and `tests/spec_matches_code.rs` do not change):
`queue_dequeue_requested` (PR watch) and `queue_dequeue_ended` (merge pass). Extended:
`merge_dequeued` gains `why` `unmergeable` (with `ahead`); `queue_readiness` gains `reason`
`unmergeable` and the fields `waiting`, `waits_for`, `dequeue`, `pr_base`, and on the end `left`
and `dequeue_wait` (absent on the existing reasons); its `command` is always null for
`unmergeable`.

### 3.5 Memory beside the ledger, ordering, restart

Memory only:

- merge.rs: the threads of open requests, by request id, and the first failure of a read (the
  300 s budget), as for attempts; lost at a restart, when every open request of an earlier start
  is only read.
- prwatch.rs: beyond the membership threads (the waits are read from the ledger each pass), the
  sequence number of the next membership read and, per open wait, the entries ahead earlier reads
  answered gone with the earliest such read's number (`gone_seen`). Lost at a restart: an entry
  then needs a second read again, so a wait ends at most one membership interval later.

Ordering: the request is committed before anything is sent; the outcome, `merge_dequeued` and the
wait record are one transaction; the end of the wait and its notice are one transaction; a
membership answer begun before the wait record (`mark` below its id) ends nothing.
Restart: an open request is read, never sent again; an open wait is read from the ledger and its
numbers read again; a wait already ended is not ended again (its second record is the newest).

## 4. Files the build changes

- New: this note; the declared test `tests/queue_unmergeable_notice.rs`.
- `src/daemon/prwatch.rs`: `MergeRead.entry`; `apply_queue` / `stay_queue` (request, hold while
  open); `PrWatch::from_env` (`MAKIBA_UNMERGEABLE_HAND`); `memberships` and the waited numbers
  (`MemberRead`); `unmergeable_wait`, `open_waits`, `apply_waits`, `end_wait`; `wait_released`
  and `ready_candidates` rule 5's case; `Recorded.waiting`; the recovery follow (`recovering`);
  the head comment. Its existing unit tests: the `MergeRead` literals gain `entry: None`, nothing
  else; new unit tests for the wait and its end. `decide` is unchanged.
- `src/daemon/merge.rs`: the open requests, their threads, the queries and the mutation, the
  outcome writer; the head comment; unit tests.
- `src/daemon/door.rs`: `merge_dequeued_unmergeable` beside `merge_dequeued_with` (the event and
  its notice). `merge_check` and the gate are not changed.
- `docs/design/spec.md`: section 5.6 (detection, the wait, the hand switch, rule 5's case), the
  merge kinds paragraph after the event table, the `queue_readiness` paragraph, the `merge` row.
- `docs/design/queue-ready.md` (rule 5's case), `docs/design/merge-act.md` section 7 (a pointer
  here).
- `skills/pull-request/SKILL.md`: lines for the "took ... out of the merge queue" notice, the
  "could not take ... out" notice and the wait-end notice. "Stop lines:" unchanged.
- No other existing test, no `.github/`, no script, no hook, no acceptance checker, no language
  check, no change of the merge gate's judgement, no door verb or tool, no status field, and
  `src/daemon/push.rs` unchanged.

## 5. The declared test: `tests/queue_unmergeable_notice.rs`

`Acceptance-Test: tests/queue_unmergeable_notice.rs`. The world of `tests/queue_ready.rs`, helpers
in the file (none under `tests/common`): makibad in the test process (`tests/common/makibad.rs`
`run_until`, a `Stub` base) on a state database of its own, `MAKIBA_PRWATCH_INTERVAL_MS` 50, one
project per test with its repository and a local bare `origin`, one `review` code run with its
worktree (`run_repo::create`) and its branch at origin, the pull request recorded with
`prwatch::register`. A `gh` stand-in (`MAKIBA_GH`, written through `tests/common/exe.rs`) answers
from files: `gh pr view` and every CI read (passed, finished); `api graphql` with `STATE_QUERY`,
the membership read, `DEQUEUE_READ`, `QUEUE_READ` and the mutation (which takes the pull request
out of the queue and adds a removal). Every call is logged. Landings are commits pushed to
origin's `main` from a clone. The secretary's steps are the door's `handle` as the secretary:
`update` {task, base `main`} and `pr` {`pushed`, repo, pr, head, task}.

1. `an_unmergeable_queue_act_is_dequeued_once_and_the_secretary_is_told_to_update_it` (PR 1 of
   `o/qa`, an act at H1, CI passed; read queued, entry `UNMERGEABLE`, position 2; PR 2 at position
   1, followed by no run). While PR 2 is queued: one `dequeuePullRequest` call naming PR 1's node
   id, one request, one outcome `dequeued` with `ahead` `[2]`, one `merge_dequeued` (`why`
   `unmergeable`, the act's attempt, H1); the wait record (`waiting` true, `waits_for` `[2]`); the
   dequeue's notice and none "left the merge queue without landing"; no `update` command; the
   door's `merge` at H1 refused ("not ready for the queue"), no attempt added. Then a landing L1
   and PR 2 `MERGED`: one closing record (`left` `[{"pr":2,"how":"merged"}]`, `command` null), no
   `update` command, origin's branch still H1, no `branch_updated` or `fix_pushed`, one wait-end
   notice ("#2 merged", "call update with task <task> and base main"), no ready notice of H1. Then
   the secretary's `update` through the door: one `update`, the secretary's, `done`; one
   `branch_updated` H1 to H2 = origin's branch, holding L1 and H1; the secretary's `pr pushed` at
   H2; exactly one "o/qa#1 at <H2 7> can be queued", a `ready` record of H2; still one dequeue, one
   `merge_dequeued`, no `update` of makibad, no ready notice of H1.
2. `an_unmergeable_entry_queued_by_hand_is_left_to_the_owner_unless_turned_on` (PR 3 of `o/qb`, no
   act, `MAKIBA_UNMERGEABLE_HAND` unset): one `merge_queue_entered`, and after ten intervals no
   dequeue call, request or `merge_dequeued`. A second makibad over the same database with the
   switch `dequeue`: one dequeue call and one `merge_dequeued` (`unmergeable`, `attempt` null).
3. `a_dequeued_head_whose_entry_ahead_left_without_landing_is_ready_again_after_the_secretarys_update`
   (PR 4 of `o/qc`, an act as in 1; PR 5 ahead then `OPEN` out of the queue; `main` unchanged): one
   closing record (`left` `[{"pr":5,"how":"left the queue"}]`), no `update` command, one wait-end
   notice, no ready notice; then the secretary's `update`: one, `done`, no `branch_updated` or
   `fix_pushed`, origin's branch H1, exactly one "o/qc#4 at <H1 7> can be queued", the newest
   record of H1 `ready`.

The GitHub answers (field names, the mutation's input, `UNMERGEABLE`) are the stand-in's
[unverified].

## 6. Known limits

- makibad waits for every entry ahead, not only the one that conflicts (section 1, point 3).
- An entry that turns `UNMERGEABLE` and back between two reads is not seen; one read is enough to
  request, and the merge pass reads again before sending.
- `dequeuePullRequest` takes the pull request's id, not the entry's: between the last read and the
  mutation (one call apart) a new entry of the same pull request could still be taken out. The
  read just before sending narrows that window to one call; GitHub offers no entry-bound dequeue
  [unverified].
- A pull request makibad does not follow is not handled.
- The dequeue and the queue read are [unverified] against GitHub until the first real case.
- makibad does not queue again (Q1); the secretary asks with `merge` on the "can be queued" notice.
- The secretary's update is not linked to the wait: its new head is recorded by the secretary
  (`pr pushed`), not by makibad. An update of the task run and recorded while the wait is open
  moves the newest head, which closes the wait silently (the new head is judged as any).
- Rule 5's "merged nothing" is read from the ledger's order (an `update` of `pr_base` `done` after
  the closing record, no `branch_updated` since), at the ledger's millisecond clock.

## 7. Questions

- **Q1, queueing again.** Recommended: makibad reports ready ("can be queued") and the secretary
  asks with `merge` (the gate judges review and CI again; under `human` the person approves).
  Alternative: makibad writes a `merge` request itself on the ready notice of a head it dequeued
  (a new path to the gate; a separate Issue).
- **Q2, "a review read it".** Recommended: the merge gate's review rule at the merge request (as
  today; commits after the reviewed head that are clean merges of the base, as makibad's update
  makes, are let through by the gate's `after_review` reading, `src/daemon/gate.rs`, while a
  conflict resolved by the worker needs its diff review). Alternative: readiness
  also waits for a passed diff review of the head before "can be queued" (changes readiness for
  every pull request).
- **Q3, the hand switch.** Recommended: `MAKIBA_UNMERGEABLE_HAND=dequeue` in makibad's
  environment, as `MAKIBA_MERGE_APPROVAL_MODE` is. Alternative: a setting recorded in the ledger
  by the person.

## 8. The design review (round 1, reader codex: passed, 2 blockers, 4 majors)

The reader had the summary only (this note was not committed, by the brief's rule); each finding
is answered in this text:

- Major, the full note absent from the tree: it stays uncommitted until approval; the secretary
  reads it at `docs/design/queue-unmergeable.md` in the worktree.
- Blocker, answers of unfollowed entries ahead are dropped by `memberships`: `Membership.waited`
  keeps every answer by number (3.3); test 1's PR 2 is followed by no run.
- Blocker, a restart read had no outcome for a merged, closed or moved pull request: every
  successful read of an earlier start's request is terminal (`dequeued`, `unknown`, `moot`), and a
  failing read ends `unreadable` after 300 s (3.2).
- Major, `entries(first:100)`: the queue is read page after page; more than 10 pages is `failed`
  and nothing is sent (3.2 step 1).
- Major, other paths end the act while a request is open: only `left the queue` is held; the
  outcome writer revalidates the act or stay, the follow's generation and the head, and writes no
  second `merge_dequeued` (3.1, 3.2).
- Major, `not_sent` after a restart does not prove nothing was sent: an earlier start's request
  still queued ends `unknown` (no new request); `not_sent` is written only by a read made before
  this start sends, and also when the entry's `id` changed (3.2 step 2).

## 9. Where the build differs from this text

- The `unmergeable` notice and record are a function of their own in `src/daemon/door.rs`,
  `merge_dequeued_unmergeable`, beside `merge_dequeued_with` (not an arm of it): it returns the
  event's id for the wait record, and `merge_dequeued`'s payload carries `ahead`.
- The answers of the entries ahead travel beside `Membership` on the membership thread's channel
  (`MemberRead`), not as a field of it, so the unit tests' `Membership` literals do not change.
- `apply_queue` keeps its signature for the unit tests (`#[cfg(test)]`); the pass calls
  `apply_queue_with`, which takes the hand switch.
- Diff review rounds of the first build (branch makiba/r25-575, codex), each fixed and kept here:
  - round 1: the three new GraphQL calls name `--hostname github.com`, as the PR watch's do; a run
    whose queue act makibad took out of the queue stays followed whatever its status or age
    (`recovering` in `followed`) until a day after a `queue_readiness` of the pull request says
    `ready` or `unknown`, or until it lands, is closed, or another attempt is made; unit tests in
    `merge.rs` (an earlier start's request read and never sent; a sent dequeue whose answer cannot
    be read; the queue read page after page, a longer queue and a changed entry sending nothing).
    (Its finding that makibad's update would merge the wrong base is moot since section 10: the
    notice names the pull request's own base.)
  - round 2: the entries ahead are read from the queue's own snapshot (the pull request's position
    in the same read; missing from it, every entry counts); the recovery follow lasts a day past
    the ready notice.
  - round 3: the membership read also asks `baseRefName`; a stay taken out with the switch on keeps
    its run followed as an act does.
  - rounds 4-6: a wait ends only from a membership read started after it that answers for the pull
    request itself, `OPEN`, out of the queue, with its own `baseRefName`, never on the base the
    dequeue read; a wait does not end while the pull request has an open stay or is read queued
    again, closed or merged.

## 10. The narrowing (owner, 2026-11-28, pending entry 131)

The first build (PR 599) had makibad itself update and push the branch after the wait. Its six
diff-review rounds each found another field that update depended on (the base, the follow, the
queue snapshot, the stay, the pull request's own state); the last was a branch reset to an
ancestor during the wait, which would have pushed removed commits back. The owner narrowed the
Issue: makibad still dequeues (a hand stay only with `MAKIBA_UNMERGEABLE_HAND=dequeue`) and waits,
and at the wait's end writes the closing record with no update command and sends the secretary one
notice naming the pull request, the head that waited, the entries that left and the update to run.
The secretary's `update` goes through push's own checks; the head it produces is told "can be
queued" once its CI passes; if it merges nothing, the same head is judged again (rule 5's case).
Removed with it: the `update` command of the wait's end, the factoring of `decide`'s command
insertion (`update_and_tell`), and the case of a pull request into another branch (`not_updated`).
The first declared test (`tests/queue_unmergeable.rs`) asserted makibad's update and is not kept.

Design review of the narrowing (round 1, reader codex: passed, 1 blocker, 2 majors), each answered
in this text: the blocker, that the reader had only a delta against a note not on origin/main
(answered by committing this note whole); a no-op update of any base would release the hold (rule
5 (a) requires `pr_base`); a run without a code worktree cannot run `update` (its own notice and
rule 5 (b)).
