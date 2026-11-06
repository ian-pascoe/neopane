# Embedding Lua 5.1 in neopane: `mlua` + vendored interpreter

Researched against `mlua 0.12.2`, `mlua-sys 0.13.0`, `lua-src 551.0.2`
(bundles PUC Lua 5.1.5), `luajit-src 210.7.4+c6ffc14`, rustc/cargo 1.99.1,
on macOS arm64 (this machine). Primary sources: the crates' unpacked source and
READMEs, their GitHub Actions workflows, upstream issue trackers, and my own
builds (scratch crate in `/tmp`, not in this repo).

## TL;DR

- **Use `mlua` with `lua51` + `vendored`** (PUC Lua 5.1.5 via `lua-src`). It
  needs only a C compiler (via `cc`), no `make`, and it is the only
  5.1-semantics option that built and linked statically on every target I could
  exercise (macOS arm64/x86_64, Linux x86_64/aarch64 glibc and musl).
- `luajit` + `vendored` works on glibc and macOS, and static musl on x86_64, but
  **fails to link on aarch64 static musl** (`__clear_cache`), needs `make`, and
  needs a separate `.bat` path on MSVC. It is 2.5x the size cost of 5.1 and its
  one advantage (speed) is irrelevant for a short-lived config process.
- PUC 5.1 is EOL upstream but mlua has no deprecation notice and tests/builds it
  in CI on all three OSes.

## 1. Build / link matrix

How mlua vendors: `vendored` -> `mlua-sys` build script -> `lua-src` (for
`lua5x`) or `luajit-src` (for `luajit`), and links the resulting static lib
(mlua README: "`vendored`: build static Lua(JIT) libraries from sources during
`mlua` compilation using lua-src or luajit-src").
`mlua-sys/Cargo.toml` pins `lua-src >= 551.0.2, < 551.1.0` and
`luajit-src >= 210.7.0, < 210.8.0`.

### `lua51` + `vendored` (lua-src)

`lua-src/src/lib.rs` compiles the C files with the `cc` crate (no `make`), and
has per-target branches: Linux (`LUA_USE_LINUX`), BSD, illumos/solaris,
`apple-darwin` (for 5.1 it uses `LUA_USE_LINUX`, not `LUA_USE_MACOSX`), iOS,
Windows (`LUA_USE_WINDOWS`), emscripten/wasi; anything else errors
"don't know how to build Lua for {target}".
lua-src CI (`.github/workflows/main.yml`) builds lua51 for x86_64 gnu **and
musl**, i686, aarch64-gnu, arm-gnueabi, aarch64-apple-darwin, x86_64 windows-gnu and
**windows-msvc**, plus wasm; and runs tests on gnu, musl, macOS, msvc.
mlua's own CI builds+tests `lua51,vendored` on ubuntu x86_64, macOS aarch64 and
windows-msvc, and cross-builds it to aarch64-unknown-linux-gnu.

My builds (release, `lto`, `strip`; each ran `Lua::new().load("return 1").eval()`):

| Target | Result |
|---|---|
| aarch64-apple-darwin (native) | OK, runs |
| x86_64-apple-darwin (cross, run under Rosetta) | OK, runs |
| aarch64-unknown-linux-gnu (Debian docker, native) | OK, runs; dynamic only to libc/libm/libgcc_s |
| x86_64-unknown-linux-musl (Alpine docker, amd64 emulated) | OK, runs; static-pie |
| aarch64-unknown-linux-musl (Alpine docker, native) | OK, runs; static-pie |
| x86_64/aarch64 linux gnu + musl via `cargo zigbuild` | OK (builds only) |
| x86_64-pc-windows-msvc | **not tested** (no MSVC toolchain here); covered by mlua + lua-src CI above |

### `luajit` + `vendored` (luajit-src)

`luajit-src` has **no `cc`-only path**: on Unix it copies the LuaJIT tree and
runs `make -e` (`make` required; `gmake` on FreeBSD/Dragonfly) with
`BUILDMODE=static`, `STATIC_CC`/`TARGET_LD`/`TARGET_AR`/`TARGET_STRIP` derived
from the `cc` crate compiler; it sets `MACOSX_DEPLOYMENT_TARGET` to 10.14
(x86_64) / 11.0 (arm64) if unset; `TARGET_SYS=Linux|Windows`. On `*msvc*` it
instead runs `msvcbuild.bat static` in a Developer-prompt environment
(`src/lib.rs`, `build_unix` / `build_msvc`). 32-bit cross builds need a `HOST_CC`.

CI: luajit-src tests x86_64/i686/aarch64 gnu, x86_64+aarch64 macOS, x86_64
windows-gnu/msvc. **Not covered: any musl target.** mlua CI builds luajit on
ubuntu x86_64, macOS aarch64, windows-msvc, cross aarch64-gnu.

My builds:

| Target | Result |
|---|---|
| aarch64-apple-darwin | OK, runs |
| x86_64-apple-darwin (cross, Rosetta) | OK, runs |
| aarch64-unknown-linux-gnu (docker) | OK, runs |
| x86_64-unknown-linux-musl (Alpine, amd64) | OK, runs; static-pie |
| **aarch64-unknown-linux-musl (Alpine, native)** | **FAIL at link**: `lj_mcode.c: undefined reference to __clear_cache`. Rust's self-contained musl link has no libgcc to provide it. Builds only with `-C target-feature=-crt-static` (dynamic musl, not a static binary). |
| any linux target via `cargo zigbuild` | **FAIL**: undefined `_Unwind_RaiseException`, `__register_frame` etc. while `make` links LuaJIT's own host exe; zig's cc wrapper doesn't link an unwinder. |
| x86_64-pc-windows-msvc | not tested; needs `msvcbuild.bat` + `cl.exe` env |

Other known LuaJIT problems:

- macOS x86_64 + Rust TLS crash: LuaJIT/LuaJIT#1099 (mlua maintainer). Closed
  as "not our bug" by Mike Pall: the Rust linker ignores TLS segment alignment
  on macOS (rust-lang/rust#51828). mlua's `tests/hooks.rs` still cfg-skips a
  test on `luajit` + macOS "causes a memory reference issue". I did not
  reproduce a crash with the minimal hello on either mac arch.
- Upstream LuaJIT 2.1 is a rolling branch (version string `210.7.4+c6ffc14`
  is a snapshot); no tagged releases to pin to.

## 2. Is PUC Lua 5.1 still supported?

- mlua 0.12.2 README: "supports Lua 5.5, 5.4, 5.3, 5.2, 5.1 (including
  LuaJIT) and Luau". Feature `lua51` exists (`Cargo.toml` `[features]`). I found
  **no deprecation notice** for 5.1 in README, FAQ or `CHANGELOG.md`; the
  changelog keeps adding 5.1 fixes (e.g. "`Lua::push_c_function` now correctly
  handles OOM for Lua 5.1 and Luau", global/per-thread hooks "Lua 5.1+").
- mlua CI matrix includes `lua51` on Linux, macOS and Windows.
- Caveat: `lua-src` bundles 5.1.5 (released 2012, the final 5.1 release; lua.org
  no longer develops 5.1). Some mlua features are weaker on 5.1 (async
  metamethods "except 5.1", limited `Thread::reset`). Security fixes are
  whatever lua-src patches; none are promised.

Dialects mlua can embed:

| Feature | Dialect | Notes |
|---|---|---|
| `lua51` | PUC Lua 5.1.5 | exact Neovim-adjacent semantics minus LuaJIT extras |
| `luajit` | Lua 5.1 + LuaJIT extensions (`ffi`, `bit`, `jit`, goto, some 5.2 bits) | what Neovim actually runs |
| `luajit52` | LuaJIT with 5.2 compat flag | not 5.1-pure |
| `lua54` / `lua53` / `lua52` / `lua55` | 5.4 / 5.3 / 5.2 / 5.5 | integers, `goto`, no `setfenv`/`unpack`/`loadstring`; breaks 5.1 configs |
| `luau` | Roblox Luau: 5.1-derived, typed, sandbox-oriented | diverges (no `loadstring` etc.), forces vendored C++ build |

Note: Neovim configs may use `jit`, `bit` or `ffi`; those exist only on LuaJIT.
`lua51` has no `bit` library (needs a polyfill).

## 3. Size and compile time (macOS arm64, this machine)

Hello program: `Lua::new().load("return 1").eval::<i64>()`; `mlua =0.12.2`
only the version + `vendored` features; profile `opt-level=3, lto=true,
codegen-units=1, strip=true, panic=abort`. Time = cold build of whole crate
graph including crate download (incremental rebuilds just relink).

| Variant | Stripped binary | Delta vs baseline | Cold build |
|---|---|---|---|
| baseline (no Lua) | 302,560 B | - | ~3 s |
| `lua51,vendored` | 559,904 B | **+257 KB** | ~13 s |
| `lua54,vendored` | 627,568 B | +325 KB | ~11 s |
| `luajit,vendored` | 955,696 B | +653 KB | ~17 s |
| `luau` | 1,645,536 B | +1.34 MB | ~28 s |

Other targets (same recipe): lua51 aarch64-glibc 595 KB, x86_64-musl static
763 KB, x86_64-apple 548 KB; luajit aarch64-glibc 988 KB, x86_64-musl static
1.13 MB, x86_64-apple 953 KB. Sizes are all negligible next to a TUI binary.

## 4. Interrupting / limiting a script

- `Lua::set_hook(HookTriggers, cb)` (not `luau`): global hook, with
  `every_nth_instruction`, per-line and call/return triggers; returning `Err`
  aborts the script. `remove_hook`. mlua's own test
  `test_limit_execution_instructions` uses a 30-instruction count hook to stop
  an infinite loop with "time's up". Works on `lua51`.
- `Lua::set_interrupt(cb)` / `remove_interrupt` are **Luau-only** (`#[cfg(feature
  = "luau")]`, wired to `lua_callbacks(..).interrupt`).
- `Lua::set_memory_limit(bytes)`: supported for 5.1/JIT/Luau (changelog "Support
  setting memory limit for Lua 5.1/JIT/Luau"), but not in module mode; mlua's
  hooks test guards `luajit` against it failing.
- **LuaJIT does not honour count hooks in JIT-compiled code.** mlua's test
  comment: "For LuaJIT disable JIT, as compiled code does not trigger hooks",
  and it calls `jit.off()` before setting the count hook. (In LuaJIT's
  `lj_dispatch.c`, `lua_sethook` only aborts trace *recording*; already-compiled
  traces run without hook checks.) So a tight Lua loop can outrun a hook unless
  `jit.off()`.
- For the plan (kill the child process on timeout) none of this is required;
  the count hook is a cheap, portable secondary guard on `lua51`.

## 5. Conclusion

**Ship `mlua = { version = "0.12", features = ["lua51", "vendored"] }`.**

| Criterion | lua51 | luajit |
|---|---|---|
| 5.1 semantics | exact (5.1.5) | 5.1 + ffi/jit/bit/goto (superset) |
| Toolchain | C compiler only | C compiler + `make` (Unix) / `msvcbuild.bat` (MSVC) |
| macOS arm64 + x86_64 | OK | OK (watch TLS issue #1099 on x86_64) |
| Linux glibc x86_64/aarch64 | OK | OK |
| Linux musl static x86_64 | OK | OK |
| Linux musl static aarch64 | OK | **link failure** |
| cross-compiling with zig | OK | **fails** |
| MSVC | covered by upstream CI | covered, different build path |
| Hooks | work | blind to JIT'd code |
| Size | +257 KB | +653 KB |

Risks to accept with `lua51`: EOL interpreter (no upstream security fixes; the
config runs the user's own code so exposure is low); missing LuaJIT-only
modules (`bit`, `ffi`, `jit`) that some Neovim snippets use, so document
neopane's Lua as "Lua 5.1, no LuaJIT extensions". Windows MSVC is untested
here; verify in CI when it is added. Keep the `luajit` feature as a possible
later opt-in only for glibc/macOS builds.
