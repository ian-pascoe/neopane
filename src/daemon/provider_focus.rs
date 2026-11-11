//! Which terminal the pane provider has focused, as the daemon last saw or
//! asked, and the echo filter (#92 decision 10, #86 decision D8).
//!
//! The daemon asks `focus` through the focus queue (`asks.rs`); until the
//! last focus act it asked ends, and for 250 ms after one that worked, another
//! terminal the pane provider says has focus is only the echo of how things
//! were before: it's ignored. The same span is the one in which keys typed
//! into another terminal go to Neovim's current window. A focus act that
//! failed or was replaced brings no echo, so no span follows it.

use std::time::{Duration, Instant};

use super::provider::TerminalId;

/// How long after a focus act worked the pane provider's focus reports of
/// other terminals are still taken for echoes.
pub(crate) const ECHO_MS: u64 = 250;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum ProviderFocus {
    /// Nothing said or asked yet.
    #[default]
    Unknown,
    /// The pane provider said it has focused this terminal.
    Settled(TerminalId),
    /// The daemon asked it to focus `terminal`: `in_flight` focus acts are
    /// asked or waiting, and once none is, reports of other terminals are
    /// echoes until `until`, if the last one worked.
    Asked {
        terminal: TerminalId,
        in_flight: u32,
        until: Option<Instant>,
    },
}

impl ProviderFocus {
    /// Whether the pane provider has focused `t`, or will once the focus
    /// asked lands: asking for it again would do nothing.
    pub(crate) fn has_focused(&self, t: &TerminalId) -> bool {
        match self {
            ProviderFocus::Unknown => false,
            ProviderFocus::Settled(s) => s == t,
            // one that failed or was replaced doesn't land
            ProviderFocus::Asked {
                terminal,
                in_flight,
                until,
            } => terminal == t && (*in_flight > 0 || until.is_some()),
        }
    }

    /// A focus act for `t` is asked, or waits its turn.
    pub(crate) fn asked(&mut self, t: TerminalId) {
        let in_flight = match self {
            ProviderFocus::Asked { in_flight, .. } => *in_flight,
            _ => 0,
        };
        *self = ProviderFocus::Asked {
            terminal: t,
            in_flight: in_flight + 1,
            until: None,
        };
    }

    /// A focus act ended: it worked if `done`, else it failed, fell back or
    /// was replaced by a newer one.
    pub(crate) fn ended(&mut self, done: bool, now: Instant) {
        let ProviderFocus::Asked {
            in_flight, until, ..
        } = self
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
    /// have, nor an echo of how things were before the daemon's own focus.
    pub(crate) fn reported(&mut self, t: &TerminalId, now: Instant) -> bool {
        let echo = self.unanswered(now);
        let user = match self {
            ProviderFocus::Unknown => false,
            ProviderFocus::Settled(s) => s != t,
            // ours, landing: settled once nothing more is asked
            ProviderFocus::Asked {
                terminal,
                in_flight,
                ..
            } if terminal == t && *in_flight > 0 => return false,
            ProviderFocus::Asked { terminal, .. } if terminal == t => false,
            ProviderFocus::Asked { .. } if echo => return false,
            ProviderFocus::Asked { .. } => true,
        };
        *self = ProviderFocus::Settled(t.clone());
        user
    }

    /// Whether the pane provider last said it has focused `t`.
    pub(crate) fn settled_on(&self, t: &TerminalId) -> bool {
        matches!(self, ProviderFocus::Settled(s) if s == t)
    }

    /// Whether a focus act is asked or waiting, or one worked less than
    /// 250 ms ago.
    pub(crate) fn unanswered(&self, now: Instant) -> bool {
        match self {
            ProviderFocus::Asked {
                in_flight, until, ..
            } => *in_flight > 0 || until.is_some_and(|u| now < u),
            _ => false,
        }
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
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        assert!(!f.reported(&t("a"), now));
        assert!(f.has_focused(&t("a")));
        // the same again is nothing new; another is the user's move
        assert!(!f.reported(&t("a"), now));
        assert!(f.reported(&t("b"), now));
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
    fn another_terminals_report_is_an_echo_while_a_focus_is_unanswered_and_250_ms_after() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"), now);
        f.asked(t("b"));
        // from before ours landed
        assert!(!f.reported(&t("a"), now));
        assert!(f.unanswered(now));
        f.ended(true, now);
        assert!(!f.reported(&t("a"), now + ms(249)));
        assert!(f.unanswered(now + ms(249)));
        assert!(!f.unanswered(now + ms(250)));
        // after it, the user's
        assert!(f.reported(&t("a"), now + ms(250)));
        assert_eq!(ProviderFocus::Settled(t("a")), f);
    }

    #[test]
    fn our_own_terminal_reported_settles_without_being_followed() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.asked(t("b"));
        // landed before its answer came: still asked
        assert!(!f.reported(&t("b"), now));
        assert!(f.unanswered(now));
        f.ended(true, now);
        assert!(!f.reported(&t("b"), now));
        assert_eq!(ProviderFocus::Settled(t("b")), f);
        assert!(!f.unanswered(now));
    }

    #[test]
    fn only_the_last_of_two_focus_acts_settles() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("o"), now);
        f.asked(t("a"));
        f.asked(t("b"));
        f.ended(true, now);
        // the first one's echo, while the second is out
        assert!(!f.reported(&t("a"), now + ms(300)));
        assert!(f.has_focused(&t("b")));
        assert!(f.unanswered(now + ms(300)));
        f.ended(true, now + ms(300));
        assert!(!f.reported(&t("b"), now + ms(310)));
        assert_eq!(ProviderFocus::Settled(t("b")), f);
    }

    #[test]
    fn a_focus_that_failed_brings_no_echo_and_isnt_taken_as_landing() {
        let (mut f, now) = (ProviderFocus::default(), Instant::now());
        f.reported(&t("a"), now);
        f.asked(t("b"));
        f.ended(false, now);
        assert!(!f.unanswered(now));
        assert!(!f.has_focused(&t("b")));
        // where focus really is, at once
        assert!(f.reported(&t("a"), now));
    }
}
