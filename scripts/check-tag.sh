#!/bin/sh
# Fails unless a release tag and the version in Cargo.toml agree.
#   sh scripts/check-tag.sh v0.1.0
# The release workflow runs this first; nothing is built or published when it fails.
# Exit 0 they agree, 1 they differ or the tag is not vX.Y.Z, 2 usage.
set -eu
[ $# -eq 1 ] || { echo "usage: check-tag.sh TAG" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
have=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$here/../Cargo.toml" | head -1)
case $1 in
  v*) want=${1#v} ;;
  *) echo "check-tag: $1 is not a version tag (vX.Y.Z)" >&2; exit 1 ;;
esac
printf '%s\n' "$want" | grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+$' \
  || { echo "check-tag: $1 is not a version tag (vX.Y.Z)" >&2; exit 1; }
if [ "$want" != "$have" ]; then
  echo "check-tag: tag $1 says $want, Cargo.toml says ${have:-nothing}; nothing is released" >&2
  exit 1
fi
echo "check-tag: $1 and Cargo.toml agree on $have"
