# Upgrading the base

The base is herdr-core. makiba runs against one release of it and refuses any other. The pin is `PINNED` and `SUPPORTED` in [`src/base/version.rs`](../src/base/version.rs) (`>=0.1.22,<0.1.23`: one patch release, the one the design was read against and the probes of [`docs/design/probes.md`](design/probes.md) were run on). [`scripts/install-base.sh`](../scripts/install-base.sh) installs that release into a prefix and reads `PINNED` from the same file, so the pin has one place.

herdr-core releases every few days and calls its API a preview. The pin does not follow the releases: a move costs a test run and several probes, so it moves on purpose and rarely.

## When the pin moves

Only for one of these reasons:

1. A release carries something makiba needs.
2. The pinned release can no longer be installed (the manifest or the file is gone).
3. At the owner's monthly look at the base's changelog.

Not on every release, and not more than about once a month.

## What a move reruns

On the new release, before the pin is changed:

1. M1-5's test, `tests/base_compat.rs`, against the new release's own definition: save `hc schema` of the new release to a file and run `MAKIBA_BASE_SCHEMA=<file> cargo test --test base_compat`. `hc schema` answers from the running server, not from the binary, so a core of the old release still running answers with the old definition. Take it from an isolated core of the new release: its own `HC_SESSION` and its own scratch `XDG_CONFIG_HOME`, `XDG_STATE_HOME` and `XDG_RUNTIME_DIR`, started with `server start` and stopped with `server stop` at the end, as `scripts/check-hello-wire.sh` does. Check that the file's `info.version` is the new release. It names every method, parameter or result field of [`src/base/uses.rs`](../src/base/uses.rs) that the new definition lacks or has changed.
2. The whole test suite, on the stand-in and on the real base (`MAKIBA_TEST_REAL_BASE=1`).
3. Probes P1, P3, P4, P5 and P8 of `docs/design/probes.md`. They measure how the base is reached (P1), typed into (P3, P4), launched (P5) and how a turn start shows in its status (P8). A patch release has changed these before: 0.1.20 changed sends. Each takes a few hours of the owner's time.
4. P2 reruns itself in CI.

A probe may be skipped only when the changelog shows nothing touching its area, and the record in probes.md Results says so. P6 is not rerun.

Then replace [`docs/base/hc.openrpc.json`](base/hc.openrpc.json) with that definition, change `PINNED` and `SUPPORTED` together, extend the Results table of probes.md with the new rows, and update the version named in the rules at the top of probes.md.

## After the pin lands: the owner's steps

makiba refuses a core of any other version on every connection (`checked_hello` in `src/base/herdr.rs`, `check` in `src/base/version.rs`), and `makiba up` never stops or restarts a running core (`start` in `src/base/herdr.rs`). So a core of the old release still running after the merge refuses the new `makibad`. `makiba up` says so at once: on an older core it stops before it starts anything and names `<its own path> base upgrade` (`other_version` in `src/up.rs`). Type each line in the person's login shell: `makiba up`, `makiba down` and `makiba base upgrade` refuse to run when a `CLAUDE*` or `HC_SESSION`, `HC_SOCKET`, `HC_SCOPE`, `HC_TERMINAL_ID` or `HC_TERMINAL_SOCKET` variable is set (`agent_variable` in `src/up.rs`).

The decision behind these steps is [`docs/design/base-upgrade.md`](design/base-upgrade.md) (Issue 381).

### The three steps

1. **`makiba down`.** Plain, without `--secretary` or `--stop-base`: it stops `makibad` and suspends its runs, and `makiba up` brings them back. It is there so that step 2 does not replace the binary under a running `makibad`, which leaves its hook and channel paths stale (`docs/installing.md`, "Update"). If it was skipped, step 3 stops a running `makibad` itself.
2. **Rebuild or reinstall makiba.** In the checkout: `git fetch origin`, `git merge --ff-only origin/main`, then `cargo build` (`docs/runbooks/self-hosting.md`, section 11.1); the binary is `target/debug/makiba`. Check that the checkout holds the merged pin: `grep 'PINNED' src/base/version.rs` prints the new release. Where makiba is installed from a release, the installer takes the newest published release (`docs/installing.md`, "Update"); use it only when that release carries both the new pin and `base upgrade`, and build from the checkout otherwise. makiba does not rebuild itself: the binary that runs step 3 must already be the new one, since the pin is compiled into it.
3. **`<path of the new makiba> base upgrade`.** Call it by its path (`target/debug/makiba base upgrade` in the checkout), not as a bare `makiba`: `PATH` may find an older build, pinned to the old release (the PR 373 case). `--no-secretary` has the meaning it has for `makiba up`. The verb:
   - checks before it changes anything, and refuses with nothing changed: inside an agent; when standard input is not a terminal (there is no `--yes`); when `HC_BIN` names no file; when the program `hc` (or `HC_BIN`) runs, links followed, is not a file named `herdr-core` or `hc` (the layout `scripts/install-base.sh` makes) or sits in a folder it cannot write; when another `herdr-core` or `hc` could start in its place (it names that file, as `scripts/install-base.sh` does); when the core or the file is newer than its pin, or its version cannot be compared; while another run of the verb holds the same session, the same state folder or the same file, or a `makiba up` runs on the session; and when Claude Code or git fails `makiba up`'s own check. While the verb runs, `makiba up` on the same session is refused (`conflict`);
   - downloads the pinned release and checks its SHA-256 (`scripts/install-base.sh`), then places it beside the file it will replace;
   - lists every terminal of the session `$MAKIBA_BASE_SESSION` and the runs that will be suspended, and asks once `[y/N]` before it ends them. When the session holds a worker or the secretary of another makiba organization, it refuses instead and names that organization's state folder: run that organization's `makiba down --secretary`, then the verb again. Anything but `y` removes what it downloaded and changes nothing;
   - then stops `makibad` and the core, replaces that program with one `rename` (`src/base/upgrade.rs`), keeps a copy of the old release as `<state>/base/herdr-core-<old version>` (also when only the core was old), and runs `makiba up`, which starts the core of the new file, `makibad` and, without `--no-secretary`, the secretary, and brings the suspended runs back. It prints the old and the new version, where the old file is kept, the id of the new `dispatcher_started` event (it carries `base_version`), and the steps to go back.

   Which of these happen depends on what it finds. Both the core and the file on the pin, or the core stopped and the file on the pin: "nothing to upgrade", and nothing is started; run the new makiba's `up` by its path. An older core with the file on the pin: the core is restarted and the file left as it is. The core on the pin with an older file: the file is replaced and no terminal ends. A core already stopped with an older file: the file is replaced and `up` starts the core. A newer core or file: refused, never backwards. After a refusal, makibad stays stopped: when the core already runs the pin, the new makiba's `up` by its path starts it again; when an older core still runs, the new `up` refuses it too (it names `base upgrade`), so either remove what the refusal names and run the verb again, or reinstall or rebuild the makiba that pins the running core and run its `up` by its path.

   **Check.** `makiba status` shows `subscribed: yes`, and `hc --version` (or `"$HC_BIN" --version`) prints the pin.

### Going back

There is no automatic rollback, and the order matters, because the old makiba refuses the new core:

1. With the **new** makiba, by its path: `makiba down --secretary --stop-base`.
2. Put the old file back: copy `<state>/base/herdr-core-<old version>` into the folder of the program `hc` runs under a new name, then `mv` that copy over the program (both paths are in the verb's "to go back" line). A `cp` straight over it fails with "Text file busy" while a core of another session still runs that file; a rename within the folder does not.
3. Reinstall or rebuild the old makiba.
4. The old makiba's `up`, by its path.

### Manual steps (the fallback)

Use these when the person's makiba has no `base upgrade`, or when the verb refuses before it stops anything and the person chooses to install by hand (for example into another prefix). They are what the verb does, one by one. They do not get around a base whose older release no longer answers `terminal.list`, `terminal.kill`, `terminal.rm` or `server.stop`: step 1 calls the same methods the verb calls (`down` in `src/down.rs`, `Base::end` and `stop` in `src/base/herdr.rs`), and fails where the verb fails (`docs/design/base-upgrade.md`, section 10).

1. **Note the newest event id, then stop makiba and the core.** `sqlite3 "${MAKIBA_STATE_DIR:-${XDG_STATE_HOME:-$HOME/.local/state}/makiba}/state.db" 'SELECT max(id) FROM events'` gives `N` for step 6 (the state folder: `state_dir` in `src/daemon/mod.rs`). Then `makiba down --secretary --stop-base` (`src/down.rs`). It stops `makibad` and suspends its runs. It ends the workers' terminals, and with `--secretary` also the secretary's. `--stop-base` then sends `server.stop` to the core over its socket (`stop` in `src/base/herdr.rs`), so it does not need the new `hc`. The core is the one of session `$MAKIBA_BASE_SESSION` (default `makiba`), at `${XDG_RUNTIME_DIR:-/tmp}/herdr-core/<session>.sock` (`socket_path` in `src/base/herdr.rs`). Stopping it ends every terminal of that session, a person's own included.
2. **Replace `hc`.** makiba runs `hc` for one call only, `hc server start` in `makiba up`. It runs `$HC_BIN` if set, else `hc` on `PATH` (`start` in `src/base/herdr.rs`); everything else goes over the socket. No config key names it. `bash scripts/install-base.sh PREFIX`, from the merged checkout, installs the release `PINNED` names in the base's own layout, after the archive's SHA-256 matches the manifest's (`scripts/install-base.sh`): the archive's `herdr-core` as `PREFIX/bin/herdr-core`, and `PREFIX/bin/hc` a link to it. Both are replaced when present, whether a plain file or a link; a directory in either place is refused before anything is replaced. Both names matter: `hc` starts its server under the name `herdr-core`, and an older `herdr-core` left beside `hc` or ahead of it on `PATH` was reported started in its place (#382). Give the prefix whose `bin` holds the `hc` that `PATH` or `HC_BIN` finds (for example `$HOME/.local` for `~/.local/bin/hc`), or set `HC_BIN=PREFIX/bin/hc`. After installing, the script checks what would run instead of the installed file: `HC_BIN` when set, and the `herdr-core` and `hc` that `PATH` finds first. When one of them is another file, it names that path on standard error and exits 3 (installed, but not what runs). Then remove or rename the file it names, or put `PREFIX/bin` before its folder on `PATH`, or point `HC_BIN` at `PREFIX/bin/hc`, and run the script again until it exits 0. It deletes nothing outside PREFIX and runs no program it finds. `hc --version` (or `"$HC_BIN" --version`) must print 0.1.22. Which server `hc server start` launches when another `herdr-core` exists is measured by probe P15 (`docs/design/probes.md`). `hc --version` reads the binary, but `hc schema` answers from the running server: before `makiba up`, a still-running old core would answer with the old definition (see "What a move reruns", step 1).
3. **The core.** It was stopped in step 1. `makiba up` starts it again with the `hc` of step 2 (`start` in `src/base/herdr.rs`, called from `src/up.rs`). It finds nothing listening at the socket, takes the core as stopped, and fails the secretary's old launch so that a new secretary is launched (`src/up.rs`). Do not start the core by hand: `makiba up` starts it with makiba's own configuration folder `${XDG_CONFIG_HOME:-$HOME/.config}/makiba/base` (`base_config_home` in `src/cli.rs`).
4. **Rebuild makiba at the merged main.** In the checkout: `git merge --ff-only origin/main`, then `cargo build` (`docs/runbooks/self-hosting.md`, section 11.1, step 3). The binary is `target/debug/makiba` (`cargo build --release` puts it at `target/release/makiba`). Where makiba is installed from a release instead, run the installer again (`docs/installing.md`, "Update"). That holds once a release carries this pin; until then the checkout build is the one with 0.1.22. Rebuild only while `makibad` is stopped: a binary replaced under a running `makibad` leaves its hook and channel paths stale (`docs/installing.md`).
5. **`makiba up`, from the rebuilt binary.** Run the binary of step 4 by its path (`target/debug/makiba up` in the checkout), or first check that `command -v makiba` names it. A bare `makiba` runs whatever `PATH` finds first, and an older `makiba` there is still pinned to 0.1.21: it refuses the new core and makiba stays stopped. `up` starts the core, then `makibad` (from the binary that ran `up`, `start_daemon` in `src/up.rs`) and the secretary. It brings the suspended runs back. `makibad` points the hooks' entry point `${MAKIBA_SHARE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/makiba}/makiba` at its own binary when it prepares an agent (`link_entry_point` in `src/agents/mod.rs`, `share_dir` in `src/cli.rs`).
6. **Check.** `hc --version` prints 0.1.22. `makiba events --after N --json | jq -c '.events[] | select(.kind == "dispatcher_started") | {id, occurred_at, base_version: .payload.base_version}'` shows the new start with `base_version` `"0.1.22"`: `makibad` writes it once it has subscribed to the base (`src/daemon/mod.rs`; `events_main` in `src/status_local.rs` returns at most 100 events by default and 1000 with `--limit 1000`). `makiba status` shows `subscribed: yes`.
7. **When `makiba` is not on `PATH`, or `PATH` finds another build.** Call the binary by its path: `target/debug/makiba` in the checkout, or `PREFIX/bin/makiba` for an install from a release (default `~/.local/bin/makiba`, `docs/installing.md`). makiba reads no variable or config key that names its own binary. `makiba up` starts `makibad` from the file that ran it (`current_exe` in `src/up.rs`), so call `down` and `up` by that same path. The hooks reach makiba through the entry-point link of step 5, not through `PATH`.

## The range stays one patch release

A wider range would let in a version that no probe has run on. It is revisited only after three pin moves in a row needed nothing but the number changed.

## Claude Code

Claude Code has its own range and its own document: [upgrading-claude-code.md](upgrading-claude-code.md).
