//! Acts: the asks the daemon makes of its pane provider, and what becomes of
//! them (#92 decision 4, ADR 0006).
//!
//! An act whose capability the pane provider lacks, or was found not to
//! support, falls back at once; any other is asked. Layout acts go one at a
//! time, each asked only once the one before is answered or timed out. An
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

/// What the daemon has to do about an act.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Step {
    /// It worked: what was asked, and the answer.
    Done(Ask, Answer),
    /// It can't be done: do what the daemon does without it.
    FallBack(Ask),
    /// Show this in Neovim as a warning.
    Warn(String),
}

/// An act asked of the pane provider and not answered yet.
#[derive(Debug)]
struct Sent {
    id: AskId,
    ask: Ask,
    due: Instant,
}

#[derive(Debug, Default)]
pub(crate) struct Asks {
    last_id: u64,
    /// Asked, unanswered: at most one layout act, and any focus acts.
    sent: Vec<Sent>,
    /// Layout acts waiting their turn, oldest first.
    queue: VecDeque<(AskId, Ask)>,
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
        } else {
            // the focus queue is #109's
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

    fn send(&mut self, p: &mut dyn PaneProvider, id: AskId, ask: Ask, now: Instant) {
        let ms = ask.capability().timeout_ms();
        let due = now + Duration::from_millis(u64::from(ms));
        p.ask(id, ask.clone());
        self.sent.push(Sent { id, ask, due });
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
                self.failed(p.name(), cap, &format!("failed: {why}"));
                self.steps.push_back(Step::FallBack(ask));
            }
        }
        self.next_layout_act(p, now);
    }

    /// Fails the acts whose time is up at `now`.
    pub(crate) fn time_out(&mut self, p: &mut dyn PaneProvider, now: Instant) {
        let mut i = 0;
        while i < self.sent.len() {
            if self.sent[i].due > now {
                i += 1;
                continue;
            }
            let Sent { ask, .. } = self.sent.remove(i);
            let cap = ask.capability();
            let secs = cap.timeout_ms() / 1000;
            self.failed(p.name(), cap, &format!("timed out after {secs} s"));
            self.steps.push_back(Step::FallBack(ask));
        }
        self.next_layout_act(p, now);
    }

    /// The pane provider failed as a whole: every act asked or waiting falls
    /// back.
    pub(crate) fn gone(&mut self) {
        for s in std::mem::take(&mut self.sent) {
            self.steps.push_back(Step::FallBack(s.ask));
        }
        for (_, ask) in std::mem::take(&mut self.queue) {
            self.steps.push_back(Step::FallBack(ask));
        }
    }

    /// Logs a failed act; its capability's first failure warns in Neovim
    /// too, as #93 decision 20 words it.
    fn failed(&mut self, provider: &str, cap: Capability, what: &str) {
        let line = format!("{provider}: {cap} {what}");
        log_warn!("{NAME} daemon: {line}");
        if !self.warned.has(cap) {
            self.warned.insert(cap);
            self.steps.push_back(Step::Warn(format!("{NAME}: {line}")));
        }
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
        self.sent.len() + self.queue.len()
    }
}

/// The ask that closes terminal `t`.
pub(crate) fn close(t: TerminalId) -> Ask {
    Ask::Close { terminal: t }
}

#[cfg(test)]
mod tests {
    use super::super::provider::ProviderEvent;
    use super::super::provider::fake::FakeProvider;
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

    #[test]
    fn a_pane_provider_gone_falls_back_every_act_asked_or_waiting() {
        let (mut asks, mut fake, now) = (Asks::default(), FakeProvider::opening(), Instant::now());
        asks.act(Some(&mut fake), open(1), now);
        asks.act(Some(&mut fake), open(2), now);
        asks.gone();
        assert_eq!(
            vec![Step::FallBack(open(1)), Step::FallBack(open(2))],
            steps(&mut asks)
        );
        assert_eq!(0, asks.waiting());
        assert_eq!(None, asks.deadline());
    }
}
