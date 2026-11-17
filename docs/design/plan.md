# makiba: plan

Status: draft, 2026-11-18. Written after reading claude-org-ja `main` (#1279), paddock `main` (#468), claude-org-runtime 0.1.58, herdr-core 0.1.21 and Orca `main` of 2026-11-17, and the Claude Agent SDK documentation of 2026-11-17.

makiba has one purpose: the person deals with one secretary, and everything is done through that conversation. The person asks the secretary, is asked by the secretary, and gets results from the secretary. The person is never called to a worker's terminal and never hears from a worker directly.

To hold that by mechanism and not by asking a model, the dispatcher is code, and no model operates a terminal. The secretary and the workers are interactive Claude Code sessions in terminals. The terminals run on herdr-core, which sits behind one module so that it can be replaced.

Names and shapes shared across Issues are in [spec.md](spec.md). Past incidents and the Issue that guards each are in [incidents.md](incidents.md). Measurements still to make are in [probes.md](probes.md).

## The measure

The design is judged by one measure, set by the person it is built for: it should raise that person's own productivity as far as it can, while nobody but the model provider shapes how the work is done. That does not mean using nothing from outside. It means the core, which is the discipline and the design of the mechanism, stays makiba's own and does not move when someone else's product moves.

What follows from it:

| Thing | Whose it is | How it is held |
| --- | --- | --- |
| The rules for each role, the skills, the goal ledger, the acceptance gate, the dispatcher, the records | makiba's own. This is the core. | Written here, tested here. Nothing outside may change them. |
| Claude Code and its features (system prompt, permissions, sandbox, hooks, skills, connectors) | The model provider's. Accepted. | Used as they come. makiba adds rules to Claude Code; it does not replace what Claude Code is. |
| herdr-core | A third party's. A part, not the core. | Behind `makiba/base/` (spec section 6). Pinned. It carries no rules for agents. |
| GitHub (pull requests, Issues, CI) | A third party's. A part, but named in the flow of code work. | Reached through `gh` from the secretary's skills and the PR watch. No seam is built for it now; it is the largest dependence on a third party that is left, and it is recorded here so that it is not mistaken for a decision. |
| Orca's orchestration (its coordinator and worker procedure) | A third party's rules for agents. | Not used. Taking it would hand the discipline to Orca and let an Orca release change it. |

## 1. The requirement: one contact

Everything else in this plan follows from it. It holds whatever the kind of work: a request for code and a request for a document (an article, a note, slides) go to the same secretary. It needs four conditions.

| | Condition | Held by | Issues |
| --- | --- | --- | --- |
| R1 | A stopped worker's question or permission prompt always goes to the secretary. The person is not called to a worker's terminal. | The dispatcher detects the stop and delivers it to the secretary. A permission prompt becomes a pending entry; the dispatcher presses the key the secretary's answer names. | M2-3, M6-4, M6-8 |
| R2 | Notices to the person come only from the secretary. | The base sends no notices of its own. When notifications to the outside are added, they come from the secretary and the pending table only. | M4-4, M2-6 |
| R3 | The secretary knows the whole state across days. | Pending decisions and running tasks live in the ledger. A renewed secretary resumes from it. | M6-4, M6-5 |
| R4 | From request to handing over the result and proposing the next task, the secretary carries it: to the pull request and merge for code, to the finished document for a document. | The secretary's skills, the acceptance gate, PR watches, triage. | M5-3, M5-5, M6-3, M6-6 |

How each predecessor and Orca stand, untouched:

| | claude-org-ja | paddock | Orca as it is |
| --- | --- | --- | --- |
| R1 | Yes. | Yes. | Questions go to the coordinator. Nothing in its guide routes a permission prompt through the coordinator. |
| R2 | Yes. | Yes. | No. The phone is notified when any agent needs input. |
| R3 | Yes. | Yes. | No mechanism. "Coordinator" is whichever agent the person is talking to, not a standing role. |
| R4 | Yes. | As far as the pull request. | No procedure. The coordinator can run the commands itself. |

claude-org-ja meets the requirement, but held it by asking a model dispatcher, which is where its incidents came from. Orca's machinery is sound, but the requirement is not among its design goals. makiba aims to hold the requirement by mechanism.

## 2. Where this is going

The aim is an organization that keeps running on a server: reachable from outside, working while nobody watches, and calling a person only when a person's judgment is needed. This plan is stage 1.

| Stage | What becomes possible | Contents |
| --- | --- | --- |
| 1. Foundation (this plan) | The work claude-org-ja does today runs on the new base with nobody watching it. | The dispatcher as code. Records in `state.db`. Models kept away from the base. |
| 2. Resident | Starting the machine starts the organization, and what happened while away reaches the person. | Installation on the server. Start at boot. Notifications of pending decisions, incidents, completions, coming only from the secretary and the pending table (R2). |
| 3. Entry from outside | The person talks to the secretary and answers approvals from elsewhere. | A chat entry to the secretary. Remote approval. |
| 4. Memory and back office | Past exchanges can be searched. Routine chores can go to a local model. | Search over past conversations. A local model for back-office work; the person still talks to Claude. |

Kept in view without a stage: a screen that reads `state.db`, agents other than Claude Code, more than one machine.

A shortcut exists for stages 2 and 3 while the secretary is Claude Code: its Remote Control feature lets the Claude app on a phone drive a session running on the person's machine and receive push notifications from it. Turned on for the secretary only, it gives an entry from outside and notices that come from the secretary alone, which is what R2 asks. It is used as a shortcut and not as the mechanism:

- Nothing in `makibad`, the ledger or the skills knows it exists. It is one launch option inside the Claude Code agent module, on for the secretary, never for a worker.
- It is Claude Code's alone. A secretary run by another agent program loses it, and Claude decides when to push, so a pending decision is not certain to ring the phone.
- Stages 2 and 3 therefore stay in the plan, built from the pending table and `makiba answer`, which depend on no agent program. The shortcut lets them wait; it does not replace them.

Whether it works for a session launched on herdr-core, and what happens to the phone's connection when the secretary is renewed, is read and not measured (probe P6).

How stage 1's decisions serve the later stages:

| Decided now | Used later |
| --- | --- |
| The dispatcher is a resident process. | Stage 2 starts it at boot; it can own sending notifications. |
| Terminal events go through a delivery ledger. | Stage 2 adds a recipient to the same ledger. |
| Pending decisions live in a table and are answered with `makiba answer`. | Stage 3's remote approval calls the same command. |
| Models touch neither the base nor `state.db`. | Stage 3 adds an entry without adding a boundary. |
| History is in `state.db`. | Stage 4 searches it. |
| Agent differences sit in one module. | Stage 4's local model, or a Codex worker, enters through the same seam. The seam leaves room; it does not make the change cheap. The boundary that keeps a worker from the base (deny rules, sandbox, hooks) is built from Claude Code's features and would be built again for another program, and Codex reports no kind of block, so telling a permission prompt from a question would rest on reading the screen. |
| Everything that touches the base sits in one module. | The base can change without the dispatcher or the rules changing. |

Not built in stage 1, only left room for: where notifications go, the chat connection, switching models.

## 3. The base, and how agents run

### 3.1 The base is herdr-core

The base launches agent terminals, knows their status, and types into them. Two candidates were read.

| | herdr-core | Orca |
| --- | --- | --- |
| What it is | A headless terminal runtime. About 12,000 lines of Rust, one maintainer. | A desktop app for running agents in parallel. MIT, over 20,000 commits on `main`, about 120,000 stars, built by a company. |
| Launch and control | Yes (API). | Yes (`terminal create`, `read`, `send`, `wait`, `agent launch`). |
| Status | OSC 7501 only. | Per-agent hooks, and OSC 7501 where hooks do not speak. |
| Screen | None. | Yes, with diff review and a browser. |
| Phone notifications and replies | None. | Yes. They come from agent status and the terminal bell, for every agent. |
| Headless server | It is one. | `orca serve`. |
| Agents | Any program that reports OSC 7501. | About 40. Its matrix records Codex 0.163.0 as reporting state only, without the kind of a block. |
| Rules for agents | None. | Yes: the coordinator and worker procedure, when to answer a question, auto-closing finished workers. Shipped with the binary and changed per version. |
| What an agent may do to it | No permission boundary: whoever reaches it can do anything. | Agents driving Orca through its CLI is a feature. |

herdr-core is chosen, by the measure above and not by its machinery, which is the smaller of the two:

1. It is neutral. It does not know which agent it runs and carries no procedure for agents. Orca ships a procedure, notifications and an app as one product; to use only its terminals, makiba would have to keep the rest switched off, release after release.
2. It is small. Less of it can change without makiba noticing, and the part makiba uses is checked against herdr-core's own schema (Issue M1-5).
3. It is measured. paddock ran the full loop on it and recorded what holds (probes.md section 1). Nothing about Orca has been run.

What Orca does better, stated so that the choice can be revisited:

- It has far more development behind it and far more users. herdr-core has one maintainer and may stop.
- It has a headless server with phone notifications and replies today. On herdr-core, stages 2 and 3 are built.
- It supports many agents and records what each reports.

Orca's low-level terminal commands could serve as a base behind the same module. That is the second choice, and it is not built. Any one of these is a reason to look at it again:

- herdr-core's development stops, or a release removes something makiba uses.
- The Remote Control shortcut (section 2) does not work, and building notifications from outside (stages 2 and 3) proves to cost more than keeping Orca's own procedure and worker notifications switched off.
- Codex becomes a real worker and herdr-core's status for it is not enough.

The checks to run before such a move are kept in probes.md section 3. They are not part of this plan's work.

One implementation of the base exists at a time. claude-org-ja ran two transports side by side and paid for it. makiba cuts the seam and fills it once.

### 3.2 Agents are interactive Claude Code sessions

The secretary and every worker is the interactive `claude` program in a terminal. Not `claude -p`, and not a session driven through the Claude Agent SDK.

Reasons:

- The person's experience is that work run through `claude -p` is less flexible and less capable than interactive Claude Code. The cause is not established.
- Read in the Agent SDK documentation (2026-11-17): a session gets Claude Code's system prompt only when the preset is asked for, and the default for which settings files are loaded has differed between versions. A session left to defaults is therefore not Claude Code, and may not load the rules files at all.
- Read in public issue reports (March, July and September 2026; not reproduced here): connectors added on claude.ai did not attach, or needed authorization, in non-interactive runs, while interactive sessions had them.
- The interactive program is the path the model provider puts first. It gets the rules, the settings and the connectors without being told to.

The Agent SDK remains a possible way to run a worker later, as Codex does. Neither is built. An SDK session is not a terminal, so it would need a launch path of its own in `makibad` beside the base; the agent module (spec section 9) is where its other differences would go.

## 4. Scope

In: the dispatcher (dispatch, monitoring, close, message delivery, ownership of PR watches); the secretary, workers and curator as terminals of the base; the secretary's skills, rebuilt from paddock's so that they operate no terminal; past incidents as tests; one week of self-hosting, then the switch from claude-org-ja.

Out, each a later plan: notifications, chat entry, conversation search, the local model, Docker distribution and the server installation, a screen over `state.db`, more than one machine, picking requests up from mail and chat (libretto does that; it is not built here), a second base, a second agent program.

### 4.1 What the predecessors are after makiba

Decided by the owner on 2026-11-18. makiba is one line of work and does not absorb the others.

| | After makiba | Reason |
| --- | --- | --- |
| claude-org-ja | Stopped at M7-3, for the projects it serves. Its repository stays as the source of the files makiba carries (section 7) and as history. makiba does not run beside it for long: two organizations on one set of projects is the state M7-3 ends. | makiba is its successor. It holds the same requirement by mechanism instead of by asking a model dispatcher (section 1). |
| paddock | Continues as a lightweight organization beside makiba. makiba does not replace it, and M7-3 does not move its projects. makiba copies files from it (section 7); those copies are makiba's own from then on. | Being light is paddock's own value: it has no resident dispatcher, no SQLite and no Python. Work that needs none of those is served by an organization that has none, and nothing in makiba requires paddock to stop or to follow makiba's choices. |
| rondo, with cadenza and continuo | Frozen. Not developed further; makiba takes nothing from it. | It did not converge toward running real work. Developing several organizations in parallel is also more than one person can follow. |

paddock and makiba can run on one machine. The names are kept apart for that (section 5, "Names"), and each keeps its own state folder.

## 5. Decisions

| Topic | Decision | Reason |
| --- | --- | --- |
| The person's contact | The secretary only. Workers and the curator do not address the person, and the person never needs to open their terminals. The base sends no notices of its own. | The requirement. The other decisions exist to hold it by mechanism. |
| Whose the core is | The rules, skills, ledger, gate, dispatcher and records are makiba's own. Third parties' products are parts behind a seam or named as a dependence; none supplies rules for agents. | The measure. |
| Kinds of work | Two: code and document. Both are asked of the secretary and made by a worker; only the workplace and the ending differ. Code: a worktree, a pull request, the acceptance gate. Document: a working folder, a second worker reads the draft before the person does, and the secretary hands it over. libretto's mechanism (its resident host, page, ledger, intake from mail and chat) is not brought in. Two of its ideas are borrowed as rules: someone other than the maker reads first, and a fact carries its source. | One contact does not need two systems joined; it needs a secretary that takes both kinds of request. |
| How agents run | Interactive Claude Code in a terminal, for the secretary and for workers. Not `claude -p`, not the Agent SDK. | Section 3.2. |
| Terminal base | herdr-core. One module touches the base; one implementation exists. Version pinned to one patch release, which the probes are also run on. The pin moves rarely and on purpose, about once a month at most; each move reruns the compatibility test, both test runs and probes P1, P3, P4 and P5 (M0-2, `docs/upgrading-the-base.md`). No tmux, no renga path, no Orca. | Section 3.1. Two transports side by side caused incidents and most of the explaining. herdr-core releases every few days and calls its API a preview; a range would let in a version no probe has run on. |
| Names | Repository, package and command are `makiba`; the resident process is `makibad`; the role keeps the name "dispatcher". | Free on PyPI and npm. One name avoids a clash with `org` on a machine that also runs paddock or claude-org-ja. |
| Sources | From claude-org-ja: the Python assets (`state.db`, deterministic tools, hooks, contracts). From paddock: the English rules and skills, worker settings, the list of lines to hold, the probes. | paddock has run the full loop on this base and measured it. claude-org-ja has the ledgered records. |
| Language | English for rules, skills, contracts, code, messages between agents, knowledge notes. Two exceptions: what the secretary says to the person follows a setting; pull requests, issues and commits default to English and can be set per project. | The English mirror of claude-org-ja stopped at ja#763 while the Japanese one reached #1279. One source. |
| Use of the base | Only `makibad` talks to it, by JSON-RPC on herdr-core's Unix socket. | herdr-core passes environment variables only through the API. |
| Models and the base | No model role touches the base, the secretary included. They ask `makibad` through `makiba`. | herdr-core has no permission boundary. With no role allowed in, there is one line to hold. |
| Rules and settings | Nothing is written into a target repository. Role rules are a file outside the worktree passed at launch; settings are outside too; only a per-task settings file is written. | paddock's way; a worker could not rewrite its settings there (measured). |
| Messages | Kept by makiba. Senders queue with `makiba send` and `makiba report`. `makibad` delivers by typing the wrapped text into the recipient's terminal when it is idle or done. No box for the receiver to check. Long bodies are cut and read with `makiba show`. | A resident dispatcher with a ledger can hold and retry, so the receiver needs no box and no "you have mail" line. No broker and no channel feature: one startup dialog fewer, nothing specific to Claude Code. If typed bodies break on a real composer (probe P4), only a short note is typed and the body is read with `makiba show`. |
| Who writes `state.db` | `makibad` only. `makiba` talks to `makibad` and never opens the database. Senders are identified by a per-terminal key. | A command that wrote directly would need the worker sandbox to allow writes to `state.db`. |
| Claude Code's version | A supported range, from the lowest to the highest version that a recorded probe run covers, checked at `makiba up`. It moves only after probes P1, P3, P4, P5 and P7, paddock's carried probes and the M4-5 runbook have been rerun on the new version (M0-2, `docs/upgrading-claude-code.md`). | Claude Code is used as it comes ("The measure"), but the screens makiba reads (startup dialogs, permission prompts) and the settings it writes belong to a version, and Claude Code updates itself and is released often. Using it as it comes does not mean not knowing which version was measured. |
| Who is the person at the door | A key in a file, like every other caller. A request with no key is refused. The key file is outside what any agent's sandbox may read. | Every agent runs as the person's own user, so the user id and the lineage of a process do not tell a worker from the person: a worker can drop its key, and a process can leave the terminal it was launched in. What an agent cannot do is read a file the sandbox hides. Whether the sandbox does hide it is probe P1 (spec section 3). |
| The base's own messaging and rules | Not used: no parent, no notify, `notes = false`. | One route for messages, one rulebook, one contact. |
| Dispatch | The secretary runs `makiba dispatch`; `makibad` creates the queued run. No model asks another model to dispatch. | The reservation already exists in `state.db`; a request cannot be lost in a conversation. |
| Terminals and runs | A label herdr-core keeps on a terminal is the record. `state.db` holds no terminal address. | Two records drift. After a restart the view is rebuilt from labels. |
| Reading the screen | Two places only: the startup dialog and a worker's permission prompt. Option lines are matched whole; the cursor is moved and the screen read again before a key. | A terminal stopped at a startup dialog reports no status (measured in paddock). Everywhere else status is enough. |
| Retrospectives | What the model dispatcher wrote moves to the secretary. `makibad` only refuses to close before a done report. | A retrospective is judgment. |
| Implementation | Python, standard library first. No dependency on claude-org-ja or claude-org-runtime: needed files are copied. | The carried tools and tests are Python. From the runtime only the dialog matcher is needed. |
| Records and runtime | SQLite (`state.db`), written by a Python program of makiba's own (`makibad`). This reverses paddock's choice: paddock keeps its records in herdr-core's document store and per-writer JSON-lines files, with no SQLite (its decision record 0004), and installs nothing beyond what herdr ships. | paddock has no resident process and its readers are herdr mods and the shell, which cannot open a database. makiba has one process with several writers (the door, monitoring, delivery, PR watches) and needs three things a file per writer cannot give: one transaction for "launch verified and run in use" (M3-6), a unique `(source_event_id, recipient)` delivery row (M0-5), and queries across runs, events and deliveries. The schema, writer and tests it carries are SQLite already, and `sqlite3` is in Python's standard library, so only the interpreter is added, not a package. What it costs: every host needs Python 3.11 or later; a herdr mod cannot read the records, so a screen over them (kept in view in section 2) is built on `makiba status` or its own reader; and a machine that runs paddock and makiba holds two record stores in two folders. |
| Starting the base | From a person's login shell or a start script, never from inside an agent; herdr-core in a dedicated session. Session-bound `CLAUDE*` variables and herdr-core's own variables are removed at each launch. | The starter's environment reaches every terminal and can turn transcripts off; `HC_*` reach workers unless removed (both measured in paddock). |
| A stopped worker | The secretary takes it first and answers questions by message. A permission prompt is recorded as a pending entry; the secretary decides or asks the person; `makiba answer` makes `makibad` read the screen again and press one key only if the prompt is unchanged. Workers run in auto permission mode. | Calling a person to a terminal for every stop defeats a resident organization. Secretary-first is claude-org-ja's design; the read-then-press relay passed on a host in paddock. |
| Reaching the secretary from a phone | While the secretary is Claude Code: its Remote Control, on for the secretary only, as an option of the agent module. The mechanism of record stays the pending table and `makiba answer`; stages 2 and 3 are deferred, not dropped. | The model provider's own feature, and it meets R2 as it comes. But it belongs to one agent program, so nothing else may lean on it. |
| Agent differences | One module per agent program: argv, removed variables, dialog policy, delivery, settings, resume. Two exist: Claude Code and the test agent. Codex and the Agent SDK are not built. | Status is a shared protocol (OSC 7501), so the differences collect here, and room for another agent costs nothing now. |

## 6. One task, end to end

1. **Request.** The secretary runs `makiba dispatch`. A `queued` run and `delegate_sent` are recorded.
2. **Dispatch.** `makibad` finds the run, checks capacity, prepares the worktree, writes the per-task settings, issues the key, and asks the base to launch. The task text travels in the launch arguments.
3. **Verification.** If a startup dialog shows, it is matched and passed. When the first turn is observed, `worker_spawn_verified` is recorded and the run becomes `in_use`. If it is not observed, the launch is recorded as failed and the secretary is told.
4. **Work.** `makibad` records status and events. An anomaly is told to the secretary once. A permission prompt becomes a pending entry.
5. **Report.** The worker runs `makiba report done`; `makibad` delivers it to the secretary, who asks the person.
6. **Pull request.** The secretary pushes, opens the PR and runs `makiba pr`. `makibad` holds one watch per PR and head and delivers the result.
7. **Close.** The secretary runs `makiba close`. `makibad` checks the done report, ends the terminal, removes the worktree, closes the run, then checks the curation threshold and forwards triage results.
8. **Unexpected end.** A terminal that ends without a report fails its run and the secretary is told. A kept shell is resumed by conversation id; failing that, a fresh worker gets the original task.

## 7. What is carried

From claude-org-ja (checked file by file for references to the transport layer):

- `tools/state_db/` (schema, writer, queries, snapshotter): no references. Carried as is.
- `tools/pending_decisions.py`, `tools/notify_gate.py`, `tools/work_discovery_goals.py`, `tools/work_discovery_dedup.py`: no references. `pending_decisions` changes its store to a table.
- `tools/work_discovery_scan.py`: capacity is computed per transport. That part is replaced.
- `tools/pr_watch.py`: pushes to the secretary itself and switches transports. The push is removed; it records only.
- `tools/acceptance_gate.py`, `tools/acceptance_merge_hook.py`, `.hooks/gate-acceptance-before-merge.sh`.
- Hooks: `block-git-push.sh`, `block-dangerous-git.sh`, `block-no-verify.sh`, `block-workers-delete.sh`, `check-worker-boundary.sh`, `block-foreground-subagent.sh`.
- `docs/contracts/state-semantics-contract.md`, `delegation-lifecycle-contract.md`, `role-contract.md`, and `.dispatcher/references/rationale.md`: translated.
- From claude-org-runtime: `dispatcher/spawn_prompt.py` (`decide` and what it needs).

From paddock:

- `templates/secretary.md`, `secretary-rules.md`, `secretary-settings.json`, `worker.md`, `worker-rules.md`, `worker-settings.json`, `curator.md`. The mailbox commands become `makiba` commands.
- Skills that hold judgment: `org-delegate`, `org-escalation`, `org-pull-request`, `org-issue`, `org-update-branch`, `org-curate`. Calls to `hc` become `makiba` commands.
- `docs/design/predecessor-inventory.md` section 6.2: the lines to hold (L-K1 to L-K12, L-N1 to L-N9).
- `scripts/probes/`: rerun when the Issue that relies on each lands.
- `scripts/second-model-review.sh` and its round cap.
- `docs/decisions/`, except 0002's conclusion ("no `hc send` for messages"): makiba has a resident dispatcher that can retry, which 0002 did not.

Not carried: claude-org-ja's renga path, transport switch, skill templates, `.dispatcher/`, the tools that check whether the model ran a step, composer screen-reading, the cockpit, the Windows layers; paddock's JSON-lines records and core documents, its shell mailbox, its start, stop, suspend, resume, watch and handover scripts (these become `makibad` functions), its mods. From Orca: nothing.

## 8. Milestones

```
M0 foundation -> M1 base module -> M2 monitoring ---------------> M5 close and periphery -> M7 self-hosting
                        -> M3 dispatch -> M4 boundary  -> M6 secretary -----------> M7
```

Before M2 and M3, a person runs probes P1 to P5 and P7 (section 9). M2 and M3 run in parallel, as do M5 and M6. Through M3 only the test agent is used; real Claude Code starts in M4. The Issues are in [`issues/`](../../issues/index.md).

| Milestone | Done when |
| --- | --- |
| M0 Foundation | The base's version is pinned, an end-to-end test with the test agent runs in CI, and every incident row names a test. |
| M1 Base module | The test agent can be launched, read, typed into and ended, and no event in between is missed. |
| M2 Monitoring | Each scripted scene raises its anomaly once; messages to the secretary arrive once; monitoring survives killing `makibad`. |
| M3 Dispatch | A queued run reaches `in_use` with no person; stopping `makibad` at any step causes neither a second terminal nor a stranded run. |
| M4 Boundary | One real worker goes from request to done report with no person, and the base's command run from inside it is refused. |
| M5 Close | After close, no terminal and no worktree remain; a push replaces the PR watch. |
| M6 Secretary | A request, a PR and a close happen through conversation with the secretary alone; no skill or rules file names a terminal tool or the base's command. |
| M7 Self-hosting | One week of makiba developing makiba, incident tests green, claude-org-ja not started. |

## 9. How it is developed

- makiba is registered as a project in claude-org-ja with the ledger in [`goals/`](../../goals/suisya-systems/makiba.md).
- Merge approval is `acceptance`: the first commit of every pull request is the failing test, declared with an `Acceptance-Test:` trailer.
- Probes P1 to P5 and P7 (probes.md section 2) are run by a person, by hand, on the pinned herdr-core version, before any Issue of M2 or M3 is started. They need no makiba code. P1 decides the door (M3-3) and whether the plan stands; P3 and P4 decide how messages are typed (M2-4); P5 decides how the task reaches the agent (M3-5, M3-6); P7 decides whether the person's own Claude Code settings reach the agents and whether a worker can notify the person (M3-4, M4-3); P2 decides where real-base tests run and is needed earlier, by M0-3. Results go into probes.md section 4, and no Issue of M2 or M3 is dispatched until all six are there. P6 (Remote Control) waits for M6-2, because nothing depends on it. A probe is rerun when the Issue that relies on it lands and when the pin moves.
- claude-org-ja's workers cannot start a herdr-core inside their sandbox (recorded in paddock's inventory, section 2). Workers test against a stand-in for the base; tests that need the real one run in CI.
- Tests that use a real base always use a place of their own. Nobody's own terminals are touched.
- Three to five workers at a time.
- Issues M4-5, M6-7's host check and all of M7 cannot start from a failing test. They are outside the acceptance gate and are closed by a person.
- Nothing outside the scope is added between M1 and M6. Ideas go into Issues for after M7.
- Left for after M7, recorded and not planned: a check that no rules file tells the person to open a worker's terminal (goal G1, fourth clause), and a mechanism that makes the second reader of a document read before the secretary hands it over (requirement R4 for documents). The first target of makiba is code work.

Estimate: 49 Issues and 50 to 60 pull requests. No calendar estimate is given: the earlier one (weeks) had nothing behind it, and what actually limits the schedule can be counted.

- Probes run by the owner. P1 to P5 and P7 by hand before M2 and M3 (a few hours each, so more than a day of the owner's own time, in a login shell, on a real sandbox and a real Claude Code); P1 again in M4-2; P6 at M6-2. No worker can do these: workers cannot start the base in their sandbox.
- Issues checked by the owner. M4-5 (the first real worker), the host check of M6-7, M7-1 and M7-2 close on the owner's look, not on a test. M7-2 is seven days of use by definition and cannot be shortened. M0-1 is merged in `human` mode, and the repository, its labels and its branch protection are made by the owner before the rest can be merged test-first. M7-3 and its week of observation come after M7-2 and are not counted here.
- Serial chains. The dependencies are in `tools/build_issues.py`, and each link is at least one pull request (a failing test commit, CI, review rounds, merge) that cannot overlap the one before it. The longest chain to M4-5 is 13 Issues: M0-1, M0-2, M0-3, M1-1, M1-3, M2-1, M3-1, M3-2, M3-4, M3-5, M3-6, M3-7, M4-5. The longest chain to M7-2 is 15: the 13 above with M4-5 last, then M7-1 and M7-2. M7-1 waits for M4-5, M5-2, M6-2, M6-5 and M6-8 as well as M6-6 and M5-5, which together are what the week of use needs (one real worker through, the secretary launched and renewed, a permission prompt relayed, a close); goal G7 still asks that every Issue of M0 to M6 be closed by the end.
- Width. The graph has thirteen levels and at most six Issues on one level, so beyond the three to five workers named above it is a chain, not a lack of workers, that is waited for.

Within those limits the pull-request count is not what decides the time; the owner's availability for the probes and for M4-5 and M7 is.
