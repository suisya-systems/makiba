#!/usr/bin/env bash
# Installs one pinned cargo-nextest release into PREFIX/bin (the real-base job: ~/.cargo/bin, #485).
#   bash scripts/install-nextest.sh PREFIX
# VERSION and SHA256 below are the pin: the release archive for x86_64-unknown-linux-gnu is fetched
# from the project's GitHub release, and nothing is installed unless its SHA-256 equals SHA256.
# To move the pin, change both lines together, from the release's own .sha256 file.
# Exit 0 installed; 1 refused or failed, nothing installed; 2 usage.
set -eu
VERSION=0.9.152
SHA256=d039b43c3e0958441b6ad0a39c8884d018b3d2e8fb9928f209074b1100b5e17a
TARGET=x86_64-unknown-linux-gnu

[ $# -eq 1 ] || { echo "usage: install-nextest.sh PREFIX" >&2; exit 2; }
bin=$1/bin
[ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] \
  || { echo "install-nextest: the pin is for $TARGET, not $(uname -s)-$(uname -m); nothing installed" >&2; exit 1; }

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
url="https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$VERSION/cargo-nextest-$VERSION-$TARGET.tar.gz"
curl -fsSL --retry 3 "$url" -o "$work/archive" || { echo "install-nextest: cannot fetch $url" >&2; exit 1; }
got=$(sha256sum "$work/archive" | cut -d' ' -f1)
if [ "$got" != "$SHA256" ]; then
  echo "install-nextest: SHA-256 of $url is $got, the pin says $SHA256; nothing installed" >&2
  exit 1
fi
# The archive holds cargo-nextest at its root.
mkdir "$work/x"
tar -xzf "$work/archive" -C "$work/x" cargo-nextest 2>/dev/null && [ -f "$work/x/cargo-nextest" ] && [ ! -L "$work/x/cargo-nextest" ] \
  || { echo "install-nextest: $url has no cargo-nextest executable; nothing installed" >&2; exit 1; }
mkdir -p "$bin" || { echo "install-nextest: cannot create $bin; nothing installed" >&2; exit 1; }
tmp=$(mktemp "$bin/.cargo-nextest.XXXXXX") || { echo "install-nextest: cannot write in $bin; nothing installed" >&2; exit 1; }
{ cp "$work/x/cargo-nextest" "$tmp" && chmod 755 "$tmp" && mv -f "$tmp" "$bin/cargo-nextest"; } \
  || { rm -f "$tmp"; echo "install-nextest: cannot place $bin/cargo-nextest; nothing installed" >&2; exit 1; }
echo "install-nextest: cargo-nextest $VERSION ($TARGET) installed at $bin/cargo-nextest"
"$bin/cargo-nextest" nextest --version
