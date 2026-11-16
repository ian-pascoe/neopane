#!/bin/sh
# Installs hc, the Herdr app's core, pinned, into DIR (/tmp/hc by default),
# for CI: the built-in herdr-app pane provider's live tests
# (tests/herdr_app.rs) need it, and panic without it under CI. VERSION is
# the one tests/fixtures/herdr_app/ was recorded on: move both together.
# hc's own install script checks the download against its release
# manifest. Outside $HOME it copies a plain herdr-core, starting no core; on
# macOS, under $HOME, it would install the Herdr Core.app bundle instead.
#
#   scripts/install-hc.sh [DIR]
set -eu
VERSION=0.1.21
DIR=${1:-/tmp/hc}
mkdir -p "$DIR"
script=$(mktemp)
trap 'rm -f "$script"' EXIT
curl -fsSL --retry 3 -o "$script" https://releases.herdr.dev/core/install.sh
HC_VERSION=$VERSION HC_INSTALL_DIR=$DIR sh "$script"
# it adds no hc when one is on PATH already
[ -e "$DIR/hc" ] || ln -s herdr-core "$DIR/hc"
"$DIR/hc" --version
