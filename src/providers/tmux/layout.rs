//! `report_layout`: the live layout of the whole tmux server, from
//! `list-panes -a` and `list-clients`.
//!
//! - One layout area per window of the server, its id `@N`. A window
//!   linked into several sessions is listed once per session, and counted
//!   once. Foreign panes, not Neovim panes, are in it too.
//! - Each pane's rect is its content, as fractions of its window: tmux's
//!   borders belong to no pane, so insets are zero. With
//!   `pane-border-status top`, a pane on the window's top row loses that
//!   row to its status line: it is reported from the top, one row taller,
//!   with an inset of 1 at the top.
//! - `focus` is the active pane of the window the most recently active
//!   client shows; none without a client.

use serde_json::{Value, json};

use super::is_pane_id;
use super::server::Server;
use crate::providers::builtin::Given;

/// One line per pane of the server, a window linked into several sessions
/// listing its panes once for each.
const PANES: &str = "#{window_id} #{window_width} #{window_height} #{pane_id} #{pane_left} \
                     #{pane_top} #{pane_width} #{pane_height} #{pane_at_top} \
                     #{pane-border-status}";

/// One line per client: its last activity, and the active pane of the
/// window it shows.
const CLIENTS: &str = "#{client_activity} #{pane_id}";

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let panes = server.run(&["list-panes", "-a", "-F", PANES])?;
    let clients = server.run(&["list-clients", "-F", CLIENTS])?;
    Ok(layout(&panes, &clients)?.to_string())
}

/// The report `report_layout` prints, from what `list-panes -a -F PANES`
/// and `list-clients -F CLIENTS` printed.
fn layout(panes: &[u8], clients: &[u8]) -> Result<Value, String> {
    let panes = std::str::from_utf8(panes).map_err(|_| "list-panes printed bytes not UTF-8")?;
    let mut areas: Vec<(String, Vec<Value>)> = Vec::new();
    let mut seen = Vec::new();
    for line in panes.lines().filter(|l| !l.trim().is_empty()) {
        let p = Pane::parse(line).ok_or_else(|| format!("list-panes printed {line:?}"))?;
        if seen.contains(&p.id) {
            continue;
        }
        seen.push(p.id.clone());
        let terminal = p.terminal();
        match areas.iter_mut().find(|(id, _)| *id == p.window) {
            Some((_, terminals)) => terminals.push(terminal),
            None => areas.push((p.window, vec![terminal])),
        }
    }
    let areas: Vec<Value> = areas
        .into_iter()
        .map(|(id, terminals)| json!({ "id": id, "terminals": terminals }))
        .collect();
    let mut report = json!({ "areas": areas });
    if let Some(focus) = focus(clients) {
        report["focus"] = focus.into();
    }
    Ok(report)
}

/// The pane the most recently active client shows: the first of the
/// latest.
fn focus(clients: &[u8]) -> Option<String> {
    let clients = std::str::from_utf8(clients).ok()?;
    let mut latest: Option<(u64, &str)> = None;
    for line in clients.lines() {
        let Some((Ok(activity), pane)) = line
            .split_once(' ')
            .map(|(a, p)| (a.parse::<u64>(), p.trim()))
        else {
            continue;
        };
        if is_pane_id(pane.as_bytes()) && latest.is_none_or(|(a, _)| activity > a) {
            latest = Some((activity, pane));
        }
    }
    latest.map(|(_, pane)| pane.to_string())
}

/// One line of `list-panes -a -F PANES`.
struct Pane {
    window: String,
    id: String,
    window_size: [f64; 2],
    /// Left, top, width, height, in cells.
    cells: [f64; 4],
    at_top: bool,
    status_top: bool,
}

impl Pane {
    fn parse(line: &str) -> Option<Pane> {
        let w: Vec<&str> = line.split(' ').collect();
        let [window, ww, wh, id, left, top, width, height, at_top, status] = w.as_slice() else {
            return None;
        };
        let n = |s: &str| s.parse::<u32>().ok().map(f64::from);
        let window_size = [n(ww)?, n(wh)?];
        if window_size.contains(&0.0) || !window.starts_with('@') || !is_pane_id(id.as_bytes()) {
            return None;
        }
        Some(Pane {
            window: window.to_string(),
            id: id.to_string(),
            window_size,
            cells: [n(left)?, n(top)?, n(width)?, n(height)?],
            at_top: *at_top == "1",
            status_top: *status == "top",
        })
    }

    /// As `report_layout` gives it.
    fn terminal(&self) -> Value {
        let [mut x, mut y, mut w, mut h] = self.cells;
        let status_row = self.status_top && self.at_top && y >= 1.0;
        if status_row {
            y -= 1.0;
            h += 1.0;
        }
        let [ww, wh] = self.window_size;
        (x, w) = (x / ww, w / ww);
        (y, h) = (y / wh, h / wh);
        let mut t = json!({ "terminal": self.id, "rect": [x, y, w, h] });
        if status_row {
            t["inset"] = json!([0, 1, 0, 0]);
        }
        t
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal as the report gives it.
    fn t(id: &str, rect: [f64; 4]) -> Value {
        json!({ "terminal": id, "rect": rect })
    }

    /// One with the top row of a window with `pane-border-status top`.
    fn under_status(id: &str, rect: [f64; 4]) -> Value {
        json!({ "terminal": id, "rect": rect, "inset": [0, 1, 0, 0] })
    }

    /// Recorded from tmux 3.8, 2 clients attached: window @0 (120x39, the
    /// border status on top) is `%0 | (%1 / %2)` in sessions `a` and `b`;
    /// @1 (100x30, shown by none) holds `%3`, in both; @3 (session `c`,
    /// 120x39) holds `%5`. The client on `a` was active last, its window
    /// @0's active pane being `%2`.
    #[test]
    fn tmux_3_8_one_area_per_window_linked_ones_once_focus_the_latest_clients() {
        let report = layout(
            include_bytes!("fixtures/3.8-panes.txt"),
            include_bytes!("fixtures/3.8-clients.txt"),
        )
        .unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [
                    under_status("%0", [0.0, 0.0, 79.0 / 120.0, 1.0]),
                    under_status("%1", [80.0 / 120.0, 0.0, 40.0 / 120.0, 20.0 / 39.0]),
                    t("%2", [80.0 / 120.0, 21.0 / 39.0, 40.0 / 120.0, 18.0 / 39.0]),
                ]},
                {"id": "@1", "terminals": [t("%3", [0.0, 0.0, 1.0, 1.0])]},
                {"id": "@3", "terminals": [t("%5", [0.0, 0.0, 1.0, 1.0])]},
            ],
            "focus": "%2",
        });
        assert_eq!(report, want);
    }

    /// Recorded from tmux 3.2a, no client attached: window @0 (80x23, the
    /// border status on top) is `(%0 | %2) / %1`, @1 (80x24) holds `%3`,
    /// both linked into sessions `a` and `b`.
    #[test]
    fn tmux_3_2_status_on_top_only_the_top_row_loses_a_row_and_no_client_no_focus() {
        let report = layout(include_bytes!("fixtures/3.2a-panes.txt"), b"").unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [
                    under_status("%0", [0.0, 0.0, 40.0 / 80.0, 11.0 / 23.0]),
                    under_status("%2", [41.0 / 80.0, 0.0, 39.0 / 80.0, 11.0 / 23.0]),
                    t("%1", [0.0, 12.0 / 23.0, 1.0, 11.0 / 23.0]),
                ]},
                {"id": "@1", "terminals": [t("%3", [0.0, 0.0, 1.0, 1.0])]},
            ],
        });
        assert_eq!(report, want);
    }

    #[test]
    fn focus_is_only_ever_a_pane_id() {
        let panes = b"@0 80 24 %0 0 0 80 24 1 off\n";
        let report = layout(panes, b"9 %\n8 %1x\n7 @1\n3 %0\n").unwrap();
        assert_eq!(report["focus"], "%0");
        let report = layout(panes, b"9 %x\n").unwrap();
        assert!(report.get("focus").is_none());
    }

    #[test]
    fn a_line_it_cant_read_fails_the_report() {
        assert!(layout(b"@0 80 24 %0 0 0 80\n", b"").is_err());
        assert!(layout(b"@0 0 24 %0 0 0 80 24 1 off\n", b"").is_err());
        // no pane at all is no area
        assert_eq!(layout(b"", b"").unwrap(), json!({ "areas": [] }));
    }
}
