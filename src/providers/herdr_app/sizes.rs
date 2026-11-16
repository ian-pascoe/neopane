//! Sizes in the Herdr app are weights (ADR 0020): a node of the core's doc
//! has a `size`, `{weight}` relative to its siblings, `{px}`, or none,
//! counted as weight 1; `herdr-app do layout.resize {id, weight}` sets
//! it. Weights share the panes' outer extent, chrome included, which the
//! core's cols and rows leave out.
//!
//! - `resize`: the doc's lowest split holding `TERMINAL`'s pane and
//!   `ORIGIN`'s, which must be split along `DIRECTION`, gets weights on
//!   the pair of its children holding each that give the terminal's part
//!   `SIZE` cells and the chrome of each pane along it, out of what the
//!   pair has together, measured from `terminal.list`'s cells and the
//!   chrome. So `ORIGIN`'s part comes short of Neovim's by a chrome per
//!   pane along it, as herdr's frame does. The pair's ratio is set: in a
//!   split of two, by one `layout.resize` of the terminal's part against
//!   the other's; in one of three or more, by one of each, keeping their
//!   sum, so the other children keep their room. A part sized in pixels
//!   exits 1, resizing nothing. The cells are trusted only in the tab
//!   `herdr-app status` says is shown, unzoomed as the app's `view.json`
//!   says: a hidden tab's may be the core's 120×40, and a zoomed tab's
//!   zoomed pane has the whole tab's; elsewhere it exits 1, setting
//!   nothing.
//! - `open_beside`, after its split ([`share_by`]): the new pane gets
//!   `RATIO` of the weight it and the origin's pane have, chrome ignored.
//!
//! The chrome, in cells, is [`chrome`]'s: measured at font size 14 and
//! zoom 1, and scaled by 14 ÷ the window's font size, as `herdr-app
//! status` says it, a scaling never measured; the app's zoom (UI scale)
//! is left out, unmeasured too.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::core::{Core, UNREADABLE};
use super::doc::pane_of;
use super::window::Window;
use super::{Failure, methods, needs, refused};
use crate::config::Capability;
use crate::providers::builtin::Given;

/// What `resize` has, of its 2 s.
const RESIZE_TIME: Duration = Duration::from_millis(1500);

/// A pane's chrome, its border, two-row header, padding and gap, in cells
/// at [`CHROME_FONT`] and zoom 1, as measured for SPI-1214.
const CHROME_COLS: f64 = 5.0;
const CHROME_ROWS: f64 = 4.0;
/// The font size the chrome was measured at.
const CHROME_FONT: f64 = 14.0;
/// The font sizes taken as the window's; any other is taken as
/// [`CHROME_FONT`].
const FONT_SIZES: std::ops::RangeInclusive<f64> = 4.0..=200.0;

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
    let window = Window::of(given).map_err(Failure::Unsupported)?;
    let mut core = Core::of(given, time)?;
    needs(&core, methods(Capability::Resize), || Ok(window.clone()))?;
    let due = core.due();

    let doc = core.doc().map_err(String::from)?;
    let pair = Pair::of(&doc, &terminal, &origin)?;
    if pair.split["type"] != axis.kind() {
        return Err(Failure::Failed(format!(
            "{terminal} and {origin} aren't {}",
            axis.way()
        )));
    }
    // sized in pixels: nothing to ask the core or the window for
    pair.weights()?;
    let status = window
        .status(due)
        .map_err(|ran| format!("can't tell which Herdr app tab is shown: {ran}"))?;
    let tab = pair.tab;
    if status["tab"] != tab {
        return Err(Failure::Failed(format!(
            "{terminal}'s Herdr app tab {tab} isn't shown, so its cells can't be trusted"
        )));
    }
    if let Some(pane) = zoomed(given, tab)? {
        return Err(Failure::Failed(format!(
            "{terminal}'s Herdr app tab {tab} is zoomed on {pane}, so its cells can't be trusted"
        )));
    }
    let cells = cells(&mut core)?;
    let measure = Measure {
        axis,
        cells: &cells,
        // a status without one: the font the chrome was measured at
        chrome: chrome(status["font_size"].as_f64()),
    };
    let share = measure.share(&pair, size).map_err(|why| match why {
        Unshared::Unmeasured => {
            format!("{terminal} and {origin} have no cells in the hc core to measure them by")
        }
        Unshared::NoRoom => format!("{terminal} and {origin} have no room to share"),
    })?;
    for (id, weight) in pair.settings(share)? {
        let args = json!({ "id": id, "weight": weight });
        window
            .act("layout.resize", &args, due)
            .map_err(|ran| refused(ran, &core, &window))?;
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

/// The window's chrome around each pane, in cells, `(cols, rows)`, at
/// font size `font_size`: measured at 14 and zoom 1, it is pixels, so
/// taken as fewer cells as the font grows, 14 ÷ `font_size`, a scaling
/// never measured. A font size unknown, or outside 4 to 200, is taken as
/// 14.
pub(super) fn chrome(font_size: Option<f64>) -> (f64, f64) {
    let font = font_size
        .filter(|f| FONT_SIZES.contains(f))
        .unwrap_or(CHROME_FONT);
    let scale = CHROME_FONT / font;
    (CHROME_COLS * scale, CHROME_ROWS * scale)
}

/// The cols and rows of each live terminal, by address, as
/// `terminal.list` has them.
fn cells(core: &mut Core) -> Result<HashMap<String, (f64, f64)>, String> {
    let list = core.call("terminal.list", json!({}))?;
    let terminals = list["terminals"].as_array().ok_or(UNREADABLE)?;
    Ok(terminals
        .iter()
        .filter_map(|t| {
            let address = t["address"].as_str()?;
            Some((
                address.to_string(),
                (t["cols"].as_f64()?, t["rows"].as_f64()?),
            ))
        })
        .collect())
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

    /// The `(cols, rows)` of `cells` along it.
    fn of_cells(self, (cols, rows): (f64, f64)) -> f64 {
        match self {
            Axis::Row => cols,
            Axis::Column => rows,
        }
    }
}

/// A node's `size`: a weight, none counting as 1, or pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Size {
    Weight(f64),
    Px,
}

fn size_of(node: &Value) -> Size {
    if node["size"].get("px").is_some() {
        return Size::Px;
    }
    let weight = node["size"]["weight"].as_f64();
    Size::Weight(weight.filter(|w| w.is_finite() && *w > 0.0).unwrap_or(1.0))
}

/// The lowest split of the doc holding two panes, and its children
/// holding each: the terminal's part and the origin's.
struct Pair<'d> {
    /// The id of the tab it's in.
    tab: &'d str,
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
                tab: tab["id"].as_str().unwrap_or_default(),
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
        let weight = |node: &Value| match size_of(node) {
            Size::Weight(w) => Ok(w),
            Size::Px => Err(format!(
                "Herdr app node {} is sized in pixels",
                node["id"].as_str().unwrap_or("?")
            )),
        };
        Ok((weight(self.part)?, weight(self.other)?))
    }

    /// The `layout.resize`s that give its part `share` of the weight it
    /// and the other have, each node's id and weight; none for a weight
    /// already so. Their ratio is what's set: in a split of two, by the
    /// part's weight alone; in one of more, by both, keeping their sum so
    /// the other children keep their room. `share` must be strictly
    /// between 0 and 1.
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

/// Why a pair has no share to give.
#[derive(Debug)]
enum Unshared {
    /// No pane of a part has cells to measure it by.
    Unmeasured,
    /// Too little room for both parts' chrome and a cell each.
    NoRoom,
}

/// Outer extents along an axis, in cells: a pane's cells and its chrome.
struct Measure<'a> {
    axis: Axis,
    cells: &'a HashMap<String, (f64, f64)>,
    /// A pane's chrome, `(cols, rows)`.
    chrome: (f64, f64),
}

impl Measure<'_> {
    /// The share of the pair's outer extent that gives its part `size`
    /// cells and the chrome of each pane along it, held to leave each
    /// part its chrome and a cell; or why there's none.
    fn share(&self, pair: &Pair, size: u32) -> Result<f64, Unshared> {
        let total = sum_of([pair.part, pair.other].map(|n| (self.outer(n), size_of(n))))
            .ok_or(Unshared::Unmeasured)?;
        let chrome = |node| self.axis.of_cells(self.chrome) * panes_along(node, self.axis) as f64;
        let (part_chrome, other_chrome) = (chrome(pair.part), chrome(pair.other));
        let (least, most) = (part_chrome + 1.0, total - other_chrome - 1.0);
        if least > most {
            return Err(Unshared::NoRoom);
        }
        let wanted = f64::from(size) + part_chrome;
        Ok(wanted.clamp(least, most) / total)
    }

    /// `node`'s outer extent: a pane's cells and chrome; a split's sum
    /// along the axis, else its widest child. None where it can't be
    /// told: a block with no live terminal, as a mod's, counts by the
    /// weight of its measured siblings.
    fn outer(&self, node: &Value) -> Option<f64> {
        if node["type"] == "block" {
            let cells = self.cells.get(node["content"]["terminal"].as_str()?)?;
            return Some(self.axis.of_cells(*cells) + self.axis.of_cells(self.chrome));
        }
        let children = node["children"].as_array()?;
        if node["type"] == self.axis.kind() {
            sum_of(children.iter().map(|c| (self.outer(c), size_of(c))))
        } else {
            children
                .iter()
                .filter_map(|c| self.outer(c))
                .reduce(f64::max)
        }
    }
}

/// The extent of siblings along their split, each measured or not, and
/// sized: those not measured take what their weight gives them beside the
/// measured; None if one is sized in pixels, or none is measured.
fn sum_of(items: impl IntoIterator<Item = (Option<f64>, Size)>) -> Option<f64> {
    let (mut fixed, mut measured, mut weighed, mut unweighed) = (0.0, 0.0, 0.0, 0.0);
    for item in items {
        match item {
            (Some(outer), Size::Px) => fixed += outer,
            (Some(outer), Size::Weight(w)) => {
                measured += outer;
                weighed += w;
            }
            (None, Size::Weight(w)) => unweighed += w,
            (None, Size::Px) => return None,
        }
    }
    if unweighed > 0.0 {
        if weighed <= 0.0 {
            return None;
        }
        measured += measured / weighed * unweighed;
    }
    (fixed + measured > 0.0).then_some(fixed + measured)
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

    /// The nested tab's panes: `t1`'s and `t3`'s.
    const P1: &str = "pf7e3d563947903ed7a8c93ee0ac58181";
    const P3: &str = "p4c229a9b57964c201d7ab577ccf4a2ea";
    /// The shapes tab's: `t4`'s, `t5`'s, and the column holding `t6`.
    const P4: &str = "pbdfcba84288586794b7f1b61d52a8b4a";
    const P5: &str = "p6167ba561752977dbf43138259612244";
    const S6: &str = "s97cb6df4a068ddeecf30d45c2c51a536";
    /// The shapes tab's id; `status` shows the nested tab.
    const SHAPES_TAB: &str = "tb647b2d25c9734a2636b21f8d1a7c8c1c";
    const NESTED_TAB: &str = "tb14c28a7769ba1044103fd9584cc818f0";

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

    /// The nested tab `[t1@0.5 | (t2 / t3)@0.5]`, its cells: 39×52,
    /// 39×24, 39×24.
    fn nested() -> FakeCore {
        FakeCore::replaying(&["nested-doc.get", "nested-terminal.list"])
    }

    /// The shapes tab `[t4@0.5 | t5@0.25 | (t6 / t7@0.5 / [t8 | t9]@0.5)@0.25]`
    /// beside the nested one, their cells: `t4` 38×52, `t5` 17×52, `t6`
    /// 17×24, `t7` 17×10, `t8` and `t9` 6×10.
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

    /// `status`, its font size `size`.
    fn status_at(size: f64) -> Run {
        let mut run = recorded_run("status");
        run.stdout = run
            .stdout
            .replace("\"font_size\": 14.0", &format!("\"font_size\": {size:?}"));
        assert!(run.stdout.contains(&format!("{size:?}")));
        run
    }

    /// `status`, showing tab `tab`.
    fn status_on(tab: &str) -> Run {
        let mut run = recorded_run("status");
        run.stdout = run.stdout.replace(NESTED_TAB, tab);
        run
    }

    /// The nested core, `edit` made to its doc and its `terminal.list`.
    fn nested_edited(edit: impl Fn(&mut Value, &mut Value)) -> FakeCore {
        let mut exchanges = recorded("nested-doc.get");
        exchanges.push(recorded_one("nested-terminal.list", "terminal.list"));
        let mut doc = exchanges[1].result_mut()["value"].take();
        let mut list = exchanges[2].result_mut().take();
        edit(&mut doc, &mut list);
        exchanges[1].result_mut()["value"] = doc;
        *exchanges[2].result_mut() = list;
        FakeCore::serve(exchanges)
    }

    /// `given`, its window's home `home`.
    fn at_home(given: Given, home: &std::path::Path) -> Given {
        let home = home.as_os_str().as_bytes().to_vec();
        Given::from(move |name| match name {
            "HERDR_APP_HOME" => Some(home.clone()),
            other => given.env(other),
        })
    }

    fn resized_line(id: &str, weight: &str) -> String {
        format!(r#"do layout.resize {{"id":"{id}","weight":{weight}}}"#)
    }

    fn resize(
        core: &FakeCore,
        app: &FakeApp,
        terminal: &str,
        origin: &str,
        direction: &str,
        size: &str,
    ) -> Result<(), Failure> {
        let given = given(
            core,
            app,
            &[
                ("TERMINAL", terminal),
                ("ORIGIN", origin),
                ("DIRECTION", direction),
                ("SIZE", size),
            ],
        );
        resize_within(&given, TIME)
    }

    #[test]
    fn in_a_split_of_two_the_terminals_part_alone_gets_its_cells_and_chrome() {
        // t1 left of the column: outer 39 + 5 and 39 + 5 at font 14; 30
        // cells and 5 of chrome are 35 of 88, so t1's weight is the
        // column's 0.5 × 35 / 53
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P1, "0.330189")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(
            app.asked(),
            [
                "status".to_string(),
                format!(r#"do layout.resize {{"id":"{P1}","weight":0.330189}}"#)
            ]
        );
        assert_eq!(core.methods(), ["hello", "doc.get", "terminal.list"]);
    }

    #[test]
    fn in_a_split_of_more_both_parts_are_set_keeping_their_sum() {
        // t5 right of t4, outer 43 + 22 = 65 of weight 0.75: t5 gets
        // 35 / 65 of it, t4 30 / 65, and the column keeps its 0.25
        let core = shapes();
        let app = FakeApp::new(&[
            status_on(SHAPES_TAB),
            resized(P5, "0.403846"),
            resized(P4, "0.346154"),
        ]);
        assert_eq!(resize(&core, &app, "t5", "t4", "right", "30"), Ok(()));
        assert_eq!(
            app.asked()[1..],
            [resized_line(P5, "0.403846"), resized_line(P4, "0.346154")]
        );
    }

    #[test]
    fn a_part_of_several_panes_along_has_the_chrome_of_each() {
        // t6's column right of t4: 17 + 5 wide, [t8 | t9] 6 + 5 twice;
        // two panes along it, so 20 cells want 30 of the 65
        let core = shapes();
        let app = FakeApp::new(&[
            status_on(SHAPES_TAB),
            resized(S6, "0.346154"),
            resized(P4, "0.403846"),
        ]);
        assert_eq!(resize(&core, &app, "t6", "t4", "right", "20"), Ok(()));
        assert_eq!(
            app.asked()[1..],
            [resized_line(S6, "0.346154"), resized_line(P4, "0.403846")]
        );
    }

    #[test]
    fn a_missing_size_is_weight_one() {
        // t3 below t2, neither sized: rows 24 + 4 twice, so 30 rows want
        // 34 of 56, and t3's weight is t2's 1 × 34 / 22
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P3, "1.545455")]);
        assert_eq!(resize(&core, &app, "t3", "t2", "down", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P3, "1.545455")]);
    }

    #[test]
    fn the_chrome_is_scaled_by_14_over_the_font_size() {
        // the arithmetic only: that the app's chrome scales so is
        // unmeasured. At font 28 the chrome is 2.5 cols: t1 and the column are 41.5
        // each, 30 cells want 32.5 of 83, and t1 gets 0.5 × 32.5 / 50.5
        let core = nested();
        let app = FakeApp::new(&[status_at(28.0), resized(P1, "0.321782")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P1, "0.321782")]);
        assert_eq!(chrome(Some(14.0)), (5.0, 4.0));
        assert_eq!(chrome(Some(7.0)), (10.0, 8.0));
        assert_eq!(chrome(None), (5.0, 4.0));
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
    fn a_part_sized_in_pixels_resizes_nothing() {
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
        let app = FakeApp::new(&[recorded_run("status"), unknown, recorded_run("version")]);
        assert_eq!(
            resize(&nested(), &app, "t3", "t2", "down", "30"),
            Err(Failure::Unsupported(
                "herdr-app has no layout.resize (hc core 0.1.21, herdr-app 0.1.16)".into()
            ))
        );
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
    fn a_tab_not_shown_resizes_nothing() {
        // status shows the nested tab; t5 and t4 are in the shapes tab
        let core = shapes();
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t5", "t4", "right", "30"),
            Err(Failure::Failed(format!(
                "t5's Herdr app tab {SHAPES_TAB} isn't shown, so its cells can't be trusted"
            )))
        );
        assert_eq!(core.methods(), ["hello", "doc.get"]);
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn a_zoomed_tab_resizes_nothing() {
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status")]);
        let view = json!({ "tabs": { NESTED_TAB: { "zoomed": P3, "focused": P3 } } });
        std::fs::write(app.dir().join("view.json"), view.to_string()).unwrap();
        let vars = [
            ("TERMINAL", "t3"),
            ("ORIGIN", "t2"),
            ("DIRECTION", "down"),
            ("SIZE", "30"),
        ];
        let at = at_home(given(&core, &app, &vars), app.dir());
        assert_eq!(
            resize_within(&at, TIME),
            Err(Failure::Failed(format!(
                "t3's Herdr app tab {NESTED_TAB} is zoomed on {P3}, so its cells can't be trusted"
            )))
        );
        assert_eq!(core.methods(), ["hello", "doc.get"]);

        // another tab zoomed, or none: resized
        let core = nested();
        let app = FakeApp::new(&[recorded_run("status"), resized(P3, "1.545455")]);
        let view =
            json!({ "tabs": { NESTED_TAB: { "zoomed": null }, SHAPES_TAB: { "zoomed": P4 } } });
        std::fs::write(app.dir().join("view.json"), view.to_string()).unwrap();
        let at = at_home(given(&core, &app, &vars), app.dir());
        assert_eq!(resize_within(&at, TIME), Ok(()));

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
    fn a_window_that_cant_say_what_it_shows_resizes_nothing() {
        let core = nested();
        let app = FakeApp::new(&[recorded_run("err-no-window-status")]);
        let Err(Failure::Failed(why)) = resize(&core, &app, "t3", "t2", "down", "30") else {
            panic!("resized");
        };
        assert!(
            why.starts_with("can't tell which Herdr app tab is shown: herdr-app: no Herdr window"),
            "{why}"
        );
        assert_eq!(core.methods(), ["hello", "doc.get"]);
    }

    #[test]
    fn too_little_room_for_both_chromes_resizes_nothing() {
        // no cols at all: 5 + 5, where each part needs 5 and a cell
        let core = nested_edited(|_, list| {
            for t in list["terminals"].as_array_mut().unwrap() {
                t["cols"] = json!(0);
            }
        });
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t1", "t2", "left", "30"),
            Err(Failure::Failed("t1 and t2 have no room to share".into()))
        );
        assert_eq!(app.asked(), ["status"]);
    }

    #[test]
    fn panes_with_no_cells_resize_nothing() {
        let core = nested_edited(|_, list| list["terminals"] = json!([]));
        let app = FakeApp::new(&[recorded_run("status")]);
        assert_eq!(
            resize(&core, &app, "t1", "t2", "left", "30"),
            Err(Failure::Failed(
                "t1 and t2 have no cells in the hc core to measure them by".into()
            ))
        );
    }

    #[test]
    fn a_block_with_no_terminal_counts_by_its_siblings_weight() {
        // [t1@0.5 | [t2 | mod]@0.5]: t2 is 44 wide at weight 1, so the mod
        // at weight 1 is 44 too; 35 of 132, t1 0.5 × 35 / 97
        let core = nested_edited(|doc, _| {
            let column = &mut doc["spaces"][0]["tabs"][0]["root"]["children"][1];
            column["type"] = json!("row");
            column["children"][1]["content"] = json!({ "mod": "files" });
        });
        let app = FakeApp::new(&[recorded_run("status"), resized(P1, "0.180412")]);
        assert_eq!(resize(&core, &app, "t1", "t2", "left", "30"), Ok(()));
        assert_eq!(app.asked()[1..], [resized_line(P1, "0.180412")]);
    }

    #[test]
    fn a_font_size_past_belief_is_taken_as_14() {
        for font in [0.0, -14.0, f64::NAN, f64::INFINITY, 3.9, 201.0] {
            assert_eq!(chrome(Some(font)), (5.0, 4.0), "{font}");
        }
        assert_eq!(chrome(Some(4.0)), (17.5, 14.0));
        assert_eq!(chrome(Some(28.0)), (2.5, 2.0));
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
