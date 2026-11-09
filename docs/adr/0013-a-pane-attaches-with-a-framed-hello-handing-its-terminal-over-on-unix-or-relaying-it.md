# A pane attaches with a framed hello, handing its terminal over on Unix or relaying it

Amended by #94 before implementation: a self-attach exits with Neovim's exit
status when Neovim quits; `stop` can be forced; `open` can wait for its file;
`nested` needs the attach's tty to be one of Neovim's terminals; the relay
diagnostic is `<NAME>_TRANSPORT`, passed on to requested panes as `--transport
relay`.

Every Neovim pane's terminal runs a helper, `<name> attach`, which connects
to the daemon address and sends a hello: its purpose, its transport, its
terminal's size and where it comes from (#91). The daemon answers every
hello, accepting it or refusing it with a reason, and says why when it later
ends the attach. On Unix the helper hands its terminal over (SCM_RIGHTS) and
the daemon reads and writes it directly, as every pane does today; or it
relays the terminal's bytes in framed messages. Windows can neither hand a
console to another process nor pass a handle with a message (#82), so there
the helper always relays, and nothing in the hello or the messages depends on
the handover.

Whichever the transport, the helper owns its terminal's settings: it puts the
terminal in raw mode before its hello and restores it as the attach ends. The
daemon owns only what it writes, the editor's modes and the pane title, and
gives the helper the bytes that undo them before it writes any, so a daemon
that dies no longer leaves a terminal raw on the alternate screen.

## Considered Options

- **Relay everywhere.** One path, and the daemon would never share a
  terminal's open file with a shell. But every key and frame of every Rex pane
  would gain a hop and a copy nobody has measured, and the daemon would learn
  only that the socket took a frame, not the terminal: a possible Rex
  regression with no gain on Unix.
- **Handing the terminal over only.** Windows can't.
- **The daemon owning the terminal's settings when it holds the terminal, as
  before.** Two owners for two transports, and a daemon that crashed would
  still leave the terminal raw for the shell after it.
- **The daemon writing the undo at the end, waiting up to 1 s for the
  terminal, as before.** One daemon thread would stall on every slow
  terminal in turn; the helper can wait in its own process.
- **The helper offering a list of transports for the daemon to choose.** A
  handed-over terminal travels with the hello, so choosing would cost a round
  trip before the first frame.
- **JSON lines.** An environment isn't always UTF-8, and relayed bytes would
  need base64.
- **A version range, with helpers speaking older versions.** Code to keep for
  a protocol with no release yet; a range can be added later behind the
  frozen hello and refusal.
- **The daemon keeping its own binary for its life**, so an upgrade can't
  change its panes' helper. A hard link fails across filesystems such as a
  tmpfs `/tmp`, so it means copying megabytes on every start, on Rex's path,
  into ADR 0004's directory, against a rare version bump.
- **SUB, or ESC alone, to end a half-written escape sequence.** SUB prints a
  glyph in Windows Terminal and xterm; ESC acts on a half OSC and is swallowed
  inside a kitty CSI.
- **Waiting for a key after every failed attach, or never.** Always would
  make every failed `<name>` typed in a Rex shell need a key; never, and the
  message goes with the terminal, since every terminal checked closes on
  exit whatever the status.
- **Leaving the relay unbuilt until Windows.** A transport nobody runs goes
  stale, and Windows work would debug the protocol and the console at once.

## Consequences

- Every message is a frame: a kind byte, a big-endian 32-bit length, and a
  payload, msgpack for control messages and raw bytes for relayed input and
  output. `docs/attach-protocol.md`, written from the resolution of #91, has
  the frames, fields, limits and reasons. The frame layout, the hello's
  version, build and purpose, the refusal with its reason and message, and the
  status and stop requests, `stop`'s `force` included, are frozen for good, so
  any two versions can tell each other apart and a newer `<name>` can always
  list and stop an older daemon.
- The attach protocol version must match exactly. It changes only when an
  existing message or field changes meaning; unknown fields and kinds are
  ignored, and new messages that need an answer are announced when the
  daemon accepts. A mismatch is refused with both builds named. After its own
  requested pane is refused so, as after an upgrade that bumps the version,
  the daemon stops asking for terminals and draws new windows in their origin
  panes, warning once, until its executable changes again.
- A requested pane runs `<exe> attach --address <address> --pane <pane id>`
  from the daemon's own executable (with `--transport relay` added under the
  relay diagnostic, since a requested pane's terminal gets its terminal
  provider's environment, not the user's; #94). Every pane has a pane id, a
  number and a random token such as `12-k3j9x2qpa8`; the pane title uses the
  token. A requested pane's id is good for one attach, from just before the
  pane provider is asked until a terminal attaches with it, the attach timeout
  runs out or the request goes. A late terminal is refused, and its helper
  exits quietly with status 0, which closes a terminal whose `open` printed no
  id.
- A self-attach is the same helper and the same hello, carrying its whole
  environment, working directory, tty, pid, any `--provider` hint and any
  file. No answer waits on Neovim, the config or a pane provider: choosing
  runs after the daemon accepts.
- Sizes travel as messages under both transports; the daemon never asks a
  terminal its size.
- The helper saves the terminal's settings and its open file's status flags
  before its hello and puts both back on every way out, since a daemon killed
  mid-write leaves the shared file non-blocking. At a normal end the daemon
  drops what it hasn't started writing, says whether it cut a frame short, and
  ends; the helper then writes CAN and `ESC \` if it did, and the undo bytes.
  A helper whose daemon vanishes does the same unasked: CAN ends a half CSI
  everywhere checked, and `ESC \` the strings kitty and tmux end only on ST.
  Since a cut OSC may be acted on, the undo bytes must close any state an OSC
  the daemon writes can leave open. Once a helper hangs up, the daemon writes
  nothing more to its terminal.
- The helper never exits within 300 ms of starting. It exits 0 when the attach
  ends in the normal course or arrived late, except that a self-attach ended
  by Neovim quitting exits with Neovim's exit status, as `nvim` does, so `:cq`
  aborts a `git commit` (#94); and 1 on any other failure, with one line. A
  failed self-attach then waits for a key unless a shell started it as a job;
  a refused requested pane is reported in Neovim instead.
- Both ends check the other runs as the same user before the hello: the uid
  on Unix; on Windows, a pipe with an owner-only DACL that refuses remote
  clients, and the peer's SID. The hello carries no credentials, and the
  token is no secret.
- An attach from a daemon's own Neovim, whose `$<NAME>` is the daemon's
  address and whose tty is one of that Neovim's `:terminal`s (or which has
  none), is refused; a terminal that only inherited `$<NAME>` is not (#94).
  The daemon never blocks its loop on a hello and never logs an environment.
- The relay is built on Unix too and reachable by users for diagnosis, by
  `<NAME>_TRANSPORT=relay`, which every helper reads (#94); the handover stays
  the default. On Windows the helper sets the console's VT input, VT output
  and window input modes and reads sizes from its window-buffer events, and
  the address is written `//./pipe/<name>`, with no `/` in the name, so the
  attach command stays bare words.

Research: `docs/research/windows-attach.md` and
`attach-exit-and-restore.md`, each on its `research/` branch.
