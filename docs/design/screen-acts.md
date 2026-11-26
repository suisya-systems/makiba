# The screen acts as the person

Status: design note for Issue 404, 2026-11-27, approved by the person the same day (through the secretary), with the revision of decision 3 in section 3 and with merge approve and refuse excluded (section 1). Nothing is built. It follows the person's answers to the Issue's decisions 1 to 5 (given through the secretary on 2026-11-27: the worker's recommendations, as written). Decision 6, where the screen runs, is not measured yet (probe P17, [probes.md](probes.md)), so this note covers each outcome of [screen.md](screen.md) section 5 and says which choice changes under each. Read beside screen.md sections 4, 5 and 10, [spec.md](spec.md) sections 3, 5.2 and 12, and [lines.md](lines.md).

The line this note keeps: only the person's word counts, and nothing else acts as the person (lines.md; goal G1). A button on the screen does exactly what the person's own `makiba` command does today. It adds no verb, no key, no role and no way around a refusal.

## 1. The person's decisions

| # | Decision | Answer |
| --- | --- | --- |
| 1 | Who holds the person key | (a) The screen runs the person's `makiba` binary for each act. The key stays where `makiba` reads it today (`person_key_path` in `src/cli.rs`). Nothing new holds or reads the key. |
| 2 | How a request from the screen is told from another local program's | (c) A Unix socket readable by the person only, or (a) no page at all, the screen acting through the app, if M1 and M2 pass. Not (b), a page on 127.0.0.1. |
| 3 | The actor recorded | (b) `person`, marked as coming through the screen. |
| 4 | Which acts | A subset. Answer yes or no (with a note). Range pause, resume and end, and range approve with `--read` (which exists since Issue 408). Send, with the screen's fixed sentences only. Close with `--abandon`. |
| 5 | Confirmation and undo | Abandon and approve need a typed confirmation, not a click. End is confirmed by a click. Nothing is undone from the screen, except that pause and resume undo each other. |
| 6 | Where it runs | Not measured: probe P17. Section 3 below takes each outcome. |

Excluded: `makiba merge approve` and `merge refuse` (spec.md section 12, the merge row). The person left them out of the subset for now (2026-11-27). They are added later by an Issue of their own, once P17 shows outcome A (section 3); until then the screen shows no control for them.

## 2. How an act is made

One mechanism, whatever the frame.

1. The screen builds the command as an argument vector, never as a shell line: `["makiba", "--via", "screen", "answer", "14", "yes"]`. The rule of screen.md section 4 holds: the vector carries the verb, numbers, a task id, the repository as `OWNER/NAME` and the screen's own fixed words, never record text (no title, no question, no reason from GitHub or an agent). The one text the person types into the screen, the note of an answer or the reason of a pause, is one element of the vector as typed, never parsed by a shell.
2. The frame (section 3) starts that process as the person's user, outside any agent's sandbox, with the person's environment, and waits for it.
3. `makiba` does what it does today. It reads the person key file, connects to the door's Unix socket (`$XDG_RUNTIME_DIR/makiba/door.sock`) and sends one request line. The only addition is the request field `via` (section 4).
4. The screen shows the command's exit code and its output: `0` done, `5` conflict, `7` stale, and so on (spec.md section 3, the table of error kinds). It retries nothing. A `stale` answer to an approve shows the new read and asks again (screen.md section 4).

What this keeps: the key is read by the same binary, from the same path, under the same sandbox rules as today. The screen's process never holds the key in its own memory. No program gains a way to act that it did not have before: to act as the person it must still run `makiba` with the key file readable, and every agent role is denied that file (lines.md L-K1), and denied making any Unix socket (probe P1, rerun of 2026-11-19: socket creation refused).

The screen keeps one thing in memory beside the ledger: "asked by you N min ago" after "Ask the secretary to look" (screen.md section 4.1). It is a display of its own sends, lost when the screen restarts, and nothing is decided from it. Every other state the screen shows comes from `makiba status --json` and `makiba events --json`. The mark of section 4 is written by `makibad` in the ledger, not kept by the screen.

## 3. Each outcome of the measurement

P17 records, per M, yes or no, and for M2 how the app ran the command: `extension` (an addition of the app ran it), `terminal` (the app typed it into a shell terminal it shows) or `none`. M4 says what can start an addition: only a click in the app, or also a request from another local program.

| Outcome of P17 | The frame | Decision 2 becomes | The buttons |
| --- | --- | --- | --- |
| A. M1 passes in the app (`m1-command`, or `m1-page` framed by the app), M2 passes by `extension`, and M4 says only a click in the app starts an addition | The screen is an addition of herdr-app. It shows the read surface, refreshed, and its control starts `makiba` itself (section 2). | (a): no page and no listener. There is nothing for another program to connect to. | Act, as section 5. |
| A'. M2 passes by `extension`, but M1 fails in the app | The app can run a command but cannot show the status and events it would act on, so the screen could not show the read being approved or the entry being answered. | As D. | Read-only, as D: a button must never act on something the screen did not show. |
| B. M1 and M2 pass as in A, but M4 says another local program can start an addition (a local API of the app, for example) | As A. | (a) holds only if that start cannot be reached by an agent. An agent reaches no base command and no base socket (lines.md L-N2), but an app API that is not the base's would be new. | Read-only until that is shown. The design stops for the person: the M4 paragraph decides. |
| C. M2 passes only by `terminal` | The app can type the line into a shell terminal it shows. | (a) "acts through its terminal". | Read-only (each control shows the line to type, as screen.md section 4). Typing into a terminal the person also types in joins the person's half-typed draft to the line (probe P3, FAIL on 0.1.21; plan.md section 3.3), so a typed line could run as a different command. The screen does not type. |
| D. M2 fails; M3 passes (the page beside the terminal) or neither passes | The page behind `makiba screen` on 127.0.0.1, read-only (screen.md section 7, step 2). | (c) has no client: a page in a browser cannot open a Unix socket, and (b) was ruled out. | Read-only. Each control shows the line to type. |

So under A', B, C and D the person's answers give a read-only screen, and none of the builds of section 7 is worth doing yet. This is where the Issue's options and the platform disagree: option 2(c) names a socket readable by the person only, but the clients that could use it (the app's addition, or a native program) are exactly the ones of outcome A, where no socket is needed, and a page cannot use one. Under every outcome no new listener is built. If the person later wants the page of D to act, that is option 2(b) again, which they ruled out, and it needs its own decision.

What changes between the outcomes is decision 2, whether the buttons act, and how far decisions 3 and 5 reach. Decisions 1 and 4 hold under every outcome: no new holder of the key, and the subset of decision 4 decides which controls show a line or act. Under the read-only outcomes:

- Decision 5 holds on the screen, before the line is shown. The model shows the line of abandon only after the person has typed the task id into the screen, and the line of approve only after the range id (section 5). The person then runs the line in their shell, where the command adds its own step: `makiba range approve <id> --read <read id> --mode <mode>`, shown without `--yes`, prints the draft and the read and asks on the terminal; `makiba close <task id> --abandon` asks nothing more, so the typed id in the screen is its only confirmation.
- Decision 3 does not reach. A line the person runs in their own shell is the person's own command, not an act through the screen, so it carries no `--via` and no mark is recorded. The mark means "the screen started this process" and is set only by the screen of outcome A. This is a revision of decision 3 for the read-only outcomes, accepted by the person on 2026-11-27. The alternative, a line that carries `--via screen`, would mark as the screen's a command the person typed and could also edit.

## 4. The mark: `via`

Decision 3 records `person`, marked as coming through the screen.

- The command: a global option of `makiba`, `--via screen`, valid before any verb. The value is one fixed word; `screen` is the only one. `makiba` sends it as a field of the request line, beside `v`, `key`, `verb`, `args` and `idem`: `{"v": 1, "key": "...", "verb": "answer", "args": {...}, "via": "screen"}`. It is not put in `args`, so a verb's arguments and its `idem` replay are unchanged.
- The door: on the Unix socket, with the person key, `via` absent or `"screen"` is accepted. Any other value is `usage`. On the MCP entrance a tool call has no such field, and an agent's key with `via` on the socket is `usage` too. The mark is never accepted from anyone but the person. A request with `via` and no valid key is `denied` as every keyless request is, recorded with actor `unknown`.
- The ledger: the `command_received` event of that request carries `via` in its payload beside `command_id`. A request without it has no `via` key. `commands` keeps its columns: no migration (spec.md section 5.2). `makiba events --json` then shows which acts came through the screen.

What the mark is and is not: it is the person's own record of where a click came from, written by the person's binary. It is not a check of who called: anything that holds the person key could set it, and the key, not the mark, is what makes a request the person's. An agent cannot set it, because it cannot send the person key.

Option, if the person prefers the mark on the command row itself: a column `commands.via` (TEXT, null by default). That is a schema change (a migration and spec.md section 5.2), so it is not the recommendation. The event payload says the same without one.

## 5. The acts, their confirmation and their undo

The subset of decision 4, each with the vector of section 2 (shown after `makiba --via screen`):

| Control | Vector | Before it is sent | Undo from the screen |
| --- | --- | --- | --- |
| Answer yes / Answer no, with a note | `answer <pending id> yes\|no [--note <text>]` | A click. The note is typed by the person. | None. |
| Hold new tasks | `range pause <range id> --reason <text>` | A click. The reason is asked for; "paused from the screen" when left empty (screen.md section 4). | Start new tasks again. |
| Start new tasks again | `range resume <range id>` | A click. | Hold new tasks. |
| End after running tasks | `range end <range id>` | One confirmation, clicked: "Running tasks go on to their end; no new task starts." | None. |
| Approve exactly this | `range approve <range id> --read <read id> --mode <mode> --yes` | A typed confirmation: the person types the range id shown, and the button stays off until it matches. `--yes` is passed because the screen has shown the draft and the read. | None. |
| Ask for this, Ask the secretary to look, Ask to change it | `send secretary <fixed sentence>` | A click. The sentence is one of the screen's fixed sentences (screen.md sections 4 and 4.1). | None (a message is sent). |
| Abandon and close… | `close <task id> --abandon` | A typed confirmation: the person types the task id shown, under the sentence "The run is abandoned, its work is not landed, and who abandoned it is recorded." | None. |

A typed confirmation is checked by the screen only: the door cannot know it happened, and does not need to, since the request is still the person's own. The screen's model (screen.md section 7, step 1) produces the vector of approve and abandon only after the typed text equals the id. That is what its test asserts.

Approve names the read the screen showed. `makiba range approve <id> --read <read id>` exists since Issue 408 (`src/cli.rs`): the named read goes to the door as named, and a newer read makes the approval `stale` (`tests/range_approve_read.rs`). The approval also fixes the merge approval mode the person was shown (Issue 326): `--mode human|acceptance` sends the mode the screen showed (`merge_approval_mode` of `status --json`), and a mode `makibad` no longer has is `stale`. The screen therefore never sends approve without `--read` and `--mode`, and shows the mode beside the read. screen.md sections 4 and 10 still say the command has no `--read`; they were written before Issue 408 and are out of date on that point.

## 6. The code this would change, and the tests

No code is in this Issue. The build Issues below would change:

| Part | File | Change |
| --- | --- | --- |
| The CLI | `src/cli.rs` | `--via <word>` as a global option, sent as the request's `via` by `ask`. `range approve --read` is there already (Issue 408) and is not changed. |
| The door | `src/daemon/door.rs` | `serve_socket` passes `req["via"]` to `handle`; `handle` refuses a `via` that is not `screen`, or that comes with a key other than the person's, as `usage`; `insert_command` writes `via` into the `command_received` payload. |
| The spec | `docs/design/spec.md` | Section 3: the request line's `via`. Section 10: the `command_received` row's payload, `command_id` and `via` when present. Section 12: `--via`. `src/spec.rs` and `tests/spec_matches_code.rs` are not changed: the payload column of the event table is not one of the facts that test compares. |
| The lines | `docs/design/lines.md` | L-N8 ("every other id speaks only as itself") gains the new door tests below. No new line. |
| The screen | the frame of outcome A (the app's addition) and the model of screen.md section 7, step 1 | The vector for each control and its confirmation. |

No verb and no tool is added, so `tests/door_tools_allowed.rs` and the tool lists of `tests/door.rs` are not changed. No skill and no role rule is changed: an agent does nothing new.

Tests that show another local program cannot act as the person, or set the mark:

- `tests/door.rs`, new: a request on the socket with the person key and `via: "screen"` is done and its `command_received` payload has `via` `screen`; the same request without `via` has no `via` in its payload; `via: "agent"` (any other word) is `usage` and changes nothing; an agent's key with `via` on the socket is `usage`, nothing done; `via` with no key is `denied`, recorded with actor `unknown`, and nothing done. These name what holds: the key decides who acts, and the mark adds nothing to a request without the person key.
- `tests/door.rs`, new: `makiba --via screen answer <id> yes`, run against a test `makibad`, records actor `person` and `via` `screen`. The same command with `MAKIBA_PERSON_KEY` pointing at a missing file is `denied`: running the binary is not enough, the key file is.
- Existing, which hold the other half and are named, not changed: `tests/base_denied.rs::every_role_settings_denies_the_base_the_person_key_and_an_unsandboxed_run` (no agent role can read the person key or run outside its sandbox), `tests/door.rs::the_sender_is_the_id_bound_to_the_key`, and probe P1's rerun (a sandboxed Bash cannot make a Unix socket).
- Existing, kept as regression cover for Approve: `tests/range_approve_read.rs` (Issue 408: a named read that is no longer the newest is `stale` and approves nothing).
- The screen's model test (screen.md section 7, step 1; Issue 169's `tests/screen_model.rs`), extended: for every control of section 5 the vector, built from fixtures whose titles, questions and reasons hold a double quote, a single quote, a backtick and `$(touch <a file>)`, contains none of that text; the vectors of approve and abandon are not produced until the typed text equals the id; every approve vector carries `--read` and `--mode` with the read and the mode the screen showed; no control produces `dispatch`, `merge` or any verb outside the subset of decision 4. For the read-only outcomes, the same model gives the lines to type: the lines of approve and abandon are not given until the typed id matches, the line of approve carries `--read` and `--mode` and not `--yes`, and no line carries `--via`.

## 7. Proposed build Issues

Already done, a prerequisite: `makiba range approve --read` (Issue 408).

1. **P1a. The gated lines to type** (S, under every outcome, with Issue 169's model). The model of screen.md section 7, step 1 gives the lines of approve and abandon only after the typed id matches, the line of approve with `--read` and `--mode` and without `--yes`, and no line with `--via`. Its assertions are those of section 6 for the read-only outcomes. It also brings screen.md sections 4 and 10 up to date on `--read`.
2. **P1b. The mark `--via screen`** (S). `src/cli.rs`, `src/daemon/door.rs`, spec sections 3, 10 and 12, lines.md L-N8, and the door tests of section 6. Only under outcome A: under the other outcomes nothing would set it.
3. **P1c. The screen acts, in the app's addition** (M, after P17 shows outcome A and after Issue 169's model and frame exist). The vectors and confirmations of section 5 in the model and its test, and the addition starting `makiba` (section 2). It needs P1b for the mark.

Under outcomes A', B, C and D only P1a is proposed. P1b and P1c wait, and the screen stays read-only.

Later, by an Issue of its own once P17 shows outcome A: `merge approve` and `merge refuse` on the screen (section 1). The proposal to start from: a typed confirmation of the pull request's number for approve, a click for refuse, and no undo.

Left to the person: under outcome B, the M4 paragraph decides whether the screen may act at all.
