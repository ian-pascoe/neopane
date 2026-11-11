//! `debug-latency LOG`, a hidden command: reads the traces in a daemon log
//! taken at `<NAME>_LOG=debug`, and prints N, the median and the 95th
//! percentile of each latency criterion, L1 to L9 (`docs/latency.md`).
//!
//! A trace line is `<name> trace <point> <id> <t> [<from>]`, times in
//! nanoseconds of the monotonic clock. Other lines are skipped, and so is the
//! name, so a log from before a rename reads the same.

use std::ffi::OsStr;

use super::Result;
use crate::log::Point;

/// A start further back than this pairs with nothing: it was for something else.
const WINDOW_NS: u64 = 1_000_000_000;

/// A key read this soon before a move to another tab page made the move.
const MOVE_KEY_NS: u64 = 100_000_000;

pub fn run(path: &OsStr) -> Result<()> {
    let bytes = std::fs::read(path)?;
    let traces = parse(&String::from_utf8_lossy(&bytes));
    print!("{}", report(&measure(&traces)));
    Ok(())
}

/// One trace line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Trace {
    pub point: Point,
    pub id: u64,
    pub t: u64,
    /// For input: when its read came.
    pub from: Option<u64>,
}

/// The traces in `log`, in time order.
pub(crate) fn parse(log: &str) -> Vec<Trace> {
    let mut out: Vec<Trace> = log.lines().filter_map(parse_line).collect();
    // stable: the daemon's own are in order already
    out.sort_by_key(|t| t.t);
    out
}

fn parse_line(line: &str) -> Option<Trace> {
    let mut words = line.split_ascii_whitespace();
    let _name = words.next()?;
    if words.next()? != "trace" {
        return None;
    }
    let point = Point::from_tag(words.next()?)?;
    let id = words.next()?.parse().ok()?;
    let t = words.next()?.parse().ok()?;
    let from = match words.next() {
        Some(w) => Some(w.parse().ok()?),
        None => None,
    };
    Some(Trace { point, id, t, from })
}

/// A criterion's samples, in nanoseconds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Criterion {
    pub name: &'static str,
    pub what: &'static str,
    pub samples: Vec<u64>,
}

/// Each criterion's samples in `traces`, which are in time order.
pub(crate) fn measure(traces: &[Trace]) -> Vec<Criterion> {
    let c = |name, what, samples| Criterion {
        name,
        what,
        samples,
    };
    vec![
        c(
            "L1",
            "split asked -> first frame in the new pane",
            pair(traces, Point::Split, Point::Frame, By::Id),
        ),
        c(
            "L2 focus",
            "key read -> focus request written",
            key_to_focus(traces),
        ),
        c(
            "L2 next",
            "key held behind a window switch read -> its nvim_input written",
            latencies(traces, Point::HeldInput),
        ),
        c(
            "L3",
            "another tab page's pane made current -> last pane's frame written",
            tab_move_to_last_frame(traces),
        ),
        c(
            "L4",
            "key read -> nvim_input written",
            latencies(traces, Point::Input),
        ),
        c(
            "L5",
            "terminal resize received -> window resize sent",
            pair(traces, Point::Resize, Point::ResizeSent, By::Id),
        ),
        c("L6 first", "exec -> first frame", exec_to_frames(traces, 1)),
        c(
            "L6 second",
            "exec -> second pane's first frame",
            exec_to_frames(traces, 2),
        ),
        c(
            "L7",
            "accept -> find confirmed",
            pair(traces, Point::Accept, Point::Found, By::Id),
        ),
        c(
            "L8",
            "placement asked -> last placement answered",
            pair(traces, Point::Place, Point::Placed, By::Order),
        ),
        c(
            "L9",
            "hello received -> accepted",
            pair(traces, Point::Hello, Point::Accepted, By::Id),
        ),
    ]
}

/// A key's latency: its read to its `nvim_input`.
fn input_latency(t: &Trace) -> Option<u64> {
    t.from.map(|f| t.t.saturating_sub(f))
}

fn is_input(t: &Trace) -> bool {
    t.point == Point::Input || t.point == Point::HeldInput
}

/// Which `end` a `start` pairs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum By {
    /// The first with its id.
    Id,
    /// The first.
    Order,
}

/// From each `start` to the first `end` after it, of its id if `by` says,
/// unless another such `start` comes first.
fn pair(traces: &[Trace], start: Point, end: Point, by: By) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, s) in traces.iter().enumerate() {
        if s.point != start {
            continue;
        }
        for e in &traces[i + 1..] {
            if by == By::Id && e.id != s.id {
                continue;
            }
            if e.point == end {
                out.push(e.t - s.t);
                break;
            }
            if e.point == start {
                break;
            }
        }
    }
    out
}

/// The latencies of the keys traced at `point`: each read to its `nvim_input`.
fn latencies(traces: &[Trace], point: Point) -> Vec<u64> {
    traces
        .iter()
        .filter(|t| t.point == point)
        .filter_map(input_latency)
        .collect()
}

/// From the read of the last key read before each focus request, within
/// the window, to the request: a key Neovim switched windows for, such as
/// `<C-w>w`'s, written before it, or one typed into another pane, held
/// behind the switch it made and written after it. Each key counts once.
fn key_to_focus(traces: &[Trace]) -> Vec<u64> {
    let mut used = vec![false; traces.len()];
    let mut out = Vec::new();
    for f in traces.iter().filter(|f| f.point == Point::Focus) {
        let key = (0..traces.len())
            .filter(|&k| is_input(&traces[k]))
            .filter_map(|k| Some((k, traces[k].from?)))
            .filter(|&(_, read)| read <= f.t)
            .max_by_key(|&(_, read)| read);
        let Some((k, read)) = key else {
            continue;
        };
        if used[k] || f.t - read > WINDOW_NS {
            continue;
        }
        used[k] = true;
        out.push(f.t - read);
    }
    out
}

/// From each move to another tab page's pane to the last frame written before
/// the next key or move, within the window. A move made by a key, such as
/// `gt`, is traced once Neovim says so, after it has drawn: it's timed from
/// the key's read.
fn tab_move_to_last_frame(traces: &[Trace]) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, m) in traces.iter().enumerate() {
        if m.point != Point::TabMove {
            continue;
        }
        let key = (0..i).rev().find(|&k| is_input(&traces[k]));
        let (from, start) = match key.and_then(|k| Some((k, traces[k].from?))) {
            Some((k, read)) if m.t.saturating_sub(read) <= MOVE_KEY_NS => (k, read),
            _ => (i, m.t),
        };
        let last = traces[from + 1..]
            .iter()
            .enumerate()
            .take_while(|&(j, e)| {
                let other_move = e.point == Point::TabMove && from + 1 + j != i;
                !is_input(e) && !other_move && e.t.saturating_sub(start) <= WINDOW_NS
            })
            .filter(|(_, e)| e.point == Point::Frame)
            .last();
        out.extend(last.map(|(_, e)| e.t.saturating_sub(start)));
    }
    out
}

/// From each launch to the first frame of the `nth` pane to get one, before
/// the next launch.
fn exec_to_frames(traces: &[Trace], nth: usize) -> Vec<u64> {
    let mut out = Vec::new();
    for (i, x) in traces.iter().enumerate() {
        if x.point != Point::Exec {
            continue;
        }
        let mut panes: Vec<u64> = Vec::new();
        for e in traces[i + 1..]
            .iter()
            .take_while(|e| e.point != Point::Exec)
        {
            if e.point != Point::Frame || panes.contains(&e.id) {
                continue;
            }
            panes.push(e.id);
            if panes.len() == nth {
                out.push(e.t - x.t);
                break;
            }
        }
    }
    out
}

/// N, the median and the 95th percentile (nearest rank) of some samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Stats {
    pub n: usize,
    pub median: u64,
    pub p95: u64,
}

/// None for no samples.
pub(crate) fn stats(samples: &[u64]) -> Option<Stats> {
    let n = samples.len();
    if n == 0 {
        return None;
    }
    let mut s = samples.to_vec();
    s.sort_unstable();
    let median = if n % 2 == 1 {
        s[n / 2]
    } else {
        (s[n / 2 - 1] + s[n / 2]) / 2
    };
    // the smallest sample at least 95% of them are no greater than
    let p95 = s[(n * 95).div_ceil(100) - 1];
    Some(Stats { n, median, p95 })
}

/// The table: a row per criterion, times in milliseconds.
pub(crate) fn report(criteria: &[Criterion]) -> String {
    let mut out = format!(
        "{:<10} {:>5} {:>10} {:>10}  {}\n",
        "criterion", "N", "median ms", "p95 ms", "timed"
    );
    for c in criteria {
        let (n, median, p95) = match stats(&c.samples) {
            Some(s) => (s.n.to_string(), ms(s.median), ms(s.p95)),
            None => ("0".to_string(), "-".to_string(), "-".to_string()),
        };
        out.push_str(&format!(
            "{:<10} {:>5} {:>10} {:>10}  {}\n",
            c.name, n, median, p95, c.what
        ));
    }
    out
}

fn ms(ns: u64) -> String {
    format!("{:.3}", ns as f64 / 1e6)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(log: &str, name: &str) -> Vec<u64> {
        measure(&parse(log))
            .into_iter()
            .find(|c| c.name == name)
            .unwrap()
            .samples
    }

    #[test]
    fn trace_lines_are_read_whatever_the_name_and_the_rest_skipped() {
        let log = "\
x daemon: Rex hung up, treating it as closed
other trace input 1 2000 1500
x trace frame 3 1000
x trace nothing 1 1
x trace input 1 abc
x trace
";
        assert_eq!(
            vec![
                Trace {
                    point: Point::Frame,
                    id: 3,
                    t: 1000,
                    from: None
                },
                Trace {
                    point: Point::Input,
                    id: 1,
                    t: 2000,
                    from: Some(1500)
                },
            ],
            parse(log)
        );
    }

    #[test]
    fn n_median_and_p95_by_nearest_rank() {
        assert_eq!(None, stats(&[]));
        let one = Stats {
            n: 1,
            median: 7,
            p95: 7,
        };
        assert_eq!(Some(one), stats(&[7]));
        assert_eq!(10, stats(&[20, 0]).unwrap().median);
        // 1..=100: the 95th smallest
        let s: Vec<u64> = (1..=100).rev().collect();
        let got = stats(&s).unwrap();
        assert_eq!((100, 50, 95), (got.n, got.median, got.p95));
        // 1..=50: rank 48 of 50
        let s: Vec<u64> = (1..=50).collect();
        assert_eq!(48, stats(&s).unwrap().p95);
    }

    #[test]
    fn a_split_is_timed_to_its_panes_first_frame() {
        let log = "\
n trace split 1 100
n trace frame 0 150
n trace frame 1 400
n trace frame 1 900
n trace split 2 1000
n trace frame 2 1700
";
        assert_eq!(vec![300, 700], samples(log, "L1"));
    }

    #[test]
    fn keys_are_timed_from_their_read_and_focus_from_the_key_before_it() {
        let log = "\
n trace input 0 100 40
n trace input 0 300 250
n trace focus 0 500
n trace input 1 800 700
n trace focus 0 5000000000
";
        assert_eq!(vec![60, 50, 100], samples(log, "L4"));
        // the last focus has no key within a second
        assert_eq!(vec![250], samples(log, "L2 focus"));
        assert!(samples(log, "L2 next").is_empty());
    }

    #[test]
    fn a_key_typed_into_another_pane_is_timed_to_its_focus_and_held_input() {
        // the key makes the switch, and is held behind it
        let log = "\
n trace input 0 100 40
n trace focus 0 1030
n trace held_input 1 1900 1000
n trace focus 0 3010
n trace held_input 0 3400 3000
";
        assert_eq!(vec![30, 10], samples(log, "L2 focus"));
        assert_eq!(vec![900, 400], samples(log, "L2 next"));
        assert_eq!(vec![60], samples(log, "L4"));
    }

    #[test]
    fn a_tab_move_is_timed_to_the_last_frame_before_the_next() {
        let log = "\
n trace tab_move 4 1000
n trace frame 4 1500
n trace frame 5 2600
n trace tab_move 1 3000
n trace frame 1 3100
n trace frame 2 2000000000
";
        assert_eq!(vec![1600, 100], samples(log, "L3"));

        // `gt`: Neovim draws, then says its current window changed
        let log = "\
n trace input 0 1000 900
n trace frame 0 1100
n trace frame 3 1300
n trace tab_move 3 1400
n trace frame 3 1500
n trace input 3 9000 8000
n trace frame 3 9100
";
        assert_eq!(vec![600], samples(log, "L3"));
    }

    #[test]
    fn a_resize_is_timed_to_its_windows_resize_if_one_was_sent() {
        let log = "\
n trace resize 1 100
n trace resize 1 200
n trace resize_sent 1 260
n trace resize 2 300
n trace resize_sent 1 350
";
        assert_eq!(vec![60], samples(log, "L5"));
    }

    #[test]
    fn exec_is_timed_to_the_first_frame_and_the_second_panes() {
        let log = "\
n trace exec 77 1000
n trace frame 0 4000
n trace frame 0 4500
n trace frame 1 9000
n trace exec 78 20000
n trace frame 0 21000
";
        assert_eq!(vec![3000, 1000], samples(log, "L6 first"));
        assert_eq!(vec![8000], samples(log, "L6 second"));
    }

    #[test]
    fn placement_is_timed_to_the_answer_after_the_last_ask() {
        let log = "\
n trace place 2 100
n trace place 2 200
n trace placed 0 350
n trace placed 0 400
";
        assert_eq!(vec![150], samples(log, "L8"));
    }

    /// A log of `scripts/latency.sh switch 55`, from the baseline's build.
    #[test]
    fn a_captured_log_reads_as_its_table() {
        let log = include_str!("../../tests/fixtures/latency-switch.log");
        let table = report(&measure(&parse(log)));
        let row = |name: &str| -> Vec<String> {
            let line = table.lines().find(|l| l.starts_with(name)).unwrap();
            let cols = line[name.len()..].split_whitespace().take(3);
            cols.map(str::to_string).collect()
        };
        assert_eq!(["55", "0.024", "0.043"], row("L2 focus")[..], "{table}");
        assert_eq!(["55", "0.384", "0.607"], row("L2 next")[..], "{table}");
        assert_eq!(["2", "0.010", "0.011"], row("L4")[..], "{table}");
        assert_eq!(["0", "-", "-"], row("L3")[..], "{table}");
    }

    #[test]
    fn the_report_has_a_row_per_criterion_with_n_median_and_p95() {
        let mut log = String::new();
        for i in 0..50u64 {
            // reads 10 µs to 500 µs before their writes
            let t = 1_000_000 * (i + 1);
            log.push_str(&format!("n trace input 0 {t} {}\n", t - 10_000 * (i + 1)));
        }
        let table = report(&measure(&parse(&log)));
        let rows: Vec<&str> = table.lines().collect();
        assert_eq!(12, rows.len(), "{table}");
        assert!(rows[0].starts_with("criterion"));
        let l4: Vec<&str> = rows[5].split_whitespace().collect();
        // median of 10..=500 µs by tens: (250 + 260) / 2; p95: rank 48
        assert_eq!(["L4", "50", "0.255", "0.480"], l4[..4]);
        let l7: Vec<&str> = rows[9].split_whitespace().collect();
        assert_eq!(["L7", "0", "-", "-"], l7[..4]);
    }
}
