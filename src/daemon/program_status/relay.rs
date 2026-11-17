//! What a Neovim pane's terminal holds of the program status neopane
//! writes it: the reports that bring it to the records it is to hold,
//! only what changed, and the reports that clear it again, for the undo
//! bytes.

use super::report::{clear_body, sequence};
use super::{Wanted, under};

/// The status records written to one terminal.
#[derive(Debug, Default)]
pub(crate) struct Written {
    /// What it holds: each record's id, None for the root record, and its
    /// report's body.
    held: Wanted,
    /// A root record was written to it once: its whole record set is
    /// neopane's to clear.
    root_written: bool,
    /// The top segment of every id written to it, first written first:
    /// what it may hold still, had a clear been dropped.
    tops: Vec<String>,
    /// Reports written may have been dropped: it is written in full next.
    unknown: bool,
}

impl Written {
    /// The reports that bring the terminal from what it holds to
    /// `wanted`, which it holds after: a record changed or new, each
    /// record gone cleared, where all of a top segment's records go by
    /// one clear of it. A root record gone clears everything first;
    /// reports perhaps dropped clear what the undo bytes would first.
    pub(crate) fn update(&mut self, wanted: &Wanted) -> Vec<u8> {
        let mut out = Vec::new();
        if self.unknown {
            out = self.clears(usize::MAX);
            self.held.clear();
        } else if self.held.contains_key(&None) && !wanted.contains_key(&None) {
            out = sequence(&clear_body(None));
            self.held.clear();
        }
        self.unknown = false;
        // the ids to clear, none beneath another
        let mut clears: Vec<&str> = Vec::new();
        for id in self.held.keys().flatten() {
            if wanted.contains_key(&Some(id.clone())) {
                continue;
            }
            let top = top_of(id);
            let kept_under_top = wanted.keys().flatten().any(|d| under(d, top));
            let clear = if kept_under_top { id.as_str() } else { top };
            if !clears.iter().any(|c| under(clear, c)) {
                clears.retain(|c| !under(c, clear));
                clears.push(clear);
            }
        }
        for c in &clears {
            out.extend(sequence(&clear_body(Some(c))));
        }
        for (id, body) in wanted {
            let cleared = id
                .as_deref()
                .is_some_and(|id| clears.iter().any(|c| under(id, c)));
            if cleared || self.held.get(id) != Some(body) {
                out.extend(sequence(body));
            }
        }
        self.root_written |= wanted.contains_key(&None);
        for id in wanted.keys().flatten() {
            let top = top_of(id);
            if !self.tops.iter().any(|t| t == top) {
                self.tops.push(top.to_string());
            }
        }
        self.held = wanted.clone();
        out
    }

    /// What the terminal holds.
    #[cfg(test)]
    pub(crate) fn held(&self) -> &Wanted {
        &self.held
    }

    /// Reports written may have been dropped, as frames are: the next
    /// `update` clears what the undo bytes would and writes it all again.
    pub(crate) fn forget(&mut self) {
        self.unknown = true;
    }

    /// The reports that clear what was written, for the undo bytes, in
    /// `room` bytes at most: once a root record was written, one clear of
    /// everything; else one clear of each top segment ever written, or, if
    /// those don't fit, one clear of everything. They change only as a
    /// report is written.
    pub(crate) fn clears(&self, room: usize) -> Vec<u8> {
        let everything = sequence(&clear_body(None));
        if self.root_written {
            return everything;
        }
        let out: Vec<u8> = self
            .tops
            .iter()
            .flat_map(|t| sequence(&clear_body(Some(t))))
            .collect();
        if out.len() > room { everything } else { out }
    }
}

/// The top segment of id `id`.
fn top_of(id: &str) -> &str {
    id.split('/').next().unwrap_or(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wanted(records: &[(Option<&str>, &str)]) -> Wanted {
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
        let mut r = Written::default();
        let d = wanted(&[(None, "state=working"), (Some("a"), "state=idle:id=a")]);
        assert_eq!(
            vec!["state=working", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
        let d = wanted(&[(None, "state=blocked"), (Some("a"), "state=idle:id=a")]);
        assert_eq!(vec!["state=blocked"], bodies(&r.update(&d)));
        // a child gone is cleared
        let d = wanted(&[(None, "state=blocked")]);
        assert_eq!(vec!["state=clear:id=a"], bodies(&r.update(&d)));
    }

    #[test]
    fn a_root_gone_clears_everything_then_writes_the_rest() {
        let mut r = Written::default();
        r.update(&wanted(&[
            (None, "state=working"),
            (Some("a"), "state=idle:id=a"),
        ]));
        let d = wanted(&[(Some("a"), "state=idle:id=a")]);
        assert_eq!(
            vec!["state=clear", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
    }

    #[test]
    fn a_top_segment_all_gone_is_cleared_once() {
        let mut r = Written::default();
        let all = wanted(&[
            (None, "state=working"),
            (Some("s-1"), "state=working:id=s-1"),
            (Some("s-1/a"), "state=idle:id=s-1/a"),
            (Some("s-1/a/b"), "state=idle:id=s-1/a/b"),
            (Some("s-2/c"), "state=idle:id=s-2/c"),
        ]);
        r.update(&all);
        // s-1 goes: one clear; s-2's own segment, never written, clears it too
        let d = wanted(&[(None, "state=working")]);
        assert_eq!(
            vec!["state=clear:id=s-1", "state=clear:id=s-2"],
            bodies(&r.update(&d))
        );
        // records kept beneath a top segment: each gone one cleared, and
        // what its clear took written again
        r.update(&all);
        let d = wanted(&[
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
        let mut r = Written::default();
        // two buffers shown, then one
        r.update(&wanted(&[
            (None, "state=working"),
            (Some("s-1"), "state=working:id=s-1"),
            (Some("s-1/a"), "state=idle:id=s-1/a"),
            (Some("s-2"), "state=done:id=s-2"),
        ]));
        let d = wanted(&[(None, "state=working"), (Some("a"), "state=idle:id=a")]);
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
        let mut r = Written::default();
        // nothing written: nothing to clear
        r.forget();
        assert!(r.update(&Wanted::new()).is_empty());
        // no root written: what the terminal holds of its own stays, and
        // every top segment written is cleared, held now or not
        r.update(&wanted(&[(Some("b/c"), "state=idle:id=b/c")]));
        let d = wanted(&[(Some("a"), "state=idle:id=a")]);
        r.update(&d);
        r.forget();
        assert_eq!(
            vec!["state=clear:id=b", "state=clear:id=a", "state=idle:id=a"],
            bodies(&r.update(&d))
        );
        assert!(r.update(&d).is_empty());
        // a root written once: everything
        let d = wanted(&[(None, "state=idle")]);
        r.update(&d);
        r.forget();
        assert_eq!(vec!["state=clear", "state=idle"], bodies(&r.update(&d)));
    }

    #[test]
    fn the_clears_undo_what_was_written() {
        let mut r = Written::default();
        assert!(r.clears(4096).is_empty());
        r.update(&wanted(&[
            (Some("a"), "state=idle:id=a"),
            (Some("a/b"), "state=idle:id=a/b"),
            (Some("c/d"), "state=idle:id=c/d"),
        ]));
        assert_eq!(
            vec!["state=clear:id=a", "state=clear:id=c"],
            bodies(&r.clears(4096))
        );
        // a top segment cleared stays in them: its clear may have been
        // dropped with frames
        r.update(&wanted(&[(Some("a"), "state=idle:id=a")]));
        assert_eq!(
            vec!["state=clear:id=a", "state=clear:id=c"],
            bodies(&r.clears(4096))
        );
        // too long for the room: everything
        assert_eq!(vec!["state=clear"], bodies(&r.clears(30)));
        // a root written once: everything, from then on
        r.update(&wanted(&[(None, "state=idle")]));
        r.update(&wanted(&[(Some("a"), "state=idle:id=a")]));
        assert_eq!(vec!["state=clear"], bodies(&r.clears(4096)));
    }
}
