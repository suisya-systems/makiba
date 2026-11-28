"""CI's Rust build caches are keyed on everything that changes what lands in target/ (Issue 640).

Every `actions/cache` step of `.github/workflows/ci.yml` whose `path` lists `target` must have a
`key` whose `hashFiles(...)` names `Cargo.toml`, `Cargo.lock` and `rust-toolchain.toml` (and
`.cargo/config.toml` when the repository has one). Without Cargo.toml a change to a `[profile.*]`
section or to features keeps the key, so the job restores a target/ built under the old settings,
and actions/cache never saves over an existing key.
"""

import re
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent
WORKFLOW = ROOT / ".github" / "workflows" / "ci.yml"
HASH_FILES = re.compile(r"hashFiles\(([^)]*)\)")
QUOTED = re.compile(r"""'([^']*)'|"([^"]*)\"""")


def required():
    names = {"Cargo.toml", "Cargo.lock", "rust-toolchain.toml"}
    if (ROOT / ".cargo" / "config.toml").exists():
        names.add(".cargo/config.toml")
    return names


def target_cache_steps(doc):
    """(job name, step) for each actions/cache step whose path lists `target`."""
    found = []
    for job_name, job in (doc.get("jobs") or {}).items():
        for step in job.get("steps") or []:
            if not str(step.get("uses", "")).startswith("actions/cache@"):
                continue
            paths = str((step.get("with") or {}).get("path", "")).split("\n")
            if any(p.strip() == "target" for p in paths):
                found.append((job_name, step))
    return found


def hashed(key):
    names = set()
    for args in HASH_FILES.findall(str(key)):
        for single, double in QUOTED.findall(args):
            names.add(single or double)
    return names


def test_every_target_cache_key_hashes_the_build_inputs():
    doc = yaml.safe_load(WORKFLOW.read_text())
    steps = target_cache_steps(doc)
    assert steps, "no actions/cache step caches target"
    need = required()
    wrong = []
    for job_name, step in steps:
        key = (step.get("with") or {}).get("key", "")
        missing = sorted(need - hashed(key))
        if missing:
            wrong.append(f"{job_name}: key {key!r} does not hash {', '.join(missing)}")
    assert not wrong, "\n".join(wrong)
