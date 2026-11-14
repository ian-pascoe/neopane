//! herdr's split tree of one tab, read back from `pane.layout`, for
//! `resize`.
//!
//! `pane.layout` lists the panes' rects and the splits, each with an id,
//! its direction, ratio and rect, but not which split holds which pane.
//! herdr 0.9.5 names a split `split_<n>_root` or `split_<n>_<bits>`: `<n>`
//! its place in pre-order, and the bits its path from the root, `0` into
//! the first child and `1` into the second, as `layout.set_split_ratio`
//! takes it. herdr doesn't document that, so it is read strictly: an id
//! of any other form, or a tree whose splits and panes aren't where their
//! ids and herdr's own arithmetic put them, can't be read, and nothing is
//! resized.

use serde_json::Value;

use super::client::text;

/// Which way a split divides its part. herdr names them by where it puts
/// a new pane: `right` or `down`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Axis {
    /// Side by side: herdr's `right`.
    SideBySide,
    /// Stacked: herdr's `down`.
    Stacked,
}

impl Axis {
    /// herdr's name for it.
    pub(super) fn herdr(self) -> &'static str {
        match self {
            Axis::SideBySide => "right",
            Axis::Stacked => "down",
        }
    }

    /// The axis herdr names `name`, if any.
    fn of_herdr(name: &str) -> Option<Axis> {
        [Axis::SideBySide, Axis::Stacked]
            .into_iter()
            .find(|a| a.herdr() == name)
    }
}

/// A part of a tab's layout: a pane, or a split of two parts.
#[derive(Debug, PartialEq)]
pub(super) enum Node {
    Pane(String),
    Split(Split),
}

/// A split of a part of the layout in two, side by side or stacked.
#[derive(Debug, PartialEq)]
pub(super) struct Split {
    /// Its path from the root, as `layout.set_split_ratio` takes it.
    pub path: Vec<bool>,
    pub axis: Axis,
    /// Its cells along its direction: its width side by side, else its
    /// height.
    pub cells: u16,
    /// Left or top, and right or bottom.
    pub first: Box<Node>,
    pub second: Box<Node>,
}

impl Node {
    /// The path from this part to pane `pane`, if it holds it.
    pub(super) fn path_to(&self, pane: &str) -> Option<Vec<bool>> {
        match self {
            Node::Pane(id) => (id == pane).then(Vec::new),
            Node::Split(s) => {
                [(false, &s.first), (true, &s.second)]
                    .into_iter()
                    .find_map(|(side, part)| {
                        let mut rest = part.path_to(pane)?;
                        rest.insert(0, side);
                        Some(rest)
                    })
            }
        }
    }

    /// The part at `path` from this one, if there is one.
    pub(super) fn at(&self, path: &[bool]) -> Option<&Node> {
        match (path.split_first(), self) {
            (None, _) => Some(self),
            (Some((&side, rest)), Node::Split(s)) => {
                let part = if side { &s.second } else { &s.first };
                part.at(rest)
            }
            (Some(_), Node::Pane(_)) => None,
        }
    }

    /// How many panes a line through this part along `axis`, left to
    /// right side by side, else top to bottom, goes through at most.
    pub(super) fn panes_along(&self, axis: Axis) -> u16 {
        match self {
            Node::Pane(_) => 1,
            Node::Split(s) => {
                let (a, b) = (s.first.panes_along(axis), s.second.panes_along(axis));
                if s.axis == axis { a + b } else { a.max(b) }
            }
        }
    }
}

/// A rect in cells, as herdr keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    x: u16,
    y: u16,
    w: u16,
    h: u16,
}

impl Rect {
    /// A herdr rect, `{x, y, width, height}`.
    fn of(v: &Value) -> Option<Rect> {
        let cell = |k: &str| v[k].as_u64().and_then(|n| u16::try_from(n).ok());
        Some(Rect {
            x: cell("x")?,
            y: cell("y")?,
            w: cell("width")?,
            h: cell("height")?,
        })
    }

    /// Its two parts, split along `axis`, the first getting `ratio` of it,
    /// as herdr 0.9.5 rounds it.
    fn halves(self, axis: Axis, ratio: f32) -> (Rect, Rect) {
        if axis == Axis::SideBySide {
            let first = (f32::from(self.w) * ratio).round() as u16;
            let second = self.w.saturating_sub(first);
            (
                Rect { w: first, ..self },
                Rect {
                    x: self.x + first,
                    w: second,
                    ..self
                },
            )
        } else {
            let first = (f32::from(self.h) * ratio).round() as u16;
            let second = self.h.saturating_sub(first);
            (
                Rect { h: first, ..self },
                Rect {
                    y: self.y + first,
                    h: second,
                    ..self
                },
            )
        }
    }
}

/// A split as `pane.layout` lists it.
struct Listed {
    n: usize,
    path: Vec<bool>,
    axis: Axis,
    ratio: f32,
    rect: Rect,
}

/// The split tree of the tab `layout`, a `pane.layout`, describes; or why
/// it can't be read.
pub(super) fn tree(layout: &Value) -> Result<Node, String> {
    let tab = text(layout, "tab_id")?;
    let unread = || format!("herdr's layout of tab {tab} can't be read");
    let area = Rect::of(&layout["area"]).ok_or_else(unread)?;
    let mut splits = Vec::new();
    for s in layout["splits"].as_array().ok_or_else(unread)? {
        let id = text(s, "id")?;
        let (n, path) =
            split_id(id).ok_or_else(|| format!("herdr's split id {id:?} can't be read"))?;
        let axis = s["direction"]
            .as_str()
            .and_then(Axis::of_herdr)
            .ok_or_else(unread)?;
        splits.push(Listed {
            n,
            path,
            axis,
            ratio: s["ratio"].as_f64().ok_or_else(unread)? as f32,
            rect: Rect::of(&s["rect"]).ok_or_else(unread)?,
        });
    }
    let mut panes = Vec::new();
    for p in layout["panes"].as_array().ok_or_else(unread)? {
        let rect = Rect::of(&p["rect"]).ok_or_else(unread)?;
        panes.push((text(p, "pane_id")?.to_string(), rect));
    }
    let mut next = 0;
    let root = build(Vec::new(), area, &mut splits, &mut panes, &mut next).ok_or_else(unread)?;
    if !splits.is_empty() || !panes.is_empty() {
        return Err(unread());
    }
    Ok(root)
}

/// The part at `path`, its rect `rect`, taking its splits from `splits`
/// and its panes from `panes`, `next` being the pre-order number its
/// split, if it is one, must have; None if they don't fit.
fn build(
    path: Vec<bool>,
    rect: Rect,
    splits: &mut Vec<Listed>,
    panes: &mut Vec<(String, Rect)>,
    next: &mut usize,
) -> Option<Node> {
    let Some(at) = splits.iter().position(|s| s.path == path) else {
        let at = panes.iter().position(|(_, r)| *r == rect)?;
        return Some(Node::Pane(panes.swap_remove(at).0));
    };
    let split = splits.swap_remove(at);
    if split.n != *next || split.rect != rect {
        return None;
    }
    *next += 1;
    let (a, b) = rect.halves(split.axis, split.ratio);
    let mut into = |side: bool, rect: Rect| {
        let mut path = path.clone();
        path.push(side);
        build(path, rect, splits, panes, next).map(Box::new)
    };
    let first = into(false, a)?;
    let second = into(true, b)?;
    Some(Node::Split(Split {
        path,
        axis: split.axis,
        cells: match split.axis {
            Axis::SideBySide => rect.w,
            Axis::Stacked => rect.h,
        },
        first,
        second,
    }))
}

/// What split id `id` says: the split's place in pre-order, and its path
/// from the root, `false` into a split's first child, `true` into its
/// second; None for an id that isn't `split_<n>_root` or
/// `split_<n>_<bits>`, `<n>` a decimal with no leading zero.
pub(super) fn split_id(id: &str) -> Option<(usize, Vec<bool>)> {
    let rest = id.strip_prefix("split_")?;
    let (n, path) = rest.split_once('_')?;
    let decimal = !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit());
    if !decimal || (n.len() > 1 && n.starts_with('0')) {
        return None;
    }
    let n = n.parse().ok()?;
    if path == "root" {
        return Some((n, Vec::new()));
    }
    if path.is_empty() {
        return None;
    }
    let path = path
        .bytes()
        .map(|b| match b {
            b'0' => Some(false),
            b'1' => Some(true),
            _ => None,
        })
        .collect::<Option<_>>()?;
    Some((n, path))
}

#[cfg(test)]
mod tests {
    use super::super::client::result_of;
    use super::super::fake::recorded;
    use super::*;
    use serde_json::json;

    /// herdr 0.9.5's layout of `(P1 / (P4 | P5)) | (P2 / P3)`, 74x29.
    fn nested() -> Value {
        result_of(recorded("resize-layout").trim().as_bytes()).unwrap()["layout"].clone()
    }

    #[test]
    fn the_tree_puts_each_pane_where_its_splits_ids_say() {
        let root = tree(&nested()).unwrap();
        for (pane, path) in [
            ("w1:p1", vec![false, false]),
            ("w1:p4", vec![false, true, false]),
            ("w1:p5", vec![false, true, true]),
            ("w1:p2", vec![true, false]),
            ("w1:p3", vec![true, true]),
        ] {
            assert_eq!(root.path_to(pane), Some(path), "{pane}");
        }
        let Some(Node::Split(s)) = root.at(&[false, true]) else {
            panic!("no split at [false, true]");
        };
        assert_eq!((s.axis, s.cells), (Axis::SideBySide, 44));
        assert_eq!(root.path_to("w1:p9"), None);
        // across P1 / (P4 | P5), two panes at most; down it, two
        let left = root.at(&[false]).unwrap();
        let along = |n: &Node| {
            (
                n.panes_along(Axis::SideBySide),
                n.panes_along(Axis::Stacked),
            )
        };
        assert_eq!(along(left), (2, 2));
        assert_eq!(along(&root), (3, 2));
    }

    #[test]
    fn a_lone_pane_is_the_whole_tree() {
        let layout = json!({
            "tab_id": "w1:t1",
            "area": {"x": 0, "y": 1, "width": 74, "height": 29},
            "panes": [{"pane_id": "w1:p1", "rect": {"x": 0, "y": 1, "width": 74, "height": 29}}],
            "splits": [],
        });
        assert_eq!(tree(&layout), Ok(Node::Pane("w1:p1".into())));
    }

    #[test]
    fn a_split_id_that_cant_be_read_is_named() {
        let mut layout = nested();
        layout["splits"][2]["id"] = "split_2_0-1".into();
        assert_eq!(
            tree(&layout),
            Err("herdr's split id \"split_2_0-1\" can't be read".into())
        );
    }

    /// A layout herdr's ids, numbers or arithmetic don't account for, as a
    /// herdr that meant something else by them would give.
    #[test]
    fn splits_or_panes_not_where_their_ids_put_them_cant_be_read() {
        let unread = Err("herdr's layout of tab w1:t1 can't be read".into());
        let changed = |f: &dyn Fn(&mut Value)| {
            let mut layout = nested();
            f(&mut layout);
            tree(&layout)
        };
        // the bits the other way round: 1 into the first child
        assert_eq!(
            changed(&|l| {
                l["splits"][1]["id"] = "split_1_1".into();
                l["splits"][2]["id"] = "split_2_10".into();
                l["splits"][3]["id"] = "split_3_0".into();
            }),
            unread
        );
        // numbered other than in pre-order
        assert_eq!(
            changed(&|l| {
                l["splits"][2]["id"] = "split_3_01".into();
                l["splits"][3]["id"] = "split_2_1".into();
            }),
            unread
        );
        // a ratio that puts the border elsewhere
        assert_eq!(changed(&|l| l["splits"][0]["ratio"] = 0.5.into()), unread);
        // a pane no split leaves room for, or none where one leaves it
        assert_eq!(
            changed(&|l| l["panes"].as_array_mut().unwrap().push(json!({
                "pane_id": "w1:p9", "rect": {"x": 0, "y": 0, "width": 1, "height": 1}
            }))),
            unread
        );
        assert_eq!(
            changed(&|l| {
                l["panes"].as_array_mut().unwrap().remove(4);
            }),
            unread
        );
        // a split under a pane, or two at one path
        assert_eq!(
            changed(&|l| l["splits"].as_array_mut().unwrap().push(json!({
                "id": "split_4_00", "direction": "right", "ratio": 0.5,
                "rect": {"x": 0, "y": 0, "width": 44, "height": 15}
            }))),
            unread
        );
        assert_eq!(
            changed(&|l| l["splits"][3]["direction"] = "left".into()),
            unread
        );
    }

    #[test]
    fn a_split_id_is_its_number_and_path_from_the_root() {
        for (id, n, path) in [
            ("split_0_root", 0, vec![]),
            ("split_1_0", 1, vec![false]),
            ("split_1_1", 1, vec![true]),
            ("split_2_01", 2, vec![false, true]),
            ("split_12_1101", 12, vec![true, true, false, true]),
        ] {
            assert_eq!(split_id(id), Some((n, path)), "{id}");
        }
    }

    #[test]
    fn any_other_form_of_split_id_is_none() {
        for id in [
            "",
            "split",
            "split_",
            "split_0",
            "split_0_",
            "split__0",
            "split_a_0",
            "split_-1_0",
            "split_+1_0",
            "split_01_0",
            "split_0_2",
            "split_0_0x",
            "split_0_0_1",
            "split_0_root1",
            "split_0_ROOT",
            "Split_0_0",
            " split_0_0",
            "split_0_0 ",
            "split_0_true",
            "split_99999999999999999999999_0",
        ] {
            assert_eq!(split_id(id), None, "{id:?}");
        }
    }
}
