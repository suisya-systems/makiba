"""The acceptance job hands the owner's signed approvals to the base's checker (Issue 593).

docs/design/acceptance-approval.md 4.3 and section 8, step 5: `.github/workflows/ci.yml`'s job
`acceptance` has the permissions `contents: read` and `pull-requests: read` and no other; on a
pull request (not at a test-commit head) one step, before the head is checked out, reads the pull
request's comments with the job's token and writes each comment whose first line is exactly
`makiba-acceptance-approval v1` to `$RUNNER_TEMP/approvals/<id>`; a read that fails leaves the folder
empty and the step succeeds. The last step takes the checker from the base and passes it
`--approvals "$RUNNER_TEMP/approvals"`, only when that checker knows the flag.
`.github/acceptance-approvers` holds the owner's one allowed-signers line.

`problems(text)` lists what is wrong with a workflow text; the real file has none, and each mutated
copy below has at least one. The read step and the checker step are also run with bash, the read
step with a stand-in `gh` first on PATH, the checker step in a temporary repository.
"""

import os
import re
import subprocess
from pathlib import Path

import pytest
import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
APPROVERS = ROOT / ".github" / "acceptance-approvers"
HEADER = "makiba-acceptance-approval v1"
TOKEN_EXPR = "${{ github.token }}"
PR_EXPR = "${{ github.event.pull_request.number }}"
BASE_EXPR = "${{ github.event.pull_request.base.sha }}"
HEAD_EXPR = "${{ github.event.pull_request.head.sha }}"
APPROVALS_ARG = '--approvals "$RUNNER_TEMP/approvals"'
COND = re.compile(
    r"""\s*(?:(?P<paren>[()])|(?P<op>&&|\|\|)|(?P<status>!cancelled\(\))"""
    r"""|(?P<name>github\.event_name|needs\.scope\.outputs\.test_commit)\s*(?P<cmp>==|!=)\s*'(?P<value>[^']*)')"""
)


def holds(cond, event, scope):
    """Whether an `if:` holds for event with the scope job's answer scope ('true' or 'false').
    Understood: the event, the scope output, `!cancelled()` (true), `&&`, `||` and parentheses;
    anything else raises, so a condition this test cannot read is a problem."""
    text = "" if cond is None else str(cond).strip()
    if text.startswith("${{") and text.endswith("}}"):
        text = text[3:-2].strip()
    parts, pos = [], 0
    while pos < len(text):
        m = COND.match(text, pos)
        if not m or m.end() == pos:
            raise ValueError(f"condition not understood: {cond!r}")
        pos = m.end()
        if m["paren"]:
            parts.append(m["paren"])
        elif m["op"]:
            parts.append(" and " if m["op"] == "&&" else " or ")
        elif m["status"]:
            parts.append("True")
        else:
            actual = event if m["name"] == "github.event_name" else scope
            equal = actual == m["value"]
            parts.append(str(equal if m["cmp"] == "==" else not equal))
    return bool(eval("".join(parts) or "True", {"__builtins__": {}}))  # only True/False/and/or/()


def steps_for(job, event, scope):
    return [s for s in job.get("steps", []) if holds(s.get("if"), event, scope)]


def is_checkout(step):
    return str(step.get("uses", "")).startswith("actions/checkout@")


def acceptance(text):
    return (yaml.safe_load(text).get("jobs") or {}).get("acceptance") or {}


def read_steps(job):
    """The steps of the job that hold on a pull request and carry GH_TOKEN in their env."""
    return [s for s in steps_for(job, "pull_request", "false") if "GH_TOKEN" in (s.get("env") or {})]


def problems(text):
    out = []
    job = acceptance(text)
    if not job:
        return ["job acceptance is missing"]
    if job.get("permissions") != {"contents": "read", "pull-requests": "read"}:
        out.append(f"acceptance's permissions are {job.get('permissions')!r}, not contents and pull-requests read")
    try:
        on_pr = steps_for(job, "pull_request", "false")
        elsewhere = steps_for(job, "merge_group", "false") + steps_for(job, "pull_request", "true")
    except ValueError as e:
        return out + [str(e)]

    reads = read_steps(job)
    if len(reads) != 1:
        out.append(f"{len(reads)} steps on pull_request carry GH_TOKEN, not one")
    else:
        read = reads[0]
        env = read.get("env") or {}
        if env.get("GH_TOKEN") != TOKEN_EXPR:
            out.append(f"the read step's GH_TOKEN is {env.get('GH_TOKEN')!r}, not {TOKEN_EXPR}")
        if env.get("PR") != PR_EXPR:
            out.append(f"the read step's PR is {env.get('PR')!r}, not {PR_EXPR}")
        if "${{" in str(read.get("run", "")):
            out.append("the read step's run holds an expression")
        checkouts = [i for i, s in enumerate(on_pr) if is_checkout(s)]
        if not checkouts or on_pr.index(read) > checkouts[0]:
            out.append("the read step does not come before the checkout of the head")
        if any(s is read for s in elsewhere):
            out.append("the read step holds on merge_group or at a test-commit head")
    for s in job.get("steps", []):
        if reads and s is reads[0]:
            continue
        dump = yaml.safe_dump(s)
        if "GH_TOKEN" in dump or "github.token" in dump or "secrets." in dump:
            out.append(f"step {s.get('name') or s.get('uses')!r} reads a token")

    last = str(on_pr[-1].get("run", "")) if on_pr else ""
    if 'git show "$base:scripts/acceptance-check.sh"' not in last:
        out.append("the checker is not taken from the base")
    if 'bash "$RUNNER_TEMP/acceptance-check.sh"' not in last:
        out.append("the last step does not run the base's checker")
    if APPROVALS_ARG not in last:
        out.append("the checker is not passed --approvals \"$RUNNER_TEMP/approvals\"")
    return out


def ci_text():
    return WORKFLOW.read_text(encoding="utf-8")


def test_the_workflow_hands_the_approvals_to_the_base_checker():
    assert problems(ci_text()) == []


def _replace_once(text, old, new):
    assert text.count(old) == 1, old
    return text.replace(old, new)


PERMS = "    permissions:\n      contents: read\n      pull-requests: read\n"
READ_NAME = "      - name: Read the owner's approval comments\n"


def _read_after_checkout(text):
    doc = yaml.safe_load(text)
    steps = doc["jobs"]["acceptance"]["steps"]
    i = next(i for i, s in enumerate(steps) if "GH_TOKEN" in (s.get("env") or {}))
    j = next(j for j, s in enumerate(steps) if is_checkout(s))
    steps.insert(j, steps.pop(i))
    return yaml.safe_dump(doc, sort_keys=False)


def _token_on_checker(text):
    doc = yaml.safe_load(text)
    steps = doc["jobs"]["acceptance"]["steps"]
    steps[-1]["env"] = {"GH_TOKEN": TOKEN_EXPR}
    return yaml.safe_dump(doc, sort_keys=False)


MUTATIONS = {
    "no permissions": lambda t: _replace_once(t, PERMS, ""),
    "pull-requests write": lambda t: _replace_once(t, "      pull-requests: read\n", "      pull-requests: write\n"),
    "contents dropped": lambda t: _replace_once(t, "      contents: read\n      pull-requests: read\n", "      pull-requests: read\n"),
    "no --approvals": lambda t: _replace_once(t, f"approvals=({APPROVALS_ARG})", "approvals=()"),
    "checker from the head": lambda t: _replace_once(
        t, 'git show "$base:scripts/acceptance-check.sh" > "$RUNNER_TEMP/acceptance-check.sh" 2>/dev/null \\\n'
        '            || cp', "cp"),
    "token on the checker step": _token_on_checker,
    "read step after the checkout": _read_after_checkout,
    "read step without the token": lambda t: _replace_once(t, f"          GH_TOKEN: {TOKEN_EXPR}\n", ""),
}


@pytest.mark.parametrize("name", sorted(MUTATIONS))
def test_each_mutation_is_a_problem(name):
    assert problems(MUTATIONS[name](ci_text())), name


# The read step, run with a stand-in gh.


def read_step():
    reads = read_steps(acceptance(ci_text()))
    assert len(reads) == 1, reads
    return reads[0]


def run_read(tmp, gh_body):
    """Run the read step under bash -e with RUNNER_TEMP tmp/runner, GITHUB_REPOSITORY o/r, PR 7 and
    a stand-in gh whose body is gh_body; it records its arguments and GH_TOKEN in tmp/gh.args."""
    bindir, runner = tmp / "bin", tmp / "runner"
    bindir.mkdir()
    runner.mkdir()
    gh = bindir / "gh"
    gh.write_text(
        "#!/usr/bin/env bash\n"
        f'printf "%s\\n" "$@" > "{tmp}/gh.args"\n'
        f'printf "%s\\n" "$GH_TOKEN" > "{tmp}/gh.token"\n' + gh_body,
        encoding="utf-8",
    )
    gh.chmod(0o755)
    env = {
        "PATH": f"{bindir}{os.pathsep}{os.environ['PATH']}",
        "HOME": str(tmp),
        "RUNNER_TEMP": str(runner),
        "GITHUB_REPOSITORY": "o/r",
        "PR": "7",
        "GH_TOKEN": "the-token",
    }
    r = subprocess.run(
        ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", read_step()["run"]],
        env=env, cwd=tmp, capture_output=True, text=True, timeout=60,
    )
    return r, runner / "approvals"


def fixture_gh(tmp, text):
    page = tmp / "pages.json"
    page.write_bytes(text.encode("utf-8"))
    return f'cat "{page}"\n'


APPROVAL_A = HEADER + "\r\nrepo o/r\r\npr 7\r\n---\r\n-----BEGIN SSH SIGNATURE-----\r\nAAAA\r\n-----END SSH SIGNATURE-----\r\n"
APPROVAL_B = HEADER + "\nrepo o/r\npr 7\n---\n-----BEGIN SSH SIGNATURE-----\nBBBB\n-----END SSH SIGNATURE-----"


def test_the_read_step_writes_each_approval_comment_by_its_id(tmp_path):
    import json

    page1 = [
        {"id": 11, "body": APPROVAL_A},
        {"id": 12, "body": "looks good to me"},
        {"id": 13, "body": "quoted:\n" + HEADER + "\nrepo o/r\n"},
    ]
    page2 = [
        {"id": 14, "body": HEADER + "0\nrepo o/r\n"},
        {"id": 15, "body": APPROVAL_B},
        {"id": "../x", "body": APPROVAL_B},
        {"id": 16, "body": None},
    ]
    text = json.dumps(page1) + json.dumps(page2) + "\n"
    r, out = run_read(tmp_path, fixture_gh(tmp_path, text))
    assert r.returncode == 0, r.stdout + r.stderr
    assert sorted(p.name for p in out.iterdir()) == ["11", "15"], r.stdout + r.stderr
    assert (out / "11").read_bytes() == APPROVAL_A.replace("\r\n", "\n").encode()
    assert (out / "15").read_bytes() == (APPROVAL_B + "\n").encode()
    assert not (tmp_path / "x").exists() and not (tmp_path / "runner" / "x").exists()
    args = (tmp_path / "gh.args").read_text().splitlines()
    assert args == ["api", "--paginate", "repos/o/r/issues/7/comments?per_page=100"]
    assert (tmp_path / "gh.token").read_text() == "the-token\n"
    assert "approval comments: 2" in r.stdout


@pytest.mark.parametrize(
    "case",
    ["gh fails", "malformed json", "not a list"],
)
def test_a_read_that_fails_leaves_the_folder_empty_and_succeeds(tmp_path, case):
    import json

    good = json.dumps([{"id": 21, "body": APPROVAL_B}])
    body = {
        "gh fails": f'printf "%s" {json.dumps(good)!r}\nexit 1\n',
        "malformed json": fixture_gh(tmp_path, good + "[{"),
        "not a list": fixture_gh(tmp_path, good + json.dumps({"id": 22, "body": APPROVAL_B})),
    }[case]
    r, out = run_read(tmp_path, body)
    assert r.returncode == 0, r.stdout + r.stderr
    assert out.is_dir()
    assert list(out.iterdir()) == []
    assert "approval comments not read" in r.stdout


# The checker step, run in a temporary repository.


def git(cwd, *args):
    return subprocess.run(
        ["git", *args], cwd=cwd, check=True, capture_output=True, text=True,
        env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.com",
             "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.com",
             "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"},
    ).stdout.strip()


def run_checker_step(tmp, base_checker):
    """A repository whose base holds base_checker as scripts/acceptance-check.sh and whose head holds
    another checker; the last pull_request step runs at the head with both shas in place."""
    repo, runner = tmp / "repo", tmp / "runner"
    repo.mkdir()
    (runner / "approvals").mkdir(parents=True)
    git(repo, "init", "-q", "-b", "main")
    (repo / "scripts").mkdir()
    checker = repo / "scripts" / "acceptance-check.sh"
    checker.write_text(base_checker, encoding="utf-8")
    git(repo, "add", ".")
    git(repo, "commit", "-q", "-m", "base")
    base = git(repo, "rev-parse", "HEAD")
    checker.write_text('echo "head checker: $*"\n', encoding="utf-8")
    git(repo, "commit", "-q", "-am", "head")
    head = git(repo, "rev-parse", "HEAD")
    on_pr = steps_for(acceptance(ci_text()), "pull_request", "false")
    script = on_pr[-1]["run"].replace(BASE_EXPR, base).replace(HEAD_EXPR, head)
    assert "${{" not in script, script
    r = subprocess.run(
        ["bash", "--noprofile", "--norc", "-eo", "pipefail", "-c", script],
        env={"PATH": os.environ["PATH"], "HOME": str(tmp), "RUNNER_TEMP": str(runner)},
        cwd=repo, capture_output=True, text=True, timeout=60,
    )
    return r, base, head, runner


def test_a_base_checker_that_knows_the_flag_is_passed_the_approvals(tmp_path):
    knows = '# usage: acceptance-check.sh [--approvals DIR] BASE HEAD\necho "base checker: $*"\n'
    r, base, head, runner = run_checker_step(tmp_path, knows)
    assert r.returncode == 0, r.stdout + r.stderr
    assert r.stdout.strip().splitlines() == [f"base checker: --approvals {runner}/approvals {base} {head}"]


def test_a_base_checker_without_the_flag_is_called_as_before(tmp_path):
    r, base, head, _ = run_checker_step(tmp_path, 'echo "base checker: $*"\n')
    assert r.returncode == 0, r.stdout + r.stderr
    assert r.stdout.strip().splitlines() == [f"base checker: {base} {head}"]


# The signers file.


def test_the_approvers_file_holds_the_owners_one_signer_line(tmp_path):
    lines = APPROVERS.read_bytes().decode("ascii").split("\n")
    assert lines[-1] == "" and len(lines) == 2, lines
    m = re.fullmatch(
        r'makiba-owner namespaces="makiba-acceptance-approval" (ssh-ed25519 [A-Za-z0-9+/]+=*)', lines[0]
    )
    assert m, lines[0]
    key = tmp_path / "key.pub"
    key.write_text(m[1] + "\n", encoding="ascii")
    r = subprocess.run(["ssh-keygen", "-l", "-f", str(key)], capture_output=True, text=True, timeout=30)
    assert r.returncode == 0 and "ED25519" in r.stdout, r.stdout + r.stderr
