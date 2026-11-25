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
| herdr-core | A third party's. A part, not the core. | Behind `src/base/` (spec section 6). Pinned. It carries no rules for agents. |
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
| 2. Resident | Starting the machine starts the organization, and what happened while away reaches the person. | Installation on the server, from makiba's own releases in the base's shape: one binary per target and a manifest giving each binary's `url` and `sha256` (Issue 62; the decision is the row "Releases" of section 5). Start at boot. Notifications of pending decisions, incidents, completions, coming only from the secretary and the pending table (R2). |
| 3. Entry from outside | The person talks to the secretary and answers approvals from elsewhere. | A chat entry to the secretary. Remote approval. |
| 4. Memory and back office | Past exchanges can be searched. Routine chores can go to a local model. | Search over past conversations. A local model for back-office work; the person still talks to Claude. |

Kept in view without a stage: a screen over the read surface (Issue 169; its design note is [screen.md](screen.md)), agents other than Claude Code, more than one machine.

A shortcut exists for stages 2 and 3 while the secretary is Claude Code: its Remote Control feature lets the Claude app on a phone drive a session running on the person's machine and receive push notifications from it. Turned on for the secretary only, it gives an entry from outside and notices that come from the secretary alone, which is what R2 asks. It is used as a shortcut and not as the mechanism:

- Nothing in `makibad`, the ledger or the skills knows it exists. It is one launch option inside the Claude Code agent module, on for the secretary, never for a worker.
- It is Claude Code's alone. A secretary run by another agent program loses it, and Claude decides when to push, so a pending decision is not certain to ring the phone.
- Stages 2 and 3 therefore stay in the plan, built from the pending table and `makiba answer`, which depend on no agent program. The shortcut lets them wait; it does not replace them.

Whether it works for a session launched on herdr-core, and what happens to the phone's connection when the secretary is renewed, is read and not measured (probe P6).

How stage 1's decisions serve the later stages:

| Decided now | Used later |
| --- | --- |
| The dispatcher is a resident process. | Stage 2 starts it at boot; it can own sending notifications. |
| Terminal events go through a delivery ledger. | Stage 2 adds a recipient to the same ledger. The person's call (Issue 272) is recorded instead as events with `to` `person` (`notify_sent`, `notify_failed`), outside that ledger: the person has no box. |
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

### 3.3 The door and delivery

Decided by the owner on 2026-11-18, after the first host measurements (probes.md section 4: P1, P3, P4, P5, and the parts of P8 to P11 that were run). Two questions were open: how an agent reaches `makibad`, and how a message reaches its recipient. P1 and P3 had failed, and each failure showed that a predecessor had already measured something this plan assumed. The decision comes first, then what was measured and what was not, then the grounds: what the predecessors had and the options that were compared.

#### The decision

1. The door. Agents (the secretary, workers, the curator) make requests by calling tools of an MCP server in `makibad`, on a local HTTP port, each with a token of its own (option B below). The person's own `makiba` keeps the Unix socket and the person key (spec section 3). Reason: on Linux the sandbox blocks the creation of any Unix socket and ignores a path allowlist (P1), and opening every socket reaches a container engine, an ssh agent and the session bus (option D). An MCP tool is called by Claude Code itself and not through the sandbox, and a Bash call inside the sandbox could not reach the port (P11). Option A, a folder per agent, is the way back.
2. The token is given in the launch arguments (`--mcp-config`) and is never put in the environment. Reason (P11): a token in the environment could be read from Bash; one in the argument list was not found; a token file under a denied path was refused; another session's tokens were not visible.
3. Delivery, the default. The body of a message is kept in `makibad`'s box (`messages` and `event_deliveries`) until delivery is confirmed (point 5). In every direction the default route is the same: a Claude Code channel carries the body straight to the recipient, and nothing is typed. Reasons: (a) The secretary's terminal is the one a person types in, and typed text joins a half-typed draft (P3 on hc 0.1.21, in `keys` mode and with a person typing; paddock's record 0002 on hc 0.1.20). A channel does not touch the input field: in the owner's operation of claude-org-ja, which ran one, no notification was ever mixed into a person's input (2026-11-18). That is operating experience, not a measurement made for makiba. (b) A channel also reaches a recipient that is working: in P9 a message sent during a long turn was delivered and acted on at a tool boundary. So `makibad` has no reason to wait for a recipient to be idle or `done` before delivering, and nothing in it holds a message for that.
4. Delivery, typed on the sender's choice. `send` and `answer` take `typed`, and the wrapped text of spec section 8 is then typed into the recipient's terminal. Reason (the owner's operating experience): a message that comes by channel is sometimes refused as not being a message from the person, and the work is declined. claude-org-ja records the same and types its operating instructions for it: since Claude Code 2.1.325 a channel message carries a note that it is external input and not from the user, and the dispatcher there holds instructions that make it spawn, close or press keys; those are sent with `deliver="user_turn"` (`.claude/skills/org-delegate/references/dispatcher-send.md`, read in full, and `CLAUDE.md`, only its lines on the receive model and on the handover path under "Role boundaries"). `makibad` cannot see a refusal: a turn starts and the recipient only answers that it declines. The sender, who reads that answer, sends again with `typed`. The secretary's rules and skills say so (M6-3), and say that an instruction known from the start to need typing is sent typed at once. Two limits. Typed goes only to a terminal nobody types in, a worker's or the curator's, never the secretary's (P3's join). And it is typed only when the recipient is `idle`, which the base checks itself (`hc send --if idle`; `hc send --help` of 0.1.21 shows `--if` taking `idle` or `working`), or `done`, for which the status is read just before the send and the race is accepted, as spec section 7 already did. In any other state `makibad` returns `conflict` to the sender and stores nothing. It could hold the message and type it at the next `idle`, but that is a watcher that waits for the recipient, which point 3 removes; the sender asks again.
5. Confirmation. After a push, `makibad` reads the recipient's status from the base (OSC 7501) to see that a turn starts. When the recipient was `idle` or `done` at the push, a turn start within a few seconds confirms it (0.4 s in P8) and the delivery becomes `delivered`. If none starts, the message has not arrived: `makibad` types one fixed line to wake the recipient, only into an `idle` or `done` terminal and after reading the prompt line of its screen so as not to type over input (paddock's watcher does this, record 0013), and the recipient reads the box with the tool `inbox`, which returns the unconfirmed messages and confirms them. A recipient that was working at the push shows no turn start, so when its turn ends and a message is still unconfirmed, `makibad` wakes it the same way. A message can therefore be seen twice, once through the channel and once from the box. Delivery is at-least-once, and the same id on both (spec section 8) is how the recipient knows it is one message. The fixed line carries no message, and a body is typed only on the sender's request and once, so goal G7's "a message is typed twice" is not met by this.
6. What is still watched. Nothing in `makibad` watches in order to deliver. What remains is for other purposes: a stop (the anomaly rules, spec section 11), the start of a launch (the first turn, M3-6), and whether a push arrived (point 5).
7. What this gives up. Section 5, Messages, chose "no broker and no channel feature: one startup dialog fewer, nothing specific to Claude Code", on the ground that a resident dispatcher can hold and retry typed text. That choice is reversed, for the reason in point 3(a): a retry does not stop typed text joining a person's draft. The costs are a development-channel dialog on the policy list (spec section 7) and a route that exists only for Claude Code. With another agent program only the fixed line, the box and typed bodies on request exist, so that route is built and tested as a real one (M2-7) and not left as an idea.
8. Left for after M7 (section 9): a message that must not interrupt a working recipient. A channel message reaches a working recipient at once; a way to hold one is built when a real case appears.

What the choice costs, stated against the plan's own text:

- "The measure" accepts Claude Code's features as they come. This goes further than the plan did: message delivery, part of the dispatcher's core, depends on a mechanism whose flag is named for development and which was described to me in the review as a research preview (I did not open Claude Code's documentation on it). If it changes, the first route changes with it. What keeps the core makiba's own is the fixed line, the box and typed bodies, which is why they are built and tested.
- A channel server per session is another process to start, supervise and give a credential, as the broker's sidecar is (it holds a credential scoped to delivery only).
- The tools tie the agents' rules and skills to tool names instead of `makiba` commands (M6-3), and tie `makibad` to the MCP behaviour of the Claude Code version in use (M0-2's range).

How it differs from claude-org-ja's broker, from its contract (`docs/contracts/backend-interface-contract.md`, Surface 8 and the amendment ratified 2026-06-15; I read both). The broker has a box and pushes through a per-session sidecar that declares the channel capability, loaded with `--dangerously-load-development-channels`; it is not at-most-once at the hand-over. A message is claimed with a lease, pushed, and marked delivered only when the notification "resolved at the transport"; a lease that expires without confirmation goes back to undelivered, so a sidecar that dies in the middle loses nothing. What the broker's "delivered" means, in the contract's words, is "harness-accepted ... not 'visibly rendered to the model'", and the contract leaves the window between accepted and visible to "at-least-once + idempotent display". Two recorded limits: a host-side reject is not reported back to the sender (runtime#81, Surface 8, known limitation 2), and where the sidecar is absent or the peer cannot take channels (it names Codex and environments without a claude.ai login) the fallback is pull, with the contract's own note that its nudge "does NOT wake an idle session", so reception then rests on the agent's own polling. What makiba adds is a confirmation at a later point (the recipient's turn starts, read from the base's status) and a wake that does wake (a typed line), where the broker confirms at acceptance and has only a nudge that does not. P8 measured that a turn start is visible; that it closes the window between accepted and visible is not shown for a recipient that was working.

#### What was measured, and what was not

Run on 2026-11-18 on hc 0.1.21 and Claude Code 2.1.326 (probes.md section 4, with the script commits). The probes are numbered as in probes.md.

| Point | Measured | Not measured |
| --- | --- | --- |
| The development-channel dialog (P10) | Both dialogs, folder-trust and the development-channel one, were passed by the policy with three keys in all; the second dialog's cursor already stood on "1. I am using this for local development". | After a resume or a renewal. |
| A turn start is seen in the status (P8) | From `idle 0` to `working 0` 0.4 s after the message was queued. With the channel stopped, the status stayed `done 1` for 20 s and nothing was acted on; after the channel was resumed the held message arrived. | A delivery to a `done` target that starts a turn; a turn that starts for another cause, to see a false confirmation. |
| A working recipient (P9) | Delivered, and acted on before the long turn ended, at a tool boundary. | A `blocked` recipient; two messages in a row. |
| A request by an MCP tool (P11) | A tool call from the agent reached the stand-in with the probe text. | - |
| The port from a sandboxed Bash (P11) | Refused with errno 111 both with the default proxy variables and with them ignored; the stand-in saw nothing and answered from outside. | - |
| The token (P11) | In the environment: readable from Bash (own environment and sandboxed child processes). In the `--mcp-config` argument: not found. In a file under a denied path: refused (errno 2). Another session's token, in the environment or the argument: not visible. A variable named in `sandbox.credentials.envVars`: gone from Bash. | The environment and arguments of the channel server that Claude Code starts for a session: the recorded cases name the session's own environment and argument. |
| A new dialog from the MCP configuration (P11) | Only the two dialogs above appeared. | - |

Not measured at all, by the owner's answer: whether a channel notification mixes into a person's input (point 3(a): operating experience). The parts above that were not run stay in probes.md as steps to run before the Issue that relies on them lands (M2-4, M3-3, M3-5).

#### Grounds for the door: what was found, what the predecessors had, four options

What was found. In the worker's sandbox on Linux, creating a Unix socket fails at once with `EPERM`, for every path, and the allowlist of socket paths is ignored (probes.md, P1, with the passages read). spec section 3 and M4-2 assumed that one path could be allowed. Nothing else about the base was shown, because every socket was blocked: the claim that opening the door alone keeps the base closed is unmeasured. Whether the filter is present depends on the host.

What the predecessors already had:

- claude-org-ja: workers connect to a resident broker over an HTTP MCP server on the local host, with a bind token per agent and a permission tier fixed when the token is issued. Tools outside the tier are not even listed. The call is made by Claude Code itself, not through Bash, so the sandbox does not apply (contract `docs/contracts/backend-interface-contract.md`, Surface 8, sections 8.2, 8.3, 8.6; ratified 2026-06-14 after a dogfood run). Its two recorded limits: folder-trust approval wiring unverified (ja#566), and a host-side reject of a delivery is not reported back to the sender (runtime#81). The contract also records a later amendment in which messages are pushed through the `claude/channel` feature; this is separate from the tools a worker calls to make a request (below).
- claude-org-ja: a command is taken out of the sandbox with `sandbox.excludedCommands` only as an exact string with no arguments (`git rebase origin/main`, the review gate with no argument), "so the worker cannot choose what runs outside the sandbox". The `docker` entries use a trailing `*`, but docker is there because it cannot run in a sandbox, not as a channel that carries a worker's content out (`tools/org_extension_schema.json`, `$comment_sandbox_hardening`). I found no use of `allowUnixSockets` or `allowAllUnixSockets` in that file.
- paddock: a mailbox of files under the state folder, written by a script the worker runs (decision record 0002; `templates/worker-rules.md`, "Talk through the mailbox"). The sender is the id the caller claims. Its recorded weakness: the write surface is the whole state folder, so a worker that skips the script can write as any id (inventory section 4.1, PRM-11). The narrowing to one file per worker was measured once for Bash and held; the Edit tool was not bounded by it.

The four options, on the same axes:

| | A. Files in a folder per agent | B. MCP tools to `makibad` | C. `makiba` taken out of the sandbox (`excludedCommands`) | D. All Unix sockets opened (`allowAllUnixSockets`), the base's paths hidden |
| --- | --- | --- | --- | --- |
| The line that protects | Sandbox `allowWrite` on that agent's own folder only; `makibad` reads it. The Edit and Write tools are not sandboxed, so `Edit(...)` rules must hold them too (paddock measured the Edit tool getting through). | A token per agent in the MCP configuration, and the tools each role may see. The sandbox is not involved, since Claude Code makes the call. Whether a sandboxed Bash can reach the local HTTP port is not measured. | None inside the command: the command runs with the user's full access. Everything rests on what `makiba` itself refuses to do with the arguments it is given. | The base's folders hidden by `denyRead` and the `hc` deny. Opening all sockets reaches whatever else listens on a path the user can open: a container engine's socket, an ssh agent, the session bus (which `hc` itself tried to use in P1). |
| Reply | Asynchronous: the agent polls a reply file, or the CLI waits with a timeout. | Synchronous: the tool call returns the result. | Synchronous. | Synchronous. |
| Who sent it | Which folder the file is in, set up by `makibad`. Only as strong as the write limit on that folder. | The token. | `MAKIBA_KEY` in the environment, which the process inherits; a caller who unsets it falls to the person-key file, which this unsandboxed process can read. | `MAKIBA_KEY` in the environment, as in the current text of spec section 3. |
| `makibad` stopped | Requests wait as files and are read after the restart. The reply is late, not lost. | The call fails at once and the agent has to try again; nothing is queued unless `makiba` queues it. | The command fails at once. | The connection fails at once. |
| What has to be built and tested | The door becomes a reader of folders; per-agent `allowWrite` in the per-task settings (M3-2, M4-2); a safe reader (no following of links, regular files only, size and count limits). M3-3 changes. | An MCP server in `makibad` (the part of the protocol Claude Code uses), the tokens in the launch arguments, per-role tool lists, the rules and skills naming tools instead of commands, and maybe a startup dialog for the MCP server in the policy list. M3-3, M3-4, M4-1, M6-3 change. | Settings entries, and a hardening of the `makiba` command: it opens files the caller names (`--brief-file`, `--file`) as an unsandboxed process, so it would have to refuse every path argument or read only what it is handed on standard input. Smallest build. | Two settings and the hide list. Smallest build, and the largest reach. |
| Record in the predecessors | paddock's mailbox ran, with the claim-based sender and the weakness above; one-file narrowing measured once. | claude-org-ja ran it through a dogfood and ratified it, with two known limits. | Used only for commands with no arguments, for a different purpose. No record of use for a channel. | None found. |
| Not measured at the first measurements | That a folder-level `allowWrite` holds for Bash on this layout; that `Edit` and `Write` rules hold; the behaviour of links, renames and a full folder against the reader (a test, not a probe). | Measured since by P11 (above): that a sandboxed Bash cannot reach the port, that a token in the launch arguments is not readable, and that no dialog beyond folder-trust and the development-channel one appears. Still not measured: whether a stdio MCP server that Claude Code starts runs outside the Bash sandbox and could carry the door. | Whether wildcard patterns and the forms `makiba` needs (text through standard input, no redirects) stay outside the sandbox; what `env -u MAKIBA_KEY makiba ...` is matched as. | That hiding the base's folders holds with all sockets open (the P1 decoy read could not tell hidden from absent); that no abstract-namespace socket reaches the host. |

How option B relates to "No broker and no channel feature" (section 5, Messages): that sentence was about delivery, a server pushing text into the receiving session through Claude Code's channel feature, which needs its own startup approval. The tools of option B go the other way: the agent calls a tool to make a request, a different part of MCP that needs none of the channel feature. Both are now taken: tools for requests (decision point 1), and a channel for delivery (point 3). The sentence is reversed in section 5.

#### Grounds for delivery: what was found, and the four options

What was found, and what paddock had already measured. paddock's record 0002 (Context) holds two measurements that this plan's sources did not carry: on hc 0.1.20 with a person attached, `hc send` waited 65 seconds and the text was still joined to the half-typed line, and text typed with `hc keys` is not treated as a draft and is joined after 0.25 seconds. Probe P3 on hc 0.1.21 ran twice (probes.md, P3). In `mode=keys` the text joined, which by the same record is the expected result. In `mode=human`, with a person typing `half typed`, the send was held for 20 seconds with the draft intact and then the draft and the wrapped text became one prompt, the send returning after 49 seconds. Whether the wait ran out or the person's Enter caused the join is not separated. So hc 0.1.21 still joins, as hc 0.1.20 did in paddock's measurement. Probe P4 shows that bodies, including `/clear`, `#note` and a closing tag in the body, arrive whole and as one prompt in a terminal nobody types in. paddock's record 0013 describes what it did in answer: a mailbox that is the only place a message lives, and a watcher that types one constant line, only into an idle or done target, after reading the screen's prompt line to skip a pass when a person has typed something.

Whether the reason for leaving 0002's conclusion out (section 7: "makiba has a resident dispatcher that can retry") still stands. In part. A retry fixes what 0002 listed as refusals: a target that is starting, `blocked` or working, where the send fails and nothing was queued. hc 0.1.21 still joins (P3), so a retry does not fix the joining: the text goes into the person's input at the moment of the paste, whenever a draft exists, and a retry only chooses the moment. Only a check that no draft exists, made just before typing, helps, and that check is a read of the screen (0013 reads the prompt line) or a fact the base does not give (whether a person is attached: not measured; I did not find it in what was read). So the reason holds for workers, where nobody types, and does not hold for the secretary, where the person types.

The terminals split: the worker and curator terminals are not typed in by a person; the secretary's is. The options below were written for the case that P3 confirmed, hc 0.1.21 still joining a person's typing (case J). The other case, that hc 0.1.21 waits for the draft and does not join, did not occur.

| | 1. Do not deliver while a person is attached | 2. A box for the secretary, and one constant line typed to wake it | 3. A box for every agent, and one constant line typed to wake each | 4. Type the body, after checking the prompt line for a draft |
| --- | --- | --- | --- | --- |
| What is typed into the terminal | The wrapped body, when no person is attached. | One fixed line ("you have mail: run `makiba inbox`") when the status is `idle` or `done`. | The same fixed line, for each recipient. | The wrapped body. |
| What a joined line costs | None, if attachment is detected. | A short fixed line joins the person's draft: one prompt, recognizable, no message lost, since the body is in the box. | The same, for the secretary; none for workers. | The person's draft and a multi-line body become one prompt, as in P3. |
| Needs a read of the screen | No. | Yes, to skip a pass when a draft is there (paddock's rule), as a stated exception to "Reading the screen" (section 5). | Yes, as option 2 for the secretary. | Yes. |
| Needs something not measured | Whether the base can say that a person is attached (not found). If it cannot, the option cannot be built. | The prompt line check on makiba's terminals; the join cost is the fixed line only. | The same. | The prompt line check; the race between the check and the paste (0013 records it as remaining). |
| What has to be built | A rule in M2-4 and the detection. | A `makiba inbox` verb that returns the stored messages (the `messages` and `event_deliveries` tables already hold them), a constant line in the delivery pass, a prompt-line reader. Messages are delivered when read, not when typed. | The same, for all roles, and every role's rules and skills tell it to read the box. | The prompt-line reader only; the delivery pass is as specified. |
| Fit with the requirements | R1 and R2 hold. A message can wait as long as the person stays attached, and the secretary is usually attached. | Adds one pull step for the secretary; keeps the body out of the composer. Closest to what paddock ran and measured (2026-11-15, host checks 1 to 6, 8 passed). | Uniform, and more machinery than P4 shows to be needed for workers. Closest to claude-org-ja's pull delivery. | Smallest change to this plan; keeps the join risk for the secretary. |
| Record in the predecessors | paddock's 0002 asked for no typing into a person's input and found no way to detect it. | paddock 0013, built and run on the host, including the screen check. | claude-org-ja's pull delivery with a nudge (contract, Surface 8, section 8.4). | The screen check of 0013, which was fixed once (the no-break space) and seen on the host only for the empty prompt. |

What the decision took from these four. It takes option 2's box and fixed line, puts a channel in front of them as the default route for every direction, and keeps typing a body (option 4, without the prompt-line check) as the sender's choice for terminals nobody types in. Option 1 was not taken: the base was not found to say that a person is attached.

## 4. Scope

In: the dispatcher (dispatch, monitoring, close, message delivery, ownership of PR watches); the secretary, workers and curator as terminals of the base; the secretary's skills, rebuilt from paddock's so that they operate no terminal; past incidents as tests; self-hosting until twenty pull requests have landed and three events have been met, then the switch from claude-org-ja.

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
| Terminal base | herdr-core. One module touches the base; one implementation exists. Version pinned to one patch release, which the probes are also run on. The pin moves rarely and on purpose, about once a month at most; each move reruns the compatibility test, both test runs and probes P1, P3, P4, P5 and P8 (M0-2, `docs/upgrading-the-base.md`). No tmux, no renga path, no Orca. | Section 3.1. Two transports side by side caused incidents and most of the explaining. herdr-core releases every few days and calls its API a preview; a range would let in a version no probe has run on. |
| Names | Repository, package and command are `makiba`; the resident process is `makibad` (the binary run as `makiba daemon`, spec section 14); the role keeps the name "dispatcher". | Free on PyPI and npm, and on crates.io (no entry in its index on 2026-11-18). One name avoids a clash with `org` on a machine that also runs paddock or claude-org-ja. |
| Releases | GitHub Releases of this repository (public, so no URL needs a token). A version tag `vX.Y.Z` that equals Cargo.toml's version (a failing job checks) releases `makiba-<version>-<target>.tar.gz` for `linux-x86_64`, `linux-aarch64`, `macos-aarch64` and `macos-x86_64`, a `manifest.json` (`version`, and per target `url` and `sha256`) and `install.sh`. The manifest of a version is `releases/download/v<version>/manifest.json`; the latest is `releases/latest/download/manifest.json`, a redirect that needs no API and no token. `scripts/install.sh` checks the sha256 and places one file. sha256 alone, no signature, in this stage (`docs/installing.md`). Issue 62. | Installation is one file (section 5). The base ships the same shape, so a person who has the base installs makiba the same way. GitHub Releases is a place with no server to run, next to the source. A signature would not stop the one attacker sha256 cannot (whoever can write the release writes the manifest too), and checking one would need another tool on the host. |
| Sources | From claude-org-ja: the records' schema (kept as SQL), the logic of its deterministic tools and hooks (ported to Rust), its contracts (translated). From paddock: the English rules and skills, worker settings, the list of lines to hold, the probes. | paddock has run the full loop on this base and measured it. claude-org-ja has the ledgered records. |
| Language | English for rules, skills, contracts, code, messages between agents, knowledge notes. Two exceptions: what the secretary says to the person follows a setting; pull requests, issues and commits default to English and can be set per project. | The English mirror of claude-org-ja stopped at ja#763 while the Japanese one reached #1279. One source. |
| Use of the base | Only `makibad` talks to it, by JSON-RPC on herdr-core's Unix socket. | herdr-core passes environment variables only through the API. |
| How agents reach `makibad` | Tools of an MCP server in `makibad` on a local HTTP port, one token per agent, in its launch arguments. The person's `makiba` uses a Unix socket and the person key. A folder per agent is the way back. | Section 3.3: on Linux the sandbox allows no Unix socket and ignores a path allowlist (P1); a tool is called by Claude Code, not through the sandbox, and a sandboxed Bash could not reach the port (P11). |
| Models and the base | No model role touches the base, the secretary included. They ask `makibad` through the tools of its door (the person through `makiba`). | herdr-core has no permission boundary. With no role allowed in, there is one line to hold. |
| Rules and settings | Nothing is written into a target repository. Role rules are a file outside the worktree passed at launch; settings are outside too; only a per-task settings file is written. | paddock's way; a worker could not rewrite its settings there (measured). |
| Messages | Kept by makiba, in a box in `makibad` (`messages`, `event_deliveries`) until delivery is confirmed. The default route is a Claude Code channel, in every direction: the body goes straight to the recipient and nothing is typed. A sender may choose `typed` (to a worker or the curator only, when it is `idle` or `done`): the wrapped text is typed. When a push is not confirmed, one fixed line is typed to wake the recipient, which reads its box with the tool `inbox`. Senders use the tools `send` and `report` (the person, `makiba send`). Long bodies are cut and read with `show`. | Section 3.3, with the measurements. This reverses the first choice of this plan, "no broker and no channel feature: one startup dialog fewer, nothing specific to Claude Code", which rested on a resident dispatcher that can hold and retry typed text. P3 showed that typed text still joins a person's draft in the secretary's terminal, and a retry does not prevent that; a channel does not touch the input field (the owner's operating experience, not measured for makiba). What is given up: one more startup dialog, and a first route that exists only for Claude Code, so the box, the fixed line and typed bodies are built as a real route for any other agent program. |
| Who writes `state.db` | `makibad` only. The tools and `makiba` talk to `makibad` and never open the database. Senders are identified by a key per agent, carried as a token in its launch arguments. | A command that wrote directly would need the worker sandbox to allow writes to `state.db`. |
| Claude Code's version | A supported range, from the lowest to the highest version that a recorded probe run covers, checked at `makiba up`. It moves only after probes P1, P3, P4, P5 and P7 to P11, paddock's carried probes and the M4-5 runbook have been rerun on the new version (M0-2, `docs/upgrading-claude-code.md`). | Claude Code is used as it comes ("The measure"), but the screens makiba reads (startup dialogs, permission prompts) and the settings it writes belong to a version, and Claude Code updates itself and is released often. Using it as it comes does not mean not knowing which version was measured. |
| Who is the person at the door | A key in a file; an agent's key is a token in its launch arguments and not in its environment (section 3.3). A request with no key is refused. The person key file is outside what any agent's sandbox may read. | Every agent runs as the person's own user, so the user id and the lineage of a process do not tell a worker from the person: a worker can drop its key, and a process can leave the terminal it was launched in. What an agent cannot do is read a file the sandbox hides. Whether the sandbox does hide it is probe P1 (spec section 3). |
| The base's own messaging and rules | Not used: no parent, no notify, `notes = false`. | One route for messages, one rulebook, one contact. |
| Dispatch | The secretary calls the tool `dispatch`; `makibad` creates the queued run. No model asks another model to dispatch. | The reservation already exists in `state.db`; a request cannot be lost in a conversation. |
| Terminals and runs | A label herdr-core keeps on a terminal is the record. `state.db` holds no terminal address. | Two records drift. After a restart the view is rebuilt from labels. |
| Reading the screen | Three places only: the startup dialog, a worker's permission prompt, and the prompt line of a terminal just before the fixed wake line is typed into it. Option lines are matched whole; the cursor is moved and the screen read again before a key. | A terminal stopped at a startup dialog reports no status (measured in paddock). The prompt line is read so that the fixed line is not typed over a person's input (paddock's watcher, record 0013). Everywhere else status is enough. |
| Retrospectives | What the model dispatcher wrote moves to the secretary. `makibad` only refuses to close before a done report. | A retrospective is judgment. |
| Implementation | Rust, decided by the owner on 2026-11-18. One package that builds one binary, `makiba`, which is the whole installation (spec section 14). The standard library first, then only the crates of section 5.1. No dependency on claude-org-ja or claude-org-runtime: SQL and documents are copied, logic is ported (section 7). | A single binary makes installation simple, and that simplicity decides whether people adopt makiba: claude-org-ja lost users to its installation hurdle (a Python environment, several tools, settings to place). Rust over Go: the Rust toolchain is what this organization already has and has shipped with (renga); states and errors held as types fit a dispatcher whose point is to hold lines by mechanism; and the base is Rust too, so installing and running makiba looks the same as installing and running the base. What Go would have given is the cost of this choice: a wider standard library (an HTTP server, among others, which section 5.1 has to take from a crate or write) and faster builds. |
| Implementation, first choice (reversed 2026-11-18) | Python, standard library first. No dependency on claude-org-ja or claude-org-runtime: needed files are copied. | The carried tools and tests are Python. From the runtime only the dialog matcher is needed. Reversed by the owner on 2026-11-18 for the reasons in the row above; what was merged in Python is handled in section 7. |
| Records and runtime | SQLite (`state.db`), written by `makibad`, makiba's own resident process. SQLite is compiled into the binary (section 5.1), so a host needs no SQLite library and no interpreter. This reverses paddock's choice: paddock keeps its records in herdr-core's document store and per-writer JSON-lines files, with no SQLite (its decision record 0004), and installs nothing beyond what herdr ships. | paddock has no resident process and its readers are herdr mods and the shell, which cannot open a database. makiba has one process with several writers (the door, monitoring, delivery, PR watches) and needs three things a file per writer cannot give: one transaction for "launch verified and run in use" (M3-6), a unique `(source_event_id, recipient)` delivery row (M0-5), and queries across runs, events and deliveries. The schema it carries is SQLite already, and is kept as SQL. What it costs: a herdr mod cannot read the records, so a screen over them (kept in view in section 2) is built on `makiba status` or its own reader; and a machine that runs paddock and makiba holds two record stores in two folders. |
| Starting the base | From a person's login shell or a start script, never from inside an agent; herdr-core in a dedicated session. Session-bound `CLAUDE*` variables and herdr-core's own variables are removed at each launch. | The starter's environment reaches every terminal and can turn transcripts off; `HC_*` reach workers unless removed (both measured in paddock). |
| A stopped worker | The secretary takes it first and answers questions by message. A permission prompt is recorded as a pending entry; the secretary decides or asks the person; `makiba answer` makes `makibad` read the screen again and press one key only if the prompt is unchanged. Workers run in auto permission mode. | Calling a person to a terminal for every stop defeats a resident organization. Secretary-first is claude-org-ja's design; the read-then-press relay passed on a host in paddock. |
| Reaching the secretary from a phone | While the secretary is Claude Code: its Remote Control, on for the secretary only, as an option of the agent module. The mechanism of record stays the pending table and `makiba answer`; stages 2 and 3 are deferred, not dropped. | The model provider's own feature, and it meets R2 as it comes. But it belongs to one agent program, so nothing else may lean on it. |
| Agent differences | One module per agent program: argv, removed variables, dialog policy, the channel, typed delivery, settings, resume. Two exist: Claude Code and the test agent. Codex and the Agent SDK are not built. | Status is a shared protocol (OSC 7501), so the differences collect here, and room for another agent costs nothing now. |

### 5.1 Crates

The standard library comes first. Beyond it, makiba may use the crates below and no other. The list replaces the first choice's "standard library first, no runtime dependency"; it is kept as short as the work allows.

| Crate | For | Why this one |
| --- | --- | --- |
| `serde` (feature `derive`), `serde_json` | JSON: the door's lines, MCP and JSON-RPC bodies, the base's JSON-RPC, settings files. | The standard for JSON in Rust. |
| `rusqlite` (feature `bundled`) | `state.db`. | `bundled` compiles SQLite into the binary, so a host needs no SQLite library. A C compiler is needed to build makiba, not to run it. |
| `httparse` | The agents' door: an MCP server over HTTP on 127.0.0.1 (spec section 3), and the channel server's request to it. | A parser with no dependencies. The server around it is makiba's own, on std's `TcpListener`: the door answers one path, to local clients, which the probe stand-in already did with Python's standard library (probes.md, P11). Not `tiny_http`, which last released 0.12.0 and whose repository was last pushed on 2024-08-14. Not `hyper` or `axum`, which need an async runtime (below). |
| none: std | The JSON-RPC client to the base on its Unix socket. | std's `UnixStream` and `serde_json` do it. A JSON-RPC crate brings an async runtime or a framework for what is one request shape. |
| `clap` (feature `derive`) | Argument parsing for the person's command and the subcommands. | Decided by the owner on 2026-11-18. makiba weighs the commands a person types (`makiba up`, `status`, `config` and the rest) heavily, and clap gives them help text, suggestions for a mistyped verb or option, and the definitions a shell completion is generated from, without makiba writing them; that is the same aim as the choice of Rust itself, a tool that is kind to the person and easy to take up. It is also the parser most Rust programs use: about 290 million downloads in the last 90 days and its latest release, 4.6.8, on 2026-11-16, against lexopt's about 3.4 million, one author, and 0.3.2 on 2026-02-28 (read on crates.io and GitHub on 2026-11-18). `derive`: each verb's arguments are written as a struct beside the code that uses them, and the macro stack it needs (`syn`, `quote`, `proc-macro2`) is already brought by `serde`'s `derive`, so it adds `clap_derive` and `heck` only. Shell completion files need the separate crate `clap_complete`, added by the rule below when they are wanted. What it costs: a larger dependency tree (`clap_builder` and what it uses), a larger binary and a slower build than a parser with no dependencies. Considered and not taken: `lexopt`, which has no dependencies and would have left the help text to makiba's own verb table. |
| `sha2` | SHA-256 of keys (spec section 4) and of prompt text (`prompt_sum`, spec section 5.5). | RustCrypto's implementation. |
| `signal-hook` | SIGTERM, for `makibad`'s clean stop (M2-1). | std has no signal handling. |
| `libc` | The peer credentials of a connection on the person's socket (spec section 3): `SO_PEERCRED` on Linux, `getpeereid` on macOS. | std's `peer_cred` is not stable. |
| `tempfile` (tests only) | Scratch folders and databases in tests. | |

std covers the rest: random keys (read from `/dev/urandom`), the lock on `makibad.lock` (`File::try_lock`, stable since Rust 1.89), file modes, times (stored times are made by SQLite), and the programs makiba runs (`git`, the GitHub CLI, `claude --version`). Errors are enums written by hand; there is no error, logging or HTTP-client crate.

Threads, not an async runtime, and not both. `makibad` has a handful of long-lived jobs (the socket door, the agents' port, the base's event subscription, the pass that delivers messages and checks anomalies, the PR watches) and a handful of connections (one per agent, and the agents are a few), and SQLite through `rusqlite` blocks anyway. A thread per job and per connection fits that, keeps every function an ordinary one, and leaves an async runtime and its dependency tree out of the binary. What it costs: every blocking read needs a timeout set by hand, and a channel server's held request, if M2-8 chooses one, holds a thread while it waits.

Adding a crate. A crate is added by a reviewed commit that adds its row to this table (the crate, what it is for, why std or a crate already listed does not do it), in the pull request that first uses it. A crate in `Cargo.toml` with no row here is a review finding. Crates these depend on are not listed; `Cargo.lock` is committed and CI builds with `--locked`.

Checked on 2026-11-18. Each crate exists in the crates.io index (`index.crates.io`), with its latest version there: `serde` 1.0.229, `serde_json` 1.0.154, `rusqlite` 0.43.0 (it declares Rust 1.89 as its minimum, and has the feature `bundled`), `httparse` 1.10.1, `clap` 4.6.8 (Rust 1.85, with the feature `derive`), `sha2` 0.11.1 (Rust 1.88), `signal-hook` 0.4.6, `libc` 0.2.191, `tempfile` 3.27.0. Whether each is maintained was read from its repository on GitHub, last pushed: `rusqlite` 2026-11-15, `serde_json` 2026-11-16, `sha2` (RustCrypto/hashes) 2026-11-17, `libc` 2026-11-17, `tempfile` 2026-11-13, `signal-hook` 2026-10-31, `clap` 2026-11-16, `httparse` 2026-06-30. The crates.io and docs.rs pages themselves could not be opened from the sandbox this was written in (neither host is on its network list), so release dates were not read there by the writer; the release dates and download counts given for `clap` and `lexopt` in the table were read on crates.io by the secretary.

## 6. One task, end to end

1. **Request.** The secretary calls the tool `dispatch`. A `queued` run and `delegate_sent` are recorded.
2. **Dispatch.** `makibad` finds the run, checks capacity, prepares the worktree, writes the per-task settings, issues the key (a token in the launch arguments), and asks the base to launch. The task text travels in the launch arguments.
3. **Verification.** If a startup dialog shows, it is matched and passed. When the first turn is observed, `worker_spawn_verified` is recorded and the run becomes `in_use`. If it is not observed, the launch is recorded as failed and the secretary is told.
4. **Work.** `makibad` records status and events. An anomaly is told to the secretary once. A permission prompt becomes a pending entry.
5. **Report.** The worker calls the tool `report` with `done`; `makibad` delivers it to the secretary through the channel, who asks the person.
6. **Pull request.** The secretary pushes, opens the PR and calls the tool `pr`. `makibad` holds one watch per PR and head and delivers the result.
7. **Close.** The secretary calls the tool `close`. `makibad` checks the done report, ends the terminal, removes the worktree, closes the run, then checks the curation threshold and forwards triage results.
8. **Unexpected end.** A terminal that ends without a report fails its run and the secretary is told. A kept shell is resumed by conversation id; failing that, a fresh worker gets the original task.

## 7. What is carried

makiba is written in Rust (section 5), so no Python file is copied into what makiba installs. Each carried file is ported to Rust, kept as SQL or as a document, or dropped, as below. A port takes the logic and the cases of the file's tests, not its code.

From claude-org-ja (checked file by file for references to the transport layer):

- `tools/state_db/`: `schema.sql` is kept as SQL and embedded in the binary. From `writer.py` and `queries.py`, what makiba uses is ported (transactions, runs, events, the delivery ledger with its idempotency key, worker folders, the session row, the run queries). Dropped: `snapshotter.py` and the writer's snapshot regeneration and worker-file archive, which write claude-org-ja's `.state/org-state.md`, JSON snapshot and worker files; and the resume-briefing and summary queries, whose readers (claude-org-ja's resume and dashboard) are not carried. No references to the transport layer (M0-5).
- `tools/pending_decisions.py`, `tools/notify_gate.py`, `tools/work_discovery_goals.py`, `tools/work_discovery_dedup.py`: no references. Ported. `pending_decisions` changes its store to a table (M6-4, M2-6, M5-5). The goal ledger keeps its format.
- `tools/work_discovery_scan.py`: capacity is computed per transport. Ported, with that part replaced (M5-5).
- `tools/pr_watch.py`: pushes to the secretary itself and switches transports. Its check reading and conclusion logic are ported; the push and the switch are dropped; it records only (M5-3).
- `tools/acceptance_gate.py`, `tools/acceptance_merge_hook.py`, `.hooks/gate-acceptance-before-merge.sh`: ported, the hook as `makiba hook merge-gate` (M6-6).
- Hooks: `block-git-push.sh`, `block-dangerous-git.sh`, `block-no-verify.sh`, `block-workers-delete.sh`, `check-worker-boundary.sh`, `block-foreground-subagent.sh`, and the payload reader in `.hooks/lib/`: ported as `makiba hook <name>` (M4-1, M4-3). The shell versions need `jq`, and `check-worker-boundary.sh` falls back to `python3`; installed as part of the binary they need neither.
- `docs/contracts/state-semantics-contract.md`, `delegation-lifecycle-contract.md`, `role-contract.md`, and `.dispatcher/references/rationale.md`: kept as documents, translated.
- From claude-org-runtime: `dispatcher/spawn_prompt.py` (`decide` and what it needs): ported (M3-5).

From paddock:

- `templates/secretary.md`, `secretary-rules.md`, `secretary-settings.json`, `worker.md`, `worker-rules.md`, `worker-settings.json`, `curator.md`: kept as documents and settings. The mailbox commands become tool calls of the door.
- Skills that hold judgment: `org-delegate`, `org-escalation`, `org-pull-request`, `org-issue`, `org-update-branch`, `org-curate`. Calls to `hc` become `makiba` commands.
- `docs/design/predecessor-inventory.md` section 6.2: the lines to hold (L-K1 to L-K12, L-N1 to L-N9).
- `scripts/probes/`: kept as scripts a person runs (shell, with `python3` for the stand-ins); a development tool, never installed. Rerun when the Issue that relies on each lands.
- `scripts/second-model-review.sh` and its round cap.
- `docs/decisions/`, except 0002's conclusion ("no `hc send` for messages"). The reason first given for leaving it out, that makiba has a resident dispatcher that can retry and 0002 had none, held only for the refused states (starting, `blocked`, working); it did not hold for the joining of typed text to a person's input, which 0002 measured on hc 0.1.20 and a retry does not prevent, and hc 0.1.21 still joins when a person types (P3). Decided by the owner on 2026-11-18 (section 3.3): the conclusion is carried for the secretary's terminal, where the only text typed is a fixed line and bodies go by channel, and, as the default, for workers; a typed body remains for workers and the curator when the sender chooses it, where nobody types (P4).

What was merged in Python before the switch, decided on 2026-11-18:

- `tools/check_incidents.py` (M0-6, #55) and `tools/check_language.py` (M0-7, #59) stay Python. They are development tools that CI runs over the repository, not part of makiba, and they are never installed; their tests stay `tests/test_check_incidents.py` and `tests/test_check_language.py`. The same holds for `tools/build_issues.py` and `tools/create_issues.py`.
- The Python package skeleton (`pyproject.toml`, `makiba/__init__.py`, #56) is removed by the reworked M0-1, and `makiba/db/*.py` with its Python tests (#58) by the reworked M0-5. Both Issues are reopened.
- `makiba/db/schema.sql` and `makiba/db/schema_v4.sql` are kept, moved to `src/db/`, and become the SQL the Rust code embeds.

Not carried: claude-org-ja's renga path, transport switch, skill templates, `.dispatcher/`, the tools that check whether the model ran a step, composer screen-reading, the cockpit, the Windows layers; paddock's JSON-lines records and core documents, its shell mailbox, its start, stop, suspend, resume, watch and handover scripts (these become `makibad` functions), its mods. From Orca: nothing.

## 8. Milestones

```
M0 foundation -> M1 base module -> M2 monitoring ---------------> M5 close and periphery -> M7 self-hosting
                        -> M3 dispatch -> M4 boundary  -> M6 secretary -----------> M7
```

Before M2 and M3, a person runs probes P1 to P5 and P7 to P11 (section 9). M2 and M3 run in parallel, as do M5 and M6. Through M3 only the test agent is used; real Claude Code starts in M4. The Issues are in [`issues/`](../../issues/index.md).

| Milestone | Done when |
| --- | --- |
| M0 Foundation | The base's version is pinned, an end-to-end test with the test agent runs in CI, and every incident row names a test. |
| M1 Base module | The test agent can be launched, read, typed into and ended, and no event in between is missed. |
| M2 Monitoring | Each scripted scene raises its anomaly once; messages to the secretary arrive once; monitoring survives killing `makibad`. |
| M3 Dispatch | A queued run reaches `in_use` with no person; stopping `makibad` at any step causes neither a second terminal nor a stranded run. |
| M4 Boundary | One real worker goes from request to done report with no person, and the base's command run from inside it is refused. |
| M5 Close | After close, no terminal and no worktree remain; a push replaces the PR watch. |
| M6 Secretary | A request, a PR and a close happen through conversation with the secretary alone; no skill or rules file names a terminal tool or the base's command. |
| M7 Self-hosting | makiba developing makiba until twenty pull requests have landed through makiba and an update of the base, a secretary renewal and a failed launch have each been met (brought about on purpose if need be), incident tests green, claude-org-ja taking no part in carrying out makiba's tasks (Issue 51 says what that means). |

## 9. How it is developed

- makiba is registered as a project in claude-org-ja with the ledger in [`goals/`](../../goals/suisya-systems/makiba.md).
- Merge approval is `acceptance`: the first commit of every pull request is the failing test, declared with an `Acceptance-Test:` trailer.
- makiba is built and checked with Cargo, on the Rust release pinned in `rust-toolchain.toml`: `cargo test --locked` runs the tests, and `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` are the lint (M0-1). An Issue's acceptance test is one Cargo integration test, `tests/<name>.rs`, run as `cargo test --test <name>`; how the checker decides that it failed at the test commit and passes at head, including a test that does not compile there and one that runs no test, is in `docs/acceptance-check.md` (M0-1). Python is used only by the development tools of section 7, which CI runs.
- Probes P1 to P5 and P7 to P11 (probes.md section 2) are run by a person, by hand, on the pinned herdr-core version, before any Issue of M2 or M3 is started. They need no makiba code. P1 decides the door (M3-3) and whether the plan stands; P3 and P4 decide how messages are typed (M2-4); P5 decides how the task reaches the agent (M3-5, M3-6); P7 decides whether the person's own Claude Code settings reach the agents and whether a worker can notify the person (M3-4, M4-3); P8 to P10 decide whether a Claude Code channel can carry messages (section 3.3, M2-4); P11 decides whether the MCP door of section 3.3 is safe (M3-3); P2 decides where real-base tests run and is needed earlier, by M0-3. Results go into probes.md section 4, and no Issue of M2 or M3 is dispatched until every one of them has a row there. P1, P3, P4, P5 and the parts of P8 to P11 named there were run on 2026-11-18; a row that names steps not run keeps those steps for the Issue that relies on them (M2-4, M3-3, M3-5). P6 (Remote Control) waits for M6-2, because nothing depends on it. A probe is rerun when the Issue that relies on it lands and when the pin moves.
- claude-org-ja's workers cannot start a herdr-core inside their sandbox (recorded in paddock's inventory, section 2). Workers test against a stand-in for the base; tests that need the real one run in CI.
- Tests that use a real base always use a place of their own. Nobody's own terminals are touched.
- Three to five workers at a time.
- Issues M4-5, M6-7's host check and all of M7 cannot start from a failing test. They are outside the acceptance gate and are closed by a person.
- Nothing outside the scope is added between M1 and M6. Ideas go into Issues for after M7.
- Left for after M7, recorded and not planned: a check that no rules file tells the person to open a worker's terminal (goal G1, fourth clause), and a mechanism that makes the second reader of a document read before the secretary hands it over (requirement R4 for documents), and a way to send a message that must not interrupt a working recipient (a channel message reaches a working recipient at once, section 3.3). The first target of makiba is code work.

Estimate: 51 Issues and 52 to 62 pull requests. The switch to Rust changed no dependency between Issues; the chains below were recomputed from `tools/build_issues.py` after it and are unchanged. No calendar estimate is given: the earlier one (weeks) had nothing behind it, and what actually limits the schedule can be counted.

- Probes run by the owner. P1 to P5 and P7 to P11 by hand before M2 and M3 (a few hours each, so more than a day of the owner's own time, in a login shell, on a real sandbox and a real Claude Code); P1 again in M4-2; P6 at M6-2. No worker can do these: workers cannot start the base in their sandbox.
- Issues checked by the owner. M4-5 (the first real worker), the host check of M6-7, M7-1 and M7-2 close on the owner's look, not on a test. M7-2 is measured by twenty landed pull requests and three events met (an update of the base, a secretary renewal, a failed launch), not by days; an event that has not come by itself is brought about on purpose, so the time it takes is the time of twenty landings, not a calendar. M0-1 is merged in `human` mode, and the repository, its labels and its branch protection are made by the owner before the rest can be merged test-first. M7-3 and its observation (one pull request landed through makiba for each moved project) come after M7-2 and are not counted here.
- Serial chains. The dependencies are in `tools/build_issues.py`, and each link is at least one pull request (a failing test commit, CI, review rounds, merge) that cannot overlap the one before it. The longest chain to M4-5 is 13 Issues: M0-1, M0-2, M0-3, M1-1, M1-3, M2-1, M3-1, M3-2, M3-4, M3-5, M3-6, M3-7, M4-5. The longest chain to M7-2 is 15: the 13 above with M4-5 last, then M7-1 and M7-2. M7-1 waits for M4-5, M5-2, M6-2, M6-5 and M6-8 as well as M6-6 and M5-5, which together are what the self-hosting needs (one real worker through, the secretary launched and renewed, a permission prompt relayed, a close); goal G9 still asks that every Issue of M0 to M6 be closed by the end.
- Width. The graph has sixteen levels (M7 included) and at most eight Issues on one level, so beyond the three to five workers named above it is a chain, not a lack of workers, that is waited for.

Within those limits the pull-request count is not what decides the time; the owner's availability for the probes and for M4-5 and M7 is.
