#!/usr/bin/env bash
# Installs the pinned herdr-core release (PINNED in src/base/version.rs) into PREFIX/bin, in the
# base's own layout: the archive's herdr-core as PREFIX/bin/herdr-core, and PREFIX/bin/hc a link to it.
#   bash scripts/install-base.sh PREFIX
# MAKIBA_BASE_PINNED, when set, is the version instead, and src/base/version.rs is not read (an
# installed makiba has no checkout); a value that is not a plain major.minor.patch is refused
# before anything is fetched. To stage a new base beside the person's install, the upgrade verb
# runs this script with HC_BIN removed, PATH = <staging>/bin: followed by the person's PATH,
# MAKIBA_BASE_PINNED = its own PINNED, and HERDR_RELEASES_URL passed on when set; the check after
# installing then finds the staged files first and exits 0.
# Reads <HERDR_RELEASES_URL or https://releases.herdr.dev>/core/<version>/manifest.json, picks this
# machine's target, downloads its url (a tar.gz) with curl, and installs nothing unless the archive's
# SHA-256 equals the manifest's. A herdr-core or hc already in PREFIX/bin is replaced; one that is a
# directory is refused before anything is replaced. An older herdr-core left beside hc or ahead on
# PATH can be the server hc starts (#382), so after installing, the script checks what would run
# instead: HC_BIN when set, and the herdr-core and hc that PATH finds first. One that is not the
# installed file is named, and the script exits 3. It deletes nothing outside PREFIX, runs no
# program it finds, and starts or stops no core.
# Exit 0 installed; 1 refused or failed, nothing installed (except that a failure while making the
# hc link leaves the new herdr-core beside the old hc); 2 usage; 3 installed, but HC_BIN or PATH
# finds another herdr-core or hc first.
set -eu
[ $# -eq 1 ] || { echo "usage: install-base.sh PREFIX" >&2; exit 2; }
prefix=$1
here=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
if [ "${MAKIBA_BASE_PINNED+set}" = set ]; then
  version=$MAKIBA_BASE_PINNED
  [[ $version =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] \
    || { echo "install-base: MAKIBA_BASE_PINNED is $version, not a plain major.minor.patch version; nothing installed" >&2; exit 1; }
else
  version=$(sed -n 's/^pub const PINNED: &str = "\(.*\)";$/\1/p' "$here/../src/base/version.rs")
  [ -n "$version" ] || { echo "install-base: no PINNED version in src/base/version.rs" >&2; exit 1; }
fi

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

curl -fsSL "$url" -o "$work/archive" || { echo "install-base: cannot fetch $url" >&2; exit 1; }
got=$(sha "$work/archive")
if [ "$got" != "$want" ]; then
  echo "install-base: SHA-256 of $url is $got, the manifest says $want; nothing installed" >&2
  exit 1
fi
# The archive (tar.gz on Unix) holds herdr-core, LICENSE and THIRD-PARTY-NOTICES at its root.
mkdir "$work/x"
tar -xzf "$work/archive" -C "$work/x" herdr-core 2>/dev/null && [ -f "$work/x/herdr-core" ] && [ ! -L "$work/x/herdr-core" ] \
  || { echo "install-base: $url has no herdr-core executable; nothing installed" >&2; exit 1; }
bin=$prefix/bin
mkdir -p "$bin" || { echo "install-base: cannot create $bin; nothing installed" >&2; exit 1; }
for name in herdr-core hc; do
  [ ! -d "$bin/$name" ] || { echo "install-base: $bin/$name is a directory; nothing installed" >&2; exit 1; }
done
# The binary: a copy at a temporary name beside it, then moved over herdr-core in one rename.
tmp=$(mktemp "$bin/.herdr-core.XXXXXX") || { echo "install-base: cannot write in $bin; nothing installed" >&2; exit 1; }
{ cp "$work/x/herdr-core" "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$bin/herdr-core"; } \
  || { rm -f "$tmp"; echo "install-base: cannot place $bin/herdr-core; nothing installed" >&2; exit 1; }
# The link: made at a temporary name, then moved over hc, so a regular file or a link elsewhere is replaced.
link="$bin/.hc.$$"
rm -f "$link"
{ ln -s herdr-core "$link" && mv -f "$link" "$bin/hc"; } \
  || { rm -f "$link"; echo "install-base: cannot link $bin/hc to herdr-core; $bin/herdr-core is already the new one" >&2; exit 1; }
echo "install-base: herdr-core $version ($target) installed at $bin/herdr-core, with $bin/hc linking to it"

# What makiba and hc would run instead of the installed file. makiba runs $HC_BIN (a bare name is
# looked up on PATH), else hc on PATH; hc starts its server under the name herdr-core.
# `type -P` gives only files found on PATH (never a shell function), relative when the PATH entry
# is; `-ef` compares a relative path from the current folder, as the lookup does.
other=0
if [ "${HC_BIN+set}" = set ]; then
  case "$HC_BIN" in
    '') named= ;;
    */*) named=$HC_BIN ;;
    *) named=$(type -P "$HC_BIN") || named=NOTFOUND ;;
  esac
  if [ -z "$named" ]; then
    echo "install-base: installed at $bin/herdr-core, but HC_BIN is set and empty; makiba up cannot run it: point HC_BIN at $bin/hc or unset it" >&2
    other=1
  elif [ "$named" = NOTFOUND ]; then
    echo "install-base: installed at $bin/herdr-core, but HC_BIN names $HC_BIN, which PATH does not find; makiba up cannot run it: point HC_BIN at $bin/hc or unset it" >&2
    other=1
  elif ! [ "$named" -ef "$bin/herdr-core" ]; then
    echo "install-base: installed at $bin/herdr-core, but HC_BIN names $named; makiba up runs that one: point HC_BIN at $bin/hc or unset it" >&2
    other=1
  fi
fi
for name in herdr-core hc; do
  found=$(type -P "$name") || continue
  if ! [ "$found" -ef "$bin/herdr-core" ]; then
    echo "install-base: installed at $bin/herdr-core, but PATH finds $name at $found first; hc may start that herdr-core instead: remove it or put $bin before it on PATH" >&2
    other=1
  fi
done
[ "$other" = 0 ] || exit 3
