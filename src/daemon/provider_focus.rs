//! Which terminal the pane provider has focused, as the daemon last saw or
//! asked (#92 decisions 10 and 19, #86 decision D8).
//!
//! The daemon asks `focus` through the focus queue (`asks.rs`). What the
//! pane provider then says has focus is the daemon's own focus landing while
//! a focus act is asked or waiting, and the user's move (or a
//! `focus_direction`'s) once none is: a pane provider tells nothing older
//! than the focus it answered, as Rex's revisions see to.
//!
//! The echo span is narrower: until the last focus act ends, and for 250 ms
//! after one that worked, a terminal's own focus-in report from a terminal
//! other than the one asked is taken for focus leaving it, not the user's
//! move, and keys typed into another terminal go to Neovim's current window.
//! A focus act that failed or was replaced brings no echo, so no span follows
//! it.
//!
//! A pane provider may hear focus move before it can tell where focus is:
//! the command runner's `events` line waits for a report, which waits
//! behind the layout acts, such as a new window's open (SPI-1190). It says
//! so (`heard`), and only then do these rules apply; a pane provider that
//! never does is followed as above:
//!
//! - **The hold.** A move heard to a terminal other than the ones known (the
//!   focus last said, and each asked since) parks the focus acts asked
//!   after it, the newest kept, until the pane provider tells where focus
//!   is (`reported`), or the hold's time is up: the user's move told then
//!   stands, and the focus parked is dropped; otherwise it is asked then.
//!   Before any focus is said, a line heard only says where focus starts,
//!   and holds nothing.
//! - **A move told as ours lands.** A report naming a terminal that isn't
//!   known tells the user's move: a pane provider that hears moves tells
//!   every focus change, so the focus it last said is where focus was. It
//!   is followed, and asked again once none lands (`take_reask`), unless a
//!   later move is heard or told first. Its own line may come late or
//!   never: herdr's open swaps panes, which moves focus for a moment. A
//!   move heard as ours lands, to a terminal not known, is the user's too.
//! - **Read again.** A report ignored as ours landed, while a move to a
//!   terminal not asked was heard, may have been the user's move back:
//!   focus is read again once none lands (`take_recheck`).

use std::time::{Duration, Instant};

use super::provider::TerminalId;

/// How long after a focus act worked terminals' focus-in reports are still
/// taken for echoes, and keys typed into another terminal rerouted.
pub(crate) const ECHO_MS: u64 = 250;

/// How long a move heard holds focus acts, at most, for the pane provider
/// to tell it, once nothing is in the way of the report that tells it.
pub(crate) const HOLD: Duration = Duration::from_millis(1000);

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct ProviderFocus {
    last: Last,
    hold: Option<Hold>,
    /// The user's move told as a focus act landed, to ask again once none
    /// does.
    reask: Option<TerminalId>,
    /// Whether the pane provider hears moves: it said it heard one.
    hears: bool,
}

/// A move heard and not told yet.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Hold {
    /// When its time is up, unless something in the way restarts it; None
    /// once the move was told.
    until: Option<Instant>,
    /// The newest focus act asked meanwhile.
    parked: Option<TerminalId>,
}

/// What the daemon last saw or asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
enum Last {
    /// Nothing said or asked yet.
    #[default]
    Unknown,
    /// The pane provider said it has focused this terminal.
    Settled(TerminalId),
    /// The daemon asked it to focus `terminal`.
    Asked(Asked),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Asked {
    terminal: TerminalId,
    /// Focus acts asked or waiting.
    in_flight: u32,
    /// Once none is, the echo span lasts until then, if the last worked.
    until: Option<Instant>,
    /// What a report may name without the user having moved: the focus
    /// said before the first, and each asked since; None if none was said.
    known: Option<Vec<TerminalId>>,
    /// Since the first of those in flight was asked.
    flight: Flight,
}

/// What happened since the first focus act still in flight was asked.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Flight {
    /// Each terminal asked.
    asked: Vec<TerminalId>,
    /// Each terminal the pane provider heard focus move to.
    heard: Vec<TerminalId>,
    /// Whether a report was ignored.
    ignored: bool,
}

impl ProviderFocus {
    /// Whether the pane provider has focused `t`, or will once the focus
    /// asked lands: asking for it again would do nothing.
    pub(crate) fn has_focused(&self, t: &TerminalId) -> bool {
        match &self.last {
            Last::Unknown => false,
            Last::Settled(s) => s == t,
            // one that failed or was replaced doesn't land
            Last::Asked(a) => a.terminal == *t && (a.in_flight > 0 || a.until.is_some()),
        }
    }

    /// A focus act for `t` is asked, or waits its turn. It replaces any
    /// focus parked or to ask again: it's newer.
    pub(crate) fn asked(&mut self, t: TerminalId) {
        self.reask = None;
        let (in_flight, mut known, mut flight) = match std::mem::take(&mut self.last) {
            Last::Asked(a) if a.in_flight > 0 => (a.in_flight, a.known, a.flight),
            Last::Asked(a) => (0, a.known, Flight::default()),
            Last::Settled(s) => (0, Some(vec![s]), Flight::default()),
            Last::Unknown => (0, None, Flight::default()),
        };
        if let Some(k) = known.as_mut().filter(|k| !k.contains(&t)) {
            k.push(t.clone());
        }
        flight.asked.push(t.clone());
        self.last = Last::Asked(Asked {
            terminal: t,
            in_flight: in_flight + 1,
            until: None,
            known,
            flight,
        });
    }

    /// A focus act ended: it worked if `done`, else it failed, fell back or
    /// was replaced by a newer one.
    pub(crate) fn ended(&mut self, done: bool, now: Instant) {
        let Last::Asked(a) = &mut self.last else {
            return;
        };
        a.in_flight = a.in_flight.saturating_sub(1);
        if a.in_flight == 0 && done {
            a.until = Some(now + Duration::from_millis(ECHO_MS));
        }
    }

    /// The pane provider says it has focused `t`. Whether that's the user
    /// moving focus, for the daemon to follow: not the first report, which
    /// only tells where focus starts, nor one of what it was already known to
    /// have, nor one while a focus act of the daemon's is still landing,
    /// unless it names a terminal not known, and the pane provider hears
    /// moves. Told when none lands, or the user's, it ends the hold; the
    /// user's drops the focus parked or to ask again.
    pub(crate) fn reported(&mut self, t: &TerminalId) -> bool {
        let user = match &mut self.last {
            Last::Unknown => false,
            Last::Settled(s) => s != t,
            Last::Asked(a) if a.in_flight > 0 => {
                let unknown = a.known.as_ref().is_some_and(|k| !k.contains(t));
                if !(unknown && self.hears) {
                    a.flight.ignored = true;
                    return false;
                }
                // still landing: settled once nothing more is asked
                self.ends_hold(true);
                return true;
            }
            Last::Asked(a) => a.terminal != *t,
        };
        self.last = Last::Settled(t.clone());
        self.ends_hold(user);
        user
    }

    /// The hold ends: told, with the user's move if `user`, which drops
    /// what was parked or to ask again.
    fn ends_hold(&mut self, user: bool) {
        if user {
            self.hold = None;
            self.reask = None;
        } else if let Some(h) = &mut self.hold {
            h.until = None;
        }
    }

    /// Whether the pane provider last said it has focused `t`.
    pub(crate) fn settled_on(&self, t: &TerminalId) -> bool {
        matches!(&self.last, Last::Settled(s) if s == t)
    }

    /// Whether a focus act is asked or waiting.
    pub(crate) fn landing(&self) -> bool {
        matches!(&self.last, Last::Asked(a) if a.in_flight > 0)
    }

    /// Whether a focus act is asked or waiting, or one worked less than
    /// 250 ms ago: the echo span.
    pub(crate) fn unanswered(&self, now: Instant) -> bool {
        match &self.last {
            Last::Asked(a) => a.in_flight > 0 || a.until.is_some_and(|u| now < u),
            _ => false,
        }
    }

    /// The terminals a report may name without the user having moved.
    fn known(&self) -> Option<&[TerminalId]> {
        match &self.last {
            Last::Unknown => None,
            Last::Settled(s) => Some(std::slice::from_ref(s)),
            Last::Asked(a) => a.known.as_deref(),
        }
    }

    /// The pane provider heard focus move to `t`, at `now`, and will tell
    /// where focus is once it has read it: whether that is the user's move,
    /// for the daemon to follow and ask again once none lands, being to a
    /// terminal not known as one lands, which may undo it. Otherwise,
    /// unless `t` is known or no focus was said yet, the hold starts, or
    /// starts again. A focus to ask again yields to it, unless `t` is that
    /// or one asked.
    pub(crate) fn heard(&mut self, t: &TerminalId, now: Instant) -> bool {
        self.hears = true;
        if let Last::Asked(a) = &mut self.last
            && a.in_flight > 0
        {
            a.flight.heard.push(t.clone());
        }
        let ours = match &self.last {
            Last::Asked(a) => a.flight.asked.contains(t),
            _ => false,
        };
        if self.reask.as_ref().is_some_and(|r| r != t) && !ours {
            self.reask = None;
        }
        if !self.known().is_some_and(|k| !k.contains(t)) {
            return false;
        }
        if self.landing() {
            return true;
        }
        let parked = self.hold.take().and_then(|h| h.parked);
        self.hold = Some(Hold {
            until: Some(now + HOLD),
            parked,
        });
        false
    }

    /// Whether the hold holds focus acts at `now`.
    pub(crate) fn is_holding(&self, now: Instant) -> bool {
        self.hold
            .as_ref()
            .and_then(|h| h.until)
            .is_some_and(|u| now < u)
    }

    /// When the hold's time is up, if one holds.
    pub(crate) fn hold_due(&self) -> Option<Instant> {
        self.hold.as_ref().and_then(|h| h.until)
    }

    /// Something in the way of the report that ends the hold went on at
    /// `now`: its time starts again.
    pub(crate) fn restart_hold(&mut self, now: Instant) {
        if let Some(u) = self.hold.as_mut().and_then(|h| h.until.as_mut()) {
            *u = (*u).max(now + HOLD);
        }
    }

    /// Parks a focus act for `t` if the hold holds at `now`, in place of
    /// any parked before: whether it did. Otherwise a hold over goes, with
    /// what it parked. Either way no focus is asked again: `t` is newer.
    pub(crate) fn park(&mut self, t: &TerminalId, now: Instant) -> bool {
        self.reask = None;
        if self.is_holding(now) {
            if let Some(h) = &mut self.hold {
                h.parked = Some(t.clone());
            }
            return true;
        }
        self.hold = None;
        false
    }

    /// The focus act parked, once the hold has ended at `now`, to be asked:
    /// none if the user's move was told.
    pub(crate) fn take_parked(&mut self, now: Instant) -> Option<TerminalId> {
        if self.is_holding(now) {
            return None;
        }
        self.hold.take().and_then(|h| h.parked)
    }

    /// The user's move to `t`, told as a focus act landed: asked again once
    /// none lands.
    pub(crate) fn ask_again(&mut self, t: TerminalId) {
        self.reask = Some(t);
    }

    /// The focus to ask again, once none lands.
    pub(crate) fn take_reask(&mut self) -> Option<TerminalId> {
        if self.landing() {
            return None;
        }
        self.reask.take()
    }

    /// Whether focus is to be read again, now that none lands: a report
    /// was ignored as one landed, while a move to a terminal not asked was
    /// heard. Asked once.
    pub(crate) fn take_recheck(&mut self) -> bool {
        let Last::Asked(a) = &mut self.last else {
            return false;
        };
        if a.in_flight > 0 {
            return false;
        }
        let f = std::mem::take(&mut a.flight);
        f.ignored && f.heard.iter().any(|h| !f.asked.contains(h))
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
    fn a_focus_asked_after_a_move_was_heard_is_parked_and_yields_to_the_users_move() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        // the user moves to c; the pane provider hears it, to tell once read
        f.heard(&t("c"), now);
        assert!(f.is_holding(now));
        assert!(f.park(&t("b"), now));
        assert_eq!(None, f.take_parked(now));
        // told: the user's move, which the focus parked yields to
        assert!(f.reported(&t("c")));
        assert!(!f.is_holding(now));
        assert_eq!(None, f.take_parked(now));
        assert!(f.settled_on(&t("c")));
    }

    #[test]
    fn a_move_heard_that_was_none_lets_the_newest_focus_parked_go() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("c"), now);
        f.park(&t("b"), now);
        f.park(&t("d"), now);
        // told where focus was all along
        assert!(!f.reported(&t("a")));
        assert_eq!(Some(t("d")), f.take_parked(now));
        assert_eq!(None, f.take_parked(now));
    }

    #[test]
    fn a_move_heard_as_ours_lands_to_a_terminal_not_known_is_the_users() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        assert!(!f.heard(&t("a"), now));
        assert!(!f.heard(&t("b"), now));
        // followed at once, not held for
        assert!(f.heard(&t("c"), now));
        assert!(!f.is_holding(now));
        // once none lands, it holds
        f.ended(true, now);
        assert!(!f.heard(&t("d"), now));
        assert!(f.is_holding(now));
    }

    #[test]
    fn a_focus_known_or_asked_heard_holds_nothing() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("a"), now);
        assert!(!f.is_holding(now));
        // the first of two asked, landing behind the second
        f.asked(t("b"));
        f.asked(t("c"));
        for s in ["a", "b", "c"] {
            f.heard(&t(s), now);
            assert!(!f.is_holding(now), "{s}");
        }
        f.ended(true, now);
        f.ended(true, now);
        f.heard(&t("b"), now);
        assert!(!f.is_holding(now));
    }

    #[test]
    fn before_any_focus_is_said_a_line_heard_only_says_where_it_starts() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.heard(&t("a"), now);
        assert!(!f.is_holding(now));
        f.asked(t("b"));
        f.heard(&t("a"), now);
        assert!(!f.is_holding(now));
    }

    #[test]
    fn a_hold_lasts_1_s_after_the_last_thing_in_its_way() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("c"), now);
        f.park(&t("b"), now);
        assert_eq!(Some(now + HOLD), f.hold_due());
        // an open the report waits behind still runs
        let later = now + HOLD * 3;
        f.restart_hold(later);
        assert_eq!(Some(later + HOLD), f.hold_due());
        assert!(f.is_holding(later + HOLD - ms(1)));
        assert_eq!(None, f.take_parked(later + HOLD - ms(1)));
        assert!(!f.is_holding(later + HOLD));
        assert_eq!(Some(t("b")), f.take_parked(later + HOLD));
    }

    #[test]
    fn a_focus_asked_once_the_hold_is_over_drops_the_one_parked() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.heard(&t("c"), now);
        f.park(&t("b"), now);
        // its time up, before anything lets the parked one go
        assert!(!f.park(&t("d"), now + HOLD));
        assert_eq!(None, f.take_parked(now + HOLD));
    }

    #[test]
    fn a_report_while_ours_lands_is_the_users_move_only_if_moves_are_heard_and_not_known() {
        let mut f = ProviderFocus::default();
        let now = Instant::now();
        f.reported(&t("a"));
        f.asked(t("b"));
        // none heard yet: the focus last said may be stale
        assert!(!f.reported(&t("d")));
        // heard, but known: read before ours landed, or a move back
        f.heard(&t("a"), now);
        assert!(!f.reported(&t("a")));
        // its own line may come late, or never
        assert!(f.reported(&t("d")));
        assert!(f.landing());
        f.ended(true, now);
        assert!(!f.landing());
    }

    #[test]
    fn the_users_move_told_as_ours_lands_is_asked_again_once_none_does() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        f.asked(t("b"));
        f.ask_again(t("d"));
        assert_eq!(None, f.take_reask());
        // ours landing, or the move itself, heard: nothing later
        f.heard(&t("b"), now);
        f.heard(&t("d"), now);
        f.ended(true, now);
        assert_eq!(Some(t("d")), f.take_reask());
        assert_eq!(None, f.take_reask());
    }

    #[test]
    fn a_later_move_heard_or_told_or_a_focus_asked_drops_the_one_to_ask_again() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        for later in ["heard", "told", "asked"] {
            f.asked(t("b"));
            f.ask_again(t("d"));
            match later {
                "heard" => assert!(!f.heard(&t("a"), now)),
                "told" => {
                    assert!(f.heard(&t("e"), now));
                    assert!(f.reported(&t("e")));
                }
                _ => f.asked(t("c")),
            }
            while f.landing() {
                f.ended(true, now);
            }
            assert_eq!(None, f.take_reask(), "{later}");
            f.reported(&t("a"));
        }
    }

    #[test]
    fn a_report_ignored_as_ours_lands_is_read_again_only_after_a_move_heard() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"));
        // only ours heard: no move
        f.asked(t("b"));
        f.heard(&t("b"), now);
        assert!(!f.reported(&t("a")));
        f.ended(true, now);
        assert!(!f.take_recheck());
        // a move back to a heard
        f.reported(&t("b"));
        f.asked(t("c"));
        f.heard(&t("b"), now);
        assert!(!f.reported(&t("b")));
        assert!(!f.take_recheck());
        f.ended(true, now);
        assert!(f.take_recheck());
        assert!(!f.take_recheck());
    }

    #[test]
    fn with_focus_unknown_nothing_told_while_ours_lands_is_the_users() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.asked(t("b"));
        f.heard(&t("a"), now);
        // where focus started, perhaps
        assert!(!f.reported(&t("a")));
        assert!(f.landing());
    }
}
