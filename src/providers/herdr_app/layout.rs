//! `report_layout`: the live layout of every tab of every space, from the
//! core's doc and `terminal.list`, and the window's `herdr-app status`.
//!
//! - One layout area per tab, its id the tab's (`tb…`). Foreign panes are
//!   in it too: a mod's block, by its pane id (`p…`).
//! - Each rect is the pane's **outer** share of its tab, from the doc's
//!   weights alone: a node with no `size` weighs 1, weights are relative
//!   among siblings, and a split shares its rect along its axis, a `row`
//!   across, a `column` down. Weights share the panes' outer extent,
//!   chrome included (SPI-1214). A node sized in pixels, `{px}`, is left
//!   out with all it holds: its room can't be told in shares.
//! - Each inset is the pane's chrome (its border, two-row header, padding
//!   and gap) in cells: the same for every pane, as it is fixed in
//!   pixels. It is solved from the shown tab's cells and shares, on each
//!   axis: a pane's cells plus the chrome are its share of the tab's
//!   outer extent, so, fitted over its panes, the chrome is where that
//!   line crosses no share. A tab whose shares along an axis don't differ
//!   enough, a lone pane's both, can't tell it; nor can stale cells, a
//!   solve outside 0 to twice the measured: then it is the measured, 5
//!   columns and 4 rows at font size 14, scaled to the window's
//!   `font_size`. Only the shown tab's cells are read, as a hidden tab's
//!   may be stale, or the 120x40 of a tab never shown. The window doesn't
//!   tell where in its chrome the content starts: the chrome is split
//!   evenly, the odd cell right and top.
//! - A terminal two blocks show is left out, as is one none shows.
//! - No zoom: neither the doc nor `status` tells it, so a zoomed pane is
//!   reported where it is unzoomed, and Neovim's screen is too large for
//!   it while its tab is zoomed.
//! - `focus` is the terminal of the window's `focused_pane`, as `status`
//!   says; none without a window to ask.

use std::time::Duration;

use serde_json::{Value, json};

use super::core::{Core, Terminal, UNREADABLE};
use super::window::Window;
use super::{Failure, methods, needs};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `report_layout` has, of its 1 s.
const LAYOUT_TIME: Duration = Duration::from_millis(900);

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, Failure> {
    report_layout_within(given, LAYOUT_TIME)
}

/// `report_layout`, its requests and `herdr-app status` having `time`.
/// Without a window to ask, the report has no focus, said on stderr.
fn report_layout_within(given: &Given, time: Duration) -> Result<String, Failure> {
    let mut core = Core::of(given, time)?;
    needs(&core, methods(Capability::ReportLayout), || {
        Window::of(given)
    })?;
    let doc = core.doc().map_err(String::from)?;
    let listed = core.list().map_err(String::from)?;
    let status = Window::of(given)
        .and_then(|w| w.status(core.due()).map_err(String::from))
        .map_err(|why| eprintln!("herdr-app report_layout: no focus: {why}"))
        .ok();
    Ok(layout(&doc, &listed, status.as_ref())?.to_string())
}

/// A pane's chrome at [`MEASURED_FONT`], in columns and rows (SPI-1214).
const MEASURED_CHROME: [f64; 2] = [5.0, 4.0];

/// The font size the chrome was measured at.
const MEASURED_FONT: f64 = 14.0;

/// How much the shown tab's shares along an axis must differ, the
/// largest less the smallest, for its cells to tell the chrome: at a
/// quarter, a cell's rounding in each pane moves the solve by a cell or
/// two at most.
const SPREAD: f64 = 0.25;

/// A pane of a tab, where its weights put it.
#[derive(Clone, Copy)]
struct Pane<'a> {
    id: &'a str,
    /// The hc terminal it shows; None for a mod.
    terminal: Option<&'a str>,
    /// x, y, w, h, as fractions of its tab.
    rect: [f64; 4],
}

impl Pane<'_> {
    /// Its id in the report: its terminal's, else its own.
    fn reported(&self) -> &str {
        self.terminal.unwrap_or(self.id)
    }
}

/// The report `report_layout` prints, from the core's `doc`, its
/// terminals `listed`, and the window's `status`, if it answered.
pub(super) fn layout(
    doc: &Value,
    listed: &[Terminal],
    status: Option<&Value>,
) -> Result<Value, String> {
    let tabs = tabs_of(doc)?;
    let doubled = |terminal: &str| {
        let shows = tabs.iter().flat_map(|(_, panes)| panes);
        shows.filter(|p| p.terminal == Some(terminal)).count() > 1
    };
    let tabs: Vec<(&str, Vec<Pane>)> = tabs
        .iter()
        .map(|(id, panes)| {
            let kept = panes.iter().filter(|p| !p.terminal.is_some_and(doubled));
            (*id, kept.copied().collect())
        })
        .collect();
    let shown = status.and_then(|s| s["tab"].as_str());
    let shown = tabs.iter().find(|(id, _)| Some(*id) == shown);
    let font = status
        .and_then(|s| s["font_size"].as_f64())
        .filter(|f| f.is_finite() && *f > 0.0)
        .unwrap_or(MEASURED_FONT);
    let [cols, rows] = chrome(shown.map_or(&[][..], |(_, p)| p), listed, font);
    let inset = [cols / 2, rows - rows / 2, cols - cols / 2, rows / 2];
    let areas: Vec<Value> = tabs
        .iter()
        .filter(|(_, panes)| !panes.is_empty())
        .map(|(id, panes)| {
            let terminals: Vec<Value> = panes
                .iter()
                .map(|p| json!({ "terminal": p.reported(), "rect": p.rect, "inset": inset }))
                .collect();
            json!({ "id": id, "terminals": terminals })
        })
        .collect();
    let mut report = json!({ "areas": areas });
    let focused = status.and_then(|s| s["focused_pane"].as_str());
    let focus = tabs
        .iter()
        .flat_map(|(_, panes)| panes)
        .find(|p| Some(p.id) == focused);
    if let Some(focus) = focus {
        report["focus"] = focus.reported().into();
    }
    Ok(report)
}

/// The chrome in cells, columns and rows: solved from `shown`, the shown
/// tab's panes, with their sizes as `listed` has them; where it can't be,
/// the measured, at font size `font`.
fn chrome(shown: &[Pane], listed: &[Terminal], font: f64) -> [u32; 2] {
    let size = |p: &Pane| {
        let t = listed
            .iter()
            .find(|t| Some(t.address.as_str()) == p.terminal)?;
        t.size
    };
    [0, 1].map(|axis| {
        let measured = (MEASURED_CHROME[axis] * MEASURED_FONT / font).round();
        let points: Vec<(f64, f64)> = shown
            .iter()
            .filter_map(|p| Some((p.rect[2 + axis], f64::from(size(p)?[axis]))))
            .collect();
        let solved = solve(&points).filter(|k| (0.0..=2.0 * measured).contains(k));
        solved.unwrap_or(measured) as u32
    })
}

/// The chrome, in cells, that `points`, each pane's share of its tab and
/// its cells along one axis, tell, rounded: fitting cells = share × outer
/// − chrome by least squares. None if their shares don't differ by
/// [`SPREAD`].
fn solve(points: &[(f64, f64)]) -> Option<f64> {
    let shares = points.iter().map(|(s, _)| *s);
    let spread = shares.clone().fold(f64::MIN, f64::max) - shares.fold(f64::MAX, f64::min);
    if points.len() < 2 || spread < SPREAD {
        return None;
    }
    let n = points.len() as f64;
    let mean_share = points.iter().map(|(s, _)| s).sum::<f64>() / n;
    let mean_cells = points.iter().map(|(_, c)| c).sum::<f64>() / n;
    let (mut covariance, mut variance) = (0.0, 0.0);
    for (s, c) in points {
        covariance += (s - mean_share) * (c - mean_cells);
        variance += (s - mean_share).powi(2);
    }
    let outer = covariance / variance;
    Some((outer * mean_share - mean_cells).round())
}

/// Every tab of `doc`, in the doc's order, its id and its panes; none for
/// a doc never written.
fn tabs_of(doc: &Value) -> Result<Vec<(&str, Vec<Pane<'_>>)>, String> {
    if doc.is_null() {
        return Ok(Vec::new());
    }
    let mut tabs = Vec::new();
    for space in doc["spaces"].as_array().ok_or(UNREADABLE)? {
        for tab in space["tabs"].as_array().ok_or(UNREADABLE)? {
            let id = tab["id"].as_str().ok_or(UNREADABLE)?;
            let mut panes = Vec::new();
            walk(&tab["root"], [0.0, 0.0, 1.0, 1.0], &mut panes)?;
            tabs.push((id, panes));
        }
    }
    Ok(tabs)
}

/// Adds the panes under `node`, `node` itself if it is one, at `rect`, to
/// `panes`.
fn walk<'a>(node: &'a Value, rect: [f64; 4], panes: &mut Vec<Pane<'a>>) -> Result<(), String> {
    let axis = match node["type"].as_str() {
        Some("block") => {
            panes.push(Pane {
                id: node["id"].as_str().ok_or(UNREADABLE)?,
                terminal: node["content"]["terminal"].as_str(),
                rect,
            });
            return Ok(());
        }
        Some("row") => 0,
        Some("column") => 1,
        _ => return Err(UNREADABLE.into()),
    };
    let mut weighed = Vec::new();
    for child in node["children"].as_array().ok_or(UNREADABLE)? {
        if let Some(weight) = weight(&child["size"])? {
            weighed.push((child, weight));
        }
    }
    let total: f64 = weighed.iter().map(|(_, w)| w).sum();
    let mut at = rect[axis];
    for (child, weight) in weighed {
        let mut part = rect;
        part[axis] = at;
        part[2 + axis] = rect[2 + axis] * weight / total;
        at += part[2 + axis];
        walk(child, part, panes)?;
    }
    Ok(())
}

/// A node's weight, from its `size`: 1 with none; None if sized in
/// pixels.
fn weight(size: &Value) -> Result<Option<f64>, String> {
    if size.is_null() {
        return Ok(Some(1.0));
    }
    if size.get("px").is_some() {
        return Ok(None);
    }
    match size["weight"].as_f64() {
        Some(w) if w.is_finite() && w > 0.0 => Ok(Some(w)),
        _ => Err(UNREADABLE.into()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::super::core::Terminal;
    use std::time::Duration;

    use super::super::Failure;
    use super::super::fake::{FakeApp, FakeCore, given, recorded, recorded_one, recorded_run};
    use super::*;

    const NESTED: &str = "tb14c28a7769ba1044103fd9584cc818f0";
    const SHAPES: &str = "tb647b2d25c9734a2636b21f8d1a7c8c1c";

    /// The doc and the list `core/shapes-doc-and-list` recorded: the
    /// nested tab `[t1 | (t2 / t3)]` and the shapes tab
    /// `[t4 | t5 | (t6 / t7 / [t8 | t9])]`, each terminal's real size.
    fn shapes() -> (Value, Vec<Terminal>) {
        let exchanges = recorded("shapes-doc-and-list");
        let reply = |method: &str| {
            let e = exchanges.iter().find(|e| e.method == method).unwrap();
            e.replies[0]["result"].clone()
        };
        let listed = reply("terminal.list")["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| Terminal {
                address: t["address"].as_str().unwrap().into(),
                live: true,
                pid: None,
                size: Some([
                    t["cols"].as_u64().unwrap() as u32,
                    t["rows"].as_u64().unwrap() as u32,
                ]),
            })
            .collect();
        (reply("doc.get")["value"].clone(), listed)
    }

    /// `herdr-app status` as recorded: the nested tab shown, `t3`'s pane
    /// focused, font size 14.
    fn status() -> Value {
        serde_json::from_str(&recorded_run("status").stdout).unwrap()
    }

    fn t(id: &str, rect: [f64; 4], inset: [u32; 4]) -> Value {
        json!({ "terminal": id, "rect": rect, "inset": inset })
    }

    /// The chrome at font size 14: 5 columns, 4 rows; the odd column on
    /// the right.
    const CHROME: [u32; 4] = [2, 2, 3, 2];

    #[test]
    fn every_tab_is_an_area_its_panes_outer_shares_by_weight_inset_by_the_chrome() {
        let (doc, listed) = shapes();
        let report = layout(&doc, &listed, Some(&status())).unwrap();
        assert_eq!(
            report,
            json!({
                "areas": [
                    {"id": NESTED, "terminals": [
                        t("t1", [0.0, 0.0, 0.5, 1.0], CHROME),
                        t("t2", [0.5, 0.0, 0.5, 0.5], CHROME),
                        t("t3", [0.5, 0.5, 0.5, 0.5], CHROME),
                    ]},
                    {"id": SHAPES, "terminals": [
                        t("t4", [0.0, 0.0, 0.5, 1.0], CHROME),
                        t("t5", [0.5, 0.0, 0.25, 1.0], CHROME),
                        t("t6", [0.75, 0.0, 0.25, 0.5], CHROME),
                        t("t7", [0.75, 0.5, 0.25, 0.25], CHROME),
                        t("t8", [0.75, 0.75, 0.125, 0.25], CHROME),
                        t("t9", [0.875, 0.75, 0.125, 0.25], CHROME),
                    ]},
                ],
                "focus": "t3",
            })
        );
    }

    /// The area, cells and inset over share, on each axis, is the same
    /// from every pane of the tab `[t1 | (t2 / t3)]` (SPI-1213's
    /// `[t1 | (t9 / t10)]`): the chrome its rows were solved for.
    #[test]
    fn the_area_is_the_same_from_every_pane_of_the_nested_tab() {
        let (doc, listed) = shapes();
        let report = layout(&doc, &listed, Some(&status())).unwrap();
        let size = |id: &str| {
            listed
                .iter()
                .find(|t| t.address == id)
                .unwrap()
                .size
                .unwrap()
        };
        let areas: Vec<[f64; 2]> = report["areas"][0]["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                let [cols, rows] = size(p["terminal"].as_str().unwrap()).map(f64::from);
                let n = |v: &Value| v.as_f64().unwrap();
                let (rect, inset) = (&p["rect"], &p["inset"]);
                [
                    (cols + n(&inset[0]) + n(&inset[2])) / n(&rect[2]),
                    (rows + n(&inset[1]) + n(&inset[3])) / n(&rect[3]),
                ]
            })
            .collect();
        assert_eq!(areas, [[88.0, 56.0]; 3]);
    }

    /// The chrome its cells tell wins over the measured: at font size 20
    /// the measured would be 4 columns and 3 rows, and the areas from
    /// `t1` and `t2` would differ by a row.
    #[test]
    fn chrome_solved_wins_over_the_measured() {
        let (doc, listed) = shapes();
        let mut status = status();
        status["font_size"] = json!(20.0);
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        // the columns can't be solved: every share across is a half
        assert_eq!(
            report["areas"][0]["terminals"][0]["inset"],
            json!([2, 2, 2, 2])
        );
        assert_eq!(
            report["areas"][1]["terminals"][0]["inset"],
            json!([2, 2, 2, 2])
        );
    }

    /// Where the shown tab's shares don't differ along an axis, a lone
    /// pane's both, the chrome can't be solved: it is 5 columns and 4 rows
    /// at font size 14, fixed in pixels, so fewer cells at a bigger font.
    #[test]
    fn chrome_unsolved_is_the_measured_at_the_windows_font_size() {
        let doc = json!({"spaces": [{"tabs": [{"id": "tb1", "root": {
        "type": "row", "id": "tb1", "children": [
            {"type": "block", "id": "p1", "content": {"terminal": "t1"}},
        ]}}]}]});
        let listed = [Terminal {
            address: "t1".into(),
            live: true,
            pid: None,
            size: Some([80, 24]),
        }];
        let mut status = status();
        status["tab"] = json!("tb1");
        status["focused_pane"] = json!("p1");
        status["font_size"] = json!(28.0);
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        assert_eq!(
            report,
            json!({"areas": [{"id": "tb1", "terminals": [
                t("t1", [0.0, 0.0, 1.0, 1.0], [1, 1, 2, 1]),
            ]}], "focus": "t1"})
        );
        // no window to ask: font size 14, and no focus
        let report = layout(&doc, &listed, None).unwrap();
        assert_eq!(
            report,
            json!({"areas": [{"id": "tb1", "terminals": [
                t("t1", [0.0, 0.0, 1.0, 1.0], CHROME),
            ]}]})
        );
    }

    /// The chrome is solved from the shown tab's own cells: a hidden
    /// tab's may be stale, or the 120x40 of one never shown.
    #[test]
    fn chrome_is_solved_from_the_shown_tab_alone() {
        let (doc, mut listed) = shapes();
        for t in listed.iter_mut().filter(|t| t.address.as_str() >= "t4") {
            t.size = Some([120, 40]);
        }
        let report = layout(&doc, &listed, Some(&status())).unwrap();
        assert_eq!(
            report["areas"][1]["terminals"][0],
            t("t4", [0.0, 0.0, 0.5, 1.0], CHROME)
        );
        // and a solve far from the measured one is taken for stale cells
        let mut status = status();
        status["tab"] = json!(SHAPES);
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        assert_eq!(report["areas"][1]["terminals"][0]["inset"], json!(CHROME));
    }

    /// A mod's pane is a foreign pane, by its pane id; a node sized in
    /// pixels is left out, with all it holds, its room unknown in shares;
    /// so is a terminal two blocks show.
    #[test]
    fn mods_are_foreign_panes_and_px_nodes_and_doubled_terminals_are_left_out() {
        let block = |id: &str, content: Value, size: Value| json!({"type": "block", "id": id, "content": content, "size": size});
        let doc = json!({"spaces": [{"tabs": [{"id": "tb1", "root": {
        "type": "row", "id": "tb1", "children": [
            block("pmod", json!({"mod": "files"}), json!({"weight": 1})),
            block("p1", json!({"terminal": "t1"}), json!({"weight": 3})),
            block("p2", json!({"terminal": "t2"}), json!({"px": 200})),
            {"type": "column", "id": "s1", "size": {"px": 100}, "children": [
                block("p3", json!({"terminal": "t3"}), Value::Null),
            ]},
        ]}}]}, {"tabs": [{"id": "tb2", "root": {
        "type": "column", "id": "tb2", "children": [
            block("p4", json!({"terminal": "t4"}), Value::Null),
            block("p5", json!({"terminal": "t4"}), Value::Null),
        ]}}]}]});
        let mut status = status();
        status["tab"] = json!("tb1");
        status["focused_pane"] = json!("pmod");
        let report = layout(&doc, &[], Some(&status)).unwrap();
        assert_eq!(
            report,
            json!({"areas": [{"id": "tb1", "terminals": [
                t("pmod", [0.0, 0.0, 0.25, 1.0], CHROME),
                t("t1", [0.25, 0.0, 0.75, 1.0], CHROME),
            ]}], "focus": "pmod"})
        );
    }

    /// Well over [`LAYOUT_TIME`], as the fakes run shell scripts.
    const TIME: Duration = Duration::from_secs(5);

    #[test]
    fn the_report_reads_the_doc_and_list_on_one_connection_and_asks_the_window_its_status() {
        let core = FakeCore::replaying(&["shapes-doc-and-list"]);
        let app = FakeApp::new(&[recorded_run("status")]);
        let given = given(&core, &app, &[]);
        let (doc, listed) = shapes();
        let line = report_layout_within(&given, TIME).unwrap();
        let report: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(report, layout(&doc, &listed, Some(&status())).unwrap());
        assert_eq!(report["focus"], "t3");
        assert_eq!(core.methods(), ["hello", "doc.get", "terminal.list"]);
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn with_no_window_the_report_has_no_focus() {
        let core = FakeCore::replaying(&["shapes-doc-and-list"]);
        let app = FakeApp::new(&[recorded_run("err-no-window-status")]);
        let given = given(&core, &app, &[]);
        let line = report_layout_within(&given, TIME).unwrap();
        let report: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(report.get("focus"), None);
        assert_eq!(report["areas"].as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn on_a_core_without_terminal_list_the_report_is_unsupported_naming_both_versions() {
        let mut hello = recorded_one("hello", "hello");
        let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
        methods.retain(|m| m != "terminal.list");
        let core = FakeCore::serve(vec![hello]);
        let app = FakeApp::new(&[recorded_run("version")]);
        let given = given(&core, &app, &[]);
        assert_eq!(
            report_layout_within(&given, TIME),
            Err(Failure::Unsupported(
                "the hc core has no terminal.list (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
        assert_eq!(core.methods(), ["hello"]);
    }

    #[test]
    fn a_doc_it_cant_read_fails_the_report() {
        let tab = |root: Value| json!({"spaces": [{"tabs": [{"id": "tb1", "root": root}]}]});
        for bad in [
            json!({"spaces": 3}),
            tab(json!({"type": "row", "children": [
                {"type": "block", "id": "p1", "content": {"terminal": "t1"},
                 "size": {"weight": -1}},
            ]})),
            tab(json!({"type": "row", "children": [
                {"type": "block", "content": {"terminal": "t1"}},
            ]})),
            json!({"spaces": [{"tabs": [{"root": {"type": "row", "children": []}}]}]}),
        ] {
            assert!(layout(&bad, &[], None).is_err(), "{bad}");
        }
        // never written: no areas
        assert_eq!(
            layout(&Value::Null, &[], None).unwrap(),
            json!({"areas": []})
        );
    }
}
