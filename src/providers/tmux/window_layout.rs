//! `#{window_layout}`, read: tmux's layout tree of a window, its splits and
//! their sizes. tmux 3.8 prints it as JSON (`{"V":2,"L":CELL}`); older ones
//! as `CSUM,WxH,X,Y` and then `,ID` for a pane, `{…}` for cells side by
//! side or `[…]` for cells stacked, the cells comma-separated, and 3.7
//! appends its floats as `<…>`. Sizes are content: siblings are one cell
//! apart, the border between them belonging to neither.

use serde_json::Value;

use super::is_pane_id;

/// Which way a split lays its cells out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Split {
    /// Side by side, left to right: tmux's `h`, `{…}`.
    SideBySide,
    /// Stacked, top to bottom: tmux's `v`, `[…]`.
    Stacked,
}

/// A cell of the tree: a pane, or a split of cells.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Cell {
    /// Left, top, width, height, in cells of the window.
    pub(super) rect: [u32; 4],
    /// A floating pane, above the tiled ones (3.7 and newer).
    pub(super) floating: bool,
    pub(super) kind: Kind,
}

#[derive(Debug, Clone, PartialEq)]
pub(super) enum Kind {
    /// A pane, by its id, `%N`.
    Pane(String),
    Split(Split, Vec<Cell>),
}

impl Cell {
    /// Its size along `split`'s axis: its width side by side, else its
    /// height.
    pub(super) fn size(&self, split: Split) -> u32 {
        match split {
            Split::SideBySide => self.rect[2],
            Split::Stacked => self.rect[3],
        }
    }

    /// Its panes, in order, floating ones too.
    pub(super) fn panes(&self) -> Vec<&Cell> {
        match &self.kind {
            Kind::Pane(_) => vec![self],
            Kind::Split(_, cells) => cells.iter().flat_map(Cell::panes).collect(),
        }
    }

    /// Its pane's id, if it is a pane.
    pub(super) fn pane(&self) -> Option<&str> {
        match &self.kind {
            Kind::Pane(id) => Some(id),
            Kind::Split(..) => None,
        }
    }
}

/// The tree `layout` gives, in either format, or why it can't be read.
pub(super) fn parse(layout: &str) -> Result<Cell, String> {
    let layout = layout.trim();
    let cell = if layout.starts_with('{') {
        v2(layout)
    } else {
        v1(layout)
    };
    cell.ok_or_else(|| format!("can't read the window layout {layout:?}"))
}

fn v2(layout: &str) -> Option<Cell> {
    let top: Value = serde_json::from_str(layout).ok()?;
    v2_cell(top.get("L")?)
}

fn v2_cell(cell: &Value) -> Option<Cell> {
    let n = |key: &str| cell.get(key)?.as_u64().and_then(|n| u32::try_from(n).ok());
    let at = |key: &str| {
        let v = cell.get(key)?.as_i64()?;
        u32::try_from(v.max(0)).ok()
    };
    let rect = [at("x")?, at("y")?, n("w")?, n("h")?];
    let split = match cell.get("t")?.as_str()? {
        "h" => Split::SideBySide,
        "v" => Split::Stacked,
        "p" => {
            let id = cell.get("I")?.as_str()?;
            if !is_pane_id(id.as_bytes()) {
                return None;
            }
            return Some(Cell {
                rect,
                floating: cell.get("z").is_some(),
                kind: Kind::Pane(id.to_string()),
            });
        }
        _ => return None,
    };
    let cells = cell
        .get("c")?
        .as_array()?
        .iter()
        .map(v2_cell)
        .collect::<Option<Vec<_>>>()?;
    if cells.is_empty() {
        return None;
    }
    Some(Cell {
        rect,
        floating: false,
        kind: Kind::Split(split, cells),
    })
}

fn v1(layout: &str) -> Option<Cell> {
    // the checksum, four hex digits, isn't checked: tmux printed it
    let (sum, rest) = layout.split_once(',')?;
    if sum.len() != 4 || !sum.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let mut r = Reader(rest.as_bytes());
    let mut root = r.cell()?;
    if r.eat(b'<') {
        // 3.7's floats, after the tree: mark them where the tree has them
        let mut floats = Vec::new();
        loop {
            floats.push(r.cell()?);
            if !r.eat(b',') {
                break;
            }
        }
        if !r.eat(b'>') {
            return None;
        }
        mark_floating(&mut root, &floats);
    }
    r.0.is_empty().then_some(root)
}

fn mark_floating(cell: &mut Cell, floats: &[Cell]) {
    match &mut cell.kind {
        Kind::Pane(id) => {
            if floats.iter().any(|f| f.pane() == Some(id)) {
                cell.floating = true;
            }
        }
        Kind::Split(_, cells) => cells.iter_mut().for_each(|c| mark_floating(c, floats)),
    }
}

/// What is left of a v1 layout to read.
struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn eat(&mut self, b: u8) -> bool {
        match self.0.split_first() {
            Some((&first, rest)) if first == b => {
                self.0 = rest;
                true
            }
            _ => false,
        }
    }

    fn number(&mut self) -> Option<u32> {
        let end = self.0.iter().position(|b| !b.is_ascii_digit());
        let end = end.unwrap_or(self.0.len());
        let (digits, rest) = self.0.split_at(end);
        self.0 = rest;
        std::str::from_utf8(digits).ok()?.parse().ok()
    }

    /// `WxH,X,Y` and then `,ID`, `{…}` or `[…]`.
    fn cell(&mut self) -> Option<Cell> {
        let w = self.number()?;
        let h = self.eat(b'x').then(|| self.number())??;
        let x = self.eat(b',').then(|| self.number())??;
        let y = self.eat(b',').then(|| self.number())??;
        let rect = [x, y, w, h];
        let (split, close) = if self.eat(b'{') {
            (Split::SideBySide, b'}')
        } else if self.eat(b'[') {
            (Split::Stacked, b']')
        } else {
            let id = self.eat(b',').then(|| self.number())??;
            return Some(Cell {
                rect,
                floating: false,
                kind: Kind::Pane(format!("%{id}")),
            });
        };
        let mut cells = vec![self.cell()?];
        while self.eat(b',') {
            cells.push(self.cell()?);
        }
        self.eat(close).then_some(Cell {
            rect,
            floating: false,
            kind: Kind::Split(split, cells),
        })
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    pub(in crate::providers::tmux) fn pane(id: &str, rect: [u32; 4]) -> Cell {
        Cell {
            rect,
            floating: false,
            kind: Kind::Pane(id.into()),
        }
    }

    pub(in crate::providers::tmux) fn split(
        split: Split,
        rect: [u32; 4],
        cells: Vec<Cell>,
    ) -> Cell {
        Cell {
            rect,
            floating: false,
            kind: Kind::Split(split, cells),
        }
    }

    use Split::{SideBySide as H, Stacked as V};

    /// Recorded from tmux 3.8, a 100x40 window: `((%0 | %3) / %2) | %1`.
    #[test]
    fn tmux_3_8_json_gives_the_tree_with_content_sizes() {
        let got = parse(include_str!("fixtures/3.8-layout-pa-q-c.txt")).unwrap();
        let want = split(
            H,
            [0, 0, 100, 40],
            vec![
                split(
                    V,
                    [0, 0, 50, 40],
                    vec![
                        split(
                            H,
                            [0, 0, 50, 20],
                            vec![pane("%0", [0, 0, 25, 20]), pane("%3", [26, 0, 24, 20])],
                        ),
                        pane("%2", [0, 21, 50, 19]),
                    ],
                ),
                pane("%1", [51, 0, 49, 40]),
            ],
        );
        assert_eq!(got, want);
    }

    /// Recorded from tmux 3.2a, an 80x23 window: `((%0 | %3) / %2) | %1`.
    #[test]
    fn tmux_3_2_string_gives_the_same_tree() {
        let got = parse(include_str!("fixtures/3.2a-layout-pa-q-c.txt")).unwrap();
        let want = split(
            H,
            [0, 0, 80, 23],
            vec![
                split(
                    V,
                    [0, 0, 40, 23],
                    vec![
                        split(
                            H,
                            [0, 0, 40, 11],
                            vec![pane("%0", [0, 0, 20, 11]), pane("%3", [21, 0, 19, 11])],
                        ),
                        pane("%2", [0, 12, 40, 11]),
                    ],
                ),
                pane("%1", [41, 0, 39, 23]),
            ],
        );
        assert_eq!(got, want);
    }

    /// The same 100x40 layout, `((P | A) / (X | Y)) | C`, recorded from
    /// both: either format gives one tree.
    #[test]
    fn both_formats_of_one_layout_read_alike() {
        let json = parse(include_str!("fixtures/3.8-layout-pa-xy-c.txt")).unwrap();
        let string = parse(include_str!("fixtures/3.2a-layout-pa-xy-c.txt")).unwrap();
        assert_eq!(json, string);
        let ids: Vec<_> = json.panes().iter().filter_map(|c| c.pane()).collect();
        assert_eq!(ids, ["%4", "%7", "%6", "%8", "%5"]);
    }

    /// 3.8 makes a float a cell of the root with `z`; 3.7 appends its
    /// floats as `<…>` (both as the research recorded them).
    #[test]
    fn floats_are_marked_floating() {
        let json = r#"{"V":2,"L":{"t":"v","w":80,"h":24,"x":0,"y":0,"c":[{"t":"p","w":80,"h":24,"x":0,"y":0,"a":true,"i":0,"I":"%0"},{"t":"p","w":18,"h":4,"x":11,"y":5,"z":0,"i":1,"I":"%1"}]}}"#;
        let string = "eb65,80x24,0,0[80x24,0,0,0,20x6,10,4,1]<20x6,10,4,1>";
        for layout in [json, string] {
            let floating: Vec<_> = parse(layout)
                .unwrap()
                .panes()
                .iter()
                .map(|c| (c.pane().unwrap().to_string(), c.floating))
                .collect();
            assert_eq!(
                floating,
                [("%0".to_string(), false), ("%1".to_string(), true)]
            );
        }
    }

    #[test]
    fn a_layout_it_cant_read_is_an_error() {
        for bad in [
            "",
            "0000,",
            "20c8,80x23,0,0",
            "20c8,80x23,0,0{40x23,0,0,0",
            "20c8,80x23,0,0,0 trailing",
            r#"{"V":2,"L":{"t":"p","w":80,"h":24,"x":0,"y":0,"I":"@1"}}"#,
            r#"{"V":2,"L":{"t":"h","w":80,"h":24,"x":0,"y":0,"c":[]}}"#,
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }
}
