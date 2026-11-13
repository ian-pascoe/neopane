//! The payloads of the eight frame kinds: msgpack maps with `str` keys for
//! control messages, raw bytes for `i` and `o`. `docs/attach-protocol.md`
//! is the reference.

use super::frame::Frame;
use super::value::{Fields, MapWriter, Value};
use super::{ATTACH_PROTOCOL_VERSION, Error, PaneId, build};

/// The most the undo bytes in a `u` frame may hold.
pub const UNDO_LIMIT: usize = 4 << 10;

/// One frame's meaning.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    /// `h`: the helper's first frame.
    Hello(Hello),
    /// `s`: the terminal's size changed.
    Size(Size),
    /// `i`: input bytes, relay only.
    Input(Vec<u8>),
    /// `a`: the daemon accepts the hello.
    Accept(Accept),
    /// `u`: the bytes that undo what the daemon writes.
    Undo(Vec<u8>),
    /// `o`: output bytes, relay only.
    Output(Vec<u8>),
    /// `r`: the answer to a control request.
    Reply(Reply),
    /// `z`: the last frame: a refusal before `a`, an ending after it.
    End(End),
}

impl Message {
    pub fn kind(&self) -> u8 {
        match self {
            Message::Hello(_) => b'h',
            Message::Size(_) => b's',
            Message::Input(_) => b'i',
            Message::Accept(_) => b'a',
            Message::Undo(_) => b'u',
            Message::Output(_) => b'o',
            Message::Reply(_) => b'r',
            Message::End(_) => b'z',
        }
    }

    /// The frame's payload.
    pub fn payload(&self) -> Result<Vec<u8>, Error> {
        Ok(match self {
            Message::Hello(h) => h.payload(),
            Message::Size(s) => s.writer().finish(),
            Message::Input(b) | Message::Output(b) => b.clone(),
            Message::Accept(a) => a.payload(),
            Message::Undo(b) => {
                if b.len() > UNDO_LIMIT {
                    return Err(Error::Invalid("undo"));
                }
                let mut w = MapWriter::default();
                w.bin("undo", b);
                w.finish()
            }
            Message::Reply(r) => r.payload(),
            Message::End(z) => z.payload(),
        })
    }

    /// The whole frame's bytes. Over a limit is an error: `i` and `o` must
    /// be split first (`frame::encode_split`).
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        Frame::encode_into(self.kind(), &self.payload()?, &mut out)?;
        Ok(out)
    }

    /// Reads any frame after the hello. None is a kind this build doesn't
    /// know, which the reader skips.
    pub fn decode(frame: &Frame) -> Result<Option<Message>, Error> {
        let p = &frame.payload[..];
        Ok(Some(match frame.kind {
            b'h' => Message::Hello(Hello::decode(p)?),
            b's' => Message::Size(Size::read(&Fields::decode(p)?)?),
            b'i' => Message::Input(p.to_vec()),
            b'a' => Message::Accept(Accept::decode(p)?),
            b'u' => {
                let undo = Fields::decode(p)?.bin("undo")?;
                if undo.len() > UNDO_LIMIT {
                    return Err(Error::Invalid("undo"));
                }
                Message::Undo(undo)
            }
            b'o' => Message::Output(p.to_vec()),
            b'r' => Message::Reply(Reply::decode(p)?),
            b'z' => Message::End(End::decode(p)?),
            _ => return Ok(None),
        }))
    }

    /// Reads a connection's first frame, which must be a hello.
    pub fn decode_hello(frame: &Frame) -> Result<Hello, Error> {
        if frame.kind != b'h' {
            return Err(Error::NotHello(frame.kind));
        }
        Hello::decode(&frame.payload)
    }
}

/// How an attach's terminal reaches the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transport {
    /// Handed over with SCM_RIGHTS on the hello's first byte.
    Fd,
    /// Copied in `i` and `o` frames.
    Relay,
}

impl Transport {
    pub const ALL: [Transport; 2] = [Transport::Fd, Transport::Relay];

    pub fn name(self) -> &'static str {
        match self {
            Transport::Fd => "fd",
            Transport::Relay => "relay",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.name() == s)
    }

    /// The variable that picks a helper's transport, and the daemon's
    /// requested panes': `<NAME>_TRANSPORT`.
    pub fn env_var() -> String {
        format!("{}_TRANSPORT", crate::name::upper())
    }

    /// The transport `<NAME>_TRANSPORT`'s `value` picks: `fd` when it's
    /// unset or empty. Any other value is `fd` too, with a warning to show.
    pub fn from_env_value(value: Option<&std::ffi::OsStr>) -> (Transport, Option<String>) {
        let Some(value) = value.filter(|v| !v.is_empty()) else {
            return (Transport::Fd, None);
        };
        match value.to_str().and_then(Transport::from_name) {
            Some(t) => (t, None),
            None => {
                let var = Transport::env_var();
                let shown = value.to_string_lossy();
                let warning = format!("ignoring {var}={shown} (use fd or relay)");
                (Transport::Fd, Some(warning))
            }
        }
    }

    /// The transport this process's `<NAME>_TRANSPORT` picks, giving `warn`
    /// the line for a value it can't take: helpers print it, the daemon
    /// logs it.
    pub fn from_env(warn: impl FnOnce(&str)) -> Transport {
        let value = std::env::var_os(Transport::env_var());
        let (transport, warning) = Transport::from_env_value(value.as_deref());
        if let Some(w) = warning {
            warn(&w);
        }
        transport
    }
}

/// A terminal's size: in cells, and its text area in pixels when known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Size {
    pub cols: u16,
    pub rows: u16,
    /// Width and height; None when the terminal reports zero.
    pub px: Option<(u32, u32)>,
}

impl Size {
    fn writer(&self) -> MapWriter {
        let mut w = MapWriter::default();
        w.int("cols", self.cols).int("rows", self.rows);
        if let Some((pw, ph)) = self.px {
            w.int_pair("px", pw, ph);
        }
        w
    }

    fn read(f: &Fields) -> Result<Self, Error> {
        let px = match f.get("px") {
            None | Some(Value::Nil) => None,
            Some(Value::Array(a)) => match a[..] {
                [Value::Int(w), Value::Int(h)] => Some((
                    u32::try_from(w).map_err(|_| Error::Invalid("px"))?,
                    u32::try_from(h).map_err(|_| Error::Invalid("px"))?,
                )),
                _ => return Err(Error::Invalid("px")),
            },
            Some(_) => return Err(Error::Invalid("px")),
        };
        Ok(Size {
            cols: f.int_in("cols")?,
            rows: f.int_in("rows")?,
            px,
        })
    }
}

/// The value of variable `name` in `env`, an environment as a hello carries
/// it, if set.
pub fn env_value<'a>(env: &'a [(Vec<u8>, Vec<u8>)], name: &str) -> Option<&'a [u8]> {
    env.iter()
        .find(|(k, _)| k == name.as_bytes())
        .map(|(_, v)| v.as_slice())
}

/// What an attaching terminal brings: requested and self-attaches alike.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Terminal {
    pub transport: Transport,
    pub size: Size,
    /// The helper's pid.
    pub pid: u32,
    /// The terminal's device path; empty where the platform has none.
    pub tty: Vec<u8>,
    /// The helper's whole environment, in order, each name once.
    pub env: Vec<(Vec<u8>, Vec<u8>)>,
    pub cwd: Vec<u8>,
}

impl Terminal {
    fn write(&self, w: &mut MapWriter) {
        w.str("transport", self.transport.name())
            .map("size", self.size.writer())
            .int("pid", self.pid)
            .bin("tty", &self.tty)
            .bin_map("env", &self.env)
            .bin("cwd", &self.cwd);
    }

    fn read(f: &Fields) -> Result<Self, Error> {
        let transport = f.str("transport")?;
        Ok(Terminal {
            transport: Transport::from_name(&transport).ok_or(Error::Invalid("transport"))?,
            size: Size::read(&f.map("size")?)?,
            pid: f.int_in("pid")?,
            tty: f.bin("tty")?,
            env: f.bin_map("env")?,
            cwd: f.bin("cwd")?,
        })
    }
}

/// Why a hello was sent, with the fields that purpose carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Purpose {
    /// A requested pane's terminal: one the daemon asked the terminal
    /// provider for, for a window or a float group, with its pane id.
    Requested { pane: PaneId, terminal: Terminal },
    /// A terminal attaching of its own accord.
    SelfAttach {
        terminal: Terminal,
        /// `--provider`, else `$<NAME>_PROVIDER`.
        provider: Option<String>,
        /// Absolute, links not resolved.
        file: Option<Vec<u8>>,
        /// The cursor's line in `file`, from 1.
        line: Option<u32>,
    },
    /// A control request for the daemon's state.
    Status { pid: Option<u32> },
    /// A control request to end the daemon; `force` ends it with unsaved
    /// changes.
    Stop { pid: Option<u32>, force: bool },
    /// A control request to open a file in Neovim, answering once Neovim
    /// has it, or once it is done if waited for.
    Open {
        pid: Option<u32>,
        cwd: Vec<u8>,
        /// The caller's terminal, if it has one: the new window splits off
        /// one showing the `:terminal` on it.
        tty: Option<Vec<u8>>,
        open: OpenFile,
    },
}

/// How long an `open` waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wait {
    /// Until Neovim has the window.
    ForWindow,
    /// Until the file is done: a waited file (ADR 0015).
    UntilDone,
}

impl Wait {
    /// `--no-wait`, given or not.
    pub fn unless(no_wait: bool) -> Wait {
        if no_wait {
            Wait::ForWindow
        } else {
            Wait::UntilDone
        }
    }

    pub fn until_done(self) -> bool {
        self == Wait::UntilDone
    }
}

/// A file an `open` opens: `[+LINE] FILE`, and how long it waits.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenFile {
    pub file: Vec<u8>,
    /// The cursor's line in `file`, from 1.
    pub line: Option<u32>,
    pub wait: Wait,
}

impl Purpose {
    pub const NAMES: [&str; 5] = ["requested", "self", "status", "stop", "open"];

    pub fn name(&self) -> &'static str {
        Self::NAMES[match self {
            Purpose::Requested { .. } => 0,
            Purpose::SelfAttach { .. } => 1,
            Purpose::Status { .. } => 2,
            Purpose::Stop { .. } => 3,
            Purpose::Open { .. } => 4,
        }]
    }

    /// Status and stop work across versions.
    fn frozen(name: &str) -> bool {
        matches!(name, "status" | "stop")
    }
}

/// `h`: the helper's first frame, once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub v: i64,
    pub build: String,
    pub purpose: Purpose,
}

impl Hello {
    /// A hello of this build.
    pub fn new(purpose: Purpose) -> Self {
        Hello {
            v: ATTACH_PROTOCOL_VERSION,
            build: build().to_string(),
            purpose,
        }
    }

    fn payload(&self) -> Vec<u8> {
        let mut w = MapWriter::default();
        w.int("v", self.v)
            .str("build", &self.build)
            .str("purpose", self.purpose.name());
        match &self.purpose {
            Purpose::Requested { pane, terminal } => {
                w.str("pane", &pane.to_string());
                terminal.write(&mut w);
            }
            Purpose::SelfAttach {
                terminal,
                provider,
                file,
                line,
            } => {
                terminal.write(&mut w);
                if let Some(p) = provider {
                    w.str("provider", p);
                }
                if let Some(f) = file {
                    w.bin("file", f);
                }
                if let Some(l) = line {
                    w.int("line", *l);
                }
            }
            Purpose::Status { pid } => {
                if let Some(p) = pid {
                    w.int("pid", *p);
                }
            }
            Purpose::Stop { pid, force } => {
                if let Some(p) = pid {
                    w.int("pid", *p);
                }
                if *force {
                    w.bool("force", true);
                }
            }
            Purpose::Open {
                pid,
                cwd,
                tty,
                open,
            } => {
                if let Some(p) = pid {
                    w.int("pid", *p);
                }
                w.bin("cwd", cwd).bin("file", &open.file);
                if let Some(t) = tty {
                    w.bin("tty", t);
                }
                if let Some(l) = open.line {
                    w.int("line", l);
                }
                if open.wait.until_done() {
                    w.bool("wait", true);
                }
            }
        }
        w.finish()
    }

    /// Reads a hello. A hello of another attach protocol version is
    /// `Error::Version`, unless it's a status or stop, whose frozen fields
    /// are read whatever the version.
    pub fn decode(payload: &[u8]) -> Result<Self, Error> {
        let f = Fields::decode(payload)?;
        let v = f.int("v")?;
        let build = f.str("build")?;
        let purpose = f.str("purpose")?;
        // the version first: another version's purposes may be unknown here
        let same = v == ATTACH_PROTOCOL_VERSION;
        if !same && !Purpose::frozen(&purpose) {
            return Err(Error::Version { v, build });
        }
        // pid isn't frozen: another version's is ignored
        let pid = if same { f.opt_int_in("pid")? } else { None };
        let purpose = match purpose.as_str() {
            "requested" => {
                let pane = f.str("pane")?;
                Purpose::Requested {
                    pane: PaneId::parse(&pane).ok_or(Error::Invalid("pane"))?,
                    terminal: Terminal::read(&f)?,
                }
            }
            "self" => {
                let file = f.opt_bin("file")?;
                let line = line(&f, file.is_some())?;
                Purpose::SelfAttach {
                    terminal: Terminal::read(&f)?,
                    provider: f.opt_str("provider")?,
                    file,
                    line,
                }
            }
            "status" => Purpose::Status { pid },
            "stop" => Purpose::Stop {
                pid,
                force: f.opt_bool("force")?.unwrap_or(false),
            },
            "open" => Purpose::Open {
                pid,
                cwd: f.bin("cwd")?,
                tty: f.opt_bin("tty")?,
                open: OpenFile {
                    file: f.bin("file")?,
                    line: line(&f, true)?,
                    wait: match f.opt_bool("wait")? {
                        Some(true) => Wait::UntilDone,
                        _ => Wait::ForWindow,
                    },
                },
            },
            _ => return Err(Error::Invalid("purpose")),
        };
        Ok(Hello { v, build, purpose })
    }

    /// Whether a hello that can't be read says it is a requested pane's.
    pub fn reads_requested(payload: &[u8]) -> bool {
        Fields::decode(payload)
            .and_then(|f| f.str("purpose"))
            .is_ok_and(|p| p == "requested")
    }

    /// The pane a hello that can't be read names, if it reads as a
    /// requested pane's: so the daemon can give up that pane, whatever else
    /// is wrong with the hello.
    pub fn requested_pane(payload: &[u8]) -> Option<PaneId> {
        if !Self::reads_requested(payload) {
            return None;
        }
        let f = Fields::decode(payload).ok()?;
        PaneId::parse(&f.str("pane").ok()?)
    }
}

/// `line`: from 1, and only with a file.
fn line(f: &Fields, has_file: bool) -> Result<Option<u32>, Error> {
    match f.opt_int_in::<u32>("line")? {
        Some(0) => Err(Error::Invalid("line")),
        Some(_) if !has_file => Err(Error::Invalid("line")),
        l => Ok(l),
    }
}

/// `a`: the daemon accepts an attach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accept {
    pub v: i64,
    pub build: String,
    /// The daemon's name.
    pub daemon: String,
    /// The daemon's log, which the helper names when the daemon vanishes.
    pub log: Vec<u8>,
    /// The messages needing an answer this daemon understands.
    pub features: Vec<String>,
}

impl Accept {
    fn payload(&self) -> Vec<u8> {
        let mut w = MapWriter::default();
        w.int("v", self.v)
            .str("build", &self.build)
            .str("daemon", &self.daemon)
            .bin("log", &self.log)
            .strs("features", &self.features);
        w.finish()
    }

    fn decode(payload: &[u8]) -> Result<Self, Error> {
        let f = Fields::decode(payload)?;
        Ok(Accept {
            v: f.int("v")?,
            build: f.str("build")?,
            daemon: f.str("daemon")?,
            log: f.bin("log")?,
            features: f.strs("features")?,
        })
    }
}

/// `r`: the answer to a control request; the daemon closes after it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub ok: bool,
    pub message: Option<String>,
    pub v: i64,
    pub build: String,
    /// The daemon's name.
    pub name: String,
    /// The daemon's pid.
    pub pid: u32,
    /// What `status` adds while versions agree.
    pub status: Option<Status>,
    /// On the answer to a waited `open`.
    pub waited: Option<bool>,
}

/// What a `status` reply adds while versions agree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    pub address: Vec<u8>,
    /// Attached Neovim panes.
    pub panes: u32,
    /// Files with unsaved changes, empty for an unnamed buffer.
    pub unsaved: Vec<Vec<u8>>,
    /// The pane provider's shown name, `(choosing)`, or `none …`.
    pub provider: String,
    /// Neovim's global working directory.
    pub dir: Option<Vec<u8>>,
    /// Started with `--detach`.
    pub kept: Option<bool>,
    pub capabilities: Option<Vec<String>>,
    /// Capabilities found unsupported since the choice.
    pub unsupported: Option<Vec<String>>,
    pub log: Option<Vec<u8>>,
    /// The ptys of Neovim's running `:terminal` jobs.
    pub terminals: Option<Vec<Vec<u8>>>,
}

impl Reply {
    fn payload(&self) -> Vec<u8> {
        let mut w = MapWriter::default();
        w.bool("ok", self.ok);
        if let Some(m) = &self.message {
            w.str("message", m);
        }
        w.int("v", self.v)
            .str("build", &self.build)
            .str("name", &self.name)
            .int("pid", self.pid);
        if let Some(s) = &self.status {
            w.bin("address", &s.address)
                .int("panes", s.panes)
                .bins("unsaved", &s.unsaved)
                .str("provider", &s.provider);
            if let Some(d) = &s.dir {
                w.bin("dir", d);
            }
            if let Some(k) = s.kept {
                w.bool("kept", k);
            }
            if let Some(c) = &s.capabilities {
                w.strs("capabilities", c);
            }
            if let Some(u) = &s.unsupported {
                w.strs("unsupported", u);
            }
            if let Some(l) = &s.log {
                w.bin("log", l);
            }
            if let Some(t) = &s.terminals {
                w.bins("terminals", t);
            }
        }
        if let Some(waited) = self.waited {
            w.bool("waited", waited);
        }
        w.finish()
    }

    /// Reads a reply. The frozen fields are read whatever the version; the
    /// status fields only when versions agree and `address` is there.
    fn decode(payload: &[u8]) -> Result<Self, Error> {
        let f = Fields::decode(payload)?;
        let v = f.int("v")?;
        let same = v == ATTACH_PROTOCOL_VERSION;
        let status = if same && f.get("address").is_some() {
            Some(Status {
                address: f.bin("address")?,
                panes: f.int_in("panes")?,
                unsaved: f.bins("unsaved")?,
                provider: f.str("provider")?,
                dir: f.opt_bin("dir")?,
                kept: f.opt_bool("kept")?,
                capabilities: f.opt_strs("capabilities")?,
                unsupported: f.opt_strs("unsupported")?,
                log: f.opt_bin("log")?,
                terminals: f.opt_bins("terminals")?,
            })
        } else {
            None
        };
        Ok(Reply {
            ok: f.bool("ok")?,
            message: f.opt_str("message")?,
            v,
            build: f.str("build")?,
            name: f.str("name")?,
            pid: f.int_in("pid")?,
            status,
            waited: if same { f.opt_bool("waited")? } else { None },
        })
    }
}

/// Why the daemon refused a hello.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Another attach protocol version.
    Version,
    /// A hello that couldn't be read, or lacks a field.
    Hello,
    /// A transport the daemon doesn't take.
    Transport,
    /// The peer runs as another user.
    User,
    /// Under `fd`, the handed-over file isn't a terminal.
    Terminal,
    /// From a `:terminal` of the daemon's own Neovim.
    Nested,
    /// Ending Neovim has begun.
    Stopping,
    PaneUnknown,
    PaneExpired,
    PaneTaken,
}

impl Refusal {
    pub const ALL: [Refusal; 10] = [
        Refusal::Version,
        Refusal::Hello,
        Refusal::Transport,
        Refusal::User,
        Refusal::Terminal,
        Refusal::Nested,
        Refusal::Stopping,
        Refusal::PaneUnknown,
        Refusal::PaneExpired,
        Refusal::PaneTaken,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Refusal::Version => "version",
            Refusal::Hello => "hello",
            Refusal::Transport => "transport",
            Refusal::User => "user",
            Refusal::Terminal => "terminal",
            Refusal::Nested => "nested",
            Refusal::Stopping => "stopping",
            Refusal::PaneUnknown => "pane-unknown",
            Refusal::PaneExpired => "pane-expired",
            Refusal::PaneTaken => "pane-taken",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.name() == s)
    }
}

/// Why an accepted attach ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// Its pane closed.
    Closed,
    /// Neovim quit.
    Quit,
    /// The daemon was stopped.
    Stopped,
    Failed,
    /// Reserved.
    Detached,
}

impl Ending {
    pub const ALL: [Ending; 5] = [
        Ending::Closed,
        Ending::Quit,
        Ending::Stopped,
        Ending::Failed,
        Ending::Detached,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Ending::Closed => "closed",
            Ending::Quit => "quit",
            Ending::Stopped => "stopped",
            Ending::Failed => "failed",
            Ending::Detached => "detached",
        }
    }

    /// An unknown reason counts as a failure.
    pub fn from_name(s: &str) -> Self {
        Self::ALL
            .into_iter()
            .find(|e| e.name() == s)
            .unwrap_or(Ending::Failed)
    }
}

/// `z`: the last frame. Its `reason` is a `Refusal` before `a` and an
/// `Ending` after it, so it is kept as sent and read by the side that knows
/// which.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct End {
    pub v: i64,
    pub build: String,
    pub reason: String,
    /// One human-readable line.
    pub message: String,
    /// The daemon cut a frame short: the helper writes CAN and `ESC \`.
    pub cut: bool,
    /// Neovim's exit status, only with `quit`.
    pub exit: Option<u8>,
}

impl End {
    /// A refusal from this build.
    pub fn refusal(reason: Refusal, message: impl Into<String>) -> Self {
        Self::new(reason.name(), message.into(), false)
    }

    /// An ending from this build; add `exit` to a `quit`.
    pub fn ending(reason: Ending, message: impl Into<String>, cut: bool) -> Self {
        Self::new(reason.name(), message.into(), cut)
    }

    fn new(reason: &str, message: String, cut: bool) -> Self {
        End {
            v: ATTACH_PROTOCOL_VERSION,
            build: build().to_string(),
            reason: reason.to_string(),
            message,
            cut,
            exit: None,
        }
    }

    /// The refusal, read before `a`. None for a reason this build doesn't
    /// know.
    pub fn as_refusal(&self) -> Option<Refusal> {
        Refusal::from_name(&self.reason)
    }

    /// The ending, read after `a`.
    pub fn as_ending(&self) -> Ending {
        Ending::from_name(&self.reason)
    }

    fn payload(&self) -> Vec<u8> {
        let mut w = MapWriter::default();
        w.int("v", self.v)
            .str("build", &self.build)
            .str("reason", &self.reason)
            .str("message", &self.message);
        if self.cut {
            w.bool("cut", true);
        }
        if let Some(e) = self.exit {
            w.int("exit", e);
        }
        w.finish()
    }

    /// Reads an end. The frozen fields are read whatever the version; a bad
    /// optional field counts as absent, so the reason and message still
    /// reach the helper.
    fn decode(payload: &[u8]) -> Result<Self, Error> {
        let f = Fields::decode(payload)?;
        let v = f.int("v")?;
        let same = v == ATTACH_PROTOCOL_VERSION;
        Ok(End {
            v,
            build: f.str("build")?,
            reason: f.str("reason")?,
            message: f.str("message")?,
            cut: same && f.opt_bool("cut").ok().flatten().unwrap_or(false),
            exit: if same {
                f.opt_int_in("exit").ok().flatten()
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::msgpack;

    fn terminal() -> Terminal {
        Terminal {
            transport: Transport::Relay,
            size: Size {
                cols: 80,
                rows: 24,
                px: Some((800, 480)),
            },
            pid: 4242,
            tty: b"/dev/ttys004".to_vec(),
            env: vec![
                (b"TERM".to_vec(), b"xterm-ghostty".to_vec()),
                (b"\xff".to_vec(), b"not utf-8 \xfe".to_vec()),
            ],
            cwd: b"/home/me".to_vec(),
        }
    }

    fn status() -> Status {
        Status {
            address: b"/tmp/x/work/sock".to_vec(),
            panes: 3,
            unsaved: vec![b"/a".to_vec(), Vec::new()],
            provider: "rex".into(),
            dir: Some(b"/src".to_vec()),
            kept: Some(true),
            capabilities: Some(vec!["close".into(), "find".into()]),
            unsupported: Some(vec!["open_float".into()]),
            log: Some(b"/tmp/x/work/log".to_vec()),
            terminals: Some(vec![b"/dev/ttys009".to_vec()]),
        }
    }

    fn pane() -> PaneId {
        PaneId::parse("12-k3j9x2qpa8").unwrap()
    }

    /// One message of each kind with every field set, then the same with
    /// only the required ones.
    fn every_message() -> Vec<Message> {
        let reply = Reply {
            ok: true,
            message: Some("fine".into()),
            v: ATTACH_PROTOCOL_VERSION,
            build: "x 1.0.0".into(),
            name: "work".into(),
            pid: 77,
            status: Some(status()),
            waited: Some(true),
        };
        let mut quit = End::ending(Ending::Quit, "Neovim quit", true);
        quit.exit = Some(3);
        let mut out = vec![
            Message::Hello(Hello::new(Purpose::Requested {
                pane: pane(),
                terminal: terminal(),
            })),
            Message::Hello(Hello::new(Purpose::SelfAttach {
                terminal: terminal(),
                provider: Some("rex".into()),
                file: Some(b"/home/me/notes.md".to_vec()),
                line: Some(12),
            })),
            Message::Hello(Hello::new(Purpose::Status { pid: Some(1) })),
            Message::Hello(Hello::new(Purpose::Stop {
                pid: Some(1),
                force: true,
            })),
            Message::Hello(Hello::new(Purpose::Open {
                pid: Some(1),
                cwd: b"/home/me".to_vec(),
                tty: Some(b"/dev/ttys004".to_vec()),
                open: OpenFile {
                    file: b"/home/me/a b".to_vec(),
                    line: Some(1),
                    wait: Wait::UntilDone,
                },
            })),
            Message::Size(Size {
                cols: 1,
                rows: u16::MAX,
                px: Some((0, u32::MAX)),
            }),
            Message::Input(b"\x1b[A".to_vec()),
            Message::Accept(Accept {
                v: ATTACH_PROTOCOL_VERSION,
                build: "x 1.0.0".into(),
                daemon: "work".into(),
                log: b"/tmp/log".to_vec(),
                features: vec!["later".into()],
            }),
            Message::Undo(b"\x1b[?2026l".to_vec()),
            Message::Output(vec![0xff; 100]),
            Message::Reply(reply.clone()),
            Message::End(quit),
        ];
        out.extend([
            Message::Hello(Hello::new(Purpose::SelfAttach {
                terminal: Terminal {
                    size: Size {
                        cols: 80,
                        rows: 24,
                        px: None,
                    },
                    env: Vec::new(),
                    tty: Vec::new(),
                    ..terminal()
                },
                provider: None,
                file: None,
                line: None,
            })),
            Message::Hello(Hello::new(Purpose::Status { pid: None })),
            Message::Hello(Hello::new(Purpose::Stop {
                pid: None,
                force: false,
            })),
            Message::Input(Vec::new()),
            Message::Undo(Vec::new()),
            Message::Reply(Reply {
                message: None,
                status: None,
                waited: None,
                ..reply
            }),
            Message::End(End::refusal(Refusal::PaneTaken, "taken")),
        ]);
        out
    }

    fn frame_of(m: &Message) -> Frame {
        let (f, n) = Frame::decode(&m.encode().unwrap()).unwrap().unwrap();
        assert_eq!(n, m.encode().unwrap().len());
        f
    }

    #[test]
    fn a_requested_panes_id_is_read_from_a_hello_that_cant_be() {
        let id = PaneId::parse("4-abcdefghij").unwrap();
        let mut w = MapWriter::default();
        w.int("v", 99)
            .str("purpose", "requested")
            .str("pane", "4-abcdefghij");
        assert_eq!(Some(id), Hello::requested_pane(&w.finish()));
        let mut w = MapWriter::default();
        w.str("purpose", "self").str("pane", "4-abcdefghij");
        assert_eq!(None, Hello::requested_pane(&w.finish()));
        assert_eq!(None, Hello::requested_pane(b"\xc1"));
        let mut w = MapWriter::default();
        w.str("purpose", "requested");
        let payload = w.finish();
        assert!(Hello::reads_requested(&payload));
        assert_eq!(None, Hello::requested_pane(&payload));
        assert!(!Hello::reads_requested(b"\x80"));
    }

    #[test]
    fn every_kind_round_trips() {
        let mut kinds = Vec::new();
        for m in every_message() {
            let f = frame_of(&m);
            assert_eq!(Some(m.clone()), Message::decode(&f).unwrap(), "{m:?}");
            kinds.push(f.kind);
        }
        for k in *b"hsiauorz" {
            assert!(kinds.contains(&k), "{}", k as char);
        }
    }

    /// Writes `v` back as msgpack.
    fn encode_value(v: &Value, w: &mut Vec<u8>) {
        match v {
            Value::Nil | Value::Ext => msgpack::write_nil(w),
            Value::Bool(b) => msgpack::write_bool(w, *b),
            Value::Int(n) => msgpack::write_int(w, *n),
            Value::Uint(_) | Value::Float(_) => unreachable!(),
            Value::Str(s) => msgpack::write_str(w, s),
            Value::Bin(b) => msgpack::write_bin(w, b),
            Value::Array(items) => {
                msgpack::write_array_header(w, items.len() as u32);
                items.iter().for_each(|i| encode_value(i, w));
            }
            Value::Map(pairs) => {
                msgpack::write_map_header(w, pairs.len() as u32);
                for (k, v) in pairs {
                    encode_value(k, w);
                    encode_value(v, w);
                }
            }
        }
    }

    /// `m`'s payload map, changed by `f`.
    fn edited(m: &Message, f: impl FnOnce(&mut Vec<(Value, Value)>)) -> Frame {
        let mut frame = frame_of(m);
        let Value::Map(mut pairs) = super::super::value::decode(&frame.payload).unwrap() else {
            panic!()
        };
        f(&mut pairs);
        frame.payload.clear();
        encode_value(&Value::Map(pairs), &mut frame.payload);
        frame
    }

    fn key(k: &str) -> Value {
        Value::Str(k.as_bytes().to_vec())
    }

    /// The keys of a control message's map, in order.
    fn keys(m: &Message) -> Vec<String> {
        if matches!(m, Message::Input(_) | Message::Output(_)) {
            return Vec::new();
        }
        let Value::Map(pairs) = super::super::value::decode(&m.payload().unwrap()).unwrap() else {
            return Vec::new();
        };
        pairs
            .iter()
            .map(|(k, _)| match k {
                Value::Str(s) => String::from_utf8(s.clone()).unwrap(),
                _ => panic!(),
            })
            .collect()
    }

    #[test]
    fn unknown_keys_are_ignored() {
        for m in every_message() {
            if matches!(m, Message::Input(_) | Message::Output(_)) {
                continue;
            }
            let f = edited(&m, |p| {
                p.insert(0, (key("later"), Value::Array(vec![Value::Nil])));
                p.push((key("zzz"), Value::Bin(b"x".to_vec())));
            });
            assert_eq!(Some(m.clone()), Message::decode(&f).unwrap(), "{m:?}");
        }
    }

    #[test]
    fn unknown_kinds_are_skipped() {
        let f = Frame {
            kind: b'q',
            payload: b"anything".to_vec(),
        };
        assert_eq!(Ok(None), Message::decode(&f));
    }

    #[test]
    fn the_first_frame_must_be_a_hello() {
        let size = Message::Size(Size {
            cols: 1,
            rows: 1,
            px: None,
        });
        assert_eq!(
            Err(Error::NotHello(b's')),
            Message::decode_hello(&frame_of(&size))
        );
        let q = Frame {
            kind: b'q',
            payload: Vec::new(),
        };
        assert_eq!(Err(Error::NotHello(b'q')), Message::decode_hello(&q));
        let hello = Hello::new(Purpose::Status { pid: None });
        assert_eq!(
            Ok(hello.clone()),
            Message::decode_hello(&frame_of(&Message::Hello(hello)))
        );
    }

    /// Every field the minimal message of each kind sends is required:
    /// without it, decoding names it.
    #[test]
    fn a_missing_required_field_is_named() {
        let minimal = [
            Message::Hello(Hello::new(Purpose::Requested {
                pane: pane(),
                terminal: Terminal {
                    size: Size {
                        cols: 1,
                        rows: 1,
                        px: None,
                    },
                    ..terminal()
                },
            })),
            Message::Hello(Hello::new(Purpose::Open {
                pid: None,
                cwd: b"/".to_vec(),
                tty: None,
                open: OpenFile {
                    file: b"/f".to_vec(),
                    line: None,
                    wait: Wait::ForWindow,
                },
            })),
            Message::Size(Size {
                cols: 1,
                rows: 1,
                px: None,
            }),
            Message::Accept(Accept {
                v: 1,
                build: "b".into(),
                daemon: "d".into(),
                log: Vec::new(),
                features: Vec::new(),
            }),
            Message::Undo(Vec::new()),
            Message::Reply(Reply {
                ok: false,
                message: None,
                v: 1,
                build: "b".into(),
                name: "n".into(),
                pid: 1,
                status: Some(Status {
                    dir: None,
                    kept: None,
                    capabilities: None,
                    unsupported: None,
                    log: None,
                    terminals: None,
                    ..status()
                }),
                waited: None,
            }),
            Message::End(End::refusal(Refusal::User, "no")),
        ];
        let mut named = Vec::new();
        for m in &minimal {
            for k in keys(m) {
                // a reply without `address` is a reply without status
                if matches!(m, Message::Reply(_)) && k == "address" {
                    continue;
                }
                let f = edited(m, |p| {
                    p.retain(|(key, _)| *key != Value::Str(k.clone().into()))
                });
                let got = Message::decode(&f);
                assert_eq!(
                    Err(Error::Missing(static_name(&k))),
                    got,
                    "{m:?} without {k}"
                );
                named.push(k);
            }
        }
        for k in ["pane", "transport", "env", "tty", "cwd", "file", "purpose"] {
            assert!(named.iter().any(|n| n == k), "{k}");
        }
    }

    /// The `&'static str` the decoder names `k` with.
    fn static_name(k: &str) -> &'static str {
        Box::leak(k.to_string().into_boxed_str())
    }

    #[test]
    fn another_version_is_refused_but_status_and_stop_still_read() {
        let set_v = |p: &mut Vec<(Value, Value)>| {
            for (k, v) in p.iter_mut() {
                if *k == key("v") {
                    *v = Value::Int(ATTACH_PROTOCOL_VERSION + 1);
                }
            }
        };
        for m in every_message() {
            let Message::Hello(h) = &m else { continue };
            let f = edited(&m, set_v);
            let got = Message::decode_hello(&f);
            match &h.purpose {
                Purpose::Status { .. } => assert_eq!(
                    Purpose::Status { pid: None },
                    got.unwrap().purpose,
                    "pid isn't frozen"
                ),
                Purpose::Stop { force, .. } => assert_eq!(
                    Purpose::Stop {
                        pid: None,
                        force: *force
                    },
                    got.unwrap().purpose
                ),
                _ => assert_eq!(
                    Err(Error::Version {
                        v: ATTACH_PROTOCOL_VERSION + 1,
                        build: h.build.clone()
                    }),
                    got
                ),
            }
        }
        // another version's reply keeps only the frozen fields
        let reply = every_message()
            .into_iter()
            .find(|m| matches!(m, Message::Reply(_)))
            .unwrap();
        let Some(Message::Reply(r)) = Message::decode(&edited(&reply, set_v)).unwrap() else {
            panic!()
        };
        assert_eq!((None, None), (r.status, r.waited));
        assert_eq!(("work", 77, true), (r.name.as_str(), r.pid, r.ok));
    }

    #[test]
    fn the_version_is_matched_before_the_purpose() {
        let hello = Message::Hello(Hello::new(Purpose::Status { pid: None }));
        let f = edited(&hello, |p| {
            for (k, v) in p.iter_mut() {
                if *k == key("v") {
                    *v = Value::Int(ATTACH_PROTOCOL_VERSION + 1);
                } else if *k == key("purpose") {
                    *v = Value::Str(b"later".to_vec());
                }
            }
        });
        assert_eq!(
            Err(Error::Version {
                v: ATTACH_PROTOCOL_VERSION + 1,
                build: build().to_string()
            }),
            Message::decode_hello(&f)
        );
    }

    #[test]
    fn a_bad_optional_end_field_counts_as_absent() {
        let mut quit = End::ending(Ending::Quit, "Neovim quit", true);
        quit.exit = Some(3);
        let m = Message::End(quit);
        for (field, bad) in [
            ("exit", Value::Int(256)),
            ("exit", Value::Str(b"3".to_vec())),
            ("cut", Value::Int(1)),
        ] {
            let f = edited(&m, |p| {
                for (k, v) in p.iter_mut() {
                    if *k == key(field) {
                        *v = bad.clone();
                    }
                }
            });
            let Some(Message::End(z)) = Message::decode(&f).unwrap() else {
                panic!()
            };
            assert_eq!(
                ("quit", "Neovim quit"),
                (z.reason.as_str(), z.message.as_str())
            );
            if field == "exit" {
                assert_eq!((None, true), (z.exit, z.cut));
            } else {
                assert_eq!((Some(3), false), (z.exit, z.cut));
            }
        }
    }

    #[test]
    fn bad_values_are_named() {
        let hello = Message::Hello(Hello::new(Purpose::Requested {
            pane: pane(),
            terminal: terminal(),
        }));
        let with = |k: &str, v: Value| {
            let k = key(k);
            Message::decode(&edited(&hello, |p| {
                for (key, val) in p.iter_mut() {
                    if *key == k {
                        *val = v.clone();
                    }
                }
            }))
        };
        let s = |s: &str| Value::Str(s.as_bytes().to_vec());
        assert_eq!(
            Err(Error::Invalid("pane")),
            with("pane", s("012-k3j9x2qpa8"))
        );
        assert_eq!(
            Err(Error::Invalid("transport")),
            with("transport", s("pipe"))
        );
        assert_eq!(Err(Error::Invalid("purpose")), with("purpose", s("later")));
        assert_eq!(Err(Error::Invalid("tty")), with("tty", s("/dev/tty")));
        assert_eq!(Err(Error::Invalid("pid")), with("pid", Value::Int(-1)));
        assert_eq!(Err(Error::Invalid("v")), with("v", s("1")));
    }

    #[test]
    fn a_line_needs_a_file_and_starts_at_1() {
        let self_attach = |file: Option<&[u8]>, line| {
            Message::Hello(Hello::new(Purpose::SelfAttach {
                terminal: terminal(),
                provider: None,
                file: file.map(<[u8]>::to_vec),
                line,
            }))
        };
        let round = |m: Message| Message::decode(&frame_of(&m)).map(|_| ());
        assert_eq!(Ok(()), round(self_attach(Some(b"/f"), Some(1))));
        assert_eq!(
            Err(Error::Invalid("line")),
            round(self_attach(None, Some(1)))
        );
        assert_eq!(
            Err(Error::Invalid("line")),
            round(self_attach(Some(b"/f"), Some(0)))
        );
    }

    #[test]
    fn the_first_env_entry_wins() {
        let mut t = terminal();
        t.env = vec![
            (b"A".to_vec(), b"1".to_vec()),
            (b"B".to_vec(), b"2".to_vec()),
            (b"A".to_vec(), b"3".to_vec()),
        ];
        let m = Message::Hello(Hello::new(Purpose::Requested {
            pane: pane(),
            terminal: t,
        }));
        let Some(Message::Hello(Hello {
            purpose: Purpose::Requested { terminal, .. },
            ..
        })) = Message::decode(&frame_of(&m)).unwrap()
        else {
            panic!()
        };
        assert_eq!(
            vec![
                (b"A".to_vec(), b"1".to_vec()),
                (b"B".to_vec(), b"2".to_vec())
            ],
            terminal.env
        );
    }

    #[test]
    fn undo_bytes_are_limited() {
        assert!(Message::Undo(vec![b'x'; UNDO_LIMIT]).encode().is_ok());
        assert_eq!(
            Err(Error::Invalid("undo")),
            Message::Undo(vec![b'x'; UNDO_LIMIT + 1]).encode()
        );
        let mut w = MapWriter::default();
        w.bin("undo", &vec![b'x'; UNDO_LIMIT + 1]);
        let f = Frame {
            kind: b'u',
            payload: w.finish(),
        };
        assert_eq!(Err(Error::Invalid("undo")), Message::decode(&f));
    }

    #[test]
    fn over_long_relay_frames_must_be_split() {
        let big = vec![0u8; super::super::frame::FRAME_LIMIT as usize + 1];
        assert!(matches!(
            Message::Output(big.clone()).encode(),
            Err(Error::TooLong { kind: b'o', .. })
        ));
        assert!(Message::Input(big).encode().is_err());
    }

    #[test]
    fn reasons() {
        for r in Refusal::ALL {
            let z = End::refusal(r, "m");
            assert_eq!(Some(r), z.as_refusal());
        }
        for e in Ending::ALL {
            assert_eq!(e, End::ending(e, "m", false).as_ending());
        }
        let mut z = End::ending(Ending::Closed, "m", false);
        z.reason = "later".into();
        assert_eq!(Ending::Failed, z.as_ending(), "unknown counts as failed");
        assert_eq!(None, z.as_refusal());
    }

    /// Every field, kind, purpose, transport and reason name the code sends
    /// appears in `docs/attach-protocol.md`, in backticks.
    #[test]
    fn the_doc_names_every_name() {
        let doc = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/attach-protocol.md"),
        )
        .unwrap();
        let mut names: Vec<String> = Vec::new();
        for m in every_message() {
            names.push((m.kind() as char).to_string());
            names.extend(keys(&m));
            if let Message::Hello(Hello {
                purpose: Purpose::Requested { terminal, .. } | Purpose::SelfAttach { terminal, .. },
                ..
            }) = &m
            {
                names.extend(keys(&Message::Size(terminal.size)));
            }
        }
        names.extend(Purpose::NAMES.map(String::from));
        names.extend(Transport::ALL.map(|t| t.name().to_string()));
        names.extend(Refusal::ALL.map(|r| r.name().to_string()));
        names.extend(Ending::ALL.map(|e| e.name().to_string()));
        names.sort();
        names.dedup();
        let missing: Vec<_> = names
            .iter()
            .filter(|n| !doc.contains(&format!("`{n}`")))
            .collect();
        assert!(missing.is_empty(), "not in the doc: {missing:?}");
    }

    #[test]
    fn the_transport_variable_is_fd_unless_set_to_relay() {
        use std::ffi::OsStr;
        let from = |v: &str| Transport::from_env_value(Some(OsStr::new(v)));
        assert_eq!((Transport::Fd, None), Transport::from_env_value(None));
        assert_eq!((Transport::Fd, None), from(""));
        assert_eq!((Transport::Fd, None), from("fd"));
        assert_eq!((Transport::Relay, None), from("relay"));
    }

    #[test]
    fn an_unknown_transport_warns_and_is_fd() {
        use std::os::unix::ffi::OsStrExt;
        let var = format!("{}_TRANSPORT", crate::name::upper());
        assert_eq!(var, Transport::env_var());
        for (value, shown) in [
            (&b"x"[..], "x"),
            (b"RELAY", "RELAY"),
            (b"relay ", "relay "),
            (b"\xff", "\u{fffd}"),
        ] {
            let (t, warning) = Transport::from_env_value(Some(std::ffi::OsStr::from_bytes(value)));
            assert_eq!(Transport::Fd, t);
            assert_eq!(
                Some(format!("ignoring {var}={shown} (use fd or relay)")),
                warning
            );
        }
    }
}
