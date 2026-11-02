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

const std = @import("std");
const windows = @import("../nvim/windows.zig");

const Layout = windows.Layout;

const TabPlacement = @This();

/// Put `win`'s pane right of (below, if not `side_by_side`) `anchor`'s,
/// leaving `anchor` the fraction `ratio` of the room they share.
pub const PaneMove = struct {
    win: i64,
    anchor: i64,
    side_by_side: bool,
    ratio: f64,

    fn sameSplit(a: PaneMove, b: PaneMove) bool {
        return a.win == b.win and a.anchor == b.anchor and a.side_by_side == b.side_by_side;
    }
};

moves: [Layout.max_nodes]PaneMove = undefined,
len: usize = 0,

pub fn items(self: *const TabPlacement) []const PaneMove {
    return self.moves[0..self.len];
}

/// The moves that arrange the panes of `shown`, the windows that have one,
/// as `layout` has them. Windows of `layout` not in `shown` are left out.
pub fn plan(layout: *const Layout, shown: []const i64) TabPlacement {
    var result: TabPlacement = .{};
    if (layout.len == 0) return result;
    var planner: Planner = .{ .layout = layout, .shown = shown, .result = &result };
    planner.arrange(0);
    return result;
}

const Planner = struct {
    layout: *const Layout,
    shown: []const i64,
    result: *TabPlacement,

    /// The first window in the node's part of the layout that has a pane.
    fn firstShown(self: Planner, at: usize) ?i64 {
        const node = self.layout.nodes[at];
        if (node.kind == .window) return if (std.mem.indexOfScalar(i64, self.shown, node.win) != null) node.win else null;
        var kids = self.layout.kids(at);
        while (kids.next()) |kid| {
            if (self.firstShown(kid)) |win| return win;
        }
        return null;
    }

    /// How much room the node's part of the layout takes along an axis, in
    /// cells: none if it has no pane.
    fn extent(self: Planner, at: usize, side_by_side: bool) u32 {
        const node = self.layout.nodes[at];
        if (node.kind == .window) {
            if (self.firstShown(at) == null) return 0;
            return @max(if (side_by_side) node.w else node.h, 1);
        }
        // along its own direction its kids add up, across it they're alike
        const along = (node.kind == .row) == side_by_side;
        var total: u32 = 0;
        var kids = self.layout.kids(at);
        while (kids.next()) |kid| {
            const e = self.extent(kid, side_by_side);
            total = if (along) total + e else @max(total, e);
        }
        return total;
    }

    fn arrange(self: Planner, at: usize) void {
        const node = self.layout.nodes[at];
        if (node.kind == .window) return;
        const side_by_side = node.kind == .row;
        // the kids with panes, and how much room each takes
        var with_panes: [Layout.max_nodes]usize = undefined;
        var room: [Layout.max_nodes]u32 = undefined;
        var n: usize = 0;
        var kids = self.layout.kids(at);
        while (kids.next()) |kid| {
            const e = self.extent(kid, side_by_side);
            if (e == 0) continue;
            with_panes[n] = kid;
            room[n] = e;
            n += 1;
        }
        if (n >= 2) {
            // the first kid's pane keeps its place; each later one is put beside it, last first
            const anchor = self.firstShown(with_panes[0]).?;
            var before: u32 = 0;
            for (room[0..n]) |r| before += r;
            var i = n;
            while (i > 1) {
                i -= 1;
                const total = before;
                before -= room[i];
                self.result.moves[self.result.len] = .{
                    .win = self.firstShown(with_panes[i]).?,
                    .anchor = anchor,
                    .side_by_side = side_by_side,
                    .ratio = @as(f64, @floatFromInt(before)) / @as(f64, @floatFromInt(total)),
                };
                self.result.len += 1;
            }
        }
        for (with_panes[0..n]) |kid| self.arrange(kid);
    }
};

/// How far a window of a tab page's layout has got toward having a pane.
pub const WindowState = enum {
    /// Neovim hasn't drawn it yet, so its pane isn't started.
    undrawn,
    /// It was drawn and has closed.
    gone,
    /// It's drawn and its pane isn't in Rex yet.
    pending,
    /// Its pane is in Rex.
    ready,
    /// It has no pane, and won't: it's closing, or its pane failed.
    skipped,
};

pub const Action = union(enum) {
    /// Not every window has a pane yet: placing the ones that do would only be moved again.
    wait,
    /// Its windows are gone.
    forget,
    nothing,
    /// The windows or their order changed: make these moves.
    rearrange: TabPlacement,
    /// Only the sizes changed: set these splits' ratios.
    resize: TabPlacement,
};

/// What to do about the panes of a tab page whose windows are in `states`,
/// one for each node of `layout`, given what was `last` done. Sizes only
/// reach us when Neovim changed them, never from the user resizing panes in
/// Rex, so a size that changed is applied.
pub fn decide(layout: *const Layout, states: []const WindowState, last: ?*const TabPlacement) Action {
    var shown: [Layout.max_nodes]i64 = undefined;
    var n: usize = 0;
    for (layout.items(), states) |node, state| {
        if (node.kind != .window) continue;
        switch (state) {
            .undrawn, .pending => return .wait,
            .ready => {
                shown[n] = node.win;
                n += 1;
            },
            .gone, .skipped => {},
        }
    }
    if (n == 0) return .forget;
    const wanted = plan(layout, shown[0..n]);
    const previous = last orelse return .{ .rearrange = wanted };
    if (previous.len != wanted.len) return .{ .rearrange = wanted };
    var resized = false;
    for (previous.items(), wanted.items()) |a, b| {
        if (!a.sameSplit(b)) return .{ .rearrange = wanted };
        if (@abs(a.ratio - b.ratio) > 0.005) resized = true;
    }
    return if (resized) .{ .resize = wanted } else .nothing;
}

/// A tab page's layout as Neovim last said, and how far its panes have got to matching.
pub const Tab = struct {
    layout: Layout,
    /// What was done to place its panes, the last time.
    placed: ?TabPlacement = null,
    /// By node: the window was drawn, so if it isn't now, it closed.
    seen: std.StaticBitSet(Layout.max_nodes) = .empty,

    /// Takes a newer layout, keeping what's known of the windows it still has.
    pub fn replace(self: *Tab, layout: Layout) void {
        var seen: std.StaticBitSet(Layout.max_nodes) = .empty;
        for (layout.items(), 0..) |node, at| {
            if (node.kind != .window) continue;
            for (self.layout.items(), 0..) |old, old_at| {
                if (old.kind == .window and old.win == node.win and self.seen.isSet(old_at)) seen.set(at);
            }
        }
        self.layout = layout;
        self.seen = seen;
    }
};

const testing = std.testing;

/// Rex's side of a tab page's floating pane, as far as moves go: a tree of
/// splits whose leaves are windows' panes.
const Rex = struct {
    gpa: std.mem.Allocator,
    root: *Node,

    const Node = union(enum) {
        pane: i64,
        split: struct { side_by_side: bool, ratio: f64, before: *Node, after: *Node },
    };

    const Rect = struct { x: f64, y: f64, w: f64, h: f64 };

    fn deinit(self: *Rex) void {
        self.free(self.root);
    }

    fn free(self: *Rex, n: *Node) void {
        if (n.* == .split) {
            self.free(n.split.before);
            self.free(n.split.after);
        }
        self.gpa.destroy(n);
    }

    fn pane(self: *Rex, win: i64) !*Node {
        const n = try self.gpa.create(Node);
        n.* = .{ .pane = win };
        return n;
    }

    /// Panes made in order, each split off the first, to its right.
    fn made(gpa: std.mem.Allocator, wins: []const i64) !Rex {
        var rex: Rex = .{ .gpa = gpa, .root = undefined };
        rex.root = try rex.pane(wins[0]);
        for (wins[1..]) |w| try rex.insert(w, wins[0], true, 0.5);
        return rex;
    }

    /// `session.move_block`'s split of `anchor`'s room: `win` goes after it,
    /// `anchor` getting `ratio` of it.
    fn insert(self: *Rex, win: i64, anchor: i64, side_by_side: bool, ratio: f64) !void {
        const link = findLink(&self.root, anchor).?;
        const old = link.*;
        const split = try self.gpa.create(Node);
        split.* = .{ .split = .{ .side_by_side = side_by_side, .ratio = ratio, .before = old, .after = try self.pane(win) } };
        link.* = split;
    }

    fn findLink(link: **Node, win: i64) ?**Node {
        switch (link.*.*) {
            .pane => |w| return if (w == win) link else null,
            .split => |*s| return findLink(&s.before, win) orelse findLink(&s.after, win),
        }
    }

    /// `session.move_block`: takes `win`'s pane out, then splits `anchor`'s.
    fn move(self: *Rex, m: PaneMove) !void {
        // out: its parent split gives way to the other side
        const link = findLink(&self.root, m.win).?;
        const parent = findParent(self.root, m.win).?;
        const sibling = if (parent.split.before == link.*) parent.split.after else parent.split.before;
        const parent_link = findLinkTo(&self.root, parent).?;
        parent_link.* = sibling;
        self.gpa.destroy(link.*);
        self.gpa.destroy(parent);
        try self.insert(m.win, m.anchor, m.side_by_side, m.ratio);
    }

    fn findParent(node: *Node, win: i64) ?*Node {
        if (node.* != .split) return null;
        const s = node.split;
        if (s.before.* == .pane and s.before.pane == win) return node;
        if (s.after.* == .pane and s.after.pane == win) return node;
        return findParent(s.before, win) orelse findParent(s.after, win);
    }

    fn findLinkTo(link: **Node, target: *Node) ?**Node {
        if (link.* == target) return link;
        if (link.*.* != .split) return null;
        return findLinkTo(&link.*.split.before, target) orelse findLinkTo(&link.*.split.after, target);
    }

    /// The pane of `win` in the unit square.
    fn rectOf(self: *const Rex, win: i64) ?Rect {
        return rectIn(self.root, .{ .x = 0, .y = 0, .w = 1, .h = 1 }, win);
    }

    fn rectIn(node: *const Node, r: Rect, win: i64) ?Rect {
        switch (node.*) {
            .pane => |w| return if (w == win) r else null,
            .split => |s| {
                const a: Rect, const b: Rect = if (s.side_by_side)
                    .{ .{ .x = r.x, .y = r.y, .w = r.w * s.ratio, .h = r.h }, .{ .x = r.x + r.w * s.ratio, .y = r.y, .w = r.w * (1 - s.ratio), .h = r.h } }
                else
                    .{ .{ .x = r.x, .y = r.y, .w = r.w, .h = r.h * s.ratio }, .{ .x = r.x, .y = r.y + r.h * s.ratio, .w = r.w, .h = r.h * (1 - s.ratio) } };
                return rectIn(s.before, a, win) orelse rectIn(s.after, b, win);
            },
        }
    }
};

fn expectNear(want: f64, got: f64) !void {
    try testing.expectApproxEqAbs(want, got, 0.02);
}

test "a file panel made last, narrow, ends up leftmost at its width" {
    // as Diffview makes a tab page: the old and new versions, then the file
    // panel, each split off the first window; Neovim shows it panel, old, new
    // at 20, 59 and 39 cells
    const panel = 1003;
    const old = 1001;
    const new = 1002;
    var layout: Layout = .{ .tab = 2 };
    layout.len = 4;
    layout.nodes[0] = .{ .kind = .row, .w = 118, .h = 27, .end = 4 };
    layout.nodes[1] = .{ .kind = .window, .win = panel, .w = 20, .h = 27, .end = 2 };
    layout.nodes[2] = .{ .kind = .window, .win = old, .w = 59, .h = 27, .end = 3 };
    layout.nodes[3] = .{ .kind = .window, .win = new, .w = 39, .h = 27, .end = 4 };

    // in the order the windows were made: old, new, panel
    var rex = try Rex.made(testing.allocator, &.{ old, new, panel });
    defer rex.deinit();
    for (TabPlacement.plan(&layout, &.{ old, new, panel }).items()) |m| try rex.move(m);

    const p = rex.rectOf(panel).?;
    const o = rex.rectOf(old).?;
    const n = rex.rectOf(new).?;
    try expectNear(0, p.x);
    try expectNear(20.0 / 118.0, p.w);
    try expectNear(p.x + p.w, o.x);
    try expectNear(59.0 / 118.0, o.w);
    try expectNear(o.x + o.w, n.x);
    try expectNear(39.0 / 118.0, n.w);
    for ([_]Rex.Rect{ p, o, n }) |r| {
        try expectNear(0, r.y);
        try expectNear(1, r.h);
    }
}

test "rows and columns nest, from whatever order the panes were made in" {
    // panel | (old over new), in a row of two whose second is a column
    var layout: Layout = .{ .tab = 2 };
    layout.len = 5;
    layout.nodes[0] = .{ .kind = .row, .w = 100, .h = 40, .end = 5 };
    layout.nodes[1] = .{ .kind = .window, .win = 30, .w = 25, .h = 40, .end = 2 };
    layout.nodes[2] = .{ .kind = .column, .w = 75, .h = 40, .end = 5 };
    layout.nodes[3] = .{ .kind = .window, .win = 10, .w = 75, .h = 10, .end = 4 };
    layout.nodes[4] = .{ .kind = .window, .win = 20, .w = 75, .h = 30, .end = 5 };

    var rex = try Rex.made(testing.allocator, &.{ 20, 30, 10 });
    defer rex.deinit();
    for (TabPlacement.plan(&layout, &.{ 20, 30, 10 }).items()) |m| try rex.move(m);

    const panel = rex.rectOf(30).?;
    const top = rex.rectOf(10).?;
    const bottom = rex.rectOf(20).?;
    try expectNear(0, panel.x);
    try expectNear(0.25, panel.w);
    try expectNear(1, panel.h);
    try expectNear(0.25, top.x);
    try expectNear(0.75, top.w);
    try expectNear(0.25, top.h);
    try expectNear(0, top.y);
    try expectNear(0.25, bottom.x);
    try expectNear(0.75, bottom.h);
    try expectNear(0.25, bottom.y);
}

test "windows without panes are left out" {
    var layout: Layout = .{ .tab = 2 };
    layout.len = 4;
    layout.nodes[0] = .{ .kind = .row, .w = 90, .h = 10, .end = 4 };
    layout.nodes[1] = .{ .kind = .window, .win = 1, .w = 30, .h = 10, .end = 2 };
    layout.nodes[2] = .{ .kind = .window, .win = 2, .w = 30, .h = 10, .end = 3 };
    layout.nodes[3] = .{ .kind = .window, .win = 3, .w = 30, .h = 10, .end = 4 };
    const placement = TabPlacement.plan(&layout, &.{ 1, 3 });
    try testing.expectEqual(@as(usize, 1), placement.items().len);
    try testing.expectEqual(@as(i64, 3), placement.items()[0].win);
    try testing.expectEqual(@as(i64, 1), placement.items()[0].anchor);
    try testing.expect(placement.items()[0].side_by_side);
    try expectNear(0.5, placement.items()[0].ratio);
    try testing.expectEqual(@as(usize, 0), TabPlacement.plan(&layout, &.{1}).items().len);
}

/// A row of windows `wins` with widths `widths`, 10 rows high.
fn windowRow(wins: []const i64, widths: []const u32) Layout {
    var layout: Layout = .{ .tab = 2 };
    var total: u32 = 0;
    for (widths) |w| total += w;
    layout.nodes[0] = .{ .kind = .row, .w = total, .h = 10 };
    for (wins, widths, 1..) |win, w, at| layout.nodes[at] = .{ .kind = .window, .win = win, .w = w, .h = 10, .end = at + 1 };
    layout.len = wins.len + 1;
    layout.nodes[0].end = layout.len;
    return layout;
}

test "it waits until every window has a pane, then places them once" {
    const layout = windowRow(&.{ 1, 2, 3 }, &.{ 20, 40, 40 });
    // a layout told before Neovim drew its windows, then a pane still to come
    try testing.expectEqual(Action.wait, decide(&layout, &.{ .skipped, .undrawn, .undrawn, .undrawn }, null));
    try testing.expectEqual(Action.wait, decide(&layout, &.{ .skipped, .ready, .ready, .pending }, null));
    // all there: moves, and none again until something changed
    const act = decide(&layout, &.{ .skipped, .ready, .ready, .ready }, null);
    try testing.expectEqual(@as(usize, 2), act.rearrange.items().len);
    try testing.expectEqual(Action.nothing, decide(&layout, &.{ .skipped, .ready, .ready, .ready }, &act.rearrange));
}

test "closed windows and windows that won't have panes don't hold the others up" {
    const layout = windowRow(&.{ 1, 2, 3 }, &.{ 20, 40, 40 });
    const act = decide(&layout, &.{ .skipped, .ready, .gone, .skipped }, null);
    try testing.expectEqual(@as(usize, 0), act.rearrange.items().len);
    const two = decide(&layout, &.{ .skipped, .ready, .ready, .skipped }, null);
    try testing.expectEqual(@as(usize, 1), two.rearrange.items().len);
    try testing.expectEqual(@as(i64, 2), two.rearrange.items()[0].win);
    try testing.expectEqual(@as(i64, 1), two.rearrange.items()[0].anchor);
}

test "a tab page whose windows are all gone is forgotten" {
    const layout = windowRow(&.{ 1, 2 }, &.{ 20, 40 });
    try testing.expectEqual(Action.forget, decide(&layout, &.{ .skipped, .gone, .gone }, null));
    try testing.expectEqual(Action.forget, decide(&layout, &.{ .skipped, .skipped, .gone }, null));
    try testing.expectEqual(Action.forget, decide(&.{ .tab = 2 }, &.{}, null));
}

test "a window added, or an order changed, is a rearrangement; only sizes changed is a resize" {
    const layout = windowRow(&.{ 1, 2, 3 }, &.{ 20, 40, 40 });
    const ready: [4]WindowState = .{ .skipped, .ready, .ready, .ready };
    const placed = decide(&layout, &ready, null).rearrange;

    // the panel made wider, as Diffview's Panel:resize does
    const wider = windowRow(&.{ 1, 2, 3 }, &.{ 30, 40, 40 });
    const resized = decide(&wider, &ready, &placed);
    try testing.expectEqual(@as(usize, 2), resized.resize.items().len);
    try testing.expect(resized.resize.items()[1].ratio > placed.items()[1].ratio);

    // the same windows, the panel last
    const reordered = windowRow(&.{ 2, 3, 1 }, &.{ 40, 40, 20 });
    try testing.expect(decide(&reordered, &ready, &placed) == .rearrange);
    // a fourth window
    const four = windowRow(&.{ 1, 2, 3, 4 }, &.{ 20, 40, 40, 10 });
    try testing.expect(decide(&four, &.{ .skipped, .ready, .ready, .ready, .ready }, &placed) == .rearrange);
    // one closing
    const gone = decide(&layout, &.{ .skipped, .gone, .ready, .ready }, &placed);
    try testing.expect(gone == .rearrange);
}

test "a newer layout keeps which windows were seen, by window" {
    var tab: Tab = .{ .layout = windowRow(&.{ 1, 2 }, &.{ 20, 40 }) };
    tab.seen.set(1);
    // a window added in front: 1 moves from node 1 to node 2
    tab.replace(windowRow(&.{ 3, 1, 2 }, &.{ 10, 20, 40 }));
    try testing.expect(!tab.seen.isSet(1));
    try testing.expect(tab.seen.isSet(2));
    try testing.expect(!tab.seen.isSet(3));
}
