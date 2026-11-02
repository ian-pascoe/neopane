//! Windows as panes, Neovim's side: `windows.lua` keeps every window except
//! a hidden anchor external and reports the current window. These calls
//! write requests; the caller flushes.

const std = @import("std");
const msgpack = @import("msgpack.zig");
const rpc = @import("rpc.zig");
const Nvim = @import("Nvim.zig");

const lua = @embedFile("windows.lua");

/// Installs the Lua on our channel `chan` and makes the current window
/// external at `cols` x `rows`. Its response is `[first window, anchor]`.
pub fn setup(nvim: *Nvim, chan: i64, cols: u32, rows: u32) !u32 {
    const id = try nvim.execLua(lua, 3);
    try msgpack.writeInt(nvim.writer(), chan);
    try msgpack.writeInt(nvim.writer(), cols);
    try msgpack.writeInt(nvim.writer(), rows);
    return id;
}

/// Makes ordinary window `win` external at `cols` x `rows`; anchors stay.
pub fn adopt(nvim: *Nvim, win: i64, cols: u32, rows: u32) !void {
    _ = try nvim.execLua("neorex.adopt(...)", 3);
    try msgpack.writeInt(nvim.writer(), win);
    try msgpack.writeInt(nvim.writer(), @max(cols, 1));
    try msgpack.writeInt(nvim.writer(), @max(rows, 1));
}

/// Sizes external window `win` to fill `cols` x `rows`, its statusline included.
pub fn size(nvim: *Nvim, win: i64, cols: u32, rows: u32) !void {
    _ = try nvim.execLua("neorex.size(...)", 3);
    try msgpack.writeInt(nvim.writer(), win);
    try msgpack.writeInt(nvim.writer(), @max(cols, 1));
    try msgpack.writeInt(nvim.writer(), @max(rows, 1));
}

/// Fits the floats hanging from editor floats `tops` to `w` x `h` at `row`,
/// `col`; `cur` is where they are now: row, col, width, height.
/// Returns the request's id.
pub fn placeGroup(nvim: *Nvim, tops: []const i64, row: i32, col: i32, w: u32, h: u32, cur: [4]i64) !u32 {
    const id = try nvim.execLua("neorex.place_group(...)", 6);
    const wr = nvim.writer();
    try msgpack.writeArrayHeader(wr, @intCast(tops.len));
    for (tops) |t| try msgpack.writeInt(wr, t);
    try msgpack.writeInt(wr, row);
    try msgpack.writeInt(wr, col);
    try msgpack.writeInt(wr, @max(w, 1));
    try msgpack.writeInt(wr, @max(h, 1));
    try msgpack.writeArrayHeader(wr, 4);
    for (cur) |v| try msgpack.writeInt(wr, v);
    return id;
}

/// Closes `win`, keeping unsaved changes in its buffer.
pub fn close(nvim: *Nvim, win: i64) !void {
    _ = try nvim.execLua("neorex.close(...)", 1);
    try msgpack.writeInt(nvim.writer(), win);
}

/// Returns the request's id.
pub fn setCurrent(nvim: *Nvim, win: i64) !u32 {
    const id = try nvim.request("nvim_set_current_win", 1);
    try msgpack.writeInt(nvim.writer(), win);
    return id;
}

/// Asks for our channel id: the response to the returned id starts with it.
pub fn askChannel(nvim: *Nvim) !u32 {
    return nvim.request("nvim_get_api_info", 0);
}

/// What the Lua tells the daemon.
pub const Note = union(enum) {
    /// The current window is now this one.
    current: i64,
    /// This window is closing and reopening; keep its pane for the new one.
    reopen: i64,
    /// The window that reopened `old`, which gets its pane.
    reopened: struct { old: i64, new: i64 },
    /// A new window, and where Neovim put it.
    split: Split,
    /// The current window is now this float.
    float: i64,
    /// This float was just entered; sent before the redraw that shows it.
    entered: i64,
    /// The tabline's rows at the top of the screen.
    tabline: i64,
    /// `<C-w>h/j/k/l` was typed: Rex knows where the panes sit, not Neovim.
    direction: Move,
    /// Where the windows of a tab page made after setup sit in it.
    layout: Layout,
};

/// The layout of a tab page's windows: nested rows, columns and windows,
/// each with the size Neovim gave it, in cells. Empty if the tab page has
/// none the daemon can place, or has closed.
///
/// A note holds it inline, about 1.5 KB: `parseNote` has no allocator, a
/// note is used and dropped at once, and the daemon keeps one per tab page.
pub const Layout = struct {
    tab: i64,
    len: usize = 0,
    /// Parents before their kids.
    nodes: [max_nodes]Node = undefined,

    /// The most nodes there are room for. The same as `MAX_LAYOUT_NODES` in
    /// windows.lua, which sends none for a bigger layout.
    pub const max_nodes = 63;

    pub const Node = struct {
        kind: Kind,
        /// A window's handle.
        win: i64 = 0,
        w: u32,
        h: u32,
        /// The index after this node and everything in it.
        end: usize = 0,
    };

    pub const Kind = enum { window, row, column };

    /// The nodes in use.
    pub fn items(self: *const Layout) []const Node {
        return self.nodes[0..self.len];
    }

    /// The indices of the kids of the node at `at`, in order.
    pub fn kids(self: *const Layout, at: usize) Kids {
        return .{ .layout = self, .next_at = at + 1, .end = self.nodes[at].end };
    }

    pub const Kids = struct {
        layout: *const Layout,
        next_at: usize,
        end: usize,

        pub fn next(self: *Kids) ?usize {
            if (self.next_at >= self.end) return null;
            const at = self.next_at;
            self.next_at = self.layout.nodes[at].end;
            return at;
        }
    };

    /// Reads one node and what's in it, from `[kind, win or kids, w, h]`.
    fn read(self: *Layout, d: *msgpack.Decoder) !void {
        if (self.len == max_nodes) return error.BadMessage;
        const at = self.len;
        self.len += 1;
        const kind = std.enums.fromInt(Kind, try rpc.expectInt(d)) orelse return error.BadMessage;
        const win_or_kids = try rpc.expectInt(d);
        const w = std.math.cast(u32, try rpc.expectInt(d)) orelse return error.BadMessage;
        const h = std.math.cast(u32, try rpc.expectInt(d)) orelse return error.BadMessage;
        self.nodes[at] = .{ .kind = kind, .w = w, .h = h };
        if (kind == .window) {
            self.nodes[at].win = win_or_kids;
        } else {
            const kid_count = std.math.cast(u32, win_or_kids) orelse return error.BadMessage;
            // a row or column of one is no row
            if (kid_count < 2) return error.BadMessage;
            for (0..kid_count) |_| try self.read(d);
        }
        self.nodes[at].end = self.len;
    }
};

/// Which way a window command goes: `h`, `j`, `k` or `l`.
pub const Direction = enum { left, down, up, right };

/// A directional window command, `count` times as typed.
pub const Move = struct { dir: Direction, count: u32 };

/// Where a new window sits in Neovim's layout.
pub const Split = struct {
    win: i64,
    /// Side by side (`:vsplit`), else one above the other.
    side_by_side: bool,
    /// Left of or above the window beside it.
    before: bool,
    /// The window it was split from, or 0.
    from: i64,
    /// The tab page made after setup it belongs to, which Neovim shows
    /// over the first one's windows, else 0.
    tab: i64 = 0,
};

/// A notification from the Lua, or null for any other.
pub fn parseNote(method: []const u8, params: *msgpack.Decoder) !?Note {
    if (std.mem.eql(u8, method, "neorex_direction")) return try parseDirection(params);
    const tag: std.meta.Tag(Note) = if (std.mem.eql(u8, method, "neorex_win"))
        .current
    else if (std.mem.eql(u8, method, "neorex_reopen"))
        .reopen
    else if (std.mem.eql(u8, method, "neorex_reopened"))
        .reopened
    else if (std.mem.eql(u8, method, "neorex_split"))
        .split
    else if (std.mem.eql(u8, method, "neorex_float"))
        .float
    else if (std.mem.eql(u8, method, "neorex_entered"))
        .entered
    else if (std.mem.eql(u8, method, "neorex_tabline"))
        .tabline
    else if (std.mem.eql(u8, method, "neorex_layout"))
        .layout
    else
        return null;
    const want: u32 = switch (tag) {
        .split => 5,
        .reopened, .layout => 2,
        else => 1,
    };
    if (try rpc.expectArray(params) < want) return error.BadMessage;
    const win = try rpc.expectHandle(params);
    return switch (tag) {
        .current => .{ .current = win },
        .reopen => .{ .reopen = win },
        .reopened => .{ .reopened = .{ .old = win, .new = try rpc.expectHandle(params) } },
        .float => .{ .float = win },
        .entered => .{ .entered = win },
        .tabline => .{ .tabline = win },
        .direction => unreachable, // read above
        .layout => blk: {
            var layout: Layout = .{ .tab = win };
            // the nodes are flattened into one array, four numbers each
            const count = try rpc.expectArray(params);
            if (count > 0) try layout.read(params);
            if (count != layout.len * 4) return error.BadMessage;
            break :blk .{ .layout = layout };
        },
        .split => .{ .split = .{
            .win = win,
            .side_by_side = try rpc.expectBool(params),
            .before = try rpc.expectBool(params),
            .from = try rpc.expectHandle(params),
            .tab = try rpc.expectHandle(params),
        } },
    };
}

/// `[key, count]`: the `wincmd` key, one of `hjkl`, and how many times.
fn parseDirection(params: *msgpack.Decoder) !Note {
    if (try rpc.expectArray(params) < 2) return error.BadMessage;
    const key = try rpc.expectStr(params);
    const count = try rpc.expectInt(params);
    if (key.len != 1 or count < 1) return error.BadMessage;
    const dir: Direction = switch (key[0]) {
        'h' => .left,
        'j' => .down,
        'k' => .up,
        'l' => .right,
        else => return error.BadMessage,
    };
    return .{ .direction = .{ .dir = dir, .count = @intCast(@min(count, std.math.maxInt(u32))) } };
}

/// `[first, anchor]` from `setup`'s response.
pub fn parseSetup(result: *msgpack.Decoder) ![2]i64 {
    if (try rpc.expectArray(result) < 2) return error.BadMessage;
    return .{ try rpc.expectHandle(result), try rpc.expectHandle(result) };
}

/// Our channel id from `askChannel`'s response.
pub fn parseChannel(result: *msgpack.Decoder) !i64 {
    if (try rpc.expectArray(result) < 1) return error.BadMessage;
    return rpc.expectInt(result);
}

const testing = std.testing;

/// Made-up reports the tests wait for: a real window handle is never this big.
const ping_base: i64 = 4000;
const fight_at_1s: i64 = 6000;
const fight_at_end: i64 = 7000;
const Grids = @import("../screen/Grids.zig");
const Grid = @import("../screen/Grid.zig");

/// A Neovim running windows.lua, and what it has said.
const Harness = struct {
    nvim: Nvim,
    grids: Grids = .{},
    notes: std.ArrayList(Note) = .empty,
    setup_id: u32 = 0,
    chan_id: u32 = 0,
    wins: [2]i64 = .{ 0, 0 },
    pane_a: i64 = 0,
    pane_b: i64 = 0,
    text: []const u8 = "",
    /// The windows a check waits for.
    want: i64 = 0,
    gone: i64 = 0,
    /// Made-up reports so far, to know Neovim got that far.
    pings: i64 = 0,

    /// A tab page and its windows, for checks that wait on them.
    tab: i64 = 0,
    watch: [3]i64 = .{ 0, 0, 0 },

    /// Spawns a clean Neovim and boots it (`boot`). Skips the test without
    /// `nvim`. `buf` is the Neovim connection's buffer.
    fn start(h: *Harness, buf: []u8) !void {
        return h.startSized(buf, 40, 10);
    }

    /// `start`, with a screen of `cols` x `rows`.
    fn startSized(h: *Harness, buf: []u8, cols: u32, rows: u32) !void {
        h.* = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, buf) catch |err| switch (err) {
            error.FileNotFound => return error.SkipZigTest,
            else => return err,
        } };
        errdefer h.deinit();
        try h.bootSized(cols, rows);
    }

    fn deinit(h: *Harness) void {
        h.grids.deinit(testing.allocator);
        h.notes.deinit(testing.allocator);
        h.nvim.deinit(testing.allocator);
    }

    /// Reads until `done`, or false if Neovim exits.
    fn until(h: *Harness, comptime done: fn (*Harness) bool) !bool {
        const gpa = testing.allocator;
        for (0..200) |_| {
            if (done(h)) return true;
            var fds = [_]std.c.pollfd{.{ .fd = h.nvim.readFd(), .events = std.c.POLL.IN, .revents = 0 }};
            if (std.c.poll(&fds, 1, 3000) <= 0) return error.TestTimedOut;
            if (!try h.nvim.read(gpa)) return false;
            while (try h.nvim.next()) |msg| switch (msg) {
                .notification => |n| {
                    var params = n.params;
                    if (std.mem.eql(u8, n.method, "redraw")) {
                        _ = try h.grids.applyRedraw(gpa, &params);
                    } else if (try parseNote(n.method, &params)) |note| try h.notes.append(gpa, note);
                },
                .response => |r| {
                    if (r.err) |e| {
                        std.debug.print("neovim: {s}\n", .{e});
                        return error.NvimError;
                    }
                    var result = r.result;
                    if (r.id == h.chan_id) {
                        h.setup_id = try setup(&h.nvim, try parseChannel(&result), 40, 9);
                        try h.nvim.flush();
                    } else if (r.id == h.setup_id) h.wins = try parseSetup(&result);
                },
                .request => |r| try h.nvim.respondError(r.id, "test"),
            };
        }
        return error.TestTimedOut;
    }

    fn input(h: *Harness, keys: []const u8) !void {
        _ = try h.nvim.request("nvim_input", 1);
        try msgpack.writeStr(h.nvim.writer(), keys);
        try h.nvim.flush();
    }

    /// Starts windows.lua and waits until the first window is external.
    fn boot(h: *Harness) !void {
        try h.bootSized(40, 10);
    }

    fn bootSized(h: *Harness, cols: u32, rows: u32) !void {
        try h.nvim.uiAttach(cols, rows, .{ .ext_multigrid = true });
        h.chan_id = try askChannel(&h.nvim);
        try h.nvim.flush();
        try testing.expect(try h.until(struct {
            fn f(x: *Harness) bool {
                return x.wins[0] != 0 and x.lastCurrent() == x.wins[0] and x.kind(x.wins[0]) == .external;
            }
        }.f));
    }

    /// Splits the current window and makes the split external at 20 x 9,
    /// as the daemon does. Returns it.
    fn adoptSplit(h: *Harness) !i64 {
        try h.input(":split<CR>");
        try testing.expect(try h.until(struct {
            fn f(x: *Harness) bool {
                const cur = x.lastCurrent();
                return cur != 0 and x.kind(cur) == .ordinary;
            }
        }.f));
        const split = h.lastCurrent();
        try adopt(&h.nvim, split, 20, 9);
        try h.nvim.flush();
        try testing.expect(try h.untilExternal(split));
        return split;
    }

    fn untilExternal(h: *Harness, win: i64) !bool {
        h.want = win;
        return h.until(struct {
            fn f(x: *Harness) bool {
                return x.kind(x.want) == .external;
            }
        }.f);
    }

    /// Runs Lua `fmt` in Neovim, as a plugin would, and waits until it has
    /// run. An error in it fails the test.
    fn sync(h: *Harness, comptime fmt: []const u8, args: anytype) !void {
        h.pings += 1;
        const ping = ping_base + h.pings;
        var buf: [2048]u8 = undefined;
        const code = try std.fmt.bufPrint(&buf, fmt ++ "\nvim.rpcnotify(0, 'neorex_win', {d})", args ++ .{ping});
        try h.lua(code);
        h.want = ping;
        try testing.expect(try h.until(Harness.isCurrent));
    }

    /// Waits for `win`, which a plugin moved back, to be external again.
    fn expectExternal(h: *Harness, win: i64) !void {
        try h.sync("assert(vim.wait(2500, function() return vim.api.nvim_win_get_config({d}).external end), 'not external')", .{win});
        try testing.expect(try h.untilExternal(win));
    }

    /// Types something, and waits until Neovim has taken it.
    fn typed(h: *Harness) !void {
        h.pings += 1;
        var buf: [96]u8 = undefined;
        try h.input(try std.fmt.bufPrint(&buf, ":lua vim.rpcnotify(0, 'neorex_win', {d})<CR>", .{ping_base + h.pings}));
        h.want = ping_base + h.pings;
        try testing.expect(try h.until(Harness.isCurrent));
    }

    /// Runs `code` in Neovim.
    fn lua(h: *Harness, code: []const u8) !void {
        _ = try h.nvim.execLua(code, 0);
        try h.nvim.flush();
    }

    fn lastCurrent(h: *const Harness) i64 {
        var i = h.notes.items.len;
        while (i > 0) {
            i -= 1;
            if (h.notes.items[i] == .current) return h.notes.items[i].current;
        }
        return 0;
    }

    /// The tabline rows last reported, or -1.
    fn lastTabline(h: *const Harness) i64 {
        var i = h.notes.items.len;
        while (i > 0) {
            i -= 1;
            if (h.notes.items[i] == .tabline) return h.notes.items[i].tabline;
        }
        return -1;
    }

    /// The layout last reported for tab page `tab`.
    fn lastLayout(h: *const Harness, tab: i64) ?Layout {
        var i = h.notes.items.len;
        while (i > 0) {
            i -= 1;
            if (h.notes.items[i] == .layout and h.notes.items[i].layout.tab == tab) return h.notes.items[i].layout;
        }
        return null;
    }

    fn watchedExternal(h: *const Harness) bool {
        for (h.watch) |win| if (h.kind(win) != .external) return false;
        return true;
    }

    fn splitNote(h: *const Harness, win: i64) ?Note {
        for (h.notes.items) |n| if (n == .split and n.split.win == win) return n;
        return null;
    }

    fn isCurrent(h: *Harness) bool {
        return h.lastCurrent() == h.want;
    }

    fn kind(h: *const Harness, win: i64) ?Grids.Win.Kind {
        const grid = h.grids.gridOf(win) orelse return null;
        return h.grids.winOn(grid).?.kind;
    }
};

test "windows become external; the anchor stays hidden" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();

    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and x.lastCurrent() == x.wins[0] and x.kind(x.wins[0]) == .external;
        }
    }.f));
    const first, const anchor = h.wins;
    try testing.expectEqual(Grids.Win.Kind.ordinary, h.kind(anchor).?);

    // a split is ordinary until the daemon adopts it
    h.notes.clearRetainingCapacity();
    try h.input(":split<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and x.kind(cur) == .ordinary;
        }
    }.f));
    const split = h.lastCurrent();
    try testing.expect(split != first and split != anchor);
    try testing.expectEqual(Note{ .split = .{ .win = split, .side_by_side = false, .before = true, .from = first } }, h.splitNote(split).?);
    try adopt(&h.nvim, split, 20, 9);
    // the anchor never becomes external
    try adopt(&h.nvim, anchor, 20, 9);
    try h.nvim.flush();
    h.want = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external;
        }
    }.f));

    // <C-w>w wraps through the anchor to the first window, <C-w>W back
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>w");
    h.want = first;
    try testing.expect(try h.until(Harness.isCurrent));
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>W");
    h.want = split;
    try testing.expect(try h.until(Harness.isCurrent));
    try testing.expectEqual(Grids.Win.Kind.ordinary, h.kind(anchor).?);

    // :vsplit with splitright: beside, after
    h.notes.clearRetainingCapacity();
    try h.input(":set splitright | vsplit<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and x.kind(cur) == .ordinary;
        }
    }.f));
    const vsplit = h.lastCurrent();
    try testing.expectEqual(Note{ .split = .{ .win = vsplit, .side_by_side = true, .before = false, .from = split } }, h.splitNote(vsplit).?);
    try close(&h.nvim, vsplit);
    try h.nvim.flush();
    h.gone = vsplit;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.gone) == null;
        }
    }.f));
    // back to `split`, wherever Neovim went
    h.notes.clearRetainingCapacity();
    _ = try setCurrent(&h.nvim, first);
    _ = try setCurrent(&h.nvim, split);
    try h.nvim.flush();
    h.want = split;
    try testing.expect(try h.until(Harness.isCurrent));

    // its pane went away
    h.notes.clearRetainingCapacity();
    try close(&h.nvim, split);
    try h.nvim.flush();
    h.want = first;
    h.gone = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.isCurrent() and x.kind(x.gone) == null;
        }
    }.f));

    // :close on the last window reopens it
    h.notes.clearRetainingCapacity();
    try h.input(":close<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and cur != x.want and x.kind(cur) == .external and x.kind(x.want) == null;
        }
    }.f));
    try testing.expectEqual(Note{ .reopen = first }, h.notes.items[0]);

    // :q on the last window quits
    try h.input(":q<CR>");
    try testing.expect(!try h.until(struct {
        fn f(_: *Harness) bool {
            return false;
        }
    }.f));
}

test "a window a plugin moved back into the layout is external again, and keeps focus" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.boot();
    const anchor = h.wins[1];
    const split = try h.adoptSplit();

    // as edgy.nvim does on WinResized: back into the layout. Nothing told the
    // daemon, so only Neovim can put it back.
    try h.sync("vim.api.nvim_win_set_config({d}, {{ split = 'left', win = {d} }})", .{ split, anchor });
    try h.expectExternal(split);
    try h.sync(
        \\local win = {d}
        \\assert(vim.api.nvim_get_current_win() == win, 'lost focus')
        \\assert(vim.w[win].edgy_disable, 'edgy.nvim would move it again')
    , .{split});
    try testing.expect(try h.untilExternal(split));
}

/// Typed, or a pane resized, between pull-backs more often than a fight's
/// limit: each is something the user did.
fn pullBacksBetween(comptime between: enum { typing, resizing }) !void {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.boot();
    const anchor = h.wins[1];
    const split = try h.adoptSplit();

    for (0..12) |i| {
        try h.sync("vim.api.nvim_win_set_config({d}, {{ split = 'left', win = {d} }})", .{ split, anchor });
        try h.expectExternal(split);
        switch (between) {
            .typing => try h.typed(),
            .resizing => {
                try size(&h.nvim, split, 20 + @as(u32, @intCast(i)), 9);
                try h.nvim.flush();
            },
        }
    }
}

test "typing between pull-backs isn't a fight" {
    try pullBacksBetween(.typing);
}

test "resizing a pane between pull-backs isn't a fight" {
    try pullBacksBetween(.resizing);
}

/// A plugin that puts the window back in the layout every 10 ms for 1.5 s,
/// running `pull_extra` as well, and the daemon, which asks for it external
/// and sized to its pane every 50 ms as it does for a window it sees
/// ordinary. The plugin says how often it did, at 1 s and at 1.5 s, as
/// `fight_at_1s` + n and `fight_at_end` + n.
fn fightEndsSoon(comptime pull_extra: []const u8) !void {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.boot();
    const anchor = h.wins[1];
    const split = try h.adoptSplit();

    try size(&h.nvim, split, 17, 9);
    var code: [1536]u8 = undefined;
    try h.lua(try std.fmt.bufPrint(&code,
        \\local win, anchor, pulls = {d}, {d}, 0
        \\local timer, daemon = vim.uv.new_timer(), vim.uv.new_timer()
        \\timer:start(10, 10, vim.schedule_wrap(function()
        \\  if vim.api.nvim_win_get_config(win).external then
        \\    pulls = pulls + 1
        \\    vim.api.nvim_win_set_config(win, {{ split = 'left', win = anchor }})
        \\    {s}
        \\  end
        \\end))
        \\daemon:start(50, 50, vim.schedule_wrap(function()
        \\  neorex.adopt(win, 17, 9)
        \\  neorex.size(win, 17, 9)
        \\end))
        \\vim.defer_fn(function() vim.rpcnotify(0, 'neorex_win', {d} + pulls) end, 1000)
        \\vim.defer_fn(function()
        \\  timer:stop()
        \\  daemon:stop()
        \\  vim.rpcnotify(0, 'neorex_win', {d} + pulls)
        \\end, 1500)
    , .{ split, anchor, pull_extra, fight_at_1s, fight_at_end }));
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.lastCurrent() >= fight_at_end;
        }
    }.f));
    var pulls_at_1s: i64 = 0;
    for (h.notes.items) |n| if (n == .current and n.current >= fight_at_1s and n.current < fight_at_end) {
        pulls_at_1s = n.current - fight_at_1s;
    };
    const pulls = h.lastCurrent() - fight_at_end;
    // it gave up, as the plugin left it no other way, and didn't go on
    try testing.expect(pulls >= 1 and pulls <= 10);
    try testing.expectEqual(pulls_at_1s, pulls);
    // and sizing it to its pane would resize the window in Neovim's layout
    try h.sync(
        \\local win = {d}
        \\assert(not vim.api.nvim_win_get_config(win).external, 'was never given up')
        \\assert(vim.api.nvim_win_get_width(win) ~= 17, 'sized as if in its pane')
    , .{split});

    // a pane resized while it waits: the window fits it once it's back
    try size(&h.nvim, split, 25, 9);
    // once the plugin stops and the wait is over, it goes back in its pane
    try h.expectExternal(split);
    try h.sync("assert(vim.api.nvim_win_get_width({d}) == 25, 'not the pane the user left it')", .{split});
}

test "a plugin that always moves a window back doesn't start a fight without end" {
    try fightEndsSoon("");
}

test "a plugin that runs :normal and feedkeys to move a window back doesn't either" {
    // none of that is the user typing
    try fightEndsSoon("vim.cmd.normal({ 'lh', bang = true }); vim.api.nvim_feedkeys('l', 'n', false)");
}

test "a fight that goes on is left alone twice as long each time" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.boot();
    const anchor = h.wins[1];
    const split = try h.adoptSplit();

    // A plugin that puts the window back whenever it can, noting when each
    // fight starts, until the third has gone on a second, and a heartbeat so
    // the wait doesn't look like a hang.
    var code: [1536]u8 = undefined;
    try h.lua(try std.fmt.bufPrint(&code,
        \\local win, anchor = {d}, {d}
        \\local function now() return vim.uv.hrtime() / 1e6 end
        \\local began = now()
        \\local last = 0
        \\bursts = {{}}
        \\local timer, beat = vim.uv.new_timer(), vim.uv.new_timer()
        \\timer:start(10, 10, vim.schedule_wrap(function()
        \\  if not vim.api.nvim_win_get_config(win).external then return end
        \\  local t = now()
        \\  if t - last > 500 then table.insert(bursts, t) end
        \\  last = t
        \\  vim.api.nvim_win_set_config(win, {{ split = 'left', win = anchor }})
        \\end))
        \\beat:start(500, 500, vim.schedule_wrap(function()
        \\  vim.rpcnotify(0, 'neorex_tabline', 0)
        \\  if (#bursts >= 3 and now() - bursts[3] > 1000) or now() - began > 40000 then
        \\    timer:stop()
        \\    beat:stop()
        \\    vim.rpcnotify(0, 'neorex_win', {d})
        \\  end
        \\end))
    , .{ split, anchor, fight_at_end }));
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.lastCurrent() >= fight_at_end;
        }
    }.f));
    // each wait at least as long as it should be, 2 s then 4 s, and the
    // window goes back after each
    try h.sync(
        \\assert(#bursts >= 3, 'did not fight three times')
        \\assert(bursts[2] - bursts[1] >= 1900, 'first wait too short')
        \\assert(bursts[3] - bursts[2] >= 3900, 'second wait too short')
    , .{});
}

/// Waits until Neovim runs a command that says `neorex_win` 4242, after
/// everything typed before it.
fn barrier(h: *Harness) !void {
    try h.input(":call rpcnotify(0, 'neorex_win', 4242)<CR>");
    h.want = 4242;
    try testing.expect(try h.until(Harness.isCurrent));
}

/// `want` is what the notes since the last clear say, in order, then the barrier's.
fn expectNotes(h: *const Harness, want: []const Note) !void {
    try testing.expectEqual(want.len + 1, h.notes.items.len);
    for (want, h.notes.items[0..want.len]) |w, got| try testing.expectEqual(w, got);
    try testing.expectEqual(Note{ .current = 4242 }, h.notes.items[want.len]);
}

test "<C-w>h/j/k/l send a direction and leave the current window, <C-w>w still cycles" {
    var buf: [1024]u8 = undefined;
    var h: Harness = undefined;
    try h.start(&buf);
    defer h.deinit();
    const first = h.wins[0];
    const split = try splitAndAdopt(&h);

    // Neovim can't tell where the panes sit: it only says which way
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>l<C-w><C-k>3<C-w>j");
    // the other spellings: <C-h> arrives as <BS> from a terminal
    try h.input("<C-w><BS><C-w><Left><C-w><Down><C-w><Up><C-w><Right>");
    // from Visual mode too
    try h.input("v<C-w>h<Esc>");
    // a mapping built on them, as LazyVim's
    try h.input(":nmap <lt>C-h> <lt>C-w>h<CR><C-h>");
    try barrier(&h);
    try expectNotes(&h, &.{
        .{ .direction = .{ .dir = .right, .count = 1 } },
        .{ .direction = .{ .dir = .up, .count = 1 } },
        .{ .direction = .{ .dir = .down, .count = 3 } },
        .{ .direction = .{ .dir = .left, .count = 1 } },
        .{ .direction = .{ .dir = .left, .count = 1 } },
        .{ .direction = .{ .dir = .down, .count = 1 } },
        .{ .direction = .{ .dir = .up, .count = 1 } },
        .{ .direction = .{ .dir = .right, .count = 1 } },
        .{ .direction = .{ .dir = .left, .count = 1 } },
        .{ .direction = .{ .dir = .left, .count = 1 } },
    });

    // <C-w>w and <C-w>W cycle as before: through the anchor to the first window
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>w");
    h.want = first;
    try testing.expect(try h.until(Harness.isCurrent));
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>W");
    h.want = split;
    try testing.expect(try h.until(Harness.isCurrent));
}

test "a new tab page's window is marked, and closing it closes the tab page" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and x.lastCurrent() == x.wins[0] and x.kind(x.wins[0]) == .external;
        }
    }.f));
    const first = h.wins[0];

    h.notes.clearRetainingCapacity();
    try h.input(":tabnew<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and x.kind(cur) == .ordinary;
        }
    }.f));
    const tab = h.lastCurrent();
    const tab_note = h.splitNote(tab).?.split;
    try testing.expect(tab_note.tab != 0);
    try testing.expectEqual(Split{ .win = tab, .side_by_side = false, .before = false, .from = 0, .tab = tab_note.tab }, tab_note);
    try adopt(&h.nvim, tab, 30, 8);
    try h.nvim.flush();
    h.want = tab;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external;
        }
    }.f));

    h.notes.clearRetainingCapacity();
    try h.input(":vsplit<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and cur != x.want and x.kind(cur) == .ordinary;
        }
    }.f));
    const split = h.lastCurrent();
    // a split in it belongs to it
    try testing.expectEqual(tab_note.tab, h.splitNote(split).?.split.tab);
    // its first window closing leaves the tab page to its other windows, as
    // Diffview replaces it
    try close(&h.nvim, tab);
    try h.nvim.flush();
    h.gone = tab;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.gone) == null;
        }
    }.f));
    var cmd_buf: [192]u8 = undefined;
    _ = try h.nvim.request("nvim_command", 1);
    try msgpack.writeStr(h.nvim.writer(), try std.fmt.bufPrint(&cmd_buf, "if tabpagenr('$') != 2 || !nvim_win_is_valid({d}) | throw 'tab page closed' | endif | call rpcnotify(0, 'neorex_win', 4242)", .{split}));
    try h.nvim.flush();
    h.want = 4242;
    try testing.expect(try h.until(Harness.isCurrent));

    // the last pane went away: the tab page goes with its anchor, back to the first
    h.notes.clearRetainingCapacity();
    try close(&h.nvim, split);
    try h.nvim.flush();
    h.want = first;
    h.gone = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.isCurrent() and x.kind(x.gone) == null;
        }
    }.f));
    // an error fails the wait; else a made-up report ends it
    _ = try h.nvim.request("nvim_command", 1);
    try msgpack.writeStr(h.nvim.writer(), try std.fmt.bufPrint(&cmd_buf, "if tabpagenr('$') != 1 || win_getid() != {d} | throw 'tab page left' | endif | call rpcnotify(0, 'neorex_win', 4242)", .{first}));
    try h.nvim.flush();
    h.want = 4242;
    try testing.expect(try h.until(Harness.isCurrent));
}

test "<C-w>l sends a direction from a tab page's window too, but not from a float" {
    var buf: [1024]u8 = undefined;
    var h: Harness = undefined;
    try h.start(&buf);
    defer h.deinit();

    // a tab page's window shows in a Rex floating pane
    try h.input(":tabnew<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and x.kind(cur) == .ordinary;
        }
    }.f));
    const tab = h.lastCurrent();
    try testing.expect(h.splitNote(tab).?.split.tab != 0);
    try adopt(&h.nvim, tab, 30, 8);
    try h.nvim.flush();
    h.want = tab;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external;
        }
    }.f));

    h.notes.clearRetainingCapacity();
    try h.input("<C-w>h");
    try barrier(&h);
    try expectNotes(&h, &.{.{ .direction = .{ .dir = .left, .count = 1 } }});

    // a float isn't a pane: Neovim moves out of it itself
    try h.input(":lua vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, { relative = 'editor', row = 1, col = 1, width = 8, height = 2 })<CR>");
    h.notes.clearRetainingCapacity();
    try h.input("<C-w>l");
    try barrier(&h);
    for (h.notes.items) |n| try testing.expect(n != .direction);
}

test ":tabclose from a tab page's window reopens the windows that followed it in" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and x.lastCurrent() == x.wins[0] and x.kind(x.wins[0]) == .external;
        }
    }.f));
    const first = h.wins[0];

    // two windows: the last one would reopen anyway, the other wouldn't
    try h.input(":vsplit<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and cur != x.wins[0] and x.kind(cur) == .ordinary;
        }
    }.f));
    const split = h.lastCurrent();
    try adopt(&h.nvim, split, 20, 9);
    try h.nvim.flush();
    h.want = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external;
        }
    }.f));

    try h.input(":tabnew<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and cur != x.want and x.kind(cur) == .ordinary;
        }
    }.f));
    const tab = h.lastCurrent();
    try adopt(&h.nvim, tab, 30, 8);
    try h.nvim.flush();
    h.want = tab;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external;
        }
    }.f));

    // closes every other window in the tab page, which holds them all
    h.notes.clearRetainingCapacity();
    try h.input(":tabclose<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            var n: usize = 0;
            for (x.notes.items) |note| if (note == .reopened and x.kind(note.reopened.new) == .external) {
                n += 1;
            };
            return n == 2 and x.kind(x.want) == null;
        }
    }.f));
    var olds: [2]i64 = undefined;
    var k: usize = 0;
    for (h.notes.items) |note| if (note == .reopened) {
        olds[k] = note.reopened.old;
        k += 1;
    };
    std.mem.sort(i64, &olds, {}, std.sort.asc(i64));
    try testing.expectEqualSlices(i64, &.{ first, split }, &olds);
}

test "the tabline is reported and drawn on grid 1" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and x.kind(x.wins[0]) == .external and x.lastTabline() == 0;
        }
    }.f));

    // as bufferline does, with a second buffer
    try h.input(":file one | lua vim.o.showtabline = 2<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const screen = x.grids.get(1) orelse return false;
            var text: [256]u8 = undefined;
            return x.lastTabline() == 1 and screen.height > 0 and std.mem.indexOf(u8, Grid.rowText(screen, 0, &text), "one") != null;
        }
    }.f));

    // a second tab page shows it with showtabline=1
    try h.input(":set showtabline=1<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.lastTabline() == 0;
        }
    }.f));
    try h.input(":tabnew<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.lastTabline() == 1;
        }
    }.f));
}

/// Grid row `r` of window `win` starts with `want`.
fn rowStarts(h: *const Harness, win: i64, r: u32, want: []const u8) bool {
    const g = h.grids.get(h.grids.gridOf(win) orelse return false) orelse return false;
    if (r >= g.height) return false;
    var buf: [256]u8 = undefined;
    return std.mem.startsWith(u8, Grid.rowText(g, r, &buf), want);
}

fn gridHeight(h: *const Harness, win: i64) u32 {
    const g = h.grids.get(h.grids.gridOf(win) orelse return 0) orelse return 0;
    return g.height;
}

test "each window shows a statusline inside its pane" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();

    // setup sizes the first window to 40 x 9: 8 rows of text and the statusline
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and gridHeight(x, x.wins[0]) == 9 and rowStarts(x, x.wins[0], 8, "[No Name]");
        }
    }.f));

    // :set statusline reaches the window
    try h.input(":set statusline=GLOBAL<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return rowStarts(x, x.wins[0], 8, "GLOBAL");
        }
    }.f));

    // no statusline: the text gets the row back
    try h.input(":set laststatus=0<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return gridHeight(x, x.wins[0]) == 9 and rowStarts(x, x.wins[0], 8, "~");
        }
    }.f));

    // the user's own statusline for a window stays
    try h.input(":set laststatus=2 | setlocal statusline=MINE | lua vim.go.statusline = 'OTHER'<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return gridHeight(x, x.wins[0]) == 9 and rowStarts(x, x.wins[0], 8, "MINE");
        }
    }.f));
}

/// Splits the current window of `h`, which is already set up, and adopts the new one as a
/// 20 x 9 pane. Returns it.
fn splitAndAdopt(h: *Harness) !i64 {
    h.notes.clearRetainingCapacity();
    try h.input(":vsplit<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const cur = x.lastCurrent();
            return cur != 0 and x.kind(cur) == .ordinary;
        }
    }.f));
    const split = h.lastCurrent();
    try adopt(&h.nvim, split, 20, 9);
    try h.nvim.flush();
    h.want = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.kind(x.want) == .external and gridHeight(x, x.want) == 9;
        }
    }.f));
    return split;
}

/// Waits until both panes `a` and `b` show `want` in their last row.
fn bothShow(h: *Harness, a: i64, b: i64, want: []const u8) !void {
    h.pane_a = a;
    h.pane_b = b;
    h.text = want;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return rowStarts(x, x.pane_a, 8, x.text) and rowStarts(x, x.pane_b, 8, x.text);
        }
    }.f));
}

test "laststatus=3 shows the statusline in every window's pane" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n", "--cmd", "set laststatus=3 statusline=GLOBAL" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();

    // set before neorex started: Neovim alone would draw no statusline here
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and gridHeight(x, x.wins[0]) == 9 and rowStarts(x, x.wins[0], 8, "GLOBAL");
        }
    }.f));
    const first = h.wins[0];
    const split = try splitAndAdopt(&h);
    try bothShow(&h, first, split, "GLOBAL");

    // set after it started: the value changes from 2, so OptionSet fires
    try h.input(":set laststatus=3 statusline=LATER<CR>");
    try bothShow(&h, first, split, "LATER");

    // set in an autocommand, as LazyVim does for lualine: no OptionSet
    try h.input(":set laststatus=2<CR>:autocmd User Later ++once set laststatus=3 statusline=INAUTO<CR>:doautocmd User Later<CR>");
    try bothShow(&h, first, split, "INAUTO");

    // From here no pane's statusline may go blank. A global statusline plugin
    // keeps only the current window's 'statusline' up to date: the other
    // panes show that line, whichever window is current.
    try h.input(":lua vim.api.nvim_create_autocmd('OptionSet', { pattern = 'statusline', callback = function() if vim.v.option_new == '' then vim.g.blanked = 1 end end })<CR>");
    var cmd: [128]u8 = undefined;
    try h.input(":setlocal statusline=CURRENT<CR>");
    try bothShow(&h, first, split, "CURRENT");
    // the other window current: its own line, set for that window only
    try h.input(try std.fmt.bufPrint(&cmd, ":lua vim.api.nvim_set_current_win({d}); vim.wo[0][0].statusline = 'OTHER'<CR>", .{first}));
    try bothShow(&h, first, split, "OTHER");
    // and back: the pane that showed a copy has its own line again
    try h.input(try std.fmt.bufPrint(&cmd, ":call nvim_set_current_win({d})<CR>", .{split}));
    try bothShow(&h, first, split, "CURRENT");
    // changed in an autocommand: no OptionSet
    try h.input(":autocmd User Chg ++once setlocal statusline=INAUTOCMD<CR>:doautocmd User Chg<CR>");
    try bothShow(&h, first, split, "INAUTOCMD");

    // a focused float: the panes keep their line
    try h.input(":lua vim.api.nvim_open_win(vim.api.nvim_create_buf(false, true), true, { relative = 'editor', row = 1, col = 1, width = 8, height = 2 })<CR>");
    try h.input(":set laststatus=3<CR>");
    try bothShow(&h, first, split, "INAUTOCMD");
    try h.input(":close<CR>:setlocal statusline=AFTER<CR>");
    try bothShow(&h, first, split, "AFTER");

    _ = try h.nvim.request("nvim_command", 1);
    try msgpack.writeStr(h.nvim.writer(), "if get(g:, 'blanked') | throw 'a pane lost its statusline' | endif | call rpcnotify(0, 'neorex_win', 4242)");
    try h.nvim.flush();
    h.want = 4242;
    try testing.expect(try h.until(Harness.isCurrent));
}

test "a window's own statusline under laststatus=3 goes when it loses focus" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n", "--cmd", "set laststatus=3 statusline=GLOBAL" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and gridHeight(x, x.wins[0]) == 9;
        }
    }.f));
    const first = h.wins[0];
    const split = try splitAndAdopt(&h);

    // a window such as quickfix sets its own statusline: every pane shows it
    try h.input(":setlocal statusline=QF<CR>");
    try bothShow(&h, first, split, "QF");
    // and leaves it with the focus
    var cmd: [128]u8 = undefined;
    try h.input(try std.fmt.bufPrint(&cmd, ":call nvim_set_current_win({d})<CR>", .{first}));
    try bothShow(&h, first, split, "GLOBAL");
    // each pane has its own again when mirroring ends: split's from before
    try h.input(":set laststatus=2 | setlocal statusline=MINE<CR>");
    h.pane_a = first;
    h.pane_b = split;
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return rowStarts(x, x.pane_a, 8, "MINE") and rowStarts(x, x.pane_b, 8, "QF");
        }
    }.f));
}

test "a window's own statusline leaves no blank row with laststatus=0" {
    var buf: [1024]u8 = undefined;
    var h: Harness = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
        error.FileNotFound => return error.SkipZigTest,
        else => return err,
    } };
    defer h.deinit();
    try h.nvim.uiAttach(40, 10, .{ .ext_multigrid = true });
    h.chan_id = try askChannel(&h.nvim);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.wins[0] != 0 and gridHeight(x, x.wins[0]) == 9;
        }
    }.f));

    try h.input(":setlocal statusline=MINE | set laststatus=0<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return gridHeight(x, x.wins[0]) == 9 and rowStarts(x, x.wins[0], 8, "~");
        }
    }.f));
}

/// The windows of `layout`, left to right, if it's one row of them.
fn layoutRow(layout: *const Layout, out: []Layout.Node) ![]const Layout.Node {
    const nodes = layout.items();
    try testing.expectEqual(Layout.Kind.row, nodes[0].kind);
    var kids = layout.kids(0);
    var n: usize = 0;
    while (kids.next()) |kid| : (n += 1) {
        try testing.expectEqual(Layout.Kind.window, nodes[kid].kind);
        if (n < out.len) out[n] = nodes[kid];
    }
    try testing.expectEqual(out.len, n);
    return out;
}

test "a tab page built as Diffview does reports its layout, final order and sizes" {
    var buf: [1024]u8 = undefined;
    var h: Harness = undefined;
    try h.startSized(&buf, 120, 30);
    defer h.deinit();
    const first = h.wins[0];

    // a tab page with a pivot window; two windows left of it, the pivot
    // closed, then a narrow file panel left of all: made in the order
    // old, new, panel, shown as panel, old, new
    h.notes.clearRetainingCapacity();
    try h.lua(
        \\local api = vim.api
        \\vim.cmd('tabnew')
        \\local pivot = api.nvim_get_current_win()
        \\for _ = 1, 2 do
        \\  api.nvim_win_call(pivot, function() vim.cmd('aboveleft vsplit') end)
        \\end
        \\api.nvim_win_close(pivot, true)
        \\vim.cmd('topleft vsplit')
        \\vim.cmd('vertical resize 20')
        \\vim.rpcnotify(0, 'neorex_win', 4242)
    );
    // the layout comes once the command is done
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            for (x.notes.items) |note| if (note == .layout) return true;
            return false;
        }
    }.f));
    var made: [4]i64 = undefined;
    var n: usize = 0;
    var tab: i64 = 0;
    for (h.notes.items) |note| if (note == .split) {
        made[n] = note.split.win;
        n += 1;
        if (note.split.tab != 0) tab = note.split.tab;
    };
    // the pivot, old, new, panel: the layout names the three left, by size
    try testing.expectEqual(@as(usize, 4), n);
    var shown: [3]Layout.Node = undefined;
    const layout = h.lastLayout(tab) orelse return error.NoLayout;
    _ = try layoutRow(&layout, &shown);
    try testing.expectEqual(made[3], shown[0].win);
    try testing.expectEqual(made[1], shown[1].win);
    try testing.expectEqual(made[2], shown[2].win);
    try testing.expect(shown[0].w >= 15 and shown[0].w <= 25);
    try testing.expect(shown[1].w > 30 and shown[2].w > 30);

    // once external, they're out of Neovim's own layout: it's still told
    h.tab = tab;
    h.watch = .{ made[1], made[2], made[3] };
    for (h.watch) |win| try adopt(&h.nvim, win, 30, 8);
    try h.nvim.flush();
    try testing.expect(try h.until(Harness.watchedExternal));
    _ = try layoutRow(&h.lastLayout(tab).?, &shown);
    try testing.expectEqual(made[3], shown[0].win);
    try testing.expectEqual(made[1], shown[1].win);
    try testing.expectEqual(made[2], shown[2].win);
    try testing.expect(shown[0].w < shown[1].w and shown[0].w < shown[2].w);

    // the panel closing leaves the other two
    h.notes.clearRetainingCapacity();
    try close(&h.nvim, made[3]);
    try h.nvim.flush();
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            return l.len == 3;
        }
    }.f));
    var two: [2]Layout.Node = undefined;
    _ = try layoutRow(&h.lastLayout(tab).?, &two);
    try testing.expectEqual(made[1], two[0].win);
    try testing.expectEqual(made[2], two[1].win);

    // a file panel again, toggled back on: leftmost again, the others keeping their shares
    h.notes.clearRetainingCapacity();
    try h.lua(
        \\vim.cmd('topleft vsplit')
        \\vim.cmd('vertical resize 20')
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            return l.len == 4;
        }
    }.f));
    _ = try layoutRow(&h.lastLayout(tab).?, &shown);
    try testing.expectEqual(made[1], shown[1].win);
    try testing.expectEqual(made[2], shown[2].win);
    try testing.expect(shown[0].w >= 15 and shown[0].w <= 25);
    // 59 and 39 cells before
    try testing.expect(shown[1].w > shown[2].w + 10);

    // Diffview's Panel:resize, on a window that's external: Neovim's size
    // for it is the layout's, not what its pane fits it to
    h.notes.clearRetainingCapacity();
    var cmd_buf: [128]u8 = undefined;
    try h.lua(try std.fmt.bufPrint(&cmd_buf, "vim.api.nvim_win_set_width({d}, 45)", .{made[1]}));
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            for (l.items()) |node| if (node.kind == .window and node.w == 45) return true;
            return false;
        }
    }.f));

    // a window as wide as the editor leaves the layout's windows a cell
    // for all of them: every size is still at least one
    h.notes.clearRetainingCapacity();
    try h.lua(
        \\vim.cmd('vnew')
        \\vim.cmd('vertical resize 999')
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            return l.len == 5;
        }
    }.f));
    for (h.lastLayout(tab).?.items()) |node| try testing.expect(node.w >= 1 and node.h >= 1);

    // :tabclose, as :DiffviewClose does: the windows go, and the window
    // current before comes back, reopened as it was
    h.notes.clearRetainingCapacity();
    try h.input(":tabclose<CR>");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            for (x.notes.items) |note| if (note == .reopened and x.kind(note.reopened.new) == .external) {
                return x.lastCurrent() == note.reopened.new and x.kind(x.watch[0]) == null and x.kind(x.watch[1]) == null;
            };
            return false;
        }
    }.f));
    for (h.notes.items) |note| if (note == .reopened) try testing.expectEqual(first, note.reopened.old);
    _ = try h.nvim.request("nvim_command", 1);
    try msgpack.writeStr(h.nvim.writer(), "if tabpagenr('$') != 1 | throw 'tab page left' | endif | call rpcnotify(0, 'neorex_win', 4242)");
    try h.nvim.flush();
    h.want = 4242;
    try testing.expect(try h.until(Harness.isCurrent));
}

/// Parses a `neorex_layout` note of tab page 7 with `flat` numbers.
fn parseLayoutNote(flat: []const i64) !?Note {
    var buf: [256]u8 = undefined;
    var w: std.Io.Writer = .fixed(&buf);
    try msgpack.writeArrayHeader(&w, 2);
    try msgpack.writeInt(&w, 7);
    try msgpack.writeArrayHeader(&w, @intCast(flat.len));
    for (flat) |n| try msgpack.writeInt(&w, n);
    var d: msgpack.Decoder = .{};
    d.feed(w.buffered());
    return parseNote("neorex_layout", &d);
}

test "a layout note is read, empty or not, and a note that doesn't add up is an error" {
    const empty = (try parseLayoutNote(&.{})).?.layout;
    try testing.expectEqual(@as(i64, 7), empty.tab);
    try testing.expectEqual(@as(usize, 0), empty.len);

    // a row of two windows
    const row = (try parseLayoutNote(&.{ 1, 2, 30, 5, 0, 1000, 10, 5, 0, 1001, 20, 5 })).?.layout;
    try testing.expectEqual(@as(usize, 3), row.len);
    var kids = row.kids(0);
    try testing.expectEqual(@as(?usize, 1), kids.next());
    try testing.expectEqual(@as(?usize, 2), kids.next());
    try testing.expectEqual(@as(?usize, null), kids.next());
    try testing.expectEqual(@as(i64, 1001), row.nodes[2].win);

    // a negative size, a row of one, an unknown kind, too few or too many numbers
    try testing.expectError(error.BadMessage, parseLayoutNote(&.{ 1, 2, -3, 5, 0, 1000, 10, 5, 0, 1001, 20, 5 }));
    try testing.expectError(error.BadMessage, parseLayoutNote(&.{ 1, 1, 30, 5, 0, 1000, 30, 5 }));
    try testing.expectError(error.BadMessage, parseLayoutNote(&.{ 9, 1000, 30, 5 }));
    try testing.expectError(error.BadMessage, parseLayoutNote(&.{ 1, 2, 30, 5, 0, 1000, 10, 5 }));
    try testing.expectError(error.BadMessage, parseLayoutNote(&.{ 0, 1000, 30, 5, 0, 1001, 10, 5 }));
}

test "a tab page's layout follows windows moved before they're external, and none is told once it's too big" {
    var buf: [1024]u8 = undefined;
    var h: Harness = undefined;
    try h.startSized(&buf, 120, 30);
    defer h.deinit();

    // two windows; the one made last, on the left, moved to the far right
    h.notes.clearRetainingCapacity();
    try h.lua(
        \\vim.cmd('tabnew')
        \\vim.cmd('vsplit')
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            for (x.notes.items) |note| if (note == .layout) return true;
            return false;
        }
    }.f));
    var made: [2]i64 = undefined;
    var n: usize = 0;
    for (h.notes.items) |note| if (note == .split) {
        made[n] = note.split.win;
        n += 1;
        h.tab = note.split.tab;
    };
    try testing.expectEqual(@as(usize, 2), n);
    var two: [2]Layout.Node = undefined;
    _ = try layoutRow(&h.lastLayout(h.tab).?, &two);
    try testing.expectEqual(made[1], two[0].win);

    h.notes.clearRetainingCapacity();
    try h.lua("vim.cmd('wincmd L')");
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            return l.nodes[1].win != x.watch[0] and l.len == 3;
        }
    }.f));
    _ = try layoutRow(&h.lastLayout(h.tab).?, &two);
    try testing.expectEqual(made[0], two[0].win);
    try testing.expectEqual(made[1], two[1].win);

    // more windows than the layout has room for: the daemon is told to
    // drop the layout it has
    h.notes.clearRetainingCapacity();
    try h.lua(
        \\for _ = 1, 6 do vim.cmd('vsplit') end
        \\for _, win in ipairs(vim.api.nvim_tabpage_list_wins(0)) do
        \\  if not vim.api.nvim_win_get_config(win).external then
        \\    vim.api.nvim_win_call(win, function() for _ = 1, 6 do vim.cmd('split') end end)
        \\  end
        \\end
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            const l = x.lastLayout(x.tab) orelse return false;
            return l.len == 0;
        }
    }.f));
    // not a layout of some of them before
    var splits: usize = 0;
    for (h.notes.items) |note| if (note == .split) {
        splits += 1;
    };
    try testing.expect(splits >= 54);
}

test "an idle floating tab page doesn't keep Neovim busy" {
    var buf: [1024]u8 = undefined;
    var h: Harness = undefined;
    try h.startSized(&buf, 120, 30);
    defer h.deinit();
    try h.lua(
        \\vim.cmd('tabnew')
        \\vim.cmd('vsplit')
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            for (x.notes.items) |note| if (note == .layout) return true;
            return false;
        }
    }.f));

    // SafeState fires again after every scheduled callback, so one that
    // schedules work keeps Neovim looping, while a floating tab page is current
    try h.lua(
        \\_G.safe_states = 0
        \\vim.api.nvim_create_autocmd('SafeState', { callback = function() _G.safe_states = _G.safe_states + 1 end })
        \\vim.defer_fn(function() vim.rpcnotify(0, 'neorex_tabline', 1000 + _G.safe_states) end, 300)
    );
    try testing.expect(try h.until(struct {
        fn f(x: *Harness) bool {
            return x.lastTabline() >= 1000;
        }
    }.f));
    try testing.expect(h.lastTabline() - 1000 < 20);
}
