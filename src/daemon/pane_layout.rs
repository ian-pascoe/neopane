//! A pane layout: how a window pane's windows share it, side by side or one
//! above another, as Neovim lays out splits (ADR 0009). neorex, not Neovim,
//! splits, closes, resizes and lays them out, since every window a pane shows
//! stays external.
//!
//! The layout keeps shares, not cells: each node's weight among its siblings.
//! Cells come from `lay_out`, for the pane's size and its current window, so
//! a pane that shrinks and grows back gives its windows their sizes back.
//!
//! Along a row, each window's slot is its columns and the separator right of
//! it; the last one's separator falls past the row's edge, so a row of `w`
//! columns lays out `w + 1` columns of slots, as Neovim gives 40 and 39
//! columns to the two halves of 80. Down a column, each window's slot is its
//! rows, its statusline included.
//!
//! Too little room squeezes windows to a floor: a column and its separator
//! across, a row (its statusline) down, two rows (one of text) for the
//! current window. Past that, the windows farthest from the current one are
//! cut, until they become current.

/// Which way a row or column runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Dir {
    /// Side by side, as `:vsplit` makes them.
    Row,
    /// One above another, as `:split` makes them.
    Column,
}

/// Which way a size runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    Cols,
    Rows,
}

impl Dir {
    /// The axis a row or column shares out.
    fn axis(self) -> Axis {
        match self {
            Dir::Row => Axis::Cols,
            Dir::Column => Axis::Rows,
        }
    }
}

/// The options that decide where space goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Options {
    /// `'equalalways'`: a split or close evens out its row or column.
    pub equalalways: bool,
    /// `'splitbelow'`: a closing window's rows go to the window above it.
    pub splitbelow: bool,
    /// `'splitright'`: a closing window's columns go to the window left of it.
    pub splitright: bool,
}

impl Default for Options {
    /// Neovim's defaults.
    fn default() -> Options {
        Options {
            equalalways: true,
            splitbelow: false,
            splitright: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Win(i64),
    Split(Dir, Vec<Kid>),
}

#[derive(Debug, Clone, PartialEq)]
struct Kid {
    /// Its share of its row or column, against its siblings'.
    weight: f64,
    node: Node,
}

/// A window as laid out, in cells of the pane's area: its rows include its
/// statusline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Placed {
    pub win: i64,
    pub row: u32,
    pub col: u32,
    pub cols: u32,
    pub rows: u32,
}

/// A separator column between windows side by side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Separator {
    pub row: u32,
    pub col: u32,
    pub rows: u32,
}

/// What `lay_out` gives: the windows shown, and the separators between.
/// Windows cut for want of room aren't in it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Laid {
    pub wins: Vec<Placed>,
    pub separators: Vec<Separator>,
}

impl Laid {
    /// Where window `win` was laid out, if it shows.
    pub(crate) fn of(&self, win: i64) -> Option<Placed> {
        self.wins.iter().copied().find(|p| p.win == win)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PaneLayout {
    root: Node,
}

impl PaneLayout {
    /// A layout of one window.
    pub(crate) fn new(win: i64) -> PaneLayout {
        PaneLayout {
            root: Node::Win(win),
        }
    }

    /// Its windows, top left first.
    pub(crate) fn windows(&self) -> Vec<i64> {
        let mut out = Vec::new();
        collect(&self.root, &mut out);
        out
    }

    pub(crate) fn contains(&self, win: i64) -> bool {
        path_to(&self.root, win).is_some()
    }

    /// Whether it holds more than one window.
    pub(crate) fn is_split(&self) -> bool {
        matches!(self.root, Node::Split(..))
    }

    /// Window `new` takes half of `origin`'s place, side by side or one above
    /// the other, before or after it. With `'equalalways'` its row or column
    /// is then evened out. An origin not in the layout splits the whole of it.
    pub(crate) fn split(
        &mut self,
        new: i64,
        origin: i64,
        side_by_side: bool,
        before: bool,
        equalalways: bool,
    ) {
        if self.contains(new) {
            return;
        }
        let dir = if side_by_side { Dir::Row } else { Dir::Column };
        let path = path_to(&self.root, origin).unwrap_or_default();
        // the origin's row or column, if it runs this way: a sibling there
        if let Some((&at, up)) = path.split_last()
            && let Node::Split(d, kids) = node_at_mut(&mut self.root, up)
            && *d == dir
        {
            let half = kids[at].weight / 2.0;
            kids[at].weight = half;
            let i = if before { at } else { at + 1 };
            kids.insert(
                i,
                Kid {
                    weight: half,
                    node: Node::Win(new),
                },
            );
            if equalalways {
                even(&mut self.root, dir);
            }
            return;
        }
        // else the origin becomes a row or column of the two
        let target = node_at_mut(&mut self.root, &path);
        let old = std::mem::replace(target, Node::Win(new));
        let mut pair = vec![
            Kid {
                weight: 1.0,
                node: old,
            },
            Kid {
                weight: 1.0,
                node: Node::Win(new),
            },
        ];
        if before {
            pair.swap(0, 1);
        }
        *target = Node::Split(dir, pair);
        if equalalways {
            even(&mut self.root, dir);
        }
    }

    /// Takes window `win` out. Its space goes to the next window, or the one
    /// before with `'splitbelow'` down a column or `'splitright'` along a row,
    /// or when there's no next; with `'equalalways'` its row or column is then
    /// evened out. False once the layout has no window left (it keeps the
    /// last one's).
    pub(crate) fn close(&mut self, win: i64, opts: Options) -> bool {
        let Some(path) = path_to(&self.root, win) else {
            return true;
        };
        let Some((&at, up)) = path.split_last() else {
            return false;
        };
        let Node::Split(dir, kids) = node_at_mut(&mut self.root, up) else {
            unreachable!("a window's parent is a split");
        };
        let dir = *dir;
        let gone = kids.remove(at);
        let back = match dir {
            Dir::Row => opts.splitright,
            Dir::Column => opts.splitbelow,
        };
        let to = if (back && at > 0) || at == kids.len() {
            at - 1
        } else {
            at
        };
        kids[to].weight += gone.weight;
        if kids.len() == 1 {
            self.collapse(up);
        }
        if opts.equalalways {
            even(&mut self.root, dir);
        }
        true
    }

    /// The row or column at `up`, left with one kid, becomes that kid; a row
    /// left in a row joins it.
    fn collapse(&mut self, up: &[usize]) {
        let Node::Split(_, kids) = node_at_mut(&mut self.root, up) else {
            unreachable!()
        };
        // a row or column of one is that one
        let only = kids.pop().unwrap().node;
        let parent = node_at_mut(&mut self.root, up);
        *parent = only;
        // a row in a row is one row
        if let Some((&pat, gup)) = up.split_last()
            && let Node::Split(gdir, gkids) = node_at_mut(&mut self.root, gup)
            && let Node::Split(kdir, _) = &gkids[pat].node
            && kdir == gdir
        {
            let kid = gkids.remove(pat);
            let Node::Split(_, inner) = kid.node else {
                unreachable!()
            };
            let total: f64 = inner.iter().map(|k| k.weight).sum();
            for (n, mut k) in inner.into_iter().enumerate() {
                k.weight = kid.weight * k.weight / total;
                gkids.insert(pat + n, k);
            }
        }
    }

    /// Neovim gave window `win` `cells` along `axis` (`:resize`, `<C-w>+`,
    /// `:vertical resize`): its width, or its height with its statusline.
    /// The window, or the row or column holding it, takes that much of the
    /// column or row around it, as laid out in `area` with `current` current,
    /// and its neighbours give or take the difference: the next ones first,
    /// then those before. A size with no row or column to share is ignored.
    pub(crate) fn resize(
        &mut self,
        win: i64,
        axis: Axis,
        cells: u32,
        area: [u32; 2],
        current: i64,
    ) {
        let Some(path) = path_to(&self.root, win) else {
            return;
        };
        // the nearest row or column sharing out this axis
        let mut depth = path.len();
        loop {
            if depth == 0 {
                return;
            }
            if let Node::Split(d, _) = node_at(&self.root, &path[..depth - 1])
                && d.axis() == axis
            {
                break;
            }
            depth -= 1;
        }
        let up = &path[..depth - 1];
        let at = path[depth - 1];
        let extent = extent_of(&self.root, up, area, current);
        let Node::Split(dir, kids) = node_at_mut(&mut self.root, up) else {
            unreachable!()
        };
        let total = match dir {
            Dir::Row => extent[0] + 1,
            Dir::Column => extent[1],
        };
        let floors: Vec<u32> = kids.iter().map(|k| floor(&k.node, *dir, current)).collect();
        let mut sizes = share_out(kids, &floors, total, current_kid(kids, current));
        if sizes.iter().any(|&s| s == 0) {
            // some are cut: no room to give
            return;
        }
        let slot = match dir {
            Dir::Row => cells + 1,
            Dir::Column => cells,
        };
        let others: u32 = floors
            .iter()
            .enumerate()
            .filter(|&(i, _)| i != at)
            .map(|(_, f)| f)
            .sum();
        let want = slot.clamp(floors[at], total.saturating_sub(others).max(floors[at]));
        let order: Vec<usize> = (at + 1..kids.len()).chain((0..at).rev()).collect();
        if want > sizes[at] {
            let mut need = want - sizes[at];
            for &i in &order {
                let give = need.min(sizes[i] - floors[i]);
                sizes[i] -= give;
                sizes[at] += give;
                need -= give;
            }
        } else {
            let spare = sizes[at] - want;
            sizes[at] = want;
            sizes[order[0]] += spare;
        }
        for (k, s) in kids.iter_mut().zip(sizes) {
            k.weight = f64::from(s);
        }
    }

    /// The windows and separators in an area of `area` cells, cols x rows,
    /// with `current` the window that keeps a row of text and isn't cut.
    pub(crate) fn lay_out(&self, area: [u32; 2], current: i64) -> Laid {
        let mut out = Laid::default();
        place(&self.root, 0, 0, area[0], area[1], current, &mut out);
        out
    }
}

fn collect(node: &Node, out: &mut Vec<i64>) {
    match node {
        Node::Win(w) => out.push(*w),
        Node::Split(_, kids) => kids.iter().for_each(|k| collect(&k.node, out)),
    }
}

/// The kids' indices down to window `win`.
fn path_to(node: &Node, win: i64) -> Option<Vec<usize>> {
    match node {
        Node::Win(w) => (*w == win).then(Vec::new),
        Node::Split(_, kids) => kids.iter().enumerate().find_map(|(i, k)| {
            let mut p = path_to(&k.node, win)?;
            p.insert(0, i);
            Some(p)
        }),
    }
}

fn node_at<'a>(node: &'a Node, path: &[usize]) -> &'a Node {
    match (path.split_first(), node) {
        (None, _) => node,
        (Some((&i, rest)), Node::Split(_, kids)) => node_at(&kids[i].node, rest),
        _ => unreachable!("a path into a window"),
    }
}

fn node_at_mut<'a>(node: &'a mut Node, path: &[usize]) -> &'a mut Node {
    match (path.split_first(), node) {
        (None, node) => node,
        (Some((&i, rest)), Node::Split(_, kids)) => node_at_mut(&mut kids[i].node, rest),
        _ => unreachable!("a path into a window"),
    }
}

/// How many windows `node` has across `dir`: what evening out shares by,
/// as Neovim evens windows, not rows and columns.
fn count(node: &Node, dir: Dir) -> u32 {
    match node {
        Node::Win(_) => 1,
        Node::Split(d, kids) if *d == dir => kids.iter().map(|k| count(&k.node, dir)).sum(),
        Node::Split(_, kids) => kids.iter().map(|k| count(&k.node, dir)).max().unwrap_or(1),
    }
}

/// Evens out every row (or column) in `node`, as Neovim evens the whole
/// tab page's windows that way.
fn even(node: &mut Node, dir: Dir) {
    let Node::Split(d, kids) = node else {
        return;
    };
    let same = *d == dir;
    for k in kids {
        if same {
            k.weight = f64::from(count(&k.node, dir));
        }
        even(&mut k.node, dir);
    }
}

fn contains(node: &Node, win: i64) -> bool {
    path_to(node, win).is_some()
}

/// The kid holding window `current`, if any.
fn current_kid(kids: &[Kid], current: i64) -> Option<usize> {
    kids.iter().position(|k| contains(&k.node, current))
}

/// The fewest cells `node`'s slot takes in a split running `dir`: across a
/// row a column and its separator for each window side by side, down a
/// column a row each, two for the current window.
fn floor(node: &Node, dir: Dir, current: i64) -> u32 {
    match (node, dir) {
        (Node::Win(_), Dir::Row) => 2,
        (Node::Win(w), Dir::Column) => {
            if *w == current {
                2
            } else {
                1
            }
        }
        (Node::Split(d, kids), _) if *d == dir => {
            kids.iter().map(|k| floor(&k.node, dir, current)).sum()
        }
        (Node::Split(_, kids), _) => kids
            .iter()
            .map(|k| floor(&k.node, dir, current))
            .max()
            .unwrap_or(1),
    }
}

/// `total` cells shared out among `kids` by weight, none under its floor.
/// Without room for every floor, the kids farthest from kid `current` (or
/// the first) get 0, cut, and what's left over goes to it.
fn share_out(kids: &[Kid], floors: &[u32], total: u32, current: Option<usize>) -> Vec<u32> {
    let n = kids.len();
    let mut sizes = vec![0u32; n];
    if floors.iter().sum::<u32>() > total {
        let c = current.unwrap_or(0);
        let mut used = floors[c].min(total);
        sizes[c] = used;
        for d in 1..n {
            for i in [c + d, c.wrapping_sub(d)] {
                if i < n && used + floors[i] <= total {
                    sizes[i] = floors[i];
                    used += floors[i];
                }
            }
        }
        // only a contiguous run around the current one shows
        let mut lo = c;
        while lo > 0 && sizes[lo - 1] > 0 {
            lo -= 1;
        }
        let mut hi = c;
        while hi + 1 < n && sizes[hi + 1] > 0 {
            hi += 1;
        }
        for (i, s) in sizes.iter_mut().enumerate() {
            if i < lo || i > hi {
                *s = 0;
            }
        }
        let shown: u32 = sizes.iter().sum();
        sizes[c] += total - shown;
        return sizes;
    }
    // water-fill: a kid whose share is under its floor gets its floor, and
    // the rest share what's left
    let mut fixed = vec![false; n];
    let mut ideal = vec![0f64; n];
    loop {
        let left = total - (0..n).filter(|&i| fixed[i]).map(|i| floors[i]).sum::<u32>();
        let weight: f64 = (0..n).filter(|&i| !fixed[i]).map(|i| kids[i].weight).sum();
        let mut again = false;
        let free: Vec<usize> = (0..n).filter(|&i| !fixed[i]).collect();
        for i in free {
            ideal[i] = if weight > 0.0 {
                f64::from(left) * kids[i].weight / weight
            } else {
                f64::from(left) / (n as f64)
            };
            if ideal[i] < f64::from(floors[i]) {
                fixed[i] = true;
                again = true;
            }
        }
        if !again {
            break;
        }
    }
    for i in 0..n {
        sizes[i] = if fixed[i] {
            floors[i]
        } else {
            (ideal[i] + 1e-9).floor() as u32
        };
    }
    // the cells rounding left go to the largest remainders, the first on a tie
    let mut short = total - sizes.iter().sum::<u32>();
    let mut order: Vec<usize> = (0..n).filter(|&i| !fixed[i]).collect();
    order.sort_by(|&a, &b| {
        let fa = ideal[a] - f64::from(sizes[a]);
        let fb = ideal[b] - f64::from(sizes[b]);
        fb.partial_cmp(&fa).unwrap().then(a.cmp(&b))
    });
    for &i in order.iter().cycle() {
        if short == 0 {
            break;
        }
        sizes[i] += 1;
        short -= 1;
    }
    sizes
}

/// Lays `node` out at `row`, `col`, `cols` x `rows` (its separator, if any,
/// not included).
fn place(node: &Node, row: u32, col: u32, cols: u32, rows: u32, current: i64, out: &mut Laid) {
    let (dir, kids) = match node {
        Node::Win(win) => {
            out.wins.push(Placed {
                win: *win,
                row,
                col,
                cols,
                rows,
            });
            return;
        }
        Node::Split(dir, kids) => (*dir, kids),
    };
    let floors: Vec<u32> = kids.iter().map(|k| floor(&k.node, dir, current)).collect();
    let total = match dir {
        Dir::Row => cols + 1,
        Dir::Column => rows,
    };
    let sizes = share_out(kids, &floors, total, current_kid(kids, current));
    let mut at = 0;
    for (k, &s) in kids.iter().zip(&sizes) {
        if s == 0 {
            continue;
        }
        match dir {
            Dir::Row => {
                let w = s - 1;
                place(&k.node, row, col + at, w, rows, current, out);
                if at + w < cols {
                    out.separators.push(Separator {
                        row,
                        col: col + at + w,
                        rows,
                    });
                }
            }
            Dir::Column => place(&k.node, row + at, col, cols, s, current, out),
        }
        at += s;
    }
}

/// The cols x rows of the node at `path`, as laid out in `area`.
fn extent_of(root: &Node, path: &[usize], area: [u32; 2], current: i64) -> [u32; 2] {
    let mut node = root;
    let mut ext = area;
    for &i in path {
        let Node::Split(dir, kids) = node else {
            unreachable!()
        };
        let floors: Vec<u32> = kids.iter().map(|k| floor(&k.node, *dir, current)).collect();
        let total = match dir {
            Dir::Row => ext[0] + 1,
            Dir::Column => ext[1],
        };
        let s = share_out(kids, &floors, total, current_kid(kids, current))[i];
        ext = match dir {
            Dir::Row => [s.saturating_sub(1), ext[1]],
            Dir::Column => [ext[0], s],
        };
        node = &kids[i].node;
    }
    ext
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each window's col, row, cols, rows, as laid out.
    fn rects(l: &PaneLayout, area: [u32; 2], current: i64) -> Vec<(i64, [u32; 4])> {
        l.lay_out(area, current)
            .wins
            .iter()
            .map(|p| (p.win, [p.col, p.row, p.cols, p.rows]))
            .collect()
    }

    const EQUAL: bool = true;
    const UNEVEN: bool = false;

    #[test]
    fn a_vertical_split_puts_the_new_window_beside_with_a_separator_between() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        // as Neovim splits 80 columns: 40, a separator, 39
        assert_eq!(
            vec![(1, [0, 0, 40, 23]), (2, [41, 0, 39, 23])],
            rects(&l, [80, 23], 1)
        );
        assert_eq!(
            vec![Separator {
                row: 0,
                col: 40,
                rows: 23
            }],
            l.lay_out([80, 23], 1).separators
        );
        // before: on the left
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, true, EQUAL);
        assert_eq!(vec![2, 1], l.windows());
        assert_eq!(
            vec![(2, [0, 0, 40, 23]), (1, [41, 0, 39, 23])],
            rects(&l, [80, 23], 1)
        );
    }

    #[test]
    fn a_horizontal_split_puts_the_new_window_above_or_below_each_with_its_statusline() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, false, true, EQUAL);
        // 23 rows: 11 and 10 of text, each with its statusline
        assert_eq!(
            vec![(2, [0, 0, 80, 12]), (1, [0, 12, 80, 11])],
            rects(&l, [80, 23], 1)
        );
        assert!(l.lay_out([80, 23], 1).separators.is_empty());
        let mut l = PaneLayout::new(1);
        l.split(2, 1, false, false, EQUAL);
        assert_eq!(vec![1, 2], l.windows());
    }

    #[test]
    fn a_split_takes_half_its_origins_place_and_equalalways_evens_the_row() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, UNEVEN);
        l.split(3, 2, true, false, UNEVEN);
        // 1 keeps half, 2 and 3 a quarter each: of 81 slot columns
        assert_eq!(
            vec![
                (1, [0, 0, 40, 10]),
                (2, [41, 0, 19, 10]),
                (3, [61, 0, 19, 10])
            ],
            rects(&l, [80, 10], 1)
        );
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        l.split(3, 2, true, false, EQUAL);
        assert_eq!(
            vec![
                (1, [0, 0, 26, 10]),
                (2, [27, 0, 26, 10]),
                (3, [54, 0, 26, 10])
            ],
            rects(&l, [80, 10], 1)
        );
    }

    #[test]
    fn a_split_across_the_origins_row_makes_a_column_in_its_place() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        l.split(3, 2, false, false, EQUAL);
        assert_eq!(
            vec![
                (1, [0, 0, 40, 20]),
                (2, [41, 0, 39, 10]),
                (3, [41, 10, 39, 10])
            ],
            rects(&l, [80, 20], 1)
        );
        // the separator runs down the whole column
        assert_eq!(
            vec![Separator {
                row: 0,
                col: 40,
                rows: 20
            }],
            l.lay_out([80, 20], 1).separators
        );
    }

    #[test]
    fn equalalways_evens_windows_not_rows_and_columns() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        l.split(3, 2, false, false, EQUAL);
        // 2 and 3 are one above the other: a third window across makes three
        // evened columns, the column of two one of them
        l.split(4, 3, true, false, EQUAL);
        let r = rects(&l, [89, 20], 1);
        assert_eq!((1, [0, 0, 29, 20]), r[0]);
        assert_eq!((2, [30, 0, 59, 10]), r[1]);
        assert_eq!((3, [30, 10, 29, 10]), r[2]);
        assert_eq!((4, [60, 10, 29, 10]), r[3]);
    }

    #[test]
    fn a_closing_windows_space_goes_to_the_next_or_with_splitright_the_one_before() {
        let opts = Options {
            equalalways: false,
            ..Options::default()
        };
        let three = || {
            let mut l = PaneLayout::new(1);
            l.split(2, 1, true, false, EQUAL);
            l.split(3, 2, true, false, EQUAL);
            l
        };
        let mut l = three();
        assert!(l.close(2, opts));
        assert_eq!(
            vec![(1, [0, 0, 26, 10]), (3, [27, 0, 53, 10])],
            rects(&l, [80, 10], 1)
        );
        let mut l = three();
        l.close(
            2,
            Options {
                splitright: true,
                ..opts
            },
        );
        assert_eq!(
            vec![(1, [0, 0, 53, 10]), (3, [54, 0, 26, 10])],
            rects(&l, [80, 10], 1)
        );
        // the last has no next: the one before takes it
        let mut l = three();
        l.close(3, opts);
        assert_eq!(
            vec![(1, [0, 0, 26, 10]), (2, [27, 0, 53, 10])],
            rects(&l, [80, 10], 1)
        );
        // splitbelow is for columns: along a row it changes nothing
        let mut l = three();
        l.close(
            2,
            Options {
                splitbelow: true,
                ..opts
            },
        );
        assert_eq!(
            vec![(1, [0, 0, 26, 10]), (3, [27, 0, 53, 10])],
            rects(&l, [80, 10], 1)
        );
        // with equalalways the rest are evened
        let mut l = three();
        l.close(2, Options::default());
        assert_eq!(
            vec![(1, [0, 0, 40, 10]), (3, [41, 0, 39, 10])],
            rects(&l, [80, 10], 1)
        );
    }

    #[test]
    fn closing_down_to_one_window_gives_it_the_whole_pane_and_the_last_ends_the_layout() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, false, false, EQUAL);
        assert!(l.close(1, Options::default()));
        assert!(!l.is_split());
        assert_eq!(vec![(2, [0, 0, 80, 20])], rects(&l, [80, 20], 2));
        assert!(!l.close(2, Options::default()));
    }

    #[test]
    fn a_row_left_alone_in_a_row_joins_it() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        l.split(3, 2, false, false, EQUAL);
        l.split(4, 3, true, false, EQUAL);
        // 2 closes: the column of 2 and the row of 3 and 4 is that row, in the
        // row of 1
        l.close(2, Options::default());
        assert_eq!(vec![1, 3, 4], l.windows());
        let r = rects(&l, [80, 10], 1);
        assert_eq!(3, r.len());
        assert!(
            r.iter()
                .all(|(_, [_, row, _, rows])| *row == 0 && *rows == 10)
        );
        assert_eq!(2, l.lay_out([80, 10], 1).separators.len());
    }

    #[test]
    fn resize_takes_rows_from_the_next_window_first_and_gives_them_back_to_it() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, false, false, EQUAL);
        l.split(3, 2, false, false, EQUAL);
        // 30 rows: 10 each
        assert_eq!(
            vec![
                (1, [0, 0, 80, 10]),
                (2, [0, 10, 80, 10]),
                (3, [0, 20, 80, 10])
            ],
            rects(&l, [80, 30], 1)
        );
        // :resize 15 on 2 (14 rows of text and its statusline)
        l.resize(2, Axis::Rows, 15, [80, 30], 2);
        assert_eq!(
            vec![
                (1, [0, 0, 80, 10]),
                (2, [0, 10, 80, 15]),
                (3, [0, 25, 80, 5])
            ],
            rects(&l, [80, 30], 2)
        );
        // more than the next has: the one before gives the rest
        l.resize(2, Axis::Rows, 25, [80, 30], 2);
        assert_eq!(
            vec![(1, [0, 0, 80, 4]), (2, [0, 4, 80, 25]), (3, [0, 29, 80, 1])],
            rects(&l, [80, 30], 2)
        );
        // smaller: the next takes it
        l.resize(2, Axis::Rows, 5, [80, 30], 2);
        assert_eq!(
            vec![(1, [0, 0, 80, 4]), (2, [0, 4, 80, 5]), (3, [0, 9, 80, 21])],
            rects(&l, [80, 30], 2)
        );
    }

    #[test]
    fn vertical_resize_sets_the_width_of_the_column_holding_the_window() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        l.split(3, 2, false, false, EQUAL);
        // :vertical resize 20 on 3: its column, 2 and 3, is 20 wide
        l.resize(3, Axis::Cols, 20, [80, 20], 3);
        assert_eq!(
            vec![
                (1, [0, 0, 59, 20]),
                (2, [60, 0, 20, 10]),
                (3, [60, 10, 20, 10])
            ],
            rects(&l, [80, 20], 3)
        );
        // :resize in a row with no column around it: nothing to share
        let before = l.clone();
        l.resize(1, Axis::Rows, 5, [80, 20], 1);
        assert_eq!(before, l);
    }

    #[test]
    fn a_pane_too_small_squeezes_windows_to_a_statusline_and_the_current_keeps_a_row() {
        let mut l = PaneLayout::new(1);
        for (new, from) in [(2, 1), (3, 2), (4, 3)] {
            l.split(new, from, false, false, EQUAL);
        }
        // 5 rows for 4 windows: three statuslines and the current's text row
        assert_eq!(
            vec![
                (1, [0, 0, 80, 1]),
                (2, [0, 1, 80, 1]),
                (3, [0, 2, 80, 2]),
                (4, [0, 4, 80, 1])
            ],
            rects(&l, [80, 5], 3)
        );
        // across: a column and a separator each
        let mut l = PaneLayout::new(1);
        for (new, from) in [(2, 1), (3, 2)] {
            l.split(new, from, true, false, EQUAL);
        }
        assert_eq!(
            vec![(1, [0, 0, 1, 3]), (2, [2, 0, 1, 3]), (3, [4, 0, 1, 3])],
            rects(&l, [5, 3], 2)
        );
    }

    #[test]
    fn past_the_floor_the_windows_farthest_from_the_current_are_cut_until_it_moves() {
        let mut l = PaneLayout::new(1);
        for (new, from) in [(2, 1), (3, 2), (4, 3)] {
            l.split(new, from, false, false, EQUAL);
        }
        // 3 rows: the current's two, and its neighbour's statusline
        assert_eq!(
            vec![(1, [0, 0, 80, 2]), (2, [0, 2, 80, 1])],
            rects(&l, [80, 3], 1)
        );
        assert_eq!(
            vec![(3, [0, 0, 80, 1]), (4, [0, 1, 80, 2])],
            rects(&l, [80, 3], 4)
        );
        // in the middle, the next goes first
        assert_eq!(
            vec![(2, [0, 0, 80, 2]), (3, [0, 2, 80, 1])],
            rects(&l, [80, 3], 2)
        );
        // across: the one cut leaves no separator at the edge
        let mut l = PaneLayout::new(1);
        l.split(2, 1, true, false, EQUAL);
        let laid = l.lay_out([1, 3], 2);
        assert_eq!(vec![2], laid.wins.iter().map(|p| p.win).collect::<Vec<_>>());
        assert!(laid.separators.is_empty());
    }

    #[test]
    fn shares_come_back_when_a_squeezed_pane_grows_again() {
        let mut l = PaneLayout::new(1);
        l.split(2, 1, false, false, UNEVEN);
        l.split(3, 2, false, false, UNEVEN);
        let big = rects(&l, [80, 40], 1);
        assert_eq!(
            vec![
                (1, [0, 0, 80, 20]),
                (2, [0, 20, 80, 10]),
                (3, [0, 30, 80, 10])
            ],
            big
        );
        // shrunk, then grown back
        assert_eq!(
            vec![(1, [0, 0, 80, 2]), (2, [0, 2, 80, 1]), (3, [0, 3, 80, 1])],
            rects(&l, [80, 4], 1)
        );
        assert_eq!(
            vec![
                (1, [0, 0, 80, 10]),
                (2, [0, 10, 80, 5]),
                (3, [0, 15, 80, 5])
            ],
            rects(&l, [80, 20], 1)
        );
        assert_eq!(big, rects(&l, [80, 40], 1));
    }
}
