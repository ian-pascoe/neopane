//! `report_layout`: the live layout of the whole tmux server, from
//! `list-panes -a` and `list-clients`.
//!
//! - One layout area per window of the server, its id `@N`. A window
//!   linked into several sessions is listed once per session, and counted
//!   once. Foreign panes, not Neovim panes, are in it too.
//! - Each tiled pane's rect is its content, as fractions of its window:
//!   tmux's borders belong to no pane, so insets are zero. With
//!   `pane-border-status top`, a pane on the window's top row loses that
//!   row to its status line: it is reported from the top, one row taller,
//!   with an inset of 1 at the top.
//! - A floating pane (3.7 and newer) is its frame, which belongs to it:
//!   its content grown by a cell on each side, with an inset of 1 all
//!   round. Either may be partly off its window.
//! - A zoomed window's area holds the panes it shows: the zoomed one. The
//!   panes the zoom hides are in an area of their own, `@N~hidden`, where
//!   the unzoomed layout, `#{window_layout}`, has them, as their own
//!   `pane_*` are stale.
//! - `focus` is the active pane of the window the most recently active
//!   client shows; none without a client.

use serde_json::{Value, json};

use super::is_pane_id;
use super::server::Server;
use super::window_layout::parse;
use crate::providers::builtin::Given;

/// One line per pane of the server, a window linked into several sessions
/// listing its panes once for each; for `display-message`, the pane's.
pub(super) const PANES: &str = "#{window_id} #{window_width} #{window_height} #{pane_id} #{pane_left} \
                     #{pane_top} #{pane_width} #{pane_height} #{pane_at_top} \
                     #{pane-border-status} #{?pane_floating_flag,1,0}";

/// One line per zoomed window: its id, then its layout as shown and
/// unzoomed, neither with a space.
const ZOOMED: &str = "#{window_id} #{window_visible_layout} #{window_layout}";

/// One line per client: its last activity, and the active pane of the
/// window it shows.
const CLIENTS: &str = "#{client_activity} #{pane_id}";

/// The live layout, as one line of JSON, or why there's none.
pub(super) fn report_layout(given: &Given) -> Result<String, String> {
    let server = Server::of(given)?;
    let panes = server.run(&["list-panes", "-a", "-F", PANES])?;
    let clients = server.run(&["list-clients", "-F", CLIENTS])?;
    let filter = "#{window_zoomed_flag}";
    let windows = server.run(&["list-windows", "-a", "-f", filter, "-F", ZOOMED])?;
    Ok(layout(&panes, &clients, &windows)?.to_string())
}

/// The focused pane, as `report_layout` reports it, if any.
pub(super) fn focused(server: &Server) -> Result<Option<String>, String> {
    Ok(focus(&server.run(&["list-clients", "-F", CLIENTS])?))
}

/// The report `report_layout` prints, from what `list-panes -a -F PANES`,
/// `list-clients -F CLIENTS` and the zoomed windows' `list-windows -a -F
/// ZOOMED` printed.
fn layout(panes: &[u8], clients: &[u8], windows: &[u8]) -> Result<Value, String> {
    let panes = std::str::from_utf8(panes).map_err(|_| "list-panes printed bytes not UTF-8")?;
    let zoomed = zoomed(windows)?;
    let mut areas: Vec<(String, Vec<Value>)> = Vec::new();
    let mut hidden: Vec<(String, Vec<Value>)> = Vec::new();
    let mut seen = Vec::new();
    for line in panes.lines().filter(|l| !l.trim().is_empty()) {
        let p = Pane::parse(line).ok_or_else(|| format!("list-panes printed {line:?}"))?;
        if seen.contains(&p.id) {
            continue;
        }
        seen.push(p.id.clone());
        let cell = zoomed
            .iter()
            .filter(|z| z.window == p.window)
            .find_map(|z| z.hidden.iter().find(|(id, _)| *id == p.id));
        let (into, terminal) = match cell {
            Some((_, cell)) => (&mut hidden, p.hidden_terminal(*cell)),
            None => (&mut areas, p.terminal()),
        };
        match into.iter_mut().find(|(id, _)| *id == p.window) {
            Some((_, terminals)) => terminals.push(terminal),
            None => into.push((p.window, vec![terminal])),
        }
    }
    let area = |id: String, terminals: Vec<Value>| json!({ "id": id, "terminals": terminals });
    let mut all = Vec::new();
    for (id, terminals) in areas {
        let behind = hidden.iter().position(|(w, _)| *w == id);
        let behind = behind.map(|at| hidden.remove(at));
        all.push(area(id, terminals));
        if let Some((id, terminals)) = behind {
            all.push(area(format!("{id}~hidden"), terminals));
        }
    }
    for (id, terminals) in hidden {
        all.push(area(format!("{id}~hidden"), terminals));
    }
    let areas = all;
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

/// The top left of the pane `line`, a line of `PANES`, describes, in
/// cells of its window, as `report_layout` gives its rect; and its
/// window's size. None if it can't read it.
pub(super) fn reported_corner(line: &[u8]) -> Option<([i64; 2], [u32; 2])> {
    let line = std::str::from_utf8(line).ok()?;
    let p = Pane::parse(line.lines().next()?)?;
    let ([x, y, _, _], _) = p.reported();
    let [ww, wh] = p.window_size;
    Some(([x as i64, y as i64], [ww as u32, wh as u32]))
}

/// A zoomed window: the panes its zoom hides, with their cells in its
/// unzoomed layout.
struct Zoomed {
    window: String,
    hidden: Vec<(String, [u32; 4])>,
}

/// The zoomed windows `list-windows -a -f '#{window_zoomed_flag}' -F
/// ZOOMED` printed. A hidden pane is a tiled one of `#{window_layout}`,
/// the unzoomed layout, that `#{window_visible_layout}` hasn't: its own
/// `pane_*` are stale, or the whole window for one made while zoomed.
fn zoomed(windows: &[u8]) -> Result<Vec<Zoomed>, String> {
    let windows =
        std::str::from_utf8(windows).map_err(|_| "list-windows printed bytes not UTF-8")?;
    let mut all = Vec::new();
    for line in windows.lines().filter(|l| !l.trim().is_empty()) {
        let bad = || format!("list-windows printed {line:?}");
        let [window, visible, layout] = line.split(' ').collect::<Vec<_>>()[..] else {
            return Err(bad());
        };
        if !window.starts_with('@') {
            return Err(bad());
        }
        let visible = parse(visible)?;
        let shown: Vec<&str> = visible.panes().iter().filter_map(|c| c.pane()).collect();
        let layout = parse(layout)?;
        let hidden = layout
            .panes()
            .into_iter()
            .filter(|c| !c.floating)
            .filter_map(|c| Some((c.pane()?, c.rect)))
            .filter(|(id, _)| !shown.contains(id))
            .map(|(id, rect)| (id.to_string(), rect))
            .collect();
        all.push(Zoomed {
            window: window.to_string(),
            hidden,
        });
    }
    Ok(all)
}

/// One line of `list-panes -a -F PANES`.
struct Pane {
    window: String,
    id: String,
    window_size: [f64; 2],
    /// Left, top, width, height, in cells: its content's. A float's may
    /// be off its window.
    cells: [f64; 4],
    at_top: bool,
    status_top: bool,
    status_bottom: bool,
    floating: bool,
}

impl Pane {
    fn parse(line: &str) -> Option<Pane> {
        let w: Vec<&str> = line.split(' ').collect();
        let [
            window,
            ww,
            wh,
            id,
            left,
            top,
            width,
            height,
            at_top,
            status,
            floating,
        ] = w.as_slice()
        else {
            return None;
        };
        let n = |s: &str| s.parse::<u32>().ok().map(f64::from);
        let at = |s: &str| s.parse::<i32>().ok().map(f64::from);
        let window_size = [n(ww)?, n(wh)?];
        if window_size.contains(&0.0) || !window.starts_with('@') || !is_pane_id(id.as_bytes()) {
            return None;
        }
        Some(Pane {
            window: window.to_string(),
            id: id.to_string(),
            window_size,
            cells: [at(left)?, at(top)?, n(width)?, n(height)?],
            at_top: *at_top == "1",
            status_top: *status == "top",
            status_bottom: *status == "bottom",
            floating: *floating == "1",
        })
    }

    /// Its rect, in cells of its window, and its inset, as `report_layout`
    /// gives them. A float is its frame, a cell outside its content on
    /// each side, whatever the border status. A tiled pane on the top row
    /// under `pane-border-status top` takes the status line's row.
    fn reported(&self) -> ([f64; 4], [u16; 4]) {
        let [x, y, w, h] = self.cells;
        if self.floating {
            ([x - 1.0, y - 1.0, w + 2.0, h + 2.0], [1; 4])
        } else if self.status_top && self.at_top && y >= 1.0 {
            ([x, y - 1.0, w, h + 1.0], [0, 1, 0, 0])
        } else {
            (self.cells, [0; 4])
        }
    }

    /// As `report_layout` gives it.
    fn terminal(&self) -> Value {
        let ([x, y, w, h], inset) = self.reported();
        let [ww, wh] = self.window_size;
        let mut t = json!({ "terminal": self.id, "rect": [x / ww, y / wh, w / ww, h / wh] });
        if self.floating {
            t["floating"] = true.into();
        }
        if inset != [0; 4] {
            t["inset"] = json!(inset);
        }
        t
    }

    /// As `report_layout` gives it hidden by a zoomed pane, its cell
    /// `[x, y, w, h]` in the unzoomed layout: as it would be shown, the
    /// border status line taking the top row of a cell on the window's
    /// top row, or the bottom row of one on its bottom row.
    fn hidden_terminal(&self, cell: [u32; 4]) -> Value {
        let [x, y, w, mut h] = cell.map(f64::from);
        let [ww, wh] = self.window_size;
        if self.status_bottom && y + h >= wh {
            h -= 1.0;
        }
        let mut t = json!({ "terminal": self.id, "rect": [x / ww, y / wh, w / ww, h / wh] });
        if self.status_top && y == 0.0 {
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
            b"",
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
        let report = layout(include_bytes!("fixtures/3.2a-panes.txt"), b"", b"").unwrap();
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

    /// A float as the report gives it: its frame, a cell outside its
    /// content on each side.
    fn float(id: &str, rect: [f64; 4]) -> Value {
        json!({ "terminal": id, "rect": rect, "floating": true, "inset": [1, 1, 1, 1] })
    }

    /// Recorded from tmux 3.8, no client attached, `pane-border-status
    /// top`: window @0 (80x24) is `%0 | %1`, with floats `%2`, from
    /// `new-pane -x 20 -y 6 -X 10 -Y 4`, and `%3`, from `-X -1 -Y -1`, its
    /// frame off the window's top left.
    #[test]
    fn tmux_3_8_a_float_is_its_frame_floating_with_an_inset_of_one() {
        let report = layout(include_bytes!("fixtures/3.8-floats.txt"), b"", b"").unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [
                    under_status("%0", [0.0, 0.0, 40.0 / 80.0, 1.0]),
                    under_status("%1", [41.0 / 80.0, 0.0, 39.0 / 80.0, 1.0]),
                    float("%2", [10.0 / 80.0, 4.0 / 24.0, 20.0 / 80.0, 6.0 / 24.0]),
                    float("%3", [-1.0 / 80.0, -1.0 / 24.0, 20.0 / 80.0, 6.0 / 24.0]),
                ]},
            ],
        });
        assert_eq!(report, want);
    }

    /// Recorded from tmux 3.7c, as the 3.8 one: 3.7's flags gave the
    /// content, so its frames are a cell bigger each side, and `%3`'s
    /// content is off the window too.
    #[test]
    fn tmux_3_7_a_float_off_the_window_is_reported_where_it_is() {
        let report = layout(include_bytes!("fixtures/3.7c-floats.txt"), b"", b"").unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [
                    under_status("%0", [0.0, 0.0, 40.0 / 80.0, 1.0]),
                    under_status("%1", [41.0 / 80.0, 0.0, 39.0 / 80.0, 1.0]),
                    float("%2", [9.0 / 80.0, 3.0 / 24.0, 22.0 / 80.0, 8.0 / 24.0]),
                    float("%3", [-2.0 / 80.0, -2.0 / 24.0, 22.0 / 80.0, 8.0 / 24.0]),
                ]},
            ],
        });
        assert_eq!(report, want);
    }

    /// Recorded from tmux 3.8, no client: window @0 (80x24) is `(%0 / %4)
    /// | (%2 / %3)`, `%0` zoomed and `%4` made while it was, so tmux says
    /// `%4` fills the window; @1 holds `%1`.
    #[test]
    fn tmux_3_8_zoomed_the_zoomed_pane_alone_and_the_hidden_ones_in_an_area_of_their_own() {
        let report = layout(
            include_bytes!("fixtures/3.8-zoomed-panes.txt"),
            b"",
            include_bytes!("fixtures/3.8-zoomed-windows.txt"),
        )
        .unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [t("%0", [0.0, 0.0, 1.0, 1.0])]},
                {"id": "@0~hidden", "terminals": [
                    t("%4", [0.0, 13.0 / 24.0, 40.0 / 80.0, 11.0 / 24.0]),
                    t("%2", [41.0 / 80.0, 0.0, 39.0 / 80.0, 12.0 / 24.0]),
                    t("%3", [41.0 / 80.0, 13.0 / 24.0, 39.0 / 80.0, 11.0 / 24.0]),
                ]},
                {"id": "@1", "terminals": [t("%1", [0.0, 0.0, 1.0, 1.0])]},
            ],
        });
        assert_eq!(report, want);
    }

    /// The same, from tmux 3.2a, its layouts in the older string, @0
    /// 80x23.
    #[test]
    fn tmux_3_2_zoomed_reads_the_hidden_panes_from_the_older_layout() {
        let report = layout(
            include_bytes!("fixtures/3.2a-zoomed-panes.txt"),
            b"",
            include_bytes!("fixtures/3.2a-zoomed-windows.txt"),
        )
        .unwrap();
        let want = json!({
            "areas": [
                {"id": "@0", "terminals": [t("%0", [0.0, 0.0, 1.0, 1.0])]},
                {"id": "@0~hidden", "terminals": [
                    t("%4", [0.0, 12.0 / 23.0, 40.0 / 80.0, 11.0 / 23.0]),
                    t("%2", [41.0 / 80.0, 0.0, 39.0 / 80.0, 11.0 / 23.0]),
                    t("%3", [41.0 / 80.0, 12.0 / 23.0, 39.0 / 80.0, 11.0 / 23.0]),
                ]},
                {"id": "@1", "terminals": [t("%1", [0.0, 0.0, 1.0, 1.0])]},
            ],
        });
        assert_eq!(report, want);
    }

    /// The 3.8 one with `pane-border-status` top, then bottom: a hidden
    /// pane on the top row has the status line as its inset, as a shown
    /// one does; one on the bottom row loses its last row to it.
    #[test]
    fn zoomed_with_a_border_status_the_hidden_panes_lose_its_row() {
        let report = layout(
            include_bytes!("fixtures/3.8-zoomed-top-panes.txt"),
            b"",
            include_bytes!("fixtures/3.8-zoomed-top-windows.txt"),
        )
        .unwrap();
        assert_eq!(
            report["areas"][0]["terminals"],
            json!([under_status("%0", [0.0, 0.0, 1.0, 1.0])])
        );
        assert_eq!(
            report["areas"][1]["terminals"],
            json!([
                t("%4", [0.0, 13.0 / 24.0, 40.0 / 80.0, 11.0 / 24.0]),
                under_status("%2", [41.0 / 80.0, 0.0, 39.0 / 80.0, 12.0 / 24.0]),
                t("%3", [41.0 / 80.0, 13.0 / 24.0, 39.0 / 80.0, 11.0 / 24.0]),
            ])
        );
        let report = layout(
            include_bytes!("fixtures/3.8-zoomed-bottom-panes.txt"),
            b"",
            include_bytes!("fixtures/3.8-zoomed-bottom-windows.txt"),
        )
        .unwrap();
        assert_eq!(
            report["areas"][0]["terminals"],
            json!([t("%0", [0.0, 0.0, 1.0, 23.0 / 24.0])])
        );
        assert_eq!(
            report["areas"][1]["terminals"],
            json!([
                t("%4", [0.0, 13.0 / 24.0, 40.0 / 80.0, 10.0 / 24.0]),
                t("%2", [41.0 / 80.0, 0.0, 39.0 / 80.0, 12.0 / 24.0]),
                t("%3", [41.0 / 80.0, 13.0 / 24.0, 39.0 / 80.0, 10.0 / 24.0]),
            ])
        );
    }

    #[test]
    fn a_zoomed_window_it_cant_read_fails_the_report() {
        let panes = b"@0 80 24 %0 0 0 80 24 1 off 0\n";
        assert!(layout(panes, b"", b"@0 nonsense\n").is_err());
        assert!(layout(panes, b"", b"@0 0000, 0000,\n").is_err());
    }

    #[test]
    fn focus_is_only_ever_a_pane_id() {
        let panes = b"@0 80 24 %0 0 0 80 24 1 off 0\n";
        let report = layout(panes, b"9 %\n8 %1x\n7 @1\n3 %0\n", b"").unwrap();
        assert_eq!(report["focus"], "%0");
        let report = layout(panes, b"9 %x\n", b"").unwrap();
        assert!(report.get("focus").is_none());
    }

    #[test]
    fn a_line_it_cant_read_fails_the_report() {
        assert!(layout(b"@0 80 24 %0 0 0 80\n", b"", b"").is_err());
        assert!(layout(b"@0 0 24 %0 0 0 80 24 1 off 0\n", b"", b"").is_err());
        assert!(layout(b"@0 80 24 %0 0 0 80 24 1 off\n", b"", b"").is_err());
        // no pane at all is no area
        assert_eq!(layout(b"", b"", b"").unwrap(), json!({ "areas": [] }));
    }
}
