# Latency checks

The latency criteria L1–L9 (#92, decision 30) are timed from trace
timestamps the daemon writes to its log at `NEOREX_LOG=debug`. This page says
what each criterion times, how to produce it, how a change is judged, and
holds the baseline every later check compares against: the numbers from the
build that added the traces (#97), not a moving `main` (#95, decision 2).

## Traces

At `NEOREX_LOG=debug` the daemon keeps a trace line for each point below, in
a buffer it writes to the log with the next log line, when the buffer fills,
and as it ends. Every log is kept. At `info` and `warn` a
trace point is one atomic load: no syscall and no allocation.

A trace line is `neorex trace <point> <id> <t> [<from>]`. `t` is the
monotonic clock in nanoseconds (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC`
on Linux), the same in every process. The id is a pane's, unless said
otherwise.

| Point | When |
|---|---|
| `exec` | The launcher started (its id is its pid). Written by the launcher. |
| `split` | A new pane's split asked of Rex. |
| `frame` | A frame written to a pane's terminal in full: once its last byte went out. |
| `input` | A read's keys written to Neovim with `nvim_input`; `from` is when the read came. |
| `held_input` | As `input`, for a read held behind a window switch. |
| `focus` | A focus request (`focus_block`, `focus_direction`) written to Rex (id 0). |
| `tab_move` | A pane of another tab page became the current pane. |
| `resize` | A pane's terminal was resized. |
| `resize_sent` | Its window asked to resize to fit, if its size changed. |
| `place` | A tab page's panes asked to be placed (moved or resized; id: the tab page). |
| `placed` | The last placement answered (id 0). |
| `accept` | A self-attached pane's `find` asked: as it chooses the pane provider, or arrives with one. |
| `found` | Its `find` answered: in reach. |
| `hello` | A hello read whole, its last byte read (the pane's id; the launcher's is 0). |
| `accepted` | Its `a` written to the helper. |

## Reading a log

```sh
neorex debug-latency LOG
```

prints N, the median and the 95th percentile (nearest rank), in
milliseconds, for each criterion found in the log. It pairs the points so:

| Criterion | From | To |
|---|---|---|
| L1 | `split` | the first `frame` of that pane |
| L2 focus | the read of the last key read before a `focus` (within 1 s): one Neovim switched windows for, such as `<C-w>w`'s, or one typed into another pane | the `focus` |
| L2 next | the read of a key held behind a window switch | its `held_input` |
| L3 | a `tab_move`, or the read of the key that made it (within 100 ms before) | the last `frame` before the next key or move (within 1 s) |
| L4 | the read of a key | its `input` (held keys are L2's) |
| L5 | `resize` | the pane's next `resize_sent`, unless another `resize` comes first |
| L6 first | `exec` | the first `frame` |
| L6 second | `exec` | the first `frame` of the second pane to get one |
| L7 | `accept` | the `found` with its id |
| L8 | `place` | the next `placed`, unless another `place` comes first |
| L9 | `hello` | the `accepted` with its id |

L2 next is a key held behind a window switch the daemon asks for, as when a
key is typed into a pane that isn't the current pane. The key after
`<C-w>w` is not L2 next but L4: Neovim makes that switch itself, and the
key goes straight to it.

Run each scenario on a log of its own: the pairing assumes nothing else
happens in between, which a scripted scenario makes so.

## Producing each scenario

`scripts/latency.sh SCENARIO [N]` runs one scenario in a Rex session of its
own, made with `rex new` and killed at the end, then prints the table above
for it. It needs `rex` and `jq`, and times `target/release/neorex` (build it
first; `BIN` names another). It runs Neovim with `--clean`, and types through
`rex send` into the pane Rex has focused, as Rex would from the keyboard. It keeps
each daemon's files under `/tmp/neorex-latency/SCENARIO/`, and the log it
read as `/tmp/neorex-latency/SCENARIO.log`. Run it with N ≥ 50 (it defaults
to 50), and nothing else busy on the machine.

| Scenario | Criteria | What it does |
|---|---|---|
| `keys` | L4 | Types N keys in insert mode, 50 ms apart. |
| `split` | L1 | N times: `:vsplit`, then `:close`. |
| `focus` | L2 focus | Two windows; N times: `<C-w>w` and `l` in one send. |
| `switch` | L2 next | Two windows; N times: `l` typed into the pane that isn't the current pane, which makes its window current and is held behind that switch. |
| `tabs` | L3 | Two tab pages of two windows each; N `gt`s, 400 ms apart. |
| `resize` | L5 | A split's terminal resized smaller and back, N times each, through Rex's `terminal.resize`. |
| `exec` | L6 | N launches of `neorex --clean -O a b`, each quit with `:qa!`. |
| `place` | L8 | N times: `:tabnew \| vsplit \| vsplit \| vsplit`, then `:tabclose`. |

A Rex session made with `rex new` that no Rex app window shows is 80×24, so
its panes are smaller than L3's 4 panes of 200×50: L3 is timed on what that
Rex session gives, the same for the baseline and every later check.

## The regression rule

On macOS and on Linux, in a Rex session made with `rex new`, time the
baseline's build and the new build on the same machine, N ≥ 50 each. Runs of
the same build differ by more than some tolerances (see the baseline's
spread below), so run each scenario 3 times, one at a time, and compare the
median of the 3 runs' medians, and of their p95s, with the baseline's. A criterion regresses when its
median is worse than the baseline's by more than the larger of 10% and its
limit, or its p95 is worse by more than the larger of 20% and twice its limit.

| | Limit |
|---|---|
| L1 | 2 ms |
| L2 | 0.5 ms; no key lands in the wrong window in a scripted `<C-w>w`+text burst |
| L3 | p95 ≤ 16 ms with 4 panes of 200×50 |
| L4 | 0.1 ms; no added syscalls per key |
| L5 | 1 ms |
| L6 | 5 ms |
| L7 | p95 ≤ 10 ms (with a config file, child + Lua p95 ≤ 50 ms, informational) |
| L8 | 10 ms over today; final ratios within one cell after 20 quick `<C-w>>` |
| L9 | 1 ms |

## Baseline: macOS

Taken with this change's build (#97), the release profile, on a MacBook Pro
(M1, 8 cores, 16 GB), macOS 27.0.1, Neovim 0.12.6, Rex 0.1.0-preview.14,
with `scripts/latency.sh SCENARIO 55`. Times in milliseconds.

`split`, `tabs` and `exec` were run 3 times, at d5b75e7 and twice at
80210b3, which changes only when a frame that waited for its terminal is
traced, and none waited at 80×24. Their baseline is the median of the 3
runs' medians, and of their p95s:

| Criterion | Scenario | N per run | Medians | p95s | Baseline median | Baseline p95 |
|---|---|---|---|---|---|---|
| L1 | `split` | 55 | 50.028, 49.971, 48.526 | 66.273, 74.679, 54.423 | 49.971 | 66.273 |
| L3 | `tabs` | 54 | 2.373, 2.404, 2.615 | 3.652, 3.311, 3.838 | 2.404 | 3.652 |
| L6 first | `exec` | 55 | 17.647, 17.308, 17.213 | 41.050, 20.155, 19.839 | 17.308 | 20.155 |
| L6 second | `exec` | 55 | 73.580, 70.822, 69.244 | 153.137, 104.629, 95.476 | 70.822 | 104.629 |

The rest were run once: `focus` and `switch` at d5b75e7, and `keys`,
`resize` and `place` at a01adb1, this change before it was rebased over
#137 and #107 (316904f is that rebase). Their code paths are unchanged
since: nothing after touches a key's way from its read to its
`nvim_input` (`on_input`, the focus gate, the forwarder) beyond the trace
points, nor `handle_resize` or `place_tabs` and the answers to its moves;
#107's branch of `size_window`, for windows laid out in one pane, isn't
taken with Rex's panes.

| Criterion | Scenario | N | Median | p95 |
|---|---|---|---|---|
| L2 focus | `focus` | 55 | 1.050 | 1.407 |
| L2 focus, typed into another pane | `switch` | 55 | 0.024 | 0.043 |
| L2 next | `switch` | 55 | 0.384 | 0.607 |
| L4 | `keys` | 59 | 0.008 | 0.021 |
| L5 | `resize` | 110 | 0.015 | 0.038 |
| L8 | `place` | 55 | 1.239 | 2.597 |

L1 and L6 include Rex starting each new pane's helper process. L3 is timed
on the 80×24 panes of a Rex session no Rex app window shows: its baseline
serves the regression rule only. Its absolute limit, p95 ≤ 16 ms with 4
panes of 200×50, is checked with the Rex session shown in a Rex app window
of that size, which no such baseline stands in for. (`tabs` makes one fewer
sample than N: its first `gt` leaves a window whose pane isn't made yet,
which isn't counted as a move.)

L4 with tracing off has no numbers of its own, since the timestamps are
the traces. Off, the key path is as before but for one atomic load at each
trace point and a flag the forwarder sets: no syscall and no allocation,
which `log`'s tests check of the trace buffer. The time a read came was
already taken for the escape timeout; it's now taken right after the read.

L9's points fire from SPI-639 (#101). Its baseline is from that change's
build, on the same machine, with `scripts/latency.sh split 55` run 3 times:
each new pane's hello, from its terminal's helper.

| Criterion | Scenario | N per run | Medians | p95s | Baseline median | Baseline p95 |
|---|---|---|---|---|---|---|
| L9 | `split` | 56 | 0.074, 0.079, 0.077 | 0.135, 0.138, 0.165 | 0.077 | 0.138 |

A launcher's hello carrying a 64 KB environment as well as Rex's took 0.111
to 0.418 ms over 5 launches.

L7 has no numbers yet: its points fire from #114, which records them as it
lands. The Linux numbers are #133's.
