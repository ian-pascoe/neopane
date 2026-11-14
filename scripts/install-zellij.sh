#!/bin/sh
# Installs zellij's release binary, pinned and checksummed, into DIR
# ($HOME/zellij by default), for CI: the built-in zellij pane provider's
# live tests (tests/zellij.rs) need 0.46.0, and panic without it under CI.
#
#   scripts/install-zellij.sh [DIR]
set -eu
VERSION=v0.46.0
case "$(uname -s)-$(uname -m)" in
Darwin-arm64)
  ASSET=zellij-aarch64-apple-darwin.tar.gz
  SUM=716a1c7887be83cc813900012a156a4d8e0ee1468ac56c58a299c9d8898cdc90
  ;;
Darwin-x86_64)
  ASSET=zellij-x86_64-apple-darwin.tar.gz
  SUM=e3b6197af04b286f74f7a9f5f519e5a8cf048be65f0371a32b0d60abac97384a
  ;;
Linux-x86_64)
  ASSET=zellij-x86_64-unknown-linux-musl.tar.gz
  SUM=a8ee599eb357b6142443c91506a4ece3aa8811cb75dcf2b28d194bd66853c79d
  ;;
Linux-aarch64)
  ASSET=zellij-aarch64-unknown-linux-musl.tar.gz
  SUM=c416b3d29ec9c71baf8754fe66663afe4f615395895e3f9b8b6e3bd0f7042427
  ;;
*)
  echo "no pinned zellij for $(uname -sm)" >&2
  exit 1
  ;;
esac
DIR=${1:-$HOME/zellij}
mkdir -p "$DIR"
curl -fsSL --retry 3 -o "$DIR/$ASSET" \
  "https://github.com/zellij-org/zellij/releases/download/$VERSION/$ASSET"
if command -v sha256sum >/dev/null; then
  echo "$SUM  $DIR/$ASSET" | sha256sum -c -
else
  echo "$SUM  $DIR/$ASSET" | shasum -a 256 -c -
fi
tar -xzf "$DIR/$ASSET" -C "$DIR" zellij
rm "$DIR/$ASSET"
"$DIR/zellij" --version
