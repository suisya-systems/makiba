from pathlib import Path
from types import SimpleNamespace

import pytest

from makiba.agents import version as agents
from makiba.base import version as base

ROOT = Path(__file__).resolve().parents[1]


def doc(name):
    return (ROOT / "docs" / name).read_text(encoding="utf-8")


class FakeBase:
    """Records every call; hello() answers with the version it was given."""

    def __init__(self, version):
        self.version = version
        self.calls = []

    def hello(self):
        self.calls.append("hello")
        return SimpleNamespace(name="herdr-core", version=self.version)

    def launch(self, spec):
        self.calls.append("launch")


def test_pinned_version_is_accepted():
    assert base.check_base(FakeBase("0.1.21")).version == "0.1.21"


@pytest.mark.parametrize("found", ["0.1.20", "0.1.22", "0.2.0", "1.1.21"])
def test_other_version_raises_before_any_other_call(found):
    fake = FakeBase(found)
    with pytest.raises(base.UnsupportedVersion) as e:
        base.check_base(fake)
    assert found in str(e.value) and base.SUPPORTED in str(e.value)
    assert fake.calls == ["hello"]


def test_base_range_is_one_patch_release():
    assert base.SUPPORTED == ">=0.1.21,<0.1.22"


def test_upgrading_the_base_names_what_a_move_reruns():
    text = doc("upgrading-the-base.md")
    for name in ["tests/test_base_compat.py", "P1", "P3", "P4", "P5", "P8"]:
        assert name in text, name


@pytest.mark.parametrize("text", ["2.1.326 (Claude Code)", "2.1.326\n"])
def test_claude_version_inside_range_is_accepted(text):
    assert agents.check_claude_version(text) == (2, 1, 326)


@pytest.mark.parametrize("text", ["2.1.318 (Claude Code)", "2.2.0 (Claude Code)", "garbage", ""])
def test_claude_version_outside_range_or_unparsable_raises(text):
    with pytest.raises(agents.UnsupportedClaudeVersion) as e:
        agents.check_claude_version(text)
    assert agents.SUPPORTED in str(e.value)
    assert (text.split()[0] if text.split() else repr(text)) in str(e.value)


def test_upgrading_claude_code_names_each_probe_and_the_runbook():
    text = doc("upgrading-claude-code.md")
    for name in ["P1", "P3", "P4", "P5", "P6", "P7", "P8", "P9", "P10", "P11",
                 "docs/runbooks/first-worker.md", "secretary.remote_control"]:
        assert name in text, name
