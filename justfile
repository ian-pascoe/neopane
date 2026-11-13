# Where `just install` puts neopane, and which Cargo profile builds it.
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

# Run neopane from target/debug, with any Neovim arguments
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
# daemons keep the binary they started with. `npane` is a symlink to it, the
# shorthand. neopane was neorex (ADR 0005): our old `nrex` symlink goes, but
# a `neorex` binary may be the original neorex's, so it only gets a warning.
[doc("Build optimized and install as PREFIX/bin/neopane (~/.local by default), with npane")]
install:
    cargo build --profile {{ profile }}
    mkdir -p {{ quote(prefix / "bin") }}
    cp target/{{ profile }}/neopane {{ quote(prefix / "bin" / ".neopane.new") }}
    mv -f {{ quote(prefix / "bin" / ".neopane.new") }} {{ quote(prefix / "bin" / "neopane") }}
    ln -sfn neopane {{ quote(prefix / "bin" / "npane") }}
    @if [ "$(readlink {{ quote(prefix / "bin" / "nrex") }})" = neorex ]; then rm {{ quote(prefix / "bin" / "nrex") }}; echo "removed the old nrex symlink"; fi
    @if [ -e {{ quote(prefix / "bin" / "neorex") }} ]; then echo "warning: {{ prefix }}/bin/neorex is still there: neopane was neorex; remove it unless it's the original neorex" >&2; fi
    @echo "installed {{ prefix }}/bin/neopane ({{ profile }}), and npane"
