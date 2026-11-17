//! One Program Status Protocol report (OSC 7501, spec rev 0.4), read from
//! the body a terminal buffer's program sent, checked against the spec's
//! rules and limits, and written back in a fixed order: never the bytes
//! the program sent.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};

/// The most bytes a whole sequence holds, `OSC` through `ST`.
pub(crate) const SEQUENCE_LIMIT: usize = 4096;
/// `ESC ] 7501 ;`.
const INTRO: &[u8] = b"\x1b]7501;";
/// `ESC \`, the `ST` a status record is written with.
const ST: &[u8] = b"\x1b\\";

const KEY_LIMIT: usize = 16;
const ID_LIMIT: usize = 128;
const ID_DEPTH: usize = 8;
const SEGMENT_LIMIT: usize = 32;
/// Encoded, then decoded.
const TITLE_LIMITS: (usize, usize) = (256, 192);
const MSG_LIMITS: (usize, usize) = (2732, 2048);

/// A status record's state; `clear` isn't one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum State {
    Idle,
    Working,
    Done,
    Blocked,
    Error,
}

impl State {
    fn name(self) -> &'static str {
        match self {
            State::Idle => "idle",
            State::Working => "working",
            State::Done => "done",
            State::Blocked => "blocked",
            State::Error => "error",
        }
    }

    /// How pressing it is, for a Neovim pane's root record: blocked, then
    /// working, error, done, idle.
    pub(crate) fn rank(self) -> u8 {
        match self {
            State::Blocked => 4,
            State::Working => 3,
            State::Error => 2,
            State::Done => 1,
            State::Idle => 0,
        }
    }

    /// Whether the program's exit or a new prompt drops it: all but done
    /// and error.
    pub(crate) fn ends_with_program(self) -> bool {
        !matches!(self, State::Done | State::Error)
    }
}

/// One status record: the keys of the last report for its id, those the
/// spec's rules keep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Record {
    pub state: State,
    pub kind: Option<&'static str>,
    pub progress: Option<u8>,
    pub app: Option<String>,
    /// Base64, as sent.
    pub title: Option<String>,
    pub msg: Option<String>,
    /// Keys the spec doesn't name, well formed, sorted.
    pub other: BTreeMap<String, String>,
}

/// What one report does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Change {
    /// Replaces its record.
    Set(Record),
    /// Removes its record and every record beneath it; with no id, every
    /// record.
    Clear,
}

/// One report, as the spec reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Report {
    /// None for the root record.
    pub id: Option<String>,
    pub change: Change,
}

/// Reads `body`, what follows `OSC 7501 ;`. None for a report the spec
/// ignores or discards whole.
pub(crate) fn parse(body: &[u8]) -> Option<Report> {
    // the shortest `ST` is BEL, one byte
    if INTRO.len() + body.len() + 1 > SEQUENCE_LIMIT {
        return None;
    }
    let mut pairs: BTreeMap<&str, &str> = BTreeMap::new();
    for pair in body.split(|&b| b == b':') {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            continue;
        };
        let key = pair[..eq].trim_ascii();
        let value = pair[eq + 1..].trim_ascii();
        if key.len() > KEY_LIMIT {
            return None;
        }
        if key.is_empty() || !key.iter().all(u8::is_ascii_lowercase) {
            continue;
        }
        if !value.iter().all(|&b| is_value_byte(b)) {
            // a bad id, even outside the value set, ignores the report: a
            // clear never falls back to clearing everything
            if key == b"id" {
                return None;
            }
            continue;
        }
        // all ASCII, checked above
        let key = std::str::from_utf8(key).unwrap();
        let value = std::str::from_utf8(value).unwrap();
        pairs.insert(key, value);
    }
    let state = match pairs.remove("state")? {
        "idle" => Some(State::Idle),
        "working" => Some(State::Working),
        "done" => Some(State::Done),
        "blocked" => Some(State::Blocked),
        "error" => Some(State::Error),
        "clear" => None,
        _ => return None,
    };
    let id = match pairs.remove("id") {
        Some(id) if is_id(id) => Some(id.to_string()),
        Some(_) => return None,
        None => None,
    };
    let title = text(pairs.remove("title"), TITLE_LIMITS)?;
    let msg = text(pairs.remove("msg"), MSG_LIMITS)?;
    let kind = pairs.remove("kind");
    let progress = pairs.remove("progress");
    let app = pairs.remove("app");
    let Some(state) = state else {
        return Some(Report {
            id,
            change: Change::Clear,
        });
    };
    let kind = kind.filter(|_| state == State::Blocked).and_then(|k| {
        ["permission", "question", "auth"]
            .into_iter()
            .find(|&n| n == k)
    });
    let progress = progress
        .filter(|_| matches!(state, State::Working | State::Blocked))
        .filter(|p| !p.is_empty() && p.len() <= 3 && p.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|p| p.parse::<u8>().ok())
        .filter(|&p| p <= 100);
    let app = app.filter(|a| is_segment(a)).map(str::to_string);
    let other = pairs
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    Some(Report {
        id,
        change: Change::Set(Record {
            state,
            kind,
            progress,
            app,
            title,
            msg,
            other,
        }),
    })
}

/// A byte of the spec's value set, `[A-Za-z0-9_.,+/=-]`.
fn is_value_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"_.,+/=-".contains(&b)
}

/// `[A-Za-z0-9_.+-]{1,32}`: an id's segment, or an `app`.
fn is_segment(s: &str) -> bool {
    (1..=SEGMENT_LIMIT).contains(&s.len())
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.+-".contains(&b))
}

/// Whether `id` is an id the spec takes: segments joined by `/`, 128 bytes
/// at most, 8 deep at most.
pub(crate) fn is_id(id: &str) -> bool {
    id.len() <= ID_LIMIT && id.split('/').count() <= ID_DEPTH && id.split('/').all(is_segment)
}

/// A `title` or `msg` value checked against `limits`, encoded then
/// decoded: Some(None) if absent, None to discard the report.
fn text(value: Option<&str>, limits: (usize, usize)) -> Option<Option<String>> {
    let Some(v) = value else {
        return Some(None);
    };
    if v.len() > limits.0 {
        return None;
    }
    const BASE64: GeneralPurpose = GeneralPurpose::new(
        &alphabet::STANDARD,
        GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
    );
    let decoded = BASE64.decode(v).ok()?;
    if decoded.len() > limits.1 {
        return None;
    }
    let text = std::str::from_utf8(&decoded).ok()?;
    if text
        .chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
    {
        return None;
    }
    Some(Some(v.to_string()))
}

impl Record {
    /// Its report's body, under `id` (None for the root record): its keys
    /// in a fixed order, those the spec doesn't name last, sorted.
    pub(crate) fn body(&self, id: Option<&str>) -> String {
        let mut out = format!("state={}", self.state.name());
        let mut add = |k: &str, v: &str| {
            out.push(':');
            out.push_str(k);
            out.push('=');
            out.push_str(v);
        };
        if let Some(id) = id {
            add("id", id);
        }
        if let Some(k) = self.kind {
            add("kind", k);
        }
        if let Some(p) = self.progress {
            add("progress", &p.to_string());
        }
        if let Some(a) = &self.app {
            add("app", a);
        }
        if let Some(t) = &self.title {
            add("title", t);
        }
        if let Some(m) = &self.msg {
            add("msg", m);
        }
        for (k, v) in &self.other {
            add(k, v);
        }
        out
    }
}

/// The report of `body`, a whole sequence, `ST` being `ESC \`.
pub(crate) fn sequence(body: &str) -> Vec<u8> {
    [INTRO, body.as_bytes(), ST].concat()
}

/// The body that clears record `id` and every record beneath it; with no
/// id, every record.
pub(crate) fn clear_body(id: Option<&str>) -> String {
    match id {
        Some(id) => format!("state=clear:id={id}"),
        None => "state=clear".to_string(),
    }
}

/// Whether `body`'s whole sequence fits the spec's limit.
pub(crate) fn fits(body: &str) -> bool {
    INTRO.len() + body.len() + ST.len() <= SEQUENCE_LIMIT
}

#[cfg(test)]
mod tests {
    use super::*;

    fn set(body: &str) -> (Option<String>, Record) {
        match parse(body.as_bytes()) {
            Some(Report {
                id,
                change: Change::Set(r),
            }) => (id, r),
            other => panic!("{body}: {other:?}"),
        }
    }

    /// The report `body` makes, written back.
    fn again(body: &str) -> String {
        let (id, r) = set(body);
        r.body(id.as_deref())
    }

    fn b64(s: &str) -> String {
        base64::engine::general_purpose::STANDARD.encode(s)
    }

    #[test]
    fn a_report_is_written_back_in_a_fixed_order() {
        let msg = b64("Apply?");
        let title = b64("Plan");
        assert_eq!(
            format!(
                "state=blocked:id=a/b:kind=permission:progress=40:app=terraform:title={title}:msg={msg}:x=1:zz=2"
            ),
            again(&format!(
                "zz=2:msg={msg}:x=1:app=terraform:title={title}:progress=40:kind=permission:id=a/b:state=blocked"
            ))
        );
        assert_eq!("state=idle", again("state=idle"));
    }

    #[test]
    fn whitespace_around_keys_and_values_goes_and_the_last_value_wins() {
        assert_eq!(
            "state=done:app=b",
            again(" state = working : app=a :state=done:app= b ")
        );
    }

    #[test]
    fn a_malformed_pair_is_skipped_the_rest_kept() {
        // no `=`, an empty key, a key outside [a-z], a value outside the set
        assert_eq!(
            "state=done:app=x",
            again("state=done:junk:=1:Bad=1:a1=2:note=semi;colon:app=x")
        );
    }

    #[test]
    fn state_is_required_and_known() {
        assert_eq!(None, parse(b"app=x"));
        assert_eq!(None, parse(b"state=paused"));
        assert_eq!(None, parse(b"state="));
        assert_eq!(None, parse(b""));
        for s in ["idle", "working", "done", "blocked", "error"] {
            assert_eq!(format!("state={s}"), again(&format!("state={s}")));
        }
    }

    #[test]
    fn clear_removes_by_id_or_all() {
        assert_eq!(
            Some(Report {
                id: None,
                change: Change::Clear
            }),
            parse(b"state=clear")
        );
        assert_eq!(
            Some(Report {
                id: Some("a/b".into()),
                change: Change::Clear
            }),
            parse(b"state=clear:id=a/b")
        );
    }

    #[test]
    fn a_bad_id_ignores_the_report() {
        let long = "a".repeat(33);
        let deep = ["a"; 9].join("/");
        let wide = vec!["a".repeat(32); 4].join("/");
        for id in ["", "a//b", "/a", "a/", "a b", "é", &long, &deep, &wide] {
            assert_eq!(
                None,
                parse(format!("state=idle:id={id}").as_bytes()),
                "{id}"
            );
        }
        // a bad id never clears everything
        assert_eq!(None, parse(b"state=clear:id=a//b"));
        assert_eq!(None, parse(b"state=clear:id=a b"));
        let deepest = ["a"; 8].join("/");
        let widest = format!("{0}/{0}/{0}/{1}", "a".repeat(32), "b".repeat(29));
        assert_eq!(128, widest.len());
        for id in ["a", "A-z_0.9+", &deepest, &widest] {
            assert_eq!(Some(id.to_string()), set(&format!("state=idle:id={id}")).0);
        }
    }

    #[test]
    fn kind_only_with_blocked_and_known() {
        assert_eq!("state=blocked:kind=auth", again("state=blocked:kind=auth"));
        assert_eq!(
            "state=blocked:kind=question",
            again("state=blocked:kind=question")
        );
        assert_eq!("state=blocked", again("state=blocked:kind=other"));
        assert_eq!("state=working", again("state=working:kind=auth"));
    }

    #[test]
    fn progress_is_0_to_100_with_working_or_blocked() {
        assert_eq!(
            "state=working:progress=0",
            again("state=working:progress=0")
        );
        assert_eq!(
            "state=blocked:progress=100",
            again("state=blocked:progress=100")
        );
        assert_eq!(
            "state=working:progress=7",
            again("state=working:progress=007")
        );
        for bad in ["101", "-1", "+5", "1.5", "", "1000", "x"] {
            assert_eq!(
                "state=working",
                again(&format!("state=working:progress={bad}")),
                "{bad}"
            );
        }
        assert_eq!("state=done", again("state=done:progress=50"));
    }

    #[test]
    fn app_outside_its_set_is_absent() {
        assert_eq!(
            "state=idle:app=claude-code",
            again("state=idle:app=claude-code")
        );
        assert_eq!("state=idle", again("state=idle:app=a/b"));
        assert_eq!(
            "state=idle",
            again(&format!("state=idle:app={}", "a".repeat(33)))
        );
        assert_eq!("state=idle", again("state=idle:app="));
    }

    #[test]
    fn title_and_msg_are_base64_utf8_without_controls() {
        // padding optional
        let padded = b64("hi");
        assert!(padded.ends_with('='));
        let bare = padded.trim_end_matches('=');
        assert_eq!(
            format!("state=idle:msg={bare}"),
            again(&format!("state=idle:msg={bare}"))
        );
        assert_eq!(
            format!("state=idle:title={padded}"),
            again(&format!("state=idle:title={padded}"))
        );
        assert_eq!("state=idle:msg=", again("state=idle:msg="));
        // not base64, not UTF-8, or a control character: discarded whole
        let latin1 = base64::engine::general_purpose::STANDARD.encode([0xe9]);
        for bad in [
            "====".to_string(),
            "ab,c".to_string(),
            "a".to_string(),
            latin1,
            b64("a\nb"),
            b64("tab\t"),
            b64("\u{7f}"),
            b64("\u{85}"),
        ] {
            assert_eq!(
                None,
                parse(format!("state=idle:msg={bad}").as_bytes()),
                "{bad}"
            );
            assert_eq!(
                None,
                parse(format!("state=idle:title={bad}").as_bytes()),
                "{bad}"
            );
        }
        // other text is fine
        assert!(parse(format!("state=idle:msg={}", b64("ünïcødé ✓")).as_bytes()).is_some());
    }

    #[test]
    fn title_and_msg_have_limits_encoded_then_decoded() {
        let title = b64(&"x".repeat(192));
        assert_eq!(256, title.len());
        assert!(parse(format!("state=idle:title={title}").as_bytes()).is_some());
        // 193 decoded: 260 encoded, over both
        let over = b64(&"x".repeat(193));
        assert_eq!(None, parse(format!("state=idle:title={over}").as_bytes()));
        // decoded within, encoded over (padding past the limit)
        let msg = b64(&"x".repeat(2048));
        assert_eq!(2732, msg.len());
        assert!(parse(format!("state=idle:msg={msg}").as_bytes()).is_some());
        // 2049 decoded fits 2732 encoded: over the decoded limit only
        let over = b64(&"x".repeat(2049));
        assert_eq!(2732, over.len());
        assert_eq!(None, parse(format!("state=idle:msg={over}").as_bytes()));
        let over = b64(&"x".repeat(2050));
        assert_eq!(None, parse(format!("state=idle:msg={over}").as_bytes()));
    }

    #[test]
    fn a_key_over_16_bytes_discards_the_report() {
        assert_eq!(
            None,
            parse(format!("state=idle:{}=1", "k".repeat(17)).as_bytes())
        );
        assert_eq!(
            "state=idle:kkkkkkkkkkkkkkkk=1",
            again(&format!("state=idle:{}=1", "k".repeat(16)))
        );
    }

    #[test]
    fn the_whole_sequence_holds_4096_bytes_at_most() {
        // ESC ] 7501 ; body BEL: 4096
        let pad = |n: usize| format!("state=idle:x={}", "a".repeat(n - "state=idle:x=".len()));
        assert!(parse(pad(4096 - 8).as_bytes()).is_some());
        assert_eq!(None, parse(pad(4096 - 7).as_bytes()));
        assert!(fits(&"a".repeat(4096 - 9)));
        assert!(!fits(&"a".repeat(4096 - 8)));
    }

    #[test]
    fn the_sequences_are_osc_7501_ended_by_esc_backslash() {
        assert_eq!(
            b"\x1b]7501;state=idle\x1b\\".to_vec(),
            sequence("state=idle")
        );
        assert_eq!("state=clear", clear_body(None));
        assert_eq!("state=clear:id=a", clear_body(Some("a")));
    }
}
