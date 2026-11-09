# CLI facts: Neovim argument parsing, `$EDITOR` conventions, `:terminal` pty

Facts for the `<name> [OWN-OPTIONS] [NVIM-ARGS…]` / `<name> --wait FILE` design
(`<name>` = neorex, to be renamed neopane). Gathered 2026-11-09 on macOS with
nvim 0.12.6 and git 2.56.0.

Pinned sources (all read from local clones, links use the pinned ref):

| Source | Ref |
|---|---|
| Neovim | `v0.12.6` = `779c36969e31080503be2dfccbcc3f999e289606` |
| git | `v2.56.0` = `a018953688f1b10bddf91bff8747068f5f4746a4` |
| less | `v668` = `e77e1176c80cca989694818866ae6cd7e7707161` (local `less --version`: 668) |
| bash | `bash-5.3` = `b8c60bc9ca365f8261fa97900b6fa939f6ebc303` (local: 5.3.20) |
| zsh | tag `zsh-5.9` (`Functions/Zle/edit-command-line`; local zsh 5.9.2) |
| Apple cron (macOS `crontab`) | `cron-52` = `189460cca8af2ee7d37d56cddd0fd9e6e8c906b8` |
| cronie (Linux `crontab`) | master `cb49729da91af528805aadf2faa4da27cb36edec` (2026-11-05, not a tag) |
| sudo | `v1.9.17p2` = `d1b48c651cec19fe37d1f0d3299d2283fb0f88e4` (local: 1.9.17p2) |
| kubectl | `v0.34.0` (kubernetes/kubectl `ed55c29bbf1ffc3c072905ce4279949e443afda8`) |
| gh | trunk `736c68f674113337685e3eb1f908602a03b8f99b` (2026-11-06, not a tag) |
| neovim-remote | `v2.5.1` = `1004d41696a3de12f0911b1949327c3dbe4a62ab` |
| VS Code | `main` as of 2026-11-09 (`cf2ba5544aee93ccde676d6290a42bcfe7d915b7`) |

Local experiments used a harness that starts `nvim --embed --headless ARGS…`
as an RPC child (`jobstart(..., {rpc=true})`), evaluates an expression, and
reports exit status and stderr. "Observed" below means that harness (or a
direct shell run) on nvim 0.12.6.

Neovim file:line references are `src/nvim/main.c` unless stated, at `v0.12.6`
(`https://github.com/neovim/neovim/blob/v0.12.6/src/nvim/main.c#L<n>`).

---

# R1. How Neovim 0.12 parses its command line

**Short answer.** One pass, `command_line_scan()` (`main.c:1098`), runs *before*
`--embed` takes over stdio. Options are hand-rolled, not getopt: a word starting
with `-` is a cluster of single-letter flags; a few letters take the *next word*
as a value; `--long` options match case-insensitively and (mostly) by prefix;
`--` ends options. Everything that is not an option is a file name, kept in
order. Nothing is validated beyond that, so wrapper options must be removed
before the rest is handed over.

## R1a. Which options take a value

Value-taking letters set `want_argument` (`main.c:1351`) and then, at
`main.c:1359-1371`, **error** if anything follows the letter in the same word
(`Garbage after option argument`) or if no word follows (`Argument missing
after`, except `-S`). So the value is always the *next* argv word, taken
unconditionally (even if it starts with `-`), **except -S** and **-q** (below).

| Option | Value form | Evidence |
|---|---|---|
| `-c {cmd}` | separate word **or attached** (`-c"set ro"`, `-cq`) | `1335-1344` |
| `+{cmd}` / `+N` / `+/pat` / `+` | attached only; bare `+` = `$`; max 10 of `+`/`-c`/`-S` combined (`MAX_ARG_CMDS`, `main.h:8`) | `1112-1121` |
| `--cmd {cmd}` | separate word only (`--cmd=x` is an error) | `1184-1186`, `1403-1409` |
| `-u {vimrc}`, `-U`, `-i {shada}`, `-W {file}`, `-l {file}`, `-s {file}` | separate word only. `-uNONE` → `Garbage after option argument` (observed) | `1345-1352`, `1359-1364` |
| `-t {tag}` | separate **or attached** (`-tmain`) | `1302-1313` |
| `-w` | `-w80` / `-w 80` (digits) = `'window'`; otherwise `-w {scriptout}` (next word) | `1325-1333`, `1466-1475` |
| `--listen {addr}`, `--server {addr}`, `--startuptime {file}` | separate word only; `--listen=x` is an error (observed) | `1172-1175`, `1179-1182`, `1187-1189`, `1410-1416` |
| `-V[N][file]` | **attached only**: `-V`, `-V3`, `-V9log.txt`, `-Vlog.txt`; it never consumes the next word | `1317-1324` |
| `-o[N]` `-O[N]` `-p[N]` | attached digits only (`get_number_arg`, `948-958`) | `1255-1269` |
| `-r`, `-L`, `-R`, `-n`, `-d`, `-b`, `-m`, `-M`, `-A`, `-H`, `-D`, `-N`, `-X`, `-f`, `-e`, `-E`, `-h`, `-?`, `-v` | no value | `1203-1334` |
| `-T`, other letters | `Unknown option argument` (observed `-T xterm`) | `1354-1355` |

**Optional-value options**

- `-S [file]` (`1345`, `1366-1391`): the value is optional. If it is the last
  word, or the next word **starts with `-`**, `-S` defaults to `Session.vim`
  and does *not* consume the word. Otherwise the next word is the script, even
  if that word is meant as a file (observed: `-S f.txt` sources `f.txt` as
  Vimscript). Executed as `so {file}` after the first file loads.
  Docs: `runtime/doc/starting.txt:146-151` ("{file} cannot start with a '-'").
- `-q [errorfile]` (`1270-1281`, `1420-1422`): attached (`-qerrs.txt`) takes the
  value; as the last word it takes none; **otherwise it takes the next word
  unconditionally, even `-n`** (observed: `-q -n g.txt` → `Too many edit
  arguments: "g.txt"`, because `-n` became the errorfile). Docs:
  `starting.txt:69-72`.

## R1b. Combining single-letter flags

Yes. `starting.txt:78-79` ("Single-letter options can be combined after one
dash"), implemented by the `argv_idx` loop (`1528-1532`: the word is only left
once no letters remain). Observed `-Rn`, `-nR`, `-nu NONE`, `-nc 'cmd'`,
`-Rc'cmd'` all work.

- A value letter must be the **last letter in the word** (its value is the next
  word) — except `-c`, `-t`, `-q`, `-V`, `-w` (digits), `-o/-O/-p`, which consume
  the *rest of the word*. `-nuNONE` → error (observed).
- `-o2`, `-O`, `-p3`: letter followed by optional digits; after the digits the
  word must end, or the next char is read as another flag. So `-pn2` and `-n2p`
  → `Unknown option argument` (observed). `-oN` N = window count, `-ON` vertical,
  `-pN` tabs; no N = one per file (`starting.txt:306-322`); last of `-o/-O/-p`
  wins (observed `-op f g` → 2 tabs).
- Letter order matters for `-s`: after an `e`/`E` in the same word, `-s` means
  silent mode (`-es`, `-Es`, `1291-1297`); `-s` alone, or in another word, takes
  a script file (`-s -e` would take `-e` as the script).

## R1c. `-v`, `-?`, `-h`, `--help`, `--version`, `--api-info`

All handled during the scan, print and `os_exit(0)` before `--embed` takes over
stdio (`channel_from_stdio` is called at `main.c:324`, after the scan at
`302`).

- `-h`, `-?`, `--help` → `usage()` (`2311`) to stdout, exit 0 (`1149-1151`, `1232-1235`).
- `-v`, `--version` → `version()` (`2300`), exit 0 (`1152-1154`, `1314-1316`).
  **Observed: with `--embed` it prints nothing** (stdout and stderr both 0 bytes,
  exit 0) while `nvim --headless --version` prints 87 bytes. Don't probe the
  version through an `--embed` process; run plain `nvim --version`.
- `--api-info` → msgpack API metadata to stdout, exit 0 (`1155-1167`); works
  with `--embed` (observed 32041 bytes).
- Long names are matched case-insensitively (`STRICMP`/`STRNICMP`; `--EMBED`
  works) and the `listen`/`remote`/`server`/`noplugin`/`cmd`/`startuptime`/`clean`
  ones are **prefix matches** (`--cleanx` is accepted; `--listenx foo` →
  `Garbage after option argument`).

## R1d. `-r` / `-L`

Same flag (`main.c:1287-1290`, `recoverymode = 1`).

- With a file name: recover that file's swap file (`create_windows`,
  `1772-1778`; `getout(1)` if recovery fails). Observed `-r f.txt` with no swap
  file: exit 1, **no message on stderr** under `--embed` (non-embed prints
  `E305: No swap file found for f.txt`).
- Without a file name: `headless_mode = true` (`335-337`), list swap files via
  `recover_names()`, exit 0 (`514-517`). Observed plain `nvim --headless -r`
  does that. **Under `--embed --headless` it did not exit by itself** within 4 s
  (it answered RPC and ended only when stdin closed, status 1). Treat `-r`/`-L`
  as unsupported for a long-lived server.

## R1e. `--`, `-`, and piped stdin

- `--` → `had_minmin = true` (`1196-1201`); afterwards even `+cmd` and `-x` are
  file names (observed `-- +3 -x` → argv `{"+3","-x"}`). Nothing can follow as an
  option. Docs: `starting.txt:30-35`.
- `-` alone (`case NUL`, `1128-1141`) → `had_stdin_file`, `edit_type = EDIT_STDIN`:
  "read stdin as text" into buffer 1. With `-e`/`-es` it instead means "read
  Ex commands from stdin".
- Without `-`, stdin is read as text implicitly **only if**
  (`edit_stdin()`, `1086-1095`): not `--headless`, not (`--embed` with `stdin_fd
  <= 0`), stdin not a tty, no `-s -`, etc. Docs `starting.txt:42-44`: "except -s,
  -es, --embed, --headless". Observed: `echo foo | nvim --headless +… +q` leaves
  the buffer empty; adding `-` makes it `foo`.
- **Interaction with `--embed`**: `channel_from_stdio` (`src/nvim/channel.c:590-598`)
  moves the RPC channel onto *duplicates* of fds 0/1 and then `dup2`s **stderr
  onto both stdin and stdout**. So for `--embed`, "stdin" as seen by the editor
  core is the stderr fd; a literal `-` argument reads from that, not from the RPC
  pipe and not from the original pipe (observed with `--embed --headless -`:
  an empty buffer, RPC unaffected). `stdin_fd` is only set by a UI attaching with
  the `stdin_fd` option (the builtin TUI client, `ui_client.c:106`). A wrapper
  that wants `echo hi | <name>` to work must read its own stdin and feed it in
  over RPC (or into a temp file).

## R1f. `-e`, `-E`, `-es`, `-Es`, `-l` under `--embed`

- **`-es`, `-Es`, `-l` are rejected:** after the scan, `if (embedded_mode &&
  (silent_mode || parmp->luaf)) mainerr("--embed conflicts with -es/-Es/-l")`
  (`1535-1537`) → exit 1, stderr `nvim: --embed conflicts with -es/-Es/-l`
  (observed for all three; applies even if `--headless` is also given).
- **`-e` / `-E` are accepted** (not covered by the check) and start the server in
  Ex mode (`exmode_active`, `1223-1228`). Observed: `mode(1)` is `cv`, RPC works,
  and with a `--remote-ui` TUI attached it stays in Ex mode until `:vi`. It
  makes sense only as "start in `gQ`"; `getout()` also adds `ex_exitval` to the
  exit status in Ex mode (`main.c:758-760`). Not useful for panes.
- `-ll` is only recognised as `argv[1]` (`main.c:265`), so it cannot follow
  `--embed`.

## R1g. Other options that conflict with `--embed --headless --listen ADDR`

- **`--remote*`** (any word starting `--remote`, `1177-1178`) sets `params.remote`;
  `remote_request()` (`979-1082`, called at `347-350`) then makes this process a
  **client** of `--server ADDR` (`vim._cs_remote`) and `os_exit`s (status 0, or 2
  on error) instead of becoming a server. Observed: `--remote-send ix` → exit 2,
  `E247: No server specified with --server`; `--remote-ui` → exit 1, `Remote ui
  failed to start: no address specified`. Must never be forwarded to the daemon.
  `--server ADDR` alone is harmless (only used by `--remote*`).
- `-es`/`-Es`/`-l`: rejected (R1f). `-e`/`-E`: allowed but odd.
- `--listen` given twice: last wins (`1412`). A bare name without `:`, `/`, `\` is
  not a path: `server_start` (`msgpack_rpc/server.c:173-176`) turns it into a
  generated path under the runtime dir (observed `--listen l.sock` →
  `…/T/nvim.user/XXXX/l.sock.<pid>.0`). Use an absolute path or `./x`.
- `-d` (diff) is fine (observed two windows, `&diff` set). Also fine: `-R`, `-n`,
  `-o/-O/-p`, `-S`, `-c`, `+N`, `--clean`, `-u`, `--cmd`, `-t`, `-q`, `-D`,
  `-b`, `-m/-M`, `-i`, `--startuptime`, `--noplugin`, `-s file`/`-w file`/`-W file`.
- **`-V` / `-V2`+ without a file hangs a headless embed**: observed `-V3`, `-V`
  (= `-V10`): the child never answers RPC (startup `sourcing …` messages; with
  `--cmd 'set verbosefile=…'` it answers and the messages go to the file).
  `-V1` and `-V9/path/log` are fine. Prefer to filter or always supply a file.
- `-` (stdin text) is useless (R1e); `-r/-L` unsupported (R1d); `--version`/`-v`
  print nothing under `--embed` (R1c).
- `--startuptime FILE` is found by a **pre-scan of all of argv**
  (`init_startuptime`, `1585-1601`) that ignores option structure, and is also
  not stopped by `-l` or `--`. (Observed as a harness artifact: running
  `nvim -l h.lua … --startuptime t.log` made the *outer* nvim write `t.log`.)
  The same pre-scan exists for `--clean` (`284-289`).

## R1h. Relative paths resolve against Neovim's starting cwd?

Yes — neither the daemon nor Neovim changes directory before arguments are
processed, so a relative path means the *daemon's* cwd, not the invoking
client's. Observed with the child started in `…/w/sub` (other files elsewhere):

| Arg | Resolved against |
|---|---|
| `-u u.vim` | starting cwd (loaded `sub/u.vim`, `g:u=7`) |
| `-S s.vim` | **the cwd at the time `-S` runs**, i.e. after `--cmd`/init files, `starting.txt:146`; observed `--cmd 'cd ../other' -S s.vim` sourced `other/s.vim` |
| `--startuptime t.log`, `-W out`, `-V1log`, `-i my.shada` | starting cwd (`sub/t.log` created; `-V1v.log` → `sub/v.log`) |
| file names | made absolute while scanning arguments (buffer created in `alist_add`, `main.c:1520-1523`), so the **starting** cwd even if `--cmd 'cd …'` runs later (observed `expand('%:p')` = `…/sub/f.txt` after `--cmd cd ../other`) |
| `--listen ./x` | starting cwd (`server_start`); a bare name is *not* a path (R1g) |

So a client that forwards NVIM-ARGS to a long-running daemon must either
absolutise the path-valued arguments it can recognise (`-u -U -i -s -w -W -S -q
--startuptime -V…file`, `--cmd`/`-c`/`+cmd` text cannot be rewritten), start
the server/session with the client's cwd, or `:cd` first. Do not assume.

## R1i. Exit status for `:cq N` and for failed startup

- `:cquit [N]` (`ex_docmd.c:4927-4934`): status = `N` (the range/count) or
  `EXIT_FAILURE` (1); it sends the `error_exit` UI event (so a `--remote-ui` TUI
  client exits with the same code) and then `getout(status)`. Observed under
  `--embed --headless`: `:cq 3` → 3, `:cq` → 1, `:cquit! 7` → 7, `:cq 300` → 44
  (mod 256), `-c 'cq 4'` → 4, `+cq 5` → 5, `qa!` → 0, `-c qa` → 0. The status is
  the **process exit code of the nvim server**. With a `--remote-ui` TUI client
  attached, the client exit status follows `error_exit`/`ui_client_exit_status`
  (`ui_client.c:367-375` stores it, `tui/tui.c:588-589` makes it the client's exit status; not run here).
- **Bad argument** → `mainerr()` (`2275-2280`): to **stderr**,
  `nvim: <message>[: "arg"]\nMore info with "nvim -h"`, exit **1**. It runs inside
  the scan, i.e. before `channel_from_stdio`, so no RPC is ever served and the
  message is not delivered through the channel — the host must capture the child's
  stderr and exit status. Messages (`main.c:148-152`): `Unknown option argument`,
  `Garbage after option argument`, `Argument missing after`, `Too many edit
  arguments`, `Too many "+command", "-c command" or "--cmd command" arguments`,
  and `--embed conflicts with -es/-Es/-l`. Observed exit 1 with exactly that
  stderr for each. Other exits at startup: `--listen` failure → `mainerr`
  (`Failed to --listen: no such file or directory: "…"`, exit 1, `main.c:372-383`);
  script open failure `-s`/`-W` → exit 2 (`457-469`); unreadable `-q` errorfile →
  exit 3 (`handle_quickfix`, `1652-1665`, observed 3); `-r FILE` with no swap → exit 1.
- Later startup problems (e.g. missing `-u` file, failing `-S`/`-c` command) are
  **not** fatal and not on stderr: in headless `--embed` messages have no UI,
  so the server just keeps running (observed `-u nonexist.vim`, `-S nonexist.vim`).
- If the RPC channel (stdin) closes, nvim exits 1 (observed with `</dev/null`).

---

# R2. How `$EDITOR` is invoked, and does `EDITOR="<name> --wait"` work?

**Short answer.** `git`, `less`, `bash fc`, `bash C-x C-e`, `zsh edit-command-line`,
`visudo`/`sudoedit`, `kubectl edit`, `gh`, and Linux (`cronie`) `crontab -e` all
accept a value with a space and arguments. **macOS `crontab -e` does not** (it
`execlp`s the whole string as a program name). The file name is always appended
as the **last argument**; most callers pass an **absolute** temp path and add
nothing else, but `less` adds `+N` *before* the file, and `visudo`/`sudoedit` add
`--` before the file. Abort-by-non-zero-exit works for git, crontab, bash `fc`,
kubectl and gh; it does **not** work for `visudo`, `sudoedit`, zsh, `less`.

## R2a. git (v2.56.0)

`git_editor()` (`editor.c:27-46`) picks the first of: `$GIT_EDITOR`, `core.editor`,
`$VISUAL` (**skipped when `$TERM` is unset or `dumb`**), `$EDITOR`, default `vi`
(`DEFAULT_EDITOR`). A variable that is *set but empty* wins and is run as an empty
command (observed `VISUAL= git commit` → `error: unable to start editor ''`). If
nothing is set and the terminal is dumb: `error: Terminal is dumb, but EDITOR
unset`. `GIT_EDITOR=:` skips the editor. Verified precedence: `GIT_EDITOR` >
`core.editor` > `VISUAL` > `EDITOR`; `TERM=dumb` ignores `VISUAL`.
For the `git rebase -i` todo list, `git_sequence_editor()` (`editor.c:48-58`)
checks `$GIT_SEQUENCE_EDITOR`, `sequence.editor`, then `git_editor()`; the
messages of `reword`/`squash` use `git_editor()`.

Invocation (`launch_specified_editor`, `editor.c:60-125`):

- argv is `[editor-string, realpath(path)]` run with `use_shell = 1`
  (`editor.c:88-93`). `prepare_shell_cmd` (`run-command.c:288-309`): if the
  editor string contains any of `` |&;<>()$`\"' \t\n*?[#~=% ``, it runs
  `sh -c '<editor> "$@"' '<editor>' <path>` (`git_shell_path()` = `/bin/sh`);
  otherwise it is executed directly via PATH. So `EDITOR="<name> --wait"` →
  `sh -c '<name> --wait "$@"' … FILE` → `<name> --wait FILE`. Observed argv:
  `[--wait] [/private/tmp/…/repo/.git/COMMIT_EDITMSG]`.
- The path is **absolute and symlink-resolved** (`strbuf_realpath`, `editor.c:88`;
  observed `/private/tmp/...` for `/tmp/...`): `<repo>/.git/COMMIT_EDITMSG`,
  `.git/rebase-merge/git-rebase-todo`, `.git/TAG_EDITMSG`, `.git/NOTES_EDITMSG`,
  `.git/config` (for `config -e`).
- **cwd**: the editor inherits git's cwd, which for work-tree commands is the
  **repository top level** (observed from `repo/sub`: `commit`, `commit
  --amend`, `tag -a`, `rebase -i`, `notes add`, `config -e` all ran with cwd =
  top level). Don't rely on it for non-work-tree contexts.
- **stdio**: not redirected; the editor inherits git's stdin/stdout/stderr, so
  they are the terminal when git is run interactively (observed with a pty:
  stdin/out/err all tty; with `>/dev/null 2>&1` stdout/stderr are not).
  Environment adds e.g. `GIT_INDEX_FILE=.git/index` for `commit`.
  Git ignores SIGINT/SIGQUIT while waiting (`editor.c:100-107`).
- **Exit status** (`editor.c:102-117`): non-zero → `error: there was a problem
  with the editor '<editor>'`, `launch_editor` returns −1. Observed: `git commit`
  → `Please supply the message using either -m or -F option.`, exit 1, **no commit
  created**; `git rebase -i` → exit 1 and **no rebase started** (no
  `.git/rebase-merge`). If exit is 0 but the message file is empty/only comments:
  `Aborting commit due to empty commit message.`, exit 1. Also git does not wait
  on anything other than the editor process exit, so a `--wait` client must stay
  alive until the file is done.
- Nothing is appended before the path (no `+N`).

## R2b. Other tools

| Tool | Variables (order) | Split / shell | Args beyond the file | Non-zero exit aborts? |
|---|---|---|---|---|
| `less` `v` | `VISUAL`, then `EDITOR` (empty `VISUAL` falls through), default `vi` (`main.c:334-345`); `LESSEDIT` overrides the template | substituted textually into the template, run as `$SHELL -c` (`command.c:1873-1902`, `lsystem.c:123-140`) | **`+N` before the file** when the line is known: default template `%E ?lm+%lm. %g` → `<ed> +12 'file'`. File is the name `less` was given (relative allowed), shell-quoted | exit status ignored |
| bash `fc` | `FCEDIT`, `EDITOR`, `vi` (`builtins/fc.def:184`) | command line `${FCEDIT:-${EDITOR:-vi}} FILE` is *parsed as shell text*, so the expansion is **word-split on IFS only, quotes inside the value are literal** (observed `EDITOR="x 'a b'"` → args `'a`, `b'`) | none; temp file `$TMPDIR/bash-fc.XXXXXX` (absolute), cwd = shell's | **yes**: non-zero → temp file removed, nothing executed (`fc.def:511-517`; observed `status=1`) |
| bash `C-x C-e` (emacs) / `v` (vi) | `fc -e "${VISUAL:-${EDITOR:-emacs}}"` (`bashline.c:937-938`; `vi` default for vi mode) — **VISUAL first** | value is expanded inside quotes, then `"%s %s" ename fn` is parsed as a **full shell command** (`fc.def:500-504`): quoting and metacharacters work (observed `"a b"` → one arg) | none | **yes**, same as `fc` |
| zsh `edit-command-line` | `zstyle :zle:edit-command-line editor`, else `${VISUAL:-${EDITOR:-vi}}`, split with `${(Q)${(z)…}}` (shell-word split, **quotes honoured, no metacharacter execution**) | no shell | by *substring match on the editor words*: `*vim*` → `-c 'normal! {byteoffset}go' -- FILE`; `*emacs*` → `+LINE:COL FILE`; else just `FILE`. `<name>` contains neither (but `nvim` would match `*vim*`) | **no**: status ignored, buffer replaced with the file's contents; `exec </dev/tty`; temp file `=(<<<…)` with suffix `.zsh` in `$TMPDIR` |
| `crontab -e` macOS (Apple cron-52) | `VISUAL`, `EDITOR` (non-NULL test only: `VISUAL=` empty fails), default `/usr/ucb/vi` | **no shell, no splitting**: `execlp(editor, editor, Filename)` (`crontab/crontab.c:465-492`) | none; cwd `/tmp`; file `/tmp/crontab.XXXXXXXXXX` | yes (`exited with status N`, crontab not installed) |
| `crontab -e` Linux (cronie) | `VISUAL`, `EDITOR`, each skipped if empty (`src/crontab.c:802-805`) | **`sh -c "<editor> <file>"`**, so spaces/quotes work (`:807-834`; upstream vixie/cron identical). *Debian's `cron` source could not be fetched (site blocked automated access), so unverified.* | none; cwd not changed in cronie | yes (`"…" exited with status N`, `:856-860`) |
| `visudo` | `SUDO_EDITOR`, `VISUAL`, `EDITOR` (non-empty), then sudoers `editor` list, **but only if sudoers `env_editor` is on** (default on, `defaults.c:529`; else must match the `editor` allowlist); `sudo`'s `env_reset` strips these unless in `env_keep` | **whitespace split with backslash escapes**, first word resolved through `PATH` (`plugins/sudoers/editor.c:126-212`) | **always `--` then FILE**; arguments may not themselves contain `--`; **`+N` only on re-edit after a syntax error and only if the editor's basename is in an allowlist** (`nvim`, `vim`, `vi`, `nano`, `*macs`, …; `<name>` is not) (`visudo.c:423-472`, `531-546`); FILE = `/etc/sudoers.tmp` | **no**: "We cannot check the editor's exit value" (`visudo.c:548-553`); only abnormal termination counts as failure (`run_command`, `911-940`). Unchanged mtime/size → "not modified" |
| `sudoedit` / `sudo -e` | same `find_editor()` (`SUDO_EDITOR`, `VISUAL`, `EDITOR`) | same split; **runs as the invoking user**, on temp copies `/var/tmp/<base>.XXXXXXXX<suffix>` (`src/sudo_edit.c:56-150`) | `-- FILE…` | **no**: copy-back is decided by mtime/size change only (`sudo_edit.c:296-306`, `741-762`) |
| `kubectl edit` | **`KUBE_EDITOR`, `EDITOR`** (no `VISUAL`) (`editoptions.go:907-912`), default `vi` | no space → bare command; spaces and no `"'\` → `strings.Split(" ")`; with quotes/backslash → `$SHELL -c '<editor> "<abs path>"'` (`editor.go:68-90`, `launcher_others.go`) | none; absolute temp file `$TMPDIR/kubectl-edit-*.yaml`; inherits stdio | yes: `there was a problem with the editor "…"`, nothing applied |
| `gh` (e.g. `pr create`) | `GH_EDITOR`, config `editor`, then `GIT_EDITOR`, `VISUAL`, `EDITOR`, default `nano` (`pkg/cmdutil/legacy.go`, `pkg/surveyext/editor.go:22-32`) | `shellquote.Split` (POSIX word-splitting with quotes, **no shell**) and `LookPath` on the first word (`editor_manual.go:75-85`) | none; absolute `$TMPDIR/survey*` file | yes: `cmd.Run()` error returned (`:98`) |

Cross-cutting consequences for `EDITOR="<name> --wait"`:

1. Anything that appends the file puts it after `--wait`; so `--wait` **must be a
   flag**, not an option that consumes a value. Callers may also insert `+N`
   (`less`) or `--` (`visudo`, `sudoedit`) between `--wait` and the file.
   `<name> --wait +12 FILE` and `<name> --wait -- FILE` must both work (and, if
   the rest goes to Neovim unchanged, `--` ends options exactly as in R1e).
2. Only macOS `crontab -e` breaks, because it does not split. Everything else
   splits on spaces (no tool here needs quoting inside `--wait`).
3. For tools that treat non-zero exit as "abort" (git, bash `fc`/C-x C-e,
   crontab, kubectl, gh) `<name> --wait` needs a way to exit non-zero (e.g. `:cq`
   in the pane). For visudo/sudoedit/zsh/less the contents on disk decide and
   exit status is ignored.
4. Temp files are absolute and often outside any repo (`$TMPDIR`, `/tmp`,
   `/var/tmp`); cwd differs per tool (git: repo top level; less/bash/zsh: the
   user's cwd; Apple crontab: `/tmp`; sudoedit: caller's).
5. The caller waits on process exit with its stdio inherited (git, kubectl, gh,
   zsh) — the client must stay in the foreground on the same tty (zsh reopens
   `/dev/tty`).
6. Observed in this environment: `$EDITOR` is already `neorex` (and `git var
   GIT_EDITOR` prints `neorex`).

Local macOS check (`crontab -e` with a no-op logging script as editor):

```
EDITOR="…/ed_noop.sh --wait"  → crontab: …/ed_noop.sh --wait: No such file or directory
                                 crontab: "…/ed_noop.sh --wait" exited with status 1
EDITOR="…/ed_noop.sh"          → ARGS: /tmp/crontab.XSpQiNw6WF pwd=/private/tmp ; "no changes made to crontab"
```

macOS crontab also checks that the *same inode* is still at the temp path
(`crontab.c:458-459`, `527-528`: "temp file must be edited in place"). Neovim
default (`backupcopy=auto`, `writebackup`) kept the inode on a plain write here
(`ls -i` unchanged), but any edit path that replaces the file would fail.

## R2c. nvr (neovim-remote) and `code --wait`

**nvr `--remote-wait`** (`nvr/nvr.py`, v2.5.1). "Done" = the **buffer is deleted**:

- For every file opened with a wait option nvr runs
  `autocmd BufDelete <buffer> silent! call rpcnotify(chan, "BufDelete")` (`:114`),
  counts buffers (`:124`), and stops waiting when all notified (`:567-571`).
  `BufDelete` fires on `:bdelete`/`:bwipeout` or when a buffer with
  `'bufhidden'` = `delete`/`wipe` is hidden — **not** on `:q` of a window that
  leaves the buffer loaded and hidden, not on `:w`. Hence the README advice
  (`README.md:183-210`): `:w | bd`, or
  `autocmd FileType gitcommit,gitrebase,gitconfig set bufhidden=delete` so `:wq`/`:x`
  completes, and `nvr --remote-tab-wait +'set bufhidden=delete' file`
  (`nvr.py:200`; `README.md:92`).
- Abort: `autocmd VimLeave * if v:exiting > 0 | rpcnotify(chan, "Exit", v:exiting)`
  (`:115`, `:572-574`): nvr exits with the nvim server's exit code **only when the
  whole Neovim exits** (`:cquit`), which also kills the server. `README.md:273`:
  "`:cquit` in the linked nvim will make nvr return with 1". A waiting client can
  be released individually by `rpcnotify(client, 'Exit', 1)` for channels in
  `b:nvr` (`README.md:276-283`).
- Each file is an `:edit`/`:tabedit` in the existing instance (`nvr.py:126-146`);
  `--remote-wait` takes `nargs='*'` files (`:221-224`), i.e. the wait option
  swallows the file arguments that git appends.

**VS Code `code --wait`**: CLI doc (https://code.visualstudio.com/docs/configure/command-line):
"`-w` or `--wait`: Wait for the files to be closed before returning." Source
(`src/vs/workbench/services/host/browser/browserHostService.ts`, `--wait` handling;
`src/vs/workbench/browser/editor.ts:176` `whenEditorClosed`): the CLI passes a
wait-marker file; the workbench waits until **editors for those resources are
closed** (`onDidCloseEditor`, ignoring `MOVE` and `REPLACE` closes), then deletes
the marker file. So "done" = the **editor tab(s) closed** (saving is irrelevant;
no abort/exit-code channel). `--wait` forces reuse of the same window
(`if (options.waitMarkerFileURI) return true; // always handle --wait in same
window`).

---

# R3. Does Neovim 0.12 report a `:terminal` job's pty path?

**Short answer.** Yes for any pty-backed job (`:terminal` and `jobstart(…,
{pty=true})`) on Unix; the `pty` key is the slave device path and matched the
path printed by `tty` inside the job (checked on macOS with three concurrent
jobs). Non-pty jobs have no `pty` key. After the job exits the key is still
present but **empty (`""`)**.

## R3a. Source and docs

- `nvim_get_chan_info()` docs (`runtime/doc/api.txt:848-850`; also
  `src/nvim/api/vim.c:1783`): `"pty" (optional) Name of pseudoterminal. On a
  POSIX system this is a device path like "/dev/pts/1". If unknown, the key will
  still be present if a pty is used (e.g. for conpty on Windows).`
- `channel_info()` (`src/nvim/channel.c:1004-1020`): for `kChannelStreamProc`,
  `if (chan->stream.proc.type == kProcTypePty) PUT_C(info, "pty",
  pty_proc_tty_name(&chan->stream.pty))`. `nvim_list_chans()` returns the same
  dicts.
- Unix: `pty_proc_tty_name` is `ptsname(ptyproc->tty_fd)` where `tty_fd` is the
  master (`src/nvim/os/pty_proc_unix.c:236-239`) → `/dev/ttysNNN` on macOS,
  `/dev/pts/N` on Linux (same code; Linux not run here). After the master is
  closed `ptsname` returns NULL → `""` (observed). Windows: the literal `"?"`
  (`pty_proc_win.c:171-174`).
- `:terminal` jobs are pty jobs with `mode = "terminal"`, `buf`/`buffer` and an
  `exitcode` (−1 while running); plain `jobstart(pty=true)` jobs have
  `mode = "bytes"`.

## R3b. Local test (macOS, nvim 0.12.6)

`nvim --headless --listen /tmp/cli-facts-scratch/n.sock`, then via
`nvim --server … --remote-expr`:

```
:terminal tty > tty.txt; …               → tty.txt = /dev/ttys000
:terminal tty > tty2.txt; …              → /dev/ttys004
jobstart(["sh","-c","tty > tty3.txt…"], {"pty": v:true})   → /dev/ttys006
jobstart(["sh","-c","tty > tty4.txt…"])  → "not a tty"      (no pty)

json_encode(nvim_list_chans()) (trimmed):
 {"id":4,"mode":"terminal","stream":"job","pty":"/dev/ttys000","buf":1,…}
 {"id":7,"mode":"terminal","stream":"job","pty":"/dev/ttys004",…}
 {"id":9,"mode":"bytes",   "stream":"job","pty":"/dev/ttys006",…}   # jobstart pty=true
 {"id":11,"mode":"bytes",  "stream":"job"}                           # no "pty" key
 {"id":12,"mode":"rpc","stream":"socket","client":{}}
```

Same path every time, including `jobstart(…, {pty:true})`. `nvim_get_chan_info(11).pty`
→ `E716: Key not present` for the non-pty job. After a terminal job exited:
`{"id":4,"pty":"","exitcode":0}` — so match on a non-empty `pty` and keep in mind
that device numbers are reused after a job ends.

## R3c. Can a process inside `:terminal` tell it is inside Neovim?

Environment for jobs is built in `create_environment()` (`src/nvim/eval/funcs.c:3391-3453`);
observed `env` of a `:terminal` job (headless server with `--listen`):

| Var | Value in `:terminal` | Source |
|---|---|---|
| `$NVIM` | **`v:servername`** (the primary listen address; the `--listen` address as given) — set for **every** job, pty or not, and for `:!` children via inheritance | `funcs.c:3432-3442` |
| `$NVIM_LISTEN_ADDRESS` | **not set** (unset at startup so it is never leaked: "must not be leaked to child jobs or :terminal") | `msgpack_rpc/server.c:75-80` |
| `$TERM` | `xterm-256color` for `:terminal` (`funcs.c:3612`); `ansi` for `jobstart(pty=true)` unless `env.TERM`/`TERM` opt given (`3623-3625`); non-pty jobs inherit | observed `xterm-256color` / `ansi` / `dumb` with `env={TERM="dumb"}` |
| `$COLORTERM` | removed, then `truecolor` only if `'termguicolors'` is set (pty jobs) | `funcs.c:3355-3367`, `3413-3416` |
| `$VIM`, `$VIMRUNTIME`, `$COLUMNS`, `$LINES`, `$TERMCAP`, `$COLORFGBG` | removed for pty jobs | `funcs.c:3355-3367` |
| inherited from the server | `$MYVIMRC`, `$NVIM_LOG_FILE` and the rest of the server's environment (observed both in the job) | — |
| `$TERM_PROGRAM` | not set by Neovim (the observed value was the host terminal's) | `tui/tui.c` only reads it |

Beyond `$NVIM`: there is no dedicated "inside `:terminal`" variable (no
`$NVIM_TERMINAL`-style marker). `$NVIM` alone only says "descendant of a
Neovim process" (also true for non-pty jobs and `:!`). The reliable test is
RPC: connect to `$NVIM`, call `nvim_list_chans()` and look for a channel with
`mode == "terminal"` whose `pty` equals the caller's `ttyname(0)` (R3b); the
`buf` field then identifies the buffer. A weaker hint is `TERM=xterm-256color`
plus a pty on stdio. Note `main.c:1002-1006`: `nvim --remote-ui` refuses to
attach when `--server` equals `$NVIM` ("Cannot attach UI of :terminal child to its
parent. (Unset $NVIM to skip this check)"), which matters if a pane process is
ever started from a `:terminal` in the daemon's own Neovim.
