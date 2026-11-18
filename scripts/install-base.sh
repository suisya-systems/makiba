#!/usr/bin/env bash
# Installs the pinned herdr-core release (PINNED in src/base/version.rs) into PREFIX/bin/hc.
#   bash scripts/install-base.sh PREFIX
# Reads <HERDR_RELEASES_URL or https://releases.herdr.dev>/core/<version>/manifest.json, picks this
# machine's target, downloads its url with curl, and installs nothing unless the file's SHA-256
# equals the manifest's. Exit 0 installed, 1 refused or failed, 2 usage.
set -eu
[ $# -eq 1 ] || { echo "usage: install-base.sh PREFIX" >&2; exit 2; }
prefix=$1
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
version=$(sed -n 's/^pub const PINNED: &str = "\(.*\)";$/\1/p' "$here/../src/base/version.rs")
[ -n "$version" ] || { echo "install-base: no PINNED version in src/base/version.rs" >&2; exit 1; }

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=linux-x86_64 ;;
  Linux-aarch64 | Linux-arm64) target=linux-aarch64 ;;
  Darwin-arm64 | Darwin-aarch64) target=macos-aarch64 ;;
  Darwin-x86_64) target=macos-x86_64 ;;
  *) echo "install-base: no release target for $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }
else sha() { shasum -a 256 "$1" | cut -d' ' -f1; }; fi

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
manifest_url="${HERDR_RELEASES_URL:-https://releases.herdr.dev}/core/$version/manifest.json"
curl -fsSL "$manifest_url" -o "$work/manifest.json" || { echo "install-base: cannot fetch $manifest_url" >&2; exit 1; }
# The manifest's url and sha256 for this target, one per line.
fields=$(python3 -I -c '
import json, sys
t = json.load(open(sys.argv[1]))["targets"][sys.argv[2]]
print(t["url"]); print(t["sha256"])' "$work/manifest.json" "$target") \
  || { echo "install-base: manifest has no usable entry for $target" >&2; exit 1; }
url=$(printf '%s\n' "$fields" | sed -n 1p)
want=$(printf '%s\n' "$fields" | sed -n 2p | tr 'A-F' 'a-f')

curl -fsSL "$url" -o "$work/hc" || { echo "install-base: cannot fetch $url" >&2; exit 1; }
got=$(sha "$work/hc")
if [ "$got" != "$want" ]; then
  echo "install-base: SHA-256 of $url is $got, the manifest says $want; nothing installed" >&2
  exit 1
fi
mkdir -p "$prefix/bin"
chmod 755 "$work/hc"
mv -f "$work/hc" "$prefix/bin/hc"
echo "install-base: herdr-core $version ($target) installed at $prefix/bin/hc"
