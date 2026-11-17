#!/usr/bin/env python3
"""Build issues/*.md and issues/index.md from the table below.

Run from the repository root:  python3 tools/build_issues.py
The Issue files are generated; change the table here, not the files.
"""
from __future__ import annotations

from pathlib import Path

GOALS = {
    "G1": "G1 One contact: the person deals with the secretary only",
    "G2": "G2 Past incidents do not come back",
    "G3": "G3 No model touches a terminal or the ledger directly",
    "G4": "G4 Dispatch and close record what was observed",
    "G5": "G5 The watch does not stop, and if it stops it shows",
    "G6": "G6 The base is a part that can be replaced, and an update of it does not break makiba",
    "G7": "G7 It runs its own development",
}

MILESTONES = {
    "M0": "Foundation",
    "M1": "Base module",
    "M2": "Monitoring",
    "M3": "Dispatch",
    "M4": "Boundary and a real worker",
    "M5": "Close and periphery",
    "M6": "Secretary",
    "M7": "Self-hosting",
}

CJ = "claude-org-ja"
PD = "paddock"
RT = "claude-org-runtime 0.1.58"

# id, title, why, build (list), carry (list), test path, test asserts (list), goal, deps, extra (dict)
I = []


def add(id, title, why, build, test, asserts, goal, deps=(), carry=(), out=(), note=None, gate=True,
        replaces=None):
    I.append(dict(id=id, title=title, why=why, build=list(build), test=test, asserts=list(asserts),
                  goal=goal, deps=list(deps), carry=list(carry), out=list(out), note=note, gate=gate,
                  replaces=replaces))


# A body that carries this heading belongs to an Issue that was closed by a merged pull request and is
# reworked: tools/create_issues.py --update lists it for reopening.
REPLACES = "## Replaces what was merged"


# ---------------------------------------------------------------- M0
add("M0-1", "Repository skeleton, CI and the acceptance job",
    "Nothing can be merged test-first until the repository can run a test in CI and say whether a declared "
    "test failed at the test commit and passed at head. makiba is written in Rust (plan.md section 5), so "
    "that test is a Cargo integration test.",
    ["`Cargo.toml`: one package `makiba`, a library (`src/lib.rs`) and one binary `makiba` (`src/main.rs`, "
     "which prints its usage for now), with the crates of plan.md section 5.1 as they are first needed and no "
     "other. `Cargo.lock` is committed. `rust-toolchain.toml` pins one stable Rust release, 1.89 or later "
     "(rusqlite's minimum), with the components `clippy` and `rustfmt`. The layout is spec section 14.",
     "Remove the Python package skeleton merged by #56: `pyproject.toml` and `makiba/__init__.py`. "
     "`makiba/db/` and its Python tests stay until M0-5 removes them.",
     "`.github/workflows/ci.yml`, the job names unchanged: `tests` runs `cargo test --locked`, then "
     "`python3 -m pytest -q tests` for the development tools' tests (and `tests/db/` until M0-5); `lint` runs "
     "`cargo fmt --check` and `cargo clippy --locked --all-targets -- -D warnings`, and keeps the Python tool "
     "checks it already runs; ruff is dropped, since the Python left is development tools it already skipped; "
     "`acceptance` installs the toolchain of `rust-toolchain.toml` and Python, and runs the checker as now.",
     "`tools/check_language.py` scans `src/` where it scanned `makiba/` (M0-7). It stays a Python development "
     "tool, run by CI and not shipped.",
     "`scripts/acceptance-check.sh <base> <head>`: the rule in Notes. Rules 1, 2 and 4 are kept as they are; "
     "rule 3 learns Cargo integration tests; a declared `tests/**/test_*.py` is still run with pytest, for the "
     "development tools.",
     "`docs/acceptance-check.md`: replaced by the text in Notes.",
     "`.github/acceptance-check` keeps the one line `acceptance`."],
    "tests/test_acceptance_check.py",
    ["In a scratch git repository holding a small Cargo package: a branch whose first commit declares "
     "`tests/<name>.rs`, failing by an assertion, and whose head makes it pass yields `ACCEPTANCE: pass`.",
     "A declared test that does not compile at the test commit (it calls a function head adds) counts as "
     "failing there, and the branch yields `ACCEPTANCE: pass` when head passes.",
     "A declared test file with no `#[test]` at the test commit (`running 0 tests`) yields `ACCEPTANCE: fail`.",
     "A declared test that already passes at the test commit yields `ACCEPTANCE: fail`.",
     "A declared test with an `#[ignore]` test at head, or with no test run at head, yields `ACCEPTANCE: fail`.",
     "A declared path that is not a test target (`tests/common/mod.rs`) yields `ACCEPTANCE: fail`.",
     "A declared `tests/test_x.py` behaves as before (fails by pytest at the test commit, passes at head).",
     "A branch with no trailer yields `ACCEPTANCE: none`."],
    "G7",
    carry=[f"{PD}: `scripts/acceptance-check.sh`, `tests/acceptance-check.test.sh`, `.github/acceptance-check`, "
           "`docs/design/acceptance-check.md`",
           "makiba itself: `scripts/acceptance-check.sh`, `tests/test_acceptance_check.py`, "
           "`docs/acceptance-check.md` (merged in #56), changed as above"],
    replaces="Issue #3 was closed by pull request #56, which built this skeleton in Python: `pyproject.toml`, "
             "`makiba/__init__.py`, a checker that runs declared tests with pytest, and CI jobs running pytest and "
             "ruff. The owner decided on 2026-11-18 that makiba is implemented in Rust (plan.md section 5). This "
             "Issue is reopened and replaces that skeleton: the job names, `.github/acceptance-check` and rules 1, "
             "2 and 4 of the checker stay; the Python package goes; rule 3 runs Cargo tests.",
    note="Replacement text for `docs/acceptance-check.md`, rule 3 and what it needs:\n\n"
         "**Test targets.** A declared path is a test target: a file `tests/<name>.rs` directly under `tests/`, "
         "or a folder `tests/<name>/` holding `main.rs` (or that `main.rs` itself); its target name is `<name>`. "
         "For the development tools, a file `tests/**/test_*.py` is also a declared test. A declared path that is "
         "neither, at the test commit, fails the check. Test helpers are the other files under `tests/` "
         "(`tests/common/`, fixtures, the test agent's source): the test commit may add them, and rule 4 protects "
         "them.\n\n"
         "**How a target runs.** In each of the two worktrees (test commit and head), with "
         "`CARGO_TERM_COLOR=never`: `cargo fetch --locked` first, so that a download failure is not mistaken for "
         "a test result; then `cargo test --locked --test <name>`, with stdout and stderr kept. With `--test` "
         "Cargo builds the library, the binaries and that one test, and runs no other test and no doc test, so the "
         "output holds at most one `running N tests` line and one `test result:` line.\n\n"
         "**Fails at the test commit** when either holds: (a) the output contains ``error: could not compile "
         "`makiba` (test \"<name>\")``: the test does not compile, as an import error did in Python; only "
         "this target, since the test commit changes nothing but tests and the base builds; (b) the output "
         "contains `running N tests` with N at least 1 and Cargo exits non-zero (a failed assertion, a panic, or "
         "a crash of the test binary). Anything else does not count as failing and fails the check: exit 0 "
         "(the test passes), `running 0 tests` (nothing ran, as pytest's exit 5), or a non-zero exit with "
         "neither (a fetch, lock-file or library build failure: the check prints the last lines and says it could "
         "not run the test).\n\n"
         "**Passes at head** only when Cargo exits 0 and the `test result: ok.` line reads P passed, 0 failed "
         "and 0 ignored, with P at least 1. An ignored test at head fails the check; so does a target that runs "
         "no test.\n\n"
         "**A Python test** (`tests/**/test_*.py`) runs as before: `python3 -m pytest <path>` from the tree root; "
         "exit 5 (no test collected) is not failing.\n\n"
         "**Known limits.** A test that needs a new dev-dependency cannot add it in the test commit (rule 2), so "
         "it fails to compile there for that reason and counts as failing; the reviewer sees it in the log. "
         "`rust-toolchain.toml` is not protected by rule 4; a change to it after the test commit is a review "
         "matter. The others of the current text stay (a pull request without a declaration passes as `none`; "
         "the workflow file is read from the pull request).\n\n"
         "Branch protection (require `acceptance`, `tests`, `lint`; require branches up to date) is a setting only "
         "the owner can make. The checker of the base runs this pull request's own test commit, and that checker "
         "runs `tests/test_acceptance_check.py` with pytest, so this rework can go through the acceptance gate.")

add("M0-2", "Pin the base's version and the supported Claude Code versions",
    "herdr-core releases every few days and calls its API a preview. makiba must run against one known "
    "version and refuse any other. The pin is not meant to follow the releases: moving it costs a test run and "
    "several probes, so it moves on purpose and rarely.",
    ["`src/base/version.rs`: the supported range (`>=0.1.21,<0.1.22`, one patch release; the version the "
     "design was read against and the probes of `docs/design/probes.md` are run on) and a check of the version "
     "`Base::hello()` returns.",
     "`scripts/install-base.sh`: installs the pinned release into a given prefix; used by CI and by a person. It "
     "reads the base's release manifest, `https://releases.herdr.dev/core/<version>/manifest.json` "
     "(`HERDR_RELEASES_URL` replaces the host), whose `targets` give a `url` and a `sha256` for each of "
     "`linux-x86_64`, `linux-aarch64`, `macos-aarch64`, `macos-x86_64` and `windows-x86_64`. It picks the "
     "machine's target (makiba does not run on Windows, so the last is never picked), downloads its `url` with "
     "`curl`, and installs nothing unless the file's SHA-256 equals the manifest's.",
     "`docs/upgrading-the-base.md`: when the pin moves, and what a move costs. It moves when a release carries "
     "something makiba needs, when the pinned release can no longer be installed, or at the owner's monthly "
     "look at the base's changelog; not on every release, and not more than about once a month. A move reruns: "
     "M1-5's test against the new release's own definition; the whole suite on the stand-in and on the real "
     "base; probes P1, P3, P4, P5 and P8 on the new version (they measure how the base is reached, typed into and "
     "launched, and how a turn start shows in its status, which a patch release has changed before: 0.1.20 "
     "changed sends), each a few hours of the "
     "owner's time; P2 reruns itself in CI. A probe may be skipped only when the changelog shows nothing "
     "touching its area and the record says so. P6 is not rerun.",
     "The range stays one patch release. A wider range would let a version in that no probe has run on. It is "
     "revisited only after three pin moves in a row needed nothing but the number changed.",
     "`src/agents/version.rs`: the supported range of Claude Code and a check of the text `claude --version` "
     "prints. The range runs from the lowest to the highest version that a recorded run of the probes covers "
     "(the `claude --version` column of `docs/design/probes.md`, Results; paddock's measurements, 2.1.319 to "
     "2.1.322, until the new ones exist), and no wider. It is a separate constant from the base's, because "
     "Claude Code is not behind the seam and updates itself.",
     "`docs/upgrading-claude-code.md`: when the range moves and what a move reruns. Claude Code releases more "
     "often than the base, so the range moves only after the owner has rerun the following on the new "
     "version: probes P1, P3, P4, P5 and P7 to P11; paddock's carried probes (probes.md section 1); the runbook of "
     "M4-5 once; and P6 only when `secretary.remote_control` is on. The startup-dialog and prompt tests of "
     "M3-5 and M6-8 use the test agent and cannot show a change in the real screens; these reruns can."],
    "tests/base_version.rs",
    ["A version inside the range is accepted.",
     "A version outside it is refused (`BaseError::UnsupportedVersion`) before any other call is made, with a "
     "message naming both versions.",
     "`docs/upgrading-the-base.md` names M1-5's test file and each of P1, P3, P4, P5 and P8.",
     "A `claude --version` text inside the range is accepted; one outside it, and one that cannot be parsed, "
     "are refused with a message naming the version found and the range.",
     "`docs/upgrading-claude-code.md` names each probe and the runbook listed above.",
     "`scripts/install-base.sh`, with `HERDR_RELEASES_URL` naming a scratch folder that holds a manifest and a "
     "file, installs the file when its SHA-256 matches, and installs nothing and exits non-zero when it does not."],
    "G6", deps=["M0-1"],
    note="Claude Code's version goes here and not in a new Issue because it is the same kind of pin, kept in "
         "the same place and checked by the same kind of test; the check at `makiba up` is M4-4's. makiba's own "
         "releases take the base's shape (one binary per target, a manifest with each `url` and `sha256`); that "
         "belongs to stage 2's installation (plan.md section 2), not to this Issue.")

add("M0-3", "A stand-in for the base, and the real base for CI",
    "claude-org-ja's workers cannot start a herdr-core inside their sandbox (paddock inventory, section 2: "
    "`cannot start core: Read-only file system`). Workers need something to "
    "test against locally; CI needs the real thing.",
    ["`src/base/stub.rs`: an in-process implementation of the `Base` trait (spec section 6.1) with "
     "scripted terminals. It is the reference for what the interface means.",
     "`tests/common/mod.rs`: a `base()` helper the tests share. By default it returns the stand-in. With "
     "`MAKIBA_TEST_REAL_BASE=1` it starts the real base in a place of its own (`HC_SESSION=makiba-test-<pid>` "
     "and scratch XDG folders) and stops only that, when the returned value is dropped.",
     "CI runs the suite twice: `cargo test --locked` with the stand-in, and again with `MAKIBA_TEST_REAL_BASE=1`.",
     "A check that a real-base run never touches the place a person works in."],
    "tests/base_fixture.rs",
    ["The same test (launch a terminal that prints a line, read the line, end it) passes on the stand-in and on "
     "the real base.",
     "With the real base, the fixture's place is never the default one and its folders are under a temp dir."],
    "G6", deps=["M0-2"],
    carry=[f"{PD}: `tests/hc-stub.sh` (as a reference for which behaviours a stand-in needs, not as code)"],
    note="Probe P2 (docs/design/probes.md) has been run before this Issue starts and decided whether the real "
         "base can run in GitHub's CI. If it did not, this Issue ships the stand-in half and says so in "
         "`docs/design/probes.md`.")

add("M0-4", "The test agent",
    "Every test through M3 needs an agent that behaves on command: reports status, takes input, shows the "
    "dialogs a real agent shows.",
    ["`src/agents/fake.rs` (the `Agent` for the test agent) and `tests/common/fake_agent.rs`, the program it "
     "launches, built as the binary `makiba-fake-agent` by a `[[bin]]` entry in `Cargo.toml`. Tests find it "
     "through `CARGO_BIN_EXE_makiba-fake-agent`; a release ships only `makiba`.",
     "The program follows a script given on its command line: emit OSC 7501 records (`idle`, `working`, "
     "`blocked` with a kind, `done`, `error`), wait for a line of input, print a startup dialog with a cursor "
     "line, print a permission prompt, exit with a code, or hang.",
     "It records every line and key it receives to a file the test reads."],
    "tests/fake_agent.rs",
    ["A script of `working, blocked(permission), done` is seen in that order through `Base::get`.",
     "Keys sent to a scripted dialog arrive in the record file in order."],
    "G4", deps=["M0-3"],
    carry=["herdr-core: `fake_tui.py` in its own tests (as a reference)"])

add("M0-5", "Carry the state.db schema and writer, and add migration v4",
    "The records are the part of claude-org-ja worth keeping unchanged: runs, events and the delivery ledger "
    "with its idempotency key.",
    ["`src/db/schema.sql` and `src/db/schema_v4.sql`: moved unchanged from `makiba/db/` and embedded in the "
     "binary with `include_str!`. They are the SQL makiba runs; the Rust code does not restate them.",
     "`src/db/mod.rs`: opens `state.db` with rusqlite (SQLite bundled into the binary) with the pragmas "
     "`makiba/db/__init__.py` sets, and brings a database to v4 as its `migrate` does (the carried schema, its "
     "in-place upgrades, then v4 in one transaction).",
     "`src/db/writer.rs` and `src/db/queries.rs`: the parts of `makiba/db/writer.py` and `queries.py` that "
     "makiba uses, ported: transactions; projects, runs and their status; worker folders; the session row; "
     "events; the delivery ledger with its idempotency key (pending deliveries, an attempt, delivered, failed); "
     "active runs, a run by task id, recent events. Their test cases, ported, in `tests/db_writer.rs` and "
     "`tests/db_queries.rs`.",
     "Remove the Python merged by #58: `makiba/db/*.py`, `tests/db/` and `tests/test_schema_v4.py`.",
     "Migration v4 is the tables of spec section 5 (`agents`, `commands`, `run_launch`, `messages`, `pending`, "
     "`pr_watches`, `dispatcher`), already in `schema_v4.sql`.",
     "The carried contract documents `docs/contracts/state-semantics-contract.md` and "
     "`delegation-lifecycle-contract.md` stay as merged."],
    "tests/schema_v4.rs",
    ["A fresh database migrates to v4 and has every table and CHECK of spec section 5.",
     "A second `(source_event_id, recipient)` delivery row is refused.",
     "A `run_launch` row with an unknown `stage` is refused."],
    "G4", deps=["M0-1"],
    carry=["makiba itself (merged in #58): `makiba/db/schema.sql` and `schema_v4.sql` (kept as SQL), "
           "`writer.py`, `queries.py` and `tests/db/` (ported)",
           f"{CJ}: `tools/state_db/schema.sql`, `writer.py`, `queries.py`, `test_writer.py`, `test_queries.py` "
           "(the sources of the above)",
           f"{CJ}: `docs/contracts/state-semantics-contract.md`, `docs/contracts/delegation-lifecycle-contract.md`"],
    out=["`importer.py`, `migrate_workers.py`, `extract_freetext.py`, `curator_archive.py`: they import "
         "claude-org-ja's old files and are not needed.",
         "`snapshotter.py`, and the writer's snapshot regeneration and worker-file archive: they wrote "
         "claude-org-ja's `.state/org-state.md`, its JSON snapshot and its worker files, none of which makiba "
         "has; makiba's view of the records is `makiba status`.",
         "The resume-briefing and organization-summary helpers of `queries.py`: their readers, claude-org-ja's "
         "resume and dashboard, are not carried. M6-5 renews the secretary from the `pending` and `runs` tables."],
    replaces="Issue #7 was closed by pull request #58, which carried the schema, the writer and the queries in "
             "Python (`makiba/db/*.py`) and added migration v4. The owner decided on 2026-11-18 that makiba is "
             "implemented in Rust (plan.md section 5). This Issue is reopened and replaces that code: "
             "`schema.sql` and `schema_v4.sql` are kept and embedded; the Python is ported or dropped as listed, "
             "and removed.")

add("M0-6", "The incident table and its check",
    "G2 is only checkable if the incident table is data a test can read.",
    ["`docs/design/incidents.md` as delivered, kept as a Markdown table.",
     "`tools/check_incidents.py`: parses the table; fails when a row has an empty `Test` cell or names a file "
     "that does not exist. A row whose guarding Issue is still open may name a file that does not exist yet "
     "only if the file is listed in `docs/design/incidents-pending.txt`; that list must be empty by M7-1."],
    "tests/test_check_incidents.py",
    ["A table with a row lacking a test fails the check.",
     "A row naming a missing file fails unless the file is in the pending list.",
     "The delivered table passes."],
    "G2",
    note="Merged in Python (#55). The check stays a Python development tool, run by CI and not shipped in the "
         "binary (plan.md section 7); the test files the table names are Rust tests (`tests/<name>.rs`).")

add("M0-7", "English only, checked in CI",
    "The English mirror of claude-org-ja fell five hundred pull requests behind the Japanese source. makiba has "
    "one source, in English.",
    ["`tools/check_language.py`: fails when a tracked file under `src/`, `roles/`, `skills/`, `docs/`, "
     "`tests/` or `knowledge/` contains a character from the Hiragana, Katakana or CJK Unified Ideographs blocks.",
     "An allow-list file for the few places that must hold other languages: `tests/fixtures/i18n/` (text the "
     "tests feed in) and `roles/locale/` (the sentence that sets the conversation language).",
     "A `lint` step in CI."],
    "tests/test_check_language.py",
    ["A scratch tree with one kana character in `skills/x.md` fails.",
     "The same character under `tests/fixtures/i18n/` passes.",
     "The repository itself passes."],
    "G7", deps=["M0-1"],
    note="Built in Python (pull request #59) while the code lived under `makiba/`. The check stays a Python "
         "development tool, run by CI and not shipped in the binary (plan.md section 7). The Rust code lives "
         "under `src/`; M0-1's rework changes the scanned folder.")

add("M0-8", "The shared specification, checked against the code",
    "M2 and M3 are built in parallel. Without one place that fixes table columns, tag keys, event kinds and "
    "exit codes, each worker invents its own.",
    ["`docs/design/spec.md` as delivered, reviewed and corrected against the carried schema.",
     "`src/spec.rs`: the same names as constants and enums (the two wrapper sentences of section 8, the wake "
     "line of section 7, tag keys, event kinds, the door's `ErrorKind` and its exit codes, agent ids, verbs per "
     "role, the wrapper's limits).",
     "A test that reads the tables of `spec.md` and compares them with `src/spec.rs`."],
    "tests/spec_matches_code.rs",
    ["Every tag key, event kind, error kind, exit code and verb in the document equals the constant, both ways.",
     "Removing one name from either side fails the test."],
    "G4", deps=["M0-5"])


# ---------------------------------------------------------------- M1
add("M1-1", "Connect to the base and say hello",
    "Every later call rests on one connection that knows what it is talking to.",
    ["`src/base/mod.rs`: the `Base` trait, the types and the `BaseError` enum of spec section 6.1.",
     "`src/base/herdr.rs`, a private module of `base`: `hello()`. Resolve the socket "
     "(`$XDG_RUNTIME_DIR/herdr-core/<session>.sock`), connect with std's `UnixStream`, send `hello` as JSON-RPC "
     "(serde_json), check version and protocol generation.",
     "`tests/base_seam.rs`: fails when a file under `src/` outside `src/base/` names `herdr` or `hc`. Because "
     "`herdr` is a private module of `base`, code elsewhere that imports it does not compile; the test holds "
     "the names. The base is a part that can be replaced (plan.md, \"The measure\"); this keeps it one.",
     "A timeout per call (the socket's read and write timeouts); no retry here."],
    "tests/base_connect.rs",
    ["`hello()` against the stand-in returns the base's name and version.",
     "A base outside the supported range returns `BaseError::UnsupportedVersion` and no other request is sent.",
     "A base that is not running returns `BaseError::Unavailable`, not a panic or an unmapped `io::Error`.",
     "`tests/base_seam.rs` passes on the repository, and its scan fails on a scratch tree with a module outside "
     "`src/base/` that names `herdr`."],
    "G6", deps=["M0-3"])

add("M1-2", "Terminal operations",
    "The dispatcher's hands: launch, look, type, end.",
    ["`launch`, `list`, `get`, `read`, `keys`, `send`, `end` in `src/base/herdr.rs`, per spec section 6.2.",
     "`launch` always applies the tags, the environment and the unset list; on herdr-core it also always sets "
     "`keep`, `no_parent`, `no_notify`. There is no way to call it without them.",
     "`list` returns only terminals tagged with this organization."],
    "tests/base_terminals.rs",
    ["Launch the test agent, read a line it printed, send it a key, end it.",
     "The launched terminal carries the tags given and none of the unset variables.",
     "A terminal launched by something else is not in `list`."],
    "G4", deps=["M1-1"])

add("M1-3", "Events and resync",
    "claude-org-ja once lost a terminal's end because an old cursor outlived a restart (#1235).",
    ["`events()` (spec section 6.5): one `events.subscribe`; a missed-events notice, a heartbeat gap of "
     "45 s or a reconnect yields a `Resync`.",
     "Reconnect with backoff (1 s to 30 s). Positions in the base's stream are never compared across "
     "connections."],
    "tests/events_resync.rs",
    ["Events for a scripted agent arrive in order.",
     "Restarting the base mid-test yields a `Resync`, and a terminal that finished during the gap is absent "
     "from its list.",
     "A dropped connection is re-established without the caller doing anything."],
    "G5", deps=["M1-1"])

add("M1-4", "Errors as types",
    "The dispatcher decides differently for 'not found', 'blocked' and 'try again'. It should not parse "
    "messages to tell them apart.",
    ["Map the base's errors to the variants of `BaseError`: `NotFound`, `Blocked`, `StatusMismatch`, "
     "`Finished`, `Unavailable`, and `Other` for the rest, each carrying the base's own kind, message and hint. "
     "herdr-core answers `{error: {kind, message, hint}}`."],
    "tests/base_errors.rs",
    ["`send` to a blocked terminal returns `BaseError::Blocked`.",
     "`send` with `only_if_idle` to a working terminal returns `BaseError::StatusMismatch`.",
     "An unknown terminal returns `BaseError::NotFound`."],
    "G4", deps=["M1-2"])

add("M1-5", "Compatibility test against the base's own definition",
    "The pin (M0-2) says which version; this says which parts of it makiba depends on, so that moving the pin "
    "is a test run and not a read-through.",
    ["`src/base/uses.rs`: the operations, parameters and result fields the implementation uses, as data.",
     "A test that checks every entry against what the base says about itself: herdr-core's OpenRPC schema "
     "(`hc schema`, or the vendored copy for the stand-in run)."],
    "tests/base_compat.rs",
    ["Green on the pinned version.",
     "With one used field removed from a copy of the definition, the test names that field."],
    "G6", deps=["M1-1"])

# ---------------------------------------------------------------- M2
add("M2-1", "makibad: one instance, a heartbeat, a clean stop",
    "A dispatcher that silently is not running is the 20-day relay silence again (incident I-3). A dispatcher "
    "that says it is ready before it listens is #1168 (I-4).",
    ["`src/daemon/mod.rs`, run as `makiba daemon` (the process named `makibad`): takes `makibad.lock` "
     "(std's `File::try_lock`) or exits; opens `state.db`; connects to the base; establishes the subscription; "
     "only then sets `dispatcher.subscribed = 1` and writes `dispatcher_started`.",
     "Writes `dispatcher.heartbeat_at` every 10 s. On SIGTERM (signal-hook): finishes the current step, writes "
     "`dispatcher_stopped`, releases the lock.",
     "`makiba status` run by the person reads `state.db` read-only "
     "itself, with no door, so that it works when `makibad` is dead; it prints the heartbeat's age and whether "
     "it is subscribed, and exits non-zero when the heartbeat is older than 30 s. The agent's tool `status` "
     "goes through the door (M6-1). The local reader is one module, "
     "`src/status_local.rs`, and the only code of the person's command that opens the database."],
    "tests/heartbeat.rs",
    ["A second `makibad` on the same state folder exits with a message naming the first one's pid.",
     "`subscribed` is 0 until the subscription exists (the stand-in delays it) and 1 after.",
     "After `makibad` is killed with SIGKILL, `makiba status` exits non-zero within 30 s."],
    "G5", deps=["M0-8", "M1-3"],
    out=["Starting `makibad` at boot (stage 2)."])

add("M2-2", "Record status and terminal events",
    "Everything the organization knows about a worker's state comes from what the worker reported, written "
    "down as it happened.",
    ["`src/daemon/monitor.rs`: for each event of a terminal carrying `makiba.org`, write `terminal_status`, "
     "`terminal_turn` or `terminal_gone` (spec section 10), tied to the run by the `makiba.task_id` tag.",
     "A status record identical to the last one written for that terminal is not written again."],
    "tests/monitor.rs",
    ["A scripted run `working, blocked(question), working, done` produces exactly those four status events "
     "in order.",
     "A terminal without the organization's tag produces none.",
     "A terminal that exits produces one `terminal_gone` with its exit code."],
    "G4", deps=["M2-1"])

add("M2-3", "Anomaly rules",
    "The model dispatcher looked at screens every three minutes and judged. These are the same judgments as "
    "functions, so they can be tested and they run every time.",
    ["`src/daemon/anomalies.rs`: the nine rules of spec section 11 as pure functions of "
     "(run, latest status, events, now); for the secretary, which has no run, of its terminal record instead.",
     "`anomaly_raised` once per (run, rule); `anomaly_cleared` when it stops being true; then it may raise again.",
     "Each raised anomaly queues one `notice` message to the secretary (delivery is M2-4)."],
    "tests/anomalies.rs",
    ["Each rule: a scene that raises it, and the same scene one second earlier that does not.",
     "A scene held for ten passes raises once.",
     "`done_no_report` does not raise when a `report_done` exists after the last turn.",
     "`secretary_gone`: a `terminal_gone` for the terminal named `secretary` with no renewal under way raises it "
     "once, as does a secretary with no status 180 s after launch; a gone caused by `makiba renew` does not."],
    "G1", deps=["M2-2"])

add("M2-4", "Deliver messages: the push and its confirmation",
    "Three incidents were about a message that did not arrive or arrived by a route nobody read (I-3, I-5, "
    "I-7). makiba has one default route, and a ledger that shows what is still unconfirmed. This Issue is the "
    "push and its confirmation; the box and the wake line are M2-7, and the channel server is M2-8.",
    ["`src/daemon/delivery.rs`: each pass, for every `pending` delivery not yet pushed (`attempt` is 0) whose "
     "recipient has a channel (the agent module's `has_channel()` and a connected channel server), push the "
     "wrapped text of spec section 8 through the channel, whatever the recipient's status. `attempt` goes up and "
     "`last_attempt_at` is set. Nothing is typed for this. Delivery runs inside `makibad` and stops only when the "
     "process dies, which the heartbeat shows. The channel is a seam (a function the agent module supplies), so "
     "this Issue is tested without the channel server.",
     "Confirmation (spec section 8): a push to a recipient that was `idle` or `done` is confirmed, and the row "
     "becomes `delivered`, when its status shows a turn start within 10 s (a first proposal). In every other "
     "case the row stays `pending` for M2-7.",
     "The wrapper of spec section 8 and its limits: bodies over the limit are cut with the `show` line."],
    "tests/delivery.rs",
    ["A message to an `idle` agent that has a channel is pushed once, nothing is typed, and the row becomes "
     "`delivered` when the test agent starts a turn within the window.",
     "A push after which no turn starts leaves the row `pending`, and it is not pushed a second time.",
     "A message pushed while the recipient is `working` stays `pending`.",
     "A recipient with no channel (the test agent launched without one) is never pushed to; its row stays "
     "`pending` with `attempt` 0.",
     "Killing `makibad` between a push and the ledger write, then restarting, does not lose the message: it may "
     "be pushed again with the same id, and the test asserts at-least-once.",
     "Every pushed text begins with `<makiba ` whatever the body is, and a body containing `</makiba>` cannot "
     "close the wrapper.",
     "A body over the limit is cut and carries the `show` line."],
    "G5", deps=["M2-2"],
    note="Decided (owner, 2026-11-18): plan.md section 3.3. A channel is the default route in every direction. "
         "Steps of probes P8 and P9 not yet run, to run before this Issue lands: a delivery to a `done` target, a "
         "turn that starts for another cause, a `blocked` recipient and two messages in a row (probes.md). P3 "
         "failed (typed text joins a person's draft, hc 0.1.20 and 0.1.21), which is why no body is typed into "
         "the secretary's terminal.")

add("M2-5", "Rebuild the view after a restart",
    "`state.db` holds no terminal address. After `makibad` or the base restarts, the tags are the record.",
    ["`src/daemon/recover.rs`: on start and on every `Resync`, list terminals with `makiba.org`; match each "
     "to a run by `makiba.task_id`; write `terminal_gone` for an `in_use` run with no terminal; write "
     "`base_resynced`.",
     "A terminal with the organization's tag and no matching run is reported as an anomaly to the secretary, "
     "never ended."],
    "tests/recover.rs",
    ["Kill `makibad` while an agent runs; the agent then reports `done`; restart; the `done` is recorded.",
     "A run whose terminal vanished while `makibad` was down gets one `terminal_gone`.",
     "An unknown tagged terminal is left alone and reported once."],
    "G5", deps=["M2-2"])

add("M2-6", "Tell the secretary only what it can act on",
    "A secretary woken for every status change stops reading. claude-org-ja's suppression rules already "
    "encode what is worth a message.",
    ["`src/daemon/notify_gate.rs`: the carried rule set, ported to Rust, applied before a `notice` is queued.",
     "A suppressed notice is still an event; only the message is skipped."],
    "tests/notify_gate.rs",
    ["The cases of the carried tests, ported, pass against the ported rules.",
     "A suppressed anomaly writes `anomaly_raised` and queues no message."],
    "G1", deps=["M2-4"],
    carry=[f"{CJ}: `tools/notify_gate.py`, `tools/test_notify_gate.py`"])

add("M2-7", "Deliver messages: the box, the wake line and typed delivery",
    "A push that did not arrive, a recipient that was working, and an agent program with no channel all end in "
    "the same place: the message is still `pending`, and the recipient has to be told to read it.",
    ["`src/daemon/inbox.rs`: the function behind the door verb `inbox` (spec section 12): the caller's "
     "`pending` messages, each wrapped, oldest first, within the limit, marked `delivered`. The door lists the "
     "tool (M3-3); this Issue holds the logic.",
     "The wake line in `src/daemon/delivery.rs`: a `pending` row whose recipient is `idle` or `done`, with no "
     "push to it in the last 10 s, gets `WAKE_LINE` typed with `Base::send`, after the prompt line of the "
     "recipient's screen is read (nothing is typed that pass if anything is on it), at most once per recipient "
     "in 60 s. A recipient with no channel is unconfirmed from the start and goes by this route alone. This is "
     "a real route for any agent program, not a fallback left untested.",
     "The typed route, only on the sender's request: a function for `send` and `answer` with `typed` (M3-3 and "
     "M6-4 pass the argument through the door). It goes to a worker or the curator whose status is `idle` "
     "(`only_if_idle`) or `done` (status read just before); any other status is `conflict`, and nothing is "
     "stored or typed. `typed` to the secretary is a `usage` error. The row is `delivered` when `Base::send` "
     "returns, and `makibad` never retries it."],
    "tests/inbox_wake.rs",
    ["A push after which no turn starts: the wake line is typed once into the `idle` recipient, and the row is "
     "`delivered` when the recipient's `inbox` returns it.",
     "A message pushed while the recipient was `working`: when the recipient turns `done` the wake line is "
     "typed, and `inbox` returns the message with the same id it was pushed with.",
     "A recipient with no channel gets the wake line and the box only.",
     "The wake line is not typed while the prompt line shows typed input, and not twice in 60 s (a clock the "
     "test controls).",
     "`send` with `typed` to a worker that is `idle` types the wrapped text once and the row is `delivered`; to "
     "a `working` worker it is `conflict` and nothing is stored or typed; to the secretary it is `usage`.",
     "`inbox` for one agent never returns another agent's message."],
    "G5", deps=["M2-4"],
    note="Decided (owner, 2026-11-18): plan.md section 3.3. P4 passed for a terminal nobody types in, which is "
         "why a typed body to a worker is kept; the prompt-line check follows paddock's watcher (record 0013), "
         "seen on a host only for the empty prompt.")

add("M2-8", "The channel server",
    "A Claude Code channel is carried by a small server that Claude Code starts for each session. It is another "
    "process to start and to give a credential, as claude-org-ja's sidecar is.",
    ["`src/channel.rs`, run as `makiba channel`: the per-session channel server that Claude Code starts from "
     "the MCP configuration (spec section 4). It is the same binary as `makibad`, so the two are always one "
     "version. It speaks MCP on its standard input and output (JSON-RPC, serde_json), declares the channel "
     "capability, takes the wrapped texts for its agent from `makibad` with the agent's token, and emits one "
     "`notifications/claude/channel` for each. How it receives them from `makibad` (a poll or a held request) "
     "is decided here; over the agents' port, the request is written on std's `TcpStream` and its answer read "
     "with httparse, with no HTTP client crate (plan.md section 5.1).",
     "`makibad` knows whether an agent's channel server is connected (M2-4's `has_channel` and connection "
     "check). When the server stops, the messages stay `pending`."],
    "tests/channel_server.rs",
    ["With a stand-in for the `makibad` side, one stored message produces exactly one notification whose text "
     "equals the wrapped text.",
     "A server given a wrong token receives nothing.",
     "After the stand-in `makibad` restarts, the server reconnects and receives messages again.",
     "While the server is stopped, `makibad` reports the agent as not connected and the messages stay "
     "`pending`."],
    "G5", deps=["M2-4", "M0-4"],
    note="Tested here against a stand-in for `makibad`; with the real door it runs from M3-3, and with real "
         "Claude Code in M4-5. Not yet shown (probes.md, P11): that the environment and arguments of this "
         "process are out of a sandboxed Bash's reach.")

# ---------------------------------------------------------------- M3
add("M3-1", "Take queued runs within capacity",
    "The secretary asks; the dispatcher decides when there is room.",
    ["`src/daemon/dispatch.rs`: each pass, take `queued` runs oldest first while the count of runs in "
     "`in_use` plus launches not yet `verified` or `failed` is below `max_concurrent_workers` "
     "(organization setting, default 4).",
     "A run taken gets its `run_launch` row moved from `planned` onward by M3-2 to M3-6."],
    "tests/dispatch_capacity.rs",
    ["With capacity 2 and three queued runs, two launches start and the third waits.",
     "When one finishes, the third starts on the next pass.",
     "Runs are taken in `dispatched_at` order."],
    "G4", deps=["M2-1"])

add("M3-2", "Prepare the worktree and the per-task settings",
    "A worker in a fresh git worktree stopped at the trust dialog in paddock, because trust comes from the "
    "repository, not from a parent folder (I-9). And a worker must not be able to edit its own rules.",
    ["Kind `code`: `git worktree add` from the project's own repository into "
     "`$MAKIBA_WORKERS_DIR/<project>/<task_id>`, on a new branch from the project's default branch (or "
     "`--branch`).",
     "Kind `document`: create `$MAKIBA_DOCS_DIR/<task_id>/` with an empty `out/`.",
     "The per-task settings file at `<state>/settings/<task_id>.json`, written by the agent module "
     "(`write_task_settings`): sandbox on, write surface = the worktree and a temp dir. No Unix socket path is allowed: a path allowance does nothing "
     "on Linux (P1), and agents reach `makibad` by the MCP configuration of M3-4.",
     "Nothing is written inside the worktree.",
     "`run_launch.stage`: `worktree_ready`, then `settings_ready`."],
    "tests/worktree.rs",
    ["After preparation `git status` in the worktree is clean and `git worktree list` in the project shows it.",
     "No file exists in the worktree that is not in the project's tree at that commit.",
     "Running preparation twice for the same launch changes nothing the second time.",
     "A `document` launch creates the working folder with `out/` and touches no git repository."],
    "G4", deps=["M3-1"])

add("M3-3", "The door, agent tokens, and the message tools",
    "Agents must be able to report and to message without being able to write the database. The door is the "
    "only thing a sandboxed agent may reach, and it reaches it as tools, because a sandboxed Bash cannot create "
    "a Unix socket on Linux.",
    ["`src/daemon/door.rs`: one request handler with two entrances (spec section 3). The Unix socket for the "
     "person's `makiba`: one JSON line in, one out. An MCP server on `127.0.0.1:$MAKIBA_PORT` for agents: one "
     "tool per verb (spec section 12), the agent's token as the key, and a role listed only the tools of its "
     "verbs. Every request recorded in `commands`; `(actor, idem)` returns the first answer again. The server "
     "is HTTP/1.1 on std's `TcpListener`, read with httparse, a thread per connection (plan.md section 5.1).",
     "`agents` rows and key checks (spec section 4); verbs allowed per role. A request with no key is "
     "`denied`, whatever the peer's user id, process lineage or terminal; on the socket a connection from "
     "another user id is refused first (its peer credentials, read with libc). Nothing at either entrance "
     "recognizes the person by the absence of a key.",
     "The person key (spec sections 2 to 4): `src/daemon/person_key.rs` creates `$MAKIBA_PERSON_KEY` "
     "(default `~/.config/makiba/person.key`, mode 0600, in a 0700 folder) when it is absent and stores only "
     "its SHA-256 in `agents` (`role = 'person'`). `makiba` sends the content of that file; with no file, it "
     "sends no key. `makiba up` (M4-4) calls the creation.",
     "`src/cli.rs` with the verbs `send`, `report`, `show`, and the tools of the same names. `report` also "
     "moves the run (`report done` sets `review`).",
     "The person's command (`src/cli.rs` and what it uses) never uses `crate::db`, except "
     "`src/status_local.rs` (M2-1), which opens it read-only for the person's `status`."],
    "tests/door.rs",
    ["A request with an unknown key is `denied` and recorded as `rejected`, on the socket and on a tool call.",
     "A worker's key may `report`; it may not `dispatch`: the worker's tool list has no `dispatch`, and a call "
     "to it by name is `denied`.",
     "The tool list of each role contains exactly the tools of its verbs in spec section 4 (the logic of `inbox` is M2-7's).",
     "`send` from a process with no access to `state.db` (the file is made unreadable to the test's CLI call) "
     "still results in a `messages` row.",
     "The message's `sender` is the id bound to the key even when the arguments claim another.",
     "The same `idem` twice creates one message.",
     "A request with no key, sent from inside a terminal `makibad` launched (the test agent, calling the "
     "socket and the port itself without a token), is `denied` and recorded as `rejected`, for every verb "
     "including `config` and `down`. The same holds when the test agent first double-forks and calls `setsid`, "
     "so that its parent is no longer the launched terminal.",
     "A request carrying the person key, sent from a process outside every launched terminal, may use "
     "`config`; the same request with a worker's key is `denied`.",
     "The person key file is mode 0600 in a folder of mode 0700, lies under no worktree and not under the "
     "role-files folder, and `state.db` holds only its SHA-256.",
     "No module of the person's command (`src/cli.rs` and every module it uses) other than "
     "`src/status_local.rs` uses `crate::db`; the daemon's own modules (`src/daemon/`) do."],
    "G3", deps=["M2-1"],
    note="Decided (owner, 2026-11-18): plan.md section 3.3. Agents reach `makibad` by the MCP server, the person "
         "by the socket, and an agent's token is in its launch arguments and never in its environment. Probe P11 "
         "measured the port and the token from a sandboxed Bash (port refused, token in the arguments not found, "
         "token in the environment readable). Still to show before this Issue lands: that the environment and "
         "arguments of the channel server that Claude Code starts are out of a sandboxed Bash's reach, which the "
         "recorded cases of P11 did not examine. That a sandboxed worker cannot read the person key is still to "
         "be shown by P1 (the steps it did not reach) and by M4-5.")

add("M3-4", "Launch",
    "The launch is where the environment is decided. paddock measured that the base's own variables (`HC_*`) "
    "and the starter's `CLAUDE*` variables reach a worker unless they are removed.",
    ["`src/daemon/launch.rs`: build `argv` from the agent module and call `Base::launch` with name, tags "
     "(`makiba.role`, `makiba.task_id`, `makiba.spawn`, `makiba.org`), `env` (no key), the unset list of "
     "spec section 6.4, and `cwd`. The agent module's `argv` carries the MCP configuration with the door's "
     "address and the agent's token (spec section 4).",
     "Write `worker_spawned` and `run_launch.stage = launched`. The key's hash goes into `agents`."],
    "tests/launch.rs",
    ["`Base::get` right after launch shows the four tags.",
     "The test agent's recorded environment contains no key and none of the names in spec section 6.4, "
     "even when the test's own environment sets them; its recorded argument list carries the token.",
     "The token in the argument list hashes to the stored `key_sha256`."],
    "G3", deps=["M3-2", "M3-3", "M1-2"])

add("M3-5", "Startup dialogs",
    "A blind Enter once ended a terminal because the dialog's default line had changed (I-2).",
    ["`src/daemon/dialogs.rs`: while a launched terminal has no status, read its screen; ask the agent "
     "module's policy list whether a known dialog is shown; if so move the cursor toward the expected line, "
     "read again, and press the key only when the cursor is on it and the option lines still match whole.",
     "A dialog not on the list: no key. The launch fails with `worker_spawn_failed` carrying the screen text.",
     "`run_launch.stage = dialog_passed` (also when no dialog appeared and a status arrived).",
     "The matcher is ported from the runtime's `decide`, with its cases; the press rule is paddock's.",
     "The policy list of the Claude Code module has two entries, `folder-trust` and `dev-channel` (spec "
     "section 7); the second is passed by choosing its line 1."],
    "tests/startup_dialog.rs",
    ["A folder-trust dialog whose cursor starts on 'No, exit' ends with the trust line chosen and the agent "
     "running.",
     "The same dialog with one option line reworded is not answered, and the launch fails with the screen "
     "recorded.",
     "No key is ever sent to a terminal that has a status.",
     "The development-channel dialog, shown after the trust dialog as P10 recorded it, ends with line 1 "
     "chosen; the same dialog with one option line reworded is not answered."],
    "G2", deps=["M3-4"],
    carry=[f"{RT}: `claude_org_runtime/dispatcher/spawn_prompt.py` (`decide`, `screen_lines` and helpers)",
           f"{PD}: the policy-list rule in `skills/org-escalation` and inventory row L-N3"],
    note="Probe P5 measures whether a task given in the launch arguments starts after the dialog on real "
         "Claude Code. It passed (2026-11-18, hc 0.1.21, Claude Code 2.1.326): the first-message fallback "
         "(probes.md, P5) is not taken. The development-channel dialog of the decision in plan.md section 3.3 is on "
         "the policy list here. Probe P10 passed it on a first launch; a resume and a renewal were not run, and "
         "are steps to run before this Issue lands.")

add("M3-6", "A launch is verified only by an observed first turn",
    "On 2026-08-18 two workers were reported as dispatched that had never started (I-1).",
    ["`worker_spawn_verified`, `run_launch.stage = verified` and `runs.status = in_use` are written in one "
     "transaction, and only when a `working` status or a turn event for that terminal has been recorded "
     "after launch.",
     "No verification within the timeout (default 180 s): `worker_spawn_failed`, stage `failed`, the terminal "
     "is left for inspection, and the secretary gets a notice.",
     "`conversation_id` is read from the agent module once verified."],
    "tests/spawn_verified.rs",
    ["An agent that reaches `working` is verified and its run is `in_use`.",
     "An agent that launches and never reports is `failed` after the timeout; the run is not `in_use`; "
     "a notice to the secretary is queued.",
     "There is no code path that writes `worker_spawn_verified` without a status or turn event id in its payload."],
    "G2", deps=["M3-5", "M2-2"])

add("M3-7", "A launch survives the dispatcher dying at any step",
    "A dispatcher that can be restarted safely is the point of making it code.",
    ["On start, for every `run_launch` not `verified` or `failed`: at `planned`/`worktree_ready`/"
     "`settings_ready`, continue from that stage; at `launched`/`dialog_passed`, look for a terminal with "
     "`makiba.spawn=<spawn_id>` and adopt it, or mark the launch `failed` if none exists.",
     "A test helper that kills `makibad` at a named step. How a test names the step to a running `makibad` "
     "is decided here; it must not be reachable in a release build."],
    "tests/launch_idempotent.rs",
    ["For each stage boundary: kill there, restart, and assert exactly one terminal carries the spawn tag "
     "and the run ends `in_use`.",
     "Kill after `Base::launch` returned but before the stage was written: the terminal is adopted, not "
     "launched again."],
    "G4", deps=["M3-6"])

# ---------------------------------------------------------------- M4
add("M4-1", "Deny the base's command in every role",
    "herdr-core does not protect itself from an agent: it has no permission boundary. Whoever reaches it "
    "can type into any terminal. The first layer is "
    "that no role's settings allow the base's command at all.",
    ["`roles/*/settings.json` for secretary, worker and curator: deny rules for the commands, and `Read`, "
     "`Edit` and sandbox denies for the folders, that `Base::worker_denies()` lists (spec section 6.2). "
     "These are `hc`, `herdr-core`, `herdr-app` and their config and state folders. The same files also "
     "deny the person key file for read and write, and the unsandboxed run of a command "
     "(`allowUnsandboxedCommands` off), because the person key is what separates the person's verbs from "
     "every agent's (spec section 3).",
     "A `PreToolUse` hook, `makiba hook deny-base` (a subcommand of the one binary, spec section 14), that "
     "refuses a Bash command naming one of those commands as a command word or as a path ending in it, "
     "fail-closed on unreadable input.",
     "The rules files say why in one sentence."],
    "tests/base_denied.rs",
    ["For each role's settings file: every entry of `worker_denies()` is present, and so are the person key "
     "file's denies and the switch that refuses an unsandboxed run (a shape test, as paddock's "
     "`tests/worker-settings-*.test.sh`).",
     "The hook refuses the command bare, by absolute path and behind `env`, and passes a command that only "
     "mentions the name as an argument.",
     "The hook refuses on empty and on malformed input."],
    "G3", deps=["M0-1", "M1-1"],
    carry=[f"{PD}: `templates/worker-settings.json`, `templates/secretary-settings.json`, "
           "`tests/worker-settings-*.test.sh`",
           f"{CJ}: `.hooks/lib/` (the rules of the fail-closed payload reader, ported)"])

add("M4-2", "The sandbox reaches the door and nothing of the base",
    "A deny rule matches command text; a script or a direct socket connection is outside it. paddock wrote "
    "the sandbox entries for herdr-core but could not measure them, because its deny rule always refused first.",
    ["Per-task settings (from M3-2) deny the base's socket and state folders and the person key file for "
     "read and write, and deny `state.db` for read and write (an agent's `status` goes through the door; the "
     "local reader is the person's). They name no Unix socket path as allowed: agents reach `makibad` by the MCP "
     "configuration in the launch arguments (M3-4).",
     "`scripts/probes/door-and-base-reach.sh`: probe P1 as a script a person can rerun, for the steps the first "
     "run did not reach (probes.md, P1), and probe P11 (the port and the token from a sandboxed Bash). They "
     "were run by hand before M2 and M3 with hand-written settings; this repeats them with the settings "
     "`makibad` really writes.",
     "The result recorded in `docs/design/probes.md` with versions and date."],
    "tests/task_settings_shape.rs",
    ["The per-task settings written for a run contain the base's denies, the person key file's denies and the "
     "`state.db` denies, and name no Unix socket path as allowed.",
     "The settings name no path inside the worktree as writable configuration.",
     "The launch arguments written for a run carry the token and the environment does not (shape only; the "
     "host rerun of P11 is what shows that a sandboxed Bash cannot read it, and it is the proof of the goal "
     "clause that an agent's token cannot be read from its own Bash)."],
    "G3", deps=["M4-1"],
    carry=[f"{PD}: `scripts/probes/core-api-reach.sh`"],
    note="The shape test goes through the acceptance gate. The host rerun of probes P1 and P11 does not: a person "
         "runs it and records the result. M4-5 may not start until that result is a pass. Decided (owner, "
         "2026-11-18): plan.md section 3.3. The first run of P1 failed on the door; the door is now the MCP "
         "server, and the sandbox entries for a socket path do not work on Linux.")

add("M4-3", "Carry the role settings and the git guard hooks",
    "The lines paddock lists as 'kept' (L-K1 to L-K12) are held by settings and hooks that already exist and "
    "have tests.",
    ["`roles/worker/`, `roles/secretary/`, `roles/curator/`: settings and rules files from paddock, with the "
     "mailbox commands replaced by tool calls of the door and the two wrapper sentences of spec section 8 added.",
     "Hooks from claude-org-ja, ported as subcommands `makiba hook <name>` with the cases of their tests: push "
     "refused for workers, dangerous git, `--no-verify`, recursive delete of worker folders, the worker "
     "boundary, foreground subagents. The shell versions need `jq`, and the worker-boundary hook falls back to "
     "`python3`; makiba's hooks need nothing but the binary.",
     "Workers start in auto permission mode.",
     "`docs/design/lines.md`: paddock's table of lines (inventory section 6.2) with, for each, what holds it "
     "in makiba."],
    "tests/role_settings.rs",
    ["The cases of the carried hook tests, ported, pass.",
     "Every line in `docs/design/lines.md` names a settings entry, a hook or a test that exists.",
     "The rules file of each role (secretary, worker, curator) contains the two wrapper sentences of spec "
     "section 8, exactly as `src/spec.rs` holds them."],
    "G3", deps=["M4-1"],
    carry=[f"{PD}: `templates/worker-rules.md`, `worker-settings.json`, `secretary-rules.md`, "
           "`secretary-settings.json`, `curator.md`; `docs/design/predecessor-inventory.md` section 6.2",
           f"{CJ}: `.hooks/block-git-push.sh`, `block-dangerous-git.sh`, `block-no-verify.sh`, "
           "`block-workers-delete.sh`, `check-worker-boundary.sh`, `block-foreground-subagent.sh`, `.hooks/lib/`, "
           "`tests/test-block-git-push.sh`, `test-block-dangerous-git.sh`, `test-check-worker-boundary.sh`, "
           "`test-hooks-payload-fail-closed.sh`"])

add("M4-4", "makiba up: start the base cleanly",
    "Whatever starts the base hands its environment to every terminal. Started from inside an agent, that "
    "turned transcripts off in paddock.",
    ["`makiba up`: refuses when run inside an agent (any of the `CLAUDE*` session variables, or one of the "
     "base's own terminal variables, is set), with a message saying to use a login shell.",
     "Calls `Base::start()`: the `makiba` session's core with `notes = false`, so that the base sends no "
     "notices and gives agents no rules of its own (requirement R2). Never stops or restarts a base it "
     "did not start.",
     "Creates the person key file if it is absent (M3-3's function), and refuses nothing about an existing one.",
     "Runs `claude --version` and refuses to start the secretary or any worker outside the supported range of "
     "M0-2, naming both. Claude Code can update itself between checks; a launch after such an update is caught "
     "at the next `up`, not at the launch.",
     "Starts `makibad` (`makiba daemon`, detached). Starts the secretary unless `--no-secretary` (the "
     "secretary itself is M6-2)."],
    "tests/up.rs",
    ["With `CLAUDE_CODE_SESSION_ID` set, `makiba up` exits non-zero and starts nothing.",
     "After `up`, a launched test agent's environment has none of the caller's extra variables named in "
     "spec section 6.4.",
     "A second `up` starts nothing new and exits 0.",
     "After `up` the person key file exists with mode 0600 in a folder of mode 0700, and a second `up` leaves "
     "its content unchanged.",
     "With a stand-in `claude` on the path that prints a version outside the range, `up` exits non-zero and "
     "starts neither secretary nor worker."],
    "G3", deps=["M2-1", "M3-3"],
    carry=[f"{PD}: `skills/org-start/scripts/org-start.sh` `--check` (as the list of preconditions)"])

add("M4-5", "One real worker, request to done report",
    "Everything before this ran the test agent. This is the first time Claude Code is launched by makibad.",
    ["A written procedure, `docs/runbooks/first-worker.md`: preconditions (probe P1 passed, a trusted scratch "
     "repository), the commands, what to look at.",
     "Run it on a host: `makiba up --no-secretary`, a `dispatch` typed by the person, the worker edits one "
     "file and calls the tool `report` with `done`, the person runs `makiba close`.",
     "Inside the worker, ask it to run the base's command, to connect to the base's socket, to read the "
     "person key file, to call the door's port with no token and with an invented one, and to look for its own "
     "token (its environment, `/proc`, the files it can read); record the refusals.",
     "Record versions, date and what happened in the runbook's results section. File an Issue for every "
     "surprise."],
    None,
    ["The run reaches `review` with no person typing into the worker's terminal.",
     "`worker_spawn_verified` and `report_done` are in `events`.",
     "The two attempts to reach the base from the worker are refused.",
     "The read of the person key file is refused, a call to the door's port with no token or an invented one "
     "is `denied`, and the agent's token is found nowhere the worker can read."],
    "G3", deps=["M3-7", "M4-2", "M4-3", "M4-4"], gate=False,
    note="No failing test can come first: this is a host run with a real agent. It is outside the acceptance "
         "gate and is closed by a person on the recorded result.")

# ---------------------------------------------------------------- M5
add("M5-1", "Close is refused before the done report",
    "In claude-org-ja a hook stopped the dispatcher from closing a pane before its retrospective. The line "
    "that matters is narrower: no close before the worker's own report has been recorded.",
    ["The `close` verb: `conflict` unless the run has a `report_done`, or `--abandon` is given (which records "
     "who abandoned and sets the run `abandoned`).",
     "A `close` for a run that is already closed is `done` again, not an error."],
    "tests/close_gate.rs",
    ["`close` before any report is rejected with `conflict` and nothing is ended.",
     "`close` after `report done` is accepted.",
     "`close --abandon` before a report is accepted and the run becomes `abandoned`."],
    "G4", deps=["M2-2", "M3-3"])

add("M5-2", "End the terminal and remove the worktree",
    "Leftover terminals and worktrees are how a dead thing comes to look alive.",
    ["`src/daemon/close.rs`: `Base::end`, revoke the agent key; for `code`, `git worktree remove` "
     "(refusing when the worktree has uncommitted or unpushed work unless `--abandon`), delete the per-task "
     "settings, set the run `completed`, write `run_closed`.",
     "For `document`: keep `out/`, remove the rest of the working folder.",
     "Each step recorded so that a restart finishes a half-done close."],
    "tests/close.rs",
    ["After close: no terminal with the task's tag, no worktree folder, no settings file, key revoked.",
     "A worktree with an unpushed commit is not removed without `--abandon`; the command fails and says why.",
     "Killing `makibad` mid-close and restarting ends in the same state.",
     "After closing a `document` run, `out/` and its files are still there."],
    "G4", deps=["M5-1", "M3-6"])

add("M5-3", "makibad owns the PR watches",
    "A finished watcher that still looked alive (I-6), and a watch whose result nobody wrote (I-8).",
    ["`makiba pr opened|pushed`: records `pr_opened` / `fix_pushed`; inserts the `pr_watches` row for the new "
     "head and marks older rows for that PR `superseded`, in one transaction.",
     "`src/daemon/prwatch.rs`: for each `watching` row, poll the checks of that exact head with `gh`; on a "
     "conclusion write `ci_completed` (with `head_oid`) and queue a notice; on an error it cannot recover "
     "from write `ci_watch_failed` and queue a notice. A row never stays `watching` after its poller is gone: "
     "on start every `watching` row gets a poller again.",
     "From the carried `pr_watch.py`: the check-reading and conclusion logic, ported, with the cases of its "
     "tests. Its own push to the secretary and its transport switch are not ported."],
    "tests/pr_watch.rs",
    ["`pr pushed` with a new head leaves one `watching` row, for the new head.",
     "A watch whose `gh` call fails for good ends `failed` with a `ci_watch_failed` event and a notice.",
     "Restarting `makibad` with a `watching` row resumes polling it.",
     "`ci_completed` carries the head it was observed for."],
    "G2", deps=["M2-4", "M3-3"],
    carry=[f"{CJ}: `tools/pr_watch.py`, `tools/test_pr_watch.py` (2,171 lines; the parts under `_notify*` and "
           "`_configured_transport` are not carried)"])

add("M5-4", "Launch the curator at the threshold",
    "Knowledge notes pile up unread unless something starts the curator. The model dispatcher did it when it "
    "remembered.",
    ["After a close, count raw notes; at the threshold, launch the curator (role `curator`, its own settings: "
     "writes only the knowledge folder), one at a time, with a time limit after which its terminal is ended "
     "and a notice sent.",
     "The threshold rule is ported, with the cases of its tests."],
    "tests/curator.rs",
    ["Below the threshold nothing is launched.",
     "At the threshold one curator is launched; a second close while it runs launches none.",
     "A curator past its time limit is ended and a notice queued."],
    "G7", deps=["M3-6"],
    carry=[f"{CJ}: `tools/check_curate_threshold.py`, `tools/test_check_curate_threshold.py`",
           f"{PD}: `templates/curator.md`, `skills/org-curate/`"])

add("M5-5", "Run the triage scan and forward its result",
    "Work discovery is what lets the organization propose its own next task. Its scan is deterministic and "
    "already tested; only its idea of free capacity was tied to a transport.",
    ["`src/work_discovery/`: the logic of claude-org-ja's `work_discovery_*.py`, ported. In the scan, capacity "
     "comes from `max_concurrent_workers` minus active runs, read through a function `makibad` supplies. The "
     "goal ledger keeps its format.",
     "After a close and at most once per 30 minutes, run the scan; if the candidate set differs from the last "
     "one sent, queue a notice to the secretary with the candidates.",
     "`work_discovery_scanned` as before."],
    "tests/triage_forward.rs",
    ["The cases of the carried scan, goals and dedup tests, ported, pass.",
     "Two scans with the same candidates queue one notice.",
     "A project with no goal ledger is reported as such, once."],
    "G7", deps=["M2-4"],
    carry=[f"{CJ}: `tools/work_discovery_scan.py`, `work_discovery_goals.py`, `work_discovery_dedup.py`, "
           "`work_discovery_repos.py`; `tests/test_work_discovery_scan.py`, `test_work_discovery_goals.py`, "
           "`test_work_discovery_dedup.py`, `test_work_discovery_repos.py`; `registry/goals/README.md` (translated)"])

add("M5-6", "makiba down and up keep the work",
    "A resident organization is restarted for updates. Nothing in progress may be lost by that.",
    ["`makiba down`: record state, end worker terminals (the secretary only with `--secretary`, the base only "
     "with `--stop-base`), set runs `suspended`.",
     "`makiba up` after a down or a stop of the base: for each `suspended` run, launch again in its worktree with "
     "`resume=<conversation_id>`; if that launch is not verified, launch a fresh worker with the stored brief "
     "and the worktree as it is.",
     "On herdr-core, a kept shell left by a core stop is ended first."],
    "tests/down_up.rs",
    ["After `down` then `up`, every run that was `in_use` is `in_use` again with one terminal.",
     "A run whose resume fails gets a fresh launch with the original brief.",
     "`down` never ends a terminal without the organization's tag."],
    "G5", deps=["M2-5", "M3-7"],
    carry=[f"{PD}: `skills/org-suspend`, `org-resume`, `org-down` (their rules, and Q4's decision)"])

# ---------------------------------------------------------------- M6
add("M6-1", "The secretary's commands",
    "The secretary acts through commands that are recorded, not through a terminal.",
    ["Verbs `dispatch`, `close`, `answer` (questions; prompts are M6-8), `pr` (wired in M5-3), `pending`, "
     "`peek`, `status`, `renew` (wired in M6-5), each checked against the role table of spec section 4.",
     "The person's `dispatch` reads `--brief-file` itself and sends the content; the tool takes the brief as "
     "text. `makibad` never opens a path an agent names.",
     "`peek` returns the last lines of a worker's screen, read by `makibad`.",
     "`makiba --help` and per-verb help come from the clap definitions (plan.md section 5.1). A test keeps them and the verb table of spec section 12 the same: every verb of the table that the person may run is a subcommand of the clap definition (read through clap's `CommandFactory`), with the arguments the table names, and the definition has no verb the table lacks. The agents' tool list (spec sections 4 and 12) does not come from clap."],
    "tests/cli_secretary.rs",
    ["Each verb, called as the secretary's tool, is recorded in `commands` and returns a result.",
     "`dispatch` creates a `queued` run, its `run_launch` row with the brief and the kind, and `delegate_sent`.",
     "`dispatch --kind code` without `--project` is a usage error; `--kind document` needs none.",
     "A worker's key cannot `peek` or `dispatch`.",
     "The tool `status` goes through the door and returns what the person's local `status` returns; the "
     "person's `status` is the local read of M2-1.",
     "Every exit code of spec section 3 is produced by at least one case.",
     "The clap definition and the verb table of spec section 12 name the same verbs for the person, with the same "
     "arguments; a verb present on one side only fails the test."],
    "G3", deps=["M2-1", "M3-3"])

add("M6-2", "Launch the secretary",
    "The secretary is one more terminal the dispatcher launches, with a fixed name and its own folder.",
    ["`makiba up` launches role `secretary` in `<state>/secretary/`, named `secretary`, with its token in the "
     "launch arguments, rules file and settings; passes its startup dialogs (folder-trust and the "
     "development-channel one) by the policy list.",
     "If a terminal named `secretary` with the organization's tag already runs, nothing is launched.",
     "Setting `secretary.remote_control` (default off): when on, the Claude Code agent module adds Claude "
     "Code's Remote Control option to the secretary's argv, so the person can reach the secretary from the "
     "Claude app. No other role ever gets it and no other module reads the setting (plan.md section 2, "
     "probe P6).",
     "`makiba status` shows how to attach to it."],
    "tests/secretary_launch.rs",
    ["After `up`, one terminal named `secretary` with `makiba.role=secretary` exists.",
     "A second `up` launches no second one.",
     "With `secretary.remote_control` on, the secretary's argv carries the Remote Control option and a "
     "worker's argv does not; with it off, neither does.",
     "The secretary's folder holds no file from the makiba checkout that the settings do not name."],
    "G3", deps=["M3-4", "M4-4"],
    carry=[f"{PD}: `templates/secretary.md`, `templates/secretary-rules.md`, "
           "`skills/org-start/scripts/org-start.sh` (the secretary-folder part)"])

add("M6-3", "The secretary's skills, operating no terminal",
    "paddock's skills are in English and have run the full loop, but they call `hc` and the shell mailbox. "
    "claude-org-ja's name pane tools seventy times.",
    ["`skills/`: `delegate`, `escalation`, `pull-request`, `issue`, `update-branch`, `curate`, rebuilt from "
     "paddock's. Every `hc` call and every `org` mailbox call becomes a tool call of the door (spec section 12); "
     "scripts that only wrapped `hc` are deleted.",
     "`skills/retro`: the retrospective the model dispatcher used to write, as a secretary skill.",
     "`skills/document`: how a document request runs. The secretary dispatches a maker with `--kind document`; "
     "when it reports, a second worker reads the draft against the request and checks that each fact carries "
     "a source; only then does the secretary hand the result to the person. Borrowed as rules from libretto; "
     "none of libretto's code or services is used.",
     "`roles/secretary/rules.md`: roles, the lines the secretary holds, situations mapped to skills. It says "
     "when to send `typed` (spec section 7): when a message that came by channel was refused as not being from "
     "the person, send it again with `typed`; send an instruction known from the start to need typing typed at "
     "once; never to the secretary. `makibad` cannot see a refusal, so the sender reads the answer.",
     "`tests/no_terminal_tools.rs`: fails when a file under `skills/` or `roles/` names `hc `, "
     "`herdr`, `send_keys`, `inspect_pane`, `list_panes`, `spawn_pane`, `close_pane`, `tmux` or "
     "`wezterm`. The check is the test, run by `cargo test` in CI."],
    "tests/no_terminal_tools.rs",
    ["The check passes on the repository.",
     "A scratch skill containing `hc send` fails its scan.",
     "Every tool or command a skill names is a verb of the table in spec section 12.",
     "`roles/secretary/rules.md` names `typed` and says both when to use it and that it never goes to the "
     "secretary.",
     "`roles/secretary/rules.md`, as rebuilt here, still contains the two wrapper sentences of spec section 8."],
    "G3", deps=["M6-1"],
    carry=[f"{PD}: `skills/org-delegate`, `org-escalation`, `org-pull-request`, `org-issue`, "
           "`org-update-branch`, `org-curate`",
           f"{CJ}: `.claude/skills/org-retro/` (translated), `.dispatcher/references/rationale.md` (translated, "
           "as `docs/rationale.md`)"])

add("M6-4", "Pending decisions in the ledger",
    "What waits for the person is the first thing stage 2 will notify about and stage 3 will answer from "
    "outside. It has to be a table, not a file one role owns.",
    ["`makiba pending add|escalate|withdraw|list` and `makiba answer <id> yes|no [--note]` for questions: the "
     "answer is queued as a message to the worker (`--typed` sends it typed, M2-4) and the entry becomes "
     "`answered`.",
     "The logic of `pending_decisions.py`, ported with the cases of its tests, with the store changed to the "
     "`pending` table.",
     "The `secretary_still` anomaly (M2-3) reads open entries."],
    "tests/pending.rs",
    ["The carried cases, ported, pass against the table.",
     "`answer` on an entry already answered is `conflict`.",
     "`answer` queues exactly one message to the run's worker."],
    "G1", deps=["M6-1"],
    carry=[f"{CJ}: `tools/pending_decisions.py`, `tests/test_pending_decisions.py`"])

add("M6-5", "Renew the secretary without typing into it",
    "A long-running secretary fills its context. paddock measured that ending its terminal and launching a "
    "fresh one from a handover works and leaves workers running.",
    ["`makiba renew --handover-file <f>`: the secretary's request. `makibad` stores the handover, waits until "
     "the secretary's terminal is `idle` or `done`, ends and removes it, revokes its key, launches a new "
     "secretary whose task is to resume from the stored handover, and writes `secretary_renewed`.",
     "Workers are not touched; messages for `secretary` stay `pending` across the gap and are delivered to "
     "the new terminal.",
     "If the new secretary is not verified, a notice goes to the log and `makiba status` shows it.",
     "When the secretary's terminal ends without a renewal (`secretary_gone`, spec section 11), `makibad` "
     "launches a new secretary by the same path, from the last stored handover or, if there is none, from the "
     "open pending entries and running tasks in `state.db`. It tries at most twice in ten minutes (first "
     "proposal); after that it stops, and `makiba status` shows the secretary as gone until the person runs "
     "`makiba up`. Nothing tells the person before stage 2: the log and `makiba status` are all there is."],
    "tests/renew.rs",
    ["After a renewal the terminal named `secretary` is a different terminal; every worker terminal is the "
     "same one.",
     "A message queued during the gap is delivered to the new secretary.",
     "Open pending entries and `in_use` runs are identical before and after, by id.",
     "A secretary terminal killed with no renewal raises `secretary_gone` once and is relaunched; messages "
     "queued while it was gone stay `pending` (none is `failed`) and are delivered to the new terminal once; "
     "after two failed relaunches in the window no third is tried and the secretary shows as gone."],
    "G1", deps=["M6-2"],
    carry=[f"{PD}: `skills/secretary-handover/` and Q16's decision"],
    note="Probe P10 did not run a renewal. Whether the development-channel dialog comes back for a renewed "
         "secretary is a step to run before this Issue lands (probes.md, P10).")

add("M6-6", "Conveyor and the acceptance gate",
    "The acceptance gate is what lets pull requests merge without a person reading each one. It reads "
    "`ci_completed` rows, which M5-3 now writes.",
    ["The gate logic of `acceptance_gate.py` and `acceptance_merge_hook.py`, ported with the cases of their "
     "tests, and the secretary's `PreToolUse` hook as the subcommand `makiba hook merge-gate`; they read "
     "`state.db` through a read-only query `makibad` answers over the door.",
     "`skills/conveyor`, rebuilt from claude-org-ja's, with `merge_approval_mode` in the organization settings.",
     "The hook also refuses the other ways to merge from a command line: the pull-request merge command with "
     "any flag, the API command with a write method on a `pulls/<n>/merge` path, its GraphQL form carrying a "
     "merge mutation, and `curl` or `wget` to the GitHub API."],
    "tests/acceptance_gate.rs",
    ["The cases of the carried gate tests, ported, pass.",
     "A merge command for a pull request with no declared test is refused in `acceptance` mode.",
     "A merge is refused when the latest `ci_completed` is for another head.",
     "Each of the other ways above is refused in `acceptance` mode, and a command that only mentions them as an "
     "argument is not."],
    "G7", deps=["M6-3", "M5-3"],
    note="A hook reads command text and cannot list every way to reach GitHub. It is kept for the ways above and "
         "is not the bound. The bound is on GitHub's side: branch protection that requires the `acceptance` check "
         "and lets nobody, the person's own login included, merge around it. M0-1 names that setting for this "
         "repository; for every other project it is the owner's to set, and makiba does not set it.",
    carry=[f"{CJ}: `tools/acceptance_gate.py`, `tools/acceptance_merge_hook.py`, "
           "`.hooks/gate-acceptance-before-merge.sh`, `tools/test_acceptance_gate.py`, "
           "`tools/test_acceptance_merge_hook.py`, `.claude/skills/org-conveyor/` (translated)"])

add("M6-7", "Language settings",
    "Rules are English; the person need not be. And a project's pull requests follow the project.",
    ["Organization setting `conversation_language` (default: the environment's): one sentence from "
     "`roles/locale/` is appended to the secretary's launch.",
     "Project setting `artifact_language` (default `en`): one sentence appended to the worker's brief and "
     "passed to the pull-request and issue skills.",
     "`makiba config set|get` for both, person only."],
    "tests/language_settings.rs",
    ["With `conversation_language=ja` the secretary's launch arguments contain the Japanese sentence; with "
     "`en` they do not.",
     "A project without `artifact_language` gives a brief with the English sentence; with `ja`, the Japanese one."],
    "G7", deps=["M6-2", "M6-3"],
    carry=[f"{PD}: Q9's decision and the `conversation_language` handling in `org-start`"],
    note="That a real secretary then answers in the set language is checked once on a host and recorded in "
         "`docs/runbooks/first-worker.md`; that part is outside the acceptance gate.")

add("M6-8", "Relay a permission prompt",
    "A resident organization cannot call a person to a terminal for every prompt. paddock's relay (read "
    "again, press one key only if unchanged) passed on a host.",
    ["On `blocked` with kind `permission`: `makibad` reads the screen, asks the agent module to recognize the "
     "prompt, and adds a `pending` entry of kind `prompt` with `prompt_text` and `prompt_sum`; `prompt_seen`.",
     "`makiba answer <id> yes|no` for a prompt: read the screen again; if the terminal is still `blocked` and "
     "the sum matches, press the one key (the one-time yes, or escape) and write `prompt_answered`; otherwise "
     "reject `stale` and press nothing.",
     "When the terminal leaves `blocked` on its own, the entry is withdrawn."],
    "tests/prompt_relay.rs",
    ["A scripted prompt answered `yes` receives exactly one key and the agent continues.",
     "If the prompt text changes between record and answer, the answer is `stale` and no key is sent.",
     "A prompt the module does not recognize becomes a pending question with the screen text, and no key is "
     "ever sent for it."],
    "G1", deps=["M6-1", "M6-4", "M3-5"],
    carry=[f"{PD}: `skills/org-escalation` (`prompt`, `answer`), `scripts/probes/worker-prompt-relay.sh`, "
           "inventory row L-N3"])

# ---------------------------------------------------------------- M7
add("M7-1", "Register makiba in makiba",
    "The switch is judged by makiba doing its own work.",
    ["makiba as a project in its own organization; the goal ledger placed; the triage scan run.",
     "`docs/design/incidents-pending.txt` is empty: every incident row names a test that exists."],
    None,
    ["`makiba status` shows the project.", "The triage scan returns candidates from makiba's open Issues.",
     "`tools/check_incidents.py` passes with an empty pending list."],
    "G7", deps=["M6-6", "M5-5", "M4-5", "M5-2", "M6-2", "M6-5", "M6-8"], gate=False,
    note="Outside the acceptance gate: closed by a person. It waits for what the week uses: one real worker "
         "through (M4-5), the secretary launched and renewed (M6-2, M6-5), a permission prompt relayed (M6-8) "
         "and a close (M5-2). Goal G7 still asks for every Issue of M0 to M6 to be closed by the end.")

add("M7-2", "One week of self-hosting",
    "A week is long enough to meet an update of the base, a secretary renewal and a failed launch at least once.",
    ["Develop makiba with makiba for seven days. claude-org-ja stays stopped.",
     "Every incident met: a row in `docs/design/incidents.md` and an Issue for its test.",
     "At the end, a short report in `docs/reports/self-hosting-week.md`: counts of runs, failed launches, "
     "undelivered messages at any point, renewals, time `makibad` was down."],
    None,
    ["The report exists and every incident it mentions has a row.", "The incident tests are green at the end."],
    "G7", deps=["M7-1"], gate=False,
    note="Outside the acceptance gate: closed by a person.")

add("M7-3", "Move the projects claude-org-ja serves",
    "Until the projects claude-org-ja serves move, two organizations run side by side. paddock is not one of "
    "them: it continues as a lightweight organization beside makiba (plan.md section 4.1), and the projects it "
    "serves stay with it.",
    ["Register each project claude-org-ja serves; move their goal ledgers; stop claude-org-ja. Nothing of "
     "paddock's is stopped or moved.",
     "One more week of observation."],
    None,
    ["For seven days after the move, claude-org-ja is not started."],
    "G7", deps=["M7-2"], gate=False,
    note="Outside the acceptance gate, and outside the schedule counted in plan.md section 9.")

add("M7-4", "What was not moved",
    "claude-org-ja has features this plan left behind on purpose (the dashboard, Docker distribution, the "
    "cockpit, metrics), and it is stopped at M7-3. Each needs a decision, not silence.",
    ["`docs/design/not-carried.md`: every feature of claude-org-ja not in makiba, each marked build, drop or "
     "later, with the stage it belongs to when later. paddock is not on this list as something left behind, "
     "because it continues; a file makiba copied from it is listed once, as makiba's own from then on."],
    None,
    ["The list exists and no entry is unmarked; no entry marks paddock or any of its features as dropped."],
    "G7", deps=["M7-2"], gate=False,
    note="Outside the acceptance gate: closed by a person.")


PROBE_GATE = ("Do not start until probes P1 to P5 and P7 to P11 are recorded as run, with versions and date, in "
              "`docs/design/probes.md` (Results). They are run by hand before any Issue of M2 or M3, on the "
              "pinned base version. A probe run in part names the steps not run; those are run before the Issue "
              "that relies on them lands.")


def body(i) -> str:
    out = []
    out.append(f"# {i['id']} {i['title']}\n")
    if i["id"][:2] in ("M2", "M3"):
        out.append("## Before starting\n")
        out.append(PROBE_GATE + "\n")
    if i["replaces"]:
        out.append(REPLACES + "\n")
        out.append(i["replaces"] + "\n")
    out.append("## Why\n")
    out.append(i["why"] + "\n")
    out.append("## What to build\n")
    out += [f"- {b}" for b in i["build"]]
    out.append("")
    if i["carry"]:
        out.append("## Carried from\n")
        out += [f"- {c}" for c in i["carry"]]
        out.append("")
    if i["out"]:
        out.append("## Not in this Issue\n")
        out += [f"- {c}" for c in i["out"]]
        out.append("")
    out.append("## Definition of done\n")
    if i["test"] and i["test"].endswith(".rs"):
        name = i["test"].removeprefix("tests/").removesuffix(".rs")
        out.append(f"The first commit adds `{i['test']}`, failing, with the trailer "
                   f"`Acceptance-Test: {i['test']}`. At head `cargo test --test {name}` passes and asserts:\n")
    elif i["test"]:
        out.append(f"The first commit adds `{i['test']}`, failing, with the trailer "
                   f"`Acceptance-Test: {i['test']}`. At head it passes and asserts:\n")
    else:
        out.append("Checked by a person:\n")
    out += [f"- {a}" for a in i["asserts"]]
    out.append("")
    if i["note"]:
        out.append("## Notes\n")
        out.append(i["note"] + "\n")
    out.append("## Goal clause\n")
    out.append(f"- {GOALS[i['goal']]}\n")
    if i["deps"]:
        out.append("Depends on " + ", ".join(i["deps"]))
    return "\n".join(out).rstrip() + "\n"


def main() -> None:
    root = Path(__file__).resolve().parent.parent
    d = root / "issues"
    d.mkdir(exist_ok=True)
    ids = [i["id"] for i in I]
    assert len(ids) == len(set(ids)), "duplicate id"
    for i in I:
        for dep in i["deps"]:
            assert dep in ids, (i["id"], dep)
            assert ids.index(dep) < ids.index(i["id"]), (i["id"], dep)
        (d / f"{i['id']}.md").write_text(body(i), encoding="utf-8")
    lines = ["# Issues\n",
             "Generated by `tools/build_issues.py`. Each file is one Issue's body; its first line is the title.",
             "`Depends on` lines use these ids; `tools/create_issues.py` replaces them with Issue numbers.",
             "Every Issue of M2 and M3 carries a `Before starting` section: probes P1 to P5 and P7 to P11 are run first "
             "(`docs/design/probes.md`).",
             "A verb such as `close` or `pr` is both the person's command `makiba close` and the agent's tool "
             "`close` (`docs/design/spec.md`, section 12). An Issue names the verb, and its test covers the tool "
             "for an agent role.",
             "An acceptance test `tests/<name>.rs` is a Cargo integration test, run as `cargo test --test <name>` "
             "(`docs/design/spec.md`, section 14).",
             "Reworked after a merge in Python (their Issues are reopened): "
             + ", ".join(i["id"] for i in I if i["replaces"]) + ".\n"]
    for m, name in MILESTONES.items():
        lines.append(f"## {m} {name}\n")
        lines.append("| Id | Title | Goal | Depends on | Gate |")
        lines.append("| --- | --- | --- | --- | --- |")
        for i in I:
            if i["id"].startswith(m + "-"):
                lines.append(f"| [{i['id']}]({i['id']}.md) | {i['title']} | {i['goal']} | "
                             f"{', '.join(i['deps']) or '-'} | {'acceptance' if i['gate'] else 'person'} |")
        lines.append("")
    (d / "index.md").write_text("\n".join(lines), encoding="utf-8")
    print(len(I), "issues")


if __name__ == "__main__":
    main()
