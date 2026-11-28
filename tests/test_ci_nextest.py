"""The tests job and the workers run the suite under cargo-nextest (Issue 642).

The tests job of `.github/workflows/ci.yml` installs the pinned cargo-nextest with
`scripts/install-nextest.sh` and runs the suite with `cargo nextest run --locked --no-fail-fast`,
plus `cargo test --locked --doc` (nextest runs no doctests). `skills/delegate/SKILL.md`'s guidance
on the brief names `cargo nextest run`, so a worker is told to run the suite under it.
"""

from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
DELEGATE = ROOT / "skills" / "delegate" / "SKILL.md"
INSTALLER = "scripts/install-nextest.sh"
SUITE = "cargo nextest run --locked --no-fail-fast"
DOCTESTS = "cargo test --locked --doc"


def runs_of_the_tests_job():
    job = yaml.safe_load(WORKFLOW.read_text(encoding="utf-8"))["jobs"]["tests"]
    return [s["run"] for s in job.get("steps", []) if "run" in s]


def test_the_tests_job_installs_the_pinned_nextest():
    assert any(f"bash {INSTALLER} " in run for run in runs_of_the_tests_job())


def test_the_tests_job_runs_the_suite_under_nextest_and_the_doctests():
    runs = runs_of_the_tests_job()
    installed = [i for i, run in enumerate(runs) if INSTALLER in run]
    suite = [i for i, run in enumerate(runs) if SUITE in run]
    assert installed and suite, "the tests job does not install nextest and run the suite with it"
    assert installed[0] < suite[0]
    assert any(DOCTESTS in run for run in runs)


def test_the_delegate_skill_names_cargo_nextest_run():
    assert "cargo nextest run" in DELEGATE.read_text(encoding="utf-8")
