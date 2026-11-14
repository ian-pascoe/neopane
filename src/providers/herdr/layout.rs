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
    let areas = layouts.iter().map(area).collect::<Result<Vec<_>, _>>()?;
    let mut report = json!({ "areas": areas });
    if let Some(focus) = snapshot["focused_pane_id"].as_str() {
        report["focus"] = focus.into();
    }
    Ok(report)
}

/// The layout area of one tab's `layout`.
fn area(layout: &Value) -> Result<Value, String> {
    let id = text(layout, "tab_id")?;
    let [ax, ay, aw, ah] = rect(&layout["area"])?;
    if aw <= 0.0 || ah <= 0.0 {
        return Err(format!("herdr tab {id} has no room"));
    }
    let panes = layout["panes"].as_array().ok_or(UNREADABLE)?;
    let framed = panes.len() > 1;
    let terminals = panes
        .iter()
        .map(|p| {
            let [x, y, w, h] = rect(&p["rect"])?;
            let mut terminal = json!({
                "terminal": text(p, "pane_id")?,
                "rect": [(x - ax) / aw, (y - ay) / ah, w / aw, h / ah],
            });
            if framed {
                terminal["inset"] = json!([1, 1, 1, 1]);
            }
            Ok(terminal)
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(json!({ "id": id, "terminals": terminals }))
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
