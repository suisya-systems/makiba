import importlib.util
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location("check_language", ROOT / "tools" / "check_language.py")
cl = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cl)

ALLOW = ROOT / "tools" / "language-allowlist.txt"
KANA = chr(0x3042)  # built from a code point so this file stays English only


def put(root, rel, text):
    p = root / rel
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(text, encoding="utf-8")


def test_kana_in_skills_fails(tmp_path):
    put(tmp_path, "skills/x.md", f"hello {KANA}\n")
    errors = cl.check(tmp_path, ALLOW)
    assert errors and "skills/x.md" in errors[0]


def test_same_character_in_i18n_fixtures_passes(tmp_path):
    put(tmp_path, "tests/fixtures/i18n/x.md", f"hello {KANA}\n")
    put(tmp_path, "roles/locale/x.md", f"hello {KANA}\n")
    assert cl.check(tmp_path, ALLOW) == []


def test_katakana_and_ideograph_fail(tmp_path):
    put(tmp_path, "docs/a.md", chr(0x30A2))
    put(tmp_path, "src/b.rs", "// " + chr(0x4E00))
    assert len(cl.check(tmp_path, ALLOW)) == 2


def test_repository_passes():
    assert cl.check(ROOT, ALLOW) == []
