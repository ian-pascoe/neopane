//! A daemon's life after its last pane, with a real Neovim (ADR 0003): it
//! ends through the channel closing with nothing unsaved, and outlives its
//! pane with unsaved changes, listing them, until an attach or a save.

use super::*;
use std::sync::{Arc, Mutex};

/// A launcher's terminal, attached by hand: a pty whose output is kept as
/// it comes, and the helper's end of its socket.
struct Launcher {
    stream: Option<UnixStream>,
    screen: Arc<Mutex<Vec<u8>>>,
}

impl Launcher {
    /// Attaches a new 80 x 24 pty to the daemon at `address`.
    fn attach(address: &Address) -> Result<Launcher> {
        let (master, slave) = crate::daemon::surface::open_pty(80, 24);
        let screen = Arc::new(Mutex::new(Vec::new()));
        let shown = Arc::clone(&screen);
        std::thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                // SAFETY: `buf` is valid for its length.
                let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
                if n <= 0 {
                    break;
                }
                shown.lock().unwrap().extend_from_slice(&buf[..n as usize]);
            }
            // SAFETY: closing the pty's master.
            unsafe { libc::close(master) };
        });
        let stream = fake_launcher_ready(address, slave)?;
        Ok(Launcher {
            stream: Some(stream),
            screen,
        })
    }

    /// Waits up to 10 s for its terminal to show `text`.
    fn until_shows(&self, text: &str) -> bool {
        within_10s(|| {
            let screen = self.screen.lock().unwrap();
            screen.windows(text.len()).any(|w| w == text.as_bytes())
        })
    }

    /// Its terminal goes, as closing its Rex pane does.
    fn hang_up(&mut self) {
        self.stream = None;
    }
}

/// Runs a daemon in `dir` over Neovim with `args`, its first pane a
/// launcher's, while `drive` runs on a thread of its own, given that
/// launcher. Returns how the daemon ended and what `drive` returned.
fn driven<T: Send + 'static>(
    dir: &Claimed,
    args: &[&str],
    drive: impl FnOnce(Launcher) -> T + Send + 'static,
) -> (Result<()>, T) {
    let address = dir.address();
    let mut drive = Some(drive);
    let mut driver = None;
    let result = serve(
        dir,
        args,
        Config {
            nvim_command: &["nvim", "--clean", "-n"],
            ready: Box::new(|| {
                let launcher = Launcher::attach(&address)?;
                let drive = drive.take().unwrap();
                driver = Some(std::thread::spawn(move || drive(launcher)));
                Ok(())
            }),
            ..Config::new()
        },
    );
    let driven = driver.unwrap().join().unwrap();
    (result, driven)
}

/// A file for test `tag` holding `text`, as Neovim names it: macOS's `/var`
/// is `/private/var`.
fn file_with(tag: &str, text: &str) -> String {
    let file = temp_file(tag);
    fs::write(&file, text).unwrap();
    fs::canonicalize(&file)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// Waits up to 10 s for `done`.
fn within_10s(mut done: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if done() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}

/// Quits the Neovim listening at its socket as it drops, unless disarmed:
/// a driver that fails mustn't leave its daemon unattached for good.
struct QuitOnDrop(Option<std::path::PathBuf>);

impl Drop for QuitOnDrop {
    fn drop(&mut self) {
        if let Some(sock) = self.0.take()
            && let Ok(stream) = UnixStream::connect(&sock)
        {
            let mut nvim = crate::nvim::nvim::for_test_on(stream);
            nvim.request("nvim_command", 1);
            msgpack::write_str(nvim.writer(), "qa!");
            let _ = nvim.flush();
        }
    }
}

/// The current window of the Neovim listening at `sock`.
fn current_win_at(sock: &std::path::Path) -> i64 {
    let mut nvim = crate::nvim::nvim::for_test_on(UnixStream::connect(sock).unwrap());
    let id = nvim.request("nvim_get_current_win", 0);
    nvim.flush().unwrap();
    let frame = crate::nvim::nvim::wait_response(&mut nvim, id);
    let rpc::Message::Response { mut result, .. } = frame.message().unwrap() else {
        unreachable!()
    };
    rpc::expect_handle(&mut result).unwrap()
}

/// Runs Ex command `cmd` in the Neovim listening at `sock`.
fn command_at(sock: &std::path::Path, cmd: &str) {
    let mut nvim = crate::nvim::nvim::for_test_on(UnixStream::connect(sock).unwrap());
    let id = nvim.request("nvim_command", 1);
    msgpack::write_str(nvim.writer(), cmd);
    nvim.flush().unwrap();
    crate::nvim::nvim::wait_response(&mut nvim, id);
}

#[test]
fn with_nothing_unsaved_the_last_pane_going_ends_neovim_through_its_channel() {
    if !have_nvim() {
        return;
    }
    let (_base, dir) = test_dir();
    let file = file_with("unchanged", "text\n");
    let marker = temp_file("vimleave-unattached");
    let vimleave = format!(
        "autocmd VimLeave * call writefile(['left'], '{}')",
        marker.display()
    );
    let started = Instant::now();
    let (result, shown) = driven(&dir, &["--cmd", &vimleave, &file], |mut l| {
        let shown = l.until_shows("text");
        l.hang_up();
        shown
    });
    let took = started.elapsed();
    let left = fs::read_to_string(&marker);
    let _ = fs::remove_file(&marker);
    let _ = fs::remove_file(&file);
    assert!(shown, "Neovim never drew");
    assert!(result.is_ok(), "{result:?}");
    assert!(took < Duration::from_secs(10), "{took:?}");
    assert_eq!("left\n", left.unwrap());
    assert!(!exists(dir.unsaved_path()));
}

#[test]
fn with_unsaved_changes_the_daemon_outlives_its_last_pane_until_they_are_saved() {
    if !have_nvim() {
        return;
    }
    let (_base, dir) = test_dir();
    let file = file_with("changed", "text\n");
    let unsaved = dir.unsaved_path();
    let nvim_sock = dir.nvim_sock();
    let address = dir.address();
    let args = [file.as_str(), "-c", "normal! ichanged"];
    let file_in = file.clone();
    let (result, said) = driven(&dir, &args, move |mut l| {
        let mut quit = QuitOnDrop(Some(nvim_sock.clone()));
        let mut said = vec![l.until_shows("changedtext")];
        let window = current_win_at(&nvim_sock);
        l.hang_up();
        // it stays, listing the file
        let listed = format!("unsaved 1\n{file_in}\n");
        said.push(within_10s(|| {
            fs::read_to_string(&unsaved).ok() == Some(listed.clone())
        }));
        // an attach takes the window it kept, and the list goes
        let mut again = Launcher::attach(&address).unwrap();
        said.push(again.until_shows("changedtext"));
        said.push(current_win_at(&nvim_sock) == window);
        said.push(within_10s(|| !exists(&unsaved)));
        // unattached again; saved, it ends
        again.hang_up();
        said.push(within_10s(|| exists(&unsaved)));
        command_at(&nvim_sock, "wall");
        quit.0 = None;
        said
    });
    let saved = fs::read_to_string(&file);
    let _ = fs::remove_file(&file);
    assert_eq!(vec![true; 6], said);
    assert!(result.is_ok(), "{result:?}");
    assert_eq!("changedtext\n", saved.unwrap());
    assert!(!exists(dir.unsaved_path()));
}

#[test]
fn a_save_on_focuslost_ends_the_daemon_as_its_last_pane_goes() {
    if !have_nvim() {
        return;
    }
    let (_base, dir) = test_dir();
    let file = file_with("autosave", "text\n");
    let args = [
        "--cmd",
        "autocmd FocusLost * silent! wall",
        file.as_str(),
        "-c",
        "normal! ichanged",
    ];
    let (result, shown) = driven(&dir, &args, |mut l| {
        let shown = l.until_shows("changedtext");
        l.hang_up();
        shown
    });
    let saved = fs::read_to_string(&file);
    let _ = fs::remove_file(&file);
    assert!(shown, "Neovim never drew");
    assert!(result.is_ok(), "{result:?}");
    assert_eq!("changedtext\n", saved.unwrap());
}

#[test]
fn an_attach_once_neovim_is_ending_is_refused_as_stopping() {
    if !have_nvim() {
        return;
    }
    let (_base, dir) = test_dir();
    let address = dir.address();
    // Neovim says it's ending, then takes a while to
    let ending = temp_file("ending");
    let vimleavepre = format!(
        "autocmd VimLeavePre * call writefile([], '{}') | lua vim.uv.sleep(2000)",
        ending.display()
    );
    let ending_in = ending.clone();
    let (result, refused) = driven(&dir, &["--cmd", &vimleavepre], move |mut l| {
        l.until_shows("~");
        l.hang_up();
        within_10s(|| exists(&ending_in));
        let stream = address.connect().unwrap();
        let hello = attach_conn::self_hello(attach_conn::test_terminal(80, 24));
        (&stream).write_all(&hello).unwrap();
        attach_conn::read_all_frames(&stream)
    });
    let _ = fs::remove_file(&ending);
    assert!(result.is_ok(), "{result:?}");
    let [Message::End(z)] = &refused[..] else {
        panic!("{refused:?}")
    };
    assert_eq!(Some(Refusal::Stopping), z.as_refusal());
}

/// The environment variable that makes `unattached_helper` run, naming a
/// base directory and a file.
fn unattached_helper_var() -> String {
    format!("{}_TEST_UNATTACHED", crate::name::upper())
}

/// For `the_log_names_the_unsaved_changes_a_daemon_stays_for`, in a process
/// of its own, its stderr the log: runs daemon `d` in the base directory
/// over the file, changed, and closes its pane.
#[test]
#[ignore = "a helper process, run by another test"]
fn unattached_helper() {
    let Some(var) = std::env::var_os(unattached_helper_var()) else {
        return;
    };
    let var = var.into_string().unwrap();
    let (base, file) = var.split_once('\n').unwrap();
    let dir = dir::claim(&dir::Base::at(std::path::Path::new(base)), "d").unwrap();
    let args = [file, "-c", "normal! ichanged"];
    let (result, shown) = driven(&dir, &args, |mut l| {
        let shown = l.until_shows("changedtext");
        l.hang_up();
        shown
    });
    assert!(shown, "Neovim never drew");
    result.unwrap();
}

#[test]
fn the_log_names_the_unsaved_changes_a_daemon_stays_for() {
    if !have_nvim() {
        return;
    }
    let base = Scratch::new();
    let file = file_with("logged", "text\n");
    let mut daemon = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "daemon::serve::tests::unattached::unattached_helper",
            "--ignored",
            "--nocapture",
        ])
        .env(
            unattached_helper_var(),
            format!("{}\n{file}", base.0.display()),
        )
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = daemon.stderr.take().unwrap();
    let log = Arc::new(Mutex::new(String::new()));
    let logged = Arc::clone(&log);
    std::thread::spawn(move || {
        for line in std::io::BufRead::lines(std::io::BufReader::new(stderr)) {
            let Ok(line) = line else { return };
            logged.lock().unwrap().push_str(&format!("{line}\n"));
        }
    });
    let dir = base.0.join("d");
    let stays = within_10s(|| exists(dir.join("unsaved")));
    let want = format!("staying unattached for unsaved changes in {file}\n");
    let named = within_10s(|| log.lock().unwrap().contains(&want));
    // saved, it ends; and anyway, it's killed
    if exists(dir.join("nvim.sock")) {
        command_at(&dir.join("nvim.sock"), "wall");
    }
    let ended = within_10s(|| matches!(daemon.try_wait(), Ok(Some(_))));
    if !ended {
        let _ = daemon.kill();
    }
    let status = daemon.wait().unwrap();
    let _ = fs::remove_file(&file);
    assert!(stays, "it didn't stay: {}", log.lock().unwrap());
    assert!(named, "{want:?} isn't in the log: {}", log.lock().unwrap());
    assert!(ended && status.success(), "{status:?}");
    assert!(!exists(dir.join("unsaved")));
}
