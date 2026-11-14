//! `report_layout`: the live layout of the whole herdr server, from one
//! `session.snapshot`.
//!
//! - One layout area per tab of every workspace, its id the tab's, such
//!   as `w1:t2`: a herdr tab is a layout area. Foreign panes are in it
//!   too.
//! - Each pane's rect is herdr's, which takes in its frame, as fractions
//!   of its tab's area.
//! - Insets are what an alternate-screen program gets, as every Neovim
//!   pane is one: with herdr's default borders, a frame of one cell all
//!   round in a tab with a split, and none for a lone pane. On the
//!   primary screen herdr keeps a column more on the right for its
//!   scrollbar.
//! - A zoomed tab shows its zoomed pane, its focused one, alone, filling
//!   its area, framed as it is when the tab has a split; herdr gives the
//!   panes it hides their unzoomed rects, which are reported in an area of
//!   their own, `<tab id>~hidden`. The zoom follows focus: focus moved
//!   within a zoomed tab, with herdr's keys or its API, leaves it zoomed
//!   on the pane focused.
//! - `focus` is the server's focused pane: focus is one per server.

use serde_json::{Value, json};

use super::client::{Herdr, UNREADABLE, text};
use crate::providers::builtin::Given;

/// What `report_layout` has, of its 1 s.
const LAYOUT_TIME: std::time::Duration = std::time::Duration::from_millis(900);

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, String> {
    let herdr = Herdr::of(given, LAYOUT_TIME)?;
    let snapshot = herdr.call("session.snapshot", json!({}))?;
    Ok(layout(&snapshot["snapshot"])?.to_string())
}

/// The report of herdr's `snapshot`.
fn layout(snapshot: &Value) -> Result<Value, String> {
    let layouts = snapshot["layouts"].as_array().ok_or(UNREADABLE)?;
    let mut areas = Vec::new();
    for l in layouts {
        areas.extend(areas_of(l)?);
    }
    let mut report = json!({ "areas": areas });
    if let Some(focus) = snapshot["focused_pane_id"].as_str() {
        report["focus"] = focus.into();
    }
    Ok(report)
}

/// The layout areas of one tab's `layout`: the tab's; and in a zoomed tab
/// with others hidden, theirs, `<tab id>~hidden`, where they were.
fn areas_of(layout: &Value) -> Result<Vec<Value>, String> {
    let id = text(layout, "tab_id")?;
    let [ax, ay, aw, ah] = rect(&layout["area"])?;
    if aw <= 0.0 || ah <= 0.0 {
        return Err(format!("herdr tab {id} has no room"));
    }
    let panes = layout["panes"].as_array().ok_or(UNREADABLE)?;
    let framed = panes.len() > 1;
    let terminal = |p: &Value, rect: [f64; 4]| -> Result<Value, String> {
        let mut terminal = json!({ "terminal": text(p, "pane_id")?, "rect": rect });
        if framed {
            terminal["inset"] = json!([1, 1, 1, 1]);
        }
        Ok(terminal)
    };
    let where_it_is = |p: &Value| -> Result<Value, String> {
        let [x, y, w, h] = rect(&p["rect"])?;
        terminal(p, [(x - ax) / aw, (y - ay) / ah, w / aw, h / ah])
    };
    // herdr gives a zoomed tab's panes their unzoomed rects: the zoomed
    // one, its focused pane, takes the whole area, keeping its frame
    if layout["zoomed"] == true {
        let zoomed = layout["focused_pane_id"].as_str().ok_or(UNREADABLE)?;
        let (shown, hidden): (Vec<&Value>, Vec<&Value>) =
            panes.iter().partition(|p| p["pane_id"] == zoomed);
        let [shown] = shown[..] else {
            return Err(UNREADABLE.into());
        };
        let mut areas = vec![json!({
            "id": id,
            "terminals": [terminal(shown, [0.0, 0.0, 1.0, 1.0])?],
        })];
        if !hidden.is_empty() {
            let hidden = hidden
                .into_iter()
                .map(where_it_is)
                .collect::<Result<Vec<_>, _>>()?;
            areas.push(json!({ "id": format!("{id}~hidden"), "terminals": hidden }));
        }
        return Ok(areas);
    }
    let terminals = panes
        .iter()
        .map(where_it_is)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(vec![json!({ "id": id, "terminals": terminals })])
}

/// A herdr rect, `{x, y, width, height}` in cells.
fn rect(r: &Value) -> Result<[f64; 4], String> {
    let cell = |k: &str| r[k].as_f64().ok_or_else(|| String::from(UNREADABLE));
    Ok([cell("x")?, cell("y")?, cell("width")?, cell("height")?])
}

#[cfg(test)]
mod tests {
    use super::super::client::result_of;
    use super::super::fake::recorded;
    use super::*;

    fn snapshot(name: &str) -> Value {
        result_of(recorded(name).trim().as_bytes()).unwrap()["snapshot"].clone()
    }

    #[test]
    fn every_tab_is_an_area_its_panes_rects_as_fractions_framed_when_split() {
        let report = layout(&snapshot("session-snapshot")).unwrap();
        let (w, h) = (74.0, 29.0);
        assert_eq!(
            report,
            json!({
                "areas": [
                    {
                        "id": "w1:t1",
                        "terminals": [
                            {"terminal": "w1:p1", "rect": [0.0, 0.0, 52.0 / w, 15.0 / h], "inset": [1, 1, 1, 1]},
                            {"terminal": "w1:p3", "rect": [0.0, 15.0 / h, 52.0 / w, 14.0 / h], "inset": [1, 1, 1, 1]},
                            {"terminal": "w1:p2", "rect": [52.0 / w, 0.0, 22.0 / w, 1.0], "inset": [1, 1, 1, 1]},
                        ],
                    },
                    {
                        "id": "w2:t1",
                        "terminals": [{"terminal": "w2:p1", "rect": [0.0, 0.0, 1.0, 1.0]}],
                    },
                ],
                "focus": "w1:p2",
            })
        );
    }

    /// herdr's tab w1:t1 zoomed on w1:p3, `(P3 | P1) / (P4 | P5)` left of
    /// `P2 / P6`, 74x29: `pane.layout` gives every pane its unzoomed rect.
    #[test]
    fn a_zoomed_tab_shows_its_zoomed_pane_alone_and_hides_the_rest_where_they_were() {
        let report = layout(&snapshot("session-snapshot-zoomed-3")).unwrap();
        let (w, h) = (74.0, 29.0);
        let hidden = |id: &str, x: f64, y: f64, pw: f64, ph: f64| json!({"terminal": id, "rect": [x / w, y / h, pw / w, ph / h], "inset": [1, 1, 1, 1]});
        assert_eq!(
            report,
            json!({
                "areas": [
                    {
                        "id": "w1:t1",
                        "terminals": [
                            {"terminal": "w1:p3", "rect": [0.0, 0.0, 1.0, 1.0], "inset": [1, 1, 1, 1]},
                        ],
                    },
                    {
                        "id": "w1:t1~hidden",
                        "terminals": [
                            hidden("w1:p1", 11.0, 0.0, 26.0, 15.0),
                            hidden("w1:p4", 0.0, 15.0, 15.0, 14.0),
                            hidden("w1:p5", 15.0, 15.0, 22.0, 14.0),
                            hidden("w1:p2", 37.0, 0.0, 37.0, 15.0),
                            hidden("w1:p6", 37.0, 15.0, 37.0, 14.0),
                        ],
                    },
                ],
                "focus": "w1:p3",
            })
        );
        // the zoomed tab among others: its hidden panes come after it
        let report = layout(&snapshot("session-snapshot-zoomed")).unwrap();
        let ids: Vec<&str> = report["areas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| a["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["w1:t1", "w2:t1", "w2:t1~hidden"]);
        assert_eq!(
            report["areas"][1]["terminals"],
            json!([{"terminal": "w2:p1", "rect": [0.0, 0.0, 1.0, 1.0], "inset": [1, 1, 1, 1]}])
        );
    }

    #[test]
    fn a_zoomed_lone_pane_is_unframed_and_a_zoom_on_no_pane_of_its_tab_cant_be_read() {
        let s = |focused: &str| {
            json!({
                "layouts": [{
                    "tab_id": "w1:t1",
                    "zoomed": true,
                    "focused_pane_id": focused,
                    "area": {"x": 0, "y": 0, "width": 10, "height": 4},
                    "panes": [{"pane_id": "w1:p1", "rect": {"x": 0, "y": 0, "width": 10, "height": 4}}],
                }],
            })
        };
        assert_eq!(
            layout(&s("w1:p1")).unwrap(),
            json!({"areas": [{"id": "w1:t1", "terminals": [
                {"terminal": "w1:p1", "rect": [0.0, 0.0, 1.0, 1.0]}
            ]}]})
        );
        assert_eq!(layout(&s("w1:p2")), Err(UNREADABLE.into()));
    }

    #[test]
    fn rects_count_from_the_areas_corner_and_a_snapshot_without_layouts_cant_be_read() {
        let s = json!({
            "layouts": [{
                "tab_id": "w1:t1",
                "area": {"x": 2, "y": 1, "width": 10, "height": 4},
                "panes": [{"pane_id": "w1:p1", "rect": {"x": 7, "y": 3, "width": 5, "height": 2}}],
            }],
            "focused_pane_id": null,
        });
        assert_eq!(
            layout(&s).unwrap(),
            json!({"areas": [{"id": "w1:t1", "terminals": [
                {"terminal": "w1:p1", "rect": [0.5, 0.5, 0.5, 0.5]}
            ]}]})
        );
        assert_eq!(layout(&json!({})), Err(UNREADABLE.into()));
    }
}
