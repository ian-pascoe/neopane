//! Going unattached with a real Neovim (ADR 0003): what comes while the
//! daemon waits for Neovim's word on its unsaved changes.

use super::lifetime::Stage;
use super::self_attach_tests::Rig;
use super::*;
use crate::daemon::dir::{self, testing::Scratch};
use crate::daemon::event_loop::Kind;
use std::io::Read;
use std::os::unix::net::UnixStream;

/// A daemon over a Neovim with a changed buffer, busy for 1.5 s on
/// FocusLost, whose one pane has just gone: it waits for Neovim's next
/// SafeState. None without Neovim.
fn waiting_for_safestate() -> Option<Rig> {
    let mut rig = Rig::start(&[
        "--cmd",
        "autocmd FocusLost * lua vim.uv.sleep(1500)",
        "-c",
        "normal! ichanged",
    ])?;
    rig.win_of(1);
    rig.terms[0].helper = None;
    rig.pump(|r| matches!(r.d.lifetime.stage, Stage::Deciding { .. }));
    Some(rig)
}

#[test]
fn a_stop_during_the_safestate_wait_ends_the_daemon() {
    let Some(mut rig) = waiting_for_safestate() else {
        return;
    };
    let base = Scratch::new();
    let claimed = dir::claim(&base.base(), "d").unwrap();
    rig.d.lifetime.unsaved_file = Some(claimed.unsaved());
    // a channel of its own, to see Neovim end
    let probe = UnixStream::connect(&rig.listen).unwrap();
    // the way a signal comes
    rig.d
        .handle(Event {
            token: Token::Signal as usize,
            kind: Kind::default(),
            eof: false,
        })
        .unwrap();
    assert_eq!(Some(StopReason::Signal), rig.d.stopped);
    // as `serve` ends it: the daemon goes, closing Neovim's channel
    drop(rig);
    // fails on macOS once Neovim has hung up, as it may have by now
    let _ = probe.set_read_timeout(Some(Duration::from_secs(10)));
    let mut buf = [0u8; 4096];
    let ended = loop {
        match (&probe).read(&mut buf) {
            Ok(0) => break true,
            Ok(_) => continue,
            Err(_) => break false,
        }
    };
    assert!(ended, "Neovim is still running");
    assert!(!claimed.unsaved_path().exists(), "unsaved was left");
}

#[test]
fn an_attach_during_the_safestate_wait_keeps_the_daemon_and_takes_the_kept_window() {
    let Some(mut rig) = waiting_for_safestate() else {
        return;
    };
    let kept = rig.d.lifetime.kept.expect("a window kept").win;
    let second = rig.attach(None, None);
    assert_eq!(kept, rig.win_of(second));
    // Neovim's answer, once it comes, ends nothing
    let deadline = Instant::now() + Duration::from_millis(2500);
    rig.pump(|_| Instant::now() >= deadline);
    assert_eq!(Stage::Attached, rig.d.lifetime.stage);
}
