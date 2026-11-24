"""The merge queue's commit gets every required check (Issue 401, docs/design/merge-wait.md Q2).

`.github/workflows/ci.yml` must run on `merge_group`, and `tests`, `lint`, `real-base` and
`acceptance` must each report there. `acceptance` on `merge_group` runs no declared test: the
declared test was judged at the pull request's head. So its steps there must not read the pull
request payload, and must succeed with only the runner's environment. On `pull_request` it runs the
checker on the pull request's base and head, as before.

`problems(text)` lists what is wrong with a workflow text; the real file must have none, and each
mutated copy below must have at least one, so the check itself is shown to catch what it guards.
"""

import os
import re
import subprocess
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
REQUIRED = ("tests", "lint", "real-base", "acceptance")
JUDGED_LINE = "ACCEPTANCE: judged at the pull request's head"
TERM = re.compile(r"""^github\.event_name\s*==\s*'([a-z_]+)'$""")


def triggers(doc):
    # YAML 1.1 reads the key `on` as the boolean true.
    on = doc.get("on", doc.get(True))
    if isinstance(on, str):
        return {on}
    return set(on or ())


def holds(cond, event):
    """Whether an `if:` holds for event. Only `github.event_name == '<e>'` terms joined by `||`
    are understood; anything else raises, so a condition the check cannot read is a problem."""
    if cond is None:
        return True
    text = str(cond).strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2].strip()
    names = []
    for term in text.split("||"):
        m = TERM.match(term.strip())
        if not m:
            raise ValueError(f"condition not understood: {cond!r}")
        names.append(m.group(1))
    return event in names


def steps_for(job, event):
    return [s for s in job.get("steps", []) if holds(s.get("if"), event)]


def run_merge_group_steps(steps):
    script = "set -e\n" + "\n".join(s["run"] for s in steps)
    env = {
        "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        "GITHUB_EVENT_NAME": "merge_group",
        "GITHUB_SHA": "0123456789abcdef0123456789abcdef01234567",
        "GITHUB_REF": "refs/heads/gh-readonly-queue/main/pr-1-89abcdef0123456789abcdef0123456789abcdef",
    }
    return subprocess.run(
        ["bash", "-c", script], env=env, capture_output=True, text=True, timeout=60
    )


def problems(text):
    out = []
    doc = yaml.safe_load(text)
    if "merge_group" not in triggers(doc):
        out.append("merge_group is not a trigger")
    jobs = doc.get("jobs") or {}
    for name in REQUIRED:
        job = jobs.get(name)
        if job is None:
            out.append(f"job {name} is missing")
            continue
        try:
            if not holds(job.get("if"), "merge_group"):
                out.append(f"job {name} does not run on merge_group")
        except ValueError as e:
            out.append(f"job {name}: {e}")
    acc = jobs.get("acceptance")
    if acc is None:
        return out
    try:
        on_queue = steps_for(acc, "merge_group")
        on_pr = steps_for(acc, "pull_request")
        if holds(acc.get("if"), "push"):
            out.append("acceptance runs on push")
        if not holds(acc.get("if"), "pull_request"):
            out.append("acceptance does not run on pull_request")
    except ValueError as e:
        return out + [f"acceptance: {e}"]

    # merge_group: only run steps, no expression, no payload; they succeed and say so.
    if not on_queue:
        out.append("acceptance has no step on merge_group")
    bad = False
    for s in on_queue:
        if "run" not in s or "uses" in s:
            out.append(f"acceptance step on merge_group is not a plain run step: {s}")
            bad = True
        dump = yaml.safe_dump(s)
        if "${{" in dump or "pull_request" in dump:
            out.append(f"acceptance step on merge_group reads an expression or the payload: {s}")
            bad = True
    if on_queue and not bad:
        r = run_merge_group_steps(on_queue)
        if r.returncode != 0:
            out.append(f"acceptance on merge_group exits {r.returncode}: {r.stdout}{r.stderr}")
        elif JUDGED_LINE not in r.stdout:
            out.append(f"acceptance on merge_group does not print {JUDGED_LINE!r}: {r.stdout}")

    # pull_request: the checker on the pull request's base and head, at its head with full history.
    checkout = [s for s in on_pr if str(s.get("uses", "")).startswith("actions/checkout@")]
    if not checkout or (checkout[0].get("with") or {}).get("ref") != (
        "${{ github.event.pull_request.head.sha }}"
    ) or (checkout[0].get("with") or {}).get("fetch-depth") != 0:
        out.append("acceptance on pull_request does not check out the head with full history")
    last = on_pr[-1].get("run", "") if on_pr else ""
    if not (
        "acceptance-check.sh" in last
        and "github.event.pull_request.base.sha" in last
        and "github.event.pull_request.head.sha" in last
    ):
        out.append("acceptance on pull_request does not end with the checker on base and head")
    if any(JUDGED_LINE in str(s.get("run", "")) for s in on_pr):
        out.append("acceptance on pull_request prints the merge queue's line instead of judging")
    return out


def ci_text():
    return WORKFLOW.read_text(encoding="utf-8")


def test_ci_reports_every_required_check_on_merge_group():
    assert problems(ci_text()) == []


def test_acceptance_on_merge_group_succeeds_without_the_payload_and_says_why():
    acc = yaml.safe_load(ci_text())["jobs"]["acceptance"]
    r = run_merge_group_steps(steps_for(acc, "merge_group"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert JUDGED_LINE in r.stdout, r.stdout


def _replace_once(text, old, new):
    assert text.count(old) == 1, old
    return text.replace(old, new)


def _drop_merge_group(text):
    return _replace_once(text, "on: [push, pull_request, merge_group]", "on: [push, pull_request]")


def _acceptance_pr_only(text):
    return _replace_once(
        text,
        "    if: github.event_name == 'pull_request' || github.event_name == 'merge_group'\n",
        "    if: github.event_name == 'pull_request'\n",
    )


def _queue_step_fails(text):
    return _replace_once(text, '          echo "queue commit', '          exit 1\n          echo "queue commit')


def _queue_step_reads_payload(text):
    return _replace_once(
        text,
        '          echo "queue commit $GITHUB_SHA on $GITHUB_REF"',
        '          echo "queue commit ${{ github.event.pull_request.head.sha }}"',
    )


def _checkout_runs_on_queue(text):
    return _replace_once(
        text,
        "      - uses: actions/checkout@v4\n        if: github.event_name == 'pull_request'\n",
        "      - uses: actions/checkout@v4\n",
    )


def _queue_step_on_every_event(text):
    return _replace_once(
        text,
        "        if: github.event_name == 'merge_group'\n",
        "",
    )


@pytest.mark.parametrize(
    "mutate",
    [
        _drop_merge_group,
        _acceptance_pr_only,
        _queue_step_fails,
        _queue_step_reads_payload,
        _checkout_runs_on_queue,
        _queue_step_on_every_event,
    ],
)
def test_the_check_catches_a_broken_workflow(mutate):
    assert problems(mutate(ci_text())) != []
