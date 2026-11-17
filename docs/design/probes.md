# Probes: what is measured, and what is still to measure

A probe is a script run on a real host against the real base (herdr-core) and a real Claude Code. Its result is recorded here with the versions it ran on. Probes are not CI tests: CI cannot run Claude Code.

Rules for every probe (from herdr-core's own guidance and paddock's practice):

- Run from a person's login shell, never from inside an agent.
- Use a dedicated session and scratch folders, and stop only that core at the end, never the one a person works in:

  ```sh
  export HC_SESSION=makiba-probe-$$
  export XDG_CONFIG_HOME=$(mktemp -d) XDG_STATE_HOME=$(mktemp -d) XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/tmp}
  # ... the probe ...
  hc server stop
  ```

- Record the base's version (`hc --version`), `claude --version`, the date, and the script's commit.
- A probe never uses a real secret. Where it needs a secret-looking file it writes a decoy.

## 1. Measured in paddock, relied on here

Measured with hc 0.1.18 and Claude Code 2.1.319 to 2.1.322. Each carried probe is run again when the Issue that relies on it lands.

| Fact | About | Where it was measured (paddock) | Used by |
| --- | --- | --- | --- |
| Ending the secretary's terminal and launching a fresh one that resumes from a handover works, and workers survive it. | Claude Code | `scripts/probes/secretary-renewal.sh`, Q16 | M6-5 |
| `claude --continue` resumes the conversation whose transcript was written last, not a chosen one. | Claude Code | `scripts/probes/continue-multi-conv.sh`, Q4 | M5-6: resume by conversation id |
| A worker's `hc ls` is refused by the settings deny rule. `HC_*` variables reach a worker unless removed; with them removed, status still reaches the core. | herdr-core | `scripts/probes/core-api-reach.sh`, inventory L-N2 | M3-4, M4-1 |
| Trust of a parent folder does not cover a git worktree; the repository's own trust does. A terminal stopped at the trust dialog reports no status. | Claude Code | `scripts/probes/folder-trust-cases.sh`, Q13 | M3-2, M3-5 |
| The environment of whatever started the core reaches every terminal; an inherited `CLAUDE_CODE_CHILD_SESSION` turns transcripts off. | Claude Code | inventory P7, `scripts/probes/worker-env-audit.sh` | M4-4, M3-4 |
| `done` is reported once per turn, after the `Stop` hooks let the turn end. | Claude Code | inventory P3 | M2-2 |
| A worker cannot rewrite its settings files or write outside its worktree. | Claude Code | `scripts/probes/settings-rewrite.sh`, `write-surface.sh` | M4-3 |
| A permission prompt can be answered by reading the screen again and pressing one key. | Claude Code | `scripts/probes/worker-prompt-relay.sh` | M6-8 |

## 2. Still to measure

Ordered by how much the plan changes on a bad result. Each takes a few hours. P1 and P2 come before any M3 or M4 work is merged. P6 can wait until the secretary exists (M6-2).

### P1. From inside a worker's sandbox: the door is reachable, the base is not

Why: the whole design rests on it. paddock could not measure the herdr-core half, because its deny rule refused `hc` before any call reached the sandbox.

Steps:

1. Start a scratch core. Start a small listener on a Unix socket at the door's path that answers one line.
2. Launch a worker with the worker settings: sandbox on, the door's path allowed, herdr-core's socket folder and state folder denied, no `Bash(hc:*)` deny rule and no hook for this run, so that calls reach the sandbox.
3. Give the worker these exact commands, one Bash call each, and have it report exit code and error text:
   - `python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(b"{}\n");print(s.recv(100))' "$MAKIBA_DOOR"`
   - the same against herdr-core's socket path
   - `hc ls`
   - `cat` of a decoy file under herdr-core's state folder

Pass: the first succeeds; the other three fail with a sandbox error, not with a deny-rule refusal.

If it fails: when the door cannot be reached, the door becomes a spool folder (the agent writes a request file into a folder it may write, `makibad` picks it up) and M3-3 changes. When herdr-core can be reached, M4-2 has no holder and the plan stops for a decision.

### P2. The real base and the test agent run in GitHub's CI

Why: workers cannot start a core in their sandbox, so CI is the only place real-core tests run.

Steps: in a scratch repository, a workflow that installs the pinned herdr-core (`hc` with its own session), launches a script that prints OSC 7501 records, and asserts the base reports the status.

Pass: green on `ubuntu-latest` three times in a row.

If it fails: real-core tests run on a person's machine, and the acceptance gate works on stand-in tests only. M0-3 says so.

### P3. A send waits for a person's unfinished typing

Why: messages are delivered by typing. paddock measured on herdr-core 0.1.18 that a send was joined to half-typed input; 0.1.20's changelog says sends now wait for an unfinished draft.

Steps:

1. Launch Claude Code in a scratch core. Attach, and type `half typed` without Enter.
2. From another shell, with `hc send <t> ...`: `'<makiba from="probe" kind="message" id="1">\nPROBE-BODY\n</makiba>'`.
3. Record what the send returns and when. After 20 s, press Enter in the attached terminal.

Pass: the send does not go in while the draft is there; after Enter, `half typed` is one prompt and the wrapped text the next.

If it fails: `makibad` does not deliver to a terminal a person is attached to, and holds the messages (a rule added to M2-4; whether an attached client can be detected is then the next thing to measure).

### P4. Delivery text survives as typed

Why: the wrapper of spec section 8 is typed into a real composer.

Cases, each sent to an idle Claude Code, with the agent asked to repeat what it received:

- a body of 12 lines
- a body of 4000 characters
- a body whose first line is `/clear`, one whose first line is `!ls`, one whose first line is `#note`
- a body containing `</makiba>`
- two wrapped messages in one send
- a send to a terminal read as `done` that starts a turn within the same second (send twice quickly)

Pass: every body arrives whole, as one prompt, and no line acts as a command.

If a case fails: that shape is never typed. Its body is replaced by the cut line and read with `makiba show` (M2-4).

### P5. A task given in the launch arguments starts after the startup dialog

Why: paddock passed the task through herdr-core's template first message, which is tried only at start, so a dialog lost it. makiba passes the task as an argument to the agent.

Steps: in a repository no person has trusted, launch through the base with `argv` ending in the task text. Pass the dialog with the policy. Watch for the first turn.

Pass: the first turn starts on the task without anything sent afterwards.

If it fails: after a dialog, `makibad` delivers the task as the first message (M3-5 and M3-6 change; the brief is already stored in `run_launch`).

### P6. The secretary can be reached from a phone through Remote Control

Why: plan.md section 2 uses Claude Code's Remote Control as a shortcut for stages 2 and 3. Read in Claude Code's documentation on 2026-11-18: the Claude app drives a session that runs on the person's machine, and the session can push notifications when a task ends or a decision is needed. Not run.

Steps, on a host with the Claude app signed in to the same account:

1. Launch the secretary through herdr-core with Remote Control on, in the way the agent module would (record the exact option and the `claude --version`). Launch one worker without it.
2. From the phone: find the secretary's session, send a request, read the reply.
3. Make a pending decision exist (a worker's permission prompt relayed to the secretary). Record whether the phone is notified, after how long, and whether an answer typed on the phone reaches `makiba answer` through the secretary.
4. Check that the worker's session is not listed on the phone and sends no notification.
5. Renew the secretary (end its terminal, launch a fresh one that resumes). Record whether the phone follows to the new session or has to be connected again.
6. Close every attached terminal on the host and repeat step 2, to see that no local client is needed.

Pass: steps 2, 4 and 6 work. Steps 3 and 5 are recorded as they are: how reliably a pending decision notifies decides how soon stage 2 is needed.

If it fails: `secretary.remote_control` stays off and stages 2 and 3 come sooner. Nothing else changes, because nothing else depends on it.

## 3. If a move to Orca is considered

Not part of this plan's work. herdr-core is the base (plan.md section 3.1), and Orca's orchestration is not used whatever these checks say. They are kept for the day one of the reasons listed there applies, and they concern Orca only as a base: its terminals, its status, its notifications.

What follows was read in Orca's repository on 2026-11-17 (`skill-guides/orca-cli.md`, `skill-guides/orchestration.md` and its references, `src/main/agent-hooks/README.md`, `src/shared/notifications/`); none of it was run. Each check replaces a reading with a measurement. The first two are requirement R2 and R1 themselves.

Setup for all: install Orca on the machine that will be the server; start `orca serve` there (or the desktop app, noting which); pair the phone app. On Linux outside an Orca terminal the command may be `orca-ide`, since `orca` can resolve to the GNOME screen reader.

### O1. Only the secretary notifies (R2)

Read: notifications have two sources, an agent's turn ending (sent to the phone as "needs input" for `blocked`, `waiting`, `needs-input`, and as "finished" for `done`) and the terminal bell. The phone has a filter for "only when an agent needs input". Nothing read says a single terminal, agent or worktree can be muted.

Steps, with the phone paired and the desktop app closed:

1. Find and record every notification setting: per source, per agent, per worktree, per terminal, and the phone's own filters.
2. Start one agent to stand for the secretary and two to stand for workers, by the launch a makiba on Orca would use (`agent launch` or `terminal create --command`).
3. Make each worker stop in turn: end a turn with a question, wait at a permission prompt, finish. Then make the secretary end a turn with a question.

Pass: with some recorded setting, no worker stop reaches the phone and the secretary's does. Record whether a reply from the phone reaches the secretary.

If only "all or nothing" exists: record that. Orca's notifications are then turned off and makiba rings the bell in the secretary's terminal itself; check that a bell alone reaches the phone.

### O2. A worker's permission prompt is settled without the person touching the worker (R1)

Read: the coordinator receives a worker's own `ask` and an `escalation` when a worker stops without `worker_done`. Nothing in the orchestration guide mentions permission prompts.

Steps:

1. Start a worker in the permission mode makiba would use (Claude Code's auto mode). Give it ten varied tasks from real work. Count the permission prompts that still appear and record their text.
2. For a prompt that appears: from outside the worker, `orca terminal read --terminal <h> --screen`, then answer it with `orca terminal send`. Record what the screen read returns and whether the key lands.

Pass: either no prompt appears in step 1, or step 2 works every time.

### O3. Orca's rules can be kept out of agents

Read: Orca installs per-agent hooks ("hooks turned off" is a supported case, with OSC 7501 as the fallback) and ships skills, among them `orchestration`, that agents load with `orca skills get`.

Steps:

1. Find and record every switch: managed hooks per agent, managed or shared skills, anything Orca writes into `~/.claude/` or the worktree.
2. Turn off what can be turned off. Launch Claude Code with `agent launch` (or `terminal create --command`) in a scratch worktree, with makiba-style rules in an appended system prompt file.
3. Ask the agent: "List every skill and slash command you can see, and every hook configured for you." Then give it a task whose natural wording invites Orca's orchestration ("have another agent review this and report back").

Pass: no Orca skill is listed, no Orca hook runs unless makiba chose it, and the agent does not start an Orca worker. Record what had to be turned off and whether an Orca update turns it back on (check after one update).

### O4. A worker cannot drive Orca

Read: agents in Orca's terminals may run the `orca` CLI; the guide's `runtime_access_denied` case says a sandbox can block the connection.

Steps: as probe P1 above, with Orca in herdr-core's place. From inside a sandboxed worker with no deny rule in the way: `orca status --json`, `orca terminal list --json`, a direct connection to Orca's runtime socket, a read of Orca's state folder. Record the socket and folder paths and every environment variable Orca sets in its terminals.

Pass: all four fail with a sandbox error, while a connection to the door (P1) succeeds.

### O5. The low-level commands are enough

For each point of spec.md section 6.2 that says "not found" or "not known" in Orca's column, find the answer and record it:

- Can a terminal be launched with given environment variables set, and with given ones unset?
- Can a terminal carry a tag (or a title that nothing else rewrites) that is still there after Orca restarts and that `terminal list --json` or `agent list --json` returns?
- Is there a way to follow status changes and terminal exits without polling? If not, how long does `agent list --json` take with ten terminals?
- Does `terminal send` accept a condition on status, and does it wait for a person's unfinished typing (see P3)?
- Can a real Orca run in GitHub's CI (see P2)?

Pass: launch with environment, a durable tag or an acceptable substitute, and status within 2 s of a change by subscription or by polling.

A move is worth considering only if all five pass. O1 or O3 failing means Orca's notifications or rules cannot be kept away from workers, and the move is off. Record the results here with versions and dates, and repeat O1 and O3 after an Orca update, since an update may switch either back on.
