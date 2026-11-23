# Accepting more than one base release

Status: design note for Issue 380, 2026-11-24. Nothing is built. It answers the Issue's question (can makiba accept more than one herdr-core release, and what shows that a release outside the pin is safe), proposes one option, and names the build Issues. A design review (one round, before the commit) was folded in: the live restart's session, binary and whole sequence (4.5), how a refusal on reconnect reaches the daemon (4.5), probe P15 as a build Issue of its own (7), and what the pin's behaviour check really holds (4.2). The diff review added the gate that keeps every other call from an unchecked core, its rules, and the restart sent by `makibad` on the person's verb (4.5).

Read beside [upgrading-the-base.md](../upgrading-the-base.md), whose section "The range stays one patch release" B' would replace if the owner agrees (section 6), and [probes.md](probes.md).

## 1. What forces the full restart today

A pin move costs the owner more than a makiba update does. The record:

- A makiba update alone is `makiba down`, rebuild, `makiba up` ([installing.md](../installing.md), "Update"; `src/down.rs`). It ends the workers' terminals and suspends their runs; `up` brings them back. The secretary's terminal and the core keep running.
- A pin move adds `--secretary --stop-base` to that `down` (upgrading-the-base.md, "After the pin lands", step 1): the secretary's terminal ends, and `server.stop` ends every terminal of the core's session, a person's own included. The self-hosting week records the one such move: 7 min 19 s stopped, events 6864 to 6869 ([self-hosting-week.md](../reports/self-hosting-week.md)).

The extra cost comes from one place in makiba, not from the base:

1. `check` in `src/base/version.rs` accepts exactly `PINNED`. A new `makibad` refuses the old core and an old `makibad` refuses the new core (`checked_hello`, called once at the start of `makibad`, `src/daemon/mod.rs`). So the makiba binary and the core must change in the same stop.
2. The base itself does not need the stop. Its definition (`docs/base/hc.openrpc.json`, 0.1.22) has `server.restart`, "Planned restart (e.g. after an update): screens are saved, programs keep running, the next core adopts them", and `server.status` lists `holders`, "after an update, older ones run their terminals to the end". makiba has never measured either.

## 2. What a release outside the pin can change

What the record shows about herdr-core's releases (the release notes themselves were not read for this note: `releases.herdr.dev` is not reachable from the worker's sandbox; what is quoted is what probes.md and the pin commits recorded from them):

- A patch release has changed behaviour that no schema shows. 0.1.20 changed sends: they wait for an unfinished draft (probes.md, P3 "Why"; upgrading-the-base.md, "What a move reruns", step 3). The same method, the same parameters, the same result fields.
- A patch release has changed the shape. 0.1.22 added the `docs` parameter of `events.subscribe`, the `terminal.put_*` methods and an `unavailable` error on some `terminal.*` calls (probes.md, the P1 row of the 0.1.22 move). None of it touched `src/base/uses.rs`, and `base_compat` named nothing.
- herdr-core releases every few days and calls its API a preview (upgrading-the-base.md).

So a schema check is necessary and not sufficient: it would have passed 0.1.20, whose change to sends is the one probes P3 and P4 exist for.

## 3. The options

**A. One release (today).** Safe by the rule "no release runs that no probe has run on". Every pin move stops everything (section 1).

**B. A range of patch releases** (`>=0.1.22,<0.2.0`, with `base_compat` in CI against each release's definition). An open range admits releases that do not exist yet; CI can check only those that do, and only their shape. A release that appears after the merge is accepted by the binary already installed, with no probe and no test. It fails the rule for the reason of section 2.

**C. Any release whose live definition passes `base_compat`'s check at hello** (`rpc.discover` gives the definition over the socket). It is B with the test moved to the host: it catches every shape change at the moment it would matter, and none of the behaviour changes of section 2. It also makes the check part of every start, and a reader of JSON Schema part of `makibad`.

**B'. A short list of checked releases (proposed).** makiba accepts the pinned release and the release pinned before it, and nothing else. Each passed the whole of "What a move reruns" when it was pinned (base_compat, the suite on the real base, and each probe either run or skipped with a recorded reason), so the rule's reason holds: no accepted release is one that the pin-move checks have not passed. The list is not a range and does not grow by itself.

## 4. Proposal: B', the pinned release and the one before it

### 4.1 What it is

- `src/base/version.rs` keeps `PINNED` (the release `scripts/install-base.sh` installs, the probes run on, the stand-in answers with: unchanged meaning, unchanged line, so the script's `sed` still reads it) and adds `PREVIOUS: Option<&str>`, the release pinned before it, or none. `check` accepts `PINNED` and `PREVIOUS`. `SUPPORTED`, the text in messages, names both (`0.1.22 or 0.1.23`). No other code reads the version: `hello` is still checked once, at the start of `makibad` and in `makiba up`.
- `docs/base/` holds the definition of each accepted release: `hc.openrpc.json` stays the pinned one, `hc-previous.openrpc.json` is the one before (today's 0.1.21 definition is in git, `285a844^:docs/base/hc.openrpc.json`).
- `tests/base_compat.rs` runs `every_use_is_in_the_definition` against every accepted release's definition, and checks that each file's `info.version` is the release it stands for. This is option B's CI check, held to the list instead of an open range: a change to `src/base/uses.rs` that the previous release lacks fails here, before it merges.

Only two, not more: the suite on the real base (`MAKIBA_TEST_REAL_BASE=1`) and the probes run on `PINNED` only. Code that merges after a release stops being pinned is checked against the previous release's shape (base_compat) and not its behaviour. One release back bounds that to one pin interval (about a month by upgrading-the-base.md), and the previous release leaves the list at the next move.

### 4.2 The check that stands in for the exact pin

Three checks, all already owned by existing parts:

1. Shape, per accepted release: `tests/base_compat.rs` against each vendored definition (in CI, every run).
2. Behaviour, per accepted release: the probes of "What a move reruns", as they were dealt with when it was pinned, with their rows kept in probes.md section 4. That is a measurement or a waiver read from the changelog, not always a measurement: for 0.1.22 all five (P1, P3, P4, P5, P8) were skipped, because its changelog touched only the `--doc` filter of `hc events`. B' carries exactly the assurance the pin carries today, and no more.
3. Identity, per start: `checked_hello` (`src/base/version.rs`) refuses any release not on the list, as today.

### 4.3 Rules for the list

- A pin move sets `PREVIOUS` to the old `PINNED` and `PINNED` to the new release, in one change, with the new definition vendored and the old one moved to `hc-previous.openrpc.json`.
- A change that needs something only `PINNED` has (a method, a parameter, a behaviour) sets `PREVIOUS` to none in the same change, and says so. base_compat forces this for shape; for behaviour it is the writer's and the reviewer's duty, as the probes are today. Such a change forces the full stop of section 1 on a host still on the previous release, and its pull request says so.
- `PREVIOUS` is set to none at the latest by the next pin move (it then becomes the release before).

### 4.4 What the owner does on a base update

1. The pin-move pull request lands (the checks of "What a move reruns", unchanged).
2. The owner updates makiba as for any other merge: `makiba down`, rebuild (or install), `makiba up`. The secretary and the core keep running. The new `makibad` accepts the old core, because it is `PREVIOUS`.
3. Later, at a time with no run in flight, the owner replaces the core: `makiba down --secretary --stop-base`, `scripts/install-base.sh`, `makiba up`, the check of step 6 of today's "After the pin lands". The order matters: makiba first, then the core, because the old makiba does not accept the new core.
4. Step 3 must happen before the next pin move lands, or that move's `makibad` refuses the core (it is then two releases back), and the owner is back to today's full stop.

What does not change: step 3 is still a full stop of the core's session. What changes: it is no longer tied to the merge, and it can be put where it costs least.

### 4.5 Not in this change: replacing the core without ending terminals

`server.restart` (section 1) would let step 3 keep every terminal. It is not proposed for building now, because makiba has not measured it and it touches what makiba holds in the ledger. What a later build has to settle (build Issues 2 and 3, section 7):

- **Which core, which binary.** A bare `hc server restart` reaches the session that `HC_SESSION` names, not `$MAKIBA_BASE_SESSION` (default `makiba`), and does not read `HC_BIN`. makiba already reaches its own core without `hc`: `makiba down --stop-base` sends `server.stop` over makiba's socket (`stop` and `socket_path` in `src/base/herdr.rs`). A restart is sent over the same socket, never by a bare `hc`, and by `makibad`, not by the person's CLI process: the gate below lives in `makibad`'s memory, so only `makibad` can close it before the restart. The person asks for it with a new verb of the person's `makiba`, which reaches `makibad` through the door, as the person's other verbs do (spec section 12); `makibad` closes its gate, then sends `server.restart`. Which binary the next core runs is not in the definition: `server.restart` takes an `environment` that "requires PATH". Whether a `PATH` with the new `hc` first is enough, and whether makiba's configuration folder (`base_config_home` in `src/cli.rs`) is kept, is measured by P15, not assumed.
- **The whole sequence, with no stop.** The live update replaces step 3 of section 4.4 entirely: no `makiba down --secretary --stop-base`. It is: install the new `hc` (`scripts/install-base.sh`); makiba's restart verb; check the recorded version. The secretary's and the workers' terminals are never ended.
- **The version is checked on every reconnect, and a refusal reaches the daemon.** Today `makibad` checks the version only at its start (`src/daemon/mod.rs`). The events stream (`src/base/herdr/events.rs`) owns the reconnect: after a lost connection it subscribes again, without `hello`, and yields a `Resync`. A new core must be checked there. But `EventStream` yields only `BaseEvent` (`Event`, `Resync`; `src/base/mod.rs`) and retries forever, and when the stream ends the daemon's loop goes on (after `subscribed`, `RecvTimeoutError::Disconnected` only sleeps). So a check in `events.rs` alone cannot stop anything. The build adds two variants to `BaseEvent`: one that carries the new core's `BaseInfo` when an accepted release replaced the old one, and one that carries the `UnsupportedVersion` refusal, after which the stream ends. `makibad` writes the first to `dispatcher.base_version` and a new event, in one transaction, before it handles the `Resync` that follows. On the second it records the refusal and exits with an error, as a refusal at its start does; nothing base-dependent runs after it. The runs are then in the state a stopped `makibad` leaves, and `makiba up` brings them back once the core is on the list (`src/daemon/recover.rs`); until then `up` is refused by the same check. The version `makibad` holds in memory (`info` today) is replaced only after the ledger write.
- **No other call reaches an unchecked core.** The events stream is not the only path to the core: every other operation opens a connection of its own and sends no `hello` (`call` in `src/base/herdr.rs`), and the daemon's passes keep running while the stream waits to reconnect. A check on the reconnect alone would let a launch or a send reach an unsupported core before the refusal is handled. So the build adds one gate in `Herdr` (`src/base/herdr.rs`), shared by its operations and the events stream. Its rules:
  - *Which calls it holds.* Only the operations on terminals (`terminal.*`, `events.subscribe`). The control calls `hello`, `server.restart` and `server.stop` pass it always (a second, ungated path beside `call`), so a `hello` can open it, a restart can be sent while it is closed, and a stop is never held.
  - *Who has one.* Only the `Herdr` that `makibad` builds is gated. The `Herdr` of a CLI process (`makiba up`, `makiba down`) has none and works as today: `up` checks with its own `hello`, and `down` ends terminals and stops the core on the person's word, with no version in question.
  - *Start.* Closed when `makibad` builds it; the start's `hello` (`src/daemon/mod.rs`) opens it when it passes. A refused start `hello` exits as today.
  - *Closed* when the events connection is lost, and by `makibad` before it sends `server.restart` for the person's verb. While it is closed, a held call returns `Unavailable` (kind `unchecked`), which the daemon already treats as a base it cannot reach.
  - *Opened* only by a reconnect `hello` that passes the check. A refused `hello` leaves it closed until `makibad` exits (above).
  - *Memory and restart.* It is state in memory beside the ledger. It records nothing and is never read back, and a new `makibad` begins closed.
  - *Known limit.* A core restarted by a path other than the verb (a person's own `hc` in that session) can answer a call sent before the events connection notices the loss. The documented path is the verb only, and the gap is no wider than today's, where nothing is checked after the start.
- **Adoption.** Whether adopted terminals keep their address, labels, `turn`, status and the secretary's channel through a restart is a probe (new P15, build Issue 2), not a reading.

## 5. Decided against

- C, because a schema check does not see the changes probes exist for (section 2), and moving it into `makibad` adds a JSON Schema reader to every start for that.
- B, because an open range admits releases no check has seen.
- Keeping A and making the stop cheaper only (a script for "After the pin lands"): it would shorten the stop, not move it away from the merge.
- Following the Claude Code rule (no upper bound, refuse by name, [upgrading-claude-code.md](../upgrading-claude-code.md)): that rule holds because Claude Code updates itself and a person cannot choose its version. The base is installed by makiba's own script at the pinned release, so the reason does not carry over.

## 6. The existing rule: an open question for the owner

upgrading-the-base.md says the range stays one patch release and is revisited "only after three pin moves in a row needed nothing but the number changed". One move is on record (0.1.21 to 0.1.22, which needed only the number and test literals). B' does not widen the range to releases that no probe has run on, which is that section's reason; it keeps one release more. **Open question for the owner, not decided:** is that section replaced by B'? Nothing in this note changes it. The question is put to the owner when build Issue 1 is opened, and Issue 1 is built only on a yes.

## 7. Build Issues

1. **Accept the pinned release and the one before it.** Only on the owner's yes to the open question of section 6. `src/base/version.rs` (`PREVIOUS`, `check`, `SUPPORTED`); `docs/base/hc-previous.openrpc.json` (the 0.1.21 definition from git); `tests/base_compat.rs` (each accepted definition, its `info.version`); existing tests whose literals name the one-release range: `tests/base_version.rs` (the `>=0.1.22,<0.1.23` text and the refused examples), `tests/base_fixture.rs` (refused example); docs: upgrading-the-base.md (replace "The range stays one patch release"; split "After the pin lands" into the makiba step and the later core step of section 4.4), `docs/runbooks/first-worker.md` (the range line), probes.md (the rule naming the pinned version). No database schema change, no new event kind, no change to the spec's event table, `scripts/install-base.sh` unchanged. The first pin move after it lands sets `PREVIOUS` to 0.1.22; until then `PREVIOUS` is 0.1.21, which this repository ran on for a week (self-hosting-week.md).
2. **Probe P15: a core restart under a running makiba.** Written into probes.md (its section and its Results row) and run by the owner, before Issue 3 starts. On a scratch session with makiba running a secretary and one worker: install a newer `hc`, and send `server.restart` over makiba's socket with an `environment` whose `PATH` finds the new `hc` first. Pass when all of these hold: the core that answers `hello` afterwards is the new release; every terminal of the session is still listed with the same address, labels and `turn`, and its program kept running (the same pid); the secretary's channel still delivers; the events stream reconnects and its `Resync` lists the same terminals; makiba's configuration folder is the one in use. Also measured: what happens when the new core fails to start (whether the old core's programs survive, and what is left to recover), and how long the socket goes unanswered. A fail ends Issue 3 before it starts and is recorded with the screen and the core's log.
3. **Replace the core under a running makibad** (after Issues 1 and 2, only if P15 passed). A verb of the person's `makiba` that asks `makibad` through the door to restart the core (`src/cli.rs`, the door's handler in `src/daemon/door.rs`, the verb table of spec section 12 in `docs/design/spec.md`); `makibad` closes its gate and sends `server.restart` over makiba's own socket with the `environment` P15 settled (the `Base` trait in `src/base/mod.rs`, beside `stop`; `src/base/herdr.rs`; the stand-in `src/base/stub.rs`); `events.rs` sends `hello` on each reconnect; the two new `BaseEvent` variants of section 4.5 (`src/base/mod.rs`) and their handling in `src/daemon/mod.rs` (the new version written to `dispatcher.base_version` with a new event before the `Resync`; on a refusal, exit with an error); the gate of section 4.5 in `src/base/herdr.rs`, with its rules (which calls it holds, only `makibad`'s `Herdr`, start, close, open); the new event kind in the spec's event table (`docs/design/spec.md`, `src/spec.rs`, `tests/spec_matches_code.rs`); tests on the stand-in for an accepted change and for a refused one; tests of the gate at start, on reconnect, through the person's verb from the CLI to `makibad` to the core, and at `makiba down`, showing that no terminal operation reaches the core between a lost connection (or the verb) and a passing `hello`, and that `hello`, `server.restart` and `server.stop` are never held; upgrading-the-base.md replaces step 3 of section 4.4 with the whole live sequence of section 4.5.
