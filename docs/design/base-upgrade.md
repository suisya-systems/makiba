# Taking a new base release: one verb, one question

Status: design note for Issue 381, 2026-11-24. Built by Issue 422 (`makiba base upgrade`; section 3a says what the build settled). Goal clause G8: the base is a part that can be replaced, and an update of it does not break makiba. Read beside [upgrading-the-base.md](../upgrading-the-base.md) ("After the pin lands") and spec section 12. A design review (one round) was folded in: a session-wide inventory before the question (S2), another organization in the session (S2), the new file placed beside the destination before the stop (S1), a core already stopped (S3), the two versions read apart (section 3, the table), and the wiring of the verb (section 7).

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

- `agent_variable` in `src/up.rs` finds a variable, as for `up` and `down`: a `CLAUDE*` variable or one of the base's terminal variables of its list (`HC_SESSION`, `HC_SOCKET`, `HC_SCOPE`, `HC_TERMINAL_ID`, `HC_TERMINAL_SOCKET`). An agent can never run it. `HC_BIN` is not in that list and stays the supported way to name the file;
- another run of the verb holds one of its two locks, each a file locked with `try_lock` as `makibad.lock` is (`daemon_running` in `src/up.rs`), taken here in this order and held until the end of S5: the session's, `${XDG_RUNTIME_DIR:-/tmp}/makiba-upgrade-<session>.lock`, so two runs never stop and start the same session at once, whatever `hc` or state folder each names; and the file's, `.<name>.makiba-upgrade.lock` in the destination file's folder (a third, the state folder's, was added by the build: section 3a), so two runs for two sessions never replace the same `hc` at once. The operating system drops them when the process ends;
- `up`'s own prerequisites fail: `check_claude` and `check_git` of `src/up.rs` (made `pub(crate)`) run here, before anything is stopped, so a missing or unsupported Claude Code or git cannot stop S5 after the terminals have ended. S5's `up::run` runs them again; they pass again unless the person changed them meanwhile;
- `MAKIBA_BASE=stub`, outside the verb's own tests;
- the standard input is not a terminal (the question of S2 cannot be asked; there is no `--yes`);
- the `hc` that `start` will run is not found, or the file it is (links followed, section 3a point 3) is not one the rule of section 3a point 5 accepts, or its folder is not writable;
- the core or the file is newer than `PINNED`: "this makiba pins P, the base is Y; run the makiba that pins Y by its path". This is the PR 373 case (an older makiba on `PATH`);
- the core's or the file's version cannot be compared: not plain `major.minor.patch` (a pre-release or build suffix, or an unreadable answer), as `parse` in `src/base/version.rs` refuses it. The message prints the version as read; nothing is changed.

**S1. Download, verify, and place beside the destination.** The script installs into `<state>/base/staging-<pid>/bin/hc` (the state folder of `state_dir`, the verb's own process id), with its own SHA-256 check. The file is then copied into the folder of the destination `hc` under a name of this run (`.hc.makiba-new-<pid>`; under the lock of S0 the run first removes any `.hc.makiba-new-*` a crashed run left), and that copy's `--version` must print `PINNED`. A copy of the old release is staged and kept at `<state>/base/herdr-core-<old version>` once the person answers `y` (section 3a point 2). Any failure (download, hash, version, a copy, a folder that is not writable or is full) removes what this step made and stops, with nothing else changed. Because the new file already sits in the destination folder, S4 is one `rename` within one folder and cannot fail for a cross-device reason after the irreversible step. The host is `releases.herdr.dev` (or `HERDR_RELEASES_URL`); the script needs `bash`, `curl` and `python3`, as today.

**S2. Inventory, then the person's word.** When S3 will stop a running core, the verb first takes a complete inventory of the session with a new `Base` operation, `list_session`: `terminal.list` with no label filter, every live terminal of the session (`src/base/herdr.rs`, and the stand-in in `src/base/stub.rs`). Today's `list` cannot: it filters by this organization's tag and refuses on a connection without one. The inventory must succeed and be read in full, or the verb refuses (staging removed). The core may be of the older release: `hello`, `terminal.list` and `server.stop`, with `terminal.kill` and `terminal.rm` inside `down::run` (`Base::end` in `src/base/herdr.rs`), are the only methods the verb calls on it, and `hello`'s answer is read only for its version.

Then the verb refuses when a terminal with a `makiba.org` tag other than this organization's (the canonical state folder, as `connect_base` in `src/daemon/mod.rs` sets it) has the role `worker` or `secretary`: another organization shares the session, and stopping the core would end its agents without suspending its runs. These are exactly the terminals that organization's `makiba down --secretary` ends (`down::run`), so the message names each such state folder and says: run that organization's `makiba down --secretary` (its own binary, its own `MAKIBA_STATE_DIR`), then run the verb again. Its other tagged terminals (a curator) are what its own `down --stop-base` would leave to the stop today; they are shown at S2 as "another organization's <role>" and end with the core.

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

**S4. Replace `hc`.** One `rename` of this run's `.hc.makiba-new-<pid>` over `hc` in the same folder. A failure leaves the old `hc` in place and the core stopped; the message says the verb can be run again (it starts from S0 and finds the core stopped), or the old makiba (which pins the old release) can start it.

**S5. Start and check.** `up::run` with the same `--no-secretary` choice the verb was given, which starts the core with the new `hc` (or finds it running), resumes the suspended runs and starts `makibad` from this binary (`current_exe`). Then `hello` must answer `PINNED`, and `up`'s own wait for `subscribed` must pass. The verb prints the old and new versions, the path of the old copy, and the newest `dispatcher_started` event id, which carries `base_version`.

There is no automatic rollback. Going back is the person's act, in this order, because after S5 the running core is the new release and the old makiba refuses it: with the new makiba, `makiba down --secretary --stop-base`; then put `<state>/base/herdr-core-<old>` back over the file replaced; then reinstall or rebuild the old makiba and run its `up` by its path. upgrading-the-base.md names these steps.

## 3a. As built (Issue 422)

The build (Issue 422, `src/base/upgrade.rs`, test `tests/base_upgrade.rs`) follows section 3 with the five points its design review settled, which change the text above where they disagree:

1. **`up` in the window between the stop and the rename.** `makiba up` takes the session lock `${XDG_RUNTIME_DIR:-/tmp}/makiba-upgrade-<session>.lock` shared (`try_lock_shared`) for its whole run (`up_lock`, called from `main` in `src/up.rs`), so it is refused (`conflict`) while a verb holds it exclusively, before `hc` runs and before the ledger is opened; two `up`s still run side by side. An existing lock file that `up` can open neither for writing nor for reading refuses `up` (a verb may hold it); only a file that does not exist and cannot be made lets `up` go on without it, since a verb holds the lock only on a file that exists. The verb calls `up::run` at S5 under its own exclusive lock. `down` takes no lock (a `down --stop-base` typed during the question makes S3 fail as a failed `down --stop-base` does).
2. **The old copy is always kept.** The release kept is the core's when the verb stops an older core, else the file's. When the file is that release, it is copied; otherwise (an old core with the file already `PINNED`) the installer is run a second time for that release at S1, before anything is stopped, and a failure refuses with nothing stopped. Both end at `<state>/base/staging-<pid>/old/herdr-core`, whose `--version` must print that release, and are moved to `<state>/base/herdr-core-<old>` only after the answer `y`, so "n" leaves no copy. The rollback of section 3 puts that file back.
3. **A link is followed.** Since #382 `hc` is a link to `herdr-core`. The file replaced is the canonical path of `$HC_BIN` (a bare name looked up on `PATH`), else of the first `hc` on `PATH`: the program itself. Links are never rewritten.
4. **The installer runs alone.** Each run of the carried installer has `HC_BIN` removed and its own prefix's `bin` first on `PATH`, so its check after installing finds the files it installed; any exit but 0 (3 included) is a failure.
5. **Which server starts.** P15 has not been run, so the rule is the conservative one: S0 refuses, before anything is staged or stopped, unless the file is a regular file named `herdr-core` or `hc` in a writable folder and each of these that exists is that same file by canonical path (a hard link elsewhere is refused): `herdr-core` beside the named `hc`, `herdr-core` beside the file, the first `herdr-core` on `PATH`, and, when `HC_BIN` is set, the first `hc` on `PATH`. At S5 a failure of `up::run` or a core that does not answer `PINNED` is reported with what the core answers and the file replaced.

Also as built: a third lock, `<state>/base/upgrade.lock`, allows one run per state folder (two runs of one state folder on two sessions would otherwise share `<state>/base`); leftovers are removed only under the lock that covers them (`staging-*` under the state lock, `.<name>.makiba-new-*` under the file lock `.<name>.makiba-upgrade.lock` beside the file). The core is taken as stopped only when its socket is absent or refuses; any other failure to reach it refuses with nothing changed. After the answer `y` the file and both versions are read again and must be what S0 found (`conflict` otherwise), and the S2 inventory is read again too, before anything stops, and refuses the same way, since another organization's `makibad` may have launched an agent while the question waited; a ledger that exists but cannot be read refuses at S2 instead of showing no runs. `MAKIBA_BASE=stub` is refused always: the tests use a stand-in core on a socket. S4 is skipped when no file is replaced. `makiba status` prints no base version; the new one is read from `makiba events` (`dispatcher_started`, `base_version`), as upgrading-the-base.md step 6 does. The verb is `makiba base <action> [--no-secretary]` with `upgrade` the one action (spec section 12).

## 4. What changes in `makiba up`

After `connect_session_for_start` and `hello`, an `UnsupportedVersion { found }` stops `up` at once, before `hc server start`, before the ledger is touched, with one of:

- found older than `PINNED`: "the base runs F, this makiba pins P. To take P: `<current_exe> base upgrade` (it asks before ending the base's terminals)";
- found newer: "the base runs F, this makiba pins P: this makiba is older than the base. Run the makiba that pins F by its path";
- found not comparable (not plain `major.minor.patch`): "the base runs F, which this makiba cannot compare with P", naming neither verb.

`<current_exe>` is the absolute path of the running binary, so the text names the right build even when `PATH` finds another (the PR 373 case). `up` still never stops a core.

## 5. What the verb never does on its own

- It never runs unless the person types it: `up`, `makibad`, the secretary and any agent never call it, and its environment check refuses inside an agent.
- It never ends a terminal without the S2 answer, and never asks it when standard input is not a terminal.
- It never stops a session that holds another organization's worker or secretary.
- It never runs twice at once on the same session or the same `hc` (the locks of S0).
- It never stops anything before `up`'s own prerequisites (Claude Code, git) have passed.
- It never moves the base to any release other than its own `PINNED`, and never backwards.
- It never rewrites a link, never edits `PATH`, `HC_BIN` or a shell profile, and replaces only the one file `start` will run.
- It never rebuilds or installs makiba, and never moves the pin.
- It never stops the core of another session; only `$MAKIBA_BASE_SESSION`.
- It never rolls back by itself.

## 6. State

The verb keeps no state of its own in memory beside the ledger, and adds no table, column or event kind. The ledger rows it causes are those `down` and `up` already write (suspension, the secretary's failed launch, `dispatcher_started` with `base_version`). Files: the run's staging folder and its `.hc.makiba-new-<pid>` copy (removed at the end, or when a step before S4 refuses; a crash may leave them, and the next run removes them under the lock), the three lock files, `${XDG_RUNTIME_DIR:-/tmp}/makiba-upgrade-<session>.lock`, `<state>/base/upgrade.lock` and `.<name>.makiba-upgrade.lock` beside the file (kept; only their locks matter, and they end with the process), and the old copy `<state>/base/herdr-core-<old>`, kept until the person removes it. A crash between S3 and S5 leaves what `down --stop-base` leaves today: the next `makiba up` (old or new `hc`) or a second run of the verb continues from there, since S0 reads both versions anew.

## 7. Files the build changes

- New: `src/base/upgrade.rs`; a test file for the verb (stand-in core, scratch `HERDR_RELEASES_URL` as `tests/base_version.rs` does, a pseudo-terminal for the question).
- `src/base/mod.rs`: the module declaration, and `list_session` on the `Base` trait.
- `src/base/herdr.rs` (`list_session`: `terminal.list` without labels) and `src/base/stub.rs` (the stand-in's `list_session`, and a way for a test to add a terminal of another organization and one with no tag). `src/base/uses.rs` already names `terminal.list`; the parameters used do not change.
- `src/up.rs`: the refusal of section 4; `run`, `check_claude` and `check_git` made `pub(crate)` so that S0 and S5 call them.
- `src/down.rs`: `run` is already `pub`; the `makibad` stop of `main` made `pub(crate)`. Behaviour unchanged.
- `src/cli.rs` (the clap command `base upgrade [--no-secretary]`) and `src/main.rs` (the dispatch of the local verb, beside `up` and `down`).
- `src/spec.rs` (`Verb`), `docs/design/spec.md` section 12 (the verb's row), and `tests/spec_matches_code.rs` if it lists the local verbs.
- `scripts/install-base.sh` (the `MAKIBA_BASE_PINNED` input) and `tests/base_version.rs` (a case for it).
- `docs/upgrading-the-base.md` ("After the pin lands": the three steps, the manual steps kept as the fallback).
- No file under `.github/`, no merge gate, no acceptance checker, no schema.

## 8. Build Issues proposed

1. **`makiba up` refuses a core of another version at once and names what to run.** Section 4. Test: a stand-in core answering an older and a newer version; `up` exits `unavailable` within a second, the text holds `current_exe` and `base upgrade` (older), "older than the base" (newer) or "cannot compare" (a pre-release version), `hc` was not run, and the ledger has no new suspension.
2. **`install-base.sh` takes the version from `MAKIBA_BASE_PINNED`.** Test in `tests/base_version.rs`: with the variable set and no `src/base/version.rs` beside the script, it installs that version from a scratch manifest.
3. **`Base::list_session`.** Section 3, S2. Test on the stand-in: it returns terminals of this organization, of another and with no tag; `list` still returns only this organization's.
4. **`makiba base upgrade`.** Sections 3, 5, 6. Depends on 1, 2 and 3. Tests: each S0 refusal changes nothing (no download, no stop); `HC_BIN` naming the file is accepted and that file is the one replaced; a second run while the first waits at S2 is refused at S0 and the first run's staged file is untouched, both with the same `hc` and with another `HC_BIN` for the same session; a Claude Code or git below its lowest version refuses at S0 with `makibad` and every terminal still running; a wrong hash at S1 stops nothing and leaves no `.hc.makiba-new-*`; a worker or secretary of another organization refuses at S2 with nothing stopped, and after that organization's `makiba down --secretary` the verb passes S2; "n" at S2 stops nothing and removes staging; "y" ends the session's terminals, replaces `hc`, keeps the old copy, and `makiba status` shows `subscribed: yes` with the new version; a stopped core with an old file is upgraded without a call to the base before S5; an old core with a `PINNED` file is restarted and the file left as it was; a newer core or file is refused; a core or file with a pre-release or malformed version is refused with nothing changed; the rollback steps of section 3 bring the old makiba back on the old core.
5. **Docs: the owner's three steps.** `docs/upgrading-the-base.md`, after 4 lands.

## 9. With Issue 380: a range of two releases

Issue 380's note (`docs/design/base-range.md`, not on this note's base; read here as its secretary summarized it) proposes that makiba accept `PINNED` and the release before it, `PREVIOUS`. A makiba update then no longer forces the core swap at the same stop: the new makiba runs on the `PREVIOUS` core, and the swap is done later, with no run in flight. This note composes with it as follows, once 380's build lands:

- `up` does not refuse a `PREVIOUS` core: `version::check` accepts it, so section 4's early refusal does not fire. It still fires for anything older than `PREVIOUS` (naming `base upgrade`) and for anything newer than `PINNED`. `up` may print one line that the core is `PREVIOUS` and that `base upgrade` takes `PINNED` when no run is in flight; it never offers to do it.
- `makiba base upgrade` becomes 380's "later swap". In section 3's table, "older" reads "`PREVIOUS` or older": a `PREVIOUS` core is upgraded by the same S0 to S5, with the same question at S2. A core older than `PREVIOUS` is upgraded the same way. Nothing in the order or the stop points changes; only the table reads `PREVIOUS` where it reads "older" today.
- What "no run in flight" means is 380's to define. This verb does not wait for it on its own: S2 already shows the runs that will be suspended, and the person answers with that in view. If 380 decides the swap must wait for an empty belt, that is a refusal added to S0, not a change of S3.
- The early refusal's text and the version table are the two places that must read `PREVIOUS`. Build Issue 1 and build Issue 4 of section 8 are written against a single `PINNED`; whichever of them lands after 380's build reads `PREVIOUS` there, and its test adds a `PREVIOUS` core: accepted by `up`, upgraded by the verb.

## 10. Known limits

- A terminal opened in the session between the S2 inventory and S3 ends without having been shown. The base has no lock against new terminals, and another organization's `makibad` takes no makiba lock before a launch. As built (section 3a), the inventory is read again after the answer, so another organization's agent refuses the stop unless it is launched in the moment between that read and `server.stop`.
- The verb talks to a core of the older release with `hello`, `terminal.list`, `terminal.kill`, `terminal.rm` and `server.stop`. If a release changes one of these, the verb refuses at S2 (inventory unreadable) or fails at S3; the person then falls back to the manual steps of upgrading-the-base.md, which stay documented.
- The `hc` the person uses for their own sessions is the same file: their other cores keep running the old binary and start the new one at their next start. S2 says the file is replaced; the verb does not look for other sessions.
