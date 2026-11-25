# What bounds the real-base job (Issue 484)

Decision note for Issue 484. It names what makes the `real-base` job of `.github/workflows/ci.yml` slow and picks how to run it faster, without dropping a test from the real base and without adding a job. Goal clause G2: the flow of work does not wait for the person's turn; every pull request and merge-queue commit waits for this job.

Status: the person approved this design (option B1, cargo-nextest in the `real-base` job only) on 2026-11-25.

## 1. What is true today

### 1.1 The job

- The step "Suite on the real base" runs `MAKIBA_TEST_REAL_BASE=1 cargo test --locked -- --test-threads=4` (PR 483, Issue 477, which gave each `Real` fixture a session and place of its own: `makiba-test-{pid}-{n}`, `mk-{pid}-{n}`). `cargo test` runs the test binaries one after another; `--test-threads` acts only inside one binary.
- **The baseline is PR 483, a prerequisite.** This note was written on a checkout of main at 3ef7435, which does not yet hold PR 483: there `ci.yml` still runs `--test-threads=1`, `tests/common/mod.rs` makes `mk-{pid}` and session `makiba-test-{pid}`, and `tests/base_fixture.rs` asserts the pid-only session. What this note says of the fixtures (per-test sessions and places) and of the run times is read from PR 483's branch (`origin/makiba/real-base-477`, d232aab: `ci.yml`, `tests/common/mod.rs`, `tests/base_fixture.rs`) and from its CI run below. The build Issue of section 4 starts only on a main that holds PR 483.
- Run 45441695999 (job 138614278593, ubuntu-latest): the step took 9 min 00 s, of which the build about 1 min 00 s and the test run about 8 min. Before PR 483 (`--test-threads=1`, run 45422781838) the test run took about 10 min 10 s.

### 1.2 Who uses the real base

Only four test binaries read `MAKIBA_TEST_REAL_BASE` (`grep -lE 'real_wanted|common::base\(|running_base\(|Real::start|MAKIBA_TEST_REAL_BASE' tests/*.rs`): `base_fixture`, `base_compat`, `fake_agent` and `real_base_wire`. Every other binary, the twelve longest except `real_base_wire` among them, runs on the stand-in (`MAKIBA_BASE=stub`, `common::local::Local`, a stand-in `hc` script) in this job exactly as in the `tests` job. So the job's time is, almost all of it, the stand-in suite run binary after binary. The local times of section 1.3 (the stand-in, a 5-core WSL2 machine) agree with the real-base log within about 1.5 s for every binary but `build_skills_path` (a compile, which depends on the machine) and `binary_replaced` (two of its tests cannot run in this sandbox).

This note does not change what runs in the job (the Issue rules that out); it only changes how it is run.

### 1.3 The longest binaries and where their time goes

Real base: "finished in" of the log above (secretary's measurement, Issue 484 comment). Local: `cargo test --locked --test <n> -- --test-threads=4` on the stand-in, this worktree at 3ef7435 with `tests/common/mod.rs` of PR 483 not yet merged locally (no binary below uses `Real`, so it does not matter). The real-base log has no per-test times: stable `cargo test` prints none, so the per-test column is local. Alone: each test run by itself (`-- --exact`), local, wall time of the `cargo test` call (about 0.2 s of it is cargo).

| Binary | Real base | Local | Tests | Where the time goes |
|---|---|---|---|---|
| `build_skills_path` | 45.3 s | 30.0 s | 1 | Two `cargo build --offline --bin makiba` into a fresh target folder: the first compiles every dependency. CPU bound (all cores). |
| `locked_write` | 31.0 s | 33.3 s | 3 | Serial: the tests hold `static ENV: Mutex` (they set process variables and run makibad in process). Alone: 22.1 s, 9.7 s, 0.5 s. Fixed: a write lock held `BUSY + 2 s` = 7 s, and 20 s in `a_lock_that_outlasts_the_retry_records_the_loss`; 1.5 s settles. The busy timeout (5 s, `src/db/mod.rs`) has no seam. |
| `heartbeat` | 30.1 s | 29.7 s | 4 | Parallel (no lock). Bound by `status_fails_within_30_s_of_a_sigkill`: `makiba status` fails only when the heartbeat is older than `STALE` = 30 s (`src/status_local.rs`); `sigterm_stops_cleanly...` waits a second beat (`HEARTBEAT` = 10 s, `src/daemon/mod.rs`). No seam for either. Alone: 30.2 s, 10.4 s, 4.2 s, 0.3 s. |
| `up_ledger_busy` | 30.1 s | 30.2 s | 2 | Parallel. Bound by `up_on_a_ledger_held_past_its_bound_says_the_ledger_is_busy`: `makiba up` waits `LEDGER_WAIT` = 30 s (`src/up.rs`), no seam; the other test holds the lock `HELD` = 8 s. Alone: 30.8 s, 8.5 s. |
| `renew` | 26.2 s | 27.2 s | 3 | Serial (`ENV` lock). Fixed: the slow agent's 6 s start (`SLOW`), a 5 s look for a third relaunch, verify timeouts of 3 s; the rest is polling makibad's one-second passes. Alone: 9.8 s, 9.5 s, 8.7 s. |
| `curator_new_notes` | 25.6 s | 26.8 s | 3 | Serial (`ENV` lock). Three `SETTLE` = 2.5 s sleeps, a 5 s curator limit, a 2 s verify timeout (seams already set short); the rest is makibad's one-second passes. Alone: 11.4 s, 10.7 s, 5.6 s. |
| `binary_replaced` | 23.3 s | 30.3 s (2 of 6 failed here: no Unix socket in this sandbox) | 6 | Parallel. Each test copies the debug `makiba` (about 90 MB) and makibad hashes it (SHA-256, unoptimized build) at start and after each change; two tests sleep 5 s ("five looks more"). Alone: 15.8 s (the one that raises it and then looks 5 s more), 7.1 s, 6.7 s, 5.4 s; the two door tests need a Unix socket and timed out here at 30 s. |
| `git_lookup_deadline` | 20.1 s | 20.3 s | 2 | Alone: 20.2 s, 0.2 s. Bound by `a_git_whose_child_keeps_stdout_open_is_ended_at_the_deadline`: the door's `ISSUE_READ_TIMEOUT` = 20 s (`src/daemon/door.rs`), no seam. |
| `curator` | 20.1 s | 20.2 s | 3 | Serial (`ENV` lock). Settles of 3 s and 2.5 s ×3, a 6 s curator limit. Alone: 10.7 s, 7.0 s, 3.3 s. |
| `real_base_wire` | 16.0 s | 0 s | 4 | Real base only (returns at once on the stand-in). Bound by `events_subscribe_sends_a_snapshot_then_notices_and_heartbeats`: a scripted 6 s + 10 s run of two terminals, made to show every status. |
| `curator_end` | 15.5 s | 15.5 s | 2 | Serial (`ENV` lock). Alone: 9.0 s, 7.2 s. Two 2.5 s settles, makibad's passes. |
| lib unittests | 12.7 s | – | 191 | Not read further. |
| `belt_stops` | 12.5 s | 12.6 s | 5 | Parallel. Alone: 12.6 s, the others 0.7–2.5 s. Bound by the budget test: `budget_hours` 0.003 = 10.8 s, then a sleep to 12 s. |

The rest, about 160 binaries, each under 10 s, add up to about 170 s (8 min less the 308 s above).

So the job is bounded by two things, not by one slow test:

1. **Binaries one after another.** The sum of all binaries is the job. No single binary is longer than 45 s.
2. **Tests one after another inside a binary.** In five of the eleven longest stand-in binaries (`locked_write`, `renew`, `curator_new_notes`, `curator`, `curator_end`; twelve binaries in all hold such a lock) the tests take a process-wide `ENV` mutex, because they set process variables and run makibad in the test process. `--test-threads=4` cannot run them at once; this is why PR 483's four threads gave about 20 %, not four times.

## 2. Options

### B1. cargo-nextest

nextest runs every test in a process of its own, as many at once as `--test-threads` (`-j`) allows, across all binaries.

- **Both bounds go.** Binaries no longer wait for each other, and a test that holds `ENV` holds it in its own process, so the twelve lock-holding binaries run their tests at once too. The bound becomes the longest single test (about 30–45 s: `build_skills_path`, `up_ledger_busy`'s 30 s, `heartbeat`'s 30 s) or the total test time divided by the slots, whichever is larger.
- **The fixtures allow it.** Every real base is per process and per test: `Real` uses `makiba-test-{pid}-{n}` and `mk-{pid}-{n}`, `base_compat` uses `makiba-test-{pid}` and `mk-compat-{pid}`; the knowledge folder is `makiba-knowledge-{pid}`; makibad runs with `MAKIBA_PORT=0` or a port from `bind("127.0.0.1:0")`; every state folder is a `tempfile` folder. No test names a fixed port, path or session. Statics that a test process shares (`OnceLock` homes, `ENV`) are per process and so per test under nextest.
- **What it needs.**
  - A tool, not a dependency: `Cargo.toml` and `Cargo.lock` do not change. The workflow uses only `actions/*` actions today, so no third-party install action: a step downloads one pinned nextest release (a version and its SHA-256 written in the workflow or a script, the way `scripts/install-base.sh` installs the pinned base) into `~/.cargo/bin`. About 5 s. `cargo install cargo-nextest --locked` would compile it (minutes) on every cache miss.
  - `ci.yml`, job `real-base`, step "Suite on the real base": `MAKIBA_TEST_REAL_BASE=1 cargo nextest run --locked --no-fail-fast` in place of `MAKIBA_TEST_REAL_BASE=1 cargo test --locked -- --test-threads=4`, with `export HC_BIN="$HOME/.makiba-base/bin/hc"` and `"$HC_BIN" --version` kept before it. nextest passes its environment to every test process, so both variables reach the fixtures as they do today; without `MAKIBA_TEST_REAL_BASE=1` the four real-base binaries would pass on the stand-in (`real_base_wire` returns at once) and a count of tests would not show it. Doctests: nextest does not run them, and the crate has none today (the only fenced blocks in `src/` are `text`); the step keeps `cargo test --locked --doc` so that a later doctest is not dropped silently.
  - `.config/nextest.toml`: a CI profile with `test-threads` (8, see below), `threads-required = 4` for `build_skills_path` (it compiles on every core; under contention it would slow, and so would every timing-sensitive test beside it), `slow-timeout` so that a hang is named, no `retries` (a flaky test must stay red), and a JUnit report kept as an artifact. The JUnit report gives per-test durations of every real-base run, which is option A done once for all.
  - The negative step ("The real base is what ran") runs the runner the suite runs: `MAKIBA_TEST_REAL_BASE=1 HC_BIN=/nonexistent cargo nextest run --locked --test base_fixture`, which must fail with "the real base is the pinned release" as today. So a missing `hc` still fails the fixture under nextest, and a run on the stand-in cannot pass for one on the real base.
  - A positive check that the real base ran: `real_base_wire`'s `events_subscribe_sends_a_snapshot_then_notices_and_heartbeats` takes at least 16 s on the real base (its scripted runs) and 0 s on the stand-in. The step reads its duration from the JUnit report and fails below 10 s.
- **Risks.**
  - More at once than today: up to 8 test processes instead of 4 threads. Assertions on elapsed time (`heartbeat`: the row before a 4 s delay; `git_lookup_deadline`: refused within 5 s; `locked_write` and `up_ledger_busy`: a lock held past 5 s) were written for at most four tests at once in one binary. They bound time from one side with margins of seconds, and most tests sleep rather than compute, but the build Issue runs the job at least three times and reads the slowest tests before the change is taken.
  - The free-port pattern (`bind("127.0.0.1:0")`, drop, pass the port) races with any other process that binds at that moment; more processes make it a little likelier. It exists today with four threads.
  - The `tests` job keeps `cargo test`; the two jobs then run the same tests by two runners. A test that passes under one and not the other shows a real dependence on its neighbours, which is worth knowing.
- **Expected time.** The longest test bounds it below at about 45 s (`build_skills_path` on GitHub's runner). The total of all test times: the lock-holding binaries add what they add today (their tests already ran one after another); a binary that ran its tests four at once adds the sum of its tests, between its time today and four times it (`heartbeat` 45 s instead of 30 s, `binary_replaced` about 35 s instead of 23 s). So the total is about 9–12 min of test time, most of it sleep or polling, plus a few milliseconds of process start for each of about 1250 tests. Over 8 slots that is about 70–90 s, of the order of the longest test; with contention for the 4 cores, about 1.5–2.5 min of tests. The step: about 3–3.5 min (build 1 min, install seconds) instead of 9 min. This is an estimate from the times above; the build Issue measures it.

### B2. A script that starts several test binaries at once

A script lists the test binaries (`cargo test --no-run --message-format=json`), runs N at a time (`xargs -P`), each with `--test-threads=4`, keeps each output and fails if one failed.

- No new tool, but a runner of our own: the working folder and variables `cargo test` sets for a test binary (`CARGO_MANIFEST_DIR`, `CARGO`, the package root as cwd), the collection of results, the naming of a failure. nextest does all of this.
- Bound 2 stays: the lock-holding binaries still run their tests one after another (`locked_write` 31 s, `renew` 26 s, `curator_new_notes` 26 s). The bound is max(45 s, total / N), and with N = 4 the binaries of 25–45 s pile up in the first slots. Expected: about 2.5–3.5 min of tests.
- The same fixture facts as B1 hold, and the same need to pass `MAKIBA_TEST_REAL_BASE=1` and `HC_BIN` to every binary; the same timing risk at N binaries × 4 threads.

### C. Shorten the long tests

Seams (variables read only by tests, like `MAKIBA_WRITE_RETRY_MS`, `MAKIBA_CURATOR_TIMEOUT_MS`) for the fixed waits without one:

| Wait | Seam | Saves | What it checks after |
|---|---|---|---|
| `LEDGER_WAIT` 30 s (`up.rs`) | `MAKIBA_UP_LEDGER_WAIT_MS` = 10 000 | 20 s | Same relation (held 8 s > busy 5 s, < bound); the 30 s figure itself is not checked today either. |
| `ISSUE_READ_TIMEOUT` 20 s (`door.rs`) | `MAKIBA_ISSUE_READ_TIMEOUT_MS` = 2000 | 18 s | Same: the child is ended at the deadline, the answer comes before the door's wait. |
| `STALE` 30 s, `HEARTBEAT` 10 s | two seams | ~25 s | Weaker: "status fails within 30 s" is a spec figure, and a scaled seam no longer checks it. |
| `locked_write` 20 s hold | none (test only: `BUSY + 2 s`) | 13 s | The same, with a 2 s margin instead of 15 s over the busy timeout. |
| `belt_stops` 0.003 h | none (test only: 0.001 h) | 7 s | The same if the first task is prepared within 3.6 s. |
| `build_skills_path` | – | 0 | Two real builds are what it checks. |

Alone, C saves about 80–90 s of 8 min (to about 6.5 min of tests): it touches the binaries the sum is made of, but the sum stays the bound. After B1 it saves nothing that counts while `build_skills_path` (45 s) is the longest test, and each seam is production code that only tests read.

## 3. Recommendation

**B1, cargo-nextest, in the `real-base` job only, with `MAKIBA_TEST_REAL_BASE=1` and `HC_BIN` as today, 8 test slots and `build_skills_path` taking 4 of them, once PR 483 is in main.** Expected: the step from 9 min to about 3–3.5 min (tests about 1.5–2.5 min). It removes both bounds of section 1.3 without changing a test, and it records per-test durations in every run. B2 removes only the first bound and leaves us a runner to keep. C is not taken now: after B1 it buys little, and the heartbeat seams would weaken a spec check; it stays open for a test that becomes the critical path later.

The `tests` job is not changed here; it has the same shape and could take the same step later, once B1 has run on the real base for a while.

## 4. Proposed build Issues

1. **The real-base job runs the suite with cargo-nextest.** Prerequisite: PR 483 is in main. `ci.yml` job `real-base`: a step that installs a pinned cargo-nextest release (version and SHA-256 checked, no third-party action); "Suite on the real base" keeps `export HC_BIN="$HOME/.makiba-base/bin/hc"` and `"$HC_BIN" --version`, then runs `MAKIBA_TEST_REAL_BASE=1 cargo nextest run --locked --profile ci --no-fail-fast`, then `cargo test --locked --doc`, then fails if `real_base_wire`'s events test took under 10 s in the JUnit report; the negative step runs `cargo nextest run --locked --test base_fixture` with `HC_BIN=/nonexistent` and keeps its check of the panic text; `.config/nextest.toml` with the `ci` profile (8 slots, `threads-required = 4` for `build_skills_path`, `slow-timeout`, no retries, JUnit). Done when: three runs of the job on a pull request pass, the step's time and the ten slowest tests of each are written in the pull request, the count of tests run equals that of `cargo test` (none dropped), and a run with `MAKIBA_TEST_REAL_BASE` removed from the suite step fails the duration check (shown once, then put back).
2. **(Only if Issue 1's runs show it)** a test whose timing assertion fails under nextest is given the margin it needs, one Issue per test, with the measured cause. Not filed in advance.
