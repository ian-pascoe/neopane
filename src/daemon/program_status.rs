//! Program status (ADR 0021): the status records programs in terminal
//! buffers report with OSC 7501, kept per terminal buffer, and the records
//! each Neovim pane's terminal is to hold, from the terminal buffers it
//! shows the status of. `relay` writes them to a pane's terminal.

pub(crate) mod relay;
pub(crate) mod report;

use indexmap::IndexMap;

use crate::name::NAME;
use crate::nvim::windows::StatusEnd as End;
use report::{Change, Record};

/// Most status records a terminal buffer keeps: the spec's cap.
pub(crate) const BUFFER_CAP: usize = 256;
/// Most status records a Neovim pane's terminal is written, the most
/// recently updated: what any conforming terminal keeps (ADR 0021).
pub(crate) const PANE_CAP: usize = 64;

/// One status record of a terminal buffer.
#[derive(Debug, Clone)]
struct Stored {
    /// None for the root record.
    id: Option<String>,
    record: Record,
    /// When it was last updated, over every terminal buffer.
    seq: u64,
}

/// The status records of the terminal buffers, and the windows showing
/// them.
#[derive(Debug, Default)]
pub(crate) struct ProgramStatus {
    /// Each status buffer's records, least recently updated first.
    buffers: IndexMap<i64, Vec<Stored>>,
    /// The windows of each status buffer, as last told: buffer, window.
    windows: Vec<(i64, i64)>,
    /// The last update's number.
    seq: u64,
}

/// The status records a Neovim pane's terminal is to hold: each one's id,
/// None for the root record, and its report's body.
pub(crate) type Desired = IndexMap<Option<String>, String>;

impl ProgramStatus {
    /// Terminal buffer `buf` reported `body`. One the spec ignores or
    /// discards changes nothing; the buffer is a status buffer all the
    /// same.
    pub(crate) fn report(&mut self, buf: i64, body: &[u8]) {
        let records = self.buffers.entry(buf).or_default();
        let Some(r) = report::parse(body) else {
            return;
        };
        match r.change {
            Change::Clear => match &r.id {
                None => records.clear(),
                Some(top) => records.retain(|s| !s.id.as_deref().is_some_and(|id| under(id, top))),
            },
            Change::Set(record) => {
                records.retain(|s| s.id != r.id);
                if records.len() == BUFFER_CAP {
                    records.remove(0);
                }
                self.seq += 1;
                records.push(Stored {
                    id: r.id,
                    record,
                    seq: self.seq,
                });
            }
        }
    }

    /// Terminal buffer `buf`'s program status ends as `end` says: a prompt
    /// or its program's exit drops its working, blocked and idle records;
    /// the buffer going drops them all, and forgets it.
    pub(crate) fn end(&mut self, buf: i64, end: End) {
        match end {
            End::Prompt | End::Exit => {
                if let Some(records) = self.buffers.get_mut(&buf) {
                    records.retain(|s| !s.record.state.ends_with_program());
                }
            }
            End::Gone => {
                self.buffers.shift_remove(&buf);
                self.windows.retain(|&(b, _)| b != buf);
            }
        }
    }

    /// The windows of the status buffers, buffer and window, as told.
    pub(crate) fn set_windows(&mut self, windows: Vec<(i64, i64)>) {
        self.windows = windows;
    }

    /// The windows of the status buffers with records: buffer, window.
    pub(crate) fn shown(&self) -> impl Iterator<Item = (i64, i64)> + '_ {
        self.windows
            .iter()
            .copied()
            .filter(|(b, _)| self.buffers.get(b).is_some_and(|r| !r.is_empty()))
    }

    /// The status records a Neovim pane's terminal is to hold, showing the
    /// status of terminal buffers `bufs`: the root record, a copy of their
    /// most pressing root record; with one, its other records under their
    /// own ids, with several, every record of each under a segment of its
    /// own, `<name>-<buffer>`. At most `PANE_CAP`, the root record and the
    /// most recently updated: the root record first, the rest oldest first.
    pub(crate) fn desired(&self, bufs: &[i64]) -> Desired {
        let with: Vec<(i64, &Vec<Stored>)> = bufs
            .iter()
            .filter_map(|b| Some((*b, self.buffers.get(b).filter(|r| !r.is_empty())?)))
            .collect();
        let root = with
            .iter()
            .flat_map(|(_, rs)| rs.iter().filter(|s| s.id.is_none()))
            .max_by_key(|s| (s.record.state.rank(), s.seq));
        let mut rest: Vec<(String, String, u64)> = Vec::new();
        for (buf, records) in &with {
            for s in records.iter() {
                let id = if with.len() == 1 {
                    let Some(id) = &s.id else {
                        continue;
                    };
                    id.clone()
                } else {
                    let seg = format!("{NAME}-{buf}");
                    match &s.id {
                        None => seg,
                        Some(id) => format!("{seg}/{id}"),
                    }
                };
                let body = s.record.body(Some(&id));
                if report::is_id(&id) && report::fits(&body) {
                    rest.push((id, body, s.seq));
                }
            }
        }
        let mut out = Desired::new();
        if let Some(r) = root {
            out.insert(None, r.record.body(None));
        }
        rest.sort_by_key(|r| std::cmp::Reverse(r.2));
        rest.truncate(PANE_CAP - out.len());
        for (id, body, _) in rest.into_iter().rev() {
            out.insert(Some(id), body);
        }
        out
    }
}

/// Whether id `id` is `top` or beneath it.
pub(crate) fn under(id: &str, top: &str) -> bool {
    id.strip_prefix(top)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(d: &Desired) -> Vec<Option<&str>> {
        d.keys().map(|k| k.as_deref()).collect()
    }

    /// Buffer `buf`'s records, each as its report's body, oldest first.
    fn records(s: &ProgramStatus, buf: i64) -> Vec<String> {
        s.buffers[&buf]
            .iter()
            .map(|r| r.record.body(r.id.as_deref()))
            .collect()
    }

    #[test]
    fn each_report_replaces_its_record_the_latest_last() {
        let mut s = ProgramStatus::default();
        s.report(1, b"state=working:app=a");
        s.report(1, b"state=idle:id=x");
        s.report(1, b"state=done");
        assert_eq!(vec!["state=idle:id=x", "state=done"], records(&s, 1));
        // ignored: nothing changes, but the buffer is known
        s.report(2, b"state=nope");
        assert!(records(&s, 2).is_empty());
    }

    #[test]
    fn clear_removes_the_id_and_beneath_it_or_everything() {
        let mut s = ProgramStatus::default();
        for b in [
            "state=idle",
            "state=idle:id=a",
            "state=idle:id=a/b",
            "state=idle:id=ab",
            "state=idle:id=c",
        ] {
            s.report(1, b.as_bytes());
        }
        s.report(1, b"state=clear:id=a");
        assert_eq!(
            vec!["state=idle", "state=idle:id=ab", "state=idle:id=c"],
            records(&s, 1)
        );
        // a bad id clears nothing
        s.report(1, b"state=clear:id=a//b");
        assert_eq!(3, records(&s, 1).len());
        s.report(1, b"state=clear");
        assert!(records(&s, 1).is_empty());
    }

    #[test]
    fn a_buffer_keeps_256_records_dropping_the_least_recently_updated() {
        let mut s = ProgramStatus::default();
        for i in 0..256 {
            s.report(1, format!("state=idle:id=r{i}").as_bytes());
        }
        // r0 updated: r1 is the oldest now
        s.report(1, b"state=done:id=r0");
        s.report(1, b"state=idle:id=new");
        let r = records(&s, 1);
        assert_eq!(256, r.len());
        assert!(!r.contains(&"state=idle:id=r1".to_string()));
        assert!(r.contains(&"state=done:id=r0".to_string()));
        assert_eq!("state=idle:id=new", r[255]);
    }

    #[test]
    fn a_prompt_or_exit_keeps_done_and_error_and_going_forgets_the_buffer() {
        for end in [End::Prompt, End::Exit] {
            let mut s = ProgramStatus::default();
            for b in [
                "state=working",
                "state=blocked:id=b",
                "state=idle:id=i",
                "state=done:id=d",
                "state=error:id=e",
            ] {
                s.report(1, b.as_bytes());
            }
            s.end(1, end);
            assert_eq!(vec!["state=done:id=d", "state=error:id=e"], records(&s, 1));
        }
        let mut s = ProgramStatus::default();
        s.report(1, b"state=done");
        s.set_windows(vec![(1, 1000), (2, 1001)]);
        s.end(1, End::Gone);
        assert!(!s.buffers.contains_key(&1));
        assert_eq!(vec![(2, 1001)], s.windows);
    }

    #[test]
    fn only_buffers_with_records_are_shown() {
        let mut s = ProgramStatus::default();
        s.report(1, b"state=done");
        s.report(2, b"state=done");
        s.report(2, b"state=clear");
        s.set_windows(vec![(1, 1000), (2, 1001), (3, 1002)]);
        assert_eq!(vec![(1, 1000)], s.shown().collect::<Vec<_>>());
    }

    #[test]
    fn one_buffer_passes_its_records_through_under_their_ids() {
        let mut s = ProgramStatus::default();
        s.report(1, b"state=working:id=a");
        s.report(1, b"state=idle:app=x");
        let d = s.desired(&[1]);
        assert_eq!(vec![None, Some("a")], ids(&d));
        assert_eq!("state=idle:app=x", d[&None]);
        assert_eq!("state=working:id=a", d[&Some("a".into())]);
        // no root: none
        s.report(1, b"state=clear");
        s.report(1, b"state=done:id=a");
        assert_eq!(vec![Some("a")], ids(&s.desired(&[1])));
        assert!(s.desired(&[]).is_empty());
        assert!(s.desired(&[7]).is_empty());
    }

    #[test]
    fn the_root_is_the_most_pressing_root_ties_to_the_latest() {
        let mut s = ProgramStatus::default();
        let root = |s: &ProgramStatus, bufs: &[i64]| s.desired(bufs).get(&None).cloned();
        s.report(1, b"state=working:app=one");
        s.report(2, b"state=blocked:app=two");
        s.report(3, b"state=done:app=three");
        assert_eq!(Some("state=blocked:app=two".into()), root(&s, &[1, 2, 3]));
        assert_eq!(Some("state=working:app=one".into()), root(&s, &[3, 1]));
        // error over done and idle
        s.report(3, b"state=error:app=three");
        s.report(4, b"state=idle:app=four");
        assert_eq!(Some("state=error:app=three".into()), root(&s, &[3, 4]));
        // a tie: the latest
        s.report(5, b"state=working:app=five");
        assert_eq!(Some("state=working:app=five".into()), root(&s, &[1, 5]));
        s.report(1, b"state=working:app=one");
        assert_eq!(Some("state=working:app=one".into()), root(&s, &[1, 5]));
        // child records aren't ranked
        s.report(6, b"state=blocked:id=c");
        s.report(6, b"state=idle");
        assert_eq!(Some("state=idle".into()), root(&s, &[6]));
        s.report(7, b"state=blocked:id=c");
        assert_eq!(Some("state=idle".into()), root(&s, &[6, 7]));
    }

    #[test]
    fn several_buffers_also_put_each_ones_records_under_a_segment_of_its_own() {
        let mut s = ProgramStatus::default();
        s.report(1, b"state=working:app=one");
        s.report(1, b"state=blocked:id=a/b");
        s.report(2, b"state=done:id=c");
        let d = s.desired(&[1, 2]);
        let seg = |b: i64, rest: &str| format!("{NAME}-{b}{rest}");
        assert_eq!(
            vec![
                None,
                Some(seg(1, "").as_str()),
                Some(seg(1, "/a/b").as_str()),
                Some(seg(2, "/c").as_str())
            ],
            ids(&d)
        );
        assert_eq!("state=working:app=one", d[&None]);
        assert_eq!(
            format!("state=working:id={}:app=one", seg(1, "")),
            d[&Some(seg(1, ""))]
        );
        assert_eq!(
            format!("state=blocked:id={}", seg(1, "/a/b")),
            d[&Some(seg(1, "/a/b"))]
        );
        // one that no longer fits the id limits isn't relayed
        let deep = ["d"; 8].join("/");
        let long = format!("{0}/{0}/{0}/{1}", "l".repeat(32), "m".repeat(29));
        s.report(2, format!("state=idle:id={deep}").as_bytes());
        s.report(2, format!("state=idle:id={long}").as_bytes());
        let d = s.desired(&[1, 2]);
        assert_eq!(4, d.len());
        // alone, they pass
        assert_eq!(3, s.desired(&[2]).len());
    }

    #[test]
    fn a_pane_holds_64_records_the_root_and_the_most_recently_updated() {
        let mut s = ProgramStatus::default();
        s.report(1, b"state=blocked");
        for i in 0..70 {
            s.report(1, format!("state=idle:id=r{i}").as_bytes());
        }
        let d = s.desired(&[1]);
        assert_eq!(64, d.len());
        assert_eq!(None, d.keys().next().unwrap().as_deref());
        // r7 to r69, oldest first
        assert_eq!(Some("r7"), d.keys().nth(1).unwrap().as_deref());
        assert_eq!(Some("r69"), d.keys().last().unwrap().as_deref());
    }

    #[test]
    fn under_is_the_id_or_beneath_it() {
        assert!(under("a", "a"));
        assert!(under("a/b", "a"));
        assert!(!under("ab", "a"));
        assert!(!under("b/a", "a"));
    }
}
