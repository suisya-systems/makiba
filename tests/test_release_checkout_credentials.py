"""The release workflow's checkouts do not leave the job's token in .git/config (Issue 680).

By default `actions/checkout` writes the job's GITHUB_TOKEN into the checked-out repository's
`.git/config`, where every later step of the job can read it, the repository's own build scripts at
the tag included. No step of the release workflow needs git credentials after its checkout: the
jobs check-version, build and manifest read local files, and the publish job's one step that needs
the token creates the release through the GitHub API with GH_TOKEN. So every checkout sets
`persist-credentials: false` and passes no `token` or `ssh-key` of its own, and GH_TOKEN is set in
that step's own env only, not in the workflow's or a job's.

The workflow is staged at docs/release/release.yml and active as .github/workflows/release.yml
(tests/release_install.rs keeps the two equal); both are read here. `problems(text)` lists what is
wrong with a workflow text; the real files have none, and each mutated copy below has at least one.
"""

from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOWS = ("docs/release/release.yml", ".github/workflows/release.yml")
ACTIVE = ".github/workflows/release.yml"
# The jobs that check the repository out today; each must have exactly one checkout, so the test
# cannot pass by finding none.
JOBS = ("check-version", "build", "manifest", "publish")
TOKEN = "${{ github.token }}"


def is_checkout(step):
    return isinstance(step, dict) and str(step.get("uses", "")).startswith("actions/checkout@")


def steps(doc):
    """Every (job name, step) of the workflow."""
    out = []
    for name, job in ((doc or {}).get("jobs") or {}).items():
        for step in (job or {}).get("steps") or []:
            out.append((name, step))
    return out


def problems(text):
    doc = yaml.safe_load(text) or {}
    found = [(job, step) for job, step in steps(doc) if is_checkout(step)]
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
    if "GH_TOKEN" in (doc.get("env") or {}):
        out.append("the workflow's env sets GH_TOKEN")
    for name, job in (doc.get("jobs") or {}).items():
        if "GH_TOKEN" in ((job or {}).get("env") or {}):
            out.append(f"job {name}'s env sets GH_TOKEN")
    holders = [(job, step) for job, step in steps(doc) if isinstance(step, dict) and "GH_TOKEN" in (step.get("env") or {})]
    if len(holders) != 1:
        out.append(f"{len(holders)} steps set GH_TOKEN, not one")
    for job, step in holders:
        if job != "publish" or "release create" not in str(step.get("run", "")):
            out.append(f"a step of job {job} that does not create the release sets GH_TOKEN")
        if step["env"]["GH_TOKEN"] != TOKEN:
            out.append(f"GH_TOKEN of a step of job {job} is {step['env']['GH_TOKEN']!r}, not {TOKEN}")
    return out


def read(path):
    return (ROOT / path).read_text(encoding="utf-8")


@pytest.mark.parametrize("path", WORKFLOWS)
def test_every_checkout_of_release_drops_the_job_token(path):
    assert problems(read(path)) == []


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


def _token_on_publish(doc):
    step = _checkout_of(doc, "publish")
    step["with"] = {**(step.get("with") or {}), "token": TOKEN}


def _ssh_key_on_build(doc):
    step = _checkout_of(doc, "build")
    step["with"] = {**(step.get("with") or {}), "ssh-key": "${{ secrets.DEPLOY_KEY }}"}


def _new_job_with_a_plain_checkout(doc):
    doc["jobs"]["extra"] = {"runs-on": "ubuntu-latest", "steps": [{"uses": "actions/checkout@v4"}, {"run": "true"}]}


def _second_checkout_in_manifest(doc):
    doc["jobs"]["manifest"]["steps"].append({"uses": "actions/checkout@v4"})


def _checkout_removed_from_check_version(doc):
    job = doc["jobs"]["check-version"]
    job["steps"] = [s for s in job["steps"] if not is_checkout(s)]


def _token_at_publish_job(doc):
    job = doc["jobs"]["publish"]
    job["env"] = {**(job.get("env") or {}), "GH_TOKEN": TOKEN}


def _token_at_workflow(doc):
    doc["env"] = {**(doc.get("env") or {}), "GH_TOKEN": TOKEN}


def _token_on_publish_download(doc):
    step = next(s for s in doc["jobs"]["publish"]["steps"] if str(s.get("uses", "")).startswith("actions/download-artifact@"))
    step["env"] = {**(step.get("env") or {}), "GH_TOKEN": TOKEN}


MUTATIONS = {}
for _job in JOBS:
    MUTATIONS[f"{_job}: persist-credentials missing"] = _mutate(_without_key(_job))
    MUTATIONS[f"{_job}: persist-credentials true"] = _mutate(_set_key(_job, True))
    MUTATIONS[f"{_job}: persist-credentials the string false"] = _mutate(_set_key(_job, "false"))
MUTATIONS["publish: a token of its own"] = _mutate(_token_on_publish)
MUTATIONS["build: an ssh key of its own"] = _mutate(_ssh_key_on_build)
MUTATIONS["a new job with a plain checkout"] = _mutate(_new_job_with_a_plain_checkout)
MUTATIONS["manifest: a second checkout"] = _mutate(_second_checkout_in_manifest)
MUTATIONS["check-version: no checkout"] = _mutate(_checkout_removed_from_check_version)
MUTATIONS["publish: GH_TOKEN in the job's env"] = _mutate(_token_at_publish_job)
MUTATIONS["GH_TOKEN in the workflow's env"] = _mutate(_token_at_workflow)
MUTATIONS["publish: GH_TOKEN on the download step"] = _mutate(_token_on_publish_download)


@pytest.mark.parametrize("name", sorted(MUTATIONS))
def test_each_mutation_is_a_problem(name):
    mutated = MUTATIONS[name](read(ACTIVE))
    assert mutated != read(ACTIVE)
    assert problems(mutated) != [], name
