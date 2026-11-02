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
/// The switch the timer is for, see `watchSwitch`.
watched_switch: ?u32 = null,
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

/// Makes `win`, the window of pane `pane`, Neovim's current window: the
/// pane has focus. Input sent from here on goes to it, once Neovim has
/// switched.
pub fn makeCurrent(self: *NvimSession, gpa: std.mem.Allocator, pane: u32, win: i64) !void {
    self.current_win = win;
    try self.gate.switchTo(gpa, self.client, win, pane);
}

/// Makes `win`, a float in floating pane `pane`, current. Doesn't touch
/// `current_win`, which floats don't change. Input waits as for `makeCurrent`.
pub fn switchToFloat(self: *NvimSession, gpa: std.mem.Allocator, pane: u32, win: i64) !void {
    try self.gate.switchTo(gpa, self.client, win, pane);
}

/// Whether `bytes`, read from pane `pane`, whose window is `win`, make it
/// current, and Rex should focus it: the pane has no focus (`rex_focused`
/// says Rex has given it) and what was typed or clicked into it takes
/// focus. Scrolling it, or moving the pointer over it, doesn't: those reach
/// its window through `typeInto` all the same, by its grid, without a
/// switch. `panes` is as for `typeInto`.
pub fn takesFocus(self: *const NvimSession, panes: anytype, pane: u32, win: ?i64, rex_focused: bool, bytes: []const u8) bool {
    const w = win orelse return false;
    if (w == self.current_win or rex_focused) return false;
    const fwd = panes.aimForwarder(pane) orelse return false;
    return fwd.takesFocus(bytes);
}

/// Sends `bytes`, typed into pane `pane`, to Neovim after the window
/// switches before it. `panes.aimForwarder(pane)` finds the pane's `Forwarder`.
pub fn typeInto(self: *NvimSession, gpa: std.mem.Allocator, panes: anytype, pane: u32, bytes: []const u8) !void {
    try self.gate.send(gpa, self.client, panes, pane, bytes);
}

/// Neovim answered a request: ours may let held input go on.
pub fn answered(self: *NvimSession, gpa: std.mem.Allocator, panes: anytype, res: rpc.Message.Response) !void {
    try self.gate.answered(gpa, self.client, panes, res);
}

/// The window switch Neovim hasn't answered yet, if any. Until it does,
/// what Neovim says about its current window is out of date.
pub fn switchInFlight(self: *const NvimSession) ?u32 {
    return self.gate.inFlight();
}

/// Whether to start the timer for the switch in flight: it's a new one.
/// Then `recheckSwitch` is due after a while.
pub fn watchSwitch(self: *NvimSession) bool {
    const id = self.gate.inFlight() orelse return false;
    if (self.watched_switch == id) return false;
    self.watched_switch = id;
    return true;
}

/// The timer fired. A switch still unanswered may be stuck behind a command
/// waiting for a key, so asks Neovim whether it is, which a busy one won't
/// answer. A newer switch than the one watched only starts its wait. True to
/// start the timer again.
pub fn recheckSwitch(self: *NvimSession) !bool {
    const id = self.gate.inFlight() orelse return false;
    if (self.watched_switch != id) {
        self.watched_switch = id;
        return true;
    }
    try self.gate.probeAgain(self.client);
    return true;
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

    pub fn aimForwarder(r: *Rig, id: u32) ?*Forwarder {
        return if (id < r.fwds.len) &r.fwds[id] else null;
    }

    /// Starts in place: the session points at the Neovim.
    fn start(r: *Rig) !void {
        r.nvim = Nvim.spawn(testing.io, &.{ "nvim", "--clean", "--embed", "-n", "--cmd", "set noswapfile" }, &r.out_buf) catch |err| switch (err) {
            error.FileNotFound => return error.SkipZigTest,
            else => return err,
        };
        errdefer r.nvim.deinit(gpa);
        r.session = .{ .client = &r.nvim };
        r.fwds = .{ .{ .ctrl_q_quits = false, .send_focus = false }, .{ .ctrl_q_quits = false, .send_focus = false } };
        try r.nvim.uiAttach(40, 10, .{});
        const ids = try r.lua(
            \\vim.bo.swapfile = false -- killed with changes, Neovim leaves swap files behind
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

    /// Reads what Neovim has sent, giving every answer to the session.
    /// Returns the reply to `id`, if it's among them.
    fn step(r: *Rig, id: ?u32) !?Reply {
        var fds = [_]std.c.pollfd{.{ .fd = r.nvim.readFd(), .events = std.c.POLL.IN, .revents = 0 }};
        if (std.c.poll(&fds, 1, 3000) <= 0) return error.TestTimedOut;
        if (!try r.nvim.read(gpa)) return error.NvimExited;
        var reply: ?Reply = null;
        while (try r.nvim.next()) |msg| switch (msg) {
            .response => |res| {
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
        return reply;
    }

    /// Reads until `id` is answered.
    fn wait(r: *Rig, id: u32) !Reply {
        for (0..200) |_| if (try r.step(id)) |reply| return reply;
        return error.TestTimedOut;
    }

    /// The timer, as the Daemon has it, firing for the switch in flight.
    fn tick(r: *Rig) !void {
        _ = r.session.watchSwitch();
        _ = try r.session.recheckSwitch();
    }

    /// Reads until the probe asked, if any, is answered.
    fn probeAnswered(r: *Rig) !void {
        if (r.session.gate.probe) |id| _ = try r.wait(id);
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

    /// A tick's worth of time, for Neovim to read what it was sent.
    fn pause(_: *Rig) !void {
        try std.Io.sleep(testing.io, .fromMilliseconds(100), .awake);
    }

    /// Reads until Neovim is waiting for a key.
    fn untilBlocking(r: *Rig) !void {
        for (0..500) |_| {
            if (try r.blocking()) return;
            std.Io.sleep(testing.io, .fromMilliseconds(5), .awake) catch {};
        }
        return error.NeverBlocked;
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

    /// Makes Neovim busy for `ms`, without waiting: it reads nothing until
    /// it's done.
    fn busy(r: *Rig, ms: u32) !void {
        var code: [64]u8 = undefined;
        _ = try r.nvim.execLua(try std.fmt.bufPrint(&code, "vim.uv.sleep({d})", .{ms}), 0);
        try r.nvim.flush();
    }

    /// Fills the first window's buffer with 100 lines.
    fn fillFirst(r: *Rig) !void {
        var code: [256]u8 = undefined;
        gpa.free(try r.lua(try std.fmt.bufPrint(&code,
            \\local b = vim.api.nvim_win_get_buf({d})
            \\local l = {{}}
            \\for i = 1, 100 do l[i] = 'line ' .. i end
            \\vim.api.nvim_buf_set_lines(b, 0, -1, false, l)
            \\return 'ok'
        , .{r.wins[0]})));
    }

    /// The first line `win` shows, after all that was typed.
    fn topline(r: *Rig, win: i64) !i64 {
        try r.settle();
        var code: [96]u8 = undefined;
        const top = try r.lua(try std.fmt.bufPrint(&code, "return tostring(vim.fn.getwininfo({d})[1].topline)", .{win}));
        defer gpa.free(top);
        return std.fmt.parseInt(i64, top, 10);
    }

    /// Input read from pane `pane`, as the Daemon takes it: the pane is
    /// made current if the input takes focus. Whether it did.
    fn input(r: *Rig, pane: u32, bytes: []const u8) !bool {
        const focus = r.session.takesFocus(r, pane, r.wins[pane], false, bytes);
        if (focus) try r.session.makeCurrent(gpa, pane, r.wins[pane]);
        try r.session.typeInto(gpa, r, pane, bytes);
        return focus;
    }

    /// The window Neovim has as current.
    fn currentWin(r: *Rig) !i64 {
        const cur = try r.lua("return tostring(vim.api.nvim_get_current_win())");
        defer gpa.free(cur);
        return std.fmt.parseInt(i64, cur, 10);
    }
};

test "keys typed after Rex moves focus to a pane go to its window" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.expectTexts("ne|two");
    try testing.expectEqual(r.wins[0], r.session.current_win);
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "held keys keep their order, across switches and reads" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // the first window in three reads, then the second, then the first again
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "A");
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, "y");
    try r.session.makeCurrent(Rig.gpa, 1, r.wins[1]);
    // Escape and the key after it are two reads, not Alt
    try r.session.typeInto(Rig.gpa, &r, 1, "\x1b");
    try r.session.typeInto(Rig.gpa, &r, 1, "A");
    try r.session.typeInto(Rig.gpa, &r, 1, "z");
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "z");
    try r.expectTexts("onexyz|twoz");
}

test "input for a pane that's gone is dropped, the rest still goes" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.session.typeInto(Rig.gpa, &r, 7, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.expectTexts("e|two");
}

test "a command waiting for a key gets one, then the switch happens" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // `f` waits for a character, and Neovim answers no switch until it has one
    try r.session.typeInto(Rig.gpa, &r, 1, "f");
    try r.untilBlocking();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    // nothing asks the moment the switch is sent, which a key sent just before may not have been read for
    try testing.expectEqual(@as(?u32, null), r.session.gate.probe);
    try r.tick();
    try r.probeAnswered();
    try r.expectTexts("ne|two");
    // the first `x` finished the `f` in the window it was in; the second
    // went to the window switched to
    try r.expectNothingHeld();
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "a key typed after a later switch can finish the command that stops it" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.typeInto(Rig.gpa, &r, 1, "f");
    try r.untilBlocking();
    // two switches, and the only key typed is after the second
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.makeCurrent(Rig.gpa, 1, r.wins[1]);
    try r.session.typeInto(Rig.gpa, &r, 1, "x");
    try r.session.typeInto(Rig.gpa, &r, 1, "x");
    try r.tick();
    try r.probeAnswered();
    try r.expectTexts("one|wo");
    try r.expectNothingHeld();
    try testing.expectEqual(r.wins[1], try r.currentWin());
}

test "a busy Neovim keeps the keys held, however often it's checked" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.busy(300);
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    // the timer fires, twice, before Neovim answers anything
    try r.tick();
    const probe = r.session.gate.probe.?;
    try testing.expect(try r.session.recheckSwitch());
    try testing.expectEqual(@as(?u32, probe), r.session.gate.probe);
    try testing.expectEqual(@as(usize, 1), r.session.gate.queue.items.len);
    try r.expectTexts("ne|two");
}

test "a command that needs two keys gets them, one tick apart" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // Ctrl-W waits for a character, and then `g` for another
    try r.session.typeInto(Rig.gpa, &r, 1, "\x17");
    try r.untilBlocking();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "g");
    try r.session.typeInto(Rig.gpa, &r, 0, "z");
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.tick();
    try r.probeAnswered();
    // `g` went. A key just went through: Neovim may not have read it when
    // it's asked, so the next tick doesn't ask, and the one after does
    try r.pause();
    try testing.expect(try r.session.recheckSwitch());
    try testing.expectEqual(@as(?u32, null), r.session.gate.probe);
    try testing.expect(try r.session.recheckSwitch());
    try testing.expect(r.session.gate.probe != null);
    try r.probeAnswered();
    // `z` finished it, and `x` waited for the switch
    try r.expectTexts("ne|two");
    try r.expectNothingHeld();
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "an Escape after the key that's let through isn't lost, nor joined to the next key" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.typeInto(Rig.gpa, &r, 1, "f");
    try r.untilBlocking();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    // two Escapes in one read; the first ends the `f`
    try r.session.typeInto(Rig.gpa, &r, 0, "\x1b\x1b");
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.tick();
    try r.probeAnswered();
    // the second Escape went with the rest, so `x` isn't Alt-x
    try r.expectTexts("ne|two");
    try r.expectNothingHeld();
}

test "the timer watches each switch once, and does nothing for one that's done" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try testing.expect(!r.session.watchSwitch());
    try testing.expect(!try r.session.recheckSwitch());

    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try testing.expect(r.session.watchSwitch());
    try testing.expect(!r.session.watchSwitch());
    // a switch queued behind it isn't the one in flight
    try r.session.makeCurrent(Rig.gpa, 1, r.wins[1]);
    try testing.expect(!r.session.watchSwitch());
    try testing.expect(try r.session.recheckSwitch());

    // the first answered, the second in flight: its wait starts over
    try r.session.typeInto(Rig.gpa, &r, 1, "x");
    const first = r.session.switchInFlight().?;
    _ = try r.wait(first);
    const second = r.session.switchInFlight().?;
    try testing.expect(first != second);
    // a stale timer: it only starts the new switch's wait
    try testing.expect(try r.session.recheckSwitch());
    try testing.expectEqual(@as(?u32, second), r.session.watched_switch);
    try r.settle();
    // nothing in flight: the timer stops
    try testing.expect(!try r.session.recheckSwitch());
    try r.expectTexts("one|wo");
}

test "a switch that fails drops its pane's keys, not another pane's, and focus is kept" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    // a window that isn't there: it closed before the switch ran
    try r.session.makeCurrent(Rig.gpa, 0, 9999);
    // a focus report, with a key: the key's dropped, the report is read
    try r.session.typeInto(Rig.gpa, &r, 0, "\x1b[Ix");
    // another pane's key, typed meanwhile, still goes
    try r.session.typeInto(Rig.gpa, &r, 1, "x");
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 0, "x");
    try r.expectTexts("ne|wo");
    try r.expectNothingHeld();
    try testing.expectEqual(@as(?bool, true), r.fwds[0].focused);
}

test "dropping input that ends a paste ends it in Neovim" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    const set = try r.lua(
        \\_G.phases = {}
        \\vim.paste = function(_, phase)
        \\  table.insert(_G.phases, phase)
        \\  return true
        \\end
        \\return 'ok'
    );
    Rig.gpa.free(set);
    // a paste begun, then a switch that fails, and the paste's end behind it
    try r.session.typeInto(Rig.gpa, &r, 0, "\x1b[200~ab");
    try r.session.makeCurrent(Rig.gpa, 0, 9999);
    try r.session.typeInto(Rig.gpa, &r, 0, "\x1b[201~");
    try r.settle();
    const phases = try r.lua("return table.concat(_G.phases, ',')");
    defer Rig.gpa.free(phases);
    try testing.expectEqualStrings("1,3", phases);
}

test "focus reports don't wait for a switch, unless the parser is partway through one" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    try r.session.typeInto(Rig.gpa, &r, 1, "\x1b[O");
    try testing.expectEqual(@as(?bool, false), r.fwds[1].focused);
    try testing.expectEqual(@as(usize, 0), r.session.gate.queue.items.len);

    // partway through a sequence, a report would be read as part of it
    r.fwds[0].parser.state = .csi;
    try r.session.typeInto(Rig.gpa, &r, 0, "\x1b[I");
    try testing.expectEqual(@as(usize, 1), r.session.gate.queue.items.len);
    try testing.expectEqual(@as(?bool, null), r.fwds[0].focused);
    r.fwds[0].parser.state = .ground;
    try r.settle();
    try testing.expectEqual(@as(?bool, true), r.fwds[0].focused);
}

// SGR mouse reports at column 31, row 2 (1-based), where the first window is
const wheel_down_first = "\x1b[<65;31;2M";
const wheel_up_first = "\x1b[<64;31;2M";
const click_first = "\x1b[<0;31;2M";

test "a wheel event in an unfocused pane scrolls its window and moves no focus" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.fillFirst();
    try testing.expect(!try r.input(0, wheel_down_first));
    try testing.expectEqual(@as(i64, 4), try r.topline(r.wins[0]));
    try r.expectNothingHeld();
    try testing.expectEqual(r.wins[1], r.session.current_win);
    try testing.expectEqual(r.wins[1], try r.currentWin());
}

test "a click in an unfocused pane makes it current" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.fillFirst();
    try testing.expect(try r.input(0, click_first));
    try r.settle();
    try testing.expectEqual(r.wins[0], r.session.current_win);
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "what is typed into an unfocused pane still makes it current" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try testing.expect(try r.input(0, "x"));
    try r.expectTexts("ne|two");
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "only input that takes focus, to a pane that has none, makes it current" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    const wheel = "\x1b[<65;31;2M";
    try testing.expect(r.session.takesFocus(&r, 0, r.wins[0], false, "x"));
    try testing.expect(!r.session.takesFocus(&r, 0, r.wins[0], false, wheel));
    // motion with no button, a release, and the focus reports Rex sends as it moves focus away
    try testing.expect(!r.session.takesFocus(&r, 0, r.wins[0], false, "\x1b[<35;31;2M"));
    try testing.expect(!r.session.takesFocus(&r, 0, r.wins[0], false, "\x1b[<0;31;2m"));
    try testing.expect(!r.session.takesFocus(&r, 0, r.wins[0], false, "\x1b[O"));
    // Rex has the pane focused, or Neovim's current window is its window, or it has none
    try testing.expect(!r.session.takesFocus(&r, 0, r.wins[0], true, "x"));
    try testing.expect(!r.session.takesFocus(&r, 1, r.wins[1], false, "x"));
    try testing.expect(!r.session.takesFocus(&r, 0, null, false, "x"));
    // a pane that's gone
    try testing.expect(!r.session.takesFocus(&r, 7, r.wins[0], false, "x"));
}

test "a wheel event waits its turn behind a switch and the keys before it, and starts none" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.fillFirst();
    // `G` in the first window, then wheel up over it: in the other order the
    // view would end at the bottom, line 93
    try testing.expect(try r.input(0, "G"));
    try testing.expect(!try r.input(0, wheel_up_first));
    try testing.expectEqual(@as(usize, 2), r.session.gate.queue.items.len);
    try r.settle();
    try r.expectNothingHeld();
    try testing.expectEqual(@as(i64, 90), try r.topline(r.wins[0]));
    try testing.expectEqual(r.wins[0], try r.currentWin());
}

test "a wheel event in a pane behind the one being switched to starts no switch" {
    var r: Rig = undefined;
    try r.start();
    defer r.deinit();
    try r.fillFirst();
    // the second window is current; Rex moves focus to the first, and the wheel is over the second
    try r.session.makeCurrent(Rig.gpa, 0, r.wins[0]);
    // over pane 1's window at column 5, row 2
    try testing.expect(!try r.input(1, "\x1b[<65;5;2M"));
    try testing.expectEqual(@as(usize, 1), r.session.gate.queue.items.len);
    try r.settle();
    try testing.expectEqual(r.wins[0], r.session.current_win);
    try testing.expectEqual(r.wins[0], try r.currentWin());
}
