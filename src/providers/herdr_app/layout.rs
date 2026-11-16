//! `report_layout`, as `docs/capabilities.md`'s Herdr app section gives
//! it: the core's doc and `terminal.list`, then `herdr-app status`.
//!
//! - [`tabs_of`] walks the doc: each tab's panes, at the rects their
//!   weights give them, as fractions of the tab. A pane under a node sized
//!   in pixels is marked so, and the panes beside such a node, which
//!   share a room it takes part of, approximate.
//! - [`layout`] makes the report: an area per tab, each pane inset by
//!   [`chrome`]; a terminal it can't place, under a pixel-sized node or in
//!   two blocks, in an area of its own; `focus` from `status`.
//! - [`chrome`] is the pane chrome in cells: the measured, scaled to the
//!   window's font size, unless the shown tab's cells, by [`solve`], tell
//!   another more than a cell away.

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
    let window = Window::of(given);
    needs(&core, methods(Capability::ReportLayout), || window.clone())?;
    let doc = core.doc().map_err(String::from)?;
    let listed = core.list().map_err(String::from)?;
    let status = window
        .and_then(|w| w.status(core.due()).map_err(String::from))
        .map_err(|why| eprintln!("herdr-app report_layout: no focus: {why}"))
        .ok();
    Ok(layout(&doc, &listed, status.as_ref())?.to_string())
}

/// A pane's chrome at [`MEASURED_FONT`], in columns and rows (SPI-1214).
const MEASURED_CHROME: [f64; 2] = [5.0, 4.0];

/// The font size the chrome was measured at.
const MEASURED_FONT: f64 = 14.0;

/// The font sizes taken as the window's: outside them, `status` is taken
/// to be wrong, and the measured font size is used.
const FONTS: std::ops::RangeInclusive<f64> = 4.0..=200.0;

/// How much the shown tab's shares along an axis must differ, the
/// largest less the smallest, for its cells to tell the chrome: at a
/// quarter, a cell's rounding in each pane moves the solve by a cell or
/// two at most.
const SPREAD: f64 = 0.25;

/// How far, in cells, a solve must be from the measured chrome to be
/// taken instead: rounding alone moves it about a cell.
const TRUSTED: f64 = 1.0;

/// A pane of a tab, where its weights put it.
#[derive(Clone, Copy)]
struct Pane<'a> {
    id: &'a str,
    /// The hc terminal it shows; None for a mod.
    terminal: Option<&'a str>,
    /// x, y, w, h, as fractions of its tab.
    rect: [f64; 4],
    /// Under a node sized in pixels: its rect can't be told.
    px: bool,
    /// Under a split that has a node sized in pixels, which takes room its
    /// weights don't share: its rect is approximate.
    approx: bool,
}

impl Pane<'_> {
    /// Its id in the report: its terminal's, else its own.
    fn reported(&self) -> &str {
        self.terminal.unwrap_or(self.id)
    }
}

/// A tab of the doc, and its panes.
struct Tab<'a> {
    id: &'a str,
    panes: Vec<Pane<'a>>,
}

/// The report `report_layout` prints, from the core's `doc`, its
/// terminals `listed`, and the window's `status`, if it answered. A tab
/// it can't read is left out, said on stderr.
pub(super) fn layout(
    doc: &Value,
    listed: &[Terminal],
    status: Option<&Value>,
) -> Result<Value, String> {
    let tabs = tabs_of(doc)?;
    let every = || tabs.iter().flat_map(|t| &t.panes);
    let blocks_of = |terminal: &str| every().filter(|p| p.terminal == Some(terminal)).count();
    let doubled = |p: &Pane| p.terminal.is_some_and(|t| blocks_of(t) > 1);
    let shown = status.and_then(|s| s["tab"].as_str());
    let shown: Vec<Pane> = tabs
        .iter()
        .filter(|t| Some(t.id) == shown)
        .flat_map(|t| &t.panes)
        .filter(|p| !p.px && !p.approx && !doubled(p))
        .copied()
        .collect();
    let font = status
        .and_then(|s| s["font_size"].as_f64())
        .filter(|f| FONTS.contains(f))
        .unwrap_or(MEASURED_FONT);
    let [cols, rows] = chrome(&shown, listed, font);
    let inset = [cols / 2, rows - rows / 2, cols - cols / 2, rows / 2];
    let mut areas = Vec::new();
    // terminals it can't place in their tab: each alone, so the daemon
    // keeps no stale place for one
    let mut alone: Vec<Value> = Vec::new();
    let mut alone_terminals: Vec<&str> = Vec::new();
    let own_area = |id: &str, terminal: &str| json!({ "id": id, "terminals": [{ "terminal": terminal, "rect": [0.0, 0.0, 1.0, 1.0] }] });
    for tab in &tabs {
        let mut terminals = Vec::new();
        for p in &tab.panes {
            if doubled(p) {
                let t = p.terminal.unwrap_or_default();
                if !alone_terminals.contains(&t) {
                    alone_terminals.push(t);
                    alone.push(own_area(p.id, t));
                }
                if !p.px {
                    // its block takes room all the same, as a foreign pane
                    terminals.push(json!({ "terminal": p.id, "rect": p.rect, "inset": inset }));
                }
            } else if p.px {
                if let Some(t) = p.terminal {
                    alone.push(own_area(p.id, t));
                }
            } else {
                terminals.push(json!({ "terminal": p.reported(), "rect": p.rect, "inset": inset }));
            }
        }
        if !terminals.is_empty() {
            areas.push(json!({ "id": tab.id, "terminals": terminals }));
        }
    }
    areas.extend(alone);
    let mut report = json!({ "areas": areas });
    let focused = status.and_then(|s| s["focused_pane"].as_str());
    if let Some(focus) = every().find(|p| Some(p.id) == focused) {
        report["focus"] = focus.reported().into();
    }
    Ok(report)
}

/// The chrome in cells, columns and rows: the measured, at font size
/// `font`, unless `shown`, the shown tab's panes its cells can tell it
/// from, with their sizes as `listed` has them, solve it more than
/// [`TRUSTED`] away, and within 0 and twice the measured.
fn chrome(shown: &[Pane], listed: &[Terminal], font: f64) -> [u32; 2] {
    let size = |p: &Pane| {
        let t = listed
            .iter()
            .find(|t| Some(t.address.as_str()) == p.terminal)?;
        t.size.filter(|_| t.live)
    };
    [0, 1].map(|axis| {
        let measured = (MEASURED_CHROME[axis] * MEASURED_FONT / font).round();
        let points: Vec<(f64, f64)> = shown
            .iter()
            .filter_map(|p| Some((p.rect[2 + axis], size(p)?[axis])))
            .filter(|&(_, cells)| cells > 0)
            .map(|(share, cells)| (share, f64::from(cells)))
            .collect();
        let solved = solve(&points)
            .filter(|k| (k - measured).abs() > TRUSTED)
            .map(f64::round)
            .filter(|k| (0.0..=2.0 * measured).contains(k));
        solved.unwrap_or(measured) as u32
    })
}

/// The chrome, in cells, that `points`, each pane's share of its tab and
/// its cells along one axis, tell, unrounded: fitting cells = share ×
/// outer − chrome by least squares. None for fewer than two, or shares
/// that don't differ by [`SPREAD`].
fn solve(points: &[(f64, f64)]) -> Option<f64> {
    if points.len() < 2 {
        return None;
    }
    let shares = points.iter().map(|(s, _)| *s);
    let spread = shares.clone().fold(f64::MIN, f64::max) - shares.fold(f64::MAX, f64::min);
    if spread < SPREAD {
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
    Some(outer * mean_share - mean_cells)
}

/// Every tab of `doc` it can read, in the doc's order; none for a doc
/// never written. A doc without spaces or tabs, or a tab without an id,
/// can't be read; a tab whose tree can't be, such as one with a node of a
/// type it doesn't know, is left out, said on stderr.
fn tabs_of(doc: &Value) -> Result<Vec<Tab<'_>>, String> {
    if doc.is_null() {
        return Ok(Vec::new());
    }
    let mut tabs = Vec::new();
    for space in doc["spaces"].as_array().ok_or(UNREADABLE)? {
        for tab in space["tabs"].as_array().ok_or(UNREADABLE)? {
            let id = tab["id"].as_str().ok_or(UNREADABLE)?;
            let mut panes = Vec::new();
            match walk(
                &tab["root"],
                [0.0, 0.0, 1.0, 1.0],
                Under::default(),
                &mut panes,
            ) {
                Ok(()) => tabs.push(Tab { id, panes }),
                Err(why) => eprintln!("herdr-app report_layout: tab {id} left out: {why}"),
            }
        }
    }
    Ok(tabs)
}

/// What a node is under.
#[derive(Clone, Copy, Default)]
struct Under {
    /// A node sized in pixels.
    px: bool,
    /// A split with a node sized in pixels.
    approx: bool,
}

/// Adds the panes under `node`, `node` itself if it is one, at `rect`, to
/// `panes`; or why its tree can't be read.
fn walk<'a>(
    node: &'a Value,
    rect: [f64; 4],
    under: Under,
    panes: &mut Vec<Pane<'a>>,
) -> Result<(), String> {
    let axis = match node["type"].as_str() {
        Some("block") => {
            panes.push(Pane {
                id: node["id"].as_str().ok_or("a block has no id")?,
                terminal: node["content"]["terminal"].as_str(),
                rect,
                px: under.px,
                approx: under.approx,
            });
            return Ok(());
        }
        Some("row") => 0,
        Some("column") => 1,
        Some(other) => return Err(format!("unknown node type {other:?}")),
        None => return Err("a node has no type".into()),
    };
    let children = node["children"]
        .as_array()
        .ok_or("a split has no children")?;
    let weights = children
        .iter()
        .map(|child| weight(&child["size"]))
        .collect::<Result<Vec<_>, _>>()?;
    let beside_px = weights.iter().any(Option::is_none);
    let total: f64 = weights.iter().flatten().sum();
    let mut at = rect[axis];
    for (child, weight) in children.iter().zip(weights) {
        let Some(weight) = weight else {
            let px = Under { px: true, ..under };
            walk(child, rect, px, panes)?;
            continue;
        };
        let mut part = rect;
        part[axis] = at;
        part[2 + axis] = rect[2 + axis] * weight / total;
        at += part[2 + axis];
        let approx = Under {
            approx: under.approx || beside_px,
            ..under
        };
        walk(child, part, approx, panes)?;
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
        _ => Err(format!("a size of {size}")),
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

    /// A live terminal of `size`.
    fn term(address: &str, size: [u32; 2]) -> Terminal {
        Terminal {
            address: address.into(),
            live: true,
            pid: None,
            size: Some(size),
        }
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

    /// The chrome its cells tell wins over the measured when more than a
    /// cell away: at font size 28 the measured would be 3 columns and 2
    /// rows, and the areas from `t1` and `t2` would differ by 2 rows.
    #[test]
    fn chrome_solved_more_than_a_cell_away_wins_over_the_measured() {
        let (doc, listed) = shapes();
        let mut status = status();
        status["font_size"] = json!(28.0);
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        let inset = &report["areas"][0]["terminals"][0]["inset"];
        // the rows solved, 4, not the measured 2
        assert_eq!((&inset[1], &inset[3]), (&json!(2), &json!(2)));
        // the columns can't be solved, every share across being a half:
        // the measured 3
        assert_eq!((&inset[0], &inset[2]), (&json!(1), &json!(2)));
        assert_eq!(report["areas"][1]["terminals"][0]["inset"], *inset);
    }

    /// With the shapes tab shown and its real cells, the columns solve to
    /// 4.5, on a rounding tie, and the rows to 4: each within a cell of the
    /// measured, which stands, so rounding doesn't flip the inset.
    #[test]
    fn a_solve_within_a_cell_of_the_measured_keeps_the_measured() {
        let (doc, mut listed) = shapes();
        let mut status = status();
        status["tab"] = json!(SHAPES);
        let insets = |listed: &[Terminal]| {
            let report = layout(&doc, listed, Some(&status)).unwrap();
            let areas = report["areas"].as_array().unwrap().clone();
            let terminals = areas
                .iter()
                .flat_map(|a| a["terminals"].as_array().unwrap().clone());
            terminals.map(|t| t["inset"].clone()).collect::<Vec<_>>()
        };
        assert_eq!(insets(&listed), vec![json!(CHROME); 9]);
        // a cell less in t4, as rounding may give: the columns solve to 4
        listed[3].size = Some([37, 52]);
        assert_eq!(insets(&listed), vec![json!(CHROME); 9]);
    }

    /// `t2` zoomed in the nested tab: its terminal takes the whole tab,
    /// 82x52, which the doc doesn't show. The solve its cells give is far
    /// below 0, taken for cells it can't use: the measured stands, and the
    /// tab is reported unzoomed.
    #[test]
    fn a_zoomed_pane_is_reported_unzoomed_with_the_measured_chrome() {
        let exchanges = recorded("zoomed-doc-and-list");
        let reply = |method: &str| {
            let e = exchanges.iter().rev().find(|e| e.method == method).unwrap();
            e.replies[0]["result"].clone()
        };
        let doc = reply("doc.get")["value"].clone();
        let listed: Vec<Terminal> = reply("terminal.list")["terminals"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                term(
                    t["address"].as_str().unwrap(),
                    [
                        t["cols"].as_u64().unwrap() as u32,
                        t["rows"].as_u64().unwrap() as u32,
                    ],
                )
            })
            .collect();
        assert_eq!(listed[1].size, Some([82, 52]));
        let status: Value = serde_json::from_str(&recorded_run("status-zoomed").stdout).unwrap();
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        assert_eq!(
            report["areas"][0],
            json!({"id": NESTED, "terminals": [
                t("t1", [0.0, 0.0, 0.5, 1.0], CHROME),
                t("t2", [0.5, 0.0, 0.5, 0.5], CHROME),
                t("t3", [0.5, 0.5, 0.5, 0.5], CHROME),
            ]})
        );
        assert_eq!(report["focus"], "t2");
    }

    /// A terminal that isn't live, or has no cells along an axis, tells
    /// nothing: at font size 28, the measured 2 rows stand.
    #[test]
    fn terminals_not_live_or_with_no_cells_are_left_out_of_the_solve() {
        let (doc, listed) = shapes();
        let mut status = status();
        status["font_size"] = json!(28.0);
        let rows = |listed: &[Terminal]| {
            let report = layout(&doc, listed, Some(&status)).unwrap();
            report["areas"][0]["terminals"][0]["inset"][1].clone()
        };
        assert_eq!(rows(&listed), json!(2 /* of 4, solved */));
        let mut finished = listed.clone();
        finished[0].live = false;
        assert_eq!(rows(&finished), json!(1 /* of 2, measured */));
        let mut no_rows = listed.clone();
        no_rows[0].size = Some([39, 0]);
        assert_eq!(rows(&no_rows), json!(1));
    }

    /// A font size outside 4 to 200 is taken for a wrong one: 14.
    #[test]
    fn a_font_size_out_of_range_is_taken_as_the_measured() {
        let (doc, listed) = shapes();
        for font in [0.5, 1000.0, -14.0] {
            let mut status = status();
            status["font_size"] = json!(font);
            let report = layout(&doc, &listed, Some(&status)).unwrap();
            assert_eq!(report["areas"][0]["terminals"][0]["inset"], json!(CHROME));
        }
    }

    #[test]
    fn solve_fits_the_chrome_by_least_squares_given_shares_that_differ() {
        let near = |got: Option<f64>, want: f64| got.is_some_and(|k| (k - want).abs() < 1e-9);
        assert!(near(solve(&[(1.0, 52.0), (0.5, 24.0)]), 4.0));
        // three on one line
        assert!(near(solve(&[(1.0, 52.0), (0.5, 24.0), (0.25, 10.0)]), 4.0));
        // off the line: the fit
        assert!(near(solve(&[(1.0, 52.0), (0.5, 25.0), (0.5, 23.0)]), 4.0));
        // shares differing by just under a quarter tell nothing
        assert_eq!(solve(&[(0.5, 24.0), (0.26, 10.0)]), None);
        assert_eq!(solve(&[(1.0, 52.0)]), None);
        assert_eq!(solve(&[]), None);
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

    fn block(id: &str, content: Value, size: Value) -> Value {
        json!({"type": "block", "id": id, "content": content, "size": size})
    }

    /// A mod's pane is a foreign pane, by its pane id. A terminal under a
    /// node sized in pixels, its room unknown in shares, is in an area of
    /// its own, as is one two blocks show, whose blocks are foreign panes;
    /// so the daemon keeps no stale place for either. Focus is told over
    /// every block.
    #[test]
    fn mods_are_foreign_panes_and_terminals_it_cant_place_have_areas_of_their_own() {
        let doc = json!({"spaces": [{"tabs": [{"id": "tb1", "root": {
        "type": "row", "id": "tb1", "children": [
            block("pmod", json!({"mod": "files"}), json!({"weight": 1})),
            block("p1", json!({"terminal": "t1"}), json!({"weight": 3})),
            block("p2", json!({"terminal": "t2"}), json!({"px": 200})),
            {"type": "column", "id": "s1", "size": {"px": 100}, "children": [
                block("p3", json!({"terminal": "t3"}), Value::Null),
                block("pmod2", json!({"mod": "x"}), Value::Null),
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
        let alone = |id: &str, t: &str| json!({"id": id, "terminals": [{"terminal": t, "rect": [0.0, 0.0, 1.0, 1.0]}]});
        assert_eq!(
            report,
            json!({"areas": [
                {"id": "tb1", "terminals": [
                    t("pmod", [0.0, 0.0, 0.25, 1.0], CHROME),
                    t("t1", [0.25, 0.0, 0.75, 1.0], CHROME),
                ]},
                {"id": "tb2", "terminals": [
                    t("p4", [0.0, 0.0, 1.0, 0.5], CHROME),
                    t("p5", [0.0, 0.5, 1.0, 0.5], CHROME),
                ]},
                alone("p2", "t2"),
                alone("p3", "t3"),
                alone("p4", "t4"),
            ], "focus": "pmod"})
        );
        // focus on a block it couldn't place is its terminal all the same
        for (pane, terminal) in [("p3", "t3"), ("p5", "t4"), ("pmod2", "pmod2")] {
            status["focused_pane"] = json!(pane);
            let report = layout(&doc, &[], Some(&status)).unwrap();
            assert_eq!(report["focus"], terminal, "{pane}");
        }
    }

    /// `[t1 | (t2 / t3 / t5@px)]`: the pixels `t5` takes come off the
    /// column before its weights share it, so `t2` and `t3` are smaller
    /// than their rects: their cells are kept out of the solve, which they
    /// would make 8 rows.
    #[test]
    fn panes_beside_a_px_node_are_approximate_and_tell_no_chrome() {
        let doc = json!({"spaces": [{"tabs": [{"id": "tb1", "root": {
        "type": "row", "id": "tb1", "children": [
            block("p1", json!({"terminal": "t1"}), Value::Null),
            {"type": "column", "id": "s1", "children": [
                block("p2", json!({"terminal": "t2"}), Value::Null),
                block("p3", json!({"terminal": "t3"}), Value::Null),
                block("p5", json!({"terminal": "t5"}), json!({"px": 300})),
            ]},
        ]}}]}]});
        let listed = [
            term("t1", [39, 52]),
            term("t2", [39, 22]),
            term("t3", [39, 22]),
            term("t5", [39, 4]),
        ];
        let mut status = status();
        status["tab"] = json!("tb1");
        let report = layout(&doc, &listed, Some(&status)).unwrap();
        assert_eq!(
            report["areas"][0],
            json!({"id": "tb1", "terminals": [
                t("t1", [0.0, 0.0, 0.5, 1.0], CHROME),
                t("t2", [0.5, 0.0, 0.5, 0.5], CHROME),
                t("t3", [0.5, 0.5, 0.5, 0.5], CHROME),
            ]})
        );
        assert_eq!(report["areas"][1]["id"], "p5");
    }

    /// A tab with a node of a type it doesn't know, or a tree it can't
    /// read, is left out; the others are reported.
    #[test]
    fn a_tab_it_cant_read_is_left_out() {
        let tab = |id: &str, child: Value| json!({"id": id, "root": {"type": "row", "id": id, "children": [child]}});
        let ok = tab("tb0", block("p0", json!({"terminal": "t0"}), Value::Null));
        for bad in [
            json!({"type": "stack", "id": "s9", "children": []}),
            block("p1", json!({"terminal": "t1"}), json!({"weight": -1})),
            json!({"type": "block", "content": {"terminal": "t1"}}),
            json!({"type": "row", "id": "s9"}),
        ] {
            let doc = json!({"spaces": [{"tabs": [ok.clone(), tab("tb1", bad.clone())]}]});
            let report = layout(&doc, &[], None).unwrap();
            let ids: Vec<&Value> = report["areas"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| &a["id"])
                .collect();
            assert_eq!(ids, [&json!("tb0")], "{bad}");
        }
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
    fn a_doc_without_spaces_or_tabs_or_a_tab_without_an_id_fails_the_report() {
        for bad in [
            json!({"spaces": 3}),
            json!({"spaces": [{"tabs": {}}]}),
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
