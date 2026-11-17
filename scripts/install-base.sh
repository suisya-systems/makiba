#!/usr/bin/env bash
# Install the pinned herdr-core release into a prefix: scripts/install-base.sh <prefix>
# The binary lands at <prefix>/bin/hc; no other hc on the machine is touched.
# HC_RELEASE_URL is the download URL of the release binary with @VERSION@ where the version
# goes. It is not recorded anywhere in this repository yet, so it is an input, not a guess.
set -euo pipefail

prefix=${1:?usage: install-base.sh <prefix>}
url_template=${HC_RELEASE_URL:?set HC_RELEASE_URL to the release binary URL, with @VERSION@ in it}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# The pin lives in one place: makiba/base/version.py.
version=$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); from makiba.base import version as v; print(".".join(map(str, v.LOW)))' "$here")

mkdir -p "$prefix/bin"
curl -fsSL "${url_template//@VERSION@/$version}" -o "$prefix/bin/hc"
chmod +x "$prefix/bin/hc"

got=$("$prefix/bin/hc" --version)
# Same exact-version rule as the check at runtime (0.1.210 or 0.1.21-rc1 do not pass).
python3 - "$here" "$got" <<'PY'
import re, sys
sys.path.insert(0, sys.argv[1])
from makiba.base import version as v
m = re.search(r"\d+\.\d+\.\d+\S*", sys.argv[2])
v.check_version(m.group(0) if m else sys.argv[2])
PY
echo "installed hc $version in $prefix/bin"
