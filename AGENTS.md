# neorex

`neorex` in a Rex pane starts a background daemon (`src/daemon/serve.rs`) that runs one Neovim with external windows and shows each window in its own Rex pane. The layers:

- `src/nvim/windows.lua` runs inside Neovim and keeps every window external, except a hidden anchor per tab page. It tells the daemon about window changes with `<name>_*` notes, `<name>` being `NAME` in `src/name.rs`. `src/nvim/windows.rs` parses those notes, and tests it against a real Neovim.
- `src/daemon/` holds the daemon. After each Neovim flush, `Daemon::reconcile` gives new windows and float groups panes and hangs up the panes whose windows closed. It also places the panes of a tab page's windows, in its Rex floating pane, as the `<name>_layout` notes say (`TabPlacement`).
- `src/screen/` draws grids into panes. `src/rex/` is the Rex WebSocket client. `src/term/` handles key input and the terminal.

A new note is a change on both sides: `note` in `windows.lua`, and the parser in `windows.rs`.

Every user-visible name comes from `NAME` in `src/name.rs` (ADR 0005): Neovim variables, highlight groups, notes, Rex labels, log prefixes, file names. `windows.lua` gets it as an argument of its chunk. A test in `src/name.rs` fails on any string literal in `src/`, or line of code in `windows.lua`, that spells the name out.

## Verify

- Run `just test` after every change. Tests that need Neovim skip when `nvim` isn't installed, so a pass without `nvim` proves less. With `CI` set they panic instead; GitHub Actions runs fmt, clippy and the tests, on macOS and Linux, on every push to `main` and every PR.
- Run `just golden` after changing what gets drawn (`src/screen/`, `PaneView`). It checks every frame through Ghostty's emulator, which it builds with Zig 0.17.
- Run `just fmt` and `just lint` on Rust sources. `windows.lua` has no formatter config: keep its two-space hand formatting. If your edit tool reformats Lua, write the file from a shell command instead.

## Running it live

The daemon focuses Rex blocks and switches tabs, so a test instance in the user's own Rex session steals their focus. Test live in a session of your own (`rex new`), or in a PTY with `REX_SESSION` pointing at it.

Each daemon keeps its files in a private directory, `neorex-<launcher pid>.XXXXXX` in `$XDG_RUNTIME_DIR`, else `$TMPDIR`, else `/tmp`. It logs to `log` there.

Install with `just install`. On macOS, a binary overwritten in place is killed at launch because the kernel keeps the old file's code signature. A running daemon keeps the binary it started with, so restart neorex to pick up a new build.

## Neovim behaviour that shapes the design

- External windows always move to the current tab page, so `windows.lua` records each window's home tab itself.
- `:tabclose` closes the current window's floats first, then the tab's other windows, external ones included. `windows.lua` reopens external windows that belong to other tabs.
- Plugins that draw with editor-relative floats, such as smear-cursor.nvim, land in the wrong place, because Neovim can't know where a pane's window is. Neovim starts with `g:neorex` set so configs can turn them off.

## Agent skills

### Issue tracker

GitHub Issues on `ian-pascoe/neorex`, through `gh`. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
