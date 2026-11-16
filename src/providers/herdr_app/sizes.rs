//! Sizes in the Herdr app are weights (ADR 0020): a node of the core's doc
//! has a `size`, `{weight}` relative to its siblings, `{px}`, or none,
//! counted as weight 1; `herdr-app do layout.resize {id, weight}` sets
//! it. Weights share the panes' outer extent, chrome included, which the
//! core's cols and rows leave out.
//!
//! - `resize`, in `report_layout`'s model (`layout.rs`): a pane's outer
//!   extent is its share of its tab, from the weights, times the tab's,
//!   and its content that less the chrome. The doc's lowest split holding
//!   `TERMINAL`'s pane and `ORIGIN`'s, which must be split along
//!   `DIRECTION`, gets weights on the pair of its children holding each
//!   that give the terminal's part `SIZE` cells and the chrome of each
//!   pane along it, out of the pair's outer extent: its share of the tab
//!   times the tab's extent, which [`measure_window`] tells from the shown
//!   tab's cells, as every tab of the window has the same room, and with
//!   the chrome as `report_layout` takes it. So `ORIGIN`'s part comes
//!   short of Neovim's by about a chrome per pane along it, as herdr's
//!   frame does. The pair's ratio is set: in a split of two, by one
//!   `layout.resize` of the terminal's part against the other's; in one
//!   of three or more, by one of each, keeping their sum, so the other
//!   children keep their room, unless the second fails after the first
//!   is set. A part sized in pixels, or a node sized in pixels in a split
//!   along the axis over the pair, exits 1, resizing nothing: the pair's
//!   share can't be told. A zoomed pane of the shown tab, as the app's
//!   `view.json` says, fills it, so it alone measures the window.
//! - `open_beside`, after its split ([`share_by`]): the new pane gets
//!   `RATIO` of the weight it and the origin's pane have, chrome ignored.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::core::Core;
use super::doc::pane_of;
use super::layout::{measure_window, weight};
use super::window::Window;
use super::{Failure, act, methods, needs, refused, window_of};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `resize` has, of its 2 s.
const RESIZE_TIME: Duration = Duration::from_millis(1500);

/// The places a weight keeps: the app reads it as an f32.
const WEIGHT_SCALE: f64 = 1e6;

/// `resize`: sets the weights that give `TERMINAL` `SIZE` cells beside
/// `ORIGIN`.
pub(super) fn resize(given: &Given) -> Result<(), Failure> {
    resize_within(given, RESIZE_TIME)
}

/// `resize`, its requests having `time`.
fn resize_within(given: &Given, time: Duration) -> Result<(), Failure> {
    let terminal = given.text("terminal")?;
    let origin = given.text("origin")?;
    let axis = Axis::of(given)?;
    let size = given.cells("size")?;
    if terminal == origin {
        return Err(Failure::Failed(format!(
            "TERMINAL and ORIGIN are both {terminal}"
        )));
    }
    // found first: no herdr-app costs no request of the core
    let window = window_of(given)?;
    let mut core = Core::of(given, time)?;
    needs(&core, methods(Capability::Resize), || Ok(window.clone()))?;

    let doc = core.doc()?;
    let pair = Pair::of(&doc, &terminal, &origin)?;
    if pair.split["type"] != axis.kind() {
        return Err(Failure::Failed(format!(
            "{terminal} and {origin} aren't {}",
            axis.way()
        )));
    }
    // sized in pixels: nothing to ask the core or the window for
    let tab_share = pair.share_of_tab(axis)?;
    let status = window
        .status(core.due())
        .map_err(|ran| match refused(ran, &core, &window) {
            Failure::Failed(why) => {
                Failure::Failed(format!("can't measure the Herdr app window: {why}"))
            }
            lacking => lacking,
        })?;
    let shown = status["tab"]
        .as_str()
        .ok_or_else(|| "herdr-app status names no tab shown".to_string())?;
    let zoomed = zoomed(given, shown)?;
    let listed = core.list()?;
    let measure = measure_window(&doc, &listed, &status, zoomed.as_deref())?;
    let extent = measure.extent[axis.index()].ok_or_else(|| {
        "the shown Herdr app tab has no cells to measure the window by".to_string()
    })?;
    let chrome = f64::from(measure.chrome[axis.index()]);
    let share = pair
        .fit(axis, extent * tab_share, chrome, size)
        .ok_or_else(|| format!("{terminal} and {origin} have no room to share"))?;
    for (id, weight) in pair.settings(share)? {
        act(
            &window,
            &core,
            "layout.resize",
            &json!({ "id": id, "weight": weight }),
        )?;
    }
    Ok(())
}

/// The pane tab `tab` is zoomed on, as the app's `view.json` under its
/// home says, `tabs.<tab>.zoomed`; None where it isn't, or the app has
/// written no `view.json`; or why it can't be read.
fn zoomed(given: &Given, tab: &str) -> Result<Option<String>, String> {
    let home = match given.env("HERDR_APP_HOME").filter(|h| !h.is_empty()) {
        Some(home) => PathBuf::from(OsStr::from_bytes(&home)),
        None => {
            let home = given.env("HOME").unwrap_or_default();
            PathBuf::from(OsStr::from_bytes(&home)).join(".config/herdr-app")
        }
    };
    let file = home.join("view.json");
    let text = match std::fs::read(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("can't read {}: {e}", file.display())),
    };
    let view: Value =
        serde_json::from_slice(&text).map_err(|e| format!("can't read {}: {e}", file.display()))?;
    Ok(view["tabs"][tab]["zoomed"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(String::from))
}

/// Gives terminal `terminal`'s pane `share` of the weight it and
/// `origin`'s have together, as `doc` shows them, by `layout.resize` before
/// `due`; or why not.
pub(super) fn share_by(
    window: &Window,
    doc: &Value,
    terminal: &str,
    origin: &str,
    share: f64,
    due: Instant,
) -> Result<(), String> {
    let pair = Pair::of(doc, terminal, origin)?;
    for (id, weight) in pair.settings(share)? {
        let args = json!({ "id": id, "weight": weight });
        window.act("layout.resize", &args, due)?;
    }
    Ok(())
}

/// The way a split lays out its children.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Axis {
    /// Side by side, a `row`: sized in cols.
    Row,
    /// Stacked, a `column`: sized in rows.
    Column,
}

impl Axis {
    /// `DIRECTION`'s: left and right are side by side.
    fn of(given: &Given) -> Result<Axis, String> {
        let direction = given.var("direction").unwrap_or_default();
        match direction.as_slice() {
            b"left" | b"right" => Ok(Axis::Row),
            b"up" | b"down" => Ok(Axis::Column),
            other => Err(format!(
                "DIRECTION {:?} isn't left, right, up or down",
                String::from_utf8_lossy(other)
            )),
        }
    }

    /// The doc's `type` of a split along it.
    fn kind(self) -> &'static str {
        match self {
            Axis::Row => "row",
            Axis::Column => "column",
        }
    }

    fn way(self) -> &'static str {
        match self {
            Axis::Row => "side by side",
            Axis::Column => "stacked",
        }
    }

    /// Its place in a `[cols, rows]`.
    fn index(self) -> usize {
        match self {
            Axis::Row => 0,
            Axis::Column => 1,
        }
    }
}

/// Node `node`'s weight, as `report_layout` reads it; or why it has
/// none: it is sized in pixels, or its size can't be read.
fn weight_of(node: &Value) -> Result<f64, String> {
    let id = node["id"].as_str().unwrap_or("?");
    match weight(&node["size"]) {
        Ok(Some(w)) => Ok(w),
        Ok(None) => Err(format!("Herdr app node {id} is sized in pixels")),
        Err(why) => Err(format!("Herdr app node {id} has {why}")),
    }
}

/// The lowest split of the doc holding two panes, and its children
/// holding each: the terminal's part and the origin's.
struct Pair<'d> {
    /// The nodes from its tab's root down to the split, both included.
    path: Vec<&'d Value>,
    split: &'d Value,
    part: &'d Value,
    other: &'d Value,
}

impl<'d> Pair<'d> {
    /// The pair of terminal `terminal` and terminal `origin` in `doc`, each
    /// shown in one block of one tab; or why not.
    fn of(doc: &'d Value, terminal: &str, origin: &str) -> Result<Pair<'d>, String> {
        let pane = pane_of(doc, terminal)?;
        let origin_pane = pane_of(doc, origin)?;
        let tabs = doc["spaces"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|space| space["tabs"].as_array().into_iter().flatten());
        for tab in tabs {
            let (Some(to_pane), Some(to_origin)) = (
                path_to(&tab["root"], &pane),
                path_to(&tab["root"], &origin_pane),
            ) else {
                continue;
            };
            let depth = to_pane
                .iter()
                .zip(&to_origin)
                .take_while(|(a, b)| std::ptr::eq(**a, **b))
                .count();
            let (Some(part), Some(other)) = (to_pane.get(depth), to_origin.get(depth)) else {
                return Err(format!("{terminal} and {origin} share a Herdr app pane"));
            };
            return Ok(Pair {
                path: to_pane[..depth].to_vec(),
                split: to_pane[depth - 1],
                part,
                other,
            });
        }
        Err(format!(
            "{terminal} and {origin} aren't in one Herdr app tab"
        ))
    }

    /// The weights of its part and the other, or why they have none.
    fn weights(&self) -> Result<(f64, f64), String> {
        Ok((weight_of(self.part)?, weight_of(self.other)?))
    }

    /// Its outer share of its tab along `axis`, by the weights of every
    /// split along it over the pair, the pair's split too; or why it
    /// can't be told: a node sized in pixels among them, whose room their
    /// weights don't share.
    fn share_of_tab(&self, axis: Axis) -> Result<f64, String> {
        let (part, other) = self.weights()?;
        let mut share = 1.0;
        let held = self.path.iter().skip(1).copied().chain([self.part]);
        for (node, child) in self.path.iter().zip(held) {
            if node["type"] != axis.kind() {
                continue;
            }
            let mut total = 0.0;
            for c in node["children"].as_array().into_iter().flatten() {
                total += weight_of(c)?;
            }
            let held = if std::ptr::eq(*node, self.split) {
                part + other
            } else {
                weight_of(child)?
            };
            share *= held / total;
        }
        Ok(share)
    }

    /// The share of the pair's weight its part takes to have `size` cells
    /// and `chrome` for each pane along `axis`, out of the pair's `outer`
    /// extent, held to leave each part its chrome and a cell; None if
    /// there's no room for that.
    fn fit(&self, axis: Axis, outer: f64, chrome: f64, size: u32) -> Option<f64> {
        let chrome = |node| chrome * panes_along(node, axis) as f64;
        let (part_chrome, other_chrome) = (chrome(self.part), chrome(self.other));
        let (least, most) = (part_chrome + 1.0, outer - other_chrome - 1.0);
        if least > most || most.is_nan() {
            return None;
        }
        let wanted = f64::from(size) + part_chrome;
        Some(wanted.clamp(least, most) / outer)
    }

    /// The `layout.resize`s that give its part `share` of the weight it
    /// and the other have, each node's id and weight; none for a weight
    /// already so. Their ratio is what's set: in a split of two, by the
    /// part's weight alone; in one of more, by both, keeping their sum so
    /// the other children keep their room, once both are set: the part's
    /// first, so if the other's then fails, the sum isn't kept. `share`
    /// must be strictly between 0 and 1.
    fn settings(&self, share: f64) -> Result<Vec<(String, f64)>, String> {
        if !(share > 0.0 && share < 1.0) {
            return Err(format!("a share of {share} isn't between 0 and 1"));
        }
        let (part, other) = self.weights()?;
        let children = self.split["children"].as_array().map_or(0, Vec::len);
        let wanted = if children == 2 {
            // only the ratio counts: the part against the other
            vec![(self.part, part, other * share / (1.0 - share))]
        } else {
            let sum = part + other;
            vec![
                (self.part, part, sum * share),
                (self.other, other, sum * (1.0 - share)),
            ]
        };
        Ok(wanted
            .into_iter()
            .map(|(node, now, weight)| {
                let weight =
                    ((weight * WEIGHT_SCALE).round() / WEIGHT_SCALE).max(1.0 / WEIGHT_SCALE);
                (node, now, weight)
            })
            .filter(|(_, now, weight)| (now - weight).abs() >= 0.5 / WEIGHT_SCALE)
            .map(|(node, _, weight)| (node["id"].as_str().unwrap_or_default().to_string(), weight))
            .collect())
    }
}

/// The nodes from `node` down to block `pane`, both included, if it's
/// under `node`.
fn path_to<'d>(node: &'d Value, pane: &str) -> Option<Vec<&'d Value>> {
    if node["type"] == "block" {
        return (node["id"] == pane).then(|| vec![node]);
    }
    for child in node["children"].as_array().into_iter().flatten() {
        if let Some(mut path) = path_to(child, pane) {
            path.insert(0, node);
            return Some(path);
        }
    }
    None
}

/// The panes along `axis` in `node`, each with its chrome: one for a
/// pane, a split's sum along it, else its most of a child.
fn panes_along(node: &Value, axis: Axis) -> usize {
    let children = node["children"].as_array().into_iter().flatten();
    let counts = children.map(|c| panes_along(c, axis));
    let n = if node["type"] == axis.kind() {
        counts.sum()
    } else {
        counts.max().unwrap_or(0)
    };
    n.max(1)
}

#[cfg(test)]
mod tests {
    use super::super::fake::{FakeApp, FakeCore, Run, given, recorded, recorded_one, recorded_run};
    use super::*;

    /// The time a test's resize has: well over [`RESIZE_TIME`], as the
    /// fakes run shell scripts on a loaded machine.
    const TIME: Duration = Duration::from_secs(5);

    /// The nested tab's panes: `t1`'s, `t2`'s and `t3`'s.
    const P1: &str = "pf7e3d563947903ed7a8c93ee0ac58181";
    const P2: &str = "p7b3398d57b6dd529d0f978c04b35c687";
    const P3: &str = "p4c229a9b57964c201d7ab577ccf4a2ea";
    /// The shapes tab's: `t4`'s, `t5`'s, and the column holding `t6`.
    const P4: &str = "pbdfcba84288586794b7f1b61d52a8b4a";
    const P5: &str = "p6167ba561752977dbf43138259612244";
    const S6: &str = "s97cb6df4a068ddeecf30d45c2c51a536";
    /// The shapes tab's `t6`'s and `t7`'s.
    const P6: &str = "p598cb60790ec64f029adc9e4ca9e793c";
    const P7: &str = "p133af3faf61769d75ada6f797dcdbdb7";
    /// The nested tab, which `status` shows, and the shapes tab.
    const NESTED_TAB: &str = "tb14c28a7769ba1044103fd9584cc818f0";
    const SHAPES_TAB: &str = "tb647b2d25c9734a2636b21f8d1a7c8c1c";

    /// A recorded `layout.resize`, answering `{"id":ID,"weight":WEIGHT}`.
    fn resized(id: &str, weight: &str) -> Run {
        let mut run = recorded_run("do-layout.resize");
        run.args = vec![
            "do".into(),
            "layout.resize".into(),
            format!(r#"{{"id":"{id}","weight":{weight}}}"#),
        ];
        run
    }

    fn resized_line(id: &str, weight: &str) -> String {
        format!(r#"do layout.resize {{"id":"{id}","weight":{weight}}}"#)
    }

    /// The nested tab `[t1@0.5 | (t2 / t3)@0.5]`, shown, its cells 39×52,
    /// 39×24, 39×24: the chrome 5 by 4, so a tab is 88 by 56, outer.
    fn nested() -> FakeCore {
        FakeCore::replaying(&["nested-doc.get", "nested-terminal.list"])
    }

    /// The nested tab beside the shapes tab
    /// `[t4@0.5 | t5@0.25 | (t6 / t7@0.5 / [t8 | t9]@0.5)@0.25]`, with
    /// every terminal's cells.
    fn shapes() -> FakeCore {
        FakeCore::replaying(&["shapes-doc-and-list"])
    }

    /// The shapes core, node `id` sized `size`.
    fn shapes_sized(id: &str, size: Value) -> FakeCore {
        fn set(node: &mut Value, id: &str, size: &Value) {
            if node["id"] == id {
                node["size"] = size.clone();
            }
            if let Some(children) = node.get_mut("children").and_then(Value::as_array_mut) {
                children.iter_mut().for_each(|c| set(c, id, size));
            }
        }
        let mut exchanges = recorded("shapes-doc-and-list");
        for e in exchanges.iter_mut().filter(|e| e.method == "doc.get") {
            for space in e.result_mut()["value"]["spaces"].as_array_mut().unwrap() {
                for tab in space["tabs"].as_array_mut().unwrap() {
                    set(&mut tab["root"], id, &size);
                }
            }
        }
        FakeCore::serve(exchanges)
    }

    /// The nested core, its `terminal.list` changed by `edit`.
    fn nested_listing(edit: impl Fn(&mut Value)) -> FakeCore {
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(recorded_one("nested-terminal.list", "terminal.list"));
        edit(exchanges[2].result_mut());
        FakeCore::serve(exchanges)
    }

    /// The nested core, its doc changed by `edit`.
    fn nested_doc(edit: impl Fn(&mut Value)) -> FakeCore {
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(recorded_one("nested-terminal.list", "terminal.list"));
        edit(&mut exchanges[1].result_mut()["value"]);
        FakeCore::serve(exchanges)
    }

    /// `status`, its font size `size`.
    fn status_at(size: f64) -> Run {
        let mut run = recorded_run("status");
        run.stdout = run
            .stdout
            .replace("\"font_size\": 14.0", &format!("\"font_size\": {size:?}"));
        assert!(run.stdout.contains(&format!("{size:?}")));
        run
    }

    /// `given`, its window's home `home`.
    fn at_home(given: Given, home: &std::path::Path) -> Given {
        let home = home.as_os_str().as_bytes().to_vec();
        Given::from(move |name| match name {
            "HERDR_APP_HOME" => Some(home.clone()),
            other => given.env(other),
        })
    }

    fn vars<'a>(
        terminal: &'a str,
        origin: &'a str,
        direction: &'a str,
        size: &'a str,
    ) -> [(&'static str, &'a str); 4] {
        [
            ("TERMINAL", terminal),
            ("ORIGIN", origin),
            ("DIRECTION", direction),
            ("SIZE", size),
        ]
    }

    fn resize(
        core: &FakeCore,
        app: &FakeApp,
        terminal: &str,
        origin: &str,
        direction: &str,
        size: &str,
    ) -> Result<(), Failure> {
        let vars = vars(terminal, origin, direction, size);
        resize_within(&given(core, app, &vars), TIME)
    }

    #[test]
    fn in_a_split_of_two_the_terminals_part_alone_gets_its_cells_and_chrome() {
        // t1 and the column share the tab's 88 columns; 30 cells and 5
        // of chrome are 35 of them, so t1's weight is the column's
        // 0.5 × 35 / 53
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P1, "0.330189")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(
            app.asked(),
            ["status".to_string(), resized_line(P1, "0.330189")]
        );
        assert_eq!(core.methods(), ["hello", "doc.get", "terminal.list"]);
    }

    #[test]
    fn a_hidden_tab_is_measured_by_the_shown_one_and_both_parts_keep_their_sum() {
        // the shapes tab is hidden; the shown tab says a tab is 88 wide.
        // t5 and t4 have 0.75 of it, 66: t5 gets 35 / 66 of their 0.75,
        // t4 31 / 66, and the column keeps its 0.25
        let core = shapes();
        let app = FakeApp::new(&[
            recorded_run("status"),
            resized(P5, "0.397727"),
            resized(P4, "0.352273"),
        ]);
        assert_eq!(resize(&core, &app, "t5", "t4", "right", "30"), Ok(()));
        assert_eq!(
            app.asked()[1..],
            [resized_line(P5, "0.397727"), resized_line(P4, "0.352273")]
        );
    }

    #[test]
    fn a_part_of_several_panes_along_has_the_chrome_of_each() {
        // t6's column right of t4: [t8 | t9] puts two panes along it, so
        // 20 cells want 30 of the pair's 66
        let core = shapes();
        let app = FakeApp::new(&[
            recorded_run("status"),
            resized(S6, "0.340909"),
            resized(P4, "0.409091"),
        ]);
        assert_eq!(resize(&core, &app, "t6", "t4", "right", "20"), Ok(()));
        assert_eq!(
            app.asked()[1..],
            [resized_line(S6, "0.340909"), resized_line(P4, "0.409091")]
        );
    }

    #[test]
    fn a_missing_size_is_weight_one() {
        // t3 below t2, neither sized, the column the tab's 56 rows: 30
        // rows want 34 of them, and t3's weight is t2's 1 × 34 / 22
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P3, "1.545455")]);
        assert_eq!(resize(&core, &app, "t3", "t2", "down", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P3, "1.545455")]);
    }

    #[test]
    fn the_chrome_is_report_layouts() {
        // at font 28 the measured chrome is 3 columns (2.5, rounded),
        // which the shown tab's cells can't tell another of, its panes all
        // half its width: a tab is 42 × 3 ÷ 1.5 = 84, 30 cells want 33,
        // and t1 gets 0.5 × 33 / 51
        let core = nested();
        let app = FakeApp::new(&[status_at(28.0), resized(P1, "0.323529")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P1, "0.323529")]);
        // its rows tell 4 over the measured 2: as at font 14
        let core = nested();
        let app = FakeApp::new(&[status_at(28.0), resized(P3, "1.545455")]);
        assert_eq!(resize(&core, &app, "t3", "t2", "down", "30"), Ok(()));
    }

    #[test]
    fn a_size_past_the_room_leaves_the_other_its_chrome_and_a_cell() {
        // 56 rows: t2 keeps 4 + 1, t3 gets 51, 1 × 51 / 5
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P3, "10.2")]);
        assert_eq!(resize(&core, &app, "t3", "t2", "down", "500"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P3, "10.2")]);
    }

    #[test]
    fn a_weight_already_so_is_left_alone() {
        // 24 rows of 56 is half, which t3 has
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(resize(&core, &app, "t3", "t2", "down", "24"), Ok(()));
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn too_little_room_for_both_chromes_resizes_nothing() {
        // at font 5 the chrome is 14 columns: a tab is 106, t8 and t9's
        // row a quarter of it, 26.5, short of 15 and 15
        let core = shapes();
        let app = FakeApp::new(&[status_at(5.0)]);
        assert_eq!(
            resize(&core, &app, "t9", "t8", "right", "3"),
            Err(Failure::Failed("t9 and t8 have no room to share".into()))
        );
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn a_shown_tab_with_no_cells_resizes_nothing() {
        let core = nested_listing(|list| list["terminals"] = json!([]));
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t1", "t2", "left", "30"),
            Err(Failure::Failed(
                "the shown Herdr app tab has no cells to measure the window by".into()
            ))
        );
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn a_zoomed_pane_of_the_shown_tab_alone_measures_the_window() {
        // recorded with t2 zoomed: 82 × 52, the whole tab's, the others'
        // cells unchanged; a tab is 82 + 5 wide, and 30 cells want 35 of
        // t1 and the column's 87, t1 0.5 × 35 / 52
        let vars = vars("t1", "t2", "left", "30");
        let zoomed = || FakeCore::replaying(&["zoomed-doc-and-list"]);
        let core = zoomed();
        let app = FakeApp::new(&[recorded_run("status-zoomed"), resized(P1, "0.336538")]);
        let view = json!({ "tabs": { NESTED_TAB: { "zoomed": P2, "focused": P2 } } });
        std::fs::write(app.dir().join("view.json"), view.to_string()).unwrap();
        let at = at_home(given(&core, &app, &vars), app.dir());
        assert_eq!(resize_within(&at, TIME), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P1, "0.336538")]);

        // unknown, t1 and the zoomed t2 would be taken for a line across
        let core = zoomed();
        let app = FakeApp::new(&[recorded_run("status-zoomed"), resized(P1, "0.336538")]);
        let view =
            json!({ "tabs": { NESTED_TAB: { "zoomed": null }, SHAPES_TAB: { "zoomed": P4 } } });
        std::fs::write(app.dir().join("view.json"), view.to_string()).unwrap();
        let at = at_home(given(&core, &app, &vars), app.dir());
        assert!(resize_within(&at, TIME).is_err());
        assert_ne!(app.asked()[1], resized_line(P1, "0.336538"));

        // a view.json that can't be read
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status")]);
        std::fs::write(app.dir().join("view.json"), "{").unwrap();
        let at = at_home(given(&core, &app, &vars), app.dir());
        let Err(Failure::Failed(why)) = resize_within(&at, TIME) else {
            panic!("resized");
        };
        assert!(
            why.starts_with("can't read ") && why.contains("view.json"),
            "{why}"
        );
    }

    #[test]
    fn cells_laid_out_by_the_weights_before_still_measure_the_tab() {
        // the doc already has t1@0.3 and the column@0.7, from a resize
        // just before, but the cells are still 39 and 39: a line across
        // the tab still crosses 44 + 44, so t1 is the column's
        // 0.7 × 35 / 53
        let core = nested_doc(|doc| {
            let root = &mut doc["spaces"][0]["tabs"][0]["root"];
            root["children"][0]["size"] = json!({ "weight": 0.3 });
            root["children"][1]["size"] = json!({ "weight": 0.7 });
        });
        let app = FakeApp::new(&[recorded_run("status"), resized(P1, "0.462264")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P1, "0.462264")]);
    }

    #[test]
    fn a_node_sized_in_pixels_across_the_axis_over_the_pair_is_no_matter() {
        // t5 in pixels beside the column; t6 and t7 are stacked in it, of
        // its whole height, 56 rows, at 1 and 0.5 of 2: 42 rows; 20 want
        // 24, t6 1.5 × 24 / 42, t7 1.5 × 18 / 42
        let core = shapes_sized(P5, json!({ "px": 300 }));
        let app = FakeApp::new(&[
            recorded_run("status"),
            resized(P6, "0.857143"),
            resized(P7, "0.642857"),
        ]);
        assert_eq!(resize(&core, &app, "t6", "t7", "down", "20"), Ok(()));
        assert_eq!(
            app.asked()[1..],
            [resized_line(P6, "0.857143"), resized_line(P7, "0.642857")]
        );
    }

    #[test]
    fn a_third_sibling_sized_in_pixels_resizes_nothing() {
        let core = shapes_sized(S6, json!({ "px": 300 }));
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t5", "t4", "right", "30"),
            Err(Failure::Failed(format!(
                "Herdr app node {S6} is sized in pixels"
            )))
        );
        assert!(app.asked().is_empty());
    }

    #[test]
    fn a_weight_that_cant_be_read_resizes_nothing() {
        let core = shapes_sized(P5, json!({ "weight": -1 }));
        let app = FakeApp::new(&[]);
        assert_eq!(
            resize(&core, &app, "t5", "t4", "right", "30"),
            Err(Failure::Failed(format!(
                "Herdr app node {P5} has a size of {{\"weight\":-1}}"
            )))
        );
    }

    #[test]
    fn a_second_resize_that_fails_leaves_the_first_and_the_sum_not_kept() {
        let mut refused = resized(P4, "0.352273");
        refused.exit = 1;
        refused.stdout = recorded_run("err-layout.resize-unknown")
            .stdout
            .replace("pnope", P4);
        let core = shapes();
        let app = FakeApp::new(&[recorded_run("status"), resized(P5, "0.397727"), refused]);
        assert_eq!(
            resize(&core, &app, "t5", "t4", "right", "30"),
            Err(Failure::Failed(format!("herdr-app: no node \"{P4}\"")))
        );
        assert_eq!(
            app.asked()[1..],
            [resized_line(P5, "0.397727"), resized_line(P4, "0.352273")]
        );
    }

    #[test]
    fn a_status_naming_no_tab_or_unknown_resizes_nothing() {
        let mut no_tab = recorded_run("status");
        no_tab.stdout = no_tab
            .stdout
            .lines()
            .filter(|l| !l.contains("\"tab\""))
            .collect::<Vec<_>>()
            .join("\n");
        let app = FakeApp::new(&[no_tab]);
        assert_eq!(
            resize(&nested(), &app, "t3", "t2", "down", "30"),
            Err(Failure::Failed(
                "herdr-app status names no tab shown".into()
            ))
        );

        let mut unknown = recorded_run("err-unknown-command");
        unknown.args = vec!["status".into()];
        unknown.stdout = unknown.stdout.replace("no.such", "status");
        let app = FakeApp::new(&[unknown, recorded_run("version")]);
        assert_eq!(
            resize(&nested(), &app, "t3", "t2", "down", "30"),
            Err(Failure::Unsupported(
                "herdr-app has no status (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
    }

    #[test]
    fn a_window_that_cant_say_what_it_shows_resizes_nothing() {
        let core = nested();
        let app = FakeApp::new(&[recorded_run("err-no-window-status")]);
        let Err(Failure::Failed(why)) = resize(&core, &app, "t3", "t2", "down", "30") else {
            panic!("resized");
        };
        assert!(
            why.starts_with("can't measure the Herdr app window: herdr-app: no Herdr window"),
            "{why}"
        );
        assert_eq!(core.methods(), ["hello", "doc.get"]);
    }

    #[test]
    fn a_part_sized_in_pixels_or_beside_one_over_it_resizes_nothing() {
        for id in [P5, P4] {
            let core = shapes_sized(id, json!({ "px": 300 }));
            let app = FakeApp::new(&[recorded_run("status")]);
            assert_eq!(
                resize(&core, &app, "t5", "t4", "right", "30"),
                Err(Failure::Failed(format!(
                    "Herdr app node {id} is sized in pixels"
                )))
            );
            assert_eq!(core.methods(), ["hello", "doc.get"]);
            assert!(app.asked().is_empty());
        }
        // t8 and t9's row is in the column beside t5: their share of the
        // tab's width can't be told
        let core = shapes_sized(P5, json!({ "px": 300 }));
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t9", "t8", "right", "3"),
            Err(Failure::Failed(format!(
                "Herdr app node {P5} is sized in pixels"
            )))
        );
        assert!(app.asked().is_empty());
    }

    #[test]
    fn panes_not_split_that_way_or_not_in_one_tab_resize_nothing() {
        for (terminal, origin, direction, why) in [
            ("t3", "t2", "right", "t3 and t2 aren't side by side"),
            ("t5", "t4", "up", "t5 and t4 aren't stacked"),
            ("t1", "t4", "left", "t1 and t4 aren't in one Herdr app tab"),
            ("t1", "t99", "left", "t99 has no Herdr app pane"),
        ] {
            let core = shapes();
            let app = FakeApp::new(&[]);
            assert_eq!(
                resize(&core, &app, terminal, origin, direction, "10"),
                Err(Failure::Failed(why.into()))
            );
            assert_eq!(core.methods(), ["hello", "doc.get"]);
            assert!(app.asked().is_empty());
        }
    }

    #[test]
    fn a_refused_resize_fails_with_the_apps_words() {
        let mut refused = resized(P3, "1.545455");
        refused.exit = 1;
        refused.stdout = recorded_run("err-layout.resize-unknown")
            .stdout
            .replace("pnope", P3);
        let app = FakeApp::new(&[recorded_run("status"), refused]);
        assert_eq!(
            resize(&nested(), &app, "t3", "t2", "down", "30"),
            Err(Failure::Failed(format!("herdr-app: no node \"{P3}\"")))
        );
    }

    #[test]
    fn a_core_or_app_lacking_what_it_calls_is_unsupported_naming_both_versions() {
        for method in ["doc.get", "terminal.list"] {
            let mut hello = recorded_one("hello", "hello");
            let methods = hello.result_mut()["methods"].as_array_mut().unwrap();
            methods.retain(|m| m != method);
            let core = FakeCore::serve(vec![hello]);
            let app = FakeApp::new(&[recorded_run("version")]);
            assert_eq!(
                resize(&core, &app, "t3", "t2", "down", "30"),
                Err(Failure::Unsupported(format!(
                    "the hc core has no {method} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
            assert_eq!(core.methods(), ["hello"]);
        }

        let mut unknown = resized(P3, "1.545455");
        unknown.exit = 1;
        unknown.stdout = recorded_run("err-unknown-command")
            .stdout
            .replace("no.such", "layout.resize");
        let mut no_weight = unknown.clone();
        no_weight.stdout = recorded_run("err-unknown-argument")
            .stdout
            .replace("pane.split", "layout.resize")
            .replace("bogus", "weight");
        for (run, why) in [
            (unknown, "herdr-app has no layout.resize"),
            (no_weight, "herdr-app's layout.resize takes no weight"),
        ] {
            let app = FakeApp::new(&[recorded_run("status"), run, recorded_run("version")]);
            assert_eq!(
                resize(&nested(), &app, "t3", "t2", "down", "30"),
                Err(Failure::Unsupported(format!(
                    "{why} (hc core 0.1.21, herdr-app 0.1.16)"
                )))
            );
        }
    }

    #[test]
    fn bad_inputs_ask_nothing() {
        let (core, app) = (nested(), FakeApp::new(&[]));
        for (terminal, origin, direction, size, why) in [
            ("t3", "t2", "down", "0", "SIZE isn't a count of cells"),
            ("t3", "t2", "down", "x", "SIZE \"x\" isn't an integer"),
            (
                "t3",
                "t2",
                "sideways",
                "30",
                "DIRECTION \"sideways\" isn't left, right, up or down",
            ),
            ("t3", "t3", "down", "30", "TERMINAL and ORIGIN are both t3"),
            ("", "t2", "down", "30", "TERMINAL isn't set"),
        ] {
            assert_eq!(
                resize(&core, &app, terminal, origin, direction, size),
                Err(Failure::Failed(why.into()))
            );
        }
        assert!(core.asked().is_empty());
        assert!(app.asked().is_empty());
    }

    #[test]
    fn a_share_not_strictly_between_0_and_1_sets_nothing() {
        let doc = recorded_one("nested-doc.get", "doc.get").replies[0]["result"]["value"].clone();
        let app = FakeApp::new(&[]);
        let window = Window::faked(&app.program());
        let due = Instant::now() + TIME;
        for share in [0.0, 1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                share_by(&window, &doc, "t3", "t2", share, due),
                Err(format!("a share of {share} isn't between 0 and 1"))
            );
        }
        assert!(app.asked().is_empty());
    }
}
