//! Keeps typed input behind the window switches before it. Neovim applies
//! `nvim_input` the moment it's read, but runs `nvim_set_current_win` on its
//! next turn of the loop, so keys sent right after a switch would reach the
//! window that was current before it. Only one switch is ever in flight: a
//! switch asked for while another is, and any input, wait their turn, so
//! nothing is dropped or reordered.
//!
//! Neovim can't make a switch while it waits for a key to finish a command,
//! such as after `f`, or in `getchar()`. Holding input then would wait for
//! keys that never come, so input is let through instead. It goes to the old
//! window, where the command is, until Neovim has the key and can switch.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Nvim = @import("../nvim/Nvim.zig");
const msgpack = @import("../nvim/msgpack.zig");
const rpc = @import("../nvim/rpc.zig");
const windows = @import("../nvim/windows.zig");

const FocusGate = @This();

/// The switch in flight: asked for, not yet answered.
pending: ?u32 = null,
/// Our `nvim_get_mode` sent beside it, which Neovim answers even when it
/// can't switch yet.
probe: ?u32 = null,
/// Neovim is waiting for a key, so the switch in flight waits too.
blocked: bool = false,
/// What waits for the switch in flight, in the order it came. Never
/// nonempty without `pending`.
queue: std.ArrayList(Item) = .empty,

const Item = union(enum) {
    switch_to: i64,
    /// Input for one target.
    input: struct { target: u32, bytes: std.ArrayList(u8) },
};

pub fn deinit(self: *FocusGate, gpa: Allocator) void {
    for (self.queue.items) |*item| switch (item.*) {
        .input => |*in| in.bytes.deinit(gpa),
        .switch_to => {},
    };
    self.queue.deinit(gpa);
    self.* = undefined;
}

/// Asks Neovim to make `win` current once the switches and input before it
/// are through.
pub fn switchTo(self: *FocusGate, gpa: Allocator, nvim: *Nvim, win: i64) !void {
    if (self.pending != null) return self.queue.append(gpa, .{ .switch_to = win });
    try self.start(nvim, win);
}

/// Sends `bytes` through the forwarder `panes.forwarderToFeed(target)` finds,
/// once the switches asked for so far are made. Dropped if it finds none.
pub fn send(self: *FocusGate, gpa: Allocator, nvim: *Nvim, target: u32, panes: anytype, bytes: []const u8) !void {
    const fwd = panes.forwarderToFeed(target) orelse return;
    if (self.pending == null or self.blocked) {
        _ = try fwd.feed(nvim, bytes);
        return;
    }
    if (self.queue.items.len > 0) switch (self.queue.items[self.queue.items.len - 1]) {
        .input => |*last| if (last.target == target) return last.bytes.appendSlice(gpa, bytes),
        .switch_to => {},
    };
    var copy: std.ArrayList(u8) = .empty;
    errdefer copy.deinit(gpa);
    try copy.appendSlice(gpa, bytes);
    try self.queue.append(gpa, .{ .input = .{ .target = target, .bytes = copy } });
}

/// The switch in flight, if any. What Neovim says about its current window
/// until it's answered is out of date.
pub fn inFlight(self: *const FocusGate) ?u32 {
    return self.pending;
}

/// Neovim answered a request: if it was ours, goes on with what waited for
/// it. A switch that failed, such as for a window that closed meanwhile,
/// drops the input typed for it. `panes` is as for `send`.
pub fn answered(self: *FocusGate, gpa: Allocator, nvim: *Nvim, res: rpc.Message.Response, panes: anytype) !void {
    if (self.probe == res.id) {
        self.probe = null;
        var result = res.result;
        if (res.err == null and (isBlocking(&result) catch false)) try self.letThrough(gpa, nvim, panes);
        return;
    }
    if (self.pending != res.id) return;
    self.pending = null;
    self.probe = null;
    self.blocked = false;
    // answers come in order, so what waits for this switch is first
    while (self.queue.items.len > 0) switch (self.queue.orderedRemove(0)) {
        .switch_to => |win| return self.start(nvim, win),
        .input => |in| {
            var bytes = in.bytes;
            defer bytes.deinit(gpa);
            if (res.err != null) continue;
            const fwd = panes.forwarderToFeed(in.target) orelse continue;
            _ = try fwd.feed(nvim, bytes.items);
        },
    };
}

/// The switch in flight has gone unanswered too long: sends what waits for
/// it anyway, as when Neovim is known to be waiting for a key.
pub fn giveUp(self: *FocusGate, gpa: Allocator, nvim: *Nvim, panes: anytype) !void {
    if (self.pending == null or self.blocked) return;
    try self.letThrough(gpa, nvim, panes);
}

/// Asks for `win` as the switch in flight.
fn start(self: *FocusGate, nvim: *Nvim, win: i64) !void {
    self.pending = try windows.setCurrent(nvim, win);
    self.probe = try nvim.request("nvim_get_mode", 0);
    self.blocked = false;
    try nvim.flush();
}

/// Stops holding input behind the switch in flight, which Neovim can't make
/// yet: sends what is held, and what comes. The switches still queued stay.
fn letThrough(self: *FocusGate, gpa: Allocator, nvim: *Nvim, panes: anytype) !void {
    self.blocked = true;
    var kept: usize = 0;
    for (self.queue.items) |*item| switch (item.*) {
        .switch_to => {
            self.queue.items[kept] = item.*;
            kept += 1;
        },
        .input => |*in| {
            // left empty, so an error below leaves nothing to free twice
            var bytes = in.bytes;
            in.bytes = .empty;
            defer bytes.deinit(gpa);
            const fwd = panes.forwarderToFeed(in.target) orelse continue;
            _ = try fwd.feed(nvim, bytes.items);
        },
    };
    self.queue.shrinkRetainingCapacity(kept);
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
