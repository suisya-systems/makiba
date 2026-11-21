#!/bin/sh
# Makes, and checks, the manifest of a release. sh, tr, sed, grep and sha256sum (or shasum) only.
#   sh scripts/release-manifest.sh make VERSION BASE_URL DIR
#   sh scripts/release-manifest.sh check VERSION DIR
# DIR holds makiba-VERSION-TARGET.tar.gz for each target. make writes DIR/manifest.json, whose urls are
# BASE_URL/<archive name>. check reads DIR/manifest.json back and fails unless it names VERSION and
# exactly the targets below, each with the url of its own archive, an archive in DIR, and the sha256
# of that archive. Exit 0 good, 1 a problem (named on stderr), 2 usage.
set -eu
TARGETS="linux-x86_64 linux-aarch64 macos-aarch64 macos-x86_64"
usage() { echo "usage: release-manifest.sh make VERSION BASE_URL DIR | check VERSION DIR" >&2; exit 2; }
fail() { echo "release-manifest: $*" >&2; exit 1; }
if command -v sha256sum >/dev/null 2>&1; then sha() { sha256sum "$1" | cut -d' ' -f1; }
else sha() { shasum -a 256 "$1" | cut -d' ' -f1; }; fi

[ $# -ge 1 ] || usage
mode=$1
case $mode in
  make) [ $# -eq 4 ] || usage; version=$2; base=$3; dir=$4 ;;
  check) [ $# -eq 3 ] || usage; version=$2; dir=$3 ;;
  *) usage ;;
esac
printf '%s\n' "$version" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' || fail "$version is not X.Y.Z"
manifest=$dir/manifest.json

if [ "$mode" = make ]; then
  for t in $TARGETS; do
    [ -f "$dir/makiba-$version-$t.tar.gz" ] || fail "no archive makiba-$version-$t.tar.gz in $dir"
  done
  {
    printf '{\n  "version": "%s",\n  "targets": {\n' "$version"
    sep=
    for t in $TARGETS; do
      f=makiba-$version-$t.tar.gz
      printf '%s    "%s": {\n      "url": "%s/%s",\n      "sha256": "%s"\n    }' "$sep" "$t" "$base" "$f" "$(sha "$dir/$f")"
      sep=',
'
    done
    printf '\n  }\n}\n'
  } > "$manifest"
  echo "release-manifest: wrote $manifest for $version"
  exit 0
fi

[ -f "$manifest" ] || fail "no $manifest"
flat=$(tr -d '\r\n' < "$manifest")
got=$(printf '%s' "$flat" | sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
[ "$got" = "$version" ] || fail "manifest version is '$got', expected $version"
# The target names the manifest holds: every key that opens an object, except "targets".
have=$(printf '%s' "$flat" | grep -o '"[^"]*"[[:space:]]*:[[:space:]]*{' | sed 's/^"\([^"]*\)".*/\1/' | grep -vx targets | sort | tr '\n' ' ')
want=$(printf '%s\n' $TARGETS | sort | tr '\n' ' ')
[ "$have" = "$want" ] || fail "manifest targets are '$have', expected '$want'"
for t in $TARGETS; do
  obj=$(printf '%s' "$flat" | sed -n 's/.*"'"$t"'"[[:space:]]*:[[:space:]]*{\([^}]*\)}.*/\1/p')
  url=$(printf '%s' "$obj" | sed -n 's/.*"url"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  want_sha=$(printf '%s' "$obj" | sed -n 's/.*"sha256"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  name=makiba-$version-$t.tar.gz
  [ -n "$url" ] || fail "$t has no url"
  [ "${url##*/}" = "$name" ] || fail "$t url is $url, expected a file named $name"
  [ -f "$dir/$name" ] || fail "$t: no $name in $dir"
  actual=$(sha "$dir/$name")
  [ "$actual" = "$want_sha" ] || fail "$t: sha256 of $name is $actual, the manifest says '$want_sha'"
done
echo "release-manifest: $manifest matches the archives in $dir"
