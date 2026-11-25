# A pull request that adds a check waits for the open test commits (Issue 331)

Design note for Issue 331. Goal clause G3: past incidents do not come back.

## 1. What happened, and what is true today

- On 2026-11-23 PR 314 (Issue 312) landed `tests/knowledge_isolation.rs`, whose scan refuses every test target that starts makibad except through `tests/common/makibad.rs` (added in the same pull request). PR 315 (Issue 282) and PR 316 (Issue 307) were open with test commits written before that helper existed. A declared test cannot change after its test commit (`scripts/acceptance-check.sh` rule 2 and 4; the gate's condition (a)), so neither could pass the new scan; both were rebuilt (PR 323, PR 324), each at the cost of a CI cycle and a review round.
- The owner decided: avoid it by the order of merges; add no exception to any check.
- Issue 461 (`src/daemon/landing.rs`, docs/design/landing-test-commits.md) already tells open runs with a pushed test commit **after** a landing that changed `tools/`, `.github/` or `tests/common/`. It runs git only: the person decided (pending entry 50) that nothing of a project runs outside a sandbox, so makibad never runs project code. It stops nothing.
- The merge gate (`src/daemon/gate.rs`, judged and recorded by `Door::judge` in `src/daemon/door.rs`, shared by the verbs `gate` and `merge`) runs no test either.

## 2. The question the Issue leaves open, and the options

The Issue's example ("the merge gate runs the new check against the declared tests of the other open pull requests") would have makibad run project code. That is what the person refused in pending entry 50. So the options are:

1. **Conservative, git only (recommended).** A pull request is *check-adding* when its own changes touch a path that may bring a check over `tests/`. While any other open pull request makibad follows in the same repository has a test commit, the gate holds it, naming those pull requests. Whether that test commit would really fail the check is not judged: every open test commit counts. No project code runs.
2. **makibad runs the check in a sandbox** over each open test commit merged with the candidate's head. It judges exactly what the Issue asks, but reverses pending entry 50 and needs a sandbox runner of makibad's own (a new capability, a build of minutes per open pull request inside a door call). Only the person can choose this.
3. **The other runs' workers run the check** in their sandboxes (as landing.rs asks after a landing) and report. That is a worker's word, not a check; the Issue asks for a check. Not proposed.

The rest of this note is option 1. Its difference from the Issue's text: "allowed ... when no declared test fails it" becomes "allowed when no other open pull request has a test commit"; a declared test that would pass the check still holds the merge (the person decides, below).

## 3. Design (option 1)

### 3.1 The owners

- What may bring a check over `tests/`: `src/daemon/landing.rs`, which owns that list (`WATCHED`, the person's list of Issue 461). A new, separate constant beside it, `CHECKS`, is `WATCHED`'s three folders plus `scripts/acceptance-check.sh` (a rule of the acceptance check: CI runs the base's copy over every open pull request). `WATCHED` is not replaced, and landing.rs's scan and notices are not changed (whether its notices should also read `scripts/acceptance-check.sh` is a proposed follow-up).
- Which pull requests are open and known: `prwatch::followed` (`src/daemon/prwatch.rs`), the set the merge verb already requires the candidate to be in. No second list.
- The refusal: the gate. `gate.rs` gains the git reads (3.2), `door.rs` the rows (`gate_rows`), the reads outside the lock (`gate_outside`) and the judgment (`judge`), beside the edited-tests rule (Issue 282), which has the same shape.

### 3.2 What the gate reads

In `gate_outside`, after `gate::evaluate`, for the pull request P at head H on base B (the ids the gate fetched):

1. `gate::check_paths(clone, B, H)`: the paths P's own changes touch (`git diff --name-only <merge-base B H> H`, with `core.quotePath=true` and the quoted names unquoted, as landing.rs reads them) that are under one of `landing::CHECKS`. Empty: P is not check-adding and nothing more is read.
2. Otherwise, each other followed pull request O of the same repository (any case), other than P's number and not read closed (`Followed.closed` false: a pull request read closed unmerged holds nothing even while its run is open or is a completed range run; read reopened, it holds again), read by `gate_rows` from `prwatch::followed` (task, pr, and its generations `gen_reg` and `gen_close`): `gate::fetch(clone, repo, O, base_branch)` (the gate's own fetch, git only, the refs under `refs/makiba/gate/`) and `gate::find_manifest(clone, B, <O's fetched head>)`. With a manifest M: O waits (its number, task, M and declared paths). A fetch or read that fails: O is named as not read, with git's reason, and holds P as a waiting one does (never read as "no test commit"). Without a manifest: O is not named. For each waiting O the gate also notes whether O's own changes touch `landing::CHECKS` (the same `check_paths`, from B to O's head): then O is check-adding too, and the two hold each other (3.3).

All of 3.2 (every fetch and manifest read) runs in `gate_outside`, with the door's lock released and no transaction open, as the gate's own fetch does today; `judge` only re-reads the ledger and records.

### 3.3 The judgment

In `judge`, after the edited-tests rule and before the range rule, when the list of waiting and not-read pull requests is not empty:

- Under `merge_approval_mode` `acceptance`, an answer that would allow becomes `allow` false with verdict `merge_order`; reason: `<repo>#<P> changes <paths, at most 10> (it may add or tighten a check over tests/); open pull requests have test commits whose declared tests cannot change: #<O> (task <t>, test commit <7>: <declared>), ...; not read: #<O> (<why>). It waits until they land or are closed, or the person decides: the person merges it in the GitHub web UI`.
- Under `human`, `allow` stays, `warn` is true, the line goes into `warnings` (so the pending merge entry the person approves with their key shows it), and the reason carries `WARNING <line>: the person decides this landing`.
- A mutual hold (a waiting O that is check-adding too) is not resolved by makibad: the line adds `#<O> is check-adding too: the two hold each other; the person decides their order`, under both modes as above (refused under `acceptance`, a warning under `human`). No rule of makibad's lets one through first: whether one's check would refuse the other's declared test is exactly what makibad cannot judge without running project code (option 2).
- A refusal of an earlier rule keeps its verdict and has the line appended.
- A range task's refusal is a gate stop, as every refusal is (`belt::gate_stop`; the verdict `merge_order` is not `tests_edited`, so `refused` is true).

The `acceptance_gate` row gains `merge_order` when the list is not empty: `paths` (step 1), `waits_for` (each `pr`, `task`, `test_commit`, `declared`) and `not_read` (each `pr`, `task`, `why`). No new event kind, no schema change.

"Changed while the gate ran": `gate_rows` takes a snapshot of the candidates, each followed pull request of the repository other than P with its `task`, `pr`, `gen_reg` (newest `pr_opened` or `fix_pushed`), `gen_close` (newest `pr_closed` or `pr_reopened`) and `closed`. `judge` reads `gate_rows` again inside its `BEGIN IMMEDIATE` transaction, and any difference of that snapshot (a pull request newly recorded, a new head, a close, a reopen, a landing that left the set) makes the answer `rejected` with "the open pull requests of the repository changed while the gate ran; ask again", as a review or CI change does today. So a test commit recorded while the fetches ran cannot escape the hold.

### 3.4 When the hold ends

O stops holding P when it is no longer followed: its `pr_merged` is recorded, or it was read closed (`pr_closed`) and its run left the followed set, or its run was closed. Then the next `merge` (or `gate`) call allows P. Nothing is watched or woken for it: the secretary asks again (the skill says when, 3.6).

### 3.5 State, ordering and restart

No state in memory beside the ledger. Each answer reads the followed set and git anew; a restart changes nothing.

### 3.6 Where the rule is written

- `skills/pull-request/SKILL.md`: in "What the gate checks", the rule (a pull request whose own changes touch `tools/`, `.github/`, `tests/common/` or `scripts/acceptance-check.sh` waits while another open pull request makibad follows has a test commit; the gate refuses it under `acceptance` with verdict `merge_order` naming them, and warns under `human`); in "Merging", what to do on `merge_order`: merge the named pull requests first, call merge again after their landing notice; when they wait for this one (each holds the other) or the person wants otherwise, tell the person. A new stop line is not added: the existing line 7 ("The merge gate refused") holds it; "Stop lines:" stays (tests/conveyor_skill.rs).
- `docs/acceptance-check.md`: a section "Merge order" with the rule and its limits.
- `docs/design/spec.md`: the `gate` row of the verb table (section 12) gains the rule and the verdict `merge_order`; the `acceptance_gate` paragraph gains the payload field `merge_order`. No event kind changes, so `src/spec.rs` and `tests/spec_matches_code.rs` do not change.

### 3.7 Files the build changes

- New: this note; the declared test `tests/merge_order.rs`.
- `src/daemon/landing.rs`: the constant `CHECKS` (public in the crate); nothing else.
- `src/daemon/gate.rs`: `check_paths`; `Verdict` gains the merge-order lists.
- `src/daemon/door.rs`: `GateRows` gains the followed list of the repository; `gate_outside` reads 3.2 (each pull request number once); `judge` applies 3.3 and writes the payload field. Added in the build: the `merge` verb's refused answers (of `request` and of `approve`) carry `allow` false, as `gate`'s answer does; the declared test reads it.
- `src/daemon/prwatch.rs`: none, unless `followed` or its `Followed` fields must be made visible to `door.rs` (both are `pub(super)` already).
- `skills/pull-request/SKILL.md`, `docs/acceptance-check.md`, `docs/design/spec.md` as in 3.6.
- No existing test, no file under `.github/`, no script, no change to the merge gate hook or the acceptance checker, no schema change.

## 4. The declared test: `tests/merge_order.rs`

The world of `tests/gate_review_head.rs` (helpers copied into the file, none under `tests/common`): the door in this process on real git repositories (an origin, the project's clone with a GitHub-shaped `origin` rewritten by `insteadOf`, `refs/pull/<n>/head` published in the origin), a `gh` stand-in for `pr view` and the check-run and workflow-run reads, and each mode in a child of the test binary with `MAKIBA_MERGE_APPROVAL_MODE` set. Each pull request has its run (`review`, with a `pr_opened` at its head) so that `prwatch::followed` holds it. PR 314 is the case of Issue 312: its test commit adds `tests/knowledge_isolation.rs` and `tests/common/makibad.rs` (declared), then a fix; CI failed at the test commit and passed at head on one base; its worker's last diff review passed at its head with no blocker. PR 315 and PR 316 each have a test commit declaring a test of their own (`tests/pr315.rs`, `tests/pr316.rs`) and a published head.

1. Under `acceptance`, the `merge` request of PR 314 at its head while 315 and 316 are followed: `status` `refused`, `verdict` `merge_order`, the reason names `#315` and `#316`, `tests/common/makibad.rs` and both test commits' first 7 characters; no `merge_attempted` is written; the newest `acceptance_gate` row has `merge_order.waits_for` with exactly the pull requests 315 and 316 and their test commits.
2. Then `pr_merged` is recorded for 315: the `gate` answer for 314 is `merge_order` naming `#316` and not `#315`. Then `pr_merged` for 316: the answer is `passed` with `allow` true and no `merge_order` in the row.
3. Under `acceptance`, with 315 followed but its branch with no `Acceptance-Test` commit (and no 316): 314 is allowed (`passed`, `allow` true).
4. Under `acceptance`, a pull request whose own changes touch none of `tools/`, `.github/`, `tests/common/`, `scripts/acceptance-check.sh` (a test commit of `tests/test_calc.py` and a fix of `calc.py`), while 315 and 316 with test commits are followed: allowed (`passed`, `allow` true).
5. Under `human`, 314 while 315 is followed: `allow` true, `warn` true, the reason holds `WARNING` and `#315`.
6. Under `acceptance`, 314 while 315 is followed and its `refs/pull/315/head` is not published: refused `merge_order`, the reason names `#315` as not read.
7. Under `acceptance`, 314 while 315's run is still `review` and a `pr_closed` of 315 is recorded on it (read closed unmerged): allowed. Then a `pr_reopened` of 315 is recorded: refused `merge_order` naming `#315`.
8. Under `acceptance`, the mutual hold: PR 317's test commit declares `tests/pr317.rs` and also adds `tests/common/other.rs`; 314 as above; both followed, with passed CI and reviews. The `gate` answer for 314 is `merge_order` naming `#317` and holding "hold each other"; the answer for 317 is `merge_order` naming `#314` and holding "hold each other".
9. Under `acceptance`, a candidate recorded while the gate ran: the `gh` stand-in, when it answers `pr view` of 314 and a marker file is present, first runs this test binary in a helper mode (selected by an environment variable; the stand-in script itself is written through `tests/common/exe.rs`) that appends a new `pr_opened` of 315's run at a new head to `state.db`; 315 is followed before and after, with no test commit before and one after. The answer is `rejected` with "changed while the gate ran".

## 5. Known limits

- Over-approximation: an open test commit that would pass the new check still holds the merge; the person decides.
- Two check-adding pull requests that both have test commits hold each other; the gate names the mutual hold, and under `acceptance` neither lands until the person decides (in a range the two refusals pause it, belt.md's rule).
- The path list is fixed: a check over `tests/` added without touching those paths (a new test target that scans `tests/` alone, `Cargo.toml`, a `build.rs`) is not seen. A check-adding PR whose check lives in a new top-level test target and no helper passes.
- Only pull requests makibad follows count: one opened outside makiba, or of a run closed more than a day ago, is not seen.
- Tightening a check that lives in a test file outside those folders is not seen either; and a pull request that only removes a check is held as one that adds it.
