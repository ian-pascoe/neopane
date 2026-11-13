//! What `list` and `status` print (#94 decisions 29–31): the table, one
//! daemon's lines, and the JSON whose shape `README.md` documents. Fields
//! are only ever added to the JSON, never removed, renamed, or changed in
//! type or meaning; a missing one means unknown.

use serde_json::{Map, Value, json};

use crate::attach::{ATTACH_PROTOCOL_VERSION, Reply};
use crate::daemon::control::{Listed, shown_dir, shown_files};

/// The table `list` prints: a header, then a row per daemon, columns two
/// spaces apart.
pub fn list_table(listed: &[Listed], home: Option<&[u8]>) -> String {
    let mut rows = vec![[
        "NAME".to_string(),
        "PANES".into(),
        "UNSAVED".into(),
        "PROVIDER".into(),
        "DIRECTORY".into(),
    ]];
    for l in listed {
        let row = match &l.answer {
            Ok(r) => match &r.status {
                Some(s) => {
                    let panes = match s.kept {
                        Some(true) => format!("{} kept", s.panes),
                        _ => s.panes.to_string(),
                    };
                    let dir = s.dir.as_deref().map_or("-".into(), |d| shown_dir(d, home));
                    [
                        l.name.clone(),
                        panes,
                        s.unsaved.len().to_string(),
                        s.provider.clone(),
                        dir,
                    ]
                }
                None => dashed(&l.name, format!("({})", build_line(r))),
            },
            Err(e) => dashed(&l.name, format!("({e})")),
        };
        rows.push(row);
    }
    let mut widths = [0; 4];
    for row in &rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.chars().count());
        }
    }
    let mut out = String::new();
    for row in rows {
        for (w, cell) in widths.iter().zip(&row) {
            out += &format!("{cell:<w$}  ");
        }
        out += &row[4];
        out.push('\n');
    }
    out
}

/// A row with only a name and what goes in the directory's place.
fn dashed(name: &str, instead: String) -> [String; 5] {
    [name.into(), "-".into(), "-".into(), "-".into(), instead]
}

/// `<build>, attach protocol <v>`.
fn build_line(r: &Reply) -> String {
    format!("{}, attach protocol {}", r.build, r.v)
}

/// What `list --json` prints.
pub fn list_json(listed: &[Listed]) -> String {
    let daemons: Vec<Value> = listed.iter().map(daemon_json).collect();
    format!("{}\n", json!({ "daemons": daemons }))
}

/// One daemon as `list --json` has it, and `status --json` prints.
pub fn daemon_json(l: &Listed) -> Value {
    let mut o = Map::new();
    o.insert("name".into(), l.name.clone().into());
    o.insert("address".into(), l.address.to_string().into());
    let r = match &l.answer {
        Ok(r) => r,
        Err(e) => {
            o.insert("error".into(), e.to_string().into());
            return Value::Object(o);
        }
    };
    o.insert("pid".into(), r.pid.into());
    o.insert("build".into(), r.build.clone().into());
    o.insert("v".into(), r.v.into());
    o.insert(
        "same_version".into(),
        (r.v == ATTACH_PROTOCOL_VERSION).into(),
    );
    let Some(s) = &r.status else {
        return Value::Object(o);
    };
    let text = |b: &[u8]| Value::from(String::from_utf8_lossy(b).into_owned());
    if let Some(d) = &s.dir {
        o.insert("dir".into(), text(d));
    }
    o.insert("panes".into(), s.panes.into());
    let unsaved: Vec<Value> = s.unsaved.iter().map(|f| text(f)).collect();
    o.insert("unsaved".into(), unsaved.into());
    o.insert("provider".into(), s.provider.clone().into());
    if let Some(c) = &s.capabilities {
        o.insert("capabilities".into(), c.clone().into());
    }
    if let Some(u) = &s.unsupported {
        o.insert("unsupported".into(), u.clone().into());
    }
    if let Some(k) = s.kept {
        o.insert("kept".into(), k.into());
    }
    if let Some(l) = &s.log {
        o.insert("log".into(), text(l));
    }
    if let Some(t) = &s.terminals {
        let ptys: Vec<Value> = t.iter().map(|p| text(p)).collect();
        o.insert("terminals".into(), ptys.into());
    }
    Value::Object(o)
}

/// The lines `status` prints for a daemon that answered, `r`, at
/// `address`.
pub fn status_text(r: &Reply, address: &str, home: Option<&[u8]>) -> String {
    let mut out = format!("{}\n", r.name);
    let mut line = |label: &str, value: &str| {
        out += &format!("  {label:<9}  {value}\n");
    };
    let Some(s) = &r.status else {
        line("pid", &r.pid.to_string());
        line("build", &build_line(r));
        out += "  (another version: only its name, pid and build are known)\n";
        return out;
    };
    let dir = s.dir.as_deref();
    line("directory", &dir.map_or("-".into(), |d| shown_dir(d, home)));
    line("panes", &s.panes.to_string());
    let unsaved = match s.unsaved.is_empty() {
        true => "none".to_string(),
        false => shown_files(&s.unsaved, dir, home),
    };
    line("unsaved", &unsaved);
    let stays = match s.kept {
        Some(true) => "until killed, started with --detach",
        _ => "while something is unsaved",
    };
    line("stays", stays);
    line("provider", &s.provider);
    let mut provider_lines = Vec::new();
    if let Some(caps) = s.capabilities.as_ref().filter(|c| !c.is_empty()) {
        provider_lines.extend(wrapped("has: ", caps));
    }
    if let Some(u) = s.unsupported.as_ref().filter(|u| !u.is_empty()) {
        provider_lines.extend(wrapped("found unsupported: ", u));
    }
    for l in provider_lines {
        line("", &l);
    }
    let address = match &s.address[..] {
        [] => address.to_string(),
        a => String::from_utf8_lossy(a).into_owned(),
    };
    line("address", &address);
    line("pid", &r.pid.to_string());
    line("build", &build_line(r));
    if let Some(l) = &s.log {
        line("log", &String::from_utf8_lossy(l));
    }
    out
}

/// The status lines' value column.
const VALUE_AT: usize = 13;

/// `lead` and `items`, joined by `, `, wrapped to 80 columns from the
/// value column, later lines under the first item.
fn wrapped(lead: &str, items: &[String]) -> Vec<String> {
    let indent = " ".repeat(lead.len());
    let room = 80 - VALUE_AT;
    let mut lines = vec![lead.to_string()];
    for (i, item) in items.iter().enumerate() {
        let piece = match i + 1 == items.len() {
            true => item.clone(),
            false => format!("{item},"),
        };
        let last = lines.last_mut().unwrap();
        let fresh = last.len() == lead.len();
        if !fresh && last.len() + 1 + piece.len() > room {
            lines.push(format!("{indent}{piece}"));
        } else {
            if !fresh {
                last.push(' ');
            }
            last.push_str(&piece);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::attach::Status;
    use crate::daemon::control::Failure;
    use crate::daemon::dir::Address;
    use std::time::Duration;

    fn reply(name: &str, status: Option<Status>) -> Reply {
        Reply {
            ok: true,
            message: None,
            v: ATTACH_PROTOCOL_VERSION,
            build: "tool 0.1.0 (a1b2c3d, 2026-11-09)".into(),
            name: name.into(),
            pid: 4242,
            status,
            waited: None,
        }
    }

    fn work_status() -> Status {
        Status {
            address: b"/tmp/np/work/sock".to_vec(),
            panes: 3,
            unsaved: vec![
                b"/h/src/x/README.md".to_vec(),
                b"/h/src/x/src/main.rs".to_vec(),
            ],
            provider: "rex".into(),
            dir: Some(b"/h/src/x".to_vec()),
            kept: Some(false),
            capabilities: Some(
                [
                    "close",
                    "find",
                    "focus",
                    "focus_direction",
                    "place_beside",
                    "resize",
                    "report_layout",
                    "events",
                    "open_beside",
                    "open_float",
                    "open_tab_page",
                ]
                .map(String::from)
                .to_vec(),
            ),
            unsupported: Some(vec!["open_float".into()]),
            log: Some(b"/tmp/np/work/log".to_vec()),
            terminals: Some(vec![]),
        }
    }

    fn listed(name: &str, answer: Result<Reply, Failure>) -> Listed {
        Listed {
            name: name.into(),
            address: Address::from_arg(format!("/tmp/np/{name}/sock").into()),
            answer,
        }
    }

    fn several() -> Vec<Listed> {
        let notes = Status {
            panes: 0,
            unsaved: vec![b"/h/notes/a".to_vec(), Vec::new()],
            provider: "none".into(),
            dir: Some(b"/h/notes".to_vec()),
            kept: Some(true),
            ..work_status()
        };
        let mut old = reply("old", None);
        old.v = ATTACH_PROTOCOL_VERSION - 1;
        old.build = "tool 0.0.9".into();
        vec![
            listed("notes", Ok(reply("notes", Some(notes)))),
            listed("old", Ok(old)),
            listed("stuck", Err(Failure::NoAnswer(Duration::from_secs(2)))),
            listed("work", Ok(reply("work", Some(work_status())))),
        ]
    }

    #[test]
    fn the_list_is_a_table_two_spaces_apart() {
        assert_eq!(
            list_table(&several(), Some(b"/h")),
            "NAME   PANES   UNSAVED  PROVIDER  DIRECTORY\n\
             notes  0 kept  2        none      ~/notes\n\
             old    -       -        -         (tool 0.0.9, attach protocol 0)\n\
             stuck  -       -        -         (didn't answer in 2 s)\n\
             work   3       2        rex       ~/src/x\n"
        );
        assert_eq!(
            list_table(&[], None),
            "NAME  PANES  UNSAVED  PROVIDER  DIRECTORY\n"
        );
    }

    #[test]
    fn the_list_as_json_has_the_documented_shape() {
        let got: Value = serde_json::from_str(&list_json(&several())).unwrap();
        let want = json!({"daemons": [
            {"name": "notes", "address": "/tmp/np/notes/sock", "pid": 4242,
             "build": "tool 0.1.0 (a1b2c3d, 2026-11-09)", "v": ATTACH_PROTOCOL_VERSION,
             "same_version": true, "dir": "/h/notes", "panes": 0,
             "unsaved": ["/h/notes/a", ""], "provider": "none",
             "capabilities": work_status().capabilities, "unsupported": ["open_float"],
             "kept": true, "log": "/tmp/np/work/log", "terminals": []},
            {"name": "old", "address": "/tmp/np/old/sock", "pid": 4242,
             "build": "tool 0.0.9", "v": ATTACH_PROTOCOL_VERSION - 1,
             "same_version": false},
            {"name": "stuck", "address": "/tmp/np/stuck/sock",
             "error": "didn't answer in 2 s"},
            {"name": "work", "address": "/tmp/np/work/sock", "pid": 4242,
             "build": "tool 0.1.0 (a1b2c3d, 2026-11-09)", "v": ATTACH_PROTOCOL_VERSION,
             "same_version": true, "dir": "/h/src/x", "panes": 3,
             "unsaved": ["/h/src/x/README.md", "/h/src/x/src/main.rs"], "provider": "rex",
             "capabilities": work_status().capabilities, "unsupported": ["open_float"],
             "kept": false, "log": "/tmp/np/work/log", "terminals": []},
        ]});
        assert_eq!(want, got);
        assert_eq!("{\"daemons\":[]}\n", list_json(&[]));
    }

    #[test]
    fn status_shows_one_daemon_wrapping_its_capabilities() {
        let r = reply("work", Some(work_status()));
        assert_eq!(
            status_text(&r, "", Some(b"/h")),
            "work\n\
             \x20 directory  ~/src/x\n\
             \x20 panes      3\n\
             \x20 unsaved    README.md, src/main.rs\n\
             \x20 stays      while something is unsaved\n\
             \x20 provider   rex\n\
             \x20            has: close, find, focus, focus_direction, place_beside, resize,\n\
             \x20                 report_layout, events, open_beside, open_float, open_tab_page\n\
             \x20            found unsupported: open_float\n\
             \x20 address    /tmp/np/work/sock\n\
             \x20 pid        4242\n\
             \x20 build      tool 0.1.0 (a1b2c3d, 2026-11-09), attach protocol 1\n\
             \x20 log        /tmp/np/work/log\n"
        );
    }

    #[test]
    fn status_of_another_version_shows_only_the_frozen_fields() {
        let mut r = reply("old", None);
        r.v = 0;
        r.build = "tool 0.0.9".into();
        assert_eq!(
            status_text(&r, "/tmp/np/old/sock", None),
            "old\n\
             \x20 pid        4242\n\
             \x20 build      tool 0.0.9, attach protocol 0\n\
             \x20 (another version: only its name, pid and build are known)\n"
        );
    }
}
