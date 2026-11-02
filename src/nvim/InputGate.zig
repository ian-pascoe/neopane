//! Keeps typed input behind the window switches before it. Neovim applies
//! `nvim_input` the moment it's read, but runs `nvim_set_current_win` on its
//! next turn of the loop, so keys sent right after a switch would reach the
//! window that was current before it. Only one switch is ever in flight: a
//! switch asked for while another is, and any input, wait their turn, so
//! nothing is dropped or reordered.

const std = @import("std");
const Allocator = std.mem.Allocator;
const Forwarder = @import("Forwarder.zig");
const Nvim = @import("Nvim.zig");
const rpc = @import("rpc.zig");
const windows = @import("windows.zig");

const InputGate = @This();

/// The switch in flight: asked for, not yet answered.
pending: ?u32 = null,
/// What waits for it, in the order it came. Never nonempty without `pending`.
queue: std.ArrayList(Item) = .empty,
/// Switches asked for and not answered yet: the one in flight and those queued.
outstanding: u32 = 0,

const Item = union(enum) {
    switch_to: i64,
    /// Input for one target.
    input: struct { target: u32, bytes: std.ArrayList(u8) },
};

pub fn deinit(self: *InputGate, gpa: Allocator) void {
    for (self.queue.items) |*item| switch (item.*) {
        .input => |*in| in.bytes.deinit(gpa),
        .switch_to => {},
    };
    self.queue.deinit(gpa);
    self.* = undefined;
}

/// Whether Neovim has switches to make that we asked for: what it says about
/// its current window until then is out of date.
pub fn switching(self: *const InputGate) bool {
    return self.outstanding > 0;
}

/// Asks Neovim to make `win` current once the switches and input before it
/// are through, and flushes.
pub fn switchTo(self: *InputGate, gpa: Allocator, nvim: *Nvim, win: i64) !void {
    if (self.pending != null) {
        try self.queue.append(gpa, .{ .switch_to = win });
    } else {
        self.pending = try windows.setCurrent(nvim, win);
        try nvim.flush();
    }
    self.outstanding += 1;
}

/// Sends `bytes` through `fwd`, the forwarder of `target`, once the switches
/// asked for so far are made. `target` names the forwarder to `answered`'s
/// `panes`.
pub fn send(self: *InputGate, gpa: Allocator, nvim: *Nvim, target: u32, fwd: *Forwarder, bytes: []const u8) !void {
    if (self.pending == null) {
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

/// Neovim answered request `req`: if it was the switch in flight, goes on
/// with what waited for it. `panes.forwarderOf(target)` finds the forwarder
/// of a target, or null once it's gone, which drops its input.
pub fn answered(self: *InputGate, gpa: Allocator, nvim: *Nvim, req: u32, panes: anytype) !void {
    if (self.pending != req) return;
    self.pending = null;
    self.outstanding -= 1;
    while (self.pending == null and self.queue.items.len > 0) switch (self.queue.orderedRemove(0)) {
        .switch_to => |win| {
            self.pending = try windows.setCurrent(nvim, win);
            try nvim.flush();
        },
        .input => |in| {
            var bytes = in.bytes;
            defer bytes.deinit(gpa);
            const fwd = panes.forwarderOf(in.target) orelse continue;
            _ = try fwd.feed(nvim, bytes.items);
        },
    };
}

const testing = std.testing;

/// Neovim, and what the tests below say to it.
const Rig = struct {
    nvim: Nvim,
    gate: InputGate = .{},
    /// The forwarders of targets 0 and 1.
    fwds: [2]Forwarder = .{ .{ .ctrl_q_quits = false, .send_focus = false }, .{ .ctrl_q_quits = false, .send_focus = false } },
    /// Two windows, each showing its own buffer.
    wins: [2]i64 = .{ 0, 0 },

    fn forwarderOf(r: *Rig, target: u32) ?*Forwarder {
        return if (target < r.fwds.len) &r.fwds[target] else null;
    }

    fn deinit(r: *Rig) void {
        r.gate.deinit(testing.allocator);
        r.nvim.deinit(testing.allocator);
    }

    /// Reads until `id` is answered, giving every answer to the gate.
    /// Returns the answer as an owned copy of its string, if it is one.
    fn wait(r: *Rig, id: u32) !?[]u8 {
        const gpa = testing.allocator;
        for (0..200) |_| {
            var fds = [_]std.c.pollfd{.{ .fd = r.nvim.readFd(), .events = std.c.POLL.IN, .revents = 0 }};
            if (std.c.poll(&fds, 1, 3000) <= 0) return error.TestTimedOut;
            if (!try r.nvim.read(gpa)) return error.NvimExited;
            var found: ?[]u8 = null;
            var done = false;
            while (try r.nvim.next()) |msg| switch (msg) {
                .response => |res| {
                    if (res.err) |e| {
                        std.debug.print("neovim: {s}\n", .{e});
                        return error.NvimError;
                    }
                    try r.gate.answered(gpa, &r.nvim, res.id, r);
                    if (res.id == id) {
                        var result = res.result;
                        if (rpc.expectStr(&result)) |s| found = try gpa.dupe(u8, s) else |_| {}
                        done = true;
                    }
                },
                .request => |req| try r.nvim.respondError(req.id, "test"),
                .notification => {},
            };
            if (done) return found;
        }
        return error.TestTimedOut;
    }

    /// Runs Lua and returns the string it returns.
    fn lua(r: *Rig, code: []const u8) ![]u8 {
        const id = try r.nvim.execLua(code, 0);
        try r.nvim.flush();
        return (try r.wait(id)) orelse error.NotAString;
    }

    /// Neovim with two side-by-side windows, `one` in the first and `two` in
    /// the second, which is current.
    fn start() !Rig {
        var buf: [1024]u8 = undefined;
        var r: Rig = .{ .nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &buf) catch |err| switch (err) {
            error.FileNotFound => return error.SkipZigTest,
            else => return err,
        } };
        errdefer r.deinit();
        try r.nvim.uiAttach(40, 10, .{});
        const ids = try r.lua(
            \\vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'one' })
            \\local first = vim.api.nvim_get_current_win()
            \\vim.cmd('vsplit | enew')
            \\vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'two' })
            \\return first .. ',' .. vim.api.nvim_get_current_win()
        );
        defer testing.allocator.free(ids);
        var it = std.mem.splitScalar(u8, ids, ',');
        for (&r.wins) |*w| w.* = try std.fmt.parseInt(i64, it.next().?, 10);
        return r;
    }

    /// The text of the windows' buffers, as `first|second`.
    fn texts(r: *Rig) ![]u8 {
        // the keys are sent when the last switch is answered
        while (r.gate.pending) |id| _ = try r.wait(id);
        var code: [320]u8 = undefined;
        return r.lua(try std.fmt.bufPrint(&code,
            \\local t = {{}}
            \\for _, w in ipairs({{ {d}, {d} }}) do
            \\  table.insert(t, table.concat(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(w), 0, -1, false), '/'))
            \\end
            \\return table.concat(t, '|')
        , .{ r.wins[0], r.wins[1] }));
    }
};

test "keys typed right after a window switch edit that window" {
    var r = try Rig.start();
    defer r.deinit();
    try r.gate.switchTo(testing.allocator, &r.nvim, r.wins[0]);
    try r.gate.send(testing.allocator, &r.nvim, 0, &r.fwds[0], "x");
    const got = try r.texts();
    defer testing.allocator.free(got);
    // the first window lost a character, the second didn't
    try testing.expectEqualStrings("ne|two", got);
}

test "held keys keep their order, across switches and chunks" {
    var r = try Rig.start();
    defer r.deinit();
    const gpa = testing.allocator;
    // typed into the first window in three pieces, then the second, then the first again
    try r.gate.switchTo(testing.allocator, &r.nvim, r.wins[0]);
    try r.gate.send(gpa, &r.nvim, 0, &r.fwds[0], "A");
    try r.gate.send(gpa, &r.nvim, 0, &r.fwds[0], "x");
    try r.gate.send(gpa, &r.nvim, 0, &r.fwds[0], "y");
    try r.gate.switchTo(testing.allocator, &r.nvim, r.wins[1]);
    try r.gate.send(gpa, &r.nvim, 1, &r.fwds[1], "\x1bA");
    try r.gate.send(gpa, &r.nvim, 1, &r.fwds[1], "z");
    try r.gate.switchTo(testing.allocator, &r.nvim, r.wins[0]);
    try r.gate.send(gpa, &r.nvim, 0, &r.fwds[0], "z");
    const got = try r.texts();
    defer gpa.free(got);
    try testing.expectEqualStrings("onexyz|twoz", got);
}

test "input for a target that's gone is dropped, the rest still goes" {
    var r = try Rig.start();
    defer r.deinit();
    const gpa = testing.allocator;
    try r.gate.switchTo(testing.allocator, &r.nvim, r.wins[0]);
    try r.gate.send(gpa, &r.nvim, 7, &r.fwds[0], "x");
    try r.gate.send(gpa, &r.nvim, 0, &r.fwds[0], "x");
    const got = try r.texts();
    defer gpa.free(got);
    try testing.expectEqualStrings("ne|two", got);
}
