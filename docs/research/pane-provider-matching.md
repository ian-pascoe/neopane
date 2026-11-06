# Pane provider matching by environment variable

Question: can "first declared variable present, in order [tmux, herdr, rex]"
identify which multiplexer/terminal a process is attached to? Checked against
tmux master (`/tmp` clone, tags up to 3.8; `CHANGES` 3.2), herdr 0.9.5
(`ee5bfb5`, 2026-10-29), Rex 0.1.x docs (superlogical.com/rex/docs) and Rex
build `0.0.1793645657-gd018d2` (this shell). neorex today reads only
`REX_SERVER`, `REX_SESSION` (required) and `REX_BLOCK` (optional)
(`src/daemon/launch.rs:36`, `src/daemon/serve.rs:161-162,290`,
`src/debug/rex.rs:45-47`).

## 1. tmux

### Variables set in every pane

`spawn.c:414-415` builds the pane environment with `environ_for_session(s, 0)`,
then sets `TMUX_PANE`. `environ.c:249-285` (`environ_for_session`):

1. copies the global environment, then the session environment over it;
2. unless `no_TERM`: sets `TERM` (= `default-terminal`),
   `TERM_PROGRAM=tmux`, `TERM_PROGRAM_VERSION=<tmux version>`,
   `COLORTERM=truecolor` (`environ.c:264-269`), unconditionally `set`, so it
   **overrides** an outer `TERM_PROGRAM`;
3. always sets `TMUX=<socket_path>,<server pid>,<session id>` (`environ.c:282`),
   overriding any inherited `TMUX`;
4. `spawn.c:415`: `TMUX_PANE=%<pane id>` (documented in `tmux.1`, "The pane ID
   is passed to the child process of the pane in the TMUX_PANE environment
   variable").

Version: `TERM_PROGRAM`/`TERM_PROGRAM_VERSION` were added by commit
`63390d2d` (2020-05-05, "Export TERM_PROGRAM and TERM_PROGRAM_VERSION like
various other terminals"), listed under `CHANGES FROM 3.1c TO 3.2` (first
release tag containing it: 3.2). `TMUX` and `TMUX_PANE` predate that (long
standing). Caveat: commit `55d660a5` (2025-11-28, first tag 3.7) skips
`TERM`/`TERM_PROGRAM*` (the `no_TERM` path) for commands run from the config
file; those are not panes, but `run-shell` children of the config file will
not have `TERM_PROGRAM=tmux`.

### What a pane inherits from outside

- `tmux.c:460-462`: `global_environ` is a copy of the environment of the
  process that **starts the server** (the first `tmux` invocation that creates
  one, e.g. `tmux new`), plus `PWD`. Every pane copies it (`environ.c:256`).
  So `REX_SERVER`, `REX_SESSION`, `REX_BLOCK`, `HERDR_*` exported in the shell
  that started the server **appear in every pane**, in every session, for the
  life of the server, whether or not later clients attach from elsewhere.
- `update-environment` (`options-table.c:1333`; default `DISPLAY KRB5CCNAME
  MSYSTEM SSH_ASKPASS SSH_AUTH_SOCK SSH_AGENT_PID SSH_CONNECTION
  WAYLAND_DISPLAY WINDOWID XAUTHORITY XDG_CURRENT_DESKTOP
  XDG_SESSION_DESKTOP XDG_SESSION_TYPE`) is applied by `environ_update`
  (`environ.c:186`) from the **attaching client's** environment into the
  **session** environment, on `new-session`, `attach-session`, and
  `switch-client` (`cmd-new-session.c:288`, `cmd-attach-session.c:151,175`,
  `cmd-switch-client.c:172`). Only names matching those patterns are copied;
  `REX_*` is not in the default, so a client attaching from a different Rex
  block does **not** update `REX_BLOCK`/`REX_SESSION`. Panes keep the values
  of the shell that created the server (or the session's creating client, since
  `new-session` also runs `environ_update`, but only for listed names).
  Unlisted names that are unset in the new client are not cleared.
  A user can add `REX_BLOCK` etc. to `update-environment`, but new panes only.
  Existing panes never change.
- Stale `TMUX`: a pane always gets a fresh `TMUX`. A terminal started
  **outside** tmux has a `TMUX` only if its own parent environment had one,
  and tmux never clears it in a client (`tmux.c:551` only reads it to refuse
  nesting; `server-client.c:100-111` uses it for nested-attach checks). So a
  GUI terminal/Rex server launched from a tmux pane (`open -a`, a launchd
  start from a tmux shell, `rex server start` in tmux) carries `TMUX` and
  `TMUX_PANE` into every shell it spawns. That `TMUX` points at a possibly
  dead socket/pid.

## 2. herdr

Sources: github.com/herdrdev/herdr, `src/pane.rs`, `src/integration/env.rs`,
`src/main.rs`, `CHANGELOG.md`.

Pane environment (`src/pane.rs:103-217`, `apply_pane_terminal_env` and the
launch path):

| Variable | Value | Set when |
| --- | --- | --- |
| `TERM_PROGRAM` | `herdr` | every pane; **overrides** outer (`pane.rs:107`; test at `pane.rs:4390-4398` sets outer `iTerm.app`, asserts `herdr`) |
| `TERM_PROGRAM_VERSION` | herdr version | every pane (`pane.rs:108`) |
| `HERDR_ENV` | `1` | every pane (`pane.rs:202`, `pty/backend/unix.rs:77`; `main.rs:3-4`) |
| `HERDR_SOCKET_PATH` | API socket path | every pane (`integration/env.rs:29-34`) |
| `HERDR_BIN_PATH` | herdr executable | every pane, if resolvable |
| `HERDR_WORKSPACE_ID`, `HERDR_TAB_ID`, `HERDR_PANE_ID` | e.g. `w1:p2` | "Managed" panes (`pane.rs:208-215`); `HERDR_PANE_ID` is removed for others (`pane.rs:217`) |

`HERDR_SESSION` names a herdr session for the CLI (`src/session.rs:10`); it is
not documented as a pane marker.

Changelog: `[0.9.2] - 2026-09-29`: "New panes set `TERM_PROGRAM=herdr` and
`TERM_PROGRAM_VERSION`. They no longer inherit terminal session IDs, such as
iTerm2's, or Claude Code session markers from the terminal that started the
server. (#4104)". Before 0.9.2 a pane kept the outer `TERM_PROGRAM`.
`HERDR_ENV` is the nested-instance marker (`main.rs:453`,
`should_block_nested_for_env`; `update.rs:854`).

Scrubbed from panes (`pane.rs:~125-140`): `ITERM_SESSION_ID`, `LC_TERMINAL`,
`LC_TERMINAL_VERSION`, `WEZTERM_PANE`, `KITTY_WINDOW_ID`, `CODEX_THREAD_ID`,
`OMPCODE`, `CLAUDECODE`, `CLAUDE_CODE_*`. **Not scrubbed**: `TMUX`,
`TMUX_PANE`, `REX_*`. A herdr server started from a tmux pane or a Rex block
therefore hands every pane that `TMUX*`/`REX_*` set. (Inferred from the
scrub list; not tested end to end.)

## 3. Rex

Docs (superlogical.com/rex/docs, "Command Targets" in the CLI page): "Rex
knows where you are from three environment variables it sets in every
terminal": `REX_SESSION` (session ID), `REX_BLOCK` (the terminal's own block
ID), `REX_SERVER` (address of the server running the terminal). The agent
page says an agent started outside Rex "has no `REX_SESSION`". Rex also
prepends its bin directory to `PATH` and sets `REX_BIN_DIR` (shell
integration docs). `REX_CLIENT` and `REX_CONFIG` are user-set inputs, not
per-terminal markers.

`TERM_PROGRAM` is **not documented** anywhere in the Rex docs (full text
fetched from `/rex/docs/llms-full.txt`, no match), and no changelog entry
found. Observed in this shell, which runs inside Rex:

```
TERM_PROGRAM=rex
TERM_PROGRAM_VERSION=0.0.1793645657-gd018d2
REX_SESSION=session:01a1a1db-...
REX_BLOCK=block:01a1a7b9-...
REX_SERVER=unix:///Users/ianpascoe/Library/Application%20Support/rex/server.sock
REX_BIN_DIR=.../com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS
```

No `TMUX` or `HERDR_*` here. So `TERM_PROGRAM=rex` is observed behaviour of one
build, not a documented contract. Also note `REX_SERVER`/`REX_SESSION` are the
only hard requirements in neorex, and Rex block shells inherit them, so any
child multiplexer started in a block (tmux, herdr) inherits all three `REX_*`.

## 4. Conclusion

What each environment looks like (`+` = variable present):

| Scenario | TMUX | HERDR_ENV | REX_* | TERM_PROGRAM |
| --- | --- | --- | --- | --- |
| Rex only | - | - | + | `rex` (observed) |
| tmux in Rex | + | - | + (inherited from server starter) | `tmux` |
| herdr in Rex | - | + | + (inherited) | `herdr` |
| herdr in tmux | + (inherited from server starter, not scrubbed) | + | - (or + if tmux in Rex) | `herdr` |
| tmux in herdr | + | + (inherited by tmux) | maybe | `tmux` |
| Rex launched from a tmux shell | + stale | - | + | `rex` |

"First declared match in order [tmux, herdr, rex]" gives:

- **tmux-in-Rex**: correct (tmux). Rex-inside-nothing: correct (rex).
- **herdr-in-Rex**: correct (herdr).
- **herdr-inside-tmux: wrong**: reports tmux. `TMUX` is inherited into herdr
  panes (herdr does not scrub it), and tmux is first.
- **tmux-inside-herdr**: correct only by luck of ordering (tmux first). Flip
  the order to fix the previous case and this one breaks: both inner
  multiplexers see the outer one's marker, so presence checks are symmetric.
  **No fixed order handles both.**
- **Rex with stale `TMUX`** (Rex server or GUI started from a tmux shell):
  wrong, reports tmux while the real provider is Rex.
- Stale `REX_*` in tmux started from another block: tmux still wins, but a
  Rex provider fallback would target the wrong block (`REX_BLOCK` is that of
  the server starter, never refreshed by `update-environment`).
- A user-exported or leaked `HERDR_ENV`/`TMUX` in a profile misleads the same
  way; presence says "an ancestor was X", not "the nearest enclosing one is X".

### Fix options

1. **Use `TERM_PROGRAM` first, presence second.** Each multiplexer overwrites
   it for its panes (tmux 3.2+, `environ.c:267`; herdr 0.9.2+, `pane.rs:107`;
   Rex, observed), so it names the *innermost* provider. Map `tmux`->tmux,
   `herdr`->herdr, `rex`->rex, then require that provider's own declared
   variables to confirm (`TMUX` and `TMUX_PANE`; `HERDR_ENV=1` and
   `HERDR_PANE_ID`/`HERDR_SOCKET_PATH`; `REX_SERVER`+`REX_SESSION`). That
   separates all six rows above. Fall back to the ordered presence scan only
   when `TERM_PROGRAM` is unset or unrecognised (tmux < 3.2, herdr < 0.9.2, old
   Rex). Weak spots: `TERM_PROGRAM` is inherited by anything started in a pane
   (a GUI app launched from a tmux pane keeps `tmux`), a user can override it,
   and tmux config-file `run-shell` children lack it.
2. **Prefer pane-specific ids as confirmation.** `TMUX_PANE` is set only by
   pane spawn (`spawn.c:415`) but is inherited like `TMUX`; `HERDR_PANE_ID`
   only for managed herdr panes. Neither is scrubbed by children, so they do
   not by themselves identify nesting order.
3. **Verify liveness.** For tmux, check that the `TMUX` socket path/pid exists
   (cheap; defeats a stale `TMUX` from a dead server, not a live one). For
   Rex, test that `REX_SERVER` connects.
4. **Ask the provider.** tmux: `tmux display-message -p '#{pane_tty}'` against
   `ttyname(0)`, matching tmux's own nested check
   (`server-client.c:100-111`, compares `wp->tty` to the client tty). herdr:
   resolve `HERDR_PANE_ID` through its API socket. Matching the attaching
   process's controlling tty against each candidate provider's pane ttys is
   the only unambiguous test; env vars alone cannot be.

Recommendation: implement option 1 with option 3 as the confirm step; keep the
ordered scan only as a documented fallback, and declare the tmux-in-herdr /
herdr-in-tmux ambiguity there.

## Not verified

- Behaviour of herdr started from tmux was inferred from source, not run.
- Rex `TERM_PROGRAM` is observed on one build only; no doc or changelog entry.
- tmux tag numbering above (3.7, 3.8) is as reported by `git tag` in the clone.
