//! What `provider check` prints (#93 decision 30, #94 decision 33): the
//! report's sections as text, a label and its value two spaces apart, the
//! lines under a section indented by two; or one JSON object, whose shape
//! `docs/config.md` documents. Fields are only ever added to the JSON,
//! never removed, renamed, or changed in type or meaning.

use std::time::Duration;

use serde_json::{Map, Value, json};

use super::find::{Answered, Find, exit_of};
use super::rex::RexReport;
use super::{Check, ConfigState, How, Verdict};
use crate::config::check::{Problem, Severity};
use crate::config::{Builtin, Capability, Choice, Defaulted, Provider};
use crate::name::NAME;
use crate::providers::rex::find::{BLOCK_VAR, Verdict as RexVerdict};
use crate::providers::tmux::check::TmuxReport;
use crate::util::seconds;

/// The widest line shown without `--full`, in columns.
const MAX_COLUMNS: usize = 100;
/// The most of the config's printed lines shown without `--full`.
const MAX_PRINTED: usize = 20;

/// What a table is in the choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Whole,
    Terminal,
    Layout,
}

impl Role {
    fn name(self) -> &'static str {
        match self {
            Role::Whole => "whole",
            Role::Terminal => "terminal",
            Role::Layout => "layout",
        }
    }

    /// The capabilities it gives.
    fn uses(self, c: Capability) -> bool {
        match self {
            Role::Whole => true,
            Role::Terminal => c.is_terminal(),
            Role::Layout => c.is_layout(),
        }
    }

    /// Whether its table lists capability `cap`: one it gives, a one-step
    /// one on a terminal side (absent from a pair), or one ignored.
    fn lists(self, cap: Capability, ignored: bool) -> bool {
        self.uses(cap) || (self == Role::Terminal && cap.is_one_step()) || ignored
    }

    fn gives_layout(self) -> bool {
        self != Role::Terminal
    }

    fn gives_terminals(self) -> bool {
        self != Role::Layout
    }
}

/// What a pair is: both sides, or one alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PairKind {
    Both,
    TerminalAlone,
    LayoutAlone,
}

impl PairKind {
    fn of(pair: &crate::config::Pair) -> PairKind {
        match (&pair.terminal, &pair.layout) {
            (Some(_), Some(_)) => PairKind::Both,
            (Some(_), None) => PairKind::TerminalAlone,
            _ => PairKind::LayoutAlone,
        }
    }

    /// As the JSON's `chose.kind` says it.
    fn name(self) -> &'static str {
        match self {
            PairKind::Both => "pair",
            PairKind::TerminalAlone => "terminal",
            PairKind::LayoutAlone => "layout",
        }
    }

    /// As the report's `chose` line says it.
    fn text(self) -> &'static str {
        match self {
            PairKind::Both => "a pair",
            PairKind::TerminalAlone => "a terminal side alone",
            PairKind::LayoutAlone => "a layout side alone",
        }
    }
}

/// How a table keeps a live layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LiveLayout {
    Events,
    ReportLayout,
    No,
}

impl LiveLayout {
    fn of(p: &Provider) -> LiveLayout {
        match (
            p.capability(Capability::ReportLayout).is_some(),
            p.capability(Capability::Events).is_some(),
        ) {
            (true, true) => LiveLayout::Events,
            (true, false) => LiveLayout::ReportLayout,
            (false, _) => LiveLayout::No,
        }
    }

    /// As the JSON says it.
    fn name(self) -> &'static str {
        match self {
            LiveLayout::Events => "events",
            LiveLayout::ReportLayout => "report_layout",
            LiveLayout::No => "no",
        }
    }

    /// As the report's `live layout` line says it.
    fn text(self) -> &'static str {
        match self {
            LiveLayout::Events => "yes: report_layout and events",
            LiveLayout::ReportLayout => "yes: report_layout (read after each act)",
            LiveLayout::No => "no: each pane is its own layout area",
        }
    }
}

/// The tables in use, each with its role.
fn tables(choice: &Choice) -> Vec<(&Provider, Role)> {
    match choice {
        Choice::Whole(p) => vec![(p, Role::Whole)],
        Choice::Pair(pair) => pair
            .terminal
            .iter()
            .map(|t| (t, Role::Terminal))
            .chain(pair.layout.iter().map(|l| (l, Role::Layout)))
            .collect(),
        Choice::None | Choice::Builtin(_) => Vec::new(),
    }
}

/// The text report; without `full`, argv lines cut at 100 columns and at
/// most 20 of the config's printed lines shown.
pub(super) fn text(c: &Check, full: bool) -> String {
    let mut out = String::new();
    let mut put = |line: String| {
        out.push_str(&line);
        out.push('\n');
    };
    put(match &c.config {
        ConfigState::File(path) => format!("config  {}", path.display()),
        ConfigState::NoFile => format!("config  no file: {NAME}.default chooses"),
        ConfigState::Skipped => "config  skipped: hint none".into(),
    });
    let a = &c.attach;
    let tty = if a.tty.is_empty() {
        "no terminal".into()
    } else {
        String::from_utf8_lossy(&a.tty).into_owned()
    };
    let hint = match (&c.config, &a.provider) {
        (ConfigState::Skipped, _) => "hint none".to_string(),
        (_, Some(h)) => format!("hint {h}"),
        (_, None) => "no hint".to_string(),
    };
    put(format!(
        "attach  tty {tty}, pid {}, cwd {}, daemon {}, {hint}",
        a.pid,
        String::from_utf8_lossy(&a.cwd),
        a.daemon
    ));
    if let Some(ran) = &c.ran {
        let n = ran.printed.len();
        let mut line = format!(
            "ran  {}, {n} line{} printed",
            took(ran.took),
            if n == 1 { "" } else { "s" }
        );
        if ran.printed_cut {
            line += &format!(
                ", more dropped past {} KiB",
                crate::config::MAX_PRINTED >> 10
            );
        }
        put(line);
        let shown = if full { n } else { n.min(MAX_PRINTED) };
        for l in &ran.printed[..shown] {
            put(format!("  {l}"));
        }
        if shown < n {
            put(format!("  ({} more: --full)", n - shown));
        }
        if let Some(e) = &ran.error {
            let mut lines = e.lines();
            put(format!("  error: {}", lines.next().unwrap_or_default()));
            for l in lines {
                put(format!("    {l}"));
            }
        }
    }
    if let Some((choice, how)) = &c.chose {
        put(chose_line(choice, *how));
        if let Some(d) = &c.default {
            for line in default_lines(d) {
                put(line);
            }
        }
        for (p, role) in tables(choice) {
            for line in table_lines(p, role, c, full) {
                put(line);
            }
        }
        if let Some(one_step) = one_step_line(choice) {
            put(format!("one-step  {one_step}"));
        }
    }
    if let Some(rex) = &c.rex {
        for line in rex_lines(rex) {
            put(line);
        }
    }
    if let Some(tmux) = &c.tmux {
        for line in tmux_lines(tmux) {
            put(line);
        }
    }
    put(format!("find  {}", find_text(&c.find)));
    let errors: Vec<&Problem> = c.problems.0.iter().filter(|p| is_error(p)).collect();
    let warnings: Vec<&Problem> = c.problems.0.iter().filter(|p| !is_error(p)).collect();
    for (label, list) in [("errors", errors), ("warnings", warnings)] {
        if list.is_empty() {
            continue;
        }
        put(label.into());
        for p in list {
            put(format!("  {}", p.text));
        }
    }
    put(format!("result  {}", result_text(&c.result)));
    out
}

/// How long something took: `4.2 ms`, or `1.25 s` from a second.
fn took(d: Duration) -> String {
    if d < Duration::from_secs(1) {
        format!("{:.1} ms", d.as_secs_f64() * 1000.0)
    } else {
        format!("{} s", seconds(d))
    }
}

/// `line` cut to [`MAX_COLUMNS`], its last one `…`.
fn cut(line: &str) -> String {
    if line.chars().count() <= MAX_COLUMNS {
        return line.to_string();
    }
    let mut s: String = line.chars().take(MAX_COLUMNS - 1).collect();
    s.push('…');
    s
}

fn is_error(p: &Problem) -> bool {
    p.severity == Severity::Error
}

fn chose_line(choice: &Choice, how: How) -> String {
    let by = match how {
        How::Provider => "by provider()".to_string(),
        How::Default => format!("by {NAME}.default"),
        How::Hint => return "chose  none: the hint is none".into(),
    };
    let what = match choice {
        Choice::None => return format!("chose  none, {by}"),
        Choice::Builtin(b) => format!("\"{}\", built in", b.name()),
        Choice::Whole(p) => format!("a table, {}, at {}", p.name, p.path),
        Choice::Pair(pair) => {
            format!(
                "{}, {}, at provider()",
                PairKind::of(pair).text(),
                pair.name
            )
        }
    };
    format!("chose  {what}, {by}")
}

/// What does without capability `cap` when it is absent; `report_layout`
/// whether the table reads the layout.
fn fallback(cap: Capability, report_layout: bool) -> &'static str {
    match cap {
        Capability::Open => "new windows are drawn in the pane they were opened from",
        Capability::Close => "a terminal it opened closes only as its attach ends",
        Capability::Find => {
            "the choice is accepted, with no id; reach rests on context variables alone"
        }
        Capability::PlaceBeside => "a terminal it opened stays where it opened",
        Capability::Resize => "a terminal keeps the size it opened at",
        Capability::Focus => "Neovim's current window doesn't focus its terminal",
        Capability::FocusDirection => "a move stops at the pane's edge",
        Capability::ReportLayout => "each pane is its own layout area",
        Capability::Events if report_layout => "the layout is read after each act",
        Capability::Events => "no live layout",
        Capability::OpenBeside => "open, then place_beside",
        Capability::OpenFloat => "float groups are drawn in panes",
        Capability::OpenTabPage => "tab pages stay whole in the pane they opened in",
    }
}

/// Why capability `cap` of table `p` is ignored, if a warning says so.
fn ignored<'a>(cap: Capability, p: &Provider, c: &'a Check) -> Option<&'a str> {
    let at = format!("{}.{}: ", p.path, cap.name());
    c.problems
        .0
        .iter()
        .find_map(|problem| problem.text.strip_prefix(&at))
}

/// A capability's state in table `p`: present, with its command; ignored,
/// with why; or absent, with what falls back.
enum State<'a> {
    Present(&'a crate::config::check::Command),
    Ignored(&'a str),
    Absent(&'static str),
}

fn state<'a>(cap: Capability, p: &'a Provider, c: &'a Check) -> State<'a> {
    if let Some(command) = p.capability(cap) {
        State::Present(command)
    } else if let Some(why) = ignored(cap, p, c) {
        State::Ignored(why)
    } else {
        let report_layout = p.capability(Capability::ReportLayout).is_some();
        State::Absent(fallback(cap, report_layout))
    }
}

/// The capabilities table `p`, used as `role`, lists, in #93's order.
fn listed<'a>(p: &'a Provider, role: Role, c: &'a Check) -> impl Iterator<Item = Capability> + 'a {
    Capability::ALL
        .into_iter()
        .filter(move |&cap| role.lists(cap, ignored(cap, p, c).is_some()))
}

fn table_lines(p: &Provider, role: Role, c: &Check, full: bool) -> Vec<String> {
    let mut lines = vec![format!(
        "{}  {} at {}, protocol {}",
        role.name(),
        p.name,
        p.path,
        p.protocol
    )];
    for cap in listed(p, role, c) {
        let name = cap.name();
        lines.push(match state(cap, p, c) {
            State::Present(command) => {
                let time = match command.timeout {
                    Some(t) => format!("{} s", seconds(t)),
                    None => "runs until it ends".into(),
                };
                let line = format!("  {name}  {}  ({time})", argv_line(&command.argv));
                // an argv line is cut, nothing else
                if full { line } else { cut(&line) }
            }
            State::Ignored(why) => format!("  {name}  ignored: {why}"),
            State::Absent(fallback) => format!("  {name}  absent: {fallback}"),
        });
    }
    if role.gives_layout() {
        lines.push(format!("  live layout  {}", LiveLayout::of(p).text()));
    }
    if role.gives_terminals() {
        let default = if p.attach_timeout == crate::config::check::ATTACH_TIMEOUT {
            " (default)"
        } else {
            ""
        };
        lines.push(format!(
            "  attach timeout  {} s{default}",
            seconds(p.attach_timeout)
        ));
    }
    if role.gives_layout() {
        let reach = if p.context_vars.is_empty() {
            "every terminal (no context variables)".to_string()
        } else {
            p.context_vars
                .iter()
                .map(|v| match value_of(c, v) {
                    Some(value) => format!("{v}={value}"),
                    None => format!("{v} unset"),
                })
                .collect::<Vec<_>>()
                .join(", ")
        };
        lines.push(format!("  reach  {reach}"));
        let vars = if p.find_vars.is_empty() {
            "none".to_string()
        } else {
            p.find_vars.join(", ")
        };
        lines.push(format!("  find vars  {vars}"));
    }
    lines
}

/// This attach's value of variable `name`.
fn value_of(c: &Check, name: &str) -> Option<String> {
    crate::attach::env_value(&c.attach.env, name).map(|v| String::from_utf8_lossy(v).into_owned())
}

/// An argv on one line, each word as a shell would read it back.
fn argv_line(argv: &[Vec<u8>]) -> String {
    argv.iter()
        .map(|w| shell_word(&String::from_utf8_lossy(w)))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_word(w: &str) -> String {
    let plain = !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-{}".contains(c));
    if plain {
        w.to_string()
    } else {
        format!("'{}'", w.replace('\'', r"'\''"))
    }
}

/// The one-step capabilities the choice has, if it can have any.
fn one_step(choice: &Choice) -> Option<Vec<&'static str>> {
    match choice {
        Choice::Whole(p) => Some(
            Capability::ALL
                .into_iter()
                .filter(|c| c.is_one_step() && p.capability(*c).is_some())
                .map(Capability::name)
                .collect(),
        ),
        Choice::Builtin(Builtin::Rex) => Some(vec!["open_beside", "open_float", "open_tab_page"]),
        Choice::Pair(_) => Some(Vec::new()),
        Choice::None => None,
    }
}

fn one_step_line(choice: &Choice) -> Option<String> {
    let caps = one_step(choice)?;
    Some(match choice {
        Choice::Pair(_) => "none: a pair opens, then places".into(),
        _ if caps.is_empty() => "none".into(),
        _ => caps.join(", "),
    })
}

fn rex_lines(r: &RexReport) -> Vec<String> {
    let mut lines = vec![format!("rex  {}", r.server)];
    if let Some(v) = &r.version {
        lines.push(format!("  version  {v}"));
    }
    if let Some(rev) = r.api_revision {
        lines.push(format!("  api revision  {rev}"));
    }
    if let (Some(needs), Some(has)) = (r.missing, r.api_revision) {
        lines.push(format!(
            "  missing  every capability (needs Rex API revision {needs}, this server has {has})"
        ));
    }
    if let Some(e) = &r.error {
        lines.push(format!("  error  {e}"));
    }
    lines
}

/// The tmux section's lines.
fn tmux_lines(t: &TmuxReport) -> Vec<String> {
    let mut lines = vec![match &t.path {
        Some(path) => format!("tmux  {path}"),
        None => "tmux  not on PATH".into(),
    }];
    if let Some(v) = &t.version {
        lines.push(format!("  version  {v}"));
    }
    lines.push(match &t.socket {
        Some(socket) => format!("  socket  {socket}"),
        None => "  socket  none: TMUX isn't set".into(),
    });
    lines.push(match &t.server {
        Ok(()) => "  server  answers".into(),
        Err(why) => format!("  server  doesn't answer: {why}"),
    });
    let this = t.version.as_deref().unwrap_or("unknown");
    for m in &t.missing {
        let (major, minor) = m.needs;
        lines.push(match m.capability {
            Some(cap) => format!(
                "  {}  exits 69: needs tmux {major}.{minor}, this is {this}",
                cap.name()
            ),
            None => {
                format!("  missing  every capability (needs tmux {major}.{minor}, this is {this})")
            }
        });
    }
    lines
}

/// The tmux section as JSON.
fn tmux_json(t: &TmuxReport) -> Value {
    let missing: Vec<Value> = t
        .missing
        .iter()
        .map(|m| {
            let (major, minor) = m.needs;
            json!({
                "capability": m.capability.map(Capability::name),
                "needs": format!("{major}.{minor}"),
            })
        })
        .collect();
    json!({
        "path": t.path,
        "version": t.version,
        "socket": t.socket,
        "answers": t.server.is_ok(),
        "error": t.server.as_ref().err(),
        "missing": missing,
    })
}

fn find_text(f: &Find) -> String {
    let after =
        |t: &Duration, title: &str| format!("{}, title {title} written, then restored", took(*t));
    match f {
        Find::NotRun(why) => format!("not run: {why}"),
        Find::Absent => format!("absent: {}", fallback(Capability::Find, false)),
        Find::Ran {
            title,
            took,
            answer,
        } => match answer {
            Answered::Id(id) => format!("accepted: id {id} (exit 0, {})", after(took, title)),
            Answered::NoId => format!("accepted: no id (exit 0, {})", after(took, title)),
            Answered::Unsupported => format!(
                "accepted: no id, unsupported (exit 69, or it can't start; {})",
                after(took, title)
            ),
            Answered::Failed(why) => format!("refused: {why} ({})", after(took, title)),
            Answered::TimedOut(t) => format!(
                "refused: no answer within {} s (title {title} written, then restored)",
                seconds(*t)
            ),
        },
        Find::Rex {
            block,
            verdict,
            theirs,
            ..
        } => match (verdict, theirs) {
            (RexVerdict::Proven, _) => format!("proven: {block} is this terminal's"),
            (RexVerdict::Disproved(why), Some(tty)) => format!("disproved: {why} ({tty})"),
            (RexVerdict::Disproved(why), None) => format!("disproved: {why}"),
            (RexVerdict::Unprovable(why), _) => format!("unprovable: {why}; trusted {BLOCK_VAR}"),
        },
    }
}

fn result_text(v: &Verdict) -> String {
    let with = |w: &Option<String>| match w {
        Some(w) => format!(", with a warning: {w}"),
        None => String::new(),
    };
    match v {
        Verdict::Ok {
            find_ran: true,
            warning,
        } => format!("ok{}", with(warning)),
        Verdict::Ok {
            find_ran: false,
            warning,
        } => format!("ok (find not run){}", with(warning)),
        Verdict::None { warning } => format!("none{}", with(warning)),
        Verdict::Error(message) => format!("error: {message}"),
        Verdict::Refused(message) => format!(
            "none (find refused): {message}; with no config, the daemon logs this and \
             warns of nothing, {NAME}.default having guessed"
        ),
    }
}

/// What `<name>.default` named and proved: `default  names tmux and rex`,
/// then each `inside` proof it ran, as `  inside.tmux  no: why`.
fn default_lines(d: &Defaulted) -> Vec<String> {
    let names: Vec<&str> = d.named.iter().map(|n| n.name()).collect();
    let named = match names.as_slice() {
        [] => "nothing".to_string(),
        [one] => format!("{one}; nothing to prove"),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    };
    let mut lines = vec![format!("default  names {named}")];
    for (table, proof) in &d.proofs {
        let said = if proof.ok { "yes" } else { "no" };
        lines.push(format!("  inside.{}  {said}: {}", table.name(), proof.why));
    }
    lines
}

fn default_json(d: &Defaulted) -> Value {
    let inside: Vec<Value> = d
        .proofs
        .iter()
        .map(|(table, proof)| json!({ "name": table.name(), "ok": proof.ok, "why": proof.why }))
        .collect();
    let named: Vec<&str> = d.named.iter().map(|n| n.name()).collect();
    json!({ "named": named, "inside": inside })
}

/// The JSON report: one object, and a newline.
pub(super) fn json(c: &Check) -> String {
    let config = match &c.config {
        ConfigState::File(path) => json!({ "path": path.to_string_lossy(), "state": "file" }),
        ConfigState::NoFile => json!({ "path": null, "state": "none" }),
        ConfigState::Skipped => json!({ "path": null, "state": "skipped" }),
    };
    let a = &c.attach;
    let hint = match &c.config {
        ConfigState::Skipped => Some("none".to_string()),
        _ => a.provider.clone(),
    };
    let attach = json!({
        "tty": String::from_utf8_lossy(&a.tty),
        "pid": a.pid,
        "cwd": String::from_utf8_lossy(&a.cwd),
        "daemon": a.daemon,
        "provider": hint,
    });
    let ran = c.ran.as_ref().map_or(Value::Null, |r| {
        json!({
            "seconds": r.took.as_secs_f64(),
            "printed": r.printed,
            "error": r.error,
        })
    });
    let (chose, tables_json, one_step_json) = match &c.chose {
        Some((choice, how)) => (
            chose_json(choice, *how),
            tables(choice)
                .into_iter()
                .map(|(p, role)| table_json(p, role, c))
                .collect(),
            one_step(choice).unwrap_or_default(),
        ),
        None => (Value::Null, Vec::new(), Vec::new()),
    };
    let rex = c.rex.as_ref().map_or(Value::Null, |r| rex_json(r, &c.find));
    let file_name = match &c.config {
        ConfigState::File(path) => path.file_name().map(|n| n.to_string_lossy().into_owned()),
        _ => None,
    };
    let problems: Vec<Value> = c
        .problems
        .0
        .iter()
        .map(|p| problem_json(p, file_name.as_deref()))
        .collect();
    let result = match &c.result {
        Verdict::Ok { warning, .. } => json!({ "state": "ok", "message": warning }),
        Verdict::None { warning } => json!({ "state": "none", "message": warning }),
        Verdict::Error(m) => json!({ "state": "error", "message": m }),
        Verdict::Refused(m) => json!({ "state": "none", "message": null, "refused": m }),
    };
    let v = json!({
        "config": config,
        "attach": attach,
        "ran": ran,
        "chose": chose,
        "tables": tables_json,
        "one_step": one_step_json,
        "find": find_json(&c.find),
        "rex": rex,
        "tmux": c.tmux.as_ref().map_or(Value::Null, tmux_json),
        "default": c.default.as_ref().map_or(Value::Null, default_json),
        "problems": problems,
        "result": result,
    });
    format!("{v}\n")
}

fn chose_json(choice: &Choice, how: How) -> Value {
    let how = match how {
        How::Provider => json!("provider"),
        How::Default => json!("default"),
        How::Hint => Value::Null,
    };
    let (kind, name, path) = match choice {
        Choice::None => ("none", None, None),
        Choice::Builtin(b) => ("builtin", Some(b.name().to_string()), None),
        Choice::Whole(p) => ("table", Some(p.name.clone()), Some(p.path.clone())),
        Choice::Pair(pair) => (
            PairKind::of(pair).name(),
            Some(pair.name.clone()),
            Some("provider()".to_string()),
        ),
    };
    json!({ "kind": kind, "name": name, "path": path, "how": how })
}

fn table_json(p: &Provider, role: Role, c: &Check) -> Value {
    let mut caps = Map::new();
    for cap in listed(p, role, c) {
        let v = match state(cap, p, c) {
            State::Present(command) => json!({
                "state": "present",
                "argv": command
                    .argv
                    .iter()
                    .map(|w| String::from_utf8_lossy(w).into_owned())
                    .collect::<Vec<_>>(),
                "timeout": command.timeout.map(|t| t.as_secs_f64()),
            }),
            State::Ignored(why) => json!({ "state": "ignored", "why": why }),
            State::Absent(fallback) => json!({ "state": "absent", "fallback": fallback }),
        };
        caps.insert(cap.name().into(), v);
    }
    let mut settings = Map::new();
    settings.insert("name".into(), json!(p.name));
    if role.gives_terminals() {
        settings.insert(
            "attach_timeout".into(),
            json!(p.attach_timeout.as_secs_f64()),
        );
    }
    let context: Map<String, Value> = p
        .context_vars
        .iter()
        .map(|v| (v.clone(), json!(value_of(c, v))))
        .collect();
    json!({
        "role": role.name(),
        "name": p.name,
        "path": p.path,
        "protocol": p.protocol,
        "capabilities": caps,
        "settings": settings,
        "live_layout": role.gives_layout().then(|| LiveLayout::of(p).name()),
        "context_vars": context,
        "find_vars": p.find_vars,
    })
}

fn find_json(f: &Find) -> Value {
    let (ran, verdict) = match f {
        Find::NotRun(_) | Find::Absent => (false, "not run"),
        _ if f.refused() => (true, "refused"),
        _ => (true, "accepted"),
    };
    let (title, exit, took, why) = match f {
        Find::NotRun(why) => (None, None, None, Some(why.clone())),
        Find::Absent => (
            None,
            None,
            None,
            Some(fallback(Capability::Find, false).to_string()),
        ),
        Find::Ran {
            title,
            took,
            answer,
        } => {
            let (exit, why) = match answer {
                Answered::Id(_) | Answered::NoId => (Some(0), None),
                Answered::Unsupported => (None, Some("unsupported".to_string())),
                Answered::Failed(why) => (exit_of(why), Some(why.clone())),
                Answered::TimedOut(t) => {
                    (None, Some(format!("no answer within {} s", seconds(*t))))
                }
            };
            (Some(title.clone()), exit, Some(*took), why)
        }
        Find::Rex { took, verdict, .. } => {
            let why = match verdict {
                RexVerdict::Proven => None,
                RexVerdict::Disproved(why) | RexVerdict::Unprovable(why) => Some(why.clone()),
            };
            (None, None, Some(*took), why)
        }
    };
    json!({
        "ran": ran,
        "title": title,
        "exit": exit,
        "seconds": took.map(|t| t.as_secs_f64()),
        "id": f.id(),
        "verdict": verdict,
        "why": why,
    })
}

fn rex_json(r: &RexReport, f: &Find) -> Value {
    let find = match f {
        Find::Rex { verdict, .. } => json!(match verdict {
            RexVerdict::Proven => "proven",
            RexVerdict::Disproved(_) => "disproved",
            RexVerdict::Unprovable(_) => "unprovable",
        }),
        _ => Value::Null,
    };
    // every capability: Rex doesn't say which revision each came in
    let missing: Vec<Value> = r
        .missing
        .iter()
        .map(|needs| json!({ "capability": null, "needs": needs }))
        .collect();
    json!({
        "server": r.server,
        "version": r.version,
        "api_revision": r.api_revision,
        "missing": missing,
        "find": find,
    })
}

/// A problem as JSON: its path, where it has one, and its message.
fn problem_json(p: &Problem, file: Option<&str>) -> Value {
    let level = if is_error(p) { "error" } else { "warning" };
    let (path, message) = split_path(&p.text, file);
    json!({ "level": level, "path": path, "message": message })
}

/// A problem's text as its path and message: a path rooted at
/// `provider()`, then `: ` or a space; or the file's name, then a space.
fn split_path<'a>(text: &'a str, file: Option<&str>) -> (Option<&'a str>, &'a str) {
    if let Some(end) = path_end(text)
        && let Some(message) = text[end..]
            .strip_prefix(": ")
            .or_else(|| text[end..].strip_prefix(' '))
    {
        return (Some(&text[..end]), message);
    }
    if let Some(file) = file
        && let Some(message) = text.strip_prefix(file).and_then(|r| r.strip_prefix(' '))
    {
        return (Some(&text[..file.len()]), message);
    }
    (None, text)
}

/// Where a path rooted at `provider()` that `text` starts with ends: past
/// its `.key`, `["odd key"]` and `[n]` steps.
fn path_end(text: &str) -> Option<usize> {
    let mut at = "provider()".len();
    if !text.starts_with("provider()") {
        return None;
    }
    let b = text.as_bytes();
    loop {
        match b.get(at) {
            Some(b'.') => {
                at += 1;
                while b
                    .get(at)
                    .is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_')
                {
                    at += 1;
                }
            }
            Some(b'[') if b.get(at + 1) == Some(&b'"') => {
                at += 2;
                while let Some(&c) = b.get(at) {
                    at += 1;
                    match c {
                        b'\\' => at += 1,
                        b'"' => break,
                        _ => {}
                    }
                }
                if b.get(at) != Some(&b']') {
                    return None;
                }
                at += 1;
            }
            Some(b'[') => {
                at += 1 + text[at + 1..].find(']')? + 1;
            }
            _ => return Some(at),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_problems_path_is_split_from_its_message() {
        let file = Some("init.lua");
        assert_eq!(
            (
                Some("provider().layout.focus[2]"),
                "expected string, got number"
            ),
            split_path(
                "provider().layout.focus[2]: expected string, got number",
                file
            )
        );
        assert_eq!(
            (Some(r#"provider()["a: b"]"#), "unknown field; ignored"),
            split_path(r#"provider()["a: b"]: unknown field; ignored"#, file)
        );
        assert_eq!(
            (Some("provider()"), "raised: boom"),
            split_path("provider() raised: boom", file)
        );
        assert_eq!(
            (Some("init.lua"), "failed to load: x"),
            split_path("init.lua failed to load: x", file)
        );
        assert_eq!((None, "/x isn't there"), split_path("/x isn't there", file));
    }

    /// A tmux 3.4 on a server that answers, whose table gives `open_float`
    /// and `events`.
    fn tmux_3_4() -> TmuxReport {
        use crate::providers::tmux::check::Missing;
        TmuxReport {
            path: Some("/usr/bin/tmux".into()),
            version: Some("3.4".into()),
            socket: Some("/tmp/tmux-501/default".into()),
            server: Ok(()),
            missing: vec![
                Missing {
                    capability: Some(Capability::OpenFloat),
                    needs: (3, 7),
                },
                Missing {
                    capability: Some(Capability::Events),
                    needs: (3, 8),
                },
            ],
        }
    }

    #[test]
    fn tmuxs_section_names_its_path_version_socket_server_and_each_capability_it_cant() {
        assert_eq!(
            tmux_lines(&tmux_3_4()),
            [
                "tmux  /usr/bin/tmux",
                "  version  3.4",
                "  socket  /tmp/tmux-501/default",
                "  server  answers",
                "  open_float  exits 69: needs tmux 3.7, this is 3.4",
                "  events  exits 69: needs tmux 3.8, this is 3.4",
            ]
        );
        assert_eq!(
            tmux_json(&tmux_3_4()),
            json!({
                "path": "/usr/bin/tmux",
                "version": "3.4",
                "socket": "/tmp/tmux-501/default",
                "answers": true,
                "error": null,
                "missing": [
                    { "capability": "open_float", "needs": "3.7" },
                    { "capability": "events", "needs": "3.8" },
                ],
            })
        );
    }

    #[test]
    fn a_tmux_below_the_floor_misses_every_capability_and_one_not_found_says_so() {
        use crate::providers::tmux::check::Missing;
        let old = TmuxReport {
            version: Some("3.1c".into()),
            server: Err("no server running on /tmp/x".into()),
            missing: vec![Missing {
                capability: None,
                needs: (3, 2),
            }],
            ..tmux_3_4()
        };
        assert_eq!(
            tmux_lines(&old)[3..],
            [
                "  server  doesn't answer: no server running on /tmp/x",
                "  missing  every capability (needs tmux 3.2, this is 3.1c)",
            ]
        );
        let v = tmux_json(&old);
        assert_eq!(v["answers"], false);
        assert_eq!(v["error"], "no server running on /tmp/x");
        assert_eq!(
            v["missing"],
            json!([{ "capability": null, "needs": "3.2" }])
        );

        let none = TmuxReport {
            path: None,
            version: None,
            socket: None,
            server: Err("TMUX isn't set".into()),
            missing: Vec::new(),
        };
        assert_eq!(
            tmux_lines(&none),
            [
                "tmux  not on PATH",
                "  socket  none: TMUX isn't set",
                "  server  doesn't answer: TMUX isn't set",
            ]
        );
    }

    #[test]
    fn long_lines_are_cut_at_100_columns() {
        let line = "é".repeat(150);
        let cut = cut(&line);
        assert_eq!(100, cut.chars().count());
        assert!(cut.ends_with('…'));
        assert_eq!("short", super::cut("short"));
    }

    #[test]
    fn argv_words_are_quoted_as_a_shell_reads_them() {
        let argv: Vec<Vec<u8>> = ["sh", "-c", "it's {attach}", "{attach}", ""]
            .iter()
            .map(|w| w.as_bytes().to_vec())
            .collect();
        assert_eq!(r#"sh -c 'it'\''s {attach}' {attach} ''"#, argv_line(&argv));
    }
}
