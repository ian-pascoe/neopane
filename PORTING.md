# Zig → Rust port (temporary; deleted when the port is done)

The goal is **full parity**: the Rust crate does everything the Zig code does,
with every Zig test ported, and then the Zig code is deleted.

## Layout

The crate is at the repo root. Rust files sit next to the Zig files they port,
named in snake_case. `src/lib.rs` declares every module; `src/main.rs` is the
binary (it ports `src/main.zig`). The module tree is already declared, with
empty files: **only fill in the files you were assigned**. Don't touch other
modules' files, `lib.rs`, the `mod.rs` files or `Cargo.toml` unless your brief
says so; if you need a crate dependency that isn't there, say so in your
final report and stop short of adding it, unless your brief allows it.

| Zig | Rust |
|---|---|
| `nvim/msgpack.zig`, `Framer.zig`, `rpc.zig`, `Nvim.zig`, `Forwarder.zig`, `windows.zig` | `nvim/msgpack.rs`, `framer.rs`, `rpc.rs`, `nvim.rs`, `forwarder.rs`, `windows.rs` |
| `screen/Grid.zig`, `Grids.zig`, `Style.zig`, `render.zig`, `PaneView.zig`, `FloatGroups.zig` | `screen/grid.rs`, `grids.rs`, `style.rs`, `render.rs`, `pane_view.rs`, `float_groups.rs` |
| `term/input.zig`, `modes.zig`, `Signals.zig`, `Terminal.zig` | `term/input.rs`, `modes.rs`, `signals.rs`, `terminal.rs` |
| `rex/WebSocket.zig`, `Client.zig`, `Session.zig`, `ids.zig` | `rex/web_socket.rs`, `client.rs`, `session.rs`, `ids.rs` |
| `daemon/Loop.zig` | `daemon/event_loop.rs` (`loop` is a keyword) |
| other `daemon/X.zig`, `debug/X.zig` | `daemon/x.rs`, `debug/x.rs` (snake_case) |
| `golden.zig` | `tests/golden.rs`, behind the `golden` feature |

`nvim/windows.lua` and `nvim/testdata/` stay as they are; use
`include_str!`/`include_bytes!` where Zig used `@embedFile`.

## How to port

- **Faithful, not creative.** Keep each Zig file's types, functions, comments
  (doc comments `//!` and `///` carry over as-is), structure and behaviour.
  Names: Zig `camelCase` functions → `snake_case`; types keep their names
  (`Grid`, `Grids`, `Nvim`, `PaneView`, ...). Don't redesign; where Rust's
  ownership forces a change (e.g. pointers between objects → ids/indices, or
  a borrow that would conflict), make the smallest change and add a comment.
- **Every Zig `test "name"` becomes a Rust `#[test] fn name()`** in a
  `#[cfg(test)] mod tests` at the bottom of the same file, checking the same
  things. Don't drop, weaken or merge tests. Tests that need Neovim skip (return
  early, printing a note) when `nvim` isn't installed, as the Zig ones did.
- Tests run one at a time (`.cargo/config.toml` sets `RUST_TEST_THREADS=1`),
  as Zig's did, so process-wide state (signals, cwd, env) is OK to touch, but
  clean up after yourself and use unique temp paths (include the pid).
- **Allocators** go away: use `Vec`, `String`, `HashMap`, etc. Zig
  `std.AutoArrayHashMap` / `StringArrayHashMap` (insertion-ordered; some code
  depends on that order, and on `swapRemove`/`orderedRemove` semantics) →
  `indexmap::IndexMap` (`swap_remove`/`shift_remove` to match).
- **Writers**: Zig `*std.Io.Writer` used to build output (msgpack, escape
  sequences, frames) → `&mut Vec<u8>`; writing to it can't fail. Where the
  Zig wrote to an fd through a buffered writer, buffer into a `Vec<u8>` and
  write it out on flush.
- **Errors**: each low-level module defines its own small error enum
  (`#[derive(Debug, Clone, Copy, PartialEq, Eq)]`, names from the Zig
  `error.X` names) when tests or callers match on specific errors; wrap
  `std::io::Error` where needed. The top layers (daemon, serve, launch,
  debug, main) may use `Box<dyn std::error::Error>` (alias
  `type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>`).
- **Syscalls**: use the `libc` crate directly (kqueue, poll, termios,
  openpty, cfmakeraw, sendmsg/recvmsg with SCM_RIGHTS, sigaction, ioctl,
  fcntl). Keep it macOS/kqueue-only, as the Zig is. Fds are `RawFd` (i32).
  Wrap unsafe in small functions, as the Zig wraps `std.c` calls.
- `std.json.Value` → `serde_json::Value`. `std.crypto.hash.Sha1` →
  `sha1_smol`. `std.base64` → `base64`. Child processes →
  `std::process::Command`. Sleeps → `std::thread::sleep`.
- Zig `unreachable`/`assert` → `unreachable!`/`debug_assert!` (or `assert!`
  where the check matters). Integer casts: keep the Zig's widths (u32 for
  grid sizes, i64 for handles, etc.) and its checked casts.
- `#[allow(dead_code)]` is set crate-wide during the port. Keep
  `cargo clippy --all-targets` free of warnings in your files where
  reasonable, and run `cargo fmt`.

## The foundation API (nvim/msgpack, rpc, framer, nvim)

Everything that talks to Neovim uses these. Read `src/nvim/msgpack.rs`,
`src/nvim/rpc.rs`, `src/nvim/framer.rs` and `src/nvim/nvim.rs` for the exact
signatures. In short:

- `msgpack::write_nil/write_bool/write_int/write_float/write_str/
  write_str_header/write_array_header/write_map_header(w: &mut Vec<u8>, ..)`.
- `msgpack::Decoder<'a>` (`Copy`): `feed(&mut self, &'a [u8])`,
  `next(&mut self) -> Result<Option<Token<'a>>, msgpack::Error>`.
- `rpc::Message<'a>` = `Request { id, method: &'a [u8], params: Decoder<'a> }`
  | `Response { id, err: Option<&'a [u8]>, result: Decoder<'a> }`
  | `Notification { method, params }`; `rpc::parse(&'a [u8])`;
  `rpc::expect_array/expect_int/expect_str/expect_bool/expect_map/
  expect_handle/expect_bytes/skip(d: &mut Decoder<'a>)` returning
  `Result<_, rpc::Error>` (`rpc::Error::BadMessage`, ...).
- `Nvim`: `spawn(argv)`, `connect(path)`, `connect_retry(path)`,
  `request(method, param_count) -> u32` (write the params into `nvim.writer()`,
  a `&mut Vec<u8>`), `flush()`, `respond_error`, `ui_attach`,
  `ui_try_resize`, `ui_try_resize_grid`, `exec_lua`, `read_fd()`, `read()`.
  **`next()` returns an owned `Frame`** (one whole message's bytes), not a
  borrowed message, so the caller can keep writing to Nvim while it handles
  the message:
  ```rust
  while let Some(frame) = nvim.next()? {
      match frame.message()? { rpc::Message::Notification { method, mut params } => .., .. }
  }
  ```
  Errors: `nvim::Error` (`Io`, `Framer`, `Rpc`, `Timeout`); most methods
  return `io::Result`. `Drop` does what Zig's `deinit` did.
- Test-only helpers in `nvim::nvim`: `spawn_for_test()` (`nvim --clean
  --embed`) and `spawn_for_test_with(&argv)` return `Option<Nvim>`, `None`
  (after printing a note) when Neovim isn't installed — use them for Zig's
  `Nvim.spawn(...) catch error.SkipZigTest`; `wait_response(&mut nvim, id)`
  returns the response's `Frame`.

## Verify

`cargo build`, `cargo test` (your module's tests, at least:
`cargo test --lib <module path>`), `cargo clippy --all-targets`,
`cargo fmt --check`. The Zig still builds and tests with `zig build test`
until it's removed; don't break it.
