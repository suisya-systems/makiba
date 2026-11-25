# A fix that must change a test helper (Issue 511)

Design note for Issue 511, follow-up of Issue 506. Goal clause G9: it runs its own development. Design only: the decision is the person's, since it changes the acceptance checker and the merge gate.

## 1. The problem, and what is true today

### 1.1 The two rules that refuse

- **The checker** (`scripts/acceptance-check.sh`, `own_changes` and the loop after it; rule 4 of `docs/acceptance-check.md`): every own commit after the test commit M must leave unchanged the declared paths, **every** test helper (`is_helper`: a file under `tests/` that is neither a Rust target nor a `test_*.py`), `.github/` and the checker. A base merge counts only for the files it leaves different from every parent.
- **The gate** (`src/daemon/gate.rs`, `src/daemon/door.rs` `judge`) refuses in two places, and they are not the same one:
  - condition (a), `check_protected`: protects the declared paths, the test infrastructure (`INFRA_NAMES`, workflows) and the paths M itself changed. A helper M left alone is **not** protected by (a).
  - edited tests (Issue 282), `gate::edited_tests` read in `door.rs` `judge`: a file under a folder `tests` that exists at M^, is not declared, was not changed by M, differs at head and is not at head as the base holds it. Under `merge_approval_mode` `acceptance` an allowed answer becomes `tests_edited` (refused); under `human` it warns and the person decides by key (`makiba merge approve`).

So the Issue's "the merge gate's condition (a) does the same" holds for a helper the test commit changed, and for a declared test (the #475 case); for #506 the gate's refusal comes from the edited-tests check, not from (a). (At #506's head condition (b) would pass by its fallback: `tests` failed at M and succeeded at head.) This note covers both.

- **GitHub** requires the `acceptance` check on `main`. Whatever the gate says, a pull request whose `acceptance` job fails cannot land until the owner removes that requirement (the lift: required check removed, web-UI merge, check restored).

### 1.2 Case 1: #506 (PR 507), the fix is the helper

Test commit `8a16fad` adds only the declared `tests/local_list_while_ending.rs`. The fix `e0a0522` changes only `tests/common/local.rs` (the test base `Local`). The fix cannot go in M (rule 3: the declared test would pass at M); no trailer gives `no_manifest`, refused under `acceptance` unless docs-only. Rule 4 fails at head; the gate's edited-tests check names `tests/common/local.rs`. Only the lift lands it.

### 1.3 Case 2: #475 (PR 500), a clean base merge into a declared test

M declares `tests/queue_ready.rs` and `tests/pr_merge_state.rs`. The base merge `5632beb` brought #504 (`8896d16`), which also edited `tests/pr_merge_state.rs`. Git merged that file with no conflict (`git merge-tree --write-tree` of the two parents gives the same blob; the conflicts were in `docs/design/spec.md`, `skills/pull-request/SKILL.md`, `src/daemon/prwatch.rs`), but the result differs from both parents, so rule 4 counts it as the branch's own change, and the gate's condition (a) refuses it (M's entry differs from head's, and head's is not the base's). The route the gate names ("rebase so the test commit sits on the new base") rewrites a pushed branch, which no worker or secretary does (no force push), so only the lift landed it.

### 1.4 What must hold

- A helper change after M must not weaken the pull request's own declared test unseen: e.g. a fix that makes an asserting helper a no-op passes rule 3 (fails at M, passes at head). No mechanical rule tells a fix from a weakening here; a person must read the helper diff.
- A helper change must not weaken **another open pull request's** declared tests unseen. Today that hole is already open for helper changes made in a test commit (rule 2 allows them): after such a landing, another pull request's test commit M2 still sits on the old helper; its declared test was shown failing with the old helper and passes at head with the new one, and neither rule 4 (a base merge brings it) nor the gate (head holds it as the base does) names it. The Issue 461 landing notice (`docs/design/landing-test-commits.md`) tells those runs' workers of a `tests/common/` landing, but nothing stops or asks.
- makibad runs only git and `gh` (pending 50): no option may make makibad run a test.

## 2. Options

### A. Baseline: rule 4 stays, the lift stays

- Lets through: nothing new; every such pull request lands only by the lift and a web-UI merge.
- Keeps out: every helper change after M, from both the checker and the makiba route.
- Who decides: the owner, per pull request, by changing branch protection.
- Other open pull requests: only the person's eye at the web-UI merge, plus the Issue 461 notice; the hole of 1.4 stays.
- Files: `docs/acceptance-check.md` known limits gain one line naming this route for helper fixes and clean base merges.
- Cost: two settings changes by the owner per pull request, and a window in which `main` lacks the required check.

### B. Helper first, in its own pull request

The helper change lands alone (no trailer: `acceptance` passes with `none`; the gate gives `no_manifest`, which under `acceptance` the person merges in the web UI, no lift), then the test-first pull request on the new base.

- Lets through: a helper change that is a preparation, not the fix.
- Keeps out: nothing the person does not see; but when the helper **is** the fix (#506), the test lands after it and passes at its test commit, so rule 3 fails: no test-first evidence. Does not help #475.
- Files: docs only. Not a fix for this Issue's case; listed to show why a rule change is needed.

### C. Rule only: a helper may change after M when rule 3 holds

The checker drops untouched helpers from rule 4; the gate drops them from edited tests. Nothing asks the person.

- Lets through: #506; also a helper change that weakens the pull request's own declared test (1.4), unseen.
- Keeps out: changes to declared paths, helpers M changed, `.github/`, the checker.
- Who decides: the rule. Not recommended: it opens the hole that rule 4 closes.

### D. The rule lets it through, the person's key accepts it at the merge (recommended)

Split rule 4 into what CI can judge and what only the person can:

1. **Checker** (rule 4): a test helper that M did not change may change after M, be added after M, or be deleted after M. The declared paths, every path M changed, `.github/`, the checker, and every helper that the gate counts as test infrastructure (`gate.rs` `is_infra`: a file named as in `INFRA_NAMES` or `INFRA_PREFIXES`, such as `tests/conftest.py`) stay protected, so the checker and the gate agree on which helpers the route covers (one list, checked by a test as `tests/acceptance_rule_shared.rs` checks rule 2). Rule 3 is unchanged, so the declared test still fails at M and passes at head. Each such helper is printed before the last line: `acceptance-check: test helper <path> changed after the test commit; the person accepts it at the merge`. The last line stays `ACCEPTANCE: pass`.
2. **Checker, base merges** (case 2): a base merge counts in rule 4 only for the files it leaves different from git's clean merge of its parents: a file `git merge-tree --write-tree <p1> <p2>` merges with no conflict and that the commit holds as that tree does is not its own resolution. A conflicted file, or one resolved differently, counts as now.
3. **Gate**: the edited-tests list is split. A helper (same classification as the checker, infrastructure excluded) that M left alone and that differs between M and head, read over the **union** of the paths of M's tree and head's tree (so a helper added after M and one deleted after M count, not only those that exist at M^ as `edited_tests` reads today), and that head does not hold as the base does, goes to a new list `helpers_changed`; a declared or M-changed path whose head entry equals the entry of `git merge-tree --write-tree M base` with no conflict on that path goes to `merged_tests` and is no longer a condition (a) refusal. Either list makes an allowed answer **warn under both modes**: the answer is pending (the existing `warn` → pending entry of Issue 432), the entry names the paths and `git diff --stat` of each from M to head, and only `makiba merge approve <id>` (the person's key) lands it. Each warning line carries, per path, the object ids it was judged from (M's and head's entry; for `merged_tests` also the base's), so the line the person was shown is bound to those contents: `merge approve` runs the gate again and compares the warnings by string (`shown_warnings`), and any other content gives a new line, so the approval is refused and the person is shown the entry again. Other edited tests (an existing test target edited) stay as today (`tests_edited` under `acceptance`).
4. **Gate, other open pull requests**: a pull request whose base holds a test helper differently from M^ (the base changed a helper after the test commit was made, through any route) and M did not change it, has `base_helpers` named: its declared test was never shown failing with that helper. That warns the same way (key). Its line names each helper with the object ids at M^ and at the base, and the entry shows `git diff --stat M^ <base> -- <helpers>`; a later landing that changes the same helper again changes the base's object id, so a pending approval made on the earlier content is refused at `merge approve`, as in point 3. The helper is read at the base the gate fetched, which is the base GitHub lands on only while the branch is required to be up to date (branch protection, as now). It clears when the test commit sits on a base with that helper (a new test-first branch).

- Lets through: #506 and #475, each with the person's key at the merge; nothing else new. Not covered: a helper that is test infrastructure (`tests/conftest.py`, `pytest.ini` and the other names of `is_infra`) keeps rule 4 and condition (a), so a fix there still needs the lift.
- Keeps out: a helper change landing without the person reading it on the makiba route; a weakened declared test of another open pull request landing without the person (point 4); conflict resolutions in declared tests; `.github/` and checker changes (lift, as now).
- Who decides: the person's key, per pull request (the pending merge entry), under either mode.
- What the required check no longer proves: `acceptance` success no longer means "no helper changed after M". A web-UI merge by a person, or claude-org-ja's landing gate (which reads only that check), does not see it; the checker's log line does. Known limit.
- Files: `scripts/acceptance-check.sh` (rule 4, `own_changes`), `docs/acceptance-check.md` (rule 4, the base-merge paragraph, a section "A fix that must change a test helper", known limits), `tests/test_acceptance_check.py` cases or a new declared test; `src/daemon/gate.rs` (`edited_tests` split, `check_protected` clean-merge exception, `base_helpers`), `src/daemon/door.rs` `judge` (the warnings, payload fields `helpers_changed`, `merged_tests`, `base_helpers`, the pending entry's text), `src/daemon/belt.rs` (a range task's stop for these warnings, as for edited tests), `docs/design/spec.md` (the `gate` row and the `acceptance_gate` payload), `docs/design/belt.md` line 363.

### E. As D, with the helper declared up front

M carries `Acceptance-Helper: <path>` for each helper the fix may change; only those may change.

- Adds: the intent is pinned in the digest before the fix.
- Costs: the fix often finds the helper only while it is written; changing M then rewrites the test commit, which the gate's pin rejects ("ask the person") and which needs a force push. Not recommended; the key at the merge already shows the person the paths.

## 3. Recommendation

D. It is the smallest change that lets #506 and #475 land without touching branch protection, and it keeps the one judgment no rule can make (fix or weakening) with the person, by the key that already exists for pending merges. CI keeps judging what it can (rule 3, the protected test commit, `.github/`, the checker); makibad keeps running only git. Point 4 also closes the older hole of 1.4 for helper changes made in test commits. A, the baseline, remains the route for `.github/` and checker changes, and for anything the person refuses at the key.

## 4. Build Issues

Order: Issue 1 (gate) before Issue 2 (checker). With the gate first, a helper change the checker later lets through meets the key; with the checker first it would meet the `tests_edited` refusal under `acceptance`, which is safe but no route.

### Issue 1: The gate asks the person by key for a helper changed after the test commit, a clean base merge into a declared test, or a helper the base changed after the test commit

- Done when: in `merge_approval_mode` `acceptance` and `human`, a pull request whose only after-M change outside code is (i) a helper M left alone, (ii) a declared path whose head entry is the clean merge of M and the base, or (iii) a base helper differing from M^, gets an answer with `allow` true and `warn` true, so that `merge request` answers pending with a pending entry naming the paths, their object ids and their diff stat, and lands only after `makiba merge approve`; a helper added after M and one deleted after M are in `helpers_changed`; an approval is refused when the head or the base changes a listed path's content after it was shown (a second landing that changes the same base helper); a helper that is test infrastructure (`tests/conftest.py`) is still refused by condition (a); an edited existing test target is still `tests_edited` under `acceptance`; a conflict resolution in a declared test is still refused by condition (a); the `acceptance_gate` payload carries the three lists; a range task's pull request gets a stop for them; `docs/design/spec.md` and `belt.md` say so.
- Declared test: `tests/gate_helper_change.rs` (the gate in process on a local repository and a `gh` stand-in, as `tests/acceptance_gate.rs`): one case per kind, plus the two that stay refused. Also `tests/belt_gate.rs`: its fixture `edits_a_test` edits `tests/other.py`, which the shared classification makes a helper, so under D it would read as `helpers_changed`. The test commit moves that fixture to an existing test target (`tests/test_other.py`, so its three edited-test cases keep their meaning) and adds a case that fails before the build: a helper edited after the test commit on a range task's pull request is a warning and a stop.

### Issue 2: The checker lets a helper the test commit left alone change after it, and does not count a clean base merge as the branch's own

- Done when: `scripts/acceptance-check.sh` passes a branch whose fix changes, adds or deletes only a helper M left alone (declared test fails at M, passes at head) and prints the helper line; fails one whose fix changes `tests/conftest.py`; fails one whose fix changes a helper M changed, a declared path, `.github/` or the checker; passes a branch whose base merge left a declared test as git's clean merge; fails one whose merge resolved a conflict in a declared test; `docs/acceptance-check.md` documents the route and the limit of 2.D.
- Declared test: `tests/test_acceptance_helper_change.py` (temporary repositories, as `tests/test_acceptance_check.py`).
- This pull request changes the checker after its test commit: it lands by the lift, once (known limit).

## 5. Known limits of D

- The required check passes a helper change; only the makiba route asks the person. A web-UI merge sees it only in the log.
- A helper M itself changed cannot change after M (it is part of the test, and pinned).
- A helper that is test infrastructure (`is_infra`) keeps the lift.
- Point 4 asks the person for every open test-first pull request after any landing that changes a helper; it does not tell whether that test uses the helper. Clearing it without the key needs a new test commit on the new base.
- `.github/` and checker changes keep the lift.

## 6. Design review

Round 1, reader codex, verdict passed, findings blocker 2, major 1, minor 0. Each was checked against the code and taken in:

- Blocker: `edited_tests` reads only files that exist at M^ (`tp.iter()` in `gate.rs`), so a helper added after M would pass a relaxed checker unasked. Held. Point 3 now reads the union of M's and head's paths; Issue 1's definition of done names the added and the deleted helper.
- Blocker: `merge approve` compares the gate's warnings with the shown ones by string (`shown_warnings` in `door.rs`), so a warning that names only paths would let an approval stand over a second base change of the same helper. Held. Points 3 and 4 put the object ids in each line and show the M^-to-base diff stat; Issue 1 tests the refused approval.
- Major: `tests/conftest.py` is a helper by `is_helper` and test infrastructure by `gate.rs` `is_infra`, so condition (a) would still refuse it. Held (no such file is tracked today). Infrastructure helpers are left out of the route in the checker and the gate alike, with a test that the two lists agree; named in the known limits.

## 7. The person's decision

Pending entry 67, answered yes: option D. Q2 yes (a clean base merge into a declared test is in the route); Q3 (a) (a helper the base changed after the test commit warns for the key, from any route); Q4 no (no advance declaration of helpers in the test commit); Q5 yes (the build Issues name the edited-tests check for #506 and condition (a) for #475); Q6 yes (`.github/`, checker and test-infrastructure helper changes keep the lift); Q7 yes (the gate Issue before the checker Issue). #506 landed afterwards as e21e8da (PR 507), by the lift.
