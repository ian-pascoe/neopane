# What attaching needs on Windows

Facts gathered while designing #82 (the attach design must not shut Windows
out). Today a pane helper passes its tty fd to the daemon over a Unix socket
(SCM_RIGHTS) and the daemon draws straight into it. Checked against neorex
066c0da (`src/daemon/pane_helper.rs`, `fdpass.rs`, `src/term/terminal.rs`),
Neovim master 893d90f (2026-11-05, 0.13.0-dev per `CMakeLists.txt:150-153`),
libuv 716390f (2026-11-02), and the Microsoft Learn pages cited below
(fetched in this session). Not tested on Windows: no Windows machine was used.

## What the daemon does with the tty fd today

- The helper sends the hello with its stdin as the tty:
  `fdpass::send(&stream, hello, tty)` (`pane_helper.rs:95`). Only a `Hello`
  (`p`) and a `Resized` (`w`) byte exist, helper to daemon
  (`pane_helper.rs:55-62`); nothing is sent daemon to helper, and the helper
  treats a hang-up as the only message (`pane_helper.rs:127-137`).
- The daemon, not the helper, then does all of this to the fd:
  - raw mode on attach (`tcgetattr`, `cfmakeraw`, `tcsetattr`; `terminal.rs:34-44`)
    and restore at the end (`terminal.rs:51-68`, `surface.rs:20` waits up to 1 s);
  - size by `ioctl(TIOCGWINSZ)` (`terminal.rs:71-79`), on each helper `Resized`
    byte (`daemon.rs:539-540`) and at attach (`surface.rs:71`);
  - frame output by `write` (`terminal.rs:94`), non-blocking with a backlog,
    by flipping `O_NONBLOCK` on the shared open file for the duration of the
    write (`backlog.rs:150-192`);
  - keyboard input by reading the fd (`daemon.rs:558`, `on_input`).
- Peer check on accept: `SO_PEERCRED` / `getpeereid` uid compare
  (`pane_helper.rs:172-229`).
- The daemon reaches Neovim over `UnixStream::connect(path)` to its
  `--listen` socket (`src/nvim/nvim.rs:131`).

## Can a console handle be handed to another process?

- No. `DuplicateHandle` lists "Console input" and "Console screen buffer"
  handles and says "Console handles can be duplicated for use only in the same
  process"
  ([DuplicateHandle, Remarks](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle)).
- Console Handles says the same and contrasts it with files, pipes and mutexes:
  "a process can create a duplicate console handle only for its own use. ...
  Access to a console must be shared during creation of the other process or
  may be requested by the other process through the AttachConsole mechanism"
  ([Console Handles](https://learn.microsoft.com/en-us/windows/console/console-handles)).
- `AttachConsole(pid)` attaches the caller to another process's console, but
  "A process can be attached to at most one console"; a second call fails with
  `ERROR_ACCESS_DENIED`
  ([AttachConsole, Remarks](https://learn.microsoft.com/en-us/windows/console/attachconsole)).
  A daemon serving many panes (many consoles) cannot attach to each.
- A ConPTY has no handle for a helper to pass either. The terminal emulator
  creates it with two ordinary pipe handles plus an `HPCON`
  (`CreatePseudoConsole(size, hInput, hOutput, flags, &hPC)`;
  [CreatePseudoConsole](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole),
  [Creating a Pseudoconsole session](https://learn.microsoft.com/en-us/windows/console/creating-a-pseudoconsole-session));
  the hosted child gets only a console (its `CONIN$`/`CONOUT$`). Pipe handles
  are duplicable between processes (DuplicateHandle's list), but the helper
  never holds them; the emulator does.
- Minimum ConPTY: Windows 10 1809 / Server 2019
  ([CreatePseudoConsole, Requirements](https://learn.microsoft.com/en-us/windows/console/createpseudoconsole)).
- `DuplicateHandle` can duplicate pipe, file, event and process handles into
  another process, but the sender needs `PROCESS_DUP_HANDLE` on the target and
  must tell it the new handle value by other IPC ("the source process must use
  interprocess communication to pass the value of the duplicate handle")
  ([DuplicateHandle, Remarks](https://learn.microsoft.com/en-us/windows/win32/api/handleapi/nf-handleapi-duplicatehandle)).
  This is a different model from SCM_RIGHTS: the kernel does not deliver the
  handle with a message.

## AF_UNIX and named pipes as the helper link

- Windows has AF_UNIX stream sockets since Insider build 17063, but no
  "Ancillary data: ... passing file descriptors (SCM_RIGHTS) or credentials
  (SCM_CREDENTIALS)"; also no `SOCK_DGRAM`/`SOCK_SEQPACKET`, no autobind and
  no `socketpair`
  ([AF_UNIX comes to Windows](https://devblogs.microsoft.com/commandline/af_unix-comes-to-windows/)).
  So `fdpass.rs` has no Windows equivalent even over AF_UNIX.
- The same post says pathname sockets are secured by file permissions on the
  socket file, enforced on Windows too. Whether Windows exposes the peer's
  uid/SID on an AF_UNIX socket (the `SO_PEERCRED` role): unconfirmed, not
  in that post.
- Named pipes: `CreateNamedPipe` takes an optional security descriptor; with
  NULL "The ACLs in the default security descriptor for a named pipe grant
  full control to the LocalSystem account, administrators, and the creator
  owner. They also grant read access to members of the Everyone group and the
  anonymous account"
  ([Named Pipe Security and Access Rights](https://learn.microsoft.com/en-us/windows/win32/ipc/named-pipe-security-and-access-rights)).
  A per-user pipe needs an explicit DACL.
- `PIPE_REJECT_REMOTE_CLIENTS` refuses network clients
  ([CreateNamedPipe](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-createnamedpipea)).
  `GetNamedPipeClientProcessId` gives the client's pid
  ([GetNamedPipeClientProcessId](https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeclientprocessid)).
  Together with the DACL these replace `peer_uid`.
- Pipes are bidirectional (`PIPE_ACCESS_DUPLEX`) and byte-oriented
  (CreateNamedPipe). Nothing in them carries a handle; handles travel by
  DuplicateHandle + a message.

## Reading input and size from a console

- Input: with `ENABLE_VIRTUAL_TERMINAL_INPUT` set on the input handle the
  console host emits VT sequences for keys on the input stream; it is meant to
  be paired with `ENABLE_VIRTUAL_TERMINAL_PROCESSING` on the output handle for
  "an application that communicates exclusively via virtual terminal
  sequences". Both are off by default
  ([SetConsoleMode](https://learn.microsoft.com/en-us/windows/console/setconsolemode),
  [Console Virtual Terminal Sequences, Input Sequences](https://learn.microsoft.com/en-us/windows/console/console-virtual-terminal-sequences)).
  A helper can set them, read bytes with `ReadFile`, and relay them.
- Size: there is no SIGWINCH. With `ENABLE_WINDOW_INPUT` set, resizes arrive
  in the input buffer as `WINDOW_BUFFER_SIZE_EVENT` records
  (`WINDOW_BUFFER_SIZE_RECORD { COORD dwSize }`: "Buffer size events are placed
  in the input buffer when the console is in window-aware mode
  (ENABLE_WINDOW_INPUT)"; [WINDOW_BUFFER_SIZE_RECORD](https://learn.microsoft.com/en-us/windows/console/window-buffer-size-record-str)).
  Reading them needs `ReadConsoleInput`, which returns `INPUT_RECORD`s, not
  VT bytes ([Reading Input Buffer Events](https://learn.microsoft.com/en-us/windows/console/reading-input-buffer-events)).
  With VT input and window input both on, whether the same `ReadFile` stream
  yields the size records: unconfirmed.
- libuv, which Neovim uses, does exactly this: `UV_TTY_MODE_RAW_VT` sets
  `ENABLE_VIRTUAL_TERMINAL_INPUT` and also `ENABLE_WINDOW_INPUT`
  (`libuv/src/win/tty.c:385-393`), and on a `WINDOW_BUFFER_SIZE_EVENT` it
  recomputes the size with `GetConsoleScreenBufferInfo` and dispatches
  `SIGWINCH` if it changed (`tty.c:781-784`, `uv__tty_console_signal_resize` at
  `tty.c:2444`). It also subscribes to `EVENT_CONSOLE_LAYOUT` via
  `SetWinEventHook` (`tty.c:2380-2410`) and notes that under "some TTY emulator
  that does not send those events" the input record is the fallback
  (`tty.c:781-783`).
- Current size: `GetConsoleScreenBufferInfo`; "Window and buffer size will
  match in a pseudoconsole session"
  ([Classic console API vs virtual terminal](https://learn.microsoft.com/en-us/windows/console/classic-vs-vt)).
  Neovim's TUI asks via `uv_tty_get_winsize` (`src/nvim/tui/tui.c:2017`) and
  registers `SIGWINCH` through libuv (`tui.c:180`).
- The terminal end resizes a ConPTY with `ResizePseudoConsole`; it "can resize the internal
  buffers" so hosted programs see the new dimensions
  ([ResizePseudoConsole](https://learn.microsoft.com/en-us/windows/console/resizepseudoconsole)).
- Pseudoconsole output is a translation, not a pass-through: "This
  translation is not perfect. It requires the console host window to maintain a
  simulated environment ... Only the effects are propagated onto the final
  host" ([Classic console API vs virtual terminal](https://learn.microsoft.com/en-us/windows/console/classic-vs-vt)).
  Whether every sequence neorex emits (synchronized output, undercurl, kitty
  keyboard) reaches the emulator unchanged through conhost with VT processing
  on: unconfirmed.

## Neovim's RPC and `--listen` on Windows

- `--listen {addr}` starts the RPC server "on pipe or TCP address"
  (`runtime/doc/starting.txt:437-439`); RPC channels can be opened by processes
  connecting "to TCP/IP sockets or named pipes listened to by Nvim", and named
  pipes are called safer than localhost TCP (`runtime/doc/api.txt:45-56`).
- A generated address on Windows is `//./pipe/<name>.<pid>.<counter>`
  (`src/nvim/msgpack_rpc/server.c:113-125`); elsewhere it is a path under
  `$XDG_RUNTIME_DIR`. A bare name is expanded into that form
  (`server.c:155-175`). A client address with `:` is TCP; otherwise a pipe.
- Implementation is libuv: `uv_pipe_bind2` for the listener
  (`src/nvim/event/socket.c:172`), `uv_pipe_connect` for the client
  (`socket.c:337`). libuv's Unix-socket-or-named-pipe type is the same
  `uv_pipe_t` on both OSes, so Neovim's wire protocol (msgpack-RPC) is
  identical; only the address differs.
- neorex's `UnixStream::connect(path)` (`nvim.rs:131`) would become a
  named-pipe client on Windows. The path is an opaque string to Neovim, so a
  `//./pipe/...` address works wherever `--listen` accepts it.
- Neovim's own TUI is the relay pattern: the TUI process writes to its stdout
  as a libuv tty when it is a console, or opens it as a pipe otherwise
  (`tui.c:612-640`), and talks to the server over RPC. Neovim does not pass
  tty handles anywhere; it also keeps console code for the server side
  (`src/nvim/os/os_win_console.c`: `CONIN$`/`CONOUT$`, hidden console swap).

## What a byte-relay costs against fd passing

- Extra process: none. A helper process already exists per pane and already
  sits in a poll loop for SIGWINCH and hang-up (`pane_helper.rs:99-137`).
  Under relay it also copies bytes.
- Extra work per frame: one more copy and a helper wake-up on output
  (daemon -> pipe -> helper -> console `WriteFile`), and the same on input
  (console `ReadFile` -> helper -> pipe -> daemon). With fd passing the daemon
  writes the tty directly and the helper sleeps.
- Backpressure moves: today a slow terminal blocks the daemon's `write` and the
  daemon keeps a per-pane backlog (`backlog.rs`). Under relay the pipe buffer
  fills first and the helper's console write is what blocks; the daemon's
  backlog still works if it writes the pipe non-blocking. The daemon can no
  longer see "terminal took it all" (`Sent::All`, `surface.rs:36-41`) directly,
  only "the pipe took it all".
- On Windows the bytes go through conhost/ConPTY on the way out regardless of
  how the daemon reaches them, so the relay is one hop on top of a path that
  already has several.
- Latency numbers: unconfirmed. No primary measurement of pipe-versus-direct
  console write latency was found, and a local macOS micro-benchmark attempt
  (Unix socket relay into a pty) was abandoned unfinished, so none is claimed.

## What this means for the pane provider interface

The pane provider interface is not affected: it only says how panes come into
being and where they go. The constraint is on the attach protocol between a
pane's helper and the daemon, and fd passing cannot be the only transport:
Windows can neither dup a console handle into another process nor send handles
with a message on AF_UNIX or a named pipe. What the attach protocol must allow
so a relay transport can be added later:

- **The terminal fd is one transport, not the protocol.** The hello names its
  transport (`fd` or `relay`) in-band, and carries nothing that needs
  ancillary data; fd passing stays the Unix fast path. Relay also works on Unix
  (ssh, a pane in a container), so it is not Windows-only.
- **Messages are framed and go both ways.** Today a message is one byte,
  helper to daemon, and the daemon to helper direction carries nothing. Relay
  needs typed, length-framed messages for output bytes (daemon -> helper),
  input bytes (helper -> daemon), size, mode and close.
- **Size travels in the message, not by `ioctl`.** The hello and each
  `Resized` carry cols and rows (and pixel size if used); the daemon stops
  calling `TIOCGWINSZ` itself. On Windows the helper gets them from
  `WINDOW_BUFFER_SIZE_EVENT` + `GetConsoleScreenBufferInfo`.
- **Terminal mode is the helper's job, asked for by the daemon.** Raw mode,
  restore and flush-then-restore (`Restore::Waiting`) become requests the
  helper performs (termios on Unix; `SetConsoleMode` with VT input, VT
  processing and window input on Windows) and acknowledges, rather than
  `tcsetattr` calls from the daemon.
- **Backpressure is the transport's, not the fd's.** The daemon's per-pane
  backlog must not rely on flipping `O_NONBLOCK` on a shared open file
  (`backlog.rs`); it writes a non-blocking stream and learns how much it took.
- **Peer authentication is a transport hook.** Unix: uid compare. Windows: a
  pipe with an explicit per-user DACL, `PIPE_REJECT_REMOTE_CLIENTS`, and the
  client pid. The listen address is an opaque string (path or
  `\\.\pipe\name`), as Neovim's is.
- **Same for the Neovim link.** Treat the `--listen` address as opaque and
  the channel as "a byte stream" so a named pipe client slots in.
- Hang-up still means "pane closed", and the helper process still exists on
  every platform; nothing here needs a second process.
