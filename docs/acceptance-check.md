# Acceptance check in CI

Carried from paddock (`docs/design/acceptance-check.md`, `scripts/acceptance-check.sh`). Issue #3.

## What runs

The `acceptance` job in [`.github/workflows/ci.yml`](../.github/workflows/ci.yml) runs on every pull request:

```sh
bash scripts/acceptance-check.sh <base sha> <head sha>
```

[`scripts/acceptance-check.sh`](../scripts/acceptance-check.sh) takes the branch's own commits: the head's first-parent line that the base cannot reach. The oldest of them is the test commit (its parent is in the base). If it carries `Acceptance-Test: <path>` trailers (files or directories), the check fails unless all of these hold:

1. The test commit is not a merge, and no other own commit declares. So no join commit comes before the test commit.
2. The test commit changes only declared paths and test helpers (files under `tests/` whose name is not `test_*.py`, such as `conftest.py`).
3. Every declared test file `test_*.py` (for a directory, every `test_*.py` under it) fails at the test commit and passes at head, each run as `python3 -m pytest <path>` from the tree root. A file that collects no test at the test commit (pytest exit 5) does not count as failing.
4. The own commits after the test commit leave the declared paths, test helpers, `.github/` and the checker itself unchanged. New `test_*.py` files may be added.

The base may be merged into the branch after the test commit; a merge that brings in anything not in the base fails. A base merge counts in rule 4 only for files it leaves different from every parent (its own conflict resolution). Renames count as both paths.

No trailer: the job passes and prints `ACCEPTANCE: none`. The last line of the log is always `ACCEPTANCE: pass`, `none` or `fail`. Tests: [`tests/test_acceptance_check.py`](../tests/test_acceptance_check.py).

## The landing gate

[`.github/acceptance-check`](../.github/acceptance-check) holds one line, `acceptance`: the name of the job above. When it is on the base branch, claude-org-ja's landing gate (`tools/acceptance_gate.py`) takes that job's success at head as the test-first evidence. Rename the job and the file together; `tests/test_acceptance_check.py` checks that they match.

## Settings only the owner can make

Branch protection on `main`: require the checks `acceptance`, `tests` and `lint`, and require branches to be up to date before merging.

## Known limits

- A pull request without a declaration passes (`none`).
- A failure is any non-zero pytest exit other than 5; an assertion failure and an import error are not told apart. A declared test that is skipped at head passes.
- The checker is taken from the base revision, but the workflow file is read from the pull request, so a PR that edits the `acceptance` job itself is not stopped by the job. Review of `.github/` changes stays with the person.
- The pull request that adds the checker fails its own `acceptance` job (the checker lands after its test commit). It is merged in `human` mode.
