//! `report_layout`: the live layout of the whole zellij session, from
//! `list-panes --json`, `list-tabs --json --all` and `list-clients`.
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
//! - A floating pane is reported floating, in its tab's area, whether its
//!   tab shows its floats or not: the listing doesn't tell a pinned float,
//!   always shown, as `open_float` opens them, from a hidden one.
//! - In a tab with a pane full screen, its area holds that pane, the whole
//!   area, and the floats; its other tiled panes are in an area of their
//!   own, `tab_N~hidden`, where they'd be without it, as their rects say.
//!   The full screen pane is the one `is_fullscreen` and the size of the
//!   viewport: a pane any client has focused may be `is_fullscreen` too.
//! - `focus` is the pane of the one client `list-clients` lists; none with
//!   none or several, as it can't tell which the user types in. Not
//!   `is_focused`, which is true of a pane any client has focused, or once
//!   had.

use serde_json::{Value, json};

use super::session::Session;
use crate::providers::builtin::Given;

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, String> {
    let session = Session::of(given)?;
    let panes = session.run(&["list-panes", "--json"])?;
    let tabs = session.run(&["list-tabs", "--json", "--all"])?;
    let clients = session.run(&["list-clients"])?;
    Ok(layout(&panes, &tabs, &clients)?.to_string())
}

/// What `list-panes --json` gives of a pane.
struct Pane {
    id: u64,
    is_plugin: bool,
    is_floating: bool,
    is_fullscreen: bool,
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
            is_fullscreen: flag("is_fullscreen")?,
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

/// What `list-tabs --json` gives of a tab.
struct Tab {
    id: u64,
    viewport: [u64; 2],
    /// A pane of it is full screen.
    fullscreen: bool,
}

impl Tab {
    fn of(v: &Value) -> Option<Tab> {
        let n = |k: &str| v[k].as_u64();
        Some(Tab {
            id: n("tab_id")?,
            viewport: [n("viewport_columns")?, n("viewport_rows")?],
            fullscreen: v["is_fullscreen_active"].as_bool()?,
        })
    }
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
/// `panes`, `list-tabs --json --all`, `tabs`, and `list-clients`,
/// `clients`, printed. A pane of a tab `list-tabs` hasn't, made between
/// the two, waits for the next.
fn layout(panes: &[u8], tabs: &[u8], clients: &[u8]) -> Result<Value, String> {
    let panes = items(panes, "list-panes", Pane::of)?;
    let tabs = items(tabs, "list-tabs", Tab::of)?;
    let mut areas = Vec::new();
    for tab in tabs {
        let [w, h] = tab.viewport;
        if w == 0 || h == 0 {
            return Err(format!("tab {} has no room", tab.id));
        }
        let ours: Vec<&Pane> = panes
            .iter()
            .filter(|p| p.shown && p.tab_id == tab.id)
            .collect();
        if ours.is_empty() {
            continue;
        }
        // With no tiled pane, the area counts from the screen's top left,
        // off by any bar above or left of the viewport: neither listing
        // says where the viewport starts, and the bars, which would, are
        // listed only with `list-panes --all`. A tab of floats alone may
        // not even last: in 0.46, closing a lone tab's last tiled pane, a
        // float still in it, ended the session.
        let tiled = ours.iter().filter(|p| !p.is_floating);
        let x0 = tiled.clone().map(|p| p.frame[0]).min().unwrap_or(0);
        let y0 = tiled.map(|p| p.frame[1]).min().unwrap_or(0);
        let full = ours.iter().copied().find(|p| {
            tab.fullscreen
                && p.is_fullscreen
                && !p.is_floating
                && [p.frame[2], p.frame[3]] == [w as i64, h as i64]
        });
        // a plugin's id may be a terminal's number
        let (shown, hidden): (Vec<&Pane>, Vec<&Pane>) = ours
            .iter()
            .partition(|p| p.is_floating || full.is_none_or(|f| std::ptr::eq(f, **p)));
        let terminal = |p: &&Pane| p.terminal([x0, y0], [w as f64, h as f64]);
        let id = format!("tab_{}", tab.id);
        let terminals: Vec<Value> = shown.iter().map(terminal).collect();
        areas.push(json!({ "id": id, "terminals": terminals }));
        if !hidden.is_empty() {
            let terminals: Vec<Value> = hidden.iter().map(terminal).collect();
            areas.push(json!({ "id": format!("{id}~hidden"), "terminals": terminals }));
        }
    }
    let mut report = json!({ "areas": areas });
    if let Some(focus) = focus(clients) {
        report["focus"] = focus.into();
    }
    Ok(report)
}

impl Pane {
    /// As `report_layout` gives it, in an area at `corner` on the screen,
    /// `size` cells: its frame, and its inset, the frame less the content.
    fn terminal(&self, corner: [i64; 2], [w, h]: [f64; 2]) -> Value {
        let [x0, y0] = corner;
        let [x, y, cols, rows] = self.frame;
        let [cx, cy, ccols, crows] = self.content;
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
        let mut t = json!({ "terminal": self.id(), "rect": rect });
        if self.is_floating {
            t["floating"] = true.into();
        }
        if inset != [0; 4] {
            t["inset"] = json!(inset.map(|i| i.max(0)));
        }
        t
    }

    /// Its id, as `report_layout` gives it: `terminal_N`, or a plugin's
    /// `plugin_N`.
    fn id(&self) -> String {
        if self.is_plugin {
            format!("plugin_{}", self.id)
        } else {
            format!("terminal_{}", self.id)
        }
    }
}

/// The pane the user types in, from what `list-clients` printed,
/// `clients`: a header row, then a row per client, its id, then its pane's.
/// That pane, if there's one client; none with none or several, as
/// zellij doesn't say which the user types in.
fn focus(clients: &[u8]) -> Option<String> {
    let clients = std::str::from_utf8(clients).ok()?;
    let mut rows = clients
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("CLIENT_ID"));
    let (Some(row), None) = (rows.next(), rows.next()) else {
        return None;
    };
    let pane = row.split_whitespace().nth(1)?;
    let number = pane
        .strip_prefix("terminal_")
        .or_else(|| pane.strip_prefix("plugin_"))?;
    (!number.is_empty() && number.bytes().all(|b| b.is_ascii_digit())).then(|| pane.to_string())
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
            NO_CLIENT,
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
        let report = layout(all, include_bytes!("fixtures/0.46-tabs.json"), NO_CLIENT).unwrap();
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
                   "is_fullscreen": false,
                   "pane_x": x, "pane_y": y, "pane_columns": w, "pane_rows": h,
                   "pane_content_x": x, "pane_content_y": y,
                   "pane_content_columns": w, "pane_content_rows": h})
        };
        let panes = json!([
            pane(0, 0, false, [0, 1, 50, 48]),
            pane(1, 1, true, [5, 6, 10, 4]),
            pane(2, 7, false, [0, 1, 50, 48]),
        ]);
        let tab = |id: u32| {
            json!({"tab_id": id, "viewport_columns": 50, "viewport_rows": 48,
                                 "is_fullscreen_active": false})
        };
        let tabs = json!([tab(0), tab(1), tab(2)]);
        let report = layout(
            panes.to_string().as_bytes(),
            tabs.to_string().as_bytes(),
            NO_CLIENT,
        )
        .unwrap();
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
        let tabs = br#"[{"tab_id": 0, "viewport_columns": 50, "viewport_rows": 48,
                         "is_fullscreen_active": false}]"#;
        let layout = |panes: &[u8], tabs: &[u8]| layout(panes, tabs, NO_CLIENT);
        assert!(layout(b"nonsense", tabs).is_err());
        assert!(layout(b"[]", b"{}").is_err());
        assert!(layout(br#"[{"id": 0}]"#, tabs).is_err());
        assert!(
            layout(
                b"[]",
                br#"[{"tab_id": 0, "viewport_columns": 50, "viewport_rows": 48}]"#
            )
            .is_err()
        );
        let no_room = br#"[{"tab_id": 0, "viewport_columns": 0, "viewport_rows": 48,
                            "is_fullscreen_active": false}]"#;
        assert!(layout(b"[]", no_room).is_err());
        assert_eq!(layout(b"[]", tabs).unwrap(), json!({ "areas": [] }));
    }

    /// Recorded from zellij 0.46.0, one 120x40 client attached, the bars
    /// on: tab 0 is `(terminal_0 / terminal_10) | terminal_1`, with float
    /// `terminal_9`, pinned, its floats hidden, and `terminal_0` full
    /// screen; `terminal_1` once had a client's focus, so it is
    /// `is_fullscreen` too. Tab 1 is `terminal_2`, with float `terminal_7`.
    #[test]
    fn zellij_0_46_full_screen_the_other_tiled_panes_are_in_an_area_of_their_own() {
        let report = layout(
            include_bytes!("fixtures/0.46-fullscreen-panes.json"),
            include_bytes!("fixtures/0.46-fullscreen-tabs.json"),
            NO_CLIENT,
        )
        .unwrap();
        let (w, h) = (120.0, 38.0);
        let float = |id: &str| {
            json!({"terminal": id, "rect": [5.0 / w, 19.0 / h, 22.0 / w, 8.0 / h],
                   "floating": true, "inset": [1, 1, 1, 1]})
        };
        assert_eq!(
            report,
            json!({"areas": [
                {"id": "tab_0", "terminals": [
                    t("terminal_0", [0.0, 0.0, 1.0, 1.0], [0, 1, 0, 0]),
                    float("terminal_9"),
                ]},
                {"id": "tab_0~hidden", "terminals": [
                    t("terminal_1", [60.0 / w, 0.0, 60.0 / w, 1.0], [0, 1, 0, 0]),
                    t("terminal_10", [0.0, 19.0 / h, 60.0 / w, 19.0 / h], [0, 1, 1, 0]),
                ]},
                {"id": "tab_1", "terminals": [
                    t("terminal_2", [0.0, 0.0, 1.0, 1.0], [0; 4]),
                    float("terminal_7"),
                ]},
            ]})
        );
    }

    /// A tiled plugin pane numbered as the full screen terminal is hidden
    /// all the same.
    #[test]
    fn full_screen_hides_a_plugin_pane_of_the_full_screen_terminals_number() {
        let pane = |plugin: bool, full: bool, [x, y, w, h]: [i64; 4]| {
            json!({"id": 0, "is_plugin": plugin, "is_floating": false,
                   "is_suppressed": false, "is_selectable": true, "tab_id": 0,
                   "is_fullscreen": full,
                   "pane_x": x, "pane_y": y, "pane_columns": w, "pane_rows": h,
                   "pane_content_x": x, "pane_content_y": y,
                   "pane_content_columns": w, "pane_content_rows": h})
        };
        let panes = json!([
            pane(true, true, [25, 1, 25, 48]),
            pane(false, true, [0, 1, 50, 48]),
        ]);
        let tabs = json!([{"tab_id": 0, "viewport_columns": 50, "viewport_rows": 48,
                           "is_fullscreen_active": true}]);
        let report = layout(
            panes.to_string().as_bytes(),
            tabs.to_string().as_bytes(),
            NO_CLIENT,
        )
        .unwrap();
        assert_eq!(
            report,
            json!({"areas": [
                {"id": "tab_0", "terminals": [t("terminal_0", [0.0, 0.0, 1.0, 1.0], [0; 4])]},
                {"id": "tab_0~hidden", "terminals": [
                    t("plugin_0", [0.5, 0.0, 0.5, 1.0], [0; 4]),
                ]},
            ]})
        );
    }

    /// A tab that says it is full screen, but whose panes none fills it,
    /// is laid out as they are.
    #[test]
    fn full_screen_with_no_pane_filling_the_tab_hides_none() {
        let tabs = include_str!("fixtures/0.46-tabs.json").replace(
            "\"is_fullscreen_active\": false",
            "\"is_fullscreen_active\": true",
        );
        let report = layout(
            include_bytes!("fixtures/0.46-panes.json"),
            tabs.as_bytes(),
            NO_CLIENT,
        )
        .unwrap();
        assert_eq!(report, want());
    }

    /// No client attached.
    const NO_CLIENT: &[u8] = include_bytes!("fixtures/0.46-clients-none.txt");

    /// Recorded from zellij 0.46.0: with one client, focus is its pane;
    /// with none, or two, there's none.
    #[test]
    fn zellij_0_46_focus_is_the_pane_of_the_one_client() {
        assert_eq!(
            focus(include_bytes!("fixtures/0.46-clients-one.txt")).as_deref(),
            Some("terminal_0")
        );
        assert_eq!(focus(NO_CLIENT), None);
        assert_eq!(focus(include_bytes!("fixtures/0.46-clients-two.txt")), None);
        let report = layout(
            include_bytes!("fixtures/0.46-fullscreen-panes.json"),
            include_bytes!("fixtures/0.46-fullscreen-tabs.json"),
            include_bytes!("fixtures/0.46-clients-one.txt"),
        )
        .unwrap();
        assert_eq!(report["focus"], "terminal_0");
    }

    #[test]
    fn focus_is_only_ever_a_pane_id() {
        let one =
            |pane: &str| format!("CLIENT_ID  ZELLIJ_PANE_ID RUNNING_COMMAND\n1  {pane}  N/A\n");
        assert_eq!(
            focus(one("plugin_2").as_bytes()).as_deref(),
            Some("plugin_2")
        );
        for not in ["terminal_", "terminal_x", "2", "%2", "N/A", ""] {
            assert_eq!(focus(one(not).as_bytes()), None, "{not:?}");
        }
        assert_eq!(focus(b"\xff"), None);
    }
}
