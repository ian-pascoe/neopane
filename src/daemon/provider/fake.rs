//! A pane provider for tests (#92 decision 31): it can do what it is told to,
//! records every ask, and answers or tells only what a test hands it.

use std::collections::VecDeque;
use std::time::Duration;

use super::{
    Answer, Ask, AskId, Capability, CapabilitySet, Outcome, PaneProvider, ProviderEvent, TerminalId,
};

#[derive(Default)]
pub(crate) struct FakeProvider {
    pub caps: CapabilitySet,
    /// Every ask, oldest first.
    pub asks: Vec<(AskId, Ask)>,
    /// The size its terminals start at before they're laid out, if any.
    pub unlaid: Option<[u32; 2]>,
    /// Its timeout for every capability, if not the default.
    pub timeout: Option<std::time::Duration>,
    /// Its attach timeout, if not the default.
    pub attach_timeout: Option<Duration>,
    /// Its context variables.
    pub context: &'static [&'static str],
    events: VecDeque<ProviderEvent>,
}

impl FakeProvider {
    /// One that can do `caps`.
    pub(crate) fn with(caps: &[Capability]) -> FakeProvider {
        FakeProvider {
            caps: CapabilitySet::of(caps),
            ..FakeProvider::default()
        }
    }

    /// One that can do every open and `close`.
    pub(crate) fn opening() -> FakeProvider {
        FakeProvider::with(&[
            Capability::OpenBeside,
            Capability::OpenFloat,
            Capability::OpenTabPage,
            Capability::Close,
        ])
    }

    /// One that can do every open and `close`, and tells its live layouts
    /// unasked.
    pub(crate) fn telling() -> FakeProvider {
        let mut f = FakeProvider::opening();
        f.caps.insert(Capability::ReportLayout);
        f.caps.insert(Capability::Events);
        f
    }

    /// Answers ask `id` with `outcome`, at the next `next`.
    pub(crate) fn answer(&mut self, id: AskId, outcome: Outcome) {
        self.push(ProviderEvent::Answered { id, outcome });
    }

    /// Answers ask `id` with the terminal `t`.
    pub(crate) fn opened(&mut self, id: AskId, t: &str) {
        self.answer(id, Outcome::Done(Answer::Terminal(TerminalId::from(t))));
    }

    /// Tells `event` at the next `next`.
    pub(crate) fn push(&mut self, event: ProviderEvent) {
        self.events.push_back(event);
    }

    /// The asks, without their ids.
    pub(crate) fn asked(&self) -> Vec<Ask> {
        self.asks.iter().map(|(_, a)| a.clone()).collect()
    }

    /// The id of the last ask.
    pub(crate) fn last_id(&self) -> AskId {
        self.asks.last().expect("an ask").0
    }
}

impl PaneProvider for FakeProvider {
    fn name(&self) -> &str {
        "fake"
    }

    fn capabilities(&self) -> CapabilitySet {
        self.caps
    }

    fn timeout(&self, cap: Capability) -> std::time::Duration {
        self.timeout.unwrap_or(cap.default_timeout())
    }

    fn ask(&mut self, id: AskId, ask: Ask) {
        self.asks.push((id, ask));
    }

    fn on_ready(&mut self, _token: usize) {}

    fn next(&mut self) -> Option<ProviderEvent> {
        self.events.pop_front()
    }

    fn unlaid_size(&self) -> Option<[u32; 2]> {
        self.unlaid
    }

    fn context_vars(&self) -> &[&str] {
        self.context
    }

    fn attach_timeout(&self) -> Duration {
        self.attach_timeout
            .unwrap_or_else(super::default_attach_timeout)
    }

    fn as_fake(&mut self) -> Option<&mut FakeProvider> {
        Some(self)
    }

    fn as_fake_ref(&self) -> Option<&FakeProvider> {
        Some(self)
    }
}
