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

/// Finds the daemon `target` asks for, in the base directory `base` gives,
/// which only a name or none needs: its address. `inside` is `$<NAME>`,
/// taken as it is until it can be proven (#123); when nothing answers there,
/// `warn` is told it is ignored.
pub fn find_daemon(
    base: impl FnOnce() -> Result<Base, dir::Error>,
    target: &Target,
    inside: Option<&OsStr>,
    warn: &mut dyn FnMut(String),
) -> Result<Address, NotFound> {
    if let Target::Address(address) = target {
        return match address.answers() {
            true => Ok(address.clone()),
            false => Err(NotFound::NoAnswer(address.clone())),
        };
    }
    if *target == Target::Found
        && let Some(inside) = inside.filter(|a| !a.is_empty())
    {
        let address = Address::from_arg(inside.to_os_string());
        if address.answers() {
            return Ok(address);
        }
        warn(format!(
            "ignoring ${}: nothing answers at {address}",
            inside_var()
        ));
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

        fn find(
            &self,
            target: &Target,
            inside: Option<&str>,
        ) -> (Result<Address, String>, Vec<String>) {
            let mut warned = Vec::new();
            let found = find_daemon(|| self.base(), target, inside.map(OsStr::new), &mut |w| {
                warned.push(w)
            });
            (found.map_err(|e| e.to_string()), warned)
        }
    }

    fn name(n: &str) -> Target {
        Target::Name(n.into())
    }

    #[test]
    fn with_none_running_each_way_says_so() {
        let mut rig = Rig::new();
        let stale = rig.stale("old");
        let (found, _) = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err(format!(
                "no daemon is running; start one with {NAME} [FILE]"
            ))
        );
        let (found, _) = rig.find(&name("work"), None);
        assert_eq!(found, Err("no daemon named work is running".into()));
        let (found, _) = rig.find(&Target::Address(stale.clone()), None);
        assert_eq!(found, Err(format!("no daemon answers at {stale}")));
        let nowhere = Address::from_arg(rig.root.0.join("nowhere/sock").into_os_string());
        let (found, _) = rig.find(&Target::Address(nowhere.clone()), None);
        assert_eq!(found, Err(format!("no daemon answers at {nowhere}")));
    }

    #[test]
    fn the_only_one_running_is_found() {
        let mut rig = Rig::new();
        rig.stale("old");
        let work = rig.run("work");
        assert_eq!(rig.find(&Target::Found, None).0, Ok(work.clone()));
        assert_eq!(rig.find(&name("work"), None).0, Ok(work.clone()));
        assert_eq!(rig.find(&Target::Address(work.clone()), None).0, Ok(work));
    }

    #[test]
    fn several_running_need_a_name() {
        let mut rig = Rig::new();
        let scratch = rig.run("scratch");
        rig.run("work");
        rig.run("notes");
        rig.stale("old");
        let (found, _) = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err("3 daemons are running (notes, scratch, work); choose one with --name".into())
        );
        assert_eq!(rig.find(&name("scratch"), None).0, Ok(scratch));
        let (found, _) = rig.find(&name("old"), None);
        assert_eq!(
            found,
            Err("no daemon named old is running (running: notes, scratch, work)".into())
        );
    }

    #[test]
    fn the_address_in_its_variable_comes_before_the_only_one_and_after_a_name() {
        let mut rig = Rig::new();
        let work = rig.run("work");
        let notes = rig.run("notes");
        let inside = notes.to_string();
        assert_eq!(
            rig.find(&Target::Found, Some(&inside)),
            (Ok(notes.clone()), vec![])
        );
        assert_eq!(rig.find(&name("work"), Some(&inside)).0, Ok(work.clone()));
        assert_eq!(
            rig.find(&Target::Address(work.clone()), Some(&inside)).0,
            Ok(work)
        );
        // empty is unset
        assert!(rig.find(&Target::Found, Some("")).1.is_empty());
    }

    #[test]
    fn an_address_in_its_variable_that_doesnt_answer_is_ignored_with_a_warning() {
        let mut rig = Rig::new();
        let gone = rig.stale("gone");
        let work = rig.run("work");
        let (found, warned) = rig.find(&Target::Found, Some(&gone.to_string()));
        assert_eq!(found, Ok(work));
        assert_eq!(
            warned,
            vec![format!(
                "ignoring ${}: nothing answers at {gone}",
                inside_var()
            )]
        );
    }

    #[test]
    fn an_address_needs_no_base_directory() {
        let mut rig = Rig::new();
        let work = rig.run("work");
        let found = find_daemon(
            || Err(dir::Error::Live("x".into())),
            &Target::Address(work.clone()),
            None,
            &mut |_| {},
        );
        assert_eq!(found.ok(), Some(work));
        let err = find_daemon(
            || Err(dir::Error::Live("x".into())),
            &Target::Found,
            None,
            &mut |_| {},
        )
        .unwrap_err();
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
        assert_eq!(rig.find(&name("work"), None).0, Ok(address.clone()));
        assert_eq!(rig.find(&Target::Found, None).0, Ok(address));
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
        let (found, _) = rig.find(&name("work"), None);
        assert_eq!(found, Err(format!("no daemon answers at {address}")));
        assert!(started.elapsed() >= ANSWER_WITHIN);
    }

    #[test]
    fn a_socket_that_answers_in_an_unlocked_directory_isnt_running() {
        let mut rig = Rig::new();
        let claimed = rig.starting("ghost");
        rig.listening.push(claimed.bind().unwrap());
        drop(claimed);
        let (found, _) = rig.find(&Target::Found, None);
        assert_eq!(
            found,
            Err(format!(
                "no daemon is running; start one with {NAME} [FILE]"
            ))
        );
    }
}
