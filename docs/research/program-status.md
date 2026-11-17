# Program Status Protocol (OSC 7501) across programs, terminals and Neovim

Facts for re-emitting OSC 7501 from Neovim `:terminal` buffers into the outer
terminal. Checked on 2026-11-17. Versions: Neovim 0.12.6 (installed), 0.11.7
and master, pi 1.5.2, Claude Code 2.1.295 (darwin-arm64 binary), Codex
rust-v0.163.0, OpenCode v2.2.0 with OpenTUI v0.7.0, tmux 3.8 and master
(2026-11-12), zellij v0.46.0, kitty v0.50.0, herdr v0.9.5 and PR #5172.

Confidence tags: **probe** = run here; **source** = read in the source or
binary; **doc** = stated in a document only.

The spec is https://www.superlogical.com/rex/docs/build/program-status (rev
0.4). Its key rules: a report with no `id` is the root record; `/` makes a
child; each report replaces its record; `?` is the feature query, answered
with the same body plus optional `states=`/`kinds=`; the terminal MUST ignore
unknown OSCs; the terminal drops `working`/`blocked` records when the process
exits or an OSC 133 `A` prompt begins; terminals MAY aggregate.

The PR linked in the request, `spiritledsoftware/neopane/issues/5172`, is not
the herdr PR. The herdr PR is `herdrdev/herdr#5172`.

## 1. Which programs emit OSC 7501

| Program | Version | Query first? | Emits only after a reply? | Records |
| --- | --- | --- | --- | --- |
| pi | 1.5.2 | yes, `?` sent with kitty-keyboard and DA1 | yes; `PI_PROGRAM_STATUS=1` forces, `0` disables | root only |
| Claude Code | 2.1.295 | yes, a probe with DA1 | yes | root and `id=` children |
| Codex CLI | 0.163.0 | no | no; gated only on stdout being a TTY | root only |
| OpenCode | 2.2.0 | no | no | root plus one `id` child per pending session |

- **pi.** The docs say: "Pi sends them only after the terminal answers the
  protocol's support query; tmux and screen do not forward them." (doc:
  `docs/terminal-setup.md#program-status`, pi 1.5.2, also
  https://www.npmjs.com/package/@earendil-works/pi-coding-agent/v/1.5.2).
  The shipped code agrees (source: the pi 1.5.2 binary). At startup it writes
  the kitty keyboard query, `OSC 7501 ; ? ST` and DA1 together. `formatProgramStatus`
  writes only `state`, `app`, `kind` and `msg`, never `id`, so pi never
  writes child records. It writes `state=clear` on stop. Confidence: high.
- **Claude Code.** Changelog: "Added Program Status Protocol (OSC 7501)
  support" under 2.1.295
  (https://github.com/anthropics/claude-code/blob/main/CHANGELOG.md, line 2359).
  Source: the minified binary in
  https://registry.npmjs.org/@anthropic-ai/claude-code-darwin-arm64/-/claude-code-darwin-arm64-2.1.295.tgz.
  Its startup probe sends `OSC 7501 ; ? ST` with DA1. The strings include
  `probe: OSC 7501 reply` and `probe: no reply to OSC 7501 ; ?`. The
  `programStatus` capability defaults to `false`, and the report writer checks
  it, so reports go out only after the terminal answers. A DA1 reply with no
  7501 reply settles it as unsupported. Reports are built as
  `state=…:app=claude-code[:id=…][:kind=…][:progress=…][:title=…][:msg=…]`, and
  children clear with `state=clear:id=…`. The `id` values come from a helper
  whose minified name I did not trace. Changelog 2.1.326 also fixes "Program
  Status Protocol (OSC 7501) terminals showing a finished turn…" and "…not
  seeing Claude Code as waiting on you while the folder trust prompt is open".
  Confidence: high for the gate, medium for what ids children carry.
- **Codex CLI.** No query. `terminal_program_status_support()` sets `osc_7501`
  from `stdout().is_terminal()`, and the writer emits
  `ESC ] 7501 ; state={idle|working|blocked}:app=codex ST`, plus
  `state=clear`. No `id`. It is not gated on a multiplexer for 7501; the screen
  check (`STY`) only gates iTerm2's OSC 21337. Source:
  https://github.com/openai/codex/blob/rust-v0.163.0/codex-rs/tui/src/terminal_program_status.rs.
  Confidence: high.
- **OpenCode.** No query. `packages/tui/src/context/program-status.ts` says:
  "Mirrors pending session state into OSC 7501 records: one root summary plus
  one child record per pending session." Each child `id` is the session id if
  it fits `[\w.+-]{1,32}`, else `ses_` plus a sha256 prefix. Children clear
  when no longer pending. Source:
  https://github.com/anomalyco/opencode/blob/v2.2.0/packages/tui/src/context/program-status.ts.
  OpenTUI's `setProgramStatus` says it "does not wait for
  `capabilities.program_status`", and it treats the `?` reply only as a
  capability flag (https://github.com/anomalyco/opentui/blob/v0.7.0/packages/core/src/renderer.ts,
  `setProgramStatus`). Confidence: high.
- The areweosc7501yet.com index (https://areweosc7501yet.com) agrees on
  shipping versions for these four. I used it only as an index.

## 2. How consumers treat child records

Aggregation rules, per consumer:

- **Rex.** The spec leaves presentation to the terminal ("the terminal decides
  which to show", and children MAY be used "for grouping. It means nothing
  else."). Rex's events doc lists `terminal.program_status_changed` (`id`,
  `record`, `reason`) and `terminal.program_status_removed` (`ids`, `reason`):
  https://www.superlogical.com/rex/docs/reference/events. The Rex changelog
  says "Rex no longer guesses program status from screen output"
  (preview.15, https://discord.com/channels/1546797889009881158/1547349572299395172/1560719900602011715)
  and fixes "OSC7501 parsing bug where an invalid id would clear the full
  status" (preview.18,
  https://discord.com/channels/1546797889009881158/1547349572299395172/1562547568133541998).
  **Unknown:** no document says whether a tab or pane aggregates child records.
- **herdr, PR #5172 (not merged, not released).** The GitHub API reports
  `state: open` on 2026-11-17. The latest herdr release, v0.9.5 (2026-10-14,
  https://github.com/herdrdev/herdr/releases), has no 7501 mention. The PR
  body says "The pane shows the record that needs the most attention". The
  diff confirms it: `refresh()` takes `max_by_key(attention)` over all
  `records`, with rank blocked (4) > working (3) > unseen result (2) > idle,
  done or error (1). So children count, and a root is not required
  (https://github.com/herdrdev/herdr/pull/5172.diff). Confidence: high.
- **herdr-core / Herdr app.** `CHANGELOG.md` on `master`
  (https://github.com/herdrdev/herdr-core/blob/master/CHANGELOG.md, latest
  `[0.1.21] - 2026-11-12`) does not mention OSC 7501 or program status. (The
  `main` branch 404s; the default branch is `master`.)
- **zellij v0.46.0 (2026-11-11).** The changelog entry is "OSC7501 support"
  (PR #5734, https://github.com/zellij-org/zellij/blob/v0.46.0/CHANGELOG.md).
  Aggregation is in `deciding_record()`
  (https://github.com/zellij-org/zellij/blob/v0.46.0/zellij-server/src/attention/program_status.rs).
  The root is the baseline. Children can only **raise** the pane: blocked
  becomes "needs you", error becomes "failed". Child `working` and `done` are
  ignored. A child inherits `app` from its nearest ancestor. Confidence: high.
- **kitty v0.50.0 (2026-11-03).** Shipped in its changelog. The docs say it
  "keeps the records reported by programs per window"
  (https://github.com/kovidgoyal/kitty/blob/v0.50.0/docs/misc-protocol.rst,
  `program_status`). `most_urgent()` in `kitty/program_status.py` ranks all of a
  window's records, children included:
  `URGENCY = {idle: 0, working: 1, done: 2, error: 3, blocked: 4}`, with ties
  broken by recency. `tab_bar.py` combines windows. Confidence: high.
- **tmux (master only, see section 3).** Root only. A report with any `id=`
  key is dropped whole. Confidence: high.
- **Ghostty.** Parser merged as ghostty-org/ghostty#14560 (merged 2026-10-06,
  https://github.com/ghostty-org/ghostty/pull/14560). Neovim's issue #42276
  (2026-10-08) says "The Ghostty app ignores these reports for now". I did not
  check the Ghostty app source.
- **cmux v0.65.1 (2026-10-12).** Release notes list "add OSC 7501 program
  status support (#18742)" and "publish OSC 7501 status events (#19091)"
  (https://github.com/manaflow-ai/cmux/releases/tag/v0.65.1). Record handling
  not checked.
- **iTerm2.** The index says 3.7.5 (2026-10-15). Not verified here.

## 3. tmux and zellij

**tmux**

- **3.8 (released 2026-09-09) has no OSC 7501.** On 3.7c, OSC 7501 is an
  unknown OSC and is dropped. Only the DA1 reply comes back
  (https://github.com/tmux/tmux/issues/5713, the log excerpt; probe there).
- **Master has minimal support.** Commit `434ab3a` (2026-11-12, "Minimal
  support for OSC 7501 program status, GitHub issue 5830")
  (https://github.com/tmux/tmux/commit/434ab3a25072837616c34f6d6256f82f52ed988b).
  Each pane keeps one root record: `program_state`, `program_name` and
  `program_message`. These are exposed as `pane_program_state`,
  `pane_program_name` and `pane_program_message` (`format.c`). `?` gets the
  reply. Any report containing `id` is dropped (`input_program_status`: `if
  (strcmp(key, "id") == 0) goto out`). Confidence: high (source).
- **Not forwarded.** tmux consumes the sequence, and I found no path that
  re-emits it to the outer terminal. Confidence: high (source grep).
- **Passthrough.** A `DCS tmux;` payload is written to the outer terminal
  when `allow-passthrough` is `on` (visible panes only) or `all`
  (`input_dcs_dispatch` in `input.c`; `tmux.1`,
  https://github.com/tmux/tmux/blob/master/tmux.1). So a wrapped
  `ESC P tmux; ESC ESC ] 7501 … ESC ESC \ ESC \` should reach the outer
  terminal. Not tested. The index says "With allow-passthrough on, reports
  still reach the outer terminal" and cites issue comment 6079182674. That
  comment is nicm's "This seems premature", and the issue thread has no
  mention of passthrough. Treat the index's claim as unsupported.

**zellij**

- **v0.46.0 implements it as a terminal for its own panes.** The pane grid
  matches `b"7501"` and answers queries
  (`zellij-server/src/panes/grid.rs`, around line 5539, `query_reply`:
  https://github.com/zellij-org/zellij/blob/v0.46.0/zellij-server/src/panes/grid.rs).
  Records are kept per pane. Confidence: high.
- **Not forwarded.** I found no path that re-emits 7501 to the host. A 7501
  written in a zellij pane is consumed. I did not check zellij's handling of
  other unknown OSCs. Confidence: medium.
- zellij's client sends the same `?` to its own host terminal at startup
  (`zellij-client/src/stdin_handler.rs`, `PROGRAM_STATUS_QUERY`), so zellij
  expects to hear the reply from its host.

## 4. Neovim

- **No version handles or forwards 7501.** No 7501 string in the installed
  0.12.6 binary (`strings` hit count 0). No match in `v0.11.7` source or docs.
  No match in master, per the issue's text. Searching neovim issues and PRs
  for "7501" returns only #42276: "TUI: OSC 7501 program status protocol"
  (opened 2026-10-08 by justinmk, open, Enhancement, milestone `backlog`, no
  linked PR, https://github.com/neovim/neovim/issues/42276). Its own notes say
  "first step could just be to emit a `working` status…" and "it will be more
  useful for Nvim to pass through status from external tools".
- **Neovim does not answer `?`.** Probe (0.12.6): the child sent
  `ESC]7501;?ST` then `ESC[c`. Its only reply was the DA1 answer
  `ESC[?61;22;52c`. Neovim answers DA1 (libvterm) and nothing for 7501.
- **TermRequest fires for both terminators.** Probe (0.12.6, headless,
  `:terminal` running a script). OSC 7501 with ST (`terminator="ST"`) and BEL
  (`terminator="BEL"`) both fire, as do OSC 133 `A` with ST and BEL. The
  `?` query fires too (`sequence="\27]7501;?"`). The source agrees:
  `emit_termrequest` sets `terminator` to `\x07` for BEL and `\x1b\\` otherwise
  (`src/nvim/terminal.c`, https://github.com/neovim/neovim/blob/master/src/nvim/terminal.c).
  Confidence: high (probe + source).
- **Other OSCs fire too.** Probe: OSC 0 (BEL), OSC 52 and OSC 99 (ST) all
  reached TermRequest on 0.12.6. `runtime/doc/terminal.txt` says "OSC 52
  sequences sent from the :terminal buffer do not emit a TermRequest event"
  (https://github.com/neovim/neovim/blob/master/runtime/doc/terminal.txt).
  The probe contradicts that line. Confidence: high for the observation.
- **0.11 vs 0.12.** 0.11.7 has the same `on_osc` guard (`command != 8 &&
  !has_event`) and the same pending-send logic. Its TermRequest docs have no
  `terminator` field; 0.12 added it (`runtime/doc/news.txt`, v0.12.6,
  https://github.com/neovim/neovim/blob/v0.12.6/runtime/doc/news.txt).
- **Replies written from a TermRequest handler go ahead of DA1.**
  `schedule_termrequest` sets `term->pending.send`, so bytes libvterm writes
  to the job (DA1 included) are held. `emit_termrequest` clears
  `pending.send`, runs the autocommands, then flushes the held bytes
  (`src/nvim/terminal.c`, same in 0.11.7). Probe: a handler that
  `chansend`s `ESC]7501;?:states=…:kinds=… ST` gave the child the 7501 reply
  first, then `ESC[?61;22;52c`. With no handler reply, the child got only DA1.
  Confidence: high for the probe; not yet tested with a real agent.

## 5. TermRequest while the buffer is hidden

- **Yes (probe, 0.12.6).** The `:terminal` job ran `sleep 1` and then emitted.
  Before that the window switched to a new buffer, so the terminal buffer had
  no window (`wins_showing=0`). Every sequence still fired TermRequest, with
  `buf` set to the terminal buffer.
- **Source.** `emit_termrequest` only checks that the buffer is valid and
  still has a terminal. There is no window check.
- **Not tested.** `bufhidden=wipe`, `unload` or `delete`, and `:bdelete`.

## Implications for neopane

1. **Neovim gives us everything we need, and nothing more.** TermRequest
   delivers the sequence and `terminator` (ST or BEL), also from hidden
   buffers. Neovim forwards nothing and replies to nothing, so the re-emit is
   ours to build.
2. **Agents need the query answered before DA1, and Neovim answers DA1
   first.** Answering `?` from a TermRequest handler with `chansend` puts the
   reply ahead of the held DA1 (probed). The cost: we would be claiming
   support on behalf of the outer terminal. Answer only the states the outer
   terminal really accepts, and stay silent when it doesn't (tmux 3.8 and
   zellij's host path differ; see section 3). This is untested against pi and
   Claude Code. pi also honours `PI_PROGRAM_STATUS=1`, which skips the query.
   I found no equivalent for Claude Code: its settle-false paths are the
   `CLAUDE_CODE_DISABLE_TERMINAL_TITLE` env var and background workers.
3. **Records: root is the only shared denominator.** tmux (master) drops any
   report with `id`. kitty, zellij and herdr (PR) aggregate children. Rex is
   undocumented. Re-emit a root summary everywhere, and child `id`s only for
   the outer terminals that take them (kitty, zellij, Rex after a test).
   Don't depend on herdr until PR #5172 ships.
4. **Lifecycle is ours to apply.** The spec drops `working` and `blocked` on
   process exit or an OSC 133 `A` prompt. The outer terminal sees neopane as
   the program, so the inner shell's prompts and exits never reach it. We get
   OSC 133 `A` through TermRequest (probed), so we can apply those rules per
   `:terminal`.
5. **Verify before shipping.** Test kitty 0.50 and zellij 0.46 against real
   output with ids. Test tmux with `allow-passthrough` for wrapped reports.
   cmux and iTerm2 are unverified. Ghostty ignores reports, per the Neovim
   issue. Don't count on Neovim to change: #42276 is in the backlog with no
   PR.
