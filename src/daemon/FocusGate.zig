//! Keeps typed input behind the window switches before it. Neovim applies
//! `nvim_input` the moment it's read, but runs `nvim_set_current_win` on its
//! next turn of the loop, so keys sent right after a switch would reach the
//! window that was current before it. Only one switch is ever in flight: a
//! switch asked for while another is, and the input typed after it, wait
//! their turn. Input reaches Neovim in the order it was typed, each key
//! after the switches typed before it, with two exceptions:
//!
//! - Focus reports never wait: they only tell us what the terminal has.
//! - Neovim can't make a switch while it waits for a key to finish a
//!   command, such as after `f`, or in `getchar()`; holding every key would
//!   wait for a key that never comes. So when a probe finds Neovim waiting,
//!   the first key held goes through, wherever it was typed, to finish the
//!   command. The rest stay held, and the switch follows.
//!
//! A switch that fails, such as for a window that closed first, drops the
//! input typed into its pane behind it.
//!
//! A failed write leaves the switch in flight: Neovim's connection is gone.

const std = @import("std");
const Allocator = std.mem.Allocator;
const input = @import("../term/input.zig");
const Nvim = @import("../nvim/Nvim.zig");
const msgpack = @import("../nvim/msgpack.zig");
const rpc = @import("../nvim/rpc.zig");
const windows = @import("../nvim/windows.zig");

const FocusGate = @This();

/// The switch in flight: asked for, not yet answered, and its pane.
pending: ?u32 = null,
pending_pane: u32 = 0,
/// Our `nvim_get_mode` asked beside it, which Neovim answers when it's
/// waiting for a key, but not when it's busy.
probe: ?u32 = null,
/// The last probe found Neovim waiting for a key, and no key has gone since.
blocked: bool = false,
/// What waits for the switch in flight, in the order it came. Never
/// nonempty without `pending`.
queue: std.ArrayList(Item) = .empty,

const Item = union(enum) {
    switch_to: struct { win: i64, pane: u32 },
    /// What one read of a pane's terminal gave, and how much has gone.
    input: struct { pane: u32, bytes: []u8, read: usize = 0 },
};

pub fn deinit(self: *FocusGate, gpa: Allocator) void {
    for (self.queue.items) |item| if (item == .input) gpa.free(item.input.bytes);
    self.queue.deinit(gpa);
    self.* = undefined;
}

/// The switch in flight, if any.
pub fn inFlight(self: *const FocusGate) ?u32 {
    return self.pending;
}

/// Asks Neovim to make `win`, the window of `pane`, current once the
/// switches and input before it are through.
pub fn switchTo(self: *FocusGate, gpa: Allocator, nvim: *Nvim, win: i64, pane: u32) !void {
    if (self.pending != null) return self.queue.append(gpa, .{ .switch_to = .{ .win = win, .pane = pane } });
    try self.start(nvim, win, pane);
}

/// Sends `bytes`, typed into `pane`, through the forwarder
/// `panes.forwarderToFeed(pane)` finds, once the switches asked for so far
/// are made. Dropped if it finds none.
pub fn send(self: *FocusGate, gpa: Allocator, nvim: *Nvim, pane: u32, panes: anytype, bytes: []const u8) !void {
    if (self.pending == null or input.onlyFocusReports(bytes)) {
        const fwd = panes.forwarderToFeed(pane) orelse return;
        _ = try fwd.feed(nvim, bytes);
        return;
    }
    // one item per read: the parser takes the end of a read for the end of
    // a key, as for a lone Escape
    const copy = try gpa.dupe(u8, bytes);
    errdefer gpa.free(copy);
    try self.queue.append(gpa, .{ .input = .{ .pane = pane, .bytes = copy } });
    if (self.blocked) try self.release(gpa, nvim, panes);
}

/// Neovim answered a request: if it was ours, goes on with what waited for
/// it. `panes` is as for `send`.
pub fn answered(self: *FocusGate, gpa: Allocator, nvim: *Nvim, res: rpc.Message.Response, panes: anytype) !void {
    if (self.probe == res.id) {
        self.probe = null;
        var result = res.result;
        if (res.err == null and (isBlocking(&result) catch false)) {
            self.blocked = true;
            try self.release(gpa, nvim, panes);
        }
        return;
    }
    if (self.pending != res.id) return;
    const failed = res.err != null;
    const pane = self.pending_pane;
    self.probe = null;
    self.blocked = false;
    // `pending` stays until the queue is through, so a failed write leaves
    // input held, never ahead of what's queued
    while (self.queue.items.len > 0) switch (self.queue.orderedRemove(0)) {
        .switch_to => |s| return self.start(nvim, s.win, s.pane),
        .input => |in| {
            defer gpa.free(in.bytes);
            const fwd = panes.forwarderToFeed(in.pane) orelse continue;
            const rest = in.bytes[in.read..];
            if (failed and in.pane == pane) fwd.discard(rest) else _ = try fwd.feed(nvim, rest);
        },
    };
    self.pending = null;
}

/// Asks again whether Neovim is waiting for a key, if the switch in flight
/// is unanswered and no ask is: a busy Neovim answers none until it's done.
pub fn probeAgain(self: *FocusGate, nvim: *Nvim) !void {
    if (self.pending == null or self.probe != null or self.blocked) return;
    self.probe = try nvim.request("nvim_get_mode", 0);
    try nvim.flush();
}

/// Asks for `win` as the switch in flight.
fn start(self: *FocusGate, nvim: *Nvim, win: i64, pane: u32) !void {
    self.pending = try windows.setCurrent(nvim, win);
    self.pending_pane = pane;
    self.probe = try nvim.request("nvim_get_mode", 0);
    self.blocked = false;
    try nvim.flush();
}

/// Neovim waits for a key: sends the first key held, which it needs to get
/// on. If none is held the next one sent goes at once.
fn release(self: *FocusGate, gpa: Allocator, nvim: *Nvim, panes: anytype) !void {
    var i: usize = 0;
    while (i < self.queue.items.len) {
        if (self.queue.items[i] != .input) {
            i += 1;
            continue;
        }
        const in = &self.queue.items[i].input;
        const fwd = panes.forwarderToFeed(in.pane) orelse {
            gpa.free(in.bytes);
            _ = self.queue.orderedRemove(i);
            continue;
        };
        const fed = try fwd.feedKey(nvim, in.bytes[in.read..]);
        in.read += fed.read;
        if (in.read >= in.bytes.len) {
            gpa.free(in.bytes);
            _ = self.queue.orderedRemove(i);
        }
        if (fed.sent_key) {
            self.blocked = false;
            return;
        }
    }
}

/// Whether the `nvim_get_mode` result at `result` says Neovim is waiting for a key.
pub fn isBlocking(result: *msgpack.Decoder) !bool {
    var pairs = try rpc.expectMap(result);
    while (pairs > 0) : (pairs -= 1) {
        const key = try rpc.expectStr(result);
        if (std.mem.eql(u8, key, "blocking")) return rpc.expectBool(result);
        try rpc.skip(result);
    }
    return false;
}
