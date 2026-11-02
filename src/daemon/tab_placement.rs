//! Where the Rex panes of a tab page's windows go: the moves that arrange
//! them as Neovim's layout has its windows, at about their sizes. Rex has
//! the panes in the order their windows were made, which isn't where
//! plugins such as Diffview put them in the end.
//!
//! Each move puts a pane beside the first window of the part of the layout
//! it's moved into, splitting the room that window's pane has, so moves
//! done in order give the whole layout from any starting arrangement. A
//! move takes a pane out and splits another in, so a layout whose windows
//! and order haven't changed only has its splits resized.

use crate::nvim::windows::{Kind, Layout};

/// Put `win`'s pane right of (below, if not `side_by_side`) `anchor`'s,
/// leaving `anchor` the fraction `ratio` of the room they share.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PaneMove {
    pub win: i64,
    pub anchor: i64,
    pub side_by_side: bool,
    pub ratio: f64,
}

impl PaneMove {
    fn same_split(&self, b: &PaneMove) -> bool {
        self.win == b.win && self.anchor == b.anchor && self.side_by_side == b.side_by_side
    }
}

#[derive(Debug, Clone, Copy)]
pub struct TabPlacement {
    pub moves: [PaneMove; Layout::MAX_NODES],
    pub len: usize,
}

impl Default for TabPlacement {
    fn default() -> Self {
        TabPlacement {
            moves: [PaneMove::default(); Layout::MAX_NODES],
            len: 0,
        }
    }
}

/// Two placements are equal if they have the same moves.
impl PartialEq for TabPlacement {
    fn eq(&self, other: &TabPlacement) -> bool {
        self.items() == other.items()
    }
}

impl TabPlacement {
    pub fn items(&self) -> &[PaneMove] {
        &self.moves[0..self.len]
    }

    /// The moves that arrange the panes of `shown`, the windows that have one,
    /// as `layout` has them. Windows of `layout` not in `shown` are left out.
    pub fn plan(layout: &Layout, shown: &[i64]) -> TabPlacement {
        let mut result = TabPlacement::default();
        if layout.len == 0 {
            return result;
        }
        let mut planner = Planner {
            layout,
            shown,
            result: &mut result,
        };
        planner.arrange(0);
        result
    }
}

struct Planner<'a> {
    layout: &'a Layout,
    shown: &'a [i64],
    result: &'a mut TabPlacement,
}

impl Planner<'_> {
    /// The first window in the node's part of the layout that has a pane.
    fn first_shown(&self, at: usize) -> Option<i64> {
        let node = self.layout.nodes[at];
        if node.kind == Kind::Window {
            return if self.shown.contains(&node.win) {
                Some(node.win)
            } else {
                None
            };
        }
        for kid in self.layout.kids(at) {
            if let Some(win) = self.first_shown(kid) {
                return Some(win);
            }
        }
        None
    }

    /// How much room the node's part of the layout takes along an axis, in
    /// cells: none if it has no pane.
    fn extent(&self, at: usize, side_by_side: bool) -> u32 {
        let node = self.layout.nodes[at];
        if node.kind == Kind::Window {
            if self.first_shown(at).is_none() {
                return 0;
            }
            return (if side_by_side { node.w } else { node.h }).max(1);
        }
        // along its own direction its kids add up, across it they're alike
        let along = (node.kind == Kind::Row) == side_by_side;
        let mut total: u32 = 0;
        for kid in self.layout.kids(at) {
            let e = self.extent(kid, side_by_side);
            total = if along { total + e } else { total.max(e) };
        }
        total
    }

    fn arrange(&mut self, at: usize) {
        let node = self.layout.nodes[at];
        if node.kind == Kind::Window {
            return;
        }
        let side_by_side = node.kind == Kind::Row;
        // the kids with panes, and how much room each takes
        let mut with_panes = [0usize; Layout::MAX_NODES];
        let mut room = [0u32; Layout::MAX_NODES];
        let mut n: usize = 0;
        for kid in self.layout.kids(at) {
            let e = self.extent(kid, side_by_side);
            if e == 0 {
                continue;
            }
            with_panes[n] = kid;
            room[n] = e;
            n += 1;
        }
        if n >= 2 {
            // the first kid's pane keeps its place; each later one is put beside it, last first
            let anchor = self.first_shown(with_panes[0]).unwrap();
            let mut before: u32 = room[0..n].iter().sum();
            let mut i = n;
            while i > 1 {
                i -= 1;
                let total = before;
                before -= room[i];
                self.result.moves[self.result.len] = PaneMove {
                    win: self.first_shown(with_panes[i]).unwrap(),
                    anchor,
                    side_by_side,
                    ratio: before as f64 / total as f64,
                };
                self.result.len += 1;
            }
        }
        for &kid in &with_panes[0..n] {
            self.arrange(kid);
        }
    }
}

/// How far a window of a tab page's layout has got toward having a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowState {
    /// Neovim hasn't drawn it yet, so its pane isn't started.
    Undrawn,
    /// It was drawn and has closed.
    Gone,
    /// It's drawn and its pane isn't in Rex yet.
    Pending,
    /// Its pane is in Rex.
    Ready,
    /// It has no pane, and won't: it's closing, or its pane failed.
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Action {
    /// Not every window has a pane yet: placing the ones that do would only be moved again.
    Wait,
    /// Its windows are gone.
    Forget,
    Nothing,
    /// The windows or their order changed: make these moves.
    Rearrange(TabPlacement),
    /// Only the sizes changed: set these splits' ratios.
    Resize(TabPlacement),
}

/// What to do about the panes of a tab page whose windows are in `states`,
/// one for each node of `layout`, given what was `last` done. Sizes only
/// reach us when Neovim changed them, never from the user resizing panes in
/// Rex, so a size that changed is applied.
pub fn decide(layout: &Layout, states: &[WindowState], last: Option<&TabPlacement>) -> Action {
    let mut shown = [0i64; Layout::MAX_NODES];
    let mut n: usize = 0;
    for (node, state) in layout.items().iter().zip(states) {
        if node.kind != Kind::Window {
            continue;
        }
        match state {
            WindowState::Undrawn | WindowState::Pending => return Action::Wait,
            WindowState::Ready => {
                shown[n] = node.win;
                n += 1;
            }
            WindowState::Gone | WindowState::Skipped => {}
        }
    }
    if n == 0 {
        return Action::Forget;
    }
    let wanted = TabPlacement::plan(layout, &shown[0..n]);
    let Some(previous) = last else {
        return Action::Rearrange(wanted);
    };
    if previous.len != wanted.len {
        return Action::Rearrange(wanted);
    }
    let mut resized = false;
    for (a, b) in previous.items().iter().zip(wanted.items()) {
        if !a.same_split(b) {
            return Action::Rearrange(wanted);
        }
        if (a.ratio - b.ratio).abs() > 0.005 {
            resized = true;
        }
    }
    if resized {
        Action::Resize(wanted)
    } else {
        Action::Nothing
    }
}

/// A set of the nodes of a layout, by index (Zig's `std.StaticBitSet(Layout.max_nodes)`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct NodeSet(u64);

impl NodeSet {
    pub const EMPTY: NodeSet = NodeSet(0);

    pub fn set(&mut self, at: usize) {
        debug_assert!(at < Layout::MAX_NODES);
        self.0 |= 1 << at;
    }

    pub fn is_set(&self, at: usize) -> bool {
        debug_assert!(at < Layout::MAX_NODES);
        self.0 & (1 << at) != 0
    }
}

/// A tab page's layout as Neovim last said, and how far its panes have got to matching.
#[derive(Debug, Clone, Copy)]
pub struct Tab {
    pub layout: Layout,
    /// What was done to place its panes, the last time.
    pub placed: Option<TabPlacement>,
    /// By node: the window was drawn, so if it isn't now, it closed.
    pub seen: NodeSet,
}

impl Tab {
    /// A tab page with `layout`, whose windows haven't been seen.
    pub fn new(layout: Layout) -> Tab {
        Tab {
            layout,
            placed: None,
            seen: NodeSet::EMPTY,
        }
    }

    /// Takes a newer layout, keeping what's known of the windows it still has.
    pub fn replace(&mut self, layout: Layout) {
        let mut seen = NodeSet::EMPTY;
        for (at, node) in layout.items().iter().enumerate() {
            if node.kind != Kind::Window {
                continue;
            }
            for (old_at, old) in self.layout.items().iter().enumerate() {
                if old.kind == Kind::Window && old.win == node.win && self.seen.is_set(old_at) {
                    seen.set(at);
                }
            }
        }
        self.layout = layout;
        self.seen = seen;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nvim::windows::Node;

    /// Rex's side of a tab page's floating pane, as far as moves go: a tree of
    /// splits whose leaves are windows' panes.
    struct Rex {
        root: RexNode,
    }

    enum RexNode {
        Pane(i64),
        Split {
            side_by_side: bool,
            ratio: f64,
            before: Box<RexNode>,
            after: Box<RexNode>,
        },
    }

    #[derive(Clone, Copy)]
    struct Rect {
        x: f64,
        y: f64,
        w: f64,
        h: f64,
    }

    impl Rex {
        /// Panes made in order, each split off the first, to its right.
        fn made(wins: &[i64]) -> Rex {
            let mut rex = Rex {
                root: RexNode::Pane(wins[0]),
            };
            for &w in &wins[1..] {
                rex.insert(w, wins[0], true, 0.5);
            }
            rex
        }

        /// `session.move_block`'s split of `anchor`'s room: `win` goes after it,
        /// `anchor` getting `ratio` of it.
        fn insert(&mut self, win: i64, anchor: i64, side_by_side: bool, ratio: f64) {
            let link = Rex::find_link(&mut self.root, anchor).unwrap();
            let old = std::mem::replace(link, RexNode::Pane(0));
            *link = RexNode::Split {
                side_by_side,
                ratio,
                before: Box::new(old),
                after: Box::new(RexNode::Pane(win)),
            };
        }

        fn find_link(node: &mut RexNode, win: i64) -> Option<&mut RexNode> {
            match node {
                RexNode::Pane(w) => {
                    if *w == win {
                        Some(node)
                    } else {
                        None
                    }
                }
                RexNode::Split { before, after, .. } => {
                    Rex::find_link(before, win).or_else(|| Rex::find_link(after, win))
                }
            }
        }

        /// `session.move_block`: takes `win`'s pane out, then splits `anchor`'s.
        fn move_pane(&mut self, m: PaneMove) {
            // out: its parent split gives way to the other side
            assert!(Rex::take_out(&mut self.root, m.win));
            self.insert(m.win, m.anchor, m.side_by_side, m.ratio);
        }

        fn take_out(node: &mut RexNode, win: i64) -> bool {
            let RexNode::Split { before, after, .. } = node else {
                return false;
            };
            let before_is = matches!(**before, RexNode::Pane(w) if w == win);
            let after_is = matches!(**after, RexNode::Pane(w) if w == win);
            if before_is || after_is {
                let kept = if before_is {
                    std::mem::replace(&mut **after, RexNode::Pane(0))
                } else {
                    std::mem::replace(&mut **before, RexNode::Pane(0))
                };
                *node = kept;
                return true;
            }
            Rex::take_out(before, win) || Rex::take_out(after, win)
        }

        /// The pane of `win` in the unit square.
        fn rect_of(&self, win: i64) -> Option<Rect> {
            Rex::rect_in(
                &self.root,
                Rect {
                    x: 0.0,
                    y: 0.0,
                    w: 1.0,
                    h: 1.0,
                },
                win,
            )
        }

        fn rect_in(node: &RexNode, r: Rect, win: i64) -> Option<Rect> {
            match node {
                RexNode::Pane(w) => {
                    if *w == win {
                        Some(r)
                    } else {
                        None
                    }
                }
                RexNode::Split {
                    side_by_side,
                    ratio,
                    before,
                    after,
                } => {
                    let (a, b) = if *side_by_side {
                        (
                            Rect {
                                x: r.x,
                                y: r.y,
                                w: r.w * ratio,
                                h: r.h,
                            },
                            Rect {
                                x: r.x + r.w * ratio,
                                y: r.y,
                                w: r.w * (1.0 - ratio),
                                h: r.h,
                            },
                        )
                    } else {
                        (
                            Rect {
                                x: r.x,
                                y: r.y,
                                w: r.w,
                                h: r.h * ratio,
                            },
                            Rect {
                                x: r.x,
                                y: r.y + r.h * ratio,
                                w: r.w,
                                h: r.h * (1.0 - ratio),
                            },
                        )
                    };
                    Rex::rect_in(before, a, win).or_else(|| Rex::rect_in(after, b, win))
                }
            }
        }
    }

    fn expect_near(want: f64, got: f64) {
        assert!((want - got).abs() <= 0.02, "want {want}, got {got}");
    }

    fn node(kind: Kind, win: i64, w: u32, h: u32, end: usize) -> Node {
        Node {
            kind,
            win,
            w,
            h,
            end,
        }
    }

    #[test]
    fn a_file_panel_made_last_narrow_ends_up_leftmost_at_its_width() {
        // as Diffview makes a tab page: the old and new versions, then the file
        // panel, each split off the first window; Neovim shows it panel, old, new
        // at 20, 59 and 39 cells
        let panel = 1003;
        let old = 1001;
        let new = 1002;
        let mut layout = Layout::new(2);
        layout.len = 4;
        layout.nodes[0] = node(Kind::Row, 0, 118, 27, 4);
        layout.nodes[1] = node(Kind::Window, panel, 20, 27, 2);
        layout.nodes[2] = node(Kind::Window, old, 59, 27, 3);
        layout.nodes[3] = node(Kind::Window, new, 39, 27, 4);

        // in the order the windows were made: old, new, panel
        let mut rex = Rex::made(&[old, new, panel]);
        for m in TabPlacement::plan(&layout, &[old, new, panel]).items() {
            rex.move_pane(*m);
        }

        let p = rex.rect_of(panel).unwrap();
        let o = rex.rect_of(old).unwrap();
        let n = rex.rect_of(new).unwrap();
        expect_near(0.0, p.x);
        expect_near(20.0 / 118.0, p.w);
        expect_near(p.x + p.w, o.x);
        expect_near(59.0 / 118.0, o.w);
        expect_near(o.x + o.w, n.x);
        expect_near(39.0 / 118.0, n.w);
        for r in [p, o, n] {
            expect_near(0.0, r.y);
            expect_near(1.0, r.h);
        }
    }

    #[test]
    fn rows_and_columns_nest_from_whatever_order_the_panes_were_made_in() {
        // panel | (old over new), in a row of two whose second is a column
        let mut layout = Layout::new(2);
        layout.len = 5;
        layout.nodes[0] = node(Kind::Row, 0, 100, 40, 5);
        layout.nodes[1] = node(Kind::Window, 30, 25, 40, 2);
        layout.nodes[2] = node(Kind::Column, 0, 75, 40, 5);
        layout.nodes[3] = node(Kind::Window, 10, 75, 10, 4);
        layout.nodes[4] = node(Kind::Window, 20, 75, 30, 5);

        let mut rex = Rex::made(&[20, 30, 10]);
        for m in TabPlacement::plan(&layout, &[20, 30, 10]).items() {
            rex.move_pane(*m);
        }

        let panel = rex.rect_of(30).unwrap();
        let top = rex.rect_of(10).unwrap();
        let bottom = rex.rect_of(20).unwrap();
        expect_near(0.0, panel.x);
        expect_near(0.25, panel.w);
        expect_near(1.0, panel.h);
        expect_near(0.25, top.x);
        expect_near(0.75, top.w);
        expect_near(0.25, top.h);
        expect_near(0.0, top.y);
        expect_near(0.25, bottom.x);
        expect_near(0.75, bottom.h);
        expect_near(0.25, bottom.y);
    }

    #[test]
    fn windows_without_panes_are_left_out() {
        let mut layout = Layout::new(2);
        layout.len = 4;
        layout.nodes[0] = node(Kind::Row, 0, 90, 10, 4);
        layout.nodes[1] = node(Kind::Window, 1, 30, 10, 2);
        layout.nodes[2] = node(Kind::Window, 2, 30, 10, 3);
        layout.nodes[3] = node(Kind::Window, 3, 30, 10, 4);
        let placement = TabPlacement::plan(&layout, &[1, 3]);
        assert_eq!(1, placement.items().len());
        assert_eq!(3, placement.items()[0].win);
        assert_eq!(1, placement.items()[0].anchor);
        assert!(placement.items()[0].side_by_side);
        expect_near(0.5, placement.items()[0].ratio);
        assert_eq!(0, TabPlacement::plan(&layout, &[1]).items().len());
    }

    /// A row of windows `wins` with widths `widths`, 10 rows high.
    fn window_row(wins: &[i64], widths: &[u32]) -> Layout {
        let mut layout = Layout::new(2);
        let total: u32 = widths.iter().sum();
        layout.nodes[0] = node(Kind::Row, 0, total, 10, 0);
        for (i, (&win, &w)) in wins.iter().zip(widths).enumerate() {
            let at = i + 1;
            layout.nodes[at] = node(Kind::Window, win, w, 10, at + 1);
        }
        layout.len = wins.len() + 1;
        layout.nodes[0].end = layout.len;
        layout
    }

    fn rearranged(action: Action) -> TabPlacement {
        match action {
            Action::Rearrange(p) => p,
            other => panic!("not a rearrangement: {other:?}"),
        }
    }

    #[test]
    fn it_waits_until_every_window_has_a_pane_then_places_them_once() {
        use WindowState::*;
        let layout = window_row(&[1, 2, 3], &[20, 40, 40]);
        // a layout told before Neovim drew its windows, then a pane still to come
        assert_eq!(
            Action::Wait,
            decide(&layout, &[Skipped, Undrawn, Undrawn, Undrawn], None)
        );
        assert_eq!(
            Action::Wait,
            decide(&layout, &[Skipped, Ready, Ready, Pending], None)
        );
        // all there: moves, and none again until something changed
        let act = rearranged(decide(&layout, &[Skipped, Ready, Ready, Ready], None));
        assert_eq!(2, act.items().len());
        assert_eq!(
            Action::Nothing,
            decide(&layout, &[Skipped, Ready, Ready, Ready], Some(&act))
        );
    }

    #[test]
    fn closed_windows_and_windows_that_wont_have_panes_dont_hold_the_others_up() {
        use WindowState::*;
        let layout = window_row(&[1, 2, 3], &[20, 40, 40]);
        let act = rearranged(decide(&layout, &[Skipped, Ready, Gone, Skipped], None));
        assert_eq!(0, act.items().len());
        let two = rearranged(decide(&layout, &[Skipped, Ready, Ready, Skipped], None));
        assert_eq!(1, two.items().len());
        assert_eq!(2, two.items()[0].win);
        assert_eq!(1, two.items()[0].anchor);
    }

    #[test]
    fn a_tab_page_whose_windows_are_all_gone_is_forgotten() {
        use WindowState::*;
        let layout = window_row(&[1, 2], &[20, 40]);
        assert_eq!(
            Action::Forget,
            decide(&layout, &[Skipped, Gone, Gone], None)
        );
        assert_eq!(
            Action::Forget,
            decide(&layout, &[Skipped, Skipped, Gone], None)
        );
        assert_eq!(Action::Forget, decide(&Layout::new(2), &[], None));
    }

    #[test]
    fn a_window_added_or_an_order_changed_is_a_rearrangement_only_sizes_changed_is_a_resize() {
        use WindowState::*;
        let layout = window_row(&[1, 2, 3], &[20, 40, 40]);
        let ready = [Skipped, Ready, Ready, Ready];
        let placed = rearranged(decide(&layout, &ready, None));

        // the panel made wider, as Diffview's Panel:resize does
        let wider = window_row(&[1, 2, 3], &[30, 40, 40]);
        let Action::Resize(resized) = decide(&wider, &ready, Some(&placed)) else {
            panic!("not a resize");
        };
        assert_eq!(2, resized.items().len());
        assert!(resized.items()[1].ratio > placed.items()[1].ratio);

        // the same windows, the panel last
        let reordered = window_row(&[2, 3, 1], &[40, 40, 20]);
        assert!(matches!(
            decide(&reordered, &ready, Some(&placed)),
            Action::Rearrange(_)
        ));
        // a fourth window
        let four = window_row(&[1, 2, 3, 4], &[20, 40, 40, 10]);
        assert!(matches!(
            decide(&four, &[Skipped, Ready, Ready, Ready, Ready], Some(&placed)),
            Action::Rearrange(_)
        ));
        // one closing
        let gone = decide(&layout, &[Skipped, Gone, Ready, Ready], Some(&placed));
        assert!(matches!(gone, Action::Rearrange(_)));
    }

    #[test]
    fn a_newer_layout_keeps_which_windows_were_seen_by_window() {
        let mut tab = Tab::new(window_row(&[1, 2], &[20, 40]));
        tab.seen.set(1);
        // a window added in front: 1 moves from node 1 to node 2
        tab.replace(window_row(&[3, 1, 2], &[10, 20, 40]));
        assert!(!tab.seen.is_set(1));
        assert!(tab.seen.is_set(2));
        assert!(!tab.seen.is_set(3));
    }
}
