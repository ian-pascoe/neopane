//! Acts: the asks the daemon makes of its pane provider, and what becomes of
//! them (#92 decision 4, ADR 0006).
//!
//! An act whose capability the pane provider lacks, or was found not to
//! support, falls back at once; any other is asked. Layout acts go one at a
//! time, each asked only once the one before is answered or timed out. Focus
//! acts have their own queue: one running and at most one waiting, a newer
//! one replacing it, as only the last place focus goes matters. An
//! ask that is unsupported marks its capability so; one that fails or times
//! out falls back, the first failure of each capability warning in Neovim.
//! A terminal id answered too late is closed.
//!
//! `Asks` only keeps the books: what the daemon has to do about each, it
//! reads as `Step`s.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::provider::{
    Answer, Ask, AskId, Capability, CapabilitySet, Outcome, PaneProvider, TerminalId,
};
use crate::log::{log_info, log_warn};
use crate::name::NAME;
use crate::util::seconds;

/// Most characters of a failure's reason a warning shows (#93 decision 20).
const WHY_MAX: usize = 200;

/// What the daemon has to do about an act.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// It worked: what was asked, and the answer.
    Done(Ask, Answer),
    /// It can't be done: do what the daemon does without it.
    FallBack(Ask),
    /// A newer focus act took its place before it was asked: it never will be.
    Replaced(Ask),
    /// Show this in Neovim as a warning.
    Warn(String),
}

/// An act asked of the pane provider and not answered yet.
#[derive(Debug)]
struct Sent {
    id: AskId,
    ask: Ask,
    due: Instant,
    /// The time it had.
    timeout: Duration,
}

#[derive(Debug, Default)]
pub(crate) struct Asks {
    last_id: u64,
    /// Asked, unanswered: at most one layout act and one focus act.
    sent: Vec<Sent>,
    /// Layout acts waiting their turn, oldest first.
    queue: VecDeque<(AskId, Ask)>,
    /// The focus act waiting for the one running.
    focus_waiting: Option<(AskId, Ask)>,
    /// Capabilities the pane provider turned out not to support.
    unsupported: CapabilitySet,
    /// Capabilities whose failure has warned.
    warned: CapabilitySet,
    steps: VecDeque<Step>,
}

impl Asks {
    /// Asks `p` for `ask`, now or once the layout acts before it are done;
    /// or, without `p` or the capability, falls back.
    pub(crate) fn act(&mut self, p: Option<&mut dyn PaneProvider>, ask: Ask, now: Instant) {
        let cap = ask.capability();
        let Some(p) = p.filter(|p| self.can(&**p, cap)) else {
            self.steps.push_back(Step::FallBack(ask));
            return;
        };
        self.last_id += 1;
        let id = AskId(self.last_id);
        if cap.is_layout() {
            self.queue.push_back((id, ask));
            self.next_layout_act(p, now);
        } else if self.focus_running() {
            if let Some((_, old)) = self.focus_waiting.replace((id, ask)) {
                self.steps.push_back(Step::Replaced(old));
            }
        } else {
            self.send(p, id, ask, now);
        }
    }

    /// Whether `p` is there and may be asked for `cap`.
    pub(crate) fn may_ask(&self, p: Option<&dyn PaneProvider>, cap: Capability) -> bool {
        p.is_some_and(|p| self.can(p, cap))
    }

    /// Whether `p` may be asked for `cap`.
    fn can(&self, p: &dyn PaneProvider, cap: Capability) -> bool {
        p.capabilities().has(cap) && !self.unsupported.has(cap)
    }

    /// Asks for the next layout act waiting, if none is running.
    fn next_layout_act(&mut self, p: &mut dyn PaneProvider, now: Instant) {
        if self.sent.iter().any(|s| s.ask.capability().is_layout()) {
            return;
        }
        while let Some((id, ask)) = self.queue.pop_front() {
            // found unsupported while it waited
            if !self.can(p, ask.capability()) {
                self.steps.push_back(Step::FallBack(ask));
                continue;
            }
            self.send(p, id, ask, now);
            return;
        }
    }

    /// Whether a focus act is asked and unanswered.
    fn focus_running(&self) -> bool {
        self.sent.iter().any(|s| !s.ask.capability().is_layout())
    }

    /// Asks for the focus act waiting, if none is running.
    fn next_focus_act(&mut self, p: &mut dyn PaneProvider, now: Instant) {
        if self.focus_running() {
            return;
        }
        let Some((id, ask)) = self.focus_waiting.take() else {
            return;
        };
        if self.can(p, ask.capability()) {
            self.send(p, id, ask, now);
        } else {
            self.steps.push_back(Step::FallBack(ask));
        }
    }

    fn send(&mut self, p: &mut dyn PaneProvider, id: AskId, ask: Ask, now: Instant) {
        let timeout = p.timeout(ask.capability());
        p.ask(id, ask.clone());
        self.sent.push(Sent {
            id,
            ask,
            due: now + timeout,
            timeout,
        });
    }

    /// `p` answered ask `id`.
    pub(crate) fn answered(
        &mut self,
        p: &mut dyn PaneProvider,
        id: AskId,
        outcome: Outcome,
        now: Instant,
    ) {
        let Some(i) = self.sent.iter().position(|s| s.id == id) else {
            // its act timed out and fell back: nothing will use the terminal
            if let Outcome::Done(Answer::Terminal(t)) = outcome {
                log_info!("{NAME} daemon: {}: closing {t}, opened too late", p.name());
                self.act(Some(p), Ask::Close { terminal: t }, now);
            }
            return;
        };
        let Sent { ask, .. } = self.sent.remove(i);
        let cap = ask.capability();
        match outcome {
            Outcome::Done(answer) => self.steps.push_back(Step::Done(ask, answer)),
            Outcome::Unsupported => {
                log_info!("{NAME} daemon: {}: {cap} is unsupported", p.name());
                self.unsupported.insert(cap);
                self.steps.push_back(Step::FallBack(ask));
            }
            Outcome::Failed(why) => {
                self.failed(p.name(), cap, "failed", Some(&why));
                self.steps.push_back(Step::FallBack(ask));
            }
        }
        self.next_layout_act(p, now);
        self.next_focus_act(p, now);
    }

    /// Fails the acts whose time is up at `now`.
    pub(crate) fn time_out(&mut self, p: &mut dyn PaneProvider, now: Instant) {
        let mut i = 0;
        while i < self.sent.len() {
            if self.sent[i].due > now {
                i += 1;
                continue;
            }
            let Sent { ask, timeout, .. } = self.sent.remove(i);
            let cap = ask.capability();
            let secs = seconds(timeout);
            self.failed(p.name(), cap, &format!("timed out after {secs} s"), None);
            self.steps.push_back(Step::FallBack(ask));
        }
        self.next_layout_act(p, now);
        self.next_focus_act(p, now);
    }

    /// The pane provider failed as a whole: every act asked or waiting falls
    /// back.
    pub(crate) fn gone(&mut self) {
        for s in std::mem::take(&mut self.sent) {
            self.steps.push_back(Step::FallBack(s.ask));
        }
        let waiting = std::mem::take(&mut self.queue);
        for (_, ask) in waiting.into_iter().chain(self.focus_waiting.take()) {
            self.steps.push_back(Step::FallBack(ask));
        }
    }

    /// Logs a failed act, and `why`; its capability's first failure warns in
    /// Neovim too, as #93 decision 20 words it, with the first line of `why`
    /// cut to 200 characters.
    fn failed(&mut self, provider: &str, cap: Capability, what: &str, why: Option<&str>) {
        let line = format!("{provider}: {cap} {what}");
        let Some(why) = why else {
            log_warn!("{NAME} daemon: {line}");
            self.warn(cap, line);
            return;
        };
        log_warn!("{NAME} daemon: {line}: {why}");
        let first: String = why
            .lines()
            .next()
            .unwrap_or("")
            .chars()
            .take(WHY_MAX)
            .collect();
        self.warn(cap, format!("{line}: {first}"));
    }

    /// Warns of `line` in Neovim, if `cap` hasn't warned yet.
    fn warn(&mut self, cap: Capability, line: String) {
        if !self.warned.has(cap) {
            self.warned.insert(cap);
            self.steps.push_back(Step::Warn(format!("{NAME}: {line}")));
        }
    }

    /// Whether an act of `cap` waits its turn, not asked yet.
    pub(crate) fn waits(&self, cap: Capability) -> bool {
        let queued = self.queue.iter().map(|(_, a)| a);
        queued
            .chain(self.focus_waiting.as_ref().map(|(_, a)| a))
            .any(|a| a.capability() == cap)
    }

    /// Whether an act of `cap` is asked and unanswered.
    pub(crate) fn asked(&self, cap: Capability) -> bool {
        self.sent.iter().any(|s| s.ask.capability() == cap)
    }

    /// Whether a `place_beside` or `resize` is asked or waiting.
    pub(crate) fn placing(&self) -> bool {
        let asked = self.sent.iter().map(|s| &s.ask);
        let waiting = self.queue.iter().map(|(_, a)| a);
        asked.chain(waiting).any(is_placement)
    }

    /// Whether `step` settles the placements: it ends a `place_beside` or a
    /// `resize`, and none is left asked or waiting. Where L8's span ends
    /// (`Point::Placed`).
    pub(crate) fn settles_placement(&self, step: &Step) -> bool {
        let ends_one = match step {
            Step::Done(ask, _) | Step::FallBack(ask) => is_placement(ask),
            Step::Replaced(_) | Step::Warn(_) => false,
        };
        ends_one && !self.placing()
    }

    /// The next thing the daemon has to do about an act.
    pub(crate) fn next_step(&mut self) -> Option<Step> {
        self.steps.pop_front()
    }

    /// When the soonest unanswered act times out.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.sent.iter().map(|s| s.due).min()
    }

    /// How many acts are asked or waiting.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.sent.len() + self.queue.len() + usize::from(self.focus_waiting.is_some())
    }
}

/// Whether `ask` places a terminal: a `place_beside` or a `resize`.
pub(crate) fn is_placement(ask: &Ask) -> bool {
    matches!(
        ask.capability(),
        Capability::PlaceBeside | Capability::Resize
    )
}

/// The ask that closes terminal `t`.
pub(crate) fn close(t: TerminalId) -> Ask {
    Ask::Close { terminal: t }
}

#[cfg(test)]
mod tests {
    use super::super::provider::fake::FakeProvider;
    use super::super::provider::{Direction, ProviderEvent};
    use super::*;
    use crate::attach::PaneId;

    fn open(n: u32) -> Ask {
        Ask::OpenTabPage {
            pane: PaneId::parse(&format!("{n}-abcdefghij")).unwrap(),
            attach: vec!["attach".into()],
            title: String::new(),
            origin: TerminalId::from("t0"),
            cols: 80,
            rows: 24,
        }
    }

    /// Hands `asks` every answer `fake` has, at `now`.
    fn deliver(asks: &mut Asks, fake: &mut FakeProvider, now: Instant) {
        while let Some(e) = fake.next() {
            if let ProviderEvent::Answered { id, outcome } = e {
                asks.answered(fake, id, outcome, now);
            }
        }
    }

    fn steps(asks: &mut Asks) -> Vec<Step> {
        std::iter::from_fn(|| asks.next_step()).collect()
    }

    fn opened(t: &str) -> Answer {
        Answer::Terminal(TerminalId::from(t))
    }

    fn resize(n: u32) -> Ask {
        Ask::Resize {
            terminal: TerminalId::from(format!("t{n}").as_str()),
            origin: TerminalId::from("t0"),
            dir: Direction::Right,
            size: 40,
            ratio: 0.5,
        }
    }

    #[test]
    fn placing_lasts_until_the_last_placement_is_answered_which_alone_settles_them() {
        let caps = [Capability::Resize, Capability::OpenTabPage];
        let (mut asks, mut fake, now) =
            (Asks::default(), FakeProvider::with(&caps), Instant::now());
        assert!(!asks.placing());
        asks.act(Some(&mut fake), resize(1), now);
        // waits its turn behind the first
        asks.act(Some(&mut fake), resize(2), now);
        assert!(asks.placing());

        // a refused one is answered too, but the second is now asked
        fake.answer(fake.last_id(), Outcome::Failed("no".into()));
        deliver(&mut asks, &mut fake, now);
        assert!(asks.placing());
        let first = steps(&mut asks);
        assert!(first.contains(&Step::FallBack(resize(1))));
        assert!(first.iter().all(|s| !asks.settles_placement(s)));

        // an open behind it doesn't count
        asks.act(Some(&mut fake), open(3), now);
        fake.answer(fake.asks[1].0, Outcome::Done(Answer::Nothing));
        deliver(&mut asks, &mut fake, now);
        assert!(!asks.placing());
        assert_eq!(1, asks.waiting());
        let last = steps(&mut asks);
        assert_eq!(vec![Step::Done(resize(2), Answer::Nothing)], last);
        assert!(asks.settles_placement(&last[0]));
        // nor do its steps, or a warning
        assert!(!asks.settles_placement(&Step::FallBack(open(3))));
        assert!(!asks.settles_placement(&Step::Warn(String::new())));
    }

    #[test]
    fn a_second_open_is_asked_only_once_the_first_is_answered() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        asks.act(Some(&mut fake), open(2), now);
        assert_eq!(vec![open(1)], fake.asked());

        fake.opened(fake.last_id(), "t1");
        deliver(&mut asks, &mut fake, now);
        assert_eq!(vec![open(1), open(2)], fake.asked());
        assert_eq!(vec![Step::Done(open(1), opened("t1"))], steps(&mut asks));
    }

    #[test]
    fn a_second_open_is_asked_once_the_first_times_out() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        asks.act(Some(&mut fake), open(2), now);
        asks.time_out(&mut fake, now + Duration::from_millis(4999));
        assert_eq!(vec![open(1)], fake.asked());
        asks.time_out(&mut fake, now + Duration::from_secs(5));
        assert_eq!(vec![open(1), open(2)], fake.asked());
    }

    #[test]
    fn an_act_past_its_deadline_falls_back_and_its_first_failure_warns() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        assert_eq!(Some(now + Duration::from_secs(5)), asks.deadline());
        asks.time_out(&mut fake, now + Duration::from_secs(5));
        assert_eq!(
            vec![
                Step::Warn(format!("{NAME}: fake: open_tab_page timed out after 5 s")),
                Step::FallBack(open(1)),
            ],
            steps(&mut asks)
        );
        assert_eq!(None, asks.deadline());

        // the second failure of it only falls back
        asks.act(Some(&mut fake), open(2), now);
        fake.answer(fake.last_id(), Outcome::Failed("no room".into()));
        deliver(&mut asks, &mut fake, now);
        assert_eq!(vec![Step::FallBack(open(2))], steps(&mut asks));
    }

    #[test]
    fn a_failures_warning_shows_the_first_line_of_its_reason_cut_to_200_characters() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        let why = format!("refused: {}\nmore", "é".repeat(300));
        fake.answer(fake.last_id(), Outcome::Failed(why));
        deliver(&mut asks, &mut fake, now);
        let shown = format!("refused: {}", "é".repeat(191));
        assert_eq!(
            vec![
                Step::Warn(format!("{NAME}: fake: open_tab_page failed: {shown}")),
                Step::FallBack(open(1)),
            ],
            steps(&mut asks)
        );
    }

    #[test]
    fn an_act_has_the_time_its_pane_provider_gives_it() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        fake.timeout = Some(Duration::from_millis(1500));
        asks.act(Some(&mut fake), open(1), now);
        assert_eq!(Some(now + Duration::from_millis(1500)), asks.deadline());
        asks.time_out(&mut fake, now + Duration::from_millis(1500));
        assert_eq!(
            Step::Warn(format!("{NAME}: fake: open_tab_page timed out after 1.5 s")),
            steps(&mut asks)[0]
        );
    }

    #[test]
    fn a_close_has_two_seconds() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), close(TerminalId::from("t1")), now);
        assert_eq!(Some(now + Duration::from_secs(2)), asks.deadline());
    }

    #[test]
    fn unsupported_falls_back_and_stops_further_asks_of_that_capability() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        asks.act(Some(&mut fake), open(2), now);
        fake.answer(fake.last_id(), Outcome::Unsupported);
        deliver(&mut asks, &mut fake, now);
        // the one waiting falls back too, unasked, and so does a later one
        asks.act(Some(&mut fake), open(3), now);
        assert_eq!(vec![open(1)], fake.asked());
        assert_eq!(
            vec![
                Step::FallBack(open(1)),
                Step::FallBack(open(2)),
                Step::FallBack(open(3)),
            ],
            steps(&mut asks)
        );
        // other capabilities are still asked
        asks.act(Some(&mut fake), close(TerminalId::from("t9")), now);
        assert_eq!(2, fake.asks.len());
    }

    #[test]
    fn a_terminal_opened_after_its_act_timed_out_is_closed() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        let id = fake.last_id();
        asks.time_out(&mut fake, now + Duration::from_secs(5));
        steps(&mut asks);

        fake.opened(id, "t1");
        deliver(&mut asks, &mut fake, now);
        assert_eq!(close(TerminalId::from("t1")), fake.asked()[1]);
        // nothing for the daemon to do: its window fell back already
        assert_eq!(Vec::<Step>::new(), steps(&mut asks));
    }

    #[test]
    fn a_missing_capability_or_no_pane_provider_falls_back_without_an_ask() {
        let (mut asks, now) = (Asks::default(), Instant::now());
        let mut fake = FakeProvider::with(&[Capability::Close]);
        asks.act(Some(&mut fake), open(1), now);
        asks.act(None, open(2), now);
        assert!(fake.asks.is_empty());
        assert_eq!(
            vec![Step::FallBack(open(1)), Step::FallBack(open(2))],
            steps(&mut asks)
        );
    }

    fn focus(t: &str) -> Ask {
        Ask::Focus {
            terminal: TerminalId::from(t),
        }
    }

    fn focusing() -> FakeProvider {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::Focus);
        f.caps.insert(Capability::FocusDirection);
        f
    }

    #[test]
    fn focus_acts_coalesce_to_one_running_and_the_newest_waiting() {
        let (mut asks, mut fake, now) = (Asks::default(), focusing(), Instant::now());
        asks.act(Some(&mut fake), focus("a"), now);
        asks.act(Some(&mut fake), focus("b"), now);
        let right = Ask::FocusDirection {
            dir: Direction::Right,
            count: 2,
        };
        asks.act(Some(&mut fake), right.clone(), now);
        asks.act(Some(&mut fake), focus("c"), now);
        // a layout act goes its own way meanwhile
        asks.act(Some(&mut fake), open(1), now);
        assert_eq!(vec![focus("a"), open(1)], fake.asked());
        assert_eq!(
            vec![Step::Replaced(focus("b")), Step::Replaced(right)],
            steps(&mut asks)
        );

        // the newest goes once the one running is answered
        fake.answer(fake.asks[0].0, Outcome::Done(Answer::Nothing));
        deliver(&mut asks, &mut fake, now);
        assert_eq!(vec![focus("a"), open(1), focus("c")], fake.asked());
        assert_eq!(
            vec![Step::Done(focus("a"), Answer::Nothing)],
            steps(&mut asks)
        );
        // or once it times out, after two seconds
        asks.act(Some(&mut fake), focus("d"), now);
        asks.time_out(&mut fake, now + Duration::from_millis(1999));
        assert_eq!(3, fake.asks.len());
        asks.time_out(&mut fake, now + Duration::from_secs(2));
        assert_eq!(focus("d"), fake.asked()[3]);
    }

    #[test]
    fn a_pane_provider_gone_falls_back_every_act_asked_or_waiting() {
        let (mut asks, mut fake, now) = (Asks::default(), focusing(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        asks.act(Some(&mut fake), open(2), now);
        asks.act(Some(&mut fake), focus("a"), now);
        asks.act(Some(&mut fake), focus("b"), now);
        asks.gone();
        assert_eq!(
            vec![
                Step::FallBack(open(1)),
                Step::FallBack(focus("a")),
                Step::FallBack(open(2)),
                Step::FallBack(focus("b")),
            ],
            steps(&mut asks)
        );
        assert_eq!(0, asks.waiting());
        assert_eq!(None, asks.deadline());
    }
}
