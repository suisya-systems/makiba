"""Issue #106: the Python tests do not depend on an exported CARGO_TARGET_DIR. A worker that
exports one for its own cargo runs (and then runs pytest in the same shell, maybe beside
cargo) must still see the Rust cases of test_acceptance_check.py pass: the scratch
repository's two checker worktrees must not share one target folder and reuse a stale build."""

import os
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CASE = "tests/test_acceptance_check.py::test_rust_test_failing_by_assertion_then_passing_is_pass"


def test_rust_case_passes_with_an_exported_cargo_target_dir(tmp_path):
    target = tmp_path / "target"
    target.mkdir()
    r = subprocess.run(
        [sys.executable, "-m", "pytest", "-q", "-p", "no:cacheprovider", CASE],
        cwd=ROOT,
        env={**os.environ, "CARGO_TARGET_DIR": str(target)},
        check=False,
        capture_output=True,
        text=True,
    )
    assert r.returncode == 0, r.stdout + r.stderr
    assert "1 passed" in r.stdout, r.stdout
