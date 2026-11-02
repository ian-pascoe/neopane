# Where `just install` puts neorex, and how it builds it. Override either
# with an environment variable or on the command line:
#   just prefix=/usr/local install
prefix := env("PREFIX", home_directory() / ".local")
optimize := env("OPTIMIZE", "ReleaseSafe")

# List the recipes
default:
    @just --list

# Build into zig-out
build *args:
    zig build {{ args }}

# Run neorex from zig-out, with any Neovim arguments
run *args:
    zig build run -- {{ args }}

# Run the tests
test:
    zig build test

# Run the tests and the golden tests (downloads Ghostty once)
golden:
    zig build test -Dgolden

# Format the Zig sources
fmt:
    zig fmt build.zig src

# Check the Zig sources are formatted
fmt-check:
    zig fmt --check build.zig src

# Zig's install writes a new file rather than over the old one. Overwriting
# a binary in place makes macOS kill it at launch: the kernel keeps the old
# file's code signature. Running daemons keep the binary they started with.
# `nrex` is a symlink to it, the shorthand.
[doc("Build optimized and install as PREFIX/bin/neorex (~/.local by default), with nrex")]
install:
    zig build -Doptimize={{ optimize }} --prefix {{ quote(prefix) }}
    ln -sfn neorex {{ quote(prefix / "bin" / "nrex") }}
    @echo "installed {{ prefix }}/bin/neorex ({{ optimize }}), and nrex"
