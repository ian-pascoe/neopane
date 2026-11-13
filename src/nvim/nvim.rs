//! A connection to Neovim speaking msgpack-RPC over an embedded child's stdin
//! and stdout.

use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
#[cfg(test)]
use std::os::unix::net::UnixStream;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use super::framer::{self, Framer};
use super::msgpack;
use super::rpc;

#[derive(Debug)]
#[doc(hidden)]
pub enum Error {
    Io(io::Error),
    Framer(framer::Error),
    Rpc(rpc::Error),
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e)
    }
}

impl From<framer::Error> for Error {
    fn from(e: framer::Error) -> Self {
        Error::Framer(e)
    }
}

impl From<rpc::Error> for Error {
    fn from(e: rpc::Error) -> Self {
        Error::Rpc(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "{e}"),
            Error::Framer(e) => write!(f, "{e}"),
            Error::Rpc(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {}

/// One whole message's bytes, owned, so handling it doesn't borrow the
/// `Nvim` it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
#[doc(hidden)]
pub struct Frame(pub Vec<u8>);

impl Frame {
    /// The message, pointing into the frame's bytes.
    pub fn message(&self) -> rpc::Result<rpc::Message<'_>> {
        rpc::parse(&self.0)
    }
}

#[doc(hidden)]
pub struct Nvim {
    /// None when someone else owns Neovim's process (`from_pipes`).
    pub(crate) child: Option<Child>,
    /// Neovim's stdout.
    reader: File,
    /// Neovim's stdin. None once closed.
    writer: Option<File>,
    /// Buffered writes to Neovim's stdin. Send with `flush`.
    out: Vec<u8>,
    /// Splits Neovim's stdout into messages.
    framer: Framer,
    pub(crate) next_id: u32,
    /// The UI size last sent, as columns and rows.
    pub(crate) ui_size: [u32; 2],
}

/// Options for `ui_attach`. ext_linegrid and rgb are always on.
#[derive(Debug, Clone, Copy, Default)]
#[doc(hidden)]
pub struct UiOptions {
    /// Unset default colors arrive as -1, so the terminal's own are used.
    pub ext_termcolors: bool,
    /// A grid per window; see `Grids`.
    pub ext_multigrid: bool,
}

impl Nvim {
    fn new(child: Option<Child>, reader: File, writer: File) -> Nvim {
        Nvim {
            child,
            reader,
            writer: Some(writer),
            out: Vec::new(),
            framer: Framer::default(),
            next_id: 1,
            ui_size: [0, 0],
        }
    }

    /// Starts `argv`, which must include `--embed`. Neovim's stderr is
    /// inherited.
    pub fn spawn<S: AsRef<OsStr>>(argv: &[S]) -> io::Result<Nvim> {
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()?;
        let stdin = OwnedFd::from(child.stdin.take().unwrap());
        let stdout = OwnedFd::from(child.stdout.take().unwrap());
        Ok(Nvim::new(
            Some(child),
            File::from(stdout),
            File::from(stdin),
        ))
    }

    /// Talks to an embedded Neovim over its `stdin` and `stdout`, its process
    /// owned by the caller. Dropping it closes the channel, which makes
    /// Neovim exit.
    pub(crate) fn from_pipes(stdin: ChildStdin, stdout: ChildStdout) -> Nvim {
        Nvim::new(
            None,
            File::from(OwnedFd::from(stdout)),
            File::from(OwnedFd::from(stdin)),
        )
    }

    /// Writes a request's header and returns its id. Write `param_count` params
    /// to `writer()` after it, then `flush`.
    pub fn request(&mut self, method: &str, param_count: u32) -> u32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        rpc::write_request_header(&mut self.out, id, method, param_count);
        id
    }

    /// Where params go, after `request`. Sent by `flush`.
    pub fn writer(&mut self) -> &mut Vec<u8> {
        &mut self.out
    }

    /// Sends everything written so far. Neovim's stdin found closed stays
    /// closed (`is_closed`).
    pub fn flush(&mut self) -> io::Result<()> {
        if self.out.is_empty() {
            return Ok(());
        }
        let result = match &mut self.writer {
            Some(w) => w.write_all(&self.out),
            None => Err(io::Error::from(io::ErrorKind::BrokenPipe)),
        };
        self.out.clear();
        if matches!(&result, Err(e) if e.kind() == io::ErrorKind::BrokenPipe) {
            self.writer = None;
        }
        result
    }

    /// Whether Neovim's stdin is closed: a write found it so, as Neovim
    /// exited or is exiting.
    pub fn is_closed(&self) -> bool {
        self.writer.is_none()
    }

    /// Answers request `id` with an error, so Neovim stops waiting, and flushes.
    pub fn respond_error(&mut self, id: u32, message: &str) -> io::Result<()> {
        let w = &mut self.out;
        msgpack::write_array_header(w, 4);
        msgpack::write_int(w, 1);
        msgpack::write_int(w, id);
        msgpack::write_str(w, message);
        msgpack::write_nil(w);
        self.flush()
    }

    /// Attaches as a UI of `cols` x `rows` and flushes.
    pub fn ui_attach(&mut self, cols: u32, rows: u32, options: UiOptions) -> io::Result<()> {
        self.request("nvim_ui_attach", 3);
        let w = &mut self.out;
        msgpack::write_int(w, cols);
        msgpack::write_int(w, rows);
        msgpack::write_map_header(w, 4);
        msgpack::write_str(w, "ext_linegrid");
        msgpack::write_bool(w, true);
        msgpack::write_str(w, "rgb");
        msgpack::write_bool(w, true);
        msgpack::write_str(w, "ext_termcolors");
        msgpack::write_bool(w, options.ext_termcolors);
        msgpack::write_str(w, "ext_multigrid");
        msgpack::write_bool(w, options.ext_multigrid);
        self.flush()?;
        self.ui_size = [cols, rows];
        Ok(())
    }

    /// Asks to resize the UI and flushes. Neovim answers with grid_resize. Does
    /// nothing for the size it already has: every resize redraws the whole screen.
    pub(crate) fn ui_try_resize(&mut self, cols: u32, rows: u32) -> io::Result<()> {
        if self.ui_size == [cols, rows] {
            return Ok(());
        }
        self.request("nvim_ui_try_resize", 2);
        msgpack::write_int(&mut self.out, cols);
        msgpack::write_int(&mut self.out, rows);
        self.flush()?;
        self.ui_size = [cols, rows];
        Ok(())
    }

    /// Starts `nvim_exec_lua(code, args)` and returns its id. Write `arg_count`
    /// args to `writer()` after it, then `flush`.
    pub(crate) fn exec_lua(&mut self, code: &str, arg_count: u32) -> u32 {
        let id = self.request("nvim_exec_lua", 2);
        msgpack::write_str(&mut self.out, code);
        msgpack::write_array_header(&mut self.out, arg_count);
        id
    }

    /// Neovim's stdout. Poll it; readable when `read` won't block.
    pub fn read_fd(&self) -> RawFd {
        self.reader.as_raw_fd()
    }

    /// Reads what Neovim has sent, blocking until something arrives. False once
    /// Neovim has exited.
    pub fn read(&mut self) -> io::Result<bool> {
        let mut buf = vec![0u8; 64 * 1024];
        let n = loop {
            match self.reader.read(&mut buf) {
                Ok(n) => break n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        };
        if n == 0 {
            return Ok(false);
        }
        self.framer.feed(&buf[..n]);
        Ok(true)
    }

    /// The next whole message, or None until `read` gets more.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> std::result::Result<Option<Frame>, framer::Error> {
        Ok(self.framer.next()?.map(|bytes| Frame(bytes.to_vec())))
    }
}

impl Nvim {
    /// Closes Neovim's stdin, which makes it exit, and waits up to `limit`
    /// for it. Whether it exited; if not, it's killed. For tests: one stuck
    /// as it exits, at a prompt no one can answer, fails rather than hangs.
    #[cfg(test)]
    pub(crate) fn close_within(&mut self, limit: std::time::Duration) -> bool {
        let _ = self.flush();
        self.writer = None;
        let Some(child) = &mut self.child else {
            return true;
        };
        let start = std::time::Instant::now();
        while start.elapsed() < limit {
            if let Ok(Some(_)) = child.try_wait() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = child.kill();
        let _ = child.wait();
        false
    }
}

impl Drop for Nvim {
    /// Closes Neovim's stdin, which makes it exit, then waits for it if it's
    /// ours.
    fn drop(&mut self) {
        let _ = self.flush();
        // close stdin ourselves; wait closes the rest
        self.writer = None;
        if let Some(child) = &mut self.child {
            let _ = child.wait();
        }
    }
}

/// Whether a missing Neovim must fail the tests rather than skip them: `ci`,
/// the `CI` environment variable, is set and not empty.
#[cfg(test)]
fn nvim_required(ci: Option<&OsStr>) -> bool {
    ci.is_some_and(|v| !v.is_empty())
}

/// What a test does when `nvim` isn't installed: skip (None) after a note,
/// or panic when `CI` is set, so a CI run can't pass without Neovim.
#[cfg(test)]
pub(crate) fn nvim_missing<T>() -> Option<T> {
    if nvim_required(std::env::var_os("CI").as_deref()) {
        panic!("nvim isn't installed, and CI is set: the tests that need Neovim can't be skipped");
    }
    eprintln!("skipped: nvim isn't installed");
    None
}

/// Starts `argv` for a test, or None if Neovim isn't installed (a panic
/// under `CI`).
#[cfg(test)]
pub(crate) fn spawn_for_test_with<S: AsRef<OsStr>>(argv: &[S]) -> Option<Nvim> {
    match Nvim::spawn(argv) {
        Ok(nvim) => Some(nvim),
        Err(e) if e.kind() == io::ErrorKind::NotFound => nvim_missing(),
        Err(e) => panic!("spawning nvim: {e}"),
    }
}

/// A connection over `stream`, for tests that need an `Nvim` but no Neovim
/// to talk to.
#[cfg(test)]
pub(crate) fn for_test_on(stream: UnixStream) -> Nvim {
    let writer = OwnedFd::from(stream.try_clone().unwrap());
    Nvim::new(None, File::from(OwnedFd::from(stream)), File::from(writer))
}

/// Starts `nvim --clean --embed` for a test, or None if it isn't installed
/// (a panic under `CI`).
#[cfg(test)]
pub(crate) fn spawn_for_test() -> Option<Nvim> {
    spawn_for_test_with(&["nvim", "--clean", "--embed"])
}

/// Reads until the response to `id`, skipping other messages. Returns the
/// frame holding it.
#[cfg(test)]
pub(crate) fn wait_response(nvim: &mut Nvim, id: u32) -> Frame {
    loop {
        while let Some(frame) = nvim.next().unwrap() {
            if let rpc::Message::Response { id: got, .. } = frame.message().unwrap()
                && got == id
            {
                return frame;
            }
        }
        assert!(nvim.read().unwrap(), "Neovim exited");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The error and result of the response in `frame`.
    fn response(frame: &Frame) -> (Option<&[u8]>, msgpack::Decoder<'_>) {
        match frame.message().unwrap() {
            rpc::Message::Response { err, result, .. } => (err, result),
            _ => unreachable!(),
        }
    }

    fn eval_1_plus_2(nvim: &mut Nvim) {
        let id = nvim.request("nvim_eval", 1);
        msgpack::write_str(nvim.writer(), "1+2");
        nvim.flush().unwrap();
        let frame = wait_response(nvim, id);
        let (err, mut result) = response(&frame);
        assert_eq!(err, None);
        assert_eq!(rpc::expect_int(&mut result), Ok(3));
    }

    #[test]
    fn eval() {
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        eval_1_plus_2(&mut nvim);
    }

    #[test]
    fn error_response() {
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        let id = nvim.request("nvim_eval", 1);
        msgpack::write_str(nvim.writer(), "no_such_variable");
        nvim.flush().unwrap();

        let frame = wait_response(&mut nvim, id);
        let (err, _) = response(&frame);
        let err = String::from_utf8_lossy(err.unwrap());
        assert!(err.contains("no_such_variable"), "{err}");
    }

    #[test]
    fn a_resize_to_the_same_size_isnt_sent() {
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        nvim.ui_attach(40, 10, UiOptions::default()).unwrap();
        let before = nvim.next_id;
        nvim.ui_try_resize(40, 10).unwrap();
        assert_eq!(nvim.next_id, before);
        nvim.ui_try_resize(30, 8).unwrap();
        assert_eq!(nvim.next_id, before + 1);
    }

    #[test]
    fn ci_makes_a_missing_nvim_fatal_unless_empty_or_unset() {
        assert!(nvim_required(Some(OsStr::new("true"))));
        assert!(nvim_required(Some(OsStr::new("1"))));
        assert!(!nvim_required(Some(OsStr::new(""))));
        assert!(!nvim_required(None));
    }

    #[test]
    fn large_reply_over_several_reads() {
        let Some(mut nvim) = spawn_for_test() else {
            return;
        };
        nvim.request("nvim_get_api_info", 0);
        let id = nvim.request("nvim_get_api_info", 0); // two in flight
        nvim.flush().unwrap();

        // [channel id, {version, functions, ...}]
        let frame = wait_response(&mut nvim, id);
        let (_, mut result) = response(&frame);
        assert_eq!(rpc::expect_array(&mut result), Ok(2));
        assert!(rpc::expect_int(&mut result).unwrap() > 0);
        rpc::skip(&mut result).unwrap();
    }
}
