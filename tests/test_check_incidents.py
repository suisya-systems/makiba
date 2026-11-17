import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("check_incidents", ROOT / "tools" / "check_incidents.py")
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)

HEAD = "| # | Incident | Cause then | Structure in makiba | Guarded by | Test |\n| --- | --- | --- | --- | --- | --- |\n"


def run(tmp_path, rows, pending="", exist=()):
    (tmp_path / "t.md").write_text("intro\n\n" + HEAD + rows, encoding="utf-8")
    (tmp_path / "pending.txt").write_text(pending, encoding="utf-8")
    for f in exist:
        (tmp_path / f).parent.mkdir(parents=True, exist_ok=True)
        (tmp_path / f).write_text("", encoding="utf-8")
    return ci.check(tmp_path / "t.md", tmp_path / "pending.txt", tmp_path)


def test_row_without_test_fails(tmp_path):
    assert run(tmp_path, "| I-1 | a | b | c | M1 |  |\n")


def test_missing_file_fails_unless_pending(tmp_path):
    row = "| I-1 | a | b | c | M1 | `tests/test_x.py`, `tests/test_y.py` |\n"
    assert run(tmp_path, row)
    assert run(tmp_path, row, pending="tests/test_x.py\n")
    assert not run(tmp_path, row, pending="# why\ntests/test_x.py\n\ntests/test_y.py\n")
    assert not run(tmp_path, row, exist=["tests/test_x.py", "tests/test_y.py"])


def test_delivered_table_passes():
    errors = ci.check(
        ROOT / "docs/design/incidents.md", ROOT / "docs/design/incidents-pending.txt", ROOT
    )
    assert errors == []
