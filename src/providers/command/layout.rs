//! Reads what `report_layout` prints into a `LiveLayout` (#93 decision 18):
//! `{"areas":[{"id":…,"terminals":[{"terminal":…,"rect":[x,y,w,h],
//! "floating"?:bool,"inset"?:[l,t,r,b]}]}],"focus"?:id}`. Rects are fractions
//! of their area; unknown fields are ignored. Anything else malformed fails
//! the whole report.

use serde_json::{Map, Value};

use crate::daemon::live_layout::{LayoutArea, LiveLayout, PlacedTerminal};
use crate::daemon::provider::TerminalId;

/// The most a report may print (#93 decision 19).
pub(crate) const MAX_REPORT: usize = 1 << 20;

/// The live layout `out`, a report's stdout, says, or why it says none.
pub(crate) fn parse_report_layout(out: &[u8]) -> Result<LiveLayout, String> {
    let v: Value =
        serde_json::from_slice(out).map_err(|e| format!("its report isn't JSON: {e}"))?;
    let Value::Object(top) = v else {
        return Err("its report isn't a JSON object".into());
    };
    let mut areas = Vec::new();
    for (i, a) in array(&top, "areas", "")?.iter().enumerate() {
        let at = format!("areas[{i}]");
        let a = object(a, &at)?;
        let id = string(a, "id", &at)?.ok_or_else(|| missing(&at, "id"))?;
        let mut terminals = Vec::new();
        for (j, t) in array(a, "terminals", &at)?.iter().enumerate() {
            terminals.push(terminal(t, &format!("{at}.terminals[{j}]"))?);
        }
        areas.push(LayoutArea {
            id: id.to_string(),
            terminals,
        });
    }
    let current = string(&top, "focus", "")?.map(TerminalId::from);
    Ok(LiveLayout { areas, current })
}

/// One terminal of an area, at `at`.
fn terminal(t: &Value, at: &str) -> Result<PlacedTerminal, String> {
    let t = object(t, at)?;
    let id = string(t, "terminal", at)?.ok_or_else(|| missing(at, "terminal"))?;
    if id.is_empty() {
        return Err(format!("{at}.terminal: empty"));
    }
    let rect = numbers(t.get("rect"))
        .ok_or_else(|| format!("{at}.rect: {}", what(t.get("rect"), "not 4 numbers")))?;
    let floating = match t.get("floating") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err(format!("{at}.floating: not a boolean")),
    };
    let inset = match t.get("inset") {
        None | Some(Value::Null) => [0; 4],
        Some(v) => cells(v).ok_or_else(|| format!("{at}.inset: not 4 cell counts"))?,
    };
    Ok(PlacedTerminal {
        terminal: TerminalId::from(id),
        rect,
        floating,
        inset,
    })
}

fn object<'a>(v: &'a Value, at: &str) -> Result<&'a Map<String, Value>, String> {
    v.as_object()
        .ok_or_else(|| format!("{at}: not a JSON object"))
}

/// Field `key` of `o`, at `at`, an array.
fn array<'a>(o: &'a Map<String, Value>, key: &str, at: &str) -> Result<&'a Vec<Value>, String> {
    let path = path(at, key);
    match o.get(key) {
        Some(Value::Array(a)) => Ok(a),
        None => Err(format!("{path}: missing")),
        Some(_) => Err(format!("{path}: not an array")),
    }
}

/// Field `key` of `o`, at `at`, a string if there; null is as missing.
fn string<'a>(o: &'a Map<String, Value>, key: &str, at: &str) -> Result<Option<&'a str>, String> {
    match o.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(format!("{}: not a string", path(at, key))),
    }
}

fn missing(at: &str, key: &str) -> String {
    format!("{}: missing", path(at, key))
}

fn what(v: Option<&Value>, wrong: &str) -> String {
    if v.is_none() { "missing" } else { wrong }.to_string()
}

/// `key` at `at`: the top level's has no prefix.
fn path(at: &str, key: &str) -> String {
    if at.is_empty() {
        key.to_string()
    } else {
        format!("{at}.{key}")
    }
}

/// Four numbers.
fn numbers(v: Option<&Value>) -> Option<[f64; 4]> {
    let a = v?.as_array()?;
    let [x, y, w, h] = a.as_slice() else {
        return None;
    };
    Some([x.as_f64()?, y.as_f64()?, w.as_f64()?, h.as_f64()?])
}

/// Four cell counts.
fn cells(v: &Value) -> Option<[u16; 4]> {
    let a = v.as_array()?;
    let [l, t, r, b] = a.as_slice() else {
        return None;
    };
    let cell = |v: &Value| v.as_u64().and_then(|n| u16::try_from(n).ok());
    Some([cell(l)?, cell(t)?, cell(r)?, cell(b)?])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon::live_layout::{LayoutArea, PlacedTerminal};
    use crate::daemon::provider::TerminalId;

    fn placed(t: &str, rect: [f64; 4], floating: bool, inset: [u16; 4]) -> PlacedTerminal {
        PlacedTerminal {
            terminal: TerminalId::from(t),
            rect,
            floating,
            inset,
        }
    }

    #[test]
    fn a_report_of_two_areas_reads_each_terminals_place_and_the_focus() {
        let out = br#"{
            "version": 7,
            "areas": [
                {"id": "w1", "name": "ignored", "terminals": [
                    {"terminal": "%1", "rect": [0, 0, 0.5, 1]},
                    {"terminal": "%2", "rect": [0.5, 0, 0.5, 1], "floating": false, "pid": 3}
                ]},
                {"id": "w2", "terminals": [
                    {"terminal": "%3", "rect": [0.1, 0.2, 0.3, 0.4], "floating": true,
                     "inset": [1, 1, 2, 1]}
                ]}
            ],
            "focus": "%2"
        }
        "#;
        let want = LiveLayout {
            areas: vec![
                LayoutArea {
                    id: "w1".into(),
                    terminals: vec![
                        placed("%1", [0.0, 0.0, 0.5, 1.0], false, [0; 4]),
                        placed("%2", [0.5, 0.0, 0.5, 1.0], false, [0; 4]),
                    ],
                },
                LayoutArea {
                    id: "w2".into(),
                    terminals: vec![placed("%3", [0.1, 0.2, 0.3, 0.4], true, [1, 1, 2, 1])],
                },
            ],
            current: Some(TerminalId::from("%2")),
        };
        assert_eq!(Ok(want), parse_report_layout(out));
    }

    #[test]
    fn no_areas_and_no_focus_is_an_empty_layout() {
        assert_eq!(
            Ok(LiveLayout::default()),
            parse_report_layout(br#"{"areas": [], "focus": null}"#)
        );
    }

    #[test]
    fn a_malformed_report_says_why() {
        for (out, why) in [
            (&b"not json"[..], "isn't JSON"),
            (b"[]", "isn't a JSON object"),
            (b"{}", "areas: missing"),
            (br#"{"areas": {}}"#, "areas: not an array"),
            (br#"{"areas": [{"terminals": []}]}"#, "areas[0].id: missing"),
            (br#"{"areas": [{"id": 3, "terminals": []}]}"#, "areas[0].id: not a string"),
            (br#"{"areas": [{"id": "w"}]}"#, "areas[0].terminals: missing"),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"rect": [0, 0, 1, 1]}]}]}"#,
                "areas[0].terminals[0].terminal: missing",
            ),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"terminal": "t", "rect": [0, 0, 1]}]}]}"#,
                "areas[0].terminals[0].rect: not 4 numbers",
            ),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"terminal": "t", "rect": [0, 0, 1, "1"]}]}]}"#,
                "areas[0].terminals[0].rect: not 4 numbers",
            ),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"terminal": "t", "rect": [0, 0, 1, 1], "floating": 1}]}]}"#,
                "areas[0].terminals[0].floating: not a boolean",
            ),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"terminal": "t", "rect": [0, 0, 1, 1], "inset": [0, 0, -1, 0]}]}]}"#,
                "areas[0].terminals[0].inset: not 4 cell counts",
            ),
            (
                br#"{"areas": [{"id": "w", "terminals": [{"terminal": "", "rect": [0, 0, 1, 1]}]}]}"#,
                "areas[0].terminals[0].terminal: empty",
            ),
            (br#"{"areas": [], "focus": 2}"#, "focus: not a string"),
        ] {
            let got = parse_report_layout(out);
            let Err(e) = &got else {
                panic!("{}: {got:?}", String::from_utf8_lossy(out));
            };
            assert!(e.contains(why), "{}: {e}", String::from_utf8_lossy(out));
        }
    }
}
