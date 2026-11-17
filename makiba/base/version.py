"""The one version of the base (herdr-core) makiba runs against.

One patch release: the version the design was read against and the probes of
docs/design/probes.md ran on. It moves on purpose and rarely: docs/upgrading-the-base.md.
"""

import re

LOW = (0, 1, 21)  # inclusive
HIGH = (0, 1, 22)  # exclusive
SUPPORTED = ">=0.1.21,<0.1.22"


class UnsupportedVersion(Exception):
    pass


def check_version(found: str) -> None:
    m = re.fullmatch(r"v?(\d+)\.(\d+)\.(\d+)", found.strip())
    if not m or not LOW <= tuple(map(int, m.groups())) < HIGH:
        raise UnsupportedVersion(f"herdr-core {found!r} found, {SUPPORTED} supported")


def check_base(base):
    """Call hello() first and nothing else; raise UnsupportedVersion outside the range."""
    info = base.hello()
    check_version(info.version)
    return info
