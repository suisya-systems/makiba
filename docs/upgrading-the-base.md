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

makiba refuses a core of any other version on every connection (`checked_hello` in `src/base/herdr.rs`, `check` in `src/base/version.rs`), and `makiba up` never stops or restarts a running core (`start` in `src/base/herdr.rs`). So a core of the old release still running after the merge refuses the new `makibad`: the core is stopped, `hc` replaced, makiba rebuilt, and makiba started again. Type each line in the person's login shell: `makiba up` and `makiba down` refuse to run when a `CLAUDE*` or `HC_SESSION`, `HC_SOCKET`, `HC_SCOPE`, `HC_TERMINAL_ID` or `HC_TERMINAL_SOCKET` variable is set (`agent_variable` in `src/up.rs`).

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
