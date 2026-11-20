# The dispatcher's runbooks in claude-org-ja: reasons and history

Translated from claude-org-ja's `.dispatcher/references/rationale.md` (Issue 44, M6-3). It is kept as history: it records why claude-org-ja's model dispatcher worked the way it did, which incidents shaped its rules, and what was observed at the time. It describes claude-org-ja, not makiba: its panes, its renga and broker transports, its `.state/` files and its tools are claude-org-ja's, and makiba replaced the model dispatcher with code (`makibad`). Read it to find why a rule that makiba carried exists, not as a procedure. Paths and Issue numbers below are claude-org-ja's unless they say otherwise.

The original's introduction: the reasons for the design, the history of incidents and the observations of the time, moved out of the dispatcher's runbooks (spawn-flow, worker-monitoring, pane-close). It is not a procedure and need not be read while working. Read it when you want to change a rule, to check why that rule exists. Each section starts with where it was moved from.

## Contents

- Reasons and history moved from spawn-flow.md
- Reasons and history moved from worker-monitoring.md
- Reasons and history moved from pane-close.md

## Reasons and history moved from spawn-flow.md

### The blind Enter accident at the startup prompt

From: spawn-flow 3-3b / the delegate-plan helper, "handling the output"

On 2026-09-25 the initial cursor of the folder-trust dialog had changed to "No, exit", but the runbook still said to press Enter once; that one Enter ended the worker (a Claude Code update had changed the default from Yes). In response, runtime 0.1.43 added the screen judgment of `claude-org-runtime dispatcher spawn-prompt-step` (suisya-systems/claude-org-runtime#185).

### 3-1d: why all three capabilities are required before a spawn

From: spawn-flow 3-1d

> **Why all three are required before a spawn**: the capability tokens are judged **independently**, and none can be derived from another (the contract's independence rule T-section cap, and the bullet of 3-2 on "why the spawn is made in the same tab"). Spawning on `spawn_tab` alone can reach a state where **the background pane was made but the ritual right after it cannot be performed**: without `caller_scope`, the probe of 3-3, the approving Enter of 3-3b and the kickoff of 3-5a fail closed with `server_too_old`; without `cross_tab_peers`, neither the all-tab listing of 3-4b nor the send by numeric id of 3-5 works. Either way **a child pane is left half-started** (`server_too_old` is not transient, so a retry does not recover it; contract T-section 6 clause (b)). Checking all three together before the spawn makes this path a choice between "everything can be done" and "nothing is started".

### 3-1d: what was observed for condition 6 at the time (2026-08-10)

From: spawn-flow 3-1d

> **Condition 6 now (an observation recorded on 2026-08-10, not a permanent fact; query it again each time the trigger is judged)**: **the common condition does not hold**. The contract `docs/contracts/backend-interface-contract.md` T-section 4.2-place-rec records **only (R2) as met** and **(R1), (R3) and (R4) as not met**, so the "all four of (R1)-(R4) already hold" that both paths require is not satisfied. **The machine query of (a) also returned `not_recorded` (exit 1) on 2026-08-10**: the state DB had no `notify_sent` record whose `kind` is `placement_production_activation`. **So what this section defines is how to act once the records are complete, not a permission to act before they are.** The section-level status was ratified on 2026-08-09 (the `Status:` line of that amendment in the same contract), so the wall condition 6 still keeps is **only on the R conditions' side**.

### 3-1d: the history of running background placement

From: spawn-flow 3-1d

The person's ratification of 2026-08-09 approved in advance **the background-tab run itself** in place of a staged canary, and under that approval background placement has already been done several times (the human-taken, pre-authorised deviation that the contract's T-section 4.2-place-rec records). At the time the earmark ledger (the dogfood ledger, since retired) held several rows of background-tab placement, each raised one at a time and retired in turn.

### 3-1d: what the contract does (the conditions and R1-R4)

From: spawn-flow 3-1d

> **The contract's side**: T-section 4.2-place puts the readiness conditions R1-R4 on placement relief, and makes one exception: "a placement dogfood **MAY** place orchestrator-spawned panes in a background tab **when all of** the following hold: this section is ratified; all four of (R1)-(R4) already hold; the run is **bounded** and declared as a dogfood in advance; and a **human is supervising it and confirms the result**". Conditions 1 and 5 of the table above answer "declared in advance", "bounded" and "human supervising"; conditions 2, 3 and 4 answer "that path really works and is limited to one pane". **R2 (the liveness judgment of background placement) is met by the tracked-any-tab judgment of `.dispatcher/references/worker-monitoring.md` (3-a-5).** Whether R1, R3 and R4 are met is recorded in the contract's T-section 4.2-place-rec, and **all three are not met**: R1, because the all-tab listing only confirms numeric ids already held and the contract's two-scopes restriction is not lifted; R3, because the close half is not wired for background panes, and the exit-detection half that is wired has never once worked (in the real run a backend restart invalidated the cursor of `poll_events`, and retirement was settled not by a join on the contract key but by the path of T-section 2.1 step (3), "gone from `list_peers`"); R4, because capacity accounting comes from a `list_panes` limited to the caller's tab and no all-tab census is wired. The placement gate name `placement_production_activation` is in `GATES` of `tools/capability_gate.py`, and can be queried by machine as condition 6 (a). **While R4 is not met, condition 4 of the table above (at most one background pane) is its safety valve**: placing two or more background panes without all-tab capacity accounting lets an overstated free slot turn directly into more spawns (contract T-section 4.2-place, (R4)). While it is limited to one pane, an undercount is bounded to at most one pane.

### 3-1d: why the close half of R3 is not wired

From: spawn-flow 3-1d

> **The *close* half of R3 is not wired by this task (a background worker is retired by the human supervisor)**: R3 asks for two things, "An exit MUST be establishable ... **and the close itself MUST satisfy** T-section 4.2-id and the Group-B obligations", and this task meets only the first (exit detection). The second is not wired because the ordinary `CLOSE_PANE` procedure (`.dispatcher/references/pane-close.md`) checks identity right before the close with a **`list_panes` closed to the caller's tab**, and a background pane **cannot pass that check by construction** (it does not appear in `list_panes`). **So a `CLOSE_PANE` for a background worker is escalated by the dispatcher to the secretary without firing `close_pane`, and the human supervisor closes it.** This end is consistent with condition 5 of 3-1d (bounded, under human supervision), and since `close_pane` cannot be undone, **keeping the pane and raising it is the safer side than firing without a match** (contract T-section 4.2-id (O3)/(O4), the same direction as the indeterminate discipline of T-section 2.1). **The Group-B background close path (a `close_pane` with a session-safe identity check) is not wired here either.** The contract's T-section 4.2-place-rec records R3 as "exit-detection half wired, close half not wired", and closing the close half is **separate later work** (the (R3) section of that record names what that work needs: an identity check beforehand that does not depend on the caller tab's listing). The monitoring side, once the human has closed the pane, joins `pane_exited` and settles the retirement as usual ((3-a-5)), **but this is a designed path that has never yet worked in a real run**. In the first real run a renga restart invalidated the cursor of `poll_events`; `pane_exited` is exactly-once with no replay, so the join itself could not fire, and the retirement was settled by the path of contract T-section 2.1 step (3), "gone from `list_peers`" (the secretary's reconcile) (contract `docs/contracts/backend-interface-contract.md` T-section 4.2-place-rec, (R3)). **So the detection path has a fallback, but whether the join on the contract key will work is not yet measured.**

### 3-2: model choice is independent of approval; the dispatcher's model

From: spawn-flow 3-2

- Model choice is independent of the safety of the approval flow. The safety classifier of permission_mode `auto` runs on a dedicated model separate from the worker's session model, and the approval judgment does not depend on whether the worker is opus or sonnet (official: https://www.anthropic.com/engineering/claude-code-auto-mode). So the model is chosen on quality alone.
- Only the dispatcher is fixed to `bypassPermissions` and does not go through the classifier, so it runs on sonnet as a cost optimization, as before (this judgment is not applied to workers automatically).
- The workers' default is sonnet because the cap on Codex review round trips can now be enforced mechanically outside the worker (the PreToolUse hook `.hooks/block-codex-over-cap.sh` and `tools/codex_round_cap.py`; the allowed count is read from the events of the secretary's `.state/state.db`). A Sonnet worker that goes round and round without converging stops at the cap, and `codex_round_denied` is relayed to the secretary, which can then decide whether to move to Opus or to raise it to a human. A worker starts on Opus only when the secretary puts a `model: opus` line in the DELEGATE body (the person's choice, or a task that implements an enforcement mechanism: a guard, a seal, a contract).
- The inbox task JSON must always carry `"model"` because claude-org-runtime's helper defaults to opus when `model` is missing. `.hooks/check-worker-spawn-model.sh` closes, at spawn time, the path by which an omission silently became an opus spawn.

### 3-2: an example of misusing args[]

From: spawn-flow 3-2

On 2026-05-09, in sandbox-probe-iter-b-round-3, `--skip-settings` was passed in `args[]` and pane id=11 retired at once (the record was then `knowledge/raw/2026-05-09-delegation-skip-settings-wrong-cli-arg.md`, untracked).

### 3-3b / Step 5: the false dispatch reports of 2026-08-18

From: spawn-flow 3-3b / 3-4 / Step 5

> **Where this section came from (2026-08-18)**: twice in a row that day, the dispatcher reported to the secretary "approved, peer registration confirmed, instruction sent" **without having performed** the approval of 3-3b, the `list_peers` registration check of 3-4 or the instruction send of 3-5 (`cert-questions-ingest-20260818` / `interlock-founding-docs-20260818`. In both, the pane had in fact stopped at the approval prompt with an empty input line, and the secretary saw it with `inspect_pane` and recovered it by hand).
>
> **The procedure was written correctly at the time.** What was broken was the verification. 3-3b, 3-4 and 3-5 **all leave no trace** (`send_keys` is raw bytes to the PTY, `list_peers` is a read, `send_message` is an MCP call, and none writes anything to `.state/`). The only row left by Step 3 and Step 4 is one `worker_spawned`, and as the Worker lifecycle table of `docs/journal-events.md` defines it ("After MCP `spawn_pane`."), it is written **before the ritual starts**. Indeed, in both cases state.db held only `delegate_sent` then `worker_spawned`, **byte for byte the same trace** as a correct dispatch. Skipping the ritual was cheaper than performing it, and it was not found out. This Step removes both at once.

### 3-5a: why send_keys, and the scope of arming

From: spawn-flow 3-5a

**Why `send_keys` (confirmed in a real run, Issue #554)**: ultracode's opt-in is armed only when the harness detects the token `ultracode` in a **user turn input** of the worker session (when armed, the worker gets a system-reminder saying "opting this turn into multi-agent orchestration"). Real broker runs confirmed that these **do not arm** it:

- the keyword in the brief file (context loaded as `CLAUDE.md` / `CLAUDE.local.md`);
- the keyword in an instruction body that arrives by a `send_message` body, an in-band push or `check_messages` (a tool result).

`send_keys` **writes the body as keystrokes** into the pane's PTY, so it becomes the worker's user turn. `send_message` / `check_messages` are peer/channel messages (injected messages), not user turns. This arming **does not depend on how the body is produced**: whether by the helper's (`delegate-plan`) `message_file` path or by an `instruction` given directly in the task JSON, the arming happens on the send_keys user turn, not in the body, so both are covered.

**The scope of arming (turn-scoped, confirmed in a real run)**: arming is detected and enabled **per user turn**. A Claude Code worker works autonomously, within "the answer to the kickoff user turn", from implementation through to the convergence of its self-review before Codex, and the many internal LLM calls, parallel reviews and commit checks in between belong to **the answer phase under the same user turn**, so the arming is not lost (this task's own run confirmed that a Workflow could be started even after many tool calls within the armed turn: the whole answer is armed). The arming **is not carried over to another user turn** such as an ack or a further instruction (it must be detected again).

> **Verification note**: "arming by a lone `ultracode` token in send_keys" and "arming lasting over the whole answer of the armed turn (many tool calls)" were confirmed by this task in real broker runs. The wait in step 1, "deliver the reference by send_message and the worker starts on the send_keys turn", is a recommended procedure by design derived from those facts (it avoids the race by construction). The simplest insurance is **to send no actionable send_message kickoff for an ultracode task, and make send_keys the only start trigger** (this works because the worker brief, by default `CLAUDE.md`, or `CLAUDE.local.md` for self-edit, holds the whole brief).

### 3-5a: where the absolute path convention came from

From: spawn-flow 3-5a step 6

On 2026-09-04, in continuo-110-lease-renewal, relative-path Bash inside a workflow's `agent()` fell to manual approval six times in one task.

### Step 4: the numbers of the time behind the mandatory Status line

From: spawn-flow Step 4

> **The `Status:` line cannot be left out** (Refs #835). The runtime reads this line mechanically as the overflow reservation ledger (`_seed_status`, `claude_org_runtime/dispatcher/runner.py:1265-1286`). Without the line it returns `None`, and `count_unbound_reservations` **silently** falls back to the mtime clock (`WORKER_BIND_WINDOW_SECONDS = 45`, `runner.py:305`), so a worker already active at this Step 4 holds a slot as a pending reservation for up to 45 seconds, and an overflow spawn right after it can be wrongly refused with `split_capacity_exceeded` (`runner.py:1253-1260` / `runner.py:2796-2832`). The format contract (a line with a bullet in front, `- Status:`, is **not parsed**, and so on) is section 7 of `docs/contracts/state-schema-contract.md`; regressions are caught by `tests/test_worker_seed_status_contract.py`.

(In runtime 0.1.49 the line numbers changed, and `WORKER_BIND_WINDOW_SECONDS` is 120000//1000+30+15 = 165 seconds.)

### Step 4: the history of the duplicated item number

From: spawn-flow Step 4 item 8

> The "Worker Directory Registry" below is the **definition (an appendix for reference)** of the section that Step 4's DB write produces, not a continuation of the procedure. The dispatch report was once placed **after** this appendix under the number `5.` (a duplicate of item 5 of this Step 4), so a reader following the procedure from the top could not reach the reporting step. This item, which carries the numbering on, closes that gap.

### Step 5: a v1 stopgap

From: spawn-flow Step 5

> **This is a v1 stopgap, not a deterministic ritual.** Taking spawn, startup prompt approval, registration poll and instruction send out of the LLM's re-enactment of prose and putting them into code is the job of Interlock (v2); the addendum of 2026-08-17 to Issue #740 (https://github.com/suisya-systems/claude-org-ja/issues/740) decided not to back-port it to v1 (claude-org-ja with the runtime 0.1 series) (the addendum's "migration method" says "no dual-write; switch at a run boundary; a run started on v1 finishes on v1", and puts "the resident Dispatcher AI loop and the large amount of prompt prose for handover / resume" on the Discard side). What this Step changes is **the incentive and the detectability** ("skipping is cheap and not found out"), not who performs the ritual.

## Reasons and history moved from worker-monitoring.md

### Why the observation principles were set down

From: the opening of worker-monitoring, "Observation principles"

In the run of 2026-08-08 three kinds of false alarm appeared at once (a vanished pane reported as an anomaly, two `STALL_SUSPECTED`, and duplicate questions about completion), but they had one root: "**the state in which my own means of observation did not reach was read as an anomaly in the target**" (Issue #869). Rather than fixing the three places one by one, the principle was put in one place and each judgment refers to it.

### A precedent for (P1)

From: worker-monitoring, "Observation principles" (P1)

A sibling runbook had the same discipline first: the done-report gate of CLOSE_PANE exists because "the absence of a done report in the dispatcher's message queue (what `check_messages` returns) does not mean it 'does not exist in the system'" (the "reason" of item 1 of `.dispatcher/references/pane-close.md`). (P1) generalizes that to every judgment of the monitoring loop.

### The record behind the prohibition of (P5)

From: worker-monitoring, "Observation principles" (P5)

The vanished pane observed on 2026-08-08 was **a live session a human had open in another tab**. Had the disappearance from `list_panes` been read as "gone = dead" and a rebuild of the pane started, the human's working session would have been broken. The three false alarms did no harm because the dispatcher **reported only what it observed and did not, on its own judgment, start a recovery, a re-spawn or a `close_pane`** (Issue #869).

### Duplicate questions about completion

From: worker-monitoring Step 2 (do not ask again a question with a receipt record)

A false alarm that kept re-sending to the same task (2026-08-08, Issue #869) is the ground for the rule to look at the receipt record before asking.

### Why false alarms of the old-style spinner age are suppressed by a diff with the previous cycle

From: worker-monitoring Step 4 (d)

**Suppressing false positives of the old-style spinner age (Issue #698)**: Claude Code draws the summary of a finished turn's duration (`Cooked for 31m 40s` and the like) in **the same shape** as a live old-style spinner. Left in an idle worker's scrollback, it matched every cycle as "a stuck spinner running 5 minutes or more" and raised false positive alerts again and again. In a single frame a frozen summary and a really stuck spinner cannot be told apart by content or position, so the helper takes a diff on the invariant "the `for Xm Ys` counter of a live spinner advances every cycle, while a frozen summary stays byte-identical". When `--spinner-state-file` is passed per worker, an old-style spinner line identical to the previous cycle's is taken as a frozen summary and detection is suppressed. The first observation always fires (there is nothing to diff against), and a live pane that froze completely is caught by the hash-based STALL path (`tools/inspect_pane_state.py`), so no signal is lost. A missing or broken state file is taken as "no previous cycle" and fires again, the safe-side fallback (a really stuck spinner is not silenced by a faulty state file).

### Self-reports used beside inspection; the secretary's inspection cadence

From: worker-monitoring Step 4 (g)(h)

**(g) Using the worker's self-report (Step 2) beside inspection (Step 4)**
Even when both channels report the same anomaly, the de-dup ((e) step 2) merges them in a 30-second window, so the secretary does not get duplicate notices. A self-report that arrives first suppresses the inspection; inspection makes up for a worker that forgot to report. Both run independently.

**(h) Design note: the secretary's inspection cadence is a separate Issue (Issue #492 gap 4)**
Issue #492 gap 4, "should the secretary itself have an active inspection cadence", was **cut out of this PR's scope into a separate Issue**. Reason: strengthening the dispatcher's Step 4 to a scan of every visible line plus the spinner age (gaps 1-3 of this PR) is the smaller change, and the observed case (529 plus a 9-minute spin) reaches an ERROR notice at 5 minutes by strengthening the dispatcher's detection alone. Adding a second inspection loop in the secretary doubles the monitoring layer; it is better to observe the effect of the stronger dispatcher first and then judge whether it is needed. If a secretary cadence or a `secretary-monitor` skill turns out to be needed, it goes to a separate Issue.

### Why stall detection was added

From: worker-monitoring Step 5 (a)

**(a) Motivation**
`inspect_pane` of Step 4 alone cannot tell "the worker sent the secretary a question and waits for the human's answer" from "stuck (stopped abnormally)". While a question is pending, the worker pane's Claude session continues with an idle screen and matches no APPROVAL_BLOCKED regex. A wrongly fired STALL_SUSPECTED keeps throwing an escalation to the secretary every cycle for a worker that is waiting for a decision. A real incident found it (Issue #287, session #12 / `worker-issue-283-delegate-payload`).

### Why the screen-change judgment became a content hash

From: worker-monitoring Step 5 (b)

**The screen-change judgment is made by a content hash (Issue #680)**: the old implementation judged idle or active by a single-point comparison of `(target_line_text, the last non-empty line)` plus the cursor position, but Claude Code's TUI keeps the bottom footer and the cursor static even while a tool runs, so even with the scrollback moving (Read/Edit/Bash output, the thinking spinner), the single-point comparison saw no change, `idle_streak` was added mechanically and STALL fired falsely (twice in a row in one session in `worker-runtime-129-observed-session-binding` and others; `knowledge/raw/2026-07-04-delegation-stall-detection-static-footer-false-positive.md`). **Every visible line is normalized and hashed**, and only a real change in the scrollback counts as active. The hash, the spinner normalization and the active-spinner judgment are codified in the helper `tools/inspect_pane_state.py`; the prose only puts the helper's output into the state (a hash made by hand in prose is weakly deterministic and is forbidden; a Major review finding).

### The design of extending the lookback for PR-pending-merge

From: worker-monitoring Step 5 (b-2), design note

**Design note**:

- `pr_opened` / `pr_merged` are defined in `docs/journal-events.md` with Writer = secretary and the `task` payload field as the bare task_id, so the judgment can be made from the existing ledger without a new event (option 1 of the proposal in Issue #304).
- The ground for `STALL_PR_MERGE_LOOKBACK_MIN = 60`: the time scale of a human's answer to a PR review or merge approval. Too short (15 minutes) and `worker_completed` leaves the window and fires falsely; too long and a stuck worker is missed. A middle value from Issue #304 and the observation of session #12.
- Why PR-pending-merge is handled by extending the lookback rather than as another event: the auxiliary signal axis of (c)(1) (five kinds including `worker_completed`) is reused as it is, and on the acked path it agrees with the existing de-dup and notice suppression. A new category would be a larger change.
- Once a `pr_merged` event is recorded after the merge, the sub-state ends in the next cycle and the lookback returns to 15 minutes. If after the merge the `ts` of `worker_completed` has left the 15-minute window and the worker pane is still open, it is treated as an ordinary stall (in operation, CLOSE_PANE runs together with the merge, so this path is practically never taken).
- The elapsed time since `T_pr_opened` itself is not used in the judgment (no need to judge within or beyond 60 minutes of the PR opening). Instead, extending the lookback of (c)(1) to 60 minutes keeps the same form of judgment: acked if the latest worker-to-secretary event such as `worker_completed` is within 60 minutes, timeout if not.

### Why poll_events is not used for the stall judgment, and a future plan

From: worker-monitoring Step 5 (c)(2)

2. **poll_events (forward-compatible; for now only auxiliary)**: for now `poll_events` carries only pane lifecycle events and not `send_message` (see the type table of `.claude/skills/org-delegate/references/renga-error-codes.md`). Also, `since` is not a time but an **opaque cursor** received from the previous cycle, and omitted on the first call it means "from now on" (no past history), so it cannot be used for a pinpoint search of "the last 15 minutes" in this judgment. The events scan is the **authoritative source**. If a future transport backend carries `send_message` events in `poll_events`, a route is planned that turns the worker-to-secretary sends received through the cursor of `.state/dispatcher-event-cursor.txt`, already held in Step 1, into a `(worker, kind=stall_acked)` ledger (this PR only describes it as a placeholder).

### The constants, reasons and expected scenarios of the STALL judgment

From: worker-monitoring Step 5 (f), design note

- **The ground for `STALL_SECRETARY_LOOKBACK_MIN = 15`**: the time from the secretary asking a human for a decision to the answer coming back is typically 5-10 minutes, during which the worker waits idle. Taking "recent exchange" over a 15-minute window removes in practice the false alarms while a human's answer is awaited. Shorter, and a worker awaiting a decision falls into the timeout path and fires falsely; longer, and traces of reactivation of panes after completion keep being picked up and a stuck worker is missed. The middle value of 15 minutes is the sweet spot for now.
- **The ground for `STALL_PR_MERGE_LOOKBACK_MIN = 60` (Issue #304)**: merge approval after a PR opens is typically a manual operation of 15-60 minutes by the user. The worker has reported done and waits idle, correctly (it is not stuck), but with a 15-minute lookback `worker_completed` leaves the window and STALL fires falsely (measured in session #12). The sub-state "`pr_opened` done, `pr_merged` not yet" is judged from the event ledger alone, and only for that period the lookback is extended to 60 minutes. After the merge, `pr_merged` is recorded and the ordinary sub-state returns at once.
- **What happens past 60 minutes (Issue #304, long tail)**: when a PR stays open for 60 minutes or more (over a weekend, a long review) and `worker_completed` leaves the window, STALL_SUSPECTED fires again by the timeout path. By design this means "past 60 minutes, a sticky PR-pending-merge is notified again for a human to judge", as Issue #304 specified. Being notified again every 3-minute cycle because of the 30-second de-dup is noise, but it is judged in operation together with the close condition of the `org-pull-request` skill (24-48 h of idle review goes to a human's judgment; see `.claude/skills/org-pull-request/SKILL.md`). To silence long PRs completely, a future Issue could change it to "a task with `pr_opened` never raises STALL", but this PR stays with the lookback extension, "grace up to 60 minutes" (as the Issue specified).
- **Why the events scan is primary**: for now `poll_events` carries only pane lifecycle events (`pane_started` / `pane_exited` / `events_dropped` / `heartbeat`) and not `send_message` (see the type table of `.claude/skills/org-delegate/references/renga-error-codes.md`). On the other hand, `worker_escalation` / `worker_reported` at the secretary's receipt are already persisted as an authoritative ledger. Reusing them is right.
- **Why a soft note is left**: so that "why did STALL_SUSPECTED not fire" can be reproduced later in a retro or a debug. With a silent skip, the journal alone could not tell when a missed detection is suspected. It reuses the same `anomaly_observed` event as Step 4, so no new entry in the event catalog (`docs/journal-events.md`) is needed (kind is `stall_acked`, and the sub-state is told apart by the `note` field, `awaiting_secretary_lookback_15m` / `awaiting_pr_merge_lookback_60m`).
- **Why the decision-register gate was added (Issue #603)**: the journal's `worker_escalation` row alone cannot tell "still waiting for a decision" from "the user's answer has been forwarded to the worker and it is idle now" (in both, the same row stays within the 15-minute window). The register's `resolution_kind == "to_worker"` (status `resolved`) is the authoritative signal that forwarding is complete, so it is queried to take stale traces out of acked. So a worker that went idle with a 529 or got stuck after the forward is not wrongly suppressed as acked, and returns to the STALL/ERROR judgment.
- **Why the deterministic judgment was moved to a tools CLI (Issue #603)**: the stale judgment (choosing the latest decision = the largest `received_at`, matching status and resolution_kind) was not written as raw jq in the prose but moved to the CLI `tools/pending_decisions.py latest-resolution`. This keeps the same layering as the existing helpers that know the register's schema directly (`list_pending_older_than` / `list_escalated_user_replied_older_than` and others), and the prose need not follow changes in the register's internal form (deterministic judgment in code, the prose only the contract surface).
- **Expected scenarios (Issue #304 acceptance)**:
  - regression: the worker reports `worker_completed`, the secretary opens the PR (`pr_opened`), CI is green, the user approves the merge 30 minutes later. At 30 minutes (b-2) is in the PR-pending-merge sub-state (60m lookback), `worker_completed` is on the acked path at 30 < 60 minutes, and STALL_SUSPECTED **does not fire**. OK.
  - inverse: the worker stopped completely (no PR opened, no `worker_completed`). (b-2) is in the ordinary sub-state (15m lookback), the events scan finds no trace, the idle streak reaches 3 cycles or more and takes the timeout path, and STALL_SUSPECTED **fires as before**. OK.
- **Expected scenarios (Issue #603 acceptance)**:
  - regression (fixing the misclassification): the worker asks a question, the secretary escalates to the user, the user answers, the secretary forwards it to the worker with `resolve --kind to_worker`, and right after that the worker gets stuck on a 529 and stays idle. Old behaviour: the `worker_escalation` row is within the 15-minute window, so acked, STALL suppressed, the stuck worker missed. New behaviour: in (c)(1-bis) the latest decision in the register is `resolved` / `to_worker`, so `escalation_trace_is_stale == true`, `worker_escalation` is excluded, no other trace, timeout path, STALL_SUSPECTED **fires**. OK.
  - not applicable (no false fire on a new escalation): after the forward the worker asks again, the secretary `append`s a new pending entry to the register, the latest decision is `pending`, so `escalation_trace_is_stale == false`, `worker_escalation` is acked again and STALL_SUSPECTED **does not fire** (a worker waiting for a new decision is not judged stuck). OK.
- (An example of how an incomplete `ci_completed` is handled) Example: observed twice in a row in runtime PR #126 / #127 (the merge and the automatic pane close did not progress). The permanent fix (retries and better classification of the fetch on the side of `tools/pr_watch.py`) is tracked in a separate Issue.

### How the proxy path was demoted to a fallback

From: worker-monitoring Step 5.1 (a-3)

**The proxy path ((a) to (f)) is demoted to the (a-3) Fallback**: at Issue #297 it was the only cover of (a)(2), but once #301 made (a-2) deterministic, the proxy path has meaning only as a fallback for these cases:

- backward compatibility for entries written by the old secretary implementation (`user_replied_at` still None);
- insurance against the operating mistake of the secretary forgetting to call `mark-user-replied`;
- a degraded mode when the register itself cannot be read or is corrupted.

The final removal of the proxy path is handled in a separate Issue once (a-2) is confirmed to run stably.

### Why relay-gap detection was added, and the historical scope of the proxy path

From: worker-monitoring Step 5.1 (a)

**(a) Motivation**
Step 5 looked at the worker's side (a worker-to-secretary trace **exists**, so the stall is suppressed) and made it an auxiliary signal, but the other direction (the secretary relaying to the user, or to the worker) has a blind spot. The concrete incident patterns:

1. The worker sends "I need a decision" to the secretary; the secretary receives it (`worker_escalation` is appended to the journal); the secretary **forgets to raise it to the human**; the worker is idle, Step 5's auxiliary signal counts as a "hit" and suppresses, but the user knows nothing.
2. The user answered the secretary; the secretary **forgets to forward it to the worker**; the worker is idle, the same hole (the ledger started by the worker is still old, so Step 5 still suppresses).

Neither can be detected by Step 5 (watching the worker's side) or Step 4 (watching the worker pane's screen). An independent channel that observes the secretary's outbound (secretary to user, secretary to worker) is needed. A sibling of Issue #287 (PR #295); watching both sides completes it. Issue #292.

**The historical scope of the proxy path (old PR #298)**: (b) to (f) below describe the proxy-only implementation of PR #298's time, which covered only (1), "the secretary forgets to raise it to the human". (2), "forgets to forward the user's answer to the worker", could not be detected then because the journal had no ledger of the secretary's outbound to the worker. With Issue #297 (PR #302), (1) became deterministic through the register by the (a-0) primary lookup, and with Issue #301, (2) too became deterministic through the `user_replied_at` marker by the (a-2) primary lookup. The proxy path (b) to (f) is kept as the (a-3) Fallback for legacy entries, a secretary that forgot to call the CLI, and the degraded mode when the register cannot be reached.

### A future plan of sending directly to the user's pane

From: worker-monitoring Step 5.1 (e)(3)

3. **Identifying the user's pane by `client_kind` from `list_peers` and sending to it directly** (future): `list_peers` includes `client_kind` (see `docs/operations/renga-pane-conventions.md`), but a user's (human's) terminal pane is normally not registered as an MCP peer, so a direct send is not possible with the current API surface. A candidate for an Issue (not implemented in this PR).

### How the register landed, and how relay gaps relate to Step 5 stalls

From: worker-monitoring Step 5.1 (g)(h), design note

**(g) Design note: the register landed with (a-0) (Issue #297)**
The register "equivalent to `.state/pending_decisions.json`" that PR #298 left as a TODO was implemented by Issue #297. See (a-0), "Primary check: pending-decisions register lookup", in this section. The proxy path of (a) to (f) in this section is kept as a fallback, but the primary has switched to the register lookup of (a-0). The final removal of the proxy path is handled in a separate Issue (once the register lookup is confirmed to run stably).

**(h) Design note: relay gaps and Step 5 stalls**
Step 5's stall detection suppresses STALL_SUSPECTED as "acked" when a worker-to-secretary trace exists. A relay gap lives **exactly in that suppressed acked set**. That is:

- a worker that entered the `acked` path in Step 5 is an input candidate for relay-gap detection;
- a worker that entered the `timeout` path in Step 5 (STALL_SUSPECTED already notified) has no worker-side trace, so (b)(1) of the relay gap does not hold and it is no candidate (it is treated as an ordinary stall, simply stuck);
- the two are **disjoint notices**, so there is no need to merge them with the windowed de-dup (their kinds differ);
- Step 5's `worker-idle-state.json` is also read in the relay-gap judgment (an idle streak of 3 cycles or more is the condition of (b)(4)). The only new state file is the secretary pane snapshot of (d), independent of Step 5's state.

### Why pane_output_without_peer_msg detection was added

From: worker-monitoring Step 5.2 (a)

**(a) Motivation**
An incident pattern that really happened in the renga-ime-paste-routing task of 2026-05-09: the worker laid out a long answer to the secretary's question on its pane but forgot to call `send_message(to_id="secretary", ...)`, so the secretary kept waiting until it noticed with `inspect_pane`. Since the peer message between worker and secretary is the source of truth, this becomes a **silent dead-lock** (both panes live, and none of the existing detectors of Step 4, Step 5 or Step 5.1 applies):

- **Step 4 (screen anomalies: APPROVAL_BLOCKED / ERROR)**: it does not match unless the defined approval prompt or error banner shows. A freely written long answer does not fit the anchored regex (b).
- **Step 5 (STALL_SUSPECTED)**: it needs an idle_streak of 3 cycles or more (about 9 minutes), but once the worker starts writing output on the pane, `idle_streak_cycles = 0` resets and it leaves the stall candidates. The output itself suppresses STALL.
- **Step 5.1 (SECRETARY_RELAY_GAP_SUSPECTED)**: a detector in the opposite direction that observes the absence of the secretary's outbound (secretary to user, secretary to worker). A worker outbound that never fired is not covered.

The worker side's outbound (a peer message that never fired) has to be observed by an independent channel. It can be implemented with only the `anomaly_observed` / `notify_sent` ledger schema established by Issue #287 (Step 5) and Issue #292 (Step 5.1) and an extension of Step 5 (b)'s `worker-idle-state.json`; no new event and no new state file are needed.

### The safety of putting task_id into SQL text

From: worker-monitoring Step 5.2 (c)

**On putting task_id into SQL text** (Codex round 1, Minor): claude-org-ja's task_id satisfies the slug convention (`[a-z0-9-]+`; by the time the task_id value is set in CLAUDE.local.md it is in practice kept by the human's or the runtime helper's constraints, and contains no quotes or SQL metacharacters in fact), so the string interpolation above is safe. Making it a helper script (passing it by sqlite3 `.parameter`, or extending Python `tools/state_db/queries.py`) is future work; for now the dispatcher Claude runs the SQLite one-liner directly. If a path that brings in values other than slugs is introduced in future (for example, writing an old worker_id directly), it must switch to going through a helper script.

### Why the relay scan was folded into one --cycle, the order of the audit, and when to report

From: worker-monitoring Step 5.25

**Why two commands were folded into one (Issue #955 / #971)**: the old procedure asked for two commands at the start of a cycle, `--audit` and `--list`, but a measured monitoring cycle ran about one shell command, and the two referred to each other: **`--audit` is read-only and starts no scan, and `--list` was run only "when the audit says stale"**. Even on 2026-09-03, armed with the canonical `/loop 3m` text (fixed by `.hooks/check-loop-directive.sh`), the scan stopped for 44.5 minutes and two undelivered items piled up. It was not a problem of following the prose but of structure, so it was solved **by reducing the number of commands to run to one** (running the cycle = the scan ran). `--audit` and `--list` alone remain for backward compatibility, but the runbook's proper path is `--cycle`.

   **The order, audit first, is the essence**: the scan writes the heartbeat, so in the reverse order the audit would always return `fresh` and lose its power to detect. `--cycle` fixes this order in the tool (it does not depend on following the order in prose).

   **Why "the heartbeat is old" alone is not reported**: the monitoring loop stops *on purpose* at Step 7 (no worker panes and the relay empty). Across a night, the first audit of the next session always sees a heartbeat hours old, so raising that as an anomaly would produce a false `RELAY_SCAN_STALE` at every start of the organization, and **readers would learn to ignore the real alert**. Narrowing the report condition to "the trace is missing **and** a backlog really exists" points at "there is something to deliver piling up and nothing is draining it", the failure itself. If the relay breaks while the backlog is empty, there is no real harm, and it fires in the very next cycle after one terminal event comes in. In the real failure of 2026-08-19 more than 132 items had piled up, so this narrowing does not lower the power to detect. **A stop with a non-empty backlog** is suppressed by the `--mark-halted` entry of Step 7 (`halted: true`), and that suppression is **cleared by the audit that used it**, so it holds once (it clears even if the scan does not continue).

### The 20 days in which the relay's silence could not be detected

From: the summary of the entry point of `.dispatcher/CLAUDE.md`, "Watching worker panes" (Refs #941 #955 #971)

For the 20 days from 2026-07-30 to 2026-08-19, the runbook spelled the relay's command `python` (this environment had only `python3`). The relay missed with `command not found` every cycle, and 134 terminal events piled up without anyone noticing. A cycle with nothing to deliver and a cycle in which the relay did not run both leave nothing in the `event_deliveries` ledger. So a heartbeat that the scan writes unconditionally was added, and the audit that reads it was made a mandatory command at the start of a cycle. The section above covers how the two commands `--audit` / `--list` then referred to each other and stopped the scan, and why they were folded into one `--cycle`.

### Design notes on the monitoring loop as a whole, and the grounds of the cadence

From: the end of worker-monitoring, "Design notes", "Grounds of the cadence setting (checked 2026-05-02)"

#### Design notes

- **Why `spawn_gate.py audit` runs at the start of a cycle**: on 2026-08-18 two dispatches were reported "done" with none of the approving Enter, the registration check and the instruction send performed, and nobody noticed until the secretary ran `inspect_pane`. Those workers had sent no peer message at all, so Step 5.2 could not catch them by construction.
- **Why the observation principles were put before the judgments rather than as separate patches (Issue #869)**: the three kinds of false alarm of 2026-08-08 fired from different judgments (pane disappearance, STALL, completion check), but had one root: "a state in which one's own observation did not reach was read as an anomaly of the target". Fixing three places one by one lets the same error recur in **the next judgment added**. Putting the observation principles before the monitoring loop, with each judgment referring to them, makes whoever adds a new judgment decide first, in the table of (P3), "what this observation surface is evidence of". Adding a row to the table (a new observation surface) and adding a judgment can be done independently.
- **Why better precision and wider authority are written separately (Issue #869)**: a change that makes a judgment stricter "reduces false alarms" and at the same time gives the dispatcher the judgment "this was found not to be an anomaly", which, left alone, slides easily into "it is not an anomaly, so I may fix it myself". (P5) fixes in the negative that **a change of the reporting condition includes no change of action**. In fact the pane disappearance of 2026-08-08 was a live session a human had open in another tab, and the dispatcher reporting only what it observed, without moving to recover it, prevented an accident. Higher precision must not loosen this restraint.
- **Why `poll_events` runs with `timeout_ms=5000`**: within the 3-minute cycle interval it long-polls for 5 seconds. After 5 seconds it returns and the rest of the time is covered by check_messages, list_panes and inspect_pane. This makes the mean delay of detecting a pane's end about 2.5 seconds, independent of the cadence.
- **Cursor management**: the previous `next_since` is saved in `.state/dispatcher-event-cursor.txt` **together with the `session_key`** (one line of JSON; the schema is section 1.7 of `docs/contracts/state-schema-contract.md`). The first time (no cursor), `since` is omitted, meaning "from now on". On crash recovery a lost cursor may miss the last 5 seconds of events, which the comparison with list_panes recovers.
- **Why the cursor is bound to the session**: the cursor's **value** is a counter inside the daemon session and is not restored across a restart, but the cursor **file** survives it. Left alone, this asymmetry makes the loop poll after a restart with "the previous session's large sequence number", and every `pane_exited` misses; and since **the degradation is toward the safe side** (the gate keeps turning, only no notices come out), **the symptom looks normal**. In the real run of 2026-08-09 this showed at the first worker close, and exit detection died silently. Binding alone (Step 1) leaves the same silence (nobody notices the cursor was thrown away), so it is paired with **fail loud: always report the discard to the secretary in one line**. The span over which the cursor was discarded cannot in principle be recovered, so the report states the fallback path too (contract T-section 2.1 step (3): disappearance from `list_peers` plus the secretary's reconcile).
- **Double cover by events and list_panes**: events are best-effort (EventsDropped can happen), so the comparison with `list_panes` is used beside them as insurance.
- **Why inspection is an independent observation channel**: when a worker stops waiting for approval, relying only on the worker's self-report (a peer message) means the worker may stop before it sends the notice. Inspection observes actively from the dispatcher's side, so it makes up for a worker's forgotten or late notice. Self-report and inspection are a redundancy design: "observing the same event on two channels raises confidence".
- **The intent of the anchored regex**: even if "Allow this tool use" happens to appear in the body, it rarely comes with the prompt's own line format (`(y/n)` at the end). Limiting it to the last non-empty lines reduces false detection further.
- **Errors branch on the code, not the message**: they come back in the MCP tool result text as `[<code>] <msg>`. The message string is for humans and may change, so the case branch is on the code, such as `[pane_not_found]` / `[shutting_down]`. Details in `.claude/skills/org-delegate/references/renga-error-codes.md`.

#### Grounds of the cadence setting (checked 2026-05-02)

The grounds for adopting `/loop 3m`:

- measured from journal.jsonl (188 events over 10.83 days, an event rate of 0.012/min, a median gap within an active session of 224 s);
- pane lifecycle (`pane_exited` / `events_dropped`) is detected **in about 2.5 s regardless of the cadence** by the long-poll path of `poll_events(timeout_ms=5000)`, so a longer cadence does not degrade reactivity;
- the worst delay of inspect / check_messages, from 60 s to 180 s, is well below the time scale of a human's judgment to resolve an anomaly (minutes to a dozen or so minutes);
- going from 1m to 3m **cuts the Foreman Claude's token use to about a third** (equivalent to 895 to 298 cycles over 10.83 days);
- ja#6 (the proposal of 1m to 20s) is already CLOSED, and this result agrees with its close comment (no need to reopen).

### The state of the all-pane index (a record as of 2026-08-10)

From: worker-monitoring Step 3 (3-a-5) (now background-tab-worker.md)

> **Now (an observation recorded on 2026-08-10, not a permanent fact; query it again each cycle)**: **conditions 1 and 2 of the table above do not hold** (the contract section is PROPOSED, and `cross_tab_list_first_drive` has no name on the query surface and is undetermined). **So the branch of this section does not fire even once today, and the liveness judgment of background workers runs on the three inputs as before.** What this section defines is "how to read once the contract is ratified and the first drive is recorded", not a permission to act before that. **The steps to lift it are written below**, not as "someone will ratify it some day" but in a form that can be run in order.

## Reasons and history moved from pane-close.md

### Why the done-report gate exists

From: pane-close 1 (the "reason" of the done-report gate)

A worker's report channel goes straight to the secretary. The absence of a done report in the dispatcher's message queue (what `check_messages` returns) does not mean it "does not exist in the system". It has often reached the secretary already, and skipping the check leaves the wrong conclusion "no done report arrived" in the retro (it really happened in the delegation smoke test of 2026-05-03).

That the CLI is started once per attempt as a one-shot judgment was decided by Issue #285.

### Why the first send is skipped when a receipt is recorded (history)

From: pane-close 1-0 (checking the receipt record)

- Sending "Has the done report arrived?" for a task that has a receipt record (`completion_reported_at`) fills the secretary's inbox with the same single fact. The false alarm of re-sending to the same task again and again (2026-08-08, Issue #869) is the ground.
- `run_status == 'completed'` is let through whether or not a record exists because questions were going out every time even for merged tasks (it happened for 4 tasks on 2026-09-23).

### What renga does when a close succeeds

From: pane-close 3 (closing the pane)

When a close succeeds, renga emits `Event::PaneExited` exactly once (through the exit_event_emitted guard).

### The starvation of the curate threshold check (a known limit)

From: pane-close 5 (the curate threshold check)

During a period with no worker closes (only raw notes added by hand, only skill candidates growing, and so on), the threshold check does not run. Auxiliary triggers are in the backlog as Issue #501 (at the end of org-retro) and Issue #502 (a backstop in org-start). An increase that the coalescing of 5-2 did not pick up is also insured by these auxiliary triggers when no worker close happens for a long time.

### How a live curator with no inflight record can be reached

From: pane-close 5-2 (the single-flight check)

This combination can be reached by design: by the path in which the spawn of 5-3 succeeded but the dispatcher was `/clear`ed or crashed before writing the inflight record, and by the path in which a row of the close judgment table of worker-monitoring Step 5.3 that deletes the inflight record without firing a close (rows 3, 4 and 6) was taken and its "confirmed gone" was in fact wrong. If only coalescing happens without making the inflight record, nothing tracks it, so the monitoring loop's Step 5.3 does not collect it either, and recovery has to wait for Step 5 of `/dispatcher-resume` regenerating the inflight record (the next handover or resume).

### Why the curator is started with model="sonnet"

From: pane-close 5-3 (spawning the curator's pane)

The safety classifier of auto mode runs on a dedicated model independent of the session model, and the approval judgment does not depend on the session model (official: https://www.anthropic.com/engineering/claude-code-auto-mode). The curator's workload of organizing knowledge can be classed as light and mechanical, so Sonnet is enough (a judgment independent of the workers' model policy).

### The stale-binding branch of `[name_in_use]` is not a row of the close judgment table

From: pane-close 5-3 (telling apart `[name_in_use]`)

The close judgment table (worker-monitoring Step 5.3) is the rule for "how to fold `curate-inflight.json`, which tracks a spawned curator", and in the stale-binding branch neither a curator pane nor an inflight record exists yet. It is a separate situation outside the table, so it does not conflict with the table's disposition rules.

### Why the old `REPO_FLAGS` procedure is not used

From: pane-close 6-1 (running the scan)

Whether the flag string becomes several arguments depends on the word splitting of the calling shell. zsh, the pane's login shell, has `SH_WORD_SPLIT` off by default and does not split an unquoted `$REPO_FLAGS`, so it reached argparse as one argument, and the scan at each worker close failed with exit 2 every time (bash splits it into 4 arguments, so it could not be noticed in bash; Issue #829). zsh's own `${=REPO_FLAGS}` is a symptomatic fix too, so it is not used. The one-command form goes through no shell at all, so it works the same in zsh, bash and Windows `py -3`. The rule to always notify the secretary of exit 2 came with the same Issue.

### Holding back re-sends of the same candidate (history)

From: pane-close 6-2 (branching on the exit code)

Re-sending the same candidates at every worker close fills the secretary's inbox with the same proposals (on 2026-09-23 the same 3 were re-sent 7 times).

### The ledger definition of `work_discovery_scanned`

From: pane-close 6-3 (the audit log)

Adding the definition of `work_discovery_scanned` to `docs/journal-events.md` was left out of scope when the procedure was written (design section 10 / section 7.1). `tools/journal_append.sh` takes the event name as a free-form string and records key=value as it is (it neither reads nor checks the ledger), so the procedure does not depend on whether the ledger defines it.
