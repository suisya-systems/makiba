# Taking a new base release: one verb, one question

Status: design note for Issue 381, 2026-11-24. Nothing is built. Goal clause G8: the base is a part that can be replaced, and an update of it does not break makiba. Read beside [upgrading-the-base.md](../upgrading-the-base.md) ("After the pin lands") and spec section 12. A design review (one round) was folded in: a session-wide inventory before the question (S2), another organization in the session (S2), the new file placed beside the destination before the stop (S1), a core already stopped (S3), the two versions read apart (section 3, the table), and the wiring of the verb (section 7).

## 1. Today

After a pin move the owner types five steps (upgrading-the-base.md): `makiba down --secretary --stop-base`, `bash scripts/install-base.sh PREFIX`, rebuild makiba, `up` from the rebuilt binary by its path, then check the versions. Three facts of the code shape every option:

- `makiba up` never stops or restarts a running core (`run` in `src/up.rs`; `start` in `src/base/herdr.rs` only runs `hc server start`). It takes the core as stopped only when nothing listens at the socket.
- When a core of another version listens, `up` sees `BaseError::UnsupportedVersion` from `hello` (`checked_hello` in `src/base/version.rs`), does not stop there, and runs `hc server start`; if that returns, it starts `makibad`, which is refused on its own `hello`, and `up` fails only after 20 s with "makibad is not subscribed to the base". The person is not told which version runs, which one this makiba wants, or what to do.
- Stopping the core (`server.stop`, `stop` in `src/base/herdr.rs`) ends the programs of every terminal of the session `$MAKIBA_BASE_SESSION`, the person's own included (the base keeps "kept" terminals and gives them new shells at its next start). That is the one irreversible step. Everything else (`down`'s suspension, replacing `hc`) can be undone by `up` or by putting the old file back.

And one fact of the history: in review round 1 of PR 373 a bare `makiba` on `PATH` was an older build still pinned to the old release. Any design must not let an older makiba act on a newer core.

## 2. Decision: option B, with the refusal of option C in `up`

A new local verb, `makiba base upgrade`, does the base's part in order and asks the person once, before the irreversible step. `makiba up` keeps its contract (it never stops a core) and gains an early, clear refusal that names the verb by the path of the binary that ran it.

Why not A (`up` offers to stop and upgrade): `up` is typed by habit, after every `down`, and from scripts and runbooks. A verb whose job is to start things must not carry a question whose "y" ends the person's own terminals; a person answering by reflex is the failure. A also mixes two directions in one verb: an `up` that finds a newer core must refuse, one that finds an older core would offer, and the person cannot tell from the command which will happen. A separate verb states the intent in the command itself.

Why not C alone: it keeps five steps, and the steps are where the PR 373 mistake happened (the wrong `makiba` on `PATH`, `hc` installed to a prefix `PATH` does not find). The refusal text of C is still needed and is taken (section 4).

The owner's steps after a pin move become three:

1. `makiba down` (plain; reversible: `up` brings the work back). Recommended so that the binary is not replaced under a running `makibad` (docs/installing.md, "Update"); the verb stops a running `makibad` itself if this was skipped.
2. Rebuild or reinstall makiba. The verb does not do this: the binary that runs the verb is the one that knows the new pin (`PINNED` is compiled in), so it must exist first.
3. `<path of the new makiba> base upgrade`, which stops, replaces, starts and checks (section 3).

## 3. `makiba base upgrade`: the order and the stop points

Owner: a new module `src/base/upgrade.rs` composes what already exists: `down::run` and the `makibad` stop of `down`'s `main` (suspends, ends terminals, stops the core), `up::run` (starts the core and `makibad`, resumes; it already suspends the work itself when it finds the core stopped), `version::check` (the pin). Installing the file stays owned by `scripts/install-base.sh`: the binary carries the script (`include_str!`) and runs it with `bash`, so there is one installer, not a second one in Rust. The script gains one input: `MAKIBA_BASE_PINNED`, used instead of reading `src/base/version.rs` when set (an installed makiba has no checkout); the verb always sets it to its own `PINNED`.

Two versions are read in S0 and decide the path: the core's (from `hello`: its version, or `found` of `UnsupportedVersion`, or "stopped" when nothing listens, the test `up` uses) and the file's (`hc --version` of the `hc` that `start` will run: `$HC_BIN`, else the first `hc` on `PATH`).

| Core | File | What the verb does |
| --- | --- | --- |
| `PINNED` or stopped | `PINNED` | Nothing: "nothing to upgrade; run `makiba up`". |
| older or stopped | older | Replace the file and restart (S1 to S5). |
| older | `PINNED` | Restart only: S1 and S4 are skipped, S2 says no file is replaced. |
| `PINNED` | older | Replace the file only: S3 does not stop the core, S2 says no terminal ends. |
| newer | any | Refuse (never backwards). |
| any | newer | Refuse (never backwards). |

Every step before S3 changes nothing outside the verb's own staging files, and removes them when it refuses.

**S0. Refusals before anything changes.** The verb exits with nothing changed when:

- a `CLAUDE*` or `HC_*` variable is set (`agent_variable` in `src/up.rs`, as `up` and `down`): an agent can never run it;
- `MAKIBA_BASE=stub`, outside the verb's own tests;
- the standard input is not a terminal (the question of S2 cannot be asked; there is no `--yes`);
- the `hc` that `start` will run is not found, is a symbolic link, or its folder is not writable. A link is not followed and not rewritten; the message names the two choices of upgrading-the-base.md step 2 (install where the link points, or point the link at a new file);
- the core or the file is newer than `PINNED`: "this makiba pins P, the base is Y; run the makiba that pins Y by its path". This is the PR 373 case (an older makiba on `PATH`).

**S1. Download, verify, and place beside the destination.** The script installs into `<state>/base/staging/bin/hc` (the state folder of `state_dir`), with its own SHA-256 check. The file is then copied into the folder of the destination `hc` under a temporary name (`.hc.makiba-new`), and that copy's `--version` must print `PINNED`. The old `hc` is copied to `<state>/base/hc-<old version>`. Any failure (download, hash, version, a copy, a folder that is not writable or is full) removes what this step made and stops, with nothing else changed. Because the new file already sits in the destination folder, S4 is one `rename` within one folder and cannot fail for a cross-device reason after the irreversible step. The host is `releases.herdr.dev` (or `HERDR_RELEASES_URL`); the script needs `bash`, `curl` and `python3`, as today.

**S2. Inventory, then the person's word.** When S3 will stop a running core, the verb first takes a complete inventory of the session with a new `Base` operation, `list_session`: `terminal.list` with no label filter, every live terminal of the session (`src/base/herdr.rs`, and the stand-in in `src/base/stub.rs`). Today's `list` cannot: it filters by this organization's tag and refuses on a connection without one. The inventory must succeed and be read in full, or the verb refuses (staging removed). The core may be of the older release: `hello`, `terminal.list` and `server.stop` (with `terminal.end` inside `down::run`) are the only methods the verb calls on it, and `hello`'s answer is read only for its version.

Then the verb refuses when any terminal carries a `makiba.org` tag other than this organization's (the canonical state folder, as `connect_base` in `src/daemon/mod.rs` sets it): another organization shares the session, and stopping the core would end its workers without suspending its runs. The message names each other organization's state folder: stop it first with its own `makiba down`, then run the verb again.

Otherwise one question, on the terminal, showing:

- the core of session `<session>`: its version, then `PINNED` (or "stopped", or "not stopped");
- the file to be replaced, its absolute path, and where its old copy is kept (or "no file is replaced");
- the terminals whose programs will end: makiba's counted by role, every terminal with no `makiba.org` tag named one per line as "not makiba's: yours";
- the runs that will be suspended and brought back.

Then `End these terminals and replace hc? [y/N]`, its wording dropping what does not apply. Anything but `y` removes the staging files and exits with nothing changed. This is the only place the person's word is asked, and it is asked every time.

**S3. Stop (irreversible).** Stop `makibad` as `down`'s `main` does. Then:

- core running and to be stopped: `down::run` with `secretary` and `stop_base`, then wait until nothing listens at the socket. From here the programs of every terminal of that session are gone. A failure (a call refused, the core still listening after the wait) stops the verb: `hc` is not replaced, the staged copy is removed, and the message says what was left (the same as a failed `down --stop-base` today).
- core already stopped: no call to the base. The suspension is `up::run`'s own (it finds the core stopped and suspends, as after any stop of the base); `down::run` is not used, since it would call `list` and `stop` on a socket that does not answer.
- core already `PINNED` (only the file is old): the core is not stopped; S5 finds it running.

**S4. Replace `hc`.** One `rename` of `.hc.makiba-new` over `hc` in the same folder. A failure leaves the old `hc` in place and the core stopped; the message says the verb can be run again (it starts from S0 and finds the core stopped), or the old makiba (which pins the old release) can start it.

**S5. Start and check.** `up::run` with the same `--no-secretary` choice the verb was given, which starts the core with the new `hc` (or finds it running), resumes the suspended runs and starts `makibad` from this binary (`current_exe`). Then `hello` must answer `PINNED`, and `up`'s own wait for `subscribed` must pass. The verb prints the old and new versions, the path of the old copy, and the newest `dispatcher_started` event id, which carries `base_version`.

There is no automatic rollback. Going back is the person's act: put `<state>/base/hc-<old>` back and run the old makiba.

## 4. What changes in `makiba up`

After `connect_session_for_start` and `hello`, an `UnsupportedVersion { found }` stops `up` at once, before `hc server start`, before the ledger is touched, with one of:

- found older than `PINNED`: "the base runs F, this makiba pins P. To take P: `<current_exe> base upgrade` (it asks before ending the base's terminals)";
- found newer: "the base runs F, this makiba pins P: this makiba is older than the base. Run the makiba that pins F by its path".

`<current_exe>` is the absolute path of the running binary, so the text names the right build even when `PATH` finds another (the PR 373 case). `up` still never stops a core.

## 5. What the verb never does on its own

- It never runs unless the person types it: `up`, `makibad`, the secretary and any agent never call it, and its environment check refuses inside an agent.
- It never ends a terminal without the S2 answer, and never asks it when standard input is not a terminal.
- It never stops a session that holds another organization's terminals.
- It never moves the base to any release other than its own `PINNED`, and never backwards.
- It never follows or rewrites a link, never edits `PATH`, `HC_BIN` or a shell profile, and replaces only the one file `start` will run.
- It never rebuilds or installs makiba, and never moves the pin.
- It never stops the core of another session; only `$MAKIBA_BASE_SESSION`.
- It never rolls back by itself.

## 6. State

The verb keeps no state of its own in memory beside the ledger, and adds no table, column or event kind. The ledger rows it causes are those `down` and `up` already write (suspension, the secretary's failed launch, `dispatcher_started` with `base_version`). Files: the staging folder and the `.hc.makiba-new` copy (removed at the end, or when a step before S4 refuses; a crash may leave them, and the next run overwrites them), and the old `hc` copy under `<state>/base/`, kept until the person removes it. A crash between S3 and S5 leaves what `down --stop-base` leaves today: the next `makiba up` (old or new `hc`) or a second run of the verb continues from there, since S0 reads both versions anew.

## 7. Files the build changes

- New: `src/base/upgrade.rs`; a test file for the verb (stand-in core, scratch `HERDR_RELEASES_URL` as `tests/base_version.rs` does, a pseudo-terminal for the question).
- `src/base/mod.rs`: the module declaration, and `list_session` on the `Base` trait.
- `src/base/herdr.rs` (`list_session`: `terminal.list` without labels) and `src/base/stub.rs` (the stand-in's `list_session`, and a way for a test to add a terminal of another organization and one with no tag). `src/base/uses.rs` already names `terminal.list`; the parameters used do not change.
- `src/up.rs`: the refusal of section 4; `run` made `pub(crate)` so that S5 calls it.
- `src/down.rs`: `run` is already `pub`; the `makibad` stop of `main` made `pub(crate)`. Behaviour unchanged.
- `src/cli.rs` (the clap command `base upgrade [--no-secretary]`) and `src/main.rs` (the dispatch of the local verb, beside `up` and `down`).
- `src/spec.rs` (`Verb`), `docs/design/spec.md` section 12 (the verb's row), and `tests/spec_matches_code.rs` if it lists the local verbs.
- `scripts/install-base.sh` (the `MAKIBA_BASE_PINNED` input) and `tests/base_version.rs` (a case for it).
- `docs/upgrading-the-base.md` ("After the pin lands": the three steps, the manual steps kept as the fallback).
- No file under `.github/`, no merge gate, no acceptance checker, no schema.

## 8. Build Issues proposed

1. **`makiba up` refuses a core of another version at once and names what to run.** Section 4. Test: a stand-in core answering an older and a newer version; `up` exits `unavailable` within a second, the text holds `current_exe` and `base upgrade` (older) or "older than the base" (newer), `hc` was not run, and the ledger has no new suspension.
2. **`install-base.sh` takes the version from `MAKIBA_BASE_PINNED`.** Test in `tests/base_version.rs`: with the variable set and no `src/base/version.rs` beside the script, it installs that version from a scratch manifest.
3. **`Base::list_session`.** Section 3, S2. Test on the stand-in: it returns terminals of this organization, of another and with no tag; `list` still returns only this organization's.
4. **`makiba base upgrade`.** Sections 3, 5, 6. Depends on 1, 2 and 3. Tests: each S0 refusal changes nothing (no download, no stop); a wrong hash at S1 stops nothing and leaves no `.hc.makiba-new`; a terminal of another organization refuses at S2 with nothing stopped; "n" at S2 stops nothing and removes staging; "y" ends the session's terminals, replaces `hc`, keeps the old copy, and `makiba status` shows `subscribed: yes` with the new version; a stopped core with an old file is upgraded without a call to the base before S5; an old core with a `PINNED` file is restarted and the file left as it was; a newer core or file is refused.
5. **Docs: the owner's three steps.** `docs/upgrading-the-base.md`, after 4 lands.

## 9. With Issue 380: a range of two releases

Issue 380's note (`docs/design/base-range.md`, not on this note's base; read here as its secretary summarized it) proposes that makiba accept `PINNED` and the release before it, `PREVIOUS`. A makiba update then no longer forces the core swap at the same stop: the new makiba runs on the `PREVIOUS` core, and the swap is done later, with no run in flight. This note composes with it as follows, once 380's build lands:

- `up` does not refuse a `PREVIOUS` core: `version::check` accepts it, so section 4's early refusal does not fire. It still fires for anything older than `PREVIOUS` (naming `base upgrade`) and for anything newer than `PINNED`. `up` may print one line that the core is `PREVIOUS` and that `base upgrade` takes `PINNED` when no run is in flight; it never offers to do it.
- `makiba base upgrade` becomes 380's "later swap". In section 3's table, "older" reads "`PREVIOUS` or older": a `PREVIOUS` core is upgraded by the same S0 to S5, with the same question at S2. A core older than `PREVIOUS` is upgraded the same way. Nothing in the order or the stop points changes; only the table reads `PREVIOUS` where it reads "older" today.
- What "no run in flight" means is 380's to define. This verb does not wait for it on its own: S2 already shows the runs that will be suspended, and the person answers with that in view. If 380 decides the swap must wait for an empty belt, that is a refusal added to S0, not a change of S3.
- The early refusal's text and the version table are the two places that must read `PREVIOUS`. Build Issue 1 and build Issue 4 of section 8 are written against a single `PINNED`; whichever of them lands after 380's build reads `PREVIOUS` there, and its test adds a `PREVIOUS` core: accepted by `up`, upgraded by the verb.

## 10. Known limits

- A terminal opened in the session between the S2 inventory and S3 ends without having been shown. The base has no lock against new terminals; the window is one answer long.
- The verb talks to a core of the older release with `hello`, `terminal.list`, `terminal.end` and `server.stop`. If a release changes one of these, the verb refuses at S2 (inventory unreadable) or fails at S3; the person then falls back to the manual steps of upgrading-the-base.md, which stay documented.
- The `hc` the person uses for their own sessions is the same file: their other cores keep running the old binary and start the new one at their next start. S2 says the file is replaced; the verb does not look for other sessions.
