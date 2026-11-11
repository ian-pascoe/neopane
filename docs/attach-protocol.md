# The attach protocol

How a helper (`<name> attach`) and a daemon talk: the frames, the hello and
its answers, the reasons an attach is refused or ends, and the attach
command. Decided in #91 and #94, recorded in ADR 0013. `<name>` and
`<NAME>` stand for the tool's name, lower and upper case. The code is
`src/attach/`; a test checks every field and reason name it sends appears
here.

## Transports

The helper names its transport in the hello; the daemon accepts it or
refuses it with `transport`, and nothing retries.

- `fd`: the terminal is handed over with SCM_RIGHTS on the hello's first
  byte, and the daemon reads and writes it directly. The default on Unix,
  and always Rex's.
- `relay`: the helper copies the terminal's bytes in `i` and `o` frames. The
  only transport on Windows. On Unix it is a diagnostic, chosen with
  `<NAME>_TRANSPORT=relay` or `--transport relay`, and never automatically.

A Unix daemon takes both, attach by attach. Hello, answer, size, end and
restore are the same under both. Sizes always travel as messages: the daemon
never asks a terminal its size.

## Frames

Every message is a frame:

| Bytes | Meaning |
|---|---|
| 0 | the kind, one ASCII letter |
| 1–4 | the payload's length, a big-endian `u32` |
| 5… | the payload |

Control payloads are msgpack maps with `str` keys. Operating-system strings
(environment, `cwd`, `tty`, `file`, paths) are `bin`: raw bytes on Unix,
WTF-8 on Windows. `i` and `o` payloads are raw bytes. A key given twice
keeps its first value.

| Kind | Direction | Payload |
|---|---|---|
| `h` | helper → daemon | the hello (a map); first, and once. Under `fd` the descriptor rides with its first byte |
| `s` | helper → daemon | `{cols, rows, px?}`: the terminal's new size |
| `i` | helper → daemon | input bytes (relay only) |
| `a` | daemon → helper | `{v, build, daemon, log, features}`: the hello is accepted |
| `u` | daemon → helper | `{undo}`: the bytes that undo the daemon's output, at most 4 KiB |
| `o` | daemon → helper | output bytes (relay only) |
| `r` | daemon → helper | the reply to a control request |
| `z` | daemon → helper | `{v, build, reason, message, cut?, exit?}`; the last frame: a refusal before `a`, an ending after it |

### Limits

- An `h` payload holds at most 4 MiB; every other frame's, at most 64 KiB.
  Over the limit is a protocol error, found as soon as the header arrives. A
  hello that won't fit is a local "environment too large" error.
- Senders split `i` and `o` into frames within the limit; the backlog drops
  whole frames.
- `u`'s `undo` holds at most 4 KiB.

### Sizes

`size` in the hello and every `s` frame: `cols` and `rows` (`u16`), and `px`,
`[width, height]` of the text area in pixels, only when non-zero. The helper
reads `TIOCGWINSZ` after each batch of SIGWINCH and sends only changes.

## The hello

A missing required field is refused as `hello`, its message naming the
field. A field that's there but nil counts as missing.

| Field | Type | `requested` | `self` | `status` / `stop` | `open` |
|---|---|---|---|---|---|
| `v` | int: the attach protocol version | required | required | required | required |
| `build` | str: the `--version` text | required | required | required | required |
| `purpose` | `requested` / `self` / `status` / `stop` / `open` | required | required | required | required |
| `transport` | `fd` / `relay` | required | required | — | — |
| `pane` | str: a pane id | required | — | — | — |
| `size` | `{cols, rows, px?}` | required | required | — | — |
| `pid` | int: the helper's pid | required | required | optional | optional |
| `tty` | bin; empty where the platform has none | required | required | — | — |
| `env` | map bin → bin: the whole environment, first occurrence wins | required | required | — | — |
| `cwd` | bin | required | required | — | required |
| `provider` | str: `--provider`, else `$<NAME>_PROVIDER` | — | optional | — | — |
| `file` | bin: made absolute against `cwd`, links not resolved | — | optional | — | required |
| `line` | int ≥ 1: the cursor's line in `file`; only with `file` | — | optional | — | optional |
| `force` | bool, default false; `stop` only | — | — | optional | — |
| `wait` | bool, default false: answer once `file` is done | — | — | — | optional |

- `requested`: a requested pane's terminal, one the daemon asked the
  terminal provider for, for a window or a float group.
- `self`: a terminal attaching of its own accord. It carries its whole
  environment, working directory, tty, pid, any provider hint and any file.
- `status`, `stop` and `open` are control requests (below).

`TERM` and terminal names travel in `env`. The helper never sends or writes
a title. The hello carries no credentials.

## Answers

The daemon answers every hello. Nothing it answers waits on Neovim, the
config or a pane provider: a self-attach is accepted first and its pane
provider chosen after. It gives a hello 2 s, without blocking its loop; the
helper gives the answer 10 s.

- **Accepted:** `a`, with `v`, `build`, `daemon` (its name), `log` (its log's
  path, bin) and `features` (the new messages needing an answer this daemon
  understands, as str). `a` and `u` go in one write, before the daemon sets
  any mode, and a new `u` whenever the undo bytes change. Between `a` and the
  first output the helper draws nothing.
- **Refused:** `z` before any `a`, with a refusal reason and a `message` of
  one human-readable line the daemon composes.
- **A control request:** one `r`, then the daemon closes.

### The undo bytes

`u`'s `undo` (bin) undoes what the daemon writes: today `CSI ?2026l`, the
editor modes off, then, once a pane title is written, `ESC ] 2 ; ESC \` and
`CSI 23;2 t`. They must close any state an OSC the daemon writes can leave
open (`OSC 8 ; ; ST` if it ever writes OSC 8).

### Ending

At a normal end the daemon drops the frames it hasn't started writing.
Under `relay` it finishes the `o` in flight, giving the connection at
most 1 s, else it closes without `z`; under `fd` it writes nothing more and
closes its copy of the terminal. It sends `z` and closes:

- `reason`: an ending reason.
- `message`: one line.
- `cut`: true if the daemon cut a frame short. The helper then writes CAN
  and `ESC \` (`18 1B 5C`) before the undo bytes.
- `exit`: int 0–255, Neovim's exit status, only with `quit`. A self-attach
  exits with it.

The helper writes the undo bytes, blocking in its own process, then
restores its terminal: it drops input not yet read (`tcflush(TCIFLUSH)`),
and puts back the settings and file status flags it saved. If the daemon
goes without `z`, the helper writes `18 1B 5C` and the latest undo bytes,
restores, prints a line naming the log, and exits 1. Once a helper hangs
up, the daemon writes nothing more to its terminal.

### The terminal's settings

The helper owns them, under both transports; the daemon never sets them.
Before its hello it saves the terminal's settings and fd 0's file status
flags (`F_GETFL`), then sets raw mode (`cfmakeraw`, `tcsetattr(TCSANOW)`),
keeping what was typed for the daemon to read. It restores both on every way
out once its hello is sent, after its 300 ms minimum: a reply to a query the
daemon wrote has had time to arrive by then, and is dropped with the rest of
the input.

## Reasons

Refusal reasons, in `z` before `a`:

| Reason | When |
|---|---|
| `version` | another attach protocol version; the message names both builds and both versions |
| `hello` | a hello that can't be read, or lacks a field |
| `transport` | a transport the daemon doesn't take |
| `user` | the peer runs as another user |
| `terminal` | under `fd`, the handed-over file isn't a terminal |
| `nested` | from the daemon's own Neovim: `env` has `<NAME>` equal to the daemon's address, and the tty is one of that Neovim's `:terminal`s, or it has none |
| `stopping` | ending Neovim has begun (an attach during ADR 0003's wait keeps the daemon) |
| `pane-unknown` | a pane id the daemon never minted |
| `pane-expired` | a pane id no longer good for an attach |
| `pane-taken` | a pane id an attach already took |

Ending reasons, in `z` after `a`:

| Reason | When |
|---|---|
| `closed` | the pane closed |
| `quit` | Neovim quit |
| `stopped` | the daemon was stopped |
| `failed` | anything else that ends the attach |
| `detached` | reserved |

An unknown ending reason counts as `failed`; an unknown refusal is a failure
too.

## Exit statuses

| End | Status | Prints | Waits for a key |
|---|---|---|---|
| `closed` / `stopped` / `detached` | 0 | nothing | no |
| `quit` | a self-attach: Neovim's own status (`exit`); a requested pane: 0 | nothing | no |
| a requested attach refused `pane-expired` or `stopping` | 0 | nothing | no |
| any other refusal, `failed`, daemon gone, no answer in 10 s, can't connect, not a terminal | 1 | `<name>: <message>` on stderr | a self-attach only, unless a shell started it as a job (its parent in the same session, another process group); on Windows, when it is the console's only process; never when stdin isn't a terminal |
| usage | 2 | usage | no |

The helper never exits within 300 ms of starting, except by a signal.
Stopped by SIGTERM, SIGINT or SIGQUIT, it closes the connection, writes
`18 1B 5C` and the undo bytes, restores, and exits 1 quietly.

## Versions and extension

- `ATTACH_PROTOCOL_VERSION` is 1. It is separate from `<NAME>_PROTOCOL`, the
  pane provider interface's, and is matched exactly right after the peer
  check, except by `status` and `stop`.
- Unknown keys are ignored. Unknown kinds after the hello are skipped.
- A new message that needs an answer is listed in `a`'s `features`.
- The version changes only when an existing message or field changes
  meaning. New optional keys and values need no bump.

### Frozen

These never change, so any two versions can tell each other apart, and a
newer `<name>` can always list and stop an older daemon:

- the frame layout;
- `h`'s `v`, `build` and `purpose`;
- `z`'s `v`, `build`, `reason` and `message`;
- the `status` and `stop` requests, `stop`'s `force` included;
- `r`'s `ok`, `message`, `v`, `build`, `name` and `pid`.

## Control requests

`status`, `stop` and `open` use the same daemon address, frames, hello and
peer check, with no terminal and no 300 ms minimum. The daemon answers with one
`r` and closes:

| Field | Type | Meaning |
|---|---|---|
| `ok` | bool | the request succeeded |
| `message` | str, optional | one line, on failure |
| `v` | int | the daemon's attach protocol version |
| `build` | str | the daemon's build |
| `name` | str | the daemon's name |
| `pid` | int | the daemon's pid |

- `status` works across versions. While versions agree it adds `address`
  (bin), `panes` (int: attached Neovim panes), `unsaved` ([bin]: files with
  unsaved changes, empty for an unnamed buffer) and `provider` (str), and the
  optional fields below. Status never waits on Neovim.
- `stop` works across versions. Without `force`, a daemon with unsaved
  changes answers `ok: false` with a `message` naming them and keeps
  running; with it, it ends, the changes kept in swap files.
- `open` needs a version match, `cwd` and `file`. Its reply waits on Neovim
  without blocking the loop; with `wait`, until the file is done.

## The peer check

Both ends check the other runs as the same user before the hello. On Unix,
the uid, through `SO_PEERCRED` or `getpeereid`. On Windows, a pipe with an
owner-only DACL, `PIPE_REJECT_REMOTE_CLIENTS` and
`FILE_FLAG_FIRST_PIPE_INSTANCE`, and the peer's SID, through
`GetNamedPipeClientProcessId` or `GetNamedPipeServerProcessId`. The token in
a pane id is no secret.

## Pane ids

Every Neovim pane has a pane id: a number and a token joined by `-`, such as
`12-k3j9x2qpa8`, matching `^(0|[1-9][0-9]*)-[a-z0-9]{10}$` with the number
fitting a `u32`. Every character is bare-word safe.

- Numbers start at 1 and are never reused within a daemon. The token comes
  from the OS random source (`getentropy` or `getrandom`; `ProcessPrng` on
  Windows).
- A requested pane's id is minted before the pane provider command runs; a
  self-attached pane's on accept. The pane title is `<name>-<token>`.
- A requested id is good for one attach, from minting until the first of: an
  attach takes it; the attach timeout (5 s from `open` finishing) runs out;
  nothing wants the pane any more; pending requests are cancelled as the
  daemon goes unattached; the daemon ends.
- The daemon remembers every id it minted, as minted, taken or expired, to
  refuse an attach naming one with `pane-unknown`, `pane-taken` or
  `pane-expired`.

## The attach command

A requested pane's terminal runs `{attach}`:

```
[<daemon's canonical exe>, "attach", "--address", <address>, "--pane", <pane id>]
```

with `--transport relay` appended when the daemon was started under
`<NAME>_TRANSPORT=relay`. Pane provider commands get it two more ways:

- `<NAME>_ATTACH`: the same words joined by spaces, each POSIX-quoted only
  when it holds a byte outside `[A-Za-z0-9_@%+=:,./-]`: in single quotes,
  each `'` written `'\''`, an empty word as `''`. It works typed into a shell
  as `exec …`.
- `<NAME>_ATTACH_JSON`: the words as a compact JSON array; unset when a word
  isn't UTF-8.

`--pane` needs `--address`, and excludes `FILE`, `+LINE`, `--provider` and
`--name`. A self-attach is `attach [--address A | --name N] [--provider P]
[+LINE] [FILE]`: the same helper and hello. Windows addresses are
`//./pipe/<name>`, with no `/` in the name, compared case-insensitively with
`/` and `\` equal.

## The build string

`build`, and `--version`'s first line: `<name> <Cargo version>`, plus
` (<short commit>[-dirty], <commit date>)` when built from git, as in
`<name> 0.1.0 (a1b2c3d, 2026-11-09)`. `build.rs` reads the commit; `-dirty`
means tracked files differ from it.

## #94's protocol additions

| Where | Field | Type and meaning | Optional | Frozen |
|---|---|---|---|---|
| `z` | `exit` | int 0–255: Neovim's exit status, only with `reason: quit`; a self-attach exits with it | yes | no |
| `h`, purpose `stop` | `force` | bool, default false. Without it, a daemon with unsaved changes answers `r{ok: false, message}` naming them and keeps running; with it, it ends, changes in swap | yes | **yes**, with `stop` |
| `h`, purposes `self`, `open` | `line` | int ≥ 1: put the cursor on this line of `file` | yes | no |
| `h`, purpose `open` | `wait` | bool, default false: answer only once `file` is done | yes | no |
| `r` to `open` | `waited` | bool, true on the answer to a waited open; `ok: false` with it means the terminal closed with unsaved changes or the daemon was stopped | yes | no |
| `r` to `status` (versions agree) | `dir` | bin: Neovim's global working directory | yes | no |
| | `kept` | bool: started with `--detach` | yes | no |
| | `capabilities` | [str]: present, snake_case | yes | no |
| | `unsupported` | [str]: found unsupported since the choice | yes | no |
| | `log` | bin: the log's path | yes | no |
| | `terminals` | [bin]: ptys of Neovim's running `:terminal` jobs | yes | no |
| | `provider` (existing) | values: shown name, `(choosing)`, `none`, `none (config error)`, `none (find refused)`, `none (hint none)` | — | no |
| `{attach}` | `--transport relay` | appended under the diagnostic | — | — |

None needs a version bump: each is a new optional key or value.
