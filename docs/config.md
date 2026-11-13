# The config

`~/.config/neopane/init.lua` (or `$XDG_CONFIG_HOME/neopane/init.lua`, or the
file `$NEOPANE_CONFIG` names) chooses each daemon's pane provider. It returns
`{ provider = function(attach) … end }`; ADR 0007 and
`docs/capabilities.md` cover what `provider` may return.

## Built-in pane providers

`neopane provider show` lists the built-ins. Today that is only `rex`, which
has no Lua table: return `"rex"` from `provider()` to use it.
`neopane provider show NAME` prints a built-in that has a Lua table as a Lua
file, to copy into your `init.lua` and change.

## Editor setup

`neopane provider types` writes the config's types for lua-language-server
(LuaLS) into the config's directory: the directory of `--config FILE`, else
of `$NEOPANE_CONFIG`, else `~/.config/neopane` (created if missing).

- `types/neopane.lua`, a `---@meta` file with the global `neopane` and the
  classes `neopane.Config`, `neopane.Attach`, `neopane.Provider`,
  `neopane.Timeouts`, `neopane.Pair`, `neopane.Choice`, `neopane.RunOptions` and
  `neopane.RunResult`. It is rewritten each time: run the command again after
  upgrading neopane. Its second line names the build that wrote it.
- `.luarc.json`, only if there is none: LuaLS's `$schema` and
  `"runtime.version": "Lua 5.1"`. It never sets `workspace.library`, since
  the types file is inside the workspace. Edit it as you like; later runs keep
  it.

```console
$ neopane provider types
wrote ~/.config/neopane/types/neopane.lua
wrote ~/.config/neopane/.luarc.json
no init.lua yet: see https://github.com/ian-pascoe/neopane/blob/main/docs/config.md
```

Open `~/.config/neopane` as the editor's workspace folder. LuaLS reads neither
file in single-file mode, so opening `init.lua` on its own gets no types.

Annotate the table you return to have it checked:

```lua
---@type neopane.Config
return {
  provider = function(attach)
    return neopane.default(attach)
  end,
}
```

[lazydev.nvim](https://github.com/folke/lazydev.nvim) adds Neovim's own
`vim` API to every Lua workspace, which the config doesn't have. Turn it off
where a `.luarc.json` is:

```lua
{
  "folke/lazydev.nvim",
  ft = "lua",
  opts = {
    enabled = function(root)
      return not vim.uv.fs_stat(root .. "/.luarc.json")
    end,
  },
}
```
