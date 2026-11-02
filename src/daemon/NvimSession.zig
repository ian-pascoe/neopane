//! Our side of the Neovim connection: setup requests, the current window,
//! and what Neovim was last told about focus.

const std = @import("std");
const Nvim = @import("../nvim/Nvim.zig");
const Forwarder = @import("../nvim/Forwarder.zig");
const windows = @import("../nvim/windows.zig");
const rpc = @import("../nvim/rpc.zig");
const FocusGate = @import("FocusGate.zig");
const Pane = @import("Pane.zig");

const NvimSession = @This();

client: *Nvim,
/// Neovim's current window, as the Lua last said; 0 before setup.
current_win: i64 = 0,
/// Our window switches, and the input typed after them.
gate: FocusGate = .{},
channel_req: u32 = 0,
setup_req: u32 = 0,
/// Setup answered: windows can be adopted and given panes.
ready: bool = false,
/// Whether some pane's terminal has focus, as Neovim was last told.
ui_focused: bool = true,
/// Neovim's current window when it's a float, else 0.
current_float: i64 = 0,
/// The editor size last asked for.
editor_size: [2]u32 = .{ 0, 0 },
/// Float windows that have been current, newest last; the oldest drop
/// off. Neovim never reuses a handle, so closed ones do no harm.
entered_floats: [32]i64 = undefined,
entered_len: u8 = 0,

/// Float `win` became current: its group may go native.
pub fn noteEntered(self: *NvimSession, win: i64) void {
    if (std.mem.indexOfScalar(i64, self.enteredFloats(), win) != null) return;
    if (self.entered_len == self.entered_floats.len) {
        std.mem.copyForwards(i64, self.entered_floats[0 .. self.entered_len - 1], self.entered_floats[1..self.entered_len]);
        self.entered_len -= 1;
    }
    self.entered_floats[self.entered_len] = win;
    self.entered_len += 1;
}

pub fn enteredFloats(self: *const NvimSession) []const i64 {
    return self.entered_floats[0..self.entered_len];
}

pub fn deinit(self: *NvimSession, gpa: std.mem.Allocator) void {
    self.gate.deinit(gpa);
}

/// Makes `win` Neovim's current window: its pane has focus. Input sent
/// from here on, and until Neovim has switched, goes to it.
pub fn makeCurrent(self: *NvimSession, gpa: std.mem.Allocator, win: i64) !void {
    self.current_win = win;
    try self.switchTo(gpa, win);
}

/// Makes `win`, a float, current. Input waits for it as for `makeCurrent`.
pub fn switchTo(self: *NvimSession, gpa: std.mem.Allocator, win: i64) !void {
    try self.gate.switchTo(gpa, self.client, win);
}

/// Sends `bytes`, typed into pane `target`, to Neovim after the window
/// switches before it, first making `switch_to` current if it's not null.
/// `panes.forwarderToFeed(target)` finds the pane's `Forwarder`.
pub fn typeInto(self: *NvimSession, gpa: std.mem.Allocator, panes: anytype, target: u32, switch_to: ?i64, bytes: []const u8) !void {
    if (switch_to) |win| try self.makeCurrent(gpa, win);
    try self.gate.send(gpa, self.client, target, panes, bytes);
}

/// Neovim answered a request: ours may let held input go on.
pub fn answered(self: *NvimSession, gpa: std.mem.Allocator, panes: anytype, res: rpc.Message.Response) !void {
    try self.gate.answered(gpa, self.client, res, panes);
}

/// Our window switches haven't all happened in Neovim: what it says about
/// its current window until then is out of date.
pub fn switching(self: *const NvimSession) bool {
    return self.gate.inFlight() != null;
}

/// The window switch Neovim hasn't answered yet, if any.
pub fn switchInFlight(self: *const NvimSession) ?u32 {
    return self.gate.inFlight();
}

/// The switch in flight is stuck: lets the input behind it through.
pub fn giveUpSwitch(self: *NvimSession, gpa: std.mem.Allocator, panes: anytype) !void {
    try self.gate.giveUp(gpa, self.client, panes);
}

/// Attaches as Neovim's UI at `cols` x `rows` and asks for our channel.
pub fn attachUi(self: *NvimSession, cols: u32, rows: u32) !void {
    try self.client.uiAttach(cols, rows, .{ .ext_termcolors = true, .ext_multigrid = true });
    self.channel_req = try windows.askChannel(self.client);
    try self.client.flush();
}

/// The editor spans the Rex window, so editor floats such as pickers can
/// too. Messages show in the focused pane. Not before setup: the first
/// window still fills the editor, and growing it to the Rex window then
/// shrinking it to its pane leaves it scrolled, as a dashboard drawn for
/// the bigger size would be.
pub fn fitUiTo(self: *NvimSession, p: *const Pane) !void {
    if (!self.ready) return;
    const size = editorSize(p) orelse return;
    if (std.mem.eql(u32, &size, &self.editor_size)) return;
    self.editor_size = size;
    try self.client.uiTryResize(size[0], size[1]);
}

/// The Rex window's size in cells, judged from pane `p`'s share of it;
/// without that, the pane's own size.
fn editorSize(p: *const Pane) ?[2]u32 {
    const s = p.terminal_surface orelse return null;
    const cols = s.view.cols;
    const rows = s.view.rows;
    const r = p.rect orelse return .{ cols, rows };
    if (r[2] <= 0 or r[3] <= 0) return .{ cols, rows };
    return .{
        @max(cols, @as(u32, @intFromFloat(@round(@as(f64, @floatFromInt(cols)) / r[2])))),
        @max(rows, @as(u32, @intFromFloat(@round(@as(f64, @floatFromInt(rows)) / r[3])))),
    };
}

test "entered floats are kept once, and the oldest drop off" {
    var s: NvimSession = .{ .client = undefined };
    s.noteEntered(1000);
    s.noteEntered(1000);
    try std.testing.expectEqualSlices(i64, &.{1000}, s.enteredFloats());
    for (0..s.entered_floats.len) |i| s.noteEntered(2000 + @as(i64, @intCast(i)));
    try std.testing.expectEqual(s.entered_floats.len, s.enteredFloats().len);
    try std.testing.expectEqual(@as(i64, 2000), s.enteredFloats()[0]);
    try std.testing.expectEqual(@as(i64, 2031), s.enteredFloats()[31]);
}

const testing = std.testing;

/// Neovim with two side-by-side windows, and a session on it as the daemon
/// has: typing and focus changes go through its methods. Its two forwarders
/// are the panes' with ids 0, for the first window, and 1.
const Rig = struct {
    nvim: Nvim,
    session: NvimSession,
    fwds: [2]Forwarder,
    /// Each showing its own buffer: `one` and `two`. The second is current.
    wins: [2]i64,
    out_buf: [1024]u8,

    const gpa = testing.allocator;

    pub fn forwarderToFeed(r: *Rig, id: u32) ?*Forwarder {
        return if (id < r.fwds.len) &r.fwds[id] else null;
    }

    /// Starts in place: the session points at the Neovim.
    fn start(r: *Rig) !void {
        r.nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n" }, &r.out_buf) catch |err| switch (err) {
            error.FileNotFound => return error.SkipZigTest,
            else => return err,
        };
        errdefer r.nvim.deinit(gpa);
        r.session = .{ .client = &r.nvim };
        r.fwds = .{ .{ .ctrl_q_quits = false, .send_focus = false }, .{ .ctrl_q_quits = false, .send_focus = false } };
        try r.nvim.uiAttach(40, 10, .{});
        const ids = try r.lua(
            \\vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'one' })
            \\local first = vim.api.nvim_get_current_win()
            \\vim.cmd('vsplit | enew')
            \\vim.api.nvim_buf_set_lines(0, 0, -1, false, { 'two' })
            \\return first .. ',' .. vim.api.nvim_get_current_win()
        );
        defer gpa.free(ids);
        var it = std.mem.splitScalar(u8, ids, ',');
        for (&r.wins) |*w| w.* = try std.fmt.parseInt(i64, it.next().?, 10);
        r.session.current_win = r.wins[1];
    }

    fn deinit(r: *Rig) void {
        r.session.deinit(gpa);
        r.nvim.deinit(gpa);
    }

    const Reply = struct {
        /// An owned copy of a string result.
        text: ?[]u8 = null,
        blocking: bool = false,
    };

    /// Reads until `id` is answered, giving every answer to the session.
    fn wait(r: *Rig, id: u32) !Reply {
        for (0..200) |_| {
            var fds = [_]std.c.pollfd{.{ .fd = r.nvim.readFd(), .events = std.c.POLL.IN, .revents = 0 }};
            if (std.c.poll(&fds, 1, 3000) <= 0) return error.TestTimedOut;
            if (!try r.nvim.read(gpa)) return error.NvimExited;
            var reply: ?Reply = null;
            while (try r.nvim.next()) |msg| switch (msg) {
                .response => |res| {
                    // a failed switch is the point of a test, any other failure isn't
                    try r.session.answered(gpa, r, res);
                    if (res.id != id) continue;
                    var str = res.result;
                    var map = res.result;
                    reply = .{
                        .text = if (rpc.expectStr(&str)) |s| try gpa.dupe(u8, s) else |_| null,
                        .blocking = FocusGate.isBlocking(&map) catch false,
                    };
                },
                .request => |req| try r.nvim.respondError(req.id, "test"),
                .notification => {},
            };
            if (reply) |found| return found;
        }
        return error.TestTimedOut;
    }

    /// Runs Lua and returns the string it returns.
    fn lua(r: *Rig, code: []const u8) ![]u8 {
        const id = try r.nvim.execLua(code, 0);
        try r.nvim.flush();
        return (try r.wait(id)).text orelse error.NotAString;
    }

    /// Reads until every switch is answered, and what waited for them sent.
    fn settle(r: *Rig) !void {
        while (r.session.switchInFlight()) |id| gpa.free((try r.wait(id)).text orelse continue);
    }

    /// Whether Neovim is waiting for a key to finish a command. Answered
    /// at once even then, which `lua` isn't.
    fn blocking(r: *Rig) !bool {
        const id = try r.nvim.request("nvim_get_mode", 0);
        try r.nvim.flush();
        return (try r.wait(id)).blocking;
    }

    /// The text of the windows' buffers, as `first|second`, after all that was typed.
    fn texts(r: *Rig) ![]u8 {
        try r.settle();
        var code: [320]u8 = undefined;
        return r.lua(try std.fmt.bufPrint(&code,
            \\local t = {{}}
            \\for _, w in ipairs({{ {d}, {d} }}) do
            \\  table.insert(t, table.concat(vim.api.nvim_buf_get_lines(vim.api.nvim_win_get_buf(w), 0, -1, false), '/'))
            \\end
            \\return table.concat(t, '|')
        , .{ r.wins[0], r.wins[1] }));
    }

    fn expectTexts(r: *Rig, want: []const u8) !void {
        const got = try r.texts();
        defer gpa.free(got);
        try testing.expectEqualStrings(want, got);
    }

    fn expectNothingHeld(r: *Rig) !void {
        try testing.expectEqual(@as(usize, 0), r.session.gate.queue.items.len);
        try testing.expectEqual(@as(?u32, null), r.session.switchInFlight());
    }
};

test "keys typed after Rex moves focus to a pane go to its window" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.makeCurrent(Rig.gpa, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, null, "x");
    try r.expectTexts("ne|two");
    try testing.expectEqual(r.wins[0], r.session.current_win);
}

test "keys typed into a pane that isn't current edit its window" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "x");
    try r.expectTexts("ne|two");
    try testing.expectEqual(r.wins[0], r.session.current_win);
}

test "held keys keep their order, across switches and chunks" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // the first window in three pieces, then the second, then the first again
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "A");
    try r.session.typeInto(Rig.gpa, &r, 0, null, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, null, "y");
    try r.session.typeInto(Rig.gpa, &r, 1, r.wins[1], "\x1bA");
    try r.session.typeInto(Rig.gpa, &r, 1, null, "z");
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "z");
    try r.expectTexts("onexyz|twoz");
}

test "input for a pane that's gone is dropped, the rest still goes" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "x");
    try r.session.typeInto(Rig.gpa, &r, 7, null, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, null, "x");
    try r.expectTexts("e|two");
}

test "a command waiting for a key doesn't hold input for good" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // `f` waits for a character, and Neovim answers no switch until it has one
    try r.session.typeInto(Rig.gpa, &r, 1, null, "f");
    while (!try r.blocking()) {}
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "x");
    try r.session.typeInto(Rig.gpa, &r, 0, null, "x");
    try r.settle();
    try r.expectNothingHeld();
    // both `x` went to the old window, where the `f` was: the first finished
    // it and the second, sent before the switch could run, deleted
    try r.expectTexts("one|wo");
    const cur = try r.lua("return tostring(vim.api.nvim_get_current_win())");
    defer Rig.gpa.free(cur);
    try testing.expectEqual(r.wins[0], try std.fmt.parseInt(i64, cur, 10));
}

test "input held behind a stuck switch goes after giving up on it" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "x");
    try testing.expect(r.session.gate.queue.items.len == 1);
    try r.session.giveUpSwitch(Rig.gpa, &r);
    try testing.expect(r.session.gate.queue.items.len == 0);
    try r.settle();
    try r.expectNothingHeld();
}

test "a switch that fails drops the keys typed for it, and the rest still goes" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // a window that isn't there: it closed before the switch ran
    try r.session.typeInto(Rig.gpa, &r, 0, 9999, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, null, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, r.wins[0], "x");
    try r.expectTexts("ne|two");
    try r.expectNothingHeld();
}
