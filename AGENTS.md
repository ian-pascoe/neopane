# neopane

`neopane` in a terminal starts a background daemon (`src/daemon/serve.rs`) that runs one Neovim with external windows; in a Rex pane, Rex is its pane provider and shows each window in its own Rex pane, and elsewhere the windows are drawn in the terminal. The layers:

- `src/nvim/windows.lua` runs inside Neovim and keeps every window external, except a hidden anchor per tab page. It tells the daemon about window changes with `<name>_*` notes, `<name>` being `NAME` in `src/name.rs`. `src/nvim/windows.rs` parses those notes, and tests it against a real Neovim.
- `src/daemon/` holds the daemon. After each Neovim flush, `Daemon::reconcile` gives new windows and float groups panes and hangs up the panes whose windows closed. It also places the panes of a tab page's windows in its tab page area, as the `<name>_layout` notes say, through the pane provider's `place_beside` and `resize` (`TabPlacement`).
- `src/cli/` parses the command line by hand: own options first, then Neovim's arguments, read by Neovim's rules (ADR 0014). `list`, `status` and `kill` send control requests (`src/daemon/control.rs`), which the daemon answers in `src/daemon/daemon/control.rs` from what it tracks, never asking Neovim; `src/cli/report.rs` prints their replies, its JSON only ever gaining fields. `open`, sent from a daemon's own `:terminal` (ADR 0015), is the exception: `src/daemon/daemon/open.rs` asks Neovim for the file's window without blocking the loop, and with `wait`, answers once Neovim's `<name>_done` note says the file is done.
- `src/config/` runs the user's config in a config child, the hidden `config-child` command, and checks what its `provider` returned with the daemon's own rules (ADR 0007). The child sends a plain tree back in a frozen encoding (`tree.rs`, `wire.rs`): a daemon reads answers from a newer binary after an upgrade in place.
- `src/attach/` defines the attach protocol: its frames, hello, pane ids and attach command (`docs/attach-protocol.md`).
- With Rex closed, a new window is drawn in its origin pane, laid out as Neovim lays out splits: `src/daemon/pane_layout.rs` keeps each pane's layout of each tab page it holds, and `PaneView::frame_laid_out` draws the one it shows (ADRs 0009, 0012).
- `src/daemon/provider.rs` is the pane provider seam: the daemon asks a capability (`Ask`) and reads answers and events (`ProviderEvent`); `src/daemon/asks.rs` queues the asks, times them out and falls back. `src/providers/` holds the pane providers: Rex's, built in (`providers/rex/`), and the command runner (`providers/command/`), which runs a config's capability commands on a loop of its own, watched under `Token::Provider`; `docs/capabilities.md` is their contract. A self-attach chooses the pane provider with the user's config, run in the config child on a thread of its own, else the built-in default, from its hello (`src/providers/choose.rs`), and the daemon confirms it with `find` and checks later attaches' reach (`src/daemon/daemon/choosing.rs`). `provider check` (`src/providers/check/`) chooses as a self-attach in its own terminal would, outside any daemon, asks only `find`, through the same seam, and reports it. The daemon holds one at a time, as a `Box<dyn PaneProvider>`: kept while a terminal in its reach is attached or on its way, dropped otherwise or when it is `Gone`; the next self-attach chooses again, and the panes attached before rejoin as `find` says. A pane provider's live layout (`src/daemon/live_layout.rs`) gives each pane its layout area: Neovim's screen is the current pane's, and each area has its own tabline panes (ADR 0008). Focus and `focus_direction` are asked through it too, one running and the newest waiting; `src/daemon/provider_focus.rs` tells the user's focus moves from the daemon's own landing.
- `src/screen/` draws grids into panes. `src/rex/` is the Rex WebSocket client and knows nothing of `<name>`. `src/term/` handles key input and the terminal.

The core knows nothing of Rex, or of any other pane provider. Nothing in `src/daemon/`, `src/screen/`, `src/nvim/` or `src/term/` may name `crate::rex` or a pane provider's module (`crate::providers::rex`, and tmux's, zellij's and herdr's); the core reaches them only through the neutral modules of `crate::providers` (`choose`, `command`, `tty`) and the `PaneProvider` trait, and a test in `tests/boundary.rs` fails if it does. `providers/tty.rs` proves a terminal by its device, for any pane provider's `find`. Something only Rex needs goes in Rex's pane provider, behind a capability every pane provider could offer.

A new note is a change on both sides: `note` in `windows.lua`, and the parser in `windows.rs`.

Every user-visible name comes from `NAME` in `src/name.rs` (ADR 0005): Neovim variables, highlight groups, notes, Rex labels, log prefixes, file names. `windows.lua` gets it as an argument of its chunk. A test in `src/name.rs` fails on any string literal in `src/`, or line of code in `windows.lua`, that spells the name out.

## Verify

- Run `just test` after every change. Tests that need Neovim skip when `nvim` isn't installed, so a pass without `nvim` proves less. With `CI` set they panic instead; GitHub Actions runs fmt, clippy and the tests, on macOS and Linux, on every push to `main` and every PR.
- Run `just golden` after changing what gets drawn (`src/screen/`, `PaneView`). It checks every frame through Ghostty's emulator, which it builds with Zig 0.17.
- Run `just fmt` and `just lint` on Rust sources. `windows.lua` has no formatter config: keep its two-space hand formatting. If your edit tool reformats Lua, write the file from a shell command instead.

## Running it live

The daemon focuses Rex blocks and switches tabs, so a test instance in the user's own Rex session steals their focus. Test live in a session of your own (`rex new`), or in a PTY with `REX_SESSION` pointing at it.

Each daemon keeps its files in `$NEOPANE_TMPDIR` (else `/tmp`) `/neopane-<uid>/<daemon name>/` (ADR 0004, `src/daemon/dir.rs`), locked with `flock` while it runs. It logs to `log` there, kept after it ends; the next daemon of that name renames it `log.1`.

Install with `just install`. On macOS, a binary overwritten in place is killed at launch because the kernel keeps the old file's code signature. A running daemon keeps the binary it started with, so restart neopane to pick up a new build.

## Neovim behaviour that shapes the design

- External windows always move to the current tab page, so `windows.lua` records each window's home tab itself.
- `:tabclose` closes the current window's floats first, then the tab's other windows, external ones included. `windows.lua` reopens external windows that belong to other tabs.
- Plugins that draw with editor-relative floats, such as smear-cursor.nvim, land in the wrong place, because Neovim can't know where a pane's window is. Neovim starts with `g:neopane` set so configs can turn them off.

## Agent skills

### Issue tracker

Issues live in Linear (team SPI, project `195685cc-087f-4ec9-9bc8-0a0606dd056d`, named by its id since its name may change); GitHub Issues on `ian-pascoe/neopane` are the public intake and sync one-way into it. Reach Linear through Executor's Linear app, falling back to the `linear` CLI. See `docs/agents/issue-tracker.md`.

### Triage labels

The five default labels, as labels of the Linear team SPI: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` and `docs/adr/` at the repo root. See `docs/agents/domain.md`.
