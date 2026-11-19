# Contributing to neopane

Thanks for helping. neopane is very early, so bug reports are as valuable as code. By taking part you agree to
the [Code of Conduct](CODE_OF_CONDUCT.md).

## Reporting bugs and asking for features

Open a [GitHub issue](https://github.com/spiritledsoftware/neopane/issues/new/choose) with the bug report or feature
request form. For a bug, the form asks for `neopane --version`, Neovim's version, the pane provider, and the
daemon's log: they answer most first questions. Report security problems privately instead, as
[SECURITY.md](SECURITY.md) says.

The maintainers track work in Linear, which copies each new GitHub issue. Its identifiers, such as `SPI-123`, show
up in commit messages and pull requests; the GitHub issue stays where you hear back.

## Before you write code

For anything bigger than a small fix, open an issue first, or comment on one, so we can agree on the approach before
you spend time on it. The design decisions so far are recorded in [docs/adr/](docs/adr/), and
[GLOSSARY.md](GLOSSARY.md) defines the terms the code and docs use.

## Setting up

You need:

- Rust 1.88 or later ([rustup](https://rustup.rs)) and [just](https://github.com/casey/just)
- Neovim 0.12 or later: without `nvim`, the tests that need it skip, so a pass proves less
- macOS or Linux

Optional, for the tests of each pane provider (they skip without it): tmux, zellij, herdr, and `hc` for the Herdr app
(`scripts/install-*.sh` install the versions CI uses). The golden tests need Zig 0.17.

```sh
just build
just run some/file.txt   # target/debug/neopane, with Neovim arguments
just test                # all the tests
just golden              # also the golden tests; downloads and builds Ghostty once
just fmt
just lint
```

[AGENTS.md](AGENTS.md) maps the source tree, layer by layer; read it before a change of any size. Testing a daemon
live in a terminal you work in can steal its focus: the "Running it live" section there says how to avoid it.

## Making a change

- Keep a pull request to one change, with tests for what it fixes or adds.
- Run `just test`, `just fmt` and `just lint` before you push. Run `just golden` too if you change what gets drawn
  (`src/screen/`, `PaneView`). CI runs fmt, clippy and the tests on macOS and Linux, and checks the minimum Rust
  version.
- Update the README or `docs/` when you change what users see. A decision that is hard to reverse gets an ADR in
  `docs/adr/`.
- A few rules are enforced by tests:
  - The core (`src/daemon/`, `src/screen/`, `src/nvim/`, `src/term/`) never names a pane provider's module
    (`tests/boundary.rs`). Something only one pane provider needs goes behind a capability
    ([docs/capabilities.md](docs/capabilities.md)).
  - Every user-visible name comes from `NAME` in `src/name.rs`, never a string literal.
  - A new note from Neovim changes both sides: `note` in `src/nvim/windows.lua`, and its parser in
    `src/nvim/windows.rs`.
- `windows.lua` has no formatter: keep its two-space hand formatting.
- Title the pull request as a [Conventional Commit](https://www.conventionalcommits.org): `feat: ...` for something
  new users see, `fix: ...` for a bug fix, or `docs:`, `perf:`, `refactor:`, `test:`, `build:`, `ci:`, `chore:`. A
  scope is optional (`fix(tmux): ...`), and `!` after the type (`feat!: ...`) marks a breaking change. Pull requests
  are squashed when they merge, so the title becomes the commit on `main`, and a check fails on a title that isn't
  one. Commits inside the pull request can say what you like.

AI-assisted contributions are welcome; neopane itself is built that way. You are responsible for what you submit:
read it, test it, and be ready to explain it.

## Releases

Releases are automated with [release-please](https://github.com/googleapis/release-please). Each merge to `main`
updates a release pull request, which bumps the version in `Cargo.toml` and `Cargo.lock` and adds the changes to
`CHANGELOG.md`: `feat` and `fix` titles (and `perf`, `revert`, `docs`) each get a line, the others none. Until 1.0, a
`feat` bumps the minor version, a `fix` the patch, and a breaking change the minor version too.

Merging the release pull request tags the version and drafts its GitHub release. The `Release` workflow then builds
the archives for macOS and Linux, uploads them, publishes the release, and publishes the crate to crates.io. It also
lands the release's formula, rendered by `scripts/homebrew-formula.sh`, in
[spiritledsoftware/homebrew-tap](https://github.com/spiritledsoftware/homebrew-tap) (the `Homebrew` workflow): change
the formula in that script, never in the tap, since each release overwrites the tap's. If a step fails, fix it and dispatch
`Release` with the tag (`gh workflow run release.yml -f tag=v0.2.0`): it picks up where the release stands. To update
the tap alone, dispatch `Homebrew` with the tag (`gh workflow run homebrew.yml -f tag=v0.2.0`). To release a version other than the one proposed, put `Release-As: 0.3.0` in the body of a commit on
`main`.

## License

neopane is [MIT licensed](LICENSE). Contributions are accepted under the same license.
