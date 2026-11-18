# A Neovim pane's terminal shows the program status of its terminal buffers, their most pressing root record as its own

Neovim keeps the Program Status Protocol (OSC 7501) that programs in a `:terminal` send, firing `TermRequest`, and neither forwards it to its UI nor answers its `?` query; pi and Claude Code report only after that answer. So neopane is a program status terminal for every terminal buffer: Neovim answers `?` itself from the `TermRequest` handler, ahead of its held DA1 reply, and the daemon keeps each terminal buffer's status records, checked against the spec's limits, and drops `working`, `blocked` and `idle` when the program exits or an OSC 133 `A` prompt begins. Each Neovim pane's terminal gets a re-serialized copy, never the raw bytes, of the status of the terminal buffers whose windows it holds or draws. Its root record is a copy of the most pressing of their root records (blocked, then working, error, done, idle, ties to the latest), since tmux reads only the root and zellij mainly it; with one terminal buffer, that buffer's child records keep their ids, and with several each buffer's records also go out under a segment of its own. neopane treats a pane's whole record set as its own, and its undo bytes clear it with a bare `state=clear`.

## Considered Options

- **Always namespace each terminal buffer's records.** tmux drops every report with an `id`, and zellij ignores child `working` and `done`, so neither would show anything.
- **Relay only the current window's terminal buffer.** It hides a blocked agent in another split, and blocked is the state that matters most.
- **Forward the raw bytes.** A program in a `:terminal` could then push any bytes to the outer terminal.
- **Answer `?` only when the outer terminal supports 7501.** The answer is given once, at the program's start, before any pane is known to show it; neopane does keep the records either way.

## Consequences

- A full reset (`RIS`) in a `:terminal` doesn't remove its records: Neovim handles it without a `TermRequest`.
- A helper killed with SIGKILL writes no undo bytes, so the outer terminal keeps neopane's records until its own prompt rule drops `working` and `blocked`.
- A pane relays at most 64 records: its root record always, then the 63 most recently updated, so what neopane wrote is what any conforming terminal keeps.
