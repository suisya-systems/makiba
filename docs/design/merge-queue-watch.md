# The PR watch tells when a pull request leaves the merge queue without landing (Issue 476): a design

Status: approved by the secretary (2026-11-25) and built: the declared test is
`tests/merge_queue_watch.rs`. It builds on docs/design/merge-act.md (Issue 425) and on Issue 434
(merge-act.md build Issue 4), whose approved design and test (`tests/merge_landing.rs`, test
commit 254cd88 on branch `makiba/r8-434`) make the PR watch follow the merge queue **for a pull
request makibad enqueued itself**. Issue 434's fix had not landed when this was written; this
note cites its test, and its approved behaviour as that test states it.

**Prerequisite.** 434's fix is on origin/main before anything of this Issue is committed: the
branch is made again from that main, the test commit is written there, and the build and its
checks run against it. On today's main (3bb84aa) `poll_state` only calls `gh pr view` and there is
no queue reader to extend.

## 1. What Issue 434 already does, and what is left

Issue 434, while a pull request has a **queue act** (merge-act.md section 7: `merge_enqueued`, or
`merge_unknown` observed `enqueued`, with no `merge_dequeued`, `pr_merged` or `pr_closed` after
the attempt; `door::act_awaiting`):

- reads the pull request with one GraphQL call per interval (`isInMergeQueue`,
  `mergeQueueEntry`, and the timeline items `AddedToMergeQueueEvent` /
  `RemovedFromMergeQueueEvent` with their `createdAt`) in place of `gh pr view`;
- ends the act with one `merge_dequeued` and one notice ("<repo>#<pr> left the merge queue
  without landing"): `left the queue` on two reads out of the queue at the act's head with a
  removal on the timeline after the attempt; `head moved` on one read at another head; `closed`
  on a read closed, before its `pr_closed`;
- does not end it on a read out of the queue with no removal (the pull request may be landing),
  and classes the landing from the act record and the timeline.

So these parts of Issue 476 are covered by 434 and are not built again: the GraphQL read of the
queue fields, the two-read rule, "a pull request that lands is not reported as dequeued, also when
the landing is read in a later pass than the exit", and `merge_dequeued` for makibad's own enqueue.

What Issue 476 adds:

1. **A pull request the person queued** (the web UI), with no makibad act: today nothing reads its
   queue membership, so its entering and its leaving go unseen (#439, #441, #442, #446 on
   2026-11-24). This is the main part.
2. **GitHub's reason** (`RemovedFromMergeQueueEvent.reason`) in `merge_dequeued` and its notice,
   for both cases (434's `left the queue` too).
3. **One record of entering** for a person's enqueue (makibad's own enqueue already has
   `merge_enqueued`).
4. The docs: merge-act.md and the spec say one event kind, `merge_dequeued`, serves both cases.

## 2. Owner, and no second queue reader of a pull request

`src/daemon/prwatch.rs` stays the only watcher and owns all of this. It already has one reader per
duty (the CI pollers, the state pollers, the attempt scouts); this adds one duty, **membership**,
for the pull requests that no queue reader follows yet, and hands each one it finds in the queue
to 434's reader. At any moment a followed pull request is in exactly one of two sets:

- **followed in the queue** (a queue act, as 434; or an open *hand stay*, section 3): its state
  poller makes 434's GraphQL read, which alone decides its leaving and landing;
- **not followed in the queue** (every other followed pull request read open): its state poller
  reads `gh pr view` as today, and the **membership read** (section 4) asks GitHub whether it is in
  the queue.

The predicate 434 uses to choose the GraphQL read for a pull request ("it has a queue act") gains
"or an open hand stay"; nothing else of 434's reader changes but the reason (section 5). So no pull
request is read for the queue by two readers, and the membership read never decides a leaving.

## 3. Records

Events (no schema change: `events.kind` is free text). Both are written by the PR watch, on the
loop's thread, actor `makibad`, on the run of the followed pull request.

| Event | When | Payload |
| --- | --- | --- |
| `merge_queue_entered` (new) | the membership read finds a followed pull request in the queue that has no act awaiting its landing and no open hand stay | `repo`, `pr`, `head` (the read's `headRefOid`), `entry` (`mergeQueueEntry`: `id`, `state`, `position`), `attempt` null |
| `merge_dequeued` (merge-act.md section 4.1, unchanged kind) | a queue act or a hand stay ends without a landing | as today (`repo`, `pr`, `head`, `attempt`, `why`, `state`), `attempt` null for a hand stay, and a new `reason`: GitHub's `RemovedFromMergeQueueEvent.reason` of the removal that ended it, or null (not readable, or `why` not `left the queue`) |

A **hand stay** is open while its `merge_queue_entered` has no `merge_dequeued`, `pr_merged` or
`pr_closed` of the same pull request (repository in any case) after it. While the pull request has
an act awaiting its landing (`door::act_awaiting` is `Some`: an open attempt, a direct merge not
yet read, or a queue act), the stay is **held**: 434's rules own the pull request and the stay
writes nothing. When the act ends without a landing by a `merge_dequeued` (434), that event is
after the stay's entry and ends the stay too, so one exit gets one `merge_dequeued`. When the act
ends any other way (`merge_failed`, `merge_unknown` observed `open`, `closed`, `merged_other_head`
or `unreadable`), the stay is open again and its rules apply from the next read: a manually queued
pull request whose makibad attempt failed is still followed in the queue. A `merge_dequeued` of a
stay is only written while no act awaits, so it can never end a makibad act wrongly
(`act_awaiting` reads a `merge_dequeued` after the attempt as the act's end).

`merge_queue_entered` writes no notice (the person queued it, and the secretary is told when it
leaves or lands); it is the record the Issue asks for, the boundary for the leaving rule, and the
durable "already recorded" for a restart.

## 4. The membership read

- **What is read.** Per repository (lowercased), at most once per interval
  (`MAKIBA_PRWATCH_INTERVAL_MS`), one `gh api graphql` call for every followed pull request of the
  repository that is not read closed, has no act awaiting its landing and no open hand stay: `query($owner,$name,
  $n0:Int!,...){repository(owner:$owner,name:$name){p0:pullRequest(number:$n0){number state
  headRefOid isInMergeQueue mergeQueueEntry{id state position}} p1:...}}`, the numbers passed as
  `-F n<i>=<number>`. This is the "one read per pass for all followed pull requests of a
  repository" of the Issue. The fields are those merge-act.md section 10 claim 3 verified on PR 428
  and 429. Aliased fields in one query are standard GraphQL; that GitHub answers them in one call
  for up to the followed count (a few) is [unverified] and needs no new claim beyond the field
  names.
- **Who runs it.** `PrWatch::pass` spawns one thread per repository when none of that repository
  is running and the last began at least one interval ago (a one-shot read, not a long-lived
  poller), as the CI and state threads: gh outside any transaction, no database, the result on a
  channel. Each spawn carries `mark`, the largest `events.id` when it was spawned. `Drop` cancels
  and joins them, as the other threads.
- **What is written.** On the loop's thread, per pull request in the answer whose `state` is
  `OPEN` and `isInMergeQueue` true, in one transaction: the pull request is still followed at the
  same generation (`followed`), not read closed, has no act awaiting its landing (`door::act_awaiting`), no open
  hand stay, and no `merge_attempted`, `merge_enqueued`, `merge_unknown`, `merge_dequeued`,
  `merge_queue_entered`, `pr_merged` or `pr_closed` of it with an id above `mark` (a read begun
  before such a record may show a queue membership that has already ended or been taken by an act);
  then `merge_queue_entered`. Anything else writes nothing: a pull request read out of the queue, a
  failed or unreadable call, a pull request missing from the answer.
- **After it.** The pull request now has an open hand stay, so its state poller is changed to
  434's GraphQL read (by the same mechanism 434 uses when an act becomes a queue act).
- **Calls.** Per interval: one membership read per repository with a followed pull request not in
  the queue, and, as 434 approved, one GraphQL read per pull request followed in the queue (in
  place of its `gh pr view`, so no call is added for it). Folding 434's per-pull-request reads into
  the repository's one read is section 9's question.

## 5. Leaving and landing of a hand stay (434's reader, its rules)

434's `apply` for a queue act is reused for a hand stay, with these values in place of the act's:
the head is the stay's `head`; `attempt` is null; and the removal test uses **no clock**. A ledger
time is no boundary for a stay: the membership read may be committed long after GitHub answered it
(a gh call may take 120 s, and the loop writes it later still), so a removal can precede the stay's
record. Instead the test is the order of GitHub's own items: the newest queue item of the timeline
is a `RemovedFromMergeQueueEvent` (newer than every `AddedToMergeQueueEvent` read). The stay was
entered on a read that showed the pull request in the queue, so an addition at least as new as
that read is on the timeline; a removal newer than every addition is therefore a removal after the
stay began, whenever its record was written. The timeline read asks for the newest queue items
only (`timelineItems(last: 20, itemTypes: [ADDED_TO_MERGE_QUEUE_EVENT,
REMOVED_FROM_MERGE_QUEUE_EVENT])`); where 434's query reads them otherwise, the build changes that
one query, and 434's attempt boundary for acts is kept as it is.

- Read `OPEN` at the stay's head, out of the queue, and the newest queue item a removal: 434's
  two-read rule, then `merge_dequeued` (`why` `left the queue`, `reason` that removal's
  `reason`).
- Read `OPEN` at another head: `merge_dequeued` (`why` `head moved`), one read.
- Read `CLOSED`: `merge_dequeued` (`why` `closed`) before `pr_closed`, in the same transaction.
- Read `MERGED`: `pr_merged` as today (no act: `merged_via` `hand`); it ends the stay; no
  `merge_dequeued`, whether the exit from the queue was read in an earlier pass or not.
- Read out of the queue with the newest queue item an addition, or no queue item: nothing (it may
  be landing).

The timeline read of 434 gains `reason` on `RemovedFromMergeQueueEvent`
(`... on RemovedFromMergeQueueEvent{createdAt reason}`) [unverified: the field name and its
values; the Issue names it, merge-act.md section 10 claim 5 leaves it open]. When the field cannot
be read the reason is null and the notice says GitHub gave none; nothing else depends on it.

The notice, one per `merge_dequeued` of `why` `left the queue`, for both cases (434's text
extended):

- "<repo>#<pr> at <head> left the merge queue without landing (GitHub's reason: <reason>, or: GitHub
  gave no reason); it is still open. <who>. Read why on the pull request; fix the cause (for a
  conflict, update the branch and record the new head), then ask again with merge."
- `<who>` is "makibad had enqueued it" for an act, "It was queued outside makibad" for a hand stay.

`head moved` and `closed` of a hand stay are told with 434's texts for those cases, naming that it
was queued outside makibad.

## 6. Memory beside the ledger, ordering and restart

The ledger is the only record. In memory:

- the membership threads, by repository, and the time each began (to space them one interval);
  lost at a restart, when the first pass spawns them again;
- 434's note of a first read out of the queue with a removal (the two-read rule), keyed for a hand
  stay by the stay's event id instead of the attempt; lost at a restart, which costs one more
  interval, as 434 says.

Ordering: a `merge_queue_entered` is committed before the state poller switches to the GraphQL
read; a membership read begun before any queue record of its pull request (`mark`) is dropped. A
membership read delivered late, after the pull request already left the queue, still writes its
entry (it showed the pull request in the queue, and no queue record is newer than its `mark`); the
next GraphQL read then finds the newest queue item a removal and ends the stay: one entry, one
exit.
Restart: an open hand stay is read from the ledger, so no second `merge_queue_entered` is written
while it is open, and a stay ended by `merge_dequeued` is not ended again; nothing is re-sent or
re-told. A stay that lasted less than one interval between two membership reads is not seen (known
limit: a queue build takes minutes; #439 stayed far longer).

## 7. Files the build changes

- `src/daemon/prwatch.rs`: the membership read and its threads, `apply_entered`, the hand stay in
  434's queue predicate and its apply, `reason` in the timeline read, `merge_dequeued` and the
  notice. Head comment: the membership duty and "a hand stay".
- `src/daemon/door.rs`: `act_awaiting` unchanged (section 3 says why a hand stay's
  `merge_dequeued` cannot end an act). Built: Issue 434 had put the writer of `merge_dequeued` and
  its notice in `door::merge_dequeued`, so it gained `merge_dequeued_with` (the head, the attempt
  or none, the reason), which it calls; approved by the secretary during the build.
- `src/daemon/prwatch.rs`'s unit tests: three `MergeRead` literals gain `removal_reason: None`
  (the read's new field), nothing else; approved by the secretary during the build.
- `docs/design/merge-act.md`: section 4.1 (the `merge_dequeued` row: also a hand stay, `attempt`
  null, `reason`; a `merge_queue_entered` row), section 7 ("The queue read": the membership read
  and the hand stay, pointing here), section 10 claim 5 (the reason).
- `docs/design/spec.md`: the paragraph of the merge kinds after the event table (line 764 today:
  `merge_queue_entered`, `merge_dequeued`'s second writer and `reason`) and section 5.6 (the
  membership read). The kinds stay out of the checked table, so `src/spec.rs` and
  `tests/spec_matches_code.rs` do not change.
- **An existing test:** `tests/pr_merge_state.rs`, in
  `behind_head_passed_notice_names_it_behind` (lines 264-281), which asserts that
  every gh call is the state read or a CI read; it gains the membership read
  (`api graphql` naming `isInMergeQueue`) as a third allowed call. Its stand-in already answers
  `exit 2` to it, which the membership read takes as a failed read. Other existing stand-ins answer
  the same way; the build runs the whole suite, and any other existing test that the extra call
  breaks is a stop and a question, not an edit.
- No skill, no `.github/`, no script, no schema step.

## 8. The declared test

`tests/merge_queue_watch.rs` (Acceptance-Test), built as `tests/merge_landing.rs`: the PR watch's
pass in this process on a state database of its own; a gh stand-in (`MAKIBA_GH`, written through
`tests/common/exe.rs`) answering `gh pr view`, the per-pull-request `gh api graphql` (434's read,
`data.repository.pullRequest`) and the membership read (`-F n<i>=<number>`, answered
`data.repository.p<i>`) from one file per pull request, logging every call per repository;
`MAKIBA_PRWATCH_INTERVAL_MS` 50. Helpers live in the test file.

1. `a_pull_request_queued_by_hand_that_leaves_open_gets_one_dequeue_and_one_notice`: a followed
   open pull request with no act, read in the queue, then out of the queue at its head with an
   addition and a later removal (`reason` `FAILED_CHECKS`) on its timeline. Assertions: exactly
   one `merge_queue_entered` with `attempt` null and `head` the head; after the removal, exactly one
   `merge_dequeued` with `why` `left the queue`, `attempt` null, `head` the head, `reason`
   `FAILED_CHECKS`; exactly one notice to the secretary containing "left the merge queue without
   landing", `o/q1#1`, the full head and `FAILED_CHECKS`; after ten more intervals still one of
   each.
2. `a_pull_request_read_out_of_the_queue_then_merged_is_not_dequeued`: queued by hand (entered),
   then read out of the queue at its head with no removal for ten intervals, then read merged.
   Assertions: one `merge_queue_entered`; no `merge_dequeued`; no notice containing "left the merge
   queue"; one `pr_merged` with `merged_via` `hand`.
3. `a_restart_writes_nothing_again`: case 1's pull request after its `merge_dequeued`, and a second
   one still in the queue with its `merge_queue_entered`; the PR watch is dropped and a new one
   made with the same answers, run for ten intervals. Assertions: still one `merge_queue_entered`
   and one `merge_dequeued` for the first, one `merge_queue_entered` and no `merge_dequeued` for
   the second, and one "left the merge queue without landing" notice in all.
4. `the_membership_of_a_repositorys_pull_requests_is_one_read`: two followed open pull requests of
   one repository, neither in the queue. Assertion: a logged `api graphql` call carries both
   `n0=` and `n1=` with their numbers; no `merge_queue_entered`.
5. `makibads_own_queue_act_is_not_entered_again_and_its_dequeue_names_the_reason`: a pull request
   with `merge_attempted` and `merge_enqueued`, read in the queue, then out of it with a removal
   after the attempt (`reason` `MERGE_CONFLICT`). Assertions: no `merge_queue_entered`; one
   `merge_dequeued` with `attempt` the attempt's id and `reason` `MERGE_CONFLICT`; its notice
   contains `MERGE_CONFLICT`.
6. `a_hand_stay_outlives_a_failed_attempt`: a pull request entered by hand, then a
   `merge_attempted` and its `merge_failed` (`sent` false), then read out of the queue at its head
   with the newest queue item a removal (`reason` `FAILED_CHECKS`). Assertions: one
   `merge_queue_entered`; one `merge_dequeued` with `attempt` null and `why` `left the queue`; one
   notice naming the pull request and `FAILED_CHECKS`.
7. `a_membership_read_delivered_after_the_exit_gives_one_entry_and_one_exit`: the membership read
   of a pull request in the queue is held by the stand-in (`hold-<n>`, as `tests/merge_landing.rs`)
   after it took its in-queue answer; the answer is then changed to out of the queue with an
   addition and a later removal, both timed more than a minute before the release, and the read is
   released. Assertions: one `merge_queue_entered`, then one `merge_dequeued` (`why` `left the
   queue`) and one notice; after ten more intervals still one of each.

The reason values are the stand-in's [unverified].

## 9. Questions this note leaves to the secretary

- **Base.** The fix needs 434's reader. The test commit is written on an origin/main that has 434's
  fix (this branch is created again from it; nothing is committed before).
- **Batching the pull requests followed in the queue (the design reader's major).** The Issue asks
  for one read per repository "where possible". This note batches the membership of every followed
  pull request not in the queue, and leaves each one in the queue to 434's per-pull-request read,
  which the person approved and which replaces that pull request's `gh pr view` (no call added).
  Folding those into the repository's read too would make it carry the state, merge-state and
  timeline fields and hand each pull request's part to `apply_state` and to `MergeRead`: a rework
  of 434's approved reader, not an extension of it. Recommended: as this note, with the fold
  proposed as a follow-up once 434 has landed and the real queue is observed. The alternative is
  to fold them now, in this Issue, with a test of two queued pull requests read in one call.
- **Option A instead of section 4.** Make 434's GraphQL read the state read of every followed open
  pull request (no membership read, one reader) — but then every existing test whose stand-in
  serves the PR watch's `gh pr view` (about fifteen files: `tests/pr_merge_state.rs`,
  `pr_hand_merge.rs`, `abandoned_landing.rs`, `belt*.rs`, `landing_test_commits.rs`, ...) must
  answer `api graphql` too, and the read is per pull request, not per repository. This note
  recommends section 4 (one existing test touched, the Issue's per-repository read).
