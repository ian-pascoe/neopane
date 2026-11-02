//! `neorex daemon BASE [nvim args]`: the background half of `neorex`. Takes
//! the launching pane's terminal, runs Neovim with `--listen`, and shows
//! each of its windows in a Rex pane (see `Daemon`). Exits when Neovim does
//! or the last pane goes away.

const std = @import("std");
const net = std.Io.net;
const Nvim = @import("../nvim/Nvim.zig");
const Client = @import("../rex/Client.zig");
const Loop = @import("Loop.zig");
const Daemon = @import("Daemon.zig");
const Pane = @import("Pane.zig");
const Surface = @import("Surface.zig");
const pane = @import("pane_helper.zig");

/// Longest path a Unix socket takes on macOS, less one for the terminator.
pub const max_path = 103;

/// `$TMPDIR/neorex-PID`, which every daemon file is named from.
pub fn basePath(buf: []u8) ![]const u8 {
    const tmp = if (std.c.getenv("TMPDIR")) |t| std.mem.span(t) else "/tmp";
    const dir = std.mem.trimEnd(u8, tmp, "/");
    const base = try std.fmt.bufPrint(buf, "{s}/neorex-{d}", .{ dir, std.c.getpid() });
    // room for the longest suffix
    if (base.len + ".nvim.sock".len > max_path) return error.NameTooLong;
    return base;
}

/// `base` plus `suffix`, such as `.sock`.
pub fn path(buf: []u8, base: []const u8, suffix: []const u8) ![]const u8 {
    return std.fmt.bufPrint(buf, "{s}{s}", .{ base, suffix });
}

/// What `serve` runs against. `run` uses the defaults; tests swap the seams.
pub const Config = struct {
    rex_url: []const u8,
    session_id: []const u8,
    /// Starts Neovim.
    nvim_exe: []const u8 = "nvim",
    /// Connects to Neovim's socket, waiting for it to appear.
    connect: *const fn (io: std.Io, path: []const u8, buffer: []u8) anyerror!Nvim = connectNvim,
    /// Called once the helpers' socket listens: the launcher may connect.
    ready: *const fn (ctx: ?*anyopaque) anyerror!void = tellLauncher,
    ctx: ?*anyopaque = null,
};

fn connectNvim(io: std.Io, nvim_sock: []const u8, buffer: []u8) anyerror!Nvim {
    return Nvim.connectRetry(io, nvim_sock, buffer);
}

pub fn run(gpa: std.mem.Allocator, io: std.Io, base: []const u8, nvim_args: []const []const u8) !void {
    // out of the pane's session, so closing the pane doesn't signal us
    _ = std.c.setsid();
    return serve(gpa, io, base, nvim_args, .{
        .rex_url = env("REX_SERVER") orelse return error.NotInRex,
        .session_id = env("REX_SESSION") orelse return error.NotInRex,
    });
}

/// One byte on stdout says we're listening; then stdout goes nowhere.
fn tellLauncher(_: ?*anyopaque) !void {
    if (std.c.write(std.posix.STDOUT_FILENO, "r", 1) != 1) return error.LauncherGone;
    const devnull = std.c.open("/dev/null", .{ .ACCMODE = .RDWR });
    if (devnull >= 0) {
        _ = std.c.dup2(devnull, std.posix.STDOUT_FILENO);
        _ = std.c.close(devnull);
    }
}

pub fn serve(gpa: std.mem.Allocator, io: std.Io, base: []const u8, nvim_args: []const []const u8, config: Config) !void {
    var sock_buf: [max_path + 1]u8 = undefined;
    var nvim_sock_buf: [max_path + 1]u8 = undefined;
    var log_buf: [max_path + 1]u8 = undefined;
    const sock = try path(&sock_buf, base, ".sock");
    const nvim_sock = try path(&nvim_sock_buf, base, ".nvim.sock");
    const log = try path(&log_buf, base, ".log");
    const rex_url = config.rex_url;
    const session_id = config.session_id;
    var exe_buf: [1024]u8 = undefined;
    const exe = exe_buf[0..try std.process.executablePath(io, &exe_buf)];

    const addr = try net.UnixAddress.init(sock);
    std.Io.Dir.cwd().deleteFile(io, sock) catch {};
    // open until we exit: every new pane's helper connects here
    var server = try addr.listen(io, .{});
    defer server.deinit(io);
    defer std.Io.Dir.cwd().deleteFile(io, sock) catch {};
    // last of all, after Neovim has had its chance to remove its own: a
    // Neovim that was stuck or killed leaves it behind
    defer std.Io.Dir.cwd().deleteFile(io, nvim_sock) catch {};

    try config.ready(config.ctx);

    const rex_block: ?Pane.RexBlock = if (env("REX_BLOCK")) |b| Pane.rexBlock(b) catch blk: {
        std.debug.print("neorex daemon: REX_BLOCK isn't a block id: {s}\n", .{b});
        break :blk null;
    } else null;
    var launch = Pane.startup(rex_block, try takePane(gpa, io, &server));
    var launch_owned = true;
    defer if (launch_owned) launch.deinit(gpa, io);

    var argv: std.ArrayList([]const u8) = .empty;
    defer argv.deinit(gpa);
    // before the user's config, so it can tell it runs under neorex
    try argv.appendSlice(gpa, &.{ config.nvim_exe, "--headless", "--listen", nvim_sock, "--cmd", "let g:neorex = 1" });
    try argv.appendSlice(gpa, nvim_args);
    // its stderr is ours: the log
    var child = std.process.spawn(io, .{ .argv = argv.items, .stdin = .ignore, .stdout = .ignore }) catch |err|
        return giveUp(gpa, io, &launch, &launch_owned, log, .start_nvim, err);
    // after the terminals are restored: SIGHUP makes Neovim save swap files and exit
    defer {
        _ = std.c.kill(child.id.?, .HUP);
        _ = child.wait(io) catch {};
    }

    var nvim_buf: [16 * 1024]u8 = undefined;
    var nvim = config.connect(io, nvim_sock, &nvim_buf) catch |err|
        return giveUp(gpa, io, &launch, &launch_owned, log, .connect_nvim, err);
    defer nvim.deinit(gpa);
    var rex_buf: [16 * 1024]u8 = undefined;
    var rex: Client = Client.connect(gpa, io, rex_url, &rex_buf) catch |err|
        return giveUp(gpa, io, &launch, &launch_owned, log, .connect_rex, err);
    defer rex.deinit(gpa);

    var loop: Loop = try .init();
    defer loop.deinit();
    for ([_]std.c.SIG{ .TERM, .INT, .HUP }) |sig| try loop.addSignal(sig, @backingInt(Daemon.Token.signal));
    try loop.addRead(nvim.readFd(), @backingInt(Daemon.Token.nvim_socket));
    try loop.addRead(server.socket.handle, @backingInt(Daemon.Token.helper_listener));
    try loop.addRead(rex.fd(), @backingInt(Daemon.Token.rex_socket));

    var d: Daemon = .{
        .gpa = gpa,
        .io = io,
        .loop = &loop,
        .nvim = .{ .client = &nvim },
        .rex = .{ .gpa = gpa, .client = &rex, .session_id = session_id },
        .exe = exe,
        .sock = sock,
    };
    // first: terminals restored and panes hung up before anything else ends
    defer d.deinit();
    launch_owned = false;
    try d.start(launch);

    const stop_reason = try d.run(&server);
    std.debug.print("neorex daemon: {t}\n", .{stop_reason});
    // a clean end needs no log
    std.Io.Dir.cwd().deleteFile(io, log) catch {};
}

/// What the daemon was doing when it gave up before showing any window.
const Step = enum { start_nvim, connect_nvim, connect_rex };

/// Restores the launcher's pane and tells it why we're stopping, then returns
/// `err`, which `main` prints to the log with its trace. `launch` is no
/// longer owned.
fn giveUp(
    gpa: std.mem.Allocator,
    io: std.Io,
    launch: *Pane,
    launch_owned: *bool,
    log: []const u8,
    step: Step,
    err: anyerror,
) anyerror {
    var buf: [256 + max_path]u8 = undefined;
    const message = failureMessage(&buf, step, err, log);
    launch_owned.* = false;
    launch.terminal_surface.?.destroyWithMessage(gpa, io, message);
    return err;
}

/// A line for the user: what failed, and where the log is.
fn failureMessage(buf: []u8, step: Step, err: anyerror, log: []const u8) []const u8 {
    var w: std.Io.Writer = .fixed(buf);
    printFailure(&w, step, err, log) catch {};
    return w.buffered();
}

fn printFailure(w: *std.Io.Writer, step: Step, err: anyerror, log: []const u8) !void {
    try w.writeAll("neorex: ");
    switch (step) {
        .start_nvim => if (err == error.FileNotFound)
            try w.writeAll("nvim not found")
        else
            try w.print("couldn't start nvim: {t}", .{err}),
        .connect_nvim => if (err == error.Timeout)
            try w.writeAll("Neovim didn't start in time")
        else
            try w.print("couldn't connect to Neovim: {t}", .{err}),
        .connect_rex => try w.print("couldn't connect to Rex: {t}", .{err}),
    }
    try w.print("; see {s}\r\n", .{log});
}

fn env(name: [*:0]const u8) ?[]const u8 {
    return if (std.c.getenv(name)) |v| std.mem.span(v) else null;
}

/// Waits up to 5 s for the launcher's helper and takes its terminal.
fn takePane(gpa: std.mem.Allocator, io: std.Io, server: *net.Server) !*Surface {
    var fds = [_]std.c.pollfd{.{ .fd = server.socket.handle, .events = std.c.POLL.IN, .revents = 0 }};
    if ((std.posix.poll(&fds, 5000) catch 0) == 0) return error.NoPane;
    const hello = try pane.acceptHello(server, io);
    errdefer hello.close(io);
    return Surface.create(gpa, hello.stream, hello.tty);
}

test "base path fits a socket" {
    var buf: [256]u8 = undefined;
    const base = try basePath(&buf);
    try std.testing.expect(std.mem.indexOf(u8, base, "/neorex-") != null);
    var sock: [max_path + 1]u8 = undefined;
    try std.testing.expect((try path(&sock, base, ".nvim.sock")).len <= max_path);
}

extern "c" fn openpty(master: *c_int, slave: *c_int, name: ?[*]u8, termp: ?*const std.posix.termios, winp: ?*const std.posix.winsize) c_int;

/// The launcher, for a test: connects to the daemon's socket as soon as it
/// listens, and hands over a pseudo-terminal's slave as its pane.
const FakeLauncher = struct {
    io: std.Io,
    sock: []const u8,
    slave: std.posix.fd_t,
    stream: ?net.Stream = null,

    fn ready(ctx: ?*anyopaque) anyerror!void {
        const self: *FakeLauncher = @ptrCast(@alignCast(ctx.?));
        const stream = try (try net.UnixAddress.init(self.sock)).connect(self.io);
        self.stream = stream;
        var buf: [16]u8 = undefined;
        var w = stream.writer(self.io, &buf);
        var hello: [16]u8 = undefined;
        try @import("fdpass.zig").send(&w, try pane.helloBytes(&hello, null), self.slave);
        // the daemon has its own copy now
        _ = std.c.close(self.slave);
    }
};

/// A pseudo-terminal's output, read as it comes: restoring a terminal waits
/// for its output to be taken.
const Screen = struct {
    master: std.posix.fd_t,
    text: [1024]u8 = undefined,
    len: usize = 0,

    fn read(self: *Screen) void {
        while (self.len < self.text.len) {
            const n = std.c.read(self.master, self.text[self.len..].ptr, self.text.len - self.len);
            if (n <= 0) return;
            self.len += @intCast(n);
        }
    }

    fn shown(self: *const Screen) []const u8 {
        return self.text[0..self.len];
    }
};

/// Neovim that never answers, leaving its socket file behind.
fn connectNever(io: std.Io, nvim_sock: []const u8, _: []u8) anyerror!Nvim {
    const stale = try std.Io.Dir.cwd().createFile(io, nvim_sock, .{});
    stale.close(io);
    return error.Timeout;
}

fn exists(io: std.Io, file: []const u8) bool {
    std.Io.Dir.cwd().access(io, file, .{}) catch return false;
    return true;
}

test "a start that can't connect to Neovim leaves no sockets and says why in the pane" {
    const gpa = std.testing.allocator;
    const io = std.testing.io;
    var base_buf: [256]u8 = undefined;
    const base = try basePath(&base_buf);
    var sock_buf: [max_path + 1]u8 = undefined;
    var nvim_sock_buf: [max_path + 1]u8 = undefined;
    const sock = try path(&sock_buf, base, ".sock");
    const nvim_sock = try path(&nvim_sock_buf, base, ".nvim.sock");

    // the pane neorex was run in
    var master: c_int = undefined;
    var slave: c_int = undefined;
    try std.testing.expectEqual(@as(c_int, 0), openpty(&master, &slave, null, null, null));
    defer _ = std.c.close(master);
    var screen: Screen = .{ .master = master };
    const reader = try std.Thread.spawn(.{}, Screen.read, .{&screen});
    var launcher: FakeLauncher = .{ .io = io, .sock = sock, .slave = slave };
    defer if (launcher.stream) |s| s.close(io);

    try std.testing.expectError(error.Timeout, serve(gpa, io, base, &.{}, .{
        .rex_url = "ws://unused",
        .session_id = "unused",
        .nvim_exe = "true",
        .connect = connectNever,
        .ready = FakeLauncher.ready,
        .ctx = &launcher,
    }));
    try std.testing.expect(!exists(io, sock));
    try std.testing.expect(!exists(io, nvim_sock));

    // the pane shows the reason; the launcher's helper was hung up on
    reader.join();
    try std.testing.expect(std.mem.indexOf(u8, screen.shown(), "Neovim didn't start in time") != null);
    var hangup: [1]u8 = undefined;
    try std.testing.expectEqual(@as(isize, 0), std.c.read(launcher.stream.?.socket.handle, &hangup, 1));
}
