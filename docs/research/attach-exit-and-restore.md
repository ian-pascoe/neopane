# Attach helper: ending the pane, restoring the terminal

Facts for designing how `<name> attach` ends and restores the terminal.
Source trees were read at the commits listed under "Sources"; line numbers
refer to those commits. Web docs were fetched on 2026-11-09.

## Summary

1. **CAN (0x18).** One CAN aborts an in-progress CSI in every terminal checked,
   and aborts OSC and DCS in all of them except **kitty** (OSC, DCS and APC end
   only on BEL or `ESC \`) and **tmux** (DCS only; its OSC state does take CAN).
   CAN prints nothing anywhere checked, except xterm configured as a VT100/VT102
   (`decTerminalID` 100 to 199, not the default 420). SUB (0x1A) does the same
   job but prints a glyph (U+2426) in Windows Terminal/conhost and xterm. ESC
   does not cancel: it starts a new sequence, and in most terminals it
   *dispatches* a half-written OSC or DCS first. The ECMA-48 text does not
   describe CAN as a parser abort at all. Rex's emulator is not named in Rex's
   own docs; press reports say it is built on libghostty.
2. **Non-zero exit after more than 300 ms.** Every terminal checked closes the
   pane by default, and none looks at the status once the command has run
   longer than its early-exit threshold. The only default that holds on
   failure is WezTerm if you set `exit_behavior = "CloseOnCleanExit"`. Ghostty
   holds on failure only within 250 ms (the `abnormal-command-exit-runtime`
   window), and on macOS holds on *any* exit in that window. Defaults:
   Ghostty close, WezTerm close, kitty close, tmux close (`remain-on-exit
   off`), herdr close, Alacritty close, foot close. Rex closes by default
   (inferred from its `--keep-open` flag; its docs give no status rule).
3. **Windows pipe paths.** Microsoft documents only the backslash form
   `\\.\pipe\<name>` for `CreateNamedPipe` and `CreateFile`. Win32 path
   normalization converts `/` to `\` for everything except an exact `\\?\`
   prefix, so `//./pipe/<name>` is a device path that normalizes to
   `\\.\pipe\<name>`. Microsoft's pipe page does not say so for the pipe APIs
   specifically. In practice Neovim generates `//./pipe/<name>.<pid>.<n>` on
   Windows and libuv passes it unchanged to `CreateNamedPipeW`/`CreateFileW`.

## 1. CAN aborting a half-written sequence

### Reference models

- **DEC parser (vt100.net)**: https://vt100.net/emu/dec_ansi_parser. The
  "anywhere" box has `18,1A / execute`, going to ground. The `osc string`
  state has `exit / osc_end` and `dcs passthrough` has `exit / unhook`, so a
  CAN that leaves those states *runs* the end action. ESC (`1B`) from anywhere
  goes to escape (and also runs those exit actions). In `osc string`, bytes
  `00-17,19,1C-1F` are ignored, so 18 and 1A are the only C0 bytes that leave
  by "anywhere" (the DCS header states likewise ignore `00-17,19,1C-1F`).
- **ECMA-48 5th ed.** (https://ecma-international.org/wp-content/uploads/ECMA-48_5th_edition_june_1991.pdf,
  fetched and text-extracted): §8.3.6 CAN: "used to indicate that the data
  preceding it in the data stream is in error. As a result, this data shall be
  ignored. The specific meaning of this control function shall be defined for
  each application and/or between sender and recipient." §8.3.148 SUB: "used in
  the place of a character that has been found to be invalid or in error."
  §5.6: a control string is closed by ST, and a *character string* may contain
  "any bit combination, except ... SOS or ST", so by the letter of ECMA-48 a CAN
  inside an OSC/DCS character string is data. The abort-from-anywhere rule is
  DEC's, not ECMA-48's. This is the source of kitty's and tmux's behavior below.

### xterm (reference implementation)

xterm-snapshots `5c75ead`.

- `VTPrsTbl.c`: every CSI table has `CASE_CAN` at the CAN slot and `CASE_SUB`
  at SUB (e.g. `ansi_table` line 186/188, `csi_table` 510/512, `dec_table`
  4082/4084), and so does `sos_table` (the OSC/DCS/APC string state), lines
  7651-7654.
- `charproc.c:3618`: `case CASE_CAN:` cites ECMA-48 and says "the current
  control sequence is cancelled". It calls `ResetState(sp)` (`charproc.c:2329`:
  back to the ground table). It prints a glyph only `if (screen->terminal_id
  >= 100 && < 200)`: U+2592 (wide builds). The default is `DFT_DECID "420"`
  (`ptyx.h:399`), so by default **CAN prints nothing**.
- `charproc.c:3646`: `case CASE_SUB:` same reset, but prints U+2426
  (`terminal_id > 200`) or U+2592 when `terminal_id >= 100`. So by default
  **SUB prints ␦**.
- I did not trace the OSC/DCS accumulation handlers, so "xterm drops the
  string instead of dispatching it" is inferred from `ResetState` going
  straight to ground, not read from a dispatcher.

### Ghostty

ghostty `cf5519b` (2026-11-08).

- `src/terminal/parse_table.zig:56-68`: `anywhere => ground` for 0x18 and
  0x1A, copied from the DEC diagram.
- `src/terminal/stream.zig:1495-1497`: in the CSI fast path, 0x18/0x1A set
  `parser.state = .ground`, with the comment "We don't currently have any
  handling for 0x18 or 0x1A", so nothing is printed.
- `stream.zig:3528`, `3693`, `6048`, `6387`, `6517`: string end reason
  `cancelled` for 0x18/0x1A, expected to reach ground for APC (6048), OSC
  (6387) and DCS (6517).
- Cancelled strings are discarded, not dispatched: `stream.zig:361-373` (APC
  handlers act only when `terminated`), `stream_terminal.zig:2285-2292` and the
  test `window_title not changed by cancelled OSC` (`stream_terminal.zig:5776`).
- Verdict: CSI, OSC, DCS, APC all aborted by CAN or SUB, nothing printed,
  nothing dispatched.

### kitty

kitty `2db470b`. Hand-written parser, not the DEC table.

- **CSI aborted, nothing printed.** `kitty/vt-parser.c` `csi_parse_loop`
  (line 863): CAN is not in the accepted set (`CSI_NORMAL_MODE_EMBEDDINGS`,
  lines 812-821, is BEL BS HT LF VT FF CR SO SI) so it hits
  `default: REPORT_ERROR("Invalid character in CSI ...ignoring the sequence"); return true;`
  (lines 901, 911, 949). The sequence is dropped, the state returns to normal,
  and the byte is consumed (not drawn). The same happens for ESC inside a CSI,
  with the ESC consumed: a following `\` would then print literally.
- **OSC, DCS, APC, PM, SOS are NOT aborted by CAN.** They all go through
  `accumulate_st_terminated_esc_code` (line 459) → `find_st_terminator`
  (lines 411-430), which only looks for `BEL` or `ESC \`
  (`find_either_of_two_bytes(..., BEL, ESC_ST)`). CAN and SUB are swallowed as
  string data until one of those arrives or the string exceeds
  `MAX_ESCAPE_CODE_LENGTH` (`BUF_SZ / 4`, line 21), at which point it is
  dropped ("escape code too long ... ignoring it", line 481).
  A BEL ends *any* of those string types, DCS included.
- Normal mode: CAN is not matched in `draw_control_char`
  (`kitty/screen.c:1318-1356`, `default: break`), so nothing is printed.
- ESC alone inside a string is data unless followed by `\`.

### WezTerm

wezterm `458a100`. `vtparse` crate = the DEC table.

- `vtparse/src/transitions.rs:27-33`: `anywhere_or` maps 0x18 and 0x1A to
  `(Execute, Ground)` and every state falls through to it (lines 64-289).
- OSC/DCS/APC end actions run on the way out: `transitions.rs:344` (exit
  actions: `Unhook` for DcsPassthrough, `OscEnd` for OscString, `ApcEnd`).
  `vtparse/src/lib.rs:146` and `:175` say the end actions fire when the string is
  "terminated by ST, CAN, SUB or ESC". So **a CAN in the middle of an OSC
  dispatches the truncated OSC** (and unhooks the DCS). `lib.rs:623` is
  `Action::OscEnd`, which calls `actor.osc_dispatch`. This is the DEC-diagram
  behavior and is a real difference from xterm/Ghostty/WT.
- CAN is then executed as a control: `wezterm-escape-parser/src/parser/mod.rs:218`
  → `Action::Control(Cancel)`; `term/src/terminalstate/performer.rs:482-486`
  falls to `_ => log "unhandled ControlCode"`. Nothing printed.

### Alacritty (vte crate)

alacritty `aa5d942` locks `vte 0.15.0` (`Cargo.lock:2124`); vte `89c9c22` is 0.15.0.

- `vte/src/lib.rs`: `anywhere` (line 440-444) executes 0x18/0x1A and goes to
  ground; the CSI, DCS-header and sos/pm/apc states fall through to it.
  Direct arms: `advance_dcs_passthrough` (322), `advance_esc` (385),
  `advance_osc_string` (416).
- `advance_dcs_passthrough` (lib.rs:322-326): `performer.unhook(); execute(byte); ground`.
- `advance_osc_string` (lib.rs:416-420): `self.osc_end(performer, byte)` and
  `osc_end` (lib.rs:556-561) calls `osc_dispatch`. So like WezTerm, a CAN
  **dispatches the truncated OSC**.
- CAN itself: `vte/src/ansi.rs:1308` `_ => debug!("[unhandled] execute byte=...")`.
  Nothing printed.

### foot

foot `152487e`. `vt.c` is a DEC-table implementation.

- `anywhere()` (`vt.c:745-750`): 0x18 and 0x1A → `action_execute`, `STATE_GROUND`.
- `vt.c:921-922`: OSC string: 0x18/0x1A → `action_osc_end(term, data)` then
  execute, ground. `action_osc_end` (`vt.c:605-614`) calls `osc_dispatch`. So
  foot also **dispatches the truncated OSC**.
- `vt.c:1014-1015`: DCS passthrough: `action_unhook` then execute, ground.
- `action_execute` (`vt.c:119`...) has no case for 0x18/0x1A, so nothing is printed.

### tmux (its input parser, i.e. what sits between the helper and the outer terminal)

tmux `a73616d`, `input.c`.

- `INPUT_STATE_ANYWHERE` (`input.c:371-375`): 0x18 and 0x1A →
  `input_c0_dispatch`, `input_state_ground`. It is included in the ground, esc,
  CSI, `dcs_enter`, `dcs_parameter`, `dcs_intermediate`, `dcs_ignore` and
  `osc_string` tables (`input.c:721-731` for OSC).
- **Not included in `dcs_handler` and `dcs_escape`** (`input.c:687-708`, comment
  "No INPUT_STATE_ANYWHERE"). In `dcs_handler`, `0x00-0x1a` are all `input_input`
  (data), so **CAN/SUB do not abort a DCS in its data phase**. Only `ESC` then
  `\` ends it (`dcs_escape`: `\` = `input_dcs_dispatch`, any other byte returns
  to `dcs_handler` as data). Practical consequence: to be sure a DCS is closed
  under tmux, send `ESC \` twice (if the first `ESC` arrived while already in
  `dcs_escape` it is taken as data and the `\` too).
- OSC: leaving `osc_string` runs `input_exit_osc` (`input.c:488-492`, 2718),
  which acts on the buffer, so a CAN-aborted OSC **is dispatched** (if it
  starts with a digit and `;`).
- `input_c0_dispatch` (`input.c:1322-1420`): CAN is not a `case`, goes to
  `default`, no output.

### Windows Terminal / conhost / ConPTY

microsoft/terminal `cbc857b`, `src/terminal/parser/`. The same state machine is
used by conhost (ConPTY) and Windows Terminal.

- `stateMachine.cpp:1840-1856` (`ProcessCharacter`): "Process 'from anywhere'
  events first": CAN and SUB → `_ActionInterrupt(); _ActionExecute(wch);
  _EnterGround();`. The only exception is for the *input* engine in the Escape
  state (Ctrl+Alt+X), not the output engine.
- `_ActionInterrupt` (`stateMachine.cpp:646-660`): "OSC strings require a full
  ST sequence to be received before they can be dispatched". Only a DCS gets its
  end-of-data signal. So **CAN abandons an OSC without dispatching**, and
  flushes a DCS handler.
- `OutputStateMachineEngine.cpp:85-90`: **SUB prints U+2426**; CAN falls to
  `default:` and prints nothing (lines 98-103).
- `_EnterGround` (`stateMachine.cpp:760`) clears the cached sequence.
- Not examined: ConPTY's re-emission of VT to the outer terminal and
  `PSEUDOCONSOLE_PASSTHROUGH_MODE`. If the helper's output is parsed by
  conhost first, the above applies at conhost and the outer terminal never sees
  the half sequence. I did not verify that.

### Summary table

| Terminal | CSI | OSC | DCS | CAN prints | CAN-aborted OSC dispatched? |
|---|---|---|---|---|---|
| xterm (default 420) | abort | abort | abort | no (▒ only for id 100-199) | no (inferred) |
| Ghostty | abort | abort | abort | no | no (tested) |
| kitty | abort | **not aborted** (needs BEL / `ESC \`) | **not aborted** | no | n/a |
| WezTerm | abort | abort | abort | no | **yes** |
| Alacritty (vte 0.15) | abort | abort | abort | no | **yes** |
| foot | abort | abort | abort | no | **yes** |
| tmux | abort | abort | **not aborted in data phase** | no | **yes** |
| WT / conhost | abort | abort | abort | no | no |

### SUB and ESC

- SUB is treated exactly like CAN in every parser above (same table row, same
  code arm). The only visible difference is that Windows Terminal/conhost and
  xterm (default id 420) print U+2426 for SUB. CAN is the better choice.
- ESC never cancels. It starts a new escape sequence. In the DEC-table
  terminals (WezTerm, Alacritty, foot, tmux, xterm per the diagram) ESC inside
  an OSC/DCS first runs the end action, i.e. it **dispatches** the truncated OSC
  (`vte/src/lib.rs:421-425`, `foot/vt.c:924`; WezTerm via the same table). kitty treats ESC inside a CSI as
  an invalid byte and swallows it (a following `\` is printed). So `ESC \` is
  an ST, not an abort.
- My inference (not a source claim): `CAN` followed by `ESC \` would close
  kitty's and tmux's string states too, and is harmless in ground in every
  parser read (`ESC \` is an ST/no-op there). In kitty and tmux that closing
  would dispatch the truncated string, which is what the other DEC-table
  terminals already do on CAN.

### Rex

- Rex docs (https://www.superlogical.com/rex/docs/, full text at
  `/rex/docs/llms-full.txt`, all pages searched for "ghostty", "libghostty",
  "emulator", "xterm") never name the terminal engine. They mention Ghostty
  only as a theme/config import format (e.g. "Import Theme File … Ghostty
  theme") and "Display P3 … like Ghostty's window-colorspace". Rex ships its
  own terminfo entry `xterm-rex` (docs, `rex terminfo`).
- Secondary: The Register, 2026-07-31
  (https://www.theregister.com/devops/2026/07/31/dev-who-gave-hashicorp-its-name-returns-with-a-faster-terminal-multiplexer/5281970)
  reports Hashimoto saying Superlogical plans to build on libghostty and that
  clients must be "smart, high-functioning, compliant" ones "such as one built
  on libghostty". developersdigest.tech and bitdoze.com say "built on
  libghostty". None is a first-party statement of what Rex's *server-side*
  emulator is. Treat as likely libghostty, so the Ghostty row applies, but
  unconfirmed. There is no public source for Rex's CAN behavior.

## 2. Command exits non-zero after more than 300 ms

| Terminal | Default | Exit 0 | Exit non-zero (>300 ms) | Knob |
|---|---|---|---|---|
| Ghostty | close | close | close | `wait-after-command = true` holds for both |
| WezTerm | `Close` | close | close | `exit_behavior`: `Hold` / `CloseOnCleanExit` |
| kitty | close | close | close | `--hold` (`kitty`, `launch`) |
| tmux | `remain-on-exit off` | close | close | `on`, `failed`, `key`, `failed-key` |
| herdr | close | close | close | none found |
| Alacritty | close | close | close | `--hold` |
| foot | close | close | close | `-H/--hold`, `hold_at_exit` |
| Rex | close (inferred) | close | close (undocumented) | `--keep-open` |

### Ghostty

ghostty `cf5519b`.

- `src/config/Config.zig:1346-1354`: `wait-after-command: bool = false`.
  "Normally, the terminal window closes when the running command ... exits.
  With this true, the terminal window will stay open until any keypress."
- `Config.zig:1356-1363`: `abnormal-command-exit-runtime: u32 = 250` (ms).
  "On Linux, this must be paired with a non-zero exit code. On macOS, we allow
  any exit code because of the way shell processes are launched via the login
  command."
- `src/Surface.zig:1297` `childExited`: if `runtime_ms <= abnormal_runtime`,
  then on non-Darwin an exit code of 0 breaks out to the normal path; **otherwise
  (Linux non-zero, or any code on macOS) it shows the error and returns without
  closing** (`Surface.zig:1303-1331`, `childExitedAbnormally` prints "Ghostty
  failed to launch the requested command" with the runtime and, off macOS, the
  exit code, `1378-1440`). Past the threshold it prints "Process exited. Press any
  key to close the terminal." (via native GUI if the apprt has one) and then
  `if (self.config.wait_after_command) return;` else `self.close()`
  (`Surface.zig:1334-1372`). The exit code is **not consulted** on that path, so
  at 300 ms the window closes for exit 0 and exit 1 alike unless
  `wait-after-command` is true. With `wait-after-command = true` it stays open
  for both.
- macOS vs Linux: only inside the 250 ms window. macOS treats every exit
  (0 included) as abnormal and holds; Linux holds only on non-zero.
  macOS code comments say the exit code detection "doesn't work, possibly
  because of our login wrapper" (`Surface.zig:1304-1306`).

### WezTerm

wezterm `458a100`.

- `config/src/config.rs:2036-2045`: `enum ExitBehavior { #[default] Close,
  CloseOnCleanExit, Hold }`. Docs (`docs/config/lua/config/exit_behavior.md`)
  list the three values; the page still says "CloseOnCleanExit ... This is the
  default setting" but a trailing note says "since 20220624-141144-bd1b7c5d
  The default is now `"Close"`". The code agrees with the note.
- `mux/src/localpane.rs:299-325`: `Close` → dead (close); `CloseOnCleanExit`
  with non-clean → hold with message, clean → close; `Hold` → hold either way.
  Clean = status 0 or in `clean_exit_codes` (`config.rs:1898-1900`, default `[]`;
  docs: "0 is always treated as a clean exit code").
- `exit_behavior_messaging` (`config.rs:2047-2054`): default `Verbose`
  (also `Brief`, `Terse`, `None`). It only controls the message shown when the
  pane is held (docs `exit_behavior_messaging.md`).
- `localpane.rs:226-241`: while an ssh domain is connecting or a spawn failed,
  it forces `CloseOnCleanExit` so the error is visible.

### kitty

kitty `2db470b`.

- `kitty/options/definition.py:2950-2962`: `close_on_child_death`, default `no`.
  "With the default value no, the terminal will remain open when the child exits
  as long as there are still other processes outputting to the terminal"; with
  `yes`, closes as soon as the child exits.
- `kitty/child-monitor.c:2045-2056`: the window is queued for removal when the
  pty read hits EOF/POLLHUP (`read_bytes`, line 1790-1812: `child_alive =
  false`). `reap_children` (line 1871) only marks it for removal early when
  `close_on_child_death` is set. Neither path reads the exit status
  (`mark_child_for_removal(self, pid, status)` stores it for monitoring only).
  So a plain `kitty -e cmd` and `launch cmd` close on exit regardless of status.
- Hold: `kitty --hold` (`kitty/cli.py:399` example) and `launch --hold`
  ("Keep the window open even after the command being executed exits, at a shell
  prompt", `kitty/launch.py:240`). Implemented by running the command under
  `kitten run-shell` (`kitty/utils.py:1171-1183`, `cmdline_for_hold`).

### tmux

tmux `a73616d`.

- `options-table.c:1806-1815`: `remain-on-exit`, choices
  `off, on, failed, key, failed-key`, **`default_num = 0` = off**.
- `server-fn.c:381-406` (`server_destroy_pane`): `0` → destroy; `2` (failed) and
  `4` (failed-key): `if (WIFEXITED(status) && WEXITSTATUS(status) == 0) break;`
  (destroy) else fall through and keep the pane with the
  `remain-on-exit-format` line; `1` (on) and `3` (key) keep it for any status.
  A pane killed by a signal counts as failed.
- `tmux.1:6551-6566` documents the same. `failed` = "only when the program exit
  status is not zero".
- Closing the last pane closes the window; closing the last window ends the
  session (general tmux behavior, not re-verified here).

### herdr

herdr `ee5bfb5` (https://github.com/herdrdev/herdr).

- `src/app/api.rs:24-27` `enum RuntimeExitAction { RespawnShell, ClosePane }`;
  `api.rs:541-554` `runtime_exit_action` returns `ClosePane` unless the terminal
  was flagged `respawn_shell_on_exit` (or, on Windows only, a post-agent
  respawn rule at `api.rs:556+`). The `PaneDied` event carries a `ChildExitReason`
  of `Exited | Interrupted | Handoff | WaitFailed` (`src/platform/mod.rs:204-211`),
  **no exit status**. So the pane is removed and `pane.exited` emitted regardless
  of status (`api.rs:310-317`).
- Docs: `docs/next/website/src/content/docs/configuration.mdx:228` ("`type =
  "pane"` opens a temporary zoomed pane and closes it when the command exits").
  I found no remain-on-exit setting.

### Alacritty

alacritty `aa5d942`.

- `--hold`: "Remain open after child process exits" (`extra/man/alacritty.1.scd:20`),
  `alacritty/src/cli.rs:164`, `192`.
- `alacritty/src/event.rs:417-424`: on `TerminalEvent::Exit` the window is
  removed unless `window.hold` (default false, `display/window.rs:207`). The exit
  status (`ChildExit(status)`, `alacritty_terminal/src/event_loop.rs:263`) is not
  used to decide. `hold` is cleared again if the user closes the window
  (`event.rs:1941`). There is no config-file key for it.

### foot

foot `152487e`.

- `main.c:567` `conf.hold_at_exit = hold` and `config.c:3631` default
  `hold_at_exit = false`; `-H/--hold`: "Remain open after child process exits"
  (`doc/foot.1.scd:151`). `terminal.c:1623`: after the client exits,
  `else if (!term->conf->hold_at_exit) term_shutdown(term);`. The status is only
  stored for foot's own exit code (`terminal.c:1614`, `2029-2037`). Also forced on
  when the locale is bad (`main.c:579`).

### Rex

- Rex docs (`rex run`, `rex split`, `rex new`...): `--keep-open` "Keeps the
  terminal open after its command exits." (llms-full.txt lines 1969, 2715, ...).
  `rex wait` "returns the child process's exit status ... so it also works with
  terminals created with `--keep-open`" and "If a terminal closes before
  reporting a child exit, wait returns 1". Event `terminal.child_exited` carries
  `exit_code`. So by default a Rex terminal closes when its command exits. The
  docs say nothing about a non-zero status changing that, so absence of a status
  rule is an inference.
- Program Status Protocol (docs `build/program-status.md`): a program that
  exits right after finishing "SHOULD report `done` or `error` right before
  exiting, so a record is left behind for the user to find", and `done`/`error`
  records "survive" the process exiting. That is the documented way for a
  short-lived command to leave a visible mark in Rex without the pane being held.

## 3. Windows named pipe path forms

### Microsoft docs

- Pipe Names (https://learn.microsoft.com/en-us/windows/win32/ipc/pipe-names):
  the form for `CreateFile`, `WaitNamedPipe`, `CallNamedPipe` is
  `\\ServerName\pipe\PipeName`, with a period for the local machine;
  `CreateNamedPipe` "must use a period for the server name":
  `\\.\pipe\PipeName`. `PipeName` can contain any character except a backslash,
  up to 256 characters, case-insensitive. **The page shows only backslashes and
  says nothing about forward slashes.**
- File path formats (https://learn.microsoft.com/en-us/dotnet/standard/io/file-path-formats),
  "Path normalization": "Almost all paths passed to Windows APIs are
  normalized." Device paths are identified as paths that "begin with two
  separators and a question mark or period (`\\?` or `\\.`)". Step "Canonicalize
  separators": "All forward slashes (`/`) are converted into the standard Windows
  separator, the back slash (`\`)." Only an exact `\\?\` prefix skips it: "Unless
  the path starts exactly with `\\?\` (note the use of the canonical backslash),
  it is normalized." A `\\.\` path is normalized and differs from `\\?\` only in
  that.
- Naming Files, Paths, and Namespaces
  (https://learn.microsoft.com/en-us/windows/win32/fileio/naming-a-file): `/` is
  a reserved character inside a *name component*, `\` separates components, and
  "the `\\?\` prefix ... tells the Windows APIs to disable all string parsing".
  Same page: the `\\.\` prefix "will access the Win32 device namespace instead of
  the Win32 file namespace", and "APIs that go through the CreateFile function
  generally work with the `\\.\` prefix".
- Conclusion from the docs: `//./pipe/x` is a device path, normalizes to
  `\\.\pipe\x` before use, and so `CreateFileW` accepts it. For
  `CreateNamedPipeW` the documentation is silent: it does not say its name goes
  through the same normalization, and no Microsoft page I found says forward
  slashes are accepted there. The strongest evidence is the Neovim use below.
  Forward-slash `//?/pipe/x` would **not** skip normalization (only exact
  `\\?\`), so it is not a verbatim path.
- Caveat: because every `/` becomes `\` and runs of separators collapse
  (after the first two), a pipe name containing `/` (`//./pipe/a/b`) turns into a
  nested path `\\.\pipe\a\b`, which `PipeName` rules forbid. Avoid `/` in the
  name part.

### libuv

libuv `9e40720`, `src/win/pipe.c`.

- `uv_pipe_bind`/`uv_pipe_bind2` (lines 752-855): converts the name UTF-8→UTF-16
  with `uv__convert_utf8_to_utf16` and stores it; **no separator rewriting**.
  `pipe_alloc_accept` → `CreateNamedPipeW(handle->name, ...)` (line 596). The
  client side uses `CreateFileW(name, ...)` (lines 173, 191, 206).
- libuv's own generated names use `"\\\\?\\pipe\\%suv\\%llu-%lu"` (line 135), the
  verbatim form. Its tests use `\\.\pipe\LOCAL\uv-test` (`test/task.h:53`).
  Nothing in libuv generates a forward-slash name.

### Neovim

neovim `5e149d5`.

- `src/nvim/msgpack_rpc/server.c:113-124` `server_address_new`: comment "Named
  pipe format: Windows: `//./pipe/<name>.<pid>.<counter>`", and under `MSWIN`:
  `snprintf(fmt, ..., "//./pipe/%s.%" PRIu64 ".%" PRIu32, ...)`. So Neovim's
  default `v:servername` on Windows is the forward-slash form, passed to libuv
  as is.
- `runtime/doc/vimfn.txt:9357-9362` (`serverstart()`) example: `echo
  serverstart('//./pipe/nvim-pipe-1234')`.
- Tests use both forms on Windows: `test/functional/core/server_spec.lua:92`,
  `175`, `439` (`//./pipe/...`) and `:325`, `test/functional/api/server_requests_spec.lua:339`
  (`\\.\pipe\...`). `server_spec.lua:328` asserts `v:servername` matches
  `//./pipe/`.
- So for Neovim on Windows: `nvim --listen`, `serverstart()` and `--server`
  accept either form, and the default it hands back is `//./pipe/...`. A client
  that compares addresses as strings must treat the two forms as the same pipe;
  case is also insignificant (pipe names are case-insensitive).

## Sources (commits read)

ghostty `cf5519b`, kitty `2db470b`, wezterm `458a100`, vte `89c9c22`
(0.15.0), alacritty `aa5d942`, foot `152487e`, tmux `a73616d`,
xterm-snapshots `5c75ead`, microsoft/terminal `cbc857b`, libuv `9e40720`,
neovim `5e149d5`, herdr `ee5bfb5`. Web: vt100.net DEC parser page, ECMA-48 5th
edition PDF, Microsoft Learn pages linked above, Rex docs and the press
articles linked above.

## Not confirmed

- Rex's emulator and its CAN/exit behavior beyond `--keep-open`.
- Whether `CreateNamedPipeW` accepts forward slashes is established by Neovim's
  production use and tests, not by a Microsoft statement about that API. I did
  not run it on Windows.
- ConPTY output re-emission/passthrough (what the outer terminal sees after
  conhost parses a half sequence).
- xterm: whether a CAN-aborted OSC is dispatched (inferred no).
- ECMA-48 doesn't define CAN as a parser abort. All the terminals above follow
  the DEC model, except kitty (OSC/DCS/APC) and tmux (DCS), which follow the
  stricter ECMA-48 string rule.
