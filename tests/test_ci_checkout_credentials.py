"""CI's checkouts do not leave the job's token in .git/config (Issue 678).

By default `actions/checkout` writes the job's GITHUB_TOKEN into the checked-out repository's
`.git/config`, where every later step of the job, including the pull request's own code (build
scripts, tests, the declared tests of the acceptance job), can read it. No step of
`.github/workflows/ci.yml` needs git credentials after its checkout: every later git call reads
local objects only. So every checkout of the workflow sets `persist-credentials: false` and passes
no `token` or `ssh-key` of its own. The one step that needs the token, the acceptance job's read of
the approval comments, takes it as GH_TOKEN in its own env (tests/test_ci_acceptance_approval.py).

`problems(text)` lists what is wrong with a workflow text; the real file has none, and each mutated
copy below has at least one.
"""

from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
# The jobs that check the repository out today; each must have exactly one checkout, so the test
# cannot pass by finding none.
JOBS = ("scope", "tests", "real-base", "lint", "acceptance")


def is_checkout(step):
    return isinstance(step, dict) and str(step.get("uses", "")).startswith("actions/checkout@")


def checkouts(doc):
    """Every (job name, step) of the workflow whose step is a checkout."""
    out = []
    for name, job in ((doc or {}).get("jobs") or {}).items():
        for step in (job or {}).get("steps") or []:
            if is_checkout(step):
                out.append((name, step))
    return out


def problems(text):
    doc = yaml.safe_load(text)
    found = checkouts(doc)
    out = []
    for name in JOBS:
        n = sum(1 for job, _ in found if job == name)
        if n != 1:
            out.append(f"job {name} has {n} checkout steps, not one")
    for job, step in found:
        given = step.get("with") or {}
        if given.get("persist-credentials") is not False:
            out.append(
                f"a checkout of job {job} has persist-credentials {given.get('persist-credentials')!r}, not false"
            )
        for key in ("token", "ssh-key"):
            if key in given:
                out.append(f"a checkout of job {job} passes {key}")
    return out


def ci_text():
    return WORKFLOW.read_text(encoding="utf-8")


def test_every_checkout_of_ci_drops_the_job_token():
    assert problems(ci_text()) == []


def _checkout_of(doc, job):
    return next(s for s in doc["jobs"][job]["steps"] if is_checkout(s))


def _mutate(edit):
    def apply(text):
        doc = yaml.safe_load(text)
        edit(doc)
        return yaml.safe_dump(doc, sort_keys=False)

    return apply


def _without_key(job):
    def edit(doc):
        step = _checkout_of(doc, job)
        given = step.get("with") or {}
        given.pop("persist-credentials", None)
        if given:
            step["with"] = given
        else:
            step.pop("with", None)

    return edit


def _set_key(job, value):
    def edit(doc):
        step = _checkout_of(doc, job)
        step["with"] = {**(step.get("with") or {}), "persist-credentials": value}

    return edit


def _token_on_acceptance(doc):
    step = _checkout_of(doc, "acceptance")
    step["with"] = {**(step.get("with") or {}), "token": "${{ github.token }}"}


def _ssh_key_on_lint(doc):
    step = _checkout_of(doc, "lint")
    step["with"] = {**(step.get("with") or {}), "ssh-key": "${{ secrets.DEPLOY_KEY }}"}


def _new_job_with_a_plain_checkout(doc):
    doc["jobs"]["extra"] = {"runs-on": "ubuntu-latest", "steps": [{"uses": "actions/checkout@v4"}, {"run": "true"}]}


def _second_checkout_in_tests(doc):
    doc["jobs"]["tests"]["steps"].append({"uses": "actions/checkout@v4"})


def _checkout_removed_from_lint(doc):
    doc["jobs"]["lint"]["steps"] = [s for s in doc["jobs"]["lint"]["steps"] if not is_checkout(s)]


MUTATIONS = {}
for _job in JOBS:
    MUTATIONS[f"{_job}: persist-credentials missing"] = _mutate(_without_key(_job))
    MUTATIONS[f"{_job}: persist-credentials true"] = _mutate(_set_key(_job, True))
    MUTATIONS[f"{_job}: persist-credentials the string false"] = _mutate(_set_key(_job, "false"))
MUTATIONS["acceptance: a token of its own"] = _mutate(_token_on_acceptance)
MUTATIONS["lint: an ssh key of its own"] = _mutate(_ssh_key_on_lint)
MUTATIONS["a new job with a plain checkout"] = _mutate(_new_job_with_a_plain_checkout)
MUTATIONS["tests: a second checkout"] = _mutate(_second_checkout_in_tests)
MUTATIONS["lint: no checkout"] = _mutate(_checkout_removed_from_lint)


@pytest.mark.parametrize("name", sorted(MUTATIONS))
def test_each_mutation_is_a_problem(name):
    mutated = MUTATIONS[name](ci_text())
    assert mutated != ci_text()
    assert problems(mutated) != [], name
