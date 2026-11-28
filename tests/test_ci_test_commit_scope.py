"""At a test-commit head CI runs only the declared tests (Issue 481, build 2 of
docs/design/test-commit-scoped-ci.md, sections 3.1 and 3.2).

`.github/workflows/ci.yml` has a job `scope`, run on every event. On pull_request it runs the base's
`scripts/acceptance-check.sh --scope <base> <head>` and writes `test_commit=true` and the declared
targets only when that exits 0 and its first line is exactly `test-commit <head sha>`; anything
else, and every other event, writes `test_commit=false`. With T = pull_request and
`needs.scope.outputs.test_commit == 'true'`, `tests` runs only the declared tests at T, `real-base`
runs nothing and succeeds, and `acceptance` fails at once, so a shortened head never reads as
passed. Everywhere else the full jobs run as before.

The file is read with pyyaml. Conditions are evaluated by a small reader of the terms the workflow
uses (anything else is a problem), and the steps are run with bash in temporary repositories, with
stand-in `cargo` and `python3` for the shortened `tests` step. `problems(text)` lists what is wrong
with a workflow text; the real file has none, and each mutated copy below has at least one.
"""

import os
import re
import subprocess
import tempfile
from pathlib import Path

import pytest
import yaml

from test_acceptance_check import ENV, SCRIPT, Repo

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
SCOPED = ("tests", "real-base", "acceptance")
EVENTS = ("pull_request", "merge_group", "push")
# The values the scope output can have where T must not hold: false, and empty (the scope job
# failed or wrote nothing). GitHub compares strings ignoring case, so 'True' is not among them.
NOT_TRUE = ("false", "")
FAIL_LINE = "ACCEPTANCE: fail"

# The terms a condition of these jobs may use; anything else is not understood.
TOKEN = re.compile(
    r"\s*(?:(?P<paren>[()])|(?P<op>&&|\|\|)|(?P<status>!cancelled\(\)|always\(\))"
    r"|(?P<name>github\.event_name|needs\.scope\.outputs\.test_commit)\s*(?P<cmp>==|!=)\s*'(?P<value>[^']*)')"
)


class Ctx:
    def __init__(self, event, scope, ok=True, cancelled=False):
        self.event = event
        self.scope = scope
        self.ok = ok  # every needed job succeeded
        self.cancelled = cancelled


def holds(cond, ctx):
    """Whether an `if:` holds in ctx, read as GitHub reads it: `&&` before `||`, string
    comparison ignoring case, and an implicit `success()` when no status function is named."""
    text = "" if cond is None else str(cond).strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2].strip()
    parts, status, pos = [], False, 0
    while pos < len(text):
        m = TOKEN.match(text, pos)
        if not m or m.end() == pos:
            raise ValueError(f"condition not understood: {cond!r}")
        pos = m.end()
        if m["paren"]:
            parts.append(m["paren"])
        elif m["op"]:
            parts.append(" and " if m["op"] == "&&" else " or ")
        elif m["status"]:
            status = True
            parts.append(str(not ctx.cancelled if m["status"] == "!cancelled()" else True))
        else:
            actual = ctx.event if m["name"] == "github.event_name" else ctx.scope
            equal = actual.lower() == m["value"].lower()
            parts.append(str(equal if m["cmp"] == "==" else not equal))
    value = eval("".join(parts) or "True", {"__builtins__": {}})  # only True/False/and/or/()
    if not status:
        value = value and ctx.ok and not ctx.cancelled
    return bool(value)


def steps_for(job, ctx):
    return [s for s in job.get("steps", []) if holds(s.get("if"), ctx)]


def t_ctx():
    return Ctx("pull_request", "true")


def not_t_contexts():
    for event in EVENTS:
        for scope in ("true", *NOT_TRUE) if event != "pull_request" else NOT_TRUE:
            yield Ctx(event, scope)


def is_checkout(step):
    return str(step.get("uses", "")).startswith("actions/checkout@")


def run_steps(steps, env, cwd):
    """The steps' run scripts in one bash -e, as GitHub's default shell runs a step."""
    script = "\n".join(s["run"] for s in steps)
    return subprocess.run(
        ["bash", "-e", "-c", script], cwd=cwd, env=env, capture_output=True, text=True, timeout=120
    )


def base_env(tmp):
    tmp.mkdir(parents=True, exist_ok=True)
    return {"PATH": os.environ.get("PATH", "/usr/bin:/bin"), "RUNNER_TEMP": str(tmp), "HOME": str(tmp)}


def problems(text):
    out = []
    jobs = yaml.safe_load(text).get("jobs") or {}

    scope = jobs.get("scope")
    if scope is None:
        return out + ["job scope is missing"]
    if "if" in scope or "needs" in scope:
        out.append("job scope does not run on every event")
    outputs = scope.get("outputs") or {}
    for key in ("test_commit", "rs", "py"):
        if outputs.get(key) != f"${{{{ steps.scope.outputs.{key} }}}}":
            out.append(f"job scope does not output {key} from its step scope")
    if "needs" in (jobs.get("lint") or {}):
        out.append("lint has needs")

    try:
        for name in SCOPED:
            job = jobs.get(name)
            if job is None:
                out.append(f"job {name} is missing")
                continue
            if job.get("needs") not in ("scope", ["scope"]):
                out.append(f"job {name} does not need scope")
            events = ("pull_request", "merge_group") if name == "acceptance" else EVENTS
            for event in events:
                for value in ("true", *NOT_TRUE):
                    if not holds(job.get("if"), Ctx(event, value, ok=False)):
                        out.append(f"job {name} does not run on {event} (scope {value!r}) when scope failed")
            if holds(job.get("if"), Ctx("pull_request", "true", ok=False, cancelled=True)):
                out.append(f"job {name} runs when the run is cancelled")

            at_t = steps_for(job, t_ctx())
            for s in job.get("steps", []):
                held_t = holds(s.get("if"), t_ctx())
                held_else = [c for c in not_t_contexts() if holds(s.get("if"), c)]
                if held_t and held_else and (name != "tests" or "if" in s):
                    out.append(
                        f"{name}: step {s.get('name') or s.get('uses') or s.get('run')!r} holds at a"
                        f" test-commit head and also on {held_else[0].event} with scope {held_else[0].scope!r}"
                    )
            for event in EVENTS:
                full = [s for s in job.get("steps", []) if holds(s.get("if"), Ctx(event, "false"))]
                for c in not_t_contexts():
                    if c.event == event and steps_for(job, c) != full:
                        out.append(f"{name}: on {event} the steps with scope {c.scope!r} differ from scope 'false'")
            shortened = [s for s in at_t if not any(holds(s.get("if"), c) for c in not_t_contexts())]
            if name == "tests":
                if len(shortened) != 1 or not at_t or at_t[-1] is not shortened[0]:
                    out.append("tests at a test-commit head does not end with one shortened step")
            elif at_t != shortened or len(at_t) != 1:
                out.append(f"{name} at a test-commit head is not one shortened step")
            for s in shortened:
                if "uses" in s or "run" not in s:
                    out.append(f"{name}: a shortened step is not a plain run step")
            if name == "acceptance" and any(is_checkout(s) for s in at_t):
                out.append("acceptance checks out at a test-commit head")
    except ValueError as e:
        return out + [str(e)]
    if out:
        return out

    # Run the shortened steps of real-base and acceptance; they read nothing.
    for name, want in (("real-base", 0), ("acceptance", 1)):
        steps = steps_for(jobs[name], t_ctx())
        if any("${{" in yaml.safe_dump(s) for s in steps):
            out.append(f"{name} at a test-commit head reads an expression")
            continue
        with tempfile.TemporaryDirectory() as d:
            r = run_steps(steps, base_env(Path(d) / "runner"), d)
        if r.returncode != want:
            out.append(f"{name} at a test-commit head exits {r.returncode}, not {want}: {r.stdout}{r.stderr}")
        if name == "acceptance" and (r.stdout.strip().splitlines() or [""])[-1] != FAIL_LINE:
            out.append(f"acceptance at a test-commit head does not end with {FAIL_LINE!r}: {r.stdout}")
    rb = " ".join(s.get("run", "") for s in steps_for(jobs["real-base"], t_ctx()))
    if any(w in rb for w in ("install", "cargo", "nextest")):
        out.append("real-base installs or builds at a test-commit head")

    # The full jobs are still there under not T.
    full_tests = [s.get("run") for s in steps_for(jobs["tests"], Ctx("pull_request", "false"))]
    for cmd in ("cargo nextest run --locked --no-fail-fast", "python3 -m pytest -q tests"):
        if cmd not in full_tests:
            out.append(f"tests under not T does not run {cmd!r}")
    full_rb = " ".join(s.get("run", "") for s in steps_for(jobs["real-base"], Ctx("merge_group", "false")))
    for part in ("cargo nextest run --locked --profile ci --no-fail-fast", "HC_BIN=/nonexistent", "took < 10"):
        if part not in full_rb:
            out.append(f"real-base under not T lost {part!r}")
    return out


def ci_text():
    return WORKFLOW.read_text(encoding="utf-8")


def jobs():
    return yaml.safe_load(ci_text())["jobs"]


def test_the_workflow_shortens_only_a_test_commit_head():
    assert problems(ci_text()) == []


# The scope step, run in temporary repositories.


def scope_steps():
    return jobs()["scope"]["steps"]


def run_scope(cwd, event, tmp, base="", head=""):
    """The scope job's steps that hold on event (the checkout is the repository at cwd), with the
    payload's base and head; returns the result and the outputs written."""
    steps = [s for s in scope_steps() if holds(s.get("if"), Ctx(event, ""))]
    checkout = [s for s in steps if is_checkout(s)]
    if event == "pull_request":
        assert len(checkout) == 1, steps
        assert checkout[0]["with"] == {"ref": "${{ github.event.pull_request.head.sha }}", "fetch-depth": 0}
    else:
        assert checkout == [], steps
    steps = [s for s in steps if not is_checkout(s)]
    assert [s.get("id") for s in steps] == ["scope"], steps
    step = steps[0]
    assert "${{" not in step["run"], step["run"]
    payload = {
        "${{ github.event.pull_request.base.sha }}": base,
        "${{ github.event.pull_request.head.sha }}": head,
    }
    env = {**ENV, **base_env(tmp / "runner")}
    for k, v in (step.get("env") or {}).items():
        assert v in payload, (k, v)
        env[k] = payload[v]
    output = tmp / "github_output"
    output.write_text("")
    env.update(GITHUB_OUTPUT=str(output), GITHUB_EVENT_NAME=event)
    r = run_steps([step], env, cwd)
    written = dict(line.split("=", 1) for line in output.read_text().splitlines())
    return r, written


def checker_repo(tmp_path, checker):
    """A repository whose base holds `checker` as scripts/acceptance-check.sh (None: no checker),
    and a head that is one test commit declaring a Rust target and a Python test."""
    repo = Repo(tmp_path / "r")
    if checker is not None:
        repo.main_commit("scripts/acceptance-check.sh", checker)
    repo.write("tests/answer.rs", "#[test]\nfn answer() { assert_eq!(1, 2); }\n")
    repo.add_test("tests/test_answer.py")
    repo.g("add", "tests/answer.rs", "tests/test_answer.py")
    repo.test_commit("tests/answer.rs", "tests/test_answer.py")
    return repo


def test_scope_answers_true_with_the_targets_at_a_test_commit_head(tmp_path):
    repo = checker_repo(tmp_path, SCRIPT.read_text(encoding="utf-8"))
    r, out = run_scope(repo.path, "pull_request", tmp_path, repo.base, repo.rev("HEAD"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert out == {"test_commit": "true", "rs": "answer", "py": "tests/test_answer.py"}, r.stdout + r.stderr


def test_scope_answers_false_at_a_fix_head(tmp_path):
    repo = checker_repo(tmp_path, SCRIPT.read_text(encoding="utf-8"))
    repo.fix()
    r, out = run_scope(repo.path, "pull_request", tmp_path, repo.base, repo.rev("HEAD"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert out == {"test_commit": "false"}, r.stdout + r.stderr


@pytest.mark.parametrize("event", ["merge_group", "push"])
def test_scope_answers_false_and_reads_nothing_off_pull_request(tmp_path, event):
    empty = tmp_path / "empty"  # not a repository: nothing there can be read
    empty.mkdir()
    r, out = run_scope(empty, event, tmp_path)
    assert r.returncode == 0, r.stdout + r.stderr
    assert out == {"test_commit": "false"}, r.stdout + r.stderr
    assert list((tmp_path / "runner").iterdir()) == []
    assert list(empty.iterdir()) == []


NO_SCOPE = 'echo "usage: acceptance-check.sh BASE_SHA HEAD_SHA | --test-commit SHA" >&2\nexit 2\n'


@pytest.mark.parametrize(
    "checker",
    [
        pytest.param(None, id="no-checker-at-base"),
        pytest.param(NO_SCOPE, id="no-scope-mode-exit-2"),
        pytest.param('echo "full: two own commits"\n', id="full"),
        pytest.param('echo "test-commit 0000000000000000000000000000000000000000"\necho "rs answer"\n', id="other-sha"),
        pytest.param('echo "test-commit $3 "\necho "rs answer"\n', id="first-line-not-exact"),
        pytest.param('echo "rs answer"\necho "test-commit $3"\n', id="not-first"),
        pytest.param('echo "test-commit $3"\necho "rs answer"\nexit 1\n', id="exit-1"),
        pytest.param('echo "test-commit $3"\necho "rs answer"\necho "xx other"\n', id="unknown-line"),
        pytest.param('echo "test-commit $3"\necho "rs an\\$swer"\n', id="odd-name"),
        pytest.param('echo "test-commit $3"\n', id="no-target"),
        pytest.param("", id="nothing"),
    ],
)
def test_scope_answers_false_on_any_other_answer_of_the_base_checker(tmp_path, checker):
    repo = checker_repo(tmp_path, checker)
    r, out = run_scope(repo.path, "pull_request", tmp_path, repo.base, repo.rev("HEAD"))
    assert r.returncode == 0, r.stdout + r.stderr
    assert out == {"test_commit": "false"}, r.stdout + r.stderr


def test_scope_takes_the_checker_from_the_base_not_the_head(tmp_path):
    repo = checker_repo(tmp_path, NO_SCOPE)
    repo.write("scripts/acceptance-check.sh", SCRIPT.read_text(encoding="utf-8"))  # the head's own copy
    r, out = run_scope(repo.path, "pull_request", tmp_path, repo.base, repo.rev("HEAD"))
    assert out == {"test_commit": "false"}, r.stdout + r.stderr


# The shortened tests step, with stand-in cargo and python3.

STAND_IN = '#!/bin/sh\nprintf \'%s\\n\' "{name} $*" >> "$LOG"\nexit "${{{var}:-0}}"\n'


def run_shortened_tests(tmp, rs, py, cargo_status, pytest_status):
    step = [s for s in steps_for(jobs()["tests"], t_ctx()) if "if" in s]
    assert len(step) == 1, step
    step = step[0]
    bindir = tmp / "bin"
    bindir.mkdir()
    for name, var in (("cargo", "CARGO_STATUS"), ("python3", "PYTEST_STATUS")):
        f = bindir / name
        f.write_text(STAND_IN.format(name=name, var=var))
        f.chmod(0o755)
    outputs = {"${{ needs.scope.outputs.rs }}": rs, "${{ needs.scope.outputs.py }}": py}
    env = base_env(tmp / "runner")
    env["PATH"] = f"{bindir}:/usr/bin:/bin"
    for k, v in (step.get("env") or {}).items():
        assert v in outputs, (k, v)
        env[k] = outputs[v]
    log = tmp / "log"
    log.write_text("")
    env.update(LOG=str(log), CARGO_STATUS=str(cargo_status), PYTEST_STATUS=str(pytest_status))
    r = run_steps([step], env, tmp)
    return r, log.read_text().splitlines()


CARGO_TWO = "cargo test --locked --no-fail-fast --test alpha --test beta"
CARGO_ONE = "cargo test --locked --no-fail-fast --test alpha"
PYTEST = "python3 -m pytest -q tests/test_x.py tests/sub/test_y.py"


@pytest.mark.parametrize(
    "rs,py,cargo_status,pytest_status,passes,calls",
    [
        pytest.param("alpha beta", "", 101, 0, False, [CARGO_TWO], id="two-rust-both-failing"),
        pytest.param("alpha", "tests/test_x.py tests/sub/test_y.py", 101, 0, False, [CARGO_ONE, PYTEST], id="rust-fails-python-passes"),
        pytest.param("alpha", "tests/test_x.py tests/sub/test_y.py", 0, 1, False, [CARGO_ONE, PYTEST], id="rust-passes-python-fails"),
        pytest.param("alpha beta", "tests/test_x.py tests/sub/test_y.py", 0, 0, True, [CARGO_TWO, PYTEST], id="both-pass"),
        pytest.param("alpha beta", "", 0, 0, True, [CARGO_TWO], id="only-rust-passes"),
        pytest.param("", "tests/test_x.py tests/sub/test_y.py", 0, 0, True, [PYTEST], id="only-python-passes"),
        pytest.param("", "tests/test_x.py tests/sub/test_y.py", 0, 1, False, [PYTEST], id="only-python-fails"),
    ],
)
def test_the_shortened_tests_step_runs_every_declared_test(tmp_path, rs, py, cargo_status, pytest_status, passes, calls):
    r, log = run_shortened_tests(tmp_path, rs, py, cargo_status, pytest_status)
    assert log == calls, r.stdout + r.stderr
    assert (r.returncode == 0) == passes, (r.returncode, r.stdout + r.stderr)


# Mutated copies, each of which the check must flag.


def _replace_once(text, old, new):
    assert text.count(old) == 1, old
    return text.replace(old, new)


T_COND = "github.event_name == 'pull_request' && needs.scope.outputs.test_commit == 'true'"


def _no_cancelled_on_acceptance(text):
    return _replace_once(text, "${{ !cancelled() && (", "${{ (")


def _no_cancelled_on_tests(text):
    return _replace_once(
        text,
        "    needs: scope\n    if: ${{ !cancelled() }}\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      # The Rust",
        "    needs: scope\n    runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n      # The Rust",
    )


def _acceptance_skipped(text):
    text = _replace_once(
        text,
        "github.event_name == 'merge_group') }}\n",
        "github.event_name == 'merge_group') && needs.scope.outputs.test_commit != 'true' }}\n",
    )
    start = text.index("      - name: Test commit - fails at once")
    end = text.index("      - uses: actions/checkout@v4", start)
    return text[:start] + text[end:]


def _replace_first_t(text, new):
    """The first shortened condition (the tests job's step) replaced by new."""
    i = text.index(T_COND)
    return text[:i] + new + text[i + len(T_COND) :]


def _not_false(text):
    return _replace_first_t(text, "github.event_name == 'pull_request' && needs.scope.outputs.test_commit != 'false'")


def _merge_group_shortened(text):
    return _replace_first_t(
        text,
        "(github.event_name == 'pull_request' || github.event_name == 'merge_group') && needs.scope.outputs.test_commit == 'true'",
    )


@pytest.mark.parametrize(
    "mutate",
    [_no_cancelled_on_acceptance, _no_cancelled_on_tests, _acceptance_skipped, _not_false, _merge_group_shortened],
)
def test_the_check_catches_a_broken_workflow(mutate):
    assert problems(mutate(ci_text())) != []
