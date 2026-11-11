# Latency checks

The latency criteria L1–L9 (#92, decision 30) are timed from trace
timestamps the daemon writes to its log at `NEOREX_LOG=debug`. This page says
what each criterion times, how to produce it, how a change is judged, and
holds the baseline every later check compares against: the numbers from the
build that added the traces (#97), not a moving `main` (#95, decision 2).

## Traces

At `NEOREX_LOG=debug` the daemon keeps a trace line for each point below, in
a buffer it writes to the log with the next log line, when the buffer fills,
and as it ends. A clean end keeps the log at `debug`. At `info` and `warn` a
trace point is one atomic load: no syscall and no allocation.

A trace line is `neorex trace <point> <id> <t> [<from>]`. `t` is the
monotonic clock in nanoseconds (`CLOCK_UPTIME_RAW` on macOS, `CLOCK_MONOTONIC`
on Linux), the same in every process. The id is a pane's, unless said
otherwise.

| Point | When |
|---|---|
| `exec` | The launcher started (its id is its pid). Written by the launcher. |
| `split` | A new pane's split asked of Rex. |
| `frame` | A frame written to a pane's terminal. |
| `input` | A read's keys written to Neovim with `nvim_input`; `from` is when the read came. |
| `held_input` | As `input`, for a read held behind a window switch. |
| `focus` | A focus request (`focus_block`, `focus_direction`) written to Rex (id 0). |
| `tab_move` | A pane of another tab page became the focused one. |
| `resize` | A pane's terminal was resized. |
| `resize_sent` | Its window asked to resize to fit, if its size changed. |
| `place` | A tab page's panes asked to be placed (moved or resized; id: the tab page). |
| `placed` | The last placement answered (id 0). |
| `accept`, `found` | L7's points. Named, not yet fired: #114 fires them. |
| `hello`, `accepted` | L9's points. Named, not yet fired: #101 fires them. |

## Reading a log

```sh
neorex debug-latency LOG
```

prints N, the median and the 95th percentile (nearest rank), in
milliseconds, for each criterion found in the log. It pairs the points so:

| Criterion | From | To |
|---|---|---|
| L1 | `split` | the first `frame` of that pane |
| L2 focus | the read of the last key written before a `focus` (within 1 s) | the `focus` |
| L2 next | the read of the first key after a `focus` | its `input` |
| L3 | a `tab_move`, or the read of the key that made it (within 100 ms before) | the last `frame` before the next key or move (within 1 s) |
| L4 | the read of a key | its `input` (held keys are L2's) |
| L5 | `resize` | the pane's next `resize_sent`, unless another `resize` comes first |
| L6 first | `exec` | the first `frame` |
| L6 second | `exec` | the first `frame` of the second pane to get one |
| L7 | `accept` | the `found` with its id |
| L8 | `place` | the next `placed`, unless another `place` comes first |
| L9 | `hello` | the `accepted` with its id |

Run each scenario on a log of its own: the pairing assumes nothing else
happens in between, which a scripted scenario makes so.

## Producing each scenario

`scripts/latency.sh SCENARIO [N]` runs one scenario in a Rex session of its
own, made with `rex new` and killed at the end, then prints the table above
for it. It needs `rex` and `jq`, and times `target/release/neorex` (build it
first; `BIN` names another). It runs Neovim with `--clean`, and types through
`rex send` into the focused pane, as Rex would from the keyboard. It keeps
each daemon's files under `/tmp/neorex-latency/SCENARIO/`, and the log it
read as `/tmp/neorex-latency/SCENARIO.log`. Run it with N ≥ 50 (it defaults
to 50), and nothing else busy on the machine.

| Scenario | Criteria | What it does |
|---|---|---|
| `keys` | L4 | Types N keys in insert mode, 50 ms apart. |
| `split` | L1 | N times: `:vsplit`, then `:close`. |
| `focus` | L2 | Two windows; N times: `<C-w>w`, then `l` at once. |
| `tabs` | L3 | Two tab pages of two windows each; N `gt`s, 400 ms apart. |
| `resize` | L5 | A split's terminal resized smaller and back, N times each, through Rex's `terminal.resize`. |
| `exec` | L6 | N launches of `neorex --clean -O a b`, each quit with `:qa!`. |
| `place` | L8 | N times: `:tabnew \| vsplit \| vsplit \| vsplit`, then `:tabclose`. |

A `rex new` session no app shows is 80×24, so its panes are smaller than L3's
4 panes of 200×50: L3 is timed on what the session gives, the same for the
baseline and every later check.

## The regression rule

On macOS and on Linux, in a `rex new` session, time the baseline's build and
the new build on the same machine, N ≥ 50 each. A criterion regresses when its
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

| Criterion | Scenario | N | Median | p95 |
|---|---|---|---|---|
| L1 | `split` | 55 | 50.002 | 89.901 |
| L2 focus | `focus` | 56 | 0.809 | 1.390 |
| L2 next | `focus` | 56 | 0.008 | 0.019 |
| L3 | `tabs` | 54 | 4.616 | 26.792 |
| L4 | `keys` | 59 | 0.008 | 0.021 |
| L5 | `resize` | 110 | 0.015 | 0.038 |
| L6 first | `exec` | 55 | 17.197 | 36.255 |
| L6 second | `exec` | 55 | 71.982 | 140.018 |
| L8 | `place` | 55 | 1.239 | 2.597 |

L1 and L6 include Rex starting each new pane's helper process; L3, that
none of the session's panes is 200×50 (see above).

L4 with tracing off has no numbers of its own, since the timestamps are
the traces. Off, the key path is as before but for one atomic load at each
trace point and a flag the forwarder sets: no syscall and no allocation,
which `log`'s tests check of the trace buffer. The time a read came was
already taken for the escape timeout; it's now taken right after the read.

L7 and L9 have no numbers yet: their points fire from #114 and #101, which
record them as they land. The Linux numbers are #133's.
