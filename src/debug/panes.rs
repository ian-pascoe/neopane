//! `neopane debug-panes SOCKET`: a stand-in daemon. Takes terminals from
//! helpers run with the attach commands it prints, and drives them: shows
//! input and resizes. `q` in a pane closes it; Ctrl+C here closes them all.

use std::fs;
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixListener;
use std::path::Path;

use super::Result;
use super::pane_host::{MAX_PANES, PaneHost};
use crate::attach::{Transport, attach_command, attach_line};
use crate::daemon::event_loop::{Event, Loop};

const TOKEN_SIGNAL: usize = 0;
const TOKEN_SERVER: usize = 1;
const TOKEN_PANES: usize = 2;

pub fn run(socket_path: impl AsRef<Path>) -> Result<()> {
    let socket_path = socket_path.as_ref();
    let mut event_loop = Loop::init()?;
    event_loop.add_signal(libc::SIGINT, TOKEN_SIGNAL)?;
    event_loop.add_signal(libc::SIGTERM, TOKEN_SIGNAL)?;

    let server = listen(socket_path)?;
    let result = serve(&mut event_loop, &server, socket_path);
    let _ = fs::remove_file(socket_path);
    result
}

fn serve(event_loop: &mut Loop, server: &UnixListener, socket_path: &Path) -> Result<()> {
    event_loop.add_read(server.as_raw_fd(), TOKEN_SERVER)?;
    let mut host = PaneHost::new(TOKEN_PANES, "debug-panes");
    let exe = std::env::current_exe()?;
    eprintln!(
        "debug-panes: listening on {}. Run one of these in a terminal each. Ctrl+C quits.",
        socket_path.display()
    );
    for _ in 0..MAX_PANES {
        let words = attach_command(
            exe.as_ref(),
            socket_path.as_ref(),
            &host.mint(),
            Transport::Fd,
        );
        eprintln!("  {}", attach_line(&words).to_string_lossy());
    }

    let mut events = [Event::default(); 16];
    loop {
        let mut pending_accept = false;
        for e in event_loop.wait(&mut events)? {
            match e.token {
                TOKEN_SIGNAL => {
                    eprintln!("closing all panes");
                    return Ok(());
                }
                TOKEN_SERVER => pending_accept = true,
                token => host.handle(event_loop, token)?,
            }
        }
        if pending_accept {
            host.accept(event_loop, server)?;
        }
    }
}

/// Listens on `socket_path`, replacing a socket left by an earlier run.
pub(crate) fn listen(socket_path: impl AsRef<Path>) -> std::io::Result<UnixListener> {
    let socket_path = socket_path.as_ref();
    let _ = fs::remove_file(socket_path);
    UnixListener::bind(socket_path)
}
