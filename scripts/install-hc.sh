#!/bin/sh
# Installs hc, the Herdr app's core, pinned and checksummed, into DIR
# (/tmp/hc by default), for CI: the built-in herdr-app pane provider's live
# tests (tests/herdr_app.rs) need it, and panic without it under CI.
# VERSION is the one tests/fixtures/herdr_app/ was recorded on: move both
# together, with the checksums of the herdr-core binary each release
# tarball holds.
#
# It runs hc's own install script, which checks the tarball against the
# release manifest, then checks the herdr-core it installed against the
# checksum here. Outside $HOME that script copies a plain herdr-core,
# starting no core; on macOS, under $HOME, it would install the Herdr
# Core.app bundle instead, whose binary these checksums aren't.
#
#   scripts/install-hc.sh [DIR]
set -eu
VERSION=0.1.21
case "$(uname -s)-$(uname -m)" in
Darwin-arm64)
  SUM=ed3e400a36eb78d01e650e38b4591b032f189e5b382a47e353b3389b06510ede
  ;;
Darwin-x86_64)
  SUM=04817308cc884f1ee66a7cca182ff3ed4a399a4ad8cbed58bed740715b310142
  ;;
Linux-x86_64)
  SUM=373f1ac70bf78c14e898c3682caafd3fb8b07d39c7f21637a57922e064c7bcba
  ;;
Linux-aarch64)
  SUM=47ef4e308c9f9e879fae948701c3b0dc36927e9fef9a283c2c7b6b9450779a64
  ;;
*)
  echo "no pinned hc for $(uname -sm)" >&2
  exit 1
  ;;
esac
DIR=${1:-/tmp/hc}
mkdir -p "$DIR"
script=$(mktemp)
trap 'rm -f "$script"' EXIT
curl -fsSL --retry 3 -o "$script" https://releases.herdr.dev/core/install.sh
HC_VERSION=$VERSION HC_INSTALL_DIR=$DIR sh "$script"
if command -v sha256sum >/dev/null; then
  echo "$SUM  $DIR/herdr-core" | sha256sum -c -
else
  echo "$SUM  $DIR/herdr-core" | shasum -a 256 -c -
fi
# hc's script adds no hc link when an hc is already on PATH, as on a
# developer's machine; CI has none, but the link is made either way
[ -e "$DIR/hc" ] || ln -s herdr-core "$DIR/hc"
"$DIR/hc" --version
