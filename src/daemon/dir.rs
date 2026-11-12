//! Where a daemon lives (ADR 0004): `$<NAME>_TMPDIR`, else `/tmp`, then
//! `/<name>-<uid>/<daemon name>/`, holding `sock`, `nvim.sock`, `log` and,
//! while it is unattached with unsaved changes, `unsaved`.
//! A daemon is live while it holds an exclusive `flock` on its directory.
//!
//! The one platform module that knows a daemon's address is a path: outside
//! it, an [`Address`] is passed whole.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::name::{self, NAME};

/// The longest daemon name.
pub const MAX_NAME: usize = 32;

/// The name of a daemon started where no name can be made from the
/// directory.
pub const DEFAULT_NAME: &str = "default";

/// Longest path a Unix socket takes, less one for the terminator: 103 on macOS,
/// 107 on Linux.
pub(crate) const MAX_PATH: usize = {
    // SAFETY: an all-zero sockaddr_un is valid, and only its size is read.
    let addr: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    std::mem::size_of_val(&addr.sun_path) - 1
};

/// The daemon's own socket, which attaches connect to.
const SOCK: &str = "sock";
/// Neovim's socket, the longest name in a daemon's directory.
const NVIM_SOCK: &str = "nvim.sock";
const LOG: &str = "log";
/// The log of the daemon that had the name before.
const OLD_LOG: &str = "log.1";
/// #106's list of unsaved files, cleaned from a stale directory.
const UNSAVED: &str = "unsaved";

/// Why a daemon's directory can't be had. Displayed without the `<name>: `
/// its printer puts first.
#[derive(Debug)]
pub enum Error {
    /// The base directory is a link, someone else's, or open to others.
    NotPrivate {
        path: PathBuf,
        owner: u32,
        mode: u32,
    },
    /// A socket in the directory for `name` would be `len` bytes.
    PathTooLong {
        name: String,
        len: usize,
    },
    /// A daemon holds `name`.
    Live(String),
    Io(PathBuf, io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let var = tmpdir_var();
        match self {
            Error::NotPrivate { path, owner, mode } => write!(
                f,
                "{} isn't a private directory (owner {owner}, mode {mode:04o}); set {var} to \
                 another place",
                path.display()
            ),
            Error::PathTooLong { name, len } => write!(
                f,
                "the socket path for {name} would be {len} bytes, over the {MAX_PATH} allowed; \
                 use a shorter --name or {var}"
            ),
            Error::Live(name) => write!(f, "{name} is already running"),
            Error::Io(path, e) => write!(f, "{}: {e}", path.display()),
        }
    }
}

impl std::error::Error for Error {}

/// `<NAME>_TMPDIR`.
pub fn tmpdir_var() -> String {
    format!("{}_TMPDIR", name::upper())
}

/// Whether `name` can name a daemon: 1 to [`MAX_NAME`] of `[A-Za-z0-9._-]`,
/// and not `.` or `..`.
pub fn valid_name(name: &OsStr) -> bool {
    let bytes = name.as_encoded_bytes();
    (1..=MAX_NAME).contains(&bytes.len())
        && bytes.iter().all(|&b| allowed(b))
        && bytes != b"."
        && bytes != b".."
}

fn allowed(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-')
}

/// The default name for a daemon started in a directory named `dir`: its
/// characters from `[A-Za-z0-9._-]`, cut to [`MAX_NAME`], else
/// [`DEFAULT_NAME`].
pub fn clean_name(dir: &OsStr) -> String {
    let mut name: String = dir
        .as_encoded_bytes()
        .iter()
        .filter(|&&b| allowed(b))
        .map(|&b| b as char)
        .collect();
    name.truncate(MAX_NAME);
    if valid_name(OsStr::new(&name)) {
        name
    } else {
        DEFAULT_NAME.to_string()
    }
}

/// The `n`th name `--dedup` tries for `name`, from 1: `name`, then `name-2`,
/// `name-3`, …, `name` cut so that each fits [`MAX_NAME`].
pub fn dedup(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    let suffix = format!("-{n}");
    let keep = name.len().min(MAX_NAME.saturating_sub(suffix.len()));
    format!("{}{suffix}", &name[..keep])
}

/// Refuses `name` in `base` when a socket in its directory would be too long
/// to bind.
pub fn socket_path_len_check(base: &Path, name: &str) -> Result<(), Error> {
    let len = base.join(name).join(NVIM_SOCK).as_os_str().len();
    if len > MAX_PATH {
        return Err(Error::PathTooLong {
            name: name.to_string(),
            len,
        });
    }
    Ok(())
}

/// Where base directories go: `$<NAME>_TMPDIR` (`tmpdir`), made absolute,
/// else `/tmp`.
pub fn tmp_root(tmpdir: Option<&OsStr>) -> PathBuf {
    match tmpdir.filter(|d| !d.is_empty()) {
        Some(d) => std::path::absolute(d).unwrap_or_else(|_| PathBuf::from(d)),
        None => PathBuf::from("/tmp"),
    }
}

/// The user's base directory, `<root>/<name>-<uid>`: made if it isn't there,
/// then refused unless it is a real directory (by `lstat`), `uid`'s, and
/// mode 0700.
pub fn base_dir(root: &Path, uid: u32) -> Result<Base, Error> {
    let path = root.join(format!("{NAME}-{uid}"));
    match fs::DirBuilder::new().mode(0o700).create(&path) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(Error::Io(path, e)),
        _ => {}
    }
    let meta = fs::symlink_metadata(&path).map_err(|e| Error::Io(path.clone(), e))?;
    let mode = meta.mode() & 0o7777;
    if !meta.file_type().is_dir() || meta.uid() != uid || mode != 0o700 {
        return Err(Error::NotPrivate {
            path,
            owner: meta.uid(),
            mode,
        });
    }
    Ok(Base(path))
}

/// This user's base directory, from the environment.
pub fn base() -> Result<Base, Error> {
    let root = tmp_root(std::env::var_os(tmpdir_var()).as_deref());
    // SAFETY: getuid can't fail.
    base_dir(&root, unsafe { libc::getuid() })
}

/// A user's base directory, which holds a directory per daemon name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base(PathBuf);

impl Base {
    /// The address of the daemon named `name`.
    pub fn address(&self, name: &str) -> Address {
        Address(self.0.join(name).join(SOCK).into_os_string())
    }

    /// The log of the daemon named `name`.
    pub fn log(&self, name: &str) -> PathBuf {
        self.0.join(name).join(LOG)
    }

    /// The base directory at `path`, unchecked.
    #[cfg(test)]
    pub(crate) fn at(path: &Path) -> Base {
        Base(path.to_path_buf())
    }
}

impl std::fmt::Display for Base {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.display())
    }
}

/// Where attaches reach a daemon (ADR 0004). Outside this module it is
/// passed whole, never taken apart: on Unix it is a socket's path, on
/// Windows it would be a named pipe.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address(OsString);

impl Address {
    /// The address given on a command line, as `--address A`.
    pub fn from_arg(arg: OsString) -> Address {
        Address(arg)
    }

    /// The address as one word of a command line, such as the attach
    /// command's `--address A` or `$<NAME>`: the one place it is written out.
    pub fn as_arg(&self) -> &OsStr {
        &self.0
    }

    /// Connects to the daemon at this address.
    pub fn connect(&self) -> io::Result<UnixStream> {
        UnixStream::connect(&self.0)
    }
}

/// For messages: lossy where the path isn't Unicode.
impl std::fmt::Display for Address {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0.to_string_lossy())
    }
}

/// A daemon's directory, claimed: locked for as long as this lives.
#[derive(Debug)]
pub struct Claimed {
    name: String,
    dir: PathBuf,
    base: Base,
    /// The directory, open, holding the `flock`.
    lock: OwnedFd,
}

impl Claimed {
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn address(&self) -> Address {
        self.base.address(&self.name)
    }

    /// Where Neovim listens.
    pub fn nvim_sock(&self) -> PathBuf {
        self.dir.join(NVIM_SOCK)
    }

    /// The log's path.
    pub fn log(&self) -> String {
        self.dir.join(LOG).to_string_lossy().into_owned()
    }

    /// The `unsaved` file's path, for tests that check it.
    #[cfg(test)]
    pub(crate) fn unsaved_path(&self) -> PathBuf {
        self.dir.join(UNSAVED)
    }

    /// The daemon's socket file, for tests that check it.
    #[cfg(test)]
    pub(crate) fn sock_path(&self) -> PathBuf {
        self.dir.join(SOCK)
    }

    /// Listens at the daemon's address, which only this user can connect
    /// to. Never over a file that's there: claiming cleaned the directory.
    pub fn bind(&self) -> io::Result<UnixListener> {
        let sock = self.dir.join(SOCK);
        let listener = UnixListener::bind(&sock)?;
        owner_only(&sock)?;
        Ok(listener)
    }

    /// Makes Neovim's socket this user's alone: Neovim makes it under its
    /// umask, which its files need.
    pub fn own_nvim_sock(&self) -> io::Result<()> {
        owner_only(&self.nvim_sock())
    }

    /// Removes Neovim's socket, then the daemon's, as the value it returns
    /// drops. The directory stays, with the log.
    pub fn remove_sockets_on_drop(&self) -> RemoveSockets<'_> {
        RemoveSockets(self)
    }

    /// The `unsaved` file, written while the daemon is unattached with
    /// unsaved changes.
    pub(crate) fn unsaved(&self) -> Unsaved {
        Unsaved(self.dir.join(UNSAVED))
    }
}

/// A daemon's `unsaved` file (ADR 0004): `unsaved 1`, then the full name of
/// each buffer with unsaved changes, one a line, empty for an unnamed one.
#[derive(Debug, Clone)]
pub(crate) struct Unsaved(PathBuf);

impl Unsaved {
    /// Writes `files` whole, mode 0600, by renaming a finished file into
    /// place, so a reader never sees half of it. A name with a newline in it
    /// can't be one line, and is left out.
    pub(crate) fn write(&self, files: &[Vec<u8>]) -> io::Result<()> {
        let mut text = b"unsaved 1\n".to_vec();
        for f in files.iter().filter(|f| !f.contains(&b'\n')) {
            text.extend_from_slice(f);
            text.push(b'\n');
        }
        let mut tmp = self.0.clone().into_os_string();
        tmp.push(".tmp");
        let tmp = PathBuf::from(tmp);
        let _ = fs::remove_file(&tmp);
        let written = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&tmp)
            .and_then(|mut f| f.write_all(&text));
        match written.and_then(|()| fs::rename(&tmp, &self.0)) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(e)
            }
        }
    }

    /// Removes it, if it's there.
    pub(crate) fn remove(&self) {
        let _ = fs::remove_file(&self.0);
    }
}

/// Removes a daemon's socket files left over as it drops: Neovim's, then the
/// daemon's own, and its `unsaved`. A Neovim that was stuck or killed leaves
/// its socket behind.
pub struct RemoveSockets<'a>(&'a Claimed);

impl Drop for RemoveSockets<'_> {
    fn drop(&mut self) {
        for file in [NVIM_SOCK, SOCK, UNSAVED] {
            let _ = fs::remove_file(self.0.dir.join(file));
        }
    }
}

/// Makes the socket at `path` this user's alone: mode 0600. Until then, its
/// directory, 0700, keeps others out.
fn owner_only(path: &Path) -> io::Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

impl Drop for Claimed {
    fn drop(&mut self) {
        // unlocked outright: a child forked a moment ago shares the lock
        // until it execs
        // SAFETY: flock on an fd we own.
        unsafe { libc::flock(self.lock.as_raw_fd(), libc::LOCK_UN) };
    }
}

/// Claims `name` in `base`: makes or reuses its directory, locks it, checks
/// nothing answers on its sockets, and cleans what a daemon before left
/// there, its `log` becoming `log.1`. A directory that is locked, or whose
/// sockets answer, is [`Error::Live`].
pub fn claim(base: &Base, name: &str) -> Result<Claimed, Error> {
    let dir = base.0.join(name);
    let io = |e| Error::Io(dir.clone(), e);
    match fs::DirBuilder::new().mode(0o700).create(&dir) {
        Err(e) if e.kind() != io::ErrorKind::AlreadyExists => return Err(io(e)),
        _ => {}
    }
    let lock: OwnedFd = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(&dir)
        .map_err(io)?
        .into();
    // SAFETY: flock on an fd we own.
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        let e = io::Error::last_os_error();
        return Err(match e.raw_os_error() {
            Some(libc::EWOULDBLOCK) => Error::Live(name.to_string()),
            _ => io(e),
        });
    }
    for sock in [SOCK, NVIM_SOCK] {
        if UnixStream::connect(dir.join(sock)).is_ok() {
            return Err(Error::Live(name.to_string()));
        }
    }
    // stale: a daemon ended here, cleanly or not
    for leftover in [SOCK, NVIM_SOCK, UNSAVED] {
        match fs::remove_file(dir.join(leftover)) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io(e)),
            _ => {}
        }
    }
    match fs::rename(dir.join(LOG), dir.join(OLD_LOG)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(io(e)),
        _ => {}
    }
    Ok(Claimed {
        name: name.to_string(),
        dir,
        base: base.clone(),
        lock,
    })
}

/// Claims `name` in `base`, or with `dedup` the first of `name`, `name-2`,
/// `name-3`, … that isn't live. Each name's path length is checked first.
pub fn claim_name(base: &Base, name: &str, dedup_it: bool) -> Result<Claimed, Error> {
    let mut n = 1;
    loop {
        let candidate = dedup(name, n);
        socket_path_len_check(&base.0, &candidate)?;
        match claim(base, &candidate) {
            Err(Error::Live(_)) if dedup_it && n < 10_000 => n += 1,
            got => return got,
        }
    }
}

/// For tests.
#[cfg(test)]
pub(crate) mod testing {
    use std::ffi::OsString;
    use std::path::PathBuf;

    use crate::name::NAME;

    /// A new directory of a test's own, mode 0700, removed as it drops.
    pub(crate) struct Scratch(pub(crate) PathBuf);

    impl Scratch {
        pub(crate) fn new() -> Scratch {
            let template = std::env::temp_dir().join(format!("{NAME}-t.XXXXXX"));
            let mut bytes = template.into_os_string().into_encoded_bytes();
            bytes.push(0);
            // SAFETY: a writable, NUL-terminated template.
            assert!(!unsafe { libc::mkdtemp(bytes.as_mut_ptr().cast()) }.is_null());
            bytes.pop();
            // SAFETY: mkdtemp keeps the bytes it was given, but the X's.
            Scratch(PathBuf::from(unsafe {
                OsString::from_encoded_bytes_unchecked(bytes)
            }))
        }

        /// Its path, left for the caller to remove.
        pub(crate) fn keep(self) -> String {
            let path = self.0.to_string_lossy().into_owned();
            std::mem::forget(self);
            path
        }

        /// It, as a base directory.
        pub(crate) fn base(&self) -> super::Base {
            super::Base::at(&self.0)
        }

        /// The file `name` in it.
        pub(crate) fn file(&self, name: &str) -> String {
            self.0.join(name).to_string_lossy().into_owned()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testing::Scratch;
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    fn uid() -> u32 {
        // SAFETY: getuid can't fail.
        unsafe { libc::getuid() }
    }

    #[test]
    fn a_name_is_cleaned_cut_and_else_default() {
        let clean = |dir: &str| clean_name(OsStr::new(dir));
        assert_eq!(NAME, clean(NAME));
        assert_eq!("my_proj-1.2", clean("my_proj-1.2"));
        assert_eq!("myproject", clean("my project"));
        assert_eq!("caf", clean("café"));
        assert_eq!("a".repeat(32), clean(&"a".repeat(40)));
        for none in ["", "é", " ", ".", "..", "..é"] {
            assert_eq!("default", clean(none), "{none:?}");
        }
        // not valid UTF-8
        use std::os::unix::ffi::OsStrExt;
        assert_eq!("ab", clean_name(OsStr::from_bytes(b"a\xffb")));
    }

    #[test]
    fn a_name_is_1_to_32_of_the_allowed_characters() {
        let ok = |n: &str| valid_name(OsStr::new(n));
        assert!(ok("work"));
        assert!(ok(".x"));
        assert!(ok(&"a".repeat(32)));
        for bad in ["", ".", "..", "a b", "a/b", "é", &"a".repeat(33)] {
            assert!(!ok(bad), "{bad:?}");
        }
    }

    #[test]
    fn dedup_adds_a_number_within_32_characters() {
        assert_eq!("work", dedup("work", 1));
        assert_eq!("work-2", dedup("work", 2));
        assert_eq!("work-13", dedup("work", 13));
        let long = "a".repeat(32);
        assert_eq!(format!("{}-2", "a".repeat(30)), dedup(&long, 2));
        assert_eq!(format!("{}-10", "a".repeat(29)), dedup(&long, 10));
    }

    #[test]
    fn a_name_whose_socket_path_is_too_long_is_refused_with_its_length() {
        let base = PathBuf::from(format!("/tmp/{}", "b".repeat(MAX_PATH - 20)));
        // `<base>/` and `/nvim.sock`
        let room = MAX_PATH - base.as_os_str().len() - 1 - 10;
        assert!(socket_path_len_check(&base, &"w".repeat(room)).is_ok());
        let name = "w".repeat(room + 6);
        let err = socket_path_len_check(&base, &name).unwrap_err();
        assert_eq!(
            format!(
                "the socket path for {name} would be {} bytes, over the {MAX_PATH} allowed; \
                 use a shorter --name or {}_TMPDIR",
                MAX_PATH + 6,
                name::upper()
            ),
            err.to_string()
        );
    }

    #[test]
    fn the_root_is_the_tmpdir_variable_else_tmp() {
        assert_eq!(PathBuf::from("/tmp"), tmp_root(None));
        assert_eq!(PathBuf::from("/tmp"), tmp_root(Some(OsStr::new(""))));
        assert_eq!(
            PathBuf::from("/var/t"),
            tmp_root(Some(OsStr::new("/var/t")))
        );
        let relative = tmp_root(Some(OsStr::new("t")));
        assert_eq!(std::env::current_dir().unwrap().join("t"), relative);
    }

    #[test]
    fn the_base_directory_is_made_private_and_refused_otherwise() {
        let root = Scratch::new();
        let base = base_dir(&root.0, uid()).unwrap().0;
        assert_eq!(root.0.join(format!("{NAME}-{}", uid())), base);
        let meta = fs::symlink_metadata(&base).unwrap();
        assert!(meta.is_dir());
        assert_eq!(0o700, meta.mode() & 0o7777);
        // again, as it is
        assert_eq!(base, base_dir(&root.0, uid()).unwrap().0);

        // open to others
        fs::set_permissions(&base, fs::Permissions::from_mode(0o755)).unwrap();
        let err = base_dir(&root.0, uid()).unwrap_err();
        assert_eq!(
            format!(
                "{} isn't a private directory (owner {}, mode 0755); set {}_TMPDIR to another \
                 place",
                base.display(),
                uid(),
                name::upper()
            ),
            err.to_string()
        );
        fs::set_permissions(&base, fs::Permissions::from_mode(0o700)).unwrap();

        // someone else's: this user's directory, checked as another's
        let other = uid() + 1;
        fs::create_dir(root.0.join(format!("{NAME}-{other}"))).unwrap();
        let err = base_dir(&root.0, other).unwrap_err();
        assert!(
            matches!(err, Error::NotPrivate { owner, .. } if owner == uid()),
            "{err}"
        );

        // a link, even to a private directory
        fs::remove_dir(root.0.join(format!("{NAME}-{other}"))).unwrap();
        std::os::unix::fs::symlink(&base, root.0.join(format!("{NAME}-{other}"))).unwrap();
        let err = base_dir(&root.0, other).unwrap_err();
        assert!(matches!(err, Error::NotPrivate { .. }), "{err}");
    }

    #[test]
    fn a_claimed_directory_is_locked_against_a_second_claimant() {
        let base = Scratch::new();
        let first = claim(&base.base(), "work").unwrap();
        assert_eq!("work", first.name());
        let err = claim(&base.base(), "work").unwrap_err();
        assert!(matches!(&err, Error::Live(n) if n == "work"), "{err}");
        assert_eq!("work is already running", err.to_string());
        drop(first);
        // its lock went with it
        assert!(claim(&base.base(), "work").is_ok());
    }

    #[test]
    fn a_name_whose_socket_answers_is_live() {
        let base = Scratch::new();
        for sock in [SOCK, NVIM_SOCK] {
            let dir = base.0.join(sock.replace('.', "-"));
            fs::create_dir(&dir).unwrap();
            let _other = UnixListener::bind(dir.join(sock)).unwrap();
            let name = dir.file_name().unwrap().to_str().unwrap();
            assert!(
                matches!(claim(&base.base(), name), Err(Error::Live(_))),
                "{sock}"
            );
            // the other one's socket is left to it
            UnixStream::connect(dir.join(sock)).unwrap();
        }
    }

    #[test]
    fn a_stale_directory_is_reclaimed_its_log_kept_as_log_1() {
        let base = Scratch::new();
        let dir = base.0.join("work");
        fs::create_dir(&dir).unwrap();
        fs::write(dir.join(LOG), "last").unwrap();
        fs::write(dir.join(OLD_LOG), "older").unwrap();
        fs::write(dir.join(UNSAVED), "unsaved 1\n").unwrap();
        // a socket nothing answers on
        drop(UnixListener::bind(dir.join(SOCK)).unwrap());
        drop(UnixListener::bind(dir.join(NVIM_SOCK)).unwrap());

        let claimed = claim(&base.base(), "work").unwrap();
        assert_eq!("last", fs::read_to_string(dir.join(OLD_LOG)).unwrap());
        for gone in [LOG, SOCK, NVIM_SOCK, UNSAVED] {
            assert!(fs::symlink_metadata(dir.join(gone)).is_err(), "{gone}");
        }
        assert_eq!(dir.join(LOG).to_str().unwrap(), claimed.log());
        assert_eq!(dir.join(SOCK).as_os_str(), claimed.address().as_arg());
    }

    #[test]
    fn unsaved_lists_one_path_a_line_under_its_version_only_its_user_can_read() {
        let base = Scratch::new();
        let claimed = claim(&base.base(), "work").unwrap();
        let path = base.0.join("work").join(UNSAVED);
        let unsaved = claimed.unsaved();
        unsaved
            .write(&[b"/a/b.txt".to_vec(), Vec::new(), b"/c".to_vec()])
            .unwrap();
        assert_eq!(
            "unsaved 1\n/a/b.txt\n\n/c\n",
            fs::read_to_string(&path).unwrap()
        );
        assert_eq!(
            0o600,
            fs::metadata(&path).unwrap().permissions().mode() & 0o777
        );
        // written again whole, by a rename: nothing of the last is left
        unsaved.write(&[b"/d".to_vec()]).unwrap();
        assert_eq!("unsaved 1\n/d\n", fs::read_to_string(&path).unwrap());
        let names: Vec<_> = fs::read_dir(base.0.join("work"))
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(vec![std::ffi::OsString::from(UNSAVED)], names);
        unsaved.remove();
        assert!(fs::symlink_metadata(&path).is_err());
        // and gone with the sockets as the daemon ends
        unsaved.write(&[b"/d".to_vec()]).unwrap();
        drop(claimed.remove_sockets_on_drop());
        assert!(fs::symlink_metadata(&path).is_err());
    }

    #[test]
    fn a_directory_that_is_a_link_is_not_claimed() {
        let base = Scratch::new();
        let elsewhere = Scratch::new();
        std::os::unix::fs::symlink(&elsewhere.0, base.0.join("work")).unwrap();
        assert!(matches!(claim(&base.base(), "work"), Err(Error::Io(..))));
    }

    #[test]
    fn two_daemons_from_one_directory_are_named_x_and_x_2() {
        let base = Scratch::new();
        let x = claim_name(&base.base(), "x", true).unwrap();
        let x2 = claim_name(&base.base(), "x", true).unwrap();
        assert_eq!(("x", "x-2"), (x.name(), x2.name()));
        // without dedup, a live name is refused
        assert!(matches!(
            claim_name(&base.base(), "x", false),
            Err(Error::Live(_))
        ));
        drop(x);
        assert_eq!("x", claim_name(&base.base(), "x", true).unwrap().name());
    }

    #[test]
    fn dedup_checks_each_names_path_length() {
        let scratch = Scratch::new();
        // a base with room for a name of 3 bytes: `<base>/www/nvim.sock`
        let len = scratch.0.as_os_str().len();
        let pad = MAX_PATH - 1 - 3 - 10 - len - 1;
        let base = Base::at(&scratch.0.join("p".repeat(pad)));
        fs::create_dir(&base.0).unwrap();
        let first = claim_name(&base, "www", true).unwrap();
        // www-2 doesn't fit
        let err = claim_name(&base, "www", true).unwrap_err();
        assert!(
            matches!(&err, Error::PathTooLong { name, len } if name == "www-2" && *len == MAX_PATH + 2),
            "{err}"
        );
        drop(first);
        // too long from the start: nothing is made
        let err = claim_name(&base, "wwww", true).unwrap_err();
        assert!(matches!(err, Error::PathTooLong { .. }), "{err}");
        assert!(fs::symlink_metadata(base.0.join("wwww")).is_err());
    }
}
