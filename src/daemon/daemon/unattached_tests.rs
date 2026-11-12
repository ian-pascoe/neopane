//! Going unattached with a real Neovim (ADR 0003): what comes while the
//! daemon waits for Neovim's word on its unsaved changes.

use super::lifetime::Stage;
use super::self_attach_tests::Rig;
use super::*;
use crate::daemon::event_loop::Kind;

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
    // the way a signal comes
    rig.d
        .handle(Event {
            token: Token::Signal as usize,
            kind: Kind::default(),
            eof: false,
        })
        .unwrap();
    assert_eq!(Some(StopReason::Signal), rig.d.stopped);
}

#[test]
fn an_attach_during_the_safestate_wait_keeps_the_daemon_and_takes_the_kept_window() {
    let Some(mut rig) = waiting_for_safestate() else {
        return;
    };
    let kept = rig.d.lifetime.kept.expect("a window kept");
    let second = rig.attach(None, None);
    assert_eq!(kept, rig.win_of(second));
    // Neovim's answer, once it comes, ends nothing
    let deadline = Instant::now() + Duration::from_millis(2500);
    rig.pump(|_| Instant::now() >= deadline);
    assert_eq!(Stage::Attached, rig.d.lifetime.stage);
}
