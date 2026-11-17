"""The range of Claude Code makiba runs with.

From the lowest to the highest version a recorded run of the probes covers (the
`claude --version` column of docs/design/probes.md, Results), and no wider. Separate from the
base's pin: Claude Code is not behind the seam and updates itself. docs/upgrading-claude-code.md.
"""

import re

LOW = (2, 1, 326)  # inclusive
HIGH = (2, 1, 326)  # inclusive
SUPPORTED = ">=2.1.326,<=2.1.326"


class UnsupportedClaudeVersion(Exception):
    pass


def check_claude_version(text: str) -> tuple[int, int, int]:
    """`text` is what `claude --version` prints, e.g. '2.1.326 (Claude Code)'."""
    m = re.match(r"\s*(\d+)\.(\d+)\.(\d+)\b", text)
    found = tuple(map(int, m.groups())) if m else None
    if found is None or not LOW <= found <= HIGH:
        raise UnsupportedClaudeVersion(
            f"Claude Code version {text.strip()!r} found, {SUPPORTED} supported"
        )
    return found
