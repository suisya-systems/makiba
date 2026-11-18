# Probes: what is measured, and what is still to measure

A probe is a script run on a real host against the real base (herdr-core) and a real Claude Code. Its result is recorded here with the versions it ran on. Probes are not CI tests: CI cannot run Claude Code.

Order of use: probes P1 to P5 and P7 to P11 are run by a person, by hand, before any Issue of M2 or M3 is started. They need no makiba code, only the base and Claude Code. P3, P4 and P5 decide how messages and the task are delivered (M2-4, M3-5, M3-6), P8 to P10 decide whether a Claude Code channel can be the first route for messages (M2-4), P1 and P11 decide the door (M3-3), and P7 decides what an agent takes from the person's own Claude Code settings (M3-4, M4-3), so a result that comes after those Issues are built costs a rebuild. P2 is needed earlier than the rest, by M0-3. P6 waits for M6-2, because nothing else depends on it. The results go into section 4. Until the rows of P1 to P5 and P7 to P11 are filled in, no Issue of M2 or M3 is dispatched.

Rules for every probe (from herdr-core's own guidance and paddock's practice):

- Run on the herdr-core version that M0-2 pins (0.1.21 at the time of writing), after installing it, and on a Claude Code version inside the range M0-2 sets (paddock measured on 2.1.319 to 2.1.322). A host that still has an older `hc` (paddock measured on 0.1.18) upgrades first: a probe on another version tells nothing about the pinned one. The same goes for the next pin (`docs/upgrading-the-base.md`).
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
| On hc 0.1.18, a send to an agent with half-typed input was joined to it with no separator and submitted as one prompt; it was refused while the target was starting (exit 5), `blocked` (exit 9), or, with `--if idle`, working (exit 10), and nothing was queued. | herdr-core, Claude Code 2.1.319 | decision record 0002, Context | M2-4 |
| On hc 0.1.20 (2026-11-17), with a half-typed line in a client a person had attached, `hc send` blocked for 65 seconds (it waits up to 60 s after the person's last key) and the text was still joined to the line. Text typed with `hc keys` is not treated as a draft: it was joined after 0.25 seconds. So the change in 0.1.20 is that the call waits; the joining did not go away. | herdr-core, Claude Code | decision record 0002, Context | M2-4, plan.md section 3.3 |
| paddock's watcher types one constant line into an idle or `done` secretary or worker that has unread mail, never into a working one, and reads the prompt line of the screen first so that it does not type over input (host check 9: the first version failed, because `❯` is followed by a no-break space and the prompt is not the last screen line; fixed). After the fix the wake of an empty prompt was seen on the host; the skip over typed input was seen only in tests. A turn that starts between the last status read and the paste is still typed into. | herdr-core, Claude Code | decision record 0013, How it wakes and Stage 2 results | M2-4, plan.md section 3.3 |
| A single-file `allowWrite` entry held for Bash (a sibling file and a new file in the same folder were refused as read-only); the Edit tool, which runs outside the OS sandbox, edited the sibling with no refusal. Measured once, on hc 0.1.18 and Claude Code 2.1.322; a glob entry, the Write tool and `Edit(...)` permission rules were not measured. | Claude Code | predecessor inventory, section 4.1 and row L-N8 (`scripts/probes/own-file-write.sh`, `own-file-allow-write.sh`) | plan.md section 3.3 |

## 2. Still to measure

Ordered by how much the plan changes on a bad result. Each takes a few hours. P1 to P5 and P7 to P11 are a gate before M2 and M3 (see the top of this document); P6 can wait until the secretary exists (M6-2).

### P1. From inside a worker's sandbox: the door is reachable; the base, the person key and other terminals' keys are not

Why: the whole design rests on it. paddock could not measure the herdr-core half, because its deny rule refused `hc` before any call reached the sandbox. The door also tells the person from an agent only by the person key (spec section 3), and an agent runs as the person's own user: whether the sandbox hides a file and other processes' environments from a process of the same user is shown only by a real sandbox.

Steps:

1. Start a scratch core. Start a small listener on a Unix socket at the door's path that answers one line and logs the peer credentials (`SO_PEERCRED`: pid, uid) of each connection. Write a decoy file at the person key's default path inside the scratch `XDG_CONFIG_HOME` (`makiba/person.key`, mode 0600, content `PROBE-PERSON-KEY`). In a second terminal outside the sandbox, as the same user, start a process with `PROBE-SECRETARY-KEY` in its environment and `PROBE-SECRETARY-TOKEN` in its argument list (it stands for the secretary; an agent's token is in the argument list, plan.md section 3.3).
2. Launch a worker with the worker settings: sandbox on, the door's path allowed, herdr-core's socket folder and state folder denied, the person key's folder denied for read and write, unsandboxed commands not allowed, no `Bash(hc:*)` deny rule and no hook for this run, so that calls reach the sandbox.
3. Give the worker these exact commands, one Bash call each, and have it report exit code and error text:
   - `python3 -c 'import socket,sys;s=socket.socket(socket.AF_UNIX);s.connect(sys.argv[1]);s.sendall(b"{}\n");print(s.recv(100))' "$MAKIBA_DOOR"`
   - the same against herdr-core's socket path
   - `hc ls`
   - `cat` of a decoy file under herdr-core's state folder
   - `cat` of the decoy person key file
   - the Read tool and the Edit tool on the same decoy file (they run outside the sandbox, and paddock measured the Edit tool as not bounded by `allowWrite`, so only a permission rule can hold them)
   - `grep -a PROBE-SECRETARY-KEY /proc/*/environ`, `grep -a PROBE-SECRETARY-TOKEN /proc/*/cmdline`, and `ls /proc` (what other processes can it see at all)
   - the first command again from a child that forks twice and calls `setsid` before connecting
   - a Bash call that asks to run outside the sandbox
4. Read the listener's log: for the worker's connection and the double-forked one, record the peer pid and uid the listener saw (the real pid, a translated one, or 0). This is evidence for why the door does not use lineage; it is not a pass condition.

Pass: the first succeeds, also from the double-forked child; the next four fail with a sandbox error, not with a deny-rule refusal; the `grep` finds nothing and no other terminal's process is listed (or its environment is unreadable); the run outside the sandbox is refused; the Read and Edit tools on the decoy person key are refused by a permission rule.

If it fails: when the door cannot be reached, the door becomes a spool folder (the agent writes a request file into a folder it may write, `makibad` picks it up) and M3-3 changes. When herdr-core can be reached, M4-2 has no holder and the plan stops for a decision. When the person key file can be read, or another process's key can be found, no key at the door separates an agent from the person; the alternative is to run agents as another operating-system user, with the costs given in spec section 3, and the plan stops for a decision before M3-3.

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit 52d87c1): the door step FAILED, and the probe ran only part of its steps (it covered the door, `HC_TERMINAL_SOCKET`, `hc ls` and the state-folder decoy; not the person key file, `/proc`, the double fork or the unsandboxed run).

- Steps 1 to 3 (the door's socket, the `HC_SOCKET` step, and `HC_TERMINAL_SOCKET`) all failed at the creation of the socket itself: `PermissionError: [Errno 1] Operation not permitted`, before any path was looked at. The door listener's log has 0 lines.
- `hc ls` exited 5: it tried to start a core of its own and failed (`cannot start core: Read-only file system`), so it did not reach the running core and tells nothing about whether the base can be reached. The state-folder decoy read failed with `No such file or directory`, which does not separate "hidden by `denyRead`" from "absent".
- `HC_SOCKET` was unset inside the terminal (the terminal's variable was `HC_TERMINAL_SOCKET`), so that case is not measured.
- Because every socket was refused, the run does not show that the base stays out of reach when the door alone is open. It shows only that no socket can be opened at all.

Cause, read in the sources (not run): sandbox-runtime's README, section "Unix Socket Restrictions (Linux)": "The BPF filter intercepts the `socket()` syscall and blocks creation of `AF_UNIX` sockets by returning `EPERM`." Its table of Unix socket settings gives, for `allowUnixSockets`, "Allowlist of socket paths" on macOS and "_Ignored_ (seccomp can't filter by path)" on Linux, and for `allowAllUnixSockets`, "Allow all sockets" on macOS and "Disable seccomp blocking" on Linux. So the settings file of this probe, which named the door's path, could not work on Linux. Claude Code's sandboxing page says, in its WSL2 note, "set `allowAllUnixSockets`, which opens every Unix socket to sandboxed commands". The README also says that where the seccomp filter is not available "sockets are unrestricted and a warning is shown", and the sandboxing page calls the filter optional; so on another host the same settings can give a different result. I read the first part of each page word for word and the rest of the README only in part; the sandboxing page has no text of its own on `allowUnixSockets`, only a link to it.

What this changes: spec section 3 and M4-2 assumed that a sandbox can allow one socket path. On Linux it cannot. The owner decided on 2026-11-18 (plan.md section 3.3) that agents reach `makibad` by MCP tools, and that the Unix socket stays for the person's own `makiba`. P1's "if it fails" branch above (a spool folder) is option A there, the way back, and not the decision.

What P1 still has to run after the decision. The door step is replaced by P11, which tests the port and the token. The steps P1 did not reach remain, for the base and for the person key file: `hc ls` against the running core, the state-folder decoy read made unambiguous (a path that exists), the person key file, `/proc`, the double fork, the unsandboxed run, and the Read and Edit tools. They are run when M4-2 repeats P1 with the settings `makibad` really writes (M4-2).

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

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit 52d87c1): FAIL, both cases, in `mode=keys`: the script typed the draft with keys as a stand-in for a person, and the send returned at once (rc 0, after about 1 s) with the wrapped text on the screen while the draft was still there; after Enter the draft and the wrapped text were one prompt (`half typed<makiba from="probe" ...>`), and the agent answered that the input looked unfinished. What this shows and what it does not:

1. Measured by paddock (decision record 0002, commit e135de7, 2026-11-17): on hc 0.1.20, with a person attached and a half-typed line, `hc send` waited 65 seconds and the text was still joined.
2. Measured here, on hc 0.1.21, in `mode=keys`: joined. By the same record, text typed with `hc keys` is not treated as a draft, so a join is the expected result of this condition; it says nothing about how 0.1.21 treats a person's typing.
3. Measured later the same day on hc 0.1.21 with Claude Code 2.1.326, in `mode=human` (a person typed `half typed` without Enter): `send-held-20s` PASS (after 20 s the draft was intact, the wrapped text was not typed, the send was still running); `order-after-enter` FAIL: the draft and the wrapped text became one prompt (`half typed<makiba from="probe" kind="message" id="1">`), with the send returning rc 0 after 49 s. The secretary confirmed the joined line with `hc read`. It is not separated whether the join came from the wait running out (`draft_wait`) or from the moment the person pressed Enter. So hc 0.1.21 still joins, as hc 0.1.20 did in paddock's measurement: case J of plan.md section 3.3.

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

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit 52d87c1): all 8 cases arrived with the body equal to what was sent. Six passed by the script. Two (`first-line-bang-ls` and `two-wrapped-in-one-send`) were left as NEEDS-HUMAN, because the script counted 0 prompts where it expected 1; the secretary read the screens and reported 1 prompt in each and no command run. I did not read those two screens again for this record. P4 types into a terminal nobody types in, so it supports typed delivery to workers and the curator, and says nothing about the secretary's terminal. Under the decision of plan.md section 3.3, a channel is the default route and typed bodies are the sender's choice for workers and the curator; P4 shows that typed bodies work for terminals nobody types in, and only a fixed one-line wake is ever typed into the secretary's.

### P5. A task given in the launch arguments starts after the startup dialog

Why: paddock passed the task through herdr-core's template first message, which is tried only at start, so a dialog lost it. makiba passes the task as an argument to the agent.

Steps: in a repository no person has trusted, launch through the base with `argv` ending in the task text. Pass the dialog with the policy. Watch for the first turn.

Pass: the first turn starts on the task without anything sent afterwards.

If it fails: after a dialog, `makibad` delivers the task as the first message (M3-5 and M3-6 change; the brief is already stored in `run_launch`).

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit 52d87c1): PASS. The dialog was answered once and the first turn answered the task given in the launch arguments, 3 seconds later, with no `hc send`. The fallback above is not taken.

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

### P7. What an agent takes from the person's own Claude Code settings, and whether a worker can notify the person

Why: plan.md, "The measure", says the core is makiba's own and nothing outside may change it. Claude Code loads user-level settings (rules in `CLAUDE.md`, hooks, skills, connectors, notification settings) from the person's own configuration folder into every session of that user, and workers and the curator are sessions of that user. Whether launch arguments or a role's own settings can keep them out, and whether a worker can notify the person's phone or desktop, is read in no document of the bundle and measured nowhere (P6 looks at notifications only with Remote Control on, and only for the secretary).

Steps, with a scratch copy of the user-level configuration folder holding decoys, and whatever notification settings the host has turned on (record each by name):

1. Decoys: a rule line (`PROBE-USER-RULE: end every answer with PROBE-USER`), a hook that writes a marker file on every turn, a skill named `probe-user-skill`, a connector entry for a harmless local server that writes a marker when started.
2. Launch a worker the way `makibad` would (role rules as an appended file, per-task settings file), once for each option Claude Code documents for this: nothing special; a choice of which settings sources to load; an alternate configuration folder holding only makiba's files. Record the exact option and `claude --version` for each.
3. In each run ask the agent: "List every rule, skill, hook and connector you can see." Check the marker files. Then make a turn end, a question be asked and a permission prompt appear, and record every notification that reaches anything outside the terminal: phone, desktop, bell.
4. For the secretary, record only which of the decoys apply (it may keep some of them) and whether the choice that isolates a worker can be applied to the secretary separately.

Pass: for at least one option, no decoy applies to a worker or the curator while the role's own rules and settings do, and no stop of a worker or the curator produces a notification outside its terminal.

If it fails: when the decoys cannot be kept out, record which user-level items reach agents and how the role rules and settings rank against them; M3-4 and M4-3 then test the role's rule wins for each, and the plan says which user-level items it tolerates. An alternate configuration folder also moves the login and the connectors, so the cost of sharing them into it is recorded. When a worker can notify the person, the notification setting has to be switched off for workers and the curator by their own settings; if it cannot be, requirement R2 and the second clause of goal G1 cannot be held for Claude Code by launch, and the plan stops for a decision.

Result: PASS for the settings part; PASS with a stated weakness for the notification part.

Run of 2026-11-18 (hc 0.1.21, Claude Code 2.1.327, script `scripts/probes/p7-user-settings.sh` at commit 2d4e8ef; the result file is `p7-result.txt`). I read the `CASE` and `RESULT-OF-RUN` lines and the `EVENT` lines of every run, the `HOST-NOTIFY` lines, and in full the screens and markers of the control run and of the first isolated worker run (`--setting-sources project,local`); I did not read the screens or markers of the other isolated runs line by line, so for them I rely on the script's own `RESULT-OF-RUN` and `CASE` lines. Each run used a scratch home holding the decoys (a rule line, a hook, the skill `probe-user-skill`, the connector `probe-user-conn`), launched with the role's rules as an appended file, a per-task settings file and a role connector.

Settings part, case by case:
- `control-plain`: PASS. With no option all four decoys reach the worker (the rule line was followed by the answer, the skill and the connector were listed, the hook markers were written), and the role's own rule, hook and connector apply too.
- `option-sources`: PASS, and `option-sources-strict`: PASS (`--setting-sources project,local`, and the same with `--strict-mcp-config`). No decoy reaches the worker (rule, skill, hook, connector) while the role's own rule, hook and connector apply. In the first run's listing the user-level rule and the decoy skill and connector are absent and the role's rule is present.
- `option-configdir`: NEEDS-HUMAN. An alternate configuration folder (an empty `settings.json` and a copy of the login file) stopped at the first-run theme screen ("Choose the text style ..."), and no input prompt appeared in 150 s. As set up here it is not usable: the login copy alone does not skip the first-run steps, so using it would need those steps done by hand or other state copied in; that cost is not measured.
- `curator-isolated`: PASS (the curator under `--setting-sources project,local`: no decoy, role items apply).
- `secretary-decoys`: observation only, not judged. Under the plain option all four decoys reach a secretary.
- `secretary-separate`: PASS. The secretary started under `--setting-sources project,local` with its own role items applying and no decoy, while the isolated worker beside it took a fresh turn and still showed no decoy marker. So the option that isolates a worker can be applied to the secretary separately (and, unchanged, the secretary can be left on plain to keep the decoys).
- `real-folder-untouched`: NEEDS-HUMAN from the script, resolved by hand by the secretary: the only files that changed during the run were two files of the skill sync under `skills/synced`, written by other running sessions; `settings.json`, `CLAUDE.md` and the login file were not modified during the run.

Notification part. Host settings found, by key only: `inputNeededNotifEnabled`, `agentPushNotifEnabled` and `remoteControlAtStartup` were on (the result file also lists cached feature flags and counters; their values are not recorded here). In every isolated run (three worker runs, the curator, the secretary under the option) the user-level Notification hook count stayed +0 and the role's own Notification hook fired +1 for each of the turn end, the question and the permission prompt; in the control run both fired. So the user-level notification hook does not reach an isolated worker or curator, while Claude Code still announces each of the three events to the role's own hook. The owner watched and answered (2026-11-18): nothing outside the terminal rang during the isolated worker and curator runs; for the control run the owner is not sure whether anything rang. That is the weakness: the control did not confirm that a notification can reach the owner's devices on this host, so the absence in the isolated runs is weaker evidence than the Pass wording asks for (the Pass wording needs no stop to produce a notification; without a control that rings, silence could also mean that nothing on this host rings at all). The verdict is therefore PASS for the settings part and PASS with that stated weakness for the notification part. A rerun with the owner watching the control would remove it.

What this changes: M3-4 and M4-3 may launch workers and the curator with `--setting-sources project,local` (with `--strict-mcp-config` when connectors are to be kept out too); the alternate configuration folder is not the route unless its first-run cost is measured. The role's own Notification hook, not the user-level one, is what a worker's stop reaches.

### P8 to P11: what the decision of plan.md section 3.3 depends on

These measure what the decision of plan.md section 3.3 depends on: messages carried by a Claude Code channel (`notifications/claude/channel`), the door as MCP tools. They are written as steps; another worker wrote the scripts and the owner ran them. Each is run on the pinned base version and a Claude Code version inside the range of M0-2.

The scripts carry other numbers than these probes. The script `p8` (commit eee6bea) ran the development-channel dialog (P10 below), the turn start (P8) and a working recipient (P9). The script `p9` (commit f03e8d6) ran the MCP door (P11). Both ran on hc 0.1.21 and Claude Code 2.1.326 on 2026-11-18 (2026-11-17T21:34Z and 21:40Z); I read their result files, `p8-result.txt` and `p9-result.txt`, which hold the dialog screens and the cases quoted below, and did not read the scripts.

Not measured, by the owner's answer: whether a channel notification mixes into what a person has typed. The owner's experience of claude-org-ja, which ran the broker and the channel (2026-11-18): no case where a notification was mixed into a person's input. By the owner's reasoning, a channel is a notification to Claude Code and not keystrokes into a terminal, so it does not depend on the base. This is operating experience, not a measurement made for makiba, and it is recorded as that.

### P8. A channel delivery to an idle target is seen as a turn starting, in the base's status

Why: under the decision, `makibad` pushes the body through the channel and treats the message as delivered only when the recipient's turn starts, read from the base's status. If that cannot be read, the decision cannot tell a lost push from a delivered one.

Steps:

1. Launch Claude Code through the base with the channel flag and a small stand-in channel server that emits one notification on command (record the exact flag and `claude --version`).
2. Wait until the target is `idle`, then `done` (after a turn); for each, emit one notification and record from `terminal.get` and the event stream: whether the status goes to `working` or the turn counter increases, after how long, and in which order.
3. Emit a notification that Claude Code does not act on (for example one it ignores) and record whether any status change appears; and let a turn start for an unrelated reason (a typed line) to see what a false confirmation looks like.

Pass: for `idle` and for `done`, a turn start is visible in the status within a few seconds, and no change appears when nothing was delivered.

Result: PASS, in two parts.

First part (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit eee6bea): the idle target. The status went from `idle 0` to `working 0` 0.4 s after the message was queued, and the agent wrote the file it was asked for. With the channel stopped, the status stayed `done 1` for 20 s and no file appeared; after the channel was resumed the held message was delivered.

Second part (2026-11-18, hc 0.1.21, Claude Code 2.1.327, script `p8` at commits cc4d6d0 (run 3, 11:19Z), 3ea2681 (run 4, 11:27Z) and 8aa6d4a (run 5, 11:34Z)); I read the three result files, screens included. Two earlier runs of the same day (dc50021 and 0a201b4) did not measure anything usable (the target sat in `blocked 1`, or the case was not exercised) and are not counted. All steps are now run; run 5 ended with 9 of 9 cases PASS (`SUMMARY p8: 0 failing case(s)`); in run 3 one case was NEEDS-HUMAN and in run 4 one case was FAIL, both named under P9, and the other eight were PASS in both.
- The idle target again: `idle 0` to `working 0` after 0.4 s (run 3), 0.4 s (run 4) and 0.7 s (run 5); with the channel stopped the status stayed `done 1` in 10 readable samples over 20 s and no file appeared, in all three runs, and the held message was delivered after the channel was resumed.
- A notification to a `done` target: `done 4` to `working 4` after 0.4 s in each of runs 3, 4 and 5, and the agent wrote the file it was asked for.
- A turn that starts for another cause while the stand-in holds its message: a typed line (`hc send`) took the status from `done 7` to `working 7` after 0.3 s in each of the three runs, while the held message was not delivered (the held message, PROBE-MSG 7, appeared on the screen only after that turn, as a turn of its own). A status change by itself therefore cannot tell a delivery from another turn start: the confirmation of plan.md section 3.3, point 5 (the recipient's turn starts, read from the status) is a heuristic, and a turn that starts for another cause looks the same as a delivery.

If it fails: delivery cannot be confirmed from status. Every delivery gets the fixed one-line wake typed after the push (paddock's form, record 0013), and the recipient reads the box with a tool.

### P9. A channel delivery to a target that is working, blocked or done

Why: workers are often mid-turn. Whether a notification arriving then is delivered at the end of the turn, shown at once, or lost decides whether `makibad` may push to a busy target.

Steps: for a target in each of `working` (a long tool call), `blocked` (a permission prompt) and `done`, emit a notification and record: when the text appears in the conversation (during the turn, at its end, after the prompt is answered, never), whether it is shown once, and what the status does. Repeat with two notifications in a row.

Pass: each notification appears once, at the latest at the end of the turn, and a lost one is visible through P8's check.

Result: PASS on what each case shows, with one case where what the agent did with the text differs between runs.

First run (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit eee6bea): `working` only. A message sent during a long turn was delivered and acted on before the turn ended: it interrupted the turn at a tool boundary. The result file notes that this means delivered, not lost, and that it interrupts. The status during the working case is not in that result file, so P8's check could not be applied to it (plan.md section 3.3, point 5 covers it by waking after the turn).

Second runs (2026-11-18, hc 0.1.21, Claude Code 2.1.327, script commits cc4d6d0 (run 3), 3ea2681 (run 4), 8aa6d4a (run 5); the result files are the source for what follows). All cases are now run.
- `working`: in each of runs 3, 4 and 5 the message arrived after the third of twenty single writes, the screen shows its header once, and it was never lost. The fixture differs from the first run: twenty single Write calls, one at a time, where the first run used a long sleep; I do not explain the difference beyond that. The first run had recorded "acted on before the turn ended"; here the text was shown at a tool boundary in every run and what the agent did with it differs. Run 3: the agent acted on it after the turn's work (it wrote `recv-3.txt` after `busy-20.txt`, before its final reply). Runs 4 and 5: it did not act on it and said why in its final reply. Run 4, in the result file's own words (Japanese, in the `while-working` screen of `run4-p8-result.txt`), in my English translation: "PROBE-MSG 3 arrived during the work, but it was notified as a message from an external channel and not an instruction from the user, so I did not carry it out." Run 5 (`run5-p8-result.txt`), in my translation: "it came through an external channel and not from the user, so I treated it as such and did not carry it out." The script counted run 4 as a failure ("lost?") because the file was absent; its screen shows the message shown once and declined, so run 4 is not a lost message, and run 5's script (8aa6d4a) reports it as shown once and not acted on.
- What this means, and no more: delivered and shown is not the same as acted on. A channel text can be declined as not coming from the user, which is the behaviour plan.md section 3.3 point 4 already names as the reason typed delivery stays available.
- `blocked` (a permission prompt): in all three runs the text appeared while the prompt was open, 2.1 s after the message was queued and before the prompt was answered, once (one header in the final history), and the status read `blocked 11` throughout the 20 s window and `done 12` at the end. After the answer, the agent acted on it in runs 4 and 5 (`recv-5.txt` complete); in run 3 it did not (`recv-5.txt` absent). Run 3 was not a valid case: the probe's own instruction told the agent to use no other tool, and the agent said so (it left PROBE-MSG 5 undone because of that instruction); its verdict was NEEDS-HUMAN. In runs 4 and 5 the instruction was changed to say that text arriving in a channel tag is an instruction too.
- `done`: the text appeared once and the agent wrote the file, in all three runs (from `done 6`).
- Two notifications in a row: each appeared once and both files were complete, in all three runs; the order on the screen was 8a before 8b, as sent.

If it fails: `makibad` pushes only to an `idle` or `done` target and holds the message otherwise, as typed delivery does today (spec section 7); a notification that is lost without any status change makes P8's check the only guard and the fixed wake the rule.

### P10. The development-channel dialog can be passed by the policy list on the base

Why: the contract of claude-org-ja records a prompt, `Load development channel? (Y/n)`, that appears for the channel sidecar in addition to the folder-trust prompt, and was machine-approved by the orchestrator there. Under the decision `makibad` passes it with the policy list (spec section 7), a startup dialog the plan did not have.

Steps: in a folder no person has trusted, launch through the base with the channel flag and with the task as the last argument (as P5). Record every dialog's full text and option lines, the order, and whether each is passed by choosing a line from the policy list with the cursor read again before the key (spec section 7). Launch again with a resume, and renew the secretary, and record whether either dialog comes back.

Pass: every dialog is matched whole and passed by the policy list, the first turn starts on the task, and a resume or renewal passes the same way.

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit eee6bea): PASS for a first launch. Two dialogs appeared, in this order, and the policy passed both with three keys in all (down and Enter on the folder-trust dialog, Enter on the second). Folder-trust: the cursor stood on "No, exit" and the line "Yes, I trust this folder" was chosen. The second began "WARNING: Loading development channels", said that `--dangerously-load-development-channels` is for local channel development only and to use `--channels` for approved channels, named the channel (`Channels: server:makiba-channel`), and offered "1. I am using this for local development" (the cursor already on it) and "2. Exit". The result files hold both screens. Not run: a resume, and a renewal of the secretary.

If it fails: for the agent where it fails, no channel; messages go by the box and the fixed wake, and the plan says which roles that is.

### P11. An MCP door: the HTTP port and the token are out of a sandboxed worker's reach, and no new dialog appears

Why: option B of plan.md section 3.3 has agents call `makibad` through MCP tools. Three points were left unmeasured: whether a sandboxed Bash can reach the daemon's local HTTP port, whether the token can be read, and whether launching with an MCP configuration raises a dialog.

Steps:

1. Start a stand-in daemon on a local port that answers one line, and launch a worker with the sandbox on and the MCP configuration naming it (record the exact option). Give the worker one Bash call each: a connection to that port from Python; the same through the sandbox's network proxy rules; a read of the MCP configuration file; a read of the claude process's command line and environment under `/proc`.
2. Record every dialog shown at launch, with its full text, and whether an MCP tool call from the agent reaches the stand-in.

Pass: the tool call reaches the stand-in; the Bash connection to the port fails with a sandbox error; the token appears in neither the readable files nor `/proc` of any process the worker can see; no dialog beyond those already on the policy list appears.

Result (2026-11-18, hc 0.1.21, Claude Code 2.1.326, script commit f03e8d6): PASS for the token in the launch arguments; the same token in the environment FAILS, as the decision expects (plan.md section 3.3, point 2).

- A tool call from the agent reached the stand-in with the probe text. The stand-in's log shows the calls of Claude Code 2.1.326 with a valid token.
- A connection to the port from a sandboxed Bash failed with `ConnectionRefusedError`, errno 111, both with the default proxy variables and with them ignored. Nothing reached the stand-in, and it answered from outside.
- A decoy token in the environment: found by the Bash scan, in the Bash process's own environment and in its children's. A decoy in the `--mcp-config` argument: not found. A decoy file under a path denied for reading: refused, errno 2. The other session's decoys, in its environment and its argument: not found. A decoy named in `sandbox.credentials.envVars`: not found in Bash's environment.
- Dialogs: only the two of P10. The MCP configuration added none.
- Not examined by the recorded cases: the environment and arguments of the channel server that Claude Code starts for a session; the decoys were in the session's own environment and argument.

If it fails: when the port can be reached from Bash, the token alone protects the daemon, which is weaker than the plan states, and option A is the way back; when the token can be read, it moves to a file in a folder the worker's settings deny, or option A; when a new dialog appears, it is added to the policy list if it can be matched whole, and otherwise option A.

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

## 4. Results

Filled in by the person who runs each probe, with the script's commit. A row is empty until the probe has been run on the pinned base version. Times are as the probe scripts wrote them (UTC): the first runs were at 2026-11-17T15:56Z to 15:57Z, which is 2026-11-18 in the owner's time zone. P6 has not been run, and P10 and P11 were run in part. P1 to P5 and P7 to P11 must have a row with a result before any Issue of M2 or M3 is dispatched, and a row for a probe run in part names the steps not run, which are run before the Issue that relies on them lands; a failed row names the Issue it changes (see each probe's "If it fails"). The same table is extended when a probe is rerun for a new pin or for the Issue that relies on it.

| Probe | Date | `hc --version` | `claude --version` | Result | Notes |
| --- | --- | --- | --- | --- | --- |
| P1 | 2026-11-18 | 0.1.21 | 2.1.326 | FAIL (door); base-side steps not shown | `socket()` refused for every Unix socket; plan.md 3.3 |
| P2 | 2026-11-18 | 0.1.21 | not used | PASS (by this repository's own job, not the scratch repository) | P2 as written asks for a scratch repository; the scratch-repository form of the probe was not run. What was run instead is this repository's required `real-base` job (`.github/workflows/ci.yml`): it installs the pinned herdr-core with `scripts/install-base.sh` on `ubuntu-latest`, runs the whole suite with `MAKIBA_TEST_REAL_BASE=1` and `--test-threads=1`, and has a negative step that fails unless a missing `hc` makes the fixture panic. As reported by CI, the workflow `CI` (which contains `real-base`) concluded `success` on main in more than three runs in a row on 2026-11-18 (UTC): 44234425183 (ca992dc), 44236167002 (5ff95e6), 44236720657 (33ed9f2), 44237531927 (fd40dd6), 44238851906 (2410b99), 44252564682 (3a4e97b); the job on PR 86 also reported success. The earlier note about `continue-on-error: true` no longer applies: since PR 83 (Issue 81) the job is a required check without it. |
| P3 | 2026-11-18 | 0.1.21 | 2.1.326 | FAIL (keys and human) | keys: joined, as expected. human: held 20 s with the draft intact, then draft and wrapper became one prompt (send rc 0 after 49 s); whether the wait ran out or the person's Enter caused the join is not separated |
| P4 | 2026-11-18 | 0.1.21 | 2.1.326 | PASS (8 of 8; 2 read by the secretary) | typed bodies arrive whole; not tested on a terminal a person types in |
| P5 | 2026-11-18 | 0.1.21 | 2.1.326 | PASS | dialog answered once, first turn on the task in 3 s |
| P6 | | | | | |
| P7 | 2026-11-18 | 0.1.21 | 2.1.327 | PASS (settings); PASS with a stated weakness (notifications) | `--setting-sources project,local`, with and without `--strict-mcp-config`: no decoy (rule, skill, hook, connector) reaches a worker or the curator, and the role's own rule, hook and connector apply; with no option all four reach the worker and the secretary. The alternate configuration folder stops at the first-run theme screen (NEEDS-HUMAN, not usable as is). The option can be applied to the secretary separately. Real folder: only two `skills/synced` files changed, by other sessions. Notifications: in isolated runs the user-level Notification hook +0 and the role's +1 for turn end, question and permission prompt; nothing outside the terminal rang (owner), but the owner is not sure the control rang, so the control did not confirm that a notification can reach the owner's devices here. A rerun with the owner watching the control removes this. Script commit 2d4e8ef |
| P8 | 2026-11-18 | 0.1.21 | 2.1.327 (first run 2.1.326) | PASS (9 of 9 cases in run 5) | idle target: 0.4 s (runs 3, 4), 0.7 s (run 5) from queue to `working`; `done` target: 0.4 s in all three; with the channel stopped, no change in 20 s, then the held message arrived; a typed line started a turn in 0.3 s while the held message was not delivered, so a status change alone cannot tell a delivery from another turn start. Nothing left unrun. Runs 3 to 5 on Claude Code 2.1.327. Script commits cc4d6d0, 3ea2681, 8aa6d4a (run 5); first run eee6bea (2.1.326) |
| P9 | 2026-11-18 | 0.1.21 | 2.1.327 (first run 2.1.326) | PASS (every case run; the working target differs between runs) | working: shown once at a tool boundary in runs 3 to 5, never lost; acted on after the turn in run 3, declined as not from the user in runs 4 and 5; blocked: shown once while the prompt was open (2.1 s), acted on after the answer in runs 4 and 5 (run 3 not valid); done, and two in a row: each once, in order. Nothing left unrun. Script commits cc4d6d0, 3ea2681, 8aa6d4a (run 5), Claude Code 2.1.327; first run eee6bea (2.1.326) |
| P10 | 2026-11-18 | 0.1.21 | 2.1.326 | PASS (first launch) | folder-trust and development-channel dialogs passed with three keys. Not run: resume, renewal. Script commit eee6bea |
| P11 | 2026-11-18 | 0.1.21 | 2.1.326 | PASS (token in the launch arguments); the token in the environment FAILS | tool call reached the stand-in; port refused from Bash (errno 111, two ways); argument and denied file not readable; env readable. Not examined: the channel server's own environment and arguments. Script commit f03e8d6 |
