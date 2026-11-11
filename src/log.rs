//! The log: `<NAME>_LOG`'s levels, and the trace timestamps the latency
//! checks are read from (`docs/latency.md`).
//!
//! The daemon's stderr is its log. A line goes there only if its level is
//! on; `log_warn!`, `log_info!` and `log_debug!` write them. Traces are kept
//! at `debug` only: each is a line in a buffer, written with the next log
//! line, when the buffer fills, or by `flush_traces`. Off, a trace is one
//! atomic load: no syscall, no allocation.

use std::ffi::OsStr;
use std::io::{self, Write};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Instant;

use crate::name::{self, NAME};

/// How much the log says, least first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum LogLevel {
    /// What went wrong.
    Warn,
    /// And what happened: the default.
    Info,
    /// And the trace timestamps.
    Debug,
}

impl LogLevel {
    /// The level `<NAME>_LOG`'s `value` names: `info` when unset or empty.
    /// For any other value, `info` and a warning to log.
    pub fn parse(value: Option<&OsStr>) -> (LogLevel, Option<String>) {
        let Some(v) = value.filter(|v| !v.is_empty()) else {
            return (LogLevel::Info, None);
        };
        for level in [LogLevel::Warn, LogLevel::Info, LogLevel::Debug] {
            if v.eq_ignore_ascii_case(level.tag_name()) {
                return (level, None);
            }
        }
        let warning = format!(
            "{} is {:?}, not warn, info or debug; using info",
            env_var(),
            v.to_string_lossy()
        );
        (LogLevel::Info, Some(warning))
    }

    /// The level's name, as `<NAME>_LOG` takes it.
    pub fn tag_name(self) -> &'static str {
        match self {
            LogLevel::Warn => "warn",
            LogLevel::Info => "info",
            LogLevel::Debug => "debug",
        }
    }
}

/// The environment variable that sets the level: `<NAME>_LOG`.
pub fn env_var() -> String {
    format!("{}_LOG", name::upper())
}

static LEVEL: AtomicU8 = AtomicU8::new(LogLevel::Info as u8);

static TRACES: Mutex<Traces> = Mutex::new(Traces::new());

/// The level `<NAME>_LOG` names, and the warning for a value it isn't one.
pub fn from_env() -> (LogLevel, Option<String>) {
    LogLevel::parse(std::env::var_os(env_var()).as_deref())
}

/// Sets the daemon's level from `<NAME>_LOG`, logging a warning for a value
/// it isn't. Read once, at start.
pub fn init_daemon() -> LogLevel {
    let (level, warning) = from_env();
    set_level(level);
    if let Some(w) = warning {
        line(LogLevel::Warn, format_args!("{NAME} daemon: {w}"));
    }
    level
}

/// Sets the level. At `debug`, makes room for the traces once, up front.
pub fn set_level(level: LogLevel) {
    LEVEL.store(level as u8, Ordering::Relaxed);
    lock_traces().set_on(level >= LogLevel::Debug);
}

/// Whether lines at `level` are logged.
#[inline]
pub fn enabled(level: LogLevel) -> bool {
    LEVEL.load(Ordering::Relaxed) >= level as u8
}

/// Whether traces are kept: the level is `debug`.
#[inline]
pub fn tracing() -> bool {
    enabled(LogLevel::Debug)
}

/// Writes a log line at `level`, if it's on, after the traces waiting. (See
/// `log_warn!`, `log_info!` and `log_debug!`.)
pub fn line(level: LogLevel, args: std::fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let mut err = io::stderr().lock();
    // a failed log write has nowhere to be told
    let _ = lock_traces().write_into(&mut err);
    let _ = writeln!(err, "{args}");
}

/// Logs a line at `warn`: something went wrong.
macro_rules! log_warn {
    ($($arg:tt)*) => {
        $crate::log::line($crate::log::LogLevel::Warn, format_args!($($arg)*))
    };
}

/// Logs a line at `info`: something happened.
macro_rules! log_info {
    ($($arg:tt)*) => {
        $crate::log::line($crate::log::LogLevel::Info, format_args!($($arg)*))
    };
}

/// Logs a line at `debug`.
#[allow(unused_macros)]
macro_rules! log_debug {
    ($($arg:tt)*) => {
        $crate::log::line($crate::log::LogLevel::Debug, format_args!($($arg)*))
    };
}

#[allow(unused_imports)]
pub(crate) use {log_debug, log_info, log_warn};

/// A moment a latency criterion is timed from or to (`docs/latency.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Point {
    /// L6: the launcher started. Its id is the launcher's pid.
    Exec,
    /// L1: a new pane's split asked for.
    Split,
    /// L1, L3, L6: a frame written to a pane.
    Frame,
    /// L4: a read's keys written to Neovim as `nvim_input`, straight away.
    /// Its `from` is when the read came.
    Input,
    /// L2: as `Input`, for a read held behind a window switch.
    HeldInput,
    /// L2: a focus request written to Rex.
    Focus,
    /// L3: a pane of another tab page became the focused one.
    TabMove,
    /// L5: a pane's terminal was resized.
    Resize,
    /// L5: the pane's window asked to resize to fit it.
    ResizeSent,
    /// L8: the panes of a tab page asked to be placed.
    Place,
    /// L8: the last placement answered.
    Placed,
    /// L7: a pane accepted, its find asked for. Fires from #114.
    Accept,
    /// L7: the find confirmed. Fires from #114.
    Found,
    /// L9: a hello received. Fires from #101.
    Hello,
    /// L9: the hello accepted. Fires from #101.
    Accepted,
}

impl Point {
    pub const ALL: [Point; 15] = [
        Point::Exec,
        Point::Split,
        Point::Frame,
        Point::Input,
        Point::HeldInput,
        Point::Focus,
        Point::TabMove,
        Point::Resize,
        Point::ResizeSent,
        Point::Place,
        Point::Placed,
        Point::Accept,
        Point::Found,
        Point::Hello,
        Point::Accepted,
    ];

    /// The point's name, as traced.
    pub fn tag_name(self) -> &'static str {
        match self {
            Point::Exec => "exec",
            Point::Split => "split",
            Point::Frame => "frame",
            Point::Input => "input",
            Point::HeldInput => "held_input",
            Point::Focus => "focus",
            Point::TabMove => "tab_move",
            Point::Resize => "resize",
            Point::ResizeSent => "resize_sent",
            Point::Place => "place",
            Point::Placed => "placed",
            Point::Accept => "accept",
            Point::Found => "found",
            Point::Hello => "hello",
            Point::Accepted => "accepted",
        }
    }

    /// The point named `tag`, as `tag_name` gives it.
    pub fn from_tag(tag: &str) -> Option<Point> {
        Point::ALL.into_iter().find(|p| p.tag_name() == tag)
    }
}

/// Traces `point` for `id` (a pane, mostly) now, at `debug`.
#[inline]
pub fn trace(point: Point, id: u64) {
    if tracing() {
        trace_at(point, id, now_ns(), None);
    }
}

/// Traces `point` for `id` now, timed from `from`, at `debug`.
#[inline]
pub fn trace_since(point: Point, id: u64, from: Instant) {
    if tracing() {
        let now = now_ns();
        let ago = u64::try_from(from.elapsed().as_nanos()).unwrap_or(u64::MAX);
        trace_at(point, id, now, Some(now.saturating_sub(ago)));
    }
}

/// Traces `point` for `id` at `t`, from `from`, both `now_ns` times, if
/// traces are kept. A full buffer is written out.
pub fn trace_at(point: Point, id: u64, t: u64, from: Option<u64>) {
    let mut traces = lock_traces();
    traces.record(point, id, t, from);
    if traces.is_full() {
        let _ = traces.write_into(&mut io::stderr().lock());
    }
}

/// Writes out the traces waiting, as the daemon ends.
pub fn flush_traces() {
    let _ = lock_traces().write_into(&mut io::stderr().lock());
}

/// The monotonic clock `Instant` reads, in nanoseconds: the same in every
/// process, so the launcher's traces line up with the daemon's. (macOS's
/// `CLOCK_MONOTONIC` counts only microseconds.)
pub fn now_ns() -> u64 {
    #[cfg(target_vendor = "apple")]
    const CLOCK: libc::clockid_t = libc::CLOCK_UPTIME_RAW;
    #[cfg(not(target_vendor = "apple"))]
    const CLOCK: libc::clockid_t = libc::CLOCK_MONOTONIC;
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid timespec to fill.
    unsafe { libc::clock_gettime(CLOCK, &mut ts) };
    (ts.tv_sec as u64)
        .saturating_mul(1_000_000_000)
        .saturating_add(ts.tv_nsec as u64)
}

fn lock_traces() -> std::sync::MutexGuard<'static, Traces> {
    // a panic mid-trace leaves a buffer still good to use
    TRACES.lock().unwrap_or_else(|e| e.into_inner())
}

/// Room for the traces between writes: some hundreds of lines.
const TRACES_CAPACITY: usize = 32 * 1024;

/// Trace lines waiting to be written. Off, it keeps nothing and holds no
/// memory.
#[derive(Debug)]
pub struct Traces {
    on: bool,
    buf: Vec<u8>,
}

impl Default for Traces {
    fn default() -> Self {
        Traces::new()
    }
}

impl Traces {
    /// Off, and empty.
    pub const fn new() -> Traces {
        Traces {
            on: false,
            buf: Vec::new(),
        }
    }

    /// Turns keeping traces on, with room for them, or off.
    pub fn set_on(&mut self, on: bool) {
        self.on = on;
        if on {
            self.buf.reserve(TRACES_CAPACITY);
        }
    }

    /// Keeps the line for `point`, if on: `<name> trace <point> <id> <t>`,
    /// then ` <from>` if it has one.
    pub fn record(&mut self, point: Point, id: u64, t: u64, from: Option<u64>) {
        if !self.on {
            return;
        }
        // writing to a Vec doesn't fail
        let _ = write!(self.buf, "{NAME} trace {} {id} {t}", point.tag_name());
        if let Some(f) = from {
            let _ = write!(self.buf, " {f}");
        }
        self.buf.push(b'\n');
    }

    /// Whether the buffer should be written out before it grows.
    pub fn is_full(&self) -> bool {
        self.buf.len() >= TRACES_CAPACITY - 256
    }

    /// The lines kept.
    pub fn pending(&self) -> &[u8] {
        &self.buf
    }

    /// Writes the lines kept to `w`, in one write, and empties the buffer.
    pub fn write_into(&mut self, w: &mut impl Write) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let r = w.write_all(&self.buf);
        self.buf.clear();
        r
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn parse(v: &str) -> (LogLevel, Option<String>) {
        LogLevel::parse(Some(OsString::from(v).as_os_str()))
    }

    #[test]
    fn the_log_level_is_info_unless_set_to_a_level() {
        assert_eq!((LogLevel::Info, None), LogLevel::parse(None));
        assert_eq!((LogLevel::Info, None), parse(""));
        assert_eq!((LogLevel::Warn, None), parse("warn"));
        assert_eq!((LogLevel::Info, None), parse("info"));
        assert_eq!((LogLevel::Debug, None), parse("debug"));
        assert_eq!((LogLevel::Debug, None), parse("DEBUG"));
    }

    #[test]
    fn an_unknown_log_level_warns_and_is_info() {
        let (level, warning) = parse("trace");
        assert_eq!(LogLevel::Info, level);
        let w = warning.expect("a warning");
        assert!(w.starts_with(&format!("{}_LOG", name::upper())), "{w}");
        assert!(w.contains("\"trace\""), "{w}");
        assert!(w.ends_with("using info"), "{w}");

        // not Unicode
        use std::os::unix::ffi::OsStrExt;
        let (level, warning) = LogLevel::parse(Some(OsStr::from_bytes(b"\xff")));
        assert_eq!(LogLevel::Info, level);
        assert!(warning.is_some());
    }

    #[test]
    fn levels_order_from_warn_to_debug() {
        assert!(LogLevel::Warn < LogLevel::Info && LogLevel::Info < LogLevel::Debug);
        for p in Point::ALL {
            assert_eq!(Some(p), Point::from_tag(p.tag_name()));
        }
        assert_eq!(None, Point::from_tag("nothing"));
    }

    /// A writer that counts the writes made to it, as syscalls would be.
    #[derive(Default)]
    struct Counted {
        writes: usize,
        bytes: Vec<u8>,
    }

    impl Write for Counted {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.writes += 1;
            self.bytes.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn at_info_no_trace_is_kept_or_written_and_no_memory_taken() {
        let mut traces = Traces::new();
        traces.set_on(LogLevel::Info >= LogLevel::Debug);
        let mut out = Counted::default();
        for key in 0..1000 {
            traces.record(Point::Input, 1, key, Some(key));
            traces.write_into(&mut out).unwrap();
        }
        assert_eq!(0, traces.buf.capacity());
        assert_eq!(0, out.writes);
        assert!(out.bytes.is_empty());
    }

    #[test]
    fn at_debug_traces_wait_in_the_buffer_and_go_in_one_write() {
        let mut traces = Traces::new();
        traces.set_on(true);
        let room = traces.buf.capacity();
        let mut out = Counted::default();
        for key in 0..100 {
            traces.record(Point::Input, 2, 1000 + key, Some(900 + key));
        }
        traces.record(Point::Focus, 0, 5000, None);
        assert!(!traces.is_full());
        // kept in the room made up front
        assert_eq!(room, traces.buf.capacity());
        assert_eq!(0, out.writes);

        traces.write_into(&mut out).unwrap();
        assert_eq!(1, out.writes);
        assert!(traces.pending().is_empty());
        let text = String::from_utf8(out.bytes).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(101, lines.len());
        assert_eq!(format!("{NAME} trace input 2 1000 900"), lines[0]);
        assert_eq!(format!("{NAME} trace focus 0 5000"), lines[100]);
    }

    #[test]
    fn the_clock_runs_forward() {
        let a = now_ns();
        let b = now_ns();
        assert!(a > 0 && b >= a);
    }
}
