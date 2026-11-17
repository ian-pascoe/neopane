//! What a Neovim pane's terminal holds of the program status neopane
//! writes it: the reports that bring it to the records it is to hold,
//! only what changed, and the reports that clear it again, for the undo
//! bytes.

use super::report::{clear_body, sequence};
use super::{Desired, under};

/// The status records written to one terminal.
#[derive(Debug, Default)]
pub(crate) struct Relayed {
    /// What it holds: each record's id, None for the root record, and its
    /// report's body.
    written: Desired,
    /// A root record was written to it once: its whole record set is
    /// neopane's to clear.
    root_written: bool,
    /// Reports written may have been dropped: it is written in full next.
    unknown: bool,
}

impl Relayed {
    /// The reports that bring the terminal from what it holds to
    /// `desired`, which it holds after: a record changed or new, each
    /// record gone cleared, where all of a top segment's records go by
    /// one clear of it. A root record gone, or reports perhaps dropped,
    /// clear everything first.
    pub(crate) fn update(&mut self, desired: &Desired) -> Vec<u8> {
        let mut out = Vec::new();
        let all_again = (self.unknown && (self.root_written || !self.written.is_empty()))
            || (self.written.contains_key(&None) && !desired.contains_key(&None));
        self.unknown = false;
        if all_again {
            out.extend(sequence(&clear_body(None)));
            self.written.clear();
        }
        // the ids to clear, none beneath another
        let mut clears: Vec<&str> = Vec::new();
        for id in self.written.keys().flatten() {
            if desired.contains_key(&Some(id.clone())) {
                continue;
            }
            let top = id.split('/').next().unwrap();
            let kept_under_top = desired.keys().flatten().any(|d| under(d, top));
            let clear = if kept_under_top { id.as_str() } else { top };
            if !clears.iter().any(|c| under(clear, c)) {
                clears.retain(|c| !under(c, clear));
                clears.push(clear);
            }
        }
        for c in &clears {
            out.extend(sequence(&clear_body(Some(c))));
        }
        for (id, body) in desired {
            let cleared = id
                .as_deref()
                .is_some_and(|id| clears.iter().any(|c| under(id, c)));
            if cleared || self.written.get(id) != Some(body) {
                out.extend(sequence(body));
            }
        }
        self.root_written |= desired.contains_key(&None);
        self.written = desired.clone();
        out
    }

    /// What the terminal holds.
    #[cfg(test)]
    pub(crate) fn held(&self) -> &Desired {
        &self.written
    }

    /// Reports written may have been dropped, as frames are: the next
    /// `update` clears everything and writes it all again.
    pub(crate) fn forget(&mut self) {
        self.unknown = true;
    }

    /// The reports that clear what was written, for the undo bytes, in
    /// `room` bytes at most: once a root record was written, one clear of
    /// everything; else one clear of each top segment it holds, or, if
    /// those don't fit, one clear of everything.
    pub(crate) fn clears(&self, room: usize) -> Vec<u8> {
        let everything = sequence(&clear_body(None));
        if self.root_written {
            return everything;
        }
        let mut tops: Vec<&str> = Vec::new();
        for id in self.written.keys().flatten() {
            let top = id.split('/').next().unwrap();
            if !tops.contains(&top) {
                tops.push(top);
            }
        }
        let out: Vec<u8> = tops
            .iter()
            .flat_map(|t| sequence(&clear_body(Some(t))))
            .collect();
        if out.len() > room { everything } else { out }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn desired(records: &[(Option<&str>, &str)]) -> Desired {
        records
            .iter()
            .map(|(id, body)| (id.map(str::to_string), body.to_string()))
            .collect()
    }

    /// `bytes`, each report's body on a line of its own.
    fn bodies(bytes: &[u8]) -> Vec<String> {
        let s = String::from_utf8(bytes.to_vec()).unwrap();
        s.split("\x1b\\")
            .filter(|b| !b.is_empty())
            .map(|b| b.strip_prefix("\x1b]7501;").unwrap().to_string())
            .collect()
    }

    #[test]
    fn only_what_changed_is_written() {
        let mut r = Relayed::default();
        let d = desired(&[(None, "state=working"), (Some("a"), "state=idle:id=a")]);
        assert_eq!(
            vec!["state=working", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
        let d = desired(&[(None, "state=blocked"), (Some("a"), "state=idle:id=a")]);
        assert_eq!(vec!["state=blocked"], bodies(&r.update(&d)));
        // a child gone is cleared
        let d = desired(&[(None, "state=blocked")]);
        assert_eq!(vec!["state=clear:id=a"], bodies(&r.update(&d)));
    }

    #[test]
    fn a_root_gone_clears_everything_then_writes_the_rest() {
        let mut r = Relayed::default();
        r.update(&desired(&[
            (None, "state=working"),
            (Some("a"), "state=idle:id=a"),
        ]));
        let d = desired(&[(Some("a"), "state=idle:id=a")]);
        assert_eq!(
            vec!["state=clear", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
    }

    #[test]
    fn a_top_segment_all_gone_is_cleared_once() {
        let mut r = Relayed::default();
        let all = desired(&[
            (None, "state=working"),
            (Some("s-1"), "state=working:id=s-1"),
            (Some("s-1/a"), "state=idle:id=s-1/a"),
            (Some("s-1/a/b"), "state=idle:id=s-1/a/b"),
            (Some("s-2/c"), "state=idle:id=s-2/c"),
        ]);
        r.update(&all);
        // s-1 goes: one clear; s-2's own segment, never written, clears it too
        let d = desired(&[(None, "state=working")]);
        assert_eq!(
            vec!["state=clear:id=s-1", "state=clear:id=s-2"],
            bodies(&r.update(&d))
        );
        // records kept beneath a top segment: each gone one cleared, and
        // what its clear took written again
        r.update(&all);
        let d = desired(&[
            (None, "state=working"),
            (Some("s-1"), "state=working:id=s-1"),
            (Some("s-1/a/b"), "state=idle:id=s-1/a/b"),
            (Some("s-2/c"), "state=idle:id=s-2/c"),
        ]);
        assert_eq!(
            vec!["state=clear:id=s-1/a", "state=idle:id=s-1/a/b"],
            bodies(&r.update(&d))
        );
    }

    #[test]
    fn rekeyed_records_clear_the_old_segment_and_write_the_new_ids() {
        let mut r = Relayed::default();
        // two buffers shown, then one
        r.update(&desired(&[
            (None, "state=working"),
            (Some("s-1"), "state=working:id=s-1"),
            (Some("s-1/a"), "state=idle:id=s-1/a"),
            (Some("s-2"), "state=done:id=s-2"),
        ]));
        let d = desired(&[(None, "state=working"), (Some("a"), "state=idle:id=a")]);
        assert_eq!(
            vec![
                "state=clear:id=s-1",
                "state=clear:id=s-2",
                "state=idle:id=a"
            ],
            bodies(&r.update(&d))
        );
    }

    #[test]
    fn reports_perhaps_dropped_are_written_again_in_full() {
        let mut r = Relayed::default();
        // nothing written: nothing to clear
        r.forget();
        assert!(r.update(&Desired::new()).is_empty());
        let d = desired(&[(Some("a"), "state=idle:id=a")]);
        r.update(&d);
        r.forget();
        assert_eq!(
            vec!["state=clear", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
    }

    #[test]
    fn the_clears_undo_what_was_written() {
        let mut r = Relayed::default();
        assert!(r.clears(4096).is_empty());
        r.update(&desired(&[
            (Some("a"), "state=idle:id=a"),
            (Some("a/b"), "state=idle:id=a/b"),
            (Some("c/d"), "state=idle:id=c/d"),
        ]));
        assert_eq!(
            vec!["state=clear:id=a", "state=clear:id=c"],
            bodies(&r.clears(4096))
        );
        // too long for the room: everything
        assert_eq!(vec!["state=clear"], bodies(&r.clears(30)));
        // a root written once: everything, from then on
        r.update(&desired(&[(None, "state=idle")]));
        r.update(&desired(&[(Some("a"), "state=idle:id=a")]));
        assert_eq!(vec!["state=clear"], bodies(&r.clears(4096)));
    }
}
