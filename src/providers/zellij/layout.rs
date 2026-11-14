//! `report_layout`: the live layout of the whole zellij session, from
//! `list-panes --json` and `list-tabs --json --all`.
//!
//! - One layout area per tab, its id `tab_N`, from the tab's stable id.
//!   `list-tabs` gives each tab's viewport, the room its panes share, bars
//!   excluded, but not where it starts: the area is the bounding box of
//!   the tab's tiled panes' top left, as wide and tall as the viewport.
//! - Every selectable pane is in it, foreign ones too: a plugin pane, such
//!   as a file picker, as `plugin_N`. The tab and status bars, which
//!   can't be selected, are outside the area; panes zellij suppresses,
//!   hidden in a stack or replaced in place, are left out.
//! - Each rect is the pane's frame, as fractions of its area; its inset is
//!   the frame less the content on each edge. zellij gives a tiled pane
//!   a title row, and the separator to its right or below; a float a
//!   whole box; a lone tiled pane none.
//! - A floating pane is reported floating, whether its tab shows its
//!   floats or not.
//! - No `focus`: `is_focused` is true of a pane any client has focused, or
//!   once had.

use serde_json::{Value, json};

use super::session::Session;
use crate::providers::builtin::Given;

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, String> {
    let session = Session::of(given)?;
    let panes = session.run(&["list-panes", "--json"])?;
    let tabs = session.run(&["list-tabs", "--json", "--all"])?;
    Ok(layout(&panes, &tabs)?.to_string())
}

/// What `list-panes --json` gives of a pane.
struct Pane {
    id: u64,
    is_plugin: bool,
    is_floating: bool,
    shown: bool,
    tab_id: u64,
    /// Its frame and its content: left, top, width, height, in cells.
    frame: [i64; 4],
    content: [i64; 4],
}

impl Pane {
    fn of(v: &Value) -> Option<Pane> {
        let flag = |k: &str| v[k].as_bool();
        let n = |k: &str| v[k].as_i64();
        let cells = |x, y, w, h| Some([n(x)?, n(y)?, n(w)?, n(h)?]);
        Some(Pane {
            id: v["id"].as_u64()?,
            is_plugin: flag("is_plugin")?,
            is_floating: flag("is_floating")?,
            shown: flag("is_selectable")? && !flag("is_suppressed")?,
            tab_id: v["tab_id"].as_u64()?,
            frame: cells("pane_x", "pane_y", "pane_columns", "pane_rows")?,
            content: cells(
                "pane_content_x",
                "pane_content_y",
                "pane_content_columns",
                "pane_content_rows",
            )?,
        })
    }
}

/// What `list-tabs --json` gives of a tab: its id and its viewport.
fn tab_of(v: &Value) -> Option<(u64, [u64; 2])> {
    let n = |k: &str| v[k].as_u64();
    Some((n("tab_id")?, [n("viewport_columns")?, n("viewport_rows")?]))
}

/// The items of the JSON list `listing`, as `of` reads them, or why not.
fn items<T>(
    listing: &[u8],
    what: &str,
    of: impl Fn(&Value) -> Option<T>,
) -> Result<Vec<T>, String> {
    let v: Value =
        serde_json::from_slice(listing).map_err(|e| format!("{what} printed no JSON: {e}"))?;
    let list = v
        .as_array()
        .ok_or_else(|| format!("{what} printed no list"))?;
    list.iter()
        .map(|item| of(item).ok_or_else(|| format!("{what} printed {item}")))
        .collect()
}

/// The report `report_layout` prints, from what `list-panes --json`,
/// `panes`, and `list-tabs --json --all`, `tabs`, printed. A pane of a
/// tab `list-tabs` hasn't, made between the two, waits for the next.
fn layout(panes: &[u8], tabs: &[u8]) -> Result<Value, String> {
    let panes = items(panes, "list-panes", Pane::of)?;
    let tabs = items(tabs, "list-tabs", tab_of)?;
    let mut areas = Vec::new();
    for (tab, [w, h]) in tabs {
        if w == 0 || h == 0 {
            return Err(format!("tab {tab} has no room"));
        }
        let (w, h) = (w as f64, h as f64);
        let ours: Vec<&Pane> = panes
            .iter()
            .filter(|p| p.shown && p.tab_id == tab)
            .collect();
        if ours.is_empty() {
            continue;
        }
        let tiled = ours.iter().filter(|p| !p.is_floating);
        let x0 = tiled.clone().map(|p| p.frame[0]).min().unwrap_or(0);
        let y0 = tiled.map(|p| p.frame[1]).min().unwrap_or(0);
        let terminals: Vec<Value> = ours
            .iter()
            .map(|p| {
                let id = if p.is_plugin {
                    format!("plugin_{}", p.id)
                } else {
                    format!("terminal_{}", p.id)
                };
                let [x, y, cols, rows] = p.frame;
                let [cx, cy, ccols, crows] = p.content;
                let rect = [
                    (x - x0) as f64 / w,
                    (y - y0) as f64 / h,
                    cols as f64 / w,
                    rows as f64 / h,
                ];
                let inset = [
                    cx - x,
                    cy - y,
                    (x + cols) - (cx + ccols),
                    (y + rows) - (cy + crows),
                ];
                let mut t = json!({ "terminal": id, "rect": rect });
                if p.is_floating {
                    t["floating"] = true.into();
                }
                if inset != [0; 4] {
                    t["inset"] = json!(inset.map(|i| i.max(0)));
                }
                t
            })
            .collect();
        areas.push(json!({ "id": format!("tab_{tab}"), "terminals": terminals }));
    }
    Ok(json!({ "areas": areas }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal as the report gives it, with its inset.
    fn t(id: &str, rect: [f64; 4], inset: [i64; 4]) -> Value {
        let mut t = json!({ "terminal": id, "rect": rect });
        if inset != [0; 4] {
            t["inset"] = json!(inset);
        }
        t
    }

    /// Recorded from zellij 0.46.0, one 120x40 client attached, the tab
    /// and status bars on: tab 0 (viewport 120x38, from row 1) is
    /// `terminal_0 | (terminal_1 / terminal_2)`, with float `terminal_3`
    /// at 10,5 40x10, and a suppressed plugin; tab 1 is `(terminal_4 over
    /// terminal_6, stacked) | (terminal_5 / plugin_6)`, a file picker,
    /// `terminal_4` suppressed by the stack.
    fn want() -> Value {
        let (w, h) = (120.0, 38.0);
        json!({
            "areas": [
                {"id": "tab_0", "terminals": [
                    t("terminal_0", [0.0, 0.0, 60.0 / w, 1.0], [0, 1, 1, 0]),
                    t("terminal_1", [60.0 / w, 0.0, 60.0 / w, 19.0 / h], [0, 1, 0, 1]),
                    t("terminal_2", [60.0 / w, 19.0 / h, 60.0 / w, 19.0 / h], [0, 1, 0, 0]),
                    {"terminal": "terminal_3",
                     "rect": [10.0 / w, 4.0 / h, 40.0 / w, 10.0 / h],
                     "floating": true, "inset": [1, 1, 1, 1]},
                ]},
                {"id": "tab_1", "terminals": [
                    t("plugin_6", [60.0 / w, 19.0 / h, 60.0 / w, 19.0 / h], [0, 1, 0, 0]),
                    t("terminal_5", [60.0 / w, 0.0, 60.0 / w, 19.0 / h], [0, 1, 0, 1]),
                    t("terminal_6", [0.0, 0.0, 60.0 / w, 1.0], [0, 3, 1, 0]),
                ]},
            ],
        })
    }

    #[test]
    fn zellij_0_46_one_area_per_tab_from_its_tiled_panes_top_left_frames_and_insets() {
        let report = layout(
            include_bytes!("fixtures/0.46-panes.json"),
            include_bytes!("fixtures/0.46-tabs.json"),
        )
        .unwrap();
        assert_eq!(report, want());
    }

    /// The same, listed with `--all`: the tab and status bars, at rows 0
    /// and 39, can't be selected, and are left out.
    #[test]
    fn zellij_0_46_the_bars_are_outside_every_area() {
        let all = include_bytes!("fixtures/0.46-panes-all.json");
        let listed: Vec<Value> = serde_json::from_slice(all).unwrap();
        assert!(listed.iter().any(|p| p["title"] == "status-bar"));
        let report = layout(all, include_bytes!("fixtures/0.46-tabs.json")).unwrap();
        assert_eq!(report, want());
    }

    /// A lone tiled pane has no frame; a float in a tab with no tiled pane
    /// counts from the screen's top left; a tab with nothing selectable
    /// has no area; a pane of a tab not listed waits.
    #[test]
    fn a_lone_pane_fills_its_area_with_no_inset() {
        let pane = |id: u32, tab: u32, floating: bool, [x, y, w, h]: [i64; 4]| {
            json!({"id": id, "is_plugin": false, "is_floating": floating,
                   "is_suppressed": false, "is_selectable": true, "tab_id": tab,
                   "pane_x": x, "pane_y": y, "pane_columns": w, "pane_rows": h,
                   "pane_content_x": x, "pane_content_y": y,
                   "pane_content_columns": w, "pane_content_rows": h})
        };
        let panes = json!([
            pane(0, 0, false, [0, 1, 50, 48]),
            pane(1, 1, true, [5, 6, 10, 4]),
            pane(2, 7, false, [0, 1, 50, 48]),
        ]);
        let tab = |id: u32| json!({"tab_id": id, "viewport_columns": 50, "viewport_rows": 48});
        let tabs = json!([tab(0), tab(1), tab(2)]);
        let report = layout(panes.to_string().as_bytes(), tabs.to_string().as_bytes()).unwrap();
        assert_eq!(
            report,
            json!({"areas": [
                {"id": "tab_0", "terminals": [t("terminal_0", [0.0, 0.0, 1.0, 1.0], [0; 4])]},
                {"id": "tab_1", "terminals": [{"terminal": "terminal_1",
                    "rect": [0.1, 0.125, 0.2, 4.0 / 48.0], "floating": true}]},
            ]})
        );
    }

    #[test]
    fn a_listing_it_cant_read_fails_the_report() {
        let tabs = br#"[{"tab_id": 0, "viewport_columns": 50, "viewport_rows": 48}]"#;
        assert!(layout(b"nonsense", tabs).is_err());
        assert!(layout(b"[]", b"{}").is_err());
        assert!(layout(br#"[{"id": 0}]"#, tabs).is_err());
        let no_room = br#"[{"tab_id": 0, "viewport_columns": 0, "viewport_rows": 48}]"#;
        assert!(layout(b"[]", no_room).is_err());
        assert_eq!(layout(b"[]", tabs).unwrap(), json!({ "areas": [] }));
    }
}
