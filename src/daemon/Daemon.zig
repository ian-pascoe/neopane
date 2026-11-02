//! The running daemon: Neovim, its grids, the Rex connection, a pane per
//! window and a floating pane per group of floats that took focus. After
//! each Neovim flush, `reconcile` gives new windows and float groups panes
//! and hangs up panes whose windows closed; then every pane is drawn.
// TODO: Needs a pretty major rewrite

const std = @import("std");
const net = std.Io.net;
const Allocator = std.mem.Allocator;
const input = @import("../term/input.zig");
const Nvim = @import("../nvim/Nvim.zig");
const msgpack = @import("../nvim/msgpack.zig");
const rpc = @import("../nvim/rpc.zig");
const windows = @import("../nvim/windows.zig");
const Forwarder = @import("../nvim/Forwarder.zig");
const Grids = @import("../screen/Grids.zig");
const Grid = @import("../screen/Grid.zig");
const FloatGroups = @import("../screen/FloatGroups.zig");
const PaneView = @import("../screen/PaneView.zig");
const Client = @import("../rex/Client.zig");
const Loop = @import("Loop.zig");
const Pane = @import("Pane.zig");
const Panes = @import("Panes.zig");
const RexSession = @import("../rex/Session.zig");
const NvimSession = @import("NvimSession.zig");
const WindowChanges = @import("WindowChanges.zig");
const Surface = @import("Surface.zig");
const pane_helper = @import("pane_helper.zig");

const Daemon = @This();

gpa: Allocator,
io: std.Io,
loop: *Loop,
/// Our executable and the helpers' socket, for new panes' command.
exe: []const u8,
sock: []const u8,
grids: Grids = .{},
panes: Panes = .{},
rex: RexSession,
nvim: NvimSession,
changes: WindowChanges = .{},
/// Rows the screen's tabline takes, as Neovim last said. Drawn over the
/// panes along the top of the Rex window, as it would be over the screen.
tabline_rows: u32 = 0,
/// Set by `stop`; `run` returns it.
stopped: ?StopReason = null,

pub const StopReason = enum { nvim_exited, pane_gone, signal };

/// What a loop event's token names; pane tokens fall in `_`.
pub const Token = enum(usize) { signal, nvim_socket, helper_listener, rex_socket, focus_timer, layout_timer, _ };
pub const PaneEvent = struct { pane: *Pane, fd: Pane.PaneFd };

comptime {
    // the fixed tokens must stay below the pane tokens
    for (@typeInfo(Token).@"enum".field_values) |v| std.debug.assert(v < Pane.token_base);
}

/// The id `Pane.startup` gives the pane neorex was run in.
const startup_pane_id = 0;

/// How long losing focus waits before Neovim hears of it: focus moving
/// from one pane to another, such as into a floating pane, arrives as an
/// out then an in, and shouldn't fire FocusLost and FocusGained.
const focus_out_delay_ms = 80;

/// How long a new split waits for the app to size it before trusting its terminal's size.
const layout_wait_ms = 1000;

/// A new Rex terminal's size, cols x rows, until the app lays it out.
const rex_default_size = [2]u32{ 80, 24 };

/// About how many cells a Rex floating pane's frame takes, across and down.
/// Only a first guess: once made, its floats fit whatever size it is.
const layer_frame = [2]i32{ 3, 2 };

/// Where a new tab page's floating pane sits, as fractions of the window: x, y, w, h.
const tab_layer_bounds = [4]f64{ 0.05, 0.05, 0.9, 0.9 };

pub fn deinit(self: *Daemon) void {
    self.panes.deinit(self.gpa, self.io);
    self.grids.deinit(self.gpa);
    self.changes.deinit(self.gpa);
    self.rex.deinit();
    self.* = undefined;
}

/// Adds the launcher's pane, already connected, and starts talking to
/// Neovim and Rex.
pub fn start(self: *Daemon, launch_pane: Pane) !void {
    // only ever called on startup
    std.debug.assert(self.panes.list.items.len == 0);

    const pane = try self.panes.add(self.gpa, launch_pane);

    try pane.registerWith(self.loop);
    const view = pane.terminal_surface.?.view;
    try self.nvim.attachUi(view.cols, view.rows);
    try self.rex.attach();
}

/// Runs the loop until something calls `stop`, returns why.
pub fn run(self: *Daemon, server: *net.Server) !StopReason {
    var events: [16]Loop.Event = undefined;
    while (self.stopped == null) {
        var helper_waiting = false;
        for (try self.loop.wait(&events)) |e| {
            if (tokenOf(e) == .helper_listener) helper_waiting = true;
            try self.handle(e);
            if (self.stopped != null) break;
        }
        // between batches, so no stale event reaches a new pane
        if (helper_waiting and self.stopped == null) {
            self.accept(server) catch |err| {
                std.debug.print("neorex daemon: a pane failed: {t}\n", .{err});
            };
        }
        if (self.stopped == null) try self.afterBatch();
    }
    return self.stopped.?;
}

/// Ends `run` once the current event is handled.
fn stop(self: *Daemon, reason: StopReason) void {
    self.stopped = reason;
}

fn tokenOf(e: Loop.Event) Token {
    return @fromBackingInt(@intCast(e.token));
}

/// Starts, or restarts, one of our timers.
fn addTimer(self: *Daemon, t: Token, ms: u32) !void {
    try self.loop.addTimer(@backingInt(t), ms, @backingInt(t));
}

/// Handles one loop event.
fn handle(self: *Daemon, e: Loop.Event) !void {
    switch (tokenOf(e)) {
        .signal => self.stop(.signal),
        .nvim_socket => try self.onNvim(),
        .rex_socket => try self.onRex(),
        .focus_timer => try self.sendFocus(true),
        .layout_timer => try self.layoutWaitOver(),
        .helper_listener => {},
        _ => try self.onPane(e.token),
    }
}

/// After each batch of events: tells Neovim whether any pane has focus.
/// Losing it waits a moment, in case another pane gains it.
fn afterBatch(self: *Daemon) !void {
    try self.sendFocus(false);
}

fn sendFocus(self: *Daemon, waited: bool) !void {
    var known = false;
    var any = false;
    for (self.panes.list.items) |p| if (p.terminal_surface) |s| if (s.forwarder.focused) |f| {
        known = true;
        any = any or f;
    };
    if (!known or any == self.nvim.ui_focused) return;
    if (!any and !waited) return self.addTimer(.focus_timer, focus_out_delay_ms);
    self.nvim.ui_focused = any;
    _ = try self.nvim.client.request("nvim_ui_set_focus", 1);
    try msgpack.writeBool(self.nvim.client.writer(), any);
    try self.nvim.client.flush();
}

// panes

/// A new helper connected: match its hello to a pane waiting for it.
fn accept(self: *Daemon, server: *net.Server) !void {
    const hello = try pane_helper.acceptHello(server, self.io);
    // its window may have closed before it connected
    const pane = (if (hello.id) |id| self.panes.byId(id) else null) orelse {
        std.debug.print("neorex daemon: a helper for a pane that's gone\n", .{});
        return hello.close(self.io);
    };
    pane.terminal_surface = Surface.create(self.gpa, hello.stream, hello.tty) catch |err| {
        hello.close(self.io);
        return err;
    };
    try pane.registerWith(self.loop);
    if (pane.awaiting_layout) {
        const v = pane.terminal_surface.?.view;
        if (v.cols != rex_default_size[0] or v.rows != rex_default_size[1]) {
            pane.awaiting_layout = false;
        } else try self.addTimer(.layout_timer, layout_wait_ms);
    }
    try self.sizeWindow(pane);
    if (pane.layer) |*l| if (try self.followLayer(pane)) |req| {
        l.fit = .{ .asked = req };
    };
    try self.nvim.client.flush();
    try self.draw();
}

/// The app never resized a new split: its terminal's size is the real one.
fn layoutWaitOver(self: *Daemon) !void {
    var any = false;
    for (self.panes.list.items) |*p| if (p.awaiting_layout and p.terminal_surface != null) {
        p.awaiting_layout = false;
        any = true;
        try self.sizeWindow(p);
    };
    if (!any) return;
    try self.nvim.client.flush();
    try self.draw();
}

// Decode pane info from token, get the pane object, forward to
// terminal or helper_socket handler depending on which fd the event came from
fn onPane(self: *Daemon, pane_token: usize) !void {
    const decoded = Pane.decodeToken(pane_token);
    // pane might be gone
    const pane = self.panes.byId(decoded.id) orelse return;
    switch (decoded.source_fd) {
        .helper_socket => try self.onHelper(pane),
        .terminal => try self.onInput(pane),
    }
}

fn onHelper(self: *Daemon, pane: *Pane) !void {
    var buf: [64]u8 = undefined;
    const surface = pane.terminal_surface.?;
    const n = std.posix.read(surface.helper_socket.socket.handle, &buf) catch 0;
    if (n == 0) return self.paneGone(pane);

    for (buf[0..n]) |byte| switch (pane_helper.Message.fromByte(byte)) {
        .resized => return self.handleResize(pane),
        .hello => {},
        _ => {},
    };
}

fn handleResize(self: *Daemon, pane: *Pane) !void {
    const surface = pane.terminal_surface.?;
    const window_size = surface.terminal.size() catch return self.paneGone(pane);
    pane.awaiting_layout = false;
    surface.view.resize(window_size.col, window_size.row);
    try self.sizeWindow(pane);
    _ = try self.followLayer(pane);
    if (pane == self.focusedPane()) try self.nvim.fitUiTo(pane);
    try self.nvim.client.flush();
    try self.draw();
}

fn onInput(self: *Daemon, pane: *Pane) !void {
    var buf: [4096]u8 = undefined;
    const surface = pane.terminal_surface.?;
    const n = std.posix.read(surface.terminal.fd, &buf) catch 0;
    if (n == 0) return self.paneGone(pane);

    const bytes = buf[0..n];
    if (self.typedIntoUnfocusedPane(pane, bytes)) {
        try self.makeCurrent(pane);
        try self.rex.focus(pane.rex_block);
    }

    // a floating pane's default grid is set as it draws
    if (pane.layer == null) surface.forwarder.grid = self.gridOfPane(pane) orelse 0;
    _ = try surface.forwarder.feed(self.nvim.client, bytes);
}

/// Typed into a pane Rex hasn't focused, as `rex send` can. Focus reports
/// don't count: Rex sends them as it moves focus away.
fn typedIntoUnfocusedPane(self: *const Daemon, pane: *const Pane, bytes: []const u8) bool {
    const win = pane.neovim_window_handle orelse return false;
    return win != self.nvim.current_win and
        !self.rex.hasFocused(pane.rex_block) and
        !input.onlyFocusReports(bytes);
}

fn newPane(self: *Daemon, win: i64) !void {
    if (self.rex.conn == .closed) return; // no Rex: the window stays in Neovim
    const sp = self.changes.splitOf(win);
    // beside the window it came from, else any pane Rex knows; null for a
    // tab page's floating pane
    // a copy: appending below can move every pane
    const anchor: ?Pane.RexBlock = if (sp.tab != 0) self.tabAnchor(sp) orelse blk: {
        // its tab page's floating pane is on its way: split that once it's made
        for (self.panes.list.items) |q| if (q.tab == sp.tab and q.layer == null) return;
        break :blk null;
    } else blk: {
        if (self.splitFrom(sp)) |f| if (f.tab == 0) if (f.rex_block) |b| break :blk b;
        for (self.panes.list.items) |q| if (q.layer == null and q.tab == 0) if (q.rex_block) |b| break :blk b;
        return; // no anchor yet: next flush
    };
    // starts at the guess, not Rex's default
    const size = self.newPaneSize(win);
    const pane = try self.panes.addForWindow(self.gpa, win);
    pane.tab = sp.tab;
    const pane_id = pane.id;
    self.changes.forgetSplit(win);

    var id_buf: [16]u8 = undefined;
    const id = try std.fmt.bufPrint(&id_buf, "{d}", .{pane_id});
    const block = .{
        .flavor = "com.superlogical.terminal.shell",
        .label = "neorex",
        .options = .{
            .shell = "none",
            .command = &[_][]const u8{ self.exe, "pane", self.sock, id },
            .size = .{ .columns = size[0], .rows = size[1] },
        },
    };
    const a = anchor orelse return self.rex.newLayer(pane_id, tab_layer_bounds, .{ .block = block });
    pane.awaiting_layout = true;
    try self.rex.request(.{ .new_split = pane_id }, "session.new_split", .{
        .session_id = self.rex.session_id,
        .anchor_id = @as([]const u8, &a),
        .direction = if (sp.side_by_side) "horizontal" else "vertical",
        .side = if (sp.before) "before" else "after",
        .layout = .{ .block = block },
    });
}

/// Where a window of a tab page made after setup splits: beside a pane of
/// its tab page, the one it came from if it can. Neovim shows the tab page
/// over every other window, so they share a Rex floating pane over theirs;
/// null for the first.
fn tabAnchor(self: *const Daemon, sp: windows.Split) ?Pane.RexBlock {
    if (self.panes.byWin(sp.from)) |f| if (f.tab == sp.tab) if (f.rex_block) |b| return b;
    for (self.panes.list.items) |q| if (q.tab == sp.tab and q.layer == null) if (q.rex_block) |b| return b;
    return null;
}

/// The helper or terminal went away: Rex closed the pane. Close its
/// window, or end if it was the last pane.
fn paneGone(self: *Daemon, p: *Pane) !void {
    p.closing = .rex_pane_closed;
    const i = self.panes.indexOf(p.id).?;
    if (p.layer) |l| {
        // closed in Rex: close its floats, which the plugin notices
        self.panes.remove(self.gpa, self.io, i);
        for (l.group.topGrids()) |top| if (self.grids.winOn(top)) |w| try windows.close(self.nvim.client, w.win);
        try self.nvim.client.flush();
        return;
    }
    var others = false;
    for (self.panes.list.items) |q| {
        if (q.id != p.id and q.closing != .request_failed and q.layer == null) others = true;
    }
    if (!others) {
        self.stop(.pane_gone);
        return;
    }
    const maybe_win = p.neovim_window_handle;
    self.panes.remove(self.gpa, self.io, i);
    const win = maybe_win orelse return;
    try self.changes.markClosing(self.gpa, win);
    try windows.close(self.nvim.client, win);
    try self.nvim.client.flush();
}

// Neovim

fn onNvim(self: *Daemon) !void {
    if (!try self.nvim.client.read(self.gpa)) return self.stop(.nvim_exited);
    while (try self.nvim.client.next()) |msg| switch (msg) {
        .notification => |n| {
            var params = n.params;
            if (std.mem.eql(u8, n.method, "redraw")) {
                if (try self.grids.applyRedraw(self.gpa, &params)) {
                    try self.reconcile();
                    try self.draw();
                }
            } else if (try windows.parseNote(n.method, &params)) |note| switch (note) {
                .current => |win| try self.onCurrent(win),
                .float => |win| try self.onCurrentFloat(win),
                .entered => |win| try self.onEnteredFloat(win),
                .tabline => |rows| try self.onTabline(@intCast(std.math.clamp(rows, 0, 1))),
                .direction => |move| try self.onDirection(move),
                .reopen => |win| if (self.panes.byWin(win) != null) try self.changes.markReopening(self.gpa, win),
                .reopened => |r| self.onReopened(r.old, r.new),
                .split => |sp| if (self.panes.byWin(sp.win) == null) try self.changes.noteSplit(self.gpa, sp),
            };
        },
        .request => |r| try self.nvim.client.respondError(r.id, "neorex serves no requests"),
        .response => |r| {
            var result = r.result;
            if (r.id == self.nvim.current_win_req) self.nvim.current_win_req = null;
            var show_now = false;
            for (self.panes.list.items) |*p| if (p.layer) |*l| if (l.fit == .asked and l.fit.asked == r.id) {
                // nothing moved, so no redraw is coming: show it now
                var res = r.result;
                const moved = r.err == null and (rpc.expectBool(&res) catch false);
                l.fit = if (moved) .answered else .none;
                show_now = !moved;
            };
            if (show_now) try self.draw();
            if (r.err) |e| {
                std.debug.print("neovim error: {s}\n", .{e});
            } else if (r.id == self.nvim.channel_req) {
                const size = self.windowSize(self.panes.byId(startup_pane_id).?);
                self.nvim.setup_req = try windows.setup(self.nvim.client, try windows.parseChannel(&result), size[0], size[1]);
                try self.nvim.client.flush();
            } else if (r.id == self.nvim.setup_req) {
                const wins = try windows.parseSetup(&result);
                if (self.panes.byId(startup_pane_id)) |launch| launch.neovim_window_handle = wins[0];
                self.nvim.ready = true;
                // its window is in its pane now: the editor can span the Rex window
                if (self.focusedPane()) |f| try self.nvim.fitUiTo(f);
                try self.reconcile();
                try self.draw();
            }
        },
    };
}

/// Neovim's current window changed: focus its pane in Rex.
fn onCurrent(self: *Daemon, win: i64) !void {
    // sent before our own change ran; the report after it is coming
    if (self.nvim.current_win_req != null) return;
    self.nvim.current_float = 0;
    self.nvim.current_win = win;
    const p = self.panes.byWin(win) orelse return;
    try self.nvim.fitUiTo(p);
    try self.draw();
    try self.rex.focus(p.rex_block);
}

/// A count on a directional window command repeats the move this many times
/// at most: a window has few panes to cross, and a stray `9999<C-w>l` must not
/// send Rex thousands of requests.
const max_direction_count = 20;

/// `<C-w>h/j/k/l`: Rex moves focus to the pane that way, if any. Its view
/// brings the new focus back, and Neovim's current window follows.
fn onDirection(self: *Daemon, move: windows.Move) !void {
    const dir: RexSession.Direction = switch (move.dir) {
        .left => .left,
        .down => .down,
        .up => .up,
        .right => .right,
    };
    for (0..@min(move.count, max_direction_count)) |_| try self.rex.focusDirection(dir);
}

/// A float became current: if it's in a floating pane, focus that.
fn onCurrentFloat(self: *Daemon, win: i64) !void {
    self.nvim.current_float = win;
    const p = self.layerOfWin(win) orelse return;
    try self.rex.focus(p.rex_block);
}

/// A float took focus. Usually before it's drawn, so the next flush gives
/// its group a floating pane; if it's already showing in a pane, now.
fn onEnteredFloat(self: *Daemon, win: i64) !void {
    self.nvim.noteEntered(win);
    if (!self.nvim.ready or self.grids.gridOf(win) == null or self.layerOfWin(win) != null) return;
    try self.reconcileLayers();
    try self.nvim.client.flush();
    try self.draw();
}

/// Matches panes to windows: new external windows get panes, ordinary ones
/// are made external, and panes whose windows closed hang up.
fn reconcile(self: *Daemon) !void {
    if (!self.nvim.ready) return;
    // this flush has the refitted floats
    for (self.panes.list.items) |*p| if (p.layer) |*l| if (l.fit == .answered) {
        l.fit = .none;
    };
    var i = self.panes.list.items.len;
    while (i > 0) {
        i -= 1;
        const p = &self.panes.list.items[i];
        const win = p.neovim_window_handle orelse continue;
        if (self.grids.gridOf(win) != null) continue;
        // its pane waits for the window that reopens it
        if (self.changes.isReopening(win)) continue;
        p.closing = .window_closed;
        // hanging up closes the Rex pane only once its helper has connected
        if (p.terminal_surface == null) if (p.rex_block) |*b| try self.rex.closeBlock(b);
        self.panes.remove(self.gpa, self.io, i);
    }
    if (self.panes.list.items.len == 0) {
        self.stop(.pane_gone);
        return;
    }
    self.changes.forgetGone(&self.grids);

    for (self.grids.wins.values()) |w| switch (w.kind) {
        .ordinary => {
            if (!try self.changes.shouldAdopt(self.gpa, w.win)) continue;
            const size = if (self.panes.byWin(w.win)) |p| self.windowSize(p) else self.newWindowSize(w.win);
            try windows.adopt(self.nvim.client, w.win, size[0], size[1]);
        },
        .external => {
            self.changes.becameExternal(w.win);
            if (self.panes.byWin(w.win) != null or self.changes.isClosing(w.win)) continue;
            try self.newPane(w.win);
        },
        .float, .hidden => {},
    };
    for (self.panes.list.items) |*p| try self.sizeWindow(p);
    try self.reconcileLayers();
    try self.nvim.client.flush();
}

/// Window `new` reopened `old`, closing: it takes `old`'s pane. The next
/// flush has it, and sizes it to the pane.
fn onReopened(self: *Daemon, old: i64, new: i64) void {
    if (!self.changes.takeReopening(old)) return;
    const p = self.panes.byWin(old) orelse return;
    p.neovim_window_handle = new;
    p.last_asked_win_size = .{};
    if (self.nvim.current_win == old) self.nvim.current_win = new;
}

/// A guess until the new pane says its size: half the pane it splits.
fn newWindowSize(self: *const Daemon, win: i64) [2]u32 {
    const size = self.newPaneSize(win);
    return .{ size[0], @max(size[1] -| 1, 1) };
}

/// A guess at a new split's terminal size, cols x rows: half the pane it splits.
fn newPaneSize(self: *const Daemon, win: i64) [2]u32 {
    const sp = self.changes.splitOf(win);
    // a tab page's first: its floating pane's size
    if (sp.tab != 0) if (self.tabAnchor(sp) == null) return self.tabPaneSize();
    const p = self.splitFrom(sp) orelse return rex_default_size;
    const s = p.terminal_surface orelse return rex_default_size;
    const cols = @max(s.view.cols, 1);
    const rows = @max(s.view.rows, 2);
    if (sp.side_by_side) return .{ @max(cols / 2, 1), rows };
    return .{ cols, @max(rows / 2, 2) };
}

/// A guess at a new tab page's floating pane's terminal size, cols x rows.
fn tabPaneSize(self: *const Daemon) [2]u32 {
    const screen = self.grids.get(1) orelse return rex_default_size;
    const w: i64 = @intFromFloat(@as(f64, @floatFromInt(screen.width)) * tab_layer_bounds[2]);
    const h: i64 = @intFromFloat(@as(f64, @floatFromInt(screen.height)) * tab_layer_bounds[3]);
    return .{ @intCast(@max(w - layer_frame[0], 1)), @intCast(@max(h - layer_frame[1], 2)) };
}

/// The pane a new window splits: its origin's, else the focused one.
fn splitFrom(self: *const Daemon, sp: windows.Split) ?*Pane {
    if (sp.from != 0) if (self.panes.byWin(sp.from)) |p| return p;
    return self.focusedPane();
}

// Rex

fn onRex(self: *Daemon) !void {
    if (!try self.rex.read()) return self.loop.remove(self.rex.client.fd());
    while (try self.rex.next()) |update| switch (update) {
        .split_made => |made| try self.onSplitMade(made.pane, made.block),
        // a floating pane that failed leaves its floats drawn in the pane
        .split_failed, .layer_failed => |failed| if (self.panes.byId(failed.pane)) |p| {
            p.closing = .request_failed;
        },
        .layer_made => |made| try self.onLayerMade(made.pane, made.block, made.layer),
        .layout => |wins| try self.onLayout(wins),
        .focus_moved => |block| try self.onRexFocus(block),
    };
}

/// Rex made the pane's split: link the block and let Rex size it.
fn onSplitMade(self: *Daemon, pane_id: u32, block: []const u8) !void {
    // its window closed while Rex made it: nothing will draw there
    const p = self.panes.byId(pane_id) orelse return self.rex.closeBlock(block);
    p.rex_block = Pane.rexBlock(block) catch blk: {
        std.debug.print("neorex daemon: not a block id: {s}\n", .{block});
        break :blk null;
    };
    try self.rex.disown(block);
    if (p.neovim_window_handle == self.nvim.current_win) try self.rex.focus(p.rex_block);
}

/// Rex made the floating pane's layer: link it, and focus it if its float is current.
fn onLayerMade(self: *Daemon, pane_id: u32, block: []const u8, layer: ?[]const u8) !void {
    // its floats closed while Rex made it: nothing will draw there
    const p = self.panes.byId(pane_id) orelse {
        if (layer) |id| return self.rex.closeLayer(id);
        return self.rex.closeBlock(block);
    };
    p.rex_block = Pane.rexBlock(block) catch blk: {
        std.debug.print("neorex daemon: not a block id: {s}\n", .{block});
        break :blk null;
    };
    try self.rex.disown(block);
    // a tab page's window: closing its last block closes the layer
    const l = if (p.layer) |*l| l else {
        if (p.neovim_window_handle == self.nvim.current_win) try self.rex.focus(p.rex_block);
        // its tab page's other windows split it
        try self.reconcile();
        return self.draw();
    };
    if (layer) |id| l.id = Pane.rexLayer(id) catch null;
    if (self.nvim.current_float != 0 and self.layerOfWin(self.nvim.current_float) == p) try self.rex.focus(p.rex_block);
}

/// The user focused one of our panes in Rex: make its window current.
fn onRexFocus(self: *Daemon, block: []const u8) !void {
    const p = self.panes.byBlock(block) orelse return;
    if (p.layer) |*l| {
        // clicked into a floating pane: into one of its floats, if not there
        if (self.nvim.current_float != 0 and self.layerOfWin(self.nvim.current_float) == p) return;
        const win = self.focusableIn(&l.group) orelse return;
        self.nvim.current_float = win;
        _ = try windows.setCurrent(self.nvim.client, win);
        try self.nvim.client.flush();
        return;
    }
    const win = p.neovim_window_handle orelse return;
    // back from a float to the window under it counts as a move
    if (win == self.nvim.current_win and self.nvim.current_float == 0) return;
    self.nvim.current_float = 0;
    try self.makeCurrent(p);
    try self.nvim.client.flush();
    try self.draw();
}

/// A newer view: where our panes sit, so the editor and floats fit.
fn onLayout(self: *Daemon, wins: []const std.json.Value) !void {
    self.readLayout(wins);
    // a pane moved to or from the top: the tabline's over it, or not
    for (self.panes.list.items) |*p| try self.sizeWindow(p);
    if (self.focusedPane()) |f| try self.nvim.fitUiTo(f);
    try self.nvim.client.flush();
    try self.draw();
}

/// Remembers which Rex window holds our panes, and where each sits in it.
fn readLayout(self: *Daemon, wins: []const std.json.Value) void {
    for (wins) |w| {
        const layers = Client.get(w, "layers") orelse continue;
        if (layers != .array) continue;
        var found = false;
        for (layers.array.items) |layer| {
            if (!std.mem.eql(u8, Client.getStr(layer, "kind") orelse "", "tiled")) {
                self.readLayerBounds(layer);
                // a tab page's windows float over the tiled ones
                if (Client.get(layer, "layout")) |l| _ = self.readRects(l);
                continue;
            }
            if (Client.get(layer, "layout")) |l| found = self.readRects(l) or found;
        }
        if (!found) continue;
        const id = Client.getStr(w, "window_id") orelse continue;
        if (id.len == 43) self.rex.window = id[0..43].*;
    }
}

/// A floating layer of ours somewhere else: the user moved it, so its
/// floats follow.
fn readLayerBounds(self: *Daemon, layer: std.json.Value) void {
    const bounds = rect4(Client.get(layer, "bounds")) orelse return;
    const blocks = Client.get(layer, "blocks") orelse return;
    if (blocks != .array or blocks.array.items.len == 0) return;
    const p = self.panes.byBlock(Client.getStr(blocks.array.items[0], "block_id") orelse return) orelse return;
    const l = if (p.layer) |*l| l else return;
    if (self.sameBounds(bounds, l.bounds)) return;
    l.bounds = bounds;
    _ = self.followLayer(p) catch {};
}

/// Finds our panes' blocks under layout node `node` and keeps their rects.
fn readRects(self: *Daemon, node: std.json.Value) bool {
    var found = false;
    switch (node) {
        .object => |o| {
            if (Client.str(o.get("block_id"))) |b| if (self.panes.byBlock(b)) |p| if (rect4(o.get("rect"))) |r| {
                p.rect = r;
                found = true;
            };
            var it = o.iterator();
            while (it.next()) |e| found = self.readRects(e.value_ptr.*) or found;
        },
        .array => |a| for (a.items) |x| {
            found = self.readRects(x) or found;
        },
        else => {},
    }
    return found;
}

/// `[x, y, w, h]` as fractions.
fn rect4(v: ?std.json.Value) ?[4]f64 {
    const a = v orelse return null;
    if (a != .array or a.array.items.len != 4) return null;
    var out: [4]f64 = undefined;
    for (a.array.items, &out) |x, *o| o.* = switch (x) {
        .float => |f| f,
        .integer => |n| @floatFromInt(n),
        else => return null,
    };
    return out;
}

// floating panes

/// Matches floating panes to groups of floats: new groups get one, and
/// those whose group closed hang up. A floating pane's size is Rex's: it's
/// never moved to follow its floats, they follow it.
fn reconcileLayers(self: *Daemon) !void {
    var buf: [FloatGroups.max_groups]FloatGroups.Group = undefined;
    const groups = if (self.rex.conn != .closed) FloatGroups.find(&self.grids, self.nvim.enteredFloats(), &buf) else buf[0..0];
    var i = self.panes.list.items.len;
    while (i > 0) {
        i -= 1;
        const p = &self.panes.list.items[i];
        const l = if (p.layer) |*l| l else continue;
        const g = for (groups) |*g| {
            if (g.key == l.group.key) break g;
        } else {
            // without an id Rex hasn't answered yet: the answer closes it
            if (l.id) |*id| try self.rex.closeLayer(id);
            if (p.closing == null) p.closing = .window_closed;
            self.panes.remove(self.gpa, self.io, i);
            continue;
        };
        l.group = g.*;
    }
    for (groups) |g| {
        if (self.panes.byLayerKey(g.key) == null) try self.newLayer(g);
    }
}

/// Bounds for a floating pane around `g`, as fractions of the window,
/// leaving room for the frame Rex draws around it.
fn layerBounds(self: *const Daemon, g: FloatGroups.Group) ?[4]f64 {
    const screen = self.grids.get(1) orelse return null;
    if (screen.width == 0 or screen.height == 0) return null;
    const sw: f64 = @floatFromInt(screen.width);
    const sh: f64 = @floatFromInt(screen.height);
    const fw = layer_frame[0];
    const fh = layer_frame[1];
    const w = @min(@as(f64, @floatFromInt(@as(i64, g.width) + fw)) / sw, 1);
    const h = @min(@as(f64, @floatFromInt(@as(i64, g.height) + fh)) / sh, 1);
    const x = std.math.clamp(@as(f64, @floatFromInt(g.col - @divFloor(fw, 2))) / sw, 0, 1 - w);
    const y = std.math.clamp(@as(f64, @floatFromInt(g.row - @divFloor(fh, 2))) / sh, 0, 1 - h);
    return .{ x, y, w, h };
}

fn newLayer(self: *Daemon, g: FloatGroups.Group) !void {
    const bounds = self.layerBounds(g) orelse return;
    const pane = try self.panes.addForGroup(self.gpa, g);
    pane.layer.?.bounds = bounds;
    const pane_id = pane.id;

    var id_buf: [16]u8 = undefined;
    const id = try std.fmt.bufPrint(&id_buf, "{d}", .{pane_id});
    try self.rex.newLayer(pane_id, bounds, .{ .block = .{
        .flavor = "com.superlogical.terminal.shell",
        .label = "neorex",
        .options = .{ .shell = "none", .command = &[_][]const u8{ self.exe, "pane", self.sock, id } },
    } });
}

/// Within half a cell of each other on grid 1.
fn sameBounds(self: *const Daemon, a: [4]f64, b: [4]f64) bool {
    const screen = self.grids.get(1) orelse return true;
    const half = [2]f64{ 0.5 / @as(f64, @floatFromInt(@max(screen.width, 1))), 0.5 / @as(f64, @floatFromInt(@max(screen.height, 1))) };
    for (a, b, 0..) |x, y, i| if (@abs(x - y) >= half[i % 2]) return false;
    return true;
}

/// Fits a floating pane's floats to where it is and the size Rex gave it.
/// Returns the request's id, if it sent one.
fn followLayer(self: *Daemon, p: *Pane) !?u32 {
    const l = if (p.layer) |*l| l else return null;
    const s = p.terminal_surface orelse return null;
    const screen = self.grids.get(1) orelse return null;
    const row: i32 = @as(i32, @intFromFloat(@round(l.bounds[1] * @as(f64, @floatFromInt(screen.height))))) + @divFloor(layer_frame[1], 2);
    const col: i32 = @as(i32, @intFromFloat(@round(l.bounds[0] * @as(f64, @floatFromInt(screen.width))))) + @divFloor(layer_frame[0], 2);
    const g = l.group;
    if (row == g.row and col == g.col and s.view.cols == g.width and s.view.rows == g.height) return null;
    var wins: [16]i64 = undefined;
    var n: usize = 0;
    for (g.topGrids()) |top| if (self.grids.winOn(top)) |w| {
        wins[n] = w.win;
        n += 1;
    };
    if (n == 0) return null;
    return try windows.placeGroup(self.nvim.client, wins[0..n], row, col, s.view.cols, s.view.rows, .{ g.row, g.col, g.width, g.height });
}

/// The floating pane showing float `win`, if one does.
fn layerOfWin(self: *const Daemon, win: i64) ?*Pane {
    const grid = self.grids.gridOf(win) orelse return null;
    const top = (self.grids.place(grid) orelse return null).top;
    if (top == 0) return null;
    for (self.panes.list.items) |*p| if (p.layer) |*l| if (l.group.hasTop(top)) return p;
    return null;
}

/// A float in group `g` that takes focus: the input of a picker, which
/// stacks above its box.
fn focusableIn(self: *const Daemon, g: *const FloatGroups.Group) ?i64 {
    var best: ?Grids.Win = null;
    for (self.grids.wins.keys(), self.grids.wins.values()) |id, w| {
        if (w.kind != .float or !w.float.focusable) continue;
        const top = (self.grids.place(id) orelse continue).top;
        if (top == 0 or !g.hasTop(top)) continue;
        if (best == null or w.float.zindex > best.?.float.zindex) best = w;
    }
    return if (best) |w| w.win else null;
}

// shared

/// Draws every pane that changed.
fn draw(self: *Daemon) !void {
    const focused = self.focusedPane();
    // floats in floating panes stay out of the others
    var skip_buf: [FloatGroups.max_groups * 16]u32 = undefined;
    var skip_n: usize = 0;
    for (self.panes.list.items) |p| if (p.layerHides()) for (p.layer.?.group.topGrids()) |t| {
        if (skip_n == skip_buf.len) break;
        skip_buf[skip_n] = t;
        skip_n += 1;
    };
    // the screen's tabline: grid 1's top row
    const screen = self.grids.get(1);
    const tabline_dirty = if (screen) |g| g.height > 0 and (g.dirty.isSet(0) or g.scrolls_len > 0) else false;
    var i = self.panes.list.items.len;
    while (i > 0) {
        i -= 1;
        // backwards, so removing this pane leaves the ones still to draw in place
        const p = &self.panes.list.items[i];
        const s = p.terminal_surface orelse continue;
        var tabline_buf: [256]Grid.Cell = undefined;
        // a pane that closes in this loop leaves the panes drawn before it, to its right, on their old columns for this frame: the next draw finds their start moved and redraws the row
        const tabline = self.prepareTabline(p, screen, &tabline_buf);
        // a floating pane waiting on its refit shows nothing yet
        if (p.layer != null and !p.layerShown()) continue;
        const drawn = if (p.layer) |*l| blk: {
            var mbuf: [Grids.max_floats]Grids.Placed = undefined;
            const members = FloatGroups.members(&self.grids, &l.group, &mbuf);
            s.forwarder.grid = if (members.len > 0) members[0].grid else 1;
            break :blk s.view.frameFloats(&self.grids, members, .{ l.group.row, l.group.col }, &s.terminal.writer) catch false;
        } else s.view.frame(&self.grids, self.gridOfPane(p), .{
            .focused = p == focused,
            .origin = self.paneOrigin(p),
            .skip = skip_buf[0..skip_n],
            .tabline = tabline.cells,
            .tabline_col = tabline.col,
            .tabline_dirty = tabline_dirty,
        }, &s.terminal.writer) catch false;
        setTargets(&s.forwarder, s.view.drawnFloats());
        if (drawn) s.terminal.flush() catch try self.paneGone(p);
    }
    // the panes along the top drew the tabline
    if (tabline_dirty) if (screen) |g| {
        g.dirty.unset(0);
        g.scrolls_len = 0;
    };
    // drawn into one pane at most; done with them
    if (self.grids.msg_grid) |id| if (self.grids.get(id)) |m| {
        m.dirty.unsetAll();
        m.scrolls_len = 0;
    };
    for (self.grids.wins.keys(), self.grids.wins.values()) |id, w| if (w.kind == .float) if (self.grids.get(id)) |f| {
        f.dirty.unsetAll();
        f.scrolls_len = 0;
    };
}

/// Grid 1's top row from column `col`, `cols` wide, in `buf`. A wide
/// character cut at the left edge leaves a blank.
fn tablineSlice(screen: *const Grid, col: u32, cols: u32, buf: []Grid.Cell) []const Grid.Cell {
    const row = screen.row(0);
    const from = @min(col, row.len);
    const n = @min(row.len - from, cols, buf.len);
    @memcpy(buf[0..n], row[from..][0..n]);
    if (n > 0 and buf[0].len == 0) buf[0] = .{ .hl = buf[0].hl };
    return buf[0..n];
}

/// Mouse events on the floats drawn go to their grids.
fn setTargets(f: *Forwarder, rects: []const PaneView.Rect) void {
    var buf: [Forwarder.max_targets]Forwarder.Target = undefined;
    const n = @min(rects.len, buf.len);
    for (rects[0..n], buf[0..n]) |r, *t| t.* = .{ .grid = r.grid, .row = r.row, .col = r.col, .width = r.width, .height = r.height };
    f.setTargets(buf[0..n]);
}

/// Where pane `p`'s top left sits on grid 1: row, col.
fn paneOrigin(self: *const Daemon, p: *const Pane) [2]i32 {
    const r = p.rect orelse return .{ 0, 0 };
    const screen = self.grids.get(1) orelse return .{ 0, 0 };
    return .{
        @intFromFloat(@round(r[1] * @as(f64, @floatFromInt(screen.height)))),
        @intFromFloat(@round(r[0] * @as(f64, @floatFromInt(screen.width)))),
    };
}

/// Asks Neovim for the window to fit the pane, unless it already was.
fn sizeWindow(self: *Daemon, p: *Pane) !void {
    if (p.terminal_surface == null or p.awaiting_layout) return;
    const win = p.neovim_window_handle orelse return;
    const grid = self.grids.gridOf(win) orelse return;
    const size = self.windowSize(p);
    const last = p.last_asked_win_size;
    if (last.grid == grid and last.cols == size[0] and last.rows == size[1]) return;
    p.last_asked_win_size = .{ .grid = grid, .cols = size[0], .rows = size[1] };
    try windows.size(self.nvim.client, win, size[0], size[1]);
}

/// Makes the pane's window Neovim's current one, sized to the pane.
fn makeCurrent(self: *Daemon, pane: *Pane) !void {
    const win = pane.neovim_window_handle.?;
    self.nvim.current_win = win;
    self.nvim.current_win_req = try windows.setCurrent(self.nvim.client, win);
    try self.nvim.fitUiTo(pane);
}

/// The pane showing Neovim's current window; before setup, the launcher's.
fn focusedPane(self: *const Daemon) ?*Pane {
    if (self.nvim.current_win == 0) return self.panes.byId(startup_pane_id);
    return self.panes.byWin(self.nvim.current_win);
}

/// The grid showing the pane's window, if it has one yet.
fn gridOfPane(self: *const Daemon, pane: *const Pane) ?u32 {
    const win = pane.neovim_window_handle orelse return null;
    return self.grids.gridOf(win);
}

/// No terminal yet: the smallest window, as Neovim allows.
fn windowSize(self: *const Daemon, p: *const Pane) [2]u32 {
    const s = p.terminal_surface orelse return .{ 1, 1 };
    const size = s.windowSize();
    return .{ size[0], @max(size[1] -| self.tablineRowsOf(p), 1) };
}

/// The tabline's rows over pane `p`: those of a window's pane along the top
/// of the Rex window, which shows its share of the screen's tabline.
/// A pane Rex hasn't placed yet counts only while none has been: it would
/// otherwise take the top left from the panes already there, until the
/// layout arrives. Without Rex's layout at all, the one pane is the window.
fn tablineRowsOf(self: *const Daemon, p: *const Pane) u32 {
    if (self.tabline_rows == 0 or p.layer != null or p.tab != 0) return 0;
    if (p.rect == null) for (self.panes.list.items) |q| if (q.rect != null) return 0;
    return if (self.paneOrigin(p)[0] == 0) self.tabline_rows else 0;
}

/// Whether pane `p` draws part of the screen's tabline. It needs its
/// terminal: its width is what the panes right of it start after, and
/// without one it draws nothing.
fn showsTabline(self: *const Daemon, p: *const Pane) bool {
    return p.terminal_surface != null and self.tablineRowsOf(p) > 0;
}

/// The column of the screen's tabline pane `p` starts at, if it shows any.
/// The panes along the top that show it lay it out left to right from
/// column 0, whatever else is in the Rex window, so each tab shows in
/// exactly one of them. Panes at the same column go in list order.
/// A pane closing in the middle of `draw` leaves the panes drawn before it
/// on their old columns for that frame; the next draw sees their start
/// move and `PaneView` redraws the row.
fn tablineStart(self: *const Daemon, p: *const Pane) ?u32 {
    if (!self.showsTabline(p)) return null;
    const mine = self.paneOrigin(p)[1];
    var col: u32 = 0;
    var listed_before = true;
    for (self.panes.list.items) |*q| {
        if (q == p) {
            listed_before = false;
            continue;
        }
        if (!self.showsTabline(q)) continue;
        const theirs = self.paneOrigin(q)[1];
        if (theirs < mine or (theirs == mine and listed_before)) col += q.terminal_surface.?.view.cols;
    }
    return col;
}

/// What pane `p` draws of the screen's tabline: its slice of `screen`'s top
/// row, in `buf`, and the column it starts at. Also points the pane's
/// mouse events on that row at the same cells.
fn prepareTabline(self: *const Daemon, p: *const Pane, screen: ?*const Grid, buf: []Grid.Cell) struct { cells: ?[]const Grid.Cell, col: u32 } {
    const s = p.terminal_surface orelse return .{ .cells = null, .col = 0 };
    const found = self.tablineStart(p);
    const col = found orelse 0;
    const cells = if (found != null) if (screen) |g| tablineSlice(g, col, s.view.cols, buf) else null else null;
    s.forwarder.tabline_rows = if (cells != null) 1 else 0;
    s.forwarder.tabline_col = @intCast(@min(col, std.math.maxInt(u16)));
    return .{ .cells = cells, .col = col };
}

/// The tabline came or went: the panes under it fit their windows again.
fn onTabline(self: *Daemon, rows: u32) !void {
    if (rows == self.tabline_rows) return;
    self.tabline_rows = rows;
    for (self.panes.list.items) |*p| try self.sizeWindow(p);
    try self.nvim.client.flush();
    try self.draw();
}

/// A daemon with a 200 x 50 screen and the tabline on, and panes to place.
/// Only its panes and grids are set up: the rest of it is undefined.
const TablineFixture = struct {
    d: Daemon,
    surfaces: [4]Surface = undefined,

    fn init(self: *TablineFixture) !void {
        self.d = .{ .gpa = std.testing.allocator, .io = undefined, .loop = undefined, .exe = "", .sock = "", .rex = undefined, .nvim = undefined, .tabline_rows = 1 };
        try self.d.grids.grids.put(std.testing.allocator, 1, .{});
        try self.d.grids.get(1).?.resize(std.testing.allocator, 200, 50);
    }

    fn deinit(self: *TablineFixture) void {
        self.d.panes.list.deinit(std.testing.allocator);
        self.d.grids.deinit(std.testing.allocator);
    }

    /// A window's pane in surface `slot` of `cols` columns, at `rect` in the
    /// Rex window; null for one Rex hasn't placed yet.
    fn add(self: *TablineFixture, slot: usize, cols: u32, rect: ?[4]f64) !*Pane {
        self.surfaces[slot] = .{ .helper_socket = undefined, .terminal = undefined, .view = .init(cols, 20) };
        const p = try self.d.panes.add(std.testing.allocator, Pane.fromNeovimWindow(@intCast(slot + 1), @intCast(1000 + slot)));
        p.terminal_surface = &self.surfaces[slot];
        p.rect = rect;
        return p;
    }
};

test "with a shell to the left, the first Neovim pane starts the tabline and the next continues it" {
    const t = std.testing;
    var fx: TablineFixture = undefined;
    try fx.init();
    defer fx.deinit();
    // a shell holds x = 0 to 0.4; the Neovim panes sit right of it
    _ = try fx.add(0, 60, .{ 0.4, 0, 0.3, 1 });
    _ = try fx.add(1, 60, .{ 0.7, 0, 0.3, 1 });
    const left = &fx.d.panes.list.items[0];
    const right = &fx.d.panes.list.items[1];
    try t.expectEqual(@as(?u32, 0), fx.d.tablineStart(left));
    try t.expectEqual(@as(?u32, 60), fx.d.tablineStart(right));

    // every tab shows once: the slices, joined, are the tabline's cells
    const screen = fx.d.grids.get(1).?;
    for (screen.cells[0..200], 0..) |*c, i| c.* = .init(&.{@intCast('a' + i % 26)}, 0);
    var lbuf: [256]Grid.Cell = undefined;
    var rbuf: [256]Grid.Cell = undefined;
    const ls = tablineSlice(screen, fx.d.tablineStart(left).?, 60, &lbuf);
    const rs = tablineSlice(screen, fx.d.tablineStart(right).?, 60, &rbuf);
    try t.expectEqual(@as(usize, 60), ls.len);
    try t.expectEqual(@as(usize, 60), rs.len);
    for (ls, screen.row(0)[0..60]) |a, b| try t.expectEqualStrings(b.str(), a.str());
    for (rs, screen.row(0)[60..120]) |a, b| try t.expectEqualStrings(b.str(), a.str());

    // what draw sets up: the slice, and the forwarder's column for clicks
    var fbuf: [256]Grid.Cell = undefined;
    const prepared = fx.d.prepareTabline(right, screen, &fbuf);
    try t.expectEqual(@as(u32, 60), prepared.col);
    try t.expectEqualStrings(screen.row(0)[60].str(), prepared.cells.?[0].str());
    try t.expectEqual(@as(u16, 60), right.terminal_surface.?.forwarder.tabline_col);
    try t.expectEqual(@as(u16, 1), right.terminal_surface.?.forwarder.tabline_rows);
    const first = fx.d.prepareTabline(left, screen, &fbuf);
    try t.expectEqual(@as(u32, 0), first.col);
    try t.expectEqual(@as(u16, 0), left.terminal_surface.?.forwarder.tabline_col);

    // the panes swapped places in the list, not in the window
    std.mem.swap(Pane, &fx.d.panes.list.items[0], &fx.d.panes.list.items[1]);
    try t.expectEqual(@as(?u32, 0), fx.d.tablineStart(&fx.d.panes.list.items[1]));
    try t.expectEqual(@as(?u32, 60), fx.d.tablineStart(&fx.d.panes.list.items[0]));
}

test "a pane that closes moves the ones to its right left, and only top panes show the tabline" {
    const t = std.testing;
    var fx: TablineFixture = undefined;
    try fx.init();
    defer fx.deinit();
    _ = try fx.add(0, 50, .{ 0, 0, 0.25, 0.5 });
    _ = try fx.add(1, 70, .{ 0.25, 0, 0.35, 0.5 });
    _ = try fx.add(2, 80, .{ 0.6, 0, 0.4, 0.5 });
    // under them, and on another tab page: neither shows it, or takes room
    const under = try fx.add(3, 90, .{ 0, 0.5, 1, 0.5 });
    try t.expectEqual(@as(?u32, null), fx.d.tablineStart(under));
    try t.expectEqual(@as(?u32, 120), fx.d.tablineStart(&fx.d.panes.list.items[2]));
    fx.d.panes.list.items[1].tab = 2;
    try t.expectEqual(@as(?u32, null), fx.d.tablineStart(&fx.d.panes.list.items[1]));
    try t.expectEqual(@as(?u32, 50), fx.d.tablineStart(&fx.d.panes.list.items[2]));
    // the first closes, as in the middle of `draw`
    _ = fx.d.panes.list.orderedRemove(0);
    try t.expectEqual(@as(?u32, 0), fx.d.tablineStart(&fx.d.panes.list.items[1]));
    // no tabline: no pane shows any
    fx.d.tabline_rows = 0;
    try t.expectEqual(@as(?u32, null), fx.d.tablineStart(&fx.d.panes.list.items[1]));
}

test "a pane Rex hasn't placed yet doesn't take the top left, unless none is placed" {
    const t = std.testing;
    var fx: TablineFixture = undefined;
    try fx.init();
    defer fx.deinit();
    // a new split, before the layout names its place
    _ = try fx.add(0, 40, null);
    // no layout yet at all: it's the window
    try t.expectEqual(@as(?u32, 0), fx.d.tablineStart(&fx.d.panes.list.items[0]));
    _ = try fx.add(1, 60, .{ 0.4, 0, 0.6, 1 });
    try t.expectEqual(@as(?u32, null), fx.d.tablineStart(&fx.d.panes.list.items[0]));
    // and the placed pane starts the tabline, not after it
    try t.expectEqual(@as(?u32, 0), fx.d.tablineStart(&fx.d.panes.list.items[1]));
}

/// The JSON of each client frame in `bytes`, as a Client wrote them: masked
/// text frames with 7-bit or 16-bit lengths.
fn clientFrames(gpa: Allocator, bytes: []const u8) !std.ArrayList([]const u8) {
    var frames: std.ArrayList([]const u8) = .empty;
    var i: usize = 0;
    while (i < bytes.len) {
        var len: usize = bytes[i + 1] & 0x7f;
        i += 2;
        if (len == 126) {
            len = std.mem.readInt(u16, bytes[i..][0..2], .big);
            i += 2;
        }
        const mask = bytes[i..][0..4];
        const text = try gpa.dupe(u8, bytes[i + 4 ..][0..len]);
        for (text, 0..) |*b, k| b.* ^= mask[k % 4];
        try frames.append(gpa, text);
        i += 4 + len;
    }
    return frames;
}

/// What `onDirection` asks Rex for with `move`: the requests Rex reads.
fn directionRequests(gpa: Allocator, move: windows.Move) !std.ArrayList([]const u8) {
    var fds: [2]std.c.fd_t = undefined;
    if (std.c.socketpair(std.c.AF.UNIX, std.c.SOCK.STREAM, 0, &fds) != 0) return error.SocketPair;
    const ours: std.Io.net.Stream = .{ .socket = .{ .handle = fds[0], .address = undefined } };
    const rex_end: std.Io.net.Stream = .{ .socket = .{ .handle = fds[1], .address = undefined } };
    defer rex_end.close(std.testing.io);
    var wbuf: [512]u8 = undefined;
    var client: Client = .{ .io = std.testing.io, .stream = ours, .writer = ours.writer(std.testing.io, &wbuf), .open = true };
    defer client.out.deinit(gpa);
    var d: Daemon = .{ .gpa = gpa, .io = std.testing.io, .loop = undefined, .exe = "", .sock = "", .rex = .{ .gpa = gpa, .client = &client, .session_id = "session:s", .conn = .attached }, .nvim = undefined };
    defer d.rex.deinit();
    try d.onDirection(move);
    ours.close(std.testing.io); // the end of what it sent

    var all: std.ArrayList(u8) = .empty;
    defer all.deinit(gpa);
    var buf: [4096]u8 = undefined;
    while (true) {
        const n = std.c.read(rex_end.socket.handle, &buf, buf.len);
        if (n <= 0) break;
        try all.appendSlice(gpa, buf[0..@intCast(n)]);
    }
    return clientFrames(gpa, all.items);
}

fn freeFrames(gpa: Allocator, frames: *std.ArrayList([]const u8)) void {
    for (frames.items) |f| gpa.free(f);
    frames.deinit(gpa);
}

test "a directional move asks Rex to focus that way without wrapping, once per count" {
    const gpa = std.testing.allocator;
    var down = try directionRequests(gpa, .{ .dir = .down, .count = 3 });
    defer freeFrames(gpa, &down);
    try std.testing.expectEqual(@as(usize, 3), down.items.len);
    try std.testing.expectEqualStrings(
        \\{"type":"request","id":"1","method":"session.focus_direction","payload":{"session_id":"session:s","direction":"down","wrap":false}}
    , down.items[0]);
    try std.testing.expectEqualStrings(
        \\{"type":"request","id":"3","method":"session.focus_direction","payload":{"session_id":"session:s","direction":"down","wrap":false}}
    , down.items[2]);

    // Neovim's hjkl are Rex's left, down, up and right
    for ([_]struct { windows.Direction, []const u8 }{ .{ .left, "left" }, .{ .up, "up" }, .{ .right, "right" } }) |c| {
        var one = try directionRequests(gpa, .{ .dir = c[0], .count = 1 });
        defer freeFrames(gpa, &one);
        try std.testing.expectEqual(@as(usize, 1), one.items.len);
        const want = try std.fmt.allocPrint(gpa, "\"direction\":\"{s}\"", .{c[1]});
        defer gpa.free(want);
        try std.testing.expect(std.mem.indexOf(u8, one.items[0], want) != null);
    }

    // a stray huge count doesn't flood Rex
    var many = try directionRequests(gpa, .{ .dir = .left, .count = 9999 });
    defer freeFrames(gpa, &many);
    try std.testing.expectEqual(@as(usize, 20), many.items.len);
}
