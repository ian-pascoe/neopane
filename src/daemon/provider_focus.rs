//! Which terminal the pane provider has focused, as the daemon last saw or
//! asked (#92 decisions 10 and 19, #86 decision D8).
//!
//! The daemon asks `focus` through the focus queue (`asks.rs`). What the
//! pane provider then says has focus is the daemon's own focus landing while
//! a focus act is asked or waiting, and the user's move (or a
//! `focus_direction`'s) once none is: a pane provider tells nothing older
//! than the focus it answered, as Rex's revisions see to. While one lands,
//! a report naming a terminal neither said before it nor asked since tells
//! a move made since: the user's, followed, and asked again after ours,
//! which may undo it. One naming a terminal that was may be the user's
//! move back: once none lands, focus is read again.
//!
//! The echo span is narrower: until the last focus act ends, and for 250 ms
//! after one that worked, a terminal's own focus-in report from a terminal
//! other than the one asked is taken for focus leaving it, not the user's
//! move, and keys typed into another terminal go to Neovim's current window.
//! A focus act that failed or was replaced brings no echo, so no span follows
//! it.
//!
//! A pane provider may hear focus move before it can tell it: the command
//! runner's `events` line waits for a report, which waits behind the
//! layout acts, such as the open of a window just made (SPI-1190). A move
//! heard to a terminal other than the one known or asked holds the focus
//! acts asked after it, the newest kept, until the pane provider tells
//! where focus is, or for 1 s at most: the user's move told then stands,
//! and the focus held is dropped; otherwise it is asked then.

use std::time::{Duration, Instant};

use super::provider::TerminalId;

/// How long after a focus act worked terminals' focus-in reports are still
/// taken for echoes, and keys typed into another terminal rerouted.
pub(crate) const ECHO_MS: u64 = 250;

/// How long a move heard holds focus acts, at most, for the pane provider
/// to tell it.
pub(crate) const HOLD_MS: u64 = 1000;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProviderFocus {
    last: Last,
    /// Until when a move heard, not told yet, holds focus acts.
    hold: Option<Instant>,
    /// The newest focus act held.
    held: Option<TerminalId>,
    /// Whether a report was ignored while a focus act landed: it may have
    /// been the user's move back, so focus is read again once none does.
    recheck: bool,
}

/// What the daemon last saw or asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Last {
    /// Nothing said or asked yet.
    #[default]
    Unknown,
    /// The pane provider said it has focused this terminal.
    Settled(TerminalId),
    /// The daemon asked it to focus `terminal`: `in_flight` focus acts are
    /// asked or waiting, and once none is, the echo span lasts until `until`,
    /// if the last one worked. `known` is what a report may name while one
    /// lands without the user having moved: the focus said before the
    /// first, and each asked since; None if no focus was said.
    Asked {
        terminal: TerminalId,
        in_flight: u32,
        until: Option<Instant>,
        known: Option<Vec<TerminalId>>,
    },
}

impl ProviderFocus {
    /// Whether the pane provider has focused `t`, or will once the focus
    /// asked lands: asking for it again would do nothing.
    pub(crate) fn has_focused(&self, t: &TerminalId) -> bool {
        match &self.last {
            Last::Unknown => false,
            Last::Settled(s) => s == t,
            // one that failed or was replaced doesn't land
            Last::Asked {
                terminal,
                in_flight,
                until,
                ..
            } => terminal == t && (*in_flight > 0 || until.is_some()),
        }
    }

    /// A focus act for `t` is asked, or waits its turn.
    pub(crate) fn asked(&mut self, t: TerminalId) {
        let (in_flight, mut known) = match std::mem::take(&mut self.last) {
            Last::Asked {
                in_flight, known, ..
            } => (in_flight, known),
            Last::Settled(s) => (0, Some(vec![s])),
            Last::Unknown => (0, None),
        };
        if let Some(k) = known.as_mut().filter(|k| !k.contains(&t)) {
            k.push(t.clone());
        }
        self.last = Last::Asked {
            terminal: t,
            in_flight: in_flight + 1,
            until: None,
            known,
        };
    }

    /// A focus act ended: it worked if `done`, else it failed, fell back or
    /// was replaced by a newer one.
    pub(crate) fn ended(&mut self, done: bool, now: Instant) {
        let Last::Asked {
            in_flight, until, ..
        } = &mut self.last
        else {
            return;
        };
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 && done {
            *until = Some(now + Duration::from_millis(ECHO_MS));
        }
    }

    /// The pane provider says it has focused `t`. Whether that's the user
    /// moving focus, for the daemon to follow: not the first report, which
    /// only tells where focus starts, nor one of what it was already known to
    /// have, nor one while a focus act of the daemon's is still landing that
    /// names a terminal it knew or asked: what it read may be older than
    /// ours. One naming another is the user's move, which ours may yet undo
    /// (`landing`). Told when no focus act lands, or the user's, it ends a
    /// hold, and the user's move drops the focus held.
    pub(crate) fn reported(&mut self, t: &TerminalId) -> bool {
        let user = match &self.last {
            Last::Unknown => false,
            Last::Settled(s) => s != t,
            Last::Asked {
                in_flight, known, ..
            } if *in_flight > 0 => {
                if !known.as_ref().is_some_and(|k| !k.contains(t)) {
                    self.recheck = true;
                    return false;
                }
                // still landing: settled once nothing more is asked
                self.hold = None;
                self.held = None;
                return true;
            }
            Last::Asked { terminal, .. } => terminal != t,
        };
        self.last = Last::Settled(t.clone());
        self.hold = None;
        if user {
            self.held = None;
        }
        user
    }

    /// Whether focus is to be read again: a report was ignored while focus
    /// acts landed, and none does any more. Asked once.
    pub(crate) fn take_recheck(&mut self) -> bool {
        if self.landing() {
            return false;
        }
        std::mem::take(&mut self.recheck)
    }

    /// Whether a focus act is asked or waiting: it may land after a move
    /// told meanwhile, and undo it.
    pub(crate) fn landing(&self) -> bool {
        matches!(&self.last, Last::Asked { in_flight, .. } if *in_flight > 0)
    }

    /// Whether the pane provider last said it has focused `t`.
    pub(crate) fn settled_on(&self, t: &TerminalId) -> bool {
        matches!(&self.last, Last::Settled(s) if s == t)
    }

    /// Whether a focus act is asked or waiting, or one worked less than
    /// 250 ms ago: the echo span.
    pub(crate) fn unanswered(&self, now: Instant) -> bool {
        match &self.last {
            Last::Asked {
                in_flight, until, ..
            } => *in_flight > 0 || until.is_some_and(|u| now < u),
            _ => false,
        }
    }

    /// The pane provider heard focus move to `t`, at `now`, and will tell
    /// where focus is once it has read it. Unless `t` is the focus known or
    /// asked, focus acts asked from now are held until then, or 1 s.
    pub(crate) fn heard(&mut self, t: &TerminalId, now: Instant) {
        if !self.has_focused(t) {
            self.hold = Some(now + Duration::from_millis(HOLD_MS));
        }
    }

    /// Whether a focus act asked at `now` is held: a move heard isn't told.
    pub(crate) fn holds(&self, now: Instant) -> bool {
        self.hold.is_some_and(|h| now < h)
    }

    /// When the hold ends at the latest, if one holds.
    pub(crate) fn hold_due(&self) -> Option<Instant> {
        self.hold
    }

    /// Holds a focus act for `t`, in place of any held before.
    pub(crate) fn hold(&mut self, t: TerminalId) {
        self.held = Some(t);
    }

    /// The focus act held, once nothing holds it at `now`, to be asked: none
    /// once the user's move was told.
    pub(crate) fn take_held(&mut self, now: Instant) -> Option<TerminalId> {
        if self.holds(now) {
            return None;
        }
        self.hold = None;
        self.held.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> TerminalId {
        TerminalId::from(s)
    }

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_first_report_only_says_where_focus_starts() {
        let mut f = ProviderFocus::default();
        assert!(!f.reported(&t("a")));
        assert!(f.has_focused(&t("a")));
        // the same again is nothing new; another is the user's move
        assert!(!f.reported(&t("a")));
        assert!(f.reported(&t("b")));
        assert!(!f.has_focused(&t("a")));
    }

    #[test]
    fn asking_for_what_is_focused_or_asked_already_would_do_nothing() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        assert!(!f.has_focused(&t("a")));
        f.asked(t("a"));
        assert!(f.has_focused(&t("a")));
        f.asked(t("b"));
        assert!(!f.has_focused(&t("a")));
        assert!(f.has_focused(&t("b")));
        // one that worked stays focused, its echo span over, no view yet
        f.ended(false, now);
        f.ended(true, now);
        assert!(f.has_focused(&t("b")));
        assert!(!f.unanswered(now + ms(ECHO_MS)));
        assert!(f.has_focused(&t("b")));
    }

    #[test]
    fn the_echo_span_lasts_while_a_focus_is_unanswered_and_250_ms_after() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        assert!(f.unanswered(now));
        f.ended(true, now);
        assert!(f.unanswered(now + ms(249)));
        assert!(!f.unanswered(now + ms(250)));
    }

    #[test]
    fn another_terminal_reported_while_a_focus_lands_is_ignored_and_after_it_followed() {
        let mut f = ProviderFocus::default();
        f.reported(&t("a"));
        f.asked(t("b"));
        // from before ours landed
        assert!(!f.reported(&t("a")));
        assert!(f.has_focused(&t("b")));
        // answered: a move at once, the span or not, is the user's or a
        // focus_direction's
        f.ended(true, Instant::now());
        assert!(f.reported(&t("c")));
        assert!(f.settled_on(&t("c")));
    }

    #[test]
    fn our_own_terminal_reported_settles_without_being_followed() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.asked(t("b"));
        // landed before its answer came: still asked
        assert!(!f.reported(&t("b")));
        assert!(f.unanswered(now));
        f.ended(true, now);
        assert!(!f.reported(&t("b")));
        assert!(f.settled_on(&t("b")));
        assert!(!f.unanswered(now));
    }

    #[test]
    fn only_the_last_of_two_focus_acts_settles() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("o"));
        f.asked(t("a"));
        f.asked(t("b"));
        f.ended(true, now);
        // the first one's landing, while the second is out
        assert!(!f.reported(&t("a")));
        assert!(f.has_focused(&t("b")));
        f.ended(true, now);
        assert!(!f.reported(&t("b")));
        assert!(f.settled_on(&t("b")));
    }

    #[test]
    fn a_focus_that_failed_brings_no_echo_and_isnt_taken_as_landing() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        f.ended(false, now);
        assert!(!f.unanswered(now));
        assert!(!f.has_focused(&t("b")));
        // where focus really is, at once
        assert!(f.reported(&t("a")));
    }

    #[test]
    fn a_focus_asked_after_a_move_was_heard_waits_and_yields_to_the_users_move() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        // the user moves to c; the pane provider hears it, to tell once read
        f.heard(&t("c"), now);
        assert!(f.holds(now));
        f.hold(t("b"));
        assert_eq!(None, f.take_held(now));
        // told: the user's move, which the focus held yields to
        assert!(f.reported(&t("c")));
        assert!(!f.holds(now));
        assert_eq!(None, f.take_held(now));
        assert!(f.settled_on(&t("c")));
    }

    #[test]
    fn a_move_heard_that_was_none_lets_the_newest_focus_held_go() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("c"), now);
        f.hold(t("b"));
        f.hold(t("d"));
        // told where focus was all along
        assert!(!f.reported(&t("a")));
        assert_eq!(Some(t("d")), f.take_held(now));
        assert_eq!(None, f.take_held(now));
    }

    #[test]
    fn our_own_focus_heard_or_the_one_known_holds_nothing() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("a"), now);
        assert!(!f.holds(now));
        f.asked(t("b"));
        f.heard(&t("b"), now);
        assert!(!f.holds(now));
        f.ended(true, now);
        f.heard(&t("b"), now);
        assert!(!f.holds(now));
    }

    #[test]
    fn a_move_heard_holds_focus_for_1_s_at_most() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("c"), now);
        f.hold(t("b"));
        assert_eq!(Some(now + ms(HOLD_MS)), f.hold_due());
        assert!(f.holds(now + ms(HOLD_MS - 1)));
        assert_eq!(None, f.take_held(now + ms(HOLD_MS - 1)));
        assert!(!f.holds(now + ms(HOLD_MS)));
        assert_eq!(Some(t("b")), f.take_held(now + ms(HOLD_MS)));
    }

    #[test]
    fn a_report_ignored_while_a_focus_lands_doesnt_end_the_hold() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        f.heard(&t("c"), now);
        f.hold(t("d"));
        // read before ours landed
        assert!(!f.reported(&t("a")));
        assert!(f.holds(now));
        f.ended(true, now);
        // told once ours is answered: the user's move
        assert!(f.reported(&t("c")));
        assert_eq!(None, f.take_held(now));
    }

    #[test]
    fn told_while_ours_lands_a_terminal_neither_known_nor_asked_is_the_users_move() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        f.asked(t("c"));
        // read before ours landed, or ours landing
        for s in ["a", "b", "c"] {
            assert!(!f.reported(&t(s)), "{s}");
        }
        // the user's, which ours may yet undo
        assert!(f.reported(&t("d")));
        assert!(f.landing());
        // asked again after ours, it's known: ignored as it lands
        f.asked(t("d"));
        assert!(!f.reported(&t("d")));
        for _ in 0..3 {
            f.ended(true, now);
        }
        assert!(!f.landing());
        assert!(!f.reported(&t("d")));
        assert!(f.settled_on(&t("d")));
    }

    #[test]
    fn a_report_ignored_while_ours_lands_is_read_again_once_none_does() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        f.asked(t("c"));
        // older than ours, or the user's move back
        assert!(!f.reported(&t("a")));
        f.ended(true, now);
        assert!(!f.take_recheck());
        f.ended(true, now);
        assert!(f.take_recheck());
        assert!(!f.take_recheck());
        // none ignored: nothing to read again
        f.asked(t("a"));
        f.ended(true, now);
        assert!(!f.take_recheck());
    }

    #[test]
    fn with_focus_unknown_nothing_told_while_ours_lands_is_the_users() {
        let mut f = ProviderFocus::default();
        f.asked(t("b"));
        // where focus started, perhaps
        assert!(!f.reported(&t("a")));
        assert!(f.landing());
    }
}
