# How integration tests are grouped into executables (Issue 636)

Decision note for Issue 636. Goal clause G9: it runs its own development.

Status: decided. The owner chose option A on 2026-11-28. No build Issue follows.

## 1. The question

How should makiba's integration tests be grouped into executables, once the debuginfo cut of #627 is measured? Cargo.toml has no `[[test]]` and no `autotests` key, so every `tests/<name>.rs` is its own executable, and each one statically links libmakiba and its dependencies.

The options of the Issue:

- A. Keep one executable per file and rely on #627 only. No contract change.
- B. `autotests = false` with one binary. Every open and future declared test changes form.
- C. A group target (for example `tests/suite/main.rs` with modules) beside the per-file targets, with a new declared kind (`--test suite <name>::`).
- D. `[[test]]` per file. No gain.

## 2. The decision

**A: one test executable per `tests/<name>.rs` file.** Decided by the owner on 2026-11-28, because the gain of option C does not pay for changing the declared-test contract.

## 3. The measurements

### 3.1 Before and after #639 (debug/deps)

- Before #627's cut, on origin/main c7f9440 (Issue 636's body, du on 2026-11-28): 227 test executables of 85-104 MB each, 19.8 GB in `debug/deps`.
- #639 (`debug = "line-tables-only"` for dev and test), measured by the owner after one clean `cargo test --locked --no-run`: `target/` 22G to 11G, of it `debug/deps` 20G to 8.5G and `incremental` 2.1G to 1.6G; the build 115 s to 80 s (#639's commit message).
- First comment of Issue 636 (owner's machine, 5 cores, origin/main 86407df with #639's setting applied uncommitted): 178 test executables of about 35 MB each, together 6.4 GB of a 7.4 G target. Incremental build after `touch src/lib.rs`: 36-44 s; the library rebuilds in 2 s and every test executable relinks. Linking is 50-60 % of each test executable, and the linker is already LLD, so a linker switch gains nothing (mold was not measured).

### 3.2 The re-measurement that decides

Second comment of Issue 636: origin/main d401970, after #639 and #643 (nextest as the runner), 5 cores, one build or run at a time. Option C here is the 67 self-contained test files (no `mod common`, no `set_var`/`remove_var`/`set_current_dir`, no makibad) as modules of one executable.

| | A: baseline (228 test binaries) | C: 67 files in one executable (162 binaries) |
|---|---|---|
| clean `cargo test --locked --no-run` | 87.8 s, 82.2 s | 64.7 s |
| incremental after `touch src/lib.rs` (5 samples) | mean 40.2 s | mean 31.9 s |
| `du -sh target` | 12 G | 9.4 G |
| `cargo nextest run --locked --no-fail-fast` | 380.8 s (a first run under load: 583.5 s) | 359.9 s |

So C saves about 8 s (20 %) of an incremental build and about 2.6 GB, and no measurable run time under nextest, where every test has its own process. The other files cannot join the group without more work: 82 call `set_var` or `remove_var` and 85 use `tests/common/makibad.rs`, whose knowledge folder is set once per process.

## 4. Why A

The file stem is part of the declared-test contract. Option C would change every place that derives the target from `tests/<name>.rs`:

1. `scripts/acceptance-check.sh`: `rs_name`, `is_test_file`, `classify` and `run_rs`, whose substring filter would need anchoring.
2. `docs/acceptance-check.md`: rule 3 and the scope section, and the `mod` line a test commit would add to the group's `main.rs`.
3. `src/daemon/gate.rs`: `is_helper` and `route_helper`.
4. `src/hooks/test_commit.rs`: `is_test_target` (rule 4's scan would skip modules).
5. `.github/workflows/ci.yml`: the scope parser, its outputs and the `--test` loop.
6. `.config/nextest.toml`: the `binary()` filters (grouping `build_skills_path` made nextest refuse to start).

It would also change the 19 lines of `docs/design/lines.md` that name their holders as `tests/<name>.rs::fn`, and moved files broke tests that assert their own path (`write_hold_log`). A change to the checker, the gate, `.github` or `tests/common` is held by merge order and lands only by the owner's lift or approval. A 20 % cut of an incremental build and 2.6 GB of disk do not pay for that; #639 already halved the target without touching any contract.

## 5. Left open

B (one binary for every integration test) is the larger lever now that nextest runs one process per test (#643), which removes the cross-file `set_var` races that made one process unsafe under plain `cargo test`. It is a separate question and was not measured. It would change the form of every open and future declared test.
