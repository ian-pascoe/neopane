//! Control requests from the command line's side (#91 decision 26, #94
//! decisions 29–32): `list` asks every running daemon its `status` at once,
//! `status` asks one, and `kill` sends `stop` and waits for the daemon to
//! be gone. Only the frozen fields are relied on, so a daemon of another
//! attach protocol version still answers, as far as it can.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use super::dir::{Address, Base};
use super::pane_helper::peer_uid;
use crate::attach::{End, Frame, Hello, Message, Purpose, Reply};

/// How long each daemon `list` asks has to answer.
pub const LIST_WITHIN: Duration = Duration::from_secs(2);

/// How long `status` waits for its daemon's answer.
pub const STATUS_WITHIN: Duration = Duration::from_secs(2);

/// How long `kill` waits for the answer to `stop`, then for the daemon to
/// be gone.
pub const KILL_WITHIN: Duration = Duration::from_secs(10);

/// Why a control request got no reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    /// Nothing came within the time it had.
    NoAnswer(Duration),
    /// Refused before any answer, as an older daemon refuses a hello it
    /// can't read.
    Refused(End),
    /// It couldn't be asked, or hung up without a reply: why.
    Failed(String),
}

impl std::fmt::Display for Failure {
    /// As `list` shows it: `didn't answer in 2 s`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::NoAnswer(within) => {
                write!(f, "didn't answer in {} s", crate::util::seconds(*within))
            }
            Failure::Refused(z) => write!(f, "refused: {}", z.message),
            Failure::Failed(why) => f.write_str(why),
        }
    }
}

/// Sends the daemon at `address` a control request for `purpose`, and
/// waits up to `within` for its reply.
pub fn ask(address: &Address, purpose: Purpose, within: Duration) -> Result<Reply, Failure> {
    let due = Instant::now() + within;
    let mut stream = address
        .connect()
        .map_err(|e| Failure::Failed(format!("can't connect: {e}")))?;
    // SAFETY: `geteuid` has no preconditions.
    let ours = unsafe { libc::geteuid() };
    match peer_uid(&stream) {
        Ok(peer) if peer == ours => {}
        Ok(_) => return Err(Failure::Failed("it runs as another user".into())),
        Err(e) => return Err(Failure::Failed(format!("can't check its user: {e}"))),
    }
    let hello = Message::Hello(Hello::new(purpose))
        .encode()
        .expect("a control request fits a frame");
    stream
        .set_write_timeout(Some(within))
        .and_then(|()| stream.write_all(&hello))
        .map_err(|e| Failure::Failed(format!("can't send the request: {e}")))?;
    read_reply(&mut stream, due, within)
}

/// Reads frames from `stream` until the reply, or a refusal, by `due`.
fn read_reply(stream: &mut UnixStream, due: Instant, within: Duration) -> Result<Reply, Failure> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16 << 10];
    loop {
        while let Some((frame, n)) = Frame::decode(&buf)
            .map_err(|e| Failure::Failed(format!("can't read its answer: {e}")))?
        {
            buf.drain(..n);
            match Message::decode(&frame) {
                Ok(Some(Message::Reply(r))) => return Ok(r),
                Ok(Some(Message::End(z))) => return Err(Failure::Refused(z)),
                Ok(_) => {}
                Err(e) => return Err(Failure::Failed(format!("can't read its answer: {e}"))),
            }
        }
        let left = due.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(Failure::NoAnswer(within));
        }
        stream
            .set_read_timeout(Some(left))
            .map_err(|e| Failure::Failed(e.to_string()))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(Failure::Failed("it hung up without an answer".into())),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                return Err(Failure::NoAnswer(within));
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(Failure::Failed(format!("can't read its answer: {e}"))),
        }
    }
}

/// A status request.
pub fn status_purpose() -> Purpose {
    Purpose::Status {
        pid: Some(std::process::id()),
    }
}

/// A daemon `list` found, and what it answered.
#[derive(Debug, Clone)]
pub struct Listed {
    /// Its directory's name.
    pub name: String,
    pub address: Address,
    pub answer: Result<Reply, Failure>,
}

/// Every live daemon in `base`, sorted by name, each asked its status at
/// once, with `within` to answer. Stale directories are skipped.
pub fn list(base: &Base, within: Duration) -> Vec<Listed> {
    let asking: Vec<_> = base
        .running()
        .into_iter()
        .map(|name| {
            let address = base.address(&name);
            let asked = address.clone();
            let thread = std::thread::spawn(move || ask(&asked, status_purpose(), within));
            (name, address, thread)
        })
        .collect();
    asking
        .into_iter()
        .map(|(name, address, thread)| Listed {
            name,
            address,
            answer: thread
                .join()
                .unwrap_or_else(|_| Err(Failure::Failed("asking it failed".into()))),
        })
        .collect()
}

/// How a `kill` of one daemon went.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Killed {
    /// It's gone.
    Gone,
    /// It refused, with its message: unsaved changes.
    Refused(String),
    /// It agreed, but is still there after `within`: its name and pid.
    StillEnding { name: String, pid: u32 },
    /// It didn't answer.
    NoAnswer(Failure),
}

/// Sends `stop`, `force`d or not, to the daemon at `address`, then waits
/// for it to be gone, all within `within`. Never a signal.
pub fn kill(address: &Address, force: bool, within: Duration) -> Killed {
    let due = Instant::now() + within;
    let purpose = Purpose::Stop {
        pid: Some(std::process::id()),
        force,
    };
    let reply = match ask(address, purpose, within) {
        Ok(r) => r,
        Err(e) => return Killed::NoAnswer(e),
    };
    if !reply.ok {
        let why = reply.message.unwrap_or_else(|| "refused to stop".into());
        return Killed::Refused(why);
    }
    while address.answers() || address.dir_locked() {
        if Instant::now() >= due {
            return Killed::StillEnding {
                name: reply.name,
                pid: reply.pid,
            };
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Killed::Gone
}

/// `dir` as a person reads it: `~` for `home`, and what's under it.
/// Lossy where it isn't UTF-8.
pub fn shown_dir(dir: &[u8], home: Option<&[u8]>) -> String {
    let home = home
        .filter(|h| h.len() > 1)
        .map(|h| h.strip_suffix(b"/").unwrap_or(h));
    if let Some(home) = home {
        if dir == home {
            return "~".into();
        }
        if let Some(rest) = dir.strip_prefix(home).and_then(|r| r.strip_prefix(b"/")) {
            return format!("~/{}", String::from_utf8_lossy(rest));
        }
    }
    String::from_utf8_lossy(dir).into_owned()
}

/// Files with unsaved changes as a person reads them, joined by `, `:
/// relative to `dir` when under it, else as `shown_dir` shows them, and
/// `[No Name]` for an unnamed buffer.
pub fn shown_files(files: &[Vec<u8>], dir: Option<&[u8]>, home: Option<&[u8]>) -> String {
    let dir = dir
        .filter(|d| d.len() > 1)
        .map(|d| d.strip_suffix(b"/").unwrap_or(d));
    files
        .iter()
        .map(|f| {
            if f.is_empty() {
                return "[No Name]".to_string();
            }
            match dir
                .and_then(|d| f.strip_prefix(d))
                .and_then(|r| r.strip_prefix(b"/"))
            {
                Some(rest) if !rest.is_empty() => String::from_utf8_lossy(rest).into_owned(),
                _ => shown_dir(f, home),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_directory_under_home_is_shown_with_a_tilde() {
        let home = Some(&b"/Users/ian"[..]);
        assert_eq!("~", shown_dir(b"/Users/ian", home));
        assert_eq!("~/src/x", shown_dir(b"/Users/ian/src/x", home));
        assert_eq!("/Users/ianx", shown_dir(b"/Users/ianx", home));
        assert_eq!("/tmp", shown_dir(b"/tmp", home));
        assert_eq!("/tmp", shown_dir(b"/tmp", None));
        // a home of / shortens nothing
        assert_eq!("/tmp", shown_dir(b"/tmp", Some(b"/")));
    }

    #[test]
    fn unsaved_files_are_shown_relative_to_the_directory() {
        let files = [
            b"/w/README.md".to_vec(),
            b"/w/src/main.rs".to_vec(),
            Vec::new(),
            b"/h/notes.md".to_vec(),
            b"/etc/hosts".to_vec(),
        ];
        assert_eq!(
            "README.md, src/main.rs, [No Name], ~/notes.md, /etc/hosts",
            shown_files(&files, Some(b"/w"), Some(b"/h"))
        );
        assert_eq!("/w/README.md", shown_files(&files[..1], None, Some(b"/h")));
    }

    #[test]
    fn nothing_listening_is_no_answer() {
        let at = std::env::temp_dir().join(format!(
            "{}-control-none-{}.sock",
            crate::name::NAME,
            std::process::id()
        ));
        let address = Address::from_arg(at.into_os_string());
        let got = ask(&address, status_purpose(), Duration::from_millis(100));
        assert!(matches!(got, Err(Failure::Failed(_))), "{got:?}");
    }

    #[test]
    fn a_daemon_that_says_nothing_has_no_answer_in_time() {
        let at = std::env::temp_dir().join(format!(
            "{}-control-mute-{}.sock",
            crate::name::NAME,
            std::process::id()
        ));
        let _ = std::fs::remove_file(&at);
        let server = std::os::unix::net::UnixListener::bind(&at).unwrap();
        let address = Address::from_arg(at.clone().into_os_string());
        let within = Duration::from_millis(200);
        let started = Instant::now();
        let got = ask(&address, status_purpose(), within);
        assert_eq!(Err(Failure::NoAnswer(within)), got);
        assert!(started.elapsed() >= within);
        assert_eq!("didn't answer in 0.2 s", got.unwrap_err().to_string());
        drop(server);
        let _ = std::fs::remove_file(&at);
    }
}
