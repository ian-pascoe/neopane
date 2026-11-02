# Where `just install` puts neorex, and which Cargo profile builds it.
# Override either with an environment variable or on the command line:
#   just prefix=/usr/local install
prefix := env("PREFIX", home_directory() / ".local")
profile := env("PROFILE", "release")

# List the recipes
default:
    @just --list

# Build into target/debug
build *args:
    cargo build {{ args }}

# Run neorex from target/debug, with any Neovim arguments
run *args:
    cargo run -- {{ args }}

# Run the tests
test:
    cargo test

# Run the tests and the golden tests (downloads and builds Ghostty once; needs Zig 0.17)
golden:
    cargo test
    scripts/golden.sh --test golden

# Format the Rust sources
fmt:
    cargo fmt

# Check the Rust sources are formatted
fmt-check:
    cargo fmt --check

# Lint the Rust sources
lint:
    cargo clippy --all-targets -- -D warnings

# The binary is copied next to the old one and renamed over it: a new file,
# not the old one overwritten. Overwriting a binary in place makes macOS kill
# it at launch: the kernel keeps the old file's code signature. Running
# daemons keep the binary they started with. `nrex` is a symlink to it, the
# shorthand.
[doc("Build optimized and install as PREFIX/bin/neorex (~/.local by default), with nrex")]
install:
    cargo build --profile {{ profile }}
    mkdir -p {{ quote(prefix / "bin") }}
    cp target/{{ profile }}/neorex {{ quote(prefix / "bin" / ".neorex.new") }}
    mv -f {{ quote(prefix / "bin" / ".neorex.new") }} {{ quote(prefix / "bin" / "neorex") }}
    ln -sfn neorex {{ quote(prefix / "bin" / "nrex") }}
    @echo "installed {{ prefix }}/bin/neorex ({{ profile }}), and nrex"
