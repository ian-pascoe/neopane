//! Finding the daemon a self-attach is to (#94 decision 23): `--address`,
//! then `--name`, then `$<NAME>`, then the only one running. It never offers
//! to start one.

use std::ffi::OsStr;
use std::time::{Duration, Instant};

use super::dir::{self, Address, Base};
use crate::name::{self, NAME};

/// How long a live daemon has to answer at its address: one that has just
/// claimed its name binds a moment later.
pub const ANSWER_WITHIN: Duration = Duration::from_secs(1);

/// The daemon a self-attach is to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `--address A`.
    Address(Address),
    /// `--name N`.
    Name(String),
    /// Neither: `$<NAME>`, else the only one running.
    Found,
}

/// Why no daemon was found. Displayed without the `<name>: ` its printer
/// puts first.
#[derive(Debug)]
pub enum NotFound {
    NoneRunning,
    /// More than one, and nothing chose: their names, sorted.
    Several(Vec<String>),
    /// None by the name asked for: the names of those running.
    NoSuchName {
        name: String,
        running: Vec<String>,
    },
    NoAnswer(Address),
    /// The base directory can't be had.
    Dir(dir::Error),
}

impl std::fmt::Display for NotFound {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotFound::NoneRunning => {
                write!(f, "no daemon is running; start one with {NAME} [FILE]")
            }
            NotFound::Several(names) => write!(
                f,
                "{} daemons are running ({}); choose one with --name",
                names.len(),
                names.join(", ")
            ),
            NotFound::NoSuchName { name, running } if running.is_empty() => {
                write!(f, "no daemon named {name} is running")
            }
            NotFound::NoSuchName { name, running } => write!(
                f,
                "no daemon named {name} is running (running: {})",
                running.join(", ")
            ),
            NotFound::NoAnswer(address) => write!(f, "no daemon answers at {address}"),
            NotFound::Dir(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for NotFound {}

/// `<NAME>`, which a daemon sets in its Neovim's environment to its address.
pub fn inside_var() -> String {
    name::upper()
}

/// The daemon whose own Neovim this process runs in, proven (#94 decision
/// 17).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Inside {
    pub address: Address,
    /// Its name, as it answered.
    pub name: String,
    /// This process's terminal, one of its Neovim's `:terminal`s; None
    /// if it has none.
    pub tty: Option<Vec<u8>>,
}

/// Proves `inside`, `$<NAME>`, names the daemon whose own Neovim this
/// process runs in (#94 decision 17): a daemon of this attach protocol
/// version answers there, and `tty`, this process's terminal, is one of its
/// Neovim's `:terminal`s. A process with no terminal is trusted on the
/// address alone. Unproven, `$<NAME>` is ignored, and `warn` told why.
pub fn prove_inside(
    inside: Option<&OsStr>,
    tty: Option<&[u8]>,
    warn: &mut dyn FnMut(String),
) -> Option<Inside> {
    let address = Address::from_arg(inside.filter(|a| !a.is_empty())?.to_os_string());
    let var = inside_var();
    let reply = match super::control::ask(
        &address,
        super::control::status_purpose(),
        super::control::STATUS_WITHIN,
    ) {
        Ok(r) => r,
        Err(_) => {
            warn(format!("ignoring ${var}: nothing answers at {address}"));
            return None;
        }
    };
    let name = reply.name;
    let Some(status) = reply
        .status
        .filter(|_| reply.v == crate::attach::ATTACH_PROTOCOL_VERSION)
    else {
        warn(format!(
            "ignoring ${var}: {name} runs another version, so this terminal can't be proven to be \
             its own"
        ));
        return None;
    };
    let ours = match tty {
        None => true,
        Some(tty) => status
            .terminals
            .unwrap_or_default()
            .iter()
            .any(|t| t == tty),
    };
    if !ours {
        warn(format!(
            "ignoring ${var}: this terminal isn't one of {name}'s (unset {var} to stop this \
             warning)"
        ));
        return None;
    }
    Some(Inside {
        address,
        name,
        tty: tty.map(<[u8]>::to_vec),
    })
}

/// This process's terminal: the first of fds 0, 1 and 2 that is one, by
/// its device path.
pub fn this_tty() -> Option<Vec<u8>> {
    (0..3).find_map(|fd| {
        // SAFETY: isatty and ttyname only inspect the fd; ttyname's result
        // is copied at once.
        unsafe {
            if libc::isatty(fd) != 1 {
                return None;
            }
            let name = libc::ttyname(fd);
            (!name.is_null()).then(|| std::ffi::CStr::from_ptr(name).to_bytes().to_vec())
        }
    })
}

/// Finds the daemon `target` asks for, in the base directory `base` gives,
/// which only a name or none needs: its address. `inside` is the daemon
/// whose own Neovim this runs in, proven, which comes before the only one
/// running.
pub fn find_daemon(
    base: impl FnOnce() -> Result<Base, dir::Error>,
    target: &Target,
    inside: Option<&Inside>,
) -> Result<Address, NotFound> {
    if let Target::Address(address) = target {
        return match address.answers() {
            true => Ok(address.clone()),
            false => Err(NotFound::NoAnswer(address.clone())),
        };
    }
    if *target == Target::Found
        && let Some(inside) = inside
    {
        return Ok(inside.address.clone());
    }
    let base = base().map_err(NotFound::Dir)?;
    let name = match target {
        Target::Name(name) if base.live(name) => name.clone(),
        Target::Name(name) => {
            return Err(NotFound::NoSuchName {
                name: name.clone(),
                running: base.running(),
            });
        }
        _ => {
            let mut running = base.running();
            match running.len() {
                0 => return Err(NotFound::NoneRunning),
                1 => running.remove(0),
                _ => return Err(NotFound::Several(running)),
            }
        }
    };
    answering(base.address(&name), ANSWER_WITHIN)
}

/// `address`, once a daemon answers there, within `within`.
fn answering(address: Address, within: Duration) -> Result<Address, NotFound> {
    let due = Instant::now() + within;
    while !address.answers() {
        if Instant::now() >= due {
            return Err(NotFound::NoAnswer(address));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::dir::testing::Scratch;
    use std::os::unix::net::UnixListener;

    /// A scratch root holding a base directory, and the daemons there,
    /// faked by claimed directories and listening daemon addresses.
    struct Rig {
        root: Scratch,
        claimed: Vec<dir::Claimed>,
        listening: Vec<UnixListener>,
    }

    impl Rig {
        fn new() -> Rig {
            Rig {
                root: Scratch::new(),
                claimed: Vec::new(),
                listening: Vec::new(),
            }
        }

        fn base(&self) -> Result<Base, dir::Error> {
            // SAFETY: getuid can't fail.
            dir::base_dir(&self.root.0, unsafe { libc::getuid() })
        }

        /// A daemon named `name`, live and listening.
        fn run(&mut self, name: &str) -> Address {
            let claimed = self.starting(name);
            self.listening.push(claimed.bind().unwrap());
            let address = claimed.address();
            self.claimed.push(claimed);
            address
        }

        /// A daemon named `name` that has claimed its directory, live, but
        /// hasn't bound its address yet.
        fn starting(&self, name: &str) -> dir::Claimed {
            dir::claim(&self.base().unwrap(), name).unwrap()
        }

        /// A directory whose daemon is gone, its socket left behind,
        /// unlocked.
        fn stale(&mut self, name: &str) -> Address {
            let claimed = dir::claim(&self.base().unwrap(), name).unwrap();
            drop(claimed.bind().unwrap());
            claimed.address()
        }

        fn find(&self, target: &Target, inside: Option<&Address>) -> Result<Address, String> {
            let inside = inside.map(|a| Inside {
                address: a.clone(),
                name: "x".into(),
                tty: None,
            });
            let found = find_daemon(|| self.base(), target, inside.as_ref());
            found.map_err(|e| e.to_string())
        }
    }

    fn name(n: &str) -> Target {
        Target::Name(n.into())
    }

    #[test]
    fn with_none_running_each_way_says_so() {
        let mut rig = Rig::new();
        let stale = rig.stale("old");
        let found = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err(format!(
                "no daemon is running; start one with {NAME} [FILE]"
            ))
        );
        let found = rig.find(&name("work"), None);
        assert_eq!(found, Err("no daemon named work is running".into()));
        let found = rig.find(&Target::Address(stale.clone()), None);
        assert_eq!(found, Err(format!("no daemon answers at {stale}")));
        let nowhere = Address::from_arg(rig.root.0.join("nowhere/sock").into_os_string());
        let found = rig.find(&Target::Address(nowhere.clone()), None);
        assert_eq!(found, Err(format!("no daemon answers at {nowhere}")));
    }

    #[test]
    fn the_only_one_running_is_found() {
        let mut rig = Rig::new();
        rig.stale("old");
        let work = rig.run("work");
        assert_eq!(rig.find(&Target::Found, None), Ok(work.clone()));
        assert_eq!(rig.find(&name("work"), None), Ok(work.clone()));
        assert_eq!(rig.find(&Target::Address(work.clone()), None), Ok(work));
    }

    #[test]
    fn several_running_need_a_name() {
        let mut rig = Rig::new();
        let scratch = rig.run("scratch");
        rig.run("work");
        rig.run("notes");
        rig.stale("old");
        let found = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err("3 daemons are running (notes, scratch, work); choose one with --name".into())
        );
        assert_eq!(rig.find(&name("scratch"), None), Ok(scratch));
        let found = rig.find(&name("old"), None);
        assert_eq!(
            found,
            Err("no daemon named old is running (running: notes, scratch, work)".into())
        );
    }

    #[test]
    fn the_daemon_proven_inside_comes_before_the_only_one_and_after_a_name() {
        let mut rig = Rig::new();
        let work = rig.run("work");
        let notes = rig.run("notes");
        assert_eq!(rig.find(&Target::Found, Some(&notes)), Ok(notes.clone()));
        assert_eq!(rig.find(&name("work"), Some(&notes)), Ok(work.clone()));
        assert_eq!(
            rig.find(&Target::Address(work.clone()), Some(&notes)),
            Ok(work)
        );
    }

    /// A daemon of attach protocol version `v` at `address`, answering
    /// every status with `terminals` until the test ends.
    fn answering(address: &Address, v: i64, terminals: &[&str]) -> UnixListener {
        use crate::attach::{Message, Reply};
        use std::io::{Read, Write};
        let path = std::path::PathBuf::from(address.as_arg());
        let _ = std::fs::remove_file(&path);
        let server = UnixListener::bind(&path).unwrap();
        let theirs = server.try_clone().unwrap();
        let terminals: Vec<Vec<u8>> = terminals.iter().map(|t| t.as_bytes().to_vec()).collect();
        std::thread::spawn(move || {
            while let Ok((mut s, _)) = theirs.accept() {
                let mut buf = [0u8; 4096];
                if !matches!(s.read(&mut buf), Ok(1..)) {
                    continue;
                }
                let mut status =
                    crate::daemon::attach_conn::idle_status(&Address::from_arg("/x".into()), "/l");
                status.terminals = Some(terminals.clone());
                let reply = Reply {
                    v,
                    status: Some(status),
                    ..crate::daemon::control::test_reply(true)
                };
                let _ = s.write_all(&Message::Reply(reply).encode().unwrap());
            }
        });
        server
    }

    #[test]
    fn inside_is_proven_by_a_daemon_listing_this_terminal_or_by_having_none() {
        let rig = Rig::new();
        let address = Address::from_arg(rig.root.0.join("work.sock").into_os_string());
        let _server = answering(
            &address,
            crate::attach::ATTACH_PROTOCOL_VERSION,
            &["/dev/ttys004"],
        );
        let prove = |tty: Option<&str>| {
            let mut warned = Vec::new();
            let got = prove_inside(Some(address.as_arg()), tty.map(str::as_bytes), &mut |w| {
                warned.push(w)
            });
            (got, warned)
        };
        let work = |tty: Option<&str>| Inside {
            address: address.clone(),
            name: "work".into(),
            tty: tty.map(|t| t.as_bytes().to_vec()),
        };
        let ours = Some("/dev/ttys004");
        assert_eq!(prove(ours), (Some(work(ours)), vec![]));
        assert_eq!(prove(None), (Some(work(None)), vec![]));
        let var = inside_var();
        assert_eq!(
            prove(Some("/dev/ttys009")),
            (
                None,
                vec![format!(
                    "ignoring ${var}: this terminal isn't one of work's (unset {var} to stop this \
                     warning)"
                )]
            )
        );
        // unset, or empty
        let mut warned = Vec::new();
        assert_eq!(None, prove_inside(None, None, &mut |w| warned.push(w)));
        assert_eq!(
            None,
            prove_inside(Some(OsStr::new("")), None, &mut |w| warned.push(w))
        );
        assert!(warned.is_empty());
    }

    #[test]
    fn inside_isnt_proven_by_another_version_or_nothing_answering() {
        let mut rig = Rig::new();
        let gone = rig.stale("gone");
        let older = Address::from_arg(rig.root.0.join("older.sock").into_os_string());
        let _server = answering(&older, crate::attach::ATTACH_PROTOCOL_VERSION - 1, &[]);
        let var = inside_var();
        for (address, why) in [
            (&gone, format!("nothing answers at {gone}")),
            (
                &older,
                "work runs another version, so this terminal can't be proven to be its own".into(),
            ),
        ] {
            let mut warned = Vec::new();
            let got = prove_inside(Some(address.as_arg()), None, &mut |w| warned.push(w));
            assert_eq!(got, None);
            assert_eq!(warned, vec![format!("ignoring ${var}: {why}")]);
        }
    }

    #[test]
    fn an_address_needs_no_base_directory() {
        let mut rig = Rig::new();
        let work = rig.run("work");
        let found = find_daemon(
            || Err(dir::Error::Live("x".into())),
            &Target::Address(work.clone()),
            None,
        );
        assert_eq!(found.ok(), Some(work));
        let err =
            find_daemon(|| Err(dir::Error::Live("x".into())), &Target::Found, None).unwrap_err();
        assert_eq!(err.to_string(), "x is already running");
    }

    #[test]
    fn a_daemon_that_holds_its_lock_but_hasnt_bound_yet_is_waited_for() {
        let mut rig = Rig::new();
        let claimed = rig.starting("work");
        let address = claimed.address();
        // live, in the lists too, before it answers
        assert_eq!(rig.base().unwrap().running(), vec!["work".to_string()]);
        let binding = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let listener = claimed.bind().unwrap();
            (claimed, listener)
        });
        assert_eq!(rig.find(&name("work"), None), Ok(address.clone()));
        assert_eq!(rig.find(&Target::Found, None), Ok(address));
        let (claimed, listener) = binding.join().unwrap();
        rig.claimed.push(claimed);
        rig.listening.push(listener);
    }

    #[test]
    fn a_live_daemon_that_never_answers_says_so() {
        let mut rig = Rig::new();
        let claimed = rig.starting("work");
        let address = claimed.address();
        rig.claimed.push(claimed);
        let started = Instant::now();
        let found = rig.find(&name("work"), None);
        assert_eq!(found, Err(format!("no daemon answers at {address}")));
        assert!(started.elapsed() >= ANSWER_WITHIN);
    }

    #[test]
    fn a_socket_that_answers_in_an_unlocked_directory_isnt_running() {
        let mut rig = Rig::new();
        let claimed = rig.starting("ghost");
        rig.listening.push(claimed.bind().unwrap());
        drop(claimed);
        let found = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err(format!(
                "no daemon is running; start one with {NAME} [FILE]"
            ))
        );
    }
}
