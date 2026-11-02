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
    /// Starts Neovim: its arguments follow.
    nvim_command: []const []const u8 = &.{"nvim"},
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
    var startup: Startup = .{
        .gpa = gpa,
        .io = io,
        .log = log,
        .launch = Pane.startup(rex_block, try takePane(gpa, io, &server)),
    };
    defer startup.deinit();

    var argv: std.ArrayList([]const u8) = .empty;
    defer argv.deinit(gpa);
    // before the user's config, so it can tell it runs under neorex
    try argv.appendSlice(gpa, config.nvim_command);
    try argv.appendSlice(gpa, &.{ "--headless", "--listen", nvim_sock, "--cmd", "let g:neorex = 1" });
    try argv.appendSlice(gpa, nvim_args);
    // its stderr is ours: the log
    var child = std.process.spawn(io, .{ .argv = argv.items, .stdin = .ignore, .stdout = .ignore }) catch |err|
        return startup.giveUp(.start_nvim, err);
    startup.nvim_child = &child;
    // after the terminals are restored: SIGHUP makes Neovim save swap files and exit
    defer if (child.id) |id| {
        _ = std.c.kill(id, .HUP);
        _ = child.wait(io) catch {};
    };

    var nvim_buf: [16 * 1024]u8 = undefined;
    var nvim = config.connect(io, nvim_sock, &nvim_buf) catch |err|
        return startup.giveUp(.connect_nvim, err);
    defer nvim.deinit(gpa);
    var rex_buf: [16 * 1024]u8 = undefined;
    var rex: Client = Client.connect(gpa, io, config.rex_url, &rex_buf) catch |err|
        return startup.giveUp(.connect_rex, err);
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
        .rex = .{ .gpa = gpa, .client = &rex, .session_id = config.session_id },
        .exe = exe,
        .sock = sock,
    };
    // first: terminals restored and panes hung up before anything else ends
    defer d.deinit();
    try d.start(startup.release());

    const stop_reason = try d.run(&server);
    std.debug.print("neorex daemon: {t}\n", .{stop_reason});
    // a clean end needs no log
    std.Io.Dir.cwd().deleteFile(io, log) catch {};
}

/// What the daemon was doing when it gave up before showing any window.
const StartStep = enum { start_nvim, connect_nvim, connect_rex };

/// How Neovim ended.
const NvimExit = union(enum) { status: u8, signal: u32 };

/// The launcher's pane, owned until the daemon takes it over, and what's
/// needed to tell its user why a start failed.
const Startup = struct {
    gpa: std.mem.Allocator,
    io: std.Io,
    log: []const u8,
    launch: Pane,
    owned: bool = true,
    /// Set once Neovim is started.
    nvim_child: ?*std.process.Child = null,

    fn deinit(self: *Startup) void {
        if (self.owned) self.launch.deinit(self.gpa, self.io);
    }

    /// The pane, for the daemon to own.
    fn release(self: *Startup) Pane {
        self.owned = false;
        return self.launch;
    }

    /// Restores the pane and tells it why we're stopping, then returns
    /// `err`, which `main` prints to the log with its trace.
    fn giveUp(self: *Startup, step: StartStep, err: anyerror) anyerror {
        var buf: [256 + max_path]u8 = undefined;
        const message = failureMessage(&buf, step, err, self.nvimExit(), self.log);
        var launch = self.release();
        launch.deinitWithMessage(self.gpa, self.io, message);
        return err;
    }

    /// How Neovim ended, if it has: a Neovim that exited never made its
    /// socket. Doesn't wait. A Neovim found ended is reaped.
    fn nvimExit(self: *Startup) ?NvimExit {
        const child = self.nvim_child orelse return null;
        const id = child.id orelse return null;
        var status: c_int = 0;
        if (std.c.waitpid(id, &status, std.c.W.NOHANG) != id) return null;
        child.id = null;
        const code: u32 = @intCast(status);
        if (std.c.W.IFEXITED(code)) return .{ .status = std.c.W.EXITSTATUS(code) };
        return .{ .signal = @backingInt(std.c.W.TERMSIG(code)) };
    }
};

/// A line for the user: what failed, and where the log is.
fn failureMessage(buf: []u8, step: StartStep, err: anyerror, nvim_exit: ?NvimExit, log: []const u8) []const u8 {
    var w: std.Io.Writer = .fixed(buf);
    printFailure(&w, step, err, nvim_exit, log) catch {};
    return w.buffered();
}

fn printFailure(w: *std.Io.Writer, step: StartStep, err: anyerror, nvim_exit: ?NvimExit, log: []const u8) !void {
    try w.writeAll("neorex: ");
    switch (step) {
        .start_nvim => if (err == error.FileNotFound)
            try w.writeAll("nvim not found")
        else
            try w.print("couldn't start nvim: {t}", .{err}),
        .connect_nvim => if (nvim_exit) |exit| switch (exit) {
            .status => |n| try w.print("Neovim exited (status {d}) before it was ready", .{n}),
            .signal => |n| try w.print("Neovim was killed (signal {d}) before it was ready", .{n}),
        } else if (err == error.Timeout)
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

/// Neovim that never answers, leaving its socket file behind. Takes a moment,
/// as the real one does: enough for a Neovim that is quitting to quit.
fn connectNever(io: std.Io, nvim_sock: []const u8, _: []u8) anyerror!Nvim {
    try io.sleep(.fromMilliseconds(300), .awake);
    const stale = try std.Io.Dir.cwd().createFile(io, nvim_sock, .{});
    stale.close(io);
    return error.Timeout;
}

fn exists(io: std.Io, file: []const u8) bool {
    std.Io.Dir.cwd().access(io, file, .{}) catch return false;
    return true;
}

/// Runs a start whose Neovim connect fails, with `nvim_command` standing in
/// for Neovim, and checks no sockets remain and the pane shows `reason`.
fn expectFailedStart(nvim_command: []const []const u8, reason: []const u8) !void {
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
        .nvim_command = nvim_command,
        .connect = connectNever,
        .ready = FakeLauncher.ready,
        .ctx = &launcher,
    }));
    try std.testing.expect(!exists(io, sock));
    try std.testing.expect(!exists(io, nvim_sock));

    // the pane shows the reason; the launcher's helper was hung up on
    reader.join();
    try std.testing.expect(std.mem.indexOf(u8, screen.shown(), reason) != null);
    var hangup: [1]u8 = undefined;
    try std.testing.expectEqual(@as(isize, 0), std.c.read(launcher.stream.?.socket.handle, &hangup, 1));
}

test "a start whose Neovim is still running says it didn't start in time" {
    try expectFailedStart(&.{ "sh", "-c", "exec sleep 30", "--" }, "Neovim didn't start in time");
}

test "a start whose Neovim exited says so, with its status" {
    try expectFailedStart(&.{ "sh", "-c", "exit 3", "--" }, "Neovim exited (status 3) before it was ready");
}
