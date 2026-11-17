#!/usr/bin/env bash
# Install the pinned herdr-core release into a prefix: scripts/install-base.sh <prefix>
# The binary lands at <prefix>/bin/hc; no other hc on the machine is touched.
# The release is found through the manifest at $HERDR_RELEASES_URL/core/<version>/manifest.json
# (the base's own default: https://releases.herdr.dev), and its sha256 is checked before unpacking.
set -euo pipefail

prefix=${1:?usage: install-base.sh <prefix>}
releases=${HERDR_RELEASES_URL:-https://releases.herdr.dev}
here=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
# The pin lives in one place: makiba/base/version.py.
version=$(python3 -c 'import sys; sys.path.insert(0, sys.argv[1]); from makiba.base import version as v; print(".".join(map(str, v.LOW)))' "$here")

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=linux-x86_64 ;;
  Linux-aarch64 | Linux-arm64) target=linux-aarch64 ;;
  Darwin-arm64) target=macos-aarch64 ;;
  Darwin-x86_64) target=macos-x86_64 ;;
  *) echo "no herdr-core build known for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
curl -fsSL "$releases/core/$version/manifest.json" -o "$work/manifest.json"
read -r url sha < <(python3 - "$work/manifest.json" "$version" "$target" <<'PY'
import json, sys
m = json.load(open(sys.argv[1], encoding="utf-8"))
if m["version"] != sys.argv[2]:
    sys.exit(f"manifest is for {m['version']}, wanted {sys.argv[2]}")
t = m["targets"][sys.argv[3]]
print(t["url"], t["sha256"])
PY
)
curl -fsSL "$url" -o "$work/pkg.tar.gz"
got=$(python3 -c 'import hashlib,sys; print(hashlib.sha256(open(sys.argv[1],"rb").read()).hexdigest())' "$work/pkg.tar.gz")
[ "$got" = "$sha" ] || { echo "sha256 mismatch: wanted $sha, got $got" >&2; exit 1; }

mkdir "$work/x"
tar -xzf "$work/pkg.tar.gz" -C "$work/x"
bin=$(find "$work/x" -type f \( -name herdr-core -o -name hc \) | head -n 1)
[ -n "$bin" ] || { echo "no herdr-core or hc executable in the archive" >&2; exit 1; }
mkdir -p "$prefix/bin"
install -m 755 "$bin" "$prefix/bin/hc"

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
