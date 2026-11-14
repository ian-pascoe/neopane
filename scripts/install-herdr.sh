#!/bin/sh
# Installs herdr's release binary, pinned and checksummed, into DIR
# ($HOME/herdr by default), for CI: the built-in herdr pane provider's
# live tests (tests/herdr.rs) need 0.9.5, and panic without it under CI.
# The Linux binaries are static musl builds.
#
#   scripts/install-herdr.sh [DIR]
set -eu
VERSION=v0.9.5
case "$(uname -s)-$(uname -m)" in
Darwin-arm64)
  ASSET=herdr-macos-aarch64
  SUM=fb77c2232a05a8035421382c894392f4174e4e38a18e48af3dd0290157c3a7e2
  ;;
Darwin-x86_64)
  ASSET=herdr-macos-x86_64
  SUM=5fb0a908ec7f6745e3e245738b06bfad56c46391c391ab3c0955fbead01f1369
  ;;
Linux-x86_64)
  ASSET=herdr-linux-x86_64
  SUM=14d9e8bc8b4a9e0118d77c32266f76d461f09b40342eb23752960bd89f59929d
  ;;
Linux-aarch64)
  ASSET=herdr-linux-aarch64
  SUM=17d7536f2da50859523680ef279dffcac8e4a235f4e46fe8da6c2ec44141897c
  ;;
*)
  echo "no pinned herdr for $(uname -sm)" >&2
  exit 1
  ;;
esac
DIR=${1:-$HOME/herdr}
mkdir -p "$DIR"
curl -fsSL --retry 3 -o "$DIR/herdr" \
  "https://github.com/herdrdev/herdr/releases/download/$VERSION/$ASSET"
if command -v sha256sum >/dev/null; then
  echo "$SUM  $DIR/herdr" | sha256sum -c -
else
  echo "$SUM  $DIR/herdr" | shasum -a 256 -c -
fi
chmod +x "$DIR/herdr"
"$DIR/herdr" --version
