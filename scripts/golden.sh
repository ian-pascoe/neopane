#!/bin/sh
# Runs the golden tests: the renderer's output through Ghostty's emulator.
# Fetches Ghostty, pinned to one commit, into target/ghostty-<sha>/ once.
# Building it needs Zig 0.17, on PATH or in $ZIG. Extra arguments go to
# `cargo test`.
set -e
SHA=bf14ea1fbda2d069112003b80ec3b9186b7688f6
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DIR="$ROOT/target/ghostty-$(printf %s "$SHA" | cut -c1-8)"
if [ ! -f "$DIR/build.zig" ]; then
    mkdir -p "$DIR"
    curl -fsSL "https://github.com/ghostty-org/ghostty/archive/$SHA.tar.gz" |
        tar xz -C "$DIR" --strip-components=1 || {
        rm -rf "$DIR"
        exit 1
    }
fi
cd "$ROOT"
# Zig gets 400s through the proxy when its package cache is cold
unset http_proxy https_proxy HTTP_PROXY HTTPS_PROXY ALL_PROXY all_proxy
GHOSTTY_SOURCE_DIR="$DIR" exec cargo test --features golden "$@"
