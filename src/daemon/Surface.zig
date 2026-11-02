//! One pane's terminal. Always heap-allocated and used through *Surface,
//! since `terminal.writer` points into `out_buf`.

const std = @import("std");
const net = std.Io.net;

const Terminal = @import("../term/Terminal.zig");
const modes = @import("../term/modes.zig");
const Forwarder = @import("../nvim/Forwarder.zig");
const PaneView = @import("../screen/PaneView.zig");

const Surface = @This();

/// Closing it makes the helper exit, and Rex close the pane.
helper_socket: net.Stream,
terminal: Terminal,
view: PaneView = .init(0, 0),
forwarder: Forwarder = .{
    .ctrl_q_quits = false,
    .send_focus = false,
},

out_buf: [32 * 1024]u8 = undefined,

pub fn create(gpa: std.mem.Allocator, helper_socket: net.Stream, tty: std.posix.fd_t) !*Surface {
    const self = try gpa.create(Surface);
    errdefer gpa.destroy(self);
    self.* = .{
        .helper_socket = helper_socket,
        .terminal = try Terminal.init(tty, &self.out_buf),
    };
    errdefer self.terminal.deinit();

    try self.terminal.writer.writeAll(modes.editor_on);
    const ws = try self.terminal.size();
    self.view = .init(ws.col, ws.row);
    return self;
}

/// `restore` is false when the terminal is already gone. A `message` is
/// left on the restored terminal: how a daemon that gives up tells the pane's
/// user why.
pub fn destroy(self: *Surface, gpa: std.mem.Allocator, io: std.Io, restore: bool, message: ?[]const u8) void {
    if (restore) {
        self.terminal.writer.writeAll(modes.editor_off) catch {};
        self.terminal.deinit();
        if (message) |m| _ = std.c.write(self.terminal.fd, m.ptr, m.len);
    }
    _ = std.c.close(self.terminal.fd);
    self.helper_socket.close(io);
    gpa.destroy(self);
}

/// The window that fits the pane, less the `msg_rows` at the bottom kept for
/// Neovim's messages and command line ('cmdheight'). Never empty.
pub fn windowSize(self: *const Surface, msg_rows: u32) [2]u32 {
    return windowFitting(self.view.cols, self.view.rows, msg_rows);
}

/// `windowSize` for a pane of `cols` x `rows`, one that may not exist yet.
/// The only place the rule is.
pub fn windowFitting(cols: u32, rows: u32, msg_rows: u32) [2]u32 {
    return .{ @max(cols, 1), @max(rows -| msg_rows, 1) };
}

test "the window is the pane less the message rows, and never empty" {
    // windowSize only reads `view`
    var surface: Surface = .{ .helper_socket = undefined, .terminal = undefined, .view = .init(80, 24) };
    // cmdheight 0 reserves none: the window fills the pane
    try std.testing.expectEqual([2]u32{ 80, 24 }, surface.windowSize(0));
    try std.testing.expectEqual([2]u32{ 80, 23 }, surface.windowSize(1));
    try std.testing.expectEqual([2]u32{ 80, 22 }, surface.windowSize(2));
    try std.testing.expectEqual([2]u32{ 80, 1 }, surface.windowSize(40));

    surface.view = .init(0, 0);
    try std.testing.expectEqual([2]u32{ 1, 1 }, surface.windowSize(0));
    try std.testing.expectEqual([2]u32{ 1, 1 }, surface.windowSize(1));

    surface.view = .init(10, 1);
    try std.testing.expectEqual([2]u32{ 10, 1 }, surface.windowSize(1));
    try std.testing.expectEqual([2]u32{ 10, 1 }, surface.windowSize(0));
}
