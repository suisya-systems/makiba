#!/bin/sh
# Installs makiba: one binary, from the manifest of a release.
#   sh install.sh [--version X.Y.Z] [--prefix DIR]
# Default: the latest release, into $HOME/.local/bin/makiba. Needs sh, curl or wget, tar, uname, mktemp
# and sha256sum or shasum; no Rust, Python or jq. MAKIBA_RELEASES_URL replaces the releases URL
# (https://github.com/suisya-systems/makiba/releases); under it the manifest is latest/download/manifest.json
# or download/vX.Y.Z/manifest.json.
# Nothing is placed unless the archive's sha256 equals the manifest's and the binary in it prints the
# manifest's version. Exit codes:
#   0 installed          2 usage                   3 this host (OS, CPU or a missing tool)
#   4 a fetch failed     5 the manifest is unusable  6 the archive's sha256 differs from the manifest's
#   7 the archive is unusable (no makiba, it does not run, or it prints another version)
#   8 DIR/bin cannot be written
set -eu
RELEASES=${MAKIBA_RELEASES_URL:-https://github.com/suisya-systems/makiba/releases}
work=
tmp=
cleanup() { [ -z "$work" ] || rm -rf "$work"; [ -z "$tmp" ] || rm -f "$tmp"; }
trap cleanup EXIT
trap 'exit 130' INT TERM
die() { code=$1; shift; echo "install.sh: $*; nothing installed" >&2; exit "$code"; }

want=
prefix=
while [ $# -gt 0 ]; do
  case $1 in
    --version) [ $# -ge 2 ] || die 2 "--version needs a value"; want=$2; shift 2 ;;
    --prefix) [ $# -ge 2 ] || die 2 "--prefix needs a value"; prefix=$2; shift 2 ;;
    *) die 2 "unknown argument $1 (usage: install.sh [--version X.Y.Z] [--prefix DIR])" ;;
  esac
done
if [ -n "$want" ]; then
  printf '%s\n' "$want" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die 2 "$want is not a version (X.Y.Z)"
fi
if [ -z "$prefix" ]; then
  [ -n "${HOME:-}" ] || die 2 "no --prefix and no HOME"
  prefix=$HOME/.local
fi

if command -v curl >/dev/null 2>&1; then dl=curl
elif command -v wget >/dev/null 2>&1; then dl=wget
else die 3 "neither curl nor wget is installed"; fi
if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }
elif command -v shasum >/dev/null 2>&1; then sha() { shasum -a 256 "$1" | cut -d' ' -f1; }
else die 3 "neither sha256sum nor shasum is installed"; fi
command -v tar >/dev/null 2>&1 || die 3 "tar is not installed"
fetch() { # url dest
  if [ "$dl" = curl ]; then curl -fsSL -o "$2" "$1"; else wget -q -O "$2" "$1"; fi
}

case "$(uname -s)-$(uname -m)" in
  Linux-x86_64) target=linux-x86_64 ;;
  Linux-aarch64 | Linux-arm64) target=linux-aarch64 ;;
  Darwin-arm64 | Darwin-aarch64) target=macos-aarch64 ;;
  Darwin-x86_64) target=macos-x86_64 ;;
  *) die 3 "no release for $(uname -s) $(uname -m)" ;;
esac

work=$(mktemp -d "${TMPDIR:-/tmp}/makiba-install.XXXXXX") || die 3 "cannot make a temporary folder"
if [ -n "$want" ]; then murl=$RELEASES/download/v$want/manifest.json; else murl=$RELEASES/latest/download/manifest.json; fi
fetch "$murl" "$work/manifest.json" || die 4 "cannot fetch $murl"

flat=$(tr -d '\r\n' < "$work/manifest.json")
mver=$(printf '%s' "$flat" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
printf '%s\n' "$mver" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || die 5 "$murl has no usable version"
[ -z "$want" ] || [ "$mver" = "$want" ] || die 5 "$murl is for $mver, not $want"
obj=$(printf '%s' "$flat" | sed -n 's/.*"'"$target"'"[[:space:]]*:[[:space:]]*{\([^}]*\)}.*/\1/p')
[ -n "$obj" ] || die 5 "the manifest of $mver has no entry for $target"
url=$(printf '%s' "$obj" | sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
exp=$(printf '%s' "$obj" | sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' | tr 'A-F' 'a-f')
[ -n "$url" ] || die 5 "the manifest entry for $target has no url"
[ "${url##*/}" = "makiba-$mver-$target.tar.gz" ] \
  || die 5 "the manifest entry for $target names $url, not makiba-$mver-$target.tar.gz"
printf '%s\n' "$exp" | grep -Eq '^[0-9a-f]{64}$' || die 5 "the manifest entry for $target has no sha256 of 64 hex digits"

fetch "$url" "$work/archive" || die 4 "cannot fetch $url"
got=$(sha "$work/archive")
[ "$got" = "$exp" ] || die 6 "sha256 of $url is $got, the manifest says $exp"

mkdir "$work/x"
tar -xzf "$work/archive" -C "$work/x" makiba 2>/dev/null || die 7 "$url holds no makiba"
[ -f "$work/x/makiba" ] && [ ! -L "$work/x/makiba" ] || die 7 "$url holds no regular file makiba"
chmod 755 "$work/x/makiba"
out=$("$work/x/makiba" --version 2>/dev/null) || die 7 "the makiba in $url does not run on this host"
[ "$out" = "makiba $mver" ] || die 7 "the makiba in $url prints '$out', the manifest says $mver"

bindir=$prefix/bin
mkdir -p "$bindir" 2>/dev/null || die 8 "cannot create $bindir"
[ ! -d "$bindir/makiba" ] || die 8 "$bindir/makiba is a directory"
replaced=no
[ ! -e "$bindir/makiba" ] || replaced=yes
tmp=$(mktemp "$bindir/.makiba.XXXXXX" 2>/dev/null) || die 8 "cannot write in $bindir"
cp "$work/x/makiba" "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$bindir/makiba" || die 8 "cannot place $bindir/makiba"
tmp=

echo "$out"
echo "install.sh: makiba $mver ($target) installed at $bindir/makiba"
[ "$replaced" = no ] || echo "install.sh: an installed makiba was replaced; a running one keeps the old file until 'makiba down' and 'makiba up'"
case ":$PATH:" in *":$bindir:"*) ;; *) echo "install.sh: $bindir is not in PATH" ;; esac
