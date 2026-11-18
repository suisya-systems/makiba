# Host probes P1, P3, P4, P5

Scripts for the probes of `docs/design/probes.md` section 2 that can run before any makiba code exists. A person runs them on a real host, from a login shell, never from inside an agent or CI. Nothing here has been run yet: the scripts were written in a sandbox that cannot start herdr-core.

Not here: **P2** (needs a GitHub repository, see probes.md) and **P6** (needs a secretary, M6-2).

## Before running

- Needs `bash`, `python3`, `git`, `claude` (logged in) and `hc`. Each script takes the base binary from `HC_BIN` (default `hc`), prints `hc --version` and `claude --version` first, and records them in the result file.
- The design reads herdr-core 0.1.21. If `hc` on PATH is older, install 0.1.21 beside it, without replacing the one you use, and point the probe at it: `HC_BIN=/path/to/hc-0.1.21 bash scripts/probes/p3-send-waits-for-draft.sh ./results`. Do not run `hc update` on the binary you use: it replaces it (`hc update --version X|--from <file>` is in `hc --help`). How to obtain a separate 0.1.21 binary was not checked here; any way that leaves your own `hc` in place will do, and the script refuses P3 below 0.1.20 either way.
- Every run uses its own core: `HC_SESSION=makiba-probe-<pid>`, scratch `XDG_CONFIG_HOME` and `XDG_STATE_HOME`, and stops only that core at the end, also on failure. The variables of spec section 6.4 are removed first. No real secret is used; P1 writes a decoy.
- Claude Code's own trust record in `~/.claude.json` is shared (as with any accepted dialog): P3, P4 and P5 add entries for their scratch repositories.
- Each script takes one argument, the directory for the result file `<probe>-result.txt` (versions, date, script commit, one line per case, captured screen text on failure). Every case prints `CASE <name>: PASS|FAIL|NEEDS-HUMAN`. Paste the result file into probes.md section 1 with the versions.

## P1 `p1-door-reach.sh <dir>`

Runs a small Unix-socket listener at a door path, builds worker settings (sandbox on, door allowed, the scratch core's socket and state folders denied, no deny rule and no hook for `hc`), launches a worker and has it run five commands, one Bash call each: connect to the door, connect to each of the base's socket paths, `hc ls`, `cat` of a decoy under the state folder. The commands live in `c1.sh` to `c5.sh` in the worker folder and write their own exit code and output to `report.txt`.

Pass: `door-reachable` PASS, and `base-socket-*`, `hc-ls`, `state-decoy-read` PASS (each failed with a sandbox-style error, not a deny-rule refusal). NEEDS-HUMAN means the error text was not recognised or the call never ran: read the captured screen.

## P3 `p3-send-waits-for-draft.sh <dir>`

Refuses to run (exit 3, `REFUSED`) on hc below 0.1.20. Types `half typed` without Enter, sends the wrapped message from a background shell, looks at the screen after 20 s, presses Enter, and checks that `half typed` is one prompt and the wrapped text the next.

- Default `P3_MODE=keys`: the draft is typed with `hc keys`. This **stands in for a person**; whether the base counts keys as a person's typing is unmeasured, so a PASS here is weaker than a human run. The result file says so.
- `P3_MODE=human`: the script prints an `hc attach` command with the right session; you type `half typed` in it, press Enter in the script's shell, and later press Enter in the attached terminal when asked.

Pass: `send-held-20s` PASS and `order-after-enter` PASS.

## P4 `p4-delivery-text.sh <dir>`

Eight cases to one idle Claude Code (acceptEdits mode, in a scratch repository): a 12-line body, a 4000-character body, first lines `/clear`, `!ls`, `#note`, a body with the closing tag (escaped as in spec section 8), two wrapped messages in one send, two sends back to back. The agent is told to write each body to `echo-<id>.txt`; the script compares the file with what was sent. "No line acts as a command" is inferred from that file coming out whole; "one prompt" is checked by counting prompt lines that start with the wrapper in the screen history (more than expected is a FAIL, fewer is NEEDS-HUMAN).

Pass: all eight PASS. A failing case is a shape that must never be typed (M2-4). The "terminal read as `done` that starts a turn in the same second" case is approximated by the two quick sends.

## P5 `p5-launch-task.sh <dir>`

Fresh git repository nobody trusted; `claude` launched through the base with the task as its last argument; the trust dialog is passed by the folder-trust policy (cursor moved onto a Yes line, screen read again, one key); `hc send` is never called. Pass: the reply to the task appears (`task-after-dialog` PASS). NEEDS-HUMAN with "no trust dialog" means the folder was already trusted and the case did not run.

## P8 `p8-channel-delivery.sh <dir>` and P9 `p9-mcp-from-sandbox.sh <dir>` (channel candidate)

Measure the design's new first candidate: an agent asks a resident program through an MCP tool, and the program delivers text through a Claude Code channel (`notifications/claude/channel`), typing into the terminal only as a fallback. `makibad` does not exist, so `channel-standin.py` (python3 standard library, no import from claude-org-runtime) plays it: an HTTP MCP server on 127.0.0.1 (`send_request` tool, bearer token) and a stdio channel server. Message shapes were read from claude-org-runtime (`broker/server.py`, `broker/channel_sidecar.py`, `broker/tokens.py`) and launch arguments from `claude-org-ja/.dispatcher/references/spawn-flow.md` 3-2 / 3-3b: `--mcp-config <json|file>` carrying the http server and the stdio channel server, plus `--dangerously-load-development-channels server:makiba-channel`. `dialog-step.py` is the startup-dialog policy (option text matched whole, cursor moved, screen read again, one key); dialog screens are written to the result file.

P8 cases: `dev-channel-dialog`, `turn-start-visible` (seconds from enqueue to a state or turn change in `hc get --json`; paired with `turn-start-unchanged-without-delivery`, where the stand-in holds the message back), `channel-while-working` (a message sent in the middle of a `sleep 45` turn: PASS means delivered, the detail says whether it acted before or after the turn's end; FAIL means lost). `channel-to-done` (one notification to a target that reads `done`: seconds to a state or turn change), `channel-while-done` (the text appears once), `turn-start-for-another-cause` (stand-in paused, a typed line starts the turn: PASS means the turn start was seen and the held message was not delivered, so status alone cannot tell the two apart), `two-in-a-row` (two notifications back to back: each once, which first), `channel-while-blocked` (a permission prompt opened by a command the agent settings do not allow; a notification is sent while the base reads `blocked`, the prompt is answered with the one-time yes only afterwards; records when the text appeared, how often, and the status). Cases that cannot decide end in NEEDS-HUMAN with the screen text. `channel-while-draft` is not measured: the owner reports that channel text never mixed into a person's draft in claude-org-ja.

P9 cases (sandbox on, no network allowance; a second agent runs beside the sandboxed one): `mcp-tool-reaches` (the tool call arrives); `bash-http-direct` (urllib with `ProxyHandler({})`) and `bash-http-via-default-proxy` (urllib defaults, proxy variables honoured), each recording one line `type= errno= reason= msg= proxyvars=` (PASS = blocked with a network-style error, nothing logged by the stand-in, listener alive from outside; FAIL = it got through, the token is then the only guard); `own-token-env`, `own-token-argv`, `own-token-file` (this session's decoys: env variable, `--mcp-config` argv, a file under a `denyRead` path; PASS = Bash cannot find or read it); `other-session-token` (the second agent's env and argv decoys searched in every readable environ and cmdline; the heavier risk); `credential-envvar-unset` (a decoy variable listed in `sandbox.credentials.envVars` with mode deny: is it gone from Bash's own environ?). Locations are reported, never values.

Notes: P4 counts submitted prompts without the live composer (the 2026-11-18 zero counts came from a grey suggestion in the composer).

## TODO(confirm)

Lines that use an `hc` or Claude Code feature not shown in paddock's measured scripts or `hc --help`, each marked `TODO(confirm)` in the source:

- The composer's prompt marker `❯` (P3 order check and P4 prompt count; without it the result is NEEDS-HUMAN, never PASS).
- `hc keys` key name `space` (P3 draft typing).
- A second `hc send` while the first turn is starting is queued and not lost (P4 quick case).
- Settings key `sandbox.network.allowUnixSockets` for letting a sandboxed command connect to one socket, and `filesystem.allowWrite` for the socket's folder (P1).
- That the core's API socket sits in the folder of `HC_SOCKET` or `HC_TERMINAL_SOCKET` (P1; the runtime folder `$XDG_RUNTIME_DIR/herdr-core` is denied as well).
- Used as in paddock but with hc 0.1.18 only: `hc run --bg --no-parent -- env -u ... claude ...` (argv form after `--`), `hc read --lines N`, `hc send --wait --timeout N`.
- P8 new cases: the base's state is spelled `blocked` at a permission prompt; the channel text is visible in `hc read` history (counted by its `PROBE-MSG <id>:` prefix); the cursor line of a permission prompt carries `Yes` and `hc keys enter` answers it once; `hc keys escape` declines a prompt; `python3 -c` is not allowed by `Bash(sleep:*)` and so opens a prompt.
- P8/P9: where `turn` sits in `hc get --json` (searched anywhere by name); that Claude Code accepts the stand-in's minimal MCP replies (shapes copied from claude-org-runtime, whose servers are known to work); the exact wording of the development-channel dialog (the policy matches "I am using this for local development" and "Exit" as in claude-org-runtime's spawn_prompt.py; an unknown dialog gives NEEDS-HUMAN with its text); that the MCP tool is named `mcp__makiba__send_request`; that an inline JSON `--mcp-config` is accepted (runtime launcher does this).
