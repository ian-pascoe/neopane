# Pinning neopane's Lua config API: sandbox, Lua 5.1 facts, Ghostty argv, LuaLS

Pins checked: `mlua 0.12.2`, `mlua-sys 0.13.0`, `lua-src 551.0.2` (PUC Lua 5.1.5),
Ghostty `v1.4.0` (83fb956, the installed `/Applications/Ghostty.app` is 1.4.0),
lua-language-server 3.19.2 (local install), lazydev.nvim `ff2cbcb` (2026-03-14),
nvim-lspconfig / Neovim master. Paths below are inside those unpacked sources
unless a URL is given. "Tested" = a throwaway check in `/tmp` (not in this repo).
Prior mlua research: `origin/research/embedded-lua:docs/research/embedded-lua.md`.

## Verdicts

**1. mlua 0.12 + lua51**
- `Lua::new()` opens `StdLib::ALL_SAFE` = every lib except `debug` (and `ffi`).
  **`debug` is not opened and `new_with` refuses it.** `io` and `os` ARE opened,
  unrestricted (`os.execute`, `os.exit`, `io.popen` all present). Tested.
- Choose a set with `Lua::new_with(StdLib::TABLE | STRING | MATH | OS | PACKAGE,
  LuaOptions::default())`. The base lib (`pairs`, `pcall`, `dofile`, `loadfile`,
  `loadstring`, `getfenv`, `coroutine`...) is always opened. `coroutine` is
  not a flag on 5.1: it comes with base. `require`/`module` come with `PACKAGE`.
  Tested.
- **Yes**, from Rust (or Lua) after creation: set `package.path`, set
  `package.cpath = ""`, nil `package.loadlib`, nil `package.loaders[3]`/`[4]`.
  Note mlua already neutralises them in safe mode: `loadlib` becomes an erroring
  stub, `loaders[3]` becomes a stub returning "can't load C modules in safe
  mode", and `loaders[4]` is removed, so `#package.loaders == 3` out of the box.
  Still clear `cpath` yourself; mlua leaves it as the compiled-in default.
- **mlua's `require` handling does not interfere** on `lua51`: mlua has no
  `require` of its own except for Luau (`src/luau/`). Lua 5.1's `require` reads
  `package.loaders` on every call, so edits take effect.

**2. Lua 5.1 facts** (all confirmed)
- No `__pairs`: `pairs(t)` just returns `next, t, nil`; a `__pairs` metamethod is
  ignored (tested: it was not called). Iteration order of a table is unspecified.
- All numbers are doubles (`LUA_NUMBER double`, `LUA_NUMBER_FMT "%.14g"`); there
  is no integer subtype, no `math.type`, `2^53+1 == 2^53` is true. Tested.
- `os.exit` calls C `exit()` and **kills the whole process** from an embedded
  state. Override it (`os.exit = function() error(...) end`) or omit `OS`; in a
  child process dying is arguably fine but it skips the host's cleanup and exit
  code reporting. Also present and worth removing: `os.execute`, `io.popen`,
  `os.remove`/`rename`, `dofile`, `loadfile`, `string.dump`, `module`.
- Also 5.1-only names exist: `unpack`, `loadstring`, `setfenv`/`getfenv`,
  `table.getn`, `newproxy`, `gcinfo`. No `table.unpack`, no `goto`, no `utf8`, no
  `bit`. Tested.

**3. Ghostty command line**
- **Only `--title=VALUE` works.** `--title VALUE` (two words) does not: the
  parser takes a value only after `=`; the bare `--title` gets no value
  (`ValueRequired`) and `VALUE` then fails the "starts with `--`" check
  ("invalid field"). Both become config diagnostics, not a title.
- `-e` must come **last**: everything after `-e` is the command.
- macOS `open -na Ghostty.app --args --title=X -e cmd`: **source-verified, not run.**
  `open --args` passes argv to the app's `main()`; `main.swift` hands argv to
  `ghostty_init`; the app loads CLI args into the config; the first surface
  applies `config.title` through `set_title` as it is created. Note `title`
  then pins the title: later title escape sequences are ignored.
  `-e` also sets `quit-after-last-window-closed = true` for that instance.
- Linux: `ghostty +new-window --title=X -e cmd` is supported (GTK only; macOS
  prints "not supported on this platform"). The receiving side also matches only
  the `--title=` prefix. `ghostty --title=X -e cmd` (plain launch) works as on
  macOS and sets `gtk-single-instance = false`. In `+new-window` the title is a
  *override* ("may be edited or removed later"), unlike the config `title`.
  Which release added `--title` to `+new-window` is **unverified** (shallow clone);
  it is present in v1.4.0.

**4. LuaLS**
- **Yes, a `---@meta` file inside the workspace is loaded with no
  `workspace.library`** (tested with `--check`: `neopane.pane`'s `string[]`
  parameter and `undefined-field` on `neopane.nope` resolved from
  `types/neopane.lua`; deleting the file gave `undefined-global`). Caveat: the
  wiki says to list definition files in `workspace.library`, but that is for
  files *outside* the workspace. Prerequisite: the editor must open the dir as a
  workspace folder; in single-file mode (no folder) nothing else is scanned and
  `.luarc.json` is not read either.
- **`"runtime.version": "Lua 5.1"` is the right key and value** in `.luarc.json`
  (no `Lua.` prefix there; default is `Lua 5.4`). Tested: under 5.1 `unpack` and
  `loadstring` are clean, `table.unpack` and `utf8`/`math.type` are flagged.
  `$schema` is **recommended** by the wiki (completion/validation) but optional;
  an unknown `$schema` host does not affect loading (tested with the real URL).
- **A `.luarc.json` does not stop lazydev from adding `vim` globals by default.**
  lazydev's default `enabled` ignores `.luarc.json` (it only checks
  `vim.g.lazydev_enabled`). LuaLS merges config per key, `.luarc.json` winning
  over the client's settings (`config.update(folder, clientConfig, rc)`), so a
  `.luarc.json` only overrides the keys it sets. With `runtime.version =
  "Lua 5.1"` it overrides lazydev's `LuaJIT` in that folder; it must not set
  `workspace.library`. lazydev itself warns "`.luarc.json` ... may break
  lazydev.nvim" in `:LazyDev`. Root detection is **not** lazydev's: it uses the
  LSP client's `workspace_folders`/`root_dir`; on Neovim >= 0.11.2 its lspconfig
  integration replaces `root_dir`, and for a file outside a known workspace it
  calls `on_dir(nil)` so the client starts in single-file mode (source reading,
  not run). If the user wants neopane's folder to be free of `vim`, they need
  `enabled = function(root) return not vim.uv.fs_stat(root.."/.luarc.json") end`
  (lazydev README) or `vim.g.lazydev_enabled = false`; neither is automatic.

## Evidence

### 1. mlua

- `src/stdlib.rs:92` `DEBUG = 1<<31` ("(**unsafe**)"); `:97` `ALL = u32::MAX`;
  `:100` `ALL_SAFE = (1<<30)-1` for non-Luau, which excludes bit 31 (`DEBUG`) and
  bit 30 (`FFI`). Doc comment: "`ALL` ... (**unsafe**) All standard libraries".
- `src/state.rs:338-343` `Lua::new()` = `new_with(StdLib::ALL_SAFE, LuaOptions::default())`.
  `:362-368` `new_with` returns `SafetyError("The unsafe `debug` module can't be
  loaded using safe `new_with`")` if `libs.contains(DEBUG)`. `:349` `unsafe_new`
  (`StdLib::ALL`) is the only way to get `debug`. Tested: `ALL_SAFE | DEBUG` errors.
- `src/state/raw.rs:151` base lib is always opened (`luaL_requiref("_G", luaopen_base)`);
  `:1695-1820` `load_std_libs` opens each lib per flag (`TABLE`, `IO`, `OS`,
  `STRING`, `MATH`, `DEBUG`, `PACKAGE`; no `COROUTINE`/`UTF8`/`BIT` flag on lua51).
  `src/state/raw.rs:298` (and `:1800`) `load_std_libs` on a safe state also
  rejects `DEBUG`.
- `src/state.rs:378-380`: after `inner_new`, if `PACKAGE` is in `libs`,
  `disable_c_modules()`. `src/state.rs:2443-2469`: sets `package.loadlib` to a
  function that errors; for `lua51`/`luajit` fetches `package.loaders` (`:2458`),
  `raw_set(3, stub)` and `raw_remove(4)` if `raw_len() >= 4` (`:2463-2466`).
  Nothing touches `package.path`/`cpath`.
- Tested dump of `Lua::new()` globals: `_G _VERSION assert collectgarbage
  coroutine dofile error gcinfo getfenv getmetatable io ipairs load loadfile
  loadstring math module newproxy next os package pairs pcall print rawequal
  rawget rawset require select setfenv setmetatable string table tonumber
  tostring type unpack xpcall`; no `debug`. `package` = `config cpath loaded
  loaders loadlib path preload seeall`; `#package.loaders` = 3 (stub at 3).
  `package.cpath` = `./?.so;/usr/local/lib/lua/5.1/?.so;.../loadall.so`.
  `package.path` = `./?.lua;/usr/local/share/lua/5.1/?.lua;...` (so the default
  includes the current directory: replace it).
- Hardening snippet, run and verified (`package.loaders` ends with 2 entries,
  `require "nonexistent"` lists only the custom path):
  ```lua
  package.path = "/home/u/.config/neopane/?.lua;/home/u/.config/neopane/?/init.lua"
  package.cpath = ""
  package.loadlib = nil
  package.loaders[4] = nil
  package.loaders[3] = nil
  os.exit = function() error("os.exit disabled") end
  ```
- `lua-src .../lua-5.1.5/loadlib.c`: `:624` default loaders are
  `loader_preload, loader_Lua, loader_C, loader_Croot` (so 3 = C, 4 = all-in-one);
  `:463` `ll_require` does `lua_getfield(L, LUA_ENVIRONINDEX, "loaders")` each
  call; `:592-603` `setpath` reads env vars `LUA_PATH`/`LUA_CPATH` at
  `luaopen_package`, so they can influence the initial value (we overwrite it);
  `:649-650` sets `path`/`cpath`. There is no `LUA_INIT` processing outside
  `lua.c`, which mlua does not build.
- mlua `README.md:233-242` "Safety": mlua's "safe" means memory safety of the API,
  not a script sandbox; hence `io`/`os` stay open.

### 2. Lua 5.1

- `.../lua-5.1.5/lbaselib.c:238-244` `luaB_pairs`: `luaL_checktype(L, 1,
  LUA_TTABLE)`; returns `next`, `t`, `nil`. No metamethod lookup. `ltm.h:19-36`
  lists the 5.1 tag methods (`index newindex gc mode eq add sub mul div mod pow
  unm len lt le concat call`): no `__pairs`. Manual:
  https://www.lua.org/manual/5.1/manual.html#pdf-pairs.
- `luaconf.h:506-507` `#define LUA_NUMBER_DOUBLE`, `LUA_NUMBER double`; `:524`
  `LUA_NUMBER_FMT "%.14g"`. Manual §2.2: "*Number* represents real
  (double-precision floating-point) numbers."
  https://www.lua.org/manual/5.1/manual.html#2.2
- `loslib.c:228-230` `os_exit` is `exit(luaL_optint(L, 1, EXIT_SUCCESS))`. Manual:
  "Calls the C function `exit`, with an optional `code`, to terminate the host
  program." https://www.lua.org/manual/5.1/manual.html#pdf-os.exit

### 3. Ghostty (v1.4.0)

- `src/cli/args.zig:112-120` args not starting with `--` are errors/diagnostics
  ("We don't support positional arguments or configuration values set with
  spaces"). `:127-132` the value is whatever follows `=` in the same argv word;
  otherwise `null`. `:400-407` a `[:0]const u8` field with `value == null`
  returns `error.ValueRequired`; optional fields (`:313-317`) use the child type.
  `src/config/Config.zig:1541` `title: ?[:0]const u8`; doc `:1527-1535`: forces
  the title at all times, ignores title escape sequences.
- `Config.zig:4959-5006` `parseManuallyHook`: `-e` swallows all remaining args as
  the command and sets `quit-after-last-window-closed = true` (`:4998`),
  `gtk-single-instance = false` (`:4997`).
- macOS: `macos/Sources/App/main.swift:8` `ghostty_init(argc, argv)`;
  `macos/Sources/Ghostty/Ghostty.Config.swift:77` `ghostty_config_load_cli_args`
  (skipped only when running in Xcode); `src/config/CApi.zig:54`;
  `Config.zig:4318` `loadCliArgs`. `src/Surface.zig:782-787` on surface init, if
  `config.title` is set, `performAction(.set_title)`; `Ghostty.App.swift:1819-1833`
  applies it with `surfaceView.setTitle`. Help text `src/cli/help.zig:57-59`
  and `src/main_ghostty.zig:86-87` document `open -na Ghostty.app --args
  --foo=bar`. `man open`: "`--args`: All remaining arguments are passed to the
  opened application in the argv parameter to main()". Not run (it would open a
  GUI window on this machine): **unverified at runtime**.
- Linux: `src/cli/new_window.zig` doc `:127-137` lists `--title` among supported
  flags ("A title that will override the title of the first surface in the new
  window"), `-e` as a trailing command, and "Only supported on GTK"; `ghostty
  +new-window --help` on macOS prints "+new-window is not supported on this
  platform." `src/apprt/gtk/class/Overrides.zig:103` parses only
  `std.mem.cutPrefix(u8, str, "--title=")`; `src/apprt/gtk/class/surface.zig:2097`
  `title_override orelse title`.

### 4. LuaLS / lazydev

- Wiki (https://luals.github.io/wiki/definition-files/): a definition file is a
  Lua file with `---@meta` on top; "To use a definition file, provide the path
  to the file (or parent directory) to `workspace.library`."
  Configuration wiki (https://luals.github.io/wiki/configuration/): `.luarc.json`
  is "(recommended)"; "It is recommended you use a JSON schema ... if your
  editor supports it"; "Settings provided in a `.luarc.json` file do not need to
  be prefixed with `Lua.`"; example uses `"runtime.version": "Lua 5.3"`.
- Local `script/config/template.lua:187-194` `Lua.runtime.version` default
  `'Lua 5.4'`, choices `Lua 5.1 ... Lua 5.5`, `LuaJIT`.
- Tested layout `/tmp/lls`: `init.lua`, `types/neopane.lua` (`---@meta`,
  `neopane = {}`, `function neopane.pane(argv) end`), `.luarc.json` with `$schema`
  and `runtime.version`. `lua-language-server --check /tmp/lls` with and without
  `.luarc.json`: `neopane` resolves, `param-type-mismatch` and `undefined-field`
  fire; with the meta file moved away, `undefined-global neopane` fires. Version
  tests listed in verdict 4. Completion itself was not driven (diagnostics prove
  the types are indexed).
- `script/provider/provider.lua:35-57`: config is loaded per `scope.folders`
  (client config, then `.luarc.json`/`.luarc.jsonc`); `script/config/config.lua:201-225`
  `update(scp, ...)` applies sources in order, key by key, later wins. With no
  workspace folder neither `.luarc.json` nor the directory is read.
- lazydev: `lua/lazydev/config.lua:17-20` default `enabled` returns
  `vim.g.lazydev_enabled == nil and true or vim.g.lazydev_enabled` (no
  `.luarc.json` test); README "disable when a .luarc.json file is found" is an
  opt-in example. `lua/lazydev/workspace.lua:166-201` `update()` writes
  `runtime.version = "LuaJIT"`, `pathStrict`, `workspace.library`... into the
  client's settings; `:212-217` debug output warns about `.luarc.json`. Root:
  `workspace.lua:107-115` `get_root` = client workspace folder containing the
  buffer, else `client.root_dir`, else `"single"`; `lua/lazydev/integrations/lspconfig.lua`
  sets `root_dir = on_dir(require("lazydev").find_workspace(bufnr))` (Neovim
  >= 0.11.2); `lazydev/init.lua:13-18` `find_workspace` returns nil if the path is
  not in an existing non-special workspace. Neovim `runtime/doc/lsp.txt:953`:
  `root_markers` is "Unused if `root_dir` is set"; `runtime/lua/vim/lsp.lua`
  `lsp_enable_callback` starts the client even when `on_dir` got `nil`.
- nvim-lspconfig `lsp/lua_ls.lua`: `.luarc.json`/`.luarc.jsonc` are the top
  `root_markers`, and its recommended `on_init` returns early (adds no Neovim
  settings) when the workspace has a `.luarc.json`; that is the upstream-endorsed
  pattern for "`.luarc.json` wins over Neovim defaults".
