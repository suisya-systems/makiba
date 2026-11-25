# Calling the person back (Issue 272): design

Status: design for Issue 272, 2026-11-26, on origin/main 4060c8f7b22e59fdf71ef14f312bf1a6bec4505e. Option A (section 2) approved by the person, and the revised design approved for the test commit; built. Revised after an outside reading of the full text (findings: majors 1 to 5 and minors 1 to 9, section 10).

## 1. What is asked, and what the records already say

- Issue 272: fed by makibad's pending entries, sent as the secretary's call and not a worker's (one contact); a design for the channel, what is said, how it is silenced. Done when a pending entry reaches a person who is not looking at the screen.
- plan.md R2: notices to the person come only from the secretary and the pending table. Section 2: "the dispatcher ... can own sending notifications"; "stage 2 adds a recipient to the same ledger".
- spec.md section 10 declares the carried kinds `notify_sent` and `notify_failed`; nothing in makiba writes them today. In claude-org-ja they record notices from the dispatcher to the secretary; here they are written only with `to` `person` (section 4), a recipient outside the delivery ledger (`event_deliveries`), because the person has no box and no terminal. spec.md section 10 and plan.md section 2 each get one line saying so.
- spec.md section 11 says `secretary_gone` and `secretary_still` are told to the person "until stage 2 adds a way to notify". They are anomalies, not pending entries, and `secretary_still` is exactly the case where the secretary is not acting, so a call for them cannot be "the secretary's call". Left out (follow-up, section 9).

Where the Issue's text and the code disagree:
- The Issue's "Held for later ... after the screen of #169": the screen is not built; the owner put this Issue back, so this design does not wait for it and does not touch the screen.
- "A pending entry": not every entry waits for the person. An `open` entry is the secretary's to answer (spec 5.5; screen.md 4: "Entries open ... are not the person's yet"). screen.md counts only `escalated` as "needs you", while spec 5.5 and 12 say a `merge` entry (kept `open`) waits for the person's key. This design takes both: what waits for the person is (a) every entry with status `escalated` (escalated by the secretary with `pending escalate`, or a stop makibad wrote escalated at once, belt.md 5, which is also told to the secretary) and (b) every entry of kind `merge` with status `open` (the secretary asked for the merge; it waits for `makiba merge approve`). A worker's own question or prompt is never called until the secretary escalates it: that is what keeps one contact.

## 2. The channel: a command the person names

Option A (approved by the person): the person sets one organization setting, `notify_command`, the absolute path of an executable. makibad runs it with no shell as `<path> <title>` (one argument: the title, which holds only a count), in a process group of its own, with:
- stdin: a file holding the body (`<state>/tmp/call/<n>.in`, mode 0600), so the body is not in `/proc/<pid>/cmdline`, which other local users may read, and writing it can never block;
- stdout: /dev/null;
- stderr: a file (`<state>/tmp/call/<n>.err`, mode 0600), never a pipe, so nothing makibad reads can block on a grandchild holding it or on a full pipe;
- makibad's own environment.

The folder `<state>/tmp/call/` is made with mode 0700; a call's two files are removed once its event is written.

makiba ships no OS-specific sender: the person's script reads the body from stdin and calls `notify-send` (which plays the desktop's sound), a Windows toast through `powershell.exe` under WSL, `osascript` on macOS, or a push service with `curl`. This note carries such example scripts (section 11). A sound is whatever the person's sender makes.

- Option B, a built-in push over HTTP (ntfy or similar): needs a network host for makibad, a service choice and a secret; rejected for the first version.
- Option C, Claude Code's push notification from the secretary (Remote Control, plan.md section 2): Claude decides when to push, it is one agent program's alone, and probe P6 is not run; the plan keeps it a shortcut, not the mechanism. Rejected.

The setting: `makiba config set notify_command <path>` (organization's, no `--project`). `config` is the person's only: the door refuses the verb to every other role, and it has no tool. Stored as `config_set` with `key` `notify_command`. `config get notify_command` returns the stored value (null when unset).

### 2.1 Which paths are refused

makibad runs outside every sandbox, so a script an agent can write would be run with the person's rights. The rule is conservative and needs no parsing:

1. Empty is off and always accepted.
2. Otherwise the text starts with `/`, holds no NUL, newline or carriage return, and is at most 4096 bytes.
3. The path is resolved (`canonicalize`: links followed, and it must exist) and the resolved file is a regular file.
4. The resolved file is not under any place an agent writes, each also resolved: the state folder (`<state>`, which holds every launch's `tmp/<id>` and the secretary's folder), the worktrees (`$MAKIBA_WORKERS_DIR`), the document folders (`$MAKIBA_DOCS_DIR`), the repositories (`$MAKIBA_REPOS_DIR`), the knowledge folder (`$MAKIBA_KNOWLEDGE_DIR`), and the system's temporary folders (`/tmp`, `/private/tmp`, `/var/tmp` and makibad's `std::env::temp_dir()`), which hold the sandbox's own writable `/tmp/claude`.
5. The file and every folder above it up to `/` are owned by the user makibad runs as or by root, and none is writable by group or others; a folder with the sticky bit set may be writable by others (only its owners may rename or remove its entries).

At `config set` a path that breaks 2 to 5 is `usage`, with the rule it broke; nothing is stored. Before every call the same checks 3 to 5 are made again on the stored path; a path that breaks one is not run, and the call is a `notify_failed` with `reason` `unsafe: <the rule broken>` (so a file replaced or a folder opened after the set is never run).

## 3. What is said

One call may carry several entries, taken oldest first (by pending id).

- Title (the argument): `makiba secretary: <n> waiting for you` (`<n>` the number of entries in this call).
- Body (stdin), one line per entry, at most 5 lines, then `and <k> more` when more were taken:
  - an escalated entry: `#<id> <the first line of its question, cut after 120 characters with "...">`
  - a merge entry: `#<id> a merge waits for your key: makiba merge approve <id>`
- Last line of the body: `Answer in the secretary's conversation, or see makiba status.`

Order of the work on each question, so nothing is cut before it is masked: (1) the whole question is masked with the read surface's mask (src/status_local.rs `Mask`: the person key, the gate key, any agent key whose SHA-256 `agents` holds become `<key>`); (2) every control character (NUL, tab, carriage return and the rest of Unicode's `Cc`) becomes a space, except newline; (3) its first line is taken; (4) it is cut after 120 characters. stderr is read from its file, masked whole, control characters other than newline replaced, and only then cut to 500 bytes (at a character boundary). The event stores the masked text only. The call opens no answer route: it says that something waits; the answer still goes through the secretary, `makiba answer` or `makiba merge approve`.

## 4. When a call is made, and how it is silenced

A pass runs every tick of makibad's loop, beside `exe_watch`, not behind the reconcile gate: once makibad has started (its base answered `hello` and the loop runs), the call does not wait for the base's records. It reads `state.db` only.

1. `notify_command` is the value of the newest `config_set` with key `notify_command` and project `''`; its event id is the setting's id. Unset or empty: no new call starts (off).
2. While a call's process runs, or while a finished call's event is not yet written, nothing new starts (one call at a time).
3. An entry is due when it waits for the person (section 1), no `notify_sent` with `to` `person` names it in `pending_ids`, fewer than 3 `notify_failed` with `to` `person` and an event id greater than the setting's id name it, and the newest of those failures, if any, is at least `RETRY` old (`MAKIBA_CALL_RETRY_MS`, default 60000). So a failure counts only against the setting it was made with: setting the command again (the same value, a new one, or after off) gives every entry that still waits three tries again.
4. When at least one entry is due and the last call started at least `GAP` ago (`MAKIBA_CALL_GAP_MS`, default 30000; in memory, so a restart may call sooner), the path is checked (2.1) and one process is started with every due entry. The entries are read again in the same read as the check, so an entry answered or withdrawn by then is not in the call.
5. The process is polled with `try_wait` each tick; makibad never blocks on it. Exit status 0: `notify_sent` (actor `makibad`; payload `to` `person`, `pending_ids` ascending, `command`, `title`, `body`). Any other exit, a signal, a start that fails, a path refused before the start, or no exit within `TIMEOUT` (`MAKIBA_CALL_TIMEOUT_MS`, default 10000): `notify_failed` (same `to`, `pending_ids`, `command`, and `reason`: `exit <n>`, `signal <n>`, `timeout`, `spawn: <error>` or `unsafe: <rule>`; `stderr`, masked and cut as in section 3) and one line to makibad's log. At `TIMEOUT` the whole process group is killed (`kill(-pgid, SIGKILL)`) and the direct child reaped. After a direct child exits, makibad does not wait for anything it left behind (stderr is a file, so nothing holds the tick). After the third failure for an entry under the current setting, the log says that entry will not be called again until the command is set again.
6. A finished call's result is kept in memory until its transaction commits; a refused write is tried again on the next tick, and no new call starts meanwhile.

Silencing:
- Off: `makiba config set notify_command ""`. Entries that become due while it is off are not lost: when it is set again those that still wait are called together in one call.
- No nagging: an entry is called at most once when the call succeeds; an entry answered, withdrawn, or no longer waiting before its call starts is not called.
- Bursts are folded by `GAP`: what becomes due inside it goes in the next call.
- Quiet hours or a snooze are not built (follow-up).

## 5. Which module owns it

New module `src/daemon/call.rs`, "calling the person". Read before deciding:
- `delivery.rs` owns messages to agents: a push through a channel, confirmation by a turn start, the box and the wake line as fallbacks. The person has no channel, no terminal and no box; none of that applies, so adding the person there would be a second protocol inside it.
- `notify_gate.rs` owns whether a notice goes to the secretary; it already classifies `secretary_gone` and `secretary_still` as `person_only` and sends nothing. The call is not a notice to an agent, so it is not put there; the call's selection is SQL over `pending` and `events`.
- Nothing owns "telling the person" today (spec section 11: the log and `makiba status`). The call module becomes that one owner; the follow-up for `secretary_gone` extends it rather than adding another.

State in memory beside the ledger: only (a) the one call in flight (its process, pending ids, title, body, start time) or its finished result not yet written, and (b) the start time of the last call (for `GAP`). Ordering: a call's event is written in one transaction after its process ends; nothing is written at its start. At a stop of makibad (beside `drop(prwatch)` at the loop's end), a call still running has its process group killed and its child reaped, and nothing is recorded; a finished result not yet written is tried once more. Restart rule: entries of a call that was not recorded are due again after the restart, so the person may be called twice for them (at least once, as delivery). `GAP` starts over at a restart. Whether an entry was called is read from the events each tick, never kept as a latch.

## 6. Files changed

New: `src/daemon/call.rs`; `tests/person_call.rs` (the declared test); `docs/design/person-call.md` (this design, with the example scripts).

Existing:
- `src/daemon/mod.rs`: the module, its pass in the tick beside `exe_watch`, and its stop at the loop's end.
- `src/daemon/door.rs`: `CONFIG_KEYS` gains `notify_command` (6 to 7) and its get and set with the rule of 2.1 (the check itself lives in `call.rs`, one function used by the set and by each call).
- `src/status_local.rs`: `Mask` with `new` and `text` become `pub(crate)` so the call masks as the read surface does. No change of behaviour.
- `src/cli.rs`: the doc line of `Config` names the setting (help text only).
- `docs/design/spec.md`: section 2, a paragraph for the three test seams (`MAKIBA_CALL_GAP_MS`, `MAKIBA_CALL_RETRY_MS`, `MAKIBA_CALL_TIMEOUT_MS`) and the folder `<state>/tmp/call/` (prose, not the table); section 10, a paragraph after the carried list (not inside it) for `notify_sent` and `notify_failed` with `to` `person`, a recipient outside the delivery ledger (the event table and its count unchanged, so `src/spec.rs` and `tests/spec_matches_code.rs` are not edited); section 12, the `config` row's description names `notify_command` and its rule (the verb column unchanged). Section 11's "until stage 2" remarks stay (not built here).
- `docs/design/plan.md`: section 2, the row "Terminal events go through a delivery ledger" gets one sentence: the person's call (Issue 272) is recorded as events with `to` `person`, outside that ledger.
- `docs/design/not-carried.md`: N3's mark becomes `build` (the owner withdrew "later" for this Issue, 2026-11-26) and its "Nearest in makiba" names the call (Issue 272); the Counts table (lines 263 to 266) moves N3 from `later` to `build` (build 3 to 4, later 9 to 8).
- Not edited: `docs/design/screen.md`.

Not changed: no door verb or tool is added, so `tests/door.rs` and `tests/door_tools_allowed.rs` are not edited; no schema change (events and `config_set` exist); no skill and no role rule; nothing under `.github/`, no script, not the merge gate nor the acceptance checker; no existing test; nothing under `tests/common`.

Kinds of file the diff touches: Rust code, one new Rust test, Markdown under docs/design.

## 7. The declared test: tests/person_call.rs

Setup, once per process before any thread: `MAKIBA_CALL_GAP_MS=300`, `MAKIBA_CALL_RETRY_MS=200`, `MAKIBA_CALL_TIMEOUT_MS=3000`, and `MAKIBA_WORKERS_DIR`, `MAKIBA_DOCS_DIR`, `MAKIBA_REPOS_DIR` set to folders of a temporary root of the test (the knowledge folder as `common::makibad` gives it), so the scripts' folder is under none of them wherever the test runs. Each test runs the daemon in this process on `Local` (as tests/belt_park.rs: `makibad::run_until`, stopped by a flag) on its own state folder, with a run `t1` (status `in_use`, no launch). The secretary and the person act through a `Door` of their own (`issue_key` for `secretary` and `person`; the person's `config` with `tool` false). The scripts are made with `tests/common/exe.rs` (`exe::write`) in a folder made under `env!("CARGO_TARGET_TMPDIR")` (not under `/tmp`, refused by 2.1). The recording script writes each call's title to `call-<n>.title` and its stdin to `call-<n>.body` in a folder of the test. Every wait polls up to 20 s; "stays" waits 2 s (more than six `GAP`s).

1. `an_escalated_entry_calls_the_person_once_and_a_later_one_alone`
   - The secretary adds A (open, question `qa`) and B (question `qb` with the five parts) for `t1` and escalates B; then the person sets `notify_command` to the recording script.
   - Exactly one call appears; its title is `makiba secretary: 1 waiting for you`; its body's first line is `#<B> qb`; the body does not contain `#<A>`; its last line is `Answer in the secretary's conversation, or see makiba status.`
   - Exactly one `notify_sent` event; payload `to` == `person`, `pending_ids` == `[B]`, `command` == the script's path, `title` and `body` equal what the script received.
   - Then C is escalated (the command still set): exactly two calls in all; the second's body's first line is `#<C> qc` and it does not contain `#<B>`; two `notify_sent`, the second with `pending_ids` == `[C]`.
   - It stays two calls and two `notify_sent` after 2 s; the daemon is stopped and started again, and it stays two after 2 s more.
2. `entries_waiting_together_are_one_call`
   - B and C escalated before `notify_command` is set: exactly one call, title `makiba secretary: 2 waiting for you`, body lines `#<B> qb` then `#<C> qc`; one `notify_sent` with `pending_ids` == `[B, C]`.
   - Seven entries escalated while the command is off (set to `""`), then the command set again, give one call whose title says `7`, whose body has five entry lines then the line `and 2 more`, and whose `notify_sent` names all seven.
   - A question of 200 characters is cut: its line is `#<id> ` + its first 120 characters + `...`.
   - A question holding a NUL and a tab: the call is made (one `notify_sent`) and its line holds a space in their places.
3. `a_merge_waiting_for_the_key_calls_with_its_command`
   - A `pending` row inserted with kind `merge`, status `open`: one call whose body's first line is `#<M> a merge waits for your key: makiba merge approve <M>`.
4. `unsetting_the_command_silences_and_setting_it_calls_what_still_waits`
   - Command set, then set to `""` (answer `value` == `""`); B escalated: no call and no `notify_sent` after 2 s.
   - D escalated and then withdrawn, and E escalated and then answered, while the command is off.
   - The command set again: one call naming `#<B>`, one `notify_sent` with `pending_ids` == `[B]`; no `notify_sent` names D or E.
5. `a_failing_command_is_tried_three_times_until_it_is_set_again`
   - The command exits 3: exactly three `notify_failed` events name B, each with `to` `person` and `reason` == `exit 3`; it stays three after 2 s; no `notify_sent`.
   - The person sets the recording script: one call naming `#<B>`, one `notify_sent` with `pending_ids` == `[B]`.
6. `keys_are_masked_before_anything_is_cut`
   - B's question is 100 characters followed by the secretary's key (so the key spans character 120): the call's body contains neither the key nor its first 20 characters, and the `notify_sent` payload does not contain them either.
   - A command that writes the secretary's key to stderr, after 490 other bytes, and exits 1: its `notify_failed` has a `stderr` that contains neither the key nor its first 10 characters.
7. `only_the_person_sets_a_safe_command`
   - `config get notify_command` by the person returns null before any set.
   - The secretary's `config set notify_command <script>` is `denied`.
   - The person's set is `usage` for: `relative/path`; a value with a newline; a path that does not exist; a script inside the state folder; a script in a folder of mode 0o775 (group-writable); a link inside the scripts' folder that points to a script inside the state folder. Nothing is stored for any of them (`get` still null).
   - After a valid set, `get` returns that value.
   - The script's folder is then made group-writable and B escalated: no call is made, and a `notify_failed` names B with a `reason` starting `unsafe:`.
8. `a_command_never_holds_the_tick`
   - A command that starts a grandchild holding its stderr (`sleep 60` in the background, writing its pid to a file) and exits 0: one `notify_sent` names B within 20 s.
   - A command that starts such a grandchild and then waits for it: within 20 s one `notify_failed` names C with `reason` == `timeout`, and the grandchild whose pid it wrote is no longer alive (`kill(pid, 0)` fails) within 5 s after.

What the test does not show: the log line after the third failure (the daemon writes it to its own stderr in this process; read by hand), and an entry answered between the check and the start of a call (a window of one read that the test cannot hold open). Test 7's "the secretary's set is denied" already holds on main; the test fails on main at the person's `get` (no setting `notify_command`).

## 8. Known limits

- At least once: a call not recorded at a stop is made again after the restart.
- A successful exit says the person's command ran, not that a person saw it.
- After three failures under one setting an entry is not called again until the command is set again; only makibad's log and the events say so (`makiba status` does not show it yet).
- What a command leaves behind after it exits 0 is not killed (a sender may start a helper on purpose).
- The checks of 2.1 cannot see a place an agent writes that is not among those named (a folder the person added to a worker's write list by hand); the ownership and mode checks are the second line.
- Group-writable is refused even where the group is the user's own private group (a umask of 002, the default for users on some systems): such a folder must be made 0755 first. For the same reason the declared test passes only where the folders it makes are not group-writable (a umask of 022, as in CI), and under a sandbox that shows root's files as owned by the overflow user (`nobody`) the ownership rule refuses `/` itself.

## 9. Proposed follow-ups (not in this diff)

- Call the person for `secretary_gone` and for the stop of relaunches (spec section 11), through the same module, marked as makibad's and not the secretary's.
- `makiba status` lines for the call: the setting, the last call, entries that failed three times.
- Quiet hours or a snooze.

## 10. The outside reading, and what changed

A fresh reader (not makiba's review) read the full text on 4060c8f. Each finding was checked against the code before the design changed:

- Major 1 (failures counted for ever): held; the rule counted every `notify_failed`. Now only failures after the setting's newest `config_set` count (4.3); test 5 sets a good command after three failures.
- Major 2 (a pipe can block the tick; a kill reaches only the child): held for the design as written (a captured stderr). Now stderr and stdin are files, the command has its own process group, and the group is killed at the timeout (2, 4.5); test 8.
- Major 3 (makibad runs unsandboxed, an agent could write the script): held; spec section 2 lists the places agents write, and makibad runs outside the sandbox. Now 2.1 refuses those places, the temporary folders, and files or folders writable by others, at the set and before each call; test 7.
- Major 4 (cutting before masking leaks part of a key): held; `Mask::text` (status_local.rs) matches whole 64-character keys only. Now each text is masked whole first, then cut (3); test 6.
- Major 5 (the tests never reach the steady state): held; every case set the command after the entries waited. Test 1 now escalates C after the first call; test 8 covers a hanging command.
- Minor 1 ("while the base is down"): held; the loop starts only after `hello` (mod.rs) and ends on a failed subscription. The text now says the pass is not behind the reconcile gate, once makibad runs.
- Minor 2 (orphaned child at a stop): held; now killed and reaped at the loop's end (5).
- Minor 3 (a failed event write): held; the result is kept until its transaction commits (4.6).
- Minor 4 (a NUL makes every start fail): held for an argument; the body now goes on stdin and control characters are replaced (3); test 2.
- Minor 5 (arguments readable in `/proc/<pid>/cmdline`): held; the body is now on stdin, the argument is the count only (2).
- Minor 6 (the carried meaning of `notify_sent`): held; plan.md and spec.md each get one line (1, 6).
- Minor 7 (the Counts table if N3 leaves `later`): first kept; then the owner withdrew "later" for this Issue, so N3 becomes `build` and the Counts table changes with it (6).
- Minor 8 (tests for `and <k> more`, an answered entry, the third-failure log line): `and <k> more` and an answered entry are added (tests 2 and 4); the log line and the window of one read are named as not shown (7).
- Minor 9 (the secretary's set already denied on main): held; said in 7.

## 11. For the person: example commands

Each is a file of your own, outside the folders of 2.1 (for example `~/bin/makiba-call`), mode 0755, in folders only you can write (check with `ls -ld ~ ~/bin`: no `w` in the group or other places). Then `makiba config set notify_command ~/bin/makiba-call` with the path written out in full.

A Linux desktop:

```sh
#!/bin/sh
exec notify-send --app-name=makiba "$1" "$(cat)"
```

WSL, a Windows toast (needs the BurntToast PowerShell module; any other toast command works the same way):

```sh
#!/bin/sh
body=$(cat)
exec powershell.exe -NoProfile -Command "New-BurntToastNotification -Text \$args[0], \$args[1]" "$1" "$body"
```

A push to a phone through a service of your choice (its host is reached by makibad, not by a worker):

```sh
#!/bin/sh
exec curl -fsS -H "Title: $1" --data-binary @- https://push.example.com/your-topic
```
